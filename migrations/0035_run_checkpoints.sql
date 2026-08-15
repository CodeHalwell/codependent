-- Adoption 04: per-turn filesystem checkpoints of a run's operating worktree.
-- One row per (run, turn ordinal); the git object itself is pinned by
-- refs/codypendent/checkpoints/<run_id>/<ordinal> in the run's repository.
-- `kind` is 'stash' (reset --hard to the stash base, clean -fd ONLY when the
-- snapshot carries an untracked third parent, then stash apply) or 'commit'
-- (clean tree at capture: reset --hard alone restores it).
-- UNIQUE (run_id, ordinal) is the durable "already checkpointed" guard: a
-- recovered or re-driven run must never overwrite the pre-turn snapshot with
-- the now-mutated workspace (cline checkpoint-hooks.ts, beforeModel comment).
CREATE TABLE run_checkpoints (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id),
    ordinal INTEGER NOT NULL,
    kind TEXT NOT NULL,
    commit_sha TEXT NOT NULL,
    base_commit TEXT NOT NULL,
    worktree_path TEXT NOT NULL,
    repository_path TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (run_id, ordinal)
);
CREATE INDEX idx_run_checkpoints_run ON run_checkpoints (run_id, ordinal);
