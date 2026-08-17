//! The daemon's promotion-pipeline host (Phase 7 STEP 7.5).
//!
//! Like [`WorkflowConductorHost`](crate::workflows::WorkflowConductorHost),
//! this lives in the assembly binary because it bridges the daemon (which
//! declares the [`PromotionGateway`] seam) and `codypendent-eval` (which owns
//! the [`Candidate`] state machine and the durable [`PromotionStore`]). The
//! daemon crate cannot name the eval crate, so the composition happens here.
//!
//! [`PromotionStoreGateway`] fills the seam by delegating every method
//! straight to [`PromotionStore`] over the daemon's pool. The state-machine
//! rules (no self-promotion, no unobserved canary) live in `codypendent-eval`
//! and this host must not re-implement (or worse, loosen) them.
//!
//! # The gates' evidence is produced here, and only here
//!
//! What this host adds beyond delegation is the *derivation* of the two gate
//! verdicts, from evidence the daemon itself holds:
//!
//! * **Regression** reads the latest `eval_suite_reports` row bound to the
//!   candidate and derives `regressed` from the case results
//!   (`migrations/0017_promotion_evidence.sql`).
//! * **Canary** reads `execution_observations`
//!   (`migrations/0043_execution_observations.sql`) — real per-run
//!   measurements — and derives the sample count, error rates and p95
//!   latencies itself. See [`PromotionStoreGateway::observe_measured_canary`].
//!
//! The canary half used to be half-done in the dangerous direction: the
//! *verdict* was derived server-side, but the *numbers* it was derived from
//! rode in on `PromotionAction::ObserveCanary { metrics }`, and the shipped CLI
//! asked a human to type all five. `MIN_CANARY_SAMPLES = 100` was therefore
//! satisfied by typing `500`, on a path whose doc comment asserted the evidence
//! was objective. The action now carries no payload at all.

use codypendent_daemon::promotion::{
    AdvancePromotionRequest, ApprovePromotionRequest, PromotionActionFuture, PromotionGateway,
    PromotionProposeFuture, ProposePromotionRequest, RollbackPromotionRequest,
    SubmitEvalEvidenceRequest,
};
use codypendent_eval::{
    ArtifactKind, ArtifactVersion, PromotionStore, PromotionStoreError, SuiteReport,
};
use codypendent_protocol::{Actor, CanaryMetrics, CodypendentError, PromotionAction};
use sqlx::SqlitePool;

/// Drives the promotion pipeline over the daemon's pool. Cheap to clone (a
/// pool handle plus a stateless store), matching
/// [`WorkflowConductorHost`](crate::workflows::WorkflowConductorHost)'s style.
#[derive(Clone)]
pub struct PromotionStoreGateway {
    pool: SqlitePool,
    store: PromotionStore,
}

impl PromotionStoreGateway {
    /// Build a gateway over the daemon's pool. The promotion tables share the
    /// daemon's pool (the migrations are workspace-wide).
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            store: PromotionStore::new(),
        }
    }
}

/// One measured canary slice: the metrics the daemon derived, plus the size of
/// the candidate-side population they were derived from.
///
/// `sample_count` is the number of *executions that actually happened* on the
/// candidate side of the slice and reported a measured outcome — not a number
/// anybody typed. It is what accumulates toward
/// [`codypendent_eval::MIN_CANARY_SAMPLES`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct MeasuredCanarySlice {
    metrics: CanaryMetrics,
    sample_count: u64,
}

/// One row of `execution_observations` as this measurement reads it. Both
/// metric columns stay `Option`: NULL means NOT MEASURED and is dropped from
/// the population it would have contributed to, never folded in as a zero.
#[derive(Debug, sqlx::FromRow)]
struct ObservationRow {
    model_id: Option<String>,
    completion: Option<String>,
    latency_ms: Option<i64>,
    observed_at: String,
}

/// The measured outcomes that speak to whether an execution succeeded.
///
/// `'incomplete'` and a NULL `completion` mean the outcome was NOT observed, so
/// neither appears here — counting an unobserved run as a success would invent
/// evidence, and counting it as a failure would invent a regression.
/// `'cancelled'` is excluded too: an operator cancelling a run says nothing
/// about the candidate's quality, and putting it in the denominator would
/// dilute a real error rate.
fn completion_is_measured_outcome(completion: &str) -> bool {
    matches!(completion, "successful" | "failed")
}

/// The p95 by nearest rank over MEASURED latencies. `None` when nothing on this
/// side reported a latency — the caller refuses rather than substituting 0.
fn p95_of(mut latencies: Vec<u64>) -> Option<u64> {
    if latencies.is_empty() {
        return None;
    }
    latencies.sort_unstable();
    // Nearest-rank: ceil(0.95 * n), 1-based, clamped into the vector.
    let rank = (latencies.len() * 95).div_ceil(100).max(1);
    latencies.get(rank - 1).copied()
}

/// Error rate in basis points over a measured population. `population` is
/// always non-zero at the call site (the caller refuses an empty one first).
fn error_rate_bps(failures: u64, population: u64) -> u16 {
    let bps = failures.saturating_mul(10_000) / population.max(1);
    u16::try_from(bps.min(10_000)).unwrap_or(10_000)
}

impl PromotionStoreGateway {
    /// Measure the canary slice for `candidate_id` from the daemon's own
    /// recorded executions and advance the candidate on the derived verdict.
    ///
    /// # What "measured" means here, exactly
    ///
    /// The population is `execution_observations`
    /// (`migrations/0043_execution_observations.sql`) — one row per real run
    /// the daemon terminated, written by `crates/daemon/src/ledger.rs` with a
    /// measured `completion`, a measured `latency_ms`, and the `model_id` the
    /// router recorded for that run. Nothing in it comes from a promotion
    /// client.
    ///
    /// The slice is `(candidate.updated_at, now]`. `updated_at` is bumped by
    /// every promotion-store write, so on the first observation it is exactly
    /// when `StartCanary` was recorded and on each later one it is the previous
    /// observation — consecutive slices never overlap, so no execution is
    /// counted toward the sample population twice.
    ///
    /// The two sides are concurrent, not sequential: within the same slice, the
    /// candidate side is the executions that ran on the candidate model and the
    /// baseline side is the executions that ran on any other model. A
    /// before/after comparison would attribute an unrelated shift in traffic to
    /// the candidate; this does not.
    ///
    /// # What it deliberately cannot do
    ///
    /// Attribution is by `model_id`, which is the only column the shipped
    /// daemon writes that identifies what an execution ran on. So only a
    /// `model-profile` candidate is measurable; a skill, prompt, router,
    /// workflow or retrieval-weights candidate has nothing in the recorded
    /// executions tying a run to it, and is refused rather than measured
    /// against ambient traffic. It also cannot distinguish two versions of the
    /// same model profile beyond their separation in time — the slice bound is
    /// the whole of that separation.
    ///
    /// Promotion is a Controller-role operation and `promotion_candidates` has
    /// no owner column, so this reads across every `owner_uid`. That is stated
    /// rather than silently done: unlike the analytics reads, which lead every
    /// index with `owner_uid`, there is no principal here to scope to.
    async fn observe_measured_canary(&self, candidate_id: &str) -> Result<(), CodypendentError> {
        let row: Option<(String, String, String, String)> = sqlx::query_as(
            "SELECT artifact_kind, artifact_name, stage, updated_at \
             FROM promotion_candidates WHERE id = ?",
        )
        .bind(candidate_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| CodypendentError::new("promotion.store-error", error.to_string(), true))?;
        let Some((kind, name, stage, updated_at)) = row else {
            return Err(CodypendentError::new(
                "promotion.unknown-candidate",
                format!("no such promotion candidate: {candidate_id}"),
                false,
            ));
        };
        // Checked before measuring so the operator gets the real reason rather
        // than "no measured executions" for a candidate that never started a
        // canary. The state machine enforces it a second time below.
        if stage != "canary" {
            return Err(CodypendentError::new(
                "promotion.illegal-transition",
                format!("cannot observe-canary a candidate in stage {stage}"),
                false,
            ));
        }
        if kind != ArtifactKind::ModelProfile.as_str() {
            return Err(CodypendentError::new(
                "promotion.canary-unattributable-artifact",
                format!(
                    "no shipped path attributes a recorded execution to a `{kind}` candidate: \
                     `execution_observations` identifies what a run executed on only by \
                     `model_id`, so canary evidence for `{kind}/{name}` cannot be measured and \
                     this promotion cannot advance. Measuring it against untargeted traffic \
                     would report a canary that never ran."
                ),
                false,
            ));
        }

        let window_start = chrono::DateTime::parse_from_rfc3339(&updated_at)
            .map(|ts| ts.with_timezone(&chrono::Utc))
            .map_err(|error| {
                CodypendentError::new(
                    "promotion.corrupt",
                    format!("candidate {candidate_id} has an unreadable updated_at: {error}"),
                    false,
                )
            })?;
        let window_end = chrono::Utc::now();

        let rows: Vec<ObservationRow> = sqlx::query_as(
            "SELECT model_id, completion, latency_ms, observed_at \
             FROM execution_observations \
             WHERE observed_at > ? AND model_id IS NOT NULL AND completion IS NOT NULL",
        )
        .bind(window_start.to_rfc3339())
        .fetch_all(&self.pool)
        .await
        .map_err(|error| CodypendentError::new("promotion.store-error", error.to_string(), true))?;

        let slice = measure_canary_slice(&rows, &name, window_start, window_end)?;

        // Persist the evidence BEFORE advancing, mirroring how the regression
        // leg writes `promotion_regression_evidence` before calling
        // `run_regression`: whatever moved the candidate is on file, with the
        // numbers it was moved on.
        let regressed = canary_regressed(&slice.metrics);
        sqlx::query(
            "INSERT INTO promotion_canary_evidence \
             (id, candidate_id, metrics_json, sample_count, regressed, observed_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(candidate_id)
        .bind(serde_json::to_string(&slice.metrics).map_err(|error| {
            CodypendentError::new("promotion.store-error", error.to_string(), true)
        })?)
        .bind(i64::try_from(slice.sample_count).unwrap_or(i64::MAX))
        .bind(i64::from(regressed))
        .bind(window_end.to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(|error| CodypendentError::new("promotion.store-error", error.to_string(), true))?;

        self.store
            .observe_canary_samples(&self.pool, candidate_id, regressed, slice.sample_count)
            .await
            .map(|_outcome| ())
            .map_err(store_error_to_protocol)
    }
}

/// Derive one slice's metrics from measured observation rows.
///
/// Split out from the query so the derivation is testable on rows without a
/// database, and so every refusal is visible in one place. Each refusal is a
/// REFUSAL, never a default: there is no branch here that substitutes a zero
/// for a population or a latency that was not measured.
fn measure_canary_slice(
    rows: &[ObservationRow],
    candidate_model: &str,
    window_start: chrono::DateTime<chrono::Utc>,
    window_end: chrono::DateTime<chrono::Utc>,
) -> Result<MeasuredCanarySlice, CodypendentError> {
    let mut candidate_population = 0_u64;
    let mut candidate_failures = 0_u64;
    let mut candidate_latencies = Vec::new();
    let mut baseline_population = 0_u64;
    let mut baseline_failures = 0_u64;
    let mut baseline_latencies = Vec::new();

    for row in rows {
        // A row whose timestamp cannot be read is not evidence of anything; it
        // is dropped rather than assumed to be inside the slice.
        let Ok(observed_at) = chrono::DateTime::parse_from_rfc3339(&row.observed_at) else {
            continue;
        };
        let observed_at = observed_at.with_timezone(&chrono::Utc);
        if observed_at <= window_start || observed_at > window_end {
            continue;
        }
        let (Some(model_id), Some(completion)) =
            (row.model_id.as_deref(), row.completion.as_deref())
        else {
            continue;
        };
        if !completion_is_measured_outcome(completion) {
            continue;
        }
        // A negative latency is not a measurement this can use; drop it from
        // the latency population without dropping the run from the outcome
        // population, because the outcome WAS measured.
        let latency = row.latency_ms.and_then(|ms| u64::try_from(ms).ok());
        if model_id == candidate_model {
            candidate_population += 1;
            if completion == "failed" {
                candidate_failures += 1;
            }
            if let Some(ms) = latency {
                candidate_latencies.push(ms);
            }
        } else {
            baseline_population += 1;
            if completion == "failed" {
                baseline_failures += 1;
            }
            if let Some(ms) = latency {
                baseline_latencies.push(ms);
            }
        }
    }

    if candidate_population == 0 {
        return Err(CodypendentError::new(
            "promotion.canary-evidence-missing",
            format!(
                "no execution on `{candidate_model}` reported a measured outcome since the last \
                 promotion write, so there is nothing to observe. Route real traffic to the \
                 candidate model and observe again."
            ),
            false,
        ));
    }
    if baseline_population == 0 {
        return Err(CodypendentError::new(
            "promotion.canary-baseline-missing",
            "no execution on any other model reported a measured outcome in the same window, so \
             the candidate has nothing to be compared against. A canary without a concurrent \
             baseline is not evidence that the candidate did not regress."
                .to_string(),
            false,
        ));
    }
    let (Some(p95_latency_ms), Some(baseline_p95_latency_ms)) =
        (p95_of(candidate_latencies), p95_of(baseline_latencies))
    else {
        return Err(CodypendentError::new(
            "promotion.canary-latency-unmeasured",
            "no latency was measured on one side of the canary window; an unmeasured latency is \
             not a latency of zero, so the comparison is refused rather than computed against a \
             fabricated baseline"
                .to_string(),
            false,
        ));
    };

    let metrics = CanaryMetrics {
        sample_count: candidate_population,
        error_rate_bps: error_rate_bps(candidate_failures, candidate_population),
        baseline_error_rate_bps: error_rate_bps(baseline_failures, baseline_population),
        p95_latency_ms,
        baseline_p95_latency_ms,
    };
    validate_canary_metrics(&metrics)?;
    Ok(MeasuredCanarySlice {
        metrics,
        sample_count: candidate_population,
    })
}

impl PromotionGateway for PromotionStoreGateway {
    fn propose(&self, request: ProposePromotionRequest) -> PromotionProposeFuture<'_> {
        let host = self.clone();
        Box::pin(async move {
            let kind = ArtifactKind::parse(&request.kind).ok_or_else(|| {
                CodypendentError::new(
                    "promotion.invalid-kind",
                    format!("unrecognized artifact kind {:?}", request.kind),
                    false,
                )
            })?;
            let artifact = ArtifactVersion::new(kind, request.name, request.version);
            // A CLI/socket-submitted proposal is attributed to the submitting
            // CLIENT, not claimed as human or agent — an agent-synthesized
            // proposal (from a future grader/clustering pipeline, not wired by
            // this task) would attribute `Actor::Agent` instead; either way,
            // authorship never implies approval (only `ApprovePromotion`'s
            // `Actor::Human` mapping does that).
            let author = Actor::Client {
                client_id: request.client_id,
            };
            host.store
                .propose_idempotent(
                    &host.pool,
                    &request.idempotency_key,
                    artifact,
                    &author,
                    request.requires_permission_review || kind == ArtifactKind::Skill,
                )
                .await
                .map_err(store_error_to_protocol)
        })
    }

    fn submit_eval_evidence(
        &self,
        request: SubmitEvalEvidenceRequest,
    ) -> PromotionActionFuture<'_> {
        let host = self.clone();
        Box::pin(async move {
            // Re-derive the artifact this evidence is about from the daemon's
            // own candidate row. The caller names only a candidate id; every
            // other column on the evidence row comes from here, so evidence
            // gathered against one artifact can never be filed against another.
            let artifact: Option<(String, String, i64)> = sqlx::query_as(
                "SELECT artifact_kind, artifact_name, artifact_version \
                 FROM promotion_candidates WHERE id = ?",
            )
            .bind(&request.candidate_id)
            .fetch_optional(&host.pool)
            .await
            .map_err(|error| {
                CodypendentError::new("promotion.store-error", error.to_string(), true)
            })?;
            let Some((kind, name, version)) = artifact else {
                return Err(CodypendentError::new(
                    "promotion.unknown-candidate",
                    format!("no such promotion candidate: {}", request.candidate_id),
                    false,
                ));
            };
            // A router candidate is only exercised by a suite that RAN under the
            // candidate policy. This check used to live in the CLI, where it was
            // advisory — a caller that skipped `--policy` simply skipped the
            // check. Here it is a condition of the row existing at all.
            if kind == "router" && request.routing_policy != name {
                return Err(CodypendentError::new(
                    "promotion.evidence-wrong-policy",
                    format!(
                        "router candidate `{name}` needs evidence produced under policy \
                         `{name}`, got `{}`",
                        request.routing_policy
                    ),
                    false,
                ));
            }
            // Parse rather than store-and-hope: a report the gate could not read
            // later would surface as `promotion.corrupt` at advancement time,
            // long after the caller could do anything about it. An empty suite
            // is refused here for the same reason `RunRegression` refuses it —
            // zero cases is not evidence of anything.
            let report: SuiteReport =
                serde_json::from_str(&request.report_json).map_err(|error| {
                    CodypendentError::new(
                        "promotion.invalid-evidence",
                        format!("submitted evidence is not a suite report: {error}"),
                        false,
                    )
                })?;
            if report.results.is_empty() {
                return Err(CodypendentError::new(
                    "promotion.regression-evidence-empty",
                    "an empty eval suite is not regression evidence".to_string(),
                    false,
                ));
            }
            // Re-serialize the PARSED report, not the caller's bytes: whatever
            // the gate reads back is exactly what this validation covered, with
            // no room for unread fields to ride along.
            let report_json = serde_json::to_string(&report).map_err(|error| {
                CodypendentError::new("promotion.store-error", error.to_string(), true)
            })?;
            sqlx::query(
                "INSERT INTO eval_suite_reports \
                 (id, candidate_id, artifact_kind, artifact_name, artifact_version, suite, \
                  routing_policy, report_json, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            )
            .bind(uuid::Uuid::now_v7().to_string())
            .bind(&request.candidate_id)
            .bind(kind)
            .bind(name)
            .bind(version)
            .bind(&request.suite)
            .bind(&request.routing_policy)
            .bind(report_json)
            .execute(&host.pool)
            .await
            .map_err(|error| {
                CodypendentError::new("promotion.store-error", error.to_string(), true)
            })?;
            Ok(())
        })
    }

    fn advance(&self, request: AdvancePromotionRequest) -> PromotionActionFuture<'_> {
        let host = self.clone();
        Box::pin(async move {
            match request.action {
                PromotionAction::RunRegression => {
                    let row: Option<(String, String)> = sqlx::query_as(
                        "SELECT report.id, report.report_json \
                         FROM eval_suite_reports AS report \
                         JOIN promotion_candidates AS candidate \
                           ON candidate.id = report.candidate_id \
                         WHERE report.candidate_id = ? \
                           AND report.suite = 'core' \
                           AND report.artifact_kind = candidate.artifact_kind \
                           AND report.artifact_name = candidate.artifact_name \
                           AND report.artifact_version = candidate.artifact_version \
                           AND (candidate.artifact_kind != 'router' \
                                OR report.routing_policy = candidate.artifact_name) \
                         ORDER BY report.created_at DESC, report.id DESC LIMIT 1",
                    )
                    .bind(&request.candidate_id)
                    .fetch_optional(&host.pool)
                    .await
                    .map_err(|error| {
                        CodypendentError::new("promotion.store-error", error.to_string(), true)
                    })?;
                    let Some((report_id, report_json)) = row else {
                        return Err(CodypendentError::new(
                            "promotion.regression-evidence-missing",
                            format!(
                                "run `codypendent eval run --suite core --candidate-id {}` \
                                 against this candidate before advancing regression",
                                request.candidate_id
                            ),
                            false,
                        ));
                    };
                    let report: SuiteReport =
                        serde_json::from_str(&report_json).map_err(|error| {
                            CodypendentError::new("promotion.corrupt", error.to_string(), false)
                        })?;
                    if report.results.is_empty() {
                        return Err(CodypendentError::new(
                            "promotion.regression-evidence-empty",
                            "an empty eval suite is not regression evidence".to_string(),
                            false,
                        ));
                    }
                    let failures = report
                        .results
                        .iter()
                        .filter(|result| !result.passed())
                        .map(|result| result.case_id.clone())
                        .collect::<Vec<_>>();
                    let regressed = !failures.is_empty();
                    sqlx::query(
                        "INSERT OR REPLACE INTO promotion_regression_evidence \
                         (candidate_id, report_id, regressed, failures_json, evaluated_at) \
                         VALUES (?, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
                    )
                    .bind(&request.candidate_id)
                    .bind(report_id)
                    .bind(if regressed { 1_i64 } else { 0_i64 })
                    .bind(serde_json::to_string(&failures).map_err(|error| {
                        CodypendentError::new("promotion.store-error", error.to_string(), true)
                    })?)
                    .execute(&host.pool)
                    .await
                    .map_err(|error| {
                        CodypendentError::new("promotion.store-error", error.to_string(), true)
                    })?;
                    host.store
                        .run_regression(&host.pool, &request.candidate_id, regressed)
                        .await
                        .map_err(store_error_to_protocol)
                }
                PromotionAction::ReviewPermissions => host
                    .store
                    .mark_permission_reviewed(&host.pool, &request.candidate_id)
                    .await
                    .map_err(store_error_to_protocol),
                PromotionAction::StartShadow => host
                    .store
                    .start_shadow(&host.pool, &request.candidate_id)
                    .await
                    .map_err(store_error_to_protocol),
                PromotionAction::StartCanary => host
                    .store
                    .start_canary(&host.pool, &request.candidate_id)
                    .await
                    .map_err(store_error_to_protocol),
                PromotionAction::ObserveCanary => {
                    host.observe_measured_canary(&request.candidate_id).await
                }
                PromotionAction::FinishCanary => host
                    .store
                    .finish_canary(&host.pool, &request.candidate_id)
                    .await
                    .map_err(store_error_to_protocol),
                // `PromotionAction::Unknown` and any future, `#[non_exhaustive]`
                // variant this build does not know (RULE 1) — reject rather
                // than guess at a transition.
                _ => Err(CodypendentError::new(
                    "promotion.unknown-action",
                    "unrecognized promotion action".to_string(),
                    false,
                )),
            }
        })
    }

    fn approve(&self, request: ApprovePromotionRequest) -> PromotionActionFuture<'_> {
        let host = self.clone();
        Box::pin(async move {
            host.store
                .approve(&host.pool, &request.candidate_id, &request.approver)
                .await
                .map(|_record| ())
                .map_err(store_error_to_protocol)
        })
    }

    fn rollback(&self, request: RollbackPromotionRequest) -> PromotionActionFuture<'_> {
        let host = self.clone();
        Box::pin(async move {
            host.store
                .rollback(&host.pool, &request.candidate_id, &request.actor)
                .await
                .map(|_record| ())
                .map_err(store_error_to_protocol)
        })
    }
}

/// A last structural check on the daemon's OWN measurement before it is
/// persisted as evidence. These conditions should be unreachable (the caller
/// refuses an empty population and an unmeasured latency first); this catches a
/// derivation bug rather than a hostile caller, since there is no longer a
/// caller who supplies these numbers.
fn validate_canary_metrics(metrics: &CanaryMetrics) -> Result<(), CodypendentError> {
    if metrics.sample_count == 0
        || metrics.error_rate_bps > 10_000
        || metrics.baseline_error_rate_bps > 10_000
        || metrics.baseline_p95_latency_ms == 0
    {
        return Err(CodypendentError::new(
            "promotion.invalid-canary-evidence",
            "canary evidence requires samples, rates in 0..=10000, and a nonzero baseline latency"
                .to_string(),
            false,
        ));
    }
    Ok(())
}

/// The canary verdict, over metrics the daemon measured: a >1pp absolute error
/// rate rise, or a >20% p95 latency rise, against the concurrent baseline.
fn canary_regressed(metrics: &CanaryMetrics) -> bool {
    let error_regressed =
        metrics.error_rate_bps > metrics.baseline_error_rate_bps.saturating_add(100);
    let latency_regressed = u128::from(metrics.p95_latency_ms) * 100
        > u128::from(metrics.baseline_p95_latency_ms) * 120;
    error_regressed || latency_regressed
}

/// Map a [`PromotionStoreError`] to the wire [`CodypendentError`] a client
/// branches on by code. A store/database hiccup is retryable; every semantic
/// rejection (unknown candidate, illegal transition, non-human approver,
/// unobserved canary, permission review still pending) is not — retrying an
/// unchanged request would fail identically.
fn store_error_to_protocol(error: PromotionStoreError) -> CodypendentError {
    let message = error.to_string();
    let code = match &error {
        PromotionStoreError::NotFound(_) => "promotion.not-found",
        PromotionStoreError::Corrupt(_) => "promotion.corrupt",
        PromotionStoreError::Promotion(inner) => promotion_error_code(inner),
        PromotionStoreError::Database(_) | PromotionStoreError::Serde(_) => "promotion.store-error",
    };
    let retryable = matches!(
        error,
        PromotionStoreError::Database(_) | PromotionStoreError::Serde(_)
    );
    CodypendentError::new(code, message, retryable)
}

fn promotion_error_code(error: &codypendent_eval::PromotionError) -> &'static str {
    use codypendent_eval::PromotionError;
    match error {
        PromotionError::RequiresHumanApproval { .. } => "promotion.requires-human-approval",
        PromotionError::RegressedOffline => "promotion.regressed-offline",
        PromotionError::IllegalTransition { .. } => "promotion.illegal-transition",
        PromotionError::PermissionReviewRequired => "promotion.permission-review-required",
        PromotionError::NotPromoted { .. } => "promotion.not-promoted",
        PromotionError::CanaryInsufficientEvidence { .. } => {
            "promotion.canary-insufficient-evidence"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_daemon::promotion::{
        AdvancePromotionRequest, ApprovePromotionRequest, ProposePromotionRequest,
        RollbackPromotionRequest,
    };
    use codypendent_eval::{AssertionResult, CaseResult, PromotionStage};
    use codypendent_protocol::ids::{AgentId, ModelId, RunId, UserId};
    use codypendent_protocol::ClientId;

    async fn temp_pool() -> (tempfile::TempDir, SqlitePool) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = codypendent_eval::db::open(&tmp.path().join("codypendent.db"))
            .await
            .unwrap();
        (tmp, pool)
    }

    fn passing_report() -> SuiteReport {
        SuiteReport::new(vec![CaseResult {
            case_id: "stored-regression-case".to_string(),
            assertion_results: Vec::new(),
            within_cost: true,
            within_duration: true,
            run_completed: true,
        }])
    }

    async fn bind_report(
        pool: &SqlitePool,
        candidate_id: &str,
        report_id: &str,
        report: &SuiteReport,
    ) {
        let (kind, name, version): (String, String, i64) = sqlx::query_as(
            "SELECT artifact_kind, artifact_name, artifact_version \
             FROM promotion_candidates WHERE id = ?",
        )
        .bind(candidate_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let routing_policy = if kind == "router" {
            name.clone()
        } else {
            "daemon-default".to_string()
        };
        sqlx::query(
            "INSERT INTO eval_suite_reports \
             (id, candidate_id, artifact_kind, artifact_name, artifact_version, suite, \
              routing_policy, report_json, created_at) \
             VALUES (?, ?, ?, ?, ?, 'core', ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        )
        .bind(report_id)
        .bind(candidate_id)
        .bind(kind)
        .bind(name)
        .bind(version)
        .bind(routing_policy)
        .bind(serde_json::to_string(report).unwrap())
        .execute(pool)
        .await
        .unwrap();
    }

    fn passing_canary_metrics() -> CanaryMetrics {
        CanaryMetrics {
            sample_count: codypendent_eval::MIN_CANARY_SAMPLES,
            error_rate_bps: 100,
            baseline_error_rate_bps: 100,
            p95_latency_ms: 100,
            baseline_p95_latency_ms: 100,
        }
    }

    /// The verdict function itself, now that it is live production code rather
    /// than a dead helper: matching the baseline is not a regression, and each
    /// tolerance is exceeded independently.
    #[test]
    fn the_canary_verdict_tolerates_parity_and_catches_each_dimension() {
        assert!(!canary_regressed(&passing_canary_metrics()));
        assert!(
            !canary_regressed(&CanaryMetrics {
                error_rate_bps: 200,
                p95_latency_ms: 120,
                ..passing_canary_metrics()
            }),
            "+1pp error and +20% latency sit exactly on the tolerances"
        );
        assert!(canary_regressed(&CanaryMetrics {
            error_rate_bps: 201,
            ..passing_canary_metrics()
        }));
        assert!(canary_regressed(&CanaryMetrics {
            p95_latency_ms: 121,
            ..passing_canary_metrics()
        }));
        // The measurement path never produces these, but a derivation bug that
        // did must not be persisted as evidence.
        assert!(validate_canary_metrics(&CanaryMetrics {
            sample_count: 0,
            ..passing_canary_metrics()
        })
        .is_err());
        assert!(validate_canary_metrics(&CanaryMetrics {
            baseline_p95_latency_ms: 0,
            ..passing_canary_metrics()
        })
        .is_err());
    }

    fn human_client_id() -> ClientId {
        ClientId::new()
    }

    /// Mirrors exactly how `crates/daemon/src/server.rs` maps a `Controller`
    /// connection to `Actor::Human` for `ApprovePromotion`/`RollbackPromotion`
    /// — the daemon's own construction is exercised by the daemon-crate's
    /// `server_it.rs` role-gating tests; this test exercises what the gateway
    /// does once handed that actor.
    fn human_actor(client_id: ClientId) -> Actor {
        Actor::Human {
            user_id: UserId(client_id.to_string()),
        }
    }

    fn agent_actor() -> Actor {
        Actor::Agent {
            agent_id: AgentId::new(),
            run_id: RunId::new(),
            model: ModelId("claude-sonnet-5".into()),
        }
    }

    /// Evidence submitted over the socket must land as a row the regression gate
    /// can consume, with the artifact identity taken from the CANDIDATE rather
    /// than from anything the caller said.
    #[tokio::test]
    async fn submitted_evidence_becomes_the_row_the_regression_gate_reads() {
        let (_tmp, pool) = temp_pool().await;
        let gateway = PromotionStoreGateway::new(pool.clone());
        let client_id = human_client_id();
        let candidate_id = gateway
            .propose(ProposePromotionRequest {
                kind: "skill".to_string(),
                name: "rust.fix-ci".to_string(),
                version: 3,
                requires_permission_review: false,
                idempotency_key: "propose-evidence".to_string(),
                client_id,
            })
            .await
            .expect("propose accepted");

        gateway
            .submit_eval_evidence(SubmitEvalEvidenceRequest {
                candidate_id: candidate_id.clone(),
                suite: "core".to_string(),
                routing_policy: "daemon-default".to_string(),
                report_json: serde_json::to_string(&passing_report()).unwrap(),
                client_id,
            })
            .await
            .expect("evidence accepted");

        // The row carries the candidate's OWN artifact identity — the submitter
        // never supplied kind/name/version, so no report can be filed against an
        // artifact it did not exercise.
        let (kind, name, version): (String, String, i64) = sqlx::query_as(
            "SELECT artifact_kind, artifact_name, artifact_version FROM eval_suite_reports \
             WHERE candidate_id = ?",
        )
        .bind(&candidate_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            (kind.as_str(), name.as_str(), version),
            ("skill", "rust.fix-ci", 3)
        );

        // …and the gate reads it: permission review first (a skill candidate
        // requires it), then the regression leg consumes the submitted row.
        for action in [
            PromotionAction::ReviewPermissions,
            PromotionAction::RunRegression,
        ] {
            gateway
                .advance(AdvancePromotionRequest {
                    candidate_id: candidate_id.clone(),
                    action,
                    client_id,
                })
                .await
                .expect("advance accepted on daemon-written evidence");
        }
    }

    /// Three ways a submission is refused, all of them before a row exists: an
    /// unknown candidate, evidence that is not a suite report, and an empty
    /// suite. A stored-then-validated design would leave junk rows behind.
    #[tokio::test]
    async fn unusable_evidence_is_refused_rather_than_stored() {
        let (_tmp, pool) = temp_pool().await;
        let gateway = PromotionStoreGateway::new(pool.clone());
        let client_id = human_client_id();
        let candidate_id = gateway
            .propose(ProposePromotionRequest {
                kind: "router".to_string(),
                name: "tool-selection".to_string(),
                version: 9,
                requires_permission_review: false,
                idempotency_key: "propose-refusals".to_string(),
                client_id,
            })
            .await
            .expect("propose accepted");

        let submit = |candidate: String, policy: String, json: String| {
            let gateway = gateway.clone();
            async move {
                gateway
                    .submit_eval_evidence(SubmitEvalEvidenceRequest {
                        candidate_id: candidate,
                        suite: "core".to_string(),
                        routing_policy: policy,
                        report_json: json,
                        client_id,
                    })
                    .await
            }
        };

        let report = serde_json::to_string(&passing_report()).unwrap();
        assert_eq!(
            submit(
                "cand-nonexistent".to_string(),
                "tool-selection".to_string(),
                report.clone()
            )
            .await
            .unwrap_err()
            .code,
            "promotion.unknown-candidate"
        );
        // A router candidate needs evidence produced under the candidate policy;
        // the CLI used to be the only place this was checked.
        assert_eq!(
            submit(
                candidate_id.clone(),
                "daemon-default".to_string(),
                report.clone()
            )
            .await
            .unwrap_err()
            .code,
            "promotion.evidence-wrong-policy"
        );
        assert_eq!(
            submit(
                candidate_id.clone(),
                "tool-selection".to_string(),
                "not json".to_string()
            )
            .await
            .unwrap_err()
            .code,
            "promotion.invalid-evidence"
        );
        assert_eq!(
            submit(
                candidate_id.clone(),
                "tool-selection".to_string(),
                serde_json::to_string(&SuiteReport::new(Vec::new())).unwrap()
            )
            .await
            .unwrap_err()
            .code,
            "promotion.regression-evidence-empty"
        );

        let rows: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM eval_suite_reports")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows.0, 0, "a refused submission must leave no row behind");
    }

    #[tokio::test]
    async fn a_controller_mapped_human_drives_a_candidate_to_promoted_and_active() {
        let (_tmp, pool) = temp_pool().await;
        let gateway = PromotionStoreGateway::new(pool.clone());
        let client_id = human_client_id();

        let candidate_id = gateway
            .propose(ProposePromotionRequest {
                kind: "router".to_string(),
                name: "tool-selection".to_string(),
                version: 4,
                requires_permission_review: false,
                idempotency_key: "propose-1".to_string(),
                client_id,
            })
            .await
            .expect("propose accepted");
        bind_report(&pool, &candidate_id, "report-promote", &passing_report()).await;

        for action in [
            PromotionAction::RunRegression,
            PromotionAction::StartShadow,
            PromotionAction::StartCanary,
        ] {
            gateway
                .advance(AdvancePromotionRequest {
                    candidate_id: candidate_id.clone(),
                    action,
                    client_id,
                })
                .await
                .expect("advance accepted");
        }

        // A router candidate has nothing in `execution_observations` tying a run
        // to it, so observing is refused rather than measured against ambient
        // traffic.
        let unattributable = gateway
            .advance(AdvancePromotionRequest {
                candidate_id: candidate_id.clone(),
                action: PromotionAction::ObserveCanary,
                client_id,
            })
            .await
            .expect_err("a router candidate has no attributable executions");
        assert_eq!(
            unattributable.code,
            "promotion.canary-unattributable-artifact"
        );

        // Record server-measured samples
        PromotionStore::new()
            .observe_canary_samples(&pool, &candidate_id, false, 100)
            .await
            .unwrap();

        gateway
            .advance(AdvancePromotionRequest {
                candidate_id: candidate_id.clone(),
                action: PromotionAction::FinishCanary,
                client_id,
            })
            .await
            .expect("advance accepted");

        gateway
            .approve(ApprovePromotionRequest {
                candidate_id: candidate_id.clone(),
                approver: human_actor(client_id),
                client_id,
            })
            .await
            .expect("a Controller-mapped human approval succeeds");

        let snapshot = PromotionStore::new()
            .get(&pool, &candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.candidate.stage(), PromotionStage::Promoted);
        assert_eq!(
            PromotionStore::new()
                .active_version(&pool, "router/tool-selection")
                .await
                .unwrap(),
            Some(4),
            "approval activates the version"
        );
    }

    #[tokio::test]
    async fn regression_requires_durable_evidence_and_persists_a_rejection() {
        let (_tmp, pool) = temp_pool().await;
        let gateway = PromotionStoreGateway::new(pool.clone());
        let client_id = human_client_id();
        let candidate_id = gateway
            .propose(ProposePromotionRequest {
                kind: "router".to_string(),
                name: "evidence-gate".to_string(),
                version: 1,
                requires_permission_review: false,
                idempotency_key: "evidence-gate".to_string(),
                client_id,
            })
            .await
            .unwrap();
        let unrelated_id = gateway
            .propose(ProposePromotionRequest {
                kind: "prompt".to_string(),
                name: "unrelated-passing-eval".to_string(),
                version: 1,
                requires_permission_review: false,
                idempotency_key: "unrelated-passing-eval".to_string(),
                client_id,
            })
            .await
            .unwrap();
        bind_report(
            &pool,
            &unrelated_id,
            "unrelated-passing-report",
            &passing_report(),
        )
        .await;
        let missing = gateway
            .advance(AdvancePromotionRequest {
                candidate_id: candidate_id.clone(),
                action: PromotionAction::RunRegression,
                client_id,
            })
            .await
            .expect_err("another candidate's passing report is not evidence for this candidate");
        assert_eq!(missing.code, "promotion.regression-evidence-missing");

        let report = SuiteReport::new(vec![CaseResult {
            case_id: "regressed-case".to_string(),
            assertion_results: vec![AssertionResult {
                label: "tests-pass".to_string(),
                passed: false,
            }],
            within_cost: true,
            within_duration: true,
            run_completed: true,
        }]);
        bind_report(&pool, &candidate_id, "failed-report", &report).await;
        let rejected = gateway
            .advance(AdvancePromotionRequest {
                candidate_id: candidate_id.clone(),
                action: PromotionAction::RunRegression,
                client_id,
            })
            .await
            .expect_err("failing stored evidence rejects the candidate");
        assert_eq!(rejected.code, "promotion.regressed-offline");
        let snapshot = PromotionStore::new()
            .get(&pool, &candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.candidate.stage(), PromotionStage::Rejected);
    }

    /// The gateway performs NO actor gating itself (see the module doc — it
    /// must not re-implement or loosen the rule); this proves the guard it
    /// relies on (`Candidate::approve`) still holds when reached through the
    /// full assembly, not just the bare eval-crate type. The daemon's OWN role
    /// gate (server.rs) is what actually prevents an `Actor::Agent` from ever
    /// being constructed for this command in production; see
    /// `crates/daemon/tests/server_it.rs`.
    #[tokio::test]
    async fn an_agent_actor_handed_to_approve_is_refused_even_by_the_real_gateway() {
        let (_tmp, pool) = temp_pool().await;
        let gateway = PromotionStoreGateway::new(pool.clone());
        let client_id = human_client_id();

        let candidate_id = gateway
            .propose(ProposePromotionRequest {
                kind: "skill".to_string(),
                name: "rust-ci".to_string(),
                version: 1,
                requires_permission_review: false,
                idempotency_key: "propose-2".to_string(),
                client_id,
            })
            .await
            .unwrap();
        bind_report(
            &pool,
            &candidate_id,
            "report-agent-approval",
            &passing_report(),
        )
        .await;
        // A skill candidate needs its permission review before evaluation,
        // whatever the proposer's flag said.
        let permission_error = gateway
            .advance(AdvancePromotionRequest {
                candidate_id: candidate_id.clone(),
                action: PromotionAction::RunRegression,
                client_id,
            })
            .await
            .expect_err("skills require review even when the proposer omitted the flag");
        assert_eq!(
            permission_error.code,
            "promotion.permission-review-required"
        );

        for action in [
            PromotionAction::ReviewPermissions,
            PromotionAction::RunRegression,
            PromotionAction::StartShadow,
            PromotionAction::StartCanary,
        ] {
            gateway
                .advance(AdvancePromotionRequest {
                    candidate_id: candidate_id.clone(),
                    action,
                    client_id,
                })
                .await
                .unwrap();
        }

        PromotionStore::new()
            .observe_canary_samples(&pool, &candidate_id, false, 100)
            .await
            .unwrap();

        gateway
            .advance(AdvancePromotionRequest {
                candidate_id: candidate_id.clone(),
                action: PromotionAction::FinishCanary,
                client_id,
            })
            .await
            .unwrap();

        let error = gateway
            .approve(ApprovePromotionRequest {
                candidate_id: candidate_id.clone(),
                approver: agent_actor(),
                client_id,
            })
            .await
            .expect_err("an agent actor must never reach Promoted");
        assert_eq!(error.code, "promotion.requires-human-approval");

        let snapshot = PromotionStore::new()
            .get(&pool, &candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.candidate.stage(), PromotionStage::ComparisonReady);
    }

    #[tokio::test]
    async fn skills_require_permission_review_regardless_of_client_flag() {
        let (_tmp, pool) = temp_pool().await;
        let gateway = PromotionStoreGateway::new(pool.clone());
        let client_id = human_client_id();
        let candidate_id = gateway
            .propose(ProposePromotionRequest {
                kind: "skill".to_string(),
                name: "dangerous-deploy".to_string(),
                version: 1,
                requires_permission_review: false,
                idempotency_key: "propose-skill-1".to_string(),
                client_id,
            })
            .await
            .unwrap();
        bind_report(&pool, &candidate_id, "report-skill", &passing_report()).await;

        let permission_error = gateway
            .advance(AdvancePromotionRequest {
                candidate_id: candidate_id.clone(),
                action: PromotionAction::RunRegression,
                client_id,
            })
            .await
            .expect_err("skills require review even when the proposer omitted the flag");
        assert_eq!(
            permission_error.code,
            "promotion.permission-review-required"
        );
        for action in [
            PromotionAction::ReviewPermissions,
            PromotionAction::RunRegression,
            PromotionAction::StartShadow,
            PromotionAction::StartCanary,
        ] {
            gateway
                .advance(AdvancePromotionRequest {
                    candidate_id: candidate_id.clone(),
                    action,
                    client_id,
                })
                .await
                .unwrap();
        }

        PromotionStore::new()
            .observe_canary_samples(&pool, &candidate_id, false, 100)
            .await
            .unwrap();

        gateway
            .advance(AdvancePromotionRequest {
                candidate_id: candidate_id.clone(),
                action: PromotionAction::FinishCanary,
                client_id,
            })
            .await
            .unwrap();

        let error = gateway
            .approve(ApprovePromotionRequest {
                candidate_id: candidate_id.clone(),
                approver: agent_actor(),
                client_id,
            })
            .await
            .expect_err("an agent actor must never reach Promoted");
        assert_eq!(error.code, "promotion.requires-human-approval");

        let snapshot = PromotionStore::new()
            .get(&pool, &candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.candidate.stage(), PromotionStage::ComparisonReady);
    }

    #[tokio::test]
    async fn finishing_an_unobserved_canary_is_rejected_with_the_right_code() {
        let (_tmp, pool) = temp_pool().await;
        let gateway = PromotionStoreGateway::new(pool.clone());
        let client_id = human_client_id();
        let candidate_id = gateway
            .propose(ProposePromotionRequest {
                kind: "prompt".to_string(),
                name: "coding-agent".to_string(),
                version: 2,
                requires_permission_review: false,
                idempotency_key: "propose-3".to_string(),
                client_id,
            })
            .await
            .unwrap();
        bind_report(&pool, &candidate_id, "report-unobserved", &passing_report()).await;
        for action in [
            PromotionAction::RunRegression,
            PromotionAction::StartShadow,
            PromotionAction::StartCanary,
        ] {
            gateway
                .advance(AdvancePromotionRequest {
                    candidate_id: candidate_id.clone(),
                    action,
                    client_id,
                })
                .await
                .unwrap();
        }

        let error = gateway
            .advance(AdvancePromotionRequest {
                candidate_id: candidate_id.clone(),
                action: PromotionAction::FinishCanary,
                client_id,
            })
            .await
            .expect_err("zero observations must not finish the canary");
        assert_eq!(error.code, "promotion.canary-insufficient-evidence");

        // There is no wire field left to assert a sample count with: the action
        // is payload-free and a prompt candidate is unattributable anyway.
        let unattributable = gateway
            .advance(AdvancePromotionRequest {
                candidate_id: candidate_id.clone(),
                action: PromotionAction::ObserveCanary,
                client_id,
            })
            .await
            .expect_err("a prompt candidate has no attributable executions");
        assert_eq!(
            unattributable.code,
            "promotion.canary-unattributable-artifact"
        );

        // Record 1 sample directly (too small)
        PromotionStore::new()
            .observe_canary_samples(&pool, &candidate_id, false, 1)
            .await
            .unwrap();

        let too_small = gateway
            .advance(AdvancePromotionRequest {
                candidate_id,
                action: PromotionAction::FinishCanary,
                client_id,
            })
            .await
            .expect_err("one favorable sample is not canary evidence");
        assert_eq!(too_small.code, "promotion.canary-insufficient-evidence");
    }

    #[tokio::test]
    async fn a_canary_regression_auto_rolls_back_and_manual_rollback_is_attributed() {
        let (_tmp, pool) = temp_pool().await;
        let gateway = PromotionStoreGateway::new(pool.clone());
        let client_id = human_client_id();
        let candidate_id = gateway
            .propose(ProposePromotionRequest {
                kind: "router".to_string(),
                name: "escalation".to_string(),
                version: 1,
                requires_permission_review: false,
                idempotency_key: "propose-4".to_string(),
                client_id,
            })
            .await
            .unwrap();
        bind_report(
            &pool,
            &candidate_id,
            "report-auto-rollback",
            &passing_report(),
        )
        .await;
        for action in [
            PromotionAction::RunRegression,
            PromotionAction::StartShadow,
            PromotionAction::StartCanary,
        ] {
            gateway
                .advance(AdvancePromotionRequest {
                    candidate_id: candidate_id.clone(),
                    action,
                    client_id,
                })
                .await
                .unwrap();
        }
        // A regression signal auto-rolls-back
        PromotionStore::new()
            .observe_canary_samples(&pool, &candidate_id, true, 100)
            .await
            .unwrap();

        let snapshot = PromotionStore::new()
            .get(&pool, &candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.candidate.stage(), PromotionStage::RolledBack);

        // A second, unrelated candidate promoted then manually rolled back —
        // the manual path attributes the mapped human actor.
        let promoted_id = gateway
            .propose(ProposePromotionRequest {
                kind: "router".to_string(),
                name: "escalation".to_string(),
                version: 2,
                requires_permission_review: false,
                idempotency_key: "propose-5".to_string(),
                client_id,
            })
            .await
            .unwrap();
        bind_report(
            &pool,
            &promoted_id,
            "report-manual-rollback",
            &passing_report(),
        )
        .await;
        for action in [
            PromotionAction::RunRegression,
            PromotionAction::StartShadow,
            PromotionAction::StartCanary,
        ] {
            gateway
                .advance(AdvancePromotionRequest {
                    candidate_id: promoted_id.clone(),
                    action,
                    client_id,
                })
                .await
                .unwrap();
        }

        PromotionStore::new()
            .observe_canary_samples(&pool, &promoted_id, false, 100)
            .await
            .unwrap();

        gateway
            .advance(AdvancePromotionRequest {
                candidate_id: promoted_id.clone(),
                action: PromotionAction::FinishCanary,
                client_id,
            })
            .await
            .unwrap();

        gateway
            .approve(ApprovePromotionRequest {
                candidate_id: promoted_id.clone(),
                approver: human_actor(client_id),
                client_id,
            })
            .await
            .unwrap();
        gateway
            .rollback(RollbackPromotionRequest {
                candidate_id: promoted_id.clone(),
                actor: human_actor(client_id),
                client_id,
            })
            .await
            .expect("manual rollback of a promoted candidate succeeds");
        let snapshot = PromotionStore::new()
            .get(&pool, &promoted_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.candidate.stage(), PromotionStage::RolledBack);
    }

    /// Seed one real execution the way `crates/daemon/src/ledger.rs` does at a
    /// run's terminal event: a session, a run, and one `execution_observations`
    /// row carrying the MEASURED `model_id`, `completion` and `latency_ms`.
    /// `latency_ms: None` reproduces the honest case the schema exists for — a
    /// run whose latency was never measured.
    async fn record_execution(
        pool: &SqlitePool,
        run_id: &str,
        model_id: &str,
        completion: &str,
        latency_ms: Option<i64>,
    ) {
        sqlx::query(
            "INSERT OR IGNORE INTO sessions (id, title, state, created_at, updated_at) \
             VALUES ('sess-canary', 'canary', 'open', '2026-01-01T00:00:00Z', \
                     '2026-01-01T00:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json) \
             VALUES (?, 'sess-canary', 'canary traffic', 'completed', 'build', 'auto', '{}')",
        )
        .bind(run_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO execution_observations \
             (owner_uid, run_id, attempt, node_id, session_id, model_id, completion, \
              latency_ms, observed_at) \
             VALUES (1, ?, 0, '', 'sess-canary', ?, ?, ?, ?)",
        )
        .bind(run_id)
        .bind(model_id)
        .bind(completion)
        .bind(latency_ms)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap();
    }

    /// Drive a fresh model-profile candidate to the `Canary` stage. Returns its
    /// id; the caller then seeds executions and observes.
    async fn candidate_in_canary(
        gateway: &PromotionStoreGateway,
        pool: &SqlitePool,
        client_id: ClientId,
        model_id: &str,
        key: &str,
    ) -> String {
        let candidate_id = gateway
            .propose(ProposePromotionRequest {
                kind: "model-profile".to_string(),
                name: model_id.to_string(),
                version: 2,
                requires_permission_review: false,
                idempotency_key: key.to_string(),
                client_id,
            })
            .await
            .expect("propose accepted");
        bind_report(
            pool,
            &candidate_id,
            &format!("report-{key}"),
            &passing_report(),
        )
        .await;
        for action in [
            PromotionAction::RunRegression,
            PromotionAction::StartShadow,
            PromotionAction::StartCanary,
        ] {
            gateway
                .advance(AdvancePromotionRequest {
                    candidate_id: candidate_id.clone(),
                    action,
                    client_id,
                })
                .await
                .expect("advance accepted");
        }
        candidate_id
    }

    /// The property the whole change exists for: the sample count that satisfies
    /// `MIN_CANARY_SAMPLES` is the number of executions that actually ran, read
    /// out of the daemon's own `execution_observations` rows. Nothing in the
    /// request says how many there were — `ObserveCanary` has no payload.
    #[tokio::test]
    async fn the_canary_sample_count_is_counted_from_recorded_executions() {
        let (_tmp, pool) = temp_pool().await;
        let gateway = PromotionStoreGateway::new(pool.clone());
        let client_id = human_client_id();
        let candidate_id =
            candidate_in_canary(&gateway, &pool, client_id, "local/qwen-coder", "measured").await;

        // 100 real executions on the candidate model, 40 concurrent baseline
        // executions on another model. Same success rate, same latency.
        for i in 0..100 {
            record_execution(
                &pool,
                &format!("run-cand-{i}"),
                "local/qwen-coder",
                "successful",
                Some(100),
            )
            .await;
        }
        for i in 0..40 {
            record_execution(
                &pool,
                &format!("run-base-{i}"),
                "hosted/strong",
                "successful",
                Some(100),
            )
            .await;
        }

        gateway
            .advance(AdvancePromotionRequest {
                candidate_id: candidate_id.clone(),
                action: PromotionAction::ObserveCanary,
                client_id,
            })
            .await
            .expect("a measured, non-regressing slice is observed");

        let (sample_count, regressed, metrics_json): (i64, i64, String) = sqlx::query_as(
            "SELECT sample_count, regressed, metrics_json FROM promotion_canary_evidence \
             WHERE candidate_id = ?",
        )
        .bind(&candidate_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(sample_count, 100, "the count is the executions on file");
        assert_eq!(regressed, 0);
        let metrics: CanaryMetrics = serde_json::from_str(&metrics_json).unwrap();
        assert_eq!(metrics.sample_count, 100);
        assert_eq!(metrics.error_rate_bps, 0, "100 measured successes");
        assert_eq!(metrics.p95_latency_ms, 100);
        assert_eq!(metrics.baseline_p95_latency_ms, 100);

        // …and only now does the population threshold clear.
        gateway
            .advance(AdvancePromotionRequest {
                candidate_id,
                action: PromotionAction::FinishCanary,
                client_id,
            })
            .await
            .expect("100 measured samples satisfy MIN_CANARY_SAMPLES");
    }

    /// Every way the measurement can come up short refuses the promotion, and
    /// `FinishCanary` stays shut behind every one of them.
    #[tokio::test]
    async fn insufficient_measured_evidence_refuses_instead_of_passing() {
        let (_tmp, pool) = temp_pool().await;
        let gateway = PromotionStoreGateway::new(pool.clone());
        let client_id = human_client_id();
        let candidate_id =
            candidate_in_canary(&gateway, &pool, client_id, "local/qwen-coder", "refusals").await;

        let observe = |candidate: String| {
            let gateway = gateway.clone();
            async move {
                gateway
                    .advance(AdvancePromotionRequest {
                        candidate_id: candidate,
                        action: PromotionAction::ObserveCanary,
                        client_id,
                    })
                    .await
            }
        };

        // Nothing ran at all.
        assert_eq!(
            observe(candidate_id.clone()).await.unwrap_err().code,
            "promotion.canary-evidence-missing"
        );

        // The candidate ran; nothing else did. A canary with no concurrent
        // baseline is not evidence that the candidate did not regress.
        record_execution(
            &pool,
            "run-solo",
            "local/qwen-coder",
            "successful",
            Some(90),
        )
        .await;
        assert_eq!(
            observe(candidate_id.clone()).await.unwrap_err().code,
            "promotion.canary-baseline-missing"
        );

        // Both sides ran, but no baseline latency was ever measured. An absent
        // latency is not a latency of zero, so the comparison is refused rather
        // than computed against a fabricated 0ms baseline (which every candidate
        // would "regress" against).
        record_execution(&pool, "run-base-nolat", "hosted/strong", "successful", None).await;
        assert_eq!(
            observe(candidate_id.clone()).await.unwrap_err().code,
            "promotion.canary-latency-unmeasured"
        );

        // Not one refusal left a sample behind, so the canary still cannot end.
        let rows: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM promotion_canary_evidence")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows.0, 0, "a refused observation records no evidence");
        let error = gateway
            .advance(AdvancePromotionRequest {
                candidate_id,
                action: PromotionAction::FinishCanary,
                client_id,
            })
            .await
            .expect_err("no measured evidence must not finish the canary");
        assert_eq!(error.code, "promotion.canary-insufficient-evidence");
    }

    /// A measured error-rate regression rolls the candidate back on the spot —
    /// derived from the recorded completions, not asserted by anyone.
    #[tokio::test]
    async fn a_measured_regression_auto_rolls_back() {
        let (_tmp, pool) = temp_pool().await;
        let gateway = PromotionStoreGateway::new(pool.clone());
        let client_id = human_client_id();
        let candidate_id =
            candidate_in_canary(&gateway, &pool, client_id, "local/qwen-coder", "regressed").await;

        // 20 of 100 candidate executions failed (2000bps) against a clean
        // concurrent baseline (0bps) — far past the 100bps tolerance.
        for i in 0..100 {
            let completion = if i < 20 { "failed" } else { "successful" };
            record_execution(
                &pool,
                &format!("run-reg-{i}"),
                "local/qwen-coder",
                completion,
                Some(100),
            )
            .await;
        }
        for i in 0..40 {
            record_execution(
                &pool,
                &format!("run-reg-base-{i}"),
                "hosted/strong",
                "successful",
                Some(100),
            )
            .await;
        }

        gateway
            .advance(AdvancePromotionRequest {
                candidate_id: candidate_id.clone(),
                action: PromotionAction::ObserveCanary,
                client_id,
            })
            .await
            .expect("observing a regression is accepted; it is the verdict that is adverse");

        let (regressed,): (i64,) = sqlx::query_as(
            "SELECT regressed FROM promotion_canary_evidence WHERE candidate_id = ?",
        )
        .bind(&candidate_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(regressed, 1);
        let snapshot = PromotionStore::new()
            .get(&pool, &candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.candidate.stage(), PromotionStage::RolledBack);
    }

    /// The derivation itself, on rows, with no database: an unmeasured latency
    /// is dropped from the latency population without dropping the run from the
    /// outcome population, and an unobserved outcome contributes to neither.
    #[test]
    fn unmeasured_columns_are_dropped_rather_than_read_as_zero() {
        let window_start = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let window_end = chrono::DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let row = |model: &str, completion: &str, latency: Option<i64>| ObservationRow {
            model_id: Some(model.to_string()),
            completion: Some(completion.to_string()),
            latency_ms: latency,
            observed_at: "2026-01-01T12:00:00Z".to_string(),
        };

        let rows = vec![
            row("cand", "successful", Some(200)),
            row("cand", "failed", None),
            // Neither of these is a measured outcome: 'incomplete' means the
            // outcome was not observed and 'cancelled' says nothing about the
            // candidate. Folding either into the denominator would move the
            // error rate on evidence that does not exist.
            row("cand", "incomplete", Some(5)),
            row("cand", "cancelled", Some(5)),
            row("base", "successful", Some(100)),
        ];
        let slice = measure_canary_slice(&rows, "cand", window_start, window_end)
            .expect("one measured latency on each side is enough to compare");
        assert_eq!(
            slice.sample_count, 2,
            "the two measured outcomes count; the unobserved two do not"
        );
        assert_eq!(
            slice.metrics.error_rate_bps, 5_000,
            "1 failure in 2 measured outcomes"
        );
        assert_eq!(
            slice.metrics.p95_latency_ms, 200,
            "the run with no measured latency is absent from the p95, not a 0ms sample"
        );

        // Drop the one baseline latency and the comparison refuses outright
        // rather than falling back to a zero baseline.
        let rows = vec![
            row("cand", "successful", Some(200)),
            row("base", "successful", None),
        ];
        assert_eq!(
            measure_canary_slice(&rows, "cand", window_start, window_end)
                .unwrap_err()
                .code,
            "promotion.canary-latency-unmeasured"
        );

        // A row outside the slice is not this slice's evidence.
        let rows = vec![
            row("cand", "successful", Some(200)),
            row("base", "successful", Some(100)),
            ObservationRow {
                model_id: Some("cand".to_string()),
                completion: Some("failed".to_string()),
                latency_ms: Some(900),
                observed_at: "2025-12-01T00:00:00Z".to_string(),
            },
        ];
        let slice = measure_canary_slice(&rows, "cand", window_start, window_end).unwrap();
        assert_eq!(slice.sample_count, 1, "the pre-window failure is excluded");
        assert_eq!(slice.metrics.error_rate_bps, 0);
    }

    #[tokio::test]
    async fn unrecognized_artifact_kind_is_rejected_before_touching_the_store() {
        let (_tmp, pool) = temp_pool().await;
        let gateway = PromotionStoreGateway::new(pool);
        let error = gateway
            .propose(ProposePromotionRequest {
                kind: "quantum-flux-capacitor".to_string(),
                name: "n/a".to_string(),
                version: 1,
                requires_permission_review: false,
                idempotency_key: "propose-bad-kind".to_string(),
                client_id: human_client_id(),
            })
            .await
            .expect_err("an unrecognized kind must not silently coerce to some default");
        assert_eq!(error.code, "promotion.invalid-kind");
    }
}
