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
    /// Load the store from `<data_dir>/auth.json`. A *missing* file yields an
    /// empty store, never an error (the common case: no keys saved yet, or an
    /// install that has never used `auth.json`; the store is a best-effort
    /// local convenience and a run falls back to `api_key_env`/none). A file
    /// that exists but can't be read, or doesn't parse as JSON, returns
    /// `Err`: a corrupt store must surface rather than silently behaving like
    /// an empty one, which would otherwise look identical to "no keys
    /// configured" and mask the failure.
    pub fn load(data_dir: &Path) -> std::io::Result<Self> {
        let bytes = match std::fs::read(data_dir.join("auth.json")) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        serde_json::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
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

    /// Remove the stored API key for `model_id`, returning whether an entry
    /// was present (the `/keys` flow uses this to skip a pointless save when
    /// there was nothing to remove).
    pub fn remove(&mut self, model_id: &str) -> bool {
        self.entries.remove(model_id).is_some()
    }

    /// Persist to `<data_dir>/auth.json` at mode `0600`, atomically: write a
    /// temp file, explicitly tightened to `0600` immediately after opening —
    /// before any secret bytes are written — then rename it over the target.
    /// The explicit tighten matters even though the temp file is *also*
    /// opened with `mode(0o600)`: that `mode` argument only takes effect when
    /// `open()` CREATES a new inode, so a stale `auth.json.tmp` left behind at
    /// a looser mode (a crashed prior write, or one planted by another
    /// process) would otherwise be reused as-is, and the secret would land in
    /// a looser-than-0600 file — if only until the post-rename `chmod` below
    /// ran a moment later. With the explicit tighten, the key can never land
    /// in a looser-than-0600 file, even for an instant. Mirrors the daemon
    /// secret write (`crates/daemon/src/server.rs:2028-2044`).
    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join("auth.json");
        let tmp = data_dir.join(format!(".auth-{}.json.tmp", std::process::id()));
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            // `mode(0o600)` above is a no-op when `open()` reuses an existing
            // inode (see the doc comment above) — tighten explicitly, before
            // any secret bytes are written, so a pre-existing loose-mode temp
            // can never carry the secret at anything looser than 0600.
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
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
    fn remove_deletes_an_entry_and_reports_presence() {
        let mut store = AuthStore::default();
        store.set("groq/llama", "sk-abc");
        assert!(store.remove("groq/llama"), "an existing entry removes");
        assert_eq!(store.get("groq/llama"), None);
        assert!(
            !store.remove("groq/llama"),
            "a second remove reports nothing was present"
        );
        assert!(!store.remove("never-present"), "absent removes as false");
    }

    #[test]
    fn set_save_load_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = AuthStore::default();
        store.set("groq/llama", "sk-abc");
        store.set("openai/gpt", "sk-xyz");
        store.save(dir.path()).expect("save");

        let loaded = AuthStore::load(dir.path()).expect("load");
        assert_eq!(loaded.get("groq/llama"), Some("sk-abc"));
        assert_eq!(loaded.get("openai/gpt"), Some("sk-xyz"));
        assert_eq!(loaded.get("absent"), None);
    }

    #[test]
    fn missing_file_loads_empty_and_never_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        // No auth.json exists in this fresh dir.
        let store = AuthStore::load(dir.path()).expect("a missing file must not error");
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

    /// Pins the fix for a world-readable window: `OpenOptions::mode(0o600)`
    /// only takes effect when `open()` CREATES the inode, so a stale
    /// `auth.json.tmp` left at a looser mode (a crashed prior write, or one
    /// planted by another process) would otherwise be reused as-is and the
    /// secret would land in a looser-than-0600 file. `save` must still
    /// produce a `0600` `auth.json` (and a successful, readable-back write)
    /// even when the temp file pre-exists loose.
    #[cfg(unix)]
    #[test]
    fn save_tightens_a_preexisting_loose_perm_temp() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");

        // Seed a stale `auth.json.tmp` at a looser-than-0600 mode, simulating
        // a crashed previous write (or a file planted by another process).
        let tmp_path = dir.path().join("auth.json.tmp");
        std::fs::write(&tmp_path, b"stale").expect("seed stale tmp");
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o644))
            .expect("chmod stale tmp to 0644");

        let mut store = AuthStore::default();
        store.set("m", "sk-loose-window");
        store.save(dir.path()).expect("save");

        let meta = std::fs::metadata(dir.path().join("auth.json")).expect("metadata");
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o600,
            "auth.json must be owner-only (0600) even when auth.json.tmp pre-existed loose"
        );

        let loaded = AuthStore::load(dir.path()).expect("load");
        assert_eq!(loaded.get("m"), Some("sk-loose-window"));
    }

    /// A corrupt `auth.json` must surface as an `Err`, not be swallowed into
    /// an empty store: only a *missing* file means "no keys configured yet";
    /// a file that exists but fails to parse is a real failure the caller
    /// (and, transitively, the user) needs to know about rather than have
    /// silently masked as "no keys saved".
    #[test]
    fn load_rejects_corrupt_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("auth.json"), b"{ not json")
            .expect("seed corrupt auth.json");

        let result = AuthStore::load(dir.path());
        assert!(
            result.is_err(),
            "a corrupt auth.json must surface as an error, not a silently-empty store"
        );
    }
}
