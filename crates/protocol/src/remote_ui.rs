//! Universal, renderer-independent remote UI wire contract.
//!
//! A producer (first-party UI or a sandboxed extension) sends semantic trees and
//! incremental patches. A trusted host validates them and renders them with
//! Ratatui, a VS Code webview, or another client. Evolving concepts deliberately
//! use transparent string newtypes instead of closed Rust enums: an older host
//! can preserve an unknown primitive, event, contribution point, or patch
//! operation and apply the supplied fallback without rejecting the frame.

use std::collections::{BTreeMap, HashSet};

use serde::de::Deserializer;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

use crate::capabilities::ClientCapabilities as DaemonClientCapabilities;

macro_rules! string_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn is_empty(&self) -> bool {
                self.0.trim().is_empty()
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

string_newtype!(/// Stable identifier of a remote UI document.
    UiDocumentId);
string_newtype!(/// Stable identifier of a node within one document revision stream.
    UiNodeId);
string_newtype!(/// Stable identifier of an action declared by a component.
    UiActionId);
string_newtype!(/// Idempotency identifier of a UI event or action invocation.
    UiEventId);
string_newtype!(/// Open-ended semantic node category (`text`, `element`, or a future kind).
    UiNodeKind);
string_newtype!(/// Open-ended primitive or namespaced custom component name.
    UiPrimitive);
string_newtype!(/// Open-ended remote UI capability name.
    UiCapability);
string_newtype!(/// Rendering client kind (`terminal`, `web`, `vscode`, desktop, test, ...).
    UiClientKind);
string_newtype!(/// Media presentation capability (`image`, `audio`, `video`, ...).
    UiMediaCapability);
string_newtype!(/// Color model (`monochrome`, `ansi16`, `ansi256`, `trueColor`, ...).
    UiColorDepth);
string_newtype!(/// Open-ended patch operation name.
    UiPatchOperation);
string_newtype!(/// Open-ended semantic event name.
    UiEventType);
string_newtype!(/// Open-ended semantic role name.
    UiSemanticRole);
string_newtype!(/// Open-ended contribution point name.
    UiContributionPoint);
string_newtype!(/// Named, host-owned contribution slot.
    UiSlotId);
string_newtype!(/// Stable identifier of a contributed surface.
    UiContributionId);
string_newtype!(/// Package or built-in producer identifier.
    UiExtensionId);

/// Version of the remote UI contract, negotiated independently of the daemon
/// envelope protocol so renderers can evolve at their own pace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl UiProtocolVersion {
    pub const V1: Self = Self { major: 1, minor: 0 };
}

/// Monotonically increasing document revision. Snapshots may begin at any
/// revision; every patch batch advances exactly one revision.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct UiRevision(pub u64);

/// Built-in primitive names. They are constants rather than enum variants so
/// namespaced plugin primitives remain representable by every protocol build.
pub mod primitives {
    // Layout.
    pub const BOX: &str = "Box";
    pub const STACK: &str = "Stack";
    pub const ROW: &str = "Row";
    pub const GRID: &str = "Grid";
    pub const SPLIT: &str = "Split";
    pub const SPACER: &str = "Spacer";
    pub const SCROLL_AREA: &str = "ScrollArea";
    pub const VIRTUAL_LIST: &str = "VirtualList";
    pub const PORTAL: &str = "Portal";

    // Content.
    pub const TEXT: &str = "Text";
    pub const MARKDOWN: &str = "Markdown";
    pub const CODE: &str = "Code";
    pub const DIFF: &str = "Diff";
    pub const IMAGE: &str = "Image";
    pub const AUDIO: &str = "Audio";
    pub const JSON_TREE: &str = "JsonTree";
    pub const LOG_VIEWER: &str = "LogViewer";

    // Data.
    pub const LIST: &str = "List";
    pub const TABLE: &str = "Table";
    pub const TREE: &str = "Tree";
    pub const KEY_VALUE: &str = "KeyValue";
    pub const TIMELINE: &str = "Timeline";
    pub const GRAPH: &str = "Graph";
    pub const CHART: &str = "Chart";
    pub const SPARKLINE: &str = "Sparkline";

    // Feedback.
    pub const BADGE: &str = "Badge";
    pub const PROGRESS: &str = "Progress";
    pub const SPINNER: &str = "Spinner";
    pub const ALERT: &str = "Alert";
    pub const TOAST: &str = "Toast";
    pub const EMPTY_STATE: &str = "EmptyState";
    pub const ERROR_BOUNDARY: &str = "ErrorBoundary";

    // Navigation.
    pub const TABS: &str = "Tabs";
    pub const BREADCRUMB: &str = "Breadcrumb";
    pub const MENU: &str = "Menu";
    pub const COMMAND_LIST: &str = "CommandList";
    pub const PAGINATION: &str = "Pagination";
    pub const LINK: &str = "Link";

    // Input.
    pub const TEXT_INPUT: &str = "TextInput";
    pub const TEXT_AREA: &str = "TextArea";
    pub const SELECT: &str = "Select";
    pub const MULTI_SELECT: &str = "MultiSelect";
    pub const CHECKBOX: &str = "Checkbox";
    pub const RADIO: &str = "Radio";
    pub const FORM: &str = "Form";

    // Actions.
    pub const BUTTON: &str = "Button";
    pub const ACTION_MENU: &str = "ActionMenu";
    pub const TOOLBAR: &str = "Toolbar";
    pub const CONTEXT_MENU: &str = "ContextMenu";
}

/// Well-known node kinds.
pub mod node_kinds {
    pub const TEXT: &str = "text";
    pub const ELEMENT: &str = "element";
}

/// Well-known incremental patch operations.
pub mod patch_operations {
    pub const REPLACE_ROOT: &str = "replaceRoot";
    pub const INSERT: &str = "insert";
    pub const REMOVE: &str = "remove";
    pub const REPLACE: &str = "replace";
    pub const UPDATE_PROPS: &str = "updateProps";
    pub const SET_TEXT: &str = "setText";
    pub const MOVE: &str = "move";
}

/// Renderer-neutral dimension. `unit` is normally `cells`, `percent`, `fr`,
/// or `auto`, but is intentionally open-ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiDimension {
    pub value: f64,
    pub unit: String,
}

/// Four-sided spacing in terminal cells or renderer logical units.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiEdges {
    #[serde(default)]
    pub top: f64,
    #[serde(default)]
    pub right: f64,
    #[serde(default)]
    pub bottom: f64,
    #[serde(default)]
    pub left: f64,
}

/// Semantic layout hints. Hosts remain authoritative for clipping, cell width,
/// responsive collapse, and terminal-safe placement.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiLayout {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub align: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub justify: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrap: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_gap: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_gap: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub padding: Option<UiEdges>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin: Option<UiEdges>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<UiDimension>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<UiDimension>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_width: Option<UiDimension>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_width: Option<UiDimension>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_height: Option<UiDimension>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_height: Option<UiDimension>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grow: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shrink: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub basis: Option<UiDimension>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<UiDimension>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<UiDimension>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overflow: Option<String>,
}

/// Semantic styling references theme tokens; producers never emit ANSI escapes,
/// CSS, or raw terminal control sequences.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiStyle {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub emphasis: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncate: Option<String>,
}

/// Accessibility metadata required to give graphical and terminal clients the
/// same semantic representation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiAccessibility {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<UiSemanticRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading_level: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_order: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyboard_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_fallback: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

/// One rich-text span within a content primitive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiTextSpan {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<UiStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accessibility_label: Option<String>,
}

/// A resource is referenced, never embedded as unbounded bytes in the UI tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiResourceReference {
    pub uri: String,
    pub media_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_length: Option<u64>,
}

/// Shared content properties for Text, Markdown, Code, Diff, Image, Audio,
/// JsonTree, LogViewer, and custom content primitives.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<UiTextSpan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<UiResourceReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternate_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_wrap: Option<String>,
}

/// Column metadata used by tables and other structured-data primitives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiDataColumn {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<UiDimension>,
    #[serde(default)]
    pub sortable: bool,
}

/// Renderer-neutral structured data. Schemas and items remain JSON so a new
/// chart, graph, or plugin-specific renderer does not require a protocol bump.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<UiDataColumn>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub selected_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

/// Progress, status, and tone shared by feedback primitives.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiFeedback {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indeterminate: Option<bool>,
}

/// Navigation state shared by links, tabs, menus, trees, and command lists.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiNavigation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expanded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
}

/// One selectable value for input primitives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiInputOption {
    pub id: String,
    pub label: String,
    pub value: Value,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Input state. Secret values must never be placed in a remote tree; secret
/// entry is a host-owned contribution point.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_message: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<UiInputOption>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub disabled: bool,
}

/// A semantic event-to-command binding. The host validates `action_id` and
/// capabilities again when an invocation arrives; this declaration is never
/// authority to perform I/O by itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiActionBinding {
    pub event: UiEventType,
    pub action_id: UiActionId,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<UiCapability>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<String>,
}

/// Presentation feature required by a node. Unknown features remain
/// representable and are resolved through the node fallback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiRequirement {
    pub feature: UiCapability,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

/// Typed common properties plus flattened extension properties. This serializes
/// as the SDK's ordinary `props` JSON object while allowing Rust renderers to
/// consume the stable semantic subset without stringly typed field access.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiNodeProps {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<UiSemanticRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<UiLayout>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<UiStyle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accessibility: Option<UiAccessibility>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<UiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_data: Option<UiData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback: Option<UiFeedback>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigation: Option<UiNavigation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<UiInput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub event_bindings: Vec<UiActionBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, Value>,
    #[serde(flatten)]
    pub extension: BTreeMap<String, Value>,
}

/// A generic semantic node shared with TypeScript. `kind` is normally `text`
/// or `element`; `node_type` is the element primitive (serialized as `type`).
/// Unknown kinds and primitives survive deserialization and use `fallback`.
#[derive(Debug, Clone, PartialEq)]
pub struct UiNode {
    pub kind: UiNodeKind,
    pub id: Option<UiNodeId>,
    pub node_type: Option<UiPrimitive>,
    pub text: Option<String>,
    pub props: UiNodeProps,
    pub children: Vec<UiNode>,
    pub fallback: Option<Box<UiNode>>,
    pub requires: Vec<UiRequirement>,
}

impl UiNode {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            kind: UiNodeKind::from(node_kinds::TEXT),
            id: None,
            node_type: None,
            text: Some(text.into()),
            props: UiNodeProps::default(),
            children: Vec::new(),
            fallback: None,
            requires: Vec::new(),
        }
    }

    pub fn element(id: impl Into<UiNodeId>, primitive: impl Into<UiPrimitive>) -> Self {
        Self {
            kind: UiNodeKind::from(node_kinds::ELEMENT),
            id: Some(id.into()),
            node_type: Some(primitive.into()),
            text: None,
            props: UiNodeProps::default(),
            children: Vec::new(),
            fallback: None,
            requires: Vec::new(),
        }
    }
}

impl Serialize for UiNode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let is_text = self.kind.as_str() == node_kinds::TEXT;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("kind", &self.kind)?;
        if let Some(id) = &self.id {
            map.serialize_entry("id", id)?;
        }
        if is_text {
            if let Some(text) = &self.text {
                map.serialize_entry("text", text)?;
            }
        } else {
            if let Some(node_type) = &self.node_type {
                map.serialize_entry("type", node_type)?;
            }
            // Element props and children are required by the TypeScript wire
            // contract, including their empty object/array forms.
            map.serialize_entry("props", &self.props)?;
            map.serialize_entry("children", &self.children)?;
            if let Some(fallback) = &self.fallback {
                map.serialize_entry("fallback", fallback)?;
            }
            if !self.requires.is_empty() {
                map.serialize_entry("requires", &self.requires)?;
            }
            // Some renderer adapters use top-level text on a Text primitive;
            // preserve it as an additive field even though SDK text nodes are
            // the canonical representation.
            if let Some(text) = &self.text {
                map.serialize_entry("text", text)?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for UiNode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WireNode {
            kind: UiNodeKind,
            #[serde(default)]
            id: Option<UiNodeId>,
            #[serde(default, rename = "type")]
            node_type: Option<UiPrimitive>,
            #[serde(default)]
            text: Option<String>,
            #[serde(default)]
            props: UiNodeProps,
            #[serde(default)]
            children: Vec<UiNode>,
            #[serde(default)]
            fallback: Option<Box<UiNode>>,
            #[serde(default)]
            requires: Vec<UiRequirement>,
        }

        let wire = WireNode::deserialize(deserializer)?;
        Ok(Self {
            kind: wire.kind,
            id: wire.id,
            node_type: wire.node_type,
            text: wire.text,
            props: wire.props,
            children: wire.children,
            fallback: wire.fallback,
            requires: wire.requires,
        })
    }
}

/// A complete semantic UI tree at one revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiDocument {
    pub protocol_version: UiProtocolVersion,
    pub document_id: UiDocumentId,
    pub revision: UiRevision,
    pub root: UiNode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<UiCapabilities>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<UiCompatibility>,
}

/// Full-state baseline used on mount, reconnect, patch rejection, and renderer
/// recovery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSnapshot {
    pub document: UiDocument,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Properties changed by `updateProps`. `set` is merged and `unset` deletes
/// top-level keys. Typed properties ride in `set` using their camelCase names,
/// so future properties remain lossless.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiPropsPatch {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unset: Vec<String>,
}

/// One incremental tree mutation. Known operation requirements are validated;
/// unknown operations remain representable and must trigger compatibility
/// fallback or snapshot recovery in a host that cannot apply them.
#[derive(Debug, Clone, PartialEq)]
pub struct UiPatch {
    pub op: UiPatchOperation,
    pub node_id: Option<UiNodeId>,
    pub parent_id: Option<UiNodeId>,
    pub index: Option<u32>,
    pub node: Option<UiNode>,
    pub props: Option<UiPropsPatch>,
    pub text: Option<String>,
    pub payload: Value,
}

impl Serialize for UiPatch {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("op", &self.op)?;
        if let Some(node_id) = &self.node_id {
            map.serialize_entry("nodeId", node_id)?;
        }
        if let Some(parent_id) = &self.parent_id {
            map.serialize_entry("parentId", parent_id)?;
        }
        if let Some(index) = self.index {
            map.serialize_entry("index", &index)?;
        }
        if let Some(node) = &self.node {
            map.serialize_entry("node", node)?;
        }
        if let Some(props) = &self.props {
            // Canonical SDK updateProps fields are top-level, not nested below
            // a Rust implementation detail.
            map.serialize_entry("set", &props.set)?;
            if !props.unset.is_empty() {
                map.serialize_entry("unset", &props.unset)?;
            }
        }
        if let Some(text) = &self.text {
            map.serialize_entry("text", text)?;
        }
        if !self.payload.is_null() {
            map.serialize_entry("payload", &self.payload)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for UiPatch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WirePatch {
            op: UiPatchOperation,
            #[serde(default, alias = "targetId")]
            node_id: Option<UiNodeId>,
            #[serde(default)]
            parent_id: Option<UiNodeId>,
            #[serde(default)]
            index: Option<u32>,
            #[serde(default)]
            node: Option<UiNode>,
            #[serde(default)]
            set: Option<BTreeMap<String, Value>>,
            #[serde(default)]
            unset: Vec<String>,
            // Accept the earlier nested Rust draft without emitting it.
            #[serde(default)]
            props: Option<UiPropsPatch>,
            #[serde(default)]
            text: Option<String>,
            #[serde(default)]
            payload: Value,
        }

        let wire = WirePatch::deserialize(deserializer)?;
        let props = wire.props.or_else(|| {
            if wire.set.is_some()
                || !wire.unset.is_empty()
                || wire.op.as_str() == patch_operations::UPDATE_PROPS
            {
                Some(UiPropsPatch {
                    set: wire.set.unwrap_or_default(),
                    unset: wire.unset,
                })
            } else {
                None
            }
        });
        Ok(Self {
            op: wire.op,
            node_id: wire.node_id,
            parent_id: wire.parent_id,
            index: wire.index,
            node: wire.node,
            props,
            text: wire.text,
            payload: wire.payload,
        })
    }
}

/// Atomic, ordered set of mutations from `base_revision` to the immediately
/// following `revision`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiPatchBatch {
    pub protocol_version: UiProtocolVersion,
    pub document_id: UiDocumentId,
    pub base_revision: UiRevision,
    pub revision: UiRevision,
    pub patches: Vec<UiPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_at: Option<String>,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub atomic: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<UiFallback>,
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

/// Keyboard/pointer modifiers accompanying a semantic event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiEventModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub meta: bool,
}

/// A host-normalized event emitted by a semantic node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiEvent {
    pub protocol_version: UiProtocolVersion,
    pub event_id: UiEventId,
    pub document_id: UiDocumentId,
    pub revision: UiRevision,
    pub target_id: UiNodeId,
    #[serde(rename = "type")]
    pub event_type: UiEventType,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modifiers: Option<UiEventModifiers>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// Opaque one-shot host authority. Renderers never mint this value; the
    /// broker adds it only to the owner-bound event forwarded to a worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction_token: Option<String>,
}

/// Validated command intent produced from an action binding. The host remains
/// responsible for permission checks and rejects stale `revision` values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiActionInvocation {
    pub invocation_id: UiEventId,
    pub document_id: UiDocumentId,
    pub revision: UiRevision,
    pub source_node_id: UiNodeId,
    pub action_id: UiActionId,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub payload: Value,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub form_data: BTreeMap<String, Value>,
    /// Echo of the broker-minted authority from the active event context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction_token: Option<String>,
    /// Event class to which the one-shot authority was bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction_event_type: Option<UiEventType>,
}

/// A component's mediated projection subscription. `kind` is open-ended
/// (`session`, `run`, `artifact`, `command`, ...); the daemon authorizes each
/// request against the plugin manifest before returning data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiProjectionSubscription {
    pub subscription_id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, Value>,
}

/// Owner-scoped teardown for one mediated projection subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiProjectionUnsubscription {
    pub subscription_id: String,
}

/// Latest-wins data delivered for one mediated subscription. The worker never
/// receives a raw path, database handle, socket, or secret through this value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiProjectionUpdate {
    pub subscription_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<UiRevision>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub value: Value,
}

/// Canonical daemon-to-SDK value returned for a `session` projection.
/// Keeping this DTO in the wire crate prevents daemon and TypeScript hooks
/// from independently inventing field names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSessionProjection {
    pub id: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// Canonical daemon-to-SDK value returned for a `run` projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiRunProjection {
    pub id: String,
    pub session_id: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Canonical daemon-to-SDK IDE context value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiContextProjection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<Value>,
    #[serde(default)]
    pub open_files: Vec<String>,
    #[serde(default)]
    pub dirty_buffers: Vec<Value>,
    #[serde(default)]
    pub diagnostics_revision: u64,
}

/// Canonical daemon-to-SDK workflow value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiWorkflowProjection {
    pub workflow_run_id: String,
    pub phase: String,
    pub nodes: Vec<UiWorkflowNodeProjection>,
}

/// Canonical daemon-to-SDK blackboard value: one workflow run's board,
/// projected read-only.
///
/// The items are the same [`BlackboardItemView`](crate::BlackboardItemView) the
/// `ReadBlackboard` socket command replies with, so a field added to the board
/// reaches a Remote UI producer without another projection change. A board is
/// part of its workflow run's observable state: the resource id is a workflow
/// run id, authorized by the same ownership join and the same `workflow-read`
/// capability as [`UiWorkflowProjection`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiBlackboardProjection {
    pub workflow_run_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<crate::blackboard::BlackboardItemView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiWorkflowNodeProjection {
    pub workflow_run_id: String,
    pub node_id: String,
    pub state: String,
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Canonical daemon-to-SDK value returned for an `artifact` projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiArtifactProjection {
    pub id: String,
    pub media_type: String,
    pub revision: u64,
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Byte window described inside an artifact projection value. `page` is
/// present only when the caller selected page/pageSize addressing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiArtifactProjectionRange {
    pub offset: u64,
    pub length: u64,
    pub total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u64>,
}

/// Idempotent cancellation of an in-flight mediated command invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiActionCancellation {
    pub invocation_id: UiEventId,
}

/// Host instruction to unmount one document at its current revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiDispose {
    pub document_id: UiDocumentId,
    pub revision: UiRevision,
}

/// Runtime request for a fresh snapshot after a missing or rejected patch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiResyncRequest {
    pub document_id: UiDocumentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_revision: Option<UiRevision>,
}

/// Development-runtime notification that compiled modules changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiHotReload {
    pub generation: u64,
    pub changed_modules: Vec<String>,
}

/// Plain-text or simpler semantic fallback for unsupported client features.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiFallback {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plain_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replacement: Option<Box<UiNode>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behavior: Option<String>,
}

/// Compatibility requirements attached to a complete document.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiCompatibility {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_protocol: Option<UiProtocolVersion>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub required_primitives: Vec<UiPrimitive>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub required_capabilities: Vec<UiCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<UiFallback>,
}

/// Semantic theme tokens. Token values are JSON scalars or small structured
/// values so future renderers can add gradients or terminal-specific palettes
/// without changing the contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiTheme {
    pub id: String,
    pub name: String,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_scheme: Option<String>,
    #[serde(default)]
    pub high_contrast: bool,
    #[serde(default)]
    pub reduced_motion: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tokens: BTreeMap<String, Value>,
}

/// Where and how one package-provided surface is mounted. `point` and `slot`
/// are strings so new host surfaces do not require a protocol release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiContributionRegistration {
    pub id: UiContributionId,
    pub extension_id: UiExtensionId,
    pub point: UiContributionPoint,
    pub slot: UiSlotId,
    pub document_id: UiDocumentId,
    #[serde(default)]
    pub priority: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<UiCapability>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

/// A host-owned mount point. `trusted_only` protects approval chrome, secret
/// entry, policy state, and other surfaces that third-party trees cannot own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSlotDefinition {
    pub id: UiSlotId,
    pub point: UiContributionPoint,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepts: Vec<String>,
    #[serde(default)]
    pub trusted_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_contributions: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<UiFallback>,
}

/// Bounded viewport advertised by a rendering client.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiViewport {
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixel_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixel_height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<f64>,
}

/// Sustained worker-to-host message rate, in messages per second.
///
/// A worker's self-imposed budget must never exceed the host's, or a
/// legitimate burst is a *kill* (the host drops the worker on
/// `MessageRateExceeded`) instead of a recoverable local error the worker can
/// coalesce around. These two constants are the single source for both sides
/// and are mirrored in `sdk/ui/src/protocol.ts` as
/// `UI_WORKER_MESSAGE_RATE_PER_SECOND` / `UI_WORKER_MESSAGE_BURST`.
pub const UI_WORKER_MESSAGE_RATE_PER_SECOND: u32 = 240;

/// Burst allowance above [`UI_WORKER_MESSAGE_RATE_PER_SECOND`] for the
/// snapshot-then-patch storm a surface emits when it first mounts.
pub const UI_WORKER_MESSAGE_BURST: u32 = 120;

/// Hard resource ceilings applied before a tree or patch reaches a renderer.
/// Defaults are deliberately conservative enough for a full-screen app while
/// bounding CPU, allocation, and pathological recursive/value input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiHardLimits {
    pub max_tree_depth: u16,
    pub max_nodes: u32,
    pub max_text_bytes: u64,
    pub max_properties_per_node: u32,
    pub max_actions_per_node: u16,
    pub max_json_depth: u16,
    pub max_json_values: u32,
    pub max_patches_per_batch: u32,
    pub max_patch_bytes: u64,
    pub max_contributions: u32,
}

impl Default for UiHardLimits {
    fn default() -> Self {
        Self {
            max_tree_depth: 64,
            max_nodes: 20_000,
            max_text_bytes: 2 * 1024 * 1024,
            max_properties_per_node: 256,
            max_actions_per_node: 64,
            max_json_depth: 32,
            max_json_values: 100_000,
            max_patches_per_batch: 2_000,
            max_patch_bytes: 4 * 1024 * 1024,
            max_contributions: 1_000,
        }
    }
}

impl UiHardLimits {
    /// The lower ceiling for each independently bounded resource.
    pub fn intersection(self, other: Self) -> Self {
        Self {
            max_tree_depth: self.max_tree_depth.min(other.max_tree_depth),
            max_nodes: self.max_nodes.min(other.max_nodes),
            max_text_bytes: self.max_text_bytes.min(other.max_text_bytes),
            max_properties_per_node: self
                .max_properties_per_node
                .min(other.max_properties_per_node),
            max_actions_per_node: self.max_actions_per_node.min(other.max_actions_per_node),
            max_json_depth: self.max_json_depth.min(other.max_json_depth),
            max_json_values: self.max_json_values.min(other.max_json_values),
            max_patches_per_batch: self.max_patches_per_batch.min(other.max_patches_per_batch),
            max_patch_bytes: self.max_patch_bytes.min(other.max_patch_bytes),
            max_contributions: self.max_contributions.min(other.max_contributions),
        }
    }
}

/// Client or host capability advertisement. All named sets are open-ended and
/// negotiated by intersection. A missing feature means the plain-text baseline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiCapabilities {
    pub client: UiClientKind,
    pub protocol_versions: Vec<UiProtocolVersion>,
    pub daemon: DaemonClientCapabilities,
    #[serde(
        serialize_with = "serialize_primitives",
        deserialize_with = "deserialize_primitives"
    )]
    pub primitives: Vec<UiPrimitive>,
    #[serde(default)]
    pub media: Vec<UiMediaCapability>,
    pub color_depth: UiColorDepth,
    pub keyboard: bool,
    pub screen_reader: bool,
    pub reduced_motion: bool,
    pub clipboard: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terminal_graphics: Vec<String>,
    pub viewport: UiViewport,
    /// Additive host command capabilities, independent of presentation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<UiCapability>,
    /// Additive plugin mount points understood by the host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contribution_points: Vec<UiContributionPoint>,
    #[serde(default, skip_serializing_if = "is_default_limits")]
    pub limits: UiHardLimits,
}

fn is_default_limits(limits: &UiHardLimits) -> bool {
    limits == &UiHardLimits::default()
}

fn serialize_primitives<S>(primitives: &[UiPrimitive], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if primitives.len() == 1 && primitives[0].as_str() == "*" {
        serializer.serialize_str("*")
    } else {
        primitives.serialize(serializer)
    }
}

fn deserialize_primitives<'de, D>(deserializer: D) -> Result<Vec<UiPrimitive>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum PrimitiveWire {
        All(String),
        List(Vec<UiPrimitive>),
    }

    match PrimitiveWire::deserialize(deserializer)? {
        PrimitiveWire::All(value) if value == "*" => Ok(vec![UiPrimitive::from(value)]),
        PrimitiveWire::All(value) => Err(serde::de::Error::custom(format!(
            "primitive wildcard must be `*`, got {value:?}"
        ))),
        PrimitiveWire::List(values) => Ok(values),
    }
}

/// Result of intersecting a producer/host offer with a rendering client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiCapabilitySelection {
    pub protocol_version: UiProtocolVersion,
    pub primitives: Vec<UiPrimitive>,
    pub capabilities: Vec<UiCapability>,
    pub contribution_points: Vec<UiContributionPoint>,
    pub image_protocols: Vec<String>,
    pub color_depth: u16,
    pub unicode: bool,
    pub mouse: bool,
    pub screen_reader: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<UiViewport>,
    pub limits: UiHardLimits,
}

/// Descriptive alias used when the rich capabilities object is specifically a
/// connected client's advertisement.
pub type UiClientCapabilities = UiCapabilities;

impl UiCapabilities {
    /// Negotiate the highest mutually supported version of the same major, and
    /// intersect feature sets and limits. `self` is the consumer/client and
    /// `offered` is the producer/host.
    pub fn negotiate(
        &self,
        offered: &UiCapabilities,
    ) -> Result<UiCapabilitySelection, UiValidationError> {
        self.validate()?;
        offered.validate()?;

        let protocol_version = self
            .protocol_versions
            .iter()
            .flat_map(|client| {
                offered.protocol_versions.iter().filter_map(move |host| {
                    (client.major == host.major).then_some(UiProtocolVersion {
                        major: client.major,
                        minor: client.minor.min(host.minor),
                    })
                })
            })
            .max()
            .ok_or_else(|| {
                UiValidationError::new(
                    "ui.capabilities.no-common-protocol",
                    "protocolVersions",
                    "there is no mutually supported remote UI protocol version",
                )
            })?;

        Ok(UiCapabilitySelection {
            protocol_version,
            primitives: primitive_intersection(&self.primitives, &offered.primitives),
            capabilities: intersection(&self.capabilities, &offered.capabilities),
            contribution_points: intersection(
                &self.contribution_points,
                &offered.contribution_points,
            ),
            image_protocols: intersection(&self.terminal_graphics, &offered.terminal_graphics),
            color_depth: color_depth_rank(&self.color_depth)
                .min(color_depth_rank(&offered.color_depth)),
            unicode: self.daemon.unicode && offered.daemon.unicode,
            mouse: self.daemon.mouse && offered.daemon.mouse,
            screen_reader: self.screen_reader && offered.screen_reader,
            viewport: Some(self.viewport),
            limits: self.limits.intersection(offered.limits),
        })
    }
}

fn primitive_intersection(left: &[UiPrimitive], right: &[UiPrimitive]) -> Vec<UiPrimitive> {
    let left_all = left.iter().any(|primitive| primitive.as_str() == "*");
    let right_all = right.iter().any(|primitive| primitive.as_str() == "*");
    match (left_all, right_all) {
        (true, true) => vec![UiPrimitive::from("*")],
        (true, false) => right.to_vec(),
        (false, true) => left.to_vec(),
        (false, false) => intersection(left, right),
    }
}

fn color_depth_rank(depth: &UiColorDepth) -> u16 {
    match depth.as_str() {
        "monochrome" => 1,
        "ansi16" => 4,
        "ansi256" => 8,
        "trueColor" => 24,
        _ => 0,
    }
}

fn intersection<T>(left: &[T], right: &[T]) -> Vec<T>
where
    T: Clone + Eq + std::hash::Hash,
{
    let right: HashSet<&T> = right.iter().collect();
    let mut seen = HashSet::new();
    left.iter()
        .filter(|item| right.contains(item) && seen.insert(*item))
        .cloned()
        .collect()
}

/// Structured renderer/protocol error with a safe fallback and recovery hint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiRemoteError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub recoverable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<UiDocumentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<UiNodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<UiFallback>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub details: Value,
}

/// Resolution of a component-originated action/command invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiActionResult {
    pub invocation_id: UiEventId,
    pub status: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<UiRemoteError>,
}

/// A forward-compatible top-level message for dedicated remote UI transports.
/// `kind` selects the populated optional payload. Unknown kinds remain
/// deserializable and can carry `extensions` plus a fallback/error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiWireMessage {
    #[serde(rename = "type", alias = "kind")]
    pub kind: String,
    pub message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<UiSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_batch: Option<UiPatchBatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<UiEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<UiActionInvocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription: Option<UiProjectionSubscription>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsubscription: Option<UiProjectionUnsubscription>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<UiProjectionUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_result: Option<UiActionResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation: Option<UiActionCancellation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispose: Option<UiDispose>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<UiViewport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resync: Option<UiResyncRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hot_reload: Option<UiHotReload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<UiClientCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<UiCapabilitySelection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contributions: Vec<UiContributionRegistration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<UiTheme>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<UiRemoteError>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, Value>,
}

/// Counts observed during validation, useful for host telemetry and budgets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UiValidationStats {
    pub nodes: u32,
    pub maximum_depth: u16,
    pub text_bytes: u64,
    pub json_values: u32,
}

/// First structural/resource error found in an untrusted remote UI value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code} at {path}: {message}")]
#[serde(rename_all = "camelCase")]
pub struct UiValidationError {
    pub code: String,
    pub path: String,
    pub message: String,
}

impl UiValidationError {
    fn new(code: impl Into<String>, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            path: path.into(),
            message: message.into(),
        }
    }
}

impl UiHardLimits {
    /// Reject a zero ceiling; zero-sized offers are ambiguous and can otherwise
    /// turn a negotiated connection into a permanent retry loop.
    pub fn validate(&self) -> Result<(), UiValidationError> {
        let values = [
            ("maxTreeDepth", u64::from(self.max_tree_depth)),
            ("maxNodes", u64::from(self.max_nodes)),
            ("maxTextBytes", self.max_text_bytes),
            (
                "maxPropertiesPerNode",
                u64::from(self.max_properties_per_node),
            ),
            ("maxActionsPerNode", u64::from(self.max_actions_per_node)),
            ("maxJsonDepth", u64::from(self.max_json_depth)),
            ("maxJsonValues", u64::from(self.max_json_values)),
            ("maxPatchesPerBatch", u64::from(self.max_patches_per_batch)),
            ("maxPatchBytes", self.max_patch_bytes),
            ("maxContributions", u64::from(self.max_contributions)),
        ];
        for (field, value) in values {
            if value == 0 {
                return Err(UiValidationError::new(
                    "ui.limit.zero",
                    field,
                    "hard limits must be greater than zero",
                ));
            }
        }
        Ok(())
    }
}

impl UiCapabilities {
    pub fn validate(&self) -> Result<(), UiValidationError> {
        self.limits.validate()?;
        require_id(&self.client, "client")?;
        require_id(&self.color_depth, "colorDepth")?;
        if !matches!(
            self.color_depth.as_str(),
            "monochrome" | "ansi16" | "ansi256" | "trueColor"
        ) {
            return Err(UiValidationError::new(
                "ui.capabilities.invalid-color-depth",
                "colorDepth",
                "colorDepth must be monochrome, ansi16, ansi256, or trueColor",
            ));
        }
        if self.protocol_versions.is_empty() {
            return Err(UiValidationError::new(
                "ui.capabilities.protocol-required",
                "protocolVersions",
                "at least one protocol version is required",
            ));
        }
        let mut versions = HashSet::new();
        for version in &self.protocol_versions {
            if version.major == 0 {
                return Err(UiValidationError::new(
                    "ui.protocol.invalid-version",
                    "protocolVersions",
                    "protocol major version must be greater than zero",
                ));
            }
            if !versions.insert(*version) {
                return Err(UiValidationError::new(
                    "ui.capabilities.duplicate",
                    "protocolVersions",
                    "protocol versions must be unique",
                ));
            }
        }
        validate_unique_strings(&self.primitives, "primitives")?;
        if self.primitives.len() > 1
            && self
                .primitives
                .iter()
                .any(|primitive| primitive.as_str() == "*")
        {
            return Err(UiValidationError::new(
                "ui.capabilities.invalid-wildcard",
                "primitives",
                "the `*` primitive wildcard cannot be combined with named primitives",
            ));
        }
        validate_unique_strings(&self.media, "media")?;
        validate_unique_strings(&self.capabilities, "capabilities")?;
        validate_unique_strings(&self.contribution_points, "contributionPoints")?;
        validate_unique_strs(&self.terminal_graphics, "terminalGraphics")?;
        validate_viewport(self.viewport, "viewport")?;
        Ok(())
    }
}

impl UiDocument {
    /// Validate a complete untrusted tree before mounting it.
    pub fn validate(&self, limits: &UiHardLimits) -> Result<UiValidationStats, UiValidationError> {
        limits.validate()?;
        require_id(&self.document_id, "documentId")?;
        validate_protocol(self.protocol_version, "protocolVersion")?;
        validate_revision(self.revision, "revision")?;
        let mut validator = Validator::new(*limits);
        validator.text(self.document_id.as_str(), "documentId")?;
        if let Some(capabilities) = &self.capabilities {
            capabilities.validate()?;
            let value = serde_json::to_value(capabilities).map_err(|error| {
                UiValidationError::new(
                    "ui.numeric.non-finite",
                    "capabilities",
                    format!("capabilities cannot be encoded safely: {error}"),
                )
            })?;
            validator.json(&value, "capabilities")?;
        }
        for (key, value) in &self.metadata {
            validator.text(key, "metadata")?;
            validator.json(value, "metadata")?;
        }
        let mut roots = vec![NodeRoot {
            node: &self.root,
            require_root_id: true,
            scope: 0,
        }];
        if let Some(compatibility) = &self.compatibility {
            validator.compatibility(compatibility, "compatibility")?;
            if let Some(replacement) = compatibility
                .fallback
                .as_ref()
                .and_then(|fallback| fallback.replacement.as_deref())
            {
                roots.push(NodeRoot {
                    node: replacement,
                    require_root_id: true,
                    scope: 1,
                });
            }
        }
        validator.nodes(roots)?;
        Ok(validator.stats)
    }
}

impl UiNode {
    /// Validate a standalone node as the root of a semantic tree.
    pub fn validate(&self, limits: &UiHardLimits) -> Result<UiValidationStats, UiValidationError> {
        limits.validate()?;
        let mut validator = Validator::new(*limits);
        validator.nodes(vec![NodeRoot {
            node: self,
            require_root_id: true,
            scope: 0,
        }])?;
        Ok(validator.stats)
    }
}

impl UiPatchBatch {
    /// Validate revision ordering, operation-specific required fields, embedded
    /// subtrees, property payloads, and the encoded batch byte ceiling.
    pub fn validate(&self, limits: &UiHardLimits) -> Result<UiValidationStats, UiValidationError> {
        limits.validate()?;
        validate_protocol(self.protocol_version, "protocolVersion")?;
        require_id(&self.document_id, "documentId")?;
        validate_revision(self.base_revision, "baseRevision")?;
        validate_revision(self.revision, "revision")?;
        if self.base_revision.0.checked_add(1) != Some(self.revision.0) {
            return Err(UiValidationError::new(
                "ui.patch.invalid-revision",
                "revision",
                "a patch batch must advance baseRevision by exactly one",
            ));
        }
        if self.patches.is_empty() {
            return Err(UiValidationError::new(
                "ui.patch.empty-batch",
                "patches",
                "a revision-advancing patch batch must contain a patch",
            ));
        }
        if self.patches.len() > limits.max_patches_per_batch as usize {
            return Err(UiValidationError::new(
                "ui.limit.patch-count",
                "patches",
                format!(
                    "patch count {} exceeds limit {}",
                    self.patches.len(),
                    limits.max_patches_per_batch
                ),
            ));
        }

        let mut validator = Validator::new(*limits);
        let mut node_roots = Vec::new();
        for (index, patch) in self.patches.iter().enumerate() {
            let path = format!("patches[{index}]");
            if patch.op.is_empty() {
                return Err(UiValidationError::new(
                    "ui.id.empty",
                    format!("{path}.op"),
                    "patch operation must not be empty",
                ));
            }
            for (field, id) in [
                ("nodeId", patch.node_id.as_ref()),
                ("parentId", patch.parent_id.as_ref()),
            ] {
                if let Some(id) = id {
                    require_id(id, &format!("{path}.{field}"))?;
                }
            }
            validator.json(&patch.payload, &format!("{path}.payload"))?;
            if let Some(text) = &patch.text {
                validator.text(text, &format!("{path}.text"))?;
            }
            if let Some(props) = &patch.props {
                if props.set.len() + props.unset.len() > limits.max_properties_per_node as usize {
                    return Err(UiValidationError::new(
                        "ui.limit.property-count",
                        format!("{path}.props"),
                        "property patch exceeds maxPropertiesPerNode",
                    ));
                }
                let mut keys = HashSet::new();
                for (key, value) in &props.set {
                    require_nonempty(key, &format!("{path}.props.set"))?;
                    keys.insert(key.as_str());
                    validator.text(key, &format!("{path}.props.set"))?;
                    validator.json(value, &format!("{path}.props.set.{key}"))?;
                }
                for key in &props.unset {
                    require_nonempty(key, &format!("{path}.props.unset"))?;
                    if !keys.insert(key) {
                        return Err(UiValidationError::new(
                            "ui.patch.conflicting-property",
                            format!("{path}.props"),
                            format!("property {key:?} is both set and unset"),
                        ));
                    }
                    validator.text(key, &format!("{path}.props.unset"))?;
                }
            }
            if let Some(node) = &patch.node {
                // All inserted/replaced trees share the live document's id scope,
                // catching duplicate stable ids across one atomic batch.
                node_roots.push(NodeRoot {
                    node,
                    require_root_id: false,
                    scope: 0,
                });
            }
            validate_patch_shape(patch, &path, self.fallback.is_some())?;
        }
        if let Some(fallback) = &self.fallback {
            validator.fallback(fallback, "fallback")?;
            if let Some(node) = fallback.replacement.as_deref() {
                node_roots.push(NodeRoot {
                    node,
                    require_root_id: true,
                    scope: 1,
                });
            }
        }
        validator.nodes(node_roots)?;

        let encoded = serde_json::to_vec(self).map_err(|error| {
            UiValidationError::new(
                "ui.numeric.non-finite",
                "patches",
                format!("patch batch cannot be encoded safely: {error}"),
            )
        })?;
        if encoded.len() as u64 > limits.max_patch_bytes {
            return Err(UiValidationError::new(
                "ui.limit.patch-bytes",
                "patches",
                format!(
                    "encoded patch bytes {} exceeds limit {}",
                    encoded.len(),
                    limits.max_patch_bytes
                ),
            ));
        }
        Ok(validator.stats)
    }
}

impl UiEvent {
    pub fn validate(&self, limits: &UiHardLimits) -> Result<(), UiValidationError> {
        limits.validate()?;
        validate_protocol(self.protocol_version, "protocolVersion")?;
        require_id(&self.event_id, "eventId")?;
        require_id(&self.document_id, "documentId")?;
        require_id(&self.target_id, "targetId")?;
        require_id(&self.event_type, "type")?;
        validate_revision(self.revision, "revision")?;
        let mut validator = Validator::new(*limits);
        for value in [
            self.event_id.as_str(),
            self.document_id.as_str(),
            self.target_id.as_str(),
            self.event_type.as_str(),
        ] {
            validator.text(value, "event")?;
        }
        validator.json(&self.payload, "payload")?;
        if let Some(timestamp) = &self.timestamp {
            validator.text(timestamp, "timestamp")?;
        }
        if let Some(token) = &self.interaction_token {
            require_id(token, "interactionToken")?;
            validator.text(token, "interactionToken")?;
        }
        Ok(())
    }
}

impl UiActionInvocation {
    pub fn validate(&self, limits: &UiHardLimits) -> Result<(), UiValidationError> {
        limits.validate()?;
        require_id(&self.invocation_id, "invocationId")?;
        require_id(&self.document_id, "documentId")?;
        require_id(&self.source_node_id, "sourceNodeId")?;
        require_id(&self.action_id, "actionId")?;
        validate_revision(self.revision, "revision")?;
        let mut validator = Validator::new(*limits);
        for value in [
            self.invocation_id.as_str(),
            self.document_id.as_str(),
            self.source_node_id.as_str(),
            self.action_id.as_str(),
        ] {
            validator.text(value, "action")?;
        }
        validator.json(&self.payload, "payload")?;
        for (key, value) in &self.form_data {
            require_nonempty(key, "formData")?;
            validator.text(key, "formData")?;
            validator.json(value, &format!("formData.{key}"))?;
        }
        if let Some(token) = &self.interaction_token {
            require_id(token, "interactionToken")?;
            validator.text(token, "interactionToken")?;
        }
        if let Some(event_type) = &self.interaction_event_type {
            require_id(event_type, "interactionEventType")?;
            validator.text(event_type.as_str(), "interactionEventType")?;
        }
        Ok(())
    }
}

impl UiProjectionSubscription {
    pub fn validate(&self, limits: &UiHardLimits) -> Result<(), UiValidationError> {
        require_nonempty(&self.subscription_id, "subscription.subscriptionId")?;
        require_nonempty(&self.kind, "subscription.kind")?;
        if let Some(resource_id) = &self.resource_id {
            require_nonempty(resource_id, "subscription.resourceId")?;
        }
        let mut validator = Validator::new(*limits);
        validator.text(&self.subscription_id, "subscription.subscriptionId")?;
        validator.text(&self.kind, "subscription.kind")?;
        if let Some(resource_id) = &self.resource_id {
            validator.text(resource_id, "subscription.resourceId")?;
        }
        for (key, value) in &self.parameters {
            require_nonempty(key, "subscription.parameters")?;
            validator.text(key, "subscription.parameters")?;
            validator.json(value, &format!("subscription.parameters.{key}"))?;
        }
        Ok(())
    }
}

impl UiProjectionUnsubscription {
    pub fn validate(&self) -> Result<(), UiValidationError> {
        require_id(&self.subscription_id, "unsubscribe.subscriptionId")
    }
}

impl UiProjectionUpdate {
    pub fn validate(&self, limits: &UiHardLimits) -> Result<(), UiValidationError> {
        require_nonempty(&self.subscription_id, "projection.subscriptionId")?;
        if let Some(revision) = self.revision {
            validate_revision(revision, "projection.revision")?;
        }
        if self.removed && !self.value.is_null() {
            return Err(UiValidationError::new(
                "ui.projection.removed-with-value",
                "projection.value",
                "a removed projection must not carry a value",
            ));
        }
        let mut validator = Validator::new(*limits);
        validator.text(&self.subscription_id, "projection.subscriptionId")?;
        validator.json(&self.value, "projection.value")
    }
}

impl UiActionCancellation {
    pub fn validate(&self) -> Result<(), UiValidationError> {
        require_id(&self.invocation_id, "cancellation.invocationId")
    }
}

impl UiDispose {
    pub fn validate(&self) -> Result<(), UiValidationError> {
        require_id(&self.document_id, "dispose.documentId")?;
        validate_revision(self.revision, "dispose.revision")
    }
}

impl UiResyncRequest {
    pub fn validate(&self) -> Result<(), UiValidationError> {
        require_id(&self.document_id, "resync.documentId")?;
        if let Some(revision) = self.known_revision {
            validate_revision(revision, "resync.knownRevision")?;
        }
        Ok(())
    }
}

impl UiHotReload {
    pub fn validate(&self, limits: &UiHardLimits) -> Result<(), UiValidationError> {
        const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
        if self.generation > MAX_SAFE_INTEGER {
            return Err(UiValidationError::new(
                "ui.hot-reload.generation-out-of-range",
                "hotReload.generation",
                "generation exceeds JavaScript's maximum safe integer",
            ));
        }
        if self.changed_modules.is_empty() {
            return Err(UiValidationError::new(
                "ui.hot-reload.empty",
                "hotReload.changedModules",
                "hot reload must name at least one changed module",
            ));
        }
        validate_unique_strs(&self.changed_modules, "hotReload.changedModules")?;
        let mut validator = Validator::new(*limits);
        for module in &self.changed_modules {
            validator.text(module, "hotReload.changedModules")?;
        }
        Ok(())
    }
}

impl UiTheme {
    pub fn validate(&self, limits: &UiHardLimits) -> Result<(), UiValidationError> {
        limits.validate()?;
        require_nonempty(&self.id, "id")?;
        require_nonempty(&self.name, "name")?;
        let mut validator = Validator::new(*limits);
        validator.text(&self.id, "id")?;
        validator.text(&self.name, "name")?;
        for (token, value) in &self.tokens {
            require_nonempty(token, "tokens")?;
            validator.text(token, "tokens")?;
            validator.json(value, &format!("tokens.{token}"))?;
        }
        Ok(())
    }
}

impl UiContributionRegistration {
    pub fn validate(&self, limits: &UiHardLimits) -> Result<(), UiValidationError> {
        limits.validate()?;
        require_id(&self.id, "id")?;
        require_id(&self.extension_id, "extensionId")?;
        require_id(&self.point, "point")?;
        require_id(&self.slot, "slot")?;
        require_id(&self.document_id, "documentId")?;
        validate_unique_strings(&self.requires, "requires")?;
        let mut validator = Validator::new(*limits);
        if let Some(condition) = &self.when {
            validator.text(condition, "when")?;
        }
        for (key, value) in &self.metadata {
            require_nonempty(key, "metadata")?;
            validator.text(key, "metadata")?;
            validator.json(value, &format!("metadata.{key}"))?;
        }
        Ok(())
    }
}

impl UiSlotDefinition {
    pub fn validate(&self, limits: &UiHardLimits) -> Result<(), UiValidationError> {
        limits.validate()?;
        require_id(&self.id, "id")?;
        require_id(&self.point, "point")?;
        validate_unique_strs(&self.accepts, "accepts")?;
        if self.maximum_contributions == Some(0) {
            return Err(UiValidationError::new(
                "ui.slot.invalid-limit",
                "maximumContributions",
                "maximumContributions must be greater than zero when present",
            ));
        }
        Ok(())
    }
}

impl UiCapabilitySelection {
    pub fn validate(&self) -> Result<(), UiValidationError> {
        validate_protocol(self.protocol_version, "protocolVersion")?;
        self.limits.validate()?;
        if !matches!(self.color_depth, 1 | 4 | 8 | 24) {
            return Err(UiValidationError::new(
                "ui.capabilities.invalid-color-depth",
                "colorDepth",
                "negotiated colorDepth must be 1, 4, 8, or 24 bits",
            ));
        }
        validate_unique_strings(&self.primitives, "primitives")?;
        validate_unique_strings(&self.capabilities, "capabilities")?;
        validate_unique_strings(&self.contribution_points, "contributionPoints")?;
        validate_unique_strs(&self.image_protocols, "imageProtocols")?;
        if let Some(viewport) = self.viewport {
            if viewport.width == 0 || viewport.height == 0 {
                return Err(UiValidationError::new(
                    "ui.capabilities.invalid-viewport",
                    "viewport",
                    "viewport width and height must be greater than zero",
                ));
            }
            if let Some(density) = viewport.density {
                finite(density, "viewport.density")?;
            }
        }
        Ok(())
    }
}

impl UiRemoteError {
    pub fn validate(&self, limits: &UiHardLimits) -> Result<(), UiValidationError> {
        require_nonempty(&self.code, "error.code")?;
        require_nonempty(&self.message, "error.message")?;
        if let Some(document_id) = &self.document_id {
            require_id(document_id, "error.documentId")?;
        }
        if let Some(node_id) = &self.node_id {
            require_id(node_id, "error.nodeId")?;
        }
        let mut validator = Validator::new(*limits);
        validator.text(&self.code, "error.code")?;
        validator.text(&self.message, "error.message")?;
        if let Some(recovery) = &self.recovery {
            validator.text(recovery, "error.recovery")?;
        }
        validator.json(&self.details, "error.details")?;
        if let Some(fallback) = &self.fallback {
            validator.fallback(fallback, "error.fallback")?;
            if let Some(replacement) = fallback.replacement.as_deref() {
                validator.nodes(vec![NodeRoot {
                    node: replacement,
                    require_root_id: true,
                    scope: 0,
                }])?;
            }
        }
        Ok(())
    }
}

impl UiActionResult {
    pub fn validate(&self, limits: &UiHardLimits) -> Result<(), UiValidationError> {
        require_id(&self.invocation_id, "actionResult.invocationId")?;
        require_nonempty(&self.status, "actionResult.status")?;
        if self.status == "succeeded" && self.error.is_some() {
            return Err(UiValidationError::new(
                "ui.action-result.success-with-error",
                "actionResult.error",
                "a succeeded action result cannot carry an error",
            ));
        }
        if self.status == "failed" && self.error.is_none() {
            return Err(UiValidationError::new(
                "ui.action-result.failure-without-error",
                "actionResult.error",
                "a failed action result requires a structured error",
            ));
        }
        let mut validator = Validator::new(*limits);
        validator.text(&self.status, "actionResult.status")?;
        validator.json(&self.value, "actionResult.value")?;
        if let Some(error) = &self.error {
            error.validate(limits)?;
        }
        Ok(())
    }
}

impl UiWireMessage {
    pub fn validate(&self, limits: &UiHardLimits) -> Result<(), UiValidationError> {
        limits.validate()?;
        require_nonempty(&self.kind, "kind")?;
        require_nonempty(&self.message_id, "messageId")?;
        if let Some(snapshot) = &self.snapshot {
            snapshot.document.validate(limits)?;
        }
        if let Some(batch) = &self.patch_batch {
            batch.validate(limits)?;
        }
        if let Some(event) = &self.event {
            event.validate(limits)?;
        }
        if let Some(action) = &self.action {
            action.validate(limits)?;
        }
        if let Some(subscription) = &self.subscription {
            subscription.validate(limits)?;
        }
        if let Some(unsubscription) = &self.unsubscription {
            unsubscription.validate()?;
        }
        if let Some(projection) = &self.projection {
            projection.validate(limits)?;
        }
        if let Some(action_result) = &self.action_result {
            action_result.validate(limits)?;
        }
        if let Some(cancellation) = &self.cancellation {
            cancellation.validate()?;
        }
        if let Some(dispose) = &self.dispose {
            dispose.validate()?;
        }
        if let Some(viewport) = self.viewport {
            validate_viewport(viewport, "viewport")?;
        }
        if let Some(resync) = &self.resync {
            resync.validate()?;
        }
        if let Some(hot_reload) = &self.hot_reload {
            hot_reload.validate(limits)?;
        }
        if let Some(capabilities) = &self.capabilities {
            capabilities.validate()?;
        }
        if let Some(selection) = &self.selection {
            selection.validate()?;
        }
        if self.contributions.len() > limits.max_contributions as usize {
            return Err(UiValidationError::new(
                "ui.limit.contribution-count",
                "contributions",
                "contribution count exceeds maxContributions",
            ));
        }
        let mut contribution_ids = HashSet::new();
        for contribution in &self.contributions {
            contribution.validate(limits)?;
            if !contribution_ids.insert(contribution.id.as_str()) {
                return Err(UiValidationError::new(
                    "ui.id.duplicate",
                    "contributions",
                    format!("duplicate contribution id {:?}", contribution.id.as_str()),
                ));
            }
        }
        if let Some(theme) = &self.theme {
            theme.validate(limits)?;
        }
        if let Some(error) = &self.error {
            error.validate(limits)?;
        }
        let mut validator = Validator::new(*limits);
        for (key, value) in &self.extensions {
            require_nonempty(key, "extensions")?;
            validator.text(key, "extensions")?;
            validator.json(value, &format!("extensions.{key}"))?;
        }
        validate_wire_message_shape(self)?;
        Ok(())
    }
}

fn validate_wire_message_shape(message: &UiWireMessage) -> Result<(), UiValidationError> {
    let contribution_replacement = !message.contributions.is_empty()
        || message
            .extensions
            .get("contributionOwner")
            .and_then(Value::as_str)
            .is_some_and(|owner| !owner.trim().is_empty());
    let payload_count = [
        message.snapshot.is_some(),
        message.patch_batch.is_some(),
        message.event.is_some(),
        message.action.is_some(),
        message.subscription.is_some(),
        message.unsubscription.is_some(),
        message.projection.is_some(),
        message.action_result.is_some(),
        message.cancellation.is_some(),
        message.dispose.is_some(),
        message.viewport.is_some(),
        message.resync.is_some(),
        message.hot_reload.is_some(),
        message.capabilities.is_some(),
        message.selection.is_some(),
        contribution_replacement,
        message.theme.is_some(),
        message.error.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    let known_kind = matches!(
        message.kind.as_str(),
        "snapshot"
            | "patchBatch"
            | "event"
            | "action"
            | "subscription"
            | "unsubscribe"
            | "projection"
            | "actionResult"
            | "cancelAction"
            | "dispose"
            | "viewport"
            | "resync"
            | "hotReload"
            | "capabilities"
            | "capabilitySelection"
            | "contributions"
            | "theme"
            | "error"
    );
    let present = match message.kind.as_str() {
        "snapshot" => message.snapshot.is_some(),
        "patchBatch" => message.patch_batch.is_some(),
        "event" => message.event.is_some(),
        "action" => message.action.is_some(),
        "subscription" => message.subscription.is_some(),
        "unsubscribe" => message.unsubscription.is_some(),
        "projection" => message.projection.is_some(),
        "actionResult" => message.action_result.is_some(),
        "cancelAction" => message.cancellation.is_some(),
        "dispose" => message.dispose.is_some(),
        "viewport" => message.viewport.is_some(),
        "resync" => message.resync.is_some(),
        "hotReload" => message.hot_reload.is_some(),
        "capabilities" => message.capabilities.is_some(),
        "capabilitySelection" => message.selection.is_some(),
        "contributions" => contribution_replacement,
        "theme" => message.theme.is_some(),
        "error" => message.error.is_some(),
        // Unknown message kinds are forward-compatible when their body rides in
        // extensions or they carry a structured error.
        _ => !message.extensions.is_empty() || message.error.is_some(),
    };
    if !present {
        return Err(UiValidationError::new(
            "ui.message.missing-payload",
            "kind",
            format!(
                "message kind {:?} has no corresponding payload",
                message.kind
            ),
        ));
    }
    if known_kind && payload_count != 1 {
        return Err(UiValidationError::new(
            "ui.message.ambiguous-payload",
            "type",
            format!(
                "message type {:?} must carry exactly one typed payload, found {payload_count}",
                message.kind
            ),
        ));
    }
    Ok(())
}

fn validate_patch_shape(
    patch: &UiPatch,
    path: &str,
    has_fallback: bool,
) -> Result<(), UiValidationError> {
    let missing = |field: &str| {
        UiValidationError::new(
            "ui.patch.missing-field",
            format!("{path}.{field}"),
            format!("operation {:?} requires {field}", patch.op.as_str()),
        )
    };
    match patch.op.as_str() {
        patch_operations::REPLACE_ROOT => {
            if patch.node.is_none() {
                return Err(missing("node"));
            }
        }
        patch_operations::INSERT => {
            if patch.parent_id.is_none() {
                return Err(missing("parentId"));
            }
            if patch.index.is_none() {
                return Err(missing("index"));
            }
            if patch.node.is_none() {
                return Err(missing("node"));
            }
        }
        patch_operations::REMOVE => {
            if patch.node_id.is_none() {
                return Err(missing("nodeId"));
            }
        }
        patch_operations::REPLACE => {
            if patch.node_id.is_none() {
                return Err(missing("nodeId"));
            }
            if patch.node.is_none() {
                return Err(missing("node"));
            }
        }
        patch_operations::UPDATE_PROPS => {
            if patch.node_id.is_none() {
                return Err(missing("nodeId"));
            }
            if patch.props.is_none() {
                return Err(missing("props"));
            }
        }
        patch_operations::SET_TEXT => {
            if patch.node_id.is_none() {
                return Err(missing("nodeId"));
            }
            if patch.text.is_none() {
                return Err(missing("text"));
            }
        }
        patch_operations::MOVE => {
            if patch.node_id.is_none() {
                return Err(missing("nodeId"));
            }
            if patch.parent_id.is_none() {
                return Err(missing("parentId"));
            }
            if patch.index.is_none() {
                return Err(missing("index"));
            }
        }
        _ if !has_fallback => {
            return Err(UiValidationError::new(
                "ui.patch.unsupported-without-fallback",
                format!("{path}.op"),
                "an unknown patch operation requires a batch fallback",
            ));
        }
        _ => {}
    }
    Ok(())
}

struct NodeRoot<'a> {
    node: &'a UiNode,
    require_root_id: bool,
    scope: u64,
}

struct Validator {
    limits: UiHardLimits,
    stats: UiValidationStats,
}

impl Validator {
    fn new(limits: UiHardLimits) -> Self {
        Self {
            limits,
            stats: UiValidationStats::default(),
        }
    }

    fn nodes(&mut self, roots: Vec<NodeRoot<'_>>) -> Result<(), UiValidationError> {
        let mut stack: Vec<_> = roots
            .into_iter()
            .map(|root| (root.node, 1_u16, root.scope, root.require_root_id))
            .collect();
        let mut ids = HashSet::<(u64, &str)>::new();

        while let Some((node, depth, scope, require_root_id)) = stack.pop() {
            self.stats.nodes = self.stats.nodes.checked_add(1).ok_or_else(|| {
                UiValidationError::new("ui.limit.node-count", "root", "node count overflow")
            })?;
            if self.stats.nodes > self.limits.max_nodes {
                return Err(UiValidationError::new(
                    "ui.limit.node-count",
                    "root",
                    format!("node count exceeds limit {}", self.limits.max_nodes),
                ));
            }
            if depth > self.limits.max_tree_depth {
                return Err(UiValidationError::new(
                    "ui.limit.tree-depth",
                    "root",
                    format!(
                        "tree depth {depth} exceeds limit {}",
                        self.limits.max_tree_depth
                    ),
                ));
            }
            self.stats.maximum_depth = self.stats.maximum_depth.max(depth);

            if node.kind.is_empty() {
                return Err(UiValidationError::new(
                    "ui.id.empty",
                    "node.kind",
                    "node kind must not be empty",
                ));
            }
            self.text(node.kind.as_str(), "node.kind")?;
            if let Some(id) = &node.id {
                require_id(id, "node.id")?;
                if !ids.insert((scope, id.as_str())) {
                    return Err(UiValidationError::new(
                        "ui.id.duplicate",
                        "node.id",
                        format!("duplicate node id {:?}", id.as_str()),
                    ));
                }
                self.text(id.as_str(), "node.id")?;
            }

            match node.kind.as_str() {
                node_kinds::TEXT => {
                    let text = node.text.as_ref().ok_or_else(|| {
                        UiValidationError::new(
                            "ui.node.missing-text",
                            "node.text",
                            "a text node requires text",
                        )
                    })?;
                    self.text(text, "node.text")?;
                    if !node.children.is_empty() {
                        return Err(UiValidationError::new(
                            "ui.node.text-has-children",
                            "node.children",
                            "a text node cannot have children",
                        ));
                    }
                    if node.node_type.is_some()
                        || node.props != UiNodeProps::default()
                        || node.fallback.is_some()
                        || !node.requires.is_empty()
                    {
                        return Err(UiValidationError::new(
                            "ui.node.invalid-text-shape",
                            "node",
                            "a text node may only contain kind, id, and text",
                        ));
                    }
                }
                node_kinds::ELEMENT => {
                    let primitive = node.node_type.as_ref().ok_or_else(|| {
                        UiValidationError::new(
                            "ui.node.missing-type",
                            "node.type",
                            "an element node requires a primitive type",
                        )
                    })?;
                    require_id(primitive, "node.type")?;
                    self.text(primitive.as_str(), "node.type")?;
                    if let Some(text) = &node.text {
                        self.text(text, "node.text")?;
                    }
                    if (require_root_id || is_interactive(node)) && node.id.is_none() {
                        return Err(UiValidationError::new(
                            "ui.node.id-required",
                            "node.id",
                            "root and interactive element nodes require a stable id",
                        ));
                    }
                }
                _ if node.fallback.is_none() => {
                    return Err(UiValidationError::new(
                        "ui.node.unknown-without-fallback",
                        "node.kind",
                        "an unknown node kind requires a fallback",
                    ));
                }
                _ => {}
            }

            let mut requirements = HashSet::new();
            for requirement in &node.requires {
                require_id(&requirement.feature, "node.requires.feature")?;
                if !requirements.insert(requirement.feature.as_str()) {
                    return Err(UiValidationError::new(
                        "ui.id.duplicate",
                        "node.requires",
                        format!("duplicate requirement {:?}", requirement.feature.as_str()),
                    ));
                }
                self.text(requirement.feature.as_str(), "node.requires")?;
            }
            self.props(&node.props, "node.props")?;

            let child_depth = depth.checked_add(1).ok_or_else(|| {
                UiValidationError::new(
                    "ui.limit.tree-depth",
                    "node.children",
                    "tree depth overflow",
                )
            })?;
            for child in node.children.iter().rev() {
                stack.push((child, child_depth, scope, false));
            }
            if let Some(fallback) = node.fallback.as_deref() {
                stack.push((fallback, child_depth, scope, false));
            }
        }
        Ok(())
    }

    fn props(&mut self, props: &UiNodeProps, path: &str) -> Result<(), UiValidationError> {
        let typed_count = [
            props.role.is_some(),
            props.layout.is_some(),
            props.style.is_some(),
            props.accessibility.is_some(),
            props.content.is_some(),
            props.structured_data.is_some(),
            props.feedback.is_some(),
            props.navigation.is_some(),
            props.input.is_some(),
            !props.event_bindings.is_empty(),
            props.value.is_some(),
            !props.attributes.is_empty(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if typed_count + props.extension.len() > self.limits.max_properties_per_node as usize {
            return Err(UiValidationError::new(
                "ui.limit.property-count",
                path,
                "node properties exceed maxPropertiesPerNode",
            ));
        }
        if props.event_bindings.len() > self.limits.max_actions_per_node as usize {
            return Err(UiValidationError::new(
                "ui.limit.action-count",
                format!("{path}.actions"),
                "node actions exceed maxActionsPerNode",
            ));
        }
        if let Some(role) = &props.role {
            require_id(role, &format!("{path}.role"))?;
            self.text(role.as_str(), &format!("{path}.role"))?;
        }
        if let Some(layout) = &props.layout {
            self.layout(layout, &format!("{path}.layout"))?;
        }
        if let Some(style) = &props.style {
            self.style(style, &format!("{path}.style"))?;
        }
        if let Some(accessibility) = &props.accessibility {
            self.accessibility(accessibility, &format!("{path}.accessibility"))?;
        }
        if let Some(content) = &props.content {
            self.content(content, &format!("{path}.content"))?;
        }
        if let Some(data) = &props.structured_data {
            self.data(data, &format!("{path}.structuredData"))?;
        }
        if let Some(feedback) = &props.feedback {
            self.feedback(feedback, &format!("{path}.feedback"))?;
        }
        if let Some(navigation) = &props.navigation {
            for text in [navigation.destination.as_ref(), navigation.target.as_ref()]
                .into_iter()
                .flatten()
            {
                self.text(text, &format!("{path}.navigation"))?;
            }
        }
        if let Some(input) = &props.input {
            self.input(input, &format!("{path}.input"))?;
        }
        for key in ["inputType", "type"] {
            if let Some(Value::String(input_type)) = props.extension.get(key) {
                reject_secret_input_type(input_type, &format!("{path}.{key}"))?;
            }
        }
        for key in [
            "secret",
            "secretName",
            "secretValue",
            "password",
            "credential",
            "sensitive",
        ] {
            if props
                .extension
                .get(key)
                .is_some_and(|value| !value.is_null() && value != &Value::Bool(false))
            {
                return Err(UiValidationError::new(
                    "ui.input.secret-forbidden",
                    format!("{path}.{key}"),
                    "secret entry is host-owned; remote UI documents may only receive an opaque handle or decision",
                ));
            }
        }
        let mut actions = HashSet::new();
        for action in &props.event_bindings {
            require_id(&action.event, &format!("{path}.actions.event"))?;
            require_id(&action.action_id, &format!("{path}.actions.actionId"))?;
            self.text(action.event.as_str(), &format!("{path}.actions.event"))?;
            self.text(
                action.action_id.as_str(),
                &format!("{path}.actions.actionId"),
            )?;
            if !actions.insert((action.event.as_str(), action.action_id.as_str())) {
                return Err(UiValidationError::new(
                    "ui.action.duplicate-binding",
                    format!("{path}.actions"),
                    "event/action bindings must be unique on a node",
                ));
            }
            self.json(&action.payload, &format!("{path}.actions.payload"))?;
            validate_unique_strings(&action.requires, &format!("{path}.actions.requires"))?;
            for capability in &action.requires {
                self.text(capability.as_str(), &format!("{path}.actions.requires"))?;
            }
            if let Some(confirmation) = &action.confirmation {
                self.text(confirmation, &format!("{path}.actions.confirmation"))?;
            }
        }
        if let Some(value) = &props.value {
            self.json(value, &format!("{path}.value"))?;
        }
        for (key, value) in props.attributes.iter().chain(&props.extension) {
            require_nonempty(key, path)?;
            self.text(key, path)?;
            self.json(value, &format!("{path}.{key}"))?;
        }
        Ok(())
    }

    fn layout(&mut self, layout: &UiLayout, path: &str) -> Result<(), UiValidationError> {
        for (field, value) in [
            ("gap", layout.gap),
            ("rowGap", layout.row_gap),
            ("columnGap", layout.column_gap),
            ("grow", layout.grow),
            ("shrink", layout.shrink),
        ] {
            if let Some(value) = value {
                finite(value, &format!("{path}.{field}"))?;
            }
        }
        for (field, dimension) in [
            ("width", layout.width.as_ref()),
            ("height", layout.height.as_ref()),
            ("minWidth", layout.min_width.as_ref()),
            ("maxWidth", layout.max_width.as_ref()),
            ("minHeight", layout.min_height.as_ref()),
            ("maxHeight", layout.max_height.as_ref()),
            ("basis", layout.basis.as_ref()),
        ] {
            if let Some(dimension) = dimension {
                self.dimension(dimension, &format!("{path}.{field}"))?;
            }
        }
        for dimension in layout.columns.iter().chain(&layout.rows) {
            self.dimension(dimension, path)?;
        }
        for edges in [layout.padding, layout.margin].into_iter().flatten() {
            for value in [edges.top, edges.right, edges.bottom, edges.left] {
                finite(value, path)?;
            }
        }
        for value in [
            layout.direction.as_ref(),
            layout.align.as_ref(),
            layout.justify.as_ref(),
            layout.wrap.as_ref(),
            layout.overflow.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            self.text(value, path)?;
        }
        Ok(())
    }

    fn dimension(&mut self, dimension: &UiDimension, path: &str) -> Result<(), UiValidationError> {
        finite(dimension.value, path)?;
        require_nonempty(&dimension.unit, path)?;
        self.text(&dimension.unit, path)
    }

    fn style(&mut self, style: &UiStyle, path: &str) -> Result<(), UiValidationError> {
        if let Some(opacity) = style.opacity {
            finite(opacity, &format!("{path}.opacity"))?;
            if !(0.0..=1.0).contains(&opacity) {
                return Err(UiValidationError::new(
                    "ui.style.invalid-opacity",
                    format!("{path}.opacity"),
                    "opacity must be between 0 and 1",
                ));
            }
        }
        for value in [
            style.foreground.as_ref(),
            style.background.as_ref(),
            style.border_color.as_ref(),
            style.border_style.as_ref(),
            style.tone.as_ref(),
            style.visibility.as_ref(),
            style.truncate.as_ref(),
        ]
        .into_iter()
        .flatten()
        .chain(style.emphasis.iter())
        {
            self.text(value, path)?;
        }
        Ok(())
    }

    fn accessibility(
        &mut self,
        accessibility: &UiAccessibility,
        path: &str,
    ) -> Result<(), UiValidationError> {
        if accessibility
            .heading_level
            .is_some_and(|level| !(1..=6).contains(&level))
        {
            return Err(UiValidationError::new(
                "ui.accessibility.invalid-heading-level",
                format!("{path}.headingLevel"),
                "heading level must be between 1 and 6",
            ));
        }
        for value in [
            accessibility.label.as_ref(),
            accessibility.description.as_ref(),
            accessibility.live_region.as_ref(),
            accessibility.keyboard_hint.as_ref(),
            accessibility.text_fallback.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            self.text(value, path)?;
        }
        if let Some(role) = &accessibility.role {
            require_id(role, &format!("{path}.role"))?;
            self.text(role.as_str(), &format!("{path}.role"))?;
        }
        Ok(())
    }

    fn content(&mut self, content: &UiContent, path: &str) -> Result<(), UiValidationError> {
        for value in [
            content.text.as_ref(),
            content.language.as_ref(),
            content.alternate_text.as_ref(),
            content.line_wrap.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            self.text(value, path)?;
        }
        for span in &content.spans {
            self.text(&span.text, path)?;
            if let Some(style) = &span.style {
                self.style(style, path)?;
            }
            for value in [span.link.as_ref(), span.accessibility_label.as_ref()]
                .into_iter()
                .flatten()
            {
                self.text(value, path)?;
            }
        }
        if let Some(resource) = &content.resource {
            require_nonempty(&resource.uri, &format!("{path}.resource.uri"))?;
            require_nonempty(&resource.media_type, &format!("{path}.resource.mediaType"))?;
            self.text(&resource.uri, path)?;
            self.text(&resource.media_type, path)?;
            if let Some(digest) = &resource.digest {
                self.text(digest, path)?;
            }
        }
        Ok(())
    }

    fn data(&mut self, data: &UiData, path: &str) -> Result<(), UiValidationError> {
        let mut columns = HashSet::new();
        for column in &data.columns {
            require_nonempty(&column.id, &format!("{path}.columns.id"))?;
            if !columns.insert(column.id.as_str()) {
                return Err(UiValidationError::new(
                    "ui.id.duplicate",
                    format!("{path}.columns"),
                    format!("duplicate column id {:?}", column.id),
                ));
            }
            self.text(&column.id, path)?;
            self.text(&column.label, path)?;
            if let Some(value_type) = &column.value_type {
                self.text(value_type, path)?;
            }
            if let Some(width) = &column.width {
                self.dimension(width, path)?;
            }
        }
        for item in &data.items {
            self.json(item, &format!("{path}.items"))?;
        }
        if let Some(schema) = &data.schema {
            self.json(schema, &format!("{path}.schema"))?;
        }
        for selected in &data.selected_ids {
            require_nonempty(selected, &format!("{path}.selectedIds"))?;
            self.text(selected, path)?;
        }
        for value in [data.kind.as_ref(), data.cursor.as_ref()]
            .into_iter()
            .flatten()
        {
            self.text(value, path)?;
        }
        Ok(())
    }

    fn feedback(&mut self, feedback: &UiFeedback, path: &str) -> Result<(), UiValidationError> {
        for (field, value) in [("current", feedback.current), ("maximum", feedback.maximum)] {
            if let Some(value) = value {
                finite(value, &format!("{path}.{field}"))?;
            }
        }
        if feedback.maximum.is_some_and(|maximum| maximum < 0.0) {
            return Err(UiValidationError::new(
                "ui.feedback.invalid-maximum",
                format!("{path}.maximum"),
                "maximum must not be negative",
            ));
        }
        for value in [
            feedback.status.as_ref(),
            feedback.tone.as_ref(),
            feedback.message.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            self.text(value, path)?;
        }
        Ok(())
    }

    fn input(&mut self, input: &UiInput, path: &str) -> Result<(), UiValidationError> {
        if let Some(input_type) = input.input_type.as_deref() {
            reject_secret_input_type(input_type, &format!("{path}.inputType"))?;
        }
        for value in [
            input.name.as_ref(),
            input.input_type.as_ref(),
            input.placeholder.as_ref(),
            input.validation_message.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            self.text(value, path)?;
        }
        if let Some(value) = &input.value {
            self.json(value, &format!("{path}.value"))?;
        }
        if let Some(value) = &input.default_value {
            self.json(value, &format!("{path}.defaultValue"))?;
        }
        let mut option_ids = HashSet::new();
        for option in &input.options {
            require_nonempty(&option.id, &format!("{path}.options.id"))?;
            if !option_ids.insert(option.id.as_str()) {
                return Err(UiValidationError::new(
                    "ui.id.duplicate",
                    format!("{path}.options"),
                    format!("duplicate option id {:?}", option.id),
                ));
            }
            self.text(&option.id, path)?;
            self.text(&option.label, path)?;
            if let Some(description) = &option.description {
                self.text(description, path)?;
            }
            self.json(&option.value, &format!("{path}.options.value"))?;
        }
        Ok(())
    }

    fn compatibility(
        &mut self,
        compatibility: &UiCompatibility,
        path: &str,
    ) -> Result<(), UiValidationError> {
        if let Some(version) = compatibility.minimum_protocol {
            validate_protocol(version, &format!("{path}.minimumProtocol"))?;
        }
        validate_unique_strings(
            &compatibility.required_primitives,
            &format!("{path}.requiredPrimitives"),
        )?;
        validate_unique_strings(
            &compatibility.required_capabilities,
            &format!("{path}.requiredCapabilities"),
        )?;
        for primitive in &compatibility.required_primitives {
            self.text(primitive.as_str(), &format!("{path}.requiredPrimitives"))?;
        }
        for capability in &compatibility.required_capabilities {
            self.text(capability.as_str(), &format!("{path}.requiredCapabilities"))?;
        }
        if let Some(fallback) = &compatibility.fallback {
            self.fallback(fallback, &format!("{path}.fallback"))?;
        }
        Ok(())
    }

    fn fallback(&mut self, fallback: &UiFallback, path: &str) -> Result<(), UiValidationError> {
        if fallback.plain_text.is_none()
            && fallback.replacement.is_none()
            && fallback.behavior.is_none()
        {
            return Err(UiValidationError::new(
                "ui.fallback.empty",
                path,
                "fallback must provide plainText, replacement, or behavior",
            ));
        }
        for text in [fallback.plain_text.as_ref(), fallback.behavior.as_ref()]
            .into_iter()
            .flatten()
        {
            self.text(text, path)?;
        }
        Ok(())
    }

    fn text(&mut self, text: &str, path: &str) -> Result<(), UiValidationError> {
        self.stats.text_bytes = self
            .stats
            .text_bytes
            .checked_add(text.len() as u64)
            .ok_or_else(|| {
                UiValidationError::new("ui.limit.text-bytes", path, "text byte count overflow")
            })?;
        if self.stats.text_bytes > self.limits.max_text_bytes {
            return Err(UiValidationError::new(
                "ui.limit.text-bytes",
                path,
                format!(
                    "text bytes {} exceeds limit {}",
                    self.stats.text_bytes, self.limits.max_text_bytes
                ),
            ));
        }
        Ok(())
    }

    fn json(&mut self, value: &Value, path: &str) -> Result<(), UiValidationError> {
        let mut stack = vec![(value, 1_u16)];
        while let Some((value, depth)) = stack.pop() {
            self.stats.json_values = self.stats.json_values.checked_add(1).ok_or_else(|| {
                UiValidationError::new("ui.limit.json-values", path, "JSON value count overflow")
            })?;
            if self.stats.json_values > self.limits.max_json_values {
                return Err(UiValidationError::new(
                    "ui.limit.json-values",
                    path,
                    format!(
                        "JSON value count exceeds limit {}",
                        self.limits.max_json_values
                    ),
                ));
            }
            if depth > self.limits.max_json_depth {
                return Err(UiValidationError::new(
                    "ui.limit.json-depth",
                    path,
                    format!("JSON depth exceeds limit {}", self.limits.max_json_depth),
                ));
            }
            match value {
                Value::String(text) => self.text(text, path)?,
                Value::Array(items) => {
                    let next = depth.checked_add(1).ok_or_else(|| {
                        UiValidationError::new("ui.limit.json-depth", path, "JSON depth overflow")
                    })?;
                    stack.extend(items.iter().map(|item| (item, next)));
                }
                Value::Object(object) => {
                    let next = depth.checked_add(1).ok_or_else(|| {
                        UiValidationError::new("ui.limit.json-depth", path, "JSON depth overflow")
                    })?;
                    for (key, child) in object {
                        self.text(key, path)?;
                        stack.push((child, next));
                    }
                }
                Value::Number(number) => {
                    // serde_json cannot construct NaN/infinity. This explicit
                    // check documents and defends that invariant if its Number
                    // representation changes or arbitrary-precision is enabled.
                    if number.as_f64().is_some_and(|value| !value.is_finite()) {
                        return Err(UiValidationError::new(
                            "ui.numeric.non-finite",
                            path,
                            "numeric values must be finite",
                        ));
                    }
                }
                Value::Null | Value::Bool(_) => {}
            }
        }
        Ok(())
    }
}

fn is_interactive(node: &UiNode) -> bool {
    if node.props.input.is_some() || !node.props.event_bindings.is_empty() {
        return true;
    }
    node.node_type.as_ref().is_some_and(|primitive| {
        matches!(
            primitive.as_str(),
            primitives::BUTTON
                | primitives::ACTION_MENU
                | primitives::CONTEXT_MENU
                | primitives::LINK
                | primitives::TEXT_INPUT
                | primitives::TEXT_AREA
                | primitives::SELECT
                | primitives::MULTI_SELECT
                | primitives::CHECKBOX
                | primitives::RADIO
                | primitives::FORM
        )
    })
}

fn reject_secret_input_type(input_type: &str, path: &str) -> Result<(), UiValidationError> {
    let normalized = input_type
        .trim()
        .to_ascii_lowercase()
        .replace(['_', ' '], "-");
    let forbidden = [
        "password",
        "secret",
        "token",
        "api-key",
        "apikey",
        "credential",
        "private-key",
        "passphrase",
        "pin",
    ];
    if forbidden.contains(&normalized.as_str()) {
        return Err(UiValidationError::new(
            "ui.input.secret-forbidden",
            path,
            "secret entry is host-owned; remote UI documents may only receive an opaque handle or decision",
        ));
    }
    Ok(())
}

fn finite(value: f64, path: &str) -> Result<(), UiValidationError> {
    if !value.is_finite() {
        return Err(UiValidationError::new(
            "ui.numeric.non-finite",
            path,
            "numeric values must be finite",
        ));
    }
    Ok(())
}

fn validate_protocol(version: UiProtocolVersion, path: &str) -> Result<(), UiValidationError> {
    if version.major == 0 {
        return Err(UiValidationError::new(
            "ui.protocol.invalid-version",
            path,
            "protocol major version must be greater than zero",
        ));
    }
    Ok(())
}

fn validate_viewport(viewport: UiViewport, path: &str) -> Result<(), UiValidationError> {
    if viewport.width == 0 || viewport.height == 0 {
        return Err(UiValidationError::new(
            "ui.capabilities.invalid-viewport",
            path,
            "viewport width and height must be greater than zero",
        ));
    }
    if let Some(density) = viewport.density {
        finite(density, &format!("{path}.density"))?;
        if density <= 0.0 {
            return Err(UiValidationError::new(
                "ui.capabilities.invalid-density",
                format!("{path}.density"),
                "viewport density must be greater than zero",
            ));
        }
    }
    Ok(())
}

fn validate_revision(revision: UiRevision, path: &str) -> Result<(), UiValidationError> {
    // Largest integer JavaScript/TypeScript can represent exactly on the wire.
    const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
    if revision.0 > MAX_SAFE_INTEGER {
        return Err(UiValidationError::new(
            "ui.revision.out-of-range",
            path,
            "revision exceeds JavaScript's maximum safe integer",
        ));
    }
    Ok(())
}

fn require_nonempty(value: &str, path: &str) -> Result<(), UiValidationError> {
    if value.trim().is_empty() {
        return Err(UiValidationError::new(
            "ui.id.empty",
            path,
            "identifier/name must not be empty",
        ));
    }
    Ok(())
}

fn require_id<T>(value: &T, path: &str) -> Result<(), UiValidationError>
where
    T: AsRef<str>,
{
    require_nonempty(value.as_ref(), path)
}

fn validate_unique_strings<T>(values: &[T], path: &str) -> Result<(), UiValidationError>
where
    T: AsRef<str>,
{
    let values: Vec<&str> = values.iter().map(AsRef::as_ref).collect();
    validate_unique_strs(&values, path)
}

fn validate_unique_strs<T>(values: &[T], path: &str) -> Result<(), UiValidationError>
where
    T: AsRef<str>,
{
    let mut seen = HashSet::new();
    for value in values {
        let value = value.as_ref();
        require_nonempty(value, path)?;
        if !seen.insert(value) {
            return Err(UiValidationError::new(
                "ui.id.duplicate",
                path,
                format!("duplicate value {value:?}"),
            ));
        }
    }
    Ok(())
}

macro_rules! impl_as_ref {
    ($($name:ident),+ $(,)?) => {
        $(
            impl AsRef<str> for $name {
                fn as_ref(&self) -> &str {
                    self.as_str()
                }
            }
        )+
    };
}

impl_as_ref!(
    UiDocumentId,
    UiNodeId,
    UiActionId,
    UiEventId,
    UiNodeKind,
    UiPrimitive,
    UiCapability,
    UiClientKind,
    UiMediaCapability,
    UiColorDepth,
    UiPatchOperation,
    UiEventType,
    UiSemanticRole,
    UiContributionPoint,
    UiSlotId,
    UiContributionId,
    UiExtensionId,
);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn root_with(children: Vec<UiNode>) -> UiNode {
        let mut root = UiNode::element("root", primitives::STACK);
        root.children = children;
        root
    }

    fn document(root: UiNode) -> UiDocument {
        UiDocument {
            protocol_version: UiProtocolVersion::V1,
            document_id: UiDocumentId::from("document-1"),
            revision: UiRevision(7),
            root,
            capabilities: None,
            metadata: BTreeMap::new(),
            compatibility: None,
        }
    }

    fn set_text_batch(text: &str) -> UiPatchBatch {
        UiPatchBatch {
            protocol_version: UiProtocolVersion::V1,
            document_id: UiDocumentId::from("document-1"),
            base_revision: UiRevision(7),
            revision: UiRevision(8),
            patches: vec![UiPatch {
                op: UiPatchOperation::from(patch_operations::SET_TEXT),
                node_id: Some(UiNodeId::from("message")),
                parent_id: None,
                index: None,
                node: None,
                props: None,
                text: Some(text.to_owned()),
                payload: Value::Null,
            }],
            issued_at: Some("2026-08-11T00:00:00Z".to_owned()),
            atomic: true,
            fallback: None,
        }
    }

    fn capabilities(protocols: Vec<UiProtocolVersion>) -> UiCapabilities {
        UiCapabilities {
            client: UiClientKind::from("terminal"),
            protocol_versions: protocols,
            daemon: DaemonClientCapabilities {
                rich_text: true,
                image_display: true,
                diff_view: true,
                mouse: true,
                unicode: true,
                true_color: true,
                ..DaemonClientCapabilities::default()
            },
            primitives: vec![
                UiPrimitive::from(primitives::TEXT),
                UiPrimitive::from(primitives::STACK),
            ],
            media: vec![UiMediaCapability::from("image")],
            color_depth: UiColorDepth::from("trueColor"),
            keyboard: true,
            screen_reader: false,
            reduced_motion: false,
            clipboard: true,
            terminal_graphics: vec!["kitty".to_owned()],
            viewport: UiViewport {
                width: 120,
                height: 40,
                pixel_width: None,
                pixel_height: None,
                density: None,
            },
            capabilities: vec![UiCapability::from("keyboard")],
            contribution_points: vec![UiContributionPoint::from("transcript.renderer")],
            limits: UiHardLimits::default(),
        }
    }

    #[test]
    fn document_round_trips_with_canonical_camel_case_wire_shape() {
        let mut button = UiNode::element("retry", primitives::BUTTON);
        button.props.accessibility = Some(UiAccessibility {
            label: Some("Retry build".to_owned()),
            ..UiAccessibility::default()
        });
        button.props.event_bindings.push(UiActionBinding {
            event: UiEventType::from("press"),
            action_id: UiActionId::from("build.retry"),
            payload: json!({"attempt": 2}),
            requires: vec![UiCapability::from("commands")],
            disabled: false,
            confirmation: None,
        });
        let mut original = document(root_with(vec![UiNode::text("failed"), button]));
        original.capabilities = Some(capabilities(vec![UiProtocolVersion::V1]));

        let value = serde_json::to_value(&original).expect("serialize");
        assert_eq!(value["protocolVersion"], json!({"major": 1, "minor": 0}));
        assert_eq!(value["documentId"], "document-1");
        assert_eq!(value["root"]["kind"], "element");
        assert_eq!(value["root"]["type"], "Stack");
        assert!(value["capabilities"].is_object());
        assert!(value.get("protocol_version").is_none());

        let parsed: UiDocument = serde_json::from_value(value).expect("deserialize");
        assert_eq!(parsed, original);
        let stats = parsed
            .validate(&UiHardLimits::default())
            .expect("valid document");
        assert_eq!(stats.nodes, 3);
        assert_eq!(stats.maximum_depth, 2);
    }

    #[test]
    fn typescript_golden_document_parses_validates_and_round_trips_semantically() {
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../sdk/ui/test/fixtures/ui-document.json"
        ));
        let expected: Value = serde_json::from_str(source).expect("golden fixture JSON");
        let document: UiDocument =
            serde_json::from_value(expected.clone()).expect("Rust parses TypeScript fixture");
        document
            .validate(&UiHardLimits::default())
            .expect("TypeScript fixture passes host validation");
        let encoded = serde_json::to_value(document).expect("serialize fixture through Rust");
        assert_eq!(encoded, expected);
    }

    #[test]
    fn interactive_typescript_props_remain_flat_and_lossless() {
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../sdk/ui/test/fixtures/ui-interactive-document.json"
        ));
        let expected: Value = serde_json::from_str(source).expect("interactive fixture JSON");
        let document: UiDocument =
            serde_json::from_value(expected.clone()).expect("Rust parses interactive fixture");
        document
            .validate(&UiHardLimits::default())
            .expect("interactive fixture passes host validation");
        let encoded = serde_json::to_value(document).expect("serialize interactive fixture");
        assert_eq!(encoded, expected);
    }

    #[test]
    fn remote_documents_cannot_request_or_carry_secret_input() {
        let mut flat = UiNode::element("credential", primitives::TEXT_INPUT);
        flat.props
            .extension
            .insert("inputType".to_owned(), json!("password"));
        flat.props
            .extension
            .insert("value".to_owned(), json!("hunter2"));
        let error = document(root_with(vec![flat]))
            .validate(&UiHardLimits::default())
            .expect_err("password input must be host-owned");
        assert_eq!(error.code, "ui.input.secret-forbidden");
        assert!(error.path.ends_with("inputType"));

        let mut typed = UiNode::element("token", primitives::TEXT_INPUT);
        typed.props.input = Some(UiInput {
            input_type: Some("api_key".to_owned()),
            value: Some(json!("sk-not-on-the-wire")),
            ..UiInput::default()
        });
        let error = document(root_with(vec![typed]))
            .validate(&UiHardLimits::default())
            .expect_err("typed secret input must be rejected too");
        assert_eq!(error.code, "ui.input.secret-forbidden");
    }

    #[test]
    fn remote_documents_reject_secret_metadata_even_on_text_inputs() {
        let mut input = UiNode::element("credential", primitives::TEXT_INPUT);
        input
            .props
            .extension
            .insert("sensitive".to_owned(), Value::Bool(true));
        let error = document(root_with(vec![input]))
            .validate(&UiHardLimits::default())
            .expect_err("secret metadata must not cross the remote UI boundary");
        assert_eq!(error.code, "ui.input.secret-forbidden");
        assert!(error.path.ends_with("sensitive"));
    }

    #[test]
    fn generic_sdk_actions_and_chart_data_remain_lossless_extension_props() {
        let expected = json!({
            "protocolVersion": {"major": 1, "minor": 0},
            "documentId": "generic-props",
            "revision": 1,
            "root": {
                "kind": "element",
                "id": "root",
                "type": "Stack",
                "props": {},
                "children": [
                    {
                        "kind": "element",
                        "id": "card",
                        "type": "ToolCard",
                        "props": {"actions": ["open", "retry"], "action": "tool.open"},
                        "children": []
                    },
                    {
                        "kind": "element",
                        "id": "chart",
                        "type": "Chart",
                        "props": {"data": [1, 2, {"x": 3}]},
                        "children": []
                    }
                ]
            }
        });
        let document: UiDocument =
            serde_json::from_value(expected.clone()).expect("generic SDK props parse");
        document
            .validate(&UiHardLimits::default())
            .expect("generic SDK props validate");
        assert_eq!(
            document.root.children[0].props.extension.get("actions"),
            Some(&json!(["open", "retry"]))
        );
        assert_eq!(
            document.root.children[1].props.extension.get("data"),
            Some(&json!([1, 2, {"x": 3}]))
        );
        assert_eq!(
            serde_json::to_value(document).expect("round trip"),
            expected
        );
    }

    #[test]
    fn patch_and_event_use_sdk_field_names() {
        let mut batch = set_text_batch("done");
        batch.patches.push(UiPatch {
            op: UiPatchOperation::from(patch_operations::UPDATE_PROPS),
            node_id: Some(UiNodeId::from("message")),
            parent_id: None,
            index: None,
            node: None,
            props: Some(UiPropsPatch {
                set: BTreeMap::from([("tone".to_owned(), json!("positive"))]),
                unset: vec!["loading".to_owned()],
            }),
            text: None,
            payload: Value::Null,
        });
        let value = serde_json::to_value(&batch).expect("serialize");
        assert_eq!(value["protocolVersion"]["major"], 1);
        assert_eq!(value["patches"][0]["nodeId"], "message");
        assert_eq!(value["patches"][1]["unset"][0], "loading");
        assert_eq!(value["patches"][1]["set"]["tone"], "positive");
        assert!(value["patches"][1].get("props").is_none());
        assert!(value["patches"][0].get("targetId").is_none());
        let reparsed: UiPatchBatch =
            serde_json::from_value(value).expect("parse canonical TypeScript patch shape");
        assert_eq!(
            reparsed.patches[1]
                .props
                .as_ref()
                .expect("Rust props view")
                .set["tone"],
            "positive"
        );
        batch
            .validate(&UiHardLimits::default())
            .expect("valid patch batch");

        let event = UiEvent {
            protocol_version: UiProtocolVersion::V1,
            event_id: UiEventId::from("event-1"),
            document_id: UiDocumentId::from("document-1"),
            revision: UiRevision(8),
            target_id: UiNodeId::from("retry"),
            event_type: UiEventType::from("press"),
            payload: Value::Null,
            modifiers: None,
            timestamp: None,
            interaction_token: None,
        };
        let value = serde_json::to_value(&event).expect("serialize event");
        assert_eq!(value["targetId"], "retry");
        assert_eq!(value["type"], "press");
        event
            .validate(&UiHardLimits::default())
            .expect("valid event");
    }

    #[test]
    fn unknown_primitives_and_patch_operations_are_forward_compatible() {
        let custom = UiNode::element("custom", "vendor.example/HeatMap");
        document(custom)
            .validate(&UiHardLimits::default())
            .expect("custom primitive remains valid");

        let mut batch = set_text_batch("unused");
        batch.patches[0] = UiPatch {
            op: UiPatchOperation::from("futurePatch"),
            node_id: None,
            parent_id: None,
            index: None,
            node: None,
            props: None,
            text: None,
            payload: json!({"future": true}),
        };
        let error = batch
            .validate(&UiHardLimits::default())
            .expect_err("unknown operation without fallback must fail safely");
        assert_eq!(error.code, "ui.patch.unsupported-without-fallback");

        batch.fallback = Some(UiFallback {
            plain_text: Some("Content changed; refresh the view".to_owned()),
            replacement: None,
            behavior: Some("requestSnapshot".to_owned()),
        });
        batch
            .validate(&UiHardLimits::default())
            .expect("unknown operation with fallback is representable");
    }

    #[test]
    fn duplicate_and_empty_ids_are_rejected() {
        let first = UiNode::element("same", primitives::BOX);
        let second = UiNode::element("same", primitives::BOX);
        let error = document(root_with(vec![first, second]))
            .validate(&UiHardLimits::default())
            .expect_err("duplicate node ids");
        assert_eq!(error.code, "ui.id.duplicate");

        let error = document(UiNode::element("   ", primitives::BOX))
            .validate(&UiHardLimits::default())
            .expect_err("blank root id");
        assert_eq!(error.code, "ui.id.empty");
    }

    #[test]
    fn interactive_nodes_require_stable_ids() {
        let button = UiNode {
            kind: UiNodeKind::from(node_kinds::ELEMENT),
            id: None,
            node_type: Some(UiPrimitive::from(primitives::BUTTON)),
            text: None,
            props: UiNodeProps::default(),
            children: vec![UiNode::text("Run")],
            fallback: None,
            requires: Vec::new(),
        };
        let error = document(root_with(vec![button]))
            .validate(&UiHardLimits::default())
            .expect_err("button without id");
        assert_eq!(error.code, "ui.node.id-required");
    }

    #[test]
    fn malicious_deep_and_wide_trees_are_bounded() {
        let mut deep = UiNode::text("leaf");
        for index in (0..8).rev() {
            let mut parent = UiNode::element(format!("depth-{index}"), primitives::BOX);
            parent.children.push(deep);
            deep = parent;
        }
        let limits = UiHardLimits {
            max_tree_depth: 5,
            ..UiHardLimits::default()
        };
        let error = document(deep).validate(&limits).expect_err("depth bomb");
        assert_eq!(error.code, "ui.limit.tree-depth");

        let limits = UiHardLimits {
            max_nodes: 2,
            ..UiHardLimits::default()
        };
        let error = document(root_with(vec![UiNode::text("a"), UiNode::text("b")]))
            .validate(&limits)
            .expect_err("node bomb");
        assert_eq!(error.code, "ui.limit.node-count");
    }

    #[test]
    fn oversized_text_and_json_payloads_are_bounded() {
        let limits = UiHardLimits {
            max_text_bytes: 10,
            ..UiHardLimits::default()
        };
        let error = document(root_with(vec![UiNode::text("far too much text")]))
            .validate(&limits)
            .expect_err("text bomb");
        assert_eq!(error.code, "ui.limit.text-bytes");

        let mut root = root_with(Vec::new());
        root.props.value = Some(json!([1, 2, 3, 4, 5]));
        let limits = UiHardLimits {
            max_json_values: 3,
            ..UiHardLimits::default()
        };
        let error = document(root)
            .validate(&limits)
            .expect_err("JSON value bomb");
        assert_eq!(error.code, "ui.limit.json-values");
    }

    #[test]
    fn non_finite_layout_numbers_are_rejected_before_serialization() {
        let mut root = root_with(Vec::new());
        root.props.layout = Some(UiLayout {
            gap: Some(f64::NAN),
            ..UiLayout::default()
        });
        let error = document(root)
            .validate(&UiHardLimits::default())
            .expect_err("NaN layout");
        assert_eq!(error.code, "ui.numeric.non-finite");
    }

    #[test]
    fn patch_revisions_counts_and_bytes_are_bounded() {
        let mut batch = set_text_batch("done");
        batch.revision = UiRevision(9);
        let error = batch
            .validate(&UiHardLimits::default())
            .expect_err("revision jump");
        assert_eq!(error.code, "ui.patch.invalid-revision");

        let batch = set_text_batch("done");
        let limits = UiHardLimits {
            max_patches_per_batch: 1,
            max_patch_bytes: 1,
            ..UiHardLimits::default()
        };
        let error = batch.validate(&limits).expect_err("encoded bytes limit");
        assert_eq!(error.code, "ui.limit.patch-bytes");

        let mut batch = set_text_batch("done");
        batch.patches.push(batch.patches[0].clone());
        let limits = UiHardLimits {
            max_patches_per_batch: 1,
            ..UiHardLimits::default()
        };
        let error = batch.validate(&limits).expect_err("patch count limit");
        assert_eq!(error.code, "ui.limit.patch-count");
    }

    #[test]
    fn patch_shape_and_property_conflicts_are_rejected() {
        let mut batch = set_text_batch("done");
        batch.patches[0].text = None;
        let error = batch
            .validate(&UiHardLimits::default())
            .expect_err("setText without text");
        assert_eq!(error.code, "ui.patch.missing-field");

        let mut batch = set_text_batch("done");
        batch.patches[0] = UiPatch {
            op: UiPatchOperation::from(patch_operations::UPDATE_PROPS),
            node_id: Some(UiNodeId::from("message")),
            parent_id: None,
            index: None,
            node: None,
            props: Some(UiPropsPatch {
                set: BTreeMap::from([("tone".to_owned(), json!("positive"))]),
                unset: vec!["tone".to_owned()],
            }),
            text: None,
            payload: Value::Null,
        };
        let error = batch
            .validate(&UiHardLimits::default())
            .expect_err("same property set and unset");
        assert_eq!(error.code, "ui.patch.conflicting-property");
    }

    #[test]
    fn capabilities_negotiate_feature_and_limit_intersections() {
        let mut client = capabilities(vec![
            UiProtocolVersion { major: 1, minor: 0 },
            UiProtocolVersion { major: 1, minor: 1 },
        ]);
        client.primitives.push(UiPrimitive::from(primitives::IMAGE));
        client.limits.max_nodes = 1_000;

        let mut host = capabilities(vec![
            UiProtocolVersion { major: 1, minor: 0 },
            UiProtocolVersion { major: 1, minor: 1 },
        ]);
        host.daemon.mouse = false;
        host.limits.max_nodes = 5_000;

        let selected = client.negotiate(&host).expect("negotiate");
        assert_eq!(
            selected.protocol_version,
            UiProtocolVersion { major: 1, minor: 1 }
        );
        assert_eq!(selected.limits.max_nodes, 1_000);
        assert!(!selected.mouse);
        assert!(!selected
            .primitives
            .contains(&UiPrimitive::from(primitives::IMAGE)));

        let mut wildcard = capabilities(vec![UiProtocolVersion::V1]);
        wildcard.primitives = vec![UiPrimitive::from("*")];
        assert_eq!(
            serde_json::to_value(&wildcard).expect("wildcard capabilities")["primitives"],
            "*"
        );
        let selected = wildcard.negotiate(&host).expect("wildcard negotiation");
        assert_eq!(selected.primitives, host.primitives);

        let mut newer_client = capabilities(vec![UiProtocolVersion { major: 1, minor: 2 }]);
        newer_client.primitives = vec![UiPrimitive::from("*")];
        let older_host = capabilities(vec![UiProtocolVersion { major: 1, minor: 1 }]);
        assert_eq!(
            newer_client
                .negotiate(&older_host)
                .expect("minor versions are backward compatible")
                .protocol_version,
            UiProtocolVersion { major: 1, minor: 1 }
        );

        let mut invalid = older_host;
        invalid.color_depth = UiColorDepth::from("millions-of-colours");
        let error = invalid
            .validate()
            .expect_err("unknown colour depths must not negotiate as zero-bit colour");
        assert_eq!(error.code, "ui.capabilities.invalid-color-depth");
    }

    #[test]
    fn lifecycle_control_messages_are_typed_and_validated() {
        let message = UiWireMessage {
            kind: "hotReload".to_owned(),
            message_id: "message-1".to_owned(),
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
            hot_reload: Some(UiHotReload {
                generation: 2,
                changed_modules: vec!["dist/card.js".to_owned()],
            }),
            capabilities: None,
            selection: None,
            contributions: Vec::new(),
            theme: None,
            error: None,
            extensions: BTreeMap::new(),
        };
        message
            .validate(&UiHardLimits::default())
            .expect("valid hot reload");
        let value = serde_json::to_value(&message).expect("serialize control message");
        assert_eq!(value["type"], "hotReload");
        assert!(value.get("kind").is_none());

        let mut invalid = message;
        invalid.hot_reload.as_mut().unwrap().changed_modules.clear();
        let error = invalid
            .validate(&UiHardLimits::default())
            .expect_err("empty hot reload must be rejected");
        assert_eq!(error.code, "ui.hot-reload.empty");
    }

    #[test]
    fn mediated_projection_and_action_result_messages_are_typed_and_bounded() {
        let mut subscription = UiWireMessage {
            kind: "subscription".to_owned(),
            message_id: "subscription-message-1".to_owned(),
            snapshot: None,
            patch_batch: None,
            event: None,
            action: None,
            subscription: Some(UiProjectionSubscription {
                subscription_id: "session-main".to_owned(),
                kind: "session".to_owned(),
                resource_id: Some("active".to_owned()),
                parameters: BTreeMap::from([("tail".to_owned(), json!(50))]),
            }),
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
        };
        subscription
            .validate(&UiHardLimits::default())
            .expect("valid subscription");
        let encoded = serde_json::to_value(&subscription).expect("serialize subscription");
        assert_eq!(encoded["type"], "subscription");
        assert_eq!(encoded["subscription"]["parameters"]["tail"], 50);

        subscription.kind = "projection".to_owned();
        subscription.projection = Some(UiProjectionUpdate {
            subscription_id: "session-main".to_owned(),
            revision: Some(UiRevision(4)),
            removed: true,
            value: json!({"must": "not coexist"}),
        });
        subscription.subscription = None;
        let error = subscription
            .validate(&UiHardLimits::default())
            .expect_err("removed projection must not carry a value");
        assert_eq!(error.code, "ui.projection.removed-with-value");

        let result = UiActionResult {
            invocation_id: UiEventId::from("invoke-1"),
            status: "failed".to_owned(),
            value: Value::Null,
            error: None,
        };
        let error = result
            .validate(&UiHardLimits::default())
            .expect_err("failed action requires a structured error");
        assert_eq!(error.code, "ui.action-result.failure-without-error");
    }

    #[test]
    fn known_wire_message_types_reject_smuggled_second_payloads() {
        let mut message = UiWireMessage {
            kind: "hotReload".to_owned(),
            message_id: "ambiguous-1".to_owned(),
            snapshot: Some(UiSnapshot {
                document: document(root_with(Vec::new())),
                reason: None,
            }),
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
            hot_reload: Some(UiHotReload {
                generation: 2,
                changed_modules: vec!["dist/card.js".to_owned()],
            }),
            capabilities: None,
            selection: None,
            contributions: Vec::new(),
            theme: None,
            error: None,
            extensions: BTreeMap::new(),
        };
        let error = message
            .validate(&UiHardLimits::default())
            .expect_err("known messages must carry exactly one typed body");
        assert_eq!(error.code, "ui.message.ambiguous-payload");

        message.snapshot = None;
        message
            .validate(&UiHardLimits::default())
            .expect("one matching payload remains valid");
    }

    #[test]
    fn empty_contribution_replacement_requires_an_explicit_owner() {
        let replacement: UiWireMessage = serde_json::from_value(json!({
            "type": "contributions",
            "messageId": "contributions-empty-1",
            "contributions": [],
            "extensions": { "contributionOwner": "acme.plugin" }
        }))
        .expect("empty replacement parses");
        replacement
            .validate(&UiHardLimits::default())
            .expect("authenticated empty replacement is a typed payload");

        let missing_owner: UiWireMessage = serde_json::from_value(json!({
            "type": "contributions",
            "messageId": "contributions-empty-2",
            "contributions": []
        }))
        .expect("ownerless replacement parses structurally");
        let error = missing_owner
            .validate(&UiHardLimits::default())
            .expect_err("ownerless empty replacement is ambiguous");
        assert_eq!(error.code, "ui.message.missing-payload");
    }

    #[test]
    fn unknown_node_kind_needs_a_fallback() {
        let future = UiNode {
            kind: UiNodeKind::from("future-node"),
            id: Some(UiNodeId::from("future")),
            node_type: None,
            text: None,
            props: UiNodeProps::default(),
            children: Vec::new(),
            fallback: None,
            requires: Vec::new(),
        };
        let error = document(future.clone())
            .validate(&UiHardLimits::default())
            .expect_err("unknown node without fallback");
        assert_eq!(error.code, "ui.node.unknown-without-fallback");

        let mut future = future;
        future.fallback = Some(Box::new(UiNode::text("Unsupported component")));
        document(future)
            .validate(&UiHardLimits::default())
            .expect("fallback makes future kind safe");
    }

    #[test]
    fn sdk_projection_dtos_have_one_canonical_camel_case_shape() {
        let session = serde_json::to_value(UiSessionProjection {
            id: "session-1".into(),
            state: "open".into(),
            title: Some("A session".into()),
            active_run_id: Some("run-1".into()),
            updated_at: Some("2026-08-11T12:00:00Z".into()),
        })
        .unwrap();
        assert_eq!(
            session,
            json!({
                "id": "session-1",
                "state": "open",
                "title": "A session",
                "activeRunId": "run-1",
                "updatedAt": "2026-08-11T12:00:00Z",
            })
        );
        assert!(session.get("activeRuns").is_none());

        let run = serde_json::to_value(UiRunProjection {
            id: "run-1".into(),
            session_id: "session-1".into(),
            state: "running".into(),
            agent_mode: Some("autonomous".into()),
            progress: None,
            cost: None,
            started_at: Some("2026-08-11T12:00:00Z".into()),
            completed_at: None,
            data: Some(json!({ "objective": "Ship it" })),
        })
        .unwrap();
        assert_eq!(run["agentMode"], "autonomous");
        assert_eq!(run["data"]["objective"], "Ship it");
        assert!(run.get("mode").is_none());
        assert!(run.get("modelPolicy").is_none());

        let context = serde_json::to_value(UiContextProjection {
            active_file: Some("src/lib.rs".into()),
            selection: None,
            open_files: vec!["src/lib.rs".into()],
            dirty_buffers: Vec::new(),
            diagnostics_revision: 3,
        })
        .unwrap();
        assert_eq!(context["activeFile"], "src/lib.rs");
        assert_eq!(context["diagnosticsRevision"], 3);

        let workflow = serde_json::to_value(UiWorkflowProjection {
            workflow_run_id: "workflow-1".into(),
            phase: "running".into(),
            nodes: vec![UiWorkflowNodeProjection {
                workflow_run_id: "workflow-1".into(),
                node_id: "node-1".into(),
                state: "running".into(),
                attempt: 1,
                cost: None,
                error: None,
                warnings: Vec::new(),
            }],
        })
        .unwrap();
        assert_eq!(workflow["workflowRunId"], "workflow-1");
        assert_eq!(workflow["nodes"][0]["nodeId"], "node-1");

        // The blackboard projection carries the board's own item view, so a
        // column added to the board reaches a producer without a wire change.
        let blackboard = serde_json::to_value(UiBlackboardProjection {
            workflow_run_id: "workflow-1".into(),
            items: vec![crate::BlackboardItemView {
                id: "item-1".into(),
                workflow_run_id: "workflow-1".into(),
                kind: "finding".into(),
                payload: json!({ "note": "flaky test" }),
                author: json!({ "role": "analyst" }),
                confidence: Some(0.8),
                evidence: vec![json!({ "path": "src/lib.rs" })],
                revision: 2,
                superseded_by: None,
            }],
        })
        .unwrap();
        assert_eq!(blackboard["workflowRunId"], "workflow-1");
        assert_eq!(blackboard["items"][0]["kind"], "finding");
        assert_eq!(blackboard["items"][0]["revision"], 2);
        assert!(blackboard["items"][0].get("supersededBy").is_none());
        let empty = serde_json::to_value(UiBlackboardProjection {
            workflow_run_id: "workflow-1".into(),
            items: Vec::new(),
        })
        .unwrap();
        assert!(empty.get("items").is_none(), "an empty board omits items");
        assert_eq!(
            serde_json::from_value::<UiBlackboardProjection>(empty)
                .expect("an omitted items array reads back as an empty board")
                .items,
            Vec::new()
        );
    }
}
