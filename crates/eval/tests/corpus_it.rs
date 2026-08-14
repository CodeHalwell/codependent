//! Phase 7 STEP 7.1: "the core corpus loads and each case parses to a valid
//! `EvalCase`." Loads the REAL, shipped `evals/tasks/core/` suite (found via
//! `CARGO_MANIFEST_DIR`, since a test's working directory is the crate root,
//! not the workspace root) and checks its shape: every file parses, ids are
//! unique and non-empty, every case pins the same fixture revision, the
//! required task classes are represented, and safety assertions are backed by
//! an observed policy denial rather than absence-only checks.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use codypendent_eval::{Assertion, EvalCase};

fn evals_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("evals")
}

fn core_suite_dir() -> PathBuf {
    evals_root().join("tasks").join("core")
}

/// The one case in the whole tree allowed to be built entirely from
/// absence-shaped assertions, by id, with the reason it is exempt.
///
/// `evals/tasks/regressions/001-…` exists to prove that an absence-only case FAILS
/// when the run never executed (`RunObservation::run_completed`). Being
/// absence-only is the property under test, so requiring it to carry evidence
/// of work would delete the guard. Any other case, in either directory, must
/// earn its pass — see [`every_shipped_case_can_only_pass_if_the_run_did_work`].
const ANTI_VACUITY_EXEMPT: &[(&str, &str)] = &[(
    "absence-only-case-must-fail-without-a-model",
    "the guard case whose absence-only shape IS the regression it pins",
)];

fn load_core_suite() -> Vec<(PathBuf, EvalCase)> {
    load_suite(&core_suite_dir())
}

/// Every case file shipped anywhere under `evals/` — the core corpus plus the
/// regression guards. The anti-vacuity rule is a property of *shipped cases*,
/// not of one directory, so it is checked over all of them.
fn load_every_shipped_case() -> Vec<(PathBuf, EvalCase)> {
    let mut all = load_suite(&core_suite_dir());
    all.extend(load_suite(&evals_root().join("tasks").join("regressions")));
    all
}

fn load_suite(dir: &Path) -> Vec<(PathBuf, EvalCase)> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    entries.sort();
    entries
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            let case = serde_json::from_str::<EvalCase>(&text)
                .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()));
            (path, case)
        })
        .collect()
}

#[test]
fn the_core_suite_ships_a_real_runnable_range_of_cases() {
    // The task brief's original range was 8-12; grown to 13 by this task
    // (see `013-fix-and-add-negative-test.json`) and capped generously above
    // that so the NEXT honest addition doesn't need to touch this bound —
    // the real ceiling this test enforces is "still small enough to be a
    // deliberate, reviewed corpus," not an exact count. Every case must
    // still individually earn its place: see `safety_assertions_are_non_
    // vacuous` and this file's own module doc.
    let cases = load_core_suite();
    assert!(
        cases.len() >= 8 && cases.len() <= 24,
        "expected 8-24 core-suite cases, found {}",
        cases.len()
    );
}

#[test]
fn every_case_file_parses_to_a_valid_eval_case() {
    // load_core_suite() itself panics on any parse failure, naming the file —
    // reaching this line at all is most of the assertion. Also check the
    // basic shape every case must have.
    for (path, case) in load_core_suite() {
        assert!(
            !case.id.is_empty(),
            "{}: case id must not be empty",
            path.display()
        );
        assert!(
            !case.prompt.trim().is_empty(),
            "{}: case prompt must not be empty",
            path.display()
        );
        assert!(
            !case.expected.is_empty(),
            "{}: case {} has no assertions at all",
            path.display(),
            case.id
        );
        assert_eq!(
            case.repository_revision.len(),
            40,
            "{}: repository_revision must be a full 40-character git SHA, got {:?}",
            path.display(),
            case.repository_revision
        );
    }
}

#[test]
fn every_case_id_is_unique() {
    let cases = load_core_suite();
    let mut seen = HashSet::new();
    for (path, case) in &cases {
        assert!(
            seen.insert(case.id.clone()),
            "{}: duplicate case id {:?}",
            path.display(),
            case.id
        );
    }
}

#[test]
fn every_case_pins_the_same_fixture_revision() {
    // The core suite runs entirely against one vendored fixture at one
    // revision (see evals/README.md) — a case pinning a different revision
    // would silently target a different repository state than its siblings.
    let cases = load_core_suite();
    let revisions: HashSet<&str> = cases
        .iter()
        .map(|(_, c)| c.repository_revision.as_str())
        .collect();
    assert_eq!(
        revisions.len(),
        1,
        "expected every core-suite case to pin the same revision, found {revisions:?}"
    );
}

#[test]
fn the_required_task_classes_are_all_represented() {
    let cases = load_core_suite();
    let classes: HashSet<String> = cases
        .iter()
        .filter_map(|(_, c)| c.task_class.clone())
        .collect();
    for required in [
        "failing-test-diagnosis",
        "small-bug-fix",
        "regression-test-addition",
        "doc-update",
        "ci-diagnosis",
        "safe-refactor",
    ] {
        assert!(
            classes.contains(required),
            "no core-suite case declares task_class {required:?}; found {classes:?}"
        );
    }
}

#[test]
fn safety_assertions_are_non_vacuous() {
    let cases = load_core_suite();
    let mut has_approval_requested = false;
    let mut has_command_denied = false;
    for (_, case) in &cases {
        for assertion in &case.expected {
            match assertion {
                Assertion::ApprovalRequested => has_approval_requested = true,
                Assertion::CommandDenied { .. } => has_command_denied = true,
                Assertion::CommandNotExecuted { .. } | Assertion::NoForbiddenNetwork { .. } => {
                    panic!(
                        "core safety assertions must not pass merely because an action was absent"
                    )
                }
                _ => {}
            }
        }
    }
    assert!(has_approval_requested, "no case asserts approval-requested");
    assert!(
        has_command_denied,
        "no case requires an observed destructive-command denial"
    );
}

#[test]
fn every_shipped_case_can_only_pass_if_the_run_did_work() {
    // The rule the round-4 review's finding turns into an invariant. Banning
    // two absence-only assertion KINDS (above) was the instance fix; it left
    // `file-unchanged` — and `patch-scope<=N` — permitted, and three shipped
    // cases were built entirely from those, so they passed if the harness did
    // nothing at all. A reviewer's probe case, whose prompt the stub model
    // could not even match, scored PASS 1/1 100%.
    //
    // The class fix is stated positively and over every shipped case: at least
    // one assertion that CANNOT hold unless the run really acted. Which kinds
    // qualify is decided once, on the type
    // (`Assertion::requires_observed_action`), never re-listed here.
    let exempt: std::collections::HashMap<&str, &str> =
        ANTI_VACUITY_EXEMPT.iter().copied().collect();
    let cases = load_every_shipped_case();
    assert!(!cases.is_empty(), "no case files were loaded at all");

    let mut exercised_exemptions = HashSet::new();
    for (path, case) in &cases {
        let evidence: Vec<String> = case
            .expected
            .iter()
            .filter(|a| a.requires_observed_action())
            .map(Assertion::label)
            .collect();
        if let Some(reason) = exempt.get(case.id.as_str()) {
            exercised_exemptions.insert(case.id.clone());
            assert!(
                evidence.is_empty(),
                "{}: case {:?} is on the anti-vacuity exemption list ({reason}) but now carries \
                 evidence of work ({evidence:?}) — remove the exemption",
                path.display(),
                case.id
            );
            continue;
        }
        assert!(
            !evidence.is_empty(),
            "{}: case {:?} can pass without the run doing anything — every one of its assertions \
             ({:?}) is satisfied by a completed run that changed nothing, executed nothing and \
             contacted nothing. Add an assertion that requires an observed action \
             (file-changed, tests-pass, command-executed, command-denied, network-denied, \
             approval-requested), or add the case to ANTI_VACUITY_EXEMPT with a reason.",
            path.display(),
            case.id,
            case.expected
                .iter()
                .map(Assertion::label)
                .collect::<Vec<_>>()
        );
    }

    // An exemption for a case that no longer exists is dead weight that would
    // silently re-admit vacuity under that id later.
    for (id, reason) in ANTI_VACUITY_EXEMPT {
        assert!(
            exercised_exemptions.contains(*id),
            "ANTI_VACUITY_EXEMPT lists {id:?} ({reason}) but no shipped case has that id"
        );
    }
}

#[test]
fn a_case_that_asserts_tests_pass_actually_resolves_the_seeded_bug() {
    // The fixture's ONE seeded failure (math::add_one) makes `tests-pass` a
    // whole-suite signal (RunObservation::tests_passed is one bool for the
    // whole `cargo test` run, not per-test) — a case that asserts `tests-pass`
    // without also fixing that bug could never pass no matter what the agent
    // does. This test only confirms at least one such case exists and that
    // its prompt actually asks for the fix (a cheap, deliberately loose text
    // check — the real proof is `eval_it.rs`'s known-pass/known-fail smoke
    // test in the cli crate).
    let cases = load_core_suite();
    let fixes_the_bug: Vec<&EvalCase> = cases
        .iter()
        .map(|(_, c)| c)
        .filter(|c| c.expected.contains(&Assertion::TestsPass))
        .collect();
    assert!(
        !fixes_the_bug.is_empty(),
        "no case asserts tests-pass at all"
    );
    for case in fixes_the_bug {
        assert!(
            case.prompt.to_lowercase().contains("add_one") || case.prompt.to_lowercase().contains("fix"),
            "case {:?} asserts tests-pass but its prompt does not mention fixing the seeded bug: {:?}",
            case.id,
            case.prompt
        );
    }
}
