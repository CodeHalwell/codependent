import type { PaginatedResult, PaginationParams, UUID } from "./common.js";

export type AuditActorKind = "user" | "daemon" | "system" | "unknown";

export interface AuditRecord {
  id: UUID;
  organizationId: UUID;
  actorKind: AuditActorKind;
  actorId: UUID | null;
  action: string;
  targetKind: string;
  targetId: string;
  actionDigest: string;
  correlationId: UUID | null;
  prevHash: string | null;
  recordHash: string;
  detail: Record<string, unknown>;
  occurredAt: string;
  actorDisplayName?: string | undefined;
}

export interface AuditQuery extends PaginationParams {
  actorKind?: AuditActorKind | undefined;
  actorId?: UUID | undefined;
  action?: string | undefined;
  targetKind?: string | undefined;
  targetId?: string | undefined;
  correlationId?: UUID | undefined;
  since?: string | undefined;
  until?: string | undefined;
}

export type AuditPage = PaginatedResult<AuditRecord>;

export interface AuditVerificationResult {
  valid: boolean;
  totalRecordsChecked: number;
  brokenAtRecordId?: UUID | null | undefined;
  expectedHash?: string | null | undefined;
  actualHash?: string | null | undefined;
  message: string;
}
