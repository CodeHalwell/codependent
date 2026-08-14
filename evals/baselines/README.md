# Eval regression-gate baselines

`core.json` is the score history `evals/ci/compare_baseline.py` compares
against — a JSON array, oldest first, each entry `{date, git_sha, note,
total, passed, success_rate, case_results}`. The LAST entry is the current
baseline; every earlier one is kept so the score is traceable release over
release (`git log -p evals/baselines/core.json` reads as the score's own
history).

**The current baseline is 13/13** (2026-08-13), and 13/13 is the only honest
number for this gate: `evals/ci/stub_model.py` replays a *precomputed correct*
trajectory for every case, so a case that fails means the harness is broken, not
that the task was hard. The gate fails on any movement away from this — up or
down — see `evals/README.md`'s "What this gate can and cannot detect".

## History

| # | date | score | what changed |
|---|---|---|---|
| 1 | 2026-08-13 | 3/13 | bootstrap: the first `eval-regression` run against a real binary |
| 2 | 2026-08-13 | 13/13 | the stub's step-selection off-by-one fixed; the three absence-only cases given an assertion that requires work |

Entry 1 was **not a difficulty level, it was a bug**, and it is worth
understanding before trusting any number here. The three cases it recorded as
passing (`diagnose-failing-test`, `ci-diagnosis`,
`explain-average-no-network`) were exactly the three whose entire assertion set
was `file-unchanged` — they passed because nothing happened. Every case that
required the agent to *do* something scored zero, because `stub_model.py` chose
its scripted step by counting `[tool result:` markers and the daemon seeds each
run's context manifest as `[tool result: context.assemble]` before the model has
called anything: step 0 of every case was skipped, so no write ever happened.
The gate was green on that number for a day, because the comparator only failed
on a *drop*.

Both halves are fixed (`scripted_step_index` counts the model's own `[calling
…]` turns; `compare_baseline.py` fails on any difference in either direction,
on a changed set of case ids, and on a corpus that shrank). Reverting just the
step-selection line drops the suite to **0/13** and the gate fails, naming all
13 cases — verified 2026-08-13.

## Re-baselining

To record a NEW baseline after a deliberate, reviewed change — growing the
corpus, rescripting `stub_model.py`, an intentional harness change:

```
evals/ci/run_gate.sh --update-baseline "<why the score changed>"
```

Do not use it to make a red gate green. A drop with no explanation is the
regression this file exists to catch; an *increase* with no explanation is the
one it exists to catch too (a case edited into vacuity and an assertion that
stopped firing both raise the score).

## One historical caveat worth keeping

While this gate was being built, a concurrent change to
`crates/runtime/src/agent.rs` began narrowing which built-in tools are
advertised per run ("retrieval narrowed this run's built-in tool
advertisement" in the daemon log), and `stub_model.py`'s scripted trajectories
assume every tool they call is advertised. Two runs seconds apart once produced
different results for the same single-case suite. That line is still printed on
every run (`offered=28 advertised=15` at the 13/13 baseline, with every tool
the corpus calls still advertised). If a future gate run fails on cases that
call a rarely-advertised tool, check that line in the daemon log before
concluding the harness regressed.
