# The baseline this review must be read against

Discovered during orchestration, not supplied in the brief. It changes what
"largely unverified" means and it is the key to the synthesis.

## What happened before the pinned commit

`docs/reviews/2026-08-11-product-review.md` — a review of **the same ten
outcomes**, at `df62ef4` (v0.3.2), 2026-08-11, run the same way: eleven
parallel reviewers, one per vertical, plus one researching provider catalogs.

Its verdict, verbatim:

> **The platform is genuinely excellent; the product is systematically one wire
> short of it.** … across all ten outcomes the same failure shape recurs: **an
> engine is built, tested, and documented — and the final wire connecting it to
> the model or the user was never attached.**

Its scorecard: 1:~70% · 2:~55% · 3:~60% · 4:~10% · 5:~35% · 6:~65% · 7:~60% ·
8:~5% · 9:~30% · 10:40/0/0%.

Then, per `docs/reviews/2026-08-11-wip-patches/README.md`:

> Eleven implementation agents were run in parallel isolated worktrees … **All
> eleven were terminated mid-edit when the account hit its monthly spend
> limit** — each stopped partway through a file, typically just before adding
> tests. … **Do not merge these blindly.** … Treat each as "someone's editor
> buffer at the moment the power went out".

Eleven `.patch` files were preserved. Then v0.4.0 → v0.4.5 shipped. That is the
"one large release" the current brief describes as built and never exercised.

And an EARLIER review, `docs/PROJECT_REVIEW_2026-07-17.md`, had already named
the same thing:

> several roadmap ✅ checkmarks describe an implemented algorithm rather than a
> wired-up feature … the wiring and concurrency envelope around the good
> algorithms has not received the same rigor as the algorithms themselves

**Three consecutive reviews, one month apart, found the same pattern.** That is
the finding. Not any individual missing wire.

## Why the reviewers were NOT told this

Deliberate. If a vertical reviewer independently rediscovers a baseline defect,
that defect certainly still exists at v0.4.5 — no confirmation bias, no
patch-reading shortcut. The cross-reference against this baseline is done here,
in synthesis, after their reports land.

## Orchestrator spot-checks of the baseline's Critical findings

| Baseline Critical | Status at 535a2f5 | Evidence |
|---|---|---|
| Assembled context never reaches the model; system prompt is one sentence | **FIXED** | `codypendentd/src/executor.rs:1563` `build_run_seed` inserts `context_turn(&manifest)` at position 0; the doc comment cites "2026-08-11 review item 1". The one-sentence `SYSTEM_PROMPT` (`runtime/src/agent.rs:386`) remains, but the manifest now arrives as a seed turn ahead of the objective. |
| No path exists to create a document | **FIXED** | `CommandBody::CreateDocument` exists at `crates/protocol/src/command.rs:216` with round-trip tests. |
| Codegraph query layer has zero production callers | **NOT FIXED** | See F-ORCH-5. `callers_of`/`blast_radius`/`tests_covering` at `knowledge/src/codegraph.rs:576,602,618` still have exactly one caller each, in `tests/semantic_it.rs`. |

So the release landed real fixes — this is not "nothing works". The task is to
establish, per outcome, which wires got attached, which did not, and which got
attached wrongly. Vertical reports supply that; this file supplies the frame.

## The recurrence at micro scale

F-ORCH-1/F-ORCH-4: `models.toml` is clobbered by `models add`
(`cli/src/commands.rs:2894`) while the other three writers of the same file all
carry comments describing that exact bug and all guard against it. The fix was
applied three times as three separate incidents and never once as an invariant
— no shared writer, four copies of the same atomic-write logic. The same
scoring error that produces class-(b) defects at feature scale produces this at
function scale: the work is judged done when the instance in front of you
passes, not when the class of defect is closed.
