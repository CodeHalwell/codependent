-- Milestone 1: additive Session Library metadata and deterministic source
-- bookkeeping. Search text remains derived from the append-only event ledger;
-- Task 2.1 may select FTS5 only after a runtime probe, otherwise Tantivy.

ALTER TABLE sessions ADD COLUMN internal INTEGER NOT NULL DEFAULT 0
    CHECK (internal IN (0, 1));
ALTER TABLE sessions ADD COLUMN parent_session_id TEXT REFERENCES sessions(id);
ALTER TABLE sessions ADD COLUMN parent_run_id TEXT REFERENCES runs(id);
ALTER TABLE sessions ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0
    CHECK (pinned IN (0, 1));
ALTER TABLE sessions ADD COLUMN archived_at TEXT;
ALTER TABLE sessions ADD COLUMN tombstoned_at TEXT;
ALTER TABLE sessions ADD COLUMN deletion_mode TEXT
    CHECK (deletion_mode IS NULL OR (
        tombstoned_at IS NOT NULL AND deletion_mode IN ('retention_policy', 'tombstone_only')
    ));
ALTER TABLE sessions ADD COLUMN purge_after TEXT
    CHECK (purge_after IS NULL OR tombstoned_at IS NOT NULL);
ALTER TABLE sessions ADD COLUMN repository_id TEXT;
ALTER TABLE sessions ADD COLUMN repository TEXT;
ALTER TABLE sessions ADD COLUMN workspace TEXT;
ALTER TABLE sessions ADD COLUMN last_activity_at TEXT;
ALTER TABLE sessions ADD COLUMN last_run_id TEXT REFERENCES runs(id);
ALTER TABLE sessions ADD COLUMN run_state TEXT;

CREATE INDEX idx_sessions_library_owner_activity
    ON sessions (owner_uid, tombstoned_at, internal, archived_at, pinned, updated_at DESC, id);
CREATE INDEX idx_sessions_parent_session
    ON sessions (parent_session_id) WHERE parent_session_id IS NOT NULL;
CREATE INDEX idx_sessions_parent_run
    ON sessions (parent_run_id) WHERE parent_run_id IS NOT NULL;

-- `source_id` is a daemon-derived stable identity within its source type, such
-- as an event sequence, artifact id, or path/symbol discriminator. Re-indexing
-- the same durable source replaces its hash rather than minting another row.
-- Ownership is always joined through `sessions`; it is intentionally not
-- duplicated in this table where it could drift.
CREATE TABLE session_search_sources (
    session_id TEXT NOT NULL REFERENCES sessions(id),
    source_type TEXT NOT NULL CHECK (
        source_type IN ('title', 'transcript', 'tool', 'patch', 'artifact', 'path', 'symbol')
    ),
    source_id TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    indexed_at TEXT NOT NULL,
    event_sequence INTEGER,
    run_id TEXT REFERENCES runs(id),
    artifact_id TEXT REFERENCES artifacts(id),
    PRIMARY KEY (session_id, source_type, source_id),
    FOREIGN KEY (session_id, event_sequence)
        REFERENCES events(session_id, sequence)
);

CREATE INDEX idx_session_search_sources_event
    ON session_search_sources (session_id, event_sequence)
    WHERE event_sequence IS NOT NULL;
CREATE INDEX idx_session_search_sources_run
    ON session_search_sources (run_id) WHERE run_id IS NOT NULL;
CREATE INDEX idx_session_search_sources_artifact
    ON session_search_sources (artifact_id) WHERE artifact_id IS NOT NULL;
