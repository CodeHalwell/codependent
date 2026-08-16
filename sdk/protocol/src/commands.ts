/** Mirrors `crates/protocol/src/command.rs`. */

import type { DataClassification } from "./artifact.js";
import type { BlackboardItemDraft, BlackboardScope } from "./blackboard.js";
import type { CodeGraphQuery } from "./codegraph.js";
import type { DocumentEditLease, DocumentMutation, PublishTarget } from "./document.js";
import type { ClientRole, Subscription } from "./handshake.js";
import type { IdeContextUpdate } from "./ide.js";
import type {
  ApprovalId,
  ArtifactId,
  CheckpointId,
  CommandId,
  DocumentId,
  JsonValue,
  MemoryId,
  ModelId,
  PromptId,
  QuestionId,
  RunId,
  SessionId,
  Timestamp,
  WorkspaceId,
} from "./ids.js";
import type { InputEnvelope } from "./input.js";
import type { MemoryScopeTier } from "./memory.js";
import type { QuestionOutcome } from "./question.js";
import type { AgentMode, ApprovalDecision, ApprovalScope, PromptDelivery } from "./run.js";

export interface Command {
  command_id: CommandId;
  idempotency_key: string;
  expected_revision?: number;
  body: CommandBody;
}

export interface CanaryMetrics {
  sample_count: number;
  error_rate_bps: number;
  baseline_error_rate_bps: number;
  p95_latency_ms: number;
  baseline_p95_latency_ms: number;
}

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type PromotionAction =
  | { type: "RunRegression" }
  | { type: "ReviewPermissions" }
  | { type: "StartShadow" }
  | { type: "StartCanary" }
  | { type: "ObserveCanary"; metrics: CanaryMetrics }
  | { type: "FinishCanary" }
  | { type: "Unknown" };

export interface SessionSummary {
  session_id: SessionId;
  /** `Option` with no `skip_serializing_if` — serialized as `null` when unset. */
  workspace_id: WorkspaceId | null;
  title: string;
  state: string;
  updated_at: Timestamp;
  created_at: Timestamp;
}

export interface FileMatchWire {
  path: string;
  indices: number[];
  score: number;
}

/** `#[serde(rename_all = "camelCase")]` — note the camelCase field names. */
export interface UiPluginLifecycleStatus {
  id: string;
  version: string;
  state: string;
  enabledScope?: string;
  updateApprovalReceipt?: string;
  updatePermissionDiff?: string;
}

/**
 * `#[serde(tag = "type")]` + `#[serde(other)] Unknown`.
 *
 * Every variant of the Rust enum is modeled. The subset with committed golden
 * vectors is pinned field-by-field by `test/protocol-vectors.test.ts`; the
 * remainder (the UI-plugin lifecycle family, `ResolveQuestion`,
 * `RestoreCheckpoint`, `ForkSession`, the queued-prompt family, `RunUserShell`,
 * `RememberMemory`) has no vector to pin it — see this package's README.
 */
export type CommandBody =
  // --- UI plugin lifecycle (no golden vectors) ---
  | { type: "InstallUiPlugin"; manifest_toml: string; artifact_base64: string; allow_unsigned: boolean }
  | { type: "SmokeTestUiPlugin"; plugin_id: string }
  | { type: "EnableUiPlugin"; plugin_id: string; scope: string; session_id?: SessionId }
  | { type: "ListUiPlugins" }
  | {
      type: "UpdateUiPlugin";
      plugin_id: string;
      manifest_toml: string;
      artifact_base64: string;
      allow_unsigned: boolean;
    }
  | { type: "ApproveUiPluginUpdate"; plugin_id: string; approval_receipt: string }
  | { type: "RejectUiPluginUpdate"; plugin_id: string; approval_receipt: string }
  | { type: "RevokeUiPlugin"; plugin_id: string }
  | { type: "RemoveTrustedUiPublisher"; publisher_id: string }
  // --- session lifecycle ---
  | {
      type: "ListSessions";
      workspace?: WorkspaceId;
      /** `#[serde(default)]` with no skip — serialized as `null` when unset. */
      limit: number | null;
    }
  | {
      type: "SearchWorkspaceFiles";
      repository: string;
      query: string;
      /** `#[serde(default)]` with no skip — serialized as `null` when unset. */
      limit: number | null;
    }
  | { type: "CreateSession"; workspace: WorkspaceId; title: string; repository?: string }
  | { type: "CloseSession"; session_id: SessionId }
  | {
      type: "AttachSession";
      session_id: SessionId;
      last_seen_sequence?: number;
      subscriptions: Subscription[];
      requested_role: ClientRole;
      repository?: string;
    }
  | {
      type: "SubmitUserInput";
      session_id: SessionId;
      text: string;
      mode: AgentMode;
      model?: ModelId;
      envelope?: InputEnvelope;
    }
  | { type: "StartRun"; session_id: SessionId; objective: string; mode: AgentMode; repository?: string; model?: ModelId }
  // --- run control ---
  | { type: "ResolveApproval"; approval_id: ApprovalId; decision: ApprovalDecision; scope: ApprovalScope }
  | { type: "ResolveQuestion"; question_id: QuestionId; outcome: QuestionOutcome }
  | { type: "CancelRun"; run_id: RunId }
  | { type: "PauseRun"; run_id: RunId }
  | { type: "ResumeRun"; run_id: RunId }
  | { type: "QueueSteering"; run_id: RunId; text: string }
  | { type: "UpdateIdeContext"; session_id: SessionId; update: IdeContextUpdate }
  // --- collaborative documents ---
  | { type: "CreateDocument"; title: string; scope?: string; repository?: string; initial_markdown?: string }
  | { type: "CheckDocuments"; repository?: string; session_id?: SessionId }
  | { type: "MutateDocument"; document_id: DocumentId; mutation: DocumentMutation }
  | { type: "AcquireDocumentLease"; lease: DocumentEditLease; ttl_seconds?: number }
  | { type: "ReleaseDocumentLease"; lease_id: string }
  | { type: "PublishDocument"; document_id: DocumentId; target: PublishTarget }
  // --- workflows ---
  | {
      type: "StartWorkflow";
      manifest: string;
      workflow_id?: string;
      /** `skip_serializing_if = "Value::is_null"`. */
      inputs?: JsonValue;
      repository?: string;
    }
  | { type: "PauseWorkflow"; workflow_run_id: string }
  | { type: "ResumeWorkflow"; workflow_run_id: string }
  | { type: "RetryWorkflowNode"; workflow_run_id: string; node_id: string }
  | { type: "CancelWorkflow"; workflow_run_id: string }
  | { type: "ReadWorkflowRun"; workflow_run_id: string }
  // --- promotion ---
  | { type: "ProposePromotion"; kind: string; name: string; version: number; requires_permission_review: boolean }
  | { type: "AdvancePromotion"; candidate_id: string; action: PromotionAction }
  | { type: "ApprovePromotion"; candidate_id: string }
  | { type: "RollbackPromotion"; candidate_id: string }
  // --- blackboard / repository task board ---
  | {
      type: "ReadBlackboard";
      workflow_run_id: string;
      kind?: string;
      /** Skipped when false. */
      include_superseded?: boolean;
      board_repository?: string;
    }
  | { type: "PostBlackboardItem"; scope: BlackboardScope; item: BlackboardItemDraft }
  | {
      type: "UpdateBlackboardItem";
      scope: BlackboardScope;
      item_id: string;
      status?: string;
      assignee?: string;
      ordinal?: number;
      payload?: JsonValue;
    }
  // --- history ---
  | {
      type: "ReadSessionEvents";
      session_id: SessionId;
      /** Skipped when zero. */
      after_sequence?: number;
      /** Skipped when zero. */
      limit?: number;
    }
  // --- curated memory ---
  | { type: "InspectMemory"; id: MemoryId; repository: string }
  | {
      type: "CorrectMemory";
      id: MemoryId;
      repository: string;
      statement: string;
      structured_value?: JsonValue;
      confidence: number;
    }
  | { type: "ForgetMemory"; id: MemoryId; repository: string }
  | { type: "ForgetMemoryScope"; repository: string; tier: MemoryScopeTier }
  | { type: "OpenMemoryEvidence"; id: MemoryId; repository: string; evidence_index: number }
  // --- promotion evidence ---
  | { type: "SubmitEvalEvidence"; candidate_id: string; suite: string; routing_policy: string; report_json: string }
  // --- artifacts ---
  | { type: "PutArtifact"; media_type: string; bytes_base64: string; sensitivity: DataClassification }
  | { type: "ReadArtifact"; artifact_id: ArtifactId; offset: number; limit: number; expected_sha256: string }
  // --- code graph ---
  | { type: "BuildCodeGraph"; repository: string }
  | { type: "ReadCodeGraphStatus"; repository: string }
  | {
      type: "ReadCodeGraph";
      repository: string;
      /** `#[serde(default)]` with no skip — always present, `{}` when empty. */
      query: CodeGraphQuery;
    }
  // --- checkpoints, forks, queued prompts (no golden vectors) ---
  | { type: "RestoreCheckpoint"; run_id: RunId; checkpoint: CheckpointId }
  | { type: "ForkSession"; session_id: SessionId; checkpoint: CheckpointId; name?: string }
  | { type: "QueuePrompt"; session_id: SessionId; text: string; mode: AgentMode; delivery: PromptDelivery }
  | { type: "UpdateQueuedPrompt"; session_id: SessionId; prompt_id: PromptId; text?: string; delivery?: PromptDelivery }
  | { type: "PromoteQueuedPrompt"; session_id: SessionId; prompt_id: PromptId }
  | { type: "DeleteQueuedPrompt"; session_id: SessionId; prompt_id: PromptId }
  | { type: "RunUserShell"; session_id: SessionId; command: string }
  | { type: "RememberMemory"; session_id: SessionId; text: string }
  /** Forward-compatibility fallback (`#[serde(other)]`). */
  | { type: "Unknown" };

/** Every `CommandBody` tag this build knows. Kept exhaustive by `tags.ts`. */
export type CommandBodyTag = CommandBody["type"];
