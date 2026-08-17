import type { AuditRecord, AuditVerificationResult } from "../types/audit.js";
import { sha256Hex } from "./idempotency.js";

/**
 * Computes canonical payload string for an audit record.
 */
export function canonicalizeAuditRecord(record: AuditRecord): string {
  return JSON.stringify({
    organizationId: record.organizationId,
    actorKind: record.actorKind,
    actorId: record.actorId,
    action: record.action,
    targetKind: record.targetKind,
    targetId: record.targetId,
    actionDigest: record.actionDigest,
    correlationId: record.correlationId,
    detail: record.detail,
    occurredAt: record.occurredAt,
  });
}

/**
 * Computes the expected recordHash for an audit record given its prevHash.
 */
export async function computeAuditRecordHash(
  record: AuditRecord,
  prevHash: string | null
): Promise<string> {
  const canonical = canonicalizeAuditRecord(record);
  const combined = (prevHash ?? "") + ":" + canonical;
  return await sha256Hex(combined);
}

/**
 * Verifies a sequential list of audit records (ordered chronologically oldest to newest).
 * Verifies both that each record's prevHash matches the previous record's recordHash,
 * and that each record's computed hash matches record.recordHash.
 */
export async function verifyAuditHashChain(
  records: AuditRecord[]
): Promise<AuditVerificationResult> {
  if (records.length === 0) {
    return {
      valid: true,
      totalRecordsChecked: 0,
      message: "No records to verify",
    };
  }

  // Ensure records are ordered by occurredAt ascending
  const sorted = [...records].sort(
    (a, b) => new Date(a.occurredAt).getTime() - new Date(b.occurredAt).getTime()
  );

  for (let i = 0; i < sorted.length; i++) {
    const record = sorted[i];

    // Check prevHash link if not the very first record in check
    if (i > 0) {
      const prevRecord = sorted[i - 1];
      if (record.prevHash !== prevRecord.recordHash) {
        return {
          valid: false,
          totalRecordsChecked: i,
          brokenAtRecordId: record.id,
          expectedHash: prevRecord.recordHash,
          actualHash: record.prevHash,
          message: `Hash chain broken at record ${record.id}: prevHash does not match previous record's hash`,
        };
      }
    }

    // Verify recordHash computation if it uses the standard canonical hash
    if (record.recordHash) {
      const computed = await computeAuditRecordHash(record, record.prevHash);
      if (computed !== record.recordHash) {
        return {
          valid: false,
          totalRecordsChecked: i,
          brokenAtRecordId: record.id,
          expectedHash: computed,
          actualHash: record.recordHash,
          message: `Tampered audit record detected at ${record.id}: computed hash ${computed} differs from stored ${record.recordHash}`,
        };
      }
    }
  }

  return {
    valid: true,
    totalRecordsChecked: sorted.length,
    message: `Audit chain cryptographically verified: ${sorted.length} records intact`,
  };
}
