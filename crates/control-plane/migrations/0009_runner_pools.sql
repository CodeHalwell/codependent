-- Warm pools and capability-aware autoscaling. PostgreSQL; forward-only.
-- Depends on M8's 0006_runner_jobs.sql (runners, runner_images, runner_jobs).

CREATE TABLE runner_pools (
    id                    UUID PRIMARY KEY,
    organization_id       UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name                  TEXT NOT NULL,
    -- Provider adapter key: 'firecracker' | 'macos' | 'container'. Selects a
    -- RunnerProvider impl; it is NOT a trust level and NOT a protocol variant.
    provider              TEXT NOT NULL,
    -- The pool's eligibility signature: os, arch, tool capabilities, region.
    -- A job is servable by this pool iff its eligibility is a subset. Stored as
    -- JSONB (not columns) because the capability vocabulary is protocol-owned
    -- and must not require a migration to extend.
    capability_signature  JSONB NOT NULL,
    -- The exact image every instance boots. FK, so a revoked image cannot be
    -- silently kept in service by a stale pool row.
    image_digest          TEXT NOT NULL REFERENCES runner_images(digest),
    region                TEXT NOT NULL,
    -- Warm floor and hard ceiling. min_warm = 0 is legal (cold pool). max_total
    -- has no NULL: an autoscaler without a ceiling is a billing incident.
    min_warm              INTEGER NOT NULL DEFAULT 0 CHECK (min_warm >= 0),
    max_total             INTEGER NOT NULL CHECK (max_total > 0),
    CONSTRAINT runner_pools_min_le_max CHECK (min_warm <= max_total),
    -- Seconds an idle warm instance survives before scale-down reclaims it.
    idle_ttl_seconds      INTEGER NOT NULL CHECK (idle_ttl_seconds > 0),
    -- Absolute lifetime cap regardless of idleness. A long-lived "warm"
    -- instance is a persistence surface; design §11.2 requires disposable job
    -- environments.
    max_lifetime_seconds  INTEGER NOT NULL CHECK (max_lifetime_seconds > 0),
    enabled               BOOLEAN NOT NULL DEFAULT TRUE,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT runner_pools_name UNIQUE (organization_id, name)
);

CREATE INDEX ix_runner_pools_match
    ON runner_pools (organization_id, region) WHERE enabled;

CREATE TABLE runner_pool_instances (
    id                    UUID PRIMARY KEY,
    pool_id               UUID NOT NULL REFERENCES runner_pools(id) ON DELETE CASCADE,
    -- The provider's own handle (microVM id, VM name). Opaque to domain logic.
    provider_instance_id  TEXT NOT NULL,
    -- The runner row this instance registered as, once it came up. NULL while
    -- provisioning: an instance with no runner_id can never be claimed against.
    runner_id             UUID REFERENCES runners(id),
    -- provisioning | warm | leased | draining | terminated | failed
    state                 TEXT NOT NULL DEFAULT 'provisioning',
    -- The snapshot every instance is reset to before entering 'warm'. Recorded
    -- per instance, not only per pool, so a snapshot change is auditable against
    -- instances that predate it.
    snapshot_digest       TEXT NOT NULL,
    -- Set when the reset verification found writable state surviving from a
    -- previous job. Such an instance goes to 'failed' and is destroyed, never
    -- returned to 'warm' (task 9.2: "reject residual writable state").
    residual_state_found  BOOLEAN NOT NULL DEFAULT FALSE,
    -- Measured provisioning latency in milliseconds. NULL while provisioning and
    -- NULL forever if provisioning failed before readiness — a failed
    -- provision has no latency, it does not have latency zero.
    provision_ms          INTEGER CHECK (provision_ms IS NULL OR provision_ms >= 0),
    -- Reconciler idempotency: the key under which this instance was requested.
    -- Two concurrent reconcilers computing the same deficit produce the same key
    -- and therefore one instance.
    reconcile_key         TEXT NOT NULL,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    ready_at              TIMESTAMPTZ,
    last_assigned_at      TIMESTAMPTZ,
    terminated_at         TIMESTAMPTZ,
    CONSTRAINT runner_pool_instances_provider UNIQUE (pool_id, provider_instance_id),
    CONSTRAINT runner_pool_instances_reconcile UNIQUE (pool_id, reconcile_key)
);

CREATE INDEX ix_runner_pool_instances_warm
    ON runner_pool_instances (pool_id) WHERE state = 'warm';

CREATE INDEX ix_runner_pool_instances_reap
    ON runner_pool_instances (state, created_at)
    WHERE state IN ('provisioning', 'warm', 'draining');

-- Every scale decision, kept. A pool that thrashes is diagnosable only from the
-- decision history, and design §13 requires queue-depth/scheduling-latency
-- observability.
CREATE TABLE runner_pool_scale_events (
    id                    BIGSERIAL PRIMARY KEY,
    pool_id               UUID NOT NULL REFERENCES runner_pools(id) ON DELETE CASCADE,
    -- scale-up | scale-down | hold | blocked-at-max | blocked-revoked-image
    decision              TEXT NOT NULL,
    -- The inputs the decision was made from, so it can be replayed: queue depth
    -- of eligible jobs, warm count, total count, ceiling.
    eligible_queue_depth  INTEGER NOT NULL CHECK (eligible_queue_depth >= 0),
    warm_count            INTEGER NOT NULL CHECK (warm_count >= 0),
    total_count           INTEGER NOT NULL CHECK (total_count >= 0),
    delta                 INTEGER NOT NULL,
    reason                TEXT NOT NULL,
    decided_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX ix_runner_pool_scale_events_pool
    ON runner_pool_scale_events (pool_id, decided_at DESC);
