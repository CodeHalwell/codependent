# Model discovery — browse a provider's models to add one — design

**Date:** 2026-07-26 · **Status:** proposed · **Branch:** `claude/tui-usability-fixes`

## Problem

The just-shipped add-model flow (`docs/superpowers/specs/2026-07-24-tui-add-model-design.md`)
makes the user **type** a provider-side model name after picking a provider
(`Overlay::AddModelId`, a free-text prompt). The user rarely remembers the exact id
(`qwen2.5-coder:14b`, `gpt-5.1-codex`, `llama-3.3-70b-versatile`), so this is a guessing game
against a 42-provider catalog.

Two concrete gaps:

1. **No discovery.** After picking a provider there is no way to *see* the models it serves.
2. **A dead `/provider` picker `Enter`.** `Enter` stages `pending_provider` and shows
   *"provider staged: {id} — applies to your next run,"* but **nothing consumes
   `pending_provider`** (confirmed: the field is written in `reduce.rs`'s `ProviderPicker`
   submit arm and read only by `render.rs`'s staged marker; no run/routing path reads it). The
   notice is false. Only `Tab` (`Action::BeginAddModel`) does anything real.

## Goal

After selecting a provider, **see that provider's available models and pick one** — no typing
the id. Query the provider's OpenAI-compatible model-list endpoint, show a filterable
pick-list, and add the chosen model through the existing `Intent::AddModel` path (so it is
immediately usable in `/model`, no daemon restart). Manual free-text entry **always remains
available** as a fallback.

Fold in the related fix: the `/provider` picker's `Enter` **begins the add-model flow** (like
`Tab`), the false "applies to your next run" notice is **removed**, and a truthful hint is
shown.

## Approach

Query the provider's **`/v1/models`** endpoint (the OpenAI-compatible list route; Ollama
serves the same shape) and present a pick-list. The TUI stays a pure, I/O-free reducer: it
emits a new **client-only** `Intent::QueryProviderModels`; the **CLI harness** performs the
HTTP GET (the one place the two worlds meet, exactly as it already does the file I/O for
`Intent::AddModel`); the result feeds back as an `Action`. On any failure the flow falls back
to today's free-text `AddModelId` prompt.

This is achievable **client-only — no daemon, protocol, or wire change** — because it reuses
two patterns already in the harness:

- **Client-only intent intercepted in the drain loop.** `Intent::AddModel` is already
  intercepted in `event_loop`'s `drain_outbox()` loop and applied locally (`write_add_model`),
  never mapped to a `CommandBody`. `Intent::QueryProviderModels` is intercepted the same way.
- **Background task → signal → `Action` feedback.** The socket reader task already forwards
  results into the loop as `ReaderSignal`s that the `select!` maps to `Action`s. The model
  query runs as a spawned task that sends a new `ReaderSignal::ProviderModels`, mapped to a new
  `Action`. (A background task — not a blocking `await` in the drain loop — so the UI stays
  responsive and the "querying" state is actually visible; see *Data flow*.)

## Target flow

1. `/provider` picker, `Enter` **or** `Tab` on a provider → begin add-model for it.
2. **Provider that can list models** (`Protocol::OpenAiChat`, has a `base_url`, first auth is
   `ApiKey` or `None`):
   - **Hosted** (`requires_key`): prompt for the API key (masked) **first** → harness
     `GET <base_url>/models` with the key → parse → **model pick-list** → pick → `Intent::AddModel`.
   - **Local / no-auth** (Ollama etc.): no key → harness `GET <base_url>/models` (no auth) →
     pick-list → `Intent::AddModel` with no key.
3. **Provider that cannot list** (Anthropic-native / Gemini-native / ACP / cloud-IAM / OAuth,
   or no `base_url`): **today's exact free-text flow** (`AddModelId` name → `AddModelKey` if a
   key is needed → `AddModel`), unchanged.
4. **Fallback** (query unreachable / non-200 / unparseable / auth rejected / empty list): fall
   back to the free-text name prompt **carrying any key already entered** (so the user is never
   asked for the key twice), with a notice explaining the list could not be fetched.

> **Correction vs. the brief.** The endpoint is `<base_url>/models`, **not**
> `<base_url>/v1/models`. Every OpenAI-compatible `base_url` in the catalog already carries its
> version segment (`https://api.openai.com/v1`, `https://api.groq.com/openai/v1`,
> `http://localhost:11434/v1`, and non-`/v1` ones like `https://api.z.ai/api/paas/v4`), and the
> chat client posts to `<base_url>/chat/completions`. The list route is its sibling
> `<base_url>/models`. Hardcoding `/v1/models` would double the version on every provider and be
> outright wrong for z.ai (`/v4`). Construction: `format!("{}/models", base_url.trim_end_matches('/'))`.

## HTTP stack the harness reuses

- **Client:** `reqwest` — the workspace HTTP stack (root `Cargo.toml`:
  `reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }`),
  the same crate `codypendent-integrations`' GitHub client uses. Add `reqwest = { workspace = true }`
  to `crates/cli/Cargo.toml` (the CLI does not depend on it directly yet; `serde`/`serde_json`
  are already CLI deps for parsing). The `codypendent-providers` crate stays network-free
  (leaf crate) — the GET lives in the harness, never in `tui` or `providers`.
- **Auth header convention:** identical to `credential.rs` / `models.rs::client_for` — the
  provider's first `AuthMethod::ApiKey { header, prefix, .. }` (defaults `Authorization` /
  `"Bearer "`; Anthropic-style would be `x-api-key` / `""`). Apply
  `req.header(header, format!("{prefix}{key}"))` **only** when a non-blank key is present;
  `AuthMethod::None` (or a blank key) sends no auth header. The one difference from `client_for`:
  the key is the value the **user just typed** (carried in the intent), not resolved from
  `auth.json`/env — it isn't in the environment yet.
- **Bounding:** `reqwest::Client::builder().timeout(Duration::from_secs(10))` so a hung
  endpoint can't wedge the query task (already off the UI thread; still bounded).

## Client-only shapes (settled)

**`crates/tui/src/action.rs` — `Intent`** (client-only; intercepted in the drain loop, never
mapped to a `CommandBody`, mirroring `Intent::AddModel`):

```rust
/// Query a provider's OpenAI-compatible model list (client-only — NOT a daemon
/// command). The harness GETs `<base_url>/models` with the provider's auth header
/// and feeds the result back as `Action::ProviderModelsLoaded` / `ProviderModelsFailed`.
/// `api_key` is the key the user entered for a hosted provider (redacted in Debug),
/// or `None` for a local/no-auth provider.
QueryProviderModels { provider_id: String, api_key: Option<SecretKey> },
```

**`crates/tui/src/action.rs` — `Action`** (fed by the harness after the GET; **no key** — the
key stays in the reducer's `AddModelQuerying` overlay across the round trip):

```rust
/// A provider's model list, fetched by the harness. Folds into the in-flight
/// `Overlay::AddModelQuerying` (matched by `provider_id`) → `Overlay::AddModelPick`.
ProviderModelsLoaded { provider_id: String, models: Vec<String> },
/// The model-list query failed (unreachable / non-200 / unparseable / auth rejected /
/// empty). `reason` is a human, key-free message. Folds the in-flight query into the
/// free-text `Overlay::AddModelId` fallback (carrying any already-entered key).
ProviderModelsFailed { provider_id: String, reason: String },
```

**`crates/tui/src/state.rs` — `Overlay`** (3 new; 1 changed; 1 removed):

```rust
// NEW — key-first masked prompt (hosted, can-list only), shown before the query.
// `buffer` is the redacting SecretKey newtype (masked in render). On submit:
// emit QueryProviderModels { provider_id, api_key } and open AddModelQuerying.
AddModelProviderKey { provider_id: String, buffer: SecretKey },

// NEW — transient "Fetching models from <provider>…" state while the harness GETs.
// Holds the key across the round trip so the Action need not carry it. Non-interactive
// except Esc (cancels; a late result is ignored via the provider_id/overlay match guard).
AddModelQuerying { provider_id: String, api_key: Option<SecretKey> },

// NEW — the model pick-list. Fuzzy-filterable like ModelPicker/ProviderPicker.
// Enter on a row → Intent::AddModel { display_id: "<provider>/<picked>", provider_id,
// model: <picked>, api_key }. Esc closes.
AddModelPick {
    provider_id: String,
    api_key: Option<SecretKey>,
    models: Vec<String>,
    query: String,
    selected: usize,
},

// CHANGED — the free-text fallback (KEPT, per the brief). Gains `api_key`:
//   None    = no key captured yet → today's rule (requires_key ? → AddModelKey : emit None).
//   Some(k) = key already captured (a can-list provider's failed query, possibly blank) →
//             emit AddModel directly with `k` (blank normalized to None), no re-prompt.
AddModelId { provider_id: String, requires_key: bool, api_key: Option<SecretKey>, buffer: String },

// KEPT (unchanged) — AddModelKey { provider_id, model, buffer }. The new key-first
// AddModelProviderKey supersedes it for can-list providers, but it is STILL USED by the
// cannot-list-but-needs-a-key path (Anthropic-native, Gemini-native), reached from
// AddModelId when `api_key == None && requires_key`. Not removed. (See Open questions #1.)
```

> After weighing it against the real branching, **`AddModelKey` is kept, not removed** — the
> cannot-list-but-needs-a-key path (Anthropic-native, Gemini-native) still uses today's
> name→key order. The new key-first prompt is a separate overlay (`AddModelProviderKey`)
> because the can-list path needs the key *before* the model name exists.

**`crates/tui/src/state.rs` — `ProviderCard`** gains one harness-computed bool:

```rust
/// Whether this provider can serve an OpenAI-compatible `/models` list: protocol is
/// OpenAiChat, a base_url is set, and the first auth method is ApiKey or None (or absent).
/// Set by the harness (`provider_can_list_models`), mirroring `requires_key`. Drives the
/// Enter/Tab branch: true → live pick-list; false → today's free-text flow.
pub can_list_models: bool,
```

**`crates/tui/src/state.rs`** — remove the now-dead `pending_provider: Option<String>` field
(nothing stages it after the Enter-arm rewrite; a staged marker that can never light up is
misleading). Touch points: `AppState` field + `new()`; `render.rs` staged-marker logic
(lines ~1431/1450/1500/1508); the render test that sets `state.pending_provider = Some("groq")`
(~line 4360). Add `filter_model_names(&[String], &str) -> Vec<usize>` next to `filter_models` /
`filter_providers` for the pick-list's substring filter.

## Architecture

### 1. Reducer (`crates/tui/src/reduce.rs`) — pure, no I/O

- **Shared entry `enter_add_model_flow(state, provider_id, requires_key, can_list_models)`**,
  called by both `Tab` and `Enter`:
  - `can_list_models && requires_key` → `overlay = AddModelProviderKey { provider_id, buffer: SecretKey(String::new()) }`.
  - `can_list_models && !requires_key` → push `Intent::QueryProviderModels { provider_id, api_key: None }`;
    `overlay = AddModelQuerying { provider_id, api_key: None }`.
  - `!can_list_models` → `overlay = AddModelId { provider_id, requires_key, api_key: None, buffer: String::new() }`
    (today's free-text entry, unchanged in behavior).
- **`begin_add_model` (Tab)** — rewrite to resolve the focused `ProviderCard` (same zero-match
  guard as today) and call `enter_add_model_flow`.
- **`submit_prompt` `Overlay::ProviderPicker` arm (Enter)** — rewrite: resolve the provider from
  the arm's own `query`/`selected` (as today, for the zero-match guard) and call
  `enter_add_model_flow`. **Delete** the `pending_provider` staging + the false notice.
  (`std::mem::take` already left `overlay = None`, so we can't call `begin_add_model`, which
  reads `state.overlay`; hence the shared helper takes the resolved fields directly.)
- **`submit_prompt` `Overlay::AddModelProviderKey` arm** — key entered: normalize blank→None,
  push `Intent::QueryProviderModels { provider_id, api_key }`, set
  `overlay = AddModelQuerying { provider_id, api_key }`.
- **`submit_prompt` `Overlay::AddModelPick` arm** — resolve the filtered selection
  (`filter_model_names(&models, &query).get(selected)`, same zero-match guard as the model
  picker); emit `Intent::AddModel { display_id: format!("{provider_id}/{model}"), provider_id, model, api_key }`;
  notice `"adding model {display_id}"`.
- **`submit_prompt` `Overlay::AddModelId` arm** — extend: if `api_key.is_some()` emit
  `AddModel` directly (blank inner key → `api_key: None`); else today's branch
  (`requires_key` → `AddModelKey`; else emit with `None`). Blank name still re-prompts.
- **New `Action` arms:**
  - `ProviderModelsLoaded { provider_id, models }` — if `overlay` is `AddModelQuerying` with a
    matching `provider_id`, move its `api_key` out and set
    `overlay = AddModelPick { provider_id, api_key, models, query: String::new(), selected: 0 }`.
    Otherwise ignore (the user dismissed / changed context — the race guard).
  - `ProviderModelsFailed { provider_id, reason }` — if `overlay` is `AddModelQuerying` with a
    matching `provider_id`, move its `api_key` into
    `overlay = AddModelId { provider_id, requires_key: api_key.is_some(), api_key: Some(key)/None, buffer: "" }`
    and set a notice `"couldn't fetch models ({reason}); type the model name"`. (For a hosted
    provider `api_key` is `Some`, so the fallback emits directly without re-prompting the key;
    for local it is `None`.) Otherwise ignore.
- **`edit_prompt`** — add arms: `AddModelProviderKey { buffer }` edits `&mut buffer.0` (the
  masked newtype, like `AddModelKey`); `AddModelPick { query, selected }` edits `query` and
  resets `selected = 0` (like `ModelPicker`); `AddModelId { buffer, .. }` unchanged.
- **`nav`** — add an `AddModelPick` arm stepping `selected` over `filter_model_names(&models, query)`
  (mirrors the `ModelPicker` arm).

### 2. Input (`crates/tui/src/input.rs`) — mapping via `input_mode()`

No new key-map functions. `state.input_mode()` (`state.rs`) classifies the new overlays:

- `AddModelProviderKey` → `InputMode::Editing` (masked text; `map_editing_key`: chars, Backspace,
  Enter=Submit, Esc=Cancel).
- `AddModelPick` → `InputMode::Palette` (filter + navigate; `map_palette_key` already gives
  Enter=Submit, Up/Down, chars, Esc=Cancel; its `Tab → BeginAddModel` is a harmless no-op here
  since `begin_add_model` only acts in the provider picker).
- `AddModelQuerying` → `InputMode::Normal` (non-interactive; `Esc`/`Dismiss` cancels the wait).

`Enter`/`Tab` in the provider picker are already `InputSubmit`/`BeginAddModel` — no input.rs
change for the picker itself; the behavior change is entirely in the reducer arms above.

### 3. Render (`crates/tui/src/render.rs`) — pure projection

- Overlay dispatch: add `AddModelProviderKey` (reuse `render_masked_prompt`, title e.g.
  `"API key for <provider> (used to list its models; stored locally 0600)"`), `AddModelQuerying`
  (a centered `"Fetching models from <provider>…  Esc to cancel"` box — the existing tick
  spinner animates it), and `AddModelPick` (a filterable list + detail, cloning
  `render_model_picker`'s shape over the `Vec<String>` model names). `AddModelId` /
  `AddModelKey` rendering unchanged.
- Provider picker hint fix: replace the body line *"Enter stages this provider for your next
  run"* with *"Enter or Tab — browse this provider's models to add one"*, and the footer
  *"↑/↓ select · Enter stage · Esc close"* with *"↑/↓ select · Enter/Tab add model · Esc close"*.
  Remove the `pending_provider` staged marker (dead after the rewrite).

### 4. CLI harness (`crates/cli/src/tui.rs`) — the only I/O

- **`ReaderSignal::ProviderModels { provider_id: String, result: Result<Vec<String>, String> }`** —
  a new signal variant. The `event_loop` `select!` maps it to
  `Action::ProviderModelsLoaded` (Ok) / `ProviderModelsFailed` (Err).
- **Query task.** `event_loop` receives a clone of the `event_tx: mpsc::Sender<ReaderSignal>`
  (in `run`, clone before moving one into `read_loop`; pass the other to `event_loop`). In the
  `drain_outbox()` loop, intercept `Intent::QueryProviderModels` (like `Intent::AddModel`):
  resolve the catalog `Provider` for `provider_id` (`Catalog::load_with_user_overrides` →
  `builtin` fallback, exactly as `write_add_model`), extract `base_url` and the first
  `AuthMethod::ApiKey`'s `header`/`prefix`, then `tokio::spawn` a task that runs
  `query_provider_models(...)` and sends `ReaderSignal::ProviderModels`. `continue` (no daemon
  command). The spawned task owns the key for the request and drops it; it is never sent back.
- **`query_provider_models(base_url, header, prefix, api_key) -> Result<Vec<String>, String>`:**
  `GET {base_url.trim_end_matches('/')}/models`, add the auth header iff a non-blank key is
  present, 10s timeout, require a 2xx (non-2xx → `Err` with status only, never the key), parse
  the body defensively, return the ids. An empty id list → `Err("provider returned no models")`
  so the reducer's `Failed` arm routes to the free-text fallback uniformly.
- **Response parsing** (OpenAI + Ollama share the shape
  `{ "object": "list", "data": [ { "id": "…" }, … ] }`):

  ```rust
  #[derive(serde::Deserialize)]
  struct ModelsResponse { #[serde(default)] data: Vec<ModelEntry> }
  #[derive(serde::Deserialize)]
  struct ModelEntry { #[serde(default)] id: String }
  ```

  Collect `data`, trim each `id`, skip blank/missing ids, dedup preserving provider order.
  (`Ollama-cloud`/`:cloud` deployments that expose a different route or shape simply fail the
  GET/parse → the fallback covers them; no special-casing.)
- **`provider_can_list_models(p: &Provider) -> bool`** — a tiny pure helper next to
  `provider_requires_key`, directly unit-testable:
  `matches!(p.protocol, Protocol::OpenAiChat) && p.base_url.as_deref().is_some_and(|u| !u.trim().is_empty()) && matches!(p.auth.first(), Some(AuthMethod::ApiKey { .. } | AuthMethod::None) | None)`.
  `load_provider_cards` sets `ProviderCard::can_list_models` from it.
- **`Cargo.toml`:** add `reqwest = { workspace = true }`.

## Data flow

```
/provider picker, Enter|Tab on a provider
        │  (reduce: enter_add_model_flow, branch on can_list_models / requires_key)
        ▼
 can-list + hosted ─▶ AddModelProviderKey (masked)  ─Enter─▶ Intent::QueryProviderModels{key}
 can-list + local  ───────────────────────────────────────▶ Intent::QueryProviderModels{None}
        │                                                          │  (drain loop intercepts)
        ▼                                                          ▼
 AddModelQuerying{provider_id, api_key}  ◀─ reduce sets it ──   harness resolves base_url+header
   "Fetching models…"  (UI stays live)                            tokio::spawn GET <base>/models
        │                                                          │  (10s timeout, auth header)
        │        ReaderSignal::ProviderModels{provider_id, result} │
        ▼                                                          ▼
   Action::ProviderModelsLoaded{provider_id, models}  ◀── Ok(ids) / Err(reason) ──▶ ProviderModelsFailed
        │  (reduce: move api_key out of AddModelQuerying)          │  (reduce: fallback)
        ▼                                                          ▼
   AddModelPick{provider_id, api_key, models}  ─Enter─▶      AddModelId{provider_id, api_key} (free-text)
        │                                                          │
        ▼                                                          ▼
   Intent::AddModel{display_id, provider_id, model, api_key}  ◀────┘
        │  (drain loop: write_add_model → models.toml + auth.json 0600; re-seed state.models)
        ▼
   the model appears in /model, immediately usable — no daemon restart
```

## Error handling / edge cases

- **Unreachable / timeout / non-2xx / TLS error / unparseable body** → `ProviderModelsFailed`
  with a short, **key-free** reason (e.g. `"HTTP 401"`, `"request timed out"`, `"connection
  refused"`) → free-text fallback (carrying any entered key) + notice. The user can **always**
  add a model manually.
- **Auth rejected (401/403)** is just a non-2xx failure → fallback; the user re-checks the key
  in the free-text step (the key they entered is carried, not lost).
- **Empty list (200 but `data: []`)** → treated as failure (`"provider returned no models"`) →
  fallback.
- **Cannot-list provider** (Anthropic-native/Gemini-native/ACP/cloud-IAM/OAuth, or no
  `base_url`) → never queried; today's free-text flow verbatim.
- **Dismiss during the query** (`Esc` on `AddModelQuerying`, or opening another overlay) → the
  `ProviderModelsLoaded`/`Failed` arms no-op because the overlay/`provider_id` no longer match
  (the race guard). The spawned task's result is dropped.
- **Blank key at `AddModelProviderKey`** → queried with no auth header (fine for a keyless
  OpenAI-compatible endpoint); on `AddModel` the blank normalizes to `api_key: None`
  (`write_add_model` already treats blank as no key, mode-0600 write skipped).
- **Duplicate `display_id`** → `write_add_model` updates the entry in place (unchanged).

## Testing

- **tui reducer** (pure, no I/O — assert on `outbox`/`overlay`):
  - Enter and Tab in the provider picker each call the same flow: can-list+hosted → `AddModelProviderKey`;
    can-list+local → `QueryProviderModels{None}` + `AddModelQuerying`; cannot-list → `AddModelId`.
  - `AddModelProviderKey` submit → `QueryProviderModels{Some(key)}` + `AddModelQuerying{key}`.
  - `ProviderModelsLoaded` → `AddModelPick` carrying the stashed key; a mismatched `provider_id`
    is ignored (race guard).
  - `ProviderModelsFailed` → `AddModelId` fallback carrying the key (hosted) / `None` (local) +
    notice; mismatch ignored.
  - `AddModelPick` submit → `Intent::AddModel { display_id: "<provider>/<model>", model, api_key }`;
    zero-match query stages nothing.
  - `AddModelId` submit with `api_key: Some` emits `AddModel` directly (no `AddModelKey`); with
    `None` + `requires_key` still routes to `AddModelKey` (cannot-list path preserved).
  - The `Intent`/`Action` `Debug` never contains the key (extend the existing redaction test to
    `QueryProviderModels`).
- **tui render:** `AddModelProviderKey` masks the key (bullets, like the existing
  `masked_key_prompt_hides_the_typed_key` test); `AddModelPick` lists and filters names; the
  provider picker hint reads "add model", not "stage".
- **cli harness:**
  - `provider_can_list_models`: true for OpenAiChat+base_url+ApiKey/None; false for
    Anthropic/Gemini/ACP/cloud-IAM/OAuth and for a missing base_url (unit test against the real
    `AuthMethod`/`Protocol` enums, like `provider_requires_key`).
  - `query_provider_models` parsing: OpenAI/Ollama `{data:[{id}]}` → ids; blanks/missing skipped;
    empty → `Err`; a non-2xx → `Err` with status and **no key**; the URL is `<base>/models`
    (assert no doubled `/v1`). (Parsing is a pure fn over a JSON string; the network GET itself
    is covered by a gated/manual integration check, matching how the repo treats live endpoints.)
- **End-to-end (manual/gated):** add a Groq model by picking from the live list; add an Ollama
  model with no key; force a failure (bad key) and confirm the free-text fallback keeps the key.

## Constraints

- **Client-only:** no protocol/daemon/wire change. `QueryProviderModels` and `AddModel` are both
  intercepted in the drain loop and never become `CommandBody`s (`intent_to_command` keeps its
  `unreachable!` for them).
- **Pure-reducer TUI (no I/O):** the HTTP GET happens only in the harness; `tui` and the
  network-free `providers` leaf crate never gain an HTTP dep.
- **Secret hygiene:** the query key is a `SecretKey` (redacted `Debug`, already tested). It
  crosses to the harness exactly once (in `QueryProviderModels`), is used for the request, and
  is **never** sent back in an `Action`, logged, put in an error/`reason` string, or rendered
  (the reducer keeps its own copy in `AddModelQuerying` for the round trip; render masks
  `AddModelProviderKey`). It reaches disk only via the existing `write_add_model` → `auth.json`
  (0600) path.
- **Responsive UI:** the query is a spawned task, not a blocking `await` in the drain loop, so
  the loop keeps ticking/redrawing and the "Fetching…" state is visible and cancelable.
- **No caching** — fetch per add. **No fuzzy/remote search** beyond the provider's own list.
- Preserve routing's data-classification hard-filter and T1/T7 cost honesty (unchanged —
  discovery only adds a `models.toml` entry through the existing path).

## Non-goals / follow-ups

- Cloud-IAM (SigV4/ADC/Entra) and OAuth model listing — those providers keep the free-text path
  (the query is API-key + no-auth only). Wiring their signing is a separate follow-up.
- Anthropic-native / Gemini-native list endpoints (different route + shape) — free-text for now;
  the `can_list_models` gate makes adding them later a localized change.
- Caching model lists, background refresh, or a "manage/remove models" view — later.
- A validity probe on the entered key beyond the list GET itself.

## Open questions

1. **Keep or retire `AddModelKey`.** This spec **keeps** it: the cannot-list-but-needs-a-key
   path (Anthropic-native, Gemini-native) still uses today's name→key order, reached from
   `AddModelId` when `api_key == None && requires_key`. Retiring it would force those providers
   through a key-first prompt too (larger change, and their model name still can't be listed).
   Confirm keeping it is acceptable (the alternative is a slightly more uniform key-first flow
   at the cost of removing just-shipped code).
2. **`AddModelQuerying` as an overlay vs. an `AppState` field.** It doubles as the in-flight
   key-holder and the "Fetching…" view. Modeling it as an overlay keeps the key and its view
   together and matches the other prompt overlays; a dedicated `AppState` field would be an
   alternative if we'd rather not grow the `Overlay` enum. Spec picks the overlay.
