-- Milestone 4: durable trigger and schedule automation.
--
-- Invocation policy lives HERE, not in a `WorkflowDefinition`: the same workflow
-- is bound to several sources with different operational and approval policies
-- (see crates/protocol/src/automation.rs module docs). A binding row is the sole
-- server-side authority for owner, repository, workflow, budget and approval —
-- no webhook payload or schedule occurrence may select any of them.

CREATE TABLE automation_bindings (
    id TEXT PRIMARY KEY,                       -- AutomationBindingId
    -- Kernel-derived at create time from the connection's peer credentials and
    -- never accepted from the wire, exactly as StartWorkflowRequest.owner_uid is
    -- stamped (crates/daemon/src/workflows.rs:46-49). Every later firing runs as
    -- this uid, not as the daemon uid.
    owner_uid INTEGER NOT NULL,
    name TEXT NOT NULL,
    source_type TEXT NOT NULL CHECK (source_type IN (
        'cron', 'one_time', 'github_webhook', 'signed_webhook', 'ci_failure',
        'repository_change', 'code_graph_change', 'dependency_alert',
        'manual', 'api'
    )),
    -- The serialized TriggerSource. Endpoint/key REFERENCES only; the contract
    -- forbids secret material crossing it, and this column is read back into
    -- clients verbatim.
    source_json TEXT NOT NULL,
    -- Denormalized out of source_json so an inbound delivery is an index probe
    -- rather than a table scan with JSON parsing on the hot ingest path.
    endpoint_id TEXT,
    -- Cron is split out of source_json for the same reason: the scheduler must
    -- compute next occurrences without deserializing every binding. `timezone`
    -- is stored separately from `next_fire_at` because next_fire_at is UTC and
    -- a DST transition can only be recomputed from the original zone.
    cron_expression TEXT,
    cron_timezone TEXT,
    one_time_at TEXT,
    -- Precomputed next occurrence in UTC. The scheduler's atomic claim is an
    -- UPDATE on this column, so it must be a real column and not a derived view.
    next_fire_at TEXT,
    -- The last occurrence timestamp actually persisted, so a restart can decide
    -- skip / run-once / bounded-catch-up from durable state rather than from
    -- "now minus an interval", which double-fires after a clock change.
    last_fire_at TEXT,
    workflow_id TEXT NOT NULL,
    workflow_version TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    -- RepositoryId is a ONE-WAY hash of the canonical checkout path
    -- (crates/codypendentd/src/scan.rs:1130 -> knowledge::stable_repository_id)
    -- and no repositories table exists in this schema. The canonical path is
    -- therefore persisted at create time; the dispatcher re-derives the id from
    -- it and refuses on mismatch (a moved/renamed checkout).
    repository_path TEXT NOT NULL,
    filters_json TEXT NOT NULL DEFAULT '{}',
    invocation_json TEXT NOT NULL DEFAULT '{}',
    -- Hot policy fields are projected out of invocation_json because dispatch
    -- reads them on every delivery and the scheduler queries on them.
    dedup_window_seconds INTEGER NOT NULL DEFAULT 86400
        CHECK (dedup_window_seconds >= 0),
    concurrency TEXT NOT NULL DEFAULT 'allow'
        CHECK (concurrency IN ('allow', 'skip', 'queue', 'replace')),
    missed_run TEXT NOT NULL DEFAULT 'skip'
        CHECK (missed_run IN ('skip', 'run_once', 'catch_up')),
    missed_run_max_occurrences INTEGER
        CHECK (missed_run_max_occurrences IS NULL
               OR (missed_run = 'catch_up' AND missed_run_max_occurrences > 0)),
    retry_max_attempts INTEGER NOT NULL DEFAULT 0 CHECK (retry_max_attempts >= 0),
    retry_initial_delay_seconds INTEGER NOT NULL DEFAULT 30
        CHECK (retry_initial_delay_seconds >= 0),
    retry_backoff_multiplier INTEGER NOT NULL DEFAULT 2
        CHECK (retry_backoff_multiplier >= 1),
    retry_max_delay_seconds INTEGER CHECK (retry_max_delay_seconds IS NULL
                                           OR retry_max_delay_seconds >= 0),
    -- Budget ceilings are NULLABLE and stay NULL when unset. Never write 0 for
    -- "unset": 0 means "no budget at all" and would silently kill every firing
    -- (measurement-honesty rule, conventions §8).
    budget_wall_time_seconds INTEGER CHECK (budget_wall_time_seconds IS NULL
                                            OR budget_wall_time_seconds > 0),
    budget_tool_calls INTEGER CHECK (budget_tool_calls IS NULL OR budget_tool_calls > 0),
    budget_tokens INTEGER CHECK (budget_tokens IS NULL OR budget_tokens > 0),
    budget_cost_micros INTEGER CHECK (budget_cost_micros IS NULL OR budget_cost_micros > 0),
    approval_mode TEXT NOT NULL DEFAULT 'inherit'
        CHECK (approval_mode IN ('inherit', 'always_require', 'policy_driven', 'preapproved')),
    -- Only meaningful for 'preapproved'. It is a REFERENCE to a receipt the
    -- approval store already issued; automation never mints one (see §4).
    approval_receipt TEXT
        CHECK (approval_receipt IS NULL OR approval_mode = 'preapproved'),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    -- Names are the operator-facing handle in CLI/desktop, so they must be
    -- unique per owner; a second principal may reuse a name freely.
    UNIQUE (owner_uid, name),
    -- Table-level constraints must follow every column definition (SQLite
    -- grammar), so the cross-column source/approval invariants are gathered
    -- here rather than beside the columns they constrain. A table CHECK is
    -- evaluated per row regardless of where it is written.
    CHECK (source_type <> 'cron'
           OR (cron_expression IS NOT NULL AND cron_timezone IS NOT NULL)),
    CHECK (source_type <> 'one_time' OR one_time_at IS NOT NULL),
    CHECK (approval_mode <> 'preapproved' OR approval_receipt IS NOT NULL)
);

-- The owner predicate leads every listing index so the ownership filter is part
-- of the index seek, not a post-filter (the session-library rule,
-- migrations/0040_session_library.sql).
CREATE INDEX idx_automation_bindings_owner
    ON automation_bindings (owner_uid, enabled, updated_at DESC, id);
CREATE INDEX idx_automation_bindings_due
    ON automation_bindings (next_fire_at)
    WHERE enabled = 1 AND next_fire_at IS NOT NULL;
CREATE INDEX idx_automation_bindings_endpoint
    ON automation_bindings (endpoint_id, enabled)
    WHERE endpoint_id IS NOT NULL;
CREATE INDEX idx_automation_bindings_repository
    ON automation_bindings (repository_id, enabled);
CREATE INDEX idx_automation_bindings_workflow
    ON automation_bindings (workflow_id, enabled);

-- One row per signed inbound endpoint. Separate from the binding because
-- several bindings share one endpoint (different filters over the same GitHub
-- app installation) and because the signing key is rotated independently of the
-- binding's policy.
CREATE TABLE automation_endpoints (
    endpoint_id TEXT PRIMARY KEY,          -- the path segment: POST /webhooks/<endpoint_id>
    owner_uid INTEGER NOT NULL,
    scheme TEXT NOT NULL CHECK (scheme IN ('hmac_sha256', 'ed25519')),
    -- An opaque reference (`env:X`, `keyring://…`, `secret://…`) resolved at
    -- verification time by the M5 broker. NEVER key material: this table is read
    -- by ordinary queries, dumped in support bundles, and shown in CLI output.
    signing_key_ref TEXT NOT NULL,
    -- Per-endpoint body ceiling, at or below the listener's MAX_BODY_BYTES
    -- (crates/integrations/src/webhook/server.rs:80). A public endpoint gets a
    -- tighter one than a loopback one.
    body_limit_bytes INTEGER NOT NULL DEFAULT 1048576
        CHECK (body_limit_bytes > 0 AND body_limit_bytes <= 8388608),
    -- Generic signed webhooks must carry a signed timestamp; a delivery older
    -- than this is refused before the dedup store is touched, so a captured
    -- delivery cannot be replayed after the dedup rows are pruned.
    replay_window_seconds INTEGER NOT NULL DEFAULT 300
        CHECK (replay_window_seconds > 0),
    -- Rotation without deletion: history must stay resolvable for audit.
    created_at TEXT NOT NULL,
    rotated_at TEXT,
    disabled_at TEXT
);

CREATE INDEX idx_automation_endpoints_owner
    ON automation_endpoints (owner_uid, disabled_at);

-- The atomic delivery reservation. This table — not the workflow store and not
-- webhook_deliveries — is the at-least-once/never-twice authority for a
-- binding's firings.
CREATE TABLE automation_receipts (
    id TEXT PRIMARY KEY,
    binding_id TEXT NOT NULL REFERENCES automation_bindings(id) ON DELETE CASCADE,
    -- Derived server-side from the binding id plus the normalized event fields
    -- named by DeduplicationPolicy.identity_fields (or the occurrence instant
    -- for a schedule). Never supplied by the payload.
    dedup_key TEXT NOT NULL,
    delivery_id TEXT,           -- X-GitHub-Delivery GUID where the source has one
    content_fingerprint TEXT,   -- sha256 over the signature, as ingest.rs:118 computes
    occurrence_at TEXT,         -- the scheduled instant, for cron/one_time
    reserved_at TEXT NOT NULL,
    -- reserved_at + dedup_window_seconds. A retention sweep may delete rows past
    -- this, which is why the *replay window* is enforced independently at
    -- verification (see automation_endpoints.replay_window_seconds).
    expires_at TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN
        ('reserved', 'dispatched', 'skipped', 'failed', 'abandoned')),
    -- Written exactly once, on the first successful dispatch, and never
    -- overwritten. A crash after reserving and before dispatch is recovered by
    -- re-driving THIS row; a crash after dispatch finds the run id here and
    -- returns the prior receipt instead of starting a second run.
    workflow_run_id TEXT,
    -- Handed verbatim to WorkflowStore::create_run_idempotent_owned
    -- (crates/workflow/src/store.rs:354), so the workflow store's own
    -- idempotency is the second, independent guard.
    workflow_idempotency_key TEXT NOT NULL,
    last_error TEXT,
    updated_at TEXT NOT NULL,
    -- THE crash-consistency constraint. The INSERT is the reservation.
    UNIQUE (binding_id, dedup_key)
);

CREATE INDEX idx_automation_receipts_binding
    ON automation_receipts (binding_id, reserved_at DESC, id);
CREATE INDEX idx_automation_receipts_pending
    ON automation_receipts (state, updated_at) WHERE state = 'reserved';
CREATE INDEX idx_automation_receipts_expiry ON automation_receipts (expires_at);
CREATE UNIQUE INDEX idx_automation_receipts_delivery
    ON automation_receipts (binding_id, delivery_id) WHERE delivery_id IS NOT NULL;

-- Trigger-level attempts. Deliberately NOT workflow node retry (which lives in
-- workflow_nodes.attempt, migrations/0010_workflow_runs.sql): a trigger retry
-- re-attempts *starting* the run; a node retry re-executes a step of a run that
-- already started. Conflating them makes one failed dispatch spend the
-- workflow's retry budget.
CREATE TABLE automation_attempts (
    id TEXT PRIMARY KEY,
    receipt_id TEXT NOT NULL REFERENCES automation_receipts(id) ON DELETE CASCADE,
    attempt INTEGER NOT NULL CHECK (attempt >= 1),
    started_at TEXT NOT NULL,
    finished_at TEXT,
    outcome TEXT CHECK (outcome IS NULL OR outcome IN (
        'started', 'skipped_concurrency', 'queued', 'replaced',
        'filtered', 'denied', 'error'
    )),
    -- A dotted code only (`policy.denied`, `automation.repository-moved`), never
    -- a rendered message containing payload text.
    error_code TEXT,
    next_retry_at TEXT,
    UNIQUE (receipt_id, attempt)
);

CREATE INDEX idx_automation_attempts_retry
    ON automation_attempts (next_retry_at)
    WHERE next_retry_at IS NOT NULL AND finished_at IS NULL;

-- Durable binding-level lease. The plan forbids using the in-process
-- DriveLockRegistry (crates/codypendentd/src/workflows.rs:438) for this: it is a
-- HashMap in one process and provides no exclusion across a restart or a second
-- daemon instance. `fence` is monotonic so a lease holder that wakes up after
-- its lease expired can detect it was fenced out before writing.
CREATE TABLE automation_leases (
    binding_id TEXT PRIMARY KEY REFERENCES automation_bindings(id) ON DELETE CASCADE,
    holder TEXT NOT NULL,                  -- DaemonInstanceId
    acquired_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    fence INTEGER NOT NULL DEFAULT 1 CHECK (fence >= 1)
);

CREATE INDEX idx_automation_leases_expiry ON automation_leases (expires_at);
