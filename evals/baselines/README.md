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

To force a NEW baseline later (after a deliberate, reviewed score change —
growing the corpus, rescripting `stub_model.py`, an intentional harness
change): `evals/ci/run_gate.sh --update-baseline "<why the score changed>"`.
