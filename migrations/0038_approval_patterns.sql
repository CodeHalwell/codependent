-- Adoption 07: arity-learned approval patterns.
--
-- `approvals.pattern` records the rule a Pattern/Repository-scoped resolution
-- created (e.g. 'git checkout *'), computed server-side from the approved
-- action at resolve time — never wire-supplied. NULL for once/run scopes.
ALTER TABLE approvals ADD COLUMN pattern TEXT;

-- Durable, per-repository learned rules ("always allow persists real rules",
-- codex execpolicy/amend.rs adapted to the house append-only-SQLite shape).
-- Rows are never updated except to stamp revoked_at; revocation is a tombstone
-- so the audit trail keeps what was in force when.
CREATE TABLE approval_rules (
    id TEXT PRIMARY KEY,
    repository TEXT NOT NULL,             -- canonical repository root
    kind TEXT NOT NULL,                   -- 'command-prefix' (closed set, v1)
    pattern TEXT NOT NULL,                -- e.g. 'git checkout *'
    created_from_approval TEXT REFERENCES approvals(id),
    created_by TEXT NOT NULL,             -- principal uid, as approvals.resolved_by
    created_at TEXT NOT NULL,
    revoked_at TEXT,
    revoked_by TEXT
);

CREATE INDEX idx_approval_rules_lookup
    ON approval_rules(repository, kind, revoked_at);
