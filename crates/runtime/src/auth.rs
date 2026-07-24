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
