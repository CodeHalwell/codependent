-- Executable skills (STEP 6.3/6.4): the audit trail for every skill behaviour
-- that was actually run.
--
-- Registered skills were marked executable long before anything could run one,
-- so there was no record of a skill ever having executed. This table is written
-- by the skill runner itself (crates/knowledge/src/skill_exec.rs), once per
-- invocation, whether the run succeeded, was refused, or was terminated by a
-- resource ceiling — a refusal is the row a reviewer most wants to find.
--
-- `content_hash` is the package hash the runner re-verified immediately before
-- executing, not the hash recorded at registration: the two are equal by
-- construction (a mismatch refuses the run), and storing the verified one makes
-- the row self-contained evidence of WHICH bytes ran.
--
-- `profile_json` is the closed sandbox profile the run was confined by, after
-- $REPOSITORY/$WORKTREE substitution — so an audit can see the concrete paths
-- granted, not the manifest's placeholders.
--
-- `denials_json` is the ordered list of privileged host requests the run policy
-- refused. A WASM guest sees only an opaque error code for these, by design, so
-- this column is the only place the refusal is legible.
CREATE TABLE skill_executions (
    id TEXT PRIMARY KEY,
    registry_item_id TEXT NOT NULL,
    skill_name TEXT NOT NULL,
    skill_version TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    runtime TEXT NOT NULL,                -- script | module
    entrypoint TEXT NOT NULL,             -- path relative to the package directory
    profile_json TEXT NOT NULL,
    limits_json TEXT NOT NULL,
    outcome TEXT NOT NULL,                -- completed | refused | terminated
    exit_status INTEGER,                  -- process exit code, or a wasm guest status
    timed_out INTEGER NOT NULL DEFAULT 0 CHECK (timed_out IN (0, 1)),
    duration_ms INTEGER NOT NULL CHECK (duration_ms >= 0),
    fuel_consumed INTEGER,                -- wasm only; NULL for a script run
    output_bytes INTEGER NOT NULL DEFAULT 0 CHECK (output_bytes >= 0),
    output_truncated INTEGER NOT NULL DEFAULT 0 CHECK (output_truncated IN (0, 1)),
    denials_json TEXT NOT NULL DEFAULT '[]',
    error TEXT,                           -- the refusal reason when outcome <> 'completed'
    created_at TEXT NOT NULL
);

CREATE INDEX idx_skill_executions_item
    ON skill_executions(registry_item_id, created_at);
CREATE INDEX idx_skill_executions_outcome
    ON skill_executions(outcome, created_at);
