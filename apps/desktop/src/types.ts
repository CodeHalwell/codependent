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
  type: "user" | "assistant" | "tool_call" | "tool_result" | "thought" | "system" | "approval" | "question";
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
}
