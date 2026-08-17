import type { UiWireMessage } from "../remote-ui/wire.js";

/**
 * TypeScript wire types for the Codypendent protocol.
 *
 * These reproduce the serde serialization contract from
 * `crates/protocol/src/*.rs` EXACTLY. Notes that drove the shapes below:
 *
 * - Every frame is one serialized `Envelope` (see `envelope.rs`).
 * - Enums are internally tagged with a `"type"` field and PascalCase variant
 *   names (`#[serde(tag = "type")]`). Receivers must tolerate an unknown `type`
 *   (the Rust side maps it to an `Unknown` variant — we treat it as ignorable).
 * - serde's internally-tagged NEWTYPE variants FLATTEN the inner struct's fields
 *   next to the tag. So `Payload::ClientHello(ClientHello { .. })` is on the wire
 *   `{ "type": "ClientHello", "client_name": .., .. }` — the ClientHello fields
 *   sit at the same level as `type`, not nested. The same holds for
 *   `Payload::Command`, `Payload::ServerHello`, `Payload::Event`,
 *   `Payload::CommandRejected`, and `Payload::Error`.
 * - `Option::None` and empty `Vec`s marked `skip_serializing_if` are omitted from
 *   the wire; on read, missing == default. We omit `undefined` fields when
 *   sending and default missing fields when reading.
 */

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

/** UUIDv7 strings on the wire (serde `transparent` newtypes over `Uuid`). */
export type Uuid = string;

/** `version.rs` — `{ major, minor }`. */
export interface ProtocolVersion {
  major: number;
  minor: number;
}

/** `version.rs::PROTOCOL_V1`. Additive Phase 1 revision; bumped to 1.2 by the
 * daemon-auto-restart feature's `ServerHello.build_id` /
 * `DaemonStatus.build_id` / `DaemonStatus.active_run_count` fields, then to 1.3
 * by its daemon-side idle-guarded shutdown (`ShutdownIfIdle` / `ShutdownRefused`),
 * then to 1.4 by external ACP tool approval payloads; the Rust protocol is now
 * 1.6 after additive adoption and bounded-artifact retrieval variants. */
export const PROTOCOL_V1: ProtocolVersion = { major: 1, minor: 6 };

// ---------------------------------------------------------------------------
// capabilities.rs
// ---------------------------------------------------------------------------

/** Legacy flags always serialize; additive platform flags may be omitted. */
export interface ClientCapabilities {
  rich_text: boolean;
  image_display: boolean;
  audio_capture: boolean;
  editor_mutations: boolean;
  diff_view: boolean;
  mouse: boolean;
  unicode: boolean;
  true_color: boolean;
  session_library?: boolean;
  editor_actions?: boolean;
  inbox?: boolean;
  analytics?: boolean;
  automation?: boolean;
  bundles?: boolean;
  marketplace?: boolean;
  secrets?: boolean;
}

/** Capabilities an editor-aware client advertises. */
export const IDE_CAPABILITIES: ClientCapabilities = {
  rich_text: true,
  image_display: false,
  audio_capture: false,
  editor_mutations: true,
  diff_view: true,
  mouse: true,
  unicode: true,
  true_color: true,
};

// ---------------------------------------------------------------------------
// handshake.rs
// ---------------------------------------------------------------------------

export interface ClientHello {
  client_name: string;
  client_version: string;
  supported_protocols: ProtocolVersion[];
  capabilities: ClientCapabilities;
  /** Opaque resume token, omitted when absent. */
  resume_token?: string;
}

export interface ServerHello {
  selected_protocol: ProtocolVersion;
  daemon_version: string;
  daemon_instance: Uuid;
  heartbeat_interval_ms: number;
  /**
   * Daemon-minted token this client presents in its next `ClientHello` to be
   * recognized as the same identity across reconnects. Optional for wire
   * compatibility with older daemons.
   */
  resume_token?: string;
  /**
   * The running daemon's per-build id (`codypendent_protocol::BUILD_ID`),
   * used to detect a stale in-memory daemon after a reinstall. Always
   * serializes (`#[serde(default)]` without `skip_serializing_if`, default
   * `""`) — an older daemon predating this field omits it, so treat a
   * missing value the same as `""`.
   */
  build_id: string;
}

/** `ClientRole` — internally tagged, `{ "type": "Contributor" }` etc. */
export type ClientRole =
  | { type: "Observer" }
  | { type: "Contributor" }
  | { type: "Controller" }
  | { type: "Approver" }
  | { type: "Unknown" };

/** `Subscription` — internally tagged; only the variants used here are typed. */
export type Subscription =
  | { type: "SessionSummary" }
  | { type: "RunTrace"; run_id: Uuid }
  | { type: "AgentActivity" }
  | { type: "RepositoryStatus" }
  | { type: "BudgetState" }
  | { type: "Unknown" };

// ---------------------------------------------------------------------------
// run.rs
// ---------------------------------------------------------------------------

export type AgentMode =
  | { type: "Ask" }
  | { type: "Explore" }
  | { type: "Plan" }
  | { type: "Build" }
  | { type: "Review" }
  | { type: "Unknown" };

export type RunState =
  | { type: "Queued" }
  | { type: "Preparing" }
  | { type: "Running" }
  | { type: "WaitingForApproval" }
  | { type: "WaitingForUserInput" }
  | { type: "Paused" }
  | { type: "Recovering" }
  | { type: "Completed" }
  | { type: "Failed" }
  | { type: "Cancelled" }
  | { type: "Unknown" };

export type RiskLevel =
  | { type: "Low" }
  | { type: "Medium" }
  | { type: "High" }
  | { type: "Critical" }
  | { type: "Unknown" };

export interface Risk {
  level: RiskLevel;
  reasons?: string[];
}

export type ApprovalDecision = { type: "Approve" } | { type: "Reject" } | { type: "Unknown" };

export type ApprovalScope =
  | { type: "Once" }
  | { type: "Run" }
  | { type: "Pattern" }
  | { type: "Repository" }
  | { type: "Unknown" };

/** `ProposedAction` — internally tagged; carried on approval events. */
export type ProposedAction =
  | { type: "ReadFiles"; paths: string[] }
  | { type: "WritePatch"; patch: Uuid }
  | {
      type: "ExecuteCommand";
      program: string;
      args: string[];
      /**
       * The child's COMPLETE environment as `[name, value]` pairs (Rust
       * `Vec<(String, String)>` — serde tuples are JSON arrays). Carried so
       * the approver sees every binding: an unshown, model-controlled
       * environment could smuggle execution hijacks (`LD_PRELOAD`,
       * `RUSTC_WRAPPER`, a shadowed `PATH`) past a benign-looking command.
       * The approval card MUST render this. Optional for older daemons.
       */
      environment?: [string, string][];
      /** The working directory the command runs in, when constrained. */
      cwd?: string | null;
    }
  | { type: "NetworkRequest"; destination: string }
  | { type: "GitCommit"; repository: string }
  | { type: "GitPush"; remote: string; branch: string }
  | { type: "GitHubMutation"; repository: string; summary: string }
  /**
   * Call a tool on an external MCP server (PR B — MCP client). The approval
   * card renders `summary` plus `args` (canonical JSON text) verbatim; the
   * server is operator-declared in the daemon's trusted `mcp.toml`.
   */
  | { type: "McpToolCall"; server: string; tool: string; summary: string; args: string }
  | { type: "AcpToolCall"; agent: string; title: string; details: string }
  | {
      type: "PublishDocument";
      document_id: Uuid;
      target: string;
      changed_files: string[];
      git_action: string;
    }
  /**
   * Record a memory proposal note (the `memory.remember` core tool,
   * smarter-memory M2). Always policy-Allowed and never serialized into a
   * `ToolProposed`/`ApprovalRequested` wire event (see
   * `crates/protocol/src/run.rs`), so it never actually reaches this client —
   * modeled here for type-level parity with the Rust enum only.
   */
  | { type: "RecordMemory" }
  | { type: "Unknown" };

export type ToolOutcome =
  | { type: "Succeeded" }
  | { type: "Failed"; message: string }
  | { type: "Unknown" };

export type RunDisposition =
  | { type: "Completed"; summary?: string }
  | { type: "Failed"; reason: string }
  | { type: "Cancelled"; reason?: string }
  | { type: "Unknown" };

export type BudgetDimension =
  | { type: "Tokens" }
  | { type: "Cost" }
  | { type: "WallClock" }
  | { type: "ToolCalls" }
  | { type: "Unknown" };

// ---------------------------------------------------------------------------
// artifact.rs
// ---------------------------------------------------------------------------

export type DataClassification =
  | { type: "Public" }
  | { type: "Internal" }
  | { type: "Confidential" }
  | { type: "Secret" }
  | { type: "Unknown" };

export interface ArtifactRef {
  id: Uuid;
  media_type: string;
  byte_length: number;
  sha256: string;
  sensitivity: DataClassification;
}

// ---------------------------------------------------------------------------
// error.rs
// ---------------------------------------------------------------------------

export interface CodypendentError {
  code: string;
  message: string;
  retryable: boolean;
  user_action?: { type: string };
  details?: unknown;
  correlation_id: Uuid;
}

/** `envelope.rs::ProtocolError` — the transport-level error shape. */
export interface ProtocolError {
  code: string;
  message: string;
  retryable: boolean;
}

// ---------------------------------------------------------------------------
// events.rs
// ---------------------------------------------------------------------------

export type Actor =
  | { type: "Human"; user_id: string }
  | { type: "Agent"; agent_id: Uuid; run_id: Uuid; model: string }
  | { type: "Client"; client_id: Uuid }
  | { type: "Integration"; integration_id: string }
  | { type: "System" };

/**
 * `EventBody` — internally tagged, `#[non_exhaustive]` with an `Unknown`
 * fallback. The named variants are the ones the extension renders / acts on;
 * the trailing open member keeps forward-compat variants parseable.
 */
export type EventBody =
  | { type: "SessionCreated"; title: string }
  | { type: "NoteAppended"; text: string; run_id?: Uuid }
  | { type: "SessionClosed" }
  | { type: "RunStarted"; run_id: Uuid; objective: string; mode: AgentMode }
  | { type: "RunStateChanged"; run_id: Uuid; state: RunState }
  | { type: "ModelStreamDelta"; run_id: Uuid; text: string }
  | { type: "ToolProposed"; run_id: Uuid; approval_id: Uuid; action: ProposedAction }
  | { type: "ToolDenied"; run_id: Uuid; action: ProposedAction; reasons?: string[] }
  | { type: "ToolStarted"; run_id: Uuid; tool: string; args_digest: string; label?: string }
  | {
      type: "ToolCompleted";
      run_id: Uuid;
      tool: string;
      outcome: ToolOutcome;
      artifact?: ArtifactRef;
    }
  | { type: "PatchProposed"; run_id: Uuid; changeset_id: Uuid; artifact: ArtifactRef }
  | { type: "ApprovalRequested"; approval_id: Uuid; action: ProposedAction; risk: Risk }
  | { type: "ApprovalResolved"; approval_id: Uuid; decision: ApprovalDecision }
  | { type: "SteeringQueued"; run_id: Uuid }
  | { type: "SteeringApplied"; run_id: Uuid }
  | { type: "BudgetWarning"; run_id: Uuid; dimension: BudgetDimension; used: number; limit: number }
  | { type: "RunCompleted"; run_id: Uuid; disposition: RunDisposition; chronicle: ArtifactRef }
  // Every field is optional because the daemon omits what the provider did not
  // measure — an absent `cost_micros` means "unmeasured", never "free", so a
  // renderer must distinguish undefined from 0 rather than defaulting it.
  | {
      type: "RunUsage";
      run_id: Uuid;
      prompt_tokens?: number;
      completion_tokens?: number;
      cost_micros?: number;
    }
  | {
      type: "LearningsCaptured";
      run_id: Uuid;
      proposed_count: number;
      proposed_ids?: Uuid[];
      activated_count: number;
      activated_ids?: Uuid[];
    }
  | { type: "ClientPresenceChanged"; client_id: Uuid; role: ClientRole; present: boolean }
  | { type: "Unknown" };

export interface SessionEvent {
  sequence: number;
  occurred_at: string;
  causation_id?: Uuid;
  correlation_id?: Uuid;
  actor: Actor;
  body: EventBody;
}

// ---------------------------------------------------------------------------
// catchup.rs
// ---------------------------------------------------------------------------

export interface SessionProjection {
  session_id: Uuid;
  title: string;
  last_sequence: number;
  active_runs?: Uuid[];
  pending_approvals?: Array<{
    approval_id: Uuid;
    run_id: Uuid;
    action: ProposedAction;
    risk: Risk;
  }>;
  closed: boolean;
}

export type Catchup =
  | { type: "Events"; from: number; through: number; events: SessionEvent[] }
  | { type: "Snapshot"; through: number; projection: SessionProjection }
  | { type: "Unknown" };

// ---------------------------------------------------------------------------
// ide.rs
// ---------------------------------------------------------------------------

export interface Position {
  line: number;
  character: number;
}

export interface Range {
  start: Position;
  end: Position;
}

export interface EditorSelection {
  path: string;
  range: Range;
}

export interface DirtyBufferDigest {
  path: string;
  /** Lowercase hex SHA-256 of the buffer bytes. */
  sha256: string;
  byte_length: number;
}

/** Severity of an editor diagnostic, mirroring the common LSP levels. */
export type DiagnosticSeverity =
  | { type: "Error" }
  | { type: "Warning" }
  | { type: "Information" }
  | { type: "Hint" }
  | { type: "Unknown" };

/** One editor diagnostic, forwarded from the IDE for context. */
export interface Diagnostic {
  path: string;
  range: Range;
  severity: DiagnosticSeverity;
  message: string;
  source?: string;
}

/** An ordinary run entry point contributed by an editor client. */
export type EditorNativeAction =
  | { type: "FixSelection" }
  | { type: "ExplainSelection" }
  | { type: "ReviewCurrentFile" }
  | { type: "GenerateTestsForSelection" }
  | { type: "FixDiagnostic"; diagnostic: Diagnostic }
  | { type: "Unknown" };

/**
 * `IdeContextUpdate` — pushed client -> daemon, debounced >= 300 ms.
 * Optional / empty-collection fields are `skip_serializing_if` in Rust; we omit
 * them when empty. `diagnostics_revision` always serializes (default 0).
 */
export interface IdeContextUpdate {
  active_file?: string;
  selection?: EditorSelection;
  open_files?: string[];
  dirty_buffers?: DirtyBufferDigest[];
  diagnostics_revision: number;
}

/** Current editor state attached to an editor-native action. */
export interface EditorActionContext {
  ide: IdeContextUpdate;
  diagnostics?: Diagnostic[];
  repository_id?: string;
}

// ---------------------------------------------------------------------------
// session.rs (Session Library, lifecycle, and search)
// ---------------------------------------------------------------------------

export interface SessionSearchFilters {
  workflow_ids?: string[];
  model_ids?: string[];
  repository_ids?: string[];
  created_after?: string;
  created_before?: string;
  run_states?: RunState[];
}

export interface SessionSearchQuery {
  query: string;
  filters?: SessionSearchFilters;
  limit?: number;
  cursor?: string;
}

export interface SessionSummary {
  session_id: Uuid;
  workspace_id?: Uuid | null;
  title: string;
  state: string;
  updated_at: string;
  created_at: string;
  internal?: boolean;
  parent_session_id?: Uuid;
  parent_run_id?: Uuid;
  pinned?: boolean;
  archived_at?: string | null;
  repository_id?: string;
  repository?: string;
  workspace?: string;
  last_activity_at?: string;
  last_run_id?: Uuid;
  run_state?: RunState;
}

export interface SessionSearchResult {
  session: SessionSummary;
  source: { type: string };
  scope: { type: string };
  stable_identity: string;
  deep_link: { type: string; [key: string]: unknown };
  score: number;
  excerpt?: string;
}

export interface SessionSearchPage {
  items: SessionSearchResult[];
  next_cursor?: string;
}

export type SessionLifecycleAction =
  | { type: "Rename"; title: string }
  | { type: "Pin" }
  | { type: "Unpin" }
  | { type: "Archive" }
  | { type: "Restore" }
  | { type: "Delete"; mode?: { type: "RetentionPolicy" } | { type: "TombstoneOnly" } | { type: "Unknown" } }
  | { type: "Export"; options?: { format: { type: "Json" | "Markdown" | "Sqlite" | "Unknown" }; include_artifacts?: boolean; include_internal_sessions?: boolean } }
  | { type: "Unknown" };

// ---------------------------------------------------------------------------
// command.rs
// ---------------------------------------------------------------------------

/**
 * `CommandBody` — internally tagged. `UpdateIdeContext` is the STEP 3.4/3.5
 * variant being added to the Rust protocol concurrently (client side is
 * implemented here against the ide.rs `IdeContextUpdate` shape). Only the
 * variants the extension issues are typed.
 */
export type CommandBody =
  | {
      type: "AttachSession";
      session_id: Uuid;
      last_seen_sequence?: number;
      subscriptions: Subscription[];
      requested_role: ClientRole;
      repository?: string;
    }
  | { type: "SubmitUserInput"; session_id: Uuid; text: string; mode: AgentMode; model?: string }
  | { type: "StartRun"; session_id: Uuid; objective: string; mode: AgentMode; repository?: string; model?: string }
  | {
      type: "RunEditorAction";
      session_id: Uuid;
      action: EditorNativeAction;
      context: EditorActionContext;
      model?: string;
    }
  | { type: "SearchSessions"; query: SessionSearchQuery }
  | { type: "MutateSessionLifecycle"; session_id: Uuid; action: SessionLifecycleAction }
  | { type: "ReadSessionHistory"; session_id: Uuid; cursor?: string; limit: number }
  | {
      type: "ResolveApproval";
      approval_id: Uuid;
      decision: ApprovalDecision;
      scope: ApprovalScope;
    }
  | { type: "CancelRun"; run_id: Uuid }
  | { type: "PauseRun"; run_id: Uuid }
  | { type: "ResumeRun"; run_id: Uuid }
  | { type: "QueueSteering"; run_id: Uuid; text: string }
  | { type: "UpdateIdeContext"; session_id: Uuid; update: IdeContextUpdate }
  | { type: "RevokeUiPlugin"; plugin_id: string }
  | { type: "ReadArtifact"; artifact_id: Uuid; offset: number; limit: number; expected_sha256: string }
  | { type: "ListInbox"; query?: InboxListQuery }
  | { type: "MutateInbox"; mutation: InboxMutation }
  | { type: "QueryAnalytics"; query?: AnalyticsQuery }
  | { type: "ExportAnalytics"; request: AnalyticsExportRequest };

// ---------------------------------------------------------------------------
// inbox.rs & analytics.rs types
// ---------------------------------------------------------------------------

export type InboxDeepLink =
  | { type: "Approval"; approval_id: Uuid }
  | { type: "Question"; question_id: Uuid }
  | { type: "Session"; session_id: Uuid }
  | { type: "Run"; session_id: Uuid; run_id: Uuid }
  | { type: "Workflow"; workflow_id: string }
  | { type: "Plugin"; plugin_id: string }
  | { type: "Repository"; repository_id: string }
  | { type: "Unknown" };

export type InboxEntryKind =
  | { type: "ApprovalRequest" }
  | { type: "AgentQuestion" }
  | { type: "RunCompleted" }
  | { type: "RunFailed" }
  | { type: "BudgetWarning" }
  | { type: "WorkflowBlocked" }
  | { type: "PluginPermissionChanged" }
  | { type: "RunnerFailed" }
  | { type: "Unknown" };

export type InboxSourceIdentity =
  | { type: "Approval"; approval_id: Uuid }
  | { type: "Question"; question_id: Uuid }
  | { type: "Run"; run_id: Uuid }
  | { type: "Budget"; budget_id: string }
  | { type: "Workflow"; workflow_id: string }
  | { type: "Plugin"; plugin_id: string }
  | { type: "Runner"; runner_id: string }
  | { type: "Unknown" };

export type InboxEntryState =
  | { type: "Unread" }
  | { type: "Acknowledged" }
  | { type: "Dismissed" }
  | { type: "Resolved" }
  | { type: "Unknown" };

export type InboxMutation =
  | { type: "Acknowledge"; entry_id: Uuid }
  | { type: "Dismiss"; entry_id: Uuid }
  | { type: "Unknown" };

export interface InboxSource {
  dedup_key: string;
  identity: InboxSourceIdentity;
  session_id?: Uuid | null;
  run_id?: Uuid | null;
  workflow_id?: string | null;
}

export interface InboxEntry {
  id: Uuid;
  repository_id: string;
  kind: InboxEntryKind;
  state?: InboxEntryState;
  title: string;
  summary?: string;
  source: InboxSource;
  deep_link: InboxDeepLink;
  created_at: string;
  acknowledged_at?: string | null;
  dismissed_at?: string | null;
  resolved_at?: string | null;
}

export interface InboxPage {
  items: InboxEntry[];
  next_cursor?: string | null;
}

export interface InboxListFilters {
  states?: InboxEntryState[];
  kinds?: InboxEntryKind[];
  repository_ids?: string[];
}

export interface InboxListQuery {
  cursor?: string | null;
  limit?: number | null;
  filters?: InboxListFilters;
}

export type AnalyticsGrouping =
  | { type: "model" }
  | { type: "provider" }
  | { type: "repository" }
  | { type: "workflow" }
  | { type: "task_class" }
  | { type: "time" }
  | { type: "completion" }
  | { type: "route" }
  | { type: "unknown" };

export type AnalyticsExportFormat =
  | { type: "json" }
  | { type: "csv" }
  | { type: "unknown" };

export interface MeasurementCoverage {
  measured: number;
  total: number;
}

export interface AnalyticsDimensionCoverage {
  input_tokens: MeasurementCoverage;
  output_tokens: MeasurementCoverage;
  cached_tokens: MeasurementCoverage;
  reasoning_tokens: MeasurementCoverage;
  cost: MeasurementCoverage;
  cost_per_successful_task: MeasurementCoverage;
  latency: MeasurementCoverage;
  retry_count: MeasurementCoverage;
  escalation_count: MeasurementCoverage;
  grader_score: MeasurementCoverage;
  completion_count: MeasurementCoverage;
}

export interface AnalyticsMetrics {
  input_tokens?: number | null;
  output_tokens?: number | null;
  cached_tokens?: number | null;
  reasoning_tokens?: number | null;
  cost_micros?: number | null;
  cost_per_successful_task_micros?: number | null;
  latency_ms?: number | null;
  retry_count?: number | null;
  escalation_count?: number | null;
  grader_score_micros?: number | null;
  completion_count?: number | null;
  coverage?: AnalyticsDimensionCoverage;
}

export interface AnalyticsBucket {
  dimensions?: string[];
  metrics: AnalyticsMetrics;
}

export interface AnalyticsPage {
  items: AnalyticsBucket[];
  next_cursor?: string | null;
}

export interface AnalyticsTimeRange {
  from?: string | null;
  until?: string | null;
}

export interface AnalyticsFilters {
  time?: AnalyticsTimeRange | null;
  models?: string[];
  providers?: string[];
  repositories?: string[];
  workflows?: string[];
  task_classes?: string[];
  routes?: string[];
  completions?: Array<{ type: string }>;
}

export interface AnalyticsQuery {
  limit?: number;
  cursor?: string | null;
  group_by?: AnalyticsGrouping[];
  filters?: AnalyticsFilters;
}

export interface AnalyticsExportRequest {
  format: AnalyticsExportFormat;
  query: AnalyticsQuery;
  max_rows?: number;
}

export interface AnalyticsExportResult {
  format: AnalyticsExportFormat;
  artifact: ArtifactRef;
  row_count: number;
  generated_at: string;
  truncated?: boolean;
}

export interface Command {
  command_id: Uuid;
  idempotency_key: string;
  expected_revision?: number;
  body: CommandBody;
}

// ---------------------------------------------------------------------------
// envelope.rs
// ---------------------------------------------------------------------------

/**
 * `Payload` — internally tagged. Newtype variants flatten their inner struct's
 * fields next to `type` (see the module doc comment). This union covers the
 * payloads the extension sends and receives; the trailing open member keeps
 * unknown / future payload tags parseable so a single frame never fails.
 */
export type Payload =
  | ({ type: "ClientHello" } & ClientHello)
  | ({ type: "ServerHello" } & ServerHello)
  | ({ type: "Command" } & Command)
  | { type: "CommandAccepted"; command_id: Uuid; sequence?: number; created_run?: Uuid }
  | { type: "EditorActionAccepted"; command_id: Uuid; run_id: Uuid }
  | { type: "SessionSearchResults"; command_id: Uuid; page: SessionSearchPage }
  | { type: "SessionLifecycleApplied"; command_id: Uuid; session: SessionSummary }
  | { type: "InboxPage"; command_id: Uuid; page: InboxPage }
  | { type: "InboxEntryApplied"; command_id: Uuid; entry: InboxEntry }
  | { type: "AnalyticsResults"; command_id: Uuid; page: AnalyticsPage }
  | { type: "AnalyticsExported"; command_id: Uuid; result: AnalyticsExportResult }
  | { type: "ArtifactChunk"; artifact_id: Uuid; offset: number; bytes_base64: string; eof: boolean; sha256: string }
  | ({ type: "CommandRejected" } & CodypendentError)
  | ({ type: "Event" } & SessionEvent)
  | { type: "RemoteUi"; message: UiWireMessage }
  | { type: "Catchup"; catchup: Catchup }
  | ({ type: "Error" } & ProtocolError)
  | { type: "Ping" }
  | { type: "Pong" }
  | { type: "Shutdown" }
  | { type: "ShutdownAck" }
  | { type: "ShutdownIfIdle" }
  | { type: "ShutdownRefused"; active_run_count?: number }
  | { type: string; [key: string]: unknown };

/**
 * `Envelope` — one per frame. `Envelope::request` sets a fresh `message_id`,
 * `PROTOCOL_V1`, and leaves the optionals absent. Absent optionals are omitted
 * on the wire (`skip_serializing_if`).
 */
export interface Envelope {
  protocol_version: ProtocolVersion;
  message_id: Uuid;
  correlation_id?: Uuid;
  client_id: Uuid;
  workspace_id?: Uuid;
  session_id?: Uuid;
  sequence?: number;
  payload: Payload;
}

// ---------------------------------------------------------------------------
// Narrowing helpers (payload `type` is a plain string on the wire)
// ---------------------------------------------------------------------------

export function payloadType(payload: Payload): string {
  return payload.type;
}
