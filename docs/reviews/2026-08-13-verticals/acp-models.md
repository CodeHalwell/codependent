# Vertical: acp-models

Reviewer scope: `crates/integrations/src/acp*.rs`, `crates/providers/**`,
`crates/cli/src/{acp,acp_clients,models_pull,doctor,finetune,update}.rs`,
`crates/runtime/src/{models,auth,agent}.rs`, `crates/routing/src/{profile,capability}.rs`,
`crates/daemon/src/model_profiles.rs`, `crates/providers/builtin_catalog.toml`.

Everything below was run against the pinned build (`0.4.5+535a2f5e3848`) with a real
daemon, a real ACP wire, and the live ACP registry. Wire logs quoted verbatim.

---

## Verdicts

**OUTCOME 2 (ACP fully working, incl. automatic model discovery from ACP agents): PARTIAL**
— the *client* direction is genuinely complete and works end to end on a real wire
(automatic model discovery is implemented, spec-correct, and observed working), but the
*server* direction (`codypendent acp serve`, the thing Zed talks to) fails **every**
`session/prompt` with a JSON-RPC error because it expects the wrong daemon reply type.

**OUTCOME 3 (easy model selection; prefilled model lists for non-ACP providers): PARTIAL**
— a 42-provider / 387-model curated catalog exists and *is* wired into the TUI add-model
pick list and `codypendent models add`, but the runtime-support gate makes Anthropic
(10 curated rows) unreachable in the TUI while the CLI silently writes an entry for it
that can never work, there is no `--model` on the headless `run` path at all, and the
catalog's context windows are systematically wrong (`1048576` used to mean "1M").

---

## What I verified working (so the failures below are read correctly)

Automatic ACP model discovery is **real and works**. I stood up a fake ACP agent
(`/tmp/cdy/bin/cursor-agent`, a Python JSON-RPC responder advertising three models via a
Model-category `session/new` `configOptions` selector) and ran the real CLI:

```
$ codypendent acp connect cursor --repo /tmp/cdy/repo
connected Cursor 2026.07.23 as model `acp/cursor` (agent default)
discovered 3 agent model(s):
  * acp/cursor#fake-sonnet                   Fake Sonnet (fake-sonnet)
    acp/cursor#fake-opus                     Fake Opus (fake-opus)
    acp/cursor#fake-mini                     Fake Mini (fake-mini)
session modes: Build, Review
```

Real wire (host → agent, from the agent-side log):

```
{"jsonrpc":"2.0","id":"24a9df60-…","method":"initialize","params":{"protocolVersion":1,
 "clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false}}}
{"jsonrpc":"2.0","id":"99fbab55-…","method":"session/new","params":{"cwd":"/tmp/cdy/repo","mcpServers":[]}}
```

The three discovered models were persisted as `acp/cursor#<model>` profiles in
`models.toml`, `codypendent acp status` and `codypendent doctor` both listed them ready,
`codypendent acp probe cursor` completed a live prompt turn
(`Cursor 2026.07.23 live prompt: EndTurn / ACP LIVE OK (model=fake-sonnet)`), and a full
headless `codypendent run` executed through the ACP path to `RunStateChanged{Completed}`
with the agent's text streaming back as `ModelStreamDelta`. `codypendent acp list` fetched
the **live** official registry (`https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json`)
and listed 40+ real agents. This half of outcome 2 is done.

---

## Findings

### F1 — `codypendent acp serve` fails every `session/prompt`. Class (c).

`crates/cli/src/acp.rs:125-140` sends `CommandBody::AttachSession` and passes the reply to
`require_accepted(...)`, which only accepts `Payload::CommandAccepted`. The daemon's
`handle_attach` (`crates/daemon/src/server.rs:4368-4372`) **always** replies
`Payload::Catchup` — there is no code path that returns `CommandAccepted` for an attach.
Every other caller in the tree uses `commands::expect_catchup`
(`crates/cli/src/commands.rs:548-558`); the ACP bridge is the only one that does not.

Observed against a running daemon with a Python ACP client driving stdio:

```
CLIENT->AGENT: {"jsonrpc":"2.0","id":3,"method":"session/prompt",
                "params":{"sessionId":"019ff87b-c718-…","prompt":[{"type":"text","text":"say hello"}]}}
AGENT->CLIENT: {"jsonrpc":"2.0","id":3,"error":{"code":-32000,
  "message":"prompt failed: acp backend error: unexpected reply to AttachSession:
   Catchup { catchup: Events { from: 1, through: 1, events: [ … SessionCreated … ] } }"}}
```

`initialize` and `session/new` succeed (I got a real `sessionId` back); only the prompt is
dead. **User-visible:** a user wires Codypendent into Zed as an ACP agent, the connection
comes up, a session opens, they type anything — and get a JSON-RPC error with a Rust
`Debug` dump of an internal enum. No run ever starts. This is the headline user journey of
outcome 2 and it is 100 % reproducible.

Untested by construction: `DaemonAcpBackend` is constructed only in `crates/cli/src/main.rs:1190,1194`
and appears in no test anywhere in the workspace. The 11 unit tests in
`crates/integrations/src/acp.rs:725-891` all drive `FakeBackend` over an in-memory duplex,
so the transport is well covered and the one production wire is not covered at all.

### F2 — `session/new` ignores the client-supplied `cwd`. Class (c).

`crates/cli/src/acp.rs:92-110` builds `CreateSession { repository: Some(self.repo…) }` from
the process's `--repo` flag (defaulting to the process cwd, `crates/cli/src/main.rs:1188-1193`)
and never reads `params.cwd`. The ACP spec makes `cwd` the per-session working directory and
requires it to be an absolute path; a client that opens two projects gets two sessions
pointed at the same repository. Proven observationally — a `session/new` naming a directory
that does not exist is accepted:

```
CLIENT->AGENT: {"jsonrpc":"2.0","id":2,"method":"session/new",
                "params":{"cwd":"/tmp/cdy/definitely-not-a-directory","mcpServers":[]}}
AGENT->CLIENT: {"jsonrpc":"2.0","id":2,"result":{"sessionId":"019ff881-8aad-…"}}
```

(The ACP *client* direction does this correctly — `acp_client.rs:678` canonicalizes and
`is_dir()`-checks the cwd before the handshake. Only the server direction is wrong.)

### F3 — Anthropic is simultaneously unreachable (TUI) and mis-wired (CLI). Class (b) + (c).

The catalog ships 10 curated Anthropic models (`builtin_catalog.toml`), all with **correct
pricing** — `claude-opus-5` $5/$25, `claude-sonnet-5` $3/$15, `claude-haiku-4-5` $1/$5 at
200 000 ctx, `claude-fable-5` $10/$50 — verified against the current Anthropic model table.

*TUI path, class (b) — data produced, never consumed.* `provider_runtime_supported`
(`crates/cli/src/tui.rs:6547-6549`) delegates to `provider_can_list_models`
(`:6534-6542`), which requires `Protocol::OpenAiChat`. Anthropic's catalog protocol is
`anthropic`, so selecting it in `/provider` short-circuits at
`crates/tui/src/reduce.rs:5483-5491` with
`"anthropic is catalog-only — its anthropic runtime adapter is not installed"`. The 10
curated rows can never be reached. `azure-openai` is blocked the same way (no `base_url`).
The notice is at least honest, but "prefilled model lists for non-ACP providers" does not
include the single most likely provider a user would pick.

*CLI path, class (c) — wire attached, wrong behaviour.* `codypendent models add` has no
such gate:

```
$ codypendent models add anthropic claude-opus-5
added model anthropic/claude-opus-5 (/tmp/cdy/data/models.toml)
```

and writes

```toml
[[model]]
id = "anthropic/claude-opus-5"
provider = "openai-compatible"      # <- Anthropic is NOT OpenAI-compatible
base_url = "https://api.anthropic.com"
provider_id = "anthropic"
```

`config_to_protocol_auth` (`crates/runtime/src/models.rs:689-717`) hard-codes
`Protocol::OpenAiChat` for any `provider == "openai-compatible"` and never consults the
catalog's declared protocol, so this entry will `POST https://api.anthropic.com/chat/completions`
with an OpenAI body. `codypendent models check` on it fails at the `/models` probe (there
is no `https://api.anthropic.com/models`). Two entry points to the same catalog disagree
about whether a provider is usable.

### F4 — Trust boundary: provider-supplied `context_length` beats the curated catalog and reaches `num_ctx`. Class (c).

`merge_catalog_rows` (`crates/cli/src/tui.rs:4975-4995`) merges a provider's live `/models`
response with the curated rows and only fills gaps — the comment is explicit: *"catalog
metadata fills any gap a live row left (never overwriting what the provider itself said)"*.
The live value is parsed by `parse_models_response` (`:4933-4963`) with the only validation
being `context_tokens.filter(|tokens| *tokens > 0)` and `price >= 0.0 && finite`. That value
is what the picker displays, and `write_add_model` (`crates/cli/src/tui.rs:4226-4253`)
persists exactly what the picker displayed into `models.toml`'s `context_tokens`.

From there it is load-bearing, not display-only: `FrameworkModelDriver::from_registry`
(`crates/runtime/src/agent.rs:5492`) reads it and `apply_context_window`
(`crates/runtime/src/agent.rs:6255-6272`) forwards it verbatim as the Ollama
`{"options":{"num_ctx": n}}` request hint, and it is the denominator of the TUI footer's
context-usage percentage (`crates/runtime/src/agent.rs:817-821`). A misconfigured or hostile
OpenAI-compatible gateway that reports `"context_length": 18446744073709551615` gets that
number sent back as `num_ctx` and shown as the user's context budget. There is no clamp,
no cross-check against the curated catalog, and no ceiling.

(Cost fields *are* honest — `crates/providers/src/model.rs:118` documents them as
display-only and I traced every consumer: `crates/tui/src/render.rs:7542` and
`crates/tui/src/accessible.rs:525` only. Nothing sums them into a budget.)

### F5 — Catalog context windows are systematically wrong; some prices and one id are wrong. Class (c).

The model ids themselves are largely **real** — I diffed the 25 curated `openrouter` rows
against the live OpenRouter registry (`https://openrouter.ai/api/v1/models`, 410 models):
24 of 25 are present. But the metadata attached to them is not:

* **`1048576` (2^20) is used throughout to mean "1M".** It appears on ~200 rows across
  openai, anthropic, gemini, xai, deepseek, openrouter, nebius and more. Live values are
  `1000000` (Anthropic, Gemini, Grok 4.3, Qwen) or `1050000` (OpenAI gpt-5.6/5.4). The
  catalog overstates every one by 4.8–5 %. Not cosmetic — per F4 this is the `num_ctx`
  hint and the context-usage denominator. Note `claude-haiku-4-5` is `200000` exactly and
  correct, so the error is specifically the "1M" rows.
* **Prices materially wrong on at least two rows.** `deepseek/deepseek-v4-pro` catalog
  $0.63/$1.26 vs live $1.168/$2.336 (−46 %); `z-ai/glm-5.2` catalog out $1.54 vs live
  $3.15 (−51 %); `qwen/qwen3.6-plus` in $0.33 vs $0.325.
* **One id does not exist on its provider.** `anthropic/claude-haiku-4-5` is listed under
  `openrouter` and is absent from the live OpenRouter catalog. Selecting it from the
  catalog-only fallback path yields a model that 404s at call time.
* Smaller context mismatches: `x-ai/grok-build-0.1` 262144 vs 256000; `z-ai/glm-5.1`
  202752 vs 204800.

10 of 42 providers (`ai21`, `github-models`, `lambda`, `lmstudio`, `vllm`, and the 5 ACP
entries) ship **zero** curated models, so for those the "prefilled list" is empty and the
add flow depends entirely on a live `/models` call.

### F6 — The headless `run` path cannot select a model at all. Class (a) for that path.

`codypendent run --help` exposes `--objective`, `--mode`, `--repo`, `--jsonl` and nothing
else; `crates/cli/src/commands.rs:481-482` hard-codes `model: None`. With routing off (the
default) the daemon falls through to `resolve_run_model`
(`crates/codypendentd/src/executor.rs:914-964`) over `ModelPolicy::with_default_candidates(ids)`
built from **models.toml file order** (`crates/codypendentd/src/executor.rs:2569-2571`) and
takes the first candidate that connects. With four models configured I watched it silently
pick `openrouter/anthropic/claude-opus-5` — the fourth entry — because the first three failed
a `/models` probe:

```
INFO codypendentd::executor: resolved model; executing run model=openrouter/anthropic/claude-opus-5
```

There is no way to say "use this one" outside the TUI's `/model` picker. Also observed: an
ACP profile pinned to a specific agent model can only be exercised through that picker or
by editing `models.toml` by hand.

### F7 — Selection diagnostics never reach the client. Class (c), observed 3×.

Three separate failed runs printed only `RunStateChanged{Failed}` on the `--jsonl` stream
and then the stream ended. Every actual reason lived exclusively in `daemon.log`:

```
WARN codypendentd::executor: run did not execute; failing it cleanly reason=no model configured:
 every candidate failed for Build: acp/cursor: ACP package runner `cursor-agent` is unavailable; …
```

A user running headlessly sees a run fail with no reason at all.

### F8 — `models add --help` advertises a command that does not exist. Class (c), minor.

The `<PROVIDER>` help text says *"(`codypendent models list-providers` spellings …)"*.

```
$ codypendent models list-providers
error: unrecognized subcommand 'list-providers'
```

The `models` subcommand set is `list|add|check|bench|pull`. There is no way to enumerate
the 42 catalog providers from the CLI at all.

### F9 — `models list` reports `key: none` for entries that do resolve a key. Cosmetic.

`models add` without `--key-env` writes `api_key_env = ""` and relies on the catalog's
documented provider env NAMEs at call time (`crates/runtime/src/models.rs:755-767`).
`models list` renders that as `key: none`, which reads as "this model needs no key".

### F10 — ACP profiles can never participate in measured routing. Class (b), design gap.

`ModelProfileStore` (`crates/daemon/src/model_profiles.rs`) is keyed by `(model_id, endpoint)`
and populated by `codypendent models bench`, which builds a chat client. For an ACP profile
`client_for` returns `ProtocolNotWired` (`crates/runtime/src/models.rs:984-987`), observed live:

```
WARN codypendentd::executor: could not build memory extraction client; extraction disabled for this run
 error=could not build client for acp/cursor: model `acp/cursor` uses protocol
 `acp (full-agent executor, not ChatClient)` which is not yet wired
```

So ACP models are unbenchable, get no `ModelProfile`, and are invisible to the Phase-7
router's eligible pool. Relevant to outcome 11 (live measured routing on 3).

### F11 — `codypendent acp serve` exposes no models to its client. Class (a), narrow.

The initialize response is `{"protocolVersion":1,"agentCapabilities":{"promptCapabilities":{"image":false}}}`
and `session/new` returns only `sessionId`. No `configOptions`, no `modes`. Everything else
returns `-32601`:

```
CLIENT->AGENT: {"jsonrpc":"2.0","id":99,"method":"session/set_config_option", …}
AGENT->CLIENT: {"jsonrpc":"2.0","id":99,"error":{"code":-32601,"message":"method not found"}}
CLIENT->AGENT: {"jsonrpc":"2.0","id":100,"method":"session/list","params":{}}
AGENT->CLIENT: {"jsonrpc":"2.0","id":100,"error":{"code":-32601,"message":"method not found"}}
```

Also `StartRun { model: None }` at `crates/cli/src/acp.rs:147-148` — an ACP client cannot
pin which Codypendent model serves its run. Discovery is one-directional: Codypendent reads
an agent's models, but publishes none of its own. Defensible as scope, worth stating.

---

## `models_pull.rs` — run, no daemon, no ollama

`codypendent models pull Qwen3-0.6B-GGUF` against the **live** Hugging Face Hub:

```
codypendent models pull: resolved unsloth/Qwen3-0.6B-GGUF : Q4_K_M
Error: `ollama` was not found on PATH — install it from https://ollama.com, then retry
```

Correct and honest. It resolved a real repo, auto-picked the Q4_K_M default
(`pick_default_quant`), spawned `ollama`, and mapped `ErrorKind::NotFound` to the actionable
`PullError::BinaryNotFound` (`crates/cli/src/models_pull.rs:125-127`) without registering a
half-configured model. `register_pulled_model` (`:254-310`) does a proper load-modify-write
that preserves `[voice]`/`[transcription]`/`[embedding]`/`[retrieval]`. No ollama daemon was
available in this container, so the success path is unexercised — but the failure path is
the one that matters and it is clean. No findings.

---

## External research

**`agent-client-protocol` — pinned `"2"`, resolves to 2.0.0 (Cargo.lock), which is the
newest on crates.io** (published 2026-07-23; the series runs 0.13.1 → 1.3.0 → 2.0.0). Pulls
`agent-client-protocol-schema` 1.5.0. The pin is current, not stale.

**`agent-framework-{core,openai,anthropic}` all exist at the pinned 0.2.0.**
`agent-framework-core` 0.2.0 was published 2026-08-08 — five days before this review — and
is the newest version (only 0.1.1 precedes it). Nothing fabricated, nothing stale.

**Protocol version.** `ProtocolVersion::V1` is `LATEST`; `V2` exists only behind the
`unstable_protocol_v2` feature and "must be selected explicitly". The repo speaks V1 in both
directions (`acp_client.rs:975` sends `ProtocolVersion::V1`; `acp.rs:54` clamps the served
version to 1 with a good comment about not claiming a version it cannot speak). Correct.

**Method names match the real spec.** The v1 schema's wire methods are `session/cancel`,
`session/close`, `session/delete`, `session/fork`, `session/list`, `session/load`,
`session/new`, `session/prompt`, `session/request_permission`, `session/resume`,
`session/set_config_option`, `session/set_mode`, `session/update`, plus `initialize`,
`authenticate`, `logout`. Everything the repo sends or answers is a real method spelled
correctly. `agentCapabilities: {promptCapabilities: {image: false}}` is spec-valid — every
`AgentCapabilities` field is `#[serde(default)]`.

**Model discovery is the spec-sanctioned mechanism, implemented correctly.** ACP carries
model selection as a `SessionConfigOption` with `category: "model"` and a `select` kind, set
via `session/set_config_option`. `AcpDiscovery::apply_config_options`
(`crates/integrations/src/acp_client.rs:484-522`) reads exactly that, handles grouped and
ungrouped option lists, prefers `session/new`'s dedicated `modes` state over a Mode-category
option, and — correctly — rebuilds from the whole set on every arrival so a dropped selector
leaves no stale model list. `set_model` (`:812-842`) targets the discovered
`SessionConfigId`. This matches the published `session-config-options` spec page. Note the
spec says the `category` field is UX metadata that "must not be required for correctness";
the repo keys discovery on it, so an agent that ships a model selector without a category
would be invisible. Minor, and every real agent sets it.

**Concrete divergences from the spec, all in the server direction:** ignores `session/new`'s
`cwd` (F2); implements 4 of ~16 v1 methods (`initialize`, `session/new`, `session/prompt`,
`session/cancel`) with no `session/load`, `session/set_mode`, `session/set_config_option`,
or `authenticate`; and advertises no session config options (F11). The client direction has
no divergences I could find.

---

## What I could not exercise, and why

* **A real vendor ACP agent** (claude-acp, gemini, codex). Every one requires vendor auth
  and `npx`/network install into this container; the registry itself fetched fine, but I
  substituted a scripted agent so I could control the `configOptions` payload and read the
  wire. The scripted agent speaks the same newline-delimited JSON-RPC 2.0 the SDK does, and
  the client's own handshake bytes are quoted above, so the discovery path is genuinely
  exercised — but "does *Claude Code's* real agent advertise a Model-category selector"
  is unanswered.
* **A live model call.** No API keys. `models check` against `api.openai.com` returned a
  real HTTP 401 (so the transport, headers and error mapping are live-verified), but no
  completion was ever produced, so `HeaderAuthChatClient`
  (`crates/runtime/src/models.rs:1004-1135`, the Azure/GitHub-Models non-bearer path) is
  untested end to end.
* **`models pull`'s success path and `models bench`.** No `ollama` binary and no GPU.
* **The TUI `/provider` and add-model pick list interactively.** No pty; I drove the same
  reducer/selection logic directly (`crates/tui/src/reduce.rs:5455-5500`) and the harness
  functions that feed it, and confirmed the CLI twin of the same catalog path.
* **`codypendent update`.** Requires `gh` and a release in a private repo; `decide_update`
  and `detect_target` are pure and read correctly, the effectful driver was not run.
* **`finetune`.** Reviewed only; it is explicit that it writes text files and shells out
  read-only, and this container has no Python/CUDA. No findings, no claims.
* **Provider-side accuracy for openai / anthropic / gemini / ollama catalogs.** I could
  diff `openrouter` against its public live registry and Anthropic against the vendor model
  table; the others have no unauthenticated catalog endpoint, so their ids are unverified
  beyond spot-checking. The `1048576` defect is systemic and applies to them regardless.
