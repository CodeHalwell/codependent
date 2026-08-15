-- Adoption 06: the daemon-owned pending-prompt queue (cline
-- pending-prompt-service.ts, made durable). One ordered list per session;
-- `position` is a sparse ordering key (renumbered on reorder), `delivery`
-- is 'queue' | 'steer'. Rows are deleted when consumed, drained, or
-- discarded; the ledger's PendingPromptsChanged snapshots are the
-- client-visible history.
CREATE TABLE IF NOT EXISTS pending_prompts (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    position INTEGER NOT NULL,
    text TEXT NOT NULL,
    mode TEXT NOT NULL,
    delivery TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_pending_prompts_session ON pending_prompts (session_id, position);
