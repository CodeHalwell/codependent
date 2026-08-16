import type { UiDocument } from "@codypendent/ui";

export type SessionId = string;
export type RunId = string;

export interface SessionSummary {
  id: SessionId;
  title: string;
  created_at: string;
  last_activity_at: string;
  run_count: number;
  active_run_id?: RunId;
}

export type RunState = "Queued" | "Running" | "Paused" | "Completed" | "Failed" | "Cancelled";

export interface RunRecord {
  id: RunId;
  session_id: SessionId;
  objective: string;
  state: RunState;
  model: string;
  cost_usd: number;
  duration_ms: number;
  input_tokens: number;
  output_tokens: number;
  created_at: string;
}

export interface TranscriptItem {
  id: string;
  type: "user" | "assistant" | "tool_call" | "tool_result" | "thought" | "system" | "approval";
  text: string;
  timestamp: string;
  toolName?: string;
  toolArgs?: Record<string, unknown>;
  toolResult?: unknown;
  status?: "pending" | "running" | "success" | "error";
  duration_ms?: number;
  approvalId?: string;
}

export interface PendingApproval {
  id: string;
  run_id: RunId;
  action_summary: string;
  details: Record<string, unknown>;
  created_at: string;
}

export interface DesktopState {
  connected: boolean;
  sessions: SessionSummary[];
  activeSessionId: SessionId | null;
  activeRun: RunRecord | null;
  transcript: TranscriptItem[];
  pendingApprovals: PendingApproval[];
  remoteDocuments: Map<string, UiDocument>;
  theme: "dark" | "light" | "system";
}
