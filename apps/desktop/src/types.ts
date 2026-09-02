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
    | "backstage"
    /**
     * A run that ended in `Failed`. Its own card, not a dim system row: the
     * one message that explains why nothing happened is the one a person
     * must be able to find, and act on. Carries the sanitised reason and
     * what the failure text suggests doing about it (`failure.ts`).
     */
    | "failure";
  text: string;
  timestamp: string;
  /**
   * `system` rows only: how loud the row should be. `warning` is a policy
   * denial or a budget warning; `info` is a retry notice. Absent means the
   * quiet default.
   */
  tone?: "info" | "warning";
  /** `failure` only: the full sanitised failure chain, folded under the card. */
  failureDetail?: string;
  /** `failure` only: the objective the run was started with, so Retry can resend it. */
  objective?: string;
  /** `failure` only: where the failure text says to go next. */
  remedy?: "keys" | "models" | "retry" | "daemon" | "none";
  /** `failure` only: one sentence naming the next step, when the text gives one. */
  hint?: string;
  toolName?: string;
  toolArgs?: Record<string, unknown>;
  toolResult?: unknown;
  status?: "pending" | "running" | "success" | "error";
  duration_ms?: number;
  approvalId?: string;
  /** `question` only: the daemon's question id, for `ResolveQuestion`. */
  questionId?: string;
  /**
   * The run that ASKED, so a resolution can be attributed. `QuestionResolved`
   * carries only the question id, and a session runs several runs at once —
   * without this, resolving a sibling's question moved the displayed run out
   * of its own waiting state.
   */
  questionRunId?: string;
  /** `question` only: every prompt in the batch, normalised for the card. */
  questionPrompts?: QuestionPromptView[];
  artifactId?: string;
  /**
   * `PatchProposed` only: the wire's bounded unified-diff preview, rendered
   * as a fold. The full patch remains the artifact.
   */
  diffPreview?: string;
  /** `PatchProposed` only: repository-relative paths the change set touches. */
  patchFiles?: string[];
  /** `PatchProposed` only: `+added −removed` line counts from the wire. */
  patchAdditions?: number;
  patchDeletions?: number;
  /** `backstage` only: lines in the most recent context manifest. */
  contextLines?: number;
  /** `backstage` only: how many `remembered:` notes have folded in. */
  memoryUpdates?: number;
  /** `backstage` only: every folded note body, in arrival order. */
  raw?: string[];
}

/** One question as asked (`QuestionPrompt`), with the wire's defaults applied. */
export interface QuestionPromptView {
  header: string;
  question: string;
  options: Array<{ label: string; description?: string }>;
  multiple: boolean;
  /** Whether a typed answer is allowed. The wire defaults this to true. */
  custom: boolean;
}

/** The protocol's `QuestionOutcome`, as this client sends it. */
export type QuestionOutcomeView =
  | { type: "Answered"; answers: string[][] }
  | { type: "Rejected"; feedback?: string };

/**
 * What the live run is doing right now, derived from its newest event.
 *
 * The TUI's `RunActivity` (`crates/tui/src/state.rs`), ported: the transcript
 * shows a spinner row for everything but `streaming` (the growing reply is its
 * own signal) and `idle`, so a run between visible updates never looks like a
 * hang — which, between `RunStarted` and the first token of a cold provider,
 * is exactly what a static screen looked like.
 */
export type RunActivity =
  | { kind: "idle" }
  | { kind: "thinking" }
  | { kind: "streaming" }
  | { kind: "tool"; tool: string }
  | { kind: "waiting"; on: "approval" | "question" }
  | {
      kind: "retrying";
      attempt: number;
      maxAttempts: number;
      /** The daemon's bounded classifier reason, e.g. "provider is overloaded". */
      message: string;
      delayMs: number;
    };

/**
 * What the provider actually reported for a run (`EventBody::RunUsage`).
 * Each part is independent and `null` means NOT MEASURED, never zero.
 */
export interface RunUsage {
  runId: string;
  promptTokens: number | null;
  completionTokens: number | null;
  /** USD millionths. */
  costMicros: number | null;
}
