-- M6 Task 6.3 — coordinated multi-repository campaigns.
--
-- A campaign is a COORDINATOR, never an authority. It aggregates the outcomes
-- of ordinary per-repository workflow runs created through
-- WorkflowStore::create_run_idempotent_owned (crates/workflow/src/store.rs:354).
-- It grants nothing: no shared worktree, no shared budget, no blanket approval,
-- no shared secret lease.

CREATE TABLE campaigns (
    id TEXT PRIMARY KEY,
    -- Kernel-derived; the coordinator's authority never exceeds this uid's.
    owner_uid INTEGER NOT NULL,
    title TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN (
        'api-migration', 'schema-migration', 'dependency-upgrade',
        'ownership-review', 'custom'
    )),
    -- The workflow every child run instantiates. One workflow, N runs — there
    -- is no campaign-specific runtime.
    workflow_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'planning', 'running', 'partially-failed', 'completed', 'cancelled'
    )),
    -- Denormalized rollup, maintained in the same transaction as the child
    -- transitions. Never authoritative — recomputable from campaign_runs.
    repository_count INTEGER NOT NULL DEFAULT 0 CHECK (repository_count >= 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    terminal_at TEXT,
    UNIQUE (owner_uid, idempotency_key),
    CHECK (terminal_at IS NULL OR state IN ('completed', 'cancelled', 'partially-failed'))
);

CREATE INDEX idx_campaigns_owner_state ON campaigns (owner_uid, state, updated_at DESC);

CREATE TABLE campaign_repositories (
    campaign_id TEXT NOT NULL REFERENCES campaigns(id),
    repository_id TEXT NOT NULL,
    -- Snapshot of the federated identity at enrolment, so a later identity
    -- change cannot silently retarget an in-flight campaign.
    federated_id TEXT NOT NULL,
    -- Per-repository worktree. NOT shared: a shared worktree would let a
    -- denial in repository A be bypassed by an approved write in repository B.
    worktree_path TEXT,
    -- Per-repository ceiling in the smallest currency unit. NULL means "no
    -- campaign-specific ceiling" — the repository's own budget still applies.
    -- It is never a grant: it can only lower the effective ceiling.
    budget_minor_units INTEGER CHECK (budget_minor_units IS NULL OR budget_minor_units >= 0),
    -- 'per-effect' requires a separate human decision for every proposed
    -- action in this repository. There is deliberately no 'campaign-wide'
    -- value: blanket approval across repositories is prohibited by design §8.7.
    approval_mode TEXT NOT NULL DEFAULT 'per-effect'
        CHECK (approval_mode IN ('per-effect', 'per-run')),
    state TEXT NOT NULL CHECK (state IN (
        'pending', 'running', 'succeeded', 'failed', 'denied', 'skipped'
    )),
    enrolled_at TEXT NOT NULL,
    terminal_at TEXT,
    PRIMARY KEY (campaign_id, repository_id)
);

CREATE INDEX idx_campaign_repositories_state
    ON campaign_repositories (campaign_id, state);

CREATE TABLE campaign_runs (
    campaign_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    -- The ordinary workflow run. UNIQUE because one run belongs to at most one
    -- campaign slot; a retry mints a new run with a new attempt number rather
    -- than rebinding this one.
    run_id TEXT NOT NULL UNIQUE,
    attempt INTEGER NOT NULL CHECK (attempt >= 1),
    -- The exact key handed to create_run_idempotent_owned. Persisting it makes
    -- a crashed coordinator's retry adopt the existing run instead of forking
    -- a second one.
    idempotency_key TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    terminal_at TEXT,
    PRIMARY KEY (campaign_id, repository_id, attempt),
    FOREIGN KEY (campaign_id, repository_id)
        REFERENCES campaign_repositories(campaign_id, repository_id)
);

CREATE TABLE campaign_approvals (
    campaign_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    approval_id TEXT NOT NULL,
    -- crates/daemon/src/approvals.rs:970. Bound per repository: the SAME digest
    -- approved in repository A confers nothing in repository B, which is the
    -- concrete meaning of "no blanket approval".
    action_digest TEXT NOT NULL CHECK (length(action_digest) = 64),
    decision TEXT NOT NULL CHECK (decision IN ('pending', 'approved', 'rejected', 'expired')),
    decided_at TEXT,
    decided_by_uid INTEGER,
    PRIMARY KEY (campaign_id, repository_id, approval_id),
    FOREIGN KEY (campaign_id, repository_id)
        REFERENCES campaign_repositories(campaign_id, repository_id)
);

CREATE UNIQUE INDEX idx_campaign_approvals_digest
    ON campaign_approvals (campaign_id, repository_id, action_digest);

-- Effect ledger: what actually happened per repository. `effect_digest` is
-- UNIQUE per (campaign, repository), which is what makes an idempotent retry
-- safe — a re-driven attempt that recomputes the same effect cannot apply it
-- twice.
CREATE TABLE campaign_effects (
    id TEXT PRIMARY KEY,
    campaign_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    effect_kind TEXT NOT NULL,
    effect_digest TEXT NOT NULL CHECK (length(effect_digest) = 64),
    applied_at TEXT NOT NULL,
    UNIQUE (campaign_id, repository_id, effect_digest),
    FOREIGN KEY (campaign_id, repository_id)
        REFERENCES campaign_repositories(campaign_id, repository_id)
);
