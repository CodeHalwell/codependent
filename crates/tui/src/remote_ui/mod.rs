//! Native terminal renderer for the renderer-independent Remote UI protocol.
//!
//! This module is deliberately a pure projection. It consumes a validated
//! [`UiDocument`], immutable client state, terminal capabilities and semantic
//! theme tokens, then paints a [`Buffer`] and returns interaction metadata. It
//! performs no I/O and never executes producer supplied data.

mod accessibility;
mod codec;
mod layout;
mod paint;
mod text;

use std::collections::{BTreeMap, BTreeSet};

use codypendent_protocol::remote_ui::{
    primitives, UiActionBinding, UiCapabilitySelection, UiDocument, UiHardLimits, UiNodeId,
    UiSemanticRole,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::Value;

use crate::Theme;

pub use accessibility::project_accessibility;
pub use text::{cell_width, sanitize_terminal_text, truncate_cells, wrap_cells};

/// Terminal presentation features used for capability and fallback resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalUiCapabilities {
    /// Primitive names rendered natively. `*` accepts all producer primitives.
    pub primitives: BTreeSet<String>,
    /// Additive semantic feature names (for node `requires`).
    pub features: BTreeSet<String>,
    /// In-terminal image protocols such as `kitty`, `iterm2`, or `sixel`.
    pub image_protocols: BTreeSet<String>,
    pub unicode: bool,
    pub mouse: bool,
    pub screen_reader: bool,
    /// Terminal colour bit depth: 1 (monochrome), 4, 8, or 24.
    pub color_depth: u32,
}

impl TerminalUiCapabilities {
    /// A fully featured native terminal renderer capability set.
    #[must_use]
    pub fn native() -> Self {
        Self {
            primitives: ALL_NATIVE_PRIMITIVES
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            features: [
                "terminal",
                "keyboard",
                "focus",
                "forms",
                "clipboard",
                "unicode",
                "mouse",
                "virtualization",
                "markdown",
                "syntaxHighlighting",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            image_protocols: BTreeSet::new(),
            unicode: true,
            mouse: true,
            screen_reader: false,
            color_depth: 24,
        }
    }

    /// Construct terminal capabilities from the protocol negotiation result.
    #[must_use]
    pub fn from_selection(selection: &UiCapabilitySelection) -> Self {
        let mut features = selection
            .capabilities
            .iter()
            .map(|feature| feature.as_str().to_owned())
            .collect::<BTreeSet<_>>();
        if selection.unicode {
            features.insert("unicode".to_owned());
        }
        if selection.mouse {
            features.insert("mouse".to_owned());
        }
        if selection.screen_reader {
            features.insert("screenReader".to_owned());
        }
        Self {
            primitives: selection
                .primitives
                .iter()
                .map(|primitive| primitive.as_str().to_owned())
                .collect(),
            features,
            image_protocols: selection.image_protocols.iter().cloned().collect(),
            unicode: selection.unicode,
            mouse: selection.mouse,
            screen_reader: selection.screen_reader,
            color_depth: u32::from(selection.color_depth),
        }
    }

    #[must_use]
    pub fn supports_primitive(&self, name: &str) -> bool {
        self.primitives.contains("*") || self.primitives.contains(name)
    }

    #[must_use]
    pub fn supports_feature(&self, name: &str) -> bool {
        self.features.contains(name)
    }
}

impl Default for TerminalUiCapabilities {
    fn default() -> Self {
        Self::native()
    }
}

/// Host-owned, mutable view state consumed immutably during a frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemoteUiViewState {
    pub focused_node: Option<UiNodeId>,
    /// Local controlled values, keyed by stable node id. They override wire
    /// values while an edit is in progress and never mutate the document.
    pub input_values: BTreeMap<UiNodeId, Value>,
    /// Vertical line offsets for scroll areas, virtual lists, code, and logs.
    pub scroll_offsets: BTreeMap<UiNodeId, u32>,
    /// Optional expanded state overrides for trees, menus, and disclosures.
    pub expanded: BTreeMap<UiNodeId, bool>,
    /// Cursor/selection indices for selectable collections.
    pub selected_indices: BTreeMap<UiNodeId, usize>,
}

/// Bounded renderer configuration. Protocol validation remains the first line
/// of defence; these limits also keep direct/test callers safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteUiRenderOptions {
    pub limits: UiHardLimits,
    pub narrow_breakpoint: u16,
    pub show_focus_hint: bool,
    pub show_diagnostics_inline: bool,
    /// Maximum off-screen rows materialised by one virtualized collection.
    pub virtual_overscan: u16,
}

impl Default for RemoteUiRenderOptions {
    fn default() -> Self {
        Self {
            limits: UiHardLimits::default(),
            narrow_breakpoint: 48,
            show_focus_hint: true,
            show_diagnostics_inline: true,
            virtual_overscan: 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderDiagnostic {
    pub severity: DiagnosticSeverity,
    pub code: &'static str,
    pub node_id: Option<UiNodeId>,
    pub message: String,
}

/// Keyboard gestures are semantic; the reducer maps concrete crossterm input
/// onto one of these without teaching widgets about commands or I/O.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum RemoteKey {
    Enter,
    Space,
    Escape,
    Tab,
    ShiftTab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Backspace,
    Delete,
    Character,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeyboardAction {
    pub key: RemoteKey,
    pub binding: UiActionBinding,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FocusDescriptor {
    pub node_id: UiNodeId,
    pub area: Rect,
    pub order: i32,
    pub role: UiSemanticRole,
    pub label: String,
    pub keyboard_hint: Option<String>,
    pub disabled: bool,
    pub keyboard_actions: Vec<KeyboardAction>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HitRegion {
    pub node_id: UiNodeId,
    pub area: Rect,
    pub binding: UiActionBinding,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormFieldDescriptor {
    pub node_id: UiNodeId,
    pub name: String,
    pub input_type: String,
    pub value: Value,
    pub required: bool,
    pub read_only: bool,
    pub disabled: bool,
    pub validation_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibilityNode {
    pub node_id: Option<UiNodeId>,
    pub role: UiSemanticRole,
    pub label: String,
    pub description: Option<String>,
    pub keyboard_hint: Option<String>,
    pub live_region: Option<String>,
    pub heading_level: Option<u8>,
    pub disabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccessibilityProjection {
    pub plain_text: String,
    pub nodes: Vec<AccessibilityNode>,
}

/// Everything needed by reducer/input integration after a pure render pass.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemoteUiRenderOutput {
    pub focus_order: Vec<FocusDescriptor>,
    pub hit_regions: Vec<HitRegion>,
    pub form_fields: Vec<FormFieldDescriptor>,
    pub accessibility: AccessibilityProjection,
    pub diagnostics: Vec<RenderDiagnostic>,
    /// Stable ids actually painted this frame (useful for virtualized focus).
    pub visible_nodes: BTreeSet<UiNodeId>,
}

/// Paint one document. Invalid snapshots become a contained diagnostic panel;
/// they cannot panic the host or partially render untrusted content.
pub fn render_remote_ui(
    buffer: &mut Buffer,
    area: Rect,
    document: &UiDocument,
    theme: &Theme,
    capabilities: &TerminalUiCapabilities,
    state: &RemoteUiViewState,
    options: RemoteUiRenderOptions,
) -> RemoteUiRenderOutput {
    paint::render(buffer, area, document, theme, capabilities, state, options)
}

/// Native primitive catalogue. Namespaced/custom primitives can still be
/// represented via their fallback or generic semantic card.
pub const ALL_NATIVE_PRIMITIVES: &[&str] = &[
    primitives::BOX,
    primitives::STACK,
    primitives::ROW,
    primitives::GRID,
    primitives::SPLIT,
    primitives::SPACER,
    primitives::SCROLL_AREA,
    primitives::VIRTUAL_LIST,
    primitives::PORTAL,
    primitives::TEXT,
    primitives::MARKDOWN,
    primitives::CODE,
    primitives::DIFF,
    primitives::IMAGE,
    primitives::AUDIO,
    primitives::JSON_TREE,
    primitives::LOG_VIEWER,
    primitives::LIST,
    primitives::TABLE,
    primitives::TREE,
    primitives::KEY_VALUE,
    primitives::TIMELINE,
    primitives::GRAPH,
    primitives::CHART,
    primitives::SPARKLINE,
    primitives::BADGE,
    primitives::PROGRESS,
    primitives::SPINNER,
    primitives::ALERT,
    primitives::TOAST,
    primitives::EMPTY_STATE,
    primitives::ERROR_BOUNDARY,
    primitives::TABS,
    primitives::BREADCRUMB,
    primitives::MENU,
    primitives::COMMAND_LIST,
    primitives::PAGINATION,
    primitives::LINK,
    primitives::TEXT_INPUT,
    primitives::TEXT_AREA,
    primitives::SELECT,
    primitives::MULTI_SELECT,
    primitives::CHECKBOX,
    primitives::RADIO,
    primitives::FORM,
    primitives::BUTTON,
    primitives::ACTION_MENU,
    primitives::TOOLBAR,
    primitives::CONTEXT_MENU,
    "Details",
    "RunCard",
    "AgentCard",
    "ToolCard",
    "ModelCard",
    "ApprovalCard",
    "MemoryCard",
    "ArtifactCard",
    "WorkflowCard",
    "WorkflowNode",
    "ProviderCard",
    "SkillCard",
    "DocumentCard",
    "PatchCard",
    "TestReport",
    "PermissionDiff",
    "TraceView",
    "CostView",
];

pub(crate) enum ResolvedNode<'a> {
    Node(&'a codypendent_protocol::remote_ui::UiNode),
    Plain(String),
}

pub(crate) fn resolve_node<'a>(
    node: &'a codypendent_protocol::remote_ui::UiNode,
    capabilities: &TerminalUiCapabilities,
) -> ResolvedNode<'a> {
    let requirements_met = node.requires.iter().all(|requirement| {
        requirement.optional || capabilities.supports_feature(requirement.feature.as_str())
    });
    let primitive_met = node.node_type.as_ref().is_none_or(|primitive| {
        capabilities.supports_primitive(primitive.as_str())
            || is_domain_primitive(primitive.as_str())
    });
    if requirements_met && primitive_met {
        return ResolvedNode::Node(node);
    }
    if let Some(fallback) = node.fallback.as_deref() {
        return resolve_node(fallback, capabilities);
    }
    let plain = node
        .props
        .accessibility
        .as_ref()
        .and_then(|accessibility| {
            accessibility
                .text_fallback
                .as_ref()
                .or(accessibility.label.as_ref())
        })
        .cloned()
        .or_else(|| {
            node.props
                .content
                .as_ref()
                .and_then(|content| content.alternate_text.as_ref().or(content.text.as_ref()))
                .cloned()
        })
        .unwrap_or_else(|| {
            format!(
                "[unsupported {}]",
                node.node_type
                    .as_ref()
                    .map_or("component", |primitive| primitive.as_str())
            )
        });
    ResolvedNode::Plain(plain)
}

pub(crate) fn is_domain_primitive(name: &str) -> bool {
    const DOMAIN_NAMES: &[&str] = &[
        "RunCard",
        "AgentCard",
        "ToolCard",
        "ModelCard",
        "ApprovalCard",
        "MemoryCard",
        "ArtifactCard",
        "WorkflowCard",
        "ProviderCard",
        "SkillCard",
        "DocumentCard",
        "PatchCard",
        "WorkflowNode",
        "TestReport",
        "PermissionDiff",
        "TraceView",
        "CostView",
    ];
    DOMAIN_NAMES.contains(&name)
        || name
            .rsplit(['/', ':', '.'])
            .next()
            .is_some_and(|suffix| DOMAIN_NAMES.contains(&suffix) || suffix.ends_with("Card"))
}

#[cfg(test)]
mod tests {
    use codypendent_protocol::remote_ui::{UiCapabilitySelection, UiHardLimits, UiProtocolVersion};

    use super::*;

    #[test]
    fn negotiated_colour_depth_is_a_bit_depth() {
        let selection = UiCapabilitySelection {
            protocol_version: UiProtocolVersion::V1,
            primitives: Vec::new(),
            capabilities: Vec::new(),
            contribution_points: Vec::new(),
            image_protocols: vec!["kitty".to_owned()],
            color_depth: 24,
            unicode: true,
            mouse: false,
            screen_reader: false,
            viewport: None,
            limits: UiHardLimits::default(),
        };
        let capabilities = TerminalUiCapabilities::from_selection(&selection);
        assert_eq!(capabilities.color_depth, 24);
        assert_eq!(TerminalUiCapabilities::native().color_depth, 24);
        assert!(capabilities.image_protocols.contains("kitty"));
    }
}
