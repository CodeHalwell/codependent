# Product Review — 2026-08-11

Reviewed at `df62ef4` (v0.3.2, branch `main`) against the owner's ten target
outcomes for the product. Ten parallel reviewers each read every file in one
vertical (TUI · ACP/MCP · providers/routing · knowledge/retrieval · Docs
Studio · workflow/council/blackboard · runtime agent loop · daemon/protocol/
platform · remote-UI SDK/frontends · docs-vs-reality), and an eleventh
researched live provider model catalogs on the web. Full vertical reports are
in [`2026-08-11-verticals/`](2026-08-11-verticals/); this document is the
synthesis. `cargo check --workspace --all-targets` is clean at the reviewed
commit.

**The ten target outcomes (the rubric):**

1. Beautiful, well-formatted, easy-to-use TUI — every menu polished
2. ACP fully working, including automatic model discovery from ACP agents
3. Easy model selection; prefilled model lists for non-ACP providers (e.g. Nebius)
4. Agent skill-writer and doc-writer
5. Fully functional DAG viewer for code-context management, accessible to user and agent
6. Fully functional AI council
7. Rich chat stream
8. Built-in TTS + STT for immersive interaction
9. Vector top-k tool/skill selection (keyword + embedding) instead of injecting all descriptions
10. Built-in blackboard + kanban board; natural-language backlog tools

## 1. Verdict

**The platform is genuinely excellent; the product is systematically one wire
short of it.** The daemon core (event-sourced ledger, crash-consistent command
path, durable approvals, deny-first policy, recovery matrix, version-negotiated
protocol with golden vectors), the workflow engine, the remote-UI plugin
pipeline (signing → sandbox → smoke-test → paint), the theme system, and the
test culture (~1,900 test functions; clippy `-D warnings`; cargo-deny) are all
at a maturity well beyond a v0.3.2.

But across all ten outcomes the same failure shape recurs: **an engine is
built, tested, and documented — and the final wire connecting it to the model
or the user was never attached.** The five most consequential instances:

1. **The knowledge fabric never reaches the model.** `assemble_context` runs a
   real retrieval funnel (repo map, top-k skill cards, curated memories) and
   renders a beautiful manifest — as a `NoteAppended` trace event the human
   sees and the LLM never does (`codypendentd/src/executor.rs:1161-1169`;
   `session_history.rs:121-173` never projects notes into the transcript). The
   model's entire system prompt is one hardcoded sentence
   (`runtime/src/agent.rs:3863-3866`). Phase 2's own exit criterion
   ("recall@8 = 1.0, top-k beats full injection") measures a code path
   production doesn't use.
2. **ACP model discovery data is received and discarded.**
   `NewSessionResponse.config_options` — where current ACP carries the agent's
   model list — is dropped on the floor (`integrations/src/acp_client.rs:481-493`).
   The pinned SDK (agent-client-protocol 2.0.0) already exposes every type
   needed, including `session/set_config_option` to switch model.
3. **The Docs Studio browses a set that can never be non-empty.** There is no
   `CreateDocument` command, CLI, or TUI action anywhere — `DocumentStore::create`
   has zero production callers. The CRDT engine, leases, suggestion rail, and
   approval-gated publish pipeline underneath are complete and well-tested.
4. **A real DAG viewer component already exists and no host can draw it.**
   `WorkflowGraphView` (`sdk/ui/src/first-party/workflow.tsx:44-89`) emits a
   typed `Graph` primitive with nodes+edges; the TUI paint layer reads edges
   from the wrong place and silently drops them
   (`tui/src/remote_ui/paint.rs:1122-1129`), VS Code renders them as a text
   list, no shipped worker mounts the component, and the protocol's
   `WorkflowRunSnapshot` doesn't carry edges at all.
5. **The chat stream's richest renderers are unreachable.** The TUI has a
   complete expanded-tool-card and diff-preview renderer
   (`tui/src/render.rs:1701-1799`) that no input path can trigger: `Tool`/`Patch`
   entries get no click target (`render.rs:1121-1133`) and the keyboard
   `Expand` action only fires in a mode the base view never enters.

The good news hiding in this pattern: **most of the distance to the ten
outcomes is S-sized wiring, not new subsystems.** Section 4 sequences it.

## 2. Scorecard

| # | Outcome | State | Verdict |
|--:|---|:--:|---|
| 1 | Polished TUI | ~70% | Strong foundation (tokens, tests, palette, pickers, empty states); dead expansion feature, no composer cursor editing, wrap-measure bug, no timestamps |
| 2 | ACP + auto model discovery | ~55% | Client role robust (registry, checksums, worktrees, durable approvals); discovery 0% (data discarded); serve mode emits non-spec updates (Zed interop broken) |
| 3 | Easy model selection + prefilled lists | ~60% | Live `/models` discovery works end-to-end; **zero prefilled models** (schema exists, unconsumed); auth-header bug breaks Azure/GitHub Models at run time |
| 4 | Skill-writer + doc-writer | ~10% | Neither exists as a feature; every substrate (skill packages, promotion gates, CRDT docs, suggestions, staleness engine) is built and tested |
| 5 | DAG viewer (user + agent) | ~35% | Live workflow node *list* in TUI; no edges drawn anywhere; agent has no graph/workflow query tool; codegraph query layer 100% unwired |
| 6 | AI council | ~65% | Works end-to-end via CLI (parallel members, rounds, chair, attribution); TUI can build but not run one; dossier silently truncates dissent; members can't read the repo |
| 7 | Rich chat stream | ~60% | Real markdown/tables/syntax highlighting; diff/tool expansion dead; no thinking channel; no tool-output streaming; per-chunk SQLite journaling throttles fast models |
| 8 | TTS + STT | ~5% | Protocol model (`InputEnvelope`, `AudioArtifact`, classification gate) complete + vector-tested; zero capture/STT/TTS code; no command carries the envelope |
| 9 | Vector top-k tool/skill selection | ~30% | Full hybrid funnel + eval harness built; embeddings are hashed trigrams (not semantic); output never consumed at prompt build; MCP tools bypass it entirely |
| 10 | Blackboard + kanban + NL backlog | 40 / 0 / 0% | Blackboard solid inside workflow runs (typed, evidenced, live); kanban and backlog tools appear in no code **and no design doc** |

## 3. Consolidated defect register

Severity-ordered; each verified in code by a vertical reviewer (see the
per-vertical reports for full detail and additional LOW items).

**Critical**

| Defect | Where |
|---|---|
| Assembled context (repo map, skills, memories) never enters the model prompt; system prompt is one sentence | `codypendentd/src/executor.rs:1161-1169`, `session_history.rs:121-173`, `runtime/src/agent.rs:3863` |
| No path exists to create a document (engine complete underneath) | no `CreateDocument` in `protocol/src/command.rs`; `DocumentStore::create` test-only |
| Retrieval selects nothing the model sees; tool list is a static allowlist injected in full every step | `runtime/src/agent.rs:1181-1280,1568` |
| Curated memories are written by a pipeline whose read side dead-ends in a display note | `knowledge/src/context.rs:241-259` |

**High**

| Defect | Where |
|---|---|
| Parallel tool calls silently dropped (only first function call executes) | `runtime/src/agent.rs:4062` |
| Add-model flattens every provider to bearer-auth: Azure OpenAI / GitHub Models list fine, then 401 on every run | `cli/src/tui.rs:2550-2556`, `runtime/src/models.rs:287-295` |
| ACP serve mode emits non-spec `session/update` shapes — Zed shows nothing until the final stop reason | `cli/src/acp.rs:199-213,241` |
| `NewSessionResponse` models/modes + `InitializeResponse` auth methods discarded | `integrations/src/acp_client.rs:471-493` |
| TUI remote-UI Graph drops SDK-authored edges (reads per-node, SDK sends top-level) | `tui/src/remote_ui/paint.rs:1122-1129` |
| Remote-UI action allowlist is exactly `run.pause/resume/cancel` — every other first-party intent is unexecutable | `daemon/src/server.rs:3195-3204` |
| Tool cards and patch diffs unexpandable by any input (renderer exists, unreachable) | `tui/src/render.rs:1121-1133`, `input.rs:392-416`, `reduce.rs:1728-1731` |
| No mid-run compaction: long sessions silently truncate at the provider | `runtime/src/agent.rs:1536-1544` (warns only) |
| Advertised tool schemas hide implemented params (`read_file.range`, `shell.timeout_secs/cwd`) | `runtime/src/agent.rs:3634-3653` vs `:3255-3323` |
| The only shipped skill is `status = "draft"` → hard-filtered from retrieval forever; no skill ingestion path exists (`register_package` test-only) | `examples/skills/fix-ci/skill.toml`, `retrieval/mod.rs:394-396` |
| Codegraph query layer (`callers_of`, `blast_radius`, `tests_covering`, adapters, watch) and docs staleness engine: zero production callers | `knowledge/src/codegraph.rs`, `docs/staleness.rs` |

**Medium (selected)**

| Defect | Where |
|---|---|
| Council dossier truncates at 64KB mid-member, alphabetically — chair can silently never see later members; chair may also be a member; members run `Ask` with tools forbidden (can't ground in repo); up to 25 unarchived sessions per run | `cli/src/council.rs:524-537,569-575,506,388-394` |
| Accepting one doc suggestion strands every other pending suggestion (revision pinning) | `knowledge/src/docs/collab.rs:284-287` |
| No model-call retry/backoff — any transient stream error fails the run | `runtime/src/agent.rs:1622` |
| One SQLite ledger write per stream chunk (throughput + ledger bloat) | `runtime/src/agent.rs:1586-1595` |
| Transcript wrap measured cell-wise but drawn word-wrapped → follow-mode can clip the newest lines | `tui/src/render.rs:833-841` vs `:1336-1339` |
| Char-count truncation misaligns CJK/emoji across lists and markdown tables | `tui/src/render.rs:6413`, `markdown.rs:386-414` |
| Remote-UI worker/host burst mismatch (SDK default 1000 vs host kill at 120) can kill legitimate workers | `sdk/ui/src/worker/runtime.ts:365` vs `ui-host/src/runtime.rs:797` |
| Catch-up snapshot >500 events can't rebuild the transcript; no paged history command | `daemon/src/server.rs:3343-3349`, `protocol/src/catchup.rs` |
| Discovery-failure fallback can write a keyless hosted model (`requires_key` mis-derived) | `tui/src/state.rs:266-270` |
| Index outbox (registry + documents) is written forever and drained by nothing | `knowledge/src/outbox.rs:83-117` |
| `repository.test` detects pytest/npm but the default policy allow-list blocks them | `daemon/src/policy/config.rs:111-138` |
| Agent shell runs unconfined (bwrap/Seatbelt machinery exists but is plugin-only) | `runtime/src/tools/shell.rs` vs `sandbox/src/executor.rs` |
| `provider-anthropic` feature is pinned, shipped, and dead — `Protocol::Anthropic` always `ProtocolNotWired` | `runtime/src/models.rs:460-464` |

Also worth noting as **honesty debt**: ROADMAP/README are frozen pre-v0.2.0
(no Remote UI platform, councils, ACP registry, migrations 0016-0018;
"Automerge-suitable" contradicts ADR-016), councils and Remote UI have no
design doc at all, keyboard-help/user-guide advertise bindings and buttons
that don't exist, and shipped superpowers specs still say "proposed".

## 4. Recommended sequence

### Phase A — close the loops (all S; days, not weeks; transforms the product)

1. **Feed the manifest to the model.** Prepend `manifest.render()` to the
   seeded objective (or a dedicated turn) in `execute`
   (`codypendentd/src/executor.rs:717-734`). One small change activates the
   repo map, skills, and the whole memory pipeline. Then fix the `fix-ci`
   skill's `draft` status and add a startup scan of `<data_dir>/skills/` +
   `.codypendent/skills/`.
2. **Capture ACP discovery.** Keep `NewSessionResponse` (+`InitializeResponse`);
   expose `discovered_models()`; print them in `acp connect/status`. This is
   the foundation for outcome 2 and ~1 file.
3. **Fix ACP serve shapes** by serializing the SDK's own `SessionUpdate` /
   `ToolCallUpdate` types instead of ad-hoc `json!` — restores Zed interop.
4. **Draw the DAG.** (a) TUI paint: accept top-level `edges` and render
   layered box-drawing connectors; (b) add `depends_on` to
   `WorkflowRunSnapshot`; (c) ASCII lanes in the native workflow overlay.
5. **Un-dead tool/patch expansion** (`fold_hit_entry` arms + a transcript
   focus keybinding) — unlocks the already-written diff renderer.
6. **Agent-side graph access:** a `workflow.query` runtime tool over the
   existing `WorkflowNodeView` projection, and protocol `ReadCodeGraph` /
   `SymbolNeighborhood` over the existing (tested, unwired) codegraph queries.
7. **`CreateDocument`** command + `docs new` + TUI action + markdown import.
8. **Council correctness:** per-member dossier byte shares + `[truncated]`
   markers; warn chair==member; archive member sessions; save a council report
   artifact.
9. **Model-add correctness:** persist auth header/prefix/extra_headers (store
   `provider_id` on the profile and re-resolve from the catalog), carry
   `context_tokens` from the `/models` response, fix the `requires_key`
   fallback.
10. **Advertise hidden tool params** (`range`, `timeout_secs`, `cwd`); execute
    all returned tool calls sequentially instead of dropping extras.
11. **Chat polish quick wins:** timestamps from `occurred_at`, spinner-aware
    redraw gating, display-width truncation, `/theme` live switcher, blank-key
    prompt consistency.

### Phase B — make the headline features real (M each)

- **Prefilled model catalog + richer picker (outcome 3).** Ship curated
  `[[model]]` rows in `builtin_catalog.toml` (this review adds the initial
  dataset — see §6), teach `AddModelPick` to show catalog cards (name / ctx /
  $ / capability badges) merged with live `/models` results, fall back to
  catalog when discovery fails, cache per-provider lists with manual refresh.
- **ACP per-model profiles (outcome 2).** Persist discovered models as
  selectable profiles (`id@version#model`), call `session/set_config_option`
  before prompting, enable `can_list_models` for ready ACP cards, map
  `ConfigOptionUpdate`/`CurrentModeUpdate` into events. Keep a persistent
  `AcpClient` per session (`session/load` where advertised) to stop replaying
  transcripts as text.
- **Kanban + NL backlog (outcome 10).** Extend blackboard with
  `status/assignee/ordinal` (or a structured `task` payload), add a
  repository-scoped board (synthetic run or new scope), role-gated
  `PostBlackboard`/`UpdateBlackboard` commands, a column-grouped TUI pane, a
  `blackboard` remote-UI projection kind, and un-gate `blackboard.*` (+ new
  `task.create/move/prioritize`) tools for chat agents so "turn this feature
  request into backlog cards" works. Note: no spec exists yet — write one
  first.
- **Doc-writer + skill-writer v1 (outcome 4).** Runtime `docs.*` tools
  (create/edit/suggest/read) with `DocumentAuthor::Agent` attribution
  (suggest-by-default already makes this safe); wire `/update-docs` as glue
  over the tested staleness engine; fix suggestion re-anchoring. Skill side: a
  `skill.draft` tool emitting packages through `register_package(Draft)` into
  the existing promotion pipeline (permission review is already mandatory for
  skills); inject a selected skill's SKILL.md into context when disclosed.
- **Retrieval that matters (outcome 9).** Real embeddings behind the existing
  `Embedder` trait (any OpenAI-compatible `/embeddings` endpoint, incl.
  Ollama/Nebius), persisted vectors keyed by content hash, outbox drained into
  the index; then retrieval-gate the *optional* tool families (MCP especially)
  via the existing funnel while core tools stay static.
- **Chat stream depth (outcome 7).** Additive `ThinkingDelta` /
  `ToolOutputDelta` / `UsageReported` events; stream tool-call preface text
  through the sink; coalesce delta journaling; paged `ReadSessionEvents`;
  mid-run compaction folding old tool results into their artifact refs; model
  retry/backoff.
- **Voice v1 (outcome 8).** Additive `SubmitUserInput.envelope` +
  `PutArtifact` upload; push-to-talk capture in the composer; STT via
  OpenAI-compatible `/audio/transcriptions` (Groq `whisper-large-v3-turbo`,
  OpenAI `gpt-4o-transcribe`, or local whisper server) behind the existing
  `transcription_allowed` classification gate; client-side TTS of finalized
  turns (zero protocol change) via `/audio/speech`-compatible endpoints. The
  protocol layer is already designed, built, and vector-tested.
- **Council-from-chat (outcome 6).** Run councils from the TUI (`/council run`)
  with streamed member progress; optionally recompile council definitions into
  fan-out/fan-in workflow manifests to inherit durability, budgets, cost
  accounting, and blackboard-attributed member reports; give members
  `Explore`-mode read tools so reviews are evidence-grounded.

### Phase C — platform bets (L)

- **Ship the first Core-trust remote-UI worker** bundling `WorkflowGraphView`
  + a new `KanbanBoard` — converts the entire (currently fixture-only)
  first-party component library into product, exercises core-only slots, and
  gives DAG/board views in VS Code for free. Extend the daemon action
  allowlist table beyond its current three commands as part of this.
- **Parallel workflow frontier execution** under `maximum_agents` (worktree
  isolation already guarantees safety) — prerequisite for fast councils-as-
  workflows.
- **OS-sandbox approved shell commands** by composing the existing
  bwrap/Seatbelt executor with the minted capability grant.
- Hook engine + WASM runtime (Phase 6 remainder); live measured routing +
  escalation re-drive (Phase 7 remainder); session forking; multi-language
  codegraph persistence + LSP edges; episode compaction (L2/L3).

## 5. Lost opportunities the outcome list doesn't name

From the vision docs and the verticals — candidates worth owning:

- **Compaction levels 2-3 + artifact rehydration** (the docs' flagship context
  story; only L1 exists). An `artifact.read` tool is the cheapest first step —
  salient views already cite artifact ids the model cannot open.
- **Doc-from-run:** "write this up" on a finished run, seeding a document with
  citations from the run's evidence — the fastest credible doc-writer demo.
- **Voice approvals:** push-to-talk approve/reject maps to a constrained enum —
  the safest possible first voice feature.
- **Kanban-from-approvals for free:** pending approvals are already a durable,
  subscribable queue — a "needs-human" column before any blackboard work.
- **models.dev import** for catalog breadth without a live dependency.
- **Forward `mcp.toml` into ACP sessions** (`NewSessionRequest.mcp_servers` is
  in the SDK) so Claude Code/Gemini sessions inherit Codypendent's MCP tools.
- **Generated TS protocol SDK** from the golden vectors (kills VS Code codec
  drift).
- **Terminal affordances:** OSC-8 hyperlinks, OSC-52 copy actions, transcript
  search, per-run tab strip, scrollbar ghost.
- **Setup assistant + AGENTS.md/CLAUDE.md importers**; **self-guide agent**
  answering from the local docs; **chronicle export/attach-to-PR**.
- **Truthfulness pass** on ROADMAP/README/specs (councils, Remote UI, v0.2-0.3
  releases, Automerge line, keyboard-help drift) — this project's credibility
  is its documented review-and-fix honesty; the docs are currently three
  releases behind the code.

## 6. Provider model catalog (research applied)

Alongside this review, curated `[[model]]` rows for the major non-ACP
providers (Nebius first, per the owner's ask) are added to
`crates/providers/builtin_catalog.toml`, using the already-shipped catalog
schema (`providers/src/model.rs:119-131` — id, provider_id, name,
context_tokens, cost per 1M in/out USD). The research notes — including each
provider's live `/models` discovery support and which hosts serve STT/TTS
models for outcome 8 — are in
[`2026-08-11-verticals/11-model-catalog-research.md`](2026-08-11-verticals/11-model-catalog-research.md).

Until the picker consumes `Catalog::models()` (Phase B above), these rows are
inert data — the review deliberately ships them anyway so the wiring change
has its dataset waiting.

## 7. What must not regress

The review would be incomplete without naming what is genuinely excellent and
should be treated as load-bearing: the event-sourced daemon write path and
recovery matrix; deny-first policy with canonicalize-before-compare scoping;
durable approvals with digest-scoped reuse; the ACP registry supply chain
(pinning, checksums, hardened extraction); plugin signing + enforcing sandbox
+ install smoke-test; the workflow engine's crash/pause/retry semantics; the
blackboard's evidence discipline; secret hygiene end-to-end; the theme
system's contrast invariants; the accessible cooked mode; and the golden-vector
protocol compatibility discipline. Several "fixes" above (e.g. retrieval-gated
tools, client board writes, voice ingestion) must be built *through* these
mechanisms — the hard filters, the policy engine, the classification gates —
not around them.
