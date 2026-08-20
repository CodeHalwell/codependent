//! Reducer-owned Remote UI host state for the terminal client.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use codypendent_protocol::{
    ClientCapabilities, MessageId, UiActionId, UiCapabilities, UiCapability, UiClientKind,
    UiColorDepth, UiContributionPoint, UiDocument, UiDocumentId, UiHardLimits, UiMediaCapability,
    UiNodeId, UiPrimitive, UiProtocolVersion, UiRevision, UiSlotDefinition, UiSlotId, UiViewport,
    UiWireMessage,
};
use codypendent_ui_host::{RegistrationTrust, UiHostSession, UiSessionError, UiSessionUpdate};

use crate::{RemoteUiRenderOutput, RemoteUiViewState, TerminalUiCapabilities};

/// Slots with a real terminal mount adapter. Other public manifest points are
/// intentionally absent until they have point-specific lifecycle and input.
pub const TERMINAL_PUBLIC_SLOTS: &[&str] = &[
    "sidebar",
    "panel",
    "status-item",
    "command",
    "command-palette",
    "composer-accessory",
    "message-renderer",
    "tool-renderer",
    "artifact-renderer",
    "workflow-inspector",
    "blackboard-renderer",
    "document-block",
    "code-graph-node",
    "settings-section",
    "setup-step",
    "form",
    "wizard",
    "dashboard-card",
    "trace-span-renderer",
    "context-menu",
    "quick-pick",
    "notification",
];

/// Main-content adapters. The remaining public points have dedicated composer,
/// footer, or overlay hosts in `render.rs`.
pub const TERMINAL_CENTRAL_SLOTS: &[&str] = &[
    "sidebar",
    "panel",
    "command",
    "command-palette",
    "message-renderer",
    "tool-renderer",
    "artifact-renderer",
    "workflow-inspector",
    "blackboard-renderer",
    "document-block",
    "code-graph-node",
    "settings-section",
    "setup-step",
    "form",
    "wizard",
    "dashboard-card",
    "trace-span-renderer",
    "quick-pick",
];

pub const TERMINAL_OVERLAY_SLOTS: &[&str] = &["context-menu", "notification"];

const CORE_SLOTS: &[&str] = &[
    "approval-frame",
    "approval-actions",
    "secret-entry",
    "policy-state",
    "terminal-lifecycle",
];

#[derive(Debug, Clone)]
pub struct RemoteUiHostState {
    pub host: UiHostSession,
    pub view: RemoteUiViewState,
    pub capabilities: TerminalUiCapabilities,
    pub last_render: RefCell<BTreeMap<UiDocumentId, RemoteUiRenderOutput>>,
    pub active: bool,
    pub focused_document: Option<UiDocumentId>,
    pub pending_confirmation: Option<(UiDocumentId, UiRevision, UiNodeId, UiActionId)>,
    /// The tick at which an armed confirmation stops counting as armed.
    ///
    /// Arming shows a notice that fades after about two seconds while the armed
    /// state itself lived forever — so a stray Enter an hour later fired the
    /// action with nothing on screen saying anything was armed. The signal and
    /// the state expire together now.
    pub pending_confirmation_expires: u64,
    next_event: u64,
}

impl PartialEq for RemoteUiHostState {
    fn eq(&self, other: &Self) -> bool {
        self.view == other.view
            && self.capabilities == other.capabilities
            && self.last_render == other.last_render
            && self.active == other.active
            && self.focused_document == other.focused_document
            && self.pending_confirmation == other.pending_confirmation
            && self.next_event == other.next_event
            && documents(&self.host) == documents(&other.host)
            && self.host.selection() == other.host.selection()
    }
}

impl Default for RemoteUiHostState {
    fn default() -> Self {
        Self::new().expect("built-in terminal Remote UI offer is valid")
    }
}

impl RemoteUiHostState {
    pub fn new() -> Result<Self, UiSessionError> {
        let limits = UiHardLimits::default();
        let mut host = UiHostSession::new(terminal_offer(80, 24, 24, false), limits)?;
        for (slot, trusted_only) in TERMINAL_PUBLIC_SLOTS
            .iter()
            .map(|slot| (*slot, false))
            .chain(CORE_SLOTS.iter().map(|slot| (*slot, true)))
        {
            host.registry_mut()
                .define_slot(UiSlotDefinition {
                    id: UiSlotId::from(slot),
                    point: UiContributionPoint::from(slot),
                    accepts: Vec::new(),
                    trusted_only,
                    maximum_contributions: Some(32),
                    fallback: None,
                })
                .map_err(UiSessionError::from)?;
        }
        Ok(Self {
            host,
            view: RemoteUiViewState::default(),
            capabilities: TerminalUiCapabilities::native(),
            last_render: RefCell::new(BTreeMap::new()),
            active: false,
            focused_document: None,
            pending_confirmation: None,
            pending_confirmation_expires: 0,
            next_event: 0,
        })
    }

    pub fn handle(&mut self, message: UiWireMessage) -> Result<UiSessionUpdate, UiSessionError> {
        let update = self.host.handle(message, RegistrationTrust::Extension)?;
        if let UiSessionUpdate::Negotiated(selection) = &update {
            self.capabilities = TerminalUiCapabilities::from_selection(selection);
        }
        if matches!(
            update,
            UiSessionUpdate::SnapshotMounted { .. }
                | UiSessionUpdate::PatchApplied { .. }
                | UiSessionUpdate::DocumentDisposed { .. }
        ) {
            self.repair_focus();
        }
        Ok(update)
    }

    #[must_use]
    pub fn mounted_documents(&self) -> Vec<&UiDocument> {
        let mounted = self.host.registry().mounted_document_ids();
        let mut documents: Vec<_> = self
            .host
            .documents()
            .documents()
            .filter(|document| mounted.contains(document.document_id.as_str()))
            .collect();
        documents.sort_by(|left, right| left.document_id.cmp(&right.document_id));
        documents
    }

    #[must_use]
    pub fn mounted_documents_for_points(&self, points: &[&str]) -> Vec<&UiDocument> {
        self.mounted_documents()
            .into_iter()
            .filter(|document| {
                self.host
                    .registry()
                    .registration_for_document(document.document_id.as_str())
                    .is_some_and(|registration| points.contains(&registration.point.as_str()))
            })
            .collect()
    }

    /// Broker-attested plugin identity used for immutable extension chrome.
    #[must_use]
    pub fn extension_for_document(&self, document_id: &UiDocumentId) -> Option<&str> {
        self.host
            .registry()
            .extension_for_document(document_id.as_str())
    }

    pub fn extension_identity_for_document(
        &self,
        document_id: &UiDocumentId,
    ) -> Option<(&str, Option<&str>, Option<&str>)> {
        let registration = self
            .host
            .registry()
            .registration_for_document(document_id.as_str())?;
        Some((
            registration
                .metadata
                .get("hostExtensionId")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| registration.extension_id.as_str()),
            registration
                .metadata
                .get("hostPublisher")
                .and_then(serde_json::Value::as_str),
            registration
                .metadata
                .get("hostTrust")
                .and_then(serde_json::Value::as_str),
        ))
    }

    #[must_use]
    pub fn next_message_id(&mut self, prefix: &str) -> String {
        self.next_event = self.next_event.wrapping_add(1);
        format!("{prefix}:terminal:{}", self.next_event)
    }

    pub fn repair_focus(&mut self) {
        if self
            .focused_document
            .as_ref()
            .is_some_and(|id| self.host.documents().document(id).is_none())
        {
            self.focused_document = None;
            self.view.focused_node = None;
            self.active = false;
        }
        if self.focused_document.is_none() {
            self.focused_document = self
                .mounted_documents()
                .first()
                .map(|document| document.document_id.clone());
        }
    }
}

#[must_use]
pub fn terminal_capabilities_message(width: u16, height: u16, color_depth: u16) -> UiWireMessage {
    let mut message = empty_message("capabilities", format!("capabilities:{}", MessageId::new()));
    message.capabilities = Some(terminal_offer(width, height, color_depth, false));
    message
}

/// Capabilities for the cooked `--accessible` shell. It has keyboard input and
/// the full semantic primitive renderer, but deliberately advertises no mouse,
/// colour, Unicode chrome, clipboard, or terminal graphics. `screen_reader` is
/// true so producers can choose their accessible fallback intentionally.
#[must_use]
pub fn accessible_terminal_capabilities_message(width: u16, height: u16) -> UiWireMessage {
    let mut message = empty_message("capabilities", format!("capabilities:{}", MessageId::new()));
    message.capabilities = Some(terminal_offer(width, height, 1, true));
    message
}

#[must_use]
pub fn terminal_viewport_message(width: u16, height: u16) -> UiWireMessage {
    let mut message = empty_message("viewport", format!("viewport:{}", MessageId::new()));
    message.viewport = Some(UiViewport {
        width: u32::from(width),
        height: u32::from(height),
        pixel_width: None,
        pixel_height: None,
        density: None,
    });
    message
}

#[must_use]
pub fn empty_message(kind: impl Into<String>, message_id: impl Into<String>) -> UiWireMessage {
    UiWireMessage {
        kind: kind.into(),
        message_id: message_id.into(),
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

fn terminal_offer(width: u16, height: u16, color_depth: u16, accessible: bool) -> UiCapabilities {
    let color_depth = match color_depth {
        0..=1 => "monochrome",
        2..=4 => "ansi16",
        5..=8 => "ansi256",
        _ => "trueColor",
    };
    UiCapabilities {
        client: UiClientKind::from("terminal"),
        protocol_versions: vec![UiProtocolVersion::V1],
        daemon: ClientCapabilities {
            rich_text: true,
            image_display: false,
            audio_capture: false,
            editor_mutations: false,
            diff_view: true,
            mouse: !accessible,
            unicode: !accessible,
            true_color: color_depth == "trueColor",
            ..ClientCapabilities::default()
        },
        primitives: advertised_primitive_names()
            .into_iter()
            .map(UiPrimitive::from)
            .collect(),
        media: Vec::<UiMediaCapability>::new(),
        color_depth: UiColorDepth::from(color_depth),
        keyboard: true,
        screen_reader: accessible,
        reduced_motion: accessible,
        clipboard: !accessible,
        terminal_graphics: Vec::new(),
        viewport: UiViewport {
            width: u32::from(width),
            height: u32::from(height),
            pixel_width: None,
            pixel_height: None,
            density: None,
        },
        capabilities: vec![
            UiCapability::from("actions"),
            UiCapability::from("forms"),
            UiCapability::from("focus"),
            UiCapability::from("artifact-read"),
            UiCapability::from("context-read"),
            UiCapability::from("run-read"),
            UiCapability::from("workflow-read"),
            UiCapability::from("command-invoke"),
        ],
        contribution_points: TERMINAL_PUBLIC_SLOTS
            .iter()
            .map(|point| UiContributionPoint::from(*point))
            .collect(),
        limits: UiHardLimits::default(),
    }
}

fn documents(host: &UiHostSession) -> Vec<UiDocument> {
    let mut documents: Vec<_> = host.documents().documents().cloned().collect();
    documents.sort_by(|left, right| left.document_id.cmp(&right.document_id));
    documents
}

#[must_use]
pub fn advertised_primitive_names() -> BTreeSet<String> {
    crate::ALL_NATIVE_PRIMITIVES
        .iter()
        .filter(|primitive| !matches!(**primitive, "ApprovalCard" | "PermissionDiff"))
        .map(|primitive| (*primitive).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_offer_advertises_only_real_public_slot_adapters() {
        let offer = terminal_offer(120, 40, 24, false);
        let points: Vec<_> = offer
            .contribution_points
            .iter()
            .map(|point| point.as_str())
            .collect();
        assert_eq!(points, TERMINAL_PUBLIC_SLOTS);
        for required in [
            "sidebar",
            "status-item",
            "command",
            "command-palette",
            "quick-pick",
            "notification",
            "composer-accessory",
            "message-renderer",
            "tool-renderer",
            "artifact-renderer",
            "workflow-inspector",
            "blackboard-renderer",
            "document-block",
            "code-graph-node",
            "settings-section",
            "setup-step",
            "form",
            "wizard",
            "dashboard-card",
            "trace-span-renderer",
            "context-menu",
        ] {
            assert!(points.contains(&required), "missing adapter {required}");
        }
        let routed: BTreeSet<_> = TERMINAL_CENTRAL_SLOTS
            .iter()
            .chain(TERMINAL_OVERLAY_SLOTS)
            .chain([&"status-item", &"composer-accessory"])
            .copied()
            .collect();
        assert_eq!(
            routed,
            TERMINAL_PUBLIC_SLOTS.iter().copied().collect(),
            "every advertised point must have exactly one terminal host route"
        );
        let primitives = advertised_primitive_names();
        assert!(!primitives.contains("ApprovalCard"));
        assert!(!primitives.contains("PermissionDiff"));
    }

    #[test]
    fn accessible_offer_is_screen_reader_first_and_has_no_terminal_only_input() {
        let message = accessible_terminal_capabilities_message(72, 20);
        let offer = message.capabilities.expect("capabilities payload");
        assert_eq!(offer.viewport.width, 72);
        assert_eq!(offer.viewport.height, 20);
        assert_eq!(offer.color_depth.as_str(), "monochrome");
        assert!(offer.keyboard);
        assert!(offer.screen_reader);
        assert!(offer.reduced_motion);
        assert!(!offer.daemon.mouse);
        assert!(!offer.daemon.unicode);
        assert!(!offer.daemon.true_color);
        assert!(!offer.clipboard);
        assert!(offer.terminal_graphics.is_empty());
    }
}
