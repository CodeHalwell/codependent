# Context-Window Protection + Visibility — Design Spec

- **Date:** 2026-07-28
- **Branch:** `claude/context-window` (base `1c31625`)
- **Status:** Draft for review (spec only — no code in this change)
- **Scope:** MEDIUM, cross-cutting (runtime + protocol reuse + TUI reuse)

---

## 1. Problem

A local-Ollama user driving a **normal (non-workflow) TUI chat** has **zero** context-window
protection and **zero** context-window visibility. Three independent gaps, each verified at
file:line:

1. **`num_ctx` is never set.** `ModelRegistry::client_for`
   (`crates/runtime/src/models.rs:327-328`) builds the client as
   `OpenAIChatCompletionClient::new(api_key, cfg.model).with_base_url(cfg.base_url)` with **no
   request options**. Ollama's server-side default `num_ctx` is small (commonly 2048–4096), so a
   long run silently overflows the window server-side and the model "forgets" the system
   prompt/objective mid-run. Nothing in the codebase ever communicates the model's real window to
   the server.

2. **The plain loop has no token accounting.** `AgentRuntime::execute_run`
   (`crates/runtime/src/agent.rs:902`) tracks only `MAX_STEPS` (`=256`,
   `agent.rs:84`) and a wall-clock ceiling `MAX_WALL_CLOCK_SECS` (`=30*60`, `agent.rs:89`).
   The transcript is an ever-growing `Vec<TurnItem>` (`agent.rs:934`) rebuilt and re-sent WHOLE
   every step (`to_messages`, `agent.rs:2656`). Nothing counts tokens or compares them to any
   window.

3. **The TUI context-% footer is permanently dead for normal chat.** `RunView.context_percent`
   (`crates/tui/src/state.rs:378`, "projected from the token budget") is rendered in the footer as
   `ctx` (`render.rs:211`, and the ambient status row `render.rs:1273`) and is populated ONLY by
   the `EventBody::BudgetWarning { dimension: BudgetDimension::Tokens, .. }` reducer arm
   (`crates/tui/src/reduce.rs:535-546`). The only emitter of `BudgetWarning{Tokens}` is the
   **workflow** budget engine (`crates/workflow/src/budget.rs:392` → observers →
   `drive.rs`). The plain loop emits only `BudgetWarning{WallClock}` (`agent.rs:981-988`). So in a
   normal chat the `ctx` field is always `—`.

Additional structural fact discovered during verification (drives Component C1 below):
`context_tokens` lives on `codypendent_providers::Model` (`crates/providers/src/model.rs:126`,
"DISPLAY-ONLY"), a **catalog** type. The runtime's `ModelConfig`
(`crates/runtime/src/models.rs:65-85`) — the type the agent loop actually has in hand via
`ModelRegistry` — has **no** context-window field at all. The window size is therefore not
reachable from the loop today; a source of truth must be added.

---

## 2. Goals / Non-Goals

### Goals (v1)

- G1. Communicate the model's real context window to the server so Ollama uses it instead of its
  tiny default (`num_ctx`), **when a window is known and the client can carry it**.
- G2. Estimate live context usage in the plain loop and surface it as a percentage through the
  **existing** `EventBody::BudgetWarning{Tokens}` event, bringing the dead `ctx` footer alive for
  normal chat — **no wire/protocol/golden change**.
- G3. Honesty: when the model's window is unknown, the footer shows `—`, never a fabricated
  number, and no `num_ctx` is invented.
- G4. Degrade gracefully: an unknown window, an older Ollama that ignores the option, or a
  non-Ollama endpoint must never break or change a run's outcome.

### Non-Goals (v1)

- N1. Auto-trimming / summarizing the transcript to fit the window (designed here at the interface
  level only; see §7 KEY DECISION — recommended **warn-only** for v1).
- N2. A real BPE tokenizer dependency (a char-based estimate is sufficient; see Constraints).
- N3. Threading the full `providers` catalog into the runtime loop (out of scope; C1 uses a local
  additive field instead).
- N4. Any change to the workflow budget engine (`crates/workflow/src/budget.rs`) — it already
  emits the event correctly; this spec only adds a *second* emitter in the plain loop.

---

## 3. Architecture Overview

```
models.toml ──► ModelConfig (+ context_tokens: Option<u64>)   [C1]
                     │
                     ├─(client build)──► num_ctx injected into ChatOptions.additional_properties [C2]
                     │                        └► OpenAIChatCompletionClient → /v1/chat/completions body
                     │
                     └─(loop denominator)──► AgentRuntime::execute_run                            [C4]
                                                 │ each step:
                                                 │   used = estimate_tokens(&transcript)          [C3]
                                                 │   limit = window (Some ⇒ known)
                                                 │   if Some(limit): emit BudgetWarning{Tokens,used,limit}
                                                 ▼
        SessionEvent::BudgetWarning{Tokens} ──► TUI reduce.rs:535 arm (UNCHANGED)
                                                 │ pct = used*100/limit
                                                 ▼
                                            RunView.context_percent = Some(pct)  ──► footer `ctx N%`
                                            (window None ⇒ never emitted ⇒ footer stays `—`)       [C5]
```

Two independent value flows share one source of truth (`context_tokens`):
the **num_ctx request hint** (server-side protection) and the **percentage denominator**
(client-side visibility). Either can be absent without breaking the other.

---

## 4. Components

### C1 — Context window as a first-class `ModelConfig` field

**Where:** `crates/runtime/src/models.rs` (`ModelConfig`, ~line 65).

**What:** Add one additive, optional field:

```rust
/// The model's context window in tokens, if known. Sourced from the built-in
/// provider catalog (`codypendent_providers::Model::context_tokens`) or set
/// directly in models.toml. Used for two things: (1) the `num_ctx` request
/// hint (C2), and (2) the denominator of the context-usage percentage (C4).
/// `None` means "unknown" — no percentage is fabricated and no num_ctx is sent.
#[serde(default)]
pub context_tokens: Option<u64>,
```

**Rationale for this source (vs. threading the `providers` catalog):** the loop already holds a
`ModelRegistry` of `ModelConfig` (`agent.rs:778`, accessor `models()` at `agent.rs:885`). Adding
the field to `ModelConfig` makes the window reachable with zero new cross-crate wiring and keeps
the honesty property trivial (`Option`). `models.toml` gains an optional key; every existing file
parses unchanged (`#[serde(default)]`). A follow-up may auto-populate this field from the
`providers` catalog at load time (matching `(provider_id, model)` → `context_tokens`), but v1 does
not require it — a user can set `context_tokens = 32768` under a `[[model]]` entry.

**models.toml example (additive):**

```toml
[[model]]
id = "local-default"
provider = "openai-compatible"
base_url = "http://localhost:11434/v1"
model = "qwen2.5-coder:14b"
api_key_env = ""
context_tokens = 32768   # NEW, optional
```

### C2 — `num_ctx` request hint

**Key verified fact:** the OpenAI client **can** carry an arbitrary extra top-level body field.
`ChatOptions` (`agent-framework-core-0.1.1/src/types/options.rs:228`) exposes
`pub additional_properties: HashMap<String, serde_json::Value>`, and the OpenAI converter forwards
every entry onto the request body:

```rust
// agent-framework-openai-0.1.1/src/convert.rs:294-296
for (k, v) in &options.additional_properties {
    body.entry(k.clone()).or_insert_with(|| v.clone());
}
```

So the mechanism to attach `num_ctx` **exists and is verified** — it does not require patching the
vendored crate.

**Where:** `crates/runtime/src/agent.rs` — `FrameworkModelDriver` (struct at `agent.rs:2439`,
`from_registry` at `agent.rs:2455`, `next_step` at `agent.rs:2717`).

**What:**
1. `FrameworkModelDriver` gains a field `num_ctx: Option<u64>`.
2. `FrameworkModelDriver::from_registry` reads it from the resolved `ModelConfig.context_tokens`
   (the registry is already passed in). `new(...)` keeps a `None` default for test drivers.
3. In `next_step`, when `Some(n)`, insert the hint into `options.additional_properties` **before**
   the streaming call at `agent.rs:2730`:

   ```rust
   if let Some(n) = self.num_ctx {
       // Ollama reads generation parameters from a nested `options` object.
       options.additional_properties
           .insert("options".into(), serde_json::json!({ "num_ctx": n }));
   }
   ```

**Wire-shape honesty (important, do not overclaim):** `additional_properties` reaching the body is
**verified**; whether a given Ollama build **honors** a nested `options.num_ctx` sent to the
OpenAI-compatible `/v1/chat/completions` endpoint is **version-dependent** and is NOT guaranteed by
this codebase. Two consequences, both designed for:

- **Harmless when ignored.** An endpoint that doesn't recognize the field ignores it (unknown
  JSON body key); the run is unaffected. This satisfies G4.
- **Guaranteed fallback (documented, not code).** The server-authoritative ways to raise Ollama's
  window are out-of-band and always work: a `Modelfile` with `PARAMETER num_ctx N` (creating a
  model variant), or the server env `OLLAMA_CONTEXT_LENGTH=N`. The spec documents these in the
  user guide as the reliable path; the in-request hint is a best-effort convenience layered on top.

**Configurability of the exact key (decision, see §7):** the body shape is not universal across
OpenAI-compatible servers. v1 hardcodes the Ollama shape `{"options":{"num_ctx":n}}` because Ollama
is the stated target. A future `[[model]]` key (e.g. `num_ctx_json_pointer` or a raw
`extra_body`) can generalize this; flagged as an open question, not built in v1.

**Degradation matrix:**

| Condition | Behavior |
|---|---|
| `context_tokens = None` | no hint inserted; run unchanged |
| `provider-openai` feature off | `FrameworkModelDriver` not compiled; N/A |
| Endpoint ignores the field | hint dropped server-side; run unchanged |
| Non-Ollama OpenAI endpoint | hint likely ignored; run unchanged |

### C3 — Token estimator (pure function)

**Where:** new pure fn in `crates/runtime/src` (co-located with the loop, e.g. a small `context`
module or private fn in `agent.rs`).

**What:**

```rust
/// Cheap, dependency-free context-size estimate for a transcript, in tokens.
/// Heuristic: ~4 characters per token (the widely-used rule of thumb for
/// English/code with BPE tokenizers), applied to the rendered text of every
/// TurnItem plus a small fixed per-message overhead for role/formatting.
/// This is deliberately an ESTIMATE — it is compared against a known window to
/// drive a percentage and a warning, never used to bill or to hard-truncate.
pub fn estimate_context_tokens(transcript: &[TurnItem]) -> u64 {
    const CHARS_PER_TOKEN: u64 = 4;
    const PER_ITEM_OVERHEAD_TOKENS: u64 = 4; // role tag + delimiters
    transcript.iter().map(|item| {
        let chars = turn_item_text_len(item) as u64; // sums objective/assistant/
                                                      // tool args+output/steering text
        chars / CHARS_PER_TOKEN + PER_ITEM_OVERHEAD_TOKENS
    }).sum()
}
```

- **No heavy dependency.** Char-count / 4 needs nothing beyond `std`. Justified: the value only
  drives a footer percentage and an 80% warning, where a ±20% estimate error is immaterial —
  it must never *undercount so badly it hides* an approaching overflow, which the conservative
  per-item overhead guards against. A real tokenizer (e.g. `tiktoken-rs`) is explicitly rejected
  for v1 (dependency weight, per-provider tokenizer mismatch — Ollama models are not GPT-BPE).
- **`turn_item_text_len`** sums the character length of each `TurnItem` variant's payload
  (`Objective`, `Assistant`, `ToolCall` tool+args JSON, `ToolResult` output, `Steering`), matching
  what `to_messages` (`agent.rs:2656`) actually sends. It is a pure helper, unit-testable in
  isolation.

### C4 — Plain-loop emit (the footer comes alive)

**Where:** `crates/runtime/src/agent.rs::execute_run` loop (`agent.rs:955` onward), at the same
per-step safe point where the wall-clock warning is emitted (`agent.rs:976-989`).

**What:**
1. Before the loop, resolve the window once: `let window = self.models.get(&model_id).and_then(|c| c.context_tokens);`
   (Equivalently, add `fn context_window(&self) -> Option<u64>` to the `ModelDriver` trait,
   default `None`, implemented by `FrameworkModelDriver` — see §7 decision. Recommended: the
   driver method, so `ScriptedDriver` naturally returns `None` and tests need no registry.)
2. Each iteration, after the transcript is updated for the step, compute:
   ```rust
   if let Some(limit) = window {                 // C5: only when KNOWN
       let used = estimate_context_tokens(&transcript);
       // Dedup: emit only when the integer percent changed since last emit,
       // so a live-updating footer costs at most ~100 journaled events/run,
       // not one per step.
       let pct = (used.saturating_mul(100) / limit.max(1)).min(100) as u16;
       if Some(pct) != last_emitted_ctx_pct {
           last_emitted_ctx_pct = Some(pct);
           self.emit(run.session_id, run_actor.clone(),
               EventBody::BudgetWarning {
                   run_id: run.run_id,
                   dimension: BudgetDimension::Tokens,
                   used,
                   limit,
               }).await?;
       }
   }
   ```
3. `used`/`limit` are the raw token estimate and the window; the **TUI reducer**
   (`reduce.rs:544`) already computes `pct = used*100/limit` and stores
   `context_percent = Some(pct.min(100))`. **No TUI change is required** — the same event the
   workflow engine emits now also originates in the plain loop.

**Naming note:** the event is named `BudgetWarning`, but the `Tokens` arm is already used by the
workflow path as a *level indicator* (any %, not only ≥80%) — the reducer stores whatever `used/
limit` yields with no threshold gate. Reusing it for a live gauge is consistent with existing
behavior; this spec does **not** repurpose the event, it adds a second producer of the identical
shape.

### C5 — Honesty (unknown window ⇒ no number)

- When `context_tokens` is `None`, C4's `if let Some(limit) = window` short-circuits: **no
  `BudgetWarning{Tokens}` is ever emitted**, so `RunView.context_percent` stays `None`, and the
  renderer already prints `—` (`render.rs:212`, `render.rs:1274`:
  `.map_or("—".to_owned(), |p| format!("{p}%"))`). No code path fabricates a percent.
- C2 mirrors this: `None` window ⇒ no `num_ctx` hint invented.
- This is the single most important invariant and is directly testable (C4 with `window = None`
  emits zero `Tokens` events).

### C6 — Overflow handling (v1 = warn-only; trim designed at interface level)

- **Warn path (BUILT in v1):** the live percentage IS the warning surface. Because the estimate is
  emitted continuously, the footer climbs toward `100%` as the window fills; the user sees the run
  approaching overflow with no fabricated data. Optionally, the loop MAY additionally treat a
  crossing of a high threshold (e.g. ≥90%) as a one-shot log/steer, but v1's minimum is the live
  gauge — no behavioral change to what the model sees.
- **Trim path (INTERFACE ONLY in v1, not built):** when `used` approaches `limit`, the loop could
  compact the oldest tool results before `to_messages`. The seam:
  ```rust
  /// Return a transcript trimmed to fit `limit` tokens, dropping or
  /// summarizing the OLDEST TurnItem::ToolResult payloads first and NEVER the
  /// Objective or the most recent turns. Pure over (transcript, limit).
  fn compact_to_fit(transcript: &[TurnItem], limit: u64) -> Vec<TurnItem>;
  ```
  This dovetails with the daemon's existing `compacted_turn` seam in session history (referenced
  by the smarter-memory work). It is **not** wired in v1 because auto-trim changes what the model
  sees — a behavioral change that must be a deliberate, separately-reviewed decision (§7).

---

## 5. Data Flow (end to end, normal chat)

1. `models.toml` parsed → `ModelConfig { context_tokens: Some(32768) }` (C1).
2. `FrameworkModelDriver::from_registry` captures `num_ctx = Some(32768)` (C2).
3. Loop start: `window = Some(32768)` resolved once (C4).
4. Each step: `next_step` inserts `{"options":{"num_ctx":32768}}` into the request body (C2);
   loop computes `used = estimate_context_tokens(&transcript)` (C3), and if the integer percent
   changed, emits `BudgetWarning{Tokens, used, limit:32768}` (C4).
5. Daemon journals + publishes the event; TUI `reduce.rs:535` arm sets
   `context_percent = Some(used*100/32768)` (unchanged reducer).
6. Footer renders `ctx 41%` (`render.rs:211`), rising over the run.
7. If instead `context_tokens = None`: steps 2, 4-emit, 5, 6 collapse to "no hint, no event,
   `ctx —`" (C5).

---

## 6. Error Handling

- **Unknown window:** treated as a normal, first-class state (`None`), not an error — no emit, no
  hint (C5).
- **`limit == 0`** (a user setting `context_tokens = 0`): guarded by `limit.max(1)` in both the
  loop and the existing reducer (`reduce.rs:544`), so no divide-by-zero; percent saturates to
  `100` and is clamped by `.min(100)`.
- **Estimator over/undercount:** the estimate is advisory; a wrong value can only make the footer
  slightly optimistic/pessimistic and can never fail a run (warn-only). If `used > limit`, percent
  clamps to `100%`.
- **`num_ctx` unsupported by endpoint:** the field is ignored server-side; no error surfaces, run
  proceeds (verified: it is an extra body key, not a required contract). This is the graceful-
  degradation requirement (G4).
- **`emit` failure** (channel closed): already propagated via `?` exactly as the existing
  wall-clock warning emit at `agent.rs:988` — no new error semantics introduced.

---

## 7. KEY DECISIONS (confirm before planning)

1. **Warn-only vs. auto-trim in v1.** *Recommend WARN-ONLY.* Auto-trim (C6 trim path) silently
   changes what the model sees mid-run — a behavioral change that deserves its own spec/review and
   risks dropping context the user assumed was retained. v1 ships the live gauge + the `num_ctx`
   hint (which *raises* the real ceiling, the higher-leverage fix) and leaves `compact_to_fit` as
   a designed-but-unwired seam. **Confirm this scope.**

2. **Source & default of the window (`context_tokens`).** *Recommend:* new optional
   `ModelConfig.context_tokens` (C1), `None` by default (honest unknown), user-settable in
   `models.toml`, with catalog auto-population deferred. **Confirm there is NO fabricated default
   window** (e.g. we do NOT default unknown models to 4096 — that would both invent a percent and
   cap a possibly-larger real window). Also confirm the `num_ctx` value = `context_tokens` verbatim
   (no separate `num_ctx` config key in v1).

3. **Window plumbing: driver method vs. registry lookup in the loop.** *Recommend* adding
   `ModelDriver::context_window(&self) -> Option<u64>` (default `None`), so `ScriptedDriver`
   returns `None` and the loop stays decoupled from the registry; `FrameworkModelDriver` returns
   its threaded value. The alternative (`self.models.get(&model_id)…` inside the loop) works too
   but couples the loop to the registry and complicates tests. **Confirm the trait-method
   approach.**

(Secondary, lower-stakes: the emit-dedup policy — "emit when integer percent changes" — bounds
journal growth to ~O(100) events/run; confirm acceptable vs. banded thresholds.)

---

## 8. Constraints (compliance check)

- **Honesty.** Unknown window ⇒ no emit ⇒ footer `—`; no fabricated percent, no invented
  `num_ctx`. `limit.max(1)` guards zero. (C5, §6.) ✔
- **Reuse `EventBody::BudgetWarning{Tokens}` — no wire/golden change.** The plain loop emits the
  **identical** event shape the workflow engine already emits (`events.rs:151-156`,
  round-tripped at `events.rs:290`) and the TUI already consumes (`reduce.rs:535`). No new event,
  no `BudgetDimension` variant, no protocol/golden change. **Confirmed additive-only** — this
  spec introduces a second *producer*, not a new *shape*. ✔
- **No heavy new dependency.** Estimator is `chars/4 + overhead`, `std`-only (C3). A real
  tokenizer is explicitly rejected with justification. ✔
- **`num_ctx` degrades gracefully.** Only inserted when known; ignored-if-unsupported; never
  fails a run; documented server-side fallback (Modelfile / `OLLAMA_CONTEXT_LENGTH`). (C2, §6.) ✔
- **Testable.** `estimate_context_tokens` and `turn_item_text_len` are pure functions; the emit
  and the "footer comes alive" are both directly testable (§9). ✔

---

## 9. Testing

**Runtime (`crates/runtime`):**
- `estimate_context_tokens` is monotonic (appending a `TurnItem` never lowers the estimate) and
  matches a hand-computed value for a fixed transcript (pure, no async).
- `turn_item_text_len` covers every `TurnItem` variant, including `ToolCall` args JSON and
  `ToolResult` output.
- Loop emits `BudgetWarning{Tokens, used, limit}` with `limit == window` when the window is
  `Some` — assert via a capturing event sink over a scripted run.
- Loop emits **zero** `Tokens` events when the window is `None` (the honesty test).
- Emit-dedup: N steps that don't change the integer percent produce ≤1 additional `Tokens` event.
- C2: `FrameworkModelDriver` with `num_ctx = Some(n)` puts `{"options":{"num_ctx":n}}` into the
  request body — test at the `build_body`/`additional_properties` boundary (mirrors the existing
  openai-crate `build_body_forwards_known_additional_properties_only` test pattern); with
  `num_ctx = None`, the body has no `options` key.
- C1: a `models.toml` with and without `context_tokens` both parse; absent ⇒ `None`.

**TUI (`crates/tui`) — the "dead footer comes alive":**
- Feed the reducer a `BudgetWarning{Tokens, used: 8192, limit: 32768}` originating from a plain
  (non-workflow) run and assert `RunView.context_percent == Some(25)` and the footer renders
  `ctx 25%`. This exercises the *exact* existing `reduce.rs:535` arm with no TUI code change,
  proving reuse. (A parallel negative test: no such event ⇒ `context_percent == None` ⇒ `ctx —`.)

**No golden/wire tests change** — `EventBody`/`BudgetDimension` serialization is untouched.

---

## 10. Component Decomposition (plan-task seeds)

- **T1 — C1:** add `ModelConfig.context_tokens: Option<u64>` (+ serde default, parse tests).
- **T2 — C3:** `estimate_context_tokens` + `turn_item_text_len` pure fns + unit tests.
- **T3 — C4:** loop emits `BudgetWarning{Tokens}` (window `Some`), honesty (window `None` ⇒ none),
  emit-dedup; capturing-sink tests. Depends on T1, T2, and the T5 window source.
- **T4 — C2:** `FrameworkModelDriver.num_ctx` field + `from_registry` wiring + `next_step`
  `additional_properties` injection + body-boundary test. Depends on T1.
- **T5 — window plumbing:** `ModelDriver::context_window()` default-`None` trait method +
  `FrameworkModelDriver` impl (feeds T3). (Or the registry-lookup alternative — §7 decision 3.)
- **T6 — TUI reuse test:** the "dead footer comes alive" reducer/render test (no production TUI
  change). Depends on T3 conceptually (proves the contract), independent in code.
- **T7 — docs:** user-guide note on `context_tokens` in `models.toml` and the Ollama server-side
  `num_ctx` fallback (Modelfile / `OLLAMA_CONTEXT_LENGTH`).
- **T8 (deferred, NOT v1):** `compact_to_fit` auto-trim behind §7 decision 1.

---

## 11. Verified References

| Claim | Location |
|---|---|
| Client built with no options | `crates/runtime/src/models.rs:327-328` |
| `ModelConfig` has no window field | `crates/runtime/src/models.rs:65-85` |
| `context_tokens` is catalog-only, DISPLAY-ONLY | `crates/providers/src/model.rs:126` |
| Loop tracks only steps + wall-clock | `crates/runtime/src/agent.rs:84,89,963-989` |
| Transcript is whole-resent `Vec<TurnItem>` | `crates/runtime/src/agent.rs:934,2656` |
| Plain loop emits only `WallClock` warning | `crates/runtime/src/agent.rs:981-988` |
| `FrameworkModelDriver` build/next_step | `crates/runtime/src/agent.rs:2439,2455,2717,2730` |
| `BudgetWarning` event shape | `crates/protocol/src/events.rs:151-156` |
| `BudgetDimension::Tokens` variant | `crates/protocol/src/run.rs:235-242` |
| TUI `Tokens` reducer arm sets `context_percent` | `crates/tui/src/reduce.rs:535-546` |
| Footer renders `ctx` with `—` fallback | `crates/tui/src/render.rs:211-214,1273-1274` |
| `ChatOptions.additional_properties` exists | `agent-framework-core-0.1.1/src/types/options.rs:228` |
| Converter forwards `additional_properties` to body | `agent-framework-openai-0.1.1/src/convert.rs:294-296` |
| Workflow engine is the only current `Tokens` emitter | `crates/workflow/src/budget.rs:392` |
