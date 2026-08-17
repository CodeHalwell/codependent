import type { UUID } from "./common.js";

export type ApprovalStatus = "pending" | "approved" | "rejected" | "expired";

export interface PendingApproval {
  id: UUID;
  organizationId: UUID;
  repositoryId: UUID;
  daemonId: UUID;
  action: string;
  targetKind: string;
  targetId: string;
  actionDigest: string;
  description: string;
  requestedAt: string;
  expiresAt: string | null;
  status: ApprovalStatus;
  decidedBy: UUID | null;
  decidedAt: string | null;
  decisionReason: string | null;
  repositoryDisplayName?: string | undefined;
  daemonDisplayName?: string | undefined;
  metadata?: Record<string, unknown> | undefined;
}

export type ApprovalDecision = "approve" | "reject";

export interface ApprovalDecisionRequest {
  decision: ApprovalDecision;
  actionDigest: string;
  reason?: string | undefined;
}

export interface ApprovalDecisionResponse {
  approvalId: UUID;
  status: ApprovalStatus;
  decidedAt: string;
  receiptId: UUID;
}
