-- The hook engine (STEP 6.4, second half): registered hook definitions and the
-- dispatch audit.
--
-- A hook.toml can come from a cloned repository, so a hook definition is
-- untrusted input that wants to observe — and, for a `mutate` hook, rewrite —
-- tool calls. Two rules are structural here rather than conventional:
--
-- 1. Discovery is not activation. A hook is inserted with approval_state
--    'pending' and never dispatched until a human approves it. There is no
--    default that makes a repository-scoped hook live on clone.
-- 2. Approval binds to `content_hash`, the hash of the hook.toml that was
--    approved. Editing the file changes the hash, which no longer matches the
--    approved one, so the hook falls back to 'pending' rather than inheriting
--    the old decision. This is the same approve-then-substitute defence the
--    plugin lifecycle applies to update receipts.
--
-- `priority` orders dispatch; ties break on (priority, id) so ordering is
-- total and deterministic and a hostile package cannot make ordering depend on
-- filesystem enumeration order.
CREATE TABLE hooks (
    id TEXT PRIMARY KEY,
    registry_item_id TEXT,                -- the RegistryItemKind::Hook row, once registered
    hook_id TEXT NOT NULL,                -- the manifest's `id` slug, e.g. rust.verify-after-patch
    name TEXT NOT NULL,
    scope_kind TEXT NOT NULL,             -- user | repository | organization | system
    scope_key TEXT NOT NULL,
    event TEXT NOT NULL,                  -- the lifecycle event this binds to
    kind TEXT NOT NULL,                   -- observe | validate | mutate
    priority INTEGER NOT NULL,
    source_path TEXT NOT NULL,            -- the hook.toml this was parsed from
    content_hash TEXT NOT NULL,           -- hash of the hook.toml bytes
    spec_json TEXT NOT NULL,              -- the parsed, validated specification
    approval_state TEXT NOT NULL,         -- pending | approved | rejected
    approved_content_hash TEXT,           -- the hash a human actually approved
    approved_by TEXT,
    approved_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (scope_kind, scope_key, hook_id)
);

CREATE INDEX idx_hooks_dispatch
    ON hooks(event, approval_state, priority, id);

-- One row per hook invocation. `verdict` records what the hook asked for;
-- `applied` records what the engine did with it, and the two differ whenever a
-- rewrite failed to re-pass policy.
--
-- `rewrote_action` is the digest of the action a mutate hook proposed, never the
-- action itself: a rewrite is only ever executed after re-entering the policy
-- engine as a fresh proposal, so the executed action is audited by the ordinary
-- proposal record and this column exists to link the two.
CREATE TABLE hook_dispatches (
    id TEXT PRIMARY KEY,
    hook_row_id TEXT NOT NULL REFERENCES hooks(id) ON DELETE CASCADE,
    run_id TEXT,
    event TEXT NOT NULL,
    subject_digest TEXT NOT NULL,         -- digest of the action/tool call dispatched on
    verdict TEXT NOT NULL,                -- observe | allow | deny | rewrite
    applied TEXT NOT NULL,                -- allowed | denied | rewrite-reentered | rewrite-refused
    rewrote_action TEXT,
    exit_status INTEGER,
    timed_out INTEGER NOT NULL DEFAULT 0 CHECK (timed_out IN (0, 1)),
    duration_ms INTEGER NOT NULL CHECK (duration_ms >= 0),
    output_bytes INTEGER NOT NULL DEFAULT 0 CHECK (output_bytes >= 0),
    error TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_hook_dispatches_hook
    ON hook_dispatches(hook_row_id, created_at);
CREATE INDEX idx_hook_dispatches_run
    ON hook_dispatches(run_id, created_at);
