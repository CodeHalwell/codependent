/** Mirrors `crates/protocol/src/run.rs`. */

import type { ArtifactId, DocumentId, PromptId } from "./ids.js";

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type AgentMode =
  | { type: "Ask" }
  | { type: "Explore" }
  | { type: "Plan" }
  | { type: "Build" }
  | { type: "Review" }
  | { type: "Unknown" };

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
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

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type RunDisposition =
  | { type: "Completed"; summary?: string }
  | { type: "Failed"; reason: string }
  | { type: "Cancelled"; reason?: string }
  | { type: "Unknown" };

/**
 * `#[serde(tag = "type")]` + `#[serde(other)] Unknown`.
 *
 * `ExecuteCommand.environment` and `ExecuteCommand.cwd` are `#[serde(default)]`
 * WITHOUT `skip_serializing_if`, so both are always present on the wire —
 * `environment` as a possibly-empty array of `[name, value]` pairs and `cwd` as
 * a string or an explicit `null`. This is the S1 regression the golden vectors
 * exist to catch (see `protocol-vectors/README.md`); they are required here on
 * purpose.
 */
export type ProposedAction =
  | { type: "ReadFiles"; paths: string[] }
  | { type: "WritePatch"; patch: ArtifactId }
  | {
      type: "ExecuteCommand";
      program: string;
      args: string[];
      environment: Array<[string, string]>;
      cwd: string | null;
    }
  | { type: "NetworkRequest"; destination: string }
  | { type: "GitCommit"; repository: string }
  | { type: "GitPush"; remote: string; branch: string }
  | { type: "GitHubMutation"; repository: string; summary: string }
  | {
      type: "PublishDocument";
      document_id: DocumentId;
      target: string;
      changed_files: string[];
      git_action: string;
    }
  | { type: "BlackboardPost"; workflow_run_id: string; kind: string }
  | { type: "BlackboardQuery"; workflow_run_id: string }
  | { type: "McpToolCall"; server: string; tool: string; summary: string; args: string }
  | { type: "AcpToolCall"; agent: string; title: string; details: string }
  | { type: "RecordMemory" }
  | { type: "SearchRegistry" }
  | { type: "DocumentEdit"; document_id: string; summary: string }
  | { type: "WorkflowQuery"; workflow_run_id: string }
  | { type: "WorkflowCreate"; workflow_id: string; summary: string }
  | { type: "WorkflowRun"; workflow_id: string; kind: string; summary: string }
  | { type: "TaskWrite"; repository: string; summary: string }
  | { type: "TaskRead"; repository: string }
  | { type: "CouncilCreate"; name: string; summary: string }
  | { type: "CouncilRun"; name: string; summary: string }
  | { type: "CouncilResultRead"; selector: string }
  | { type: "CodeGraphQuery"; repository: string; summary: string }
  | { type: "CodeGraphAssert"; repository: string; summary: string }
  | { type: "AskUser"; question_count: number; headers: string[] }
  | { type: "RestoreCheckpoint"; run_id: string; ordinal: number; worktree: string; commit: string }
  | { type: "WriteProcessStdin"; process_id: number; byte_len: number }
  | { type: "PlanTransition"; target: AgentMode }
  | { type: "Unknown" };

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type CheckpointKind = { type: "Stash" } | { type: "Commit" } | { type: "Unknown" };

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type RiskLevel =
  | { type: "Low" }
  | { type: "Medium" }
  | { type: "High" }
  | { type: "Critical" }
  | { type: "Unknown" };

export interface Risk {
  level: RiskLevel;
  /** `skip_serializing_if = "Vec::is_empty"` — absent, never `[]`. */
  reasons?: string[];
}

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type ApprovalDecision = { type: "Approve" } | { type: "Reject" } | { type: "Unknown" };

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type ApprovalScope =
  | { type: "Once" }
  | { type: "Run" }
  | { type: "Pattern" }
  | { type: "Repository" }
  | { type: "Unknown" };

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type BudgetDimension =
  | { type: "Tokens" }
  | { type: "Cost" }
  | { type: "WallClock" }
  | { type: "ToolCalls" }
  | { type: "Unknown" };

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type ToolOutcome =
  | { type: "Succeeded" }
  | { type: "Failed"; message: string }
  | { type: "Unknown" };

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type PromptDelivery = { type: "Queue" } | { type: "Steer" } | { type: "Unknown" };

export interface PendingPromptView {
  id: PromptId;
  text: string;
  mode: AgentMode;
  delivery: PromptDelivery;
}
