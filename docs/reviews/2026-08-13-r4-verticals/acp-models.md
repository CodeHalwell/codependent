# Vertical: acp-models (round 4)

Reviewed at the pinned commit `c255bec8b175d62942b3312cff2335b97d43a59a` (v0.5.1)
against `target/debug/codypendent` + `target/debug/codypendentd` built by the
orchestrator. No `cargo build`/`cargo test` was run by me.

Scope, all read in full: `crates/integrations/src/acp.rs`,
`crates/integrations/src/acp_client.rs`, `crates/integrations/src/acp_registry.rs`,
`crates/cli/src/acp.rs`, `crates/cli/src/acp_clients.rs`,
`crates/cli/src/models_file.rs`, `crates/cli/src/models_pull.rs`,
`crates/providers/**` (`catalog.rs`, `model.rs`, `credential.rs`, `lib.rs`,
`builtin_catalog.toml`), plus the consuming sites in `crates/cli/src/tui.rs`,
`crates/cli/src/commands.rs`, `crates/tui/src/reduce.rs`, `crates/tui/src/render.rs`,
`crates/runtime/src/models.rs` and `crates/codypendentd/src/executor.rs`.

**Everything marked "observed" below was run.** A fake ACP *agent*
(`/tmp/review-acp-models/bin/cursor-agent`, a Python JSON-RPC responder that logs
the wire) was put on `PATH` so `local_acp_agent_spec("cursor")` resolves it; a
hand-written ACP *client* (`/tmp/review-acp-models/acp_client.py`) drove
`codypendent acp serve` over stdio; a real `codypendentd` ran against
`/tmp/review-acp-models/data`; SQLite was queried directly afterwards; the TUI was
driven in a pty through a real terminal emulator (`pyte`).

---

## Verdicts

**OUTCOME 2 (ACP fully working, incl. automatic model discovery): PARTIAL.**
Both directions now carry a prompt end to end — this is a real repair, verified on
the wire. Serve mode answered `session/prompt` with `{"stopReason":"end_turn"}` and
streamed a genuine `agent_message_chunk`; the previous round's total failure is
gone. But a **failed** run is reported to the ACP client as that same successful
`end_turn`, with the reason discarded; `session/new`'s `cwd` is still ignored
entirely; cancelling a turn leaves the run recorded as `Failed` with an internal
error and poisons long-term memory; and discovery is one-directional — Codypendent
reads an agent's models and publishes none of its own.

**OUTCOME 3 (easy model selection; prefilled lists): PARTIAL.**
**42 providers / 386 curated models** shipped (counted from
`crates/providers/builtin_catalog.toml`, not from any doc). `--model` now exists on
`run` and works, including ACP model pins. Anthropic is now reachable from both TUI
and CLI, and the catalog's context windows are — contrary to the previous round's
report — **accurate where I could check them against a live registry**. What is
left: 6 of 42 providers cannot be added at all and nothing says so; the prefilled
list is gated behind an API-key prompt that mislabels its own purpose; and
per-provider context values disagree with the provider's own `/models` for Venice.

---

## What the previous round found, and what is true now

| Prior finding | State at `c255bec` | Evidence |
|---|---|---|
| F1 serve mode fails every prompt | **FIXED** | `require_attached` (`crates/cli/src/acp.rs:80-91`); observed a live `end_turn` + `agent_message_chunk` |
| F2 `session/new` ignores `cwd` | **NOT FIXED** | observed both directions (A/B below) |
| F3 Anthropic unreachable (TUI) / mis-wired (CLI) | **FIXED** | `provider_runtime_supported` (`crates/cli/src/tui.rs:6713-6716`), `config_to_protocol_auth` (`crates/runtime/src/models.rs:795-800`); `models check` hit `https://api.anthropic.com/v1/models` |
| F4 provider `context_length` beats the catalog, reaches `num_ctx` | **FIXED** | `context_tokens_for` (`crates/runtime/src/models.rs:836`), consumed at `crates/runtime/src/agent.rs:6101` |
| F5 "every 1M context value overstated" | **LARGELY WRONG / now mostly right** | 24/24 openrouter rows match the live registry exactly; see §Catalog accuracy |
| F6 no `--model` on `run` | **FIXED** | `codypendent run --model` exists, works, validates |
| F7 selection diagnostics never reach the client | **FIXED** | `RunCompleted{Failed, reason:"pinned model … is not available: …"}` on `--jsonl` |
| F8 `models list-providers` does not exist | **FIXED (partially honest)** | command exists; see A8 |
| F9 `models list` says `key: none` | **NOT FIXED** | observed |
| F10 ACP profiles unbenchable / invisible to the router | **NOT FIXED** | `could not build client for acp/cursor: … protocol acp … not wired in this build` (daemon log) |
| F11 serve mode exposes no models | **NOT FIXED** | observed, `-32601` on `session/set_config_option` |
| `models add` destroys `[embedding]`/`[retrieval]`/`[transcription]`/`[speech]` | **FIXED, and fixed as a class** | see §models.toml |

---

## Outcome 2 — the live wire

### Client direction (Codypendent discovering an agent's models): WORKING

```
$ codypendent acp connect cursor --repo /tmp/review-acp-models/repo
connected Cursor 2026.08.01 as model `acp/cursor` (agent default)
discovered 3 agent model(s):
  * acp/cursor#fake-sonnet                   Fake Sonnet (fake-sonnet)
    acp/cursor#fake-opus                     Fake Opus (fake-opus)
    acp/cursor#fake-mini                     Fake Mini (fake-mini)
session modes: Build, Review
```

Agent-side wire log (verbatim):

```
CLIENT->AGENT {"jsonrpc":"2.0","id":"d9f882d8-…","method":"initialize","params":{"protocolVersion":1,
  "clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false}}}
CLIENT->AGENT {"jsonrpc":"2.0","id":"7baa553b-…","method":"session/new",
  "params":{"cwd":"/tmp/review-acp-models/repo","mcpServers":[]}}
```

`models.toml` afterwards (`cat`):

```toml
[[model]]
id = "acp/cursor"
model = "cursor@2026.08.01"
provider = "acp"
[[model]]
id = "acp/cursor#fake-sonnet"
model = "cursor@2026.08.01#fake-sonnet"
provider = "acp"
… (#fake-opus, #fake-mini)
```

`codypendent acp status`, `codypendent doctor` and `codypendent acp probe cursor`
all report them ready; `acp list --refresh` fetched the live registry
(`https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json`, **38
agents**, `version 1.0.0`) and printed 39 rows (38 + the pinned community
antigravity bridge).

A model pin is honoured on the wire:

```
$ codypendent run --objective "say hello" --repo … --model 'acp/cursor#fake-opus' --jsonl
# agent received:
{"method":"session/set_config_option","params":{"sessionId":"fake-session-1","configId":"model","value":"fake-opus"}}
```

This half of outcome 2 is genuinely done.

### Serve direction: prompts now work

```
CLIENT->AGENT: {"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"019ffd50-eebc-…",
                "prompt":[{"type":"text","text":"say hello"}]}}
AGENT->CLIENT: {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"019ffd50-eebc-…",
                "update":{"sessionUpdate":"agent_message_chunk",
                          "content":{"type":"text","text":"ACP LIVE OK (model=fake-sonnet)"}}}}
AGENT->CLIENT: {"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}
```

`session/cancel` also resolves the turn correctly (`{"stopReason":"cancelled"}`).
The round-3 repair is real.

---

## Findings

### A1 — A failed run is reported to the ACP client as a *successful* `end_turn`, and the reason is thrown away. Class (c). TOP FINDING.

`crates/cli/src/acp.rs:250-255`:

```rust
EventBody::RunCompleted { disposition, .. } => {
    return Ok(match disposition {
        RunDisposition::Cancelled { .. } => StopReason::Cancelled,
        _ => StopReason::EndTurn,
    });
}
```

`RunDisposition::Failed { reason: String }` (`crates/protocol/src/run.rs:66-68`)
carries the reason in the very event being matched; the `_` arm discards both the
variant and the string. ACP has a `refusal` stop reason and JSON-RPC has errors;
neither is used.

Observed, with `--repo` pointed at a git repo that has no commit:

```
### session/prompt -> {"jsonrpc": "2.0", "id": 3, "result": {"stopReason": "end_turn"}}
```

daemon, same run:

```
WARN codypendentd::executor: run did not execute; failing it cleanly
  run_id=019ffd51-797d-… reason=could not allocate an isolated worktree:
  `git rev-parse HEAD` failed: fatal: ambiguous argument 'HEAD': unknown revision …
```

Reproduced a second time with a different cause (`no model configured: every
candidate failed for Build: openai/gpt-5.4: … HTTP 401 …`) — same `end_turn`, zero
`agent_message_chunk`.

**User-visible:** in Zed, the user types a prompt, watches a large "thinking" blob
(A5), and the turn ends with no reply and no error. Success and failure are
indistinguishable. This is worse than the round-3 bug it replaced: that one at
least *said* something was wrong.

### A2 — `session/new`'s `cwd` is ignored; every session is pinned to the process's `--repo`. Class (c). Unchanged from round 3.

`AcpBackend::new_session(&self)` takes no cwd (`crates/integrations/src/acp.rs:163`);
`serve` calls `backend.new_session()` without touching `params`
(`crates/integrations/src/acp.rs:488-494`); `DaemonAcpBackend::new_session` builds
`CreateSession { repository: Some(self.repo…) }` (`crates/cli/src/acp.rs:111-129`),
and `prompt` does the same for `AttachSession`/`StartRun` (`:154`, `:164`).

The SDK schema is unambiguous — `NewSessionRequest.cwd` is required:
`agent-client-protocol-schema-1.5.0/src/v1/agent.rs:1011-1013`,
*"The working directory for this session. Must be an absolute path."*

Observed, A/B, using two repos with distinguishable contents
(`repo` = one `fn main`; `other-project` = `fn one/two/three/four`), reading the
repository map the daemon streams back:

| run | `--repo` | `session/new` `cwd` | repository actually used |
|---|---|---|---|
| A | `…/repo` | `…/other-project` | `50068a78-…` — *"1 APIs … fn main"* |
| B | `…/other-project` | `…/repo` | `262918fe-…` — *"4 APIs … fn four, fn one, fn three, fn two"* |

Daemon confirms: `code-graph scan complete repository=50068a78-… files=1 nodes=2`
and `repository=262918fe-… files=2 nodes=6`.

**User-visible:** a client with two projects open gets two sessions that both edit
whichever directory the `codypendent acp serve` process was started in. Zed's own
agent-server config launches one process per workspace, which masks it — but a
client that multiplexes (the spec's model) silently operates on the wrong tree.

### A3 — Cancelling an ACP turn records the run as `Failed` with an internal error, and writes that error into long-term memory. Class (c).

The bridge's cancel arm (`crates/cli/src/acp.rs:189-208`) opens a fresh connection
and sends `CancelRun`; the client is told `cancelled`. The daemon then
double-transitions:

```
sequence 5  RunStateChanged {"state":{"type":"Cancelled"}}
sequence 7  RunStateChanged {"state":{"type":"Failed"}}
sequence 8  RunCompleted     {"disposition":{"type":"Failed",
              "reason":"refused stale runtime transition for 019ffd53-6ce5-…: Some(Cancelled) -> Cancelled"}}
sequence 9  NoteAppended     {"text":"remembered: Run failed: refused stale runtime transition …"}
```

`select id, state from runs` → `Failed`. Reproduced 2/2.

And it compounds. `select statement from memories order by rowid desc`:

```
Run failed: refused stale runtime transition for 019ffd54-563b-…: Some(Cancelled) -> Cancelled
Run failed: refused stale runtime transition for 019ffd53-6ce5-…: Some(Cancelled) -> Cancelled
Run failed: could not allocate an isolated worktree: `git rev-parse HEAD` failed: …
Run failed: no model configured: every candidate failed for Build: openai/gpt-5.4: …
```

Those rows are then **injected into every later run's context** — I read them back
verbatim in the `=== MEMORIES ===` block of a subsequent run's manifest. So an
internal state-machine message becomes a durable "learning" the model is shown.

**User-visible:** press Esc in Zed, and the run history shows a failure with a Rust
`Debug` string; every future run in that repository is prompted with it.
(The executor half is another vertical's code; it is reported here because it is on
the outcome-2 path and I observed it there.)

### A4 — Serve mode publishes no models, no modes, and 4 of ~16 v1 methods. Class (a) for the reverse discovery, (b) for the rest.

Observed:

```
AGENT->CLIENT: {"id":1,"result":{"protocolVersion":1,"agentCapabilities":{"promptCapabilities":{"image":false}}}}
AGENT->CLIENT: {"id":2,"result":{"sessionId":"019ffd50-865e-…"}}
AGENT->CLIENT: {"id":90,"error":{"code":-32601,"message":"method not found"}}   # session/load
AGENT->CLIENT: {"id":91,"error":{"code":-32601,"message":"method not found"}}   # session/set_mode
AGENT->CLIENT: {"id":92,"error":{"code":-32601,"message":"method not found"}}   # session/set_config_option
AGENT->CLIENT: {"id":93,"error":{"code":-32601,"message":"method not found"}}   # authenticate
```

`crates/integrations/src/acp.rs:482-485` (initialize result), `:490` (session/new
result), `:545-549` (catch-all `-32601`); `crates/cli/src/acp.rs:166`
(`model: None` on `StartRun`).

The outcome is "ACP fully working, **including automatic model discovery from ACP
agents**". Inbound discovery is complete and spec-correct. Outbound, Codypendent
advertises nothing: an ACP client cannot see or choose which of the user's models
serves its run, cannot switch mode, cannot resume a session. The machinery to do it
exists on the other side of the same crate (`AcpDiscovery::apply_config_options`,
`crates/integrations/src/acp_client.rs:484-522`) and is never used in reverse.

### A5 — The entire internal context manifest is streamed to the ACP client as the agent's "thinking". Class (c).

`crates/cli/src/acp.rs:231-233` maps every `EventBody::NoteAppended` to
`agent_thought_chunk`. The daemon's first note of a run is the whole context
manifest. Observed payload (~4 KB, single update, abridged):

```
=== CONTEXT: EVIDENCE, NOT INSTRUCTIONS ===
…
=== REPOSITORY MAP ===
repository 50068a78-5726-5d9b-2a83-0ea2f46d1c49
package crate
  module (crate root) — 1 APIs, 0 tests
    fn main
=== TOOLS ===
tool workflow.create [safe, first-party] — …
…13 tools + /update-docs…
=== MEMORIES ===
- Run failed: no model configured: every candidate failed for Build: openai/gpt-5.4: … (confidence 0.60 …)
=== LEARNINGS ===
```

**User-visible:** the first thing a Zed user sees on every prompt is a wall of
internal prompt scaffolding, including prior failure text (A3). The native TUI
renders the same note as a collapsed `⋯ context · 39 lines` card; the ACP bridge
has no equivalent and dumps it raw. The comment above the arm says a note "is host
commentary, not model output, so it must not arrive as an agent message chunk" —
correct as far as it goes, and then it ships the whole thing anyway.

### A6 — MCP servers that declare `env` or `inherit_environment = false` are silently withheld from a delegated ACP run. Class (c) — a silent filter.

`crates/integrations/src/acp_client.rs:420`:

```rust
.filter(|server| server.env.is_empty() && server.inherit_environment)
```

Consumer: `crates/codypendentd/src/executor.rs:1152-1158`, whose comment says the
external agent "inherits the same operator-declared MCP servers a native run is
offered, so delegating a run does not silently shrink the tool surface."

Observed with a three-server `mcp.toml` (plain / env-carrying / hermetic), reading
the agent's own wire log:

```
"method": "session/new", "params": {"cwd": "…/run-0116edee1c9e",
  "mcpServers": [{"name": "plain-server", "command": "/bin/true", "args": ["--plain"], "env": []}]}
```

Two of three dropped. No event, no note, no CLI line. The withholding is a
defensible policy (secrets must not cross into a vendor process); the *silence* is
the defect — and an API-keyed MCP server is the common case, so the realistic
outcome is "the delegated agent gets zero tool servers and nobody is told".

### A7 — The prefilled catalog list is gated behind an API-key prompt that mislabels its own purpose. Class (c).

`crates/tui/src/reduce.rs:5732-5737`:

```rust
let can_offer = can_list_models || catalog_models > 0;
state.overlay = if can_offer && requires_key && !has_key {
    Overlay::AddModelProviderKey { … }
```

The prompt reads (`crates/tui/src/render.rs:3341`):
`"API key for {provider_id} (used to list its models; stored locally 0600)"`.

Observed in a pty, no `ANTHROPIC_API_KEY` set: `/provider` → search `anthropic` →
details pane says **`models: catalog 10 models`** → Enter →

```
┌────────────────────────────────────────────────────────────────────────────────┐
│API key for anthropic (used to list its models; stored locally 0600)            │
│› █                                                                             │
│Enter to submit · Esc to cancel                                                 │
└────────────────────────────────────────────────────────────────────────────────┘
```

Esc closes the whole flow. Typing any junk string proceeds to:

```
╭ Choose model · Step 2 of 2 · anthropic · 10 of 10 · catalog · no listing endpoint ─╮
│  ~ claude-fable-5      — · ctx 1000k · in $10 · out $50                            │
│  ~ claude-opus-5       — · ctx 1000k · in $5  · out $25                            │
…
```

So the key is demanded to display a list the very next screen admits has **no
listing endpoint** — `provider_can_list_models` returns `false` for
`Protocol::Anthropic` by design (`crates/cli/src/tui.rs:6696-6699`). With
`ANTHROPIC_API_KEY` set, the prompt is skipped and the 10 rows appear immediately;
with Ollama down, the flow correctly degrades to
`7 of 7 · catalog · could not connect to the provider`. The catalog fallback is
built and good — it is just unreachable on a first run without a key.

**User-visible:** the headline of outcome 3 ("prefilled model lists") does not work
for the exact user it is for — someone who has not configured a key yet.

### A8 — `models list-providers` lists 6 providers that cannot be added, unmarked, and `models add --help` names one of them as its example. Class (c).

`crates/cli/src/commands.rs:3326-3351` prints id / protocol / curated count and
nothing about usability:

```
$ codypendent models list-providers
anthropic            anthropic      10 model(s) curated
azure-openai         openai-chat    7 model(s) curated
claude-code          acp            0 model(s) curated
codex                acp            0 model(s) curated
cursor               acp            0 model(s) curated
gemini-cli           acp            0 model(s) curated
opencode             acp            0 model(s) curated
```

```
$ codypendent models add azure-openai gpt-5.4
Error: provider `azure-openai` has no base URL and cannot be added
$ codypendent models add claude-code sonnet
Error: provider `claude-code` has no base URL and cannot be added
```

`models add --help` says: *"The catalog provider id (`codypendent models
list-providers` spellings: `openai`, `nebius`, `azure-openai`, …)"* — it recommends
`azure-openai` by name.

Computed from the catalog + the two gates
(`provider_runtime_supported`/`provider_endpoint_usable`,
`crates/cli/src/tui.rs:6677-6716`):

* 42 providers; **36 addable**, **6 not**.
* `azure-openai` strands **7 curated models** with no route at all (it has no
  `base_url` because the user must supply their own resource URL — nothing offers
  to take one).
* the 5 ACP catalog entries (`claude-code`, `codex`, `cursor`, `gemini-cli`,
  `opencode`) are reachable only through `codypendent acp connect`, which neither
  the listing nor the error mentions.
* 5 addable providers curate **zero** models (`ai21`, `github-models`, `lambda`,
  `lmstudio`, `vllm`) — those depend entirely on a live `/models` call.

The TUI is better here: its picker merges the catalog with the live ACP registry
("Provider catalog · Step 1 of 2 · **76 of 76 adapters**") and shows availability
per row. The CLI enumeration is the one that lies by omission.

### A9 — `models list` reports `key: none` for entries that do resolve a key. Cosmetic, unchanged from round 3.

```
openai/gpt-5.4
    openai · https://api.openai.com/v1 · context: 1050000 · key: none
anthropic/claude-opus-5
    anthropic · https://api.anthropic.com · context: 1000000 · key: none
```

Both resolve `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` from the catalog at call time
(`crates/runtime/src/models.rs:801-810`). "key: none" reads as "needs no key".

### A10 — A sixth `models.toml` writer bypasses the shared lock. Class (c), **inferred, not observed**.

Five writers go through the one module (verified by grep, all call sites):

```
crates/cli/src/commands.rs:3084   models add
crates/cli/src/models_pull.rs:280 models pull
crates/cli/src/acp_clients.rs:422 acp disconnect
crates/cli/src/acp_clients.rs:525 acp connect
crates/cli/src/tui.rs:4257        TUI add-model
```

`write_remove_model` (`crates/cli/src/tui.rs:4509-4599`) is the sixth. It does
*not* reintroduce the destructive class — it uses `toml_edit` and preserves
everything, which I verified live (below) — but it read-modify-writes without
taking `.models.toml.lock`, the advisory lock `update_model_entries` uses
(`crates/cli/src/models_file.rs:61-76`), and it names its temp file
`.models-remove-{pid}.tmp`, the pid-only scheme whose hazard `models_file.rs:112-118`
documents. A remove concurrent with any of the five locked writers can silently
drop the other's edit. I did not construct that race.

### A11 — Catalog accuracy: better than reported, still uneven. Class (c), narrow.

Counted, not quoted: **42 providers, 386 `[[model]]` rows**; 145 rows carry no
`context_tokens`; **52 rows still hold `1048576`**.

Against the **live** OpenRouter registry (`https://openrouter.ai/api/v1/models`,
411 models, fetched during this review):

* **24 of 24** curated `openrouter` rows exist upstream (round 3's missing
  `anthropic/claude-haiku-4-5` is gone).
* **0 of 24 context mismatches.** `1048576` is what OpenRouter itself reports for
  `google/gemini-3.6-flash`, `deepseek/deepseek-v4-pro`, `moonshotai/kimi-k3`,
  `z-ai/glm-5.2`, `minimax/minimax-m3`. The round-3 claim that *every* "1M" value
  is overstated does not hold at this commit. (The two `1047576` rows are
  `openai/gpt-4.1`, which is that exact number upstream — not a typo.)
* **3 of 24 price mismatches**, all overstatements:
  `google/gemini-3.6-flash` $1.5/$7.5 vs live $0.75/$3.75 (2×);
  `z-ai/glm-5.2` $0.5/$3.15 vs $0.392/$1.232;
  `z-ai/glm-5.1` $1.4/$4.4 vs $0.952/$2.992.
  The section header above those rows reads *"re-verified 2026-08-13 against
  https://openrouter.ai/api/v1/models"*. Three rows disagree with that exact
  endpoint on the date claimed.

Against Venice's own public `/models` (110 models): **7 of 16** curated venice rows
have the wrong context window — the catalog carries the power-of-two family value
(`1048576`, `262144`, `131072`) where Venice serves round numbers (`1000000`,
`256000`, `128000`). Against Chutes' public list, the 4 rows it exposes match
exactly. So the real defect is not "1M is wrong" but **one number per model family,
applied to every provider that serves it**, when each provider publishes its own.
Consequence is bounded — the live `/models` value wins when reachable
(`merge_catalog_rows`), and `context_tokens_for` clamps to the curated row — so the
overstatement only bites offline or catalog-only adds.

DeepInfra: 40 of 41 curated ids present upstream; `canopylabs/orpheus-3b-0.1-ft`
is not in DeepInfra's public list.

---

## `models.toml` — the class fix holds

`crates/cli/src/models_file.rs` is a genuine shared writer: parse to
`toml::Value`, replace only the `model` key, atomic rename at 0600, under an
advisory lock, with a per-write temp ticket. Seeded a file with `[embedding]`,
`[retrieval]` (`builtin_top_k = 0`), `[transcription]`, `[speech]`, `[voice]` and
`[a-future-table]`, then ran every reachable writer:

```
### models add        → tables: a-future-table, embedding, retrieval, speech, transcription, voice  ✓
### acp disconnect    → same ✓
### acp connect       → same ✓
### TUI remove (Ctrl-D on lmstudio/junk) → same ✓, retrieval = {builtin_top_k: 0, mcp_top_k: 12}
```

`models pull` uses the same module (`models_pull.rs:280`); with no `ollama` on PATH
it fails cleanly (`` `ollama` was not found on PATH — install it from
https://ollama.com, then retry ``) after resolving the real repo and quant against
the live Hub, and registers nothing. The round-3 headline defect is fixed at the
class, not the instance. A10 is the one remaining gap in that discipline.

---

## External research

* **`agent-client-protocol` is pinned `"2"` → `2.0.0` in `Cargo.lock`, which is the
  newest on crates.io** (`max_version: 2.0.0`, updated 2026-07-23). Not stale.
  It pulls `agent-client-protocol-schema 1.5.0`; **1.6.0** exists upstream
  (2026-07-21) but the lock pins 1.5.0 — a `cargo update` decision, not a defect.
* **`agent-framework-{core,openai,anthropic}` are all `0.2.0`, the newest**
  (2026-08-08). Nothing fabricated.
* **Protocol usage is correct in the client direction.** `ProtocolVersion::V1` is
  sent; the served version is clamped to 1 rather than echoed
  (`crates/integrations/src/acp.rs:474-484`) — the right call, with a good comment.
  Model discovery keys on `SessionConfigOptionCategory::Model` + a
  `SessionConfigKind::Select`, which is the spec's mechanism; grouped and ungrouped
  option shapes are both flattened; the full set is rebuilt on every arrival so a
  dropped selector leaves no stale list. I confirmed the wire shape against the
  SDK's own types: my first fake agent used `"kind":"select"` and discovery
  silently found **zero** models; changing it to the schema's `"type":"select"`
  discriminator (`schema/src/v1/agent.rs:2496-2505`) made all three appear. That
  tolerant-parse behaviour is the SDK's (`DefaultOnError`), not the repo's, but it
  means a subtly non-conforming agent yields "the agent advertised no model
  selector" with no diagnostic — worth knowing when a real vendor agent shows empty.
* **The community Antigravity descriptor is real.** `community_acp_agent`
  (`crates/integrations/src/acp_registry.rs:557-613`) hard-codes four SHA-256s
  against `github.com/shubzkothekar/antigravity-acp` v1.0.0. The repo and that
  release exist (fetched); the asset list would not render for me, so **I did not
  verify the four digests**. The install path refuses an unpinned binary and
  verifies the digest before extraction, and the CLI fails closed without
  `--accept-community-risk` (verified: the error names Google's ToS risk).
* Registry hardening (`AcpRegistry::validate`, `install_archive`, `safe_join`,
  bounded reads, HTTPS-only redirects, `.archive.sha256` marker check in
  `launch_spec_for`) is thorough and I found nothing wrong with it by reading.

---

## What I did NOT verify

* **A real vendor ACP agent** (claude-acp, codex-acp, gemini). All need vendor auth
  and an `npx` network install; I substituted a scripted agent so I could control
  the `configOptions` payload and read the wire. Whether Claude Code's real agent
  advertises a Model-category selector is unanswered.
* **A live hosted model call.** No API keys. `models check anthropic/claude-opus-5`
  reached `https://api.anthropic.com/v1/models` (correct route — the round-3 defect
  is fixed) but the proxy refused the connection, so no response was parsed.
  `models check openai/gpt-5.4` produced a real HTTP 401 via the daemon's own probe,
  which is the strongest live evidence I have for the OpenAI-compatible path.
* **`models pull`'s success path and `models bench`** — no `ollama`, no GPU.
* **The A10 concurrency race** — reasoned from the code, not constructed.
* **The four Antigravity SHA-256 digests** — GitHub asset listing blocked.
* **Permission requests over serve mode.** My fake agent never requests one, and I
  could not get Codypendent's own tools to park an approval inside an ACP turn in
  this container (no `bwrap`, so tool execution fails closed). `resolve`
  (`crates/cli/src/acp.rs:294-323`) and the `ApprovalRequested`/`ToolProposed`
  de-duplication are read-only assessments.
* **Provider-side accuracy for openai / anthropic / gemini / groq / together etc.**
  — no unauthenticated catalog endpoint. I could diff only openrouter, venice,
  chutes and deepinfra live; every other provider's ids and numbers are unchecked.

---

## The pattern

Round 3's diagnosis was *"the final wire is attached to the wrong terminal."* Round
4's is narrower and more specific: **the wire is now attached, and it carries only
the happy path.** Every defect above is a *reverse-direction* omission at a seam
that works forwards. `RunCompleted` is consumed but its `Failed` reason is dropped
(A1). `session/new` is answered but its one required parameter is not read (A2).
Cancel reaches the daemon but the daemon's answer is not reconciled (A3). Models
are discovered *from* agents and published to none (A4). Notes are forwarded but
not shaped (A5). MCP servers are filtered but the filtering is never reported (A6).
The catalog is complete but the enumeration of it does not say which half is usable
(A8). In each case the producer exists, the consumer exists, and what is missing is
the *error, absence, or refusal* travelling back the other way. The class of bug has
moved from "no wire" to "one-way wire" — which is progress, and which is why every
one of these still reads to a user as silence rather than as a failure.
