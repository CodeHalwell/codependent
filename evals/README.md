# Codypendent evaluation corpus

The benchmark task set for `codypendent eval run` (Phase 7 STEP 7.1, [Chapter
16](../docs/docs/16-testing-strategy.md)). Every case is a Chapter 16
`EvalCase` — a pinned `repository_revision`, a `prompt`, a `policy`, a list of
objective `Assertion`s, and cost/duration budgets — run headlessly over the
JSONL client and scored against what actually happened, never against a
model's own account of what it did.

## Layout

```
evals/
  tasks/
    core/            # the runnable core suite — see below
      001-....json
      ...
    regressions/     # guard cases for fixed failures — see its own README
  fixtures/
    tiny-crate.bundle  # a vendored git repository, one pinned commit
  baselines/
    core.json        # the eval-regression gate's score history
  ci/                # run_gate.sh, stub_model.py, compare_baseline.py
```

`evals/tasks/regressions/` was at `evals/regressions/` until 2026-08-13, where
the fixture-path convention below did not resolve and the suite could not be
run at all — see that directory's own README.

- **`evals/tasks/<suite>/*.json`** — one `EvalCase` per file. `codypendent
  eval run --suite <suite>` loads every `*.json` file directly under that
  directory (non-recursive), in filename order — hence the numeric prefixes.
- **`evals/fixtures/<name>.bundle`** — a fixture repository vendored as a `git
  bundle`, not a plain checkout. A plain checkout would need its own nested
  `.git` directory, which the *parent* repository (this one) would then treat
  as a submodule gitlink rather than tracked file content — a bundle is an
  ordinary blob to the parent repo, and `git clone` accepts a bundle file
  directly as a clone source, exactly like a live remote. `codypendent eval
  run` clones the suite's bundle into a fresh scratch directory per case
  (never mutating the vendored bundle) and checks out that case's pinned
  `repository_revision`.
- A suite's fixture is resolved by **name convention**: `evals/tasks/<suite>/`
  runs against `evals/fixtures/<name>.bundle`, where `<name>` is currently
  hardcoded to `tiny-crate` in `codypendent-cli`'s `commands::eval_run`.
  `EvalCase` itself carries only a `repository_revision`, not a repository
  path — see "Growing the corpus" below for how a multi-fixture suite would
  extend this.

## The core suite (`evals/tasks/core/`)

13 cases
<!-- doc-count:match sources="evals/tasks/core" glob="*.json" pattern="^\s*\"id\":" expect=13 label="core eval cases" -->
(the original task brief asked for a real, runnable 8–12; grown to
13 by the Outcome 16 repair pass — see "Growing the corpus" below for the
honesty rule that governed the addition. The full 50–100 the roadmap
eventually wants is a separate, later content-authoring effort). Every case
runs against the **same single pinned commit**
(`8e7644ddbbe0dd04052b47f0e2bfefd45b535ee6`) of the vendored
`codypendent-eval-fixture` crate — a tiny, dependency-free Rust crate with:

- one deliberate bug (`math::add_one` is off by one — `math::tests::
  add_one_increments` fails against the pinned commit);
- one undocumented function (`greet::loud_greet`);
- one broken CI config (`.github/workflows/ci.yml` never checks out the
  repository before running `cargo test`).

Task classes covered (the six the brief named): failing-test-diagnosis
(`002`), small-bug-fix (`001`, `009`, `013`), regression-test-addition
(`003`), doc-update (`004`, `011`), ci-diagnosis (`005`), safe-refactor
(`006`, `008`, `010`). Also covered: an architecture-explanation-style
read-only case (`007`) and a PR-feedback-response case (`009`) from the
broader Chapter 16 list. Approval behavior is exercised by `001` and `008`.
Case `012` is an explicit policy-boundary test: it asks for a destructive
command and requires an observed `command-denied` event. Case `013` is a
**compound** small-bug-fix: it requires the seeded bug fixed AND a new test
added in the SAME change (`tests-pass` + `file-changed` + `symbol-exists`
together) — exercising more than one positive assertion kind on one case,
which no single case among `001`-`012` does.

### Every case must be able to fail (the anti-vacuity rule)

**Every case carries at least one assertion that cannot hold unless the run
really acted** — `Assertion::requires_observed_action` decides which kinds
qualify, once, on the type, and `crates/eval/tests/corpus_it.rs`
(`every_shipped_case_can_only_pass_if_the_run_did_work`) enforces it over
every case file in `evals/tasks/`.

This is the round-4 repair of a real defect, not a style rule. Absence-only
`command-not-executed` and `no-forbidden-network` checks had already been
removed from the corpus for passing vacuously — but `file-unchanged` and
`patch-scope<=N` are absence-shaped too, and cases `002`, `005` and `007` were
built *entirely* from `file-unchanged`. A run that did nothing at all satisfied
all three: a reviewer's probe case, whose prompt the stub model could not even
match, scored `PASS 1/1 100%`. `RunObservation::run_completed` closes only the
narrower shape "the run never started".

The three read-only cases now each require an observed
`command-executed` — the file they are asked to reason about must actually have
been read, through an approved shell command, which the event stream records.
That is the only evidence-of-work this harness can observe for a case that must
not change a file: nothing reads the model's response text (`RunObservation` has
no field for it), so "did it answer well" is out of reach and "did it do the
work" is not. The one deliberate exemption is the guard case in
`evals/tasks/regressions/`, listed by id with its reason in `corpus_it.rs`.

**A whole-suite caveat, by design:** `RunObservation::tests_passed` is a
single pass/fail for the *entire* fixture's `cargo test` run, not per-test
(Chapter 16's `EvalCase` shape doesn't carry a test filter). Concretely, this
means a case can only honestly assert `tests-pass` if resolving it *also*
fixes `math::add_one`'s pre-existing failure — cases `001`, `009`, and `013`
do; every other case that changes the repository deliberately leaves that bug
alone and so does **not** assert `tests-pass`. Growing the corpus with a
multi-fixture-revision suite (see below) removes this constraint.

### How a case is run and scored

`codypendent eval run` (`crates/cli/src/eval.rs`) builds the objective
`RunObservation` two ways:

1. **From the run's own event stream** — `approval_requested`,
   `executed_commands` (what `command-executed` / `command-not-executed`
   assert on), `network_hosts`, policy-denied commands/destinations,
   and `cost_usd` come from `ApprovalRequested`/`ApprovalResolved`/
   `ToolDenied`/`BudgetWarning` events as the run streams by. Only an
   **approved** action counts as executed/contacted, while `ToolDenied` retains
   the typed action that was explicitly blocked. An action that somehow executes *without*
   going through the approval flow is invisible to this — every
   allow-listed shell command in this codebase's default policy requires
   approval (`crates/daemon/src/policy/mod.rs`), so this is a narrow,
   documented gap, not a silent one.
2. **From the run's own isolated worktree, after the run completes** —
   `changed_files` (tracked + untracked diff against the pinned revision),
   `existing_symbols` (a literal `git grep`, checked only when a case
   actually asserts `symbol-exists`), and `tests_passed` (a real `cargo test`,
   checked only when a case asserts `tests-pass`). These facts live in the
   repository, not on the wire. **Not `repository` itself** — STEP 1.8
   isolates every writing run onto its own `git worktree` (a sibling
   directory, `codypendent-worktrees/<repo>/run-<short-id>`), and
   `WorktreeManager::release` never merges that back into the checkout that
   spawned it (it exports the diff as a patch artifact and, whenever the
   worktree holds real changes, *retains the directory* instead of deleting
   it). Diffing `repository` alone would report `file-changed` as false for
   every case that actually worked — a false negative baked into the harness,
   found and fixed in this task (see `crate::eval::run_worktree_root`'s own
   doc in `crates/cli/src/eval.rs` for the full trust chain, and
   `run_worktree_root_matches_the_daemons_own_layout`, which cross-checks the
   reconstructed path against a real `WorktreeManager::allocate` call rather
   than a hand-verified formula).

`correct_citations` has **no signal yet** — no event carries a claim/source
pair — so it is always empty and a `citation-correct` assertion would always
fail. No case in this suite uses it; see "Deferred" below.

## Growing the corpus to 50–100

1. **More cases against the same fixture.** The cheapest growth path: add
   more `evals/tasks/core/NNN-*.json` files against the existing pinned
   commit. Keep the "does this assertion set need `tests-pass`" rule above in
   mind, or extend the fixture with a second commit (see next point).
2. **A second pinned commit in the same fixture.** `git bundle create` again
   after adding more commits to the same working tree (`git bundle create
   evals/fixtures/tiny-crate.bundle --all` captures every ref/commit, so old
   pinned revisions keep resolving). A later commit that fixes `add_one` lets
   new cases assert `tests-pass` freely without touching that history.
3. **A second fixture.** Vendor another tiny crate the same way (build it as
   its own git repo, `git bundle create evals/fixtures/<new-name>.bundle
   --all`), add `evals/tasks/<new-suite>/`, and update
   `commands::eval_run`'s hardcoded fixture name to read a per-suite manifest
   instead (e.g. a `evals/tasks/<suite>/suite.toml` naming its fixture) —
   today it is a single hardcoded string because there is only one suite.
4. **Vendoring this repository itself at a fixed revision** (the brief's
   other suggested option) works the same way: `git bundle create
   evals/fixtures/codypendent-self.bundle <sha>` from a shallow or full clone
   of this repository, pinned to a specific commit. Prefer a small, purpose-
   built fixture like `tiny-crate` for most cases — `cargo test` on the real
   workspace is far slower per case, and a case designer rarely needs the
   whole codebase's surface area.
5. **Task classes still uncovered here** that the roadmap's full corpus wants:
   architecture explanation (partially covered, `007`), PR-feedback response
   (partially covered, `009`). Add cases as the fixture(s) grow.

## CI: two different, complementary jobs

`.github/workflows/ci.yml` runs the eval harness in TWO jobs that prove two
different things — read both before assuming either one is "the eval CI
job":

### `eval-smoke` — is the harness's own machinery correct?

- `cargo test -p codypendent-eval` — the harness's own scoring/promotion unit
  and integration tests, including `corpus_it.rs`, which loads the *real*
  `evals/tasks/core/` suite shipped here and checks its shape (parses, ids
  unique, required task classes present, the mandated assertion kinds each
  appear, a fixed-revision consistency check).
- `cargo test -p codypendent-cli --test eval_it` — a deterministic,
  hand-rolled mock daemon (no `codypendentd` subprocess, no live model) drives
  the exact same runner code path (`eval::run_case`) end to end, including
  real `git`/`cargo test` repository inspection against a real throwaway git
  repo it builds on the fly. It proves a known-pass case passes and a
  known-fail case fails — the "mock model" here is the mock daemon's scripted
  behaviour, which is deterministic by construction (see the test file's own
  doc comment for why this, rather than faking the model-provider wire
  protocol, is the appropriately-scoped mock for this task).

Neither of these RUNS the shipped corpus through `codypendent eval run` — they
prove the harness's code is correct in isolation, nothing about whether the
13 real cases still score what they used to.

### `eval-regression` — does the shipped corpus's score still hold?

The gate `evals/ci/run_gate.sh` wires up: builds `codypendent-cli`, starts
`evals/ci/stub_model.py` (a deterministic, hand-scripted "model" — see that
file's own docstring), points a throwaway daemon at it, runs
`codypendent eval run --suite core` against the REAL, shipped
`evals/tasks/core/` suite for real, and compares the result against the stored
baseline (`evals/baselines/core.json`) via `evals/ci/compare_baseline.py`.

The current baseline is **13/13**, established 2026-08-13. It was 3/13 for one
day, and that number was a bug, not a difficulty level — see "How the baseline
moved from 3/13 to 13/13" below.

## What this gate can and cannot detect

This section is the claim of record. `.github/workflows/ci.yml` and
`evals/ci/compare_baseline.py` both point here.

**It fails on ANY difference from the baseline**, in either direction: a lower
score, a higher score, a case that flipped either way, a case id added or
removed, a corpus of a different size, or a case file in `evals/tasks/core/`
that produced no result at all (`--corpus-dir` cross-checks the report against
the directory). A one-directional "fail only if the score dropped" gate could
not catch the two failures it exists for — **a case edited into vacuity and an
assertion that silently stopped firing both make the score go UP.** That is not
hypothetical: a reviewer deleted the 10 failing cases from a report, re-ran the
comparator, and got `OK: success rate 1.0000 (3/3) … PASSED, EXIT=0`. Deleting
77% of the corpus passed the gate. It does not any more.

Because the stub model is fixed and scripted, the ONLY thing that can move this
result between two commits is a change to the harness itself (`crates/eval/**`,
`crates/cli/src/eval.rs`, a case file, the stub's own script, the pinned
fixture) — so a failure here is real evidence that something in the SCORING
MACHINERY changed.

**It cannot detect a prompt or skill edit that lowers quality, and cannot be
made to.** The roadmap's sentence for this outcome — *"a skill or prompt edit
that lowers the score fails CI"* — is **not true of this gate**. The stub reads
no prompt file and no skill file; it selects its reply by matching a literal
substring of the case's own `prompt` and replays a precomputed answer. Editing a
system prompt, a skill, or a retrieval policy therefore cannot move this score
in either direction. Measuring that needs a live model — nondeterministic, paid,
and deliberately out of CI's reach (see "What's still NOT in CI"). Anyone
reading the outcome as satisfied by this job is reading it wrong.

To (re-)establish the baseline after an intentional, reviewed change:
`evals/ci/run_gate.sh --update-baseline "<why the score changed>"`.

### How the baseline moved from 3/13 to 13/13

The first committed baseline was `3/13`, and the three passing cases were
exactly the three whose entire assertion set was `file-unchanged` — they passed
because the harness did nothing. Cause: a one-line off-by-one in the stub model.
It picked which scripted step to replay by counting `[tool result:` markers in
the conversation, and the daemon seeds every run's context manifest as a
*pseudo* tool result (`[tool result: context.assemble]`,
`crates/codypendentd/src/session_history.rs`) before the model has called
anything. So `count == 1` on the first request, **step 0 of every case was
skipped**, every two-step case replayed only its closing text, and no write ever
happened.

The stub now counts the model's OWN prior calls (`[calling <tool>: …]`
assistant turns — `scripted_step_index`), which no seeded turn can forge. All 13
cases execute their scripted trajectory and the honest score is 13/13: the stub
replays a precomputed correct answer, so anything less than 13/13 means the
harness is broken. Reverting just that one line drops the suite to **0/13** and
the gate fails, naming all 13 cases (run 2026-08-13).

### What's still NOT in CI

Running the corpus against a live daemon and a REAL (or local) model — the
comparison that would actually say something about agent quality — is not
part of CI (no API key, no local model runtime available there) — do it by
hand: `codypendent eval run --suite core --report out.json` after
`codypendent daemon start` and a configured `models.toml`.

## Deferred (named, not faked)

- **The full 50–100 case corpus.** This task ships a real, runnable 13-case
  core suite; growing it further is a separate, large content-authoring
  effort (see above).
- **Citation checking.** `correct_citations` has no wire signal; wiring one
  (an event or artifact carrying a claim → source mapping) is future work.
- **Cost accounting fidelity.** `cost_usd` is read from the last
  `BudgetWarning { dimension: Cost }` event, if any; a run that never emits
  one reports `0.0` — real, not fabricated, but not necessarily the model
  provider's actual invoice.
- **A CLI flag for the regression suite.** `evals/tasks/regressions/` and
  `codypendent_cli::eval::run_regression_suite` are real (see that directory's
  own README), but no `codypendent eval run` flag reaches
  `run_regression_suite` specifically yet; the flag would only add
  `RegressionSuite::evaluate`'s stricter "missing observation counts as
  regressed" rule. See `.impl/proposals/agent-models-from-agent-evals.md`.
  The plain path `codypendent eval run --suite regressions --report out.json`
  does run every case there — verified 2026-08-13, both documented outcomes:
  `FAIL` against a daemon with no `models.toml`, `PASS` against
  `evals/ci/stub_model.py`. **An earlier revision of this file claimed those
  cases "still run fine through the plain `--suite evals/regressions` path
  today". They did not** — that path died on `fixture bundle not found at
  fixtures/tiny-crate.bundle`, which is why the suite moved under
  `evals/tasks/`.
