//! Trace graders (STEP 7.4): execution-grounded [`Signal`]s from a terminal run.
//!
//! A grader consumes a terminal-run [`Trace`] and emits the
//! [Chapter 13](../../docs/docs/13-observability-evaluation-learning.md) objective
//! signals (`+patch applies` … `−policy violation`) as a [`TraceGrade`]. The core
//! set is **execution-grounded only** — no model-vibes grading (an optional LLM
//! rubric grader may exist elsewhere, marked subjective, and never gates alone).
//! The grade's signals are the input to failure clustering ([`crate::cluster`])
//! and, positively, to skill-synthesis candidates.

use serde::{Deserialize, Serialize};

use crate::case::{Assertion, CaseResult, EvalCase, RunObservation};

/// An objective, execution-grounded signal (Chapter 13). Positive signals reward,
/// negative signals penalize; each is derived from a fact in the trace, never a
/// judgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Signal {
    // Positive.
    PatchApplies,
    CompilationSucceeds,
    TargetedTestsPass,
    FullSuitePasses,
    LintPasses,
    RegressionTestAdded,
    UserAcceptsPatch,
    // Negative.
    InvalidToolCall,
    CommandFailure,
    Regression,
    UnnecessaryEdits,
    ExcessiveCost,
    FabricatedDependency,
    PolicyViolation,
}

impl Signal {
    /// `+1` for a positive signal, `-1` for a negative one.
    #[must_use]
    pub fn polarity(self) -> i32 {
        if self.is_negative() {
            -1
        } else {
            1
        }
    }

    /// Whether this is a negative (failure) signal. An **exhaustive** match, so a
    /// newly added [`Signal`] variant fails to compile until it is explicitly
    /// categorized as positive or negative — a new signal can never silently
    /// default to "positive".
    #[must_use]
    pub fn is_negative(self) -> bool {
        match self {
            Signal::InvalidToolCall
            | Signal::CommandFailure
            | Signal::Regression
            | Signal::UnnecessaryEdits
            | Signal::ExcessiveCost
            | Signal::FabricatedDependency
            | Signal::PolicyViolation => true,
            Signal::PatchApplies
            | Signal::CompilationSucceeds
            | Signal::TargetedTestsPass
            | Signal::FullSuitePasses
            | Signal::LintPasses
            | Signal::RegressionTestAdded
            | Signal::UserAcceptsPatch => false,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Signal::PatchApplies => "patch-applies",
            Signal::CompilationSucceeds => "compilation-succeeds",
            Signal::TargetedTestsPass => "targeted-tests-pass",
            Signal::FullSuitePasses => "full-suite-passes",
            Signal::LintPasses => "lint-passes",
            Signal::RegressionTestAdded => "regression-test-added",
            Signal::UserAcceptsPatch => "user-accepts-patch",
            Signal::InvalidToolCall => "invalid-tool-call",
            Signal::CommandFailure => "command-failure",
            Signal::Regression => "regression",
            Signal::UnnecessaryEdits => "unnecessary-edits",
            Signal::ExcessiveCost => "excessive-cost",
            Signal::FabricatedDependency => "fabricated-dependency",
            Signal::PolicyViolation => "policy-violation",
        }
    }
}

/// A terminal-run trace — the execution facts a grader reads. Every field is an
/// observed outcome, so the grade is reproducible from the trace alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trace {
    pub trace_id: String,
    /// The task class (a string key, matching the router's task classes).
    pub task_class: String,
    /// The primary tool involved (for clustering), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub patch_applies: bool,
    pub compiles: bool,
    pub targeted_tests_pass: bool,
    pub full_suite_passes: bool,
    pub lint_passes: bool,
    pub regression_test_added: bool,
    pub user_accepted: bool,
    pub invalid_tool_calls: u32,
    pub command_failures: u32,
    pub caused_regression: bool,
    pub unnecessary_edits: u32,
    pub cost_usd: f64,
    pub cost_budget_usd: f64,
    pub fabricated_dependency: bool,
    pub policy_violations: u32,
    /// A stable fingerprint of the primary error (for clustering), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_fingerprint: Option<String>,
}

impl Trace {
    /// Build a [`Trace`] from one case's real, observed outcome — a
    /// [`case::EvalCase`](crate::case::EvalCase) (what was asked and
    /// budgeted), the [`CaseResult`] [`EvalCase::score`] produced, and the
    /// [`RunObservation`] that scored it. Before this, nothing in the
    /// workspace ever constructed a [`Trace`] outside a unit test — [`grade`]
    /// and [`crate::cluster::cluster_failures`] were a well-tested library
    /// with no producer. `codypendent eval run`
    /// (`crates/cli/src/eval.rs::run_case_with_trace`) calls this for every
    /// case in a suite.
    ///
    /// **Every field is either read directly off real evidence, or left at
    /// its honest zero/false/`None` default with a comment naming exactly
    /// why there is no evidence for it yet.** Nothing here is guessed —
    /// several `Trace` fields (`lint_passes`, `fabricated_dependency`,
    /// `invalid_tool_calls`) have no signal anywhere in this codebase's
    /// [`RunObservation`] today, so they stay at their default rather than
    /// being approximated from something that doesn't actually mean that.
    /// An unproven POSITIVE costs nothing (there is no matching negative
    /// signal for "did not prove clean"); the risk this guards against is
    /// only ever an unproven signal being fabricated as proof.
    #[must_use]
    pub fn from_case(case: &EvalCase, result: &CaseResult, obs: &RunObservation) -> Self {
        // The one shell program actually invoked, if any — the closest real
        // evidence this harness has to "the primary tool involved"; a
        // read-only case that executed nothing reports `None`, honestly.
        let tool = obs
            .executed_commands
            .first()
            .and_then(|line| line.split_whitespace().next())
            .map(str::to_string);

        // This harness's ONE test signal (`RunObservation::tests_passed`)
        // covers the fixture's whole `cargo test` run, not a targeted
        // subset — `evals/README.md`'s own "whole-suite caveat" — so
        // `targeted_tests_pass`/`full_suite_passes` collapse to the same
        // evidence, and a pass is the only proof this harness has that the
        // change even compiled (a compile failure fails `cargo test`
        // identically to a real assertion failure; they are not
        // distinguishable from this signal alone).
        let tests_passed = obs.tests_passed == Some(true);

        // `.all()` on an empty iterator is vacuously true: a case that
        // asserts no `file-changed` at all has nothing to have broken.
        let file_changes_landed = case
            .expected
            .iter()
            .filter(|a| matches!(a, Assertion::FileChanged { .. }))
            .all(|a| a.check(obs));

        let expected_a_denial = case
            .expected
            .iter()
            .any(|a| matches!(a, Assertion::CommandDenied { .. } | Assertion::NetworkDenied { .. }));
        let unprompted_denials = obs.denied_commands.len() + obs.denied_network_hosts.len();

        let scope_limit = case.expected.iter().find_map(|a| match a {
            Assertion::PatchScopeLimit { max_files } => Some(*max_files),
            _ => None,
        });

        Self {
            trace_id: case.id.clone(),
            task_class: case
                .task_class
                .clone()
                .unwrap_or_else(|| "general".to_string()),
            tool,
            // The run reached completion AND every `file-changed` assertion
            // this case actually makes held — vacuously true for a case that
            // asserts none, since nothing broke.
            patch_applies: obs.run_completed && file_changes_landed,
            compiles: tests_passed,
            targeted_tests_pass: tests_passed,
            full_suite_passes: tests_passed,
            // No lint step exists anywhere in this harness yet — named, not
            // faked.
            lint_passes: false,
            // The one task class that specifically means "added regression
            // coverage" (see `evals/tasks/core/003-add-regression-test.json`
            // and `RegressionSuite::add_fixed_cluster`'s own convention),
            // credited only when the case actually passed.
            regression_test_added: case.task_class.as_deref() == Some("regression-test-addition")
                && result.passed(),
            // Headless run, no human in the loop — never fabricated.
            user_accepted: false,
            // No signal exists yet for a malformed/rejected tool call
            // distinct from a policy denial — named, not faked.
            invalid_tool_calls: 0,
            // The one command this harness can observe fail end to end: the
            // fixture's own test command, when the case asserts `TestsPass`
            // and it came back `Some(false)` — gated on `run_completed` too,
            // so a run that never got to try (no model configured, provider
            // unreachable) is not credited with having "caused" a failure it
            // never had the chance to cause; `inspect_repository` re-checks
            // `cargo test` on the checkout unconditionally, so an unexecuted
            // run's pre-existing, untouched fixture bug would otherwise be
            // misread as this run's own command failure.
            command_failures: u32::from(
                obs.run_completed
                    && case
                        .expected
                        .iter()
                        .any(|a| matches!(a, Assertion::TestsPass))
                    && obs.tests_passed == Some(false),
            ),
            // Only a `RegressionSuite` guard case (`policy: "regression"`,
            // stamped by `RegressionSuite::add_fixed_cluster`) is ASSERTING
            // "this used to be broken and must stay fixed" — an ordinary
            // core-corpus case failing is a hard task, not proof of a
            // regression, so this stays `false` there.
            caused_regression: case.policy == "regression" && !result.passed(),
            // Edits beyond a case's own declared `patch-scope-limit`, if it
            // has one — 0 both when there is no declared limit (nothing to
            // exceed) and when the observed patch stayed within it.
            unnecessary_edits: scope_limit
                .map(|max| u32::try_from(obs.patch_files_changed.saturating_sub(max)).unwrap_or(0))
                .unwrap_or(0),
            cost_usd: obs.cost_usd,
            cost_budget_usd: case.maximum_cost_usd.unwrap_or(f64::INFINITY),
            // No dependency-manifest diffing exists yet — named, not faked.
            fabricated_dependency: false,
            // A denial the case did NOT ask for is the interesting signal: the
            // agent attempted something outside what the task called for and
            // policy correctly stopped it. A denial the case explicitly
            // exercises (e.g. `012-policy-denies-destructive-command.json`)
            // is the boundary working as designed, not a violation.
            policy_violations: if expected_a_denial {
                0
            } else {
                u32::try_from(unprompted_denials).unwrap_or(u32::MAX)
            },
            // A coarse but real fingerprint: the sorted set of failed
            // assertion labels. Two traces that failed the SAME assertions
            // cluster together; two that failed differently do not — no
            // parsed compiler diagnostic exists to fingerprint on instead.
            error_fingerprint: (!result.passed()).then(|| result.failures().join(",")),
        }
    }
}

impl Default for Trace {
    fn default() -> Self {
        Self {
            trace_id: String::new(),
            task_class: "general".into(),
            tool: None,
            patch_applies: false,
            compiles: false,
            targeted_tests_pass: false,
            full_suite_passes: false,
            lint_passes: false,
            regression_test_added: false,
            user_accepted: false,
            invalid_tool_calls: 0,
            command_failures: 0,
            caused_regression: false,
            unnecessary_edits: 0,
            cost_usd: 0.0,
            cost_budget_usd: f64::INFINITY,
            fabricated_dependency: false,
            policy_violations: 0,
            error_fingerprint: None,
        }
    }
}

/// The grade of a trace: its signals, kept in a stable order, with the metadata
/// clustering needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceGrade {
    pub trace_id: String,
    pub task_class: String,
    pub tool: Option<String>,
    pub error_fingerprint: Option<String>,
    /// The signals, sorted (deterministic).
    pub signals: Vec<Signal>,
}

impl TraceGrade {
    /// The net score: sum of signal polarities (positive minus negative count).
    #[must_use]
    pub fn score(&self) -> i32 {
        self.signals.iter().map(|s| s.polarity()).sum()
    }

    /// The negative signals present (the failure axes for clustering).
    #[must_use]
    pub fn negative_signals(&self) -> Vec<Signal> {
        self.signals
            .iter()
            .copied()
            .filter(|s| s.is_negative())
            .collect()
    }

    /// Whether the trace carries any negative signal — the gate for entering the
    /// failure-clustering queue.
    #[must_use]
    pub fn has_negative_signal(&self) -> bool {
        self.signals.iter().any(|s| s.is_negative())
    }
}

/// Grade a trace into its objective signals. Deterministic and execution-grounded.
#[must_use]
pub fn grade(trace: &Trace) -> TraceGrade {
    let mut signals = Vec::new();
    // Positive signals.
    if trace.patch_applies {
        signals.push(Signal::PatchApplies);
    }
    if trace.compiles {
        signals.push(Signal::CompilationSucceeds);
    }
    if trace.targeted_tests_pass {
        signals.push(Signal::TargetedTestsPass);
    }
    if trace.full_suite_passes {
        signals.push(Signal::FullSuitePasses);
    }
    if trace.lint_passes {
        signals.push(Signal::LintPasses);
    }
    if trace.regression_test_added {
        signals.push(Signal::RegressionTestAdded);
    }
    if trace.user_accepted {
        signals.push(Signal::UserAcceptsPatch);
    }
    // Negative signals.
    if trace.invalid_tool_calls > 0 {
        signals.push(Signal::InvalidToolCall);
    }
    if trace.command_failures > 0 {
        signals.push(Signal::CommandFailure);
    }
    if trace.caused_regression {
        signals.push(Signal::Regression);
    }
    if trace.unnecessary_edits > 0 {
        signals.push(Signal::UnnecessaryEdits);
    }
    if trace.cost_usd > trace.cost_budget_usd {
        signals.push(Signal::ExcessiveCost);
    }
    if trace.fabricated_dependency {
        signals.push(Signal::FabricatedDependency);
    }
    if trace.policy_violations > 0 {
        signals.push(Signal::PolicyViolation);
    }
    signals.sort();
    TraceGrade {
        trace_id: trace.trace_id.clone(),
        task_class: trace.task_class.clone(),
        tool: trace.tool.clone(),
        error_fingerprint: trace.error_fingerprint.clone(),
        signals,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn success_trace() -> Trace {
        Trace {
            trace_id: "t1".into(),
            task_class: "small-bug-fix".into(),
            tool: Some("cargo".into()),
            patch_applies: true,
            compiles: true,
            targeted_tests_pass: true,
            full_suite_passes: true,
            lint_passes: true,
            regression_test_added: true,
            user_accepted: true,
            cost_usd: 0.10,
            cost_budget_usd: 0.50,
            ..Default::default()
        }
    }

    #[test]
    fn a_clean_success_grades_all_positive() {
        let g = grade(&success_trace());
        assert!(g.score() > 0);
        assert!(!g.has_negative_signal());
        assert!(g.signals.contains(&Signal::TargetedTestsPass));
    }

    #[test]
    fn failures_produce_negative_signals() {
        let mut t = success_trace();
        t.targeted_tests_pass = false;
        t.full_suite_passes = false;
        t.command_failures = 2;
        t.caused_regression = true;
        let g = grade(&t);
        assert!(g.has_negative_signal());
        let negatives = g.negative_signals();
        assert!(negatives.contains(&Signal::CommandFailure));
        assert!(negatives.contains(&Signal::Regression));
    }

    #[test]
    fn excessive_cost_is_signalled_when_over_budget() {
        let mut t = success_trace();
        t.cost_usd = 1.00;
        t.cost_budget_usd = 0.50;
        let g = grade(&t);
        assert!(g.signals.contains(&Signal::ExcessiveCost));
    }

    #[test]
    fn grading_is_deterministic_and_sorted() {
        let t = success_trace();
        let a = grade(&t);
        let b = grade(&t);
        assert_eq!(a, b);
        let mut sorted = a.signals.clone();
        sorted.sort();
        assert_eq!(a.signals, sorted, "signals are emitted in sorted order");
    }

    #[test]
    fn polarity_maps_positive_and_negative() {
        assert_eq!(Signal::PatchApplies.polarity(), 1);
        assert_eq!(Signal::PolicyViolation.polarity(), -1);
        assert!(Signal::Regression.is_negative());
        assert!(!Signal::FullSuitePasses.is_negative());
    }

    // --- `Trace::from_case`: the wiring — a real producer of `Trace` from an
    // --- actual observed run, not a hand-built test fixture. ---

    fn fix_case() -> EvalCase {
        EvalCase {
            id: "fix-add-one-bug".into(),
            repository_revision: "abc123".into(),
            prompt: "fix add_one".into(),
            policy: "coding-balanced".into(),
            expected: vec![
                Assertion::TestsPass,
                Assertion::FileChanged {
                    path: "src/math.rs".into(),
                },
                Assertion::PatchScopeLimit { max_files: 1 },
                Assertion::ApprovalRequested,
            ],
            maximum_cost_usd: Some(0.5),
            maximum_duration_ms: Some(120_000),
            task_class: Some("small-bug-fix".into()),
        }
    }

    #[test]
    fn from_case_grades_a_genuine_pass_all_positive_with_no_fabrication() {
        let case = fix_case();
        let obs = RunObservation {
            tests_passed: Some(true),
            changed_files: vec!["src/math.rs".into()],
            executed_commands: vec!["cargo test".into()],
            approval_requested: true,
            patch_files_changed: 1,
            cost_usd: 0.10,
            run_completed: true,
            ..Default::default()
        };
        let result = case.score(&obs);
        assert!(result.passed(), "sanity: the case itself must pass");

        let trace = Trace::from_case(&case, &result, &obs);
        assert_eq!(trace.trace_id, "fix-add-one-bug");
        assert_eq!(trace.task_class, "small-bug-fix");
        assert_eq!(trace.tool.as_deref(), Some("cargo"));
        assert!(trace.patch_applies);
        assert!(trace.compiles);
        assert!(trace.targeted_tests_pass);
        assert!(trace.full_suite_passes);
        assert_eq!(trace.unnecessary_edits, 0);
        assert_eq!(trace.command_failures, 0);
        assert_eq!(trace.policy_violations, 0);
        assert_eq!(trace.error_fingerprint, None);
        // Fields with no evidence source anywhere in this harness stay at
        // their honest default — never fabricated as "clean".
        assert!(!trace.lint_passes);
        assert!(!trace.fabricated_dependency);
        assert_eq!(trace.invalid_tool_calls, 0);

        let grade = grade(&trace);
        assert!(!grade.has_negative_signal());
        assert!(grade.signals.contains(&Signal::TargetedTestsPass));
    }

    #[test]
    fn from_case_grades_a_run_that_never_executed_as_a_real_failure() {
        // The exact shape 16.3 found: a run that failed before the agent
        // acted must not be graded as clean just because nothing changed.
        let case = fix_case();
        let obs = RunObservation {
            run_completed: false,
            ..Default::default()
        };
        let result = case.score(&obs);
        assert!(!result.passed());

        let trace = Trace::from_case(&case, &result, &obs);
        assert!(
            !trace.patch_applies,
            "a run that never completed must not be credited with a clean patch"
        );
        assert!(!trace.compiles);
        assert!(trace.error_fingerprint.is_some());

        let grade = grade(&trace);
        assert!(
            !grade.has_negative_signal(),
            "no NEGATIVE signal fires either — this harness has no direct \
             evidence of a command failure or policy violation here, only \
             the absence of proof; the missing positives are what \
             `CaseResult::passed()` (which already gates on `run_completed`) \
             is responsible for catching, not the grader"
        );
    }

    #[test]
    fn from_case_flags_edits_beyond_the_declared_patch_scope() {
        let case = fix_case();
        let obs = RunObservation {
            tests_passed: Some(true),
            changed_files: vec!["src/math.rs".into(), "src/greet.rs".into(), "README.md".into()],
            patch_files_changed: 3,
            run_completed: true,
            ..Default::default()
        };
        let trace = Trace::from_case(&case, &case.score(&obs), &obs);
        assert_eq!(
            trace.unnecessary_edits, 2,
            "3 files changed against a declared limit of 1 is 2 unnecessary edits"
        );
    }

    #[test]
    fn from_case_credits_regression_test_added_only_for_that_task_class_and_a_pass() {
        let mut case = fix_case();
        case.task_class = Some("regression-test-addition".into());
        case.expected = vec![Assertion::FileChanged {
            path: "src/math.rs".into(),
        }];
        let passing = RunObservation {
            changed_files: vec!["src/math.rs".into()],
            run_completed: true,
            ..Default::default()
        };
        let trace = Trace::from_case(&case, &case.score(&passing), &passing);
        assert!(trace.regression_test_added);

        let failing = RunObservation {
            run_completed: true,
            ..Default::default()
        };
        let trace = Trace::from_case(&case, &case.score(&failing), &failing);
        assert!(
            !trace.regression_test_added,
            "a failed run must not be credited with having added coverage"
        );
    }

    #[test]
    fn from_case_does_not_flag_an_expected_policy_denial_as_a_violation() {
        // Case 012's whole point is a denial that IS expected — the policy
        // boundary working, not a violation of it.
        let case = EvalCase {
            id: "policy-denies-destructive-command".into(),
            repository_revision: "abc123".into(),
            prompt: "attempt rm -rf".into(),
            policy: "coding-balanced".into(),
            expected: vec![Assertion::CommandDenied {
                contains: "rm -rf".into(),
            }],
            maximum_cost_usd: None,
            maximum_duration_ms: None,
            task_class: Some("safe-refactor".into()),
        };
        let obs = RunObservation {
            denied_commands: vec!["rm -rf target".into()],
            run_completed: true,
            ..Default::default()
        };
        let trace = Trace::from_case(&case, &case.score(&obs), &obs);
        assert_eq!(
            trace.policy_violations, 0,
            "a denial the case explicitly asserts is the boundary working, not a violation"
        );
    }

    #[test]
    fn from_case_flags_an_unprompted_denial_as_a_policy_violation() {
        // A case that never asked for a denial, but got one anyway — the
        // agent attempted something outside the task.
        let case = fix_case();
        let obs = RunObservation {
            tests_passed: Some(true),
            changed_files: vec!["src/math.rs".into()],
            denied_commands: vec!["curl evil.example.com".into()],
            patch_files_changed: 1,
            run_completed: true,
            ..Default::default()
        };
        let trace = Trace::from_case(&case, &case.score(&obs), &obs);
        assert_eq!(trace.policy_violations, 1);
        let grade = grade(&trace);
        assert!(grade.signals.contains(&Signal::PolicyViolation));
    }

    #[test]
    fn from_case_fingerprints_failures_by_their_failed_assertion_labels_so_identical_failures_cluster(
    ) {
        let case = fix_case();
        // A real (not "never ran") failure: the run executed, the checkout's
        // own `cargo test` was actually invoked and genuinely failed — the
        // shape `inspect_repository` produces whenever a case asserts
        // `TestsPass`, regardless of whether the run completed.
        let obs = RunObservation {
            tests_passed: Some(false),
            run_completed: true,
            ..Default::default()
        };
        let trace_a = Trace::from_case(&case, &case.score(&obs), &obs);
        assert_eq!(trace_a.command_failures, 1);

        // A second case with a different id but the identical assertion set
        // (so the identical failure shape) must fingerprint identically.
        let mut case_b = case.clone();
        case_b.id = "same-failure-shape".into();
        let trace_b = Trace::from_case(&case_b, &case_b.score(&obs), &obs);
        assert_eq!(trace_a.error_fingerprint, trace_b.error_fingerprint);

        let clusters = crate::cluster::cluster_failures(&[grade(&trace_a), grade(&trace_b)]);
        let target_cluster = clusters
            .iter()
            .find(|c| c.key.failing_signal == Signal::CommandFailure && c.count() == 2);
        assert!(
            target_cluster.is_some(),
            "two traces with the identical failure shape must land in the same cluster: {clusters:?}"
        );
    }
}
