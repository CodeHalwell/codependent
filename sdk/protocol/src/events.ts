import type { SessionId, RunId, ApprovalId, QuestionId } from "./ids.js";

export interface SessionEventEnvelope<T = SessionEventBody> {
  session_id: SessionId;
  sequence: number;
  occurred_at: string;
  body: T;
}

export type SessionEventBody =
  | { type: "RunStarted"; run_id: RunId; objective: string; model: string }
  | { type: "TurnFinalized"; run_id: RunId; turn_index: number; text: string }
  | { type: "ToolStarted"; run_id: RunId; call_id: string; tool_name: string; args: Record<string, unknown> }
  | { type: "ToolCompleted"; run_id: RunId; call_id: string; outcome: unknown }
  | { type: "ApprovalRequested"; approval_id: ApprovalId; run_id: RunId; action: string; details: Record<string, unknown> }
  | { type: "QuestionAsked"; question_id: QuestionId; run_id: RunId; questions: Array<{ id: string; prompt: string }> }
  | { type: "RunCompleted"; run_id: RunId; duration_ms: number; cost_usd: number }
  | { type: "RunFailed"; run_id: RunId; error: string };
