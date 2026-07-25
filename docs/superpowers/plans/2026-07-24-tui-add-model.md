# Add a Usable Model from the TUI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user pick a provider from the catalog in the TUI, name a model, enter its API key (for hosted providers), and have that model immediately usable — appearing in `/model` and running — with no manual file editing and no daemon restart.

**Architecture:** A new `AuthStore` module in `crates/runtime` owns `<data_dir>/auth.json` (a `0600` JSON secrets store keyed by model id, with a redacting `Debug`). The daemon's per-run `load_model_registry` loads it and `ModelRegistry::client_for` resolves a model's key in the order **auth.json → `api_key_env` → none** (purely additive). The TUI gains a pure-reducer add-model flow (pick provider in the `/provider` picker → model-name prompt → masked key prompt for hosted providers) that emits a new client-only `Intent::AddModel`; because the TUI does no I/O, the CLI harness applies it — appending a `[[model]]` to `models.toml` and writing the key to `auth.json` — then re-seeds the `/model` picker.

**Tech Stack:** Rust (edition 2021, rust-version 1.82); `serde`/`serde_json`/`toml`; `std::os::unix::fs::{OpenOptionsExt, PermissionsExt}` for the `0600` atomic write; `agent-framework-core` `ChatClient` + `agent-framework-openai` (feature `provider-openai`); the pure-reducer ratatui TUI (`crates/tui`) + its CLI harness (`crates/cli`); the `crates/providers` catalog.

## Global Constraints

- **Additive; no protocol wire change.** `Intent::AddModel` is a client→harness intent mapped to local file writes, **never** a daemon `CommandBody`; existing `models.toml` / `api_key_env` behavior is preserved exactly (an absent `auth.json` ⇒ behavior identical to today).
- **Pure-reducer TUI (no I/O).** The `crates/tui` crate never writes files or the key; the harness does all file writes; the entered key travels to the harness on the intent and is stored only by the harness.
- **Secret hygiene.** `auth.json` is mode `0600`, in the data dir (never the repo, never git, never logged), and the key value never appears in any `Debug`/error output — every secret-carrying type has a hand-written redacting `Debug` mirroring `codypendent_providers::credential::ResolvedCredential` (`crates/providers/src/credential.rs:29-41`).
- **Routing invariants untouched.** The classification hard-filter and T1/T7 cost honesty are unaffected — a hosted added model is still gated by data classification through the unchanged executor routing path (`crates/codypendentd/src/executor.rs:380-451`); this feature only writes config, it never bypasses routing.
- **Never edit/stage** `README.md`, `docs/cli-and-tui-user-guide.md`, `docs/docs/*`, `ROADMAP.md`, or anything under `.superpowers/`. Stage only changed files by explicit path; never `git add -A`. The working tree may carry unauthored dirty files — do not stage them.
- **Clippy runs on Linux CI:** `cargo clippy --workspace --all-targets --all-features -- -D warnings`. Gate any platform-only helper (`#[cfg(unix)]`) exactly like its sole caller.
- **Commit trailer on every commit:** `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- **Full gate green per task:** `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo test --workspace --all-features`.

---

## Design decisions

**Where `AuthStore` + `auth.json` live — `crates/runtime` (`src/auth.rs`).** The spec's open question is resolved to `runtime`, justified:
- The daemon resolves the key at client-build time inside `ModelRegistry::client_for` (`crates/runtime/src/models.rs`), and the per-run loader `load_model_registry` (`crates/codypendentd/src/executor.rs:939`) builds that registry. Both are `runtime`-adjacent, so the store lives next to `models.rs`.
- It must NOT live in `crates/providers`: that crate is a "network-free leaf … a secret is referenced by env-var NAME only; its value is … never stored here" (`crates/providers/src/model.rs:1-6`). `auth.json` stores secret *values*, which would violate that crate's stated invariant.
- `runtime` already depends on `serde`, `serde_json`, `toml`, and `tempfile` (dev) — no new dependency (`crates/runtime/Cargo.toml:31-44`).

**Keying `auth.json` by model id (not provider id).** One key per added model matches how `client_for` resolves per model. The JSON shape is the spec's `{ "<model_id>": { "api_key": "<key>" } }`; a provider-shared key is a noted follow-up.

**The key travels ON `Intent::AddModel`, wrapped in a redacting `SecretKey` newtype.** A pure reducer's only channel to the harness is `AppState::outbox: Vec<Intent>`, so the key must ride the intent. "Out-of-band" means it is *not a daemon command*: the harness intercepts `Intent::AddModel` locally and never maps it to a `CommandBody`. To keep secret hygiene, the key is a `SecretKey(String)` newtype with a hand-written `Debug` that prints `SecretKey(<redacted>)` (mirroring `ResolvedCredential`), so `Intent` keeps `#[derive(Debug, Clone, PartialEq)]` and no `{intent:?}` can leak the key. The in-flight key buffer in the `AddModelKey` overlay is the *same* `SecretKey` newtype, so `AppState`/`Overlay` `Debug` cannot leak the key mid-typing, and the render masks it on screen.

**The `models.toml` entry is always `provider = "openai-compatible"`.** That is the only wire adapter `ModelConfig`/`client_for` supports today (`crates/runtime/src/models.rs:224-241`); `base_url` comes from the catalog `Provider`. Pointing this adapter at a non-OpenAI endpoint is the user's responsibility and surfaces at run time (native-protocol entry is a documented non-goal). `models.toml` is written with default perms (it holds only env-var names + an empty `api_key_env`, no secret — unchanged from today); the secret lives in `auth.json` at `0600`.

**The add-model trigger is `Tab` in the `/provider` picker.** In `InputMode::Palette` printable keys filter the list, so the trigger must be non-printable; `Tab` is currently unmapped in `map_palette_key` (`crates/tui/src/input.rs:208-219`). This directly implements the spec's "the `/provider` picker's staged provider feeds step 1 … replacing the inert notice with a real action" (spec §4). The reducer gates `Action::BeginAddModel` to the provider picker (a no-op elsewhere).

**`ProviderCard` gains a typed `requires_key: bool`.** The reducer decides whether the key step is needed from a typed field (set by the harness from the catalog `AuthMethod`), never by parsing a rendered label. Adding the field ripples to four mechanical sites, all enumerated in Task 4.

## File structure

- **New:** `crates/runtime/src/auth.rs` (the `AuthStore`); **modify** `crates/runtime/src/lib.rs` (`pub mod auth;`) — Task 1.
- **Modify:** `crates/runtime/src/models.rs` (`ModelRegistry` gains an `AuthStore`; `client_for` precedence) — Task 2.
- **Modify:** `crates/codypendentd/src/executor.rs` (`load_model_registry` loads `auth.json`) — Task 2.
- **Modify:** `crates/tui/src/action.rs` (`SecretKey`, `Intent::AddModel`, `Action::BeginAddModel`), `crates/tui/src/lib.rs` (re-export `SecretKey`) — Tasks 3 & 4.
- **Modify:** `crates/cli/src/tui.rs` (`write_add_model`, drain-loop interception, `intent_to_command` arm), `crates/cli/Cargo.toml` (add `toml`) — Task 3.
- **Modify:** `crates/tui/src/state.rs` (two `Overlay` variants, `input_mode`, `ProviderCard.requires_key`), `crates/tui/src/reduce.rs` (the flow), `crates/tui/src/input.rs` (`Tab`), `crates/tui/src/render.rs` (masked prompt + arms), `crates/cli/src/tui.rs` (`load_provider_cards` sets `requires_key`) — Task 4.

---

## Task 1: `AuthStore` — the `auth.json` secrets store

Create the `0600` JSON secrets store over `<data_dir>/auth.json` in `crates/runtime`, with a hand-written redacting `Debug`.

**Files:**
- Create: `crates/runtime/src/auth.rs`
- Modify: `crates/runtime/src/lib.rs` (add `pub mod auth;` after `pub mod agent;`, line 8)
- Test: `crates/runtime/src/auth.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `AuthStore` (`Clone + Default`, redacting `Debug`) with `AuthStore::load(data_dir: &Path) -> AuthStore` (missing/unreadable/malformed ⇒ empty, never errors), `get(&self, model_id: &str) -> Option<&str>`, `set(&mut self, model_id: impl Into<String>, api_key: impl Into<String>)`, and `save(&self, data_dir: &Path) -> std::io::Result<()>` (atomic create-temp-at-`0600` + rename).

- [ ] **Step 1: Write the failing tests** — append to a new `crates/runtime/src/auth.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_save_load_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = AuthStore::default();
        store.set("groq/llama", "sk-abc");
        store.set("openai/gpt", "sk-xyz");
        store.save(dir.path()).expect("save");

        let loaded = AuthStore::load(dir.path());
        assert_eq!(loaded.get("groq/llama"), Some("sk-abc"));
        assert_eq!(loaded.get("openai/gpt"), Some("sk-xyz"));
        assert_eq!(loaded.get("absent"), None);
    }

    #[test]
    fn missing_file_loads_empty_and_never_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        // No auth.json exists in this fresh dir.
        let store = AuthStore::load(dir.path());
        assert_eq!(store.get("anything"), None);
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_the_file_at_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = AuthStore::default();
        store.set("m", "k");
        store.save(dir.path()).expect("save");

        let meta = std::fs::metadata(dir.path().join("auth.json")).expect("metadata");
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o600,
            "auth.json must be owner-only (0600)"
        );
    }

    #[test]
    fn debug_never_prints_the_key_value() {
        let mut store = AuthStore::default();
        store.set("groq/llama", "sk-super-secret");
        let dbg = format!("{store:?}");
        assert!(
            !dbg.contains("sk-super-secret"),
            "the key value must never appear in Debug: {dbg}"
        );
        assert!(dbg.contains("<redacted>"), "the value is redacted: {dbg}");
        assert!(
            dbg.contains("groq/llama"),
            "the non-secret model id stays visible: {dbg}"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p codypendent-runtime auth:: 2>&1 | head -30`
Expected: FAIL — `cannot find type AuthStore` / `module auth` unresolved.

- [ ] **Step 3: Write the implementation** — prepend to `crates/runtime/src/auth.rs` (above the test module):

```rust
//! `AuthStore` — the local secrets store over `<data_dir>/auth.json`.
//!
//! A deliberate, scoped departure from the "env-var-name-only" invariant, for
//! models a user ADDS from the TUI: their API key is persisted here, in the data
//! dir, at mode `0600` — never the repo, never git, never logged. The key value
//! never appears in `Debug`/errors (a hand-written redacting `Debug`, mirroring
//! `codypendent_providers::credential::ResolvedCredential`). Models configured
//! via `models.toml`'s `api_key_env` keep the env-var-name behavior unchanged;
//! an absent `auth.json` ⇒ behavior identical to today.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// One stored credential. Intentionally does NOT derive `Debug`, so nothing can
/// print the key through it.
#[derive(Clone, Serialize, Deserialize)]
struct AuthEntry {
    api_key: String,
}

/// The `auth.json` secrets store: a JSON map `{ "<model_id>": { "api_key": ".." } }`.
/// `BTreeMap` gives a stable on-disk key order. `#[serde(transparent)]` makes the
/// serialized form the bare map (the spec's shape), not `{ "entries": { .. } }`.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AuthStore {
    entries: BTreeMap<String, AuthEntry>,
}

// Hand-written to REDACT every key value (the map keys — model ids — are not
// secret and stay visible for diagnosis). A derived `Debug` would print the
// secret, so a stray `debug!("{store:?}")` anywhere downstream would leak it.
impl std::fmt::Debug for AuthStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.entries.keys().map(|id| (id, "<redacted>")))
            .finish()
    }
}

impl AuthStore {
    /// Load the store from `<data_dir>/auth.json`. A missing, unreadable, or
    /// malformed file yields an empty store — never an error (the store is a
    /// best-effort local convenience; a run falls back to `api_key_env`/none).
    #[must_use]
    pub fn load(data_dir: &Path) -> Self {
        std::fs::read(data_dir.join("auth.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// The stored API key for `model_id`, if any.
    #[must_use]
    pub fn get(&self, model_id: &str) -> Option<&str> {
        self.entries.get(model_id).map(|e| e.api_key.as_str())
    }

    /// Set (or replace) the API key for `model_id`.
    pub fn set(&mut self, model_id: impl Into<String>, api_key: impl Into<String>) {
        self.entries.insert(
            model_id.into(),
            AuthEntry {
                api_key: api_key.into(),
            },
        );
    }

    /// Persist to `<data_dir>/auth.json` at mode `0600`, atomically: write a
    /// temp file created `0600` (so the secret is never briefly world-readable in
    /// a create-then-chmod TOCTOU window), then rename it over the target — the
    /// renamed inode carries the temp's `0600`. Mirrors the daemon secret write
    /// (`crates/daemon/src/server.rs:2028-2044`).
    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join("auth.json");
        let tmp = data_dir.join("auth.json.tmp");
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        #[cfg(not(unix))]
        std::fs::write(&tmp, &bytes)?;

        std::fs::rename(&tmp, &path)?;

        // Defense in depth: if `path` somehow pre-existed with looser perms, the
        // rename already replaced its inode with the 0600 temp — assert 0600 anyway
        // (the spec: tighten looser-than-0600 perms on save).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}
```

- [ ] **Step 4: Register the module** — in `crates/runtime/src/lib.rs`, add `pub mod auth;` immediately after `pub mod agent;` (line 8):

```rust
pub mod agent;
pub mod auth;
pub mod bench;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p codypendent-runtime auth::`
Expected: PASS (4 tests on Unix, 3 on non-Unix).

- [ ] **Step 6: Gate + commit**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p codypendent-runtime auth::
git add crates/runtime/src/auth.rs crates/runtime/src/lib.rs
git commit -m "runtime: add the AuthStore auth.json secrets store (0600, redacted Debug)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 2: Daemon key resolution — `auth.json` → `api_key_env` → none

Give `ModelRegistry` an optional `AuthStore` and make `client_for` prefer an `auth.json` key over the env var. Wire `load_model_registry` to load `auth.json`. Purely additive: an empty store leaves every model resolving exactly as before.

**Files:**
- Modify: `crates/runtime/src/models.rs` (`ModelRegistry` struct ~189-192; `new` ~197-200; add `with_auth`; `client_for` OpenAiChat arm ~273-297)
- Modify: `crates/codypendentd/src/executor.rs` (`load_model_registry` ~939-956)
- Test: `crates/runtime/src/models.rs` `#[cfg(test)]`; `crates/codypendentd/src/executor.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `codypendent_runtime::auth::AuthStore` (Task 1).
- Produces: `ModelRegistry::with_auth(self, auth: AuthStore) -> Self`; `client_for` resolves keys `auth.json[id] → api_key_env → none`.

- [ ] **Step 1: Write the failing tests** — add to the `tests` module in `crates/runtime/src/models.rs` (after `client_for_allows_empty_api_key_env_for_local_endpoints`, ~line 619):

```rust
    #[cfg(feature = "provider-openai")]
    #[tokio::test]
    async fn auth_json_key_resolves_even_when_the_env_var_is_unset() {
        use crate::auth::AuthStore;
        // A unique, never-set var: env alone cannot satisfy this model.
        let var = "CODYPENDENT_TEST_MODELS_AUTHJSON_UNSET_5b2e";
        assert!(std::env::var(var).is_err(), "precondition: {var} unset");

        let id = model_id("groq/llama");
        let cfg = ModelConfig {
            id: id.clone(),
            provider: "openai-compatible".to_string(),
            base_url: "https://api.groq.com/openai/v1".to_string(),
            model: "llama-3.1-8b".to_string(),
            api_key_env: var.to_string(),
        };

        // Without an auth.json entry the env var is required and missing → error.
        let registry = ModelRegistry::new([cfg.clone()]);
        assert!(
            matches!(
                registry.client_for(&id).await,
                Err(ModelsError::MissingApiKeyEnv { .. })
            ),
            "with no auth.json entry the env path is unchanged (missing → error)"
        );

        // With an auth.json entry the key resolves from it — env never consulted.
        let mut auth = AuthStore::default();
        auth.set("groq/llama", "sk-from-authjson");
        let registry = ModelRegistry::new([cfg]).with_auth(auth);
        assert!(
            registry.client_for(&id).await.is_ok(),
            "an auth.json key must satisfy a model whose api_key_env is unset"
        );
    }

    #[cfg(feature = "provider-openai")]
    #[tokio::test]
    async fn local_model_needs_no_key_with_an_empty_auth_store() {
        use crate::auth::AuthStore;
        let id = model_id("ollama/qwen");
        let registry = ModelRegistry::new([ModelConfig {
            id: id.clone(),
            provider: "openai-compatible".to_string(),
            base_url: "http://localhost:11434/v1".to_string(),
            model: "qwen2.5-coder:14b".to_string(),
            api_key_env: String::new(),
        }])
        .with_auth(AuthStore::default());
        assert!(
            registry.client_for(&id).await.is_ok(),
            "a local model (empty api_key_env, empty auth.json) needs no key"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p codypendent-runtime --features provider-openai auth_json_key_resolves 2>&1 | head -30`
Expected: FAIL — `no method named with_auth found for struct ModelRegistry`.

- [ ] **Step 3: Add the `AuthStore` field + `with_auth`** — in `crates/runtime/src/models.rs`:

Add an import near the top (with the other non-gated `use`s, after line 33):

```rust
use crate::auth::AuthStore;
```

Change the `ModelRegistry` struct (lines 189-192) to carry the store:

```rust
/// The set of configured model profiles, keyed by [`ModelId`], plus the
/// resolved [`AuthStore`] (`auth.json`) so [`ModelRegistry::client_for`] can
/// prefer a stored key over the model's `api_key_env`. The store's own redacting
/// `Debug` keeps the derived `Debug` here from leaking a key.
#[derive(Debug, Clone, Default)]
pub struct ModelRegistry {
    configs: HashMap<ModelId, ModelConfig>,
    auth: AuthStore,
}
```

Change `ModelRegistry::new` (lines 197-200) to default the store, and add `with_auth` right after it:

```rust
    /// Build a registry from already-parsed configs. Later entries with a
    /// duplicate `id` overwrite earlier ones. The auth store starts empty (no
    /// `auth.json` keys), so every model resolves exactly as before until one is
    /// attached with [`with_auth`](Self::with_auth).
    pub fn new(configs: impl IntoIterator<Item = ModelConfig>) -> Self {
        let configs = configs.into_iter().map(|c| (c.id.clone(), c)).collect();
        Self {
            configs,
            auth: AuthStore::default(),
        }
    }

    /// Attach the resolved [`AuthStore`] (`auth.json`) so `client_for` prefers a
    /// stored key over the model's `api_key_env`. Additive: the default empty
    /// store leaves every model resolving exactly as before.
    #[must_use]
    pub fn with_auth(mut self, auth: AuthStore) -> Self {
        self.auth = auth;
        self
    }
```

- [ ] **Step 4: Make `client_for` prefer the `auth.json` key** — replace the `Protocol::OpenAiChat` arm (lines 273-297) with:

```rust
            Protocol::OpenAiChat => {
                // Key resolution precedence (additive): (a) an `auth.json` key for
                // this model id wins → (b) the model's `api_key_env` (today's
                // path) → (c) none. A model with no `auth.json` entry behaves
                // exactly as before. The stored key is moved straight into the
                // client and is never logged or retained by this function.
                let api_key = if let Some(key) = self.auth.get(id.0.as_str()) {
                    key.to_string()
                } else {
                    match credential_for(&auth).resolve().await {
                        Ok(ResolvedCredential::ApiKey { value, .. }) => value,
                        Ok(ResolvedCredential::None) => String::new(),
                        Err(CredentialError::MissingEnv { var }) => {
                            return Err(ModelsError::MissingApiKeyEnv {
                                model: id.clone(),
                                var,
                            });
                        }
                        // `CredentialError` is `#[non_exhaustive]`: this also
                        // catches `NotWired` (unreachable today) plus any future
                        // variant.
                        Err(other) => {
                            return Err(ModelsError::ProtocolNotWired {
                                model: id.clone(),
                                protocol: other.to_string(),
                            });
                        }
                    }
                };
                let client = OpenAIChatCompletionClient::new(api_key, cfg.model.clone())
                    .with_base_url(cfg.base_url.clone());
                Ok(Arc::new(client))
            }
```

- [ ] **Step 5: Run the runtime tests to verify they pass**

Run: `cargo test -p codypendent-runtime --features provider-openai models::`
Expected: PASS (the two new tests plus every existing `client_for`/`resolve_model` test — the existing tests use `ModelRegistry::new` with an empty store, so they are unchanged).

- [ ] **Step 6: Write the failing executor test** — add to the `tests` module in `crates/codypendentd/src/executor.rs` (after `first_run_emits_the_full_context_a_continuation_does_not`, ~line 1337):

```rust
    // NOT `#[cfg(feature = "provider-openai")]`: `codypendentd` pulls
    // `codypendent-runtime` with default features (provider-openai on), uses
    // `client_for`/`from_registry` unconditionally (executor.rs:454), and defines
    // no `provider-openai` feature of its own — so gating here would make the test
    // dead code.
    #[tokio::test]
    async fn load_model_registry_resolves_a_key_from_auth_json() {
        use codypendent_runtime::auth::AuthStore;
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        paths.ensure_directories().expect("directories");

        // A hosted model whose api_key_env is deliberately unset: env alone fails.
        std::fs::write(
            paths.data_dir.join("models.toml"),
            r#"
[[model]]
id = "groq/llama"
provider = "openai-compatible"
base_url = "https://api.groq.com/openai/v1"
model = "llama-3.1-8b"
api_key_env = "CODYPENDENT_TEST_EXECUTOR_AUTHJSON_UNSET_9c1d"
"#,
        )
        .expect("write models.toml");

        // auth.json carries the key, so the model must build.
        let mut auth = AuthStore::default();
        auth.set("groq/llama", "sk-authjson");
        auth.save(&paths.data_dir).expect("save auth.json");

        let (registry, _policy) = load_model_registry(&paths).expect("load registry");
        assert!(
            registry
                .client_for(&ModelId("groq/llama".to_string()))
                .await
                .is_ok(),
            "load_model_registry must attach auth.json so the key resolves"
        );
    }
```

- [ ] **Step 7: Run it to verify it fails**

Run: `cargo test -p codypendentd load_model_registry_resolves 2>&1 | head -30`
Expected: FAIL — the built registry has no `auth.json` attached, so `client_for` returns `MissingApiKeyEnv`.

- [ ] **Step 8: Load `auth.json` in `load_model_registry`** — in `crates/codypendentd/src/executor.rs`, replace the body of `load_model_registry` (lines 939-956) with:

```rust
pub(crate) fn load_model_registry(
    paths: &RuntimePaths,
) -> Result<(ModelRegistry, ModelPolicy), String> {
    let path = paths.data_dir.join("models.toml");
    if !path.exists() {
        return Err("no model configured (no models.toml)".to_string());
    }
    let configs = load_models(&path).map_err(|e| format!("invalid models.toml: {e}"))?;
    if configs.is_empty() {
        return Err("no model configured (models.toml is empty)".to_string());
    }
    let ids: Vec<_> = configs.iter().map(|c| c.id.clone()).collect();
    // Additive: also load `<data_dir>/auth.json` so a TUI-added model's stored key
    // resolves at client build (precedence: auth.json → api_key_env → none). An
    // absent file yields an empty store, leaving every model resolving as before.
    let auth = codypendent_runtime::auth::AuthStore::load(&paths.data_dir);
    let registry = ModelRegistry::new(configs).with_auth(auth);
    // Phase-1 policy: every mode tries every configured model, in file order,
    // until one connects. (The Phase-7 utility router replaces this.)
    let policy = ModelPolicy::new().with_default_candidates(ids);
    Ok((registry, policy))
}
```

- [ ] **Step 9: Run the executor test to verify it passes**

Run: `cargo test -p codypendentd load_model_registry_resolves`
Expected: PASS.

- [ ] **Step 10: Gate + commit**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
git add crates/runtime/src/models.rs crates/codypendentd/src/executor.rs
git commit -m "daemon: resolve a model key from auth.json (auth.json -> api_key_env -> none)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 3: CLI harness writes — `Intent::AddModel`, `SecretKey`, `write_add_model`

Introduce the client-only `Intent::AddModel` + the redacting `SecretKey`, and have the CLI harness apply it: append/update a `[[model]]` in `models.toml` and (for hosted) store the key in `auth.json`. Task 4 wires the reducer to emit it; here it is applied and tested directly via `write_add_model`.

**Files:**
- Modify: `crates/tui/src/action.rs` (add `SecretKey` + `Intent::AddModel`; add a `tests` module)
- Modify: `crates/tui/src/lib.rs` (re-export `SecretKey`, line 41)
- Modify: `crates/cli/src/tui.rs` (import `SecretKey`; add `write_add_model`; intercept in the drain loop ~614; `intent_to_command` arm ~816-883)
- Modify: `crates/cli/Cargo.toml` (add `toml = { workspace = true }`)
- Test: `crates/tui/src/action.rs` `#[cfg(test)]`; `crates/cli/src/tui.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `codypendent_runtime::auth::AuthStore` (Task 1); `codypendent_runtime::models::{load_models, ModelConfig}`; `codypendent_providers::Catalog`.
- Produces: `codypendent_tui::SecretKey(pub String)` (redacting `Debug`); `Intent::AddModel { display_id: String, provider_id: String, model: String, api_key: Option<SecretKey> }`; `write_add_model(paths: &RuntimePaths, display_id: &str, provider_id: &str, model: &str, api_key: Option<&str>) -> anyhow::Result<()>`.

- [ ] **Step 1: Write the failing `SecretKey`/`Intent` tests** — add to the end of `crates/tui/src/action.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_key_debug_is_redacted() {
        let k = SecretKey("sk-super-secret".to_string());
        let dbg = format!("{k:?}");
        assert!(!dbg.contains("sk-super-secret"), "key redacted in Debug: {dbg}");
        assert!(dbg.contains("<redacted>"));
    }

    #[test]
    fn add_model_intent_debug_redacts_the_key() {
        let intent = Intent::AddModel {
            display_id: "groq/llama".to_string(),
            provider_id: "groq".to_string(),
            model: "llama-3.1-8b".to_string(),
            api_key: Some(SecretKey("sk-secret".to_string())),
        };
        assert!(
            !format!("{intent:?}").contains("sk-secret"),
            "the key must never leak through the intent's Debug"
        );
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p codypendent-tui action:: 2>&1 | head -20`
Expected: FAIL — `cannot find type SecretKey` / no variant `AddModel`.

- [ ] **Step 3: Add `SecretKey` + `Intent::AddModel`** — in `crates/tui/src/action.rs`:

Add the `SecretKey` newtype just above the `Intent` enum (before line 195 `#[derive(Debug, Clone, PartialEq)]`):

```rust
/// A secret API key carried from the add-model flow to the CLI harness (the one
/// place that performs I/O), for a hosted provider. The `tui` crate never writes
/// it to disk; the harness stores it in `auth.json` (mode `0600`). `Debug` is
/// hand-written to REDACT the value — mirroring
/// `codypendent_providers::credential::ResolvedCredential` — so a stray
/// `{intent:?}` can never leak the key into a log or a snapshot. `PartialEq`/`Eq`
/// compare the inner value (so a test can assert on the exact key it supplied).
#[derive(Clone, PartialEq, Eq)]
pub struct SecretKey(pub String);

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretKey(<redacted>)")
    }
}
```

Add the `AddModel` variant at the end of the `Intent` enum (after `MutateDocument { .. }`, line 259, before the closing `}` on line 260):

```rust
    /// Add a usable model from the TUI (client-only — NOT a daemon command). The
    /// harness maps this to local `models.toml` + `auth.json` writes and never
    /// sends an envelope, so it is intercepted in the drain loop before
    /// `intent_to_command`. `display_id` is the `models.toml` id (the flow
    /// defaults it to `<provider>/<model>`); `provider_id` selects the catalog
    /// entry the harness reads `base_url` from; `model` is the provider-side model
    /// name. `api_key` is the entered key for a hosted provider (redacted in
    /// `Debug`), or `None` for a local/no-auth provider.
    AddModel {
        display_id: String,
        provider_id: String,
        model: String,
        api_key: Option<SecretKey>,
    },
```

- [ ] **Step 4: Re-export `SecretKey`** — in `crates/tui/src/lib.rs`, change line 41:

```rust
pub use action::{Action, Intent, SecretKey};
```

- [ ] **Step 5: Run the tui tests to verify they pass**

Run: `cargo test -p codypendent-tui action::`
Expected: PASS (both new tests).

- [ ] **Step 6: Add the `toml` dependency to the CLI** — in `crates/cli/Cargo.toml`, add to `[dependencies]` (after the `directories` line, line 54):

```toml
# Serializes an appended `[[model]]` back to `models.toml` in the add-model flow
# (`write_add_model`). Already a transitive dep via the config loaders.
toml = { workspace = true }
```

- [ ] **Step 7: Write the failing harness tests** — add to the `tests` module in `crates/cli/src/tui.rs` (it already exists at line 2194 and `use`s `super::*`):

```rust
    #[test]
    fn write_add_model_appends_an_entry_that_round_trips_through_load_models() {
        use codypendent_runtime::models::load_models;
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());

        // "groq" is a built-in catalog provider (hosted, api-key).
        write_add_model(&paths, "groq/llama", "groq", "llama-3.1-8b", Some("sk-secret"))
            .expect("write_add_model");

        let configs = load_models(&paths.data_dir.join("models.toml")).expect("parse");
        let entry = configs
            .iter()
            .find(|c| c.id.0 == "groq/llama")
            .expect("the entry is present");
        assert_eq!(entry.provider, "openai-compatible");
        assert_eq!(entry.model, "llama-3.1-8b");
        assert!(
            entry.base_url.contains("groq"),
            "base_url comes from the catalog: {}",
            entry.base_url
        );
        assert_eq!(entry.api_key_env, "", "the key lives in auth.json, not api_key_env");

        // The key landed in auth.json.
        let auth = codypendent_runtime::auth::AuthStore::load(&paths.data_dir);
        assert_eq!(auth.get("groq/llama"), Some("sk-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn write_add_model_stores_the_key_at_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        write_add_model(&paths, "groq/llama", "groq", "llama-3.1-8b", Some("sk-secret"))
            .expect("write");
        let meta = std::fs::metadata(paths.data_dir.join("auth.json")).expect("metadata");
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn write_add_model_for_a_local_provider_writes_no_key() {
        use codypendent_runtime::models::load_models;
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());

        // "ollama" is a built-in LOCAL provider (auth none) — no key entered.
        write_add_model(&paths, "ollama/qwen", "ollama", "qwen2.5-coder:14b", None)
            .expect("write");

        let configs = load_models(&paths.data_dir.join("models.toml")).expect("parse");
        assert!(configs.iter().any(|c| c.id.0 == "ollama/qwen"));
        assert!(
            !paths.data_dir.join("auth.json").exists(),
            "a local add writes no auth.json"
        );
    }

    #[test]
    fn write_add_model_updates_a_duplicate_display_id_in_place() {
        use codypendent_runtime::models::load_models;
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());

        write_add_model(&paths, "groq/llama", "groq", "llama-3.1-8b", Some("k1")).expect("write 1");
        write_add_model(&paths, "groq/llama", "groq", "llama-3.3-70b", Some("k2")).expect("write 2");

        let configs = load_models(&paths.data_dir.join("models.toml")).expect("parse");
        let matching: Vec<_> = configs.iter().filter(|c| c.id.0 == "groq/llama").collect();
        assert_eq!(matching.len(), 1, "a duplicate display id updates in place, never dupes");
        assert_eq!(matching[0].model, "llama-3.3-70b", "the entry took the new model");
        assert_eq!(
            codypendent_runtime::auth::AuthStore::load(&paths.data_dir).get("groq/llama"),
            Some("k2"),
            "the key updated too"
        );
    }
```

- [ ] **Step 8: Run to verify they fail**

Run: `cargo test -p codypendent-cli write_add_model 2>&1 | head -20`
Expected: FAIL — `cannot find function write_add_model`.

- [ ] **Step 9: Implement `write_add_model`** — add it in `crates/cli/src/tui.rs` (place it just after `intent_to_command`, ~line 883):

```rust
/// Apply an `Intent::AddModel` to the local config: append (or update in place) a
/// `[[model]]` entry in `<data_dir>/models.toml`, and, when a key was entered,
/// store it in `<data_dir>/auth.json` (mode `0600`). This is the harness's job
/// because the `tui` crate performs no I/O and never touches the key.
///
/// The written entry is always `provider = "openai-compatible"` (the only wire
/// adapter `ModelConfig`/`client_for` supports today); `base_url` is read from the
/// catalog provider (`<data_dir>/providers.toml` layered over the built-ins). A
/// duplicate `display_id` UPDATES its entry rather than duplicating it. Both files
/// are written atomically (temp + rename) so a concurrent daemon read never sees a
/// torn file.
fn write_add_model(
    paths: &RuntimePaths,
    display_id: &str,
    provider_id: &str,
    model: &str,
    api_key: Option<&str>,
) -> anyhow::Result<()> {
    use codypendent_providers::Catalog;
    use codypendent_runtime::auth::AuthStore;
    use codypendent_runtime::models::{load_models, ModelConfig};

    let data_dir = &paths.data_dir;
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("creating the data dir {}", data_dir.display()))?;

    // Resolve the catalog base_url for the chosen provider (built-ins layered with
    // any user providers.toml; a load failure falls back to the built-ins).
    let catalog = Catalog::load_with_user_overrides(&data_dir.join("providers.toml"))
        .unwrap_or_else(|_| Catalog::builtin());
    let base_url = catalog
        .get(provider_id)
        .and_then(|p| p.base_url.clone())
        .unwrap_or_default();

    // Read the existing models.toml (absent ⇒ start empty), drop any entry sharing
    // the new display id (update-in-place), then append the new one.
    let models_path = data_dir.join("models.toml");
    let mut configs = if models_path.exists() {
        load_models(&models_path).with_context(|| format!("reading {}", models_path.display()))?
    } else {
        Vec::new()
    };
    configs.retain(|c| c.id.0 != display_id);
    configs.push(ModelConfig {
        id: ModelId(display_id.to_string()),
        provider: "openai-compatible".to_string(),
        base_url,
        model: model.to_string(),
        api_key_env: String::new(),
    });

    // Serialize back to `[[model]]` tables and write atomically.
    #[derive(serde::Serialize)]
    struct ModelsToml {
        #[serde(rename = "model")]
        model: Vec<ModelConfig>,
    }
    let rendered =
        toml::to_string_pretty(&ModelsToml { model: configs }).context("serializing models.toml")?;
    let models_tmp = data_dir.join("models.toml.tmp");
    std::fs::write(&models_tmp, rendered.as_bytes())
        .with_context(|| format!("writing {}", models_tmp.display()))?;
    std::fs::rename(&models_tmp, &models_path)
        .with_context(|| format!("replacing {}", models_path.display()))?;

    // Store the key (hosted providers only) in auth.json at 0600.
    if let Some(key) = api_key {
        let mut auth = AuthStore::load(data_dir);
        auth.set(display_id, key);
        auth.save(data_dir)
            .with_context(|| format!("writing {}", data_dir.join("auth.json").display()))?;
    }
    Ok(())
}
```

- [ ] **Step 10: Intercept `AddModel` in the drain loop** — in `crates/cli/src/tui.rs`, at the top of the `for intent in state.drain_outbox()` loop body (line 614, before the `doc_intent_target` block on line 619), insert:

```rust
        for intent in state.drain_outbox() {
            // `AddModel` is the one client-only intent: apply it locally (models.toml
            // + auth.json) and skip the daemon-command mapping entirely.
            if let Intent::AddModel {
                display_id,
                provider_id,
                model,
                api_key,
            } = &intent
            {
                let key = api_key.as_ref().map(|k| k.0.as_str());
                match write_add_model(paths, display_id, provider_id, model, key) {
                    Ok(()) => {
                        // Re-seed the model picker so the new model shows immediately.
                        state.models = load_model_cards(paths).await;
                        reduce(state, Action::Notice(format!("added model {display_id}")));
                    }
                    Err(error) => {
                        reduce(state, Action::Notice(format!("could not add model: {error}")));
                    }
                }
                continue;
            }
```

(The existing `if let Some(document_id) = doc_intent_target(&intent)` block and the `command_envelope(...)` send that follow it are unchanged — a non-`AddModel` intent falls straight through to them.)

- [ ] **Step 11: Add the exhaustive `intent_to_command` arm** — in `crates/cli/src/tui.rs`, add to the `match intent` in `intent_to_command` (after the `Intent::MutateDocument` arm, ~line 882):

```rust
        // `AddModel` is a CLIENT-ONLY intent applied locally by the harness (see the
        // drain loop's interception); it never becomes a daemon command, so this
        // mapping is never reached.
        Intent::AddModel { .. } => unreachable!(
            "AddModel is applied locally by the harness (write_add_model), never sent to the daemon"
        ),
```

- [ ] **Step 12: Run the harness tests + build to verify they pass**

Run: `cargo test -p codypendent-cli write_add_model && cargo build -p codypendent-cli`
Expected: PASS (4 tests on Unix, 3 on non-Unix) and a clean build (the drain loop + `intent_to_command` compile).

- [ ] **Step 13: Gate + commit**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
git add crates/tui/src/action.rs crates/tui/src/lib.rs crates/cli/src/tui.rs crates/cli/Cargo.toml
git commit -m "cli: apply Intent::AddModel locally (models.toml + auth.json 0600 writes)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 4: TUI add-model flow — pick provider → model name → masked key → emit

Wire the pure reducer to drive the multi-step flow and emit `Intent::AddModel`, add the two prompt overlays (the key one masked), the `Tab` trigger, and the `ProviderCard.requires_key` field the flow branches on.

**Files:**
- Modify: `crates/tui/src/action.rs` (add `Action::BeginAddModel`, ~line 163 near `OpenPalette`)
- Modify: `crates/tui/src/state.rs` (add `ProviderCard.requires_key`, line 714; two `Overlay` variants after `ProviderPicker`, line 163; `input_mode` `Editing` arm, lines 953-955; `use` add `SecretKey`, line 15)
- Modify: `crates/tui/src/input.rs` (`Tab` in `map_palette_key`, line 217)
- Modify: `crates/tui/src/reduce.rs` (`use` add `SecretKey`, line 15; `Action::BeginAddModel` arm; `begin_add_model`; `edit_prompt` arms ~956-957; `submit_prompt` arms ~1164; `provider_card` test helper ~3447)
- Modify: `crates/tui/src/render.rs` (two `render_overlays` arms, line 980; new `render_masked_prompt`; two test `ProviderCard` literals, lines 4294 & 4301)
- Modify: `crates/cli/src/tui.rs` (`load_provider_cards` sets `requires_key`, line 1353)
- Test: `crates/tui/src/reduce.rs`, `crates/tui/src/input.rs`, `crates/tui/src/render.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `Intent::AddModel` + `SecretKey` (Task 3); `filter_providers` + `ProviderCard` (`crates/tui/src/state.rs`).
- Produces: `Action::BeginAddModel`; `Overlay::AddModelId { provider_id: String, requires_key: bool, buffer: String }`; `Overlay::AddModelKey { provider_id: String, model: String, buffer: SecretKey }`; `ProviderCard.requires_key: bool`.

- [ ] **Step 1: Add `requires_key` to `ProviderCard`** — in `crates/tui/src/state.rs`, add a field to `ProviderCard` (after `local: bool,`, line 713):

```rust
    /// On-device (Ollama/LM Studio/vLLM) vs. hosted.
    pub local: bool,
    /// Whether adding a model from this provider needs an API key (its first auth
    /// method is `ApiKey`). Drives the add-model flow's key step — a local/no-auth/
    /// ACP provider skips it. Set by the CLI harness from the catalog `AuthMethod`.
    pub requires_key: bool,
}
```

- [ ] **Step 2: Add the two overlay variants + `SecretKey` import** — in `crates/tui/src/state.rs`:

Change the `use crate::action::Intent;` (line 15) to also bring `SecretKey`:

```rust
use crate::action::{Intent, SecretKey};
```

Add the two variants at the end of the `Overlay` enum (after the `ProviderPicker { .. }` variant, before the closing `}` on line 164):

```rust
    /// Add-model flow, step 2 (text prompt): the provider-side model name, for the
    /// catalog provider chosen in step 1 (`provider_id`). `requires_key` was read
    /// from that provider's card so submit knows whether step 3 (the key prompt) is
    /// needed. On submit, a key-requiring provider advances to
    /// [`Overlay::AddModelKey`]; a local/no-auth one emits `Intent::AddModel`
    /// directly. A blank name is rejected (the prompt stays open).
    AddModelId {
        provider_id: String,
        requires_key: bool,
        buffer: String,
    },
    /// Add-model flow, step 3 (masked text prompt; key-requiring providers only):
    /// the API key for the chosen `provider_id` + `model`. `buffer` holds the key in
    /// a REDACTING newtype so it can never leak through `Debug`; the render masks it
    /// on screen. On submit, emits `Intent::AddModel` with the key handed to the
    /// harness (an empty key emits `api_key: None`).
    AddModelKey {
        provider_id: String,
        model: String,
        buffer: SecretKey,
    },
```

- [ ] **Step 3: Route the new overlays to `InputMode::Editing`** — in `crates/tui/src/state.rs` `input_mode` (lines 953-955), extend the `Editing` arm:

```rust
            Overlay::NewRun(_)
            | Overlay::Steering(_)
            | Overlay::DocEdit { .. }
            | Overlay::AddModelId { .. }
            | Overlay::AddModelKey { .. } => InputMode::Editing,
```

- [ ] **Step 4: Write the failing input test** — add to the `tests` module in `crates/tui/src/input.rs` (near the other `map_event` palette tests, ~line 400):

```rust
    #[test]
    fn tab_in_palette_mode_begins_add_model() {
        assert_eq!(
            map_event(&key(KeyCode::Tab), InputMode::Palette, W),
            Action::BeginAddModel
        );
    }
```

- [ ] **Step 5: Run to verify it fails**

Run: `cargo test -p codypendent-tui tab_in_palette_mode 2>&1 | head -20`
Expected: FAIL — no variant `Action::BeginAddModel`.

- [ ] **Step 6: Add `Action::BeginAddModel` + the `Tab` mapping**

In `crates/tui/src/action.rs`, add the variant right after `OpenPalette` (line 163):

```rust
    /// Toggle the command palette (`/`): a searchable list of every command.
    OpenPalette,
    /// Begin the add-model flow for the focused provider in the `/provider` picker
    /// (`Tab`): opens the model-name prompt (step 2). A no-op outside the provider
    /// picker.
    BeginAddModel,
```

In `crates/tui/src/input.rs` `map_palette_key` (line 217), add the `Tab` arm before `_ => Action::NoOp`:

```rust
        KeyCode::Char(c) if !ctrl(key) => Action::InputChar(c),
        KeyCode::Tab => Action::BeginAddModel,
        _ => Action::NoOp,
```

- [ ] **Step 7: Run the input test to verify it passes**

Run: `cargo test -p codypendent-tui tab_in_palette_mode`
Expected: PASS.

- [ ] **Step 8: Write the failing reducer tests** — add to the `tests` module in `crates/tui/src/reduce.rs`, right after `provider_picker_escape_closes_without_staging` (~line 3646). They reuse the existing `provider_card` helper and `open_provider_picker` (which focuses row 0):

```rust
    #[test]
    fn provider_picker_tab_begins_the_add_model_flow_for_the_focused_provider() {
        let mut s = AppState::new();
        s.providers = vec![
            provider_card("groq", "Groq", "openai-chat", "api-key: GROQ_API_KEY", false),
            provider_card("ollama", "Ollama (local)", "openai-chat", "none", true),
        ];
        open_provider_picker(&mut s); // focuses row 0 (groq)
        reduce(&mut s, Action::BeginAddModel);
        assert_eq!(
            s.overlay,
            Overlay::AddModelId {
                provider_id: "groq".to_owned(),
                requires_key: true,
                buffer: String::new(),
            }
        );
        assert_eq!(s.input_mode(), crate::state::InputMode::Editing);
    }

    #[test]
    fn add_model_hosted_flow_prompts_for_a_key_then_emits_the_intent() {
        let mut s = AppState::new();
        s.providers = vec![provider_card(
            "groq",
            "Groq",
            "openai-chat",
            "api-key: GROQ_API_KEY",
            false,
        )];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::BeginAddModel);
        for c in "llama-3.1-8b".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit); // step 2 → step 3 (masked key)
        assert_eq!(
            s.overlay,
            Overlay::AddModelKey {
                provider_id: "groq".to_owned(),
                model: "llama-3.1-8b".to_owned(),
                buffer: SecretKey(String::new()),
            }
        );
        assert!(s.outbox.is_empty(), "no intent until the key is entered");

        for c in "sk-secret".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit); // step 3 → emit
        assert_eq!(s.overlay, Overlay::None);
        assert_eq!(
            s.outbox,
            vec![Intent::AddModel {
                display_id: "groq/llama-3.1-8b".to_owned(),
                provider_id: "groq".to_owned(),
                model: "llama-3.1-8b".to_owned(),
                api_key: Some(SecretKey("sk-secret".to_owned())),
            }]
        );
    }

    #[test]
    fn add_model_local_provider_skips_the_key_step_and_emits_no_key() {
        let mut s = AppState::new();
        s.providers = vec![provider_card(
            "ollama",
            "Ollama (local)",
            "openai-chat",
            "none",
            true,
        )];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::BeginAddModel);
        assert!(matches!(
            s.overlay,
            Overlay::AddModelId { requires_key: false, .. }
        ));
        for c in "qwen2.5-coder".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputSubmit); // no key step → emit directly
        assert_eq!(s.overlay, Overlay::None);
        assert_eq!(
            s.outbox,
            vec![Intent::AddModel {
                display_id: "ollama/qwen2.5-coder".to_owned(),
                provider_id: "ollama".to_owned(),
                model: "qwen2.5-coder".to_owned(),
                api_key: None,
            }]
        );
    }

    #[test]
    fn add_model_rejects_a_blank_model_name() {
        let mut s = AppState::new();
        s.providers = vec![provider_card(
            "groq",
            "Groq",
            "openai-chat",
            "api-key: GROQ_API_KEY",
            false,
        )];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::BeginAddModel);
        reduce(&mut s, Action::InputSubmit); // empty buffer
        assert!(
            matches!(s.overlay, Overlay::AddModelId { .. }),
            "the prompt stays open on a blank name"
        );
        assert!(s.outbox.is_empty(), "no intent for a blank model name");
        assert!(s.notice.is_some(), "a notice explains the rejection");
    }

    #[test]
    fn add_model_escape_abandons_the_flow_without_emitting() {
        let mut s = AppState::new();
        s.providers = vec![provider_card(
            "groq",
            "Groq",
            "openai-chat",
            "api-key: GROQ_API_KEY",
            false,
        )];
        open_provider_picker(&mut s);
        reduce(&mut s, Action::BeginAddModel);
        for c in "x".chars() {
            reduce(&mut s, Action::InputChar(c));
        }
        reduce(&mut s, Action::InputCancel); // Esc on the model-name prompt
        assert_eq!(s.overlay, Overlay::None);
        assert!(s.outbox.is_empty());
    }
```

Also update the `provider_card` test helper (line 3447) to populate the new field from its `auth` arg — no call site changes (existing calls pass `"api-key: …"` / `"none"`):

```rust
    fn provider_card(
        id: &str,
        name: &str,
        protocol: &str,
        auth: &str,
        local: bool,
    ) -> crate::state::ProviderCard {
        crate::state::ProviderCard {
            id: id.to_owned(),
            name: name.to_owned(),
            protocol: protocol.to_owned(),
            auth: auth.to_owned(),
            local,
            requires_key: auth.starts_with("api-key"),
        }
    }
```

- [ ] **Step 9: Run to verify they fail**

Run: `cargo test -p codypendent-tui add_model 2>&1 | head -30`
Expected: FAIL — no `BeginAddModel` handler / no `AddModelId` handling in the reducer.

- [ ] **Step 10: Implement the reducer flow** — in `crates/tui/src/reduce.rs`:

Change the `use crate::action::{Action, Intent};` (line 15) to bring `SecretKey`:

```rust
use crate::action::{Action, Intent, SecretKey};
```

Add the dispatch arm in `reduce` (after `Action::OpenPalette => { .. }`, ~line 153):

```rust
        Action::BeginAddModel => begin_add_model(state),
```

Add the `begin_add_model` helper (place it just before `fn run_palette_command`, ~line 1173):

```rust
/// Begin the add-model flow (`Tab` in the `/provider` picker): open the model-name
/// prompt for the focused catalog provider, carrying its `requires_key` so the flow
/// knows whether a key step follows. A no-op outside the provider picker, or when
/// the filtered selection matches no provider (the same zero-match guard the
/// Enter-stage arm uses).
fn begin_add_model(state: &mut AppState) {
    let (provider_id, requires_key) = {
        let Overlay::ProviderPicker { query, selected } = &state.overlay else {
            return;
        };
        let Some(&idx) = filter_providers(&state.providers, query).get(*selected) else {
            return;
        };
        match state.providers.get(idx) {
            Some(card) => (card.id.clone(), card.requires_key),
            None => return,
        }
    };
    state.overlay = Overlay::AddModelId {
        provider_id,
        requires_key,
        buffer: String::new(),
    };
}
```

Add two arms to `edit_prompt` (in the `match &mut state.overlay`, after the `Overlay::DocEdit { buffer, .. } => edit(buffer),` arm, ~line 957):

```rust
        Overlay::AddModelId { buffer, .. } => edit(buffer),
        // The key buffer is a redacting newtype; edit its inner String.
        Overlay::AddModelKey { buffer, .. } => edit(&mut buffer.0),
```

Add two arms to `submit_prompt` (in the `match std::mem::take(&mut state.overlay)`, immediately before the final `other => state.overlay = other,` arm, ~line 1164):

```rust
        // Add-model flow step 2: a hosted provider advances to the masked key
        // prompt; a local one emits `Intent::AddModel` now. A blank name reopens
        // the prompt. `mem::take` left the overlay `None`.
        Overlay::AddModelId {
            provider_id,
            requires_key,
            buffer,
        } => {
            let model = buffer.trim().to_owned();
            if model.is_empty() {
                state.notice = Some(("model name cannot be blank".to_owned(), state.tick + 25));
                state.overlay = Overlay::AddModelId {
                    provider_id,
                    requires_key,
                    buffer: String::new(),
                };
            } else if requires_key {
                state.overlay = Overlay::AddModelKey {
                    provider_id,
                    model,
                    buffer: SecretKey(String::new()),
                };
            } else {
                let display_id = format!("{provider_id}/{model}");
                state.notice = Some((format!("adding model {display_id}"), state.tick + 25));
                state.outbox.push(Intent::AddModel {
                    display_id,
                    provider_id,
                    model,
                    api_key: None,
                });
            }
        }
        // Add-model flow step 3 (masked key): emit `Intent::AddModel` with the key
        // handed to the harness. An empty key emits `api_key: None`.
        Overlay::AddModelKey {
            provider_id,
            model,
            buffer,
        } => {
            let key = buffer.0.trim().to_owned();
            let display_id = format!("{provider_id}/{model}");
            let api_key = if key.is_empty() {
                None
            } else {
                Some(SecretKey(key))
            };
            state.notice = Some((format!("adding model {display_id}"), state.tick + 25));
            state.outbox.push(Intent::AddModel {
                display_id,
                provider_id,
                model,
                api_key,
            });
        }
```

(`Esc` needs no new arm: `input_cancel`'s default `_ => state.overlay = Overlay::None` already abandons both add overlays, dropping the `SecretKey` buffer.)

- [ ] **Step 11: Run the reducer tests to verify they pass**

Run: `cargo test -p codypendent-tui reduce::`
Expected: PASS (the five new tests plus every existing reducer test).

- [ ] **Step 12: Write the failing render test** — add to the `tests` module in `crates/tui/src/render.rs` (near the picker render tests, ~line 4250), using the existing `render_to_string` helper:

```rust
    #[test]
    fn masked_key_prompt_hides_the_typed_key() {
        let mut state = AppState::new();
        state.overlay = Overlay::AddModelKey {
            provider_id: "groq".to_owned(),
            model: "llama-3.1-8b".to_owned(),
            buffer: crate::action::SecretKey("sk-secret".to_owned()),
        };
        let text = render_to_string(&state, 80, 24);
        assert!(text.contains("API key"), "the key prompt title:\n{text}");
        assert!(text.contains('•'), "the key is masked with bullets:\n{text}");
        assert!(
            !text.contains("sk-secret"),
            "the raw key must never render:\n{text}"
        );
    }
```

- [ ] **Step 13: Run to verify it fails**

Run: `cargo test -p codypendent-tui masked_key_prompt 2>&1 | head -30`
Expected: FAIL — `render_overlays` has no arm for `Overlay::AddModelKey` (non-exhaustive match error), and `render_masked_prompt` does not exist.

- [ ] **Step 14: Add the render arms + masked prompt** — in `crates/tui/src/render.rs`:

Add two arms to `render_overlays` (in the `match &state.overlay`, after the `Overlay::DocEdit { buffer, .. } => { .. }` arm, ~line 992):

```rust
        Overlay::AddModelId { buffer, .. } => {
            render_prompt(frame, area, theme, "Model name (provider-side id)", buffer);
        }
        Overlay::AddModelKey { buffer, .. } => {
            render_masked_prompt(
                frame,
                area,
                theme,
                "API key (stored locally, mode 0600)",
                &buffer.0,
            );
        }
```

Add `render_masked_prompt` right after `render_prompt` (~line 2604):

```rust
/// Like [`render_prompt`] but renders the buffer MASKED (one `•` per character),
/// so a secret (an API key) is never shown on screen. The buffer is itself a
/// redacting newtype, so it also cannot leak through `Debug`.
fn render_masked_prompt(frame: &mut Frame, area: Rect, theme: &Theme, title: &str, buffer: &str) {
    let rect = centered_rect(70, 20, area);
    frame.render_widget(Clear, rect);
    let masked: String = "•".repeat(buffer.chars().count());
    let lines = vec![
        Line::styled(title, Style::default().fg(theme.text.heading)),
        Line::from(vec![
            Span::styled("› ", Style::default().fg(theme.focus.active)),
            Span::styled(masked, Style::default().fg(theme.text.primary)),
            Span::styled("█", Style::default().fg(theme.focus.active)),
        ]),
        Line::styled(
            "Enter to submit · Esc to cancel",
            Style::default().fg(theme.text.muted),
        ),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.focus.active))
        .style(
            Style::default()
                .bg(theme.surface.overlay)
                .fg(theme.text.primary),
        );
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
        rect,
    );
}
```

Update the two test `ProviderCard` literals in the render tests (lines 4294 and 4301) so they compile with the new field — add `requires_key: false,` after each literal's `local: …,` line. For example the first (~line 4294):

```rust
            ProviderCard {
                id: "groq".to_owned(),
                name: "Groq".to_owned(),
                protocol: "openai-chat".to_owned(),
                auth: "api-key: GROQ_API_KEY".to_owned(),
                local: false,
                requires_key: true,
            },
```

Apply the same field addition to the second literal at ~line 4301 (match its `local` value: a local provider gets `requires_key: false`).

- [ ] **Step 15: Set `requires_key` in the CLI provider projection** — in `crates/cli/src/tui.rs` `load_provider_cards` (the `ProviderCard { .. }` literal at line 1353), add the field (after `local: p.local,`, line 1370):

```rust
            local: p.local,
            // Adding a model from this provider needs a key iff its first auth
            // method is an API key (local/none/acp/cloud-iam/oauth skip the key
            // step). `AuthMethod` is already imported in this function.
            requires_key: matches!(p.auth.first(), Some(AuthMethod::ApiKey { .. })),
        })
```

- [ ] **Step 16: Run the render test + full tui/cli build to verify they pass**

Run: `cargo test -p codypendent-tui render:: && cargo build -p codypendent-cli`
Expected: PASS and a clean build (every `ProviderCard` construction now sets `requires_key`).

- [ ] **Step 17: Gate + commit**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
git add crates/tui/src/action.rs crates/tui/src/state.rs crates/tui/src/input.rs crates/tui/src/reduce.rs crates/tui/src/render.rs crates/cli/src/tui.rs
git commit -m "tui: add-model flow (pick provider -> name -> masked key -> Intent::AddModel)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Self-Review

**1. Spec coverage** (against `docs/superpowers/specs/2026-07-24-tui-add-model-design.md`):

| Spec item | Task |
| --- | --- |
| §1 `AuthStore` over `auth.json`; `load`(missing⇒empty)/`get`/`set`/`save`(0600, temp+rename); redacting `Debug` | Task 1 |
| §1 tests: round-trip, missing-file-empty, 0600 perms, Debug-redaction | Task 1 (Step 1) |
| §2 `load_model_registry` loads `auth.json`; key precedence auth.json → api_key_env → none; additive | Task 2 |
| §2 tests: auth.json satisfies unset-env, no-entry unchanged (`MissingApiKeyEnv`), local needs none, `load_model_registry` resolves | Task 2 (Steps 1, 6) |
| §3 `Intent::AddModel`; harness appends `[[model]]` (`openai-compatible`, catalog `base_url`, `api_key_env=""`); `AuthStore::set`+save for hosted; re-seed via `load_model_cards`; duplicate = update | Task 3 |
| §3 tests: round-trip through `load_models`, auth.json for hosted / skipped for local, duplicate updates | Task 3 (Step 7) |
| §3/§4 "+ Add model" from the `/provider` picker; pick provider → model-id prompt → (ApiKey only) masked key prompt → emit; pure-reducer; key to harness | Task 4 |
| §4 the picker's staged provider feeds step 1 (replaces the inert notice with a real action) | Task 4 (`BeginAddModel` on the provider picker) |
| §Testing: reduce steps (pick→id→key→confirm emits AddModel); local skips key; render shows masked prompt | Task 4 (Steps 8, 12) |
| Error handling: blank id rejected; duplicate overwrites; local no-key; bad/absent key surfaces at run | Task 3 (duplicate/local) + Task 4 (blank id) + Task 2 (unchanged `MissingApiKeyEnv` path) |

Every spec requirement maps to a task. The spec's noted non-goals (Cloud-IAM/OAuth/ACP-live/connectivity-probe/edit-remove UI) are intentionally out of scope.

**2. Placeholder scan:** No `TODO`/`TBD`/"similar to"/"handle errors" — every code step is complete. The single `unreachable!` in `intent_to_command` (Task 3 Step 11) is intentional and documented (the drain loop intercepts `AddModel` before this arm).

**3. Type consistency (cross-task):**
- `AuthStore` API — `load(&Path)`, `get(&str)->Option<&str>`, `set(impl Into<String>, impl Into<String>)`, `save(&Path)->io::Result<()>` — is defined in Task 1 and used identically in Tasks 2 & 3.
- `ModelRegistry::with_auth(self, AuthStore)->Self` — defined Task 2 Step 3, used in `load_model_registry` (Task 2 Step 8).
- `client_for` reads `self.auth.get(id.0.as_str())` — `id: &ModelId`, `ModelId(pub String)` (`crates/protocol/src/ids.rs:77`), so `.0.as_str()` is `&str` matching `AuthStore::get`.
- `SecretKey(pub String)` — defined Task 3, its public `.0` is read by the harness (`k.0.as_str()`, Task 3 Step 10) and by the reducer (`buffer.0`, Task 4 Step 10); `Intent::AddModel { …, api_key: Option<SecretKey> }` is constructed in the reducer (Task 4) and matched in the harness (Task 3) with the same field names.
- `Overlay::AddModelId { provider_id, requires_key, buffer }` / `AddModelKey { provider_id, model, buffer: SecretKey }` — field names identical across `state.rs`, `reduce.rs`, `render.rs`, and the reducer tests.
- `ProviderCard.requires_key: bool` — added in Task 4 Step 1; every construction site set: `load_provider_cards` (Task 4 Step 15), the `provider_card` test helper (Task 4 Step 8), and the two render-test literals (Task 4 Step 14). `render_overlays` and `input_mode` are exhaustive matches with no wildcard, so the compiler enforces the new `Overlay` arms are handled.
- `write_add_model(&RuntimePaths, &str, &str, &str, Option<&str>)` — defined Task 3 Step 9, called in the drain loop (Task 3 Step 10) and the harness tests (Task 3 Step 7).

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-07-24-tui-add-model.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints.

**Which approach?**
