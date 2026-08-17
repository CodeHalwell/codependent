/**
 * The desktop client's only route to `codypendentd`.
 *
 * A webview cannot open a Unix domain socket, so the connection lives in the
 * Tauri shell (`src-tauri/src/daemon.rs`) and this module is a thin call
 * surface over it: Tauri commands out, a Tauri channel of daemon frames back.
 * There is no wire codec here — every frame that crosses the boundary was
 * produced by the shared Rust protocol crate (adoption 14 §6, criterion 2).
 *
 * `createTransport()` returns `null` when the app is not running inside the
 * shell (a plain `vite dev` browser tab, or a test). That is deliberate: with
 * no shell there is no transport, and the UI must say so rather than pretend.
 */
import { Channel, invoke } from "@tauri-apps/api/core";
import type {
  AnalyticsExportRequest,
  AnalyticsExportResult,
  AnalyticsPage,
  AnalyticsQuery,
  ArtifactRef,
  Catchup,
  InboxEntry,
  InboxListQuery,
  InboxMutation,
  InboxPage,
  SessionEvent,
  SessionSummary,
} from "@codypendent/protocol";
import {
  exportAnalytics,
  listInbox,
  mutateInbox,
  queryAnalytics,
  type CommandExecutor,
  type ProtocolCommandCaller,
} from "@codypendent/protocol";

export type ConnectionInfo = {
  socket_path: string;
  protocol_version: string;
  daemon_version: string;
  daemon_instance: string;
  build_id: string;
};

export type SessionRow = SessionSummary;

export type RunHandle = {
  session_id: string;
  run_id: string | null;
};

export type SessionEventFrame = SessionEvent;

export type ApprovalChoice = "approve" | "reject";

export type DaemonFrame =
  | { kind: "event"; session_id: string | null; event: SessionEvent }
  | { kind: "catchup"; session_id: string; snapshot: Catchup }
  | { kind: "history"; session_id: string; through: number; events: SessionEvent[] }
  | { kind: "disconnected"; reason: string };

export type DesktopTransport = {
  /** Where the shell will look for the daemon socket. */
  socketPath(): Promise<string>;
  /** Connect and handshake. Rejects when no daemon answers. */
  connect(onFrame: (frame: DaemonFrame) => void): Promise<ConnectionInfo>;
  disconnect(): Promise<void>;
  listSessions(): Promise<SessionSummary[]>;
  /** Send a real `StartRun` (preceded by `CreateSession` + `AttachSession`). */
  startObjective(objective: string): Promise<RunHandle>;
  /** Attach to an existing session and replay its catch-up. */
  attachSession(sessionId: string): Promise<void>;
  /** Send a real `CancelRun`. */
  cancelRun(runId: string): Promise<void>;
  /** Resolve a daemon-owned pending approval for this attached client. */
  resolveApproval(approvalId: string, decision: ApprovalChoice): Promise<void>;
  /** List notifications and human work from the durable inbox. */
  listInbox(query?: InboxListQuery): Promise<InboxPage>;
  /** Apply an idempotent mutation (Acknowledge, Dismiss) to an inbox entry. */
  mutateInbox(mutation: InboxMutation): Promise<InboxEntry>;
  /** Query measured execution observations and aggregates. */
  queryAnalytics(query?: AnalyticsQuery): Promise<AnalyticsPage>;
  /** Export measured analytics as bounded JSON or CSV artifact. */
  exportAnalytics(request: AnalyticsExportRequest): Promise<AnalyticsExportResult>;
  /**
   * Read an artifact's whole content.
   *
   * Takes the `ArtifactRef` rather than a bare id because `ReadArtifact` binds
   * every chunk request to the digest the client observed
   * (`crates/protocol/src/command.rs`); an id alone cannot form the command.
   * Yields bytes, not text: an artifact may be a patch, a CSV or audio, so the
   * caller decodes with `TextDecoder` when it knows the media type is textual.
   */
  readArtifact?(artifact: ArtifactRef): Promise<Uint8Array>;
};

export type { CommandExecutor, ProtocolCommandCaller };
export { exportAnalytics, listInbox, mutateInbox, queryAnalytics };

/** True only inside the Tauri shell, where the bridge commands exist. */
export function shellAvailable(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/**
 * The transport backed by the Tauri shell, or `null` when there is no shell.
 */
export function createTransport(): DesktopTransport | null {
  if (!shellAvailable()) {
    return null;
  }
  return {
    socketPath: () => invoke<string>("daemon_socket"),
    connect: (onFrame) => {
      const channel = new Channel<DaemonFrame>();
      channel.onmessage = onFrame;
      return invoke<ConnectionInfo>("daemon_connect", { channel });
    },
    disconnect: () => invoke<void>("daemon_disconnect"),
    listSessions: () => invoke<SessionSummary[]>("list_sessions"),
    startObjective: (objective) => invoke<RunHandle>("start_objective", { objective }),
    attachSession: (sessionId) => invoke<void>("attach_session", { sessionId }),
    cancelRun: (runId) => invoke<void>("cancel_run", { runId }),
    resolveApproval: (approvalId, decision) =>
      invoke<void>("resolve_approval", { approvalId, approved: decision === "approve" }),
    listInbox: (query) => invoke<InboxPage>("list_inbox", { query }),
    mutateInbox: (mutation) => invoke<InboxEntry>("mutate_inbox", { mutation }),
    queryAnalytics: (query) => invoke<AnalyticsPage>("query_analytics", { query }),
    exportAnalytics: (request) => invoke<AnalyticsExportResult>("export_analytics", { request }),
    // The shell answers with a raw IPC body, which Tauri delivers to the
    // webview as an `ArrayBuffer`; the bytes are the daemon's, verified against
    // `artifact` in the shell before they get here.
    readArtifact: async (artifact) =>
      new Uint8Array(await invoke<ArrayBuffer>("read_artifact", { artifact })),
  };
}
