-- Policy-controlled real execution traces, experiments, and canary evidence.
-- PostgreSQL; forward-only.

CREATE TABLE quality_observations (
    id                    UUID PRIMARY KEY,
    organization_id       UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    repository_id         UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    -- Provenance back to the execution. Nullable because an observation may come
    -- from a runner job with no workflow, or a workflow node with no runner.
    workflow_run_id       TEXT,
    node_id               TEXT,
    runner_job_id         UUID REFERENCES runner_jobs(id) ON DELETE SET NULL,
    -- TaskClass::as_str() verbatim (crates/routing/src/classify.rs), the same
    -- key model_task_outcomes uses (migrations/0025_routing_outcomes.sql), so
    -- local and control-plane task-class data join without a lookup table.
    task_class            TEXT NOT NULL,
    model_id              TEXT NOT NULL,
    -- The routing policy revision, 'router/<name>/<version>'
    -- (RoutingPolicy::registry_key, crates/routing/src/policy.rs:150-153).
    routing_policy        TEXT,
    -- The publication class this observation was captured under
    -- (design §6.4). Capture beyond the org's policy is refused, not truncated.
    publication_class     TEXT NOT NULL,
    -- The trace payload lives in object storage as a classified artifact ref;
    -- only measured metadata lives here (task 9.3).
    trace_object_key      TEXT,
    trace_content_hash    TEXT,
    trace_classification  TEXT NOT NULL,
    -- === Measured metrics. Every one nullable. NULL means NOT MEASURED. ===
    input_tokens          BIGINT CHECK (input_tokens IS NULL OR input_tokens >= 0),
    output_tokens         BIGINT CHECK (output_tokens IS NULL OR output_tokens >= 0),
    cached_tokens         BIGINT CHECK (cached_tokens IS NULL OR cached_tokens >= 0),
    reasoning_tokens      BIGINT CHECK (reasoning_tokens IS NULL OR reasoning_tokens >= 0),
    cost_micro_usd        BIGINT CHECK (cost_micro_usd IS NULL OR cost_micro_usd >= 0),
    latency_ms            BIGINT CHECK (latency_ms IS NULL OR latency_ms >= 0),
    -- codypendent_eval::TraceGrade::score() — an i32 sum of signal polarities,
    -- legitimately negative. NULL when no grade was computed.
    grade_score           INTEGER,
    -- The graded signals, as an ordered array of Signal wire strings. Empty
    -- array = graded, no signals. NULL = not graded. The distinction matters.
    grade_signals         JSONB,
    succeeded             BOOLEAN,
    escalated             BOOLEAN,
    retry_count           INTEGER CHECK (retry_count IS NULL OR retry_count >= 0),
    observed_at           TIMESTAMPTZ NOT NULL,
    captured_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX ix_quality_observations_class
    ON quality_observations (organization_id, task_class, observed_at DESC);
CREATE INDEX ix_quality_observations_model
    ON quality_observations (organization_id, model_id, observed_at DESC);
CREATE INDEX ix_quality_observations_repository
    ON quality_observations (repository_id, observed_at DESC);

CREATE TABLE quality_experiments (
    id                    UUID PRIMARY KEY,
    organization_id       UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    -- 'shadow' | 'canary'
    kind                  TEXT NOT NULL,
    -- The local promotion candidate this experiment produces evidence for.
    -- A TEXT id, not a FK: promotion_candidates lives in the daemon's SQLite
    -- (migrations/0015_promotion.sql) and is deliberately NOT mirrored here.
    candidate_id          TEXT NOT NULL,
    artifact_kind         TEXT NOT NULL,
    artifact_name         TEXT NOT NULL,
    artifact_version      INTEGER NOT NULL,
    -- The RouteArm wire string for each side (crates/routing/src/arms.rs:33-47).
    baseline_arm          TEXT NOT NULL,
    candidate_arm         TEXT NOT NULL,
    -- Deterministic assignment seed. Assignment is a pure function of
    -- (seed, eligibility key) so the same input lands in the same arm on every
    -- replay, and a reassignment is impossible without changing the seed.
    assignment_seed       BYTEA NOT NULL,
    -- Fraction of the eligible population routed to the candidate, in basis
    -- points. A shadow experiment runs both sides on 100% and discards the
    -- candidate's effects, so it stores 10000.
    candidate_share_bps   INTEGER NOT NULL CHECK (candidate_share_bps BETWEEN 0 AND 10000),
    -- Independent budget ceiling for the candidate side, micro-USD. NOT NULL:
    -- an experiment without a ceiling can spend without bound.
    candidate_budget_micro_usd BIGINT NOT NULL CHECK (candidate_budget_micro_usd > 0),
    -- The analysis plan, fixed BEFORE the experiment starts: minimum samples,
    -- horizon kind ('fixed' | 'sequential'), non-inferiority margin, cost and
    -- latency limits, alpha. Immutable after activation — see §5.3.
    analysis_plan         JSONB NOT NULL,
    -- draft | active | stopped | analyzed | rolled-back
    state                 TEXT NOT NULL DEFAULT 'draft',
    activated_at          TIMESTAMPTZ,
    stopped_at            TIMESTAMPTZ,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT quality_experiments_candidate UNIQUE (candidate_id, kind)
);

CREATE INDEX ix_quality_experiments_active
    ON quality_experiments (organization_id) WHERE state = 'active';

CREATE TABLE quality_experiment_samples (
    id                    BIGSERIAL PRIMARY KEY,
    experiment_id         UUID NOT NULL REFERENCES quality_experiments(id) ON DELETE CASCADE,
    -- 'baseline' | 'candidate'
    arm                   TEXT NOT NULL CHECK (arm IN ('baseline', 'candidate')),
    observation_id        UUID NOT NULL REFERENCES quality_observations(id) ON DELETE CASCADE,
    -- The stable key assignment was computed from. UNIQUE with experiment_id so
    -- one unit of the population contributes at most one sample per experiment —
    -- the structural defence against a favourable unit being counted twice.
    assignment_key        TEXT NOT NULL,
    recorded_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT quality_experiment_samples_unit UNIQUE (experiment_id, assignment_key)
);

CREATE INDEX ix_quality_experiment_samples_arm
    ON quality_experiment_samples (experiment_id, arm);

-- The server-computed comparison. One row per analysis run; the latest is the
-- evidence a human approves against. Every metric column is nullable.
CREATE TABLE quality_comparisons (
    id                    UUID PRIMARY KEY,
    experiment_id         UUID NOT NULL REFERENCES quality_experiments(id) ON DELETE CASCADE,
    baseline_samples      INTEGER NOT NULL CHECK (baseline_samples >= 0),
    candidate_samples     INTEGER NOT NULL CHECK (candidate_samples >= 0),
    -- Serialized codypendent_routing::RouteEvalReport, built from measured
    -- samples only (task 9.5). An arm with no samples is ABSENT from the report,
    -- which makes meets_release_gate() return false
    -- (crates/routing/src/arms.rs:138-146) — the correct behaviour, preserved.
    route_eval_report     JSONB NOT NULL,
    -- Non-inferiority verdict on quality, and the limit checks. NULL = the test
    -- could not be evaluated (insufficient samples, or the metric was never
    -- measured). NULL is NOT a pass and NOT a fail.
    quality_non_inferior  BOOLEAN,
    cost_within_limit     BOOLEAN,
    latency_within_limit  BOOLEAN,
    -- Which measurements were absent. A named list, so the UI can render
    -- "unknown" explicitly rather than rendering a blank that reads as zero
    -- (task 9.6).
    missing_measurements  JSONB NOT NULL,
    -- pass | fail | insufficient-evidence | safety-rollback
    verdict               TEXT NOT NULL,
    computed_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX ix_quality_comparisons_experiment
    ON quality_comparisons (experiment_id, computed_at DESC);

-- Drift: a task class whose measured quality/cost moved against a reference
-- window, independent of any experiment.
CREATE TABLE quality_drift_alerts (
    id                    UUID PRIMARY KEY,
    organization_id       UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    task_class            TEXT NOT NULL,
    model_id              TEXT,
    -- quality | cost | latency | escalation-rate
    dimension             TEXT NOT NULL,
    reference_window_start TIMESTAMPTZ NOT NULL,
    reference_window_end   TIMESTAMPTZ NOT NULL,
    current_window_start   TIMESTAMPTZ NOT NULL,
    current_window_end     TIMESTAMPTZ NOT NULL,
    reference_value       DOUBLE PRECISION NOT NULL,
    current_value         DOUBLE PRECISION NOT NULL,
    reference_samples     INTEGER NOT NULL CHECK (reference_samples > 0),
    current_samples       INTEGER NOT NULL CHECK (current_samples > 0),
    -- open | acknowledged | resolved
    state                 TEXT NOT NULL DEFAULT 'open',
    detected_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    acknowledged_by       UUID,
    acknowledged_at       TIMESTAMPTZ
);

CREATE INDEX ix_quality_drift_alerts_open
    ON quality_drift_alerts (organization_id, detected_at DESC) WHERE state = 'open';

-- The promotion approval receipt, control-plane side. This records that a
-- scoped human approved against exact evidence; the promotion STATE MACHINE
-- still lives in the daemon's SQLite (migrations/0015_promotion.sql) and is the
-- only thing that can mint a PromotionRecord.
CREATE TABLE quality_promotion_approvals (
    id                    UUID PRIMARY KEY,
    organization_id       UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    repository_id         UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    candidate_id          TEXT NOT NULL,
    comparison_id         UUID NOT NULL REFERENCES quality_comparisons(id),
    -- The authenticated human. A workload identity in this column is a bug; the
    -- API must reject a non-human principal before insert (task 9.6).
    approver_user_id      UUID NOT NULL REFERENCES users(id),
    -- The role the approval was exercised under, and the scope it was granted
    -- in. Recorded so a later scope change does not retroactively legitimise
    -- this approval.
    approver_role         TEXT NOT NULL,
    -- sha256 over the canonical (candidate, artifact version, comparison_id,
    -- verdict, metric values) bytes. The approval binds to EXACT evidence:
    -- re-running the analysis produces a different digest and invalidates it
    -- (design §12.2, "approval decisions bound to exact action digests").
    action_digest         BYTEA NOT NULL,
    expires_at            TIMESTAMPTZ NOT NULL,
    approved_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT quality_promotion_approvals_digest UNIQUE (candidate_id, action_digest)
);

CREATE INDEX ix_quality_promotion_approvals_candidate
    ON quality_promotion_approvals (candidate_id, approved_at DESC);
