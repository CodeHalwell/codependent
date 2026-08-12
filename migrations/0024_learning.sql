-- Curated, reviewable learning records. This ledger is deliberately separate
-- from the legacy memories table: compact facts and reusable procedures have
-- different content contracts and lifecycle controls, while existing memory
-- rows continue to decode unchanged.
CREATE TABLE learning_records (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,                   -- fact | procedure
    scope_kind TEXT NOT NULL,             -- user | repository | provider | council
    scope_key TEXT NOT NULL,
    content_json TEXT NOT NULL,
    normalized_hash TEXT NOT NULL,
    conflict_key TEXT,
    provenance_json TEXT NOT NULL,
    confidence REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
    state TEXT NOT NULL,                  -- proposed | active | rejected
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    verified_at TEXT,
    expires_at TEXT,
    pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    rejection_reason TEXT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0)
);

CREATE INDEX idx_learning_scope
    ON learning_records(scope_kind, scope_key, state, updated_at);
CREATE INDEX idx_learning_expiry
    ON learning_records(expires_at) WHERE expires_at IS NOT NULL;
CREATE INDEX idx_learning_conflicts
    ON learning_records(scope_kind, scope_key, kind, conflict_key)
    WHERE conflict_key IS NOT NULL AND state != 'rejected';
CREATE UNIQUE INDEX idx_learning_live_dedup
    ON learning_records(scope_kind, scope_key, kind, normalized_hash)
    WHERE state != 'rejected';
