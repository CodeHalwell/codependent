//! codypendent-eval — the evaluation and learning loop (Phase 7, STEP 7.1/7.4/7.5).
//!
//! The closed loop that lets Codypendent improve without letting anything improve
//! *itself*:
//!
//! * [`case`] — the [`EvalCase`](case::EvalCase)/[`Assertion`](case::Assertion)
//!   harness (STEP 7.1): objective assertions scored against a
//!   [`RunObservation`](case::RunObservation) of what a headless run actually did.
//! * [`grade`] — execution-grounded [`Signal`](grade::Signal)s and the
//!   [`TraceGrade`](grade::TraceGrade) a [`grade`](grade::grade) produces from a
//!   terminal-run [`Trace`](grade::Trace) (STEP 7.4). No model-vibes grading.
//! * [`cluster`] — [`FailureCluster`](cluster::FailureCluster)ing of negative-signal
//!   traces into the improvement queue (STEP 7.4), deterministic.
//! * [`regression`] — the [`RegressionSuite`](regression::RegressionSuite) that
//!   grows with every fixed failure and gates promotion (STEP 7.4/7.5).
//! * [`promote`] — the [`Candidate`](promote::Candidate) promotion pipeline
//!   (STEP 7.5): draft → regression → shadow → canary → **human approval** →
//!   promote → rollback. **No self-promotion**: only an
//!   [`Actor::Human`](codypendent_protocol::events::Actor) can approve, enforced in
//!   the state machine (ADR-010, exit criterion 2).
//! * [`store`] — [`PromotionStore`](store::PromotionStore): durable persistence
//!   for `promote`'s state machine (STEP 7.5 daemon wiring), over a SQLite pool
//!   (migration `0015_promotion.sql`). Adds `sqlx` to this crate but keeps it
//!   "daemon-free" in the sense the rest of this module doc uses the phrase: no
//!   dependency on `codypendent-daemon` (mirrors `codypendent-workflow`'s own
//!   store), and the same no-self-promotion property, since the store only ever
//!   persists the RESULT of a real state-machine transition.
//!
//! The `case`/`cluster`/`grade`/`regression`/`promote` state machines are pure
//! and daemon-free, so the gate — *nothing promotes itself* — is proven in
//! isolation; `store` is this crate's one persistence seam.

pub mod case;
pub mod cluster;
pub mod db;
pub mod experiment;
pub mod grade;
pub mod observation;
pub mod promote;
pub mod regression;
pub mod store;

pub use case::{Assertion, AssertionResult, CaseResult, EvalCase, RunObservation, SuiteReport};
pub use cluster::{cluster_failures, rank_by_frequency, ClusterKey, FailureCluster};
pub use experiment::{
    build_arm_result, compute_action_digest, detect_drift, evaluate_experiment,
    validate_promotion_actor, verify_approval, AnalysisPlan, AssignedArm, ComparisonVerdict,
    ExperimentError, ExperimentKind, ExperimentSample, ExperimentState, HorizonKind,
    QualityComparison, QualityDriftAlert, QualityExperiment, QualityPromotionApproval,
};
pub use grade::{grade, Signal, Trace, TraceGrade};
pub use observation::{
    capture_observation, ObservationError, QualityAggregate, QualityObservation,
    QualityObservationBuilder,
};
pub use promote::{
    ActiveVersions, ArtifactKind, ArtifactVersion, CanaryOutcome, Candidate, PromotionError,
    PromotionRecord, PromotionStage, MIN_CANARY_SAMPLES,
};
pub use regression::{RegressionReport, RegressionSuite};
pub use store::{CandidateSnapshot, PromotionStore, PromotionStoreError};
