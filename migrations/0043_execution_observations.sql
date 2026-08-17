-- Milestone 3 Task 3.3: one normalized row per MEASURED execution observation,
-- keyed by logical run/attempt. The existing `runs.{prompt_tokens,
-- completion_tokens, cost_micros}` compatibility columns (0032) stay exactly as
-- they are — this table is additive and does not replace them.
--
-- EVERY measurement column below is nullable and NULL means NOT MEASURED. It is
-- never a fabricated zero, and a reader must never coerce it to one: an absent
-- value and a measured `Some(0)` are different facts, pinned on the wire by
-- `absent_measurement_is_distinct_from_measured_zero`
-- (`crates/protocol/src/analytics.rs:207`). The nullability here is what makes
-- `MeasurementCoverage { measured, total }` computable at all.

CREATE TABLE execution_observations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Resolved from the run's session at write time. Every analytics query
    -- filters on this BEFORE grouping, counting, or paging.
    owner_uid INTEGER NOT NULL,
    run_id TEXT NOT NULL REFERENCES runs(id),
    -- Logical attempt within the run: `workflow_nodes.attempt` (0010) for a
    -- workflow node, 0 for a plain run. Part of the dedup key so a re-driven
    -- attempt after recovery adds a row while a retried WRITE of the same
    -- attempt does not.
    attempt INTEGER NOT NULL DEFAULT 0,
    -- '' for a non-workflow run. NOT NULL with a '' sentinel rather than a
    -- nullable column because SQLite treats NULLs as DISTINCT inside a UNIQUE
    -- index: a nullable `node_id` would silently let a crash-retried write
    -- insert a duplicate observation and double every aggregate.
    node_id TEXT NOT NULL DEFAULT '',
    session_id TEXT REFERENCES sessions(id),
    -- Denormalized dimensions. Copied at write time rather than joined at query
    -- time so an aggregate over six months does not depend on a session still
    -- existing — and so a later tombstone does not retroactively change a
    -- historical cost report.
    repository_id TEXT,
    workflow_id TEXT,
    workflow_run_id TEXT,
    -- `TaskClass::as_str()` (`crates/routing/src/classify.rs:39`), e.g.
    -- 'small-bug-fix'. NULL means NOT CLASSIFIED — never write 'general' as a
    -- stand-in, because 'general' is a real classification the router assigns.
    task_class TEXT,
    provider TEXT,
    model_id TEXT,
    -- The concrete endpoint, matching `model_task_outcomes.endpoint` (0025), so
    -- the two stores can be reconciled without a lookup table.
    endpoint TEXT,
    -- `RouteArm::as_str()` (`crates/routing/src/arms.rs:65`), e.g.
    -- 'router-escalation'. NULL means the request did not go through the router.
    route TEXT,

    -- --- measurements: NULL == not measured -------------------------------
    input_tokens INTEGER CHECK (input_tokens IS NULL OR input_tokens >= 0),
    output_tokens INTEGER CHECK (output_tokens IS NULL OR output_tokens >= 0),
    -- No current producer measures these two. See §6 gotcha 2: `ModelUsage`
    -- (`crates/runtime/src/agent.rs:655-668`) has no cached/reasoning fields, so
    -- until it gains them these columns stay NULL and their coverage is
    -- honestly reported as `measured: 0`. The columns exist now because the
    -- migration is append-only and the contract already demands the dimension.
    cached_tokens INTEGER CHECK (cached_tokens IS NULL OR cached_tokens >= 0),
    reasoning_tokens INTEGER
        CHECK (reasoning_tokens IS NULL OR reasoning_tokens >= 0),
    -- Micro-USD, same unit as `runs.cost_micros` (0032) and
    -- `AnalyticsMetrics.cost_micros`. Commonly NULL while tokens are populated:
    -- the live driver measures tokens but has no price.
    cost_micros INTEGER CHECK (cost_micros IS NULL OR cost_micros >= 0),
    latency_ms INTEGER CHECK (latency_ms IS NULL OR latency_ms >= 0),
    retry_count INTEGER CHECK (retry_count IS NULL OR retry_count >= 0),
    escalation_count INTEGER
        CHECK (escalation_count IS NULL OR escalation_count >= 0),
    -- Grader score scaled to millionths, matching
    -- `AnalyticsMetrics.grader_score_micros`. Integer, not REAL: a float would
    -- make two identical evaluations compare unequal after a round trip.
    grader_score_micros INTEGER,
    -- `AnalyticsCompletion` serde tag (snake_case — this enum DOES carry
    -- `rename_all = "snake_case"`, unlike the inbox enums). NULL means the
    -- outcome was not observed. 'incomplete' exists precisely so absence of
    -- failure is never recorded as success.
    completion TEXT CHECK (completion IS NULL OR completion IN (
        'successful', 'failed', 'cancelled', 'incomplete'
    )),
    observed_at TEXT NOT NULL,

    -- The idempotency key. A recovered or re-driven write for the same
    -- logical attempt updates in place instead of double-counting, exactly as
    -- `idx_model_task_outcomes_run` (0025) does for routing outcomes.
    UNIQUE (run_id, attempt, node_id)
);

-- Every index leads with `owner_uid` so the authorization predicate is part of
-- the seek. An aggregate that filters by owner only after scanning leaks
-- another user's volume through timing and through `MeasurementCoverage.total`.
CREATE INDEX idx_execution_observations_time
    ON execution_observations (owner_uid, observed_at, id);
CREATE INDEX idx_execution_observations_model
    ON execution_observations (owner_uid, model_id, observed_at);
CREATE INDEX idx_execution_observations_provider
    ON execution_observations (owner_uid, provider, observed_at);
CREATE INDEX idx_execution_observations_repository
    ON execution_observations (owner_uid, repository_id, observed_at);
CREATE INDEX idx_execution_observations_workflow
    ON execution_observations (owner_uid, workflow_id, observed_at);
CREATE INDEX idx_execution_observations_task_class
    ON execution_observations (owner_uid, task_class, observed_at);

-- Task 3.4 configuration, landed here because 3.4 is assigned no migration and
-- the numbered sequence is fixed (0044 belongs to M4 automation).
CREATE TABLE analytics_budgets (
    id TEXT PRIMARY KEY,
    owner_uid INTEGER NOT NULL,
    scope TEXT NOT NULL
        CHECK (scope IN ('owner', 'repository', 'workflow', 'model')),
    -- '' for scope='owner'. NOT NULL with a sentinel for the same SQLite
    -- UNIQUE/NULL reason as `node_id` above.
    scope_value TEXT NOT NULL DEFAULT '',
    -- Which measured dimension the threshold applies to. Only measured
    -- dimensions are eligible: a budget over an unmeasured dimension would
    -- alert on `NULL` treated as 0, which is the exact defect the program rule
    -- forbids.
    dimension TEXT NOT NULL CHECK (dimension IN (
        'cost_micros', 'input_tokens', 'output_tokens', 'latency_ms'
    )),
    window TEXT NOT NULL CHECK (window IN ('day', 'week', 'month')),
    threshold INTEGER NOT NULL CHECK (threshold > 0),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (owner_uid, scope, scope_value, dimension, window)
);

CREATE INDEX idx_analytics_budgets_active
    ON analytics_budgets (owner_uid, enabled, dimension);
