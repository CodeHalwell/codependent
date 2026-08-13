//! The daemon's promotion-pipeline host (Phase 7 STEP 7.5).
//!
//! Like [`WorkflowConductorHost`](crate::workflows::WorkflowConductorHost),
//! this lives in the assembly binary because it bridges the daemon (which
//! declares the [`PromotionGateway`] seam) and `codypendent-eval` (which owns
//! the [`Candidate`] state machine and the durable [`PromotionStore`]). The
//! daemon crate cannot name the eval crate, so the composition happens here.
//!
//! [`PromotionStoreGateway`] fills the seam by delegating every method
//! straight to [`PromotionStore`] over the daemon's pool — there is no
//! additional logic to get wrong here, which is deliberate: the state-machine
//! rules (no self-promotion, no unobserved canary) live in `codypendent-eval`
//! and this host must not re-implement (or worse, loosen) them. Its only two
//! jobs are (1) translating the wire-carried artifact `kind` string into an
//! [`ArtifactKind`] and (2) attributing a fresh proposal's author.

use codypendent_daemon::promotion::{
    AdvancePromotionRequest, ApprovePromotionRequest, PromotionActionFuture, PromotionGateway,
    PromotionProposeFuture, ProposePromotionRequest, RollbackPromotionRequest,
    SubmitEvalEvidenceRequest,
};
use codypendent_eval::{
    ArtifactKind, ArtifactVersion, CanaryOutcome, PromotionStore, PromotionStoreError, SuiteReport,
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
                PromotionAction::ObserveCanary { metrics } => {
                    validate_canary_metrics(&metrics)?;
                    let regressed = canary_regressed(&metrics);
                    sqlx::query(
                        "INSERT INTO promotion_canary_evidence \
                         (id, candidate_id, metrics_json, sample_count, regressed, observed_at) \
                         VALUES (?, ?, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
                    )
                    .bind(uuid::Uuid::now_v7().to_string())
                    .bind(&request.candidate_id)
                    .bind(serde_json::to_string(&metrics).map_err(|error| {
                        CodypendentError::new("promotion.store-error", error.to_string(), true)
                    })?)
                    .bind(i64::try_from(metrics.sample_count).map_err(|_| {
                        CodypendentError::new(
                            "promotion.invalid-canary-evidence",
                            "sample_count is too large".to_string(),
                            false,
                        )
                    })?)
                    .bind(if regressed { 1_i64 } else { 0_i64 })
                    .execute(&host.pool)
                    .await
                    .map_err(|error| {
                        CodypendentError::new("promotion.store-error", error.to_string(), true)
                    })?;
                    host.store
                        .observe_canary_samples(
                            &host.pool,
                            &request.candidate_id,
                            regressed,
                            metrics.sample_count,
                        )
                        .await
                        .map(|outcome| {
                            // The auto-rollback record is already persisted (with
                            // its own audit row) by the store; the command reply
                            // only needs to signal success, so the outcome itself
                            // is not surfaced further here (a `PromotionProposed`-
                            // style reply carrying it is a natural follow-up if a
                            // client ever needs to react to an auto-rollback
                            // synchronously — see the report's deferred-items list).
                            let _: CanaryOutcome = outcome;
                        })
                        .map_err(store_error_to_protocol)
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

    fn regressing_canary_metrics() -> CanaryMetrics {
        CanaryMetrics {
            error_rate_bps: 201,
            ..passing_canary_metrics()
        }
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
            PromotionAction::ObserveCanary {
                metrics: passing_canary_metrics(),
            },
            PromotionAction::FinishCanary,
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

    #[tokio::test]
    async fn an_agent_actor_handed_to_approve_is_refused_even_by_the_real_gateway() {
        // The gateway performs NO actor gating itself (see the module doc — it
        // must not re-implement or loosen the rule); this proves the guard it
        // relies on (`Candidate::approve`) still holds when reached through
        // the full assembly, not just the bare eval-crate type. The daemon's
        // OWN role gate (server.rs) is what actually prevents an
        // `Actor::Agent` from ever being constructed for this command in
        // production; see `crates/daemon/tests/server_it.rs`.
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
            PromotionAction::ObserveCanary {
                metrics: passing_canary_metrics(),
            },
            PromotionAction::FinishCanary,
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

        gateway
            .advance(AdvancePromotionRequest {
                candidate_id: candidate_id.clone(),
                action: PromotionAction::ObserveCanary {
                    metrics: CanaryMetrics {
                        sample_count: 1,
                        ..passing_canary_metrics()
                    },
                },
                client_id,
            })
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
        // A regression signal auto-rolls-back — no approve/rollback command
        // needed, and the audit trail attributes "system" (proven at the
        // store layer already; here it just must not surface as an error).
        gateway
            .advance(AdvancePromotionRequest {
                candidate_id: candidate_id.clone(),
                action: PromotionAction::ObserveCanary {
                    metrics: regressing_canary_metrics(),
                },
                client_id,
            })
            .await
            .expect("an auto-rollback is a successful advance, not an error");
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
            PromotionAction::ObserveCanary {
                metrics: passing_canary_metrics(),
            },
            PromotionAction::FinishCanary,
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
