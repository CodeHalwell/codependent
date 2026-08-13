# Eval regression-gate baselines

`core.json` is the score history `evals/ci/compare_baseline.py` compares
against — a JSON array, oldest first, each entry `{date, git_sha, note,
total, passed, success_rate, case_results}`. The LAST entry is the current
baseline; every earlier one is kept so the score is traceable release over
release (`git log -p evals/baselines/core.json` reads as the score's own
history).

**Ships empty (`[]`) — deliberately, not an oversight.** This gate needs a
real `codypendent` binary built from the current tree, plus
`evals/ci/stub_model.py` actually running, to produce a real number; nothing
in this task's environment could produce one that was ACTUALLY measured
against the tree as merged (see the task report — the shared working tree
this was built in had multiple other agents mid-edit throughout, so any
number computed locally would have been measured against a moving,
sometimes-non-compiling target, not the tree this ships in). Rather than
write down a guessed or stale figure, `evals/ci/run_gate.sh` bootstraps: the
first time the `eval-regression` CI job runs against a real, merged,
compiling tree, it establishes the baseline from that real run and passes;
every run after that is a genuine comparison. This is a one-time path,
not a standing escape hatch — see that script's own comment.

**A live run WAS captured and then deliberately discarded — worth knowing
before you assume `[]` means "never tried".** Late in this task, once the
tree briefly compiled, `evals/ci/run_gate.sh` ran for real and scored
3/13. It is not committed here because, in the same session, a concurrent,
unrelated change (`crates/runtime/src/agent.rs`'s "retrieval narrowed this
run's built-in tool advertisement" — a different outcome's work landing
mid-session) started narrowing which tools get advertised to the model per
run, and `stub_model.py`'s scripted trajectories assume every tool it calls
is always available. Two consecutive runs, seconds apart, produced
DIFFERENT results for the identical single-case suite (`file-changed`
true, then false) purely from this — evidence the number was not yet a
stable baseline, not evidence of a bug in `codypendent_cli::eval::
run_worktree_root` (the worktree-inspection fix itself checks out
separately and directly: see `run_worktree_root_matches_the_daemons_own_
layout` in `crates/cli/src/eval.rs`, which cross-checks the reconstructed
worktree path against a REAL `WorktreeManager::allocate` call and passed).
Whoever next runs this gate on a settled tree should expect `stub_model.py`
may need updating to work with narrowed tool advertisement — check
`codypendent eval run`'s daemon log for that "narrowed" line before
assuming a low score means the harness regressed.

To force a NEW baseline later (after a deliberate, reviewed score change —
growing the corpus, rescripting `stub_model.py`, an intentional harness
change): `evals/ci/run_gate.sh --update-baseline "<why the score changed>"`.
