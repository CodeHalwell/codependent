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

/** What the daemon said about itself during the handshake. */
export interface ConnectionInfo {
  socket_path: string;
  protocol_version: string;
  daemon_version: string;
  daemon_instance: string;
  build_id: string;
}

/** One session row as the daemon lists it. */
export interface SessionRow {
  session_id: string;
  title: string;
  state: string;
  created_at: string;
  updated_at: string;
}

/** The run a submitted objective created, as reported by the daemon. */
export interface RunHandle {
  session_id: string;
  run_id: string | null;
}

/**
 * A durable session event, forwarded verbatim. `body.type` is the protocol's
 * own event name (`ModelStreamDelta`, `ToolStarted`, ...); an event kind this
 * client does not know is ignored, never rendered as agent output.
 */
export interface SessionEventFrame {
  sequence: number;
  occurred_at: string;
  body: { type: string; [field: string]: unknown };
}

export type DaemonFrame =
  | { kind: "event"; session_id: string | null; event: SessionEventFrame }
  | { kind: "catchup"; session_id: string; snapshot: unknown }
  | { kind: "disconnected"; reason: string };

export interface DesktopTransport {
  /** Where the shell will look for the daemon socket. */
  socketPath(): Promise<string>;
  /** Connect and handshake. Rejects when no daemon answers. */
  connect(onFrame: (frame: DaemonFrame) => void): Promise<ConnectionInfo>;
  disconnect(): Promise<void>;
  listSessions(): Promise<SessionRow[]>;
  /** Send a real `StartRun` (preceded by `CreateSession` + `AttachSession`). */
  startObjective(objective: string): Promise<RunHandle>;
  /** Attach to an existing session and replay its catch-up. */
  attachSession(sessionId: string): Promise<void>;
  /** Send a real `CancelRun`. */
  cancelRun(runId: string): Promise<void>;
}

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
    listSessions: () => invoke<SessionRow[]>("list_sessions"),
    startObjective: (objective) => invoke<RunHandle>("start_objective", { objective }),
    attachSession: (sessionId) => invoke<void>("attach_session", { sessionId }),
    cancelRun: (runId) => invoke<void>("cancel_run", { runId }),
  };
}
