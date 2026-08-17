# M9 — Managed execution and continuous quality: implementation guide

**Companion to** `docs/superpowers/plans/2026-08-16-hybrid-platform-program.md` §Milestone 9
(tasks 9.1–9.7) and `docs/superpowers/specs/2026-08-16-hybrid-platform-program-design.md`
§8.5, §11.2–11.3, §13, §16.

Assumes M8 has landed. This document does **not** restate the plan's task list, **Files:**
lines, or commit messages. It supplies the DDL, the measurement contract, the exact extension
points in shipped code, and the traps — with the central one stated up front.

---

## 0. The one thing this milestone is about

**Quality evidence must be measured by the server, not supplied by the caller.**

This is not a hypothetical. It is a shipped, half-fixed defect. Migration
`migrations/0017_promotion_evidence.sql:1-4` records the first half of the fix verbatim:

> *"Server-consumed evidence for promotion gates. Callers no longer submit a bare pass/fail
> boolean: the regression verdict is derived from a persisted `SuiteReport`, and canary
> regression is derived from recorded metrics."*

The **regression** half is genuinely fixed: `SubmitEvalEvidence`
(`crates/protocol/src/command.rs:760-769`) takes a serialized `SuiteReport`, and the daemon
re-derives the verdict by counting failed cases
(`crates/codypendentd/src/promotion.rs:219-227`), refusing an unparseable or empty report.

The **canary** half is not. `PromotionAction::ObserveCanary { metrics: CanaryMetrics }`
(`crates/protocol/src/command.rs:1177`) still accepts the *numbers themselves* from the
request:

```rust
pub struct CanaryMetrics {              // crates/protocol/src/command.rs:1189-1195
    pub sample_count: u64,
    pub error_rate_bps: u16,
    pub baseline_error_rate_bps: u16,
    pub p95_latency_ms: u64,
    pub baseline_p95_latency_ms: u64,
}
```

The daemon validates their *shape* (`validate_canary_metrics`,
`crates/codypendentd/src/promotion.rs:350-363`) and derives the *verdict* from them
(`canary_regressed`, `:366-372`), then writes them to `promotion_canary_evidence` and feeds
`sample_count` into the state machine (`promotion.rs:263-296`). The verdict is server-derived;
the **evidence is not**. And the shipped caller is a human at a terminal:

```text
codypendent promote --step observe-canary \
  --sample-count 500 --error-rate-bps 40 --baseline-error-rate-bps 35 \
  --p95-latency-ms 900 --baseline-p95-latency-ms 950
```

(`crates/cli/src/main.rs:1012-1035` — every one of those five flags is `required for
observe-canary`.) A person types the numbers that decide whether a candidate regressed, and
`MIN_CANARY_SAMPLES = 100` (`crates/eval/src/promote.rs:83`) is satisfied by typing `500`.
`PromotionError::CanaryInsufficientEvidence`'s own doc comment names the hole:
*"This protects the trust boundary even if a client tries to submit one hand-written favorable
observation"* (`crates/eval/src/promote.rs:221-227`) — it stops *one* hand-written observation,
not a hand-written *five hundred*.

M9 task 9.5 closes it: *"Deprecate/reject production caller-supplied `CanaryMetrics`; retain
wire compatibility until a major protocol release."* Everything in §5 below is downstream of
that sentence.

---

## 1. Status — verified before writing

Verified against the working tree at `b8e17bd` (branch `release/v0.9.0`):

| Path | State |
|---|---|
| `crates/runner-provider/` | **absent** |
| `crates/runner/` | **absent** (M8) |
| `crates/runner-controller/` | **absent** (M8) |
| `crates/codypendentd/src/remote_node_executor.rs` | **absent** (M8) |
| `crates/control-plane/migrations/` | **absent** (M7 creates `0001`–`0005`; M8 adds `0006`–`0008`) |
| `apps/web/` | **absent** — only `apps/desktop/` and `extensions/vscode/` exist. M7 task 7.8 creates it; task 9.6's `apps/web/src/features/quality/*` depends on that. |

What **does** exist and M9 extends:

| Path | Shipped |
|---|---|
| `crates/eval/src/{case,cluster,grade,promote,regression,store,db}.rs` | the whole evaluation + promotion state machine |
| `crates/routing/src/{arms,router,policy,classify,profile,capability}.rs` | the router, the five eval arms, the release-gate report |
| `crates/codypendentd/src/{routing,promotion,routing_outcomes}.rs` | the daemon seams over both |
| `evals/{tasks,baselines,ci,fixtures}/` | 55 task files (13 `core`, 40 `extended`, 2 `regressions`), `baselines/core.json`, `ci/run_gate.sh` |
| `migrations/{0015_promotion,0017_promotion_evidence,0024_learning,0025_routing_outcomes}.sql` | local durable state |

---

## 2. What M9 reuses rather than reimplements

### 2.1 The promotion state machine — `crates/eval/src/promote.rs`

`Candidate` (`promote.rs:292`) is the pipeline: `draft` (`:316`) → `run_regression` (`:361`) →
`start_shadow` (`:375`) → `start_canary` (`:382`) → `observe_canary_samples` (`:402`) →
`finish_canary` (`:431`) → `approve` (`:447`) → `rollback` (`:470`), across
`PromotionStage { Draft, RegressionPassed, Shadow, Canary, ComparisonReady, Promoted,
RolledBack, Rejected }` (`promote.rs:165-183`).

**ADR-010 is structural, not documentary.** `approve()` requires `Actor::Human` and is the only
path to `Promoted` (`promote.rs:447-460`). `PromotionRecord` (`promote.rs:241-258`) has private
fields, no public constructor, and derives `Serialize` but **not** `Deserialize` — its doc
comment explains why: *"a caller cannot rehydrate a forged receipt from JSON and hand it to
`ActiveVersions::activate` … the only way to obtain a `Promoted` receipt is a real human
approval."* `PromotionError::NotPromoted` (`promote.rs:216-220`) is the activation-bypass guard.

The daemon layer adds a second lock: `ApprovePromotion` (`crates/protocol/src/command.rs:548-560`)
carries **no actor field on the wire** — *"No field on the wire lets a caller supply an actor —
that would defeat the whole point of ADR-010"* — and the daemon derives `Actor::Human` from the
connection's `Controller` role (`crates/daemon/src/server.rs:2284`,
`crates/daemon/src/promotion.rs:17-29`, `crates/cli/src/main.rs:982`).

**M9 adds evidence to this machine. It does not add a bypass.** Every new quality signal feeds
`observe_canary_samples` / `finish_canary`; none of them reaches `Promoted`.

### 2.2 Durable promotion storage — `crates/eval/src/store.rs`

`PromotionStore` is a unit struct (`store.rs:59`) with the pool passed per call:
`propose` (`:70`), `propose_idempotent` (`:91`), `mark_permission_reviewed` (`:125`),
`run_regression` (`:140`), `start_shadow` (`:155`), `start_canary` (`:169`),
`observe_canary` (`:189`), `observe_canary_samples` (`:199`), `finish_canary` (`:222`),
`approve` (`:243`), `rollback` (`:268`), `get` (`:285`), `list_by_stage` (`:305`),
`list_by_artifact` (`:322`), `active_version` (`:341`).

Every mutation is `begin` → `load_for_update` → call the real `Candidate` method → `save_candidate`
→ `commit` (see `observe_canary_samples`, `store.rs:199-215`). Migration
`migrations/0015_promotion.sql:1-12` explains the anti-backdoor design: `candidate_json` is the
**whole serialized `Candidate`, private fields and all**, so *"the only way a row ever reaches
`stage = 'promoted'` is by round-tripping through the real `Candidate::approve` method"*, and
the denormalized `stage` column is *"always derived FROM the just-mutated `Candidate`, never
written independently."*

**Do not add a control-plane promotion table that writes `stage` directly.** M9's control-plane
quality tables hold *evidence*; the promotion state machine stays where it is.

### 2.3 Routing — `crates/routing` + `crates/codypendentd/src/routing.rs`

- `RouteArm` (`arms.rs:33-47`): `StaticStrongest`, `StaticCheap`, `Router`, `RouterEscalation`,
  `LocalFirstRouter`; `RouteArm::all()` (`:49`), `RouteArm::select(...)` (`:73`, pure —
  arm→selection mapping only), `escalates()` (`:88`).
- `RouteArmResult` (`arms.rs:96-110`): `arm`, `task_success_rate`, `mean_cost_usd`,
  `mean_latency_ms`, `escalation_rate`, `tool_call_error_rate`, `unsafe_proposal_rate` — all
  `f64`, all documented as **measured** by the harness.
- `RouteEvalReport` (`arms.rs:114-125`) with `meets_release_gate()` (`:138-146`) — *router+
  escalation meets the quality threshold **and** costs less than static-strongest* — and
  `gate_summary()` (`:150`). **`meets_release_gate` returns `false` when either arm is missing
  from the report.** That is the shipped absent-≠-zero behaviour; preserve it.
- `RoutingCoordinator` (`crates/codypendentd/src/routing.rs:304`): `select` (`:367`),
  `validate_pin` (`:437`), `escalate` (`:488`), `record_decision` (`:515`),
  `record_transition` (`:535`), `escalation_candidate` (`:555`),
  `record_escalation_candidate` (`:570`).
- The measured-outcome writer: `migrations/0025_routing_outcomes.sql` and
  `crates/daemon/src/model_profiles.rs::ModelProfileStore::record_outcome`, which inserts a raw
  `model_task_outcomes` row **and** recomputes the aggregate in the same transaction. Migration
  0025's header is worth reading in full — it names the exact class of defect M9 must not
  repeat: *"the headline 'data produced, never consumed' defect."*

### 2.4 The eval corpus and gate — `crates/eval` + `evals/`

- `Trace` (`crates/eval/src/grade.rs:98-124`) is the grader's input: 19 fields, every one an
  observed outcome. `Trace::from_case(case, result, obs)` (`grade.rs:147`) is the only
  non-test constructor, called by `crates/cli/src/eval.rs::run_case_with_trace`.
- `grade(&Trace) -> TraceGrade` (`grade.rs:339`) emits `Signal`s (`grade.rs:20-37`) that are
  *"execution-grounded only — no model-vibes grading."*
- `RunObservation` (`crates/eval/src/case.rs:249`) is the honesty exemplar: it deliberately
  keeps `executed_commands` (proven) separate from `approved_commands` (possible) because
  *"a negative assertion must key on everything that might have run … a positive one must key
  on what is proven to have run"* (`case.rs:257-283`).
- `SuiteReport` (`case.rs:384-411`), `RegressionSuite` (`regression.rs:19-95`) — which *"treats
  a missing observation as a regression"*.
- The corpus: 13 files in `evals/tasks/core/`, 40 in `evals/tasks/extended/`, 2 in
  `evals/tasks/regressions/`. `evals/ci/run_gate.sh` runs the **core** suite against a
  deterministic stub model (`evals/ci/stub_model.py`) and compares to
  `evals/baselines/core.json` via `evals/ci/compare_baseline.py`.

**Read `run_gate.sh:1-8` and `evals/README.md`'s "What this gate can and cannot detect" before
extending it.** `ROADMAP.md:513-517` states the limit plainly: with a deterministic stub model,
*"a prompt or skill edit cannot move this score, so 'a skill or prompt edit that lowers the
score fails CI' is not what this gate does."* M9's continuous quality is the **live measured**
counterpart — it does not replace the stub gate and must not weaken it.

---

## 3. Data model

### 3.1 Which migration directory

Same split as M8. M9 adds **nothing** to root `migrations/` (append-only SQLite, checksum-gated
by `.github/scripts/check_migration_immutability.py`) and two files to
`crates/control-plane/migrations/` (PostgreSQL, forward-only): `0009_runner_pools.sql`
(task 9.2) and `0010_quality_observations.sql` (task 9.3). Plan program rules 18–19.

### 3.2 `0009_runner_pools.sql`

```sql
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
```

### 3.3 `0010_quality_observations.sql`

Note the nullability discipline throughout: **every measurement column is nullable and NULL
means "not measured".** There is no `DEFAULT 0` on a measurement anywhere in this file, and no
`COALESCE(x, 0)` may appear in any query over it. Plan rule 23, design §8.5.

```sql
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
```

---

## 4. Managed provisioning (tasks 9.1–9.2)

### 4.1 The provider trait is narrow on purpose

Design §4.2: *"Keep managed infrastructure providers outside the execution core behind narrow
traits."* Design §10: *"Cloud-specific infrastructure is implemented through deployment
adapters, not branches in domain logic."*

```rust
#[async_trait]
pub trait RunnerProvider: Send + Sync {
    /// Idempotent on `request.reconcile_key`: a repeat call returns the
    /// existing instance rather than provisioning a second.
    async fn provision(&self, request: ProvisionRequest) -> Result<Instance, ProviderError>;
    async fn inspect(&self, id: &InstanceId) -> Result<InstanceStatus, ProviderError>;
    /// Idempotent and safe on an already-terminated instance.
    async fn terminate(&self, id: &InstanceId) -> Result<(), ProviderError>;
}
```

Four properties the contract test must pin (task 9.1):

1. **Idempotency** — same `reconcile_key`, one instance. Backed by
   `runner_pool_instances_reconcile` (§3.2).
2. **Timeout** — a provision that does not reach ready within its deadline is terminated and
   recorded `failed`, with `provision_ms` left **NULL**. A failed provision has no latency; it
   does not have latency zero.
3. **Revocation** — an image or key revoked mid-provision aborts the provision. `image_digest`
   is a FK to `runner_images` (M8's `0006`), so a revoked image cannot be quietly kept in
   service by a stale pool row.
4. **Endpoint identity** — the provisioned endpoint must authenticate as the runner it claims
   to be before any job is placed on it. A provider handing back an endpoint is not a proof of
   identity; the M8 workload credential is.

`firecracker.rs` speaks to the Firecracker API over a Unix socket and nothing else. **The guest
runs the unchanged `crates/runner` binary and the unchanged protocol** (plan task 9.1) — this
is acceptance criterion 12 (*"Self-hosted containers and managed microVMs execute through one
runner protocol"*). `macos.rs` stays feature-gated with an explicit unsupported error until a
provider is configured; the plan defers a real macOS provider (plan §Explicitly deferred).

### 4.2 Warm-pool reset

Task 9.2: *"Reset every warm instance to a known snapshot and issue fresh runner identity/job
credentials; reject residual writable state."* Concretely:

- Reset to `snapshot_digest` before the instance enters `warm`, and record the digest on the
  instance row (§3.2) so a snapshot change is auditable against instances that predate it.
- **Fresh identity per assignment**, not per instance. A warm instance that served job A and is
  reassigned to job B must present a new workload credential; carrying A's credential into B
  breaks M8's job-scoping (design §5.2).
- `residual_state_found = TRUE` ⇒ `failed` ⇒ destroy. Never return such an instance to `warm`.
  The check is cheap and the failure mode — one tenant's artifacts visible to the next — is the
  worst one in the milestone.
- `max_lifetime_seconds` bounds a "warm" instance's total life regardless of idleness. Design
  §11.2 requires disposable job environments; an instance warm for a week is not disposable.

### 4.3 Autoscaling inputs

Scale on **eligible queued jobs for this pool's capability signature**, not on total queue
depth. A queue full of jobs no instance in this pool can serve must produce `hold`, not
`scale-up`. Record every decision's inputs in `runner_pool_scale_events` (§3.2) so a
thrashing pool is diagnosable. Two reconcilers running concurrently must converge, not
double-provision — that is what `reconcile_key` is for.

---

## 5. The quality subsystem (tasks 9.3–9.6)

### 5.1 Capture (task 9.3) — measured or absent, never zero

`quality_observations` rows are written from **real execution**, translated into
`codypendent_eval::Trace` shape. Two rules, both with shipped precedent:

**Rule 1 — capture only what policy permits.** Design §6.4 publication classes; §12.3
organization policy. Large payloads go to object storage as classified artifact refs; only
measured metadata lands in PostgreSQL (task 9.3). Search indexes and derived rows *"inherit the
strictest classification of their sources"* (design §6.4) — a `quality_observations` row
derived from a `Confidential` trace is `Confidential`.

**Rule 2 — never fabricate a measurement.** `Trace::from_case`'s doc comment
(`crates/eval/src/grade.rs:130-146`) is the standard to hold the capture path to:

> *"Every field is either read directly off real evidence, or left at its honest
> zero/false/`None` default with a comment naming exactly why there is no evidence for it yet.
> Nothing here is guessed — several `Trace` fields (`lint_passes`, `fabricated_dependency`,
> `invalid_tool_calls`) have no signal anywhere in this codebase's `RunObservation` today, so
> they stay at their default rather than being approximated from something that doesn't
> actually mean that."*

The same discipline already governs cost: `crates/codypendentd/src/executor.rs:876-878` —
*"A pin bypasses routing, so no measured price exists for it. Unmeasured price ⇒ unmeasured
cost, never a fabricated zero"* — and `crates/workflow/src/budget.rs:331-343`, where a `None`
cost is not charged. `estimate_input_tokens` (`crates/codypendentd/src/routing.rs:746-748`) is
explicitly *"a conservative floor the router can only tighten, never a fabricated precise
figure."*

In SQL terms: measurement columns are nullable, there is no `DEFAULT 0` on any of them, and
`COALESCE(metric, 0)` must not appear in any aggregate. An aggregate over a window where
30% of rows have `cost_micro_usd IS NULL` reports the mean of the 70% **and** the count of the
30%; it does not report a mean depressed by zeros.

### 5.2 Shadow (task 9.4) — isolation is the whole feature

A shadow experiment runs the candidate on the **same approved inputs** as the baseline and
throws its effects away. Required, each testable:

- **Same inputs.** Both arms see identical, already-approved input. The shadow arm never
  triggers a fresh approval.
- **Separate budget.** `candidate_budget_micro_usd` (§3.3) is independently enforced; shadow
  spend never draws down the production workflow's envelope.
- **Separate credentials.** The shadow arm gets its own short-lived credential. Reusing the
  production run's credential means a shadow failure is indistinguishable from a production
  one in the audit log.
- **No production effects.** No patch applied, no PR written, no approval requested, no inbox
  entry. The park-before-effect ordering the workflow already relies on
  (`crates/workflow/src/drive.rs:344-352`) is the model: the effect boundary is a real place in
  the code, not a flag.
- **No output influence.** The shadow result must not reach the production run's context,
  memory, or routing decision. Design §13 lists shadow under continuous quality, not under
  execution.
- **Persist before activating.** Task 9.4: *"Persist baseline/candidate `RouteArmResult`
  observations before activating shadow state."* This is the persist-then-publish ordering
  `RoutingCoordinator::record_decision` already uses
  (`crates/codypendentd/src/routing.rs:511-518`).

Assignment is a **pure deterministic function** of `(assignment_seed, assignment_key)` —
replayable, and `quality_experiment_samples_unit` (§3.3) makes double-counting a unit a
constraint violation rather than a subtle bias.

### 5.3 Canary, drift, rollback (task 9.5) — the defect closed

Build `RouteEvalReport` (`crates/routing/src/arms.rs:114`) **from `quality_experiment_samples`
the server recorded**, and drive the existing `PromotionStore` transitions with it. The wire
change:

| Today | After task 9.5 |
|---|---|
| `ObserveCanary { metrics: CanaryMetrics }` carries the numbers (`crates/protocol/src/command.rs:1177`) | `CanaryMetrics` remains on the wire for compatibility but is **refused in production**; the daemon reads the measured population from the experiment store |
| `codypendent promote --step observe-canary --sample-count …` supplies five numbers (`crates/cli/src/main.rs:1012-1035`) | the flags are deprecated with a message naming the replacement; supplying them errors rather than silently winning |
| `canary_regressed(&metrics)` (`crates/codypendentd/src/promotion.rs:366-372`) compares caller numbers | the same comparison runs over server-measured values |
| `validate_canary_metrics` (`promotion.rs:350-363`) checks shape | shape validation stays, but shape was never the problem |

The compatibility clause matters: plan task 9.5 says *"retain wire compatibility until a major
protocol release"*, so the variant stays in `PromotionAction` and stays in the golden vectors
(`crates/protocol/tests/golden_vectors.rs`). It is the **production path** that refuses it — a
new `promotion.caller-supplied-canary-evidence` rejection alongside the existing
`promotion.regression-evidence-missing` (`crates/codypendentd/src/promotion.rs:199-209`), whose
message already models the right shape by naming the command the operator should run instead.

Analysis rules:

- `MIN_CANARY_SAMPLES = 100` (`crates/eval/src/promote.rs:83`) stays the floor and is now
  counted from **recorded samples**, not from a caller's `sample_count`.
- The analysis horizon (`fixed` or `sequential`) is fixed in `analysis_plan` **before**
  activation and immutable after. A sequential test whose stopping rule is chosen after
  looking at the data is not a test.
- **Quality is non-inferiority**, not superiority — a candidate must not be *worse* by more
  than the margin.
- Cost and latency are separate limit checks, each independently `NULL` when unmeasured.
- **Missing data blocks, it does not pass.** `verdict = 'insufficient-evidence'` is a distinct
  terminal from `pass` and `fail`, and does not advance the candidate. `RouteEvalReport::
  meets_release_gate` already returns `false` when an arm is absent
  (`crates/routing/src/arms.rs:143-145`) — preserve that behaviour rather than filling the gap.
- **Safety rollback is immediate and does not wait for the horizon.**
  `Candidate::observe_canary_samples` already rolls back *"immediately regardless of the
  accumulated sample count"* (`crates/eval/src/promote.rs:401-419`), minting a `PromotionRecord`
  attributed to `"system"` with a reason. Auto-rollback needs no human — *"stopping a bad change
  needs no human, only promoting a good one does"*
  (`crates/protocol/src/command.rs:563-566`).
- **Drift is independent of any experiment.** It compares a current window against a reference
  window per `(task_class, model, dimension)`, both windows with recorded sample counts, and
  raises a `quality_drift_alerts` row. It does not silently retune anything.

### 5.4 Human promotion and the UI (task 9.6)

Nothing here creates a new promotion path. The control plane authenticates a **scoped**
Approver or Maintainer, records `quality_promotion_approvals` with an `action_digest` binding
the exact comparison, and then calls the **existing** state machine — which independently
refuses any non-`Actor::Human` approver (`crates/eval/src/promote.rs:447-460`).

Task 9.6's failing tests are the specification: a runner, a candidate, a grader, and an
unscoped organization administrator each **cannot** self-promote. Note that "organization
administrator" is not a promotion scope — design §5.3 gives org admins identity, policy,
runner, and marketplace allowlist authority; **approval is `Approver`'s, within an explicit
repository/action scope**. An org admin without that scope must be refused, and the test must
prove it.

The UI (`apps/web/src/features/quality/*`) must render baseline / shadow / canary / drift /
rollback evidence with **unknown measurements explicit**. A missing metric renders as
"not measured", never as `0`, `0%`, `—`, or an empty cell that reads as zero. That is why
`quality_comparisons.missing_measurements` is a named list (§3.3) rather than something the UI
infers from NULLs.

---

## 6. Acceptance criteria

Objectively checkable, each tied to a test name.

1. **Provider provisioning is idempotent.** Two concurrent `provision` calls with the same
   `reconcile_key` yield one instance. Test: `provision_is_idempotent_on_reconcile_key`.
2. **A provisioning timeout leaves latency absent, not zero.** `provision_ms IS NULL` and the
   instance is `failed`. Test: `failed_provision_records_no_latency`.
3. **A revoked image aborts provisioning.** Test: `revoked_image_blocks_provisioning`.
4. **`terminate` is safe twice.** Test: `terminate_is_idempotent`.
5. **The endpoint must authenticate before a job is placed.** Test:
   `unauthenticated_endpoint_receives_no_job`.
6. **Autoscaling keys on eligible depth.** A queue of jobs no pool instance can serve produces
   `hold`. Test: `autoscale_ignores_ineligible_queue_depth`.
7. **Concurrent reconcilers converge.** Test: `concurrent_reconcilers_do_not_double_provision`.
8. **`max_total` is honoured.** Test: `autoscale_stops_at_pool_ceiling`.
9. **Residual writable state destroys the instance.** Test:
   `instance_with_residual_state_is_destroyed_not_reused`.
10. **A reassigned warm instance gets fresh credentials.** The previous job's credential is
    rejected. Test: `warm_reassignment_issues_fresh_identity`.
11. **Capture obeys publication policy.** Under a metadata-only policy no transcript, patch, or
    path text is stored. Test: `capture_respects_publication_policy`.
12. **An unmeasured metric is stored NULL and aggregates as absent.** A window that is 30% NULL
    reports the mean of the measured 70% plus an explicit unmeasured count — never a
    zero-depressed mean. Tests: `unmeasured_metric_is_null_not_zero`,
    `aggregate_reports_unmeasured_count_separately`.
13. **Shadow assignment is deterministic and replayable.** Same seed and key ⇒ same arm across
    process restarts. Test: `shadow_assignment_is_deterministic`.
14. **A unit contributes at most one sample per experiment.** Test:
    `experiment_sample_unit_is_unique`.
15. **Shadow produces no production effect.** No patch, PR, approval request, or inbox entry;
    the production run's context and routing decision are byte-identical with the shadow on and
    off. Tests: `shadow_produces_no_production_effect`,
    `shadow_does_not_influence_production_output`.
16. **Shadow budget and credentials are separate.** Exhausting the shadow budget does not touch
    the production envelope; the shadow credential cannot act as the production one. Tests:
    `shadow_budget_is_independent`, `shadow_credentials_are_separate`.
17. **Baseline and candidate observations are persisted before shadow activation.** Test:
    `shadow_persists_observations_before_activation`.
18. **Caller-supplied canary metrics are refused in production.** An `ObserveCanary` carrying
    `CanaryMetrics` is rejected with a specific code; the golden vector for the variant still
    round-trips. Tests: `production_refuses_caller_supplied_canary_metrics`,
    `canary_metrics_variant_remains_wire_compatible`.
19. **`MIN_CANARY_SAMPLES` is counted from recorded samples.** 99 recorded samples cannot finish
    a canary regardless of any request field. Test:
    `finish_canary_counts_only_recorded_samples`.
20. **The analysis plan is immutable after activation.** Test:
    `analysis_plan_cannot_change_after_activation`.
21. **A missing measurement yields `insufficient-evidence`, not `pass`.** Test:
    `missing_measurement_blocks_the_verdict`.
22. **`RouteEvalReport` with a missing arm fails the gate.** The shipped behaviour
    (`crates/routing/src/arms.rs:143-145`) still holds through the new builder. Test:
    `incomplete_report_does_not_meet_release_gate`.
23. **Safety rollback fires immediately, before the horizon.** Test:
    `safety_rollback_precedes_the_analysis_horizon`.
24. **Drift is detected with both window sample counts recorded.** Test:
    `drift_alert_records_both_window_populations`.
25. **No non-human principal can promote.** Runner, candidate, grader, and unscoped org admin
    each refused. Tests: `runner_cannot_self_promote`, `candidate_cannot_self_promote`,
    `grader_cannot_promote`, `unscoped_org_admin_cannot_promote`.
26. **Approval binds to the exact evidence digest and expiry.** A re-run analysis invalidates a
    prior approval; an expired approval is refused. Tests:
    `approval_binds_to_the_exact_action_digest`, `expired_approval_is_refused`.
27. **The UI renders unknown measurements explicitly.** No metric renders as `0` when
    unmeasured. Test (vitest): `quality evidence renders unknown measurements as unknown`.
28. **The stub-model eval gate still passes unchanged.** `evals/ci/run_gate.sh` exits 0 against
    `evals/baselines/core.json` with no baseline update.
29. **Container and managed microVM agree on the same golden job.** Accepted semantic outputs
    and attestations match (plan task 9.7). Test:
    `container_and_microvm_agree_on_the_golden_job`.
30. **Control-plane migrations apply forward from every prior schema fixture.** Test:
    `control_plane_migrations_apply_from_each_fixture`.
31. **The root SQLite checksum gate still passes.** `python3
    .github/scripts/check_migration_immutability.py` exits 0 — M9 adds no SQLite migration.

---

## 7. Gotchas

1. **The canary defect is half-fixed, and the fixed half looks like the whole fix.** Migration
   `0017_promotion_evidence.sql` and `promotion_canary_evidence` make the canary path *look*
   server-consumed. Read `crates/codypendentd/src/promotion.rs:263-296` and
   `crates/cli/src/main.rs:1012-1035` before concluding otherwise. §0.

2. **`Candidate::observe_canary(regressed: bool)`** (`crates/eval/src/promote.rs:395-400`)
   still exists and takes a bare boolean, delegating to `observe_canary_samples(regressed, 1)`.
   Its own doc says *"Production callers should use `observe_canary_samples` with the measured
   population size."* Do not let a new code path reach the boolean form.

3. **`PromotionRecord` is `Serialize` but not `Deserialize`** (`crates/eval/src/promote.rs:241`).
   Any control-plane design that round-trips a promotion receipt through JSON is trying to
   forge one. Store the *approval* (§3.3) and let the daemon mint the record.

4. **`migrations/0015_promotion.sql` stores the whole `Candidate` as JSON on purpose.** Writing
   `stage` independently is the documented back door the schema exists to prevent
   (`0015_promotion.sql:1-12`). A control-plane mirror of the stage column, kept in sync by
   application code, reintroduces exactly that.

5. **`RouteArm` has never had a driver.** `crates/routing/src/arms.rs:15-25` states it plainly:
   *"No shipped command drives this yet … the exit criterion is not evaluable by any shipped
   path and the types below are exercised only by this crate's own tests."* It also names the
   prerequisite: comparing arms needs *"several benchmarked `model_profiles` — including a
   hosted one with a real price, without which static-strongest and static-cheap collapse onto
   the same model and the gate compares nothing."* M9.5 builds the first real
   `RouteEvalReport`; budget for the profile prerequisite.

6. **`RoutingCoordinator::escalate` and `record_transition` are `#[cfg_attr(not(test),
   allow(dead_code))]`** (`crates/codypendentd/src/routing.rs:487`, `:534`) — they have no
   production caller today because *"re-driving `execute_run` would emit a second terminal
   `RunCompleted`, breaking the '`RunCompleted` is terminal' contract clients stream against."*
   A canary that re-drives a run hits the same wall. `record_transition`'s doc is blunt:
   *"**Only call this for a switch that ACTUALLY HAPPENED** … recording it without re-driving
   the run writes a fabricated audit record."* Use `record_escalation_candidate`
   (`routing.rs:570`) for a merely-identified tier.

7. **The eval baseline is 13 cases at a 100% recorded pass rate, from a stub model.**
   `evals/baselines/core.json` starts at 3/13 and was updated as cases were fixed;
   `evals/ci/run_gate.sh:1-8` and `evals/README.md` document what it cannot detect. Do not
   present the stub gate as evidence of live quality, and do not update the baseline as a side
   effect of M9 — `--update-baseline` is *"a deliberate, human-reviewed action"*
   (`run_gate.sh:11-15`).

8. **The corpus is 55 files, not 55 runnable core cases.** 13 in `evals/tasks/core/`, 40 in
   `evals/tasks/extended/`, 2 in `evals/tasks/regressions/`. Only `core` is gated. Suites load
   `*.json` **non-recursively** in filename order and resolve their fixture by a name
   convention currently hardcoded to `tiny-crate` (`evals/README.md`) — a multi-fixture suite
   needs that convention extended first.

9. **`Trace` has fields with no producer.** `lint_passes`, `fabricated_dependency`, and
   `invalid_tool_calls` have no signal in `RunObservation` today
   (`crates/eval/src/grade.rs:138-146`). Capturing live traces will tempt you to approximate
   them. Don't — an unproven positive is worse than an absent one, because there is no matching
   negative signal for "did not prove clean".

10. **`grade_score` is legitimately negative.** `TraceGrade::score()`
    (`crates/eval/src/grade.rs:315`) sums signal polarities. A `CHECK (grade_score >= 0)` would
    reject real data.

11. **`RunObservation` splits `executed_commands` from `approved_commands` deliberately**
    (`crates/eval/src/case.rs:257-283`). Any quality metric derived from "commands run" must
    pick the right one: positive claims key on `executed_commands` (proven), negative claims on
    `approved_commands` (possible). Collapsing them makes one direction wrong.

12. **`hosted_allows` and the classification ceiling apply to quality capture too.** Sending a
    trace to the control plane is off-device disclosure. The composition at
    `crates/codypendentd/src/routing.rs:612-619` — *"a per-run or derived classification may
    only ever RAISE sensitivity above it, never lower it"* — governs what may be captured, not
    just what may be routed. `RoutingPolicy::hosted_allows`
    (`crates/routing/src/policy.rs:157-162`) returns `false` for `Unknown` on either side.

13. **`apps/web/` does not exist yet.** Task 9.6's UI path presumes M7 task 7.8. Confirm before
    scheduling.

14. **`sqlx` is SQLite-only in the workspace manifest** (`Cargo.toml:132`,
    `default-features = false, features = [… "sqlite" …]`). Same M7-inherited issue M8 hits;
    re-verify it was resolved deliberately rather than by widening the workspace feature set
    for every crate.

15. **`docs/MANIFEST.json` indexes every file under `docs/` recursively**
    (`.github/scripts/check_docs_manifest.py:38-43`), and `check_doc_test_counts.py` validates
    `<!-- doc-count:test … -->` markers against real test counts. Any doc M9 adds must be listed
    in the manifest; any test-count claim must carry a marker or be omitted. Both scripts are in
    the plan's common verification set.

16. **Do not weaken the `promotion.*` error codes.** Clients branch on them
    (`crates/codypendentd/src/promotion.rs:395-405`), and `store_error_to_protocol`
    (`promotion.rs:379-393`) marks only database/serde failures retryable — *"every semantic
    rejection … is not"*. A new `insufficient-evidence` verdict must be non-retryable for the
    same reason.

---

## 8. Contradictions between plan/spec and shipped code

Recorded for M9's milestone review (plan §Milestone review template, item 8).

- **Plan task 9.5** says *"Build `RouteEvalReport` from server-measured samples and drive
  existing `PromotionStore` transitions."* `PromotionStore` is a **local SQLite** store
  (`crates/eval/src/store.rs`, migration `migrations/0015_promotion.sql`) while the samples are
  **control-plane PostgreSQL** (`0010_quality_observations.sql`). The plan does not say which
  side owns the join. This guide's answer: the control plane computes `quality_comparisons` and
  the daemon consumes the comparison to drive `PromotionStore`, keeping the state machine and
  its no-self-promotion guarantee local. Confirm this reading before task 9.5.
- **Plan task 9.6** places `apps/web/src/features/quality/*` in M9 while `apps/web/` is created
  in M7 task 7.8; the working tree has only `apps/desktop/` and `extensions/vscode/`.
- **Design §13** lists *"statistically controlled shadow and canary routing"* under the
  evaluation loop, and **plan task 9.4** says to *"reuse routing `RouteArm*` and eval
  candidates."* `crates/routing/src/arms.rs:15-25` states no shipped command drives the arms
  and that the release-gate exit criterion is *"not evaluable by any shipped path"*, and
  `ROADMAP.md:519-524` names the *"live measured paths"* as the remaining Phase 7 slice. M9 is
  where that gap closes; the plan reads as though the driver already exists.
- **Design §16 criterion 15** — *"Real execution observations feed human-gated quality and
  routing experiments"* — is currently false in a specific, narrow way: the observations feeding
  the canary gate come from the request, not from execution (§0). Task 9.5 is what makes the
  criterion true, so it must be verified by a test that fails today.
- **`crates/codypendentd/src/routing.rs:480-487`** records that live escalation re-drive awaits
  a runtime mid-run model-switch hook that does not exist. A canary arm that must switch models
  mid-run hits the same missing seam; scope the experiment to whole-run arm assignment.
