//! Experimentation, shadow execution, canary comparison, drift detection, and promotion gating.
//!
//! # Core invariants
//! 1. **Deterministic assignment**: Assignment is a pure function of `(seed, key)`
//!    and replayable across processes.
//! 2. **Single sample per unit**: Each assignment key contributes at most once to an
//!    experiment.
//! 3. **Safety rollback precedes horizon**: Immediate rollback on severe quality
//!    degradation or error rate spike, without waiting for sample accumulation.
//! 4. **Human promotion only**: Runner, candidate, grader, and unscoped org admin
//!    cannot self-promote. Approvals bind to the exact action digest and expiry.

use chrono::{DateTime, Utc};
use codypendent_protocol::events::Actor;
use codypendent_routing::arms::{RouteArm, RouteArmResult, RouteEvalReport};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::observation::QualityObservation;
use crate::promote::MIN_CANARY_SAMPLES;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExperimentError {
    #[error("experiment is in state '{current}', expected '{expected}'")]
    IllegalState { current: String, expected: String },
    #[error("analysis plan cannot be modified after activation")]
    PlanImmutableAfterActivation,
    #[error("insufficient evidence: observed {observed} samples, required {required}")]
    InsufficientEvidence { observed: usize, required: usize },
    #[error("safety rollback triggered: {reason}")]
    SafetyRollback { reason: String },
    #[error("principal '{principal}' of role '{role}' is not authorized to approve promotion (human approver with scoped authority required)")]
    UnauthorizedApprover { principal: String, role: String },
    #[error("approval expired at {expired_at}")]
    ApprovalExpired { expired_at: DateTime<Utc> },
    #[error("action digest mismatch: approval is not bound to this exact comparison evidence")]
    DigestMismatch,
    #[error("duplicate sample unit for key '{key}'")]
    DuplicateSampleUnit { key: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExperimentKind {
    Shadow,
    Canary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HorizonKind {
    Fixed,
    Sequential,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExperimentState {
    Draft,
    Active,
    Stopped,
    Analyzed,
    RolledBack,
}

impl ExperimentState {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Stopped => "stopped",
            Self::Analyzed => "analyzed",
            Self::RolledBack => "rolled-back",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssignedArm {
    Baseline,
    Candidate,
}

impl AssignedArm {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Candidate => "candidate",
        }
    }
}

/// The analysis plan, fixed BEFORE the experiment starts. Immutable after activation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisPlan {
    pub min_samples: u64,
    pub horizon_kind: HorizonKind,
    /// Non-inferiority margin: candidate success rate must not be worse than
    /// baseline minus this margin (e.g. 0.05 = 5%).
    pub non_inferiority_margin: f64,
    /// Maximum allowable relative cost increase (e.g. 0.10 = +10%).
    pub max_cost_increase_pct: Option<f64>,
    /// Maximum allowable relative latency increase (e.g. 0.20 = +20%).
    pub max_latency_increase_pct: Option<f64>,
    /// Maximum allowable absolute error rate in basis points (0..=10000).
    pub max_error_rate_bps: Option<u16>,
    /// Significance level.
    pub alpha: f64,
}

impl Default for AnalysisPlan {
    fn default() -> Self {
        Self {
            min_samples: MIN_CANARY_SAMPLES,
            horizon_kind: HorizonKind::Fixed,
            non_inferiority_margin: 0.05,
            max_cost_increase_pct: Some(0.15),
            max_latency_increase_pct: Some(0.20),
            max_error_rate_bps: Some(500), // 5%
            alpha: 0.05,
        }
    }
}

/// A quality experiment (Shadow or Canary).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityExperiment {
    pub id: String,
    pub organization_id: String,
    pub kind: ExperimentKind,
    pub candidate_id: String,
    pub artifact_kind: String,
    pub artifact_name: String,
    pub artifact_version: u32,
    pub baseline_arm: RouteArm,
    pub candidate_arm: RouteArm,
    pub assignment_seed: Vec<u8>,
    pub candidate_share_bps: u16,
    pub candidate_budget_micro_usd: u64,
    pub analysis_plan: AnalysisPlan,
    pub state: ExperimentState,
    pub activated_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl QualityExperiment {
    /// Create a new draft experiment.
    // One argument per NOT-NULL column of `quality_experiments`
    // (`crates/control-plane/migrations/0010_quality_observations.sql`). Every
    // one is required at insert time and none has a safe default — a
    // partially-built experiment with a defaulted seed, share or budget is
    // exactly the thing that must not exist.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        organization_id: impl Into<String>,
        kind: ExperimentKind,
        candidate_id: impl Into<String>,
        artifact_kind: impl Into<String>,
        artifact_name: impl Into<String>,
        artifact_version: u32,
        baseline_arm: RouteArm,
        candidate_arm: RouteArm,
        assignment_seed: Vec<u8>,
        candidate_share_bps: u16,
        candidate_budget_micro_usd: u64,
        analysis_plan: AnalysisPlan,
    ) -> Self {
        Self {
            id: Uuid::now_v7().to_string(),
            organization_id: organization_id.into(),
            kind,
            candidate_id: candidate_id.into(),
            artifact_kind: artifact_kind.into(),
            artifact_name: artifact_name.into(),
            artifact_version,
            baseline_arm,
            candidate_arm,
            assignment_seed,
            candidate_share_bps,
            candidate_budget_micro_usd,
            analysis_plan,
            state: ExperimentState::Draft,
            activated_at: None,
            stopped_at: None,
            created_at: Utc::now(),
        }
    }

    /// Activate the experiment.
    pub fn activate(&mut self) -> Result<(), ExperimentError> {
        if self.state != ExperimentState::Draft {
            return Err(ExperimentError::IllegalState {
                current: self.state.as_str().to_string(),
                expected: "draft".to_string(),
            });
        }
        self.state = ExperimentState::Active;
        self.activated_at = Some(Utc::now());
        Ok(())
    }

    /// Update analysis plan before activation.
    pub fn update_analysis_plan(&mut self, plan: AnalysisPlan) -> Result<(), ExperimentError> {
        if self.state != ExperimentState::Draft {
            return Err(ExperimentError::PlanImmutableAfterActivation);
        }
        self.analysis_plan = plan;
        Ok(())
    }

    /// Stop the experiment.
    pub fn stop(&mut self) -> Result<(), ExperimentError> {
        if self.state != ExperimentState::Active {
            return Err(ExperimentError::IllegalState {
                current: self.state.as_str().to_string(),
                expected: "active".to_string(),
            });
        }
        self.state = ExperimentState::Stopped;
        self.stopped_at = Some(Utc::now());
        Ok(())
    }

    /// Deterministic assignment: a pure function of `(assignment_seed, key)`.
    ///
    /// For a Shadow experiment: both arms run on 100% (candidate share = 10000)
    /// and candidate effects are discarded by the runner.
    #[must_use]
    pub fn assign_arm(&self, key: &str) -> AssignedArm {
        let mut hasher = Sha256::new();
        hasher.update(&self.assignment_seed);
        hasher.update(b":");
        hasher.update(key.as_bytes());
        let digest = hasher.finalize();

        let slice: [u8; 8] = digest[0..8].try_into().unwrap_or([0; 8]);
        let val = u64::from_be_bytes(slice);
        let bucket = (val % 10_000) as u16;

        if bucket < self.candidate_share_bps {
            AssignedArm::Candidate
        } else {
            AssignedArm::Baseline
        }
    }
}

/// A recorded sample unit in an experiment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperimentSample {
    pub id: i64,
    pub experiment_id: String,
    pub arm: AssignedArm,
    pub observation_id: String,
    pub assignment_key: String,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComparisonVerdict {
    Pass,
    Fail,
    InsufficientEvidence,
    SafetyRollback,
}

impl ComparisonVerdict {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::InsufficientEvidence => "insufficient-evidence",
            Self::SafetyRollback => "safety-rollback",
        }
    }
}

/// Server-computed comparison for an experiment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityComparison {
    pub id: String,
    pub experiment_id: String,
    pub baseline_samples: usize,
    pub candidate_samples: usize,
    pub route_eval_report: RouteEvalReport,
    pub quality_non_inferior: Option<bool>,
    pub cost_within_limit: Option<bool>,
    pub latency_within_limit: Option<bool>,
    pub missing_measurements: Vec<String>,
    pub verdict: ComparisonVerdict,
    pub computed_at: DateTime<Utc>,
}

/// Build a [`RouteArmResult`] from measured [`QualityObservation`]s.
///
/// Each rate and mean is computed over the observations that MEASURED that
/// dimension, and is `None` when none did. Nothing here folds an absent
/// measurement into a total:
///
/// * an observation with `succeeded: None` is outside the success-rate
///   population entirely — it is neither a success nor a failure, and counting
///   it as either would move a gate on evidence that does not exist;
/// * `mean_cost_usd` is `None` rather than `0.0` for an arm with no priced
///   execution, so it cannot undercut a priced comparator;
/// * `tool_call_error_rate` and `unsafe_proposal_rate` are always `None`,
///   because [`QualityObservation`] measures neither. The previous revision
///   returned `1.0 - task_success_rate` for the first (a different quantity
///   wearing this field's name) and a flat `0.0` for the second (which reads as
///   "nothing unsafe was produced" when nothing was checked).
#[must_use]
pub fn build_arm_result(
    arm: RouteArm,
    observations: &[QualityObservation],
) -> Option<RouteArmResult> {
    if observations.is_empty() {
        return None;
    }

    let mut succeeded_count = 0_usize;
    let mut measured_success_count = 0_usize;
    let mut escalated_count = 0_usize;
    let mut measured_escalation_count = 0_usize;
    let mut cost_sum_usd = 0.0_f64;
    let mut measured_cost_count = 0_usize;
    let mut latency_sum_ms = 0.0_f64;
    let mut measured_latency_count = 0_usize;

    for obs in observations {
        if let Some(succeeded) = obs.succeeded {
            measured_success_count += 1;
            if succeeded {
                succeeded_count += 1;
            }
        }
        if let Some(escalated) = obs.escalated {
            measured_escalation_count += 1;
            if escalated {
                escalated_count += 1;
            }
        }
        if let Some(c) = obs.cost_micro_usd {
            cost_sum_usd += (c as f64) / 1_000_000.0;
            measured_cost_count += 1;
        }
        if let Some(l) = obs.latency_ms {
            latency_sum_ms += l as f64;
            measured_latency_count += 1;
        }
    }

    let mean_over =
        |sum: f64, count: usize| -> Option<f64> { (count > 0).then(|| sum / (count as f64)) };

    Some(RouteArmResult {
        arm,
        task_success_rate: mean_over(succeeded_count as f64, measured_success_count),
        mean_cost_usd: mean_over(cost_sum_usd, measured_cost_count),
        mean_latency_ms: mean_over(latency_sum_ms, measured_latency_count),
        escalation_rate: mean_over(escalated_count as f64, measured_escalation_count),
        tool_call_error_rate: None,
        unsafe_proposal_rate: None,
    })
}

/// Evaluate an experiment across its recorded baseline and candidate observations.
#[must_use]
pub fn evaluate_experiment(
    experiment: &QualityExperiment,
    baseline_obs: &[QualityObservation],
    candidate_obs: &[QualityObservation],
) -> QualityComparison {
    let baseline_samples = baseline_obs.len();
    let candidate_samples = candidate_obs.len();

    let baseline_res = build_arm_result(experiment.baseline_arm, baseline_obs);
    let candidate_res = build_arm_result(experiment.candidate_arm, candidate_obs);

    let mut results = Vec::new();
    if let Some(ref b) = baseline_res {
        results.push(b.clone());
    }
    if let Some(ref c) = candidate_res {
        results.push(c.clone());
    }

    let report = RouteEvalReport::new(0.80, results);

    let mut missing_measurements = Vec::new();
    if baseline_obs.iter().any(|o| o.latency_ms.is_none())
        || candidate_obs.iter().any(|o| o.latency_ms.is_none())
    {
        missing_measurements.push("latency".to_string());
    }
    if baseline_obs.iter().any(|o| o.cost_micro_usd.is_none())
        || candidate_obs.iter().any(|o| o.cost_micro_usd.is_none())
    {
        missing_measurements.push("cost".to_string());
    }
    if baseline_obs.iter().any(|o| o.succeeded.is_none())
        || candidate_obs.iter().any(|o| o.succeeded.is_none())
    {
        missing_measurements.push("success".to_string());
    }

    // Check Safety Rollback FIRST (precedes horizon).
    //
    // Each sub-check fires only when BOTH sides measured the dimension it reads.
    // A dimension nobody measured cannot show a regression; it also cannot show
    // the absence of one, which is why an unmeasured dimension still blocks the
    // pass verdict below via `missing_measurements`.
    if let (Some(b), Some(c)) = (&baseline_res, &candidate_res) {
        let paired = |cand: Option<f64>, base: Option<f64>| cand.zip(base);
        let error_regressed = paired(c.task_success_rate, b.task_success_rate)
            .is_some_and(|(cand, base)| cand < base - 0.01);
        let latency_regressed = paired(c.mean_latency_ms, b.mean_latency_ms)
            .is_some_and(|(cand, base)| base > 0.0 && cand > base * 1.20);
        let quality_severely_dropped = paired(c.task_success_rate, b.task_success_rate)
            .is_some_and(|(cand, base)| {
                cand < base - (experiment.analysis_plan.non_inferiority_margin * 2.0)
            });

        if error_regressed || latency_regressed || quality_severely_dropped {
            return QualityComparison {
                id: Uuid::now_v7().to_string(),
                experiment_id: experiment.id.clone(),
                baseline_samples,
                candidate_samples,
                route_eval_report: report,
                quality_non_inferior: Some(false),
                cost_within_limit: Some(false),
                latency_within_limit: Some(false),
                missing_measurements,
                verdict: ComparisonVerdict::SafetyRollback,
                computed_at: Utc::now(),
            };
        }
    }

    // Check sample size and missing measurements
    if (candidate_samples as u64) < experiment.analysis_plan.min_samples
        || baseline_samples == 0
        || !missing_measurements.is_empty()
    {
        return QualityComparison {
            id: Uuid::now_v7().to_string(),
            experiment_id: experiment.id.clone(),
            baseline_samples,
            candidate_samples,
            route_eval_report: report,
            quality_non_inferior: None,
            cost_within_limit: None,
            latency_within_limit: None,
            missing_measurements,
            verdict: ComparisonVerdict::InsufficientEvidence,
            computed_at: Utc::now(),
        };
    }

    let (Some(b), Some(c)) = (&baseline_res, &candidate_res) else {
        return QualityComparison {
            id: Uuid::now_v7().to_string(),
            experiment_id: experiment.id.clone(),
            baseline_samples,
            candidate_samples,
            route_eval_report: report,
            quality_non_inferior: None,
            cost_within_limit: None,
            latency_within_limit: None,
            missing_measurements,
            verdict: ComparisonVerdict::InsufficientEvidence,
            computed_at: Utc::now(),
        };
    };

    // Each check is `None` when the dimension it reads was not measured on both
    // sides. `None` is neither a pass nor a fail — it is "not evaluable", and a
    // verdict cannot be Pass unless all three came back `Some(true)`.
    let quality_non_inferior = c
        .task_success_rate
        .zip(b.task_success_rate)
        .map(|(cand, base)| cand >= base - experiment.analysis_plan.non_inferiority_margin);

    let cost_within_limit = match experiment.analysis_plan.max_cost_increase_pct {
        Some(limit_pct) => c
            .mean_cost_usd
            .zip(b.mean_cost_usd)
            .map(|(cand, base)| cand <= base * (1.0 + limit_pct)),
        None => Some(true),
    };

    let latency_within_limit = match experiment.analysis_plan.max_latency_increase_pct {
        Some(limit_pct) => c
            .mean_latency_ms
            .zip(b.mean_latency_ms)
            .map(|(cand, base)| cand <= base * (1.0 + limit_pct)),
        None => Some(true),
    };

    let verdict = match (
        quality_non_inferior,
        cost_within_limit,
        latency_within_limit,
    ) {
        (Some(true), Some(true), Some(true)) => ComparisonVerdict::Pass,
        (Some(false), _, _) | (_, Some(false), _) | (_, _, Some(false)) => ComparisonVerdict::Fail,
        // At least one check could not be evaluated and none of them failed.
        // That is not a pass: it is missing evidence, and it must not promote.
        _ => ComparisonVerdict::InsufficientEvidence,
    };

    QualityComparison {
        id: Uuid::now_v7().to_string(),
        experiment_id: experiment.id.clone(),
        baseline_samples,
        candidate_samples,
        route_eval_report: report,
        quality_non_inferior,
        cost_within_limit,
        latency_within_limit,
        missing_measurements,
        verdict,
        computed_at: Utc::now(),
    }
}

/// A quality drift alert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityDriftAlert {
    pub id: String,
    pub organization_id: String,
    pub task_class: String,
    pub model_id: Option<String>,
    pub dimension: String,
    pub reference_window_start: DateTime<Utc>,
    pub reference_window_end: DateTime<Utc>,
    pub current_window_start: DateTime<Utc>,
    pub current_window_end: DateTime<Utc>,
    pub reference_value: f64,
    pub current_value: f64,
    pub reference_samples: usize,
    pub current_samples: usize,
    pub state: String,
    pub detected_at: DateTime<Utc>,
    pub acknowledged_by: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
}

/// Detect quality drift comparing a reference window against a current window.
// The two observation slices and the two window bounds are four of the nine:
// the alert row records the windows it compared, so a reader can tell a real
// drift from a window that simply moved.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn detect_drift(
    organization_id: impl Into<String>,
    task_class: impl Into<String>,
    model_id: Option<String>,
    dimension: impl Into<String>,
    reference_obs: &[QualityObservation],
    current_obs: &[QualityObservation],
    threshold_pct: f64,
    ref_window: (DateTime<Utc>, DateTime<Utc>),
    cur_window: (DateTime<Utc>, DateTime<Utc>),
) -> Option<QualityDriftAlert> {
    if reference_obs.is_empty() || current_obs.is_empty() {
        return None;
    }

    let dim = dimension.into();
    let (ref_val, cur_val) = match dim.as_str() {
        "latency" => {
            let ref_l: Vec<u64> = reference_obs.iter().filter_map(|o| o.latency_ms).collect();
            let cur_l: Vec<u64> = current_obs.iter().filter_map(|o| o.latency_ms).collect();
            if ref_l.is_empty() || cur_l.is_empty() {
                return None;
            }
            let r = ref_l.iter().sum::<u64>() as f64 / ref_l.len() as f64;
            let c = cur_l.iter().sum::<u64>() as f64 / cur_l.len() as f64;
            (r, c)
        }
        "cost" => {
            let ref_c: Vec<u64> = reference_obs
                .iter()
                .filter_map(|o| o.cost_micro_usd)
                .collect();
            let cur_c: Vec<u64> = current_obs
                .iter()
                .filter_map(|o| o.cost_micro_usd)
                .collect();
            if ref_c.is_empty() || cur_c.is_empty() {
                return None;
            }
            let r = ref_c.iter().sum::<u64>() as f64 / ref_c.len() as f64;
            let c = cur_c.iter().sum::<u64>() as f64 / cur_c.len() as f64;
            (r, c)
        }
        "quality" => {
            let ref_s: Vec<bool> = reference_obs.iter().filter_map(|o| o.succeeded).collect();
            let cur_s: Vec<bool> = current_obs.iter().filter_map(|o| o.succeeded).collect();
            if ref_s.is_empty() || cur_s.is_empty() {
                return None;
            }
            let r = ref_s.iter().filter(|&&s| s).count() as f64 / ref_s.len() as f64;
            let c = cur_s.iter().filter(|&&s| s).count() as f64 / cur_s.len() as f64;
            (r, c)
        }
        _ => return None,
    };

    let drifted = match dim.as_str() {
        "quality" => cur_val < ref_val * (1.0 - threshold_pct),
        _ => cur_val > ref_val * (1.0 + threshold_pct),
    };

    if drifted {
        Some(QualityDriftAlert {
            id: Uuid::now_v7().to_string(),
            organization_id: organization_id.into(),
            task_class: task_class.into(),
            model_id,
            dimension: dim,
            reference_window_start: ref_window.0,
            reference_window_end: ref_window.1,
            current_window_start: cur_window.0,
            current_window_end: cur_window.1,
            reference_value: ref_val,
            current_value: cur_val,
            reference_samples: reference_obs.len(),
            current_samples: current_obs.len(),
            state: "open".to_string(),
            detected_at: Utc::now(),
            acknowledged_by: None,
            acknowledged_at: None,
        })
    } else {
        None
    }
}

/// Compute the canonical action digest for an approval.
#[must_use]
pub fn compute_action_digest(
    candidate_id: &str,
    artifact_version: u32,
    comparison_id: &str,
    verdict: &str,
) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(b"codypendent-promotion-approval-v1\n");
    hasher.update(format!("{candidate_id}\n").as_bytes());
    hasher.update(format!("{artifact_version}\n").as_bytes());
    hasher.update(format!("{comparison_id}\n").as_bytes());
    hasher.update(format!("{verdict}\n").as_bytes());
    hasher.finalize().to_vec()
}

/// A promotion approval receipt binding exact evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityPromotionApproval {
    pub id: String,
    pub organization_id: String,
    pub repository_id: String,
    pub candidate_id: String,
    pub comparison_id: String,
    pub approver_user_id: String,
    pub approver_role: String,
    pub action_digest: Vec<u8>,
    pub expires_at: DateTime<Utc>,
    pub approved_at: DateTime<Utc>,
}

/// Validate that an approver actor has authority to approve promotion.
///
/// Refuses runner, candidate, grader, and unscoped org admin.
pub fn validate_promotion_actor(
    actor: &Actor,
    role: &str,
    is_scoped: bool,
) -> Result<(), ExperimentError> {
    match actor {
        Actor::Human { .. } => {
            // Must be a scoped Approver or Maintainer
            if (role == "approver" || role == "maintainer") && is_scoped {
                Ok(())
            } else {
                Err(ExperimentError::UnauthorizedApprover {
                    principal: format!("{actor:?}"),
                    role: role.to_string(),
                })
            }
        }
        _ => Err(ExperimentError::UnauthorizedApprover {
            principal: format!("{actor:?}"),
            role: role.to_string(),
        }),
    }
}

/// Verify an approval against current time and comparison digest.
pub fn verify_approval(
    approval: &QualityPromotionApproval,
    expected_digest: &[u8],
    now: DateTime<Utc>,
) -> Result<(), ExperimentError> {
    if now > approval.expires_at {
        return Err(ExperimentError::ApprovalExpired {
            expired_at: approval.expires_at,
        });
    }
    if approval.action_digest != expected_digest {
        return Err(ExperimentError::DigestMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(
        model_id: &str,
        succeeded: Option<bool>,
        cost_micro_usd: Option<u64>,
        latency_ms: Option<u64>,
    ) -> QualityObservation {
        QualityObservation::builder(
            "org-1",
            "repo-1",
            "small-bug-fix",
            model_id,
            "local-only",
            "internal",
        )
        .succeeded(succeeded)
        .cost_micro_usd(cost_micro_usd)
        .latency_ms(latency_ms)
        .escalated(Some(false))
        .build()
    }

    /// An arm whose executions had no price reports `None`, not `0.0`. A `0.0`
    /// mean cost undercuts every priced comparator, so the release gate would
    /// have read "cheapest arm" out of the absence of a price.
    #[test]
    fn an_unpriced_arm_reports_no_cost_rather_than_a_free_one() {
        let observations = vec![
            observation("local/qwen", Some(true), None, Some(900)),
            observation("local/qwen", Some(true), None, Some(1100)),
        ];
        let result = build_arm_result(RouteArm::LocalFirstRouter, &observations)
            .expect("two observations are an arm");
        assert_eq!(result.mean_cost_usd, None, "no price was measured");
        assert_eq!(result.mean_latency_ms, Some(1000.0));
        assert_eq!(result.task_success_rate, Some(1.0));
    }

    /// An execution whose outcome was never observed is outside the success
    /// population entirely — counting it as a failure would invent a regression
    /// and counting it as a success would invent evidence of quality.
    #[test]
    fn an_unobserved_outcome_is_neither_a_success_nor_a_failure() {
        let observations = vec![
            observation("hosted/strong", Some(true), Some(1_000), Some(100)),
            observation("hosted/strong", None, Some(1_000), Some(100)),
        ];
        let result = build_arm_result(RouteArm::Router, &observations).unwrap();
        assert_eq!(
            result.task_success_rate,
            Some(1.0),
            "1 success out of the 1 outcome that was measured, not out of 2"
        );
    }

    /// Neither of these is measured by a `QualityObservation`, so neither is
    /// reported. `unsafe_proposal_rate: 0.0` would assert that nothing unsafe
    /// was produced by a check that never ran.
    #[test]
    fn unmeasured_safety_dimensions_are_absent_not_clean() {
        let result = build_arm_result(
            RouteArm::Router,
            &[observation("hosted/strong", Some(true), Some(1), Some(1))],
        )
        .unwrap();
        assert_eq!(result.tool_call_error_rate, None);
        assert_eq!(result.unsafe_proposal_rate, None);
    }

    fn experiment_with(min_samples: u64) -> QualityExperiment {
        QualityExperiment::new(
            "org-1",
            ExperimentKind::Canary,
            "cand-1",
            "model-profile",
            "local/qwen",
            2,
            RouteArm::StaticStrongest,
            RouteArm::LocalFirstRouter,
            vec![7; 32],
            1_000,
            10_000,
            AnalysisPlan {
                min_samples,
                ..AnalysisPlan::default()
            },
        )
    }

    /// A comparison in which a limit could not be evaluated is
    /// `InsufficientEvidence`, never `Pass`. Fail closed: the candidate has not
    /// been shown to be within the limit, it has not been measured against it.
    #[test]
    fn an_unevaluable_limit_is_insufficient_evidence_not_a_pass() {
        let experiment = experiment_with(2);
        // Both sides measured success and latency; NEITHER measured cost, so
        // the cost limit has nothing to check.
        let baseline = vec![
            observation("hosted/strong", Some(true), None, Some(100)),
            observation("hosted/strong", Some(true), None, Some(100)),
        ];
        let candidate = vec![
            observation("local/qwen", Some(true), None, Some(100)),
            observation("local/qwen", Some(true), None, Some(100)),
        ];
        let comparison = evaluate_experiment(&experiment, &baseline, &candidate);
        assert_eq!(comparison.verdict, ComparisonVerdict::InsufficientEvidence);
        assert!(comparison
            .missing_measurements
            .contains(&"cost".to_string()));
    }

    /// The measured, fully-evidenced case still passes — the honesty rules
    /// above must not have turned every comparison into a refusal.
    #[test]
    fn a_fully_measured_non_inferior_candidate_passes() {
        let experiment = experiment_with(2);
        let baseline = vec![
            observation("hosted/strong", Some(true), Some(5_000), Some(100)),
            observation("hosted/strong", Some(true), Some(5_000), Some(100)),
        ];
        let candidate = vec![
            observation("local/qwen", Some(true), Some(1_000), Some(100)),
            observation("local/qwen", Some(true), Some(1_000), Some(100)),
        ];
        let comparison = evaluate_experiment(&experiment, &baseline, &candidate);
        assert_eq!(comparison.verdict, ComparisonVerdict::Pass);
        assert_eq!(comparison.quality_non_inferior, Some(true));
        assert_eq!(comparison.cost_within_limit, Some(true));
        assert!(comparison.missing_measurements.is_empty());
    }

    /// Assignment is a pure function of (seed, key): the same pair lands in the
    /// same arm on every replay, in this process and any other.
    #[test]
    fn assignment_is_deterministic_for_a_seed_and_key() {
        let experiment = experiment_with(100);
        let first = experiment.assign_arm("repo-1/run-42");
        for _ in 0..8 {
            assert_eq!(experiment.assign_arm("repo-1/run-42"), first);
        }
    }
}
