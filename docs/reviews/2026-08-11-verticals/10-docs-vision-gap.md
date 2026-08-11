# Agent report: Docs vision vs reality gap

Docs freeze at ~2026-07-30; git shows four undocumented releases since (`v0.1.1` hardening, `v0.2.0` "universal Remote UI platform", `v0.3.1` "ACP agents + collaborative councils", `v0.3.2` councils + TUI polish).

## Rubric × docs map

**1. Beautiful easy TUI — heavily planned, largely built.** Six superpowers specs shipped (codex-chat-shell, streaming-chat, tui-overhaul, rich-formatting, tui-experience, themes). Remaining per ROADMAP: composer polish (multiline, history, `@`-mentions, paste placeholders, queue-while-working), side conversations/forks, terminal-native polish (resize reflow, IME, hyperlinks).

**2. ACP + automatic model discovery — planned and built in two waves.** ACP server adapter Phase 3.6 done; ACP client via universal-providers + v0.3.1. Registry auto-discovery is README-only (never in the manual). `/v1/models` discovery per 2026-07-26 spec — real.

**3. Easy model selection + prefilled lists — mostly built, one nuance.** builtin_catalog.toml has **42 providers, 0 `[[model]]` entries** — provider list prefilled; *model* lists fetched live or typed. "Prefilled model lists" per the rubric does not exist as shipped.

**4. Agent skill-writer & doc-writer — planned as fragments, never built as agents.** Skill Studio "propose improvements" (docs/docs/05), skill synthesis from trace clusters (docs/docs/13, Phase 7.5 draft shapes only), Docs Studio Maintain mode + `/update-docs` (docs/docs/08). Zero code hits for any writer agent; `/update-docs` is "a registry card with no execution wiring". Weakest rubric item; the staleness engine + suggestion machinery it would sit on IS built.

**5. DAG viewer — partially planned/built.** Code-graph edge inspector + workflow-graph TUI overlay done read-only. Gaps: workflow view shows pre-run state (live per-node overlay deferred); no interactive code-DAG navigation; agent consumes graph via repo-map context, not a viewer.

**6. AI council — built but never designed in the docs.** Zero mentions in docs/docs/01-21, ROADMAP, build guide, or superpowers specs. Exists only in README + user guide + code. No ADR, no spec, no manual chapter.

**7. Rich chat stream — planned and shipped** (streaming deltas, codex shell, continuous session, markdown + syntax highlighting).

**8. TTS/STT — promised repeatedly, only the data model exists.** README multimodal, docs/docs/10 voice, build STEP 6.5, ROADMAP Phase 6 "client capture (voice/clipboard)" open. `transcription`/`AudioArtifact` only in protocol/src/input.rs + golden vectors; zero capture/whisper/TTS code.

**9. Vector top-k — built, but "vector" is nominal.** Phase 2.3 shipped with real eval gate. `HashingEmbedder` — hashed char-trigram TF, "deterministic, offline"; no embedding model, no Qdrant. The 07-20 review already flagged: "dense retrieval is not semantic".

**10. Blackboard + kanban + NL backlog — one of three.** Blackboard planned + wired. Kanban: **zero** mentions in all docs and code. NL backlog: zero. Both are net-new scope with no design groundwork.

## Promised-but-unbuilt inventory

| Doc ref | Feature | Evidence of absence |
|---|---|---|
| README, docs/docs/10, build/16 §6.5 | Voice/image capture, transcription, TTS | audio terms only in protocol |
| docs/docs/05, build/16 §6.3, ROADMAP 6.2-4 | WASM component runtime + SDK | wasmtime not in Cargo.toml |
| ADR-012, docs/specs/hook.toml, build/16 §6.4 | Hook engine | HookKind/HookDefinition: zero code hits |
| README "Agentic Setup", ROADMAP 6.7 | `codypendent setup` assistant + AGENTS.md/CLAUDE.md importers | no setup module |
| docs/docs/03, build/15 §5.6, ROADMAP 5.6 | ForkSession{checkpoint} + fork-compare UI | zero code hits |
| README "Compaction levels", docs/docs/09 L2/L3 | Episode + session compaction, rehydration, pinning | EpisodeSummary: zero hits; only L1 |
| ROADMAP Phase 4 | Live language-server spawn (rust-analyzer/pyright) | edges synthesized |
| ROADMAP Phase 7 | Live measured routing run, real shadow/canary, eval-export scrubbing, live escalation | escalat only inside crates/routing |
| README/docs/docs/10 | JetBrains plugin; GitHub App org mode | extensions/ has only vscode |
| docs/docs/19-20 | Remote attach, runner broker, browser verification, chronicle export --redact | absent |
| docs/docs/02/06 | Real embeddings / optional Qdrant | HashingEmbedder only |
| ROADMAP cross-cutting | Generated TS protocol SDK | codec hand-written |

## Prior-review debt still open

- C11 — lexicographic TEXT revision comparison in knowledge/src/memory.rs.
- Async CLI harness has zero tests (reconnect/gap-repair path) — "least-tested path real users hit first".
- No live-provider model test (everything via ScriptedDriver/wiremock).
- Linux OS-sandbox enforcement partial (bubblewrap arg-generator; enforcement for hooks/plugins gated on unbuilt 6.2-6.4).
- Dead/unwired inventory (07-20 §5): pending_effects production writers, inert ApprovalScope::Pattern/Repository, provider-anthropic feature, integrations Debouncer.
- 07-20 §4 performance cluster: per-run in-RAM index rebuild, O(session) catch-up, per-delta redraw.
- cargo audit/npm audit absent (deny covers advisories).

## Vision features the owner didn't list (lost opportunities)

1. **Compaction levels 2-3 + rehydration/pinning** — flagship story; only L1 exists. Improves long sessions.
2. **Doc-writer's missing last mile**: staleness + Maintain + suggestion rail + publish-through-approval all shipped; `/update-docs` just needs an executor.
3. **Hook engine + WASM plugin host** — extensibility differentiator; decision layer built and reviewed sound.
4. **Learning loop closure** (Phase 7): eval run CLI, corpus, persisted promotion exist; clustering over real traces + routing arms never run.
5. **Chronicles as product surface**: chronicle v0 per run exists; export/attach-to-PR/redacted sharing unbuilt.
6. **Session forking + side conversations** — synergizes with councils.
7. **Setup assistant + compatibility importers** (AGENTS.md/CLAUDE.md).
8. **Self-guide agent** answering from local docs — cheap and delightful.
9. **Approval scopes Pattern/Repository** (stored, inert) — fewer approval prompts.
10. **Remote attach / runner broker** — undelivered.

## Doc hygiene issues

- **ROADMAP frozen pre-v0.1.1**: knows nothing of Remote UI platform (ui-host, sdk/ui, migration 0018), v0.3.1 ACP registry/councils, migrations 0016-17.
- **README contradicts ADR-016**: still says "Automerge-suitable"; Loro was selected.
- **Councils and Remote UI have no design docs** (no manual chapter, ADR, or spec); docs/docs/02 module list omits providers, routing, eval, workflow, sandbox, ui-host.
- **Build guide migration numbers drifted** (build/15 says 0005 for workflow tables → shipped 0010-11; build/17 says 0006 for model_profiles → shipped 0014); no historical banner.
- **MCP shipped differently than Phase 6 planned** (agent-side client outside plugin/sandbox lifecycle); no doc reconciles.
- **Kanban + NL backlog appear in no document at all** — need specs before anyone can "finish" them.
- Superpowers specs carry no "shipped" stamps.

## Prioritized opportunities

| Size | Opportunity | Impact |
|---|---|---|
| S | Truthfulness pass: update ROADMAP/README for v0.1.1-v0.3.2, fix Automerge line, banner build guide, stamp shipped specs | High — credibility |
| S | Wire /update-docs executor over existing staleness + suggestion + publish-jobs machinery | High — doc-writer half of rubric 4 almost free |
| S | Real embeddings behind Embedder trait (any OpenAI-compatible /embeddings endpoint incl. Ollama) | Med-high |
| S | Composer polish batch (multiline, history, @-mentions, queue-while-working) | Med — biggest daily-feel gap |
| M | Live workflow-graph overlay (fold WorkflowEvent stream + node cost into shipped TUI view) | Med |
| M | Voice v1: push-to-talk STT via local whisper-compatible endpoint + transcript review; TTS after | High for rubric 8 |
| M | Skill-writer agent: /skill new flow emitting draft packages through Skill Studio + promotion permission review | Med |
| M | Kanban + NL backlog: spec first; build on blackboard store + document Checklist blocks | Med |
| M | Episode compaction (L2) feeding continuous-session seeds | Med |
| L | Hook engine + WASM runtime + sandboxed skill scripts (Phase 6 remainder) | High long-term |
| L | Live measured routing + escalation + shadow/canary (Phase 7 remainder) | Med |
