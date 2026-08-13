# Proposal to **agent-retrieval** from **agent-models**

One change, in `crates/runtime/src/agent.rs` (yours per the brief). Small,
additive, and the vulnerability it closes further is already closed at the
layer below it — this is about precision, not an open hole.

Source: `docs/reviews/2026-08-13-verticals/acp-models.md`, finding F4.

## Context

The review found `context_tokens` crossing a trust boundary unclamped: a
provider's own `/models` response can win over the curated catalog
(`crates/cli/src/tui.rs::merge_catalog_rows`), gets persisted to
`models.toml` verbatim, and from there is load-bearing — `FrameworkModelDriver::
from_registry` (`crates/runtime/src/agent.rs:5792-5793`) reads
`ModelConfig::context_tokens` directly, and `apply_context_window`
(`:6652+`, used at `:6587`) forwards it verbatim as Ollama's
`{"options":{"num_ctx": n}}` request hint. It is also the denominator of the
TUI footer's context-usage percentage.

## What I already fixed underneath this (no action required for correctness)

`crates/runtime/src/models.rs::load_models` — the ONE place every
`models.toml` entry is parsed regardless of writer — now clamps every
`context_tokens` to a new public `MAX_PLAUSIBLE_CONTEXT_TOKENS` (2,000,000)
via a new `clamp_context_tokens` function. `ModelConfig::from_registry` reads
the config `load_models` already produced, so the value it sees today is
already capped; a `u64::MAX`-shaped attack is no longer live. This is tested
(`load_models_clamps_an_implausible_context_tokens_reading`,
`cargo test -p codypendent-runtime --lib models::` — 36/36 passing).

## The ask (tightens the ceiling further, still optional)

I also added `ModelRegistry::context_tokens_for(&self, id: &ModelId) ->
Option<u64>`, which additionally clamps to the SPECIFIC catalog row's own
documented ceiling when the config's `provider_id` names a known one — e.g. a
provider's live response overstating Anthropic's real 1,000,000-token ceiling
to 1,900,000 is under the absolute 2,000,000 cap (so `load_models`'s clamp
does not catch it) but IS caught by this tighter, per-model check. Tested in
`context_tokens_for_clamps_to_the_specific_catalog_rows_ceiling`.

If you'd like `from_registry` to use the tighter number instead of the blunt
one, the change is small:

```rust
// crates/runtime/src/agent.rs, in FrameworkModelDriver::from_registry (~5793)
- let context_tokens = models.get(&model_id).and_then(|cfg| cfg.context_tokens);
+ let context_tokens = models.context_tokens_for(&model_id);
```

`context_tokens_for` is on `ModelRegistry` (`#[cfg(feature =
"provider-openai")]`, same gating `from_registry` already lives under) and
already applies `clamp_context_tokens` internally, so this is strictly a
tightening — no other behavior changes, and every model without a
`provider_id`/catalog match behaves exactly as it does today (falls through
to the absolute clamp, same as calling `cfg.context_tokens` directly then
clamping). Not urgent: the blunt clamp already means nothing implausible
reaches `num_ctx` or the footer percentage; this only sharpens the "how
implausible" bar for models the catalog specifically curates.

---

## Second, unrelated ask — outcome 11's missing writeback hook

`crates/daemon/src/model_profiles.rs::ModelProfileStore::record_outcome`
(new, migration `0025_routing_outcomes.sql`) is the writer outcome 11 was
missing entirely: it takes `(model_id, endpoint, TaskClass, success, run_id)`,
appends a durable raw observation, and folds the recomputed aggregate rate
into the stored profile's `performance.task_class_success` — the map that was
permanently `Default::default()` before (`crates/runtime/src/bench.rs`'s
`into_profile` was the only non-test constructor, and it always wrote an
empty one), which is why `classify.rs`'s nine-class classifier never actually
changed which model routing picked. Fully tested
(`cargo test -p codypendent-daemon --lib model_profiles::` — 8/8 passing,
including `record_outcome_folds_real_run_results_into_task_class_success`).

Nothing calls it yet. I looked for the right hook and it is squarely in your
file, not mine or `codypendentd`'s (the ACP path's terminal emission also
lives in `codypendentd/src/executor.rs::finish_acp_run`, but that file was
under concurrent edit by another agent for the whole window I had — I did not
want to risk a collision on a contested file for an addition this size, so
I'm leaving it as a proposal with the exact seam rather than guessing at a
patch against code I couldn't safely re-read at the moment of editing).

The single-agent (non-ACP) path's terminal disposition is assembled in
`FrameworkAgentRuntime::execute_run` (`crates/runtime/src/agent.rs:2151+`):
by the time it reaches the `(state, disposition)` match (`:2727-2741`) it
already has `driver.model_id()` (bound at `:2172`) and `run.objective`
(`RunContext::objective`) in scope — both of what `classify::classify(&
TaskSignals::from_objective(...))` needs to produce a `TaskClass`, and
`disposition` maps directly to `record_outcome`'s `success: bool`
(`RunDisposition::Completed => true`, `Failed => false`, `Cancelled => skip —
ambiguous, not a model-quality signal either way`).

What's missing to actually call it from there: (1) `agent.rs` does not
currently depend on `codypendent-daemon` (a `SqlitePool` + `ModelProfileStore`
call needs one) or `codypendent-routing`'s `classify` module — both are
one-way, acyclic additions per their own Cargo.toml comments
(`codypendent-daemon = { workspace = true }` is already a dependency
actually — worth double-checking whether it's already in scope before adding
a new edge); (2) the run's serving *endpoint* (the second half of
`record_outcome`'s key) is not visible at this layer — `ModelDriver` abstracts
over it, so either the trait needs an accessor or the caller of `execute_run`
threads it down alongside `driver`. I'd suggest making the actual DB write
best-effort and non-fatal (log a warning, never fail an already-terminal run
over a telemetry write), mirroring how `harvest_memories`/learning capture
already treat their own post-run side effects as advisory in this file.
