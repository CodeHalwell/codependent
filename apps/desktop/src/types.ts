export type SessionId = string;
export type RunId = string;

/**
 * `connected` means the shell's handshake with `codypendentd` succeeded and
 * nothing has closed the socket since. It is never assumed, and never a
 * default.
 */
export type ConnectionStatus = "disconnected" | "connecting" | "connected";

/** A session row exactly as the daemon lists it — no client-side embellishment. */
export interface SessionSummary {
  id: SessionId;
  title: string;
  /** `open` | `closed`, the daemon's own session state. */
  state: string;
  created_at: string;
  updated_at: string;
}

export interface TranscriptItem {
  id: string;
  type:
    | "user"
    | "assistant"
    | "tool_call"
    | "tool_result"
    | "thought"
    | "system"
    | "approval"
    | "question"
    /**
     * The run's backstage material: the context manifest and curated-memory
     * writes. Real, but not part of the visible conversation — the TUI folds
     * these into one dim expandable line per run
     * (`TranscriptEntry::Backstage`) rather than printing the whole manifest
     * into the transcript, and this client now does the same.
     */
    | "backstage";
  text: string;
  timestamp: string;
  toolName?: string;
  toolArgs?: Record<string, unknown>;
  toolResult?: unknown;
  status?: "pending" | "running" | "success" | "error";
  duration_ms?: number;
  approvalId?: string;
  questionPrompt?: unknown;
  artifactId?: string;
  /** `backstage` only: lines in the most recent context manifest. */
  contextLines?: number;
  /** `backstage` only: how many `remembered:` notes have folded in. */
  memoryUpdates?: number;
  /** `backstage` only: every folded note body, in arrival order. */
  raw?: string[];
}
