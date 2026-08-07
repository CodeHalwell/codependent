-- Durable continuation records for approval-gated document publication.
-- The publication plan is written before its approval request, so a daemon
-- restart can re-arm the waiter instead of failing the synthetic publish run
-- and discarding the only copy of the plan.

CREATE TABLE document_publish_jobs (
    approval_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    plan_json TEXT NOT NULL,
    -- pending | executing | completed | failed | cancelled
    state TEXT NOT NULL CHECK (
        state IN ('pending', 'executing', 'completed', 'failed', 'cancelled')
    ),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX ix_document_publish_jobs_run
    ON document_publish_jobs (run_id, state);
CREATE INDEX ix_document_publish_jobs_state
    ON document_publish_jobs (state);
