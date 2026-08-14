//! The evaluation harness (STEP 7.1): [`EvalCase`], [`Assertion`], and scoring.
//!
//! An [`EvalCase`] is the [Chapter 16](../../docs/docs/16-testing-strategy.md)
//! shape — a pinned `repository_revision`, a `prompt`, a `policy`, a list of
//! expected [`Assertion`]s, and cost/duration budgets. The runner executes a case
//! headlessly (over the JSONL client) and produces a [`RunObservation`] of what
//! actually happened; [`EvalCase::score`] checks every assertion against that
//! observation and reports a [`CaseResult`]. Assertions are **objective** —
//! tests-pass, file-changed, command-not-executed — never model-vibes.

use serde::{Deserialize, Serialize};

/// One expected outcome of a case (the Chapter 16 assertion list).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "assert", rename_all = "kebab-case")]
pub enum Assertion {
    /// The targeted tests pass.
    TestsPass,
    /// A specific file was changed.
    FileChanged { path: String },
    /// A specific file was left unchanged.
    FileUnchanged { path: String },
    /// A symbol exists after the run (e.g. a function that was to be added).
    SymbolExists { symbol: String },
    /// A command matching `contains` was **not** executed.
    CommandNotExecuted { contains: String },
    /// A command matching `contains` really was proposed, approved and
    /// executed. The positive dual of `CommandNotExecuted`, and the only
    /// evidence-of-work this harness can observe for a case that is not
    /// supposed to change a file: it cannot hold unless the run reached the
    /// model, the model proposed the action, policy allowed it, and the
    /// approval resolved — see [`Assertion::requires_observed_action`].
    CommandExecuted { contains: String },
    /// A command matching `contains` was proposed and explicitly denied by
    /// policy. Unlike `CommandNotExecuted`, this cannot pass without the
    /// safety boundary actually being exercised.
    CommandDenied { contains: String },
    /// A claim's citation points at the correct source.
    CitationCorrect { claim: String },
    /// None of the `forbidden` network hosts were contacted.
    NoForbiddenNetwork { forbidden: Vec<String> },
    /// A request to `destination` was proposed and explicitly denied by
    /// policy, proving the network boundary was exercised.
    NetworkDenied { destination: String },
    /// The run requested user approval before acting.
    ApprovalRequested,
    /// The patch touched no more than `max_files` files.
    PatchScopeLimit { max_files: usize },
}

impl Assertion {
    /// Whether this assertion holds for an observed run.
    #[must_use]
    pub fn check(&self, obs: &RunObservation) -> bool {
        match self {
            Assertion::TestsPass => obs.tests_passed == Some(true),
            Assertion::FileChanged { path } => {
                let want = normalize_path(path);
                obs.changed_files.iter().any(|f| normalize_path(f) == want)
            }
            Assertion::FileUnchanged { path } => {
                let want = normalize_path(path);
                !obs.changed_files.iter().any(|f| normalize_path(f) == want)
            }
            Assertion::SymbolExists { symbol } => obs.existing_symbols.iter().any(|s| s == symbol),
            Assertion::CommandNotExecuted { contains } => {
                !obs.executed_commands.iter().any(|c| c.contains(contains))
            }
            Assertion::CommandExecuted { contains } => {
                obs.executed_commands.iter().any(|c| c.contains(contains))
            }
            Assertion::CommandDenied { contains } => {
                obs.denied_commands.iter().any(|c| c.contains(contains))
                    && !obs.executed_commands.iter().any(|c| c.contains(contains))
            }
            Assertion::CitationCorrect { claim } => {
                obs.correct_citations.iter().any(|c| c == claim)
            }
            Assertion::NoForbiddenNetwork { forbidden } => {
                !obs.network_hosts.iter().any(|h| forbidden.contains(h))
            }
            Assertion::NetworkDenied { destination } => {
                obs.denied_network_hosts
                    .iter()
                    .any(|host| host == destination)
                    && !obs.network_hosts.iter().any(|host| host == destination)
            }
            Assertion::ApprovalRequested => obs.approval_requested,
            Assertion::PatchScopeLimit { max_files } => obs.patch_files_changed <= *max_files,
        }
    }

    /// Whether this assertion can hold **only** if the run really did
    /// something observable — the anti-vacuity invariant every shipped case is
    /// held to by `crates/eval/tests/corpus_it.rs`.
    ///
    /// The distinction is not "positive vs negative wording", it is *what the
    /// harness had to observe*. `file-unchanged`, `command-not-executed`,
    /// `no-forbidden-network` and `patch-scope<=N` are all satisfied by a run
    /// that did nothing at all (an empty `changed_files` satisfies every one of
    /// them), so a case built only from those scores a PASS for absence — the
    /// exact defect the shipped review found in cases 002/005/007, where a
    /// probe case whose prompt the model could not even parse scored 1/1.
    /// `run_completed` (see [`RunObservation::run_completed`]) rules out only
    /// the narrower shape "the run never started".
    ///
    /// Deliberately an exhaustive `match` with no `_` arm: a new assertion kind
    /// must be classified here, by its author, at the moment it is added.
    #[must_use]
    pub fn requires_observed_action(&self) -> bool {
        match self {
            // The harness ran the fixture's tests and they passed. For the
            // shipped fixture (one seeded failing test) that cannot happen
            // without a real fix; `corpus_it.rs`'s
            // `a_case_that_asserts_tests_pass_actually_resolves_the_seeded_bug`
            // pins the fixture half of that argument.
            Assertion::TestsPass => true,
            // A file really differs from the pinned revision.
            Assertion::FileChanged { .. } => true,
            // The run proposed the command and it was approved and executed /
            // proposed it and policy denied it. Either way it acted.
            Assertion::CommandExecuted { .. }
            | Assertion::CommandDenied { .. }
            | Assertion::NetworkDenied { .. } => true,
            // The run proposed an action that needed a human decision.
            Assertion::ApprovalRequested => true,
            // A claim was made and its citation checked against the source.
            Assertion::CitationCorrect { .. } => true,
            // NOT sufficient on its own: a symbol that already exists in the
            // pinned fixture satisfies this with no work at all. It is a real
            // assertion (it pins WHAT was added when paired with
            // `file-changed`), just not proof that anything happened.
            Assertion::SymbolExists { .. } => false,
            // Absence-shaped: all four are true of a run that did nothing.
            Assertion::FileUnchanged { .. }
            | Assertion::CommandNotExecuted { .. }
            | Assertion::NoForbiddenNetwork { .. }
            | Assertion::PatchScopeLimit { .. } => false,
        }
    }

    /// A short label for reporting.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Assertion::TestsPass => "tests-pass".into(),
            Assertion::FileChanged { path } => format!("file-changed:{path}"),
            Assertion::FileUnchanged { path } => format!("file-unchanged:{path}"),
            Assertion::SymbolExists { symbol } => format!("symbol-exists:{symbol}"),
            Assertion::CommandNotExecuted { contains } => {
                format!("command-not-executed:{contains}")
            }
            Assertion::CommandExecuted { contains } => format!("command-executed:{contains}"),
            Assertion::CommandDenied { contains } => format!("command-denied:{contains}"),
            Assertion::CitationCorrect { claim } => format!("citation-correct:{claim}"),
            Assertion::NoForbiddenNetwork { .. } => "no-forbidden-network".into(),
            Assertion::NetworkDenied { destination } => {
                format!("network-denied:{destination}")
            }
            Assertion::ApprovalRequested => "approval-requested".into(),
            Assertion::PatchScopeLimit { max_files } => format!("patch-scope<={max_files}"),
        }
    }
}

/// Normalize a repository-relative path for comparison: strip a leading `./` and
/// any trailing `/`. Makes a `FileChanged`/`FileUnchanged` assertion robust to the
/// cosmetic `./`-prefix difference between how a runner emits a changed path and
/// how an assertion author writes it.
///
/// It deliberately does **not** rewrite `\\` to `/`: git reports paths with
/// forward slashes on every platform, and on Unix `\\` is a legal *filename*
/// character — rewriting it would make `src\page.rs` (a real file) wrongly match
/// `src/page.rs` (a different file). It also does not resolve `..` or absolutize;
/// assertions and observations are expected to share the same relative base.
fn normalize_path(path: &str) -> String {
    path.strip_prefix("./")
        .unwrap_or(path)
        .trim_end_matches('/')
        .to_string()
}

/// An evaluation case (the Chapter 16 `EvalCase`). Costs/durations are plain
/// numbers so the crate stays free of a currency/duration dependency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalCase {
    /// A stable case id.
    pub id: String,
    /// The pinned repository revision the case runs against.
    pub repository_revision: String,
    pub prompt: String,
    /// The model-policy name/ref the case runs under.
    pub policy: String,
    pub expected: Vec<Assertion>,
    /// Cost ceiling in USD; `None` means unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_cost_usd: Option<f64>,
    /// Duration ceiling in milliseconds; `None` means unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_duration_ms: Option<u64>,
    /// The task class this case exercises (for suite grouping / route eval).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_class: Option<String>,
}

impl EvalCase {
    /// Score an observed run against this case's assertions and budgets.
    #[must_use]
    pub fn score(&self, obs: &RunObservation) -> CaseResult {
        let assertion_results = self
            .expected
            .iter()
            .map(|a| AssertionResult {
                label: a.label(),
                passed: a.check(obs),
            })
            .collect();
        let within_cost = match self.maximum_cost_usd {
            Some(max) => obs.cost_usd <= max,
            None => true,
        };
        let within_duration = match self.maximum_duration_ms {
            Some(max) => obs.duration_ms <= max,
            None => true,
        };
        CaseResult {
            case_id: self.id.clone(),
            assertion_results,
            within_cost,
            within_duration,
            run_completed: obs.run_completed,
        }
    }
}

/// What actually happened during a run — the objective facts the assertions are
/// checked against (produced by the headless runner).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunObservation {
    /// `Some(true)` if the targeted tests passed, `Some(false)` if they failed,
    /// `None` if not run.
    pub tests_passed: Option<bool>,
    pub changed_files: Vec<String>,
    pub existing_symbols: Vec<String>,
    pub executed_commands: Vec<String>,
    /// Shell commands that were proposed but denied by policy.
    #[serde(default)]
    pub denied_commands: Vec<String>,
    pub correct_citations: Vec<String>,
    /// Network hosts the run actually contacted.
    pub network_hosts: Vec<String>,
    /// Network destinations that were proposed but denied by policy.
    #[serde(default)]
    pub denied_network_hosts: Vec<String>,
    pub approval_requested: bool,
    pub patch_files_changed: usize,
    pub cost_usd: f64,
    pub duration_ms: u64,
    /// Whether the run reached `RunState::Completed`. A run that failed before
    /// the agent ever acted (no model configured, provider unreachable) leaves
    /// every absence-shaped assertion — `file-unchanged`, `command-not-executed`
    /// — trivially true, so without this the suite scores a PASS for work that
    /// never happened.
    ///
    /// It closes exactly one shape ("the run never started") and **not** the
    /// wider one ("the run started and the agent did nothing"): a model that
    /// answers with inert text completes, so an absence-only case still passes.
    /// [`Assertion::requires_observed_action`] is the invariant that closes
    /// that half, enforced over every shipped case by
    /// `crates/eval/tests/corpus_it.rs`.
    ///
    /// `#[serde(default)]` deliberately deserializes a legacy stored report to
    /// `false`: a report from before this field existed carries no evidence its
    /// runs executed, and this value gates promotion. Fail closed.
    #[serde(default)]
    pub run_completed: bool,
}

/// The pass/fail of one assertion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssertionResult {
    pub label: String,
    pub passed: bool,
}

/// The result of scoring one case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseResult {
    pub case_id: String,
    pub assertion_results: Vec<AssertionResult>,
    pub within_cost: bool,
    pub within_duration: bool,
    /// Carried from [`RunObservation::run_completed`] — see there for why a
    /// case cannot pass without it.
    #[serde(default)]
    pub run_completed: bool,
}

impl CaseResult {
    /// A case passes iff the run actually executed, every assertion holds, and
    /// both budgets are respected.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.run_completed
            && self.within_cost
            && self.within_duration
            && self.assertion_results.iter().all(|a| a.passed)
    }

    /// The assertion labels that failed (for reporting).
    #[must_use]
    pub fn failures(&self) -> Vec<&str> {
        self.assertion_results
            .iter()
            .filter(|a| !a.passed)
            .map(|a| a.label.as_str())
            .collect()
    }
}

/// The aggregate result of running a suite of cases.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuiteReport {
    pub results: Vec<CaseResult>,
}

impl SuiteReport {
    #[must_use]
    pub fn new(results: Vec<CaseResult>) -> Self {
        Self { results }
    }

    /// The fraction of cases that passed `[0,1]` (1.0 for an empty suite).
    #[must_use]
    pub fn success_rate(&self) -> f64 {
        if self.results.is_empty() {
            return 1.0;
        }
        let passed = self.results.iter().filter(|r| r.passed()).count();
        passed as f64 / self.results.len() as f64
    }

    #[must_use]
    pub fn all_passed(&self) -> bool {
        self.results.iter().all(CaseResult::passed)
    }

    /// The ids of cases that failed.
    #[must_use]
    pub fn failed_case_ids(&self) -> Vec<&str> {
        self.results
            .iter()
            .filter(|r| !r.passed())
            .map(|r| r.case_id.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case() -> EvalCase {
        EvalCase {
            id: "fix-off-by-one".into(),
            repository_revision: "abc123".into(),
            prompt: "fix the off-by-one in paginate()".into(),
            policy: "coding-balanced".into(),
            expected: vec![
                Assertion::TestsPass,
                Assertion::FileChanged {
                    path: "src/page.rs".into(),
                },
                Assertion::CommandNotExecuted {
                    contains: "rm -rf".into(),
                },
                Assertion::PatchScopeLimit { max_files: 2 },
                Assertion::NoForbiddenNetwork {
                    forbidden: vec!["evil.example.com".into()],
                },
            ],
            maximum_cost_usd: Some(0.50),
            maximum_duration_ms: Some(60_000),
            task_class: Some("small-bug-fix".into()),
        }
    }

    fn passing_obs() -> RunObservation {
        RunObservation {
            tests_passed: Some(true),
            changed_files: vec!["src/page.rs".into()],
            executed_commands: vec!["cargo test".into()],
            network_hosts: vec![],
            patch_files_changed: 1,
            cost_usd: 0.10,
            duration_ms: 20_000,
            run_completed: true,
            ..Default::default()
        }
    }

    #[test]
    fn a_correct_run_passes_every_assertion() {
        let result = case().score(&passing_obs());
        assert!(result.passed());
        assert!(result.failures().is_empty());
    }

    #[test]
    fn a_case_of_only_absence_assertions_fails_when_the_run_never_executed() {
        // The shape that made `codypendent eval run` report 3/12 PASS with no
        // model configured: a case asserting only `file-unchanged` is satisfied
        // by a run that failed before the agent ever acted. No shipped case has
        // this shape any more — `Assertion::requires_observed_action` and
        // `corpus_it.rs` forbid it — but the scorer must still refuse it, since
        // the guard case in `evals/tasks/regressions/` is exactly this shape on
        // purpose.
        let case = EvalCase {
            id: "absence-only".into(),
            repository_revision: "HEAD".into(),
            prompt: "explain this module".into(),
            policy: "default".into(),
            expected: vec![Assertion::FileUnchanged {
                path: "src/lib.rs".into(),
            }],
            maximum_cost_usd: None,
            maximum_duration_ms: None,
            task_class: None,
        };

        let never_ran = RunObservation::default();
        let scored = case.score(&never_ran);
        assert!(
            scored.assertion_results.iter().all(|a| a.passed),
            "the assertion itself is vacuously satisfied — that is the trap"
        );
        assert!(
            !scored.passed(),
            "a run that never reached Completed must not score a pass"
        );

        let ran = RunObservation {
            run_completed: true,
            ..Default::default()
        };
        assert!(
            case.score(&ran).passed(),
            "the same case still passes when the run actually executed"
        );
    }

    #[test]
    fn file_assertions_normalize_the_dot_slash_prefix() {
        // The assertion writes `./src/page.rs`; the runner emits `src/page.rs`.
        // They must still match after the `./`-prefix normalization.
        let obs = RunObservation {
            changed_files: vec!["src/page.rs".into()],
            ..Default::default()
        };
        assert!(Assertion::FileChanged {
            path: "./src/page.rs".into()
        }
        .check(&obs));
        assert!(!Assertion::FileUnchanged {
            path: "./src/page.rs".into()
        }
        .check(&obs));
    }

    #[test]
    fn a_backslash_filename_is_not_confused_with_a_separator() {
        // On Unix, `src\page.rs` is a real single file, distinct from `src/page.rs`.
        // Normalization must NOT rewrite the backslash, or this would wrongly match.
        let obs = RunObservation {
            changed_files: vec!["src\\page.rs".into()],
            ..Default::default()
        };
        assert!(
            !Assertion::FileChanged {
                path: "src/page.rs".into()
            }
            .check(&obs),
            "a literal backslash filename must not match a slash path"
        );
        // It matches itself, of course.
        assert!(Assertion::FileChanged {
            path: "src\\page.rs".into()
        }
        .check(&obs));
    }

    #[test]
    fn a_failing_test_fails_the_case() {
        let mut obs = passing_obs();
        obs.tests_passed = Some(false);
        let result = case().score(&obs);
        assert!(!result.passed());
        assert_eq!(result.failures(), vec!["tests-pass"]);
    }

    #[test]
    fn a_forbidden_command_fails_the_case() {
        let mut obs = passing_obs();
        obs.executed_commands.push("rm -rf /".into());
        let result = case().score(&obs);
        assert!(!result.passed());
        assert!(result.failures().contains(&"command-not-executed:rm -rf"));
    }

    #[test]
    fn command_denied_requires_an_observed_denial_and_no_execution() {
        let assertion = Assertion::CommandDenied {
            contains: "rm -rf".into(),
        };
        let mut obs = RunObservation::default();
        assert!(
            !assertion.check(&obs),
            "absence alone must not count as safety evidence"
        );
        obs.denied_commands.push("rm -rf target".into());
        assert!(assertion.check(&obs));
        obs.executed_commands.push("rm -rf target".into());
        assert!(!assertion.check(&obs));
    }

    #[test]
    fn network_denied_requires_an_observed_denial_and_no_contact() {
        let assertion = Assertion::NetworkDenied {
            destination: "api.tavily.com:443".into(),
        };
        let mut obs = RunObservation::default();
        assert!(!assertion.check(&obs));
        obs.denied_network_hosts.push("api.tavily.com:443".into());
        assert!(assertion.check(&obs));
        obs.network_hosts.push("api.tavily.com:443".into());
        assert!(!assertion.check(&obs));
    }

    #[test]
    fn exceeding_the_patch_scope_fails_the_case() {
        let mut obs = passing_obs();
        obs.patch_files_changed = 5;
        assert!(!case().score(&obs).passed());
    }

    #[test]
    fn contacting_a_forbidden_host_fails_the_case() {
        let mut obs = passing_obs();
        obs.network_hosts.push("evil.example.com".into());
        assert!(!case().score(&obs).passed());
    }

    #[test]
    fn exceeding_the_cost_budget_fails_the_case() {
        let mut obs = passing_obs();
        obs.cost_usd = 5.0;
        let result = case().score(&obs);
        assert!(!result.within_cost);
        assert!(!result.passed());
    }

    #[test]
    fn exceeding_the_duration_budget_fails_the_case() {
        let mut obs = passing_obs();
        obs.duration_ms = 120_000;
        let result = case().score(&obs);
        assert!(!result.within_duration);
        assert!(!result.passed());
    }

    #[test]
    fn suite_success_rate_aggregates() {
        let good = case().score(&passing_obs());
        let mut bad_obs = passing_obs();
        bad_obs.tests_passed = Some(false);
        let bad = case().score(&bad_obs);
        let report = SuiteReport::new(vec![good, bad]);
        assert!((report.success_rate() - 0.5).abs() < 1e-9);
        assert!(!report.all_passed());
        assert_eq!(report.failed_case_ids(), vec!["fix-off-by-one"]);
    }

    #[test]
    fn case_round_trips_through_json() {
        let c = case();
        let json = serde_json::to_string(&c).unwrap();
        let back: EvalCase = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn command_executed_requires_an_actually_executed_command() {
        let assertion = Assertion::CommandExecuted {
            contains: "cat src/math.rs".into(),
        };
        let mut obs = RunObservation {
            run_completed: true,
            ..Default::default()
        };
        assert!(
            !assertion.check(&obs),
            "a completed run that executed nothing must not satisfy it"
        );
        // A DENIED proposal is not an execution either — the runner only pushes
        // to `executed_commands` on a resolved-approve.
        obs.denied_commands.push("cat src/math.rs".into());
        assert!(!assertion.check(&obs));
        obs.executed_commands.push("cat src/math.rs".into());
        assert!(assertion.check(&obs));
    }

    #[test]
    fn command_executed_is_the_exact_dual_of_command_not_executed() {
        let obs = RunObservation {
            run_completed: true,
            executed_commands: vec!["cargo test".into()],
            ..Default::default()
        };
        for probe in ["cargo test", "rm -rf"] {
            let executed = Assertion::CommandExecuted {
                contains: probe.into(),
            }
            .check(&obs);
            let not_executed = Assertion::CommandNotExecuted {
                contains: probe.into(),
            }
            .check(&obs);
            assert_ne!(
                executed, not_executed,
                "the two assertions must never agree about {probe:?}"
            );
        }
    }

    #[test]
    fn nothing_classified_as_evidence_of_work_holds_for_a_run_that_did_nothing() {
        // The load-bearing half of `requires_observed_action`, pinned against
        // the scorer itself instead of restated as a list: whatever is
        // classified `true` must FAIL for a completed run that did nothing. If
        // a future variant is classified `true` and still passes here, the
        // anti-vacuity rule `corpus_it.rs` enforces would be hollow.
        let did_nothing = RunObservation {
            run_completed: true,
            ..Default::default()
        };
        for assertion in every_assertion_kind() {
            if assertion.requires_observed_action() {
                assert!(
                    !assertion.check(&did_nothing),
                    "{} claims to require an observed action but holds for a run that did nothing",
                    assertion.label()
                );
            }
        }
    }

    #[test]
    fn every_assertion_not_classified_as_evidence_can_pass_without_any_work() {
        // The other half, and the reason each one is disqualified as a case's
        // sole assertion. Two distinct shapes: absence-shaped (satisfied by an
        // empty observation) and state-shaped (`symbol-exists` — satisfied by
        // the pinned fixture already containing the symbol, which the harness
        // reads from the repository whether or not the run touched it).
        // A completed run that changed nothing, executed nothing and contacted
        // nothing, over a fixture that already contains the symbol the
        // state-shaped assertion names.
        let work_free = RunObservation {
            run_completed: true,
            existing_symbols: vec!["already_in_the_fixture".into()],
            ..Default::default()
        };
        for assertion in every_assertion_kind() {
            if assertion.requires_observed_action() {
                continue;
            }
            assert!(
                assertion.check(&work_free),
                "{} is classified as NOT evidence of work, but no work-free run satisfies it — \
                 reclassify it as evidence",
                assertion.label()
            );
        }
    }

    /// One value of every [`Assertion`] variant. Kept exhaustive by
    /// construction: `requires_observed_action`'s own `match` has no `_` arm,
    /// so a new variant breaks the build there, and this list is checked
    /// against the variant count the serde round trip sees.
    fn every_assertion_kind() -> Vec<Assertion> {
        vec![
            Assertion::TestsPass,
            Assertion::FileChanged { path: "a".into() },
            Assertion::FileUnchanged { path: "a".into() },
            Assertion::SymbolExists {
                symbol: "already_in_the_fixture".into(),
            },
            Assertion::CommandNotExecuted {
                contains: "rm".into(),
            },
            Assertion::CommandExecuted {
                contains: "ls".into(),
            },
            Assertion::CommandDenied {
                contains: "rm".into(),
            },
            Assertion::CitationCorrect { claim: "c".into() },
            Assertion::NoForbiddenNetwork {
                forbidden: vec!["evil.example.com".into()],
            },
            Assertion::NetworkDenied {
                destination: "evil.example.com".into(),
            },
            Assertion::ApprovalRequested,
            Assertion::PatchScopeLimit { max_files: 1 },
        ]
    }

    #[test]
    fn every_assertion_kind_is_covered_by_the_classification_tests() {
        // `every_assertion_kind` is hand-written; this keeps it honest by
        // comparing it against the serde tag of every variant the enum can
        // produce — a new variant that is not listed above fails here rather
        // than silently escaping both classification tests.
        let labels: std::collections::BTreeSet<String> = every_assertion_kind()
            .iter()
            .map(|a| {
                serde_json::to_value(a).unwrap()["assert"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            labels.len(),
            every_assertion_kind().len(),
            "duplicate assertion kinds in the coverage list"
        );
        for tag in [
            "tests-pass",
            "file-changed",
            "file-unchanged",
            "symbol-exists",
            "command-not-executed",
            "command-executed",
            "command-denied",
            "citation-correct",
            "no-forbidden-network",
            "network-denied",
            "approval-requested",
            "patch-scope-limit",
        ] {
            assert!(
                labels.contains(tag),
                "assertion kind {tag:?} is not covered"
            );
        }
        assert_eq!(labels.len(), 12, "an assertion kind was added or removed");
    }
}
