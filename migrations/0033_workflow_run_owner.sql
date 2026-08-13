-- Ownership for durable workflow runs.
--
-- `workflow_runs.run_id` is nullable by design (0010: "the session run this
-- workflow drives, WHEN BOUND"), and `WorkflowStore::create_run_idempotent`
-- inserts NULL for it unconditionally — so EVERY client-created workflow run is
-- unbound, and a gate that resolved ownership only through that column had no
-- owner to check for any of them.
--
-- This is the workflow-run twin of 0031's `sessions.owner_uid`: the principal
-- the daemon derived from the connection's peer credentials at creation time,
-- never a value a client can assert. NULL means a row created before this
-- migration; the daemon adopts those once at boot for the uid it runs as,
-- exactly as it already does for pre-0031 sessions.
ALTER TABLE workflow_runs ADD COLUMN owner_uid INTEGER;

CREATE INDEX IF NOT EXISTS ix_workflow_runs_owner_uid ON workflow_runs (owner_uid);
