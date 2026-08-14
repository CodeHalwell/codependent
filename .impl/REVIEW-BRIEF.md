# Reviewer brief — 2026-08-13 (round 4), read this first

Repo `/home/user/codypendent`, **pinned commit `c255bec8b175d62942b3312cff2335b97d43a59a`**
(v0.5.1, branch `claude/review-repair-twenty-outcomes-5fynno`). Do not check out
anything else. Do not commit, do not push, do not edit code — this is a review
pass only. Your deliverable is a written report.

## Environment — this is the binding constraint, obey it

4 CPUs, ~30 GB free disk, and up to a dozen of you run at once. A previous
round filled the disk TWICE with parallel cargo invocations.

- The orchestrator is building the workspace right now. The binaries
  `target/debug/codypendent` (CLI + TUI) and `target/debug/codypendentd`
  (daemon) will appear within ~15 minutes. **Use those.** Start by reading
  code; poll for the binaries with
  `until [ -x target/debug/codypendent ]; do sleep 20; done`.
- **NEVER** run `cargo build --workspace` or `cargo test --workspace` — you
  will race the orchestrator's build and fill the disk.
- If you must run a crate's tests, run exactly one crate and always prefix:
  `CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 cargo test -p <crate>`
  A debug test binary here is 400-600 MB; with that prefix it is ~40 MB.
- **NEVER** `cargo clean`, never delete `target/`.
- On "No space left on device": stop, say so in your report, do not fight it.
- Put scratch files under `/tmp/review-<your-vertical>/`, never in the repo.

## What this review is

The product is measured against twenty target outcomes (in your task prompt).
Outcomes 1-10 shipped and were repaired in the previous round; 11-20 were built
in that same round. **Everything is now suspect for the same reason: it was
built and repaired fast, and green tests were never evidence here.**

Three consecutive prior reviews reached the identical conclusion, which is
recorded in `docs/reviews/2026-08-13-product-review.md`:

> *"the engine is built, tested and documented; the final wire is attached to
> the wrong terminal"* — and the root cause: **"done" is scored at the library
> boundary, and the fix is applied to the instance rather than to the class.**

Read that synthesis (§1 and §2) and your vertical's prior report in
`docs/reviews/2026-08-13-verticals/` before you start. They tell you where the
bodies were buried last time. **They do not tell you the current truth** — the
repairs claimed there may be real, partial, or wrong. Verify, do not assume.

## The review question

For every outcome in your vertical the question is **not** "does the code
exist" — it does. It is **"what happens when a user actually does this."**

So run it. Start the daemon. Drive the CLI. Drive the TUI in a pty. Write a
stub server and point the product at it. Query the SQLite file afterwards and
check the row is really there. **Treat "tests pass" as no evidence.**

Read **every** file in your vertical. Do not sample.

## Classify every gap

- **(a)** engine missing entirely
- **(b)** engine built, tested, documented — final wire never attached
- **(c)** wire attached, wrong behaviour

**(b) is the cheapest and highest-value class. Hunt it deliberately**: for every
capability, find its producer, then find a real consumer on a path a user can
reach. "Called only from its own tests" is category (b), and it is a finding.

## Hunt these patterns specifically — they hide well

1. **Silent filters.** Status gates, scope filters, capability checks that drop
   items and report nothing. Anything that returns "not found" or an empty list
   where the honest answer is "filtered". Grep for every `.filter(`, `WHERE`
   clause and `match … => continue` on a status/scope/kind dimension and ask
   what the user sees when it drops everything.
2. **Data produced but never consumed.** Assembled, rendered into a trace,
   dropped. Follow every producer to a real consumer.
3. **Trust-boundary reads.** Any code that acts on metadata supplied by a
   caller rather than re-deriving it from what the server stored. Any by-id
   fetch whose sibling list path filters by scope but which does not.

## Evidence rules

- Every finding needs `file:line` **and** the user-visible consequence.
  *A defect nobody can observe is not a finding.* Drop it.
- Quote the actual command you ran and its actual output. Not a paraphrase.
- If you could not run something, say so explicitly — do not imply you did.
- Verify every number, version and count you repeat from a doc. Delete any
  number you cannot compute yourself.
- Distinguish **what you observed** from **what you inferred from reading**.
  Mark inferred findings as such. An overstated report is worse than a short one.

## Return format

Write your full report to `docs/reviews/2026-08-13-r4-verticals/<vertical>.md`
(create the directory if needed; that file IS your deliverable and may be long).

Then return, under 900 words:
1. **Verdict per outcome** you own: WORKING / PARTIAL / BROKEN / ABSENT, one
   line of evidence each.
2. **Top findings**, ranked by user-visible consequence, each with file:line and
   its (a)/(b)/(c) class.
3. **The pattern** — what these findings have in common. One paragraph. This
   matters more than the list.
4. **"What I did not verify"** — every claim you are making from reading rather
   than running, and why.
