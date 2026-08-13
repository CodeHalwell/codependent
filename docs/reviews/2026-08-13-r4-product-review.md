# Product Review — 2026-08-13 (round 4)

Reviewed at **`c255bec8b175d62942b3312cff2335b97d43a59a`** (v0.5.1, branch
`claude/review-repair-twenty-outcomes-5fynno`) against the twenty target
outcomes: ten shipped-and-repaired in the previous round, ten built in that
same round. Fourteen reviewers each read one vertical in full and, per the
brief, **ran the product** — live daemons, stub model servers, pty-driven TUI
sessions with a VT emulator, hand-written ACP clients, two real OS principals,
adversarial WASM modules, and direct SQLite reads after every run. Vertical
reports are in [`2026-08-13-r4-verticals/`](2026-08-13-r4-verticals/).

`cargo build --workspace --all-features` is clean at this commit.
**CI is not.** The `test` job fails on `main` at this exact SHA (run
[31711683814](https://github.com/CodeHalwell/codypendent/actions/runs/31711683814));
the identical tree passed on the PR branch an hour earlier. See §5.

---

## 1. The finding

The previous three reviews each concluded *"the fix is applied to the instance
rather than the class."* That diagnosis was correct and it was acted on. This
round's finding is **why it did not stick**, and it is not carelessness:

> **The class was defined by the symptom, not by the invariant. So each repair
> generalised exactly as far as the reproduction that motivated it, and stopped
> at the edge of the file where the bug had been demonstrated.**

The control case proves this is a real mechanism and not a slur. Round 3 found
`models add` destroying `[embedding]`, `[retrieval]`, `[transcription]` and
`[speech]`. There, the class was drawn at the *invariant* — "this file has one
writer" — and a single `crates/cli/src/models_file.rs` was built. **That fix
held completely.** Three reviewers independently tried to break it this round:
five tables plus an invented future table survived `models add`, `acp connect`,
`acp disconnect` and the TUI remove path. When the invariant is named, the
repair is permanent.

Now the counter-case, and it is the largest single defect class in the product.

### 1.1 One fact, six derivations, and every disagreement reported as absence

**"Which repository is this?"** is re-derived independently at six call sites
from four different inputs:

| # | site | derives identity from | user-visible result |
|---|---|---|---|
| 1 | `runtime/src/agent.rs:4356,4391,4412,4446` (`docs.*`) | the run's **worktree** | agent writes a document; `docs list` says *"No documents yet."* — **permanently**, the worktree is deleted |
| 2 | `runtime/src/agent.rs:4282` (`graph.*`) | the run's **worktree** | every code-graph question in the default mode answers *"no results"* |
| 3 | `cli/src/commands.rs:659` (`skill add`) | the **package directory** | installed skill is never disclosed by retrieval |
| 4 | `codypendentd/src/lib.rs:338-352` (boot scan) | the **daemon's cwd** | a third identity for the same skills root |
| 5 | `cli/src/tui.rs:3068,3101` (board) | the **opened directory** | `repo/` → 6 cards, `repo/src` → **0 cards**; a card made from `src/` is invisible from the root forever |
| 6 | `codypendentd/src/blackboard.rs:88` (relative paths) | the **daemon's cwd** | `repository="."` over ACP silently writes to the daemon's own board |

Round 3 fixed **one** of these — `skills.search` at `agent.rs:4249-4252` — and
left in place a detailed comment explaining the worktree trap. Three hundred
lines below it, `docs.*` and `graph.*` still pass the worktree. The comment is
the proof that the mechanism was understood at the moment of the fix and was
not turned into an invariant.

**Why this survived three reviews and a 2,700-test suite:** every one of these
six failures is reported as an **empty result**, never as an error. *"No
documents yet."* *"no results."* *"0 cards."* *"not among the skills this
search disclosed."* An empty list is a legitimate answer to a legitimate
question. No assertion can distinguish "nothing matched" from "I looked
somewhere else", no user can file it as a bug, and the only way to find it is
to run the product while already knowing what the answer should have been.
That is precisely what this round did, and six reviewers hit the same root
cause from six different verticals without coordinating.

### 1.2 The same rule, seen from the other side: nothing flows back

The second half of the round's findings is the mirror image. Value flows
forward to a consumer that is assumed; **errors, absences, refusals and
results do not flow back at all.**

- `RunUsage` is measured, stored in `runs.*`, added to the protocol, given a
  golden vector, a TypeScript type and five unit tests — and has **no reducer
  arm**, so the TUI prints the literal words **`? unsupported event`** into the
  chat transcript at the end of every run while the header still shows `cost: —`
  (`tui/src/reduce.rs:1992-2000`).
- Five memory commands (`InspectMemory`/`CorrectMemory`/`ForgetMemory`/…) with
  five handlers, a migration and integration tests — all work when driven
  directly, and **zero clients send them** (`protocol/src/command.rs:600-649`).
- Delegation's whole point — three isolated workers' patches consolidated into
  one correct 750-byte diff — and **no command in the product can fetch it**
  (`workflow_exec.rs:1864`; `CommandBody` has `PutArtifact` and no get).
- Hooks parse, and the three verbs work in-library; a planted hook produced
  `hooks 0 / hook_dispatches 0` against a live daemon.
- `SkillRunner` (the WASM host, whose ceilings genuinely bite) has **no
  production caller** — `policy_gate.rs:22-29` says so itself.
- A *failed* ACP run is reported to the client as `{"stopReason":"end_turn"}`,
  the reason discarded (`cli/src/acp.rs:250-255`).
- Every transient status notice — including every voice error — is discarded by
  a render precondition once a session has run anything (`tui/src/render.rs:2747`).
- `ReleaseOutcome` is computed and dropped, so failed workers leave orphan
  worktrees and branches and the user is told nothing.

Producer and consumer both exist in almost every case. What is missing is the
return path. The honesty is real and local — `policy_gate.rs` says "no guest
currently runs", `hook.rs` says "no hook can fire", `blackboard.rs` says "not
wired to a wire command yet" — but **"done" is scored one layer above where
that honesty is written**, so "engine + threat model + tests" reads as a
shipped outcome.

### 1.3 The instrument that measures this was itself never calibrated

Outcome 16 is supposed to be the mechanism that stops all of the above.

`evals/ci/run_gate.sh` scores **3/13** and prints `eval regression gate: PASSED`.
The three passing cases are exactly the three whose entire assertion set is
`file-unchanged` — they pass if the harness does nothing at all. The cause is a
one-line off-by-one: the stub model picks its scripted step by counting
`[tool result:` markers, and the daemon seeds the context manifest as
`[tool result: context.assemble]` on the first call, so **step 0 of every case
is skipped** (`evals/ci/stub_model.py:641-646`). And the comparator only fails
on a score *drop* (`compare_baseline.py:104-153`) — a reviewer deleted the 10
failing cases and the gate reported `success rate 1.0000 … PASSED, EXIT=0`.

So the product's own regression gate is green, has been green, and is measuring
a model that never writes a file.

---

## 2. Scorecard

As measured by the vertical reviewers at `c255bec`, **before** the repairs in §4.

| # | Outcome | State | Verdict |
|--:|---|:--:|---|
| 1 | Polished TUI | PARTIAL | Help alignment/truncation and the first-run `/` genuinely repaired; palette, model picker, `/keys` and theme picker render as **empty boxes** at 10 rows; every status notice invisible for the life of any session with a run; losing the daemon dumps a 36-frame backtrace over the terminal |
| 2 | ACP + auto discovery | PARTIAL | `acp serve` answers a real prompt (round 3's headline bug fixed); a **failed** run returns `end_turn` with the reason dropped; `session/new`'s required `cwd` ignored; cancel records `Failed` and poisons memory |
| 3 | Model selection | PARTIAL | 42 providers / 386 models (computed); `--model` exists and works; 6 providers unusable and unmarked; **`claude-opus-4-5` ships a 5× context window** (1M vs 200K) — 190K of context displays as 19% used |
| 4 | Skill-writer + doc-writer | PARTIAL | both exist and are advertised; the agent's document is written into a **throwaway worktree** and orphaned forever; the skill-writer's own printed promotion instruction produces an invisible skill |
| 5 | DAG viewer (user + agent) | BROKEN (agent) | `graph.*` answers correctly in read-only modes and returns **"no results" for everything in the default Build mode**; still a flat alphabetical edge table, not a viewer; `--accessible` renders zero rows |
| 6 | AI council | PARTIAL | genuinely convenes — 3 concurrent members, distinct models, chair synthesis; **cost understated exactly 2×**, and the extra spend goes to an unchosen model |
| 7 | Rich chat stream | **WORKING** | round 3's headline defect fixed and driven: `Alt-↑`/`Alt-Enter` walk and expand folds across turns; markdown, tool cards, virtualization all real. One defect: tables laid out to a fixed 100-col budget |
| 8 | TTS + STT | PARTIAL | both directions work against a configured remote endpoint; **zero audio crates in the workspace** — nothing is built-in; every voice error is invisible; `local = true` disables the privacy gate unverified and `doctor` blesses it |
| 9 | Vector top-k selection | **WORKING** | the "instead of" now holds — two unrelated objectives advertised different 15-tool sets from 28 offered, floor excluded from the budget. Caveat: the "embedding" half is a character-trigram hash; a configured embedding model is ignored |
| 10 | Blackboard + kanban | PARTIAL | board works end to end; path-spelling identity fixed (5 spellings → 1 board); **the subdirectory axis still forks the board**; the NL backlog tools are silently withheld by the top-k funnel |
| 11 | Live measured routing | PARTIAL | **the loop genuinely closes** — a model won on bench numbers, failed a real run, and every subsequent task of that class went elsewhere; but the catalog's price still feeds the decision, latency/cost are never re-measured, and memory extraction bypasses the router entirely |
| 12 | Executable skills (WASM) | PARTIAL | `wasmi 0.51.5` is real and the ceilings bite (killed at 2.03s, capped at 241/256 pages, output truncated at 1 MiB); **nothing constructs a `SkillRunner`**; the wall clock is **not enforced against a guest that loops on host calls — 167× overshoot, returning `Ok`**; a package self-promotes to first-party trust with one TOML line |
| 13 | Hook engine | BROKEN | a planted hook against a live daemon produced `hooks 0 / hook_dispatches 0`. The three verbs work in-library only. Privilege escalation could **not** be broken — all four attempts refused |
| 14 | Live code graph | PARTIAL | **works for a human editor** — uncommitted edit visible to the very next tool call, measured; robust under rename/delete/burst with no debounce drop; but the agent's own writes land in a worktree outside every watch, so the agent cannot see its own edits |
| 15 | Delegation | PARTIAL | concurrency (3 workers, 4.64s vs 8.70s at cap 2), worktree isolation and per-worker board attribution all **real and measured**; the consolidated diff is correct and **unreachable by any client** |
| 16 | Evals as a product loop | BROKEN | gate scores **3/13** and prints `PASSED`; the 3 are the vacuous ones; the comparator passes when 10 cases are deleted; "a prompt edit that lowers the score fails CI" is impossible — the stub reads neither |
| 17 | Compounding memory | PARTIAL | persists and is retrieved across sessions with real provenance; **inspect/edit/delete have zero clients**; two contradictory user statements both stay `active` at 0.95 |
| 18 | Docs round-trip | PARTIAL | the PR handle is really persisted and `pr_merged` really is updated by a poller; **nothing above SQLite reads it**, and a draft unmerged PR sets status `published` |
| 19 | Real multi-user | PARTIAL | **the open trust boundary is genuinely closed** — verified cross-uid: foreign principal reads nothing, resolves nothing, and refusals are byte-identical to "does not exist"; `PublishDocument` still skips the owner check (cross-uid confused-deputy + enumeration oracle); org/workspace scopes are not enforcement axes |
| 20 | Ledger made visible | PARTIAL | 3 of 6 classes reach a user (tool traces, denials, classification); **tokens render as `? unsupported event`**; cost is never computed on the agent path at all |

Two outcomes moved to WORKING this round (7 and 9), and three of the largest
engines — the trust boundary (19), routing's measured loop (11) and
delegation's concurrency and isolation (15) — are genuinely built and were
measured working. This is not a failing product. It is a product whose last
hop is unbuilt in twelve places.

---

## 3. What is worth fixing first, and why

Ranked by (user-visible harm × cheapness), preferring the **class** over the
instance in every case.

1. **Repository identity (§1.1).** One invariant, six sites, four user-visible
   bugs including silent permanent data loss. Fix the class: one authoritative
   accessor, every site through it.
2. **The WASM wall clock (`sandbox/src/wasm.rs:612-621`).** A declared limit
   that is not enforced is worse than an undeclared one, and outcome 12's whole
   claim is "enforced, not advisory".
3. **First-party trust self-promotion (`manifest.rs:437-438`).** One TOML line
   in an untrusted package buys the trust tier that suppresses prompt-injection
   labelling.
4. **`PublishDocument`'s missing owner gate (`server.rs:1756-1815`).** Its two
   siblings have it; this is the retrofit's missed arm.
5. **The eval gate (§1.3).** Until it measures, nothing else can be defended.
6. **`? unsupported event` (`reduce.rs:1992`).** Visible garbage in every run.
7. **The Anthropic context window.** A 5× overstatement drives silent truncation.
8. **CI red at HEAD (§5).**

---

## 4. Repairs landed in this pass

See [`2026-08-13-r4-repairs.md`](2026-08-13-r4-repairs.md) — written after the
implementation pass, with the exact commands run and the honest list of what
was left unfixed.

---

## 5. CI is red at the reviewed commit

`codypendent-codypendentd::workflows::tests::pause_and_resume_publish_the_persisted_prompt_phase_immediately`
(`crates/codypendentd/src/workflows.rs:1456`) fails on `main` at `c255bec` and
passed on the identical tree one hour earlier.

It is a test defect, not a product defect, and the mechanism is documented in
the product's own comment: node transitions are **persist-before-publish**
(`workflows.rs:915-917`). The test reaches its starting point by *polling the
database* (`wait_for_node_state:1817`), so it can subscribe inside the window
after the row is written and before the corresponding `NodeTransitioned` is
published — and then asserts that the *first* event it receives is the phase
change. A client that merges by kind is unaffected; the consequence is that CI
is red and a contributor cannot tell a real regression from this flake.

---

## 6. Corrections to the previous round

Round 4 reviewers falsified two of round 3's published claims. Both are
recorded here because a review that only accumulates findings is not a review.

- **"Every '1M' context value is overstated by 4.8–5%."** False for Gemini,
  DeepSeek, Kimi, GLM and MiniMax — 24 of 24 curated OpenRouter rows match
  `context_length` exactly; `1048576` is what OpenRouter itself reports. The
  round-3 "fix" edited rows that were already correct. The real context defect
  is a different one and 5× larger (`claude-opus-4-5`, §2 row 3).
- **"Migration files are immutable, verified byte-for-byte"**
  (`docs/cli-and-tui-user-guide.md:53-57`, booked as a fix in
  `docs/releases/v0.5.1.md:21`). Falsified: published tags `v0.1.0-build.43/44/45`
  ship `migrations/0003_phase2.sql` with a different hash than HEAD, and
  `migrations/README.md:9-11` says so itself. A correct safety warning was
  deleted and replaced with a false all-clear.
