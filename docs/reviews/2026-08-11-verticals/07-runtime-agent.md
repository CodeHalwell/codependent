# Agent report: Runtime agent loop & tools

## Prompt assembly reality (what's injected, sizes, file:line)

**System prompt: one hardcoded sentence.** `FrameworkModelDriver::to_messages` prepends exactly `"You are a coding agent. Use the provided tools to inspect and modify the repository, then finish with a short summary."` (`crates/runtime/src/agent.rs:3863-3866`). No repo instructions file, no skills, no memories, no repo map — nothing else ever enters the system slot.

**Tool injection: full offered set every step, no top-k.** The static catalog `static_tool_definitions()` (`agent.rs:3622-3857`) holds **17 tools**: `shell.run`, `workspace.read_file`, `workspace.search`, `git.diff`, `git.apply_patch`, `workspace.write_file`, `workspace.edit_file`, `repository.test`, `memory.remember`, 5× `github.*`, `web.search`, `blackboard.post/query`. Per run it is filtered by `offered_tool_names` (`agent.rs:1181-1280`) — configured-gate (github/search/mcp/blackboard) plus mode-overlay filter — and recomputed each step (`agent.rs:1568`). A Build-mode solo run advertises **9 tools** (pinned by test `agent.rs:5520-5535`); Ask/Explore advertise 3. Descriptions are terse (1-4 lines), so the built-in payload is small (~2-4 KB) — but **every MCP server tool is appended verbatim, description + inputSchema, unfiltered** (`agent.rs:1302-1318`), so MCP is the unbounded axis. **Rubric 9 ground truth: there is no vector selection anywhere in the loop — membership is config gates, never relevance.**

**The retrieval funnel exists but is display-only (headline finding).** `codypendent_knowledge::assemble_context` runs a real Chapter-05 funnel — hash-embedding dense + lexical + exact scores, hard security filters, rerank (`crates/knowledge/src/retrieval/mod.rs:110-226`, weights in `retrieval/config.rs`), progressive disclosure of "6–12 tool cards" + cited memories + repo map (`knowledge/src/context.rs:113-261`). The executor calls it per first-run and renders the manifest — **as a `NoteAppended` trace event only** (`crates/codypendentd/src/executor.rs:1143-1169`). The model transcript is seeded exclusively from `run.prior + Objective` (`agent.rs:1444-1455`), and the ledger→transcript projection maps only `RunStarted`/`ModelStreamDelta`/`ToolCompleted`/`SteeringApplied` — `NoteAppended` is never converted (`codypendentd/src/session_history.rs:121-173`). **So the repo map, skill cards, and curated memories never reach the model.** The TUI shows a beautiful "=== CONTEXT" note; the LLM starts blind.

**Token budgeting: advisory only, no mid-run compaction.** Estimate = chars/4 + 4/turn (`agent.rs:201-261`), compared against `models.toml` `context_tokens` to emit deduped `BudgetWarning{Tokens}` (`agent.rs:282-301`, `1536-1544`) and forward Ollama `num_ctx` (`agent.rs:4011-4028`). Nothing ever truncates or summarizes the live transcript — on overflow the provider silently clips. The estimator also ignores the system prompt and tool definitions. Cross-**run** compaction exists: continuations replay the last `VERBATIM_RUNS = 3` runs verbatim, older runs collapse to one summary turn (`executor.rs:78`, `session_history.rs:61-248`), with tool artifacts rehydrated at 2 KB/turn, 16 KB aggregate (`executor.rs:95-103`, `1215-1253`).

## Agent loop mechanics (traced)

**Invocation path:** daemon `StartRun` → `RunExecutor::spawn_run` (seam: `crates/daemon/src/executor.rs:1-116`) → `codypendentd/src/executor.rs:1869-1961`: reconstruct prior, `emit_run_opening`, then `execute` (`executor.rs:568-766`): routing/pin validation → `check_model` (`/models` probe, `models.rs:330-412`) → `FrameworkModelDriver::from_registry` → `FrameworkAgentRuntime::new(...).with_github/mcp/search` → worktree bind (Build gets isolated worktree; read modes run in repo root, `executor.rs:694-715`) → `execute_run`. ACP profiles branch to an external agent process (`executor.rs:658-661`, `828+`).

**The loop** (`agent.rs:1340-1809`): per iteration — cancel/pause check → drain steering → `MAX_STEPS` 256 (`agent.rs:98`) → wall clock 30 min, warn at 80% (`agent.rs:103`, `1507-1524`) → token warning → `driver.next_step(transcript, tools, sink)` inside `select!{cancel, wall-clock sleep, step, delta-rx}` (`agent.rs:1570-1597`). Text chunks stream through `ChannelSink` and are **journaled one SQLite event per chunk** then published (`agent.rs:1554-1615`; "deltas are journaled" contract, `agent.rs:1550-1553`).

**Tool middleware** (`run_tool`, `agent.rs:1896-2100`): `prepare` maps name→typed input + `ProposedAction` (unknown tool → observation error); policy evaluate under mode overlay; **Deny** → `ToolDenied` + failure observation with a strategy hint for allowlist denials (`agent.rs:1936-1950`); **RequireApproval** → persist request, `RunState::WaitingForApproval`, `ToolProposed`, park on `await_decision` raced against cancellation (`agent.rs:1995-2054`); **Allow/approved** → `ToolStarted{args_digest, label}` → `execute_prepared` raced against cancel → `ToolCompleted{outcome, artifact}`. Observation pushed as `ToolResult`; steering drained at the safe point. Repeated-identical-call guard: 3 consecutive same-digest calls short-circuit into a steering turn (`agent.rs:142`, `1686-1725`). Termination: review node emits `PatchProposed` change-set from worktree diff (`agent.rs:2918-2965`), chronicle artifact + `RunCompleted` (`agent.rs:1762-1808`).

**Events → TUI chat stream:** `ModelStreamDelta` (assistant text), `ToolProposed`/`ToolStarted`(+label)/`ToolCompleted`, `ToolDenied`, `PatchProposed`, `SteeringApplied`, `BudgetWarning`, `NoteAppended` (context manifest, memory notes), `RunStateChanged`, `RunCompleted` — all consumed in `crates/tui/src/reduce.rs:1026-1380`.

**Model driver** (`agent.rs:3569-4148`): translates transcript to messages, streams via `get_streaming_response`, 16 MB stream cap (`agent.rs:105`, `3962-3970`), coalesces via `absorb_update`, maps to step. Tool results replay as **user-role** `[tool result: …]` text and tool calls as assistant `[calling …]` markers (args clipped to 200 chars, `agent.rs:3898-3918`) — deliberately no `role:tool` threading (orphan-rejection rationale, `agent.rs:3560-3568`).

## Verified working

- Streaming deltas live and ordered, error-path chunks preserved (tests `agent.rs:6737-6829`); scripted-driver IT suite covers full event sequence, disconnect-immunity, cancellation incl. parked-on-approval, steering, explore-mode write denial (`tests/agent_it.rs:216-1029`).
- Mode overlays and advertised≡offered≡dispatchable invariant, incl. MCP and plan-mode filtering (`agent.rs:1181-1320`, tests `agent.rs:5474-5609`).
- Approval gating with run-scoped reuse digest (canonical JSON for MCP, `agent.rs:3130-3156`); GitHub writes park for approval (`tests/agent_it.rs:1246-1296`).
- Untrusted-content chokepoints: MCP/web-search results and errors sanitized + evidence-framed (`agent.rs:2711-2795`); GitHub/blackboard renders evidence-labeled (`agent.rs:3183-3220`).
- Tool sandboxing: descriptor-relative `O_NOFOLLOW` opens (`tools/secure_fs.rs:41-146`), env-hijack deny-list (`tools/shell.rs:251-289`), empty child env, process-group kill + drop guard (`shell.rs:369-442`), git env hardening (`tools/git.rs:331-355`), timeouts on every helper (search 120 s, git 300 s, shell clamp ≤1 h).
- Usage honesty (measured-vs-unmeasured `Option`s) end to end (`agent.rs:340-457`); read-your-writes worktree binding (`tests/agent_it.rs:1759-1905`); pre-start pause/cancel races handled (`agent.rs:1364-1436`).

## Bugs & broken wiring (severity)

1. **CRITICAL — assembled context never reaches the model.** Repo map / top-k skill cards / curated memories are trace-note-only (`executor.rs:1161-1169`); `session_history.rs:121-173` never projects `NoteAppended` into `TurnItem`s. The entire knowledge fabric, memory harvest, and `memory.remember` loop write to a store the prompt never reads. (Retrieval exists; wiring absent.)
2. **HIGH — parallel tool calls silently dropped.** `chat_response_to_step` takes only the first function call: `message.function_calls().into_iter().next()` (`agent.rs:4062`). A model emitting multiple `tool_calls` loses all but one with no error — desync between what the model believes it did and what ran.
3. **HIGH — no mid-run compaction.** Long sessions exceed `num_ctx` and get provider-side silent truncation (head-loss of objective/system) — the loop only warns (`agent.rs:1536-1544`).
4. **HIGH — schema/parser drift.** Advertised `workspace.read_file` schema exposes only `path` (`agent.rs:3646-3653`) though the parser accepts `range` (`agent.rs:3315-3323`); `shell.run` schema omits `cwd`/`environment`/`timeout_secs` (`agent.rs:3634-3643` vs `3255-3296`). Models can't discover paging or timeouts → repeated 200-line reads of big files.
5. **MED — no model-request retry/backoff.** Any transient stream error fails the entire run (`agent.rs:1622`); `models.rs` has no retry taxonomy, only a 3 s `/models` probe.
6. **MED — one ledger write per stream chunk** (`agent.rs:1586-1595`): SQLite append + broadcast per token-burst; bloats ledger and throttles the chat stream under fast local models.
7. **MED — `repository.test` unusable outside Rust by default:** detected `pytest`/`npm` aren't in the default allow-list (`daemon/src/policy/config.rs:111-138`; pinned denied in `agent.rs:6249-6295`).
8. **MED — `provider-anthropic` feature is dead code:** Cargo feature + optional dep exist (`Cargo.toml:36,61`) but `models.rs` maps any non-`openai-compatible` provider to `UnsupportedProvider` (`models.rs:281-284`); nothing constructs an Anthropic client.
9. **LOW — token estimator omits system prompt + tool definitions** (`agent.rs:232-261`), understating usage exactly when MCP schemas are large.
10. **LOW — replayed-history-only threading:** tool results ride as user text, tool calls as assistant markers (`agent.rs:3861-3895`) — defeats provider prompt-caching and confuses stronger models expecting native tool history.

## Tool suite scorecard (1 line each)

- **shell.run** — strong sandbox (allowlist, empty env, env-deny, pgroup kill, spill+salient) but schema hides `timeout_secs`/`cwd`; every allow-listed command still needs approval → high friction (`tools/shell.rs`).
- **workspace.read_file** — TOCTOU-safe, streaming, 200-line default; `range` unadvertised (bug #4); no binary/image handling (`tools/read_file.rs`).
- **workspace.search** — rg `--json`, 200-match cap, 120 s bound, scope-confined; no context lines, no case-insensitive/fixed-string options, results lack surrounding code (`tools/search.rs`).
- **workspace.edit_file** — exact-unique-sequential-atomic search/replace with excellent error taxonomy (`SearchAmbiguous` count + guidance); single file per call, no `replace_all`, no fuzzy/whitespace tolerance for weak models (`tools/edit_file.rs`).
- **workspace.write_file** — leaf-swap-guarded create/overwrite, parents auto-created; solid (`tools/write_file.rs`).
- **git.diff / git.apply_patch** — hardened env, check-then-apply, 300 s bound; no commit/branch/log/status tools (`tools/git.rs`).
- **repository.test** — auto-detect + override; blocked by allow-list for non-Rust (bug #7) (`tools/repository.rs`).
- **github.\*** — 2 reads + 3 idempotency-keyed writes, evidence-framed; client also implements review comments/job logs that are never offered as tools (`tools/github.rs`, `agent.rs:5351-5368`).
- **web.search** — Tavily only, 5-10 results, sanitized; no URL-fetch companion, key via env only (`tools/web_search.rs`).
- **memory.remember** — cheap NoteAppended → harvest pipeline; useless at runtime until bug #1 is fixed (`tools/memory.rs`).
- **blackboard.post/query** — workflow-only, server-side authorship, evidence-required kinds; solid (`tools/blackboard.rs`).
- **salient.rs** — head 40 + tail 40 + ≤200 error lines, 2 KB/line clamp — good observation compaction.
- **Missing vs modern agents:** glob/list-dir, fetch-URL, multi-file edit batch, git commit/status/log, subagent/task spawn (no council primitive in-loop), plan/todo tool, notebook, image reads, artifact-rehydrate tool (salient tells the model the artifact id but offers no tool to read it back).

## Gaps vs rubrics #7/#8/#9

- **#7 Rich chat stream:** deltas are text-only — `update_text_delta` reads `text_content()` (`agent.rs:4036-4039`); **no reasoning/thinking channel** (only `reasoning_controls: false` flag in `bench.rs:488`), no tool-call-argument streaming (calls appear only after completion via `ToolStarted`), no structured segments (markdown/code fence semantics left to TUI), preface text of tool-calling turns reaches the transcript but not the stream (it's never sent through the sink — `agent.rs:1674-1676` records it silently, so spoken-while-acting text never renders live). Per-chunk journaling (bug #6) caps stream fluidity.
- **#8 TTS/STT:** **nothing.** Zero audio code paths in runtime/models/tools; the only mention is the static capability flag `audio_input: false` (`bench.rs:485`). No audio protocols in `models.rs` (chat-completions only), no bytes-in/bytes-out seam on `ModelDriver`.
- **#9 Vector top-k tool/skill selection:** the funnel (embedder, rerank, disclosure budget) is fully built in `crates/knowledge/src/retrieval/*` and even produces per-objective 6-12 tool cards — but the loop's advertisement is config-gated-all (`agent.rs:1181-1280`) and the manifest never enters the prompt (bug #1). Ground truth: **all offered tool descriptions are injected every step; retrieval influences nothing the model sees.**

## Prioritized opportunities (S/M/L, impact)

1. **(S, very high)** Bridge bug #1: prepend `manifest.render()` (already evidence-framed) to the seeded objective or as a second system/user turn in `execute` (`executor.rs:717-734`). One-line-ish change; instantly activates memories, repo map, and skills.
2. **(S, high)** Advertise `range` on read_file and `timeout_secs`/`cwd` on shell.run (`agent.rs:3634-3653`); mention the 200-line default in the description.
3. **(S, high)** Execute all returned function calls sequentially (or reject extras with an explicit observation) instead of dropping them (`agent.rs:4056-4079`).
4. **(M, high)** Mid-run compaction: at ~80% of window, fold oldest ToolResults into their artifact refs (the refs already exist per read/shell result) and re-summarize — reuse `session_history::compacted_turn` logic in-loop.
5. **(M, high)** Retry-with-backoff wrapper in `FrameworkModelDriver::next_step` for transient stream errors; error taxonomy in `ModelsError` for 429/5xx.
6. **(M, high — rubric 9)** Feed `assemble_context`'s disclosed tool cards into `offered_tool_names` to top-k-select the MCP/optional families (built-ins stay static); the funnel and budgets already exist.
7. **(M, med — rubric 7)** Coalesce delta journaling (buffer N ms or sentence boundaries; or make deltas ephemeral + persist one `Assistant` turn), and push `preface` text through the sink.
8. **(L, high — rubric 8)** Add an `AudioDriver` seam beside `ModelDriver` in `models.rs` (OpenAI-compatible `/audio/speech` + `/audio/transcriptions` fits the existing base_url pattern); stream STT text into the steering channel — steering injection points already exist.
9. **(L, med)** Subagent-spawn tool bridging to the workflow DAG executor (council/DAG rubrics), reusing `WorkflowContext` + blackboard for result hand-back.

## Extra ideas

- An `artifact.read` tool: salient views cite `artifact <id> sha256:…` (`tools/salient.rs:190-199`) but the model has no tool to rehydrate one — cheap, closes the "consult the artifact" loop the docs promise.
- Use measured `LocalBench`/`ModelCapabilities` (bench.rs) to auto-populate `context_tokens` instead of manual `models.toml` entry (`models.rs:81-89` says "no auto-population in v1").
- `is_error_line` substring match (`salient.rs:84-87`) flags any line containing "error" (e.g. filenames like `error.rs`) — consider word-boundary anchors to keep salient views tight.
- The plan-mode instruction (`agent.rs:121-128`) is the only mode-specific prompt; Review/Ask deserve equivalents — trivial with the same seed mechanism.
- `MAX_CONSECUTIVE_IDENTICAL_CALLS` steering text claims "its result is in the transcript above" (`agent.rs:1713-1718`) — true today, but if the earlier duplicates were denied/rejected, the "result" is a denial; wording could steer better on that path.
