# Agent capabilities — `web.search` tool + plan mode

**Status:** design approved 2026-07-30. Third of a 4-PR program adopting selected Codex/Claude-Code CLI features (A operability → B MCP client → **C web-search + plan mode** → D session ergonomics).

**Goal:** two agent powers layered on existing machinery. **C1** — a `web.search` tool: the capability gap in the agent loop, backed by Tavily (key from `TAVILY_API_KEY`), network-gated exactly like the `github.*` reads, with all returned content sanitized as untrusted evidence. **C2** — plan mode as a real UX: read-only explore → the model presents a concrete plan → the human re-submits in Build to execute. No protocol change anywhere.

**Non-goals (v1):** extra search backends (the `SearchApi` trait leaves room); mid-run mode-flip protocol (`SetMode`/`ModeChanged`); approval-carried plans (the `PublishDocument` shape); production wiring of the steering channel; plan artifacts on disk; per-call search approval (reads ride the network allow-list, like github reads).

## Context (verified)

- **HTTP client pattern:** `crates/integrations/src/github/client.rs` — reqwest with a 30s timeout, bounded redirects (5), body ceilings, `set_sensitive(true)` on the auth header. `crates/integrations` owns HTTP clients; `crates/runtime` depends on it and carries no reqwest. Boot-time key discovery precedent: `GitHubToken::discover()` + the boot block `crates/codypendentd/src/lib.rs:121-142` ("no github token found; github tools disabled"). Credential seam: `crates/providers/src/credential.rs` — `AuthMethod::ApiKey { env, header, prefix }`; keys resolve from env **by name**, never stored.
- **Network policy:** `ProposedAction::NetworkRequest { destination }` → `eval_network`; matching is exact `host:port` (`policy/scope.rs:189`). GitHub's endpoint is admitted post-load in `executor.rs` via `policy.admitting_network([GITHUB_API_ENDPOINT])`. A search endpoint follows this path verbatim — **no `ProposedAction` variant, no golden change**.
- **New-tool anatomy — five touch points in `crates/runtime/src/agent.rs`:** `NAME` into `offered_tool_names` (the github gate — `self.X.is_some()` — doubles as the not-configured gate), a `decl(...)` in `static_tool_definitions`, a `prepare` arm, a `PreparedTool` variant, an `execute_prepared` arm; plus a `tools/label.rs` arm. The `github.*` read tools propose `NetworkRequest` — the template.
- **Sanitizer chokepoint:** the MCP arm (`agent.rs:2382-2412`) runs BOTH result and error text through `sanitize_untrusted(origin, …).as_evidence_block()` before anything enters the observation stream. Web content is maximally untrusted — same treatment, origin `search:tavily`.
- **Plan mode today:** `AgentMode::Plan` exists and is enforced read-only by `mode_overlay` (commands yes, writes/network no). Mode travels on `StartRun` **and** `SubmitUserInput` — every continuation carries its own mode, so "approve → implement" needs no protocol change: a Build-mode continuation sees the plan in the session transcript. The CLI already has `run --mode`. The TUI hardcodes `default_mode = Build` and has Model/Provider palette pickers as the picker pattern. No mode-aware prompt exists (one hardcoded system line); the objective is seeded server-side in the loop. Offered tools don't vary by mode — models learn boundaries by bouncing off policy denials.

## C1 — `web.search` (Tavily)

1. `crates/integrations/src/search/`: `pub trait SearchApi: Send + Sync` — `search(query, max_results) -> Result<SearchOutcome, SearchError>`; `SearchOutcome { answer: Option<String>, results: Vec<{ title, url, content }> }`. `TavilyClient` POSTs `https://api.tavily.com/search` (reqwest, 30s timeout, redirects ≤5, sensitive bearer header, body ceiling). `TavilyKey::discover()` resolves from `TAVILY_API_KEY` by env-name only (the `CredentialProvider`/`AuthMethod::ApiKey` seam). `wiremock` tests.
2. Policy: `TAVILY_API_ENDPOINT = "api.tavily.com:443"` next to `GITHUB_API_ENDPOINT`; `load_run_policy` admits it when a search client is configured.
3. Boot: discover → build → `executor.with_search(...)`; absent → "web search disabled". Executor stores `Option<Arc<dyn SearchApi>>` with a `with_search` builder — including the workflow-host rebuild carry (both builder orders safe) and the `drive_agent` site, so workflow agent nodes get the tool too.
4. Runtime tool `web.search` (`tools/web_search.rs`): args `{query, max_results? (default 5, ≤ 10)}`; proposes `NetworkRequest` to the Tavily endpoint. Rendering: answer line + numbered title/url/content results → `sanitize_untrusted("search:tavily", rendered, 64 KiB)` → evidence block (64 KiB because results are model-context, not bulk spill; the MCP 8 MiB cap is for tool bulk). Error path sanitized identically. Label shows the query.
5. Tests: wiremock client tests; loop tests over a stub `SearchApi` (offered only when configured; sanitized framing incl. ANSI stripping, both paths); policy admission test.

## C2 — plan mode

6. Plan-mode instruction (runtime): when `run.mode == Plan`, the loop seeds a server-side instruction with the objective — investigate read-only, then finish with a numbered, concrete implementation plan (files, steps, verification), explicitly not attempting writes. Other modes byte-identical.
7. Mode-aware offered tools: `offered_tool_names` consults `mode_overlay(run.mode)` — drop write tools when `!write_allowed`; `shell.run`/`repository.test`/MCP names when `!command_allowed`; `github.*` when `!network_allowed`. Reads, `workspace.search`, `git.diff`, `memory.remember` always stay. Stops denial-bouncing in every read-only mode while offered ≡ advertised ≡ policy-accepted stays exact. The filter only ever mirrors the overlay's denials — it can never strand a tool policy would allow.
8. TUI `/mode`: a palette picker (the Model/Provider pattern) selecting the submission `default_mode` across Ask/Explore/Plan/Build/Review, shown in the status area. No wire change — outbound intents already read `default_mode`. The handoff: switch to Build, submit "implement it"; the plan is in the transcript.

## Testing

Per-slice tests above; `CapturingDriver` asserts the Plan instruction reaches the transcript; per-mode offered-set tests (Ask, Explore, Plan, Build, Review); TUI reducer/palette tests. Full workspace `fmt`/`clippy`/`test` + vscode vitest; `web.search` offered/absent smoke with and without `TAVILY_API_KEY`.

## Tasks

- **C1 `web.search`** — integrations client + policy admission + boot/executor wiring + runtime tool + tests.
- **C2 plan mode** — instruction + mode-aware offered set + TUI picker + tests.

Independent PR off `main`. Controller-verifies the sanitize chokepoint and the overlay-filter invariant by hand.
