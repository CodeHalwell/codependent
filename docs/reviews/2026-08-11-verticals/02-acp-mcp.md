# Agent report: ACP + MCP integration

## How ACP works today (traced flow, file:line)

Codypendent implements **both ACP roles**, plus a registry pipeline.

**Serve role (Zed → Codypendent as agent).** `codypendent acp serve` (`crates/cli/src/main.rs:1010-1018`) → `cli/src/acp.rs:28` builds a `DaemonAcpBackend` and runs the hand-rolled ndjson JSON-RPC server in `integrations/src/acp.rs:341` (this direction does **not** use the SDK). `session/new` → daemon `CreateSession` (`cli/src/acp.rs:93-111`); `session/prompt` → `AttachSession` as `Approver` + `StartRun` (`:127-155`), then daemon events map to `session/update`s (`:196-225`); `ApprovalRequested` → `session/request_permission` with allow/reject options → `ResolveApproval` (`:234-257`); `session/cancel` → `CancelRun` on a fresh connection (cancellation-safety comment `:161-187`).

**Client role (Codypendent → external agents), the rubric-#2 direction.**
1. **Discovery**: `AcpRegistryStore` (`integrations/src/acp_registry.rs`) fetches `https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json` (`:18`), 4 MiB cap, HTTPS enforced post-redirect (`:277-281`), validated (`:137-186`), atomically cached 0600 (`:1208-1235`). `load_or_refresh` (`:294-309`): 24 h freshness, network refresh, **stale-cache fallback offline**. Binaries: sha256 verified (`:399-430`, `--allow-unverified` escape), hardened extraction (zip/tar/gz/bz2; link/traversal/bomb defenses `:743-976`). Connected profiles pin `id@version` with an immutable local snapshot (`:216-234`, `:457-499`) so a registry refresh never silently upgrades a connected agent.
2. **Connect**: TUI provider picker appends live-registry agents as cards (`cli/src/tui.rs:3619-3668`); selecting one spawns `connect_acp_agent` off-thread (`:1395-1418`, `:2610-2631`) = install + real handshake, then writes a `models.toml` profile `provider="acp", model="id@version"` (`:2520-2538`). CLI mirrors: `acp refresh|list|install|connect|probe|disconnect|status|serve` (`main.rs:268-330`, `cli/src/acp_clients.rs`). `probe` does a live tool-denied prompt (`acp_clients.rs:173-218`).
3. **Execute**: `StartRun` with an `acp/...` model → `codypendentd/src/executor.rs:658` detects via `registry.acp_agent_id` (`runtime/src/models.rs:258`) → `execute_acp` (`executor.rs:828`): `launch_spec` → isolated worktree bind → `AcpClient::spawn` with cwd=worktree (`:876-883`) → one `prompt` turn (`:908-913`). Prior conversation is **re-rendered as flattened text** (`render_acp_prompt` `:1752-1781`, 1 MiB cap). `AcpRunSink` persists mapped events to the ledger and routes permissions through the durable `ApprovalBroker` with `RunState::WaitingForApproval` transitions and cancellation (`:1581-1687`). After the turn: `git add --all` + bounded diff (64 MiB) → `PatchProposed` diff review (`:1034-1108`), chronicle artifact, `RunCompleted` (`:981-1032`).
4. **The client itself** (`integrations/src/acp_client.rs`) uses the official SDK (`agent-client-protocol 2.0.0` + `agent-client-protocol-schema 1.5.0`, Cargo.lock:12-18). Handshake: `InitializeRequest::new(ProtocolVersion::V1)` then `NewSessionRequest::new(cwd)` (`:469-494`), 30 s timeout (`:200`). Streaming: `session_update_to_events` (`:66-82`) maps `AgentMessageChunk`/`AgentThoughtChunk` → `ModelStreamDelta`, `ToolCall` → `ToolStarted`, terminal `ToolCallUpdate` → `ToolCompleted`. Permissions resolve through the sink (`:533-571`). Drop aborts the driver, SIGKILLing the child process group (`:263-270`).

## Verified working

- Registry supply chain: bounds, pinning, checksums, traversal-safe extraction — extensively unit-tested (`acp_registry.rs:1238-1470`).
- Client handshake, streamed text/thought, tool start/complete mapping, permission→approval round trip incl. cancel-interrupts-permission (`acp.rs:296-320`), scripted-peer e2e tests (`acp_client.rs:845-1108`).
- Daemon integration is first-class: worktree isolation, ledger events (same vocabulary as native runs, so TUI streams identically), durable approvals, diff review, chronicle. `resolve_run_model` auto-seeds the ACP catalog on the daemon path and falls back across candidates (`executor.rs:772-822`).
- ACP profiles are usable everywhere models are (council members: `main.rs:1219-1223`).
- MCP: end-to-end complete (below).

## Bugs & broken wiring (file:line, severity)

1. **HIGH — serve-mode `session/update` payloads are not spec ACP.** `cli/src/acp.rs:199-213` emits `{"type":"agent_text","text":…}`, `{"type":"tool",…}`, `{"type":"note",…}`. The wire requires `{"sessionUpdate":"agent_message_chunk","content":{"type":"text",…}}` (schema tag `sessionUpdate`, `v1/client.rs:96` in the SDK). Zed (which parses with this same schema) will fail/drop every update — `codypendent acp serve` streams **nothing visible** into Zed; only the final `stopReason` arrives. Same defect: `request_permission`'s `toolCall: {"action": …}` (`cli/src/acp.rs:241`) is not a `ToolCallUpdate` (no `toolCallId`), so spec-strict clients may reject the permission request. The correct shapes are demonstrably known — the client-side tests speak them (`acp_client.rs:974-1003`).
2. **HIGH (rubric) — `NewSessionResponse` discarded except `session_id`** (`acp_client.rs:481-493`): `modes` and `config_options` (the model list) are dropped.
3. **MEDIUM — `InitializeResponse` fully discarded** (`acp_client.rs:471-479`): `agent_capabilities`, `auth_methods` (SDK `v1/agent.rs:149`), and the negotiated version are unread; there is **no `authenticate` flow anywhere** (grep-verified). An agent needing auth fails opaquely as "session/new failed" instead of "run `claude /login`".
4. **MEDIUM — no session persistence.** Every run spawns a fresh process + session (`executor.rs:876`); continuity is emulated by replaying the transcript as text (`:887-889`), losing the agent's native context/cache; `session/load` is never used (grep-verified).
5. **LOW/MED — client advertises no capabilities**: `ClientCapabilities::default()` = fs read/write false, terminal false (`v1/agent.rs:58-95`, `v1/client.rs:1730-1741`), and only `SessionNotification` + `RequestPermissionRequest` handlers are registered (`acp_client.rs:440-468`). Agents cannot read unsaved buffers (despite Codypendent's IDE-provenance machinery) or use a client terminal.
6. **LOW/MED — cancellation is SIGKILL, not `session/cancel`**: `executor.rs:908-914` drops the prompt future and client; no graceful cancel notification is sent, so the wire-correct `Cancelled` stop reason path is never exercised on the daemon path (the serve direction does it properly).
7. **LOW — serve-mode `initialize` echoes any requested `protocolVersion` verbatim** (`integrations/src/acp.rs:395-407`), claiming support for versions it doesn't implement; should clamp to 1.
8. **LOW — dead config**: the five builtin-catalog ACP providers (`crates/providers/builtin_catalog.toml:347-392`, incl. `cursor`, `opencode`) are filtered out of the picker (`tui.rs:3584-3586`) and their `AuthMethod::Acp` command/args are never used for spawning (`runtime/src/models.rs:270-279` stores empty command; only a display label uses it, `tui.rs:3596`). Launching always goes through the registry — these entries are misleading vestiges.
9. **Note** — `AcpAgentConfig` (SDK `acp_agent.rs:53-110`) has no cwd; the child inherits the daemon's cwd. Spec-fine (cwd travels in `session/new`), but agents that read process cwd pre-session may misbehave.

## Model discovery: exact current state + exact required changes

**Current state: not implemented, by explicit design.** "The agent owns its model; we send no model id" (`acp_client.rs:8`, `:289`, `cli/src/acp.rs:148-150`). One connected agent = one opaque "model" profile (`acp/<agent>` → `id@version`). The picker hard-disables listing: `can_list_models: false` (`tui.rs:3648`). Nothing anywhere reads models from the agent.

**The pinned SDK (2.0.0 / schema 1.5.0) exposes everything needed** — no upgrade required:
- `NewSessionResponse.modes: Option<SessionModeState>` and `config_options: Option<Vec<SessionConfigOption>>` (schema `v1/agent.rs:1086-1113`; v2 likewise `v2/agent.rs:1190-1204`). Current ACP carries the model list as a `SessionConfigOption` with `category: SessionConfigOptionCategory::Model` (`v1/agent.rs:~2477-2493`) and `SessionConfigKind::Select` holding `current_value` + `(id, name)` options (`:2513-2545`).
- Change model: `SetSessionConfigOptionRequest` → `session/set_config_option` (`v1/agent.rs:~2700`; tests `:6164-6300` literally use `"model-1"/"model-2"`). Modes: `SetSessionModeRequest` → `session/set_mode` (`:2166`).
- Live updates: `SessionUpdate::ConfigOptionUpdate` / `CurrentModeUpdate` — currently deliberately dropped (`acp_client.rs:57, 74-80`).

**Exact changes:**
1. `integrations/src/acp_client.rs:481-493` — keep the whole `NewSessionResponse`; extract `config_options` entries with `category == Model` (and `Mode`); store on `AcpClient`; expose `discovered_models() -> Vec<AcpModel{id,name,current}>` and `set_model(id)` implemented as a new `PromptCommand::SetConfigOption` handled in the `run_connection` command loop (`:499-523`). Also read `InitializeResponse.auth_methods` at `:471-479` for actionable auth errors.
2. `cli/src/acp_clients.rs:128-168` (`connect`) and `cli/src/tui.rs:2610-2631` (`connect_acp_agent`) — both currently `drop(client)` immediately after handshake; instead read `discovered_models()` and persist them. `ModelConfig` (`runtime/src/models.rs`) needs a place to carry the agent-model id — e.g. extend the coordinate (`id@version#model`) or add a field; today `upsert_profile` (`acp_clients.rs:316-337`) stores only the coordinate.
3. `codypendentd/src/executor.rs:876-889` — after spawn, if the profile pins an agent-model, call `set_model` before the prompt.
4. `cli/src/tui.rs:3641-3653` — set `can_list_models: true` for ready ACP cards and add an ACP branch to the model-listing flow (mirror the existing off-thread `ReaderSignal::AcpConnected` pattern, `:1903-1916`, `:1321-1342`: spawn → handshake → read options → list).
5. `acp_client.rs:66-82` — map `ConfigOptionUpdate`/`CurrentModeUpdate` to an event (new `EventBody` or `NoteAppended`) so agent-initiated model switches surface in the TUI.
Degradation is safe: agents predating `config_options` return `None` → today's single-profile behavior.

## MCP client state

Complete and genuinely wired end-to-end, matching its spec (`docs/superpowers/specs/2026-07-30-mcp-client-design.md`):
- **Transport**: stdio only (explicit v1 non-goal); hand-rolled ndjson JSON-RPC (`mcp/jsonrpc.rs`) with 1 MiB frame cap, id-correlated waiters, per-request timeouts, method-not-found replies to server→client requests so `roots/list`-blocking servers don't hang (`:344-356`), process-group kill (`:450-468`).
- **Client** (`mcp/client.rs`): offers `2025-06-18`, accepts the server's version (`:16`, `:210-239`); `tools/list` with cursor pagination + loop/size guards (`:244-316`); `tools/call` with content flattening and `isError` mapping (`:321-344`); `structuredContent` ignored (`:352`).
- **Registry** (`mcp/registry.rs`): lazy spawn, boot-time `warm_all` fire-and-forget (`codypendentd/src/lib.rs:161-177`), fail-closed transport invalidation **without replaying ambiguous calls** (`:249-275`).
- **Runtime**: tools offered as `mcp.<server>.<tool>` (`runtime/src/agent.rs:1232-1260`), server schemas advertised verbatim (`:1302-1310`), `prepare` → `ProposedAction::McpToolCall` with canonical args (`:2338-2352`, `protocol/src/run.rs` variant), gated by `[mcp]` policy dispositions (`cli/src/commands.rs:2291-2302`), executed through the `sanitize_untrusted` evidence chokepoint (`:2751-2790`). Workflow agent nodes get it too (`workflow_exec.rs:388-411`).
- **UX gaps**: config is hand-edited `mcp.toml` (`mcp/config.rs`) with `codypendent mcp list` read-only (`commands.rs:2274`); no `mcp add`/health-probe/TUI management; no HTTP/SSE remotes; no `tools/list_changed` refresh (notifications ignored, `jsonrpc.rs:357-359`); ACP sessions never forward MCP servers even though `NewSessionRequest.mcp_servers` exists in the SDK (`v1/agent.rs:1060`) — external agents can't reuse Codypendent's MCP config.

## Gaps vs rubric #2

1. **Automatic model discovery — absent** (the explicit ask; SDK-ready, see above).
2. Serve direction emits non-spec updates → Zed interop broken (bug #1).
3. No auth-methods flow; no `session/load`/persistent sessions; no modes surface; no fs/terminal client capabilities; text-only prompts (no image/audio content blocks).
4. `UsageUpdate` (token/cost), `Plan`, and `AvailableCommandsUpdate` dropped (`acp_client.rs:74-80`) — losing data other rubric items want (rich stream, DAG viewer, cost display).
5. Pinned to `ProtocolVersion::V1` while the SDK ships V2 (`version.rs:34-50`; V2 adds terminal updates, message patching, plan-by-id).

## Prioritized opportunities (S/M/L, impact)

- **S, very high**: capture `NewSessionResponse` + `InitializeResponse` (models, modes, auth methods) and print them in `acp connect`/`status` — the discovery foundation, ~1 file.
- **S, high**: fix serve-mode update/permission shapes by serializing the SDK's `schema::v1::SessionUpdate`/`ToolCallUpdate` types (already dependencies) instead of ad-hoc `json!` — restores Zed interop.
- **M, very high**: full model selection — per-model ACP profiles, `set_config_option` before prompt, TUI picker listing (changes 2–4 above).
- **M, high**: persistent `AcpClient` per Codypendent session (keyed map in the executor; `session/load` where advertised) — removes transcript-replay degradation and respawn latency.
- **M, med**: auth UX — detect `auth_required` on `session/new`, surface the agent's `auth_methods` with a hint (`codypendent acp login <agent>`).
- **L, med**: client fs capability (route `fs/read_text_file` through IDE dirty-buffer provenance) and terminal capability (sandbox executor).
- **L, med**: map `Plan` → plan/DAG events, `UsageUpdate` → cost display, `AvailableCommandsUpdate` → TUI command palette (feeds rubrics 5/7).

## Extra ideas

- Forward `mcp.toml` servers into `NewSessionRequest.mcp_servers` so Claude/Gemini ACP sessions inherit Codypendent's MCP tools — a two-line win once discovery lands.
- `codypendent doctor` ACP section: registry age, per-profile launch+handshake probe (reusing `probe`'s tool-deny sink).
- Mixed councils are already possible (`acp/claude=architect`); model discovery would let councils pit *specific* agent-models against each other.
- MCP `add`/`test` CLI and TUI server cards (spawn + `tools/list` preview) to match the polish of the ACP picker.
- Registry ETag/If-Modified-Since to cut the 24 h refetch cost.
