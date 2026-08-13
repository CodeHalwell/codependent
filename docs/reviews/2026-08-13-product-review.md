# Product Review — 2026-08-13

Reviewed at `535a2f5` (v0.4.5, branch `main`) against twenty target outcomes:
the ten from the [2026-08-11 review](2026-08-11-product-review.md), which were
to be **verified rather than re-designed**, plus ten new ones each building on a
named shipped outcome. Ten parallel reviewers each read every file in one
vertical and, unlike the previous pass, **ran the product** — live daemons,
stub model servers, real pty sessions, hand-written ACP agents, SQLite dumps.
Full vertical reports are in [`2026-08-13-verticals/`](2026-08-13-verticals/).
This document is the synthesis.

`cargo build --workspace --all-features` is clean at the reviewed commit.

---

## 1. The finding

**This is the third consecutive review to reach the same conclusion, and that
— not any individual defect — is the finding.**

- **2026-07-17** (`ac4032a`): *"several roadmap ✅ checkmarks describe an
  implemented algorithm rather than a wired-up feature … the wiring and
  concurrency envelope around the good algorithms has not received the same
  rigor as the algorithms themselves."*
- **2026-08-11** (`df62ef4`): *"The platform is genuinely excellent; the product
  is systematically one wire short of it."*
- **2026-08-13** (`535a2f5`, this review): ten reviewers, working independently
  and told nothing about either prior review, each independently named the same
  shape. Verbatim, from four of them:

  > *"the engine is built, tested and documented; the final wire is attached to
  > the wrong terminal."* (acp-models)

  > *"Everything between the parser and the consumer is a stub with a good
  > comment on it."* (codegraph)

  > *"Validation and measurement are complete; the last consuming call site is
  > missing."* (council-workflow)

  > *"Every number and every identity crosses a seam as an opaque reference, and
  > the reference is where the trail ends."* (daemon-core)

Between the second review and this one, eleven implementation agents were run
against its findings. Per
[`2026-08-11-wip-patches/README.md`](2026-08-11-wip-patches/README.md), **all
eleven were terminated mid-edit when the account hit its monthly spend limit**,
and their patches were preserved rather than merged. v0.4.0–v0.4.5 shipped
after that.

So the honest framing of v0.4.5 is not "a release that was never verified". It
is **a release assembled from eleven interrupted repairs of a diagnosis that
was already correct.** Some of those repairs landed and work. Some landed
halfway. Some never landed, and the roadmap was ticked anyway.

### Why the pattern reproduces

The three reviews did not fail to find the problem. They found it, and it came
back. The mechanism is visible in the code, and it is not carelessness — it is
a scoring rule:

**"Done" is evaluated at the library boundary, and the fix is applied to the
instance rather than to the class.**

The clearest single piece of evidence is one config file. `models.toml` carries
`[[model]]`, `[embedding]`, `[retrieval]`, `[transcription]` and `[speech]`.
Four places write it. Three of them carry a comment describing, precisely, the
bug of serializing it from a model-only struct:

- `cli/src/models_pull.rs:288` — *"would silently delete every one of them,
  disabling a user's voice and retrieval setup"*
- `cli/src/acp_clients.rs:487` — *"silently erased those tables"* (past tense —
  it was a live bug)
- `cli/src/tui.rs:4254` — *"must survive adding a model from the TUI"*

And `cli/src/commands.rs:2894` — `codypendent models add` — did exactly that,
destroying the user's embedding, retrieval, STT and TTS configuration and
printing `added model openai/gpt-4o`. Reproduced live, twice, independently.

The invariant was understood well enough to be written down three times and was
never turned into a shared writer. There is no `write_models_toml`; there are
four copies of the same atomic-write logic. The same shape recurs verbatim:
`anchor_repository_id` exists specifically to make repository identity agree
with the daemon, its doc comment says *"a mismatch here would make the
installed skill silently invisible"* — and it was used at **one of six** sites.

That is the pattern. Not "wires are missing" — *"the fix is applied where the
bug was noticed, never where the bug can occur."* A defect class is closed by
patching its instances, so the class survives and re-emerges at the next site.
Three reviews, three rounds of instance-patching, same diagnosis.

### The second-order effect

Because "done" is scored at the library boundary, **the test suite is the wrong
instrument to detect any of this, and it is a very good test suite** (2,494 `#[test]`/`#[tokio::test]`
attributes under `crates/`, clippy `-D warnings`, cargo-deny). Every defect below is
invisible to it by construction:

- `integrations/src/acp.rs` has 11 tests over `FakeBackend`. The one real
  backend, `DaemonAcpBackend`, has **zero**, and it demanded the wrong reply
  type — so every ACP prompt failed.
- `codypendentd/tests/docs_agent_it.rs:200` asserts on `offered_tool_names` and
  drives the calls with a `ScriptedDriver` — proving dispatch while skipping
  advertisement. The four `docs.*` tools are dispatchable and **absent from the
  catalog the model is shown**.
- `knowledge/tests/semantic_it.rs` is the only caller of `callers_of`,
  `blast_radius` and `tests_covering`. The tests pass; nothing else calls them.

Green tests were never evidence here. That is why this review ran the product.

---

## 2. Scorecard

Verdicts are as measured by the vertical reviewers at `535a2f5`, **before** the
repairs in §4. Outcome 1/7 has no verdict: that reviewer had not reported when
this document was written (see §5).

| # | Outcome | State | Verdict |
|--:|---|:--:|---|
| 1 | Polished TUI | — | not reported (§5) |
| 2 | ACP + auto model discovery | PARTIAL | discovery works on a real wire — 3 models found, profiles written, live prompt turn completed; **serve mode failed every prompt** |
| 3 | Model selection + prefilled lists | PARTIAL | 42-provider/387-model catalog real and wired to TUI + `models add`; Anthropic unreachable in TUI and mis-wired in CLI; no `--model` on `run`; every "1M" context value overstated |
| 4 | Skill-writer + doc-writer | BROKEN | `docs.*` implemented, dispatchable, and never advertised to the model; no skill-writer of any kind |
| 5 | DAG viewer (user + agent) | BROKEN | flat paginated edge table, not a DAG; empty from any subdirectory; no `graph.*` tool exists |
| 6 | AI council | PARTIAL | genuinely convenes — concurrent members, distinct models, fair-share dossier, chair synthesis; cost undercounts ~2×; member failure reason discarded |
| 7 | Rich chat stream | — | not reported (§5) |
| 8 | TTS + STT | PARTIAL | STT proven end to end against a configured endpoint; **zero audio crates in the workspace** — nothing is "built-in"; TTS has no privacy gate |
| 9 | Vector top-k selection | PARTIAL | funnel runs and its cards reach the model, but **additively** — all 21 built-in tools are injected every step, byte-identical across unrelated objectives |
| 10 | Blackboard + kanban + NL backlog | PARTIAL | both real and distinct; board worked end to end in a pty; board identity keyed on an uncanonicalized caller path |
| 11 | Live measured routing | PARTIAL | router reads a store whose only writer is a one-shot local bench; per-task-class table always empty |
| 12 | Executable skills (WASM) | PARTIAL | **WASM absent** (no runtime dependency); OS sandbox genuinely built and enforcing; nothing calls the skill-script runner |
| 13 | Hook engine | ABSENT | a `hook.toml` spec and a bare enum label; no parser, no dispatch, no interceptor |
| 14 | Live code graph | ABSENT | `watch()` is complete, documented, and **constructed by nobody**; graph refreshes only on git HEAD change |
| 15 | Delegation | PARTIAL | worktree isolation, attributed blackboard and per-node ledger all real; frontier executes **strictly sequentially**; no merge-back |
| 16 | Evals as a product loop | PARTIAL | harness runs end to end; **scored 3/12 PASS on runs that never executed**; CI runs eval unit tests, not evals |
| 17 | Compounding memory | PARTIAL | genuinely persists across sessions with real provenance; no delete, no edit, no protocol command; the store that *has* decay/promotion/contradiction has no reader |
| 18 | Docs round-trip | PARTIAL | publish really commits and records; PR path pushes before checking it can open a PR; **merge readback absent down to the schema** |
| 19 | Real multi-user | ABSENT | **and the trust boundary is open** — see §3 |
| 20 | The ledger made visible | BROKEN | every event type *does* have a consumer; the **numbers** are not in events at all |

---

## 3. What must be fixed before outcomes 12, 13, 15 and 19 are built

The brief requires a written threat model for each of those four, because each
widens what untrusted input can reach. That work cannot start, because **there
is currently no boundary to widen.**

`daemon-core` demonstrated, on the wire, against a live daemon:

- **There is no principal on a connection.** `client_id` and `ClientRole` are
  both asserted by the client in plaintext (`daemon/src/server.rs:1195`,
  `:1275`); the connection defaults to `Controller` (`:630`). `UserId` is
  manufactured from the client's own UUID.
- A fresh client that had never attached to anything **read another session's
  entire event history**, including its full context manifest
  (`ReadSessionEvents`, `server.rs:2267` — no gate).
- The same client **approved another client's parked `ls -la`, and it
  executed**. That is an approval-gate bypass.
- The daemon's *plugin* projection path re-derives ownership on every by-id
  read (`authorize_session_resource:3697`, `authorize_workflow_resource:3707`).
  The *client wire* path re-derives nothing. Two read surfaces over the same
  resources, opposite discipline.

The whole trust boundary today is the 0700 mode on the socket's directory — and
that mechanism was itself actively destructive: `ensure_directories` chmodded
`socket_path.parent()` unconditionally, so following the product's own advice
(`SocketPathTooLong`: *"Set CODYPENDENT_SOCKET to a shorter path (for example
under /tmp)"*) made the machine's shared `/tmp` private to one user.

**Outcome 19 is not "add presence and shared boards to the existing scopes".**
It is "there are no enforced scopes yet". Sequencing 12/13/15/19 before an
authenticated principal exists would build four new untrusted-input surfaces on
top of an open door. That is the one hard blocker this review found.

---

## 4. Repairs landed in this pass

Each was verified against the running product, and each regression test was
confirmed to **fail against the previous behaviour** before being kept.

| Fix | Class | Evidence |
|---|:--:|---|
| ACP serve answered every prompt with an error — `AttachSession` replies `Catchup`, never `CommandAccepted`, and the ACP bridge was the only caller demanding the latter (`cli/src/acp.rs`) | (c) | the `run`/`attach` clients already had the right contract in `expect_catchup` |
| Eval scored passes for runs that never executed; cases 002/005/007 are built entirely from `file-unchanged` (`eval/src/case.rs`) | (c) | `RunObservation.run_completed`; `evals/README.md` records dropping two *other* absence-only kinds for exactly this reason |
| Repository identity re-derived from the CWD at five sites, emptying documents, memories, learnings and graph edges from any subdirectory (`cli/src/tui.rs`, `commands.rs`) | (c) | `docs list` from `repo/src` now returns the document instead of "No documents yet." |
| The daemon chmodded borrowed socket directories, including a shared `/tmp` (`protocol/src/discovery.rs`) | (c) | 0700 now applies only to directories we created or own |
| Three spellings of one checkout minted three separate task boards (`codypendentd/src/blackboard.rs`) | (c) | without the fix the card is invisible through the plain spelling — 0 of 1 |

The `models add` config-clobber (§1) is **not** in this list. Fixing it at
`commands.rs:2894` alone would reproduce the exact per-site-patching reflex this
review identifies as the root cause; it needs the single shared writer named in
[`2026-08-13-shared-surface.md`](2026-08-13-shared-surface.md), which is a `cli-owner`
change.

---

## 5. Scope actually completed, and what remains

This review completed **Phase 1** (parallel vertical review, ten of eleven
verticals reporting) and a first tranche of **Phase 2** repairs. It did **not**
complete Phase 2 for the seventeen remaining outcomes, Phase 3 (adversarial
integration pass over the seams), or Phase 4 verification of anything not
listed in §4.

Specifically not done:

- **Outcome 1 and 7 have no verdict.** The TUI reviewer had not reported. Every
  statement about the TUI in this document comes from other verticals touching
  it in passing, and outcomes 1 and 7 should be treated as unassessed.
- **No new outcome (11–20) was built.** The brief forbids building on an
  unverified base; verification consumed the run, and §3 is a hard blocker for
  four of the ten.
- **No adversarial integration pass.** The shared-surface ownership map was
  written before any code changed, and the changes above touch five files
  across four crates, but nobody has diffed the seams against what each vertical
  expected.

Environment note, because it shaped the evidence: the container has 4 CPUs and
filled its 252 GB filesystem to 100% twice under ten concurrent reviewers
(`target/` peaked at 27 GB). Two reviewers abandoned probe builds mid-pass and
substituted weaker evidence, which they labelled as such. Their reports say
where.

---

## 6. Recommended sequence

1. **Authenticate the connection** (§3). One principal, established
   server-side, before anything else. Every downstream `UserId` already derives
   from that one value, so the change is small and it unblocks 12/13/15/19.
2. **Gate by-id reads the way by-list reads are gated.** The daemon already has
   the correct discipline in its plugin path; apply it to the client wire path.
3. **Collapse the four `models.toml` writers onto one helper**, then fix
   `models add`. Fix the class, not the instance.
4. **Make the registry the source of truth for what is offered** (outcome 9).
   The fix is not better ranking — it is that `advertised_tool_definitions`
   must consume the funnel's output instead of a hard-coded vec.
5. **Add the `graph.*` tools** (outcome 5, then 14). Four tested functions,
   named in their own doc comments as the tools they were meant to be, waiting
   for a tool module.
6. **Arm the watcher** (outcome 14) — a `tokio::spawn` of a function that is
   already written, tested, and called by nothing.

Items 4–6 are single call sites. That remains the good news the previous review
reported, and it is still true. What has changed is the reason to distrust it:
this exact list of cheap fixes has been written twice before.
