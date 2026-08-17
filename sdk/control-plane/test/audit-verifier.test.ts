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
});
