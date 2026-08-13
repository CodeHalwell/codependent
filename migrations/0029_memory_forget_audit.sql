-- The durable, content-free "right to forget" audit trail (Chapter 06:
-- "audit records that do not retain deleted sensitive content"). Before this
-- table, `MemoryStore::forget`/`forget_scope` already deleted the row and
-- enqueued an index tombstone, but the audit summary they compute
-- (`ForgetAudit`) existed only as a return value handed to the ONE immediate
-- caller — nothing durable survived past that call to answer "was memory X
-- ever forgotten, and when" (2026-08-13 review, memory-docs vertical, F3).
--
-- One row per completed forget call. `forgotten_ids_json` names which memory
-- ids were removed — never their statement text. `scope_tier`/`scope_key` are
-- set only for a `forget_scope` call (mirroring `ForgetAudit.scope`); a
-- single-id `forget` leaves them NULL.
CREATE TABLE memory_forget_audits (
    id TEXT PRIMARY KEY,
    forgotten_ids_json TEXT NOT NULL, -- Vec<MemoryId>, ids only, never content
    scope_tier TEXT,                  -- set for forget_scope; NULL for a single-id forget
    scope_key TEXT,
    removed_at TEXT NOT NULL
);
CREATE INDEX idx_memory_forget_audits_removed_at ON memory_forget_audits(removed_at);
