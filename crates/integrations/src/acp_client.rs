//! ACP (Agent Client Protocol) *client* — the inverse of `acp.rs`.
//!
//! `acp.rs` is the SERVER role (Codypendent serves ACP to Zed). This module is
//! the CLIENT/host role: Codypendent spawns an external ACP agent
//! (`gemini --acp`, `npx @agentclientprotocol/claude-agent-acp`, ...), does the
//! initialize/session handshake, delegates a run's objective as an ACP prompt,
//! and maps the agent's streamed `session/update`s onto Codypendent's existing
//! `EventBody` model. The agent owns its model catalog — the handshake's
//! `config_options` advertise it ([`AcpClient::discovered_models`]), and a
//! profile pinned to one of those models switches the session with
//! `session/set_config_option` ([`AcpClient::set_model`]); nothing is ever
//! guessed on the agent's behalf.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use agent_client_protocol::schema::v1::{
    ConfigOptionUpdate, ContentBlock, ContentChunk, CurrentModeUpdate, SessionConfigKind,
    SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelect,
    SessionConfigSelectOptions, SessionUpdate, ToolCall, ToolCallContent, ToolCallStatus,
    ToolCallUpdate,
};
// The ACP *client* surface (see the client section below). `ContentBlock` above
// is shared with the Task 6 mapping; the rest are client-only.
use agent_client_protocol::schema::v1::{
    CancelNotification, InitializeRequest, InitializeResponse, McpServer as WireMcpServer,
    McpServerStdio, NewSessionRequest, NewSessionResponse,
    PermissionOption as WirePermissionOption, PermissionOptionKind, PromptRequest,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionConfigId, SessionConfigOptionValue, SessionNotification,
    SetSessionConfigOptionRequest, StopReason, TextContent,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{
    AcpAgent, AcpAgentConfig, Agent, Client, ConnectTo, ConnectionTo, Lines,
};

use crate::acp::PermissionOption;
use crate::poison::lock_recovering;
use codypendent_protocol::{EventBody, RunId, ToolOutcome};

/// Map one ACP `session/update` payload onto zero or more Codypendent events
/// for the run it belongs to.
///
/// Pure and deterministic (no I/O, no clock): the same `SessionUpdate` always
/// produces the same `Vec<EventBody>`. This takes just the `update` half of
/// the wire `SessionNotification` — never its `session_id` — because mapping
/// the ACP `SessionId` an update arrived on to Codypendent's own `RunId` is
/// the session driver's job (Task 7), not this function's; the caller passes
/// the already-resolved `run_id` in.
///
/// ACP updates with no Codypendent `EventBody` equivalent produce no events
/// rather than a fabricated one — additive, so an ACP-backed turn renders
/// from exactly the same event vocabulary as a native one:
/// - `UserMessageChunk` echoes the user's own prompt back; it is not model
///   output.
/// - `Plan`, `AvailableCommandsUpdate`, and `SessionInfoUpdate` are ACP
///   session/UI concepts with no Codypendent parallel.
/// - `UsageUpdate` carries token/cost accounting; turning it into an
///   `EventBody::BudgetWarning` would fabricate a threshold breach that never
///   happened — the same cost-honesty rule that keeps the provider catalog's
///   cost metadata display-only and out of any budget sum.
///
/// `CurrentModeUpdate` and `ConfigOptionUpdate` DO map — to run-scoped
/// `NoteAppended` events — so an agent-initiated model or mode switch surfaces
/// in the transcript instead of silently changing what the run executes on.
///
/// The inverse of the server-side bridge in `crates/cli/src/acp.rs`.
#[must_use]
pub fn session_update_to_events(update: &SessionUpdate, run_id: RunId) -> Vec<EventBody> {
    match update {
        // The two are NOT the same channel and were merged here until v0.12.2.
        // ACP agents that deliberate out loud sent their reasoning as
        // `AgentThoughtChunk`; flattening it into the reply meant the answer
        // arrived buried under the model's own narration, and there was no way
        // for any client to tell the two apart because the distinction was
        // discarded one line from where it arrived.
        SessionUpdate::AgentMessageChunk(chunk) => model_stream_delta(chunk, run_id, false),
        SessionUpdate::AgentThoughtChunk(chunk) => model_stream_delta(chunk, run_id, true),
        SessionUpdate::ToolCall(tool_call) => tool_started(tool_call, run_id),
        SessionUpdate::ToolCallUpdate(tool_call_update) => tool_completed(tool_call_update, run_id),
        SessionUpdate::CurrentModeUpdate(update) => current_mode_note(update, run_id),
        SessionUpdate::ConfigOptionUpdate(update) => config_option_notes(update, run_id),
        // No Codypendent `EventBody` equivalent (see the doc comment above) —
        // covers `UserMessageChunk`, `Plan`, `AvailableCommandsUpdate`,
        // `SessionInfoUpdate`, `UsageUpdate`, and any variant a future ACP
        // schema bump adds that this build does not know yet (`SessionUpdate`
        // is `#[non_exhaustive]` — RULE 1: unknown wire content is handled
        // safely, not a hard error).
        _ => Vec::new(),
    }
}

/// The agent switched its own session mode mid-turn. Codypendent has no mode
/// event, so this surfaces as a run-scoped note — visible in the transcript,
/// never mistaken for model output.
fn current_mode_note(update: &CurrentModeUpdate, run_id: RunId) -> Vec<EventBody> {
    vec![EventBody::NoteAppended {
        text: format!(
            "ACP agent switched session mode to `{}`",
            update.current_mode_id
        ),
        run_id: Some(run_id),
    }]
}

/// The agent re-announced its session configuration (the full set arrives on
/// every change). Only the Model/Mode selector categories are noted — one note
/// per selector, naming the now-current selection — so an agent-initiated
/// model switch surfaces without spamming a note for every unrelated toggle.
fn config_option_notes(update: &ConfigOptionUpdate, run_id: RunId) -> Vec<EventBody> {
    update
        .config_options
        .iter()
        .filter_map(|option| {
            let label = match option.category {
                Some(SessionConfigOptionCategory::Model) => "model",
                Some(SessionConfigOptionCategory::Mode) => "mode",
                // Other categories (thought level, misc model config, unknown
                // future ones — the enum is `#[non_exhaustive]`) stay silent.
                _ => return None,
            };
            let SessionConfigKind::Select(select) = &option.kind else {
                return None;
            };
            let current = select.current_value.to_string();
            let name = select_entries(select)
                .into_iter()
                .find(|(id, _)| *id == current)
                .map(|(_, name)| name);
            let text = match name {
                Some(name) if name != current => {
                    format!("ACP agent {label} is now `{name}` ({current})")
                }
                _ => format!("ACP agent {label} is now `{current}`"),
            };
            Some(EventBody::NoteAppended {
                text,
                run_id: Some(run_id),
            })
        })
        .collect()
}

/// Flatten a select option's `(value id, display name)` pairs, whether the
/// agent grouped them or not. An options shape this build does not know
/// (`SessionConfigSelectOptions` is `#[non_exhaustive]`) flattens to nothing
/// rather than failing.
fn select_entries(select: &SessionConfigSelect) -> Vec<(String, String)> {
    match &select.options {
        SessionConfigSelectOptions::Ungrouped(options) => options
            .iter()
            .map(|option| (option.value.to_string(), option.name.clone()))
            .collect(),
        SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter())
            .map(|option| (option.value.to_string(), option.name.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

/// A chunk of the agent's reply or internal reasoning, streamed as
/// `EventBody::ModelStreamDelta`. Codypendent has no separate "thinking"
/// event, so both `AgentMessageChunk` and `AgentThoughtChunk` land here — the
/// same event kind the TUI already renders incrementally, so an ACP turn's
/// stream looks identical to a native one. Non-text content (image, audio,
/// resource) and empty text produce no event: there is nothing to append to
/// the transcript.
fn model_stream_delta(chunk: &ContentChunk, run_id: RunId, thought: bool) -> Vec<EventBody> {
    let ContentBlock::Text(text) = &chunk.content else {
        return Vec::new();
    };
    if text.text.is_empty() {
        return Vec::new();
    }
    vec![EventBody::ModelStreamDelta {
        run_id,
        text: text.text.clone(),
        thought,
    }]
}

/// A newly-initiated tool call maps to `EventBody::ToolStarted`. `args_digest`
/// stays empty: the agent built these arguments, not Codypendent's own tool
/// executor, so there is no digest comparable to the native path's
/// `hash_json` (`crates/runtime/src/agent.rs`) to record here — never
/// fabricate one. `label` stays `None` for the same reason: the native path's
/// `crate::tools::tool_label` (in `codypendent-runtime`) derives it from
/// Codypendent's own typed tool arguments, which have no equivalent here — the
/// external agent's `ToolCall` does carry a `locations`/`raw_input` that could
/// plausibly seed a label, but `tool_call.title` (already `tool` above) is
/// already the human-readable summary ACP gives us, so inventing a second,
/// differently-derived label for this one client is left out of scope rather
/// than guessed at.
fn tool_started(tool_call: &ToolCall, run_id: RunId) -> Vec<EventBody> {
    vec![EventBody::ToolStarted {
        run_id,
        tool: tool_call.title.clone(),
        args_digest: String::new(),
        label: None,
    }]
}

/// A tool call update maps to `EventBody::ToolCompleted` only once it reaches
/// a terminal status. `Pending`/`InProgress` — or an update that does not
/// touch `status` at all — is not terminal yet and produces no event (ACP
/// reports progress this way; Codypendent has no "tool progressed" event).
fn tool_completed(update: &ToolCallUpdate, run_id: RunId) -> Vec<EventBody> {
    let outcome = match update.fields.status {
        Some(ToolCallStatus::Completed) => ToolOutcome::Succeeded,
        Some(ToolCallStatus::Failed) => ToolOutcome::Failed {
            message: failure_message(update),
        },
        _ => return Vec::new(),
    };
    vec![EventBody::ToolCompleted {
        run_id,
        tool: tool_label(update),
        outcome,
        artifact: None,
    }]
}

/// The update's own title, else the tool call id it targets — always
/// something, since `tool_call_id` is required on every `ToolCallUpdate`.
fn tool_label(update: &ToolCallUpdate) -> String {
    update
        .fields
        .title
        .clone()
        .unwrap_or_else(|| update.tool_call_id.to_string())
}

/// The first text content block reported alongside a failed tool call, else
/// a generic message. ACP has no field dedicated to "why did this fail"
/// distinct from the call's reported content, so this is the closest real
/// signal to a failure message — never a placeholder when the agent actually
/// told us something.
fn failure_message(update: &ToolCallUpdate) -> String {
    update
        .fields
        .content
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find_map(|item| match item {
            ToolCallContent::Content(content) => match &content.content {
                ContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            },
            _ => None,
        })
        .unwrap_or_else(|| "ACP tool call failed".to_string())
}

// ===========================================================================
// ACP CLIENT — spawn/connect an external agent, handshake, delegate a prompt.
// ===========================================================================
//
// The real `agent-client-protocol` 2.0.0 client API is closure-scoped: one
// `Client.builder()…connect_with(transport, main_fn)` call owns the connection
// for the whole lifetime of `main_fn`, and the agent's streamed `session/update`
// notifications + `session/request_permission` requests are delivered to
// callbacks registered on the *builder* (not to a `Client` trait we implement,
// and there is no `ClientSideConnection` handle — the plan drafted an older
// shape). To still expose a reusable `connect` / `prompt` split, we run that one
// call on a background task and bridge it with channels: `prompt` sends a
// command in, and drains mapped events / permission asks back out to feed the
// caller's [`AcpEventSink`]. `session_update_to_events` (above) stays the single
// translation point for every streamed update.

/// Bounded depth of the command channel feeding the connection driver. A prompt
/// is delegated one at a time (`prompt` takes `&mut self`), so a shallow queue
/// suffices; it only decouples the caller's `send` from the driver's `recv`.
const PROMPT_QUEUE_DEPTH: usize = 8;
/// Bound agent-to-host updates so a noisy external client is backpressured by
/// durable event/approval processing instead of growing the daemon heap.
const PROMPT_EVENT_QUEUE_DEPTH: usize = 256;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Why an ACP prompt turn ended. Mirrors [`crate::acp::StopReason`] (the server
/// role's type) but is owned by the client role so the two directions stay
/// independent. The ACP wire distinguishes more terminal reasons than
/// Codypendent models: `max_tokens` / `max_turn_requests` collapse into
/// `EndTurn` (the turn simply ended), and any future variant does too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpStopReason {
    /// The agent finished its turn (ACP `end_turn`, `max_tokens`, `max_turn_requests`).
    EndTurn,
    /// The turn was cancelled (ACP `cancelled`).
    Cancelled,
    /// The agent declined to act on the prompt (ACP `refusal`).
    Refusal,
}

/// A failure in the ACP client.
#[derive(Debug, thiserror::Error)]
pub enum AcpClientError {
    /// An I/O failure on the transport.
    #[error("acp client I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The `initialize` / `session/new` handshake did not complete.
    #[error("acp handshake failed: {0}")]
    Handshake(String),
    /// Delegating or streaming a prompt turn failed.
    #[error("acp prompt failed: {0}")]
    Prompt(String),
}

/// One model advertised by a connected ACP agent — a `session/new`
/// `config_options` entry with `category: "model"`, flattened to what a picker
/// needs. `id` is the agent's own stable model id (the `SessionConfigValueId`
/// that `session/set_config_option` accepts back).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpModel {
    /// The agent's stable id for this model.
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Whether the agent reports this as the currently-selected model.
    pub current: bool,
}

/// One session mode advertised by a connected ACP agent, from the `session/new`
/// `modes` state (or a Mode-category config option when no dedicated state was
/// sent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpMode {
    /// The agent's stable id for this mode.
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Whether the agent reports this as the current mode.
    pub current: bool,
}

/// One authentication method advertised in the agent's `initialize` response.
/// Surfaced verbatim so a failed `session/new` can tell the user exactly what
/// the agent asked for, in the agent's own words, rather than an opaque
/// handshake failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpAuthMethod {
    /// The agent's stable id for this method.
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Optional agent-provided instructions.
    pub description: Option<String>,
}

/// A launch spec for one MCP stdio server, forwarded to the agent in
/// `session/new`'s `mcp_servers` so an external agent inherits the same tool
/// servers a native run would see. Deliberately carries NO environment pairs:
/// `mcp.toml`'s `env` is the operator's channel for secrets, and secrets never
/// cross into an external vendor process (see [`forwardable_mcp_servers`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpMcpServer {
    /// Server name (the `mcp.toml` server id).
    pub name: String,
    /// The executable to spawn.
    pub command: String,
    /// Arguments to the executable.
    pub args: Vec<String>,
}

/// Options applied to the ACP session created during the handshake.
#[derive(Debug, Clone, Default)]
pub struct AcpSessionOptions {
    /// MCP servers the agent should connect to (`session/new.mcpServers`).
    pub mcp_servers: Vec<AcpMcpServer>,
}

/// Read `mcp.toml` at `mcp_toml` and derive the launch specs safe to forward to
/// an external ACP agent: name + command + args only, so a delegated session
/// reaches the same tool servers a native run would.
///
/// Two classes of server are withheld rather than forwarded half-configured:
/// one declaring explicit `env` pairs (that is the operator's channel for
/// secrets, and secrets never cross into an external vendor process), and one
/// declaring `inherit_environment = false` (the external agent launches it, so
/// the hermetic environment that server asked for cannot be promised here).
///
/// Forwarding is ON by default; `[acp] forward_mcp_servers = false` in
/// `mcp.toml` — a table the MCP loader itself ignores — turns it off. A missing
/// or malformed file forwards nothing: the daemon already reports the parse
/// error loudly at boot, so this stays quiet rather than double-reporting.
#[must_use]
pub fn forwardable_mcp_servers(mcp_toml: &std::path::Path) -> Vec<AcpMcpServer> {
    #[derive(Deserialize)]
    struct ForwardFile {
        #[serde(default)]
        acp: ForwardTable,
    }
    #[derive(Default, Deserialize)]
    struct ForwardTable {
        #[serde(default)]
        forward_mcp_servers: Option<bool>,
    }

    // The validated loader owns the `[[server]]` shape; this only reads the
    // one extra opt-out key off the same file.
    let Ok(config) = crate::mcp::load_mcp_config(mcp_toml) else {
        return Vec::new();
    };
    let forward = std::fs::read_to_string(mcp_toml)
        .ok()
        .and_then(|text| toml::from_str::<ForwardFile>(&text).ok())
        .and_then(|file| file.acp.forward_mcp_servers)
        .unwrap_or(true);
    if !forward {
        return Vec::new();
    }
    config
        .servers
        .into_iter()
        .filter(|server| server.env.is_empty() && server.inherit_environment)
        .map(|server| AcpMcpServer {
            name: server.name,
            command: server.command,
            args: server.args,
        })
        .collect()
}

/// What the handshake taught us about the agent, shared between the connection
/// driver (which populates it before signalling ready and refreshes it on
/// `ConfigOptionUpdate` / `set_config_option` responses) and the
/// [`AcpClient`]'s accessors.
#[derive(Debug, Clone, Default)]
struct AcpDiscovery {
    auth_methods: Vec<AcpAuthMethod>,
    models: Vec<AcpModel>,
    modes: Vec<AcpMode>,
    /// The `SessionConfigId` of the Model-category selector, kept so
    /// [`AcpClient::set_model`] can target it.
    model_config_id: Option<String>,
    /// Whether `modes` came from the response's dedicated `modes` state (the
    /// richer source); a Mode-category config option never overwrites it then.
    modes_from_state: bool,
}

impl AcpDiscovery {
    fn apply_initialize(&mut self, response: &InitializeResponse) {
        self.auth_methods = response
            .auth_methods
            .iter()
            .map(|method| AcpAuthMethod {
                id: method.id().to_string(),
                name: method.name().to_string(),
                description: method.description().map(str::to_string),
            })
            .collect();
    }

    fn apply_session(&mut self, response: &NewSessionResponse) {
        if let Some(state) = &response.modes {
            self.modes_from_state = true;
            let current = state.current_mode_id.to_string();
            self.modes = state
                .available_modes
                .iter()
                .map(|mode| AcpMode {
                    current: mode.id.to_string() == current,
                    id: mode.id.to_string(),
                    name: mode.name.clone(),
                })
                .collect();
        }
        if let Some(options) = &response.config_options {
            self.apply_config_options(options);
        }
    }

    /// Fold a full config-option set (from `session/new`, a
    /// `set_config_option` response, or a `ConfigOptionUpdate`) into the
    /// discovery state. Every arrival carries the COMPLETE current set, so the
    /// derived selectors are rebuilt from the whole slice rather than merged
    /// entry by entry — an agent that drops its model selector leaves no stale
    /// model list behind to pin a run to.
    fn apply_config_options(&mut self, options: &[SessionConfigOption]) {
        let mut models = Vec::new();
        let mut model_config_id = None;
        let mut modes = Vec::new();
        for option in options {
            let SessionConfigKind::Select(select) = &option.kind else {
                continue;
            };
            let current = select.current_value.to_string();
            let entries = || {
                select_entries(select)
                    .into_iter()
                    .map(|(id, name)| (id == current, id, name))
            };
            match option.category {
                Some(SessionConfigOptionCategory::Model) => {
                    model_config_id = Some(option.id.to_string());
                    models = entries()
                        .map(|(current, id, name)| AcpModel { id, name, current })
                        .collect();
                }
                Some(SessionConfigOptionCategory::Mode) => {
                    modes = entries()
                        .map(|(current, id, name)| AcpMode { id, name, current })
                        .collect();
                }
                // ModelConfig/ThoughtLevel/unknown categories (the enum is
                // `#[non_exhaustive]`) are not model discovery.
                _ => {}
            }
        }
        self.models = models;
        self.model_config_id = model_config_id;
        // `session/new`'s dedicated `modes` state is the richer source; a
        // Mode-category config option never overwrites it.
        if !self.modes_from_state {
            self.modes = modes;
        }
    }

    fn apply_current_mode(&mut self, mode_id: &str) {
        for mode in &mut self.modes {
            mode.current = mode.id == mode_id;
        }
    }
}

/// A parenthesized suffix for `session/new` failures naming the agent's
/// advertised authentication methods, in the agent's own words — turning an
/// opaque "session/new failed" into a message that says what to go do. Empty
/// when the agent advertised none: never fabricate a remedy it did not offer.
fn auth_methods_hint(methods: &[AcpAuthMethod]) -> String {
    if methods.is_empty() {
        return String::new();
    }
    let methods = methods
        .iter()
        .map(|method| match &method.description {
            Some(description) => format!("{} — {description}", method.name),
            None => method.name.clone(),
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(" (the agent advertises authentication: {methods})")
}

/// Receives the events an ACP turn produces and answers the agent's permission
/// requests. The daemon implements this to fan mapped events into a run's ledger
/// and route permissions through the existing durable approval broker; tests
/// implement it to record events and auto-answer.
#[async_trait]
pub trait AcpEventSink: Send {
    /// A Codypendent event mapped from a streamed `session/update`.
    async fn on_event(&mut self, event: EventBody);

    /// Answer an ACP `session/request_permission`: return the chosen `optionId`,
    /// or `None` to cancel. `tool_call` is the agent's pending call as opaque
    /// JSON; `options` are the choices in the server role's [`PermissionOption`]
    /// shape (reused so both ACP directions speak one permission vocabulary).
    async fn on_permission(
        &mut self,
        tool_call: Value,
        options: Vec<PermissionOption>,
    ) -> Option<String>;
}

/// A connected ACP agent session. Dropping it closes the command channel, which
/// ends the driver's `main_fn`; the connection then shuts down and — for the
/// spawn path — [`AcpAgent`] tears down the child process group (SIGKILL on
/// Unix, covering `npx`/`uvx` wrapper descendants).
pub struct AcpClient {
    commands: mpsc::Sender<PromptCommand>,
    /// The driver task owns the live `agent_client_protocol` connection. Held so
    /// it is not detached mid-handshake; it exits on its own once `commands`
    /// (its only sender) is dropped.
    driver: JoinHandle<Result<(), AcpClientError>>,
    /// Handshake discovery (auth methods, models, modes), kept live by the
    /// driver as the agent announces config changes.
    discovery: Arc<Mutex<AcpDiscovery>>,
}

/// A clonable handle that asks the agent to wind down the in-flight prompt
/// turn (`session/cancel`) without borrowing the [`AcpClient`] — so a caller
/// can hold it across the same `select!` that polls
/// [`prompt`](AcpClient::prompt). Best-effort: a closed connection means there
/// is nothing left to cancel.
#[derive(Clone)]
pub struct AcpCancelHandle {
    commands: mpsc::Sender<PromptCommand>,
}

impl AcpCancelHandle {
    /// Send the ACP `session/cancel` notification for the current turn. The
    /// agent then finishes the in-flight `session/prompt` with the
    /// wire-correct `cancelled` stop reason (or the caller times out and tears
    /// the process down).
    pub async fn cancel(&self) {
        let _ = self.commands.send(PromptCommand::Cancel).await;
    }
}

impl Drop for AcpClient {
    fn drop(&mut self) {
        // `JoinHandle` normally detaches on drop. An ACP prompt may still be
        // awaiting the external agent, so detaching would also retain its child
        // process. Aborting drops the SDK transport's process-group guard.
        self.driver.abort();
    }
}

impl AcpClient {
    /// Connect over an existing byte transport (`reader` = the agent's output,
    /// `writer` = the agent's input) and complete the ACP handshake. Generic over
    /// the stream halves so an in-memory `tokio::io::duplex` can drive the whole
    /// client in tests; [`spawn`](Self::spawn) is the production entry point.
    pub async fn connect<R, W>(reader: R, writer: W, cwd: &str) -> Result<AcpClient, AcpClientError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::connect_with(reader, writer, cwd, AcpSessionOptions::default()).await
    }

    /// [`connect`](Self::connect) with explicit [`AcpSessionOptions`] (e.g. MCP
    /// servers to forward into `session/new`).
    pub async fn connect_with<R, W>(
        reader: R,
        writer: W,
        cwd: &str,
        options: AcpSessionOptions,
    ) -> Result<AcpClient, AcpClientError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::connect_transport(tokio_transport(reader, writer), cwd, options).await
    }

    /// Spawn `command args` (with `env`) as a child ACP agent and connect over
    /// its stdio. `env` carries the provider config's environment (secrets are
    /// referenced by NAME upstream and resolved into `env` by the caller — never
    /// stored here).
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        cwd: &str,
    ) -> Result<AcpClient, AcpClientError> {
        Self::spawn_with(command, args, env, cwd, AcpSessionOptions::default()).await
    }

    /// [`spawn`](Self::spawn) with explicit [`AcpSessionOptions`] (e.g. MCP
    /// servers to forward into `session/new`).
    pub async fn spawn_with(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        cwd: &str,
        options: AcpSessionOptions,
    ) -> Result<AcpClient, AcpClientError> {
        let config = AcpAgentConfig::new(command)
            .args(args.iter().cloned())
            .envs(env.clone());
        Self::connect_transport(AcpAgent::new(config), cwd, options).await
    }

    async fn connect_transport<T>(
        transport: T,
        cwd: &str,
        options: AcpSessionOptions,
    ) -> Result<AcpClient, AcpClientError>
    where
        T: ConnectTo<Client> + Send + 'static,
    {
        let cwd = std::fs::canonicalize(cwd).map_err(AcpClientError::Io)?;
        if !cwd.is_dir() {
            return Err(AcpClientError::Handshake(format!(
                "session working directory is not a directory: {}",
                cwd.display()
            )));
        }
        let (ready_tx, ready_rx) = oneshot::channel();
        let (commands, command_rx) = mpsc::channel(PROMPT_QUEUE_DEPTH);
        let discovery = Arc::new(Mutex::new(AcpDiscovery::default()));
        let driver = tokio::spawn(run_connection(
            transport,
            cwd,
            options,
            Arc::clone(&discovery),
            ready_tx,
            command_rx,
        ));
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, ready_rx).await {
            Err(_) => {
                driver.abort();
                Err(AcpClientError::Handshake(format!(
                    "agent did not initialize within {} seconds",
                    HANDSHAKE_TIMEOUT.as_secs()
                )))
            }
            Ok(Ok(Ok(()))) => Ok(AcpClient {
                commands,
                driver,
                discovery,
            }),
            // The driver reported a specific handshake failure before returning.
            Ok(Ok(Err(error))) => {
                driver.abort();
                Err(error)
            }
            // The driver dropped `ready_tx` without signalling (e.g. the
            // transport itself failed before `main_fn` ran): recover its error.
            Ok(Err(_)) => match driver.await {
                Ok(Err(error)) => Err(error),
                Ok(Ok(())) => Err(AcpClientError::Handshake(
                    "acp connection closed before completing the handshake".to_string(),
                )),
                Err(join_error) => Err(AcpClientError::Handshake(format!(
                    "acp connection task failed: {join_error}"
                ))),
            },
        }
    }

    /// Delegate `objective` to the agent as one ACP `session/prompt` turn,
    /// feeding every mapped `session/update` event and permission request to
    /// `sink`, and returning why the turn ended. The turn runs on whatever
    /// model the session currently has — the agent's default, or the one
    /// pinned earlier via [`set_model`](Self::set_model).
    pub async fn prompt(
        &mut self,
        objective: &str,
        run_id: RunId,
        sink: &mut dyn AcpEventSink,
    ) -> Result<AcpStopReason, AcpClientError> {
        let (events, mut incoming) = mpsc::channel(PROMPT_EVENT_QUEUE_DEPTH);
        self.commands
            .send(PromptCommand::Prompt {
                objective: objective.to_string(),
                run_id,
                events,
            })
            .await
            .map_err(|_| {
                AcpClientError::Prompt("acp connection is no longer running".to_string())
            })?;

        while let Some(message) = incoming.recv().await {
            match message {
                PromptOut::Event(event) => sink.on_event(event).await,
                PromptOut::Permission {
                    tool_call,
                    options,
                    reply,
                } => {
                    let choice = sink.on_permission(tool_call, options).await;
                    // The driver's permission callback awaits this; if it is gone
                    // the turn is already ending, so a failed send is harmless.
                    let _ = reply.send(choice);
                }
                PromptOut::Done(stop) => return Ok(stop),
                PromptOut::Failed(reason) => return Err(AcpClientError::Prompt(reason)),
            }
        }
        Err(AcpClientError::Prompt(
            "acp connection closed before the prompt completed".to_string(),
        ))
    }

    /// The models the agent advertised over the session-config handshake, in
    /// the agent's own order. Empty when the agent predates `config_options`
    /// or exposes no model selector — the agent's default model then applies,
    /// exactly the pre-discovery behavior.
    #[must_use]
    pub fn discovered_models(&self) -> Vec<AcpModel> {
        lock_recovering(&self.discovery).models.clone()
    }

    /// The session modes the agent advertised, in the agent's own order.
    #[must_use]
    pub fn discovered_modes(&self) -> Vec<AcpMode> {
        lock_recovering(&self.discovery).modes.clone()
    }

    /// The authentication methods the agent advertised in `initialize`.
    #[must_use]
    pub fn auth_methods(&self) -> Vec<AcpAuthMethod> {
        lock_recovering(&self.discovery).auth_methods.clone()
    }

    /// Switch the session to one of the agent's own models via
    /// `session/set_config_option`, targeting the Model-category selector the
    /// handshake advertised. `model_id` goes to the agent verbatim — the agent
    /// is the authority on its catalog, so an unknown id fails with the
    /// agent's own error rather than a local guess. Fails when the agent
    /// advertised no model selector at all.
    pub async fn set_model(&mut self, model_id: &str) -> Result<(), AcpClientError> {
        let config_id = lock_recovering(&self.discovery)
            .model_config_id
            .clone()
            .ok_or_else(|| {
                AcpClientError::Prompt(format!(
                    "the agent advertises no model selector; cannot select `{model_id}`"
                ))
            })?;
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(PromptCommand::SetConfigOption {
                config_id,
                value: model_id.to_string(),
                reply,
            })
            .await
            .map_err(|_| {
                AcpClientError::Prompt("acp connection is no longer running".to_string())
            })?;
        match answer.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(reason)) => Err(AcpClientError::Prompt(reason)),
            Err(_) => Err(AcpClientError::Prompt(
                "acp connection closed before set_config_option resolved".to_string(),
            )),
        }
    }

    /// A clonable [`AcpCancelHandle`] for graceful turn cancellation. Taken
    /// BEFORE calling [`prompt`](Self::prompt) so the handle can be used while
    /// the prompt future holds the `&mut self` borrow.
    #[must_use]
    pub fn cancel_handle(&self) -> AcpCancelHandle {
        AcpCancelHandle {
            commands: self.commands.clone(),
        }
    }
}

/// A prompt turn's live routing: which run the streamed updates belong to and
/// where to push the mapped events / permission asks. The driver installs it for
/// the duration of one `session/prompt`; the notification and permission
/// callbacks read it to reach the in-flight [`AcpClient::prompt`] call.
#[derive(Clone)]
struct ActivePrompt {
    run_id: RunId,
    events: mpsc::Sender<PromptOut>,
}

/// A command from an [`AcpClient`] handle (or [`AcpCancelHandle`]) to its
/// connection driver.
enum PromptCommand {
    Prompt {
        objective: String,
        run_id: RunId,
        events: mpsc::Sender<PromptOut>,
    },
    /// `session/set_config_option` — switch a select option (the model) and
    /// report back whether the agent accepted it.
    SetConfigOption {
        config_id: String,
        value: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// `session/cancel` — ask the agent to wind down the in-flight turn. The
    /// driver services this DURING a prompt (the whole point); with no turn in
    /// flight it is still forwarded, which the agent treats as a no-op.
    Cancel,
}

/// One item streamed from the connection driver back to an in-flight `prompt`.
enum PromptOut {
    /// A Codypendent event mapped from a `session/update`.
    Event(EventBody),
    /// A permission request awaiting the sink's choice.
    Permission {
        tool_call: Value,
        options: Vec<PermissionOption>,
        reply: oneshot::Sender<Option<String>>,
    },
    /// The turn resolved with this stop reason.
    Done(AcpStopReason),
    /// The turn failed; carries a human-readable reason.
    Failed(String),
}

/// Drive one ACP connection: run the builder's `connect_with`, complete the
/// handshake (signalling `ready` and populating `discovery` from the
/// `initialize` / `session/new` responses), then service commands until every
/// [`AcpClient`] handle drops and `commands` closes.
async fn run_connection<T>(
    transport: T,
    cwd: PathBuf,
    options: AcpSessionOptions,
    discovery: Arc<Mutex<AcpDiscovery>>,
    ready: oneshot::Sender<Result<(), AcpClientError>>,
    mut commands: mpsc::Receiver<PromptCommand>,
) -> Result<(), AcpClientError>
where
    T: ConnectTo<Client> + Send + 'static,
{
    let active: Arc<Mutex<Option<ActivePrompt>>> = Arc::new(Mutex::new(None));

    let outcome = Client
        .builder()
        .name("codypendent-acp-client")
        // Every streamed `session/update` maps through the single Task 6 point.
        // Config/mode announcements ALSO refresh the shared discovery state, so
        // `discovered_models()` stays truthful after an agent-initiated switch.
        .on_receive_notification(
            {
                let active = Arc::clone(&active);
                let discovery = Arc::clone(&discovery);
                async move |notification: SessionNotification, _cx| {
                    match &notification.update {
                        SessionUpdate::ConfigOptionUpdate(update) => {
                            lock_recovering(&discovery)
                                .apply_config_options(&update.config_options);
                        }
                        SessionUpdate::CurrentModeUpdate(update) => {
                            lock_recovering(&discovery)
                                .apply_current_mode(&update.current_mode_id.to_string());
                        }
                        _ => {}
                    }
                    let current = lock_recovering(&active).clone();
                    if let Some(prompt) = current {
                        for event in session_update_to_events(&notification.update, prompt.run_id) {
                            let _ = prompt.events.send(PromptOut::Event(event)).await;
                        }
                    }
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        // A permission request is routed to the sink and answered with its choice.
        .on_receive_request(
            {
                let active = Arc::clone(&active);
                async move |request: RequestPermissionRequest, responder, _cx| {
                    let outcome = resolve_permission(&active, request).await;
                    responder.respond(RequestPermissionResponse::new(outcome))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport, move |cx: ConnectionTo<Agent>| async move {
            // Handshake. The responses are KEPT: `initialize` advertises auth
            // methods (actionable errors), `session/new` advertises the
            // agent's models/modes (the rubric's automatic model discovery).
            match cx
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await
            {
                Ok(response) => lock_recovering(&discovery).apply_initialize(&response),
                Err(error) => {
                    let _ = ready.send(Err(AcpClientError::Handshake(format!(
                        "initialize failed: {error}"
                    ))));
                    return Err(error);
                }
            }
            let mcp_servers = options
                .mcp_servers
                .iter()
                .map(|server| {
                    WireMcpServer::Stdio(
                        McpServerStdio::new(server.name.clone(), server.command.clone())
                            .args(server.args.clone()),
                    )
                })
                .collect::<Vec<_>>();
            let session_id = match cx
                .send_request(NewSessionRequest::new(cwd).mcp_servers(mcp_servers))
                .block_task()
                .await
            {
                Ok(response) => {
                    let mut slot = lock_recovering(&discovery);
                    slot.apply_session(&response);
                    response.session_id
                }
                Err(error) => {
                    // An auth-gated agent fails exactly here; name its
                    // advertised remedies instead of an opaque failure.
                    let hint = {
                        let slot = lock_recovering(&discovery);
                        auth_methods_hint(&slot.auth_methods)
                    };
                    let _ = ready.send(Err(AcpClientError::Handshake(format!(
                        "session/new failed: {error}{hint}"
                    ))));
                    return Err(error);
                }
            };
            let _ = ready.send(Ok(()));

            // One delegated prompt turn per command. The `active` slot lets the
            // concurrently-dispatched update/permission callbacks reach this
            // turn's event channel and run id while the request is in flight.
            while let Some(command) = commands.recv().await {
                match command {
                    PromptCommand::Prompt {
                        objective,
                        run_id,
                        events,
                    } => {
                        *lock_recovering(&active) = Some(ActivePrompt {
                            run_id,
                            events: events.clone(),
                        });
                        let request = cx
                            .send_request(PromptRequest::new(
                                session_id.clone(),
                                vec![ContentBlock::Text(TextContent::new(objective))],
                            ))
                            .block_task();
                        let mut request = std::pin::pin!(request);
                        // Keep servicing commands while the turn is in flight —
                        // a `Cancel` must reach the agent as `session/cancel`
                        // DURING the prompt or graceful cancellation could
                        // never work.
                        let mut commands_open = true;
                        let result = loop {
                            if !commands_open {
                                break request.as_mut().await;
                            }
                            tokio::select! {
                                result = request.as_mut() => break result,
                                command = commands.recv() => match command {
                                    Some(PromptCommand::Cancel) => {
                                        let _ = cx.send_notification(CancelNotification::new(
                                            session_id.clone(),
                                        ));
                                    }
                                    Some(PromptCommand::SetConfigOption { reply, .. }) => {
                                        let _ = reply.send(Err(
                                            "a prompt turn is in flight".to_string(),
                                        ));
                                    }
                                    Some(PromptCommand::Prompt { events, .. }) => {
                                        // `prompt` takes `&mut self`, so a second
                                        // in-flight turn cannot happen through the
                                        // public API; refuse defensively anyway.
                                        let _ = events
                                            .send(PromptOut::Failed(
                                                "a prompt turn is already in flight".to_string(),
                                            ))
                                            .await;
                                    }
                                    None => commands_open = false,
                                },
                            }
                        };
                        *lock_recovering(&active) = None;
                        let resolved = match result {
                            Ok(response) => PromptOut::Done(map_stop_reason(response.stop_reason)),
                            Err(error) => {
                                PromptOut::Failed(format!("session/prompt failed: {error}"))
                            }
                        };
                        let _ = events.send(resolved).await;
                    }
                    PromptCommand::SetConfigOption {
                        config_id,
                        value,
                        reply,
                    } => {
                        let result = cx
                            .send_request(SetSessionConfigOptionRequest::new(
                                session_id.clone(),
                                SessionConfigId::new(config_id),
                                SessionConfigOptionValue::value_id(value),
                            ))
                            .block_task()
                            .await;
                        let outcome = match result {
                            Ok(response) => {
                                // The response carries the full refreshed set;
                                // fold it so `discovered_models()` reports the
                                // new current selection.
                                lock_recovering(&discovery)
                                    .apply_config_options(&response.config_options);
                                Ok(())
                            }
                            Err(error) => Err(format!("session/set_config_option failed: {error}")),
                        };
                        let _ = reply.send(outcome);
                    }
                    PromptCommand::Cancel => {
                        let _ = cx.send_notification(CancelNotification::new(session_id.clone()));
                    }
                }
            }
            Ok(())
        })
        .await;

    outcome.map_err(|error| AcpClientError::Prompt(format!("acp connection ended: {error}")))
}

/// Resolve one `session/request_permission` by asking the in-flight prompt's
/// sink (via the `active` slot) and translating its answer to the ACP outcome.
/// With no active prompt, or if the prompt is gone, the request is cancelled.
async fn resolve_permission(
    active: &Arc<Mutex<Option<ActivePrompt>>>,
    request: RequestPermissionRequest,
) -> RequestPermissionOutcome {
    let current = lock_recovering(active).clone();
    let Some(prompt) = current else {
        return RequestPermissionOutcome::Cancelled;
    };
    // The agent's `ToolCallUpdate` is passed to the sink as opaque JSON — the
    // approval flow decides on it; we never re-model it.
    let tool_call = serde_json::to_value(&request.tool_call).unwrap_or(Value::Null);
    let options = request
        .options
        .iter()
        .map(to_permission_option)
        .collect::<Vec<_>>();
    let (reply, answer) = oneshot::channel();
    if prompt
        .events
        .send(PromptOut::Permission {
            tool_call,
            options,
            reply,
        })
        .await
        .is_err()
    {
        return RequestPermissionOutcome::Cancelled;
    }
    match answer.await {
        Ok(Some(option_id)) => {
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id))
        }
        _ => RequestPermissionOutcome::Cancelled,
    }
}

/// Project the ACP wire [`WirePermissionOption`] onto the server role's
/// [`PermissionOption`] so the whole codebase answers permissions in one shape.
fn to_permission_option(option: &WirePermissionOption) -> PermissionOption {
    PermissionOption {
        option_id: option.option_id.to_string(),
        name: option.name.clone(),
        kind: permission_kind_wire(option.kind).to_string(),
    }
}

/// The ACP wire string for a permission-option kind (`#[non_exhaustive]`, so an
/// unknown future kind degrades to `"unknown"` rather than a hard error).
fn permission_kind_wire(kind: PermissionOptionKind) -> &'static str {
    match kind {
        PermissionOptionKind::AllowOnce => "allow_once",
        PermissionOptionKind::AllowAlways => "allow_always",
        PermissionOptionKind::RejectOnce => "reject_once",
        PermissionOptionKind::RejectAlways => "reject_always",
        _ => "unknown",
    }
}

/// Map the ACP `stopReason` onto the client's [`AcpStopReason`]. `max_tokens` and
/// `max_turn_requests` — and any future variant (`StopReason` is
/// `#[non_exhaustive]`) — collapse into `EndTurn`: the turn ended.
fn map_stop_reason(reason: StopReason) -> AcpStopReason {
    match reason {
        StopReason::EndTurn | StopReason::MaxTokens | StopReason::MaxTurnRequests => {
            AcpStopReason::EndTurn
        }
        StopReason::Cancelled => AcpStopReason::Cancelled,
        StopReason::Refusal => AcpStopReason::Refusal,
        _ => AcpStopReason::EndTurn,
    }
}

/// Build the crate's line transport from a tokio reader/writer pair without any
/// `tokio-util` compat shim (the workspace has only `futures` + `tokio`, and ACP
/// is the sole new dependency): newline-framed JSON in, newline-framed JSON out.
fn tokio_transport<R, W>(
    reader: R,
    writer: W,
) -> Lines<
    impl futures::Sink<String, Error = std::io::Error> + Send + 'static,
    impl futures::Stream<Item = std::io::Result<String>> + Send + 'static,
>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let incoming = futures::stream::unfold(Some(BufReader::new(reader)), |state| async move {
        let mut reader = state?;
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => None,                           // clean EOF ends the stream
            Ok(_) => Some((Ok(line), Some(reader))), // one framed message
            Err(error) => Some((Err(error), None)),  // surface once, then stop
        }
    });
    let outgoing = futures::sink::unfold(writer, |mut writer, line: String| async move {
        let mut bytes = line.into_bytes();
        bytes.push(b'\n');
        writer.write_all(&bytes).await?;
        writer.flush().await?;
        Ok::<_, std::io::Error>(writer)
    });
    Lines::new(outgoing, incoming)
}

#[cfg(test)]
mod mapping_tests {
    use super::*;
    use agent_client_protocol::schema::v1::{
        Content, ImageContent, Plan, SessionConfigSelectOption, TextContent, ToolCallUpdateFields,
    };

    fn rid() -> RunId {
        RunId::new()
    }

    /// A `ContentChunk` wrapping a single text block, the common case for
    /// both `AgentMessageChunk` and `AgentThoughtChunk`.
    fn text_chunk(text: &str) -> ContentChunk {
        ContentChunk::new(ContentBlock::Text(TextContent::new(text)))
    }

    fn permission_request() -> RequestPermissionRequest {
        serde_json::from_value(serde_json::json!({
            "sessionId": "session-1",
            "toolCall": { "toolCallId": "call-1" },
            "options": [],
        }))
        .expect("a minimal v1 request deserializes")
    }

    /// Poison the active-prompt slot the only way it can be poisoned — a panic
    /// while holding it — and prove BOTH directions still hold: no in-flight
    /// turn still cancels (never a silent grant), and a live turn still reaches
    /// its sink. With `.expect(...)` back in place, both calls panic instead,
    /// and the panic propagates into the connection's request callback.
    #[tokio::test]
    async fn a_poisoned_active_prompt_slot_still_routes_permission_requests() {
        let active: Arc<Mutex<Option<ActivePrompt>>> = Arc::new(Mutex::new(None));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = active.lock().expect("fresh mutex");
            panic!("a holder panicked");
        }));
        assert!(active.is_poisoned());

        // No turn in flight: the request is cancelled, not granted.
        assert!(matches!(
            resolve_permission(&active, permission_request()).await,
            RequestPermissionOutcome::Cancelled
        ));

        // A turn in flight: the request still reaches the sink, which answers.
        let (events, mut inbox) = mpsc::channel(4);
        *active.lock().unwrap_or_else(|e| e.into_inner()) = Some(ActivePrompt {
            run_id: rid(),
            events,
        });
        let resolved = tokio::spawn({
            let active = Arc::clone(&active);
            async move { resolve_permission(&active, permission_request()).await }
        });
        match inbox.recv().await.expect("the sink was asked") {
            PromptOut::Permission { reply, .. } => {
                let _ = reply.send(None);
            }
            _ => panic!("expected a permission request on the sink channel"),
        }
        assert!(matches!(
            resolved.await.unwrap(),
            RequestPermissionOutcome::Cancelled
        ));
    }

    #[test]
    fn agent_message_chunk_maps_to_a_model_stream_delta() {
        let run_id = rid();
        let update = SessionUpdate::AgentMessageChunk(text_chunk("hello"));
        let events = session_update_to_events(&update, run_id);
        assert_eq!(
            events,
            vec![EventBody::ModelStreamDelta {
                run_id,
                text: "hello".to_string(),
                thought: false
            }]
        );
    }

    #[test]
    /// Reasoning streams as text, but MARKED — the distinction ACP draws must
    /// survive the bridge. Merging the two arms is what buried the answer under
    /// the model's own narration, so this asserts the flag and not merely that
    /// some delta came out.
    fn agent_thought_chunk_streams_as_marked_reasoning() {
        let run_id = rid();
        let update = SessionUpdate::AgentThoughtChunk(text_chunk("thinking"));
        let events = session_update_to_events(&update, run_id);
        assert_eq!(
            events,
            vec![EventBody::ModelStreamDelta {
                run_id,
                text: "thinking".to_string(),
                thought: true
            }]
        );
    }

    #[test]
    fn agent_message_chunk_with_empty_text_produces_no_events() {
        let run_id = rid();
        let update = SessionUpdate::AgentMessageChunk(text_chunk(""));
        assert!(session_update_to_events(&update, run_id).is_empty());
    }

    #[test]
    fn agent_message_chunk_with_non_text_content_produces_no_events() {
        let run_id = rid();
        let image = ContentBlock::Image(ImageContent::new("base64data", "image/png"));
        let update = SessionUpdate::AgentMessageChunk(ContentChunk::new(image));
        assert!(session_update_to_events(&update, run_id).is_empty());
    }

    #[test]
    fn tool_call_maps_to_tool_started() {
        let run_id = rid();
        let update = SessionUpdate::ToolCall(ToolCall::new("t1", "read_file"));
        let events = session_update_to_events(&update, run_id);
        assert_eq!(
            events,
            vec![EventBody::ToolStarted {
                run_id,
                tool: "read_file".to_string(),
                args_digest: String::new(),
                label: None,
            }]
        );
    }

    #[test]
    fn completed_tool_call_update_maps_to_tool_completed_succeeded() {
        let run_id = rid();
        let update = SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "t1",
            ToolCallUpdateFields::new()
                .title("read_file")
                .status(ToolCallStatus::Completed),
        ));
        let events = session_update_to_events(&update, run_id);
        assert_eq!(
            events,
            vec![EventBody::ToolCompleted {
                run_id,
                tool: "read_file".to_string(),
                outcome: ToolOutcome::Succeeded,
                artifact: None,
            }]
        );
    }

    #[test]
    fn failed_tool_call_update_maps_to_tool_completed_failed() {
        let run_id = rid();
        let update = SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "shell-1",
            ToolCallUpdateFields::new()
                .title("shell")
                .status(ToolCallStatus::Failed),
        ));
        let events = session_update_to_events(&update, run_id);
        assert!(matches!(
            events.as_slice(),
            [EventBody::ToolCompleted {
                outcome: ToolOutcome::Failed { .. },
                ..
            }]
        ));
    }

    #[test]
    fn failed_tool_call_update_uses_reported_content_as_the_failure_message() {
        let run_id = rid();
        let update = SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "shell-1",
            ToolCallUpdateFields::new()
                .title("shell")
                .status(ToolCallStatus::Failed)
                .content(vec![ToolCallContent::Content(Content::new(
                    "permission denied",
                ))]),
        ));
        let events = session_update_to_events(&update, run_id);
        assert_eq!(
            events,
            vec![EventBody::ToolCompleted {
                run_id,
                tool: "shell".to_string(),
                outcome: ToolOutcome::Failed {
                    message: "permission denied".to_string()
                },
                artifact: None,
            }]
        );
    }

    #[test]
    fn tool_call_update_without_a_title_falls_back_to_the_tool_call_id() {
        let run_id = rid();
        let update = SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "t-42",
            ToolCallUpdateFields::new().status(ToolCallStatus::Completed),
        ));
        let events = session_update_to_events(&update, run_id);
        assert_eq!(
            events,
            vec![EventBody::ToolCompleted {
                run_id,
                tool: "t-42".to_string(),
                outcome: ToolOutcome::Succeeded,
                artifact: None,
            }]
        );
    }

    #[test]
    fn an_incomplete_tool_call_update_produces_no_events() {
        let run_id = rid();
        let in_progress = SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "t1",
            ToolCallUpdateFields::new()
                .title("x")
                .status(ToolCallStatus::InProgress),
        ));
        assert!(session_update_to_events(&in_progress, run_id).is_empty());

        let no_status_change = SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
            "t1",
            ToolCallUpdateFields::new().title("renamed"),
        ));
        assert!(session_update_to_events(&no_status_change, run_id).is_empty());
    }

    #[test]
    fn plan_update_produces_no_events() {
        let run_id = rid();
        let update = SessionUpdate::Plan(Plan::new(vec![]));
        assert!(session_update_to_events(&update, run_id).is_empty());
    }

    #[test]
    fn user_message_chunk_produces_no_events() {
        let run_id = rid();
        let update = SessionUpdate::UserMessageChunk(text_chunk("what does this do?"));
        assert!(session_update_to_events(&update, run_id).is_empty());
    }

    #[test]
    fn usage_update_produces_no_events() {
        use agent_client_protocol::schema::v1::UsageUpdate;

        let run_id = rid();
        let update = SessionUpdate::UsageUpdate(UsageUpdate::new(100, 1000));
        assert!(session_update_to_events(&update, run_id).is_empty());
    }

    /// A Model-category `select` option, the shape current ACP carries a model
    /// list in.
    fn model_selector(id: &str, current: &str, options: &[(&str, &str)]) -> SessionConfigOption {
        SessionConfigOption::new(
            id.to_string(),
            "Model",
            SessionConfigKind::Select(SessionConfigSelect::new(
                current.to_string(),
                options
                    .iter()
                    .map(|(value, name)| {
                        SessionConfigSelectOption::new((*value).to_string(), *name)
                    })
                    .collect::<Vec<_>>(),
            )),
        )
        .category(SessionConfigOptionCategory::Model)
    }

    #[test]
    fn current_mode_update_maps_to_a_run_scoped_note() {
        let run_id = rid();
        let update = SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new("review"));
        let events = session_update_to_events(&update, run_id);
        assert!(
            matches!(
                events.as_slice(),
                [EventBody::NoteAppended { text, run_id: Some(run) }]
                    if *run == run_id && text.contains("review")
            ),
            "expected one run-scoped mode note, got {events:?}"
        );
    }

    #[test]
    fn config_option_update_notes_only_the_model_and_mode_selectors() {
        let run_id = rid();
        let update = SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(vec![
            model_selector(
                "model",
                "agent-model-2",
                &[("agent-model-1", "First"), ("agent-model-2", "Second")],
            ),
            // A non-model/mode selector stays silent.
            SessionConfigOption::new(
                "thought",
                "Thought level",
                SessionConfigKind::Select(SessionConfigSelect::new(
                    "high",
                    vec![SessionConfigSelectOption::new("high", "High")],
                )),
            )
            .category(SessionConfigOptionCategory::ThoughtLevel),
            // A boolean toggle is not a selector at all.
            SessionConfigOption::new(
                "verbose",
                "Verbose",
                SessionConfigKind::Boolean(
                    agent_client_protocol::schema::v1::SessionConfigBoolean::new(true),
                ),
            )
            .category(SessionConfigOptionCategory::Model),
        ]));
        let events = session_update_to_events(&update, run_id);
        assert!(
            matches!(
                events.as_slice(),
                [EventBody::NoteAppended { text, run_id: Some(run) }]
                    if *run == run_id
                        && text.contains("model")
                        && text.contains("Second")
                        && text.contains("agent-model-2")
            ),
            "expected exactly one model note naming the new selection, got {events:?}"
        );
    }

    #[test]
    fn grouped_select_options_flatten_for_notes() {
        use agent_client_protocol::schema::v1::SessionConfigSelectGroup;

        let run_id = rid();
        let grouped = SessionConfigOption::new(
            "model",
            "Model",
            SessionConfigKind::Select(SessionConfigSelect::new(
                "agent-model-2",
                vec![SessionConfigSelectGroup::new(
                    "group-1",
                    "Group one",
                    vec![
                        SessionConfigSelectOption::new("agent-model-1", "First"),
                        SessionConfigSelectOption::new("agent-model-2", "Second"),
                    ],
                )],
            )),
        )
        .category(SessionConfigOptionCategory::Model);
        let update = SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(vec![grouped]));
        let events = session_update_to_events(&update, run_id);
        assert!(
            matches!(
                events.as_slice(),
                [EventBody::NoteAppended { text, .. }] if text.contains("Second")
            ),
            "expected the grouped option's display name in the note, got {events:?}"
        );
    }

    #[test]
    fn a_config_option_set_without_a_model_selector_clears_the_discovered_models() {
        // Every arrival is the COMPLETE set, so a dropped selector must not
        // leave a stale model list behind for a run to pin against.
        let mut discovery = AcpDiscovery::default();
        discovery.apply_config_options(&[model_selector(
            "model",
            "agent-model-1",
            &[("agent-model-1", "First")],
        )]);
        assert_eq!(discovery.models.len(), 1);
        assert_eq!(discovery.model_config_id.as_deref(), Some("model"));
        discovery.apply_config_options(&[]);
        assert!(discovery.models.is_empty());
        assert!(discovery.model_config_id.is_none());
    }

    #[test]
    fn a_dedicated_modes_state_outranks_a_mode_category_config_option() {
        use agent_client_protocol::schema::v1::{SessionMode, SessionModeState};

        let mut discovery = AcpDiscovery::default();
        let response = NewSessionResponse::new("s-1")
            .modes(SessionModeState::new(
                "build",
                vec![
                    SessionMode::new("build", "Build"),
                    SessionMode::new("review", "Review"),
                ],
            ))
            .config_options(vec![SessionConfigOption::new(
                "mode",
                "Mode",
                SessionConfigKind::Select(SessionConfigSelect::new(
                    "other",
                    vec![SessionConfigSelectOption::new("other", "Other")],
                )),
            )
            .category(SessionConfigOptionCategory::Mode)]);
        discovery.apply_session(&response);
        assert_eq!(
            discovery
                .modes
                .iter()
                .map(|mode| mode.id.as_str())
                .collect::<Vec<_>>(),
            vec!["build", "review"],
            "the richer `modes` state must win over a Mode-category option"
        );
        assert!(discovery.modes.iter().any(|mode| mode.current));
    }
}

#[cfg(test)]
mod client_tests {
    //! End-to-end tests over a scripted in-process ACP *agent* peer that speaks
    //! the real newline-delimited JSON-RPC 2.0 wire over `tokio::io::duplex`
    //! (mirroring the harness in `crate::acp`'s tests). They assert the handshake
    //! completes, a prompt is delegated, streamed `session/update`s reach
    //! `session_update_to_events`, a `session/request_permission` maps onto the
    //! sink's approval, and — for model discovery — that the handshake's
    //! `config_options` / `authMethods` are captured, `session/set_config_option`
    //! is spoken on the wire, and `session/cancel` reaches the agent mid-turn.

    use super::*;
    use serde_json::json;
    use tokio::io::AsyncBufRead;

    /// Records mapped events and auto-approves any permission request by choosing
    /// the first offered option.
    struct RecordingSink {
        events: Arc<Mutex<Vec<EventBody>>>,
    }

    #[async_trait]
    impl AcpEventSink for RecordingSink {
        async fn on_event(&mut self, event: EventBody) {
            self.events.lock().unwrap().push(event);
        }
        async fn on_permission(
            &mut self,
            _tool_call: Value,
            options: Vec<PermissionOption>,
        ) -> Option<String> {
            options.first().map(|option| option.option_id.clone())
        }
    }

    /// How the scripted agent answers the handshake and one `session/prompt`.
    /// Defaults to the pre-discovery agent: no auth methods, no config options,
    /// no permission ask, stream a text chunk + a tool call, end the turn.
    #[derive(Clone, Default)]
    struct Script {
        /// `initialize` advertises these `authMethods`.
        auth_methods: Value,
        /// `session/new` answers with this JSON-RPC `error` instead of a result.
        session_new_error: Option<Value>,
        /// Extra keys merged into the `session/new` result (`configOptions`,
        /// `modes`).
        session_new_extra: serde_json::Map<String, Value>,
        /// Answer `session/set_config_option` with this full refreshed set.
        set_config_options: Option<Value>,
        /// Ask for tool permission before streaming (and record the answer).
        permission: bool,
        /// `session/update` payloads streamed before the turn resolves.
        updates: Vec<Value>,
        /// Park the turn until the client sends `session/cancel`, then resolve
        /// with `stopReason: cancelled` instead of `end_turn`.
        await_cancel: bool,
    }

    impl Script {
        fn session_new_result(&self) -> Value {
            let mut result = self.session_new_extra.clone();
            result.insert("sessionId".to_string(), json!("s-1"));
            Value::Object(result)
        }

        fn stop_reason(&self) -> &'static str {
            if self.await_cancel {
                "cancelled"
            } else {
                "end_turn"
            }
        }
    }

    /// A Model-category `select` config option in wire shape.
    fn wire_model_selector(current: &str, options: &[(&str, &str)]) -> Value {
        json!({
            "id": "model",
            "name": "Model",
            "category": "model",
            "type": "select",
            "currentValue": current,
            "options": options
                .iter()
                .map(|(value, name)| json!({ "value": value, "name": name }))
                .collect::<Vec<_>>(),
        })
    }

    async fn write_message<W: AsyncWrite + Unpin>(writer: &mut W, message: &Value) {
        let mut line = serde_json::to_string(message).expect("serialize");
        line.push('\n');
        writer.write_all(line.as_bytes()).await.expect("write");
        writer.flush().await.expect("flush");
    }

    async fn read_message<R: AsyncBufRead + Unpin>(reader: &mut R, sent: &Sent) -> Option<Value> {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await.ok()?;
        if read == 0 {
            return None;
        }
        let message: Value = serde_json::from_str(line.trim()).ok()?;
        sent.lock().unwrap().push(message.clone());
        Some(message)
    }

    /// Everything the client sent the agent, in order — requests, responses and
    /// notifications alike. Tests assert against the real wire rather than an
    /// internal call count.
    type Sent = Arc<Mutex<Vec<Value>>>;

    /// The first recorded message whose `method` matches, if any.
    fn sent_method(sent: &Sent, method: &str) -> Option<Value> {
        sent.lock()
            .unwrap()
            .iter()
            .find(|message| message.get("method").and_then(Value::as_str) == Some(method))
            .cloned()
    }

    /// A scripted ACP *agent* peer. Answers `initialize` and `session/new` per
    /// [`Script`], then on `session/prompt` (optionally after a
    /// `session/request_permission` round trip) streams the scripted updates
    /// plus one text chunk + one tool call and resolves the turn. Reads/writes
    /// newline-delimited JSON-RPC on the duplex halves, recording every client
    /// message into `sent`.
    async fn scripted_agent<R, W>(reader: R, mut writer: W, script: Script, sent: Sent)
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut reader = BufReader::new(reader);
        while let Some(message) = read_message(&mut reader, &sent).await {
            let method = message
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let id = message.get("id").cloned();
            match method {
                "initialize" => {
                    write_message(
                        &mut writer,
                        &json!({
                            "jsonrpc": "2.0", "id": id,
                            "result": {
                                "protocolVersion": 1,
                                "agentCapabilities": {},
                                "authMethods": script.auth_methods,
                            }
                        }),
                    )
                    .await;
                }
                "session/new" => {
                    let cwd = message
                        .pointer("/params/cwd")
                        .and_then(Value::as_str)
                        .expect("session/new cwd");
                    assert!(PathBuf::from(cwd).is_absolute(), "ACP cwd must be absolute");
                    let reply = match &script.session_new_error {
                        Some(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
                        None => {
                            json!({ "jsonrpc": "2.0", "id": id, "result": script.session_new_result() })
                        }
                    };
                    write_message(&mut writer, &reply).await;
                }
                "session/set_config_option" => {
                    let reply = match &script.set_config_options {
                        Some(options) => json!({
                            "jsonrpc": "2.0", "id": id,
                            "result": { "configOptions": options }
                        }),
                        None => json!({
                            "jsonrpc": "2.0", "id": id,
                            "error": { "code": -32602, "message": "unknown config option" }
                        }),
                    };
                    write_message(&mut writer, &reply).await;
                }
                "session/prompt" => {
                    if script.permission {
                        write_message(
                            &mut writer,
                            &json!({
                                "jsonrpc": "2.0", "id": 9001,
                                "method": "session/request_permission",
                                "params": {
                                    "sessionId": "s-1",
                                    "toolCall": { "toolCallId": "call-1", "title": "write_file" },
                                    "options": [
                                        { "optionId": "allow", "name": "Allow", "kind": "allow_once" },
                                        { "optionId": "deny", "name": "Deny", "kind": "reject_once" }
                                    ]
                                }
                            }),
                        )
                        .await;
                        // The main loop is parked here, so the permission
                        // response must be read (and recorded) inline.
                        read_message(&mut reader, &sent).await;
                    }
                    for update in &script.updates {
                        write_message(
                            &mut writer,
                            &json!({
                                "jsonrpc": "2.0", "method": "session/update",
                                "params": { "sessionId": "s-1", "update": update }
                            }),
                        )
                        .await;
                    }
                    write_message(
                        &mut writer,
                        &json!({
                            "jsonrpc": "2.0", "method": "session/update",
                            "params": { "sessionId": "s-1", "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": { "type": "text", "text": "hi from agent" }
                            } }
                        }),
                    )
                    .await;
                    write_message(
                        &mut writer,
                        &json!({
                            "jsonrpc": "2.0", "method": "session/update",
                            "params": { "sessionId": "s-1", "update": {
                                "sessionUpdate": "tool_call",
                                "toolCallId": "t1", "title": "read_file", "status": "pending"
                            } }
                        }),
                    )
                    .await;
                    // A real agent only reports `cancelled` after the client's
                    // `session/cancel` reaches it — park until it does.
                    if script.await_cancel {
                        loop {
                            match read_message(&mut reader, &sent).await {
                                Some(message)
                                    if message.get("method").and_then(Value::as_str)
                                        == Some("session/cancel") =>
                                {
                                    break
                                }
                                Some(_) => continue,
                                None => return,
                            }
                        }
                    }
                    write_message(
                        &mut writer,
                        &json!({
                            "jsonrpc": "2.0", "id": id,
                            "result": { "stopReason": script.stop_reason() }
                        }),
                    )
                    .await;
                }
                _ => {
                    if id.is_some() {
                        write_message(
                            &mut writer,
                            &json!({
                                "jsonrpc": "2.0", "id": id,
                                "error": { "code": -32601, "message": "method not found" }
                            }),
                        )
                        .await;
                    }
                }
            }
        }
    }

    /// Wire a client to a freshly-spawned scripted agent over two duplex pipes,
    /// completing the handshake. Returns the connected client and the recorded
    /// client→agent wire.
    async fn connect_to_scripted_agent(script: Script) -> (AcpClient, Sent) {
        connect_with_options(script, AcpSessionOptions::default())
            .await
            .expect("handshake completes")
    }

    /// [`connect_to_scripted_agent`] without the success expectation, so a
    /// handshake failure is itself assertable.
    async fn connect_with_options(
        script: Script,
        options: AcpSessionOptions,
    ) -> Result<(AcpClient, Sent), AcpClientError> {
        // agent -> client, and client -> agent.
        let (client_reads, agent_writes) = tokio::io::duplex(8192);
        let (agent_reads, client_writes) = tokio::io::duplex(8192);
        let sent: Sent = Arc::new(Mutex::new(Vec::new()));
        tokio::spawn(scripted_agent(
            agent_reads,
            agent_writes,
            script,
            Arc::clone(&sent),
        ));
        // A caller-friendly relative cwd is canonicalized before session/new;
        // a real agent rejects relative ACP working-directory URIs.
        let client = AcpClient::connect_with(client_reads, client_writes, ".", options).await?;
        Ok((client, sent))
    }

    #[tokio::test]
    async fn client_delegates_a_prompt_and_maps_streamed_updates() {
        let (mut client, _sent) = connect_to_scripted_agent(Script::default()).await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut sink = RecordingSink {
            events: Arc::clone(&events),
        };
        let run_id = RunId::new();

        let stop = client
            .prompt("do the thing", run_id, &mut sink)
            .await
            .expect("prompt resolves");
        assert_eq!(stop, AcpStopReason::EndTurn);

        let events = events.lock().unwrap().clone();
        assert!(
            events.contains(&EventBody::ModelStreamDelta {
                run_id,
                text: "hi from agent".to_string(),
                thought: false,
            }),
            "expected a ModelStreamDelta from the streamed chunk, got {events:?}"
        );
        assert!(
            events.iter().any(|event| matches!(
                event,
                EventBody::ToolStarted { tool, .. } if tool == "read_file"
            )),
            "expected a ToolStarted(read_file) from the streamed tool_call, got {events:?}"
        );
    }

    #[tokio::test]
    async fn client_answers_a_permission_request_with_the_sinks_choice() {
        let (mut client, sent) = connect_to_scripted_agent(Script {
            permission: true,
            ..Script::default()
        })
        .await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut sink = RecordingSink {
            events: Arc::clone(&events),
        };
        let run_id = RunId::new();

        let stop = client
            .prompt("do the thing", run_id, &mut sink)
            .await
            .expect("prompt resolves");
        assert_eq!(stop, AcpStopReason::EndTurn);

        // The agent received the sink's choice as an ACP `selected`/`allow` outcome.
        let response = sent
            .lock()
            .unwrap()
            .iter()
            .find(|message| message.pointer("/result/outcome").is_some())
            .cloned()
            .expect("agent captured a permission response");
        assert_eq!(response["result"]["outcome"]["outcome"], json!("selected"));
        assert_eq!(response["result"]["outcome"]["optionId"], json!("allow"));

        // Streamed updates still flow after the permission round-trip.
        let events = events.lock().unwrap().clone();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, EventBody::ModelStreamDelta { .. })),
            "expected streamed updates after the permission, got {events:?}"
        );
    }

    // -- model discovery ----------------------------------------------------

    #[tokio::test]
    async fn handshake_captures_the_agents_models_modes_and_auth_methods() {
        let mut extra = serde_json::Map::new();
        extra.insert(
            "configOptions".to_string(),
            json!([wire_model_selector(
                "agent-model-2",
                &[("agent-model-1", "First"), ("agent-model-2", "Second")]
            )]),
        );
        extra.insert(
            "modes".to_string(),
            json!({
                "currentModeId": "build",
                "availableModes": [
                    { "id": "build", "name": "Build" },
                    { "id": "review", "name": "Review" },
                ]
            }),
        );
        let (client, _sent) = connect_to_scripted_agent(Script {
            auth_methods: json!([
                { "id": "agent-login", "name": "Agent login", "description": "Sign in first" }
            ]),
            session_new_extra: extra,
            ..Script::default()
        })
        .await;

        assert_eq!(
            client.discovered_models(),
            vec![
                AcpModel {
                    id: "agent-model-1".to_string(),
                    name: "First".to_string(),
                    current: false,
                },
                AcpModel {
                    id: "agent-model-2".to_string(),
                    name: "Second".to_string(),
                    current: true,
                },
            ]
        );
        assert_eq!(
            client
                .discovered_modes()
                .iter()
                .map(|mode| (mode.id.as_str(), mode.current))
                .collect::<Vec<_>>(),
            vec![("build", true), ("review", false)]
        );
        assert_eq!(
            client.auth_methods(),
            vec![AcpAuthMethod {
                id: "agent-login".to_string(),
                name: "Agent login".to_string(),
                description: Some("Sign in first".to_string()),
            }]
        );
    }

    #[tokio::test]
    async fn an_agent_without_config_options_discovers_no_models() {
        // The pre-discovery degradation path: the agent's own default model
        // applies and nothing is invented on its behalf.
        let (client, _sent) = connect_to_scripted_agent(Script::default()).await;
        assert!(client.discovered_models().is_empty());
        assert!(client.discovered_modes().is_empty());
        assert!(client.auth_methods().is_empty());
    }

    #[tokio::test]
    async fn set_model_speaks_set_config_option_and_folds_the_refreshed_set() {
        let mut extra = serde_json::Map::new();
        extra.insert(
            "configOptions".to_string(),
            json!([wire_model_selector(
                "agent-model-1",
                &[("agent-model-1", "First"), ("agent-model-2", "Second")]
            )]),
        );
        let (mut client, sent) = connect_to_scripted_agent(Script {
            session_new_extra: extra,
            set_config_options: Some(json!([wire_model_selector(
                "agent-model-2",
                &[("agent-model-1", "First"), ("agent-model-2", "Second")]
            )])),
            ..Script::default()
        })
        .await;

        client
            .set_model("agent-model-2")
            .await
            .expect("agent accepts its own model id");

        let request = sent_method(&sent, "session/set_config_option")
            .expect("client sent session/set_config_option");
        assert_eq!(request["params"]["sessionId"], json!("s-1"));
        assert_eq!(request["params"]["configId"], json!("model"));
        assert_eq!(request["params"]["value"], json!("agent-model-2"));
        // The response's full refreshed set became the new discovery state.
        assert_eq!(
            client
                .discovered_models()
                .into_iter()
                .find(|model| model.current)
                .map(|model| model.id),
            Some("agent-model-2".to_string())
        );
    }

    #[tokio::test]
    async fn set_model_fails_without_ever_guessing_a_selector() {
        // No Model-category option was advertised: refuse locally rather than
        // inventing a `configId` the agent never named.
        let (mut client, sent) = connect_to_scripted_agent(Script::default()).await;
        let error = client
            .set_model("agent-model-1")
            .await
            .expect_err("no model selector to target");
        assert!(
            error.to_string().contains("no model selector"),
            "unexpected error: {error}"
        );
        assert!(sent_method(&sent, "session/set_config_option").is_none());
    }

    #[tokio::test]
    async fn a_rejected_model_surfaces_the_agents_own_error() {
        let mut extra = serde_json::Map::new();
        extra.insert(
            "configOptions".to_string(),
            json!([wire_model_selector(
                "agent-model-1",
                &[("agent-model-1", "First")]
            )]),
        );
        let (mut client, _sent) = connect_to_scripted_agent(Script {
            session_new_extra: extra,
            // `set_config_options: None` answers with a JSON-RPC error.
            ..Script::default()
        })
        .await;
        let error = client
            .set_model("not-a-model")
            .await
            .expect_err("the agent rejects an unknown model id");
        assert!(
            error.to_string().contains("set_config_option"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn a_failed_session_new_names_the_agents_advertised_auth_methods() {
        let error = connect_with_options(
            Script {
                auth_methods: json!([
                    { "id": "agent-login", "name": "Agent login", "description": "Sign in first" }
                ]),
                session_new_error: Some(json!({ "code": -32000, "message": "auth required" })),
                ..Script::default()
            },
            AcpSessionOptions::default(),
        )
        .await
        .err()
        .expect("session/new failed");
        let message = error.to_string();
        assert!(message.contains("session/new failed"), "{message}");
        assert!(
            message.contains("Agent login") && message.contains("Sign in first"),
            "the advertised auth methods must be surfaced verbatim: {message}"
        );
    }

    #[tokio::test]
    async fn a_failed_session_new_invents_no_remedy_the_agent_did_not_offer() {
        let error = connect_with_options(
            Script {
                session_new_error: Some(json!({ "code": -32000, "message": "boom" })),
                ..Script::default()
            },
            AcpSessionOptions::default(),
        )
        .await
        .err()
        .expect("session/new failed");
        let message = error.to_string();
        assert!(message.contains("session/new failed"), "{message}");
        assert!(
            !message.contains("authentication"),
            "no auth methods were advertised, so none may be suggested: {message}"
        );
    }

    #[tokio::test]
    async fn an_agent_initiated_model_switch_notes_the_run_and_refreshes_discovery() {
        let mut extra = serde_json::Map::new();
        extra.insert(
            "configOptions".to_string(),
            json!([wire_model_selector(
                "agent-model-1",
                &[("agent-model-1", "First"), ("agent-model-2", "Second")]
            )]),
        );
        let (mut client, _sent) = connect_to_scripted_agent(Script {
            session_new_extra: extra,
            updates: vec![json!({
                "sessionUpdate": "config_option_update",
                "configOptions": [wire_model_selector(
                    "agent-model-2",
                    &[("agent-model-1", "First"), ("agent-model-2", "Second")]
                )]
            })],
            ..Script::default()
        })
        .await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut sink = RecordingSink {
            events: Arc::clone(&events),
        };

        client
            .prompt("do the thing", RunId::new(), &mut sink)
            .await
            .expect("prompt resolves");

        let events = events.lock().unwrap().clone();
        assert!(
            events.iter().any(|event| matches!(
                event,
                EventBody::NoteAppended { text, .. } if text.contains("agent-model-2")
            )),
            "an agent-initiated model switch must reach the transcript, got {events:?}"
        );
        assert_eq!(
            client
                .discovered_models()
                .into_iter()
                .find(|model| model.current)
                .map(|model| model.id),
            Some("agent-model-2".to_string()),
            "the live discovery state must follow the agent's announcement"
        );
    }

    #[tokio::test]
    async fn the_cancel_handle_sends_session_cancel_during_a_turn() {
        let (mut client, sent) = connect_to_scripted_agent(Script {
            await_cancel: true,
            ..Script::default()
        })
        .await;
        let cancel = client.cancel_handle();
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut sink = RecordingSink {
            events: Arc::clone(&events),
        };

        let canceller = tokio::spawn(async move {
            // The scripted agent parks until `session/cancel` arrives, so this
            // races only against the turn never being cancellable at all.
            cancel.cancel().await;
        });
        let stop = client
            .prompt("do the thing", RunId::new(), &mut sink)
            .await
            .expect("prompt resolves");
        canceller.await.expect("canceller joins");

        assert_eq!(
            stop,
            AcpStopReason::Cancelled,
            "the agent's own `cancelled` stop reason must be honored"
        );
        let cancel_notification =
            sent_method(&sent, "session/cancel").expect("client sent session/cancel");
        assert_eq!(cancel_notification["params"]["sessionId"], json!("s-1"));
        assert!(
            cancel_notification.get("id").is_none_or(Value::is_null),
            "session/cancel is a notification, not a request"
        );
    }

    #[tokio::test]
    async fn configured_mcp_servers_are_forwarded_into_session_new() {
        let (_client, sent) = connect_with_options(
            Script::default(),
            AcpSessionOptions {
                mcp_servers: vec![AcpMcpServer {
                    name: "docs".to_string(),
                    command: "server-bin".to_string(),
                    args: vec!["--stdio".to_string()],
                }],
            },
        )
        .await
        .expect("handshake completes");

        let request = sent_method(&sent, "session/new").expect("client sent session/new");
        let servers = request["params"]["mcpServers"]
            .as_array()
            .expect("mcpServers array");
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0]["name"], json!("docs"));
        assert_eq!(servers[0]["command"], json!("server-bin"));
        assert_eq!(servers[0]["args"], json!(["--stdio"]));
        assert_eq!(
            servers[0]["env"],
            json!([]),
            "a forwarded launch spec never carries environment pairs"
        );
    }

    #[test]
    fn forwardable_mcp_servers_withholds_secret_bearing_and_hermetic_servers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp.toml");
        std::fs::write(
            &path,
            r#"
[[server]]
name = "plain"
command = "server-bin"
args = ["--stdio"]

[[server]]
name = "with-secret"
command = "server-bin"
env = [["TOKEN", "s3cret"]]

[[server]]
name = "hermetic"
command = "server-bin"
inherit_environment = false
"#,
        )
        .expect("write mcp.toml");
        let servers = forwardable_mcp_servers(&path);
        assert_eq!(
            servers,
            vec![AcpMcpServer {
                name: "plain".to_string(),
                command: "server-bin".to_string(),
                args: vec!["--stdio".to_string()],
            }]
        );
    }

    #[test]
    fn forwarding_is_on_by_default_and_opt_out_is_honored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = "\n[[server]]\nname = \"plain\"\ncommand = \"server-bin\"\n";

        let default_path = dir.path().join("default.toml");
        std::fs::write(&default_path, server).expect("write");
        assert_eq!(forwardable_mcp_servers(&default_path).len(), 1);

        let opt_out = dir.path().join("opt-out.toml");
        std::fs::write(
            &opt_out,
            format!("[acp]\nforward_mcp_servers = false\n{server}"),
        )
        .expect("write");
        assert!(forwardable_mcp_servers(&opt_out).is_empty());

        // A missing file forwards nothing and never errors.
        assert!(forwardable_mcp_servers(&dir.path().join("absent.toml")).is_empty());
    }
}
