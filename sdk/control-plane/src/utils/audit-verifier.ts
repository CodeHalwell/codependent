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
 * Order records by the hash chain itself, oldest first.
 *
 * Sorting by `occurredAt` was wrong for the case the chain exists to survive:
 * timestamps are not unique, `Array.prototype.sort` is stable, so records
 * sharing a timestamp kept whatever order they arrived in. If that was not
 * chain order — and the server's own tests cover appends that land on the same
 * timestamp — the prevHash link check failed and reported a *tampered ledger*.
 * A false alarm on that particular question is expensive: it is the one
 * verdict a reader cannot afford to learn to ignore.
 *
 * `prevHash` is the real ordering, so it is what is followed here. Timestamps
 * are used only to pick where to start among several possible beginnings,
 * which keeps the result deterministic.
 *
 * Nothing is dropped. Records unreachable from any beginning — a fork, a cycle
 * — are appended in timestamp order so the verification pass still judges them
 * rather than silently passing a ledger it never looked at.
 */
function orderByChain(records: AuditRecord[]): AuditRecord[] {
  const oldestFirst = (a: AuditRecord, b: AuditRecord): number =>
    new Date(a.occurredAt).getTime() - new Date(b.occurredAt).getTime();

  const knownHashes = new Set<string>();
  for (const record of records) {
    if (record.recordHash) knownHashes.add(record.recordHash);
  }

  const successorOf = new Map<string, AuditRecord>();
  for (const record of records) {
    const key = record.prevHash ?? "";
    // Two records claiming the same predecessor is a fork. Keep the first and
    // leave the other to the tail pass rather than silently choosing.
    if (!successorOf.has(key)) successorOf.set(key, record);
  }

  // A beginning is a record whose predecessor is absent: the genesis record, or
  // the first record of a page that starts mid-chain.
  const beginnings = records
    .filter((record) => !record.prevHash || !knownHashes.has(record.prevHash))
    .sort(oldestFirst);

  const ordered: AuditRecord[] = [];
  const placed = new Set<AuditRecord>();
  for (const beginning of beginnings) {
    let current: AuditRecord | undefined = beginning;
    while (current && !placed.has(current)) {
      ordered.push(current);
      placed.add(current);
      current = current.recordHash ? successorOf.get(current.recordHash) : undefined;
    }
  }

  for (const record of [...records].sort(oldestFirst)) {
    if (!placed.has(record)) {
      ordered.push(record);
      placed.add(record);
    }
  }

  return ordered;
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

  const sorted = orderByChain(records);

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
