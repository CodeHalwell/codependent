-- Outcome 15 (delegation): reclaim worker branches, and index the board the
-- way concurrent workers read it.
--
-- 1. `branch_deleted_at` records when a lease's per-run branch
--    (`codypendent/run-<short>`) was deleted. Releasing a worktree removed the
--    directory but never the branch, and the only branch deletion — the reclaim
--    in `allocate` — keys on a worktree path derived from a fresh run id, so it
--    never matched a second time. Every writing run therefore left a
--    `codypendent/run-*` ref in the user's repository forever, and a workflow
--    that fans out to N workers left N per invocation. Recording the deletion
--    (rather than inferring it) keeps the sweep idempotent and auditable: NULL
--    means "branch may still exist", a timestamp means "we deleted it", and a
--    row that still has NULL after its worktree is gone is exactly what a
--    reclaim pass looks for. Deletion itself stays gated on
--    `git merge-base --is-ancestor <branch> HEAD`, so work is never discarded.
--
-- 2. Concurrent workers read and write the blackboard by (run, kind) — the
--    declared-output harvest, `resolve_proposed_patch`, and the consolidation
--    step all do. `ix_blackboard_items_run` covers the run alone; with several
--    workers posting at once, the kind belongs in the index too.

ALTER TABLE workspace_leases ADD COLUMN branch_deleted_at TEXT;

CREATE INDEX ix_blackboard_items_run_kind ON blackboard_items (workflow_run_id, kind);
