# Codypendent regression suite

Guard cases for **fixed failures that must never silently come back** —
STEP 7.4/7.5's `codypendent_eval::RegressionSuite`
(`crates/eval/src/regression.rs`). Distinct from `evals/tasks/core/`: that
corpus asks "can the agent do this task at all"; this one asks "did a
specific, previously-broken thing stay fixed".

Every file here is a plain [Chapter 16](../../../docs/docs/16-testing-strategy.md)
`EvalCase`, identical in shape to `evals/tasks/core/*.json` — a pinned
`repository_revision`, a `prompt`, a `policy`, `expected` assertions, and
budgets — loaded the same way (`codypendent_cli::eval::load_suite`) and
runnable the same way:

```
codypendent eval run --suite regressions --report out.json
```

> **This directory moved (2026-08-13), and the old documented command never
> worked.** It used to live at `evals/regressions/`, documented as
> `--suite evals/regressions`. That command loaded the cases and then died:
>
> ```
> eval: loaded 1 case(s) from evals/regressions
> Error: fixture bundle not found at fixtures/tiny-crate.bundle (referenced by the suite's cases)
> ```
>
> A suite's fixture is resolved as `<suite_dir>/../../fixtures/<name>.bundle`
> (`codypendent_cli::eval::fixture_root`), which is only correct for a suite
> two levels under `evals/`. From `evals/regressions/` it resolved to
> `./fixtures/…`, which does not exist — so the one guard case written to
> catch the vacuous-pass bug could not be run at all, while `evals/README.md`
> claimed it "runs fine today". Moving the suite to `evals/tasks/regressions/`
> puts it where the convention already works, and `--suite regressions` now
> resolves through `resolve_suite_dir`'s `evals/tasks/<name>` branch.
> The underlying trap — `--suite <any other path>` still fails this way — is
> written up for the owner of `crates/cli/src/eval.rs` in
> `.impl/r4-proposals/C-cli-from-F-evals-docs.md`.

## Why this directory exists

Before this task, `crates/eval/src/regression.rs`'s own module doc named
this directory as where the suite lives, and it did not exist —
`RegressionSuite`/`RegressionReport` had real unit tests (a hand-built
`BTreeMap<String, RunObservation>` in every one of them) but no case ever
ran through here, and no production code ever called
`RegressionSuite::evaluate` against a REAL run. `codypendent_cli::eval::
run_regression_suite` (`crates/cli/src/eval.rs`) is the wiring: it loads a
batch of cases exactly like these, drives each headlessly over a real
daemon connection (the same `run_case_over_connection` / `inspect_repository`
pipeline `evals/tasks/core/` uses), and calls the real
`RegressionSuite::evaluate` — not a re-implementation of its "a case with no
observation counts as regressed" rule — to produce a `RegressionReport`.

No CLI flag drives `run_regression_suite` yet — see
`.impl/proposals/agent-models-from-agent-evals.md` for the follow-up
`commands.rs` wiring a `codypendent eval run --suite regressions
--regression` flag would need. Until then, every case here is still a fully
runnable, real `EvalCase` through the plain `--suite` path above; the only
thing `--regression` would add is `RegressionSuite::evaluate`'s stricter
"missing observation ⇒ regressed" rule instead of per-case pass/fail.

## Cases

- **`001-absence-only-case-must-fail-without-a-model.json`** — the harness's
  own historical failure (the review's finding this whole outcome was
  written to fix): an absence-only case (`file-unchanged` and nothing else)
  scores a vacuous PASS when the agent never runs at all — a missing
  `models.toml`, an unreachable provider, anything that stops the run before
  the model ever acts. `CaseResult::passed()` (`crates/eval/src/case.rs`)
  now requires `RunObservation::run_completed`, so this must FAIL under
  exactly that condition. **How to exercise the regression this guards**:
  run it against a daemon with no `models.toml` configured (the cheapest,
  always-reproducible way to make a run never execute — no stub, no live
  model needed):

  ```
  CODYPENDENT_DATA_DIR=<a dir with no models.toml> \
    codypendent eval run --suite regressions --report out.json
  ```

  Expect `FAIL`. Both halves were run for real on 2026-08-13 against
  `target/debug/codypendent`: unconfigured →
  `absence-only-case-must-fail-without-a-model FAIL`, `0/1 (0%)`; against
  `evals/ci/stub_model.py` → `PASS`, `1/1 (100%)`. If this ever prints `PASS` against an unconfigured daemon,
  the `run_completed` gate has regressed — see `crates/eval/src/case.rs`'s
  own `a_case_of_only_absence_assertions_fails_when_the_run_never_executed`
  unit test for the same property pinned at the pure-scoring-function level,
  and `crates/eval/src/grade.rs`'s
  `from_case_grades_a_run_that_never_executed_as_a_real_failure` for the
  same property at the grading level.

  Run against `evals/ci/stub_model.py` (a real, if scripted, model that DOES
  act) instead, and it correctly PASSES — the guard is specifically about the
  NO-model path, not about read-only cases being unable to pass.

  **This is the one case in the tree allowed to be absence-only**, and it is
  exempt by name in `crates/eval/tests/corpus_it.rs`
  (`ANTI_VACUITY_EXEMPT`), with the reason recorded there: being absence-only
  IS the property under test, so requiring it to carry evidence of work would
  delete the guard. Every other shipped case — here and in
  `evals/tasks/core/` — must carry at least one assertion that cannot hold
  unless the run really acted (`Assertion::requires_observed_action`). It was
  modelled on `evals/tasks/core/007-explain-average-no-network.json`, which
  was absence-only when this case was written; `007` has since been given a
  `command-executed` assertion for exactly that reason, so the resemblance no
  longer holds.

## Growing this suite

Per `RegressionSuite::add_fixed_cluster`'s own doc: a guard case's id
convention is `regression/<cluster-key>` when it is minted programmatically
from a `codypendent_eval::cluster::FailureCluster` (STEP 7.4's clustering —
see `crates/cli/src/eval.rs`'s `report_failure_clusters`, wired into every
`codypendent eval run`). A hand-authored guard case (like `001` above) is
free to use a descriptive id instead; either shape loads and runs
identically.
