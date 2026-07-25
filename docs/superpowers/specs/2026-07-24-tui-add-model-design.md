# Add a usable model from the TUI — design

**Date:** 2026-07-24 · **Status:** proposed · **Branch:** `claude/tui-add-model` (off main)

## Problem

The 42-provider catalog and the `/provider` picker (PR #27) are **browse-only**: the
picker stages `pending_provider` and shows *"provider staged — applies to your next run,"*
but **nothing consumes it**. The `/model` picker only pins a model that is **already in
`models.toml`**. The daemon resolves models from `models.toml` per run. So the only way to
add a model the daemon can actually run is **hand-editing `models.toml`** — there is no way
to "get more models in the TUI." This closes that loop.

## Goal

From the TUI: pick a provider from the catalog, name a model, enter its API key (for hosted
providers), and have that model **immediately usable** — it appears in `/model` and runs use
it — with **no manual file editing and no daemon restart** (the daemon reads `models.toml`
per run).

## Approach

An **"Add model"** flow that writes the daemon's real config: a `[[model]]` entry to
`models.toml` plus (for hosted providers) the API key to a new local **`auth.json`** secrets
store. The daemon resolves the key from `auth.json` at client-build time.

Key handling (user-approved): **enter the key in the TUI**, stored in `auth.json`
(`<data_dir>/auth.json`, mode `0600`, in the data dir — never the repo, never git, never
logged). This is a deliberate, scoped departure from the "env-var-name-only" invariant **for
TUI-added models**: those models' keys ARE persisted locally at 0600. Models configured via
`models.toml`'s `api_key_env` keep the env-var-name behavior unchanged.

## Architecture

### 1. Secrets store — `auth.json` (`crates/runtime` or `crates/providers`)
- A small module: `AuthStore` over `<data_dir>/auth.json`, a JSON map
  `{ "<model_id>": { "api_key": "<key>" } }` (keyed by the model id the entry is for).
- `load(data_dir) -> AuthStore` (missing file ⇒ empty, never an error); `set(model_id, key)`
  + `save` writes atomically with **mode 0600** (create-then-rename; verify perms). `get(model_id)`.
- Never logged; the key value never appears in `Debug`/errors (mirror the `ResolvedCredential`
  redaction already merged).

### 2. Daemon key resolution (`crates/codypendentd/src/executor.rs` `load_model_registry`;
`crates/runtime/src/models.rs` `client_for`)
- `load_model_registry` (which already reads `<data_dir>/models.toml`) also loads
  `auth.json`. When building a model's client, the key is resolved in this order:
  **(a)** `auth.json[model_id].api_key` if present → **(b)** the model's `api_key_env`
  environment variable (today's path) → **(c)** none (local). The registry entry carries the
  resolved key (or the client is built with it) so `client_for` uses it. This is additive —
  a model with no `auth.json` entry behaves exactly as today.

### 3. TUI add-model flow (`crates/tui` + `crates/cli`)
- The `/model` picker (or `/provider` picker) gains an **"+ Add model"** action.
- A small multi-step flow using the existing text-prompt overlay pattern
  (`Overlay::NewRun`/`Steering` are the reference):
  1. **Pick a provider** from the catalog (reuse the `/provider` picker; its known
     `base_url`, `protocol`, and `local` flag drive the rest).
  2. **Model id** — a text prompt for the provider-side model name (e.g. `gpt-5.1-codex`,
     `qwen2.5-coder:14b`), with a display id defaulting to `<provider>/<model>`.
  3. **API key** — only if the provider's auth is `ApiKey` (skipped for `local`/`none`/`acp`):
     a text prompt (masked in render).
- On confirm, the TUI emits a client-only `Intent::AddModel { display_id, provider_id,
  model, /* key handled out-of-band */ }`; because the key is a secret and the TUI does no
  I/O, the **CLI harness** performs the writes: append the `[[model]]` to `models.toml`
  (id, `provider = "openai-compatible"` from the catalog protocol, `base_url` from the
  catalog, `model`, and `api_key_env = ""` since the key lives in `auth.json`) and, if a key
  was entered, `AuthStore::set(display_id, key)` + save (0600). Then the harness re-seeds
  `state.models` (re-runs `load_model_cards`) so the new model shows in `/model` immediately.
- **Local providers** (Ollama/LM Studio/OpenAI-compatible with no auth): the key step is
  skipped; only the `models.toml` write happens — a one-step "add a local model."

### 4. Consume the staged provider
- The `/provider` picker's staged `pending_provider` now feeds step 1 of the add flow
  (selecting a provider → "add a model from it"), replacing the inert "applies to your next
  run" notice with a real action.

## Data flow

`/model` → "+ Add model" → pick catalog provider → model id → (hosted) key → TUI
`Intent::AddModel` (+ the key passed to the harness) → CLI harness writes `models.toml`
entry + `auth.json` key (0600) → re-seed `state.models` → the model appears in `/model`
→ user pins it (`pending_model` → `StartRun.model`, MP2) → the daemon `load_model_registry`
reads the new `models.toml` entry, resolves the key from `auth.json`, builds the client →
**the model runs.** No daemon restart (per-run registry load).

## Error handling / edge cases

- **Missing/duplicate model id**: reject a blank id; a duplicate display id overwrites its
  `models.toml` entry + `auth.json` key (an update), with a notice — never silently dupes.
- **Bad/absent key for a hosted model**: the write succeeds; the failure surfaces at run
  time as today's `MissingApiKeyEnv`-equivalent (now "no key for `<id>`"). Optionally a
  best-effort connectivity check is a follow-up, not v1.
- **`auth.json` perms**: if it exists with looser-than-0600 perms, tighten on save and warn.
- **Local model**: no key path; immediately usable (like the hand-added Ollama entry).
- **Back-compat**: existing `models.toml` + `api_key_env` models are untouched; `auth.json`
  absent ⇒ behavior identical to today.

## Testing

- `AuthStore`: round-trip; missing file ⇒ empty; `save` writes 0600; the key never appears
  in `Debug`.
- daemon: `load_model_registry` resolves a key from `auth.json` (precedence over env);
  a model with no auth.json entry is unchanged; a local model needs none.
- cli harness: `Intent::AddModel` appends a valid `[[model]]` (round-trips through
  `load_models`) and writes the key to `auth.json`; a duplicate id updates in place; a local
  provider writes no key.
- tui: the add flow's reduce steps (pick → model-id prompt → key prompt → confirm emits
  `Intent::AddModel`); a local provider skips the key step; render shows the masked key
  prompt.
- End-to-end (manual/gated): add a hosted model in the TUI, pin it, run — it works.

## Constraints

- Additive: no protocol wire change (`Intent::AddModel` is client→harness, mapped to file
  writes, not a daemon command); existing `models.toml` behavior preserved.
- Pure-reducer TUI (no I/O): the harness does all file writes; the key is passed to the
  harness, never written by the `tui` crate.
- Secret hygiene: `auth.json` 0600, in the data dir, gitignored-by-location, never logged,
  redacted in `Debug`.
- Preserve the routing classification hard-filter (a hosted added model is still gated by
  data classification) and T1/T7 cost honesty.

## Non-goals / follow-ups

- Cloud-IAM (SigV4/ADC/Entra) and OAuth key entry — still follow-ups (this is API-key +
  local only).
- Wiring the ACP client into a live run (separate follow-up from #27).
- A connectivity/validity check on the entered key (best-effort probe) — later.
- Editing/removing models from the TUI (v1 is add + overwrite; a manage/delete view later).

## Open questions

- **Where `auth.json` + `AuthStore` live**: `crates/providers` (next to the catalog/credential
  trait) vs `crates/runtime` (next to `models.rs`/`client_for`). The daemon reads it at
  client build, so `runtime` is the natural home; the plan pins it.
- **Keying `auth.json` by model-id vs provider-id**: model-id is simplest (one key per added
  model) and matches how `client_for` resolves per model; provider-id would share a key
  across a provider's models (fewer re-entries). Plan pins model-id for v1, notes provider-id
  as an easy extension.
