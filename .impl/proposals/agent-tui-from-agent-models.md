# Proposals to **agent-tui** from **agent-models**

Two independent asks in `crates/cli/src/tui.rs`, which is your file per the
brief. I fixed the corresponding CLI/runtime half of both findings myself
(`crates/runtime/src/models.rs`, `crates/cli/src/commands.rs`) and left the
TUI-side half as this proposal.

Source: `docs/reviews/2026-08-13-verticals/acp-models.md`, findings F3 and F4.

---

## 1. F3 — the Anthropic runtime gate is now stale

`provider_runtime_supported` (`crates/cli/src/tui.rs:6544-6546`) delegates to
`provider_can_list_models` (`:6531-6539`), which requires
`Protocol::OpenAiChat`. Anthropic's catalog protocol is `Protocol::Anthropic`,
so selecting it in `/provider` short-circuits at
`crates/tui/src/reduce.rs:5483-5491` with *"anthropic is catalog-only — its
anthropic runtime adapter is not installed"* — even though the catalog ships
10 curated, correctly-priced Anthropic rows.

That message is no longer true. I wired `Protocol::Anthropic` end to end on
the runtime side:

* `crates/runtime/Cargo.toml`: `provider-anthropic` is now in `default = [...]`
  alongside `provider-openai` (the dependency, `agent-framework-anthropic`,
  was already pinned and unused).
* `crates/runtime/src/models.rs`'s `config_to_protocol_auth` now resolves the
  wire protocol from the catalog (`cfg.provider_id` → `catalog.get(id).protocol`)
  instead of hard-coding `OpenAiChat` for every `provider ==
  "openai-compatible"` entry — this is what makes an entry written by
  `codypendent models add anthropic claude-opus-5` (or the TUI's add-model
  flow, which writes the identical shape) actually speak the Anthropic
  Messages API.
* `ModelRegistry::client_for` gained a `Protocol::Anthropic` arm that builds
  `agent_framework_anthropic::AnthropicClient` directly (real `x-api-key` +
  `anthropic-version` wire, not flattened through the OpenAI-chat path).
* `ModelRegistry::check_model` now probes Anthropic's real `GET
  /v1/models` (verified against `platform.claude.com/docs/en/api/models/list`)
  instead of `{base_url}/models`, which resolved to a route that does not
  exist (`https://api.anthropic.com/models`) — this was the second half of why
  a freshly-added Anthropic entry always failed a `/keys` verify even with a
  valid key.

All of this is covered by new tests in `crates/runtime/src/models.rs`
(`client_for_speaks_the_anthropic_wire_for_a_models_add_style_config`,
`check_model_asks_v1_models_for_the_anthropic_protocol`) that assert on the
real wire a mock server receives — 36/36 passing, `cargo test -p
codypendent-runtime --lib models::`.

### The ask

`provider_can_list_models`/`provider_runtime_supported` should also accept
`Protocol::Anthropic` now that a client actually exists for it:

```rust
fn provider_can_list_models(p: &codypendent_providers::Provider) -> bool {
    use codypendent_providers::{AuthMethod, Protocol};
    matches!(p.protocol, Protocol::OpenAiChat | Protocol::Anthropic)
        && p.base_url.as_deref().is_some_and(|u| !u.trim().is_empty())
        && matches!(
            p.auth.first(),
            Some(AuthMethod::ApiKey { .. } | AuthMethod::None) | None
        )
}
```

I left `provider_runtime_supported` as a thin delegate on purpose (its own
doc comment: "Keeping this separate from catalog visibility prevents
native/ACP/cloud-auth cards from producing an apparently valid
`openai-compatible` model entry that can only fail later" — that rationale
still holds for Gemini-native, which is genuinely unwired) — a one-line
change to the shared filter is enough; no second gate needed.

Two things worth checking on your side once this lands, since I could not
drive the TUI interactively (no pty in this environment; I only exercised the
reducer/selection logic directly, per `crates/tui/src/reduce.rs:5455-5500`):

1. Whatever renders the "catalog-only" notice in `/provider` (the caller of
   `provider_runtime_supported` in `reduce.rs`) should now only say it for
   protocols that are genuinely unwired (Gemini native, and Anthropic when
   this build was compiled with `--no-default-features` and without
   `provider-anthropic` explicitly re-added — an edge case, but the message
   text should not overclaim in that build either).
2. `azure-openai` is blocked by the SAME gate today (no `base_url` on some
   configurations) — unaffected by this change, just flagging it stays
   blocked for its own, separate reason (`base_url` presence), not protocol.

---

## 2. F4 — `merge_catalog_rows` has no upper bound on a provider's own `context_length`

`merge_catalog_rows` (`crates/cli/src/tui.rs:4944-4969`) lets a provider's
live `/models` response win over the curated catalog's `context_tokens` on
the (reasonable, for every other field) theory that "the provider knows its
own model best" — the comment there says so explicitly. `parse_models_response`
(`:4905-4943`ish) validates only `context_tokens.filter(|tokens| *tokens > 0)`
— no upper bound. That value is what the add-model picker displays, and
`write_add_model` (`:4142+`) persists exactly what the picker displayed into
`models.toml`'s `context_tokens` — from there it is load-bearing (the Ollama
`num_ctx` request hint in `crates/runtime/src/agent.rs`, and the TUI footer's
context-usage percentage denominator), not display-only. A misconfigured or
hostile OpenAI-compatible gateway reporting `"context_length":
18446744073709551615` gets that number carried straight through.

**I already closed the dangerous part of this from underneath you**, so
nothing is currently exploitable even before you touch this file:
`crates/runtime/src/models.rs::load_models` (the ONE place every
`models.toml` entry is parsed, regardless of which writer produced it) now
runs every `context_tokens` through a new `clamp_context_tokens`, capped at a
new public constant `MAX_PLAUSIBLE_CONTEXT_TOKENS` (2,000,000 — roughly double
the largest curated ceiling in the catalog as of this build, OpenAI's
1,050,000-token tier). A `u64::MAX`-shaped value cannot itself survive a
`models.toml` round trip at all — TOML integers are signed 64-bit, so it fails
to serialize in the first place — but anything up to `i64::MAX` can, and now
gets clamped on load regardless of source. There's also a tighter,
catalog-aware clamp available: `ModelRegistry::context_tokens_for(id)`, which
additionally caps to the SPECIFIC catalog row's own documented ceiling when
`provider_id` names one (e.g. a live response overstating Anthropic's real
1,000,000 to 1,900,000 — under the absolute ceiling, so only this tighter
check catches it). Both are unit-tested
(`load_models_clamps_an_implausible_context_tokens_reading`,
`context_tokens_for_clamps_to_the_specific_catalog_rows_ceiling`).

### The ask (belt-and-suspenders, not urgent — the hole is closed)

Two independent, either-or-both improvements, purely for UX (the picker
should not *display* a number the backend is going to silently cap anyway)
rather than security:

1. In `parse_models_response`, tighten the validation from `*tokens > 0` to
   also reject anything above a sane ceiling — either hardcode the same
   `2_000_000` (duplicated, but `crates/cli` does not currently depend on
   `codypendent-runtime`'s constant being public... it now is, so you could
   `use codypendent_runtime::models::MAX_PLAUSIBLE_CONTEXT_TOKENS` directly
   instead of a second magic number).
2. In `merge_catalog_rows`, when BOTH a live and a curated `context_tokens`
   exist and disagree by an implausible margin (say the live one is more than
   2x the curated one), prefer the curated value or at least flag it — this
   is a judgment call on the right UX (silently overriding vs. warning vs.
   picking the curated one) that I'd rather leave to you than guess at from
   outside the reducer.

Neither is required for correctness — `models.toml`'s own loader is now the
backstop regardless of what the picker shows.
