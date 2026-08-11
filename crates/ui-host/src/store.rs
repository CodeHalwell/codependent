//! Revisioned, atomic semantic-tree storage.

use std::collections::HashMap;

use codypendent_protocol::{
    patch_operations, UiActionBinding, UiActionId, UiActionInvocation, UiDocument, UiDocumentId,
    UiEvent, UiHardLimits, UiNode, UiNodeId, UiPatch, UiPatchBatch, UiProtocolVersion,
    UiValidationError,
};
use serde_json::Value;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UiHostError {
    #[error("remote UI document id is empty")]
    EmptyDocumentId,
    #[error("remote UI document `{0}` is not mounted")]
    DocumentNotFound(String),
    #[error("remote UI document `{0}` is already mounted at a newer revision")]
    StaleSnapshot(String),
    #[error("remote UI document `{0}` conflicts with the mounted tree at the same revision")]
    ConflictingSnapshot(String),
    #[error("remote UI validation failed ({code} at {path}): {message}")]
    Validation {
        code: String,
        path: String,
        message: String,
    },
    #[error("remote UI protocol {major}.{minor} is unsupported")]
    UnsupportedProtocol { major: u16, minor: u16 },
    #[error("patch for `{document}` starts at revision {actual}, expected {expected}")]
    StalePatch {
        document: String,
        expected: u64,
        actual: u64,
    },
    #[error("patch for `{document}` must advance exactly one revision ({base} -> {revision})")]
    InvalidRevision {
        document: String,
        base: u64,
        revision: u64,
    },
    #[error("patch batch for `{0}` is not atomic")]
    NonAtomicPatch(String),
    #[error("patch batch contains {actual} operations, maximum is {maximum}")]
    PatchLimit { actual: usize, maximum: u32 },
    #[error("patch {index} uses unsupported operation `{operation}`")]
    UnsupportedPatch { index: usize, operation: String },
    #[error("patch {index} is missing `{field}`")]
    MissingPatchField { index: usize, field: &'static str },
    #[error("patch {index} targets missing node `{node_id}`")]
    NodeNotFound { index: usize, node_id: String },
    #[error("patch {index} targets a node id that occurs more than once: `{node_id}`")]
    AmbiguousNode { index: usize, node_id: String },
    #[error("patch {index} cannot remove or move the document root")]
    RootMutation { index: usize },
    #[error("patch {index} insert position {position} exceeds child count {children}")]
    InvalidInsert {
        index: usize,
        position: usize,
        children: usize,
    },
    #[error("patch {index} would move `{node_id}` into its own subtree")]
    MoveCycle { index: usize, node_id: String },
    #[error("remote UI tree is invalid: {0}")]
    InvalidTree(String),
    #[error("event for `{document}` targets revision {actual}, expected {expected}")]
    StaleEvent {
        document: String,
        expected: u64,
        actual: u64,
    },
    #[error("event target `{0}` is not present in the current document")]
    EventTargetNotFound(String),
    #[error("action for `{document}` targets revision {actual}, expected {expected}")]
    StaleAction {
        document: String,
        expected: u64,
        actual: u64,
    },
    #[error("action source `{0}` is not present in the current document")]
    ActionTargetNotFound(String),
    #[error("node `{node}` does not bind event `{event}`")]
    EventNotBound { node: String, event: String },
    #[error("node `{0}` binds an action but is currently disabled")]
    ActionDisabled(String),
}

/// In-memory view cache. Durable state belongs to the daemon; a disconnected
/// client reconstructs this cache from snapshots and patch streams.
#[derive(Debug, Clone)]
pub struct DocumentStore {
    limits: UiHardLimits,
    documents: HashMap<UiDocumentId, UiDocument>,
}

impl Default for DocumentStore {
    fn default() -> Self {
        Self::new(UiHardLimits::default())
    }
}

impl DocumentStore {
    #[must_use]
    pub fn new(limits: UiHardLimits) -> Self {
        Self {
            limits,
            documents: HashMap::new(),
        }
    }

    #[must_use]
    pub fn limits(&self) -> UiHardLimits {
        self.limits
    }

    /// Replace the active ceilings only when every mounted snapshot remains
    /// valid. Failure leaves both limits and documents unchanged.
    pub fn set_limits(&mut self, limits: UiHardLimits) -> Result<(), UiHostError> {
        limits.validate().map_err(validation_error)?;
        for document in self.documents.values() {
            document.validate(&limits).map_err(validation_error)?;
            validate_tree(document, limits)?;
        }
        self.limits = limits;
        Ok(())
    }

    #[must_use]
    pub fn document(&self, id: &UiDocumentId) -> Option<&UiDocument> {
        self.documents.get(id)
    }

    pub fn documents(&self) -> impl Iterator<Item = &UiDocument> {
        self.documents.values()
    }

    pub fn remove(&mut self, id: &UiDocumentId) -> Option<UiDocument> {
        self.documents.remove(id)
    }

    /// Mount or replace an authoritative snapshot. An equal revision is useful
    /// after reconnect because it restores the exact daemon-owned view state.
    pub fn mount(&mut self, document: UiDocument) -> Result<(), UiHostError> {
        check_protocol(document.protocol_version)?;
        if document.document_id.is_empty() {
            return Err(UiHostError::EmptyDocumentId);
        }
        document.validate(&self.limits).map_err(validation_error)?;
        validate_tree(&document, self.limits)?;
        if let Some(current) = self.documents.get(&document.document_id) {
            if current.revision > document.revision {
                return Err(UiHostError::StaleSnapshot(document.document_id.to_string()));
            }
            if current.revision == document.revision && current != &document {
                return Err(UiHostError::ConflictingSnapshot(
                    document.document_id.to_string(),
                ));
            }
        }
        self.documents
            .insert(document.document_id.clone(), document);
        Ok(())
    }

    /// Apply an entire batch to a clone and publish only after every operation
    /// and the resulting tree validate. A failed batch leaves the current view
    /// byte-for-byte unchanged.
    pub fn apply(&mut self, batch: &UiPatchBatch) -> Result<&UiDocument, UiHostError> {
        check_protocol(batch.protocol_version)?;
        batch.validate(&self.limits).map_err(validation_error)?;
        if !batch.atomic {
            return Err(UiHostError::NonAtomicPatch(batch.document_id.to_string()));
        }
        if batch.patches.len() > self.limits.max_patches_per_batch as usize {
            return Err(UiHostError::PatchLimit {
                actual: batch.patches.len(),
                maximum: self.limits.max_patches_per_batch,
            });
        }
        let current = self
            .documents
            .get(&batch.document_id)
            .ok_or_else(|| UiHostError::DocumentNotFound(batch.document_id.to_string()))?;
        if current.revision != batch.base_revision {
            return Err(UiHostError::StalePatch {
                document: batch.document_id.to_string(),
                expected: current.revision.0,
                actual: batch.base_revision.0,
            });
        }
        if batch.revision.0 != batch.base_revision.0.saturating_add(1) {
            return Err(UiHostError::InvalidRevision {
                document: batch.document_id.to_string(),
                base: batch.base_revision.0,
                revision: batch.revision.0,
            });
        }

        let mut next = current.clone();
        for (index, patch) in batch.patches.iter().enumerate() {
            apply_patch(&mut next.root, patch, index)?;
        }
        next.protocol_version = batch.protocol_version;
        next.revision = batch.revision;
        next.validate(&self.limits).map_err(validation_error)?;
        validate_tree(&next, self.limits)?;
        self.documents.insert(batch.document_id.clone(), next);
        Ok(self
            .documents
            .get(&batch.document_id)
            .expect("just inserted"))
    }

    /// Validate an event against the live tree and resolve its declared action.
    /// Returning an invocation is not permission to perform it; the caller still
    /// routes it through the daemon command/policy layer.
    pub fn action_for_event(&self, event: &UiEvent) -> Result<UiActionInvocation, UiHostError> {
        self.validate_event(event)?
            .ok_or_else(|| UiHostError::EventNotBound {
                node: event.target_id.to_string(),
                event: event.event_type.to_string(),
            })
    }

    /// Validate a revision-bound semantic event while allowing a React-local
    /// handler that intentionally has no daemon command binding.
    pub fn validate_event(
        &self,
        event: &UiEvent,
    ) -> Result<Option<UiActionInvocation>, UiHostError> {
        check_protocol(event.protocol_version)?;
        event.validate(&self.limits).map_err(validation_error)?;
        let document = self
            .documents
            .get(&event.document_id)
            .ok_or_else(|| UiHostError::DocumentNotFound(event.document_id.to_string()))?;
        if document.revision != event.revision {
            return Err(UiHostError::StaleEvent {
                document: event.document_id.to_string(),
                expected: document.revision.0,
                actual: event.revision.0,
            });
        }
        let node = unique_node(&document.root, &event.target_id).map_err(|count| {
            if count == 0 {
                UiHostError::EventTargetNotFound(event.target_id.to_string())
            } else {
                UiHostError::AmbiguousNode {
                    index: 0,
                    node_id: event.target_id.to_string(),
                }
            }
        })?;
        if node
            .props
            .extension
            .get("disabled")
            .and_then(Value::as_bool)
            == Some(true)
            || node
                .props
                .input
                .as_ref()
                .is_some_and(|input| input.disabled)
            || node
                .props
                .navigation
                .as_ref()
                .and_then(|navigation| navigation.disabled)
                == Some(true)
        {
            return Err(UiHostError::ActionDisabled(event.target_id.to_string()));
        }
        if let Some(binding) = node
            .props
            .event_bindings
            .iter()
            .find(|binding| binding.event == event.event_type && !binding.disabled)
        {
            return Ok(Some(invocation(event, binding)));
        }
        let action_id = action_property(node, event);
        if action_id.is_none() {
            let declared = node
                .props
                .extension
                .get("eventHandlers")
                .or_else(|| node.props.extension.get("events"))
                .and_then(Value::as_array)
                .is_some_and(|events| {
                    events
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|allowed| allowed == event.event_type.as_str())
                });
            if declared {
                return Ok(None);
            }
            return Err(UiHostError::EventNotBound {
                node: event.target_id.to_string(),
                event: event.event_type.to_string(),
            });
        }
        let declared_payload = node
            .props
            .extension
            .get("payload")
            .cloned()
            .unwrap_or(Value::Null);
        Ok(Some(invocation_for_action(
            event,
            action_id.expect("checked above"),
            &declared_payload,
        )))
    }

    /// Revalidate an action emitted asynchronously by a component worker before
    /// it reaches daemon command policy. This guards the document revision and
    /// source node; command/capability authorization remains the caller's job.
    pub fn validate_action(&self, action: &UiActionInvocation) -> Result<(), UiHostError> {
        action.validate(&self.limits).map_err(validation_error)?;
        let document = self
            .documents
            .get(&action.document_id)
            .ok_or_else(|| UiHostError::DocumentNotFound(action.document_id.to_string()))?;
        if document.revision != action.revision {
            return Err(UiHostError::StaleAction {
                document: action.document_id.to_string(),
                expected: document.revision.0,
                actual: action.revision.0,
            });
        }
        unique_node(&document.root, &action.source_node_id).map_err(|count| {
            if count == 0 {
                UiHostError::ActionTargetNotFound(action.source_node_id.to_string())
            } else {
                UiHostError::AmbiguousNode {
                    index: 0,
                    node_id: action.source_node_id.to_string(),
                }
            }
        })?;
        Ok(())
    }
}

fn check_protocol(version: UiProtocolVersion) -> Result<(), UiHostError> {
    if version.major != UiProtocolVersion::V1.major {
        return Err(UiHostError::UnsupportedProtocol {
            major: version.major,
            minor: version.minor,
        });
    }
    Ok(())
}

fn validation_error(error: UiValidationError) -> UiHostError {
    UiHostError::Validation {
        code: error.code,
        path: error.path,
        message: error.message,
    }
}

fn invocation(event: &UiEvent, binding: &UiActionBinding) -> UiActionInvocation {
    invocation_for_action(event, binding.action_id.clone(), &binding.payload)
}

fn invocation_for_action(
    event: &UiEvent,
    action_id: UiActionId,
    declared_payload: &Value,
) -> UiActionInvocation {
    // Renderer event data is untrusted input. Declarative constants are owned
    // by the producer and cannot be overwritten by a forged renderer payload.
    // Schema-checked user fields travel separately as submit form_data.
    let payload = declared_payload.clone();
    let form_data = if event.event_type.as_str() == "submit" {
        event
            .payload
            .as_object()
            .map_or_else(Default::default, |object| {
                let fields = object
                    .get("formData")
                    .and_then(Value::as_object)
                    .unwrap_or(object);
                fields
                    .iter()
                    .filter(|(key, _)| {
                        !matches!(key.as_str(), "action" | "payload" | "declaredPayload")
                    })
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
    } else {
        Default::default()
    };
    UiActionInvocation {
        invocation_id: event.event_id.clone(),
        document_id: event.document_id.clone(),
        revision: event.revision,
        source_node_id: event.target_id.clone(),
        action_id,
        payload,
        form_data,
        interaction_token: event.interaction_token.clone(),
        interaction_event_type: Some(event.event_type.clone()),
    }
}

fn action_property(node: &UiNode, event: &UiEvent) -> Option<UiActionId> {
    let keys: &[&str] = match event.event_type.as_str() {
        "action" => &["action"],
        "change" => &["changeAction"],
        "submit" => &["submitAction"],
        "select" => &["selectAction", "changeAction", "action"],
        "navigate" => &["navigateAction", "action"],
        "custom" => &["validateAction"],
        _ => &[],
    };
    keys.iter()
        .filter_map(|key| node.props.extension.get(*key).and_then(Value::as_str))
        .find(|value| !value.trim().is_empty())
        .map(UiActionId::from)
}

fn validate_tree(document: &UiDocument, limits: UiHardLimits) -> Result<(), UiHostError> {
    // The protocol crate performs the complete adversarial validation. Keep a
    // local defensive pass too so this host remains safe while decoding legacy
    // documents that predate a validator field.
    let mut nodes = 0_u32;
    let mut text_bytes = 0_u64;
    let mut ids = HashMap::<String, u32>::new();
    validate_node(
        &document.root,
        0,
        limits,
        &mut nodes,
        &mut text_bytes,
        &mut ids,
    )?;
    Ok(())
}

fn validate_node(
    node: &UiNode,
    depth: u16,
    limits: UiHardLimits,
    nodes: &mut u32,
    text_bytes: &mut u64,
    ids: &mut HashMap<String, u32>,
) -> Result<(), UiHostError> {
    if depth > limits.max_tree_depth {
        return Err(UiHostError::InvalidTree(format!(
            "depth {depth} exceeds {}",
            limits.max_tree_depth
        )));
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > limits.max_nodes {
        return Err(UiHostError::InvalidTree(format!(
            "node count exceeds {}",
            limits.max_nodes
        )));
    }
    if let Some(id) = &node.id {
        if id.is_empty() {
            return Err(UiHostError::InvalidTree("node id is empty".into()));
        }
        let count = ids.entry(id.to_string()).or_default();
        *count += 1;
        if *count > 1 {
            return Err(UiHostError::InvalidTree(format!(
                "duplicate node id `{id}`"
            )));
        }
    }
    if let Some(text) = &node.text {
        *text_bytes = text_bytes.saturating_add(text.len() as u64);
    }
    if let Some(content) = &node.props.content {
        if let Some(text) = &content.text {
            *text_bytes = text_bytes.saturating_add(text.len() as u64);
        }
        for span in &content.spans {
            *text_bytes = text_bytes.saturating_add(span.text.len() as u64);
        }
    }
    if *text_bytes > limits.max_text_bytes {
        return Err(UiHostError::InvalidTree(format!(
            "text exceeds {} bytes",
            limits.max_text_bytes
        )));
    }
    if node.props.event_bindings.len() > limits.max_actions_per_node as usize {
        return Err(UiHostError::InvalidTree(format!(
            "node has more than {} actions",
            limits.max_actions_per_node
        )));
    }
    for child in &node.children {
        validate_node(
            child,
            depth.saturating_add(1),
            limits,
            nodes,
            text_bytes,
            ids,
        )?;
    }
    if let Some(fallback) = &node.fallback {
        validate_node(
            fallback,
            depth.saturating_add(1),
            limits,
            nodes,
            text_bytes,
            ids,
        )?;
    }
    Ok(())
}

fn apply_patch(root: &mut UiNode, patch: &UiPatch, index: usize) -> Result<(), UiHostError> {
    match patch.op.as_str() {
        patch_operations::REPLACE_ROOT => {
            *root = required_node(patch, index)?.clone();
        }
        patch_operations::INSERT => {
            let parent = required_parent(patch, index)?;
            let position = required_index(patch, index)?;
            let node = required_node(patch, index)?.clone();
            insert_child(root, parent, position, node, index)?;
        }
        patch_operations::REMOVE => {
            let id = required_id(patch, index)?;
            take_non_root(root, id, index)?;
        }
        patch_operations::REPLACE => {
            let id = required_id(patch, index)?;
            let replacement = required_node(patch, index)?.clone();
            replace_node(root, id, replacement, index)?;
        }
        patch_operations::UPDATE_PROPS => {
            let id = required_id(patch, index)?;
            let props = patch.props.as_ref().ok_or(UiHostError::MissingPatchField {
                index,
                field: "props",
            })?;
            let target = unique_node_mut(root, id, index)?;
            let mut value = serde_json::to_value(&target.props)
                .map_err(|error| UiHostError::InvalidTree(error.to_string()))?;
            let object = value
                .as_object_mut()
                .ok_or_else(|| UiHostError::InvalidTree("props are not an object".into()))?;
            for key in &props.unset {
                object.remove(key);
            }
            for (key, value) in &props.set {
                object.insert(key.clone(), value.clone());
            }
            target.props = serde_json::from_value(value)
                .map_err(|error| UiHostError::InvalidTree(error.to_string()))?;
        }
        patch_operations::SET_TEXT => {
            let id = required_id(patch, index)?;
            let text = patch.text.as_ref().ok_or(UiHostError::MissingPatchField {
                index,
                field: "text",
            })?;
            unique_node_mut(root, id, index)?.text = Some(text.clone());
        }
        patch_operations::MOVE => {
            let id = required_id(patch, index)?;
            let parent = required_parent(patch, index)?;
            let position = required_index(patch, index)?;
            if id == parent || unique_node(root, id).is_ok_and(|node| contains_id(node, parent)) {
                return Err(UiHostError::MoveCycle {
                    index,
                    node_id: id.to_string(),
                });
            }
            let moving = take_non_root(root, id, index)?;
            insert_child(root, parent, position, moving, index)?;
        }
        operation => {
            return Err(UiHostError::UnsupportedPatch {
                index,
                operation: operation.to_owned(),
            });
        }
    }
    Ok(())
}

fn required_id(patch: &UiPatch, index: usize) -> Result<&UiNodeId, UiHostError> {
    patch
        .node_id
        .as_ref()
        .ok_or(UiHostError::MissingPatchField {
            index,
            field: "nodeId",
        })
}

fn required_parent(patch: &UiPatch, index: usize) -> Result<&UiNodeId, UiHostError> {
    patch
        .parent_id
        .as_ref()
        .ok_or(UiHostError::MissingPatchField {
            index,
            field: "parentId",
        })
}

fn required_index(patch: &UiPatch, index: usize) -> Result<usize, UiHostError> {
    patch
        .index
        .map(|value| value as usize)
        .ok_or(UiHostError::MissingPatchField {
            index,
            field: "index",
        })
}

fn required_node(patch: &UiPatch, index: usize) -> Result<&UiNode, UiHostError> {
    patch.node.as_ref().ok_or(UiHostError::MissingPatchField {
        index,
        field: "node",
    })
}

fn unique_node<'a>(root: &'a UiNode, id: &UiNodeId) -> Result<&'a UiNode, usize> {
    fn visit<'a>(node: &'a UiNode, id: &UiNodeId, found: &mut Vec<&'a UiNode>) {
        if node.id.as_ref() == Some(id) {
            found.push(node);
        }
        for child in &node.children {
            visit(child, id, found);
        }
        if let Some(fallback) = &node.fallback {
            visit(fallback, id, found);
        }
    }
    let mut found = Vec::new();
    visit(root, id, &mut found);
    if found.len() == 1 {
        Ok(found[0])
    } else {
        Err(found.len())
    }
}

fn unique_node_mut<'a>(
    root: &'a mut UiNode,
    id: &UiNodeId,
    index: usize,
) -> Result<&'a mut UiNode, UiHostError> {
    let count = count_id(root, id);
    if count == 0 {
        return Err(UiHostError::NodeNotFound {
            index,
            node_id: id.to_string(),
        });
    }
    if count > 1 {
        return Err(UiHostError::AmbiguousNode {
            index,
            node_id: id.to_string(),
        });
    }
    find_node_mut(root, id).ok_or_else(|| UiHostError::NodeNotFound {
        index,
        node_id: id.to_string(),
    })
}

fn count_id(node: &UiNode, id: &UiNodeId) -> usize {
    usize::from(node.id.as_ref() == Some(id))
        + node
            .children
            .iter()
            .map(|child| count_id(child, id))
            .sum::<usize>()
        + node
            .fallback
            .as_ref()
            .map_or(0, |fallback| count_id(fallback, id))
}

fn find_node_mut<'a>(node: &'a mut UiNode, id: &UiNodeId) -> Option<&'a mut UiNode> {
    if node.id.as_ref() == Some(id) {
        return Some(node);
    }
    for child in &mut node.children {
        if let Some(found) = find_node_mut(child, id) {
            return Some(found);
        }
    }
    node.fallback
        .as_mut()
        .and_then(|fallback| find_node_mut(fallback, id))
}

fn contains_id(node: &UiNode, id: &UiNodeId) -> bool {
    node.id.as_ref() == Some(id)
        || node.children.iter().any(|child| contains_id(child, id))
        || node
            .fallback
            .as_ref()
            .is_some_and(|fallback| contains_id(fallback, id))
}

fn insert_child(
    root: &mut UiNode,
    parent: &UiNodeId,
    position: usize,
    node: UiNode,
    patch_index: usize,
) -> Result<(), UiHostError> {
    let target = unique_node_mut(root, parent, patch_index)?;
    if position > target.children.len() {
        return Err(UiHostError::InvalidInsert {
            index: patch_index,
            position,
            children: target.children.len(),
        });
    }
    target.children.insert(position, node);
    Ok(())
}

fn take_non_root(
    root: &mut UiNode,
    id: &UiNodeId,
    patch_index: usize,
) -> Result<UiNode, UiHostError> {
    if root.id.as_ref() == Some(id) {
        return Err(UiHostError::RootMutation { index: patch_index });
    }
    fn take(node: &mut UiNode, id: &UiNodeId) -> Option<UiNode> {
        if let Some(position) = node
            .children
            .iter()
            .position(|child| child.id.as_ref() == Some(id))
        {
            return Some(node.children.remove(position));
        }
        for child in &mut node.children {
            if let Some(found) = take(child, id) {
                return Some(found);
            }
        }
        if node
            .fallback
            .as_ref()
            .is_some_and(|fallback| fallback.id.as_ref() == Some(id))
        {
            return node.fallback.take().map(|fallback| *fallback);
        }
        if let Some(fallback) = node.fallback.as_mut() {
            if let Some(found) = take(fallback, id) {
                return Some(found);
            }
        }
        None
    }
    take(root, id).ok_or_else(|| UiHostError::NodeNotFound {
        index: patch_index,
        node_id: id.to_string(),
    })
}

fn replace_node(
    root: &mut UiNode,
    id: &UiNodeId,
    replacement: UiNode,
    patch_index: usize,
) -> Result<(), UiHostError> {
    if root.id.as_ref() == Some(id) {
        *root = replacement;
        return Ok(());
    }
    let target = unique_node_mut(root, id, patch_index)?;
    *target = replacement;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_protocol::{
        node_kinds, primitives, UiActionId, UiEventId, UiEventType, UiNodeKind, UiNodeProps,
        UiPatchOperation, UiPrimitive, UiPropsPatch, UiRevision,
    };
    use serde_json::Value;
    use std::collections::BTreeMap;

    fn node(id: &str, text: &str) -> UiNode {
        UiNode {
            kind: UiNodeKind::from(node_kinds::ELEMENT),
            id: Some(UiNodeId::from(id)),
            node_type: Some(UiPrimitive::from(primitives::TEXT)),
            text: Some(text.into()),
            props: UiNodeProps::default(),
            children: Vec::new(),
            fallback: None,
            requires: Vec::new(),
        }
    }

    fn document() -> UiDocument {
        let mut root = UiNode::element("root", primitives::STACK);
        root.children.push(node("a", "before"));
        UiDocument {
            protocol_version: UiProtocolVersion::V1,
            document_id: UiDocumentId::from("view"),
            revision: UiRevision(1),
            root,
            capabilities: None,
            metadata: BTreeMap::new(),
            compatibility: None,
        }
    }

    fn patch(op: &str) -> UiPatch {
        UiPatch {
            op: UiPatchOperation::from(op),
            node_id: None,
            parent_id: None,
            index: None,
            node: None,
            props: None,
            text: None,
            payload: Value::Null,
        }
    }

    fn batch(patches: Vec<UiPatch>) -> UiPatchBatch {
        UiPatchBatch {
            protocol_version: UiProtocolVersion::V1,
            document_id: UiDocumentId::from("view"),
            base_revision: UiRevision(1),
            revision: UiRevision(2),
            patches,
            issued_at: None,
            atomic: true,
            fallback: None,
        }
    }

    #[test]
    fn a_failed_batch_is_atomic() {
        let mut store = DocumentStore::default();
        store.mount(document()).unwrap();
        let before = store.document(&UiDocumentId::from("view")).unwrap().clone();
        let mut first = patch(patch_operations::SET_TEXT);
        first.node_id = Some(UiNodeId::from("a"));
        first.text = Some("changed".into());
        let mut second = patch(patch_operations::REMOVE);
        second.node_id = Some(UiNodeId::from("missing"));
        assert!(store.apply(&batch(vec![first, second])).is_err());
        assert_eq!(store.document(&UiDocumentId::from("view")), Some(&before));
    }

    #[test]
    fn patches_text_properties_and_children() {
        let mut store = DocumentStore::default();
        store.mount(document()).unwrap();
        let mut set_text = patch(patch_operations::SET_TEXT);
        set_text.node_id = Some(UiNodeId::from("a"));
        set_text.text = Some("after".into());
        let mut update = patch(patch_operations::UPDATE_PROPS);
        update.node_id = Some(UiNodeId::from("a"));
        update.props = Some(UiPropsPatch {
            set: BTreeMap::from([("custom".into(), Value::String("value".into()))]),
            unset: Vec::new(),
        });
        let mut insert = patch(patch_operations::INSERT);
        insert.parent_id = Some(UiNodeId::from("root"));
        insert.index = Some(1);
        insert.node = Some(node("b", "second"));
        let next = store.apply(&batch(vec![set_text, update, insert])).unwrap();
        assert_eq!(next.root.children[0].text.as_deref(), Some("after"));
        assert_eq!(
            next.root.children[0].props.extension.get("custom"),
            Some(&Value::String("value".into()))
        );
        assert_eq!(next.root.children[1].id.as_ref().unwrap().as_str(), "b");
    }

    #[test]
    fn stale_events_and_unbound_actions_are_rejected() {
        let mut store = DocumentStore::default();
        let mut doc = document();
        doc.root.children[0]
            .props
            .event_bindings
            .push(UiActionBinding {
                event: UiEventType::from("action"),
                action_id: UiActionId::from("open"),
                payload: Value::String("default".into()),
                requires: Vec::new(),
                disabled: false,
                confirmation: None,
            });
        store.mount(doc).unwrap();
        let mut event = UiEvent {
            protocol_version: UiProtocolVersion::V1,
            event_id: UiEventId::from("evt"),
            document_id: UiDocumentId::from("view"),
            revision: UiRevision(0),
            target_id: UiNodeId::from("a"),
            event_type: UiEventType::from("action"),
            payload: Value::Null,
            modifiers: None,
            timestamp: None,
            interaction_token: None,
        };
        assert!(matches!(
            store.action_for_event(&event),
            Err(UiHostError::StaleEvent { .. })
        ));
        event.revision = UiRevision(1);
        assert_eq!(
            store.action_for_event(&event).unwrap().action_id.as_str(),
            "open"
        );
        event.event_type = UiEventType::from("change");
        assert!(matches!(
            store.action_for_event(&event),
            Err(UiHostError::EventNotBound { .. })
        ));
    }

    #[test]
    fn sdk_action_props_and_form_payloads_become_typed_invocations() {
        let mut store = DocumentStore::default();
        let mut doc = document();
        doc.root.children[0]
            .props
            .extension
            .insert("submitAction".into(), Value::String("settings.save".into()));
        doc.root.children[0].props.extension.insert(
            "payload".into(),
            serde_json::json!({"scope": "trusted", "stream": false}),
        );
        store.mount(doc).unwrap();
        let event = UiEvent {
            protocol_version: UiProtocolVersion::V1,
            event_id: UiEventId::from("form-event"),
            document_id: UiDocumentId::from("view"),
            revision: UiRevision(1),
            target_id: UiNodeId::from("a"),
            event_type: UiEventType::from("submit"),
            payload: serde_json::json!({"model": "gpt-5", "stream": true}),
            modifiers: None,
            timestamp: None,
            interaction_token: None,
        };
        let invocation = store.action_for_event(&event).unwrap();
        assert_eq!(invocation.action_id.as_str(), "settings.save");
        assert_eq!(invocation.form_data.get("stream"), Some(&Value::Bool(true)));
        assert_eq!(
            invocation.payload,
            serde_json::json!({"scope": "trusted", "stream": false})
        );
    }

    #[test]
    fn equal_authoritative_snapshots_restore_after_reconnect() {
        let mut store = DocumentStore::default();
        let authoritative = document();
        store.mount(authoritative.clone()).unwrap();
        store.mount(authoritative).unwrap();

        let mut stale = document();
        stale.revision = UiRevision(0);
        assert!(matches!(
            store.mount(stale),
            Err(UiHostError::StaleSnapshot(_))
        ));

        let mut conflicting = document();
        conflicting.root.children[0].text = Some("different tree".to_owned());
        assert!(matches!(
            store.mount(conflicting),
            Err(UiHostError::ConflictingSnapshot(_))
        ));
    }

    #[test]
    fn react_handler_presence_allows_owner_forwarding_without_an_action() {
        let mut store = DocumentStore::default();
        let mut doc = document();
        doc.root.children[0]
            .props
            .extension
            .insert("eventHandlers".to_owned(), serde_json::json!(["change"]));
        store.mount(doc).unwrap();
        let event = UiEvent {
            protocol_version: UiProtocolVersion::V1,
            event_id: UiEventId::from("local-change"),
            document_id: UiDocumentId::from("view"),
            revision: UiRevision(1),
            target_id: UiNodeId::from("a"),
            event_type: UiEventType::from("change"),
            payload: serde_json::json!({"value": "updated"}),
            modifiers: None,
            timestamp: None,
            interaction_token: None,
        };
        assert_eq!(store.validate_event(&event).unwrap(), None);
    }

    #[test]
    fn cyclic_moves_and_duplicate_insertions_leave_the_tree_unchanged() {
        let mut store = DocumentStore::default();
        let mut doc = document();
        let mut parent = UiNode::element("parent", primitives::STACK);
        parent
            .children
            .push(UiNode::element("nested", primitives::STACK));
        doc.root.children.push(parent);
        store.mount(doc).unwrap();
        let before = store.document(&UiDocumentId::from("view")).unwrap().clone();

        let mut cycle = patch(patch_operations::MOVE);
        cycle.node_id = Some(UiNodeId::from("parent"));
        cycle.parent_id = Some(UiNodeId::from("nested"));
        cycle.index = Some(0);
        assert!(matches!(
            store.apply(&batch(vec![cycle])),
            Err(UiHostError::MoveCycle { .. })
        ));
        assert_eq!(store.document(&UiDocumentId::from("view")), Some(&before));

        let mut duplicate = patch(patch_operations::INSERT);
        duplicate.parent_id = Some(UiNodeId::from("root"));
        duplicate.index = Some(0);
        duplicate.node = Some(node("a", "duplicate"));
        assert!(matches!(
            store.apply(&batch(vec![duplicate])),
            Err(UiHostError::InvalidTree(_)) | Err(UiHostError::Validation { .. })
        ));
        assert_eq!(store.document(&UiDocumentId::from("view")), Some(&before));
    }
}
