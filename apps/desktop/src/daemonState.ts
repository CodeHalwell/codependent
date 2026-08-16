/**
 * The desktop client's store: daemon frames in, transcript out.
 *
 * Every transcript item here is derived from a durable `SessionEvent` the
 * daemon emitted. There is no branch that produces assistant text, a run, or a
 * session from client-side state — if the daemon said nothing, the transcript
 * stays empty. The one client-owned field is `error`, which reports a failed
 * *command*, and is rendered as a client error, never as agent output.
 */
import type { ConnectionInfo, DaemonFrame, RunHandle, SessionEventFrame, SessionRow } from "./transport.js";
import type { ConnectionStatus, SessionSummary, TranscriptItem } from "./types.js";

export interface DaemonState {
  status: ConnectionStatus;
  /** Why the client is in this state, in words an operator can act on. */
  detail: string;
  info: ConnectionInfo | null;
  sessions: SessionSummary[];
  activeSessionId: string | null;
  activeRunId: string | null;
  isRunning: boolean;
  transcript: TranscriptItem[];
  /** The last command that failed, if any. Cleared on the next submit. */
  error: string | null;
}

export type DaemonAction =
  | { type: "shell-missing"; detail: string }
  | { type: "connecting"; detail: string }
  | { type: "connected"; info: ConnectionInfo }
  | { type: "connect-failed"; detail: string }
  | { type: "sessions"; sessions: SessionRow[] }
  | { type: "session-selected"; sessionId: string }
  | { type: "run-submitted"; handle: RunHandle }
  | { type: "command-failed"; message: string }
  | { type: "frame"; frame: DaemonFrame };

export const initialState: DaemonState = {
  status: "disconnected",
  detail: "No connection attempted yet.",
  info: null,
  sessions: [],
  activeSessionId: null,
  activeRunId: null,
  isRunning: false,
  transcript: [],
  error: null,
};

export function reduce(state: DaemonState, action: DaemonAction): DaemonState {
  switch (action.type) {
    case "shell-missing":
    case "connect-failed":
      return {
        ...state,
        status: "disconnected",
        detail: action.detail,
        info: null,
        activeRunId: null,
        isRunning: false,
      };

    case "connecting":
      return { ...state, status: "connecting", detail: action.detail };

    case "connected":
      return {
        ...state,
        status: "connected",
        info: action.info,
        detail: `codypendentd ${action.info.daemon_version} on ${action.info.socket_path}`,
      };

    case "sessions":
      return {
        ...state,
        sessions: action.sessions.map((session) => ({
          id: session.session_id,
          title: session.title,
          state: session.state,
          created_at: session.created_at,
          updated_at: session.updated_at,
        })),
      };

    case "session-selected":
      return { ...state, activeSessionId: action.sessionId, transcript: [], error: null };

    case "run-submitted":
      return {
        ...state,
        activeSessionId: action.handle.session_id,
        activeRunId: action.handle.run_id ?? state.activeRunId,
        isRunning: true,
        error: null,
      };

    case "command-failed":
      return { ...state, error: action.message, isRunning: false };

    case "frame":
      return applyFrame(state, action.frame);
  }
}

function applyFrame(state: DaemonState, frame: DaemonFrame): DaemonState {
  switch (frame.kind) {
    case "disconnected":
      return {
        ...state,
        status: "disconnected",
        detail: frame.reason,
        info: null,
        activeRunId: null,
        isRunning: false,
      };
    case "catchup":
      // A projection snapshot rather than an event replay. The transcript is
      // event-shaped, so there is nothing to append; the session is still
      // attached and live events follow.
      return { ...state, activeSessionId: frame.session_id };
    case "event":
      return applyEvent(state, frame.event);
  }
}

function applyEvent(state: DaemonState, event: SessionEventFrame): DaemonState {
  const body = event.body;
  const at = event.occurred_at;
  const key = `${event.sequence}`;

  switch (body.type) {
    case "RunStarted": {
      const objective = asText(body.objective);
      return {
        ...state,
        activeRunId: asText(body.run_id) || state.activeRunId,
        isRunning: true,
        transcript: [
          ...state.transcript,
          { id: `user-${key}`, type: "user", text: objective, timestamp: at },
        ],
      };
    }

    case "ModelStreamDelta": {
      const text = asText(body.text);
      if (!text) {
        return state;
      }
      const runId = asText(body.run_id);
      const last = state.transcript[state.transcript.length - 1];
      if (last && last.type === "assistant" && last.id.startsWith(`assistant-${runId}-`)) {
        const merged: TranscriptItem = { ...last, text: last.text + text };
        return { ...state, transcript: [...state.transcript.slice(0, -1), merged] };
      }
      return {
        ...state,
        transcript: [
          ...state.transcript,
          { id: `assistant-${runId}-${key}`, type: "assistant", text, timestamp: at },
        ],
      };
    }

    case "ToolStarted": {
      const tool = asText(body.tool);
      return {
        ...state,
        transcript: [
          ...state.transcript,
          {
            id: `tool-${key}`,
            type: "tool_call",
            text: asText(body.label) || tool,
            toolName: tool,
            status: "running",
            timestamp: at,
          },
        ],
      };
    }

    case "ToolCompleted": {
      const tool = asText(body.tool);
      const outcome = body.outcome as { type?: string } | undefined;
      const status: TranscriptItem["status"] = outcome?.type === "Succeeded" ? "success" : "error";
      let patched = false;
      const transcript = [...state.transcript]
        .reverse()
        .map((item) => {
          if (!patched && item.type === "tool_call" && item.toolName === tool && item.status === "running") {
            patched = true;
            return { ...item, status };
          }
          return item;
        })
        .reverse();
      return { ...state, transcript };
    }

    case "ApprovalRequested":
      return {
        ...state,
        transcript: [
          ...state.transcript,
          {
            id: `approval-${key}`,
            type: "approval",
            text: describeAction(body.action),
            approvalId: asText(body.approval_id),
            timestamp: at,
          },
        ],
      };

    case "NoteAppended": {
      const text = asText(body.text);
      if (!text) {
        return state;
      }
      return {
        ...state,
        transcript: [
          ...state.transcript,
          { id: `note-${key}`, type: "system", text, timestamp: at },
        ],
      };
    }

    case "RunCompleted": {
      const disposition = body.disposition as { type?: string; reason?: string; summary?: string } | undefined;
      const kind = disposition?.type ?? "Unknown";
      const reason = disposition?.reason ?? disposition?.summary;
      return {
        ...state,
        isRunning: false,
        activeRunId: null,
        transcript: [
          ...state.transcript,
          {
            id: `run-${key}`,
            type: "system",
            text: reason ? `Run ${kind.toLowerCase()}: ${reason}` : `Run ${kind.toLowerCase()}`,
            timestamp: at,
          },
        ],
      };
    }

    case "RunStateChanged": {
      const runState = (body.state as { type?: string } | undefined)?.type ?? "";
      if (["Completed", "Failed", "Cancelled"].includes(runState)) {
        return { ...state, isRunning: false, activeRunId: null };
      }
      if (runState === "Running") {
        return { ...state, isRunning: true };
      }
      return state;
    }

    // Every other event kind — including one a newer daemon invented — is
    // carried by the stream but not rendered. Silence is correct here;
    // inventing a card for an event this client cannot read is not.
    default:
      return state;
  }
}

function asText(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function describeAction(action: unknown): string {
  if (action && typeof action === "object") {
    const record = action as Record<string, unknown>;
    const kind = asText(record.type) || "action";
    const detail =
      asText(record.command) || asText(record.path) || asText(record.summary) || asText(record.url);
    return detail ? `${kind}: ${detail}` : kind;
  }
  return "action";
}
