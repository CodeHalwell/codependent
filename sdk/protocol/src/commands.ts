import type { SessionId, RunId, ApprovalId, QuestionId } from "./ids.js";

export type ClientCommand =
  | {
      type: "StartRun";
      session_id: SessionId;
      objective: string;
      mode?: string;
      model?: string;
    }
  | {
      type: "Steer";
      session_id: SessionId;
      run_id: RunId;
      instructions: string;
    }
  | {
      type: "Approve";
      approval_id: ApprovalId;
      decision: "approved" | "rejected";
      feedback?: string;
    }
  | {
      type: "AnswerQuestion";
      question_id: QuestionId;
      answers: Record<string, string>;
    }
  | {
      type: "CancelRun";
      session_id: SessionId;
      run_id: RunId;
    };
