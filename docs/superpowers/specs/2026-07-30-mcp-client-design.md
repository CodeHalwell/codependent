# MCP client support — consume external tool servers through the gated tool layer

**Status:** design approved 2026-07-30. Second of a 4-PR program adopting selected Codex/Claude-Code CLI features (A operability → **B MCP client** → C web-search + plan mode → D session ergonomics).

**Goal:** the agent loop can call tools hosted by external MCP (Model Context Protocol) servers — GitHub, Postgres, a browser, … — without writing Rust per integration. MCP tools slot in as another prepared-tool kind, gated by the SAME policy/approval machinery every other side-effecting tool goes through, with all returned content sanitized as untrusted evidence. Stdio transport only; a hand-rolled ndjson JSON-RPC client in `crates/integrations` (the `acp_client.rs` pattern) — **no new external dependencies**.

**Non-goals (v1):** streamable-HTTP/SSE transport; MCP resources/prompts/roots/sampling/elicitation; `notifications/tools/list_changed` refresh; being an MCP *server*; repo-level server declaration (repos can only *narrow* via policy); OAuth; `doctor` integration.

## Context (verified)

- **Tool dispatch** (`crates/runtime/src/agent.rs`): `prepare()` (`:1693`) matches tool name → typed input + `ProposedAction`, wrapped in `Prepared { action, tool: PreparedTool }` (`:2435`); `execute_prepared` (`:1945`) runs it under policy-granted scopes. `offered_tool_names(&self, run) -> Vec<&'static str>` (`:1013`) is the single source of truth the advertisement and `prepare` agree on; `advertised_tools` (`:3132`) projects the static `tool_definitions()` (`:2897`) down to the offered names; the loop computes the offered set at `:1209` and hands it to `ModelDriver::next_step`.
- **Injection seam:** `FrameworkAgentRuntime` (`:932`) holds `github: Option<Arc<dyn GitHubApi>>` + `with_github()` (`:985`); `runtime` already depends on `codypendent-integrations`. MCP follows this exactly: `mcp: Option<Arc<dyn McpBridge>>` + `with_mcp()`.
- **Policy** (`crates/daemon/src/policy/`): `evaluate` (`mod.rs:298`) → `Allow | Deny | RequireApproval`. `ApprovalAction { Allow, Approval, AlwaysApproval, Deny }` with `more_restrictive` (`config.rs:35-63`) drives the narrow-only repo-layer merge. `MergedPolicy` (`config.rs:72`) is merged from builtin defaults → trusted `<config_dir>/codypendent/policy.toml` → untrusted `<repo>/.codypendent/policy.toml` (`mod.rs:224`), wired in `crates/codypendentd/src/executor.rs:417`.
- **Approvals are free:** `RequireApproval` parks the run (`agent.rs:1538-1647`), round-trips `ResolveApproval` from an Approver/Controller client, audits to the ledger, and a Run-scoped approval auto-approves identical repeats via the canonical action-JSON digest (`approvals.rs:210,743`) — so MCP action args must serialize **canonically**.
- **Sanitizer:** `sanitize_untrusted(origin, raw, max)` (`crates/sandbox/src/sanitize.rs:87`) strips ANSI/controls/bidi/zero-width, caps bytes, and frames content as `[untrusted output from mcp:<server>]` evidence (`:33`) — "MCP is a protocol, not a trust guarantee" (`sanitize.rs` module docs). In-loop evidence precedent: `github_evidence` (`agent.rs:2518`). Bulk output spills via `ArtifactSink` (`tools/mod.rs:234`).
- **`ProposedAction`** (`crates/protocol/src/run.rs:87`): `#[serde(tag = "type")]`, `#[non_exhaustive]`, `Unknown` catch-all (old clients degrade), all variants `Eq` — so the new variant carries args as a canonical JSON **String**, not a `Value`. Golden vectors regenerate additively (`crates/protocol/tests/golden_vectors.rs`; TS drift guard `extensions/vscode/test/protocol-vectors.test.ts`).
- **Client template** (`crates/integrations/src/acp_client.rs`): transport-generic `connect(reader, writer)` (`:264`), `spawn` over child stdio (`:276`), mpsc + driver-task bridge (`:177-200`, `:288-306`), ndjson `Lines` framing (`:588-616`), scripted-peer test harness (`:993+`). MCP stdio is newline-delimited JSON-RPC 2.0 — the same framing discipline.
- **Config-loader pattern:** `models.rs:96-144` / `trust_store.rs:31-45` — bare `[[server]]` TOML array, `thiserror` Read/Parse error enum, **missing file = empty**, malformed = hard error, tempfile unit tests. Trusted operator config lives in `config_dir` (the `policy.toml` precedent).
- **No MCP dependency exists** in Cargo.lock (`rmcp`/`jsonrpc` absent); `tokio::process` is the spawn convention.
- The `mcp-stdio` groundwork in `crates/sandbox/src/manifest.rs` (`RuntimeSpec.protocol`, `PluginKind::McpRemote`) is the *plugin* boundary and stays untouched — this PR is about the agent consuming MCP tools, not sandboxing plugins.

## Design

### 1. Server declaration — `<config_dir>/codypendent/mcp.toml` (trusted operator config)

```toml
[[server]]
name = "github"                      # used in tool names + policy + logs
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = [["GITHUB_TOKEN", "…"]]        # optional explicit pairs, merged over inherited env
# inherit_environment = true         # default; the launch line is operator-trusted
```

Missing file = no servers (fine). Malformed = hard error at daemon boot (the `policy.toml` convention). A server absent from this file is unreachable — the model can never name one into existence. Env inheritance defaults on because the launch line is operator-declared (unlike `ExecuteCommand`'s model-controlled env, which defaults empty); `inherit_environment = false` + explicit `env` gives a hermetic launch.

### 2. Policy — `[mcp]` section

```toml
[mcp]
default = "approval"          # ApprovalAction for servers without an explicit entry

[mcp.servers]
github = "allow"              # operator-trusted server: skip per-call approval
filesystem = "always-approval"
experimental = "deny"
```

`MergedPolicy` gains `mcp_default: ApprovalAction` (builtin `Approval`) and `mcp_servers: BTreeMap<String, ApprovalAction>`; the repo layer merges narrow-only via `more_restrictive`. `eval_mcp_tool_call`: disposition = `mcp_servers[server].unwrap_or(mcp_default)` → `Allow` / `RequireApproval` / `Deny`; `AlwaysApproval` requires a fresh approval every call (no pattern carry-over). Run-scoped approvals auto-approve identical repeats through the existing digest path.

### 3. Protocol — one additive variant

```rust
ProposedAction::McpToolCall {
    server: String,
    tool: String,
    /// One-line human summary rendered on the approval card.
    summary: String,
    /// Canonical JSON (recursively key-sorted) of the model-supplied arguments —
    /// shown verbatim on the approval card and covered by the action digest.
    args: String,
}
```

One golden-vector instance added; regeneration must be additive-only; TS vitest stays green. Old clients see `Unknown` and degrade.

### 4. The MCP client — `crates/integrations/src/mcp/`

- `config.rs` — the `mcp.toml` loader (pattern above).
- `jsonrpc.rs` — minimal JSON-RPC 2.0 over ndjson: request/response/notification types, id-correlated pending map, bounded frame size, per-request timeouts; transport-generic `connect(reader, writer)` + `spawn(command, args, env, cwd)` via `tokio::process`; mpsc + driver task (the `acp_client.rs` bridge shape).
- `client.rs` — MCP semantics: `initialize` (offer the newest protocol version we speak, accept the server's), `notifications/initialized`, `list_tools()`, `call_tool(name, args)`. Server→client notifications are logged and ignored in v1.
- `registry.rs` — `McpRegistry` built from `McpConfig`: per-server lazy spawn, cached tool list, one respawn+retry on a dead child, then a legible error. Exposes `pub trait McpBridge: Send + Sync` — offered tool infos (name/description/inputSchema) + `call_tool(server, tool, args)`. The runtime faces **only** this trait via `Arc<dyn McpBridge>` (the `GitHubApi` precedent), keeping the client testable with scripted `tokio::io::duplex` peers.

### 5. Runtime wiring (`crates/runtime/src/agent.rs`)

- `with_mcp()` builder; `offered_tool_names` becomes `Vec<String>` and appends `mcp.<server>.<tool>` names from the bridge's cache (a cold/dead server simply contributes no tools until warm).
- `prepare` gains an `mcp.` prefix arm: split server/tool, canonicalize the model's args (recursive key-sort → String), build the `McpToolCall` action + `PreparedTool::Mcp{..}`.
- `execute_prepared` MCP arm: bounded `call_tool` → result text through `sanitize_untrusted(format!("mcp:{server}"), …)` → evidence-block observation; bulk output spills via `ArtifactSink` (shell precedent); failures are `ToolError`s with stable dotted codes (`mcp.unavailable`, `mcp.call_failed`).
- Advertisement: the driver handoff gains the bridge-provided `ToolDefinition`s (server-supplied description + `inputSchema` verbatim; `ApprovalMode::NeverRequire` — policy gates, not the framework) alongside the filtered static catalog. The single-source-of-truth contract (`offered_tool_names` ≡ advertised ≡ dispatchable) and its drift test are preserved. `tool_label` gets an `mcp.*` arm.

### 6. Assembly + surfaces

- `crates/codypendentd/src/executor.rs`: load `mcp.toml` where policy loads, build the registry, `with_mcp(..)`, pre-warm servers as fire-and-forget background tasks (code-graph scan precedent); a server that won't start is logged, never fatal.
- TUI: `McpToolCall` arms in `reduce.rs` (summary) and `render.rs` (approval card shows server, tool, args verbatim).
- CLI: `codypendent mcp list` prints each configured server's launch command, policy disposition, and env summary — config-level only, no spawning.

## Testing

- `integrations`: config loader (missing/malformed/happy); JSON-RPC over duplex (handshake, list, call, error, timeout, unsolicited-notification tolerance); registry (lazy spawn, cache, respawn-on-death) with scripted peers.
- `daemon` policy: disposition resolution, narrow-only repo merge, `AlwaysApproval` semantics.
- `runtime` (agent_it-style, `ScriptedDriver` + stub `McpBridge`): MCP tools offered; full prepare → RequireApproval → park → approve → execute path; sanitized evidence block in the transcript; Run-scope auto-approval of an identical repeat; denied and unknown servers → legible tool errors.
- `protocol`: golden regeneration additive-only + round-trip; vscode vitest.
- `cli`: `mcp list` render test on temp files.
- Full workspace `cargo fmt --check` / `clippy --workspace --all-targets` / `cargo test --workspace`.

## Tasks

- **B1 protocol + policy** — `McpToolCall` variant + golden; `[mcp]` config + `eval_mcp_tool_call` + tests.
- **B2 integrations client** — `mcp/` module (config, jsonrpc, client, registry) + duplex tests.
- **B3 runtime wiring** — builder, offered names, prepare/execute arms, advertisement, labels + agent tests.
- **B4 assembly + surfaces** — executor wiring + pre-warm, TUI arms, `mcp list`, full gate, PR.

Independent PR off `main`. Controller-verifies by hand: the policy narrow-only merge, the sanitize chokepoint in `execute_prepared`, and the golden-vector diff.
