//! Idempotent routing for one connected Remote UI renderer.

use std::collections::{HashSet, VecDeque};

use codypendent_protocol::{
    UiActionCancellation, UiActionInvocation, UiActionResult, UiCapabilities,
    UiCapabilitySelection, UiContributionId, UiHardLimits, UiHotReload, UiProjectionSubscription,
    UiProjectionUnsubscription, UiProjectionUpdate, UiRemoteError, UiResyncRequest, UiRevision,
    UiTheme, UiValidationError, UiViewport, UiWireMessage,
};

use crate::{ContributionRegistry, DocumentStore, RegistrationTrust, UiHostError, UiRegistryError};

const DEFAULT_SEEN_MESSAGE_LIMIT: usize = 4_096;

#[derive(Debug, Clone, PartialEq)]
pub enum UiSessionUpdate {
    Negotiated(UiCapabilitySelection),
    SnapshotMounted {
        document_id: String,
        revision: UiRevision,
    },
    PatchApplied {
        document_id: String,
        revision: UiRevision,
    },
    DocumentDisposed {
        document_id: String,
        revision: UiRevision,
    },
    Action(UiActionInvocation),
    SubscriptionRequested(UiProjectionSubscription),
    SubscriptionCancelled(UiProjectionUnsubscription),
    ProjectionChanged(UiProjectionUpdate),
    ActionResult(UiActionResult),
    ActionCancelled(UiActionCancellation),
    ViewportChanged(UiViewport),
    ResyncRequested(UiResyncRequest),
    HotReload(UiHotReload),
    ContributionsChanged(Vec<UiContributionId>),
    ThemeChanged(UiTheme),
    RemoteError(UiRemoteError),
    DuplicateIgnored {
        message_id: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum UiSessionError {
    #[error(transparent)]
    Validation(#[from] UiValidationError),
    #[error(transparent)]
    Host(#[from] UiHostError),
    #[error(transparent)]
    Registry(#[from] UiRegistryError),
    #[error("remote UI capability negotiation must complete before `{0}`")]
    NegotiationRequired(&'static str),
    #[error("remote UI dispose for `{document}` targets revision {actual}, expected {expected}")]
    StaleDispose {
        document: String,
        expected: u64,
        actual: u64,
    },
    #[error("unsupported remote UI message type `{0}`")]
    UnsupportedMessage(String),
}

/// Per-renderer trusted boundary shared by terminal and graphical clients.
/// It contains no sockets or widgets: callers fold its outcomes into their own
/// unidirectional application loop.
#[derive(Debug, Clone)]
pub struct UiHostSession {
    offer: UiCapabilities,
    client_capabilities: Option<UiCapabilities>,
    selection: Option<UiCapabilitySelection>,
    documents: DocumentStore,
    registry: ContributionRegistry,
    theme: Option<UiTheme>,
    seen_ids: HashSet<String>,
    seen_order: VecDeque<String>,
    seen_limit: usize,
}

impl UiHostSession {
    pub fn new(offer: UiCapabilities, limits: UiHardLimits) -> Result<Self, UiSessionError> {
        offer.validate()?;
        limits.validate()?;
        Ok(Self {
            offer,
            client_capabilities: None,
            selection: None,
            documents: DocumentStore::new(limits),
            registry: ContributionRegistry::new(limits),
            theme: None,
            seen_ids: HashSet::new(),
            seen_order: VecDeque::new(),
            seen_limit: DEFAULT_SEEN_MESSAGE_LIMIT,
        })
    }

    #[must_use]
    pub fn documents(&self) -> &DocumentStore {
        &self.documents
    }

    #[must_use]
    pub fn registry(&self) -> &ContributionRegistry {
        &self.registry
    }

    pub fn registry_mut(&mut self) -> &mut ContributionRegistry {
        &mut self.registry
    }

    #[must_use]
    pub fn selection(&self) -> Option<&UiCapabilitySelection> {
        self.selection.as_ref()
    }

    #[must_use]
    pub fn client_capabilities(&self) -> Option<&UiCapabilities> {
        self.client_capabilities.as_ref()
    }

    #[must_use]
    pub fn theme(&self) -> Option<&UiTheme> {
        self.theme.as_ref()
    }

    /// Apply one validated message. Successfully handled message IDs remain in
    /// a bounded replay window, preventing reconnect/retry from dispatching the
    /// same user action twice.
    pub fn handle(
        &mut self,
        message: UiWireMessage,
        trust: RegistrationTrust,
    ) -> Result<UiSessionUpdate, UiSessionError> {
        message.validate(&self.documents.limits())?;
        let message_id = message.message_id.clone();
        if self.seen_ids.contains(&message_id) {
            return Ok(UiSessionUpdate::DuplicateIgnored { message_id });
        }

        let update = match message.kind.as_str() {
            "capabilities" => {
                let capabilities = message
                    .capabilities
                    .as_ref()
                    .expect("validated capability message");
                let selection = capabilities.negotiate(&self.offer)?;
                self.documents.set_limits(selection.limits)?;
                self.client_capabilities = Some(capabilities.clone());
                self.selection = Some(selection.clone());
                UiSessionUpdate::Negotiated(selection)
            }
            "capabilitySelection" => {
                let selection = message
                    .selection
                    .as_ref()
                    .expect("validated capability selection");
                self.documents.set_limits(selection.limits)?;
                self.selection = Some(selection.clone());
                UiSessionUpdate::Negotiated(selection.clone())
            }
            "snapshot" => {
                let document = message
                    .snapshot
                    .expect("validated snapshot message")
                    .document;
                let document_id = document.document_id.to_string();
                let revision = document.revision;
                self.documents.mount(document)?;
                UiSessionUpdate::SnapshotMounted {
                    document_id,
                    revision,
                }
            }
            "patchBatch" => {
                let batch = message
                    .patch_batch
                    .as_ref()
                    .expect("validated patch message");
                let document_id = batch.document_id.to_string();
                let revision = batch.revision;
                self.documents.apply(batch)?;
                UiSessionUpdate::PatchApplied {
                    document_id,
                    revision,
                }
            }
            "event" => {
                self.require_selection("event")?;
                let event = message.event.as_ref().expect("validated event message");
                UiSessionUpdate::Action(self.documents.action_for_event(event)?)
            }
            "action" => {
                self.require_selection("action")?;
                let action = message.action.expect("validated action message");
                self.documents.validate_action(&action)?;
                UiSessionUpdate::Action(action)
            }
            "subscription" => {
                self.require_selection("subscription")?;
                UiSessionUpdate::SubscriptionRequested(
                    message
                        .subscription
                        .expect("validated subscription message"),
                )
            }
            "unsubscribe" => {
                self.require_selection("unsubscribe")?;
                UiSessionUpdate::SubscriptionCancelled(
                    message
                        .unsubscription
                        .expect("validated unsubscription message"),
                )
            }
            "projection" => {
                self.require_selection("projection")?;
                UiSessionUpdate::ProjectionChanged(
                    message.projection.expect("validated projection message"),
                )
            }
            "actionResult" => {
                self.require_selection("actionResult")?;
                UiSessionUpdate::ActionResult(
                    message
                        .action_result
                        .expect("validated action result message"),
                )
            }
            "cancelAction" => {
                self.require_selection("cancelAction")?;
                UiSessionUpdate::ActionCancelled(
                    message
                        .cancellation
                        .expect("validated cancellation message"),
                )
            }
            "dispose" => {
                let dispose = message.dispose.expect("validated dispose message");
                if let Some(current) = self.documents.document(&dispose.document_id) {
                    if current.revision != dispose.revision {
                        return Err(UiSessionError::StaleDispose {
                            document: dispose.document_id.to_string(),
                            expected: current.revision.0,
                            actual: dispose.revision.0,
                        });
                    }
                }
                self.documents.remove(&dispose.document_id);
                UiSessionUpdate::DocumentDisposed {
                    document_id: dispose.document_id.to_string(),
                    revision: dispose.revision,
                }
            }
            "viewport" => {
                let viewport = message.viewport.expect("validated viewport message");
                if let Some(capabilities) = self.client_capabilities.as_mut() {
                    capabilities.viewport = viewport;
                }
                if let Some(selection) = self.selection.as_mut() {
                    selection.viewport = Some(viewport);
                }
                UiSessionUpdate::ViewportChanged(viewport)
            }
            "resync" => {
                UiSessionUpdate::ResyncRequested(message.resync.expect("validated resync message"))
            }
            "hotReload" => {
                UiSessionUpdate::HotReload(message.hot_reload.expect("validated hot reload"))
            }
            "contributions" => {
                let selection = self.require_selection("contributions")?;
                let mut registry = self.registry.clone();
                let mut extension_ids: HashSet<_> = message
                    .contributions
                    .iter()
                    .map(|registration| registration.extension_id.to_string())
                    .collect();
                if message.contributions.is_empty() {
                    if let Some(owner) = message
                        .extensions
                        .get("contributionOwner")
                        .and_then(serde_json::Value::as_str)
                    {
                        extension_ids.insert(owner.to_owned());
                    }
                }
                for extension_id in extension_ids {
                    registry.unregister_extension(&extension_id);
                }
                let mut ids = Vec::with_capacity(message.contributions.len());
                for registration in message.contributions {
                    ids.push(registration.id.clone());
                    registry.register(trust, registration, selection)?;
                }
                self.registry = registry;
                UiSessionUpdate::ContributionsChanged(ids)
            }
            "theme" => {
                let theme = message.theme.expect("validated theme message");
                self.theme = Some(theme.clone());
                UiSessionUpdate::ThemeChanged(theme)
            }
            "error" => {
                UiSessionUpdate::RemoteError(message.error.expect("validated error message"))
            }
            other => return Err(UiSessionError::UnsupportedMessage(other.to_owned())),
        };

        self.remember(message_id);
        Ok(update)
    }

    fn require_selection(
        &self,
        operation: &'static str,
    ) -> Result<&UiCapabilitySelection, UiSessionError> {
        self.selection
            .as_ref()
            .ok_or(UiSessionError::NegotiationRequired(operation))
    }

    fn remember(&mut self, message_id: String) {
        self.seen_ids.insert(message_id.clone());
        self.seen_order.push_back(message_id);
        while self.seen_order.len() > self.seen_limit {
            if let Some(expired) = self.seen_order.pop_front() {
                self.seen_ids.remove(&expired);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use codypendent_protocol::{
        primitives, ClientCapabilities, UiClientKind, UiColorDepth, UiDocument, UiDocumentId,
        UiEvent, UiEventId, UiEventType, UiMediaCapability, UiNode, UiNodeId, UiPrimitive,
        UiProtocolVersion, UiRevision, UiSnapshot,
    };
    use serde_json::Value;

    use super::*;

    fn capabilities() -> UiCapabilities {
        UiCapabilities {
            client: UiClientKind::from("terminal"),
            protocol_versions: vec![UiProtocolVersion::V1],
            daemon: ClientCapabilities {
                rich_text: true,
                image_display: false,
                audio_capture: false,
                editor_mutations: false,
                diff_view: true,
                mouse: true,
                unicode: true,
                true_color: true,
            },
            primitives: vec![UiPrimitive::from("*")],
            media: Vec::<UiMediaCapability>::new(),
            color_depth: UiColorDepth::from("trueColor"),
            keyboard: true,
            screen_reader: false,
            reduced_motion: false,
            clipboard: true,
            terminal_graphics: Vec::new(),
            viewport: UiViewport {
                width: 100,
                height: 30,
                pixel_width: None,
                pixel_height: None,
                density: None,
            },
            capabilities: Vec::new(),
            contribution_points: Vec::new(),
            limits: UiHardLimits::default(),
        }
    }

    fn message(kind: &str, id: &str) -> UiWireMessage {
        UiWireMessage {
            kind: kind.into(),
            message_id: id.into(),
            snapshot: None,
            patch_batch: None,
            event: None,
            action: None,
            subscription: None,
            unsubscription: None,
            projection: None,
            action_result: None,
            cancellation: None,
            dispose: None,
            viewport: None,
            resync: None,
            hot_reload: None,
            capabilities: None,
            selection: None,
            contributions: Vec::new(),
            theme: None,
            error: None,
            extensions: BTreeMap::new(),
        }
    }

    fn document() -> UiDocument {
        let mut button = UiNode::element("run", primitives::BUTTON);
        button
            .props
            .extension
            .insert("action".into(), Value::String("run.start".into()));
        let mut root = UiNode::element("root", primitives::STACK);
        root.children.push(button);
        UiDocument {
            protocol_version: UiProtocolVersion::V1,
            document_id: UiDocumentId::from("view"),
            revision: UiRevision(0),
            root,
            capabilities: None,
            metadata: BTreeMap::new(),
            compatibility: None,
        }
    }

    #[test]
    fn negotiates_mounts_and_deduplicates_actions() {
        let mut session = UiHostSession::new(capabilities(), UiHardLimits::default()).unwrap();
        let mut negotiate = message("capabilities", "caps-1");
        negotiate.capabilities = Some(capabilities());
        assert!(matches!(
            session.handle(negotiate, RegistrationTrust::Core).unwrap(),
            UiSessionUpdate::Negotiated(_)
        ));

        let mut snapshot = message("snapshot", "snapshot-1");
        snapshot.snapshot = Some(UiSnapshot {
            document: document(),
            reason: None,
        });
        session.handle(snapshot, RegistrationTrust::Core).unwrap();

        let mut event_message = message("event", "event-message-1");
        event_message.event = Some(UiEvent {
            protocol_version: UiProtocolVersion::V1,
            event_id: UiEventId::from("event-1"),
            document_id: UiDocumentId::from("view"),
            revision: UiRevision(0),
            target_id: UiNodeId::from("run"),
            event_type: UiEventType::from("action"),
            payload: Value::Null,
            modifiers: None,
            timestamp: None,
            interaction_token: None,
        });
        let duplicate = event_message.clone();
        assert!(matches!(
            session
                .handle(event_message, RegistrationTrust::Core)
                .unwrap(),
            UiSessionUpdate::Action(action) if action.action_id.as_str() == "run.start"
        ));
        assert!(matches!(
            session.handle(duplicate, RegistrationTrust::Core).unwrap(),
            UiSessionUpdate::DuplicateIgnored { .. }
        ));
    }

    #[test]
    fn routes_mediated_projection_requests_after_negotiation() {
        let mut session = UiHostSession::new(capabilities(), UiHardLimits::default()).unwrap();
        let mut negotiate = message("capabilities", "caps-mediated");
        negotiate.capabilities = Some(capabilities());
        session.handle(negotiate, RegistrationTrust::Core).unwrap();

        let mut subscribe = message("subscription", "subscription-message");
        subscribe.subscription = Some(UiProjectionSubscription {
            subscription_id: "active-session".to_owned(),
            kind: "session".to_owned(),
            resource_id: Some("session-1".to_owned()),
            parameters: BTreeMap::new(),
        });
        assert!(matches!(
            session.handle(subscribe, RegistrationTrust::Core).unwrap(),
            UiSessionUpdate::SubscriptionRequested(subscription)
                if subscription.subscription_id == "active-session"
                    && subscription.resource_id.as_deref() == Some("session-1")
        ));
    }
}
