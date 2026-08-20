import { describe, it, expect } from "vitest";
import {
  computeAuditRecordHash,
  verifyAuditHashChain,
} from "../src/utils/audit-verifier.js";
import type { AuditRecord } from "../src/types/audit.js";

describe("audit-verifier", () => {
  it("verifies a valid hash-chained sequence of audit records", async () => {
    const record1: AuditRecord = {
      id: "rec-1",
      organizationId: "org-1",
      actorKind: "user",
      actorId: "user-1",
      action: "organization.create",
      targetKind: "organization",
      targetId: "org-1",
      actionDigest: "digest-1",
      correlationId: null,
      prevHash: null,
      recordHash: "",
      detail: { slug: "acme" },
      occurredAt: "2026-08-17T10:00:00Z",
    };
    record1.recordHash = await computeAuditRecordHash(record1, null);

    const record2: AuditRecord = {
      id: "rec-2",
      organizationId: "org-1",
      actorKind: "user",
      actorId: "user-1",
      action: "repository.register",
      targetKind: "repository",
      targetId: "repo-1",
      actionDigest: "digest-2",
      correlationId: null,
      prevHash: record1.recordHash,
      recordHash: "",
      detail: { federatedId: "fed-123" },
      occurredAt: "2026-08-17T10:05:00Z",
    };
    record2.recordHash = await computeAuditRecordHash(record2, record1.recordHash);

    const record3: AuditRecord = {
      id: "rec-3",
      organizationId: "org-1",
      actorKind: "daemon",
      actorId: "daemon-1",
      action: "daemon.pair",
      targetKind: "daemon",
      targetId: "daemon-1",
      actionDigest: "digest-3",
      correlationId: null,
      prevHash: record2.recordHash,
      recordHash: "",
      detail: { scope: "metadata-shared" },
      occurredAt: "2026-08-17T10:10:00Z",
    };
    record3.recordHash = await computeAuditRecordHash(record3, record2.recordHash);

    const result = await verifyAuditHashChain([record1, record2, record3]);
    expect(result.valid).toBe(true);
    expect(result.totalRecordsChecked).toBe(3);
  });

  it("detects tampered details inside an audit record", async () => {
    const record1: AuditRecord = {
      id: "rec-1",
      organizationId: "org-1",
      actorKind: "user",
      actorId: "user-1",
      action: "role.grant",
      targetKind: "role_grant",
      targetId: "grant-1",
      actionDigest: "digest-1",
      correlationId: null,
      prevHash: null,
      recordHash: "",
      detail: { role: "observer" },
      occurredAt: "2026-08-17T10:00:00Z",
    };
    record1.recordHash = await computeAuditRecordHash(record1, null);

    // Tamper detail after computing hash: change observer to organization-admin
    record1.detail = { role: "organization-admin" };

    const result = await verifyAuditHashChain([record1]);
    expect(result.valid).toBe(false);
    expect(result.brokenAtRecordId).toBe("rec-1");
    expect(result.message).toContain("Tampered audit record detected");
  });

  it("detects deleted/missing record in the middle of a chain", async () => {
    const record1: AuditRecord = {
      id: "rec-1",
      organizationId: "org-1",
      actorKind: "user",
      actorId: "user-1",
      action: "action-1",
      targetKind: "t",
      targetId: "1",
      actionDigest: "d1",
      correlationId: null,
      prevHash: null,
      recordHash: "",
      detail: {},
      occurredAt: "2026-08-17T10:00:00Z",
    };
    record1.recordHash = await computeAuditRecordHash(record1, null);

    const record2: AuditRecord = {
      id: "rec-2",
      organizationId: "org-1",
      actorKind: "user",
      actorId: "user-1",
      action: "action-2",
      targetKind: "t",
      targetId: "2",
      actionDigest: "d2",
      correlationId: null,
      prevHash: record1.recordHash,
      recordHash: "",
      detail: {},
      occurredAt: "2026-08-17T10:01:00Z",
    };
    record2.recordHash = await computeAuditRecordHash(record2, record1.recordHash);

    const record3: AuditRecord = {
      id: "rec-3",
      organizationId: "org-1",
      actorKind: "user",
      actorId: "user-1",
      action: "action-3",
      targetKind: "t",
      targetId: "3",
      actionDigest: "d3",
      correlationId: null,
      prevHash: record2.recordHash,
      recordHash: "",
      detail: {},
      occurredAt: "2026-08-17T10:02:00Z",
    };
    record3.recordHash = await computeAuditRecordHash(record3, record2.recordHash);

    // Omit record2: verify [record1, record3]
    const result = await verifyAuditHashChain([record1, record3]);
    expect(result.valid).toBe(false);
    expect(result.brokenAtRecordId).toBe("rec-3");
    expect(result.message).toContain("prevHash does not match previous record's hash");
  });

  /**
   * Two records appended in the same clock tick must still verify.
   *
   * The verifier used to re-sort by `occurredAt`. Timestamps are not unique —
   * the server has a test for appends that land on the same one — and
   * `Array.prototype.sort` is stable, so same-timestamp records kept their
   * arrival order. When that was not chain order the link check failed and
   * reported a TAMPERED LEDGER: a false alarm on the one verdict a reader
   * cannot afford to learn to ignore.
   */
  it("verifies a chain whose records share a timestamp", async () => {
    const sameInstant = "2026-08-17T10:00:00Z";
    const chain = await buildChain(3, () => sameInstant);

    // Arrival order is not chain order — exactly what a stable sort preserves.
    const shuffled = [chain[2], chain[0], chain[1]];

    const result = await verifyAuditHashChain(shuffled);
    expect(result.valid).toBe(true);
    expect(result.totalRecordsChecked).toBe(3);
  });

  /** Chain links, not arrival order, decide. */
  it("verifies a chain handed over in reverse", async () => {
    const chain = await buildChain(4, (i) => `2026-08-17T10:0${i}:00Z`);

    const result = await verifyAuditHashChain([...chain].reverse());
    expect(result.valid).toBe(true);
    expect(result.totalRecordsChecked).toBe(4);
  });

  /** A genuinely altered record is still caught, whatever the timestamps say. */
  it("still detects tampering when timestamps collide", async () => {
    const sameInstant = "2026-08-17T10:00:00Z";
    const chain = await buildChain(3, () => sameInstant);
    chain[1].detail = { tampered: true };

    const result = await verifyAuditHashChain(chain);
    expect(result.valid).toBe(false);
    expect(result.brokenAtRecordId).toBe(chain[1].id);
  });
});

/** Build a valid `length`-record chain, timestamped by `at`. */
async function buildChain(
  length: number,
  at: (index: number) => string
): Promise<AuditRecord[]> {
  const chain: AuditRecord[] = [];
  let prevHash: string | null = null;
  for (let index = 0; index < length; index += 1) {
    const record: AuditRecord = {
      id: `rec-${index}`,
      organizationId: "org-1",
      actorKind: "user",
      actorId: "user-1",
      action: `action-${index}`,
      targetKind: "t",
      targetId: `${index}`,
      actionDigest: `d${index}`,
      correlationId: null,
      prevHash,
      recordHash: "",
      detail: {},
      occurredAt: at(index),
    };
    record.recordHash = await computeAuditRecordHash(record, prevHash);
    prevHash = record.recordHash;
    chain.push(record);
  }
  return chain;
}
