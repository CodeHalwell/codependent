import type { PaginatedResult, PaginationParams, UUID } from "./common.js";
import type { PublicationClass } from "./organization.js";

export type SharedSessionState =
  | "running"
  | "completed"
  | "failed"
  | "pending-approval"
  | "cancelled"
  | "unknown";

export interface SharedSession {
  id: UUID;
  organizationId: UUID;
  repositoryId: UUID;
  daemonId: UUID;
  remoteSessionKey: string;
  class: PublicationClass;
  title: string | null;
  state: SharedSessionState;
  startedAt: string;
  lastActivityAt: string | null;
  tombstonedAt: string | null;
  updatedAt: string;
  stepCount?: number | undefined;
  repositoryDisplayName?: string | undefined;
  daemonDisplayName?: string | undefined;
}

export type RunStepKind =
  | "planning"
  | "tool_call"
  | "tool_result"
  | "message"
  | "approval_request"
  | "diff_summary";

export interface SharedRunStep {
  id: string;
  stepIndex: number;
  kind: RunStepKind;
  title: string;
  status: "running" | "completed" | "failed" | "pending";
  startedAt: string;
  completedAt?: string | null | undefined;
  summary?: string | undefined;
  details?: Record<string, unknown> | undefined;
}

export interface SharedSessionDetail extends SharedSession {
  steps: SharedRunStep[];
}

export interface SessionListQuery extends PaginationParams {
  repositoryId?: UUID | undefined;
  state?: SharedSessionState | string | undefined;
  since?: string | undefined;
  until?: string | undefined;
  search?: string | undefined;
}

export type SessionListPage = PaginatedResult<SharedSession>;
