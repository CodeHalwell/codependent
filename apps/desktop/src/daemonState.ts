/**
 * The desktop client's store: daemon frames in, transcript out.
 *
 * Every transcript item here is derived from authoritative daemon state: a
 * durable `SessionEvent`, or actionable projection data in a compact catch-up
 * snapshot. There is no branch that produces assistant text, a run, or a
 * session from client-side state — if the daemon said nothing, the transcript
 * stays empty. The one client-owned field is `error`, which reports a failed
 * *command*, and is rendered as a client error, never as agent output.
 */
import type {
  Catchup,
  InboxEntry,
  SessionEvent,
} from "@codypendent/protocol";
import type {
  ConnectionInfo,
  DaemonFrame,
  RunHandle,
  SessionRow,
} from "./transport.js";
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
  /** Durable events retained for sequence de-duplication and history rebuilds. */
  durableEvents: SessionEvent[];
  lastSequence: number;
  /** The last command that failed, if any. Cleared on the next submit. */
  error: string | null;
  /** Durable inbox entries. */
  inbox: InboxEntry[];
  /** Count of unread inbox items. */
  unreadInboxCount: number;
  /**
   * Whether `inbox` reflects a real answer from the daemon.
   *
   * An empty `inbox` is ambiguous on its own: it is the same array whether the
   * daemon said "nothing pending" or was never asked. Only `"loaded"` licenses
   * the UI to state that there is no pending work.
   */
  inboxStatus: "unloaded" | "loaded" | "unavailable";
  /** Why the inbox could not be read, when `inboxStatus` is `"unavailable"`. */
  inboxDetail: string | null;
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
  | { type: "inbox-loaded"; entries: InboxEntry[] }
  | { type: "inbox-unavailable"; detail: string }
  | { type: "inbox-entry-updated"; entry: InboxEntry }
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
  durableEvents: [],
  lastSequence: 0,
  error: null,
  inbox: [],
  unreadInboxCount: 0,
  inboxStatus: "unloaded",
  inboxDetail: null,
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
        // Without a daemon the inbox is unreadable, not empty.
        inboxStatus: "unavailable",
        inboxDetail: action.detail,
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
      return resetSessionProjection(state, action.sessionId);

    case "run-submitted": {
      const base = state.activeSessionId === action.handle.session_id
        ? state
        : resetSessionProjection(state, action.handle.session_id);
      return {
        ...base,
        activeSessionId: action.handle.session_id,
        activeRunId: action.handle.run_id ?? base.activeRunId,
        isRunning: true,
        error: null,
      };
    }

    case "command-failed":
      return { ...state, error: action.message };

    case "inbox-loaded": {
      const unreadInboxCount = action.entries.filter(
        (e) => !e.state || e.state.type === "Unread",
      ).length;
      return {
        ...state,
        inbox: action.entries,
        unreadInboxCount,
        inboxStatus: "loaded",
        inboxDetail: null,
      };
    }

    case "inbox-unavailable":
      return {
        ...state,
        inbox: [],
        unreadInboxCount: 0,
        inboxStatus: "unavailable",
        inboxDetail: action.detail,
      };

    case "inbox-entry-updated": {
      const exists = state.inbox.some((e) => e.id === action.entry.id);
      const inbox = exists
        ? state.inbox.map((e) => (e.id === action.entry.id ? action.entry : e))
        : [action.entry, ...state.inbox];
      const unreadInboxCount = inbox.filter(
        (e) => !e.state || e.state.type === "Unread",
      ).length;
      return {
        ...state,
        inbox,
        unreadInboxCount,
      };
    }

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
      return applySnapshot(
        state.activeSessionId === frame.session_id
          ? state
          : resetSessionProjection(state, frame.session_id),
        frame.session_id,
        frame.snapshot,
      );
    case "history": {
      const base = state.activeSessionId === frame.session_id
        ? state
        : resetSessionProjection(state, frame.session_id);
      return rebuildFromEvents(base, mergeEvents(base.durableEvents, frame.events));
    }
    case "event": {
      const base = frame.session_id && state.activeSessionId !== frame.session_id
        ? resetSessionProjection(state, frame.session_id)
        : state;
      return mergeDurableEvent(base, frame.event);
    }
  }
}

function resetSessionProjection(state: DaemonState, sessionId: string): DaemonState {
  return {
    ...state,
    activeSessionId: sessionId,
    activeRunId: null,
    isRunning: false,
    transcript: [],
    durableEvents: [],
    lastSequence: 0,
    error: null,
  };
}

function applySnapshot(
  state: DaemonState,
  sessionId: string,
  snapshot: Catchup,
): DaemonState {
  if (!isProjectionSnapshot(snapshot)) {
    return { ...state, activeSessionId: sessionId };
  }
  const projection = snapshot.projection;
  const activeRuns = projection.active_runs ?? [];
  const pendingApprovals = projection.pending_approvals ?? [];
  const approvals: TranscriptItem[] = pendingApprovals.map((approval) => ({
    id: `approval-snapshot-${approval.approval_id}`,
    type: "approval",
    text: describeAction(approval.action),
    approvalId: approval.approval_id,
    timestamp: "",
  }));
  return {
    ...state,
    activeSessionId: sessionId,
    activeRunId: activeRuns.at(-1) ?? null,
    isRunning: activeRuns.length > 0,
    transcript: [...state.transcript.filter((item) => item.type !== "approval"), ...approvals],
  };
}

function isProjectionSnapshot(snapshot: Catchup): snapshot is Extract<Catchup, { type: "Snapshot" }> {
  return snapshot.type === "Snapshot" && Boolean(snapshot.projection);
}

function mergeDurableEvent(state: DaemonState, event: SessionEvent): DaemonState {
  if (state.durableEvents.some((candidate) => candidate.sequence === event.sequence)) {
    return state;
  }
  const events = mergeEvents(state.durableEvents, [event]);
  if (event.sequence > state.lastSequence) {
    return {
      ...applyEvent(state, event),
      durableEvents: events,
      lastSequence: event.sequence,
    };
  }
  return rebuildFromEvents(state, events);
}

function mergeEvents(
  current: SessionEvent[],
  incoming: SessionEvent[],
): SessionEvent[] {
  const bySequence = new Map(current.map((event) => [event.sequence, event]));
  for (const event of incoming) {
    bySequence.set(event.sequence, event);
  }
  return [...bySequence.values()].sort((left, right) => left.sequence - right.sequence);
}

function rebuildFromEvents(state: DaemonState, events: SessionEvent[]): DaemonState {
  let projected: DaemonState = {
    ...state,
    activeRunId: null,
    isRunning: false,
    transcript: [],
    durableEvents: events,
    lastSequence: 0,
  };
  for (const event of events) {
    projected = applyEvent(projected, event);
  }
  return {
    ...projected,
    durableEvents: events,
    lastSequence: events.at(-1)?.sequence ?? 0,
  };
}

function applyEvent(state: DaemonState, event: SessionEvent): DaemonState {
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
      const artifactId = ("artifact" in body && body.artifact && typeof body.artifact === "object" && "id" in body.artifact)
        ? String(body.artifact.id)
        : undefined;
      let patched = false;
      const transcript = [...state.transcript]
        .reverse()
        .map((item) => {
          if (!patched && item.type === "tool_call" && item.toolName === tool && item.status === "running") {
            patched = true;
            return { ...item, status, ...(artifactId ? { artifactId } : {}) };
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

    case "ApprovalResolved": {
      const approvalId = asText(body.approval_id);
      return {
        ...state,
        transcript: state.transcript.filter((item) => item.approvalId !== approvalId),
      };
    }

    case "QuestionAsked": {
      const questions = "questions" in body && Array.isArray(body.questions) ? body.questions : [];
      const question = questions[0] as { header?: string; question?: string } | undefined;
      const text = question ? `${question.header ?? ""}: ${question.question ?? ""}` : "Question prompted";
      return {
        ...state,
        transcript: [
          ...state.transcript,
          {
            id: `question-${key}`,
            type: "question",
            text,
            timestamp: at,
            questionPrompt: question,
          },
        ],
      };
    }

    case "QuestionResolved": {
      return {
        ...state,
        transcript: [
          ...state.transcript,
          {
            id: `question-resolved-${key}`,
            type: "system",
            text: "Question answered",
            timestamp: at,
          },
        ],
      };
    }

    case "PatchProposed": {
      const artifactId = ("artifact" in body && body.artifact && typeof body.artifact === "object" && "id" in body.artifact)
        ? String(body.artifact.id)
        : "";
      return {
        ...state,
        transcript: [
          ...state.transcript,
          {
            id: `patch-${key}`,
            type: "system",
            text: `Patch proposed: artifact ${artifactId}`,
            artifactId,
            timestamp: at,
          },
        ],
      };
    }

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
    const programStr = record.program
      ? `${record.program} ${Array.isArray(record.args) ? record.args.join(" ") : ""}`.trim()
      : "";
    const detail =
      programStr
      || asText(record.command)
      || asText(record.path)
      || asText(record.summary)
      || asText(record.url);
    return detail ? `${kind}: ${detail}` : kind;
  }
  return "action";
}
