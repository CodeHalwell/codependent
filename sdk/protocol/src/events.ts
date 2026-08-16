/** Mirrors `crates/protocol/src/events.rs`. */

import type { ArtifactRef } from "./artifact.js";
import type { ClientRole } from "./handshake.js";
import type {
  AgentId,
  ApprovalId,
  ChangeSetId,
  CheckpointId,
  ClientId,
  CommandId,
  CorrelationId,
  LearningId,
  ModelId,
  QuestionId,
  RunId,
  SessionId,
  Timestamp,
  UserId,
} from "./ids.js";
import type { QuestionOutcome, QuestionPrompt } from "./question.js";
import type {
  AgentMode,
  ApprovalDecision,
  BudgetDimension,
  CheckpointKind,
  PendingPromptView,
  ProposedAction,
  Risk,
  RunDisposition,
  RunState,
  ToolOutcome,
} from "./run.js";

/**
 * One durable ledger entry. Note the session id is NOT carried here — it lives
 * on the transport {@link import("./envelope.js").Envelope}.
 */
export interface SessionEvent {
  sequence: number;
  occurred_at: Timestamp;
  causation_id?: CommandId;
  correlation_id?: CorrelationId;
  actor: Actor;
  body: EventBody;
}

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type Actor =
  | { type: "Human"; user_id: UserId }
  | { type: "Agent"; agent_id: AgentId; run_id: RunId; model: ModelId }
  | { type: "Client"; client_id: ClientId }
  | { type: "Integration"; integration_id: string }
  | { type: "System" }
  | { type: "Unknown" };

/**
 * `#[serde(tag = "type")]` + `#[serde(other)] Unknown` (RULE 1): an event type
 * produced by a newer daemon deserializes to `Unknown` in an older peer rather
 * than failing the frame.
 */
export type EventBody =
  // --- Phase 0: session lifecycle ---
  | { type: "SessionCreated"; title: string }
  | { type: "NoteAppended"; text: string; run_id?: RunId }
  | { type: "SessionClosed" }
  // --- Phase 1: run lifecycle and agent activity ---
  | { type: "RunStarted"; run_id: RunId; objective: string; mode: AgentMode }
  | { type: "RunStateChanged"; run_id: RunId; state: RunState }
  | { type: "ModelStreamDelta"; run_id: RunId; text: string }
  | {
      type: "ModelRetrying";
      run_id: RunId;
      attempt: number;
      max_attempts: number;
      message: string;
      delay_ms: number;
    }
  | { type: "ToolProposed"; run_id: RunId; approval_id: ApprovalId; action: ProposedAction }
  | {
      type: "ToolDenied";
      run_id: RunId;
      action: ProposedAction;
      /** `skip_serializing_if = "Vec::is_empty"`. */
      reasons?: string[];
    }
  | {
      type: "ToolStarted";
      run_id: RunId;
      /** Tool name, e.g. `shell.run`. */
      tool: string;
      /** Digest of the tool arguments, not the arguments themselves. */
      args_digest: string;
      /** Short human-readable display label; absent on older ledger bytes. */
      label?: string;
    }
  | { type: "ToolCompleted"; run_id: RunId; tool: string; outcome: ToolOutcome; artifact?: ArtifactRef }
  | {
      type: "PatchProposed";
      run_id: RunId;
      changeset_id: ChangeSetId;
      artifact: ArtifactRef;
      /** `skip_serializing_if = "Vec::is_empty"`. */
      files?: string[];
      /** Skipped when zero. */
      additions?: number;
      /** Skipped when zero. */
      deletions?: number;
      /** Skipped when empty. */
      preview?: string;
      /** Skipped when false. */
      preview_truncated?: boolean;
    }
  | { type: "ApprovalRequested"; approval_id: ApprovalId; action: ProposedAction; risk: Risk; pattern?: string }
  | { type: "ApprovalResolved"; approval_id: ApprovalId; decision: ApprovalDecision }
  | { type: "SteeringQueued"; run_id: RunId }
  | { type: "SteeringApplied"; run_id: RunId }
  | { type: "BudgetWarning"; run_id: RunId; dimension: BudgetDimension; used: number; limit: number }
  | {
      type: "ContextUsage";
      run_id: RunId;
      used_tokens: number;
      window_tokens: number;
      system_tokens: number;
      tool_tokens: number;
      transcript_tokens: number;
    }
  | { type: "RunCompleted"; run_id: RunId; disposition: RunDisposition; chronicle: ArtifactRef }
  | {
      type: "RunUsage";
      run_id: RunId;
      /** Absent — never zero — when the provider reported no count. */
      prompt_tokens?: number;
      completion_tokens?: number;
      /** USD millionths. Absent when the model has no price on file. */
      cost_micros?: number;
    }
  | {
      type: "LearningsCaptured";
      run_id: RunId;
      proposed_count: number;
      /** `skip_serializing_if = "Vec::is_empty"`. */
      proposed_ids?: LearningId[];
      activated_count: number;
      /** `skip_serializing_if = "Vec::is_empty"`. */
      activated_ids?: LearningId[];
    }
  | { type: "ClientPresenceChanged"; client_id: ClientId; role: ClientRole; present: boolean }
  | { type: "QuestionAsked"; question_id: QuestionId; run_id: RunId; questions: QuestionPrompt[] }
  | { type: "QuestionResolved"; question_id: QuestionId; outcome: QuestionOutcome }
  | {
      type: "CheckpointRecorded";
      run_id: RunId;
      checkpoint_id: CheckpointId;
      ordinal: number;
      kind: CheckpointKind;
      commit: string;
      base_commit: string;
    }
  | { type: "CheckpointRestored"; run_id: RunId; checkpoint_id: CheckpointId; restored: boolean }
  | { type: "SessionForked"; from_session: SessionId; checkpoint: CheckpointId }
  | { type: "PendingPromptsChanged"; prompts: PendingPromptView[] }
  /** Forward-compatibility fallback (`#[serde(other)]`). */
  | { type: "Unknown" };

/** Every `EventBody` tag this build knows. Kept exhaustive by `tags.ts`. */
export type EventBodyTag = EventBody["type"];
