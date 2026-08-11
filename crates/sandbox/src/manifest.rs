//! Plugin manifests (STEP 6.1): the parsed shape of `docs/specs/plugin.toml`.
//!
//! A plugin declares its identity, the runtime that hosts it, the *capabilities*
//! it needs (filesystem, network, secrets, subprocess), the *resources* it may
//! consume, its *security* record (checksum, signature, sandbox profile), and its
//! *update* policy. This module is the parser and validator; verification lives
//! in [`crate::verify`], the capability model in [`crate::permission`], and the
//! lifecycle in [`crate::lifecycle`]. Nothing here executes a plugin — the
//! manifest is untrusted input that every later stage gates on.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// The plugin-manifest schema version this build understands.
pub const SUPPORTED_PLUGIN_SCHEMA_VERSION: u32 = 1;

/// The version of the embedded-UI declaration understood by this build.
pub const SUPPORTED_UI_SCHEMA_VERSION: u32 = 1;

/// The remote-UI protocol contract implemented by the current hosts.
pub const SUPPORTED_UI_PROTOCOL_VERSION: &str = "1.0.0";

/// The TypeScript component SDK contract implemented by the current hosts.
pub const SUPPORTED_UI_SDK_VERSION: &str = "1.0.0";

/// Host policy bounds applied before any plugin-controlled value reaches an OS
/// limit or runtime argument. These are schema-level ceilings, not defaults.
pub const MIN_PLUGIN_MEMORY_MB: u64 = 32;
pub const MAX_PLUGIN_MEMORY_MB: u64 = 4_096;
pub const MAX_PLUGIN_CPU_SECONDS: u64 = 3_600;
pub const MAX_PLUGIN_WALL_SECONDS: u64 = 86_400;
pub const MAX_PLUGIN_OUTPUT_MB: u64 = 1_024;

/// A parse/validation failure for a plugin manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("invalid plugin manifest: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("unsupported plugin schema_version {found} (this build supports {supported})")]
    UnsupportedSchemaVersion { found: u32, supported: u32 },
    #[error("plugin id must not be empty")]
    EmptyId,
    #[error("plugin version must not be empty")]
    EmptyVersion,
    #[error("plugin publisher must not be empty")]
    EmptyPublisher,
    #[error("plugin runtime command must not be empty for a {kind} plugin")]
    EmptyCommand { kind: &'static str },
    #[error(
        "plugin filesystem capability paths must be absolute, normalized, non-root paths: {path}"
    )]
    InvalidFilesystemPath { path: String },
    #[error("plugin network capability must be a non-empty host:port destination: {destination}")]
    InvalidNetworkDestination { destination: String },
    #[error("plugin resource cap `{field}` must be greater than zero")]
    ZeroResourceCap { field: &'static str },
    #[error(
        "plugin resource cap `{field}` value {value} is outside host policy {minimum}..={maximum}"
    )]
    ResourceCapOutOfPolicy {
        field: &'static str,
        value: u64,
        minimum: u64,
        maximum: u64,
    },
    #[error("unsupported plugin UI schema_version {found} (this build supports {supported})")]
    UnsupportedUiSchemaVersion { found: u32, supported: u32 },
    #[error("unsupported plugin UI protocol `{found}` (this build supports {supported})")]
    UnsupportedUiProtocol {
        found: String,
        supported: &'static str,
    },
    #[error("unsupported plugin UI SDK `{found}` (this build supports {supported})")]
    UnsupportedUiSdk {
        found: String,
        supported: &'static str,
    },
    #[error("plugin UI host capability `{capability}` is not available in public UI v1")]
    UnsupportedUiCapability { capability: &'static str },
    #[error("invalid plugin UI {contract} compatibility range `{requirement}`")]
    InvalidUiCompatibility {
        contract: &'static str,
        requirement: String,
    },
    #[error("plugin UI `{target}` entrypoint must be a normalized relative package path: {path}")]
    InvalidUiEntrypoint { target: &'static str, path: String },
    #[error("plugin UI declaration must contain at least one entrypoint")]
    EmptyUiEntrypoints,
    #[error("plugin UI contribution `{id}` has no render target")]
    EmptyUiTargets { id: String },
    #[error("plugin UI contribution `{id}` cannot combine shared with terminal/web targets")]
    AmbiguousUiTargets { id: String },
    #[error("plugin UI contribution id is invalid: {id}")]
    InvalidUiContributionId { id: String },
    #[error("plugin UI contribution id is declared more than once: {id}")]
    DuplicateUiContributionId { id: String },
    #[error("plugin UI renderer identifier is invalid: {renderer}")]
    InvalidUiRendererId { renderer: String },
    #[error("plugin UI contribution `{id}` targets {target} without a compatible entrypoint")]
    MissingUiEntrypoint { id: String, target: &'static str },
    #[error(
        "web-only plugin UI contribution `{id}` must declare a terminal-safe fallback_renderer"
    )]
    MissingTerminalFallback { id: String },
    #[error(
        "web-only plugin UI contribution `{id}` fallback_renderer `{renderer}` must reference a different same-point terminal/shared contribution"
    )]
    InvalidTerminalFallback { id: String, renderer: String },
    #[error("plugin UI contribution `{id}` targets the host-owned core slot `{slot}`")]
    ReservedUiCoreSlot { id: String, slot: &'static str },
    #[error("theme-pack plugins are data-only and cannot declare execution surface: {declared}")]
    ThemeExecutionForbidden { declared: String },
    #[error("ui-component plugins must declare a [ui] component bundle")]
    UiComponentMissingUi,
    #[error(
        "ui-component plugins execute only in the UI worker and cannot declare daemon runtime/capabilities: {declared}"
    )]
    UiComponentExecutionForbidden { declared: String },
}

/// The execution class that hosts a plugin. Process, WASM, and remote plugins
/// use the daemon sandbox; UI components use the governed JS worker; theme packs
/// are data-only and structurally barred from every executable surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginKind {
    /// A native OS process, isolated by the platform sandbox.
    NativeProcess,
    /// A WASM component loaded into the daemon's `wasmtime` runtime.
    WasmComponent,
    /// A remote MCP server reached over the network (still capability-gated).
    McpRemote,
    /// A TypeScript/React-only package executed by the governed UI worker. It
    /// has `[ui]` entrypoints but no daemon process/component command.
    UiComponent,
    /// A data-only semantic theme pack. It may never carry runtime code,
    /// execution capabilities, or an embedded UI declaration.
    ThemePack,
}

impl PluginKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PluginKind::NativeProcess => "native-process",
            PluginKind::WasmComponent => "wasm-component",
            PluginKind::McpRemote => "mcp-remote",
            PluginKind::UiComponent => "ui-component",
            PluginKind::ThemePack => "theme-pack",
        }
    }
}

/// A parsed plugin manifest (`plugin.toml`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub schema_version: u32,
    /// The plugin's stable id (e.g. `github`).
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: PluginKind,
    /// The publisher identity — the key by which a signature is trusted.
    pub publisher: String,
    /// The scopes this plugin may be installed at.
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub runtime: RuntimeSpec,
    #[serde(default)]
    pub capabilities: CapabilitiesSpec,
    #[serde(default)]
    pub resources: ResourcesSpec,
    #[serde(default)]
    pub security: SecuritySpec,
    #[serde(default)]
    pub update: UpdateSpec,
    /// Optional native UI contribution declaration. The entire value is part of
    /// the canonical whole-manifest signature digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<UiSpec>,
}

/// A versioned embedded-UI declaration. All fields are declarative; the actual
/// TypeScript/React worker is still launched behind the sandbox boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiSpec {
    pub schema_version: u32,
    pub compatibility: UiCompatibilitySpec,
    pub entrypoints: UiEntrypointsSpec,
    /// Host APIs the component worker asks to use. Unknown names fail parsing,
    /// and additions are surfaced by the permission-diff gate.
    #[serde(default)]
    pub requested_capabilities: Vec<UiCapability>,
    #[serde(default)]
    pub contributions: Vec<UiContributionSpec>,
}

/// Protocol/SDK contracts required by a UI bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiCompatibilitySpec {
    pub protocol: String,
    pub sdk: String,
}

/// Package-local JavaScript entrypoints. A shared entrypoint may serve both
/// clients; client-specific entrypoints can override it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiEntrypointsSpec {
    #[serde(default)]
    pub shared: Option<String>,
    #[serde(default)]
    pub terminal: Option<String>,
    #[serde(default)]
    pub web: Option<String>,
}

impl UiEntrypointsSpec {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shared.is_none() && self.terminal.is_none() && self.web.is_none()
    }

    fn supports(&self, target: UiTarget) -> bool {
        match target {
            UiTarget::Shared => self.shared.is_some(),
            UiTarget::Terminal => self.shared.is_some() || self.terminal.is_some(),
            UiTarget::Web => self.shared.is_some() || self.web.is_some(),
        }
    }
}

/// Host-facing APIs available to UI workers. These do not bypass daemon policy:
/// they only allow a component to request the corresponding governed host action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UiCapability {
    ArtifactRead,
    ContextRead,
    RunRead,
    WorkflowRead,
    CommandInvoke,
    ClipboardRead,
    ClipboardWrite,
    OpenExternal,
    Notifications,
    InputCapture,
    Webview,
}

impl UiCapability {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ArtifactRead => "artifact-read",
            Self::ContextRead => "context-read",
            Self::RunRead => "run-read",
            Self::WorkflowRead => "workflow-read",
            Self::CommandInvoke => "command-invoke",
            Self::ClipboardRead => "clipboard-read",
            Self::ClipboardWrite => "clipboard-write",
            Self::OpenExternal => "open-external",
            Self::Notifications => "notifications",
            Self::InputCapture => "input-capture",
            Self::Webview => "webview",
        }
    }

    /// Whether the daemon has a complete governed service implementation for
    /// this capability in the public v1 contract. Other variants remain
    /// deserializable so stored manifests fail with a precise compatibility
    /// error instead of an opaque TOML parse failure.
    #[must_use]
    pub const fn is_supported_public_v1(self) -> bool {
        matches!(
            self,
            Self::ArtifactRead
                | Self::ContextRead
                | Self::RunRead
                | Self::WorkflowRead
                | Self::CommandInvoke
        )
    }
}

/// Client bundle in which a renderer is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UiTarget {
    Shared,
    Terminal,
    Web,
}

impl UiTarget {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Terminal => "terminal",
            Self::Web => "web",
        }
    }
}

/// Governed extension points exposed by the host. The final five variants are
/// deliberately deserializable so attempts to target security-sensitive slots
/// produce an explicit policy error rather than being mistaken for a typo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UiContributionPoint {
    Sidebar,
    Panel,
    StatusItem,
    Command,
    CommandPalette,
    ComposerAccessory,
    MessageRenderer,
    ToolRenderer,
    ArtifactRenderer,
    WorkflowInspector,
    BlackboardRenderer,
    DocumentBlock,
    CodeGraphNode,
    SettingsSection,
    SetupStep,
    Form,
    Wizard,
    DashboardCard,
    TraceSpanRenderer,
    ContextMenu,
    QuickPick,
    Notification,
    ApprovalFrame,
    ApprovalActions,
    SecretEntry,
    PolicyState,
    TerminalLifecycle,
}

impl UiContributionPoint {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sidebar => "sidebar",
            Self::Panel => "panel",
            Self::StatusItem => "status-item",
            Self::Command => "command",
            Self::CommandPalette => "command-palette",
            Self::ComposerAccessory => "composer-accessory",
            Self::MessageRenderer => "message-renderer",
            Self::ToolRenderer => "tool-renderer",
            Self::ArtifactRenderer => "artifact-renderer",
            Self::WorkflowInspector => "workflow-inspector",
            Self::BlackboardRenderer => "blackboard-renderer",
            Self::DocumentBlock => "document-block",
            Self::CodeGraphNode => "code-graph-node",
            Self::SettingsSection => "settings-section",
            Self::SetupStep => "setup-step",
            Self::Form => "form",
            Self::Wizard => "wizard",
            Self::DashboardCard => "dashboard-card",
            Self::TraceSpanRenderer => "trace-span-renderer",
            Self::ContextMenu => "context-menu",
            Self::QuickPick => "quick-pick",
            Self::Notification => "notification",
            Self::ApprovalFrame => "approval-frame",
            Self::ApprovalActions => "approval-actions",
            Self::SecretEntry => "secret-entry",
            Self::PolicyState => "policy-state",
            Self::TerminalLifecycle => "terminal-lifecycle",
        }
    }

    #[must_use]
    pub fn is_core_only(self) -> bool {
        matches!(
            self,
            Self::ApprovalFrame
                | Self::ApprovalActions
                | Self::SecretEntry
                | Self::PolicyState
                | Self::TerminalLifecycle
        )
    }
}

/// A renderer registration at one governed host contribution point.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiContributionSpec {
    pub id: String,
    pub point: UiContributionPoint,
    pub renderer: String,
    pub targets: Vec<UiTarget>,
    /// Required for web-only contributions so every terminal can present a safe,
    /// useful representation. This names the renderer of a separately declared
    /// same-point terminal/shared contribution in this signed manifest.
    #[serde(default)]
    pub fallback_renderer: Option<String>,
}

/// How the plugin is started.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSpec {
    /// The command (native process) or component path (WASM) to launch.
    #[serde(default)]
    pub command: String,
    /// The wire protocol the runtime speaks (e.g. `mcp-stdio`).
    #[serde(default)]
    pub protocol: String,
    /// Working-directory policy (`isolated` = a fresh pre-opened dir only).
    #[serde(default)]
    pub working_directory: String,
}

/// The capabilities a plugin declares it needs. Anything not listed here is
/// denied at run time — the manifest is the *complete* statement of what the
/// plugin may touch (STEP 6.1 / exit criterion 1).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitiesSpec {
    /// Filesystem paths the plugin may read (pre-opened; nothing else is visible).
    #[serde(default)]
    pub filesystem_read: Vec<String>,
    /// Filesystem paths the plugin may write.
    #[serde(default)]
    pub filesystem_write: Vec<String>,
    /// `host:port` network destinations the plugin may reach (an allowlist).
    #[serde(default)]
    pub network: Vec<String>,
    /// Named secrets the broker may pass to the plugin (never via env).
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Whether the plugin may spawn subprocesses.
    #[serde(default)]
    pub subprocess: bool,
}

/// Resource caps enforced by the sandbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourcesSpec {
    pub memory_mb: u64,
    pub cpu_seconds: u64,
    pub wall_seconds: u64,
    pub maximum_output_mb: u64,
}

impl Default for ResourcesSpec {
    fn default() -> Self {
        // Conservative defaults for a manifest that omits `[resources]`: a plugin
        // gets a small, bounded slice unless it asks for more (and is granted it).
        Self {
            memory_mb: 128,
            cpu_seconds: 30,
            wall_seconds: 60,
            maximum_output_mb: 8,
        }
    }
}

/// The plugin's security record: how the artifact is identified and trusted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecuritySpec {
    /// `sha256:<hex>` of the plugin artifact. Verified before install.
    #[serde(default)]
    pub checksum: String,
    /// Base64 ed25519 signature over the checksum, or a placeholder when unsigned.
    #[serde(default)]
    pub signature: String,
    /// The named sandbox profile the plugin runs under.
    #[serde(default)]
    pub sandbox_profile: String,
}

impl SecuritySpec {
    /// Whether the manifest carries a real (non-placeholder) signature. The
    /// packaging placeholders in `docs/specs/plugin.toml`
    /// (`set-during-packaging`) count as unsigned.
    #[must_use]
    pub fn is_signed(&self) -> bool {
        let sig = self.signature.trim();
        !sig.is_empty() && sig != "set-during-packaging"
    }
}

/// The plugin's update policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSpec {
    /// The release channel (e.g. `stable`).
    #[serde(default)]
    pub channel: String,
    /// Whether a permission change on update requires re-approval. Defaults to
    /// `true` — the safe posture; a manifest cannot silently opt out of the
    /// permission-diff gate (STEP 6.1 / exit criterion 2 is enforced in
    /// [`crate::lifecycle`] regardless of this flag).
    #[serde(default = "default_true")]
    pub permission_change_requires_approval: bool,
}

impl Default for UpdateSpec {
    fn default() -> Self {
        Self {
            channel: String::new(),
            permission_change_requires_approval: true,
        }
    }
}

fn default_true() -> bool {
    true
}

fn validate_package_entrypoint(
    target: UiTarget,
    entrypoint: &Option<String>,
) -> Result<(), ManifestError> {
    let Some(path) = entrypoint else {
        return Ok(());
    };
    let valid = !path.is_empty()
        && path.trim() == path
        && path.len() <= 1024
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && !path.contains('\\')
        && !path.contains('%')
        && !path.contains(':')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..");
    if !valid {
        return Err(ManifestError::InvalidUiEntrypoint {
            target: target.as_str(),
            path: path.clone(),
        });
    }
    Ok(())
}

fn valid_ui_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
        && !value.contains("..")
        && !value.contains("//")
        && !value.ends_with(['.', '_', '-', '/'])
}

fn validate_ui(ui: &UiSpec) -> Result<(), ManifestError> {
    if ui.schema_version != SUPPORTED_UI_SCHEMA_VERSION {
        return Err(ManifestError::UnsupportedUiSchemaVersion {
            found: ui.schema_version,
            supported: SUPPORTED_UI_SCHEMA_VERSION,
        });
    }
    let protocol_requirement = semver::VersionReq::parse(ui.compatibility.protocol.trim())
        .map_err(|_| ManifestError::InvalidUiCompatibility {
            contract: "protocol",
            requirement: ui.compatibility.protocol.clone(),
        })?;
    let protocol_version = semver::Version::parse(SUPPORTED_UI_PROTOCOL_VERSION)
        .expect("supported UI protocol constant is semver");
    if !protocol_requirement.matches(&protocol_version) {
        return Err(ManifestError::UnsupportedUiProtocol {
            found: ui.compatibility.protocol.clone(),
            supported: SUPPORTED_UI_PROTOCOL_VERSION,
        });
    }
    let sdk_requirement = semver::VersionReq::parse(ui.compatibility.sdk.trim()).map_err(|_| {
        ManifestError::InvalidUiCompatibility {
            contract: "sdk",
            requirement: ui.compatibility.sdk.clone(),
        }
    })?;
    let sdk_version = semver::Version::parse(SUPPORTED_UI_SDK_VERSION)
        .expect("supported UI SDK constant is semver");
    if !sdk_requirement.matches(&sdk_version) {
        return Err(ManifestError::UnsupportedUiSdk {
            found: ui.compatibility.sdk.clone(),
            supported: SUPPORTED_UI_SDK_VERSION,
        });
    }
    if ui.entrypoints.is_empty() {
        return Err(ManifestError::EmptyUiEntrypoints);
    }
    if let Some(capability) = ui
        .requested_capabilities
        .iter()
        .copied()
        .find(|capability| !capability.is_supported_public_v1())
    {
        return Err(ManifestError::UnsupportedUiCapability {
            capability: capability.as_str(),
        });
    }
    validate_package_entrypoint(UiTarget::Shared, &ui.entrypoints.shared)?;
    validate_package_entrypoint(UiTarget::Terminal, &ui.entrypoints.terminal)?;
    validate_package_entrypoint(UiTarget::Web, &ui.entrypoints.web)?;

    let mut ids = BTreeSet::new();
    for contribution in &ui.contributions {
        if !valid_ui_identifier(&contribution.id) {
            return Err(ManifestError::InvalidUiContributionId {
                id: contribution.id.clone(),
            });
        }
        if !ids.insert(contribution.id.clone()) {
            return Err(ManifestError::DuplicateUiContributionId {
                id: contribution.id.clone(),
            });
        }
        if !valid_ui_identifier(&contribution.renderer) {
            return Err(ManifestError::InvalidUiRendererId {
                renderer: contribution.renderer.clone(),
            });
        }
        if let Some(fallback) = &contribution.fallback_renderer {
            if !valid_ui_identifier(fallback) {
                return Err(ManifestError::InvalidUiRendererId {
                    renderer: fallback.clone(),
                });
            }
        }
        if contribution.point.is_core_only() {
            return Err(ManifestError::ReservedUiCoreSlot {
                id: contribution.id.clone(),
                slot: contribution.point.as_str(),
            });
        }
        if contribution.targets.is_empty() {
            return Err(ManifestError::EmptyUiTargets {
                id: contribution.id.clone(),
            });
        }
        let targets: BTreeSet<_> = contribution.targets.iter().copied().collect();
        if targets.contains(&UiTarget::Shared) && targets.len() > 1 {
            return Err(ManifestError::AmbiguousUiTargets {
                id: contribution.id.clone(),
            });
        }
        for target in &targets {
            if !ui.entrypoints.supports(*target) {
                return Err(ManifestError::MissingUiEntrypoint {
                    id: contribution.id.clone(),
                    target: target.as_str(),
                });
            }
        }
        let web_only = targets.contains(&UiTarget::Web)
            && !targets.contains(&UiTarget::Shared)
            && !targets.contains(&UiTarget::Terminal);
        if web_only && contribution.fallback_renderer.is_none() {
            return Err(ManifestError::MissingTerminalFallback {
                id: contribution.id.clone(),
            });
        }
    }
    for contribution in &ui.contributions {
        let targets = contribution
            .targets
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let web_only = targets.contains(&UiTarget::Web)
            && !targets.contains(&UiTarget::Shared)
            && !targets.contains(&UiTarget::Terminal);
        if !web_only {
            continue;
        }
        let fallback = contribution
            .fallback_renderer
            .as_deref()
            .expect("web-only fallback presence checked above");
        let resolved = ui.contributions.iter().any(|candidate| {
            candidate.id != contribution.id
                && candidate.renderer == fallback
                && candidate.point == contribution.point
                && candidate
                    .targets
                    .iter()
                    .any(|target| matches!(target, UiTarget::Terminal | UiTarget::Shared))
        });
        if !resolved {
            return Err(ManifestError::InvalidTerminalFallback {
                id: contribution.id.clone(),
                renderer: fallback.to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_theme_is_data_only(manifest: &PluginManifest) -> Result<(), ManifestError> {
    if manifest.kind != PluginKind::ThemePack {
        return Ok(());
    }
    let mut declared = Vec::new();
    if !manifest.runtime.command.trim().is_empty()
        || !manifest.runtime.protocol.trim().is_empty()
        || !manifest.runtime.working_directory.trim().is_empty()
    {
        declared.push("runtime");
    }
    if !manifest.capabilities.filesystem_read.is_empty()
        || !manifest.capabilities.filesystem_write.is_empty()
        || !manifest.capabilities.network.is_empty()
        || !manifest.capabilities.secrets.is_empty()
        || manifest.capabilities.subprocess
    {
        declared.push("capabilities");
    }
    if manifest.ui.is_some() {
        declared.push("ui");
    }
    if !declared.is_empty() {
        return Err(ManifestError::ThemeExecutionForbidden {
            declared: declared.join(", "),
        });
    }
    Ok(())
}

fn validate_ui_component_runtime(manifest: &PluginManifest) -> Result<(), ManifestError> {
    if manifest.kind != PluginKind::UiComponent {
        return Ok(());
    }
    if manifest.ui.is_none() {
        return Err(ManifestError::UiComponentMissingUi);
    }
    let mut declared = Vec::new();
    if !manifest.runtime.command.trim().is_empty()
        || !manifest.runtime.protocol.trim().is_empty()
        || !manifest.runtime.working_directory.trim().is_empty()
    {
        declared.push("runtime");
    }
    if !manifest.capabilities.filesystem_read.is_empty()
        || !manifest.capabilities.filesystem_write.is_empty()
        || !manifest.capabilities.network.is_empty()
        || !manifest.capabilities.secrets.is_empty()
        || manifest.capabilities.subprocess
    {
        declared.push("capabilities");
    }
    if !declared.is_empty() {
        return Err(ManifestError::UiComponentExecutionForbidden {
            declared: declared.join(", "),
        });
    }
    Ok(())
}

/// Parse a plugin manifest from TOML and validate its schema version + required
/// identity fields. Does **not** verify checksum/signature or evaluate
/// permissions — those are separate, later lifecycle stages.
pub fn parse_manifest(toml_str: &str) -> Result<PluginManifest, ManifestError> {
    let manifest: PluginManifest = toml::from_str(toml_str)?;
    if manifest.schema_version != SUPPORTED_PLUGIN_SCHEMA_VERSION {
        return Err(ManifestError::UnsupportedSchemaVersion {
            found: manifest.schema_version,
            supported: SUPPORTED_PLUGIN_SCHEMA_VERSION,
        });
    }
    if manifest.id.trim().is_empty() {
        return Err(ManifestError::EmptyId);
    }
    if manifest.version.trim().is_empty() {
        return Err(ManifestError::EmptyVersion);
    }
    if manifest.publisher.trim().is_empty() {
        return Err(ManifestError::EmptyPublisher);
    }
    validate_theme_is_data_only(&manifest)?;
    validate_ui_component_runtime(&manifest)?;
    // A process/WASM plugin must say what to launch; a remote MCP plugin is
    // reached over its declared network allowlist instead.
    if matches!(
        manifest.kind,
        PluginKind::NativeProcess | PluginKind::WasmComponent
    ) && manifest.runtime.command.trim().is_empty()
    {
        return Err(ManifestError::EmptyCommand {
            kind: manifest.kind.as_str(),
        });
    }
    for path in manifest
        .capabilities
        .filesystem_read
        .iter()
        .chain(manifest.capabilities.filesystem_write.iter())
    {
        let parsed = std::path::Path::new(path);
        let normalized = parsed.components().all(|component| {
            matches!(
                component,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        });
        if path.trim().is_empty() || path == "/" || !parsed.is_absolute() || !normalized {
            return Err(ManifestError::InvalidFilesystemPath { path: path.clone() });
        }
    }
    for destination in &manifest.capabilities.network {
        let Some((host, port)) = destination.rsplit_once(':') else {
            return Err(ManifestError::InvalidNetworkDestination {
                destination: destination.clone(),
            });
        };
        if host.trim().is_empty() || port.parse::<u16>().ok().filter(|p| *p > 0).is_none() {
            return Err(ManifestError::InvalidNetworkDestination {
                destination: destination.clone(),
            });
        }
    }
    for (field, value) in [
        ("memory_mb", manifest.resources.memory_mb),
        ("cpu_seconds", manifest.resources.cpu_seconds),
        ("wall_seconds", manifest.resources.wall_seconds),
        ("maximum_output_mb", manifest.resources.maximum_output_mb),
    ] {
        if value == 0 {
            return Err(ManifestError::ZeroResourceCap { field });
        }
    }
    for (field, value, minimum, maximum) in [
        (
            "memory_mb",
            manifest.resources.memory_mb,
            MIN_PLUGIN_MEMORY_MB,
            MAX_PLUGIN_MEMORY_MB,
        ),
        (
            "cpu_seconds",
            manifest.resources.cpu_seconds,
            1,
            MAX_PLUGIN_CPU_SECONDS,
        ),
        (
            "wall_seconds",
            manifest.resources.wall_seconds,
            1,
            MAX_PLUGIN_WALL_SECONDS,
        ),
        (
            "maximum_output_mb",
            manifest.resources.maximum_output_mb,
            1,
            MAX_PLUGIN_OUTPUT_MB,
        ),
    ] {
        if !(minimum..=maximum).contains(&value) {
            return Err(ManifestError::ResourceCapOutOfPolicy {
                field,
                value,
                minimum,
                maximum,
            });
        }
    }
    if let Some(ui) = &manifest.ui {
        validate_ui(ui)?;
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GITHUB_MANIFEST: &str = r#"
schema_version = 1
id = "github"
name = "GitHub Integration"
version = "0.1.0"
kind = "native-process"
publisher = "codypendent-project"
scopes = ["user", "organization", "repository"]

[runtime]
command = "codypendent-plugin-github"
protocol = "mcp-stdio"
working_directory = "isolated"

[capabilities]
filesystem_read = []
filesystem_write = []
network = ["api.github.com:443", "uploads.github.com:443"]
secrets = ["github-token"]
subprocess = false

[resources]
memory_mb = 256
cpu_seconds = 60
wall_seconds = 120
maximum_output_mb = 20

[security]
checksum = "sha256:set-during-packaging"
signature = "set-during-packaging"
sandbox_profile = "network-client"

[update]
channel = "stable"
permission_change_requires_approval = true
"#;

    #[test]
    fn parses_the_canonical_github_manifest() {
        let m = parse_manifest(GITHUB_MANIFEST).expect("canonical manifest parses");
        assert_eq!(m.id, "github");
        assert_eq!(m.kind, PluginKind::NativeProcess);
        assert_eq!(
            m.capabilities.network,
            ["api.github.com:443", "uploads.github.com:443"]
        );
        assert_eq!(m.capabilities.secrets, ["github-token"]);
        assert!(!m.capabilities.subprocess);
        assert_eq!(m.resources.memory_mb, 256);
        assert!(
            !m.security.is_signed(),
            "packaging placeholder is not a signature"
        );
        assert!(m.update.permission_change_requires_approval);
    }

    #[test]
    fn resource_caps_cannot_disable_host_policy_limits() {
        let oversized = GITHUB_MANIFEST.replace(
            "memory_mb = 256",
            &format!("memory_mb = {}", MAX_PLUGIN_MEMORY_MB + 1),
        );
        assert!(matches!(
            parse_manifest(&oversized),
            Err(ManifestError::ResourceCapOutOfPolicy {
                field: "memory_mb",
                ..
            })
        ));

        let undersized = GITHUB_MANIFEST.replace("memory_mb = 256", "memory_mb = 1");
        assert!(matches!(
            parse_manifest(&undersized),
            Err(ManifestError::ResourceCapOutOfPolicy {
                field: "memory_mb",
                ..
            })
        ));
    }

    #[test]
    fn checked_in_plugin_spec_stays_parseable() {
        let manifest = include_str!("../../../docs/specs/plugin.toml");
        let parsed = parse_manifest(manifest).expect("docs/specs/plugin.toml is canonical");
        assert!(parsed.ui.is_some());
    }

    #[test]
    fn rejects_unsupported_schema_version() {
        let bad = GITHUB_MANIFEST.replace("schema_version = 1", "schema_version = 99");
        assert!(matches!(
            parse_manifest(&bad),
            Err(ManifestError::UnsupportedSchemaVersion { found: 99, .. })
        ));
    }

    #[test]
    fn rejects_unknown_field() {
        let bad = format!("{GITHUB_MANIFEST}\nmalicious = true\n");
        assert!(matches!(parse_manifest(&bad), Err(ManifestError::Parse(_))));
    }

    #[test]
    fn rejects_process_plugin_without_a_command() {
        let bad =
            GITHUB_MANIFEST.replace("command = \"codypendent-plugin-github\"", "command = \"\"");
        assert!(matches!(
            parse_manifest(&bad),
            Err(ManifestError::EmptyCommand { .. })
        ));
    }

    #[test]
    fn resources_default_when_absent() {
        let minimal = r#"
schema_version = 1
id = "wc"
name = "Word Count"
version = "0.1.0"
kind = "wasm-component"
publisher = "me"
[runtime]
command = "word_count.wasm"
"#;
        let m = parse_manifest(minimal).expect("minimal wasm manifest parses");
        assert_eq!(m.resources, ResourcesSpec::default());
        assert!(
            m.update.permission_change_requires_approval,
            "approval gate defaults on"
        );
    }

    #[test]
    fn signature_placeholder_detection() {
        let mut sec = SecuritySpec::default();
        assert!(!sec.is_signed());
        sec.signature = "set-during-packaging".into();
        assert!(!sec.is_signed());
        sec.signature = "  ".into();
        assert!(!sec.is_signed());
        sec.signature = "aGVsbG8=".into();
        assert!(sec.is_signed());
    }

    fn manifest_with_ui(entrypoints: &str, contributions: &str) -> String {
        format!(
            r#"{GITHUB_MANIFEST}
[ui]
schema_version = 1
requested_capabilities = ["artifact-read", "command-invoke"]
[ui.compatibility]
protocol = ">=1.0,<2.0"
sdk = "^1.0"
[ui.entrypoints]
{entrypoints}
{contributions}
"#
        )
    }

    const ARTIFACT_CONTRIBUTION: &str = r#"
[[ui.contributions]]
id = "github.checks"
point = "artifact-renderer"
renderer = "github.ChecksRenderer"
targets = ["shared"]
"#;

    #[test]
    fn parses_versioned_cross_client_ui_declaration() {
        let manifest = manifest_with_ui(
            r#"shared = "dist/ui/shared.js"
terminal = "dist/ui/terminal.js"
web = "dist/ui/web.js""#,
            ARTIFACT_CONTRIBUTION,
        );
        let parsed = parse_manifest(&manifest).expect("full UI declaration parses");
        let ui = parsed.ui.expect("UI declaration retained");
        assert_eq!(ui.schema_version, SUPPORTED_UI_SCHEMA_VERSION);
        assert_eq!(ui.compatibility.protocol, ">=1.0,<2.0");
        assert_eq!(ui.compatibility.sdk, "^1.0");
        assert_eq!(ui.requested_capabilities.len(), 2);
        assert_eq!(ui.contributions[0].id, "github.checks");
        assert_eq!(
            ui.contributions[0].point,
            UiContributionPoint::ArtifactRenderer
        );
    }

    #[test]
    fn rejects_host_capabilities_without_a_public_v1_service() {
        let manifest = manifest_with_ui(r#"shared = "dist/ui/shared.js""#, ARTIFACT_CONTRIBUTION)
            .replace("command-invoke", "clipboard-read");
        assert!(matches!(
            parse_manifest(&manifest),
            Err(ManifestError::UnsupportedUiCapability {
                capability: "clipboard-read"
            })
        ));
    }

    #[test]
    fn rejects_unsafe_package_entrypoints_for_every_client_target() {
        let attacks = [
            "/tmp/evil.js",
            "../evil.js",
            "dist/../evil.js",
            "./dist/ui.js",
            "dist//ui.js",
            r"dist\\ui.js",
            "C:/evil.js",
            "https://evil.example/ui.js",
            "dist/%2e%2e/evil.js",
            " dist/ui.js",
        ];
        for target in ["shared", "terminal", "web"] {
            for attack in attacks {
                let entrypoints = format!(r#"{target} = "{attack}""#);
                let result = parse_manifest(&manifest_with_ui(&entrypoints, ""));
                assert!(
                    matches!(result, Err(ManifestError::InvalidUiEntrypoint { .. })),
                    "{target} entrypoint attack unexpectedly parsed: {attack:?}; result={result:?}"
                );
            }
        }
        assert!(matches!(
            validate_package_entrypoint(UiTarget::Shared, &Some("dist/ui.js\n".into())),
            Err(ManifestError::InvalidUiEntrypoint { .. })
        ));
    }

    #[test]
    fn web_only_renderer_requires_a_terminal_fallback() {
        let contribution = r#"
[[ui.contributions]]
id = "github.graph"
point = "code-graph-node"
renderer = "github.GraphRenderer"
targets = ["web"]
"#;
        let err = parse_manifest(&manifest_with_ui(r#"web = "dist/ui/web.js""#, contribution))
            .unwrap_err();
        assert!(matches!(err, ManifestError::MissingTerminalFallback { .. }));

        let with_fallback = format!(
            "{contribution}\nfallback_renderer = \"github.GraphSummary\"\n\n[[ui.contributions]]\nid = \"github.graph-summary\"\npoint = \"code-graph-node\"\nrenderer = \"github.GraphSummary\"\ntargets = [\"terminal\"]\n"
        );
        parse_manifest(&manifest_with_ui(
            r#"terminal = "dist/ui/terminal.js"
web = "dist/ui/web.js""#,
            &with_fallback,
        ))
        .expect("a web-only renderer with a declared terminal fallback is valid");

        let dangling =
            format!("{contribution}\nfallback_renderer = \"builtin.code-graph-summary\"\n");
        assert!(matches!(
            parse_manifest(&manifest_with_ui(
                r#"terminal = "dist/ui/terminal.js"
web = "dist/ui/web.js""#,
                &dangling,
            )),
            Err(ManifestError::InvalidTerminalFallback { .. })
        ));
    }

    #[test]
    fn contribution_must_have_a_compatible_entrypoint() {
        let contribution = r#"
[[ui.contributions]]
id = "github.status"
point = "status-item"
renderer = "github.Status"
targets = ["terminal"]
"#;
        assert!(matches!(
            parse_manifest(&manifest_with_ui(r#"web = "dist/ui/web.js""#, contribution,)),
            Err(ManifestError::MissingUiEntrypoint {
                target: "terminal",
                ..
            })
        ));
    }

    #[test]
    fn shared_target_is_mutually_exclusive_with_concrete_targets() {
        let contribution = r#"
[[ui.contributions]]
id = "github.shared-race"
point = "panel"
renderer = "github.SharedRace"
targets = ["shared", "terminal"]
"#;
        assert!(matches!(
            parse_manifest(&manifest_with_ui(
                r#"shared = "dist/ui/shared.js""#,
                contribution,
            )),
            Err(ManifestError::AmbiguousUiTargets { .. })
        ));
    }

    #[test]
    fn host_owned_core_slots_are_never_contribution_points() {
        for slot in [
            "approval-frame",
            "approval-actions",
            "secret-entry",
            "policy-state",
            "terminal-lifecycle",
        ] {
            let contribution = format!(
                r#"
[[ui.contributions]]
id = "attacker.core"
point = "{slot}"
renderer = "attacker.Spoof"
targets = ["shared"]
"#
            );
            let err = parse_manifest(&manifest_with_ui(
                r#"shared = "dist/ui/shared.js""#,
                &contribution,
            ))
            .unwrap_err();
            assert!(
                matches!(err, ManifestError::ReservedUiCoreSlot { .. }),
                "reserved slot was not explicitly rejected: {slot}"
            );
        }
    }

    #[test]
    fn unknown_ui_capabilities_and_contribution_points_fail_closed() {
        let unknown_capability = manifest_with_ui(r#"shared = "dist/ui/shared.js""#, "")
            .replace("command-invoke", "raw-host-access");
        assert!(matches!(
            parse_manifest(&unknown_capability),
            Err(ManifestError::Parse(_))
        ));

        let unknown_point = ARTIFACT_CONTRIBUTION.replace("artifact-renderer", "root-shell");
        assert!(matches!(
            parse_manifest(&manifest_with_ui(
                r#"shared = "dist/ui/shared.js""#,
                &unknown_point,
            )),
            Err(ManifestError::Parse(_))
        ));
    }

    #[test]
    fn duplicate_contribution_ids_are_rejected() {
        let contributions = format!("{ARTIFACT_CONTRIBUTION}{ARTIFACT_CONTRIBUTION}");
        assert!(matches!(
            parse_manifest(&manifest_with_ui(
                r#"shared = "dist/ui/shared.js""#,
                &contributions,
            )),
            Err(ManifestError::DuplicateUiContributionId { .. })
        ));
    }

    #[test]
    fn incompatible_ui_contracts_are_rejected() {
        let manifest = manifest_with_ui(r#"shared = "dist/ui/shared.js""#, "");
        let protocol = manifest.replace("protocol = \">=1.0,<2.0\"", "protocol = \"^2.0\"");
        assert!(matches!(
            parse_manifest(&protocol),
            Err(ManifestError::UnsupportedUiProtocol { .. })
        ));
        let sdk = manifest.replace("sdk = \"^1.0\"", "sdk = \"<1.0\"");
        assert!(matches!(
            parse_manifest(&sdk),
            Err(ManifestError::UnsupportedUiSdk { .. })
        ));

        let malformed = manifest.replace(
            "protocol = \">=1.0,<2.0\"",
            "protocol = \"definitely-not-semver\"",
        );
        assert!(matches!(
            parse_manifest(&malformed),
            Err(ManifestError::InvalidUiCompatibility {
                contract: "protocol",
                ..
            })
        ));
    }

    #[test]
    fn theme_pack_kind_is_structurally_data_only() {
        let theme = r#"
schema_version = 1
id = "nord"
name = "Nord"
version = "1.0.0"
kind = "theme-pack"
publisher = "me"
"#;
        let parsed = parse_manifest(theme).expect("a data-only theme manifest parses");
        assert_eq!(parsed.kind, PluginKind::ThemePack);

        for executable in [
            "\n[runtime]\ncommand = \"steal-secrets\"\n",
            "\n[capabilities]\nnetwork = [\"evil.example:443\"]\n",
            r#"
[ui]
schema_version = 1
[ui.compatibility]
protocol = ">=1.0,<2.0"
sdk = "^1.0"
[ui.entrypoints]
shared = "dist/theme.js"
"#,
        ] {
            assert!(matches!(
                parse_manifest(&format!("{theme}{executable}")),
                Err(ManifestError::ThemeExecutionForbidden { .. })
            ));
        }
    }

    #[test]
    fn ui_component_kind_uses_only_the_governed_ui_worker() {
        let component = manifest_with_ui(r#"shared = "dist/ui/shared.js""#, "")
            .replace("kind = \"native-process\"", "kind = \"ui-component\"")
            .replace(
                r#"[runtime]
command = "codypendent-plugin-github"
protocol = "mcp-stdio"
working_directory = "isolated"

[capabilities]
filesystem_read = []
filesystem_write = []
network = ["api.github.com:443", "uploads.github.com:443"]
secrets = ["github-token"]
subprocess = false
"#,
                "",
            );
        let parsed = parse_manifest(&component).expect("UI-only package has a worker story");
        assert_eq!(parsed.kind, PluginKind::UiComponent);
        assert!(parsed.runtime.command.is_empty());
        assert!(parsed.ui.is_some());

        let missing_ui = component
            .split("\n[ui]\n")
            .next()
            .expect("prefix exists")
            .to_string();
        assert!(matches!(
            parse_manifest(&missing_ui),
            Err(ManifestError::UiComponentMissingUi)
        ));

        let with_runtime = format!("{component}\n[runtime]\ncommand = \"native-backdoor\"\n");
        assert!(matches!(
            parse_manifest(&with_runtime),
            Err(ManifestError::UiComponentExecutionForbidden { .. })
        ));
    }
}
