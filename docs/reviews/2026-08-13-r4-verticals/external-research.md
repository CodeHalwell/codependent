# Vertical: external-research — 2026-08-13 (round 4)

Reviewer scope: everything this product claims to integrate with that lives
**outside** this repo — dependency versions and their real APIs, the ACP wire
spec, the MCP spec, the provider catalog against providers' live APIs, and every
external service named in code or docs.

Pinned commit `c255bec8b175d62942b3312cff2335b97d43a59a` (v0.5.1). Every
external fact below was fetched live on 2026-08-13; every command and its
output is quoted verbatim. Where I could not verify something, §7 says so.

---

## Verdicts

**OUTCOME 2 (ACP, incl. automatic model discovery): PARTIAL** — the pinned
`agent-client-protocol = "2"` → 2.0.0 is the newest crates.io release, the
repo speaks protocol **V1** which *is* `LATEST` in that crate, every method
name it sends or answers is real, the live agent registry fetches (HTTP 200,
38 agents), and the prior round's headline bug (`acp serve` failing every
`session/prompt`) **is genuinely repaired**. One spec divergence survives:
model discovery is keyed on a field the ACP spec says must not be load-bearing.

**OUTCOME 3 (easy model selection; prefilled model lists): PARTIAL** — the
42-provider / 386-model catalog is far more accurate than the previous round
reported (that round's headline claim was itself wrong — see §3.0), and the
Anthropic wiring is repaired. But it still ships **two retired providers and
one unreachable one**, and at least one context window that is **5× the real
value** and reaches `models.toml` on a path I drove myself.

**MCP subcommand: BROKEN against the current spec.** The client offers protocol
revision `2025-06-18`. The current revision is **`2026-07-28`**, which *removes*
the `initialize` / `notifications/initialized` handshake this client is built
on. Per the spec's own compatibility matrix, legacy-client + modern-server
**fails**, and "legacy clients have no fall-forward mechanism."

**Dependency hygiene: PARTIAL.** `cargo deny check` passes (I ran it; exit 0),
but it passes because cargo-deny's default `unsound` scope hides five
unsoundness advisories on this tree — two of which have fixes available today.

---

## 1. Dependencies with a protocol or API contract

All version data from `https://crates.io/api/v1/crates/<name>`, fetched
2026-08-13.

| Crate | Cargo.toml | Cargo.lock | Newest on crates.io | Verdict |
|---|---|---|---|---|
| `agent-client-protocol` | `"2"` | 2.0.0 | **2.0.0** (2026-07-23) | current |
| `agent-client-protocol-schema` | (transitive) | 1.5.0 | 1.6.0 (2026-07-21) | one behind |
| `agent-framework-core` | `0.2.0` | 0.2.0 | **0.2.0** (2026-08-08) | current |
| `agent-framework-openai` | `0.2.0` | 0.2.0 | **0.2.0** (2026-08-08) | current |
| `agent-framework-anthropic` | `0.2.0` | 0.2.0 | **0.2.0** (2026-08-08) | current |
| `wasmi` | `0.51` | 0.51.5 | 1.1.0 stable / 2.0.0-beta.10 | **two majors behind** |
| `sqlx` | `0.8` | 0.8.6 | 0.9.0 (2026-05-21) | one minor behind |
| `tantivy` | `0.22` | 0.22.1 | **0.26.1** (2026-04-21) | **4 minors behind** |
| `tree-sitter` | `0.24` | 0.24.7 | 0.26.12 (2026-08-08) | 2 minors behind |
| `ratatui` | `0.29` | 0.29.0 | **0.30.2** (2026-06-19) | one minor behind |
| `loro` | `1.13` | 1.13.9 | 1.13.9 | current |
| `pulldown-cmark` | `0.13` | 0.13.4 | 0.13.4 | current |
| `synoptic` | `2.2` | 2.2.9 | 2.2.9 (published 2024-11-30) | current-but-dormant |

**Every pinned crate exists at the pinned version.** Nothing is fabricated.
`agent-framework-core/openai/anthropic` 0.2.0 were published 2026-08-08 — five
days before this commit — and 0.2.0 is the newest; the pin is fresh, not stale.

### 1.1 A WASM runtime is present: `wasmi`, not `wasmtime`

`crates/sandbox/Cargo.toml:37` — `wasmi = { version = "0.51", default-features
= false, features = ["std"] }`, resolving to 0.51.5. The comment at `:28`
states the choice deliberately ("`wasmi` rather than `wasmtime`: a skill …").
`wasmtime`/`wasmer` are absent from `Cargo.lock` entirely.

`wasmi`'s current stable is **1.1.0**, with a 2.0.0 beta line (2.0.0-beta.10,
2026-08-10). 0.51 is two major versions behind. No advisory affects it. This is
a currency finding, not a correctness one.

### 1.2 `cargo deny check` — I ran it, and it passes for the wrong reason

cargo-deny is not installed in this container; I fetched the upstream 0.20.2
musl binary and ran it against the repo unmodified:

```
$ cargo-deny --version
cargo-deny 0.20.2

$ cargo-deny check
…
advisories ok, bans ok, licenses ok, sources ok
$ echo $?
0
```

`check advisories` alone reports:

```
$ cargo-deny --log-level debug check advisories
2026-08-13 22:48:42 [INFO] using config from /home/user/codypendent/deny.toml
2026-08-13 22:48:42 [INFO] No advisory database configured, falling back to default 'https://github.com/RustSec/advisory-db'
2026-08-13 22:48:42 [INFO] advisory database https://github.com/RustSec/advisory-db fetched in 954.856169ms
…
 advisories ok: 0 errors, 0 warnings, 12 notes
```

The 12 notes are 6 unmaintained advisories, each paired with its
`deny.toml` ignore — exactly what `deny.toml:21-28` documents.

**But five *unsound* advisories on this tree are never mentioned at all.**
`deny.toml`'s `[advisories]` block sets `version = 2` and an `ignore` list but
does **not** set `unsound`; cargo-deny 0.20's default for that field is
`"workspace"` (direct workspace dependencies only), and all five affected
crates are transitive. Proof — the same binary, same repo, one line added
(`unsound = "all"` inside `[advisories]`, config written to
`/tmp/review-external-research/deny-unsound.toml`):

```
$ cargo-deny --manifest-path /home/user/codypendent/Cargo.toml \
    --config /tmp/review-external-research/deny-unsound.toml check advisories
error[unsound]: `event-listener` allows `!Send` tags to cross thread boundaries via `StackSlot`
error[unsound]: Aliasing violation in `OrdSet` insertion
error[unsound]: `IterMut` violates Stacked Borrows by invalidating internal pointer
error[unsound]: Potential use-after-free due to lack of panic safety in `LruCache::pop()`
error[unsound]: Panic-safety unsoundness in `Chunk`, `RingBuffer`, and `InlineArray` (use-after-free / double-free)
advisories FAILED
```

| Advisory | Crate (locked) | Dated | Fix available? | Reaches |
|---|---|---|---|---|
| RUSTSEC-2026-0221 | `event-listener` 5.4.1 | 2026-07-13 | **yes — `>= 5.4.2`** | `sqlx-core`, `agent-client-protocol` |
| RUSTSEC-2026-0002 | `lru` 0.12.5 | 2026-01-07 | **yes — `>= 0.16.3`** | `ratatui` 0.29, `tantivy` 0.22 |
| RUSTSEC-2026-0253 | `lru` 0.12.5 | 2026-05-12 | **yes — `>= 0.18.2`** | same |
| RUSTSEC-2023-0126 | `im` 15.1.0 | 2023-02-04 | no (crate archived) | `loro` |
| RUSTSEC-2026-0255 | `sized-chunks` 0.6.5 | **2026-08-11** | no (crate archived) | `loro` → `im` |

RUSTSEC-2026-0255 is dated **two days before this commit** and is a
use-after-free / double-free reachable from safe Rust. RUSTSEC-2026-0253 is
also a use-after-free/double-free, and it *is* fixable — `lru` 0.12.5 is pulled
in by `ratatui` 0.29 and `tantivy` 0.22, both of which are the stale pins in
the table above. Upgrading `ratatui` → 0.30.2 and `tantivy` → 0.26.1 is
plausibly the whole fix; `event-listener` 5.4.1 → 5.4.2 is a lockfile bump.

`deny.toml:2-3` says *"the supply-chain gate for the 'every-release hygiene'
checklist (`cargo deny check` clean, or with dated exceptions)"* and
`.github/workflows/ci.yml:169` runs `EmbarkStudios/cargo-deny-action@v2` with
`command: check`. **The gate is green and the doc claim is technically true —
but the gate's scope is narrower than the file's own framing implies, and
nothing in `deny.toml` records that unsound-on-transitive-deps is out of
scope.** Class (c): wire attached, silently narrower behaviour.

### 1.3 Yanked versions

None of the locked versions are yanked. Two near-misses worth noting because
they show the pins were chosen, not defaulted: `sqlx` 0.8.4 is yanked (the lock
has 0.8.6), and `tantivy` **0.22.0 is yanked** — the lock has 0.22.1, published
2025-07-17, *after* 0.24.x, i.e. a backport. So the `tantivy = "0.22"` pin
resolves to a non-yanked patch. Verified:

```
$ curl -sS https://crates.io/api/v1/crates/tantivy/0.22.0 | …
yanked: True | created: 2024-04-12T04:49:17.060022Z
$ curl -sS https://crates.io/api/v1/crates/tantivy/0.22.1 | …
yanked: False | created: 2025-07-17T03:23:11.159327Z
```

### 1.4 API usage vs the real crates — correct where I could check it

`crates/runtime/src/models.rs:1140`:

```rust
agent_framework_anthropic::AnthropicClient::new(api_key, cfg.model.clone())
    .with_base_url(cfg.base_url.clone())
```

The real crate (`agent-framework-anthropic-0.2.0/src/lib.rs`) has
`pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self` at
`:210` and `pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self`
at `:237`. It sends `.header("anthropic-version", ANTHROPIC_VERSION)` at `:163`
with `ANTHROPIC_VERSION = "2023-06-01"` at `:72` — matching the catalog's
`extra_headers = { "anthropic-version" = "2023-06-01" }`. **Correct.**

`rsa` 0.9.10 (RUSTSEC-2023-0071, Marvin Attack) appears in `Cargo.lock` via
`sqlx-mysql`, but sqlx is `default-features = false` with only `sqlite`, so it
is not compiled. cargo-deny agrees — `[DEBUG] filtered rsa 0.9.10`, and
`cargo tree --offline -i rsa -e normal` prints *"nothing to print."* **Not a
finding**; recorded so a future `cargo audit` run isn't mistaken for a
regression.

---

## 2. The ACP protocol against the real spec

### 2.1 Version and method names — correct

`agent-client-protocol-schema-1.5.0/src/version.rs:41-47`:

```rust
    #[cfg(not(feature = "unstable_protocol_v2"))]
    pub const LATEST: Self = Self::V1;
```

with V2 documented as *"an unstable draft … only available when the
`unstable_protocol_v2` feature is enabled and must be selected explicitly."*
The repo speaks V1 in both directions. **The V1 pin is current, not stale.**

The v1 schema's method constants (`v1/agent.rs:4878-4899`) are
`INITIALIZE_METHOD_NAME = "initialize"`, `SESSION_NEW_METHOD_NAME =
"session/new"`, `SESSION_PROMPT_METHOD_NAME = "session/prompt"`, and the
schema states at `:4160` that *"all Agents **MUST** support `session/new`,
`session/prompt`, `session/cancel`, and `session/update`."* The repo's server
direction implements exactly those four. **Spec-conformant at the baseline.**

### 2.2 The live agent registry works

```
$ curl -sS "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json"
HTTP:200 bytes:48810
agents: 38
  {'id': 'agoragentic-acp', 'name': 'Agoragentic', 'version': '1.3.0', …}
  {'id': 'claude-acp', 'name': 'Claude Agent', 'version': '0.66.0', …}
  {'id': 'codex-acp', 'name': 'Codex', 'version': '1.2.0', …}
```

The URL `crates/integrations/src/acp_registry.rs` fetches is live and returns
38 real agents.

### 2.3 The round-3 headline bug is genuinely repaired

Prior finding F1 (`acp-models.md`) was that `crates/cli/src/acp.rs` passed the
`AttachSession` reply to `require_accepted`, which only accepts
`Payload::CommandAccepted`, while the daemon always answers `Payload::Catchup`
— failing every `session/prompt`. At this commit `crates/cli/src/acp.rs:78-89`
is a new `require_attached` that accepts `Payload::Catchup`, with a comment
naming the old bug, and `:149` calls it. **Repaired** (read, not re-driven —
see §7).

### 2.4 Divergence that survives: model discovery keys on UX-only metadata

Fetched `https://agentclientprotocol.com/protocol/session-config-options`:

> The `category` field is **UX metadata only**. Per the specification:
> *"Categories are for UX purposes only and MUST NOT be required for
> correctness."* Missing or unknown categories must be handled gracefully by
> clients.

`crates/integrations/src/acp_client.rs:488-505` does the opposite:

```rust
            let SessionConfigKind::Select(select) = &option.kind else {
                continue;
            };
            …
            match option.category {
                Some(SessionConfigOptionCategory::Model) => { … }
                Some(SessionConfigOptionCategory::Mode) => { … }
                _ => {}
            }
```

An agent that advertises a `select` option holding its model list without
setting `category: "model"` is invisible: `self.models` is assigned the empty
vector at `:517`. **User-visible consequence:** `codypendent acp connect <agent>`
reports zero discovered models for a spec-compliant agent, and the user is
told nothing was found rather than "the agent advertised models I could not
classify." This is outcome 2's headline capability. Class (c).

Mitigating: every agent I can see in the registry is likely to set the
category, and the rest of the discovery logic is correct — it rebuilds from
the whole option set on every arrival (`:513-516`), so a dropped selector
leaves no stale list, and `set_model` targets the discovered `SessionConfigId`.

---

## 3. The provider catalog against providers' real APIs

42 providers, 386 `[[model]]` rows (counted:
`grep -c '^\[\[provider\]\]'` → 42, `grep -c '^\[\[model\]\]'` → 386).

### 3.0 First: the previous round's headline catalog finding was wrong

`docs/reviews/2026-08-13-verticals/acp-models.md` F5 claims:

> **`1048576` (2^20) is used throughout to mean "1M".** … Live values are
> `1000000` (Anthropic, Gemini, Grok 4.3, Qwen) … The catalog overstates every
> one by 4.8–5 %.

**For Gemini, DeepSeek, Kimi, GLM and MiniMax that is false.** Live OpenRouter
(`https://openrouter.ai/api/v1/models`, HTTP 200, 411 models, fetched today)
reports `context_length: 1048576` for exactly those families:

```
google/gemini-3.6-flash                      ctx= 1048576
google/gemini-3.5-flash                      ctx= 1048576
google/gemini-3.1-pro-preview                ctx= 1048576
deepseek/deepseek-v4-pro                     ctx= 1048576
deepseek/deepseek-v4-flash                   ctx= 1048576
moonshotai/kimi-k3                           ctx= 1048576
z-ai/glm-5.2                                 ctx= 1048576
minimax/minimax-m3                           ctx= 1048576
```

A full diff of all 24 curated `openrouter` rows against the live registry:
**24/24 ids present, 24/24 context windows exact.** The catalog's `1048576` is
right for these providers; only three prices differ (below). 52 rows still
carry `1048576`; that is not by itself an error.

I flag this because the round-3 fix pattern is visible in the file: `anthropic`
rows were changed to `1000000`, `openai` to `1050000`, `xai`/`qwen` to
`1000000` — while gemini/deepseek/moonshot/zhipu/minimax and every reseller
kept `1048576`. That is the "fix applied to the instance, not the class"
pattern from the synthesis — except here the *unfixed* instances were the
correct ones and some of the *fixed* ones may not have needed fixing.

### 3.1 Anthropic — verified against Anthropic's own model table

Fetched `https://platform.claude.com/docs/en/about-claude/models/overview.md`.

| Catalog id | Catalog ctx | Catalog $in/$out | Real ctx | Real $in/$out | |
|---|---|---|---|---|---|
| `claude-fable-5` | 1000000 | 10 / 50 | 1M | 10 / 50 | ✅ |
| `claude-opus-5` | 1000000 | 5 / 25 | 1M | 5 / 25 | ✅ |
| `claude-opus-4-8` | 1000000 | 5 / 25 | 1M | 5 / 25 | ✅ |
| `claude-opus-4-7` | 1000000 | 5 / 25 | 1M | 5 / 25 | ✅ |
| `claude-opus-4-6` | 1000000 | 5 / 25 | 1M | 5 / 25 | ✅ |
| `claude-sonnet-5` | 1000000 | 2 / 10 | 1M | 2 / 10 | ✅ |
| `claude-sonnet-4-6` | 1000000 | 3 / 15 | 1M | 3 / 15 | ✅ |
| `claude-haiku-4-5` | 200000 | 1 / 5 | 200k | 1 / 5 | ✅ |
| `claude-sonnet-4-5` | 200000 | 3 / 15 | 200k | 3 / 15 | ✅ |
| **`claude-opus-4-5`** | **1000000** | 5 / 25 | **200k** | 5 / 25 | ❌ **5×** |

Anthropic's legacy-models table gives Claude Opus 4.5 a context window of
**200k tokens** and a 64k max output. Independently confirmed by OpenRouter:

```
anthropic/claude-opus-4.5                    ctx=  200000  in=  5.0000 out= 25.0000
```

**This is not theoretical — I drove it:**

```
$ export CODYPENDENT_DATA_DIR=/tmp/review-external-research/data
$ codypendent models add anthropic claude-opus-4-5
added model anthropic/claude-opus-4-5 (/tmp/review-external-research/data/models.toml)
verify it with: codypendent models check anthropic/claude-opus-4-5

$ codypendent models list
anthropic/claude-opus-4-5
    anthropic · https://api.anthropic.com · context: 1000000 · key: none

$ cat data/models.toml
[[model]]
api_key_env = ""
base_url = "https://api.anthropic.com"
context_tokens = 1000000
id = "anthropic/claude-opus-4-5"
model = "claude-opus-4-5"
provider = "openai-compatible"
provider_id = "anthropic"
```

**User-visible consequence:** the TUI's context-usage footer divides by
1,000,000 for a model whose real window is 200,000, so a session sitting at
190K tokens — one turn from a hard API error — displays as **19 %** used. The
same value is `FrameworkModelDriver`'s `num_ctx` hint. The round-3 repair
`clamp_context_tokens` (`crates/runtime/src/models.rs:127,153`) caps at
`MAX_PLAUSIBLE_CONTEXT_TOKENS = 2_000_000`, so it catches a hostile `u64::MAX`
and does nothing at all for a curated row that is wrong by 5×. Class (c).

Also absent from the catalog: `claude-opus-4-1` — correctly so, it retired
2026-08-05, eight days before this commit. `claude-mythos-5` is absent, also
correct (Project Glasswing, invitation-only). Max-output tokens (128k / 64k)
are not representable at all — `crates/providers/src/model.rs:120-130` shows
`Model` has `context_tokens`, `cost_per_1m_input_usd`, `cost_per_1m_output_usd`
and no max-output field.

### 3.2 OpenAI — right numbers, wrong shape

Fetched `https://developers.openai.com/api/docs/pricing`. All nine curated
`openai` rows match the page's **short-context** tier exactly (sol 5/30,
terra 2/12, luna 0.20/1.20, 5.5 5/30, 5.5-pro 30/180, 5.4 2.5/15, mini
0.75/4.50, nano 0.20/1.25, 5.3-codex 1.75/14) and all context windows match
OpenRouter's live values (1050000 for the 5.6/5.5/5.4 family, 400000 for
mini/nano/codex).

**The real page prices three of them in two tiers:**

| Model | Short | Long |
|---|---|---|
| `gpt-5.6-sol` | $5 / $30 | **$10 / $45** |
| `gpt-5.6-terra` | $2 / $12 | **$4 / $18** |
| `gpt-5.6-luna` | $0.20 / $1.20 | **$0.40 / $1.80** |
| `gpt-5.5` | $5 / $30 | **$10 / $45** |

The `Model` struct has one price field per direction, so the long-context rate
is unrepresentable. A user pointing a 600K-token prompt at `gpt-5.6-sol` is
shown a cost basis **half** the real one. Display-only (§3.6), but the
catalog's own header comment says *"OpenAI (official pricing page, 2026-08)"*
— it reproduces half of that page. Class (c), low severity.

### 3.3 Gemini — one price is the 2027 price, one is the low tier

Fetched `https://ai.google.dev/gemini-api/docs/pricing`.

| Catalog id | Catalog $in/$out | Real | |
|---|---|---|---|
| `gemini-3.6-flash` | **1.5 / 7.5** | **$0.75 / $3.75 through Dec 31 2026**; $1.50/$7.50 from Jan 1 2027 | ❌ 2× |
| `gemini-3.1-pro-preview` | 2.0 / 12.0 | $2/$12 ≤200k; **$4/$18 >200k** | ⚠ low tier only |
| `gemini-3.1-flash-lite` | 0.25 / 1.5 | $0.25 / $1.50 (text) | ✅ |
| `gemini-3.5-flash` | *(none)* | $1.50 / $9.00 | missing |
| `gemini-3.5-flash-lite` | *(none)* | $0.30 / $2.50 | missing |
| `gemini-2.5-pro` | *(none)* | $1.25/$10 ≤200k | missing |
| `gemini-2.5-flash` | *(none)* | $0.30 / $2.50 | missing |

`gemini-3.6-flash` is the standout: the catalog carries the **post-promotion
price that takes effect in 4½ months**, so it tells a user the model costs
twice what it actually costs today. Independently corroborated by OpenRouter
(`google/gemini-3.6-flash ctx= 1048576 in= 0.7500 out= 3.7500`).

Context windows: all `1048576`, which OpenRouter confirms for every Gemini
model listed. ✅

### 3.4 Retired and unreachable providers still shipped

`docs/reviews/2026-08-11-verticals/11-model-catalog-research.md` told this
project two rounds ago to *"Drop or flag: AI21 (retired), GitHub Models
(retired), Lambda (winding down)"* and *"Update: Hyperbolic base_url `.xyz` →
`.ai`."* **Nothing was dropped, flagged, or updated.** Live probes today:

```
410  https://api.ai21.com/studio/v1/models
        {"detail":"This API has been retired. The AI21 Gateway is available at https://app.ai21.com — see https://docs.ai21.com/august-deprecation-notice"}
410  https://models.github.ai/inference/models
        {"error":{"code":"github_models_retirement_brownout","message":"GitHub Models is temporarily unavailable as part of a scheduled retirement brownout."}}
000  https://api.lambda.ai/v1/models
        curl: (56) CONNECT tunnel failed, response 502
401  https://api.hyperbolic.xyz/v1/models
        {"detail":"Not authenticated"}
000  https://api.hyperbolic.ai/v1/models
        curl: (56) CONNECT tunnel failed, response 502
```

- **`ai21`** (`base_url = "https://api.ai21.com/studio/v1"`) — HTTP **410, API
  retired**, and the catalog ships **zero** curated models for it, so the add
  flow depends entirely on a live `/models` call that returns 410.
- **`github-models`** (`https://models.github.ai/inference`) — HTTP **410,
  retirement brownout**. Also zero curated models.
- **`lambda`** (`https://api.lambda.ai/v1`) — host does not resolve from here.
  Also zero curated models.
- **`hyperbolic`** — the prior round's advice to move `.xyz` → `.ai` was
  **wrong**, and it is fortunate it was ignored: `.xyz` answers 401 (endpoint
  live, key required) and `.ai` does not resolve. The catalog is correct.

**User-visible consequence:** three dead providers sit in the picker with no
models and no marker. A user selects AI21, the add flow tries `/models`,
gets 410, and reports a network/auth failure — the honest answer is "this API
was retired." Class (b): the research that identified them exists in the repo
and was never wired to the catalog. The `Provider` struct
(`crates/providers/src/model.rs:100-114`) has no deprecation/status field, so
there is nowhere to record it even if someone wanted to.

### 3.5 Live diff of every open provider catalog

I fetched every provider `/models` endpoint that answers without a key and
diffed the curated rows programmatically
(`/tmp/review-external-research/diff_live.py`). Results:

| Provider | Rows | Live | Result |
|---|---|---|---|
| openrouter | 24 | 411 | 24/24 present, **ctx 24/24 exact**, 3 price mismatches |
| deepinfra | 41 | 184 | 40/41 present; `canopylabs/orpheus-3b-0.1-ft` absent |
| novita | 24 | 146 | 24/24 present; 3 minor ctx diffs |
| venice | 16 | 110 | 16/16 present; 7 ctx diffs (catalog uses 2^n, Venice uses round numbers) |
| sambanova | 6 | 6 | 6/6 exact (ids, ctx and prices) |
| inference-net | 21 | 39 | 21/21 exact |
| opencode-zen | 28 | 61 | 27/28 present; `longcat-2.0-free` absent |
| featherless | 7 | 21702 | 7/7 present |
| nebius | 29 | 29 | 29/29 present; 2 real ctx errors (below) |
| chutes | 8 | 13 unauth | 4 absent — but the unauth list is the TEE subset only, so **not a finding** |

The three OpenRouter price mismatches:

| Row | Catalog | Live | |
|---|---|---|---|
| `google/gemini-3.6-flash` | 1.5 / 7.5 | 0.75 / 3.75 | 2× high (same root cause as §3.3) |
| `z-ai/glm-5.2` | 0.5 / 3.15 | 0.392 / 1.232 | out 2.6× high |
| `z-ai/glm-5.1` | 1.4 / 4.4 | 0.952 / 2.992 | ~1.5× high |

Nebius, against its **open** `models_info` endpoint (`flavors[].model_id`,
`context_window_k`, `input_price_per_million_tokens`):

- `deepseek-ai/DeepSeek-V4-Flash` — catalog `context_tokens = 131072`
  (line 466); Nebius reports `context_window_k: 1024` and its own description
  reads *"a 1M-context reasoning model."* **Catalog understates by ~8×.**
- `deepseek-ai/DeepSeek-V4-Pro` — catalog `1048576` (line 459); Nebius reports
  `context_window_k: 1000` → 1,000,000. Overstated ~4.9 %.
- Every Nebius price in the catalog matches live exactly. The remaining ctx
  "diffs" my script printed are rounding artifacts of Nebius's K-granularity
  field (262 ↔ 262144) and are not errors.

The 8× understatement is the more damaging direction: a user is told the model
holds 131K tokens when it holds ~1M, so retrieval and compaction are throttled
to an eighth of the real budget with no error and no way to notice.

### 3.6 Are wrong prices load-bearing? No — verified

`crates/providers/src/model.rs:128-130` documents the cost fields as
display-only. I traced the consumers: they are rendered in
`crates/tui/src/render.rs` and `crates/tui/src/accessible.rs` only; nothing
sums them into a budget or a routing decision. **A wrong price misleads; it
does not mis-bill.** A wrong `context_tokens` is a different matter — §3.1.

---

## 4. MCP against the current spec — the largest external gap

`crates/integrations/src/mcp/client.rs:16`:

```rust
/// The newest MCP protocol revision this client speaks.
const PROTOCOL_VERSION: &str = "2025-06-18";
```

Fetched `https://modelcontextprotocol.io/specification/versioning`:

> The **current** protocol version is [**2026-07-28**](/specification/2026-07-28/).

That is two revisions ahead (2025-06-18 → 2025-11-25 → 2026-07-28), and the
newest is a **hard break**. From `/specification/2026-07-28/changelog`:

> 2. Make MCP stateless: **remove the `initialize`/`notifications/initialized`
> handshake**. Every request now carries its protocol version and client
> capabilities in `_meta` (`io.modelcontextprotocol/protocolVersion`,
> `io.modelcontextprotocol/clientCapabilities`) … Version mismatches return
> `UnsupportedProtocolVersionError`.
>
> 3. Add `server/discover`: servers MUST implement this RPC …
>
> 8. All results now carry a required `resultType` field …

The repo's client is squarely "legacy" in the spec's own terminology — it opens
with `initialize` (`mcp/client.rs:210-239`) and sends
`notifications/initialized`. The spec's compatibility matrix
(`/specification/2026-07-28/basic/versioning`) is explicit:

> | Legacy | Modern | **Fails.** stdio: the server rejects `initialize` with a
> JSON-RPC error … **Legacy clients have no fall-forward mechanism.** |

**User-visible consequence:** a user adds any MCP server that has adopted
2026-07-28 without dual-era support. `McpClient::spawn` fails at the handshake,
`ServerState` becomes `Failed`, and every tool that server offers silently
vanishes from `offered_tools()` — the run proceeds with fewer tools and no
error surfaced to the user. Servers that kept legacy support still work, so
this is a fleet-degrading failure, not a total one, and the failure mode is a
silent capability drop rather than a crash. Class (c).

Two more spec-currency notes, both minor while the client stays legacy:

- `flatten_content` (`mcp/client.rs:352-368`) ignores `structuredContent` —
  documented as a v1 choice at `:350`, and honest.
- The 2026-07-28 revision **removes** `ping`, `logging/setLevel` and the
  server→client request pattern the driver answers with `-32601`
  (`mcp/jsonrpc.rs:344-356`), replacing them with Multi Round-Trip Requests.
  Answering method-not-found is correct for a legacy client.

**What the MCP client gets right against 2025-06-18:** the `initialize` params
shape, `notifications/initialized`, `tools/list` `cursor` pagination with
loop/empty-cursor detection (`:289-306`), `tools/call` with `name`/`arguments`,
and `isError` → error mapping. It accepts whatever revision the server answers
without validating it (`:225-233`) — lenient, and reasonable for a legacy
client, but it means a server answering `2026-07-28` to a legacy `initialize`
would be accepted and then mis-driven.

**And a consumer gap:** the `mcp` subcommand is `mcp list` only, and
`commands::mcp_list` (`crates/cli/src/commands.rs:2650-2664`) never connects —
it prints `mcp.toml` and policy dispositions. The only production consumer of
`McpRegistry` is `crates/codypendentd/src/lib.rs:242`. **There is no CLI path
that will tell a user their MCP server cannot be reached.** Given §4's
handshake break, that is the difference between a diagnosable failure and an
invisible one. Class (b) for the diagnostic surface.

---

## 5. Everything else the product claims to integrate with

Every external host referenced by `README.md`, the user guide,
`crates/integrations/src/` and `crates/cli/src/`, probed live where possible:

| Service | Where | Status |
|---|---|---|
| ACP agent registry | `cdn.agentclientprotocol.com` | ✅ HTTP 200, 38 agents |
| Anthropic API | `api.anthropic.com` | ✅ 401 (`x-api-key header is required`) |
| OpenAI API | `api.openai.com` | ✅ 401 (`Missing bearer authentication`) |
| Hugging Face Hub | `huggingface.co` (`models pull`) | ✅ reachable |
| Tavily search | `api.tavily.com` | ✅ 401 (`Unauthorized: missing or invalid API key`) |
| Nebius Token Factory | `tokenfactory.nebius.com` | ✅ 200, open catalog |
| Ollama / LM Studio / vLLM | localhost | n/a — local |
| GitHub API | `api.github.com` | blocked by **this sandbox's** proxy, not a product defect |
| AI21 | `api.ai21.com` | ❌ **410 retired** |
| GitHub Models | `models.github.ai` | ❌ **410 retired** |
| Lambda | `api.lambda.ai` | ❌ unreachable |

Nothing is claimed that does not exist. The three failures are the catalog
providers in §3.4.

---

## 6. The pattern

Every finding here is the same shape: **the repo's picture of the outside world
was correct on the day it was written and nothing re-derives it.** The MCP
revision string, the Gemini price, the Anthropic context window, the retired
provider base URLs, the `unsound` scope of `cargo deny` — each was a true
statement about 2026-06 or 2026-07 that quietly became false, and in every case
the code has no mechanism that would notice. The repo even *contains* the
research that identifies three dead providers
(`docs/reviews/2026-08-11-verticals/11-model-catalog-research.md`, two rounds
old) — it was written down and never wired to the artifact it describes, which
is category (b) applied to knowledge rather than to code. And the one place a
refresh mechanism obviously exists — Nebius publishes an open `models_info`
endpoint that the same prior report called *"a natural 'refresh catalog'
command target"* — is exactly where two of the catalog's context windows are
wrong. The catalog is a hand-maintained snapshot of ten live APIs with no
snapshot date, no staleness marker, no provider status field, and no `models
refresh` command; the only guard against drift, `clamp_context_tokens`, has a
2,000,000 ceiling that by construction cannot catch a curated row that is wrong
by 5×.

---

## 7. What I did not verify

- **I did not re-drive `codypendent acp serve` end to end.** §2.3's "repaired"
  is read from `crates/cli/src/acp.rs:78-89,149` — the code now accepts
  `Payload::Catchup` where it previously demanded `CommandAccepted`, with a
  comment naming the old bug. I did not start a daemon and speak JSON-RPC to
  it. **Inferred, not observed.**
- **I did not stand up an MCP server at revision 2026-07-28.** §4's failure
  claim is the spec's own compatibility matrix applied to code I read
  (`mcp/client.rs:210-239`). The direction of the break is unambiguous in the
  spec text; the specific error a given server returns is implementation-defined
  and I did not observe one. **Inferred.**
- **No provider API key.** Every provider probe is an unauthenticated HTTP
  status, so I verified that endpoints exist and how they answer, never that a
  completion succeeds. Key-gated catalogs (openai, anthropic, gemini, xai,
  moonshot, mistral, cohere, qwen, groq, cerebras, fireworks, together,
  baseten, parasail, perplexity, minimax, zhipu, azure-openai, amazon-bedrock)
  were checked against **vendor documentation** — Anthropic's own model table,
  OpenAI's pricing page, Google's pricing page — and cross-checked against
  OpenRouter's live registry where the same model is resold. Providers with
  neither an open catalog nor a doc page I fetched are **unverified**:
  moonshot, mistral, cohere, qwen, groq, cerebras, fireworks, together,
  baseten, parasail, perplexity, minimax, zhipu, hyperbolic, azure-openai,
  amazon-bedrock.
- **`agent-framework-core`'s streaming/tool APIs.** I verified the
  `AnthropicClient` constructor and `with_base_url` against the vendored 0.2.0
  source, and confirmed `ChatClient` / `ChatOptions` / `Message` /
  `ChatStream` are the real import paths. I did not audit the full
  `get_streaming_response` usage in `crates/runtime/src/agent.rs` against the
  crate's contract.
- **`cargo deny check licenses`** passed but I did not independently audit the
  SPDX allow-list against every crate's actual licence.
- **`wasmi` API usage** — I confirmed the pin exists and its currency; I did
  not read `crates/sandbox`'s use of the 0.51 API.
- **I did not run any crate's test suite** (disk budget), and per the brief I
  would not have treated a green suite as evidence anyway.
