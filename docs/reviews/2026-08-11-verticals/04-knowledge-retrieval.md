# Agent report: Knowledge fabric / retrieval / skills / codegraph

## Retrieval reality check (embeddings real? wired where?)

**Embeddings are a hash trick, not a model.** `HashingEmbedder` (crates/knowledge/src/retrieval/embed.rs:43-110) builds a 512-dim vector of TF-weighted **character trigrams**, bucketed by FNV-1a, L2-normalized, cached by SHA-256. Explicitly "not a semantic model" (embed.rs:37). The doc claims a real model "plugs in… configured via models.toml's `embedding` entry" (embed.rs:6-8), but **no such config exists anywhere**. The `Embedder` trait seam (embed.rs:26-29) is real and clean; the model is vapor.

**Vector index: brute-force, in-RAM, ephemeral.** `VectorIndex` = flat `Vec<(id, embedding)>` O(n) cosine scan (vector.rs:17-60), no delete/persistence/incremental update — rebuilt wholesale from `Registry::list`. **BM25 is real Tantivy** but `Index::create_in_ram` (bm25.rs:69). lib.rs:10-13 promises outbox-replaying indexer workers into `<data_dir>/index/` — false: `outbox::unprocessed`/`mark_processed` (outbox.rs:83-117) have **zero production callers**; `codypendent index rebuild` deletes an index dir that is "a no-op today" (commands.rs:555-561).

**The funnel itself is genuinely good.** `retrieve()` (retrieval/mod.rs:170-362): dense top-100 ∪ BM25 top-100 ∪ exact-token top-50 ∪ history top-50; **hard filters before scoring** (status Active, scope chain, min trust, executable-only, risk ceiling, mod.rs:393-418); scope-shadow collapse; weighted rerank (dense 1.0 + lexical 1.0 + exact 2.0 + dependency 0.5 + trust 0.25 − risk 0.25); dependency closure within filtered survivors; budget 6-12 tool cards + 1-3 skill cards, 280-byte summaries; versioned config in an audit `RetrievalTrace`.

**Eval harness** (tests/retrieval_eval.rs): 30 labelled cases, recall@8 ≥ 0.8 gate, 100% exclusion of High-risk decoys, token-budget comparison vs full injection. Solid methodology — but pool is only 32 items with the hashing embedder, so "dense" is mostly along for the ride behind exact/lexical.

**Where wired:** the funnel runs in exactly two production places — `assemble_context` (context.rs:218-267, rebuilding both indexes from scratch per call at context.rs:231) invoked per first-run-of-session (executor.rs:1151-1169), and the CLI `index rebuild` canary. **It never touches the model's tool list.**

## How tools/skills reach the prompt today (traced, file:line)

1. **Tool list = static allowlist, not retrieval.** `offered_tool_names` (runtime/src/agent.rs:1181-1280) hardcodes 9 core tools + conditional github/web/blackboard/mcp, filtered by mode overlay. **Full definition set passed to the model every step** (agent.rs:1568-1569). No top-k, no query-dependence.
2. **Funnel output display-only.** First run: `emit_context` → `assemble_context` → `manifest.render()` → **`NoteAppended` ledger event only** (executor.rs:1161-1163). Model transcript seeded as prior + Objective only (agent.rs:1441-1455); `continuation_prior` projects only RunStarted/ModelStreamDelta/ToolCompleted/SteeringApplied/RunCompleted (session_history.rs:120-124). **Repo map, tool/skill cards, cited memories never reach the model — any run.** Same for ACP path (`render_acp_prompt`).
3. **Skill content never reaches any prompt.** SKILL.md validated to exist + hashed, but no code ever reads its content for a model. Cards carry only the 280-byte description. Workflow steps validate `skill:` names at compile then **discard the field at execution** (workflow_exec.rs:2220-2223; `synthesize_agent_objective` never mentions the skill). No model-callable retrieval/search tool either.
4. **Memory's loop is half-open.** Write path works end-to-end (memory.remember → NoteAppended → harvest → observer/extractor → `MemoryStore::curate` gates). Read path dead-ends in the display-only note (context.rs:241-259, capped 32).

## Verified working

- Registry: scoped CRUD, transactional outbox on every write, shadow resolution, builtins idempotently registered, risk derived from permissions.
- Skill packages: strict manifest (deny_unknown_fields), entrypoint existence + package-escape rejection, scope/semver/status validation, deterministic content hash (64 MiB cap), Modified-flag detection. Sandboxed script execution with permission→profile lowering, fail-closed (skill_exec.rs:85-183).
- Retrieval funnel + eval as above; hard-filter security model coherent and tested.
- Memory: curation gate order (secret→scope→contradiction→dedup→provenance→retention), supersede-never-delete, content-free forget audits, secret detection + entropy, SQL scope isolation. "EVIDENCE, NOT INSTRUCTIONS" framing tested against injection payload (context.rs:141-149).
- Code graph: tree-sitter Rust into file-scoped `SymbolKey` nodes + Contains/Defines/Imports/Calls edges with byte-span evidence (codegraph.rs:932-1391), transactional single-file upsert retiring removed symbols, semantic-edge supersession API, snapshot diffing, depth-bounded parse, preflight-validated rebuild capped at 2000 files, stable repo IDs. Incremental≡full reparse test.
- Repomap: bounded significance-ranked render — reaches the trace note.

## Bugs & broken wiring (severity)

1. **[Critical, rubric 9] Retrieval selects nothing the model sees.** Funnel output → ledger note; model tool list static + injected in full every step. The system's own exit-criterion eval measures a code path production doesn't use.
2. **[Critical, memory] Curated memories never re-enter any prompt.** Executor comments claim "a memory a prior run curated resurfaces" (executor.rs:1158-1160) — to the human, not the agent. The LLM extractor spends tokens producing facts no model reads back.
3. **[High] The only real skill can never be selected.** `examples/skills/fix-ci/skill.toml` declares `status = "draft"`; the hard filter drops non-Active (mod.rs:394-396). Four eval cases expect `rust.fix-ci` disclosed; their low recalls are absorbed by the 0.8 mean gate — suite passes while skill disclosure is silently dead. No test asserts any skill card is ever disclosed. No promote-to-active API.
4. **[High] No production ingestion of skills at all.** `register_package` called exclusively from tests. No daemon scan of a skills dir, no CLI/protocol install command. Skill Studio TUI is read-only over builtins.
5. **[Medium] Outbox never drained**: unbounded growth; indexes rebuilt from scratch inside every `assemble_context` (fine at 32 items; cliff later).
6. **[Medium] Code-graph query layer 100% unwired**: `callers_of`/`blast_radius`/`tests_covering`/`hierarchical_map`/`symbol_snapshot`/`changed_between`, `upsert_semantic_edges`, LanguageAdapter layer, `docs::detect_staleness`, `codegraph::watch` — zero callers outside knowledge's own tests. `/update-docs` command card advertises a flow that doesn't exist daemon-side.
7. **[Low] Graph staleness**: rebuild once per repo per daemon boot (`ensure_scanned`); watcher never armed; SCAN_FILE_CAP 2000 silently truncates.
8. **[Low] Memory TTL rows filtered at read but never GC'd**; no compaction of episodic breadcrumbs.
9. **[Low] `blast_radius` BFS N+1 SQL** (codegraph.rs:664-708); `reverse_reachable` discards `repository` arg.

## Gaps vs rubrics #9, #4 (skills), #5 (graph data)

**#9:** Machinery exists (union retrieval, hard filters, budgets, traces, eval gate) but (a) no real embedding model; (b) selection output never consumed at prompt-build; (c) no agent-callable search tool; (d) MCP tools (the set that will explode) bypass retrieval entirely. Verdict: ~60% of the library, 0% of the wiring.

**#4 (skill-writer):** Nothing agent-facing. Missing: draft tool, production register path, promote/deprecate API, skill-content injection when selected, executable tests-for-skills, per-skill outcome traces. Rails that DO exist: package format + validation, sandboxed scripts/ execution, Phase-7 promotion pipeline where `ArtifactKind::Skill` forces permission review + canary/rollback, failure clustering framed as skill-synthesis input. Governance built; writer and registry bridge not.

**#5 (graph data):** Strongest asset: real per-file graph with evidence spans, stable IDs, revision stamps, retirement, supersession API, snapshot diffs, two map foldings. But Rust-only in persistence (scan collects `*.rs`; Python/TS adapters' symbols never persisted), exposed to **neither user nor agent**: no protocol command, no TUI view beyond flattened repomap text, no tool.

## Prioritized opportunities (S/M/L, impact)

1. **(S, very high) Feed the manifest to the model.** Prepend `manifest.render()` to the seeded objective in `execute` (executor.rs:717-734) or add a TurnItem. Highest value-per-line in the vertical.
2. **(S, high) Activate skills.** fix-ci status→active (or register_package promote); `codypendent skill add <dir>` + daemon startup scan of `<data_dir>/skills/` + `.codypendent/skills/`; eval assertion that a skill card IS disclosed.
3. **(M, high — rubric 9 core) Retrieval-gated tool advertisement.** In `advertised_tool_definitions`, run `retrieve()` against objective (+ recent turns); advertise always-on core ∪ top-k registry/MCP tools. Register MCP tools as registry items. Hard-filter design already guarantees safety.
4. **(M, high) Real embeddings behind the trait.** models.toml `embedding` entry → Embedder impl in runtime (inject like FactExtractor); persist vectors keyed by content hash; drain outbox to update; re-tune eval weights.
5. **(M, high — rubric 5) Expose the graph.** Protocol commands (`ReadCodeGraph`, `SymbolNeighborhood`) + agent tools `graph.callers`/`graph.blast_radius`/`graph.tests_covering` — queries exist and are tested; only handlers missing.
6. **(M, medium) Skill-writer v1.** `skill.draft` tool (write skill.toml+SKILL.md → register_package(Draft)) through the promotion pipeline; inject selected skill's SKILL.md (bounded) into context when its card is disclosed.
7. **(S, medium) Arm the watcher.** Debounce `codegraph::watch` into `upsert_file_graph` per changed file; drop once-per-boot `ensure_scanned` for revision checks.
8. **(S, low) Hygiene**: outbox GC or real consumer; memory TTL sweep; cache RetrievalIndexes on the executor; raise/paginate SCAN_FILE_CAP.

## Extra ideas

- Memory as retrieval corpus: `memories.embedding_hash` column anticipates embedded memories — rank the 32-cap by query relevance instead of recency.
- History source dormant: feed successful tool ids per task-class from eval/promotion traces back into `RetrievalQuery.history`.
- Graph-aware retrieval: boost tools/skills whose keywords intersect the objective's blast_radius symbols.
- Persist `RetrievalTrace` to the ledger per run for Phase-7 tuning + skill-synthesis clustering.
- Draft shadowing UX: render Draft status prominently in Skill Studio (silent non-selection today).
