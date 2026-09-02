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
    /// Every id the store holds, in stable order.
    ///
    /// The API Keys page needs this to show a key whose provider is no longer
    /// in the catalog: enumerating the CATALOG alone made a deleted custom
    /// provider's stored secret invisible, and so unremovable, while it sat on
    /// disk.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

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
        // One temp path per process, which is safe ONLY because every writer
        // goes through [`hold`] (directly or via [`update`]) and so cannot be
        // in this function concurrently with another writer here. A save taken
        // outside that hold would race a same-process sibling for this path;
        // that is the reason to route new writers through `update`, not to make
        // the name unique and leave the lost update in place.
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

/// Whether a critical section changed anything worth persisting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Save {
    Yes,
    No,
}

/// Serializes same-process auth writes. Paired with the file lock below, not a
/// substitute for it: threads in one process and separate processes are both
/// real, and the desktop shell and the CLI routinely run at once.
static AUTH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// An exclusive hold on `auth.json`, released when dropped.
///
/// Not `Send`: it is a synchronous hold for a synchronous critical section, and
/// carrying one across an await is exactly how a lock outlives its reason.
pub struct AuthHold {
    _process: std::sync::MutexGuard<'static, ()>,
    #[cfg(unix)]
    _file: std::fs::File,
}

/// Take the hold [`update`] takes, for a caller whose read-modify-write is
/// entangled with another file's commit and so cannot fit in a closure —
/// removing a model writes `models.toml` and `auth.json` together, with each
/// failure path undoing the other half, and that whole transaction has to be
/// one critical section.
pub fn hold(data_dir: &Path) -> std::io::Result<AuthHold> {
    let process = AUTH_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    std::fs::create_dir_all(data_dir)?;

    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(data_dir.join(".auth.lock"))?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)?;
        file
    };

    Ok(AuthHold {
        _process: process,
        #[cfg(unix)]
        _file: file,
    })
}

/// Load, mutate and save `auth.json` as ONE critical section, held against
/// every other writer on the machine.
///
/// Each write loads the whole map, changes one entry, and renames a full
/// snapshot back. Two writers — the shell saving one credential while the CLI
/// saves another — therefore both read the same map and whichever renamed last
/// silently discarded the other's key, with both reporting success. The atomic
/// rename makes each write whole; it does nothing about a lost update.
///
/// The hold is an advisory `flock` on `<data_dir>/.auth.lock`, so it spans
/// processes and is released even if one is killed. On Windows there is no
/// equivalent without a new dependency, and only the process lock applies: a
/// concurrent write from a second Windows process can still lose an entry.
///
/// The store is saved only when `mutate` returns [`Save::Yes`], so a no-op
/// removal creates no file, and an error returned from `mutate` writes nothing
/// and releases the lock.
pub fn update<T, E>(
    data_dir: &Path,
    mutate: impl FnOnce(&mut AuthStore) -> Result<(Save, T), E>,
) -> Result<T, E>
where
    E: From<std::io::Error>,
{
    let _hold = hold(data_dir).map_err(E::from)?;
    let mut store = AuthStore::load(data_dir).map_err(E::from)?;
    let (save, outcome) = mutate(&mut store)?;
    if save == Save::Yes {
        store.save(data_dir).map_err(E::from)?;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two writers ACCUMULATING different entries must lose none of them.
    ///
    /// Every save renames a whole map back, so without a hold spanning the load
    /// each writer builds on a stale snapshot and silently drops the keys the
    /// other added in between — while both report success. The sleep inside the
    /// closure widens the load-to-save window so the race is reliable rather
    /// than occasional; it is the window the hold exists to close.
    #[test]
    fn concurrent_writers_do_not_lose_each_others_keys() {
        const PER_WRITER: usize = 20;
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();

        std::thread::scope(|scope| {
            for writer in ["a", "b"] {
                let data_dir = data_dir.clone();
                scope.spawn(move || {
                    for index in 0..PER_WRITER {
                        update(&data_dir, |store| -> std::io::Result<_> {
                            store.set(format!("{writer}-{index}"), "key");
                            std::thread::sleep(std::time::Duration::from_millis(1));
                            Ok((Save::Yes, ()))
                        })
                        .expect("update");
                    }
                });
            }
        });

        let store = AuthStore::load(&data_dir).expect("load");
        let missing: Vec<String> = ["a", "b"]
            .into_iter()
            .flat_map(|writer| (0..PER_WRITER).map(move |index| format!("{writer}-{index}")))
            .filter(|id| store.get(id).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "{} of {} credentials were lost: {missing:?}",
            missing.len(),
            PER_WRITER * 2
        );
    }

    /// A no-op removal writes nothing, so a store that never existed is not
    /// created empty by asking to remove a key it never held.
    #[test]
    fn a_removal_that_changes_nothing_creates_no_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let removed = update(dir.path(), |store| -> std::io::Result<_> {
            let removed = store.remove("absent");
            Ok((if removed { Save::Yes } else { Save::No }, removed))
        })
        .expect("update");
        assert!(!removed, "nothing was there to remove");
        assert!(
            !dir.path().join("auth.json").exists(),
            "an empty store was written for a no-op"
        );
    }

    /// An error out of the closure writes nothing and releases the hold.
    #[test]
    fn a_refused_mutation_leaves_the_store_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        update(dir.path(), |store| -> std::io::Result<_> {
            store.set("seed", "value");
            Ok((Save::Yes, ()))
        })
        .expect("seed");

        let refused: std::io::Result<()> = update(dir.path(), |store| {
            store.set("seed", "clobbered");
            Err(std::io::Error::other("refused"))
        });
        assert!(refused.is_err(), "the closure's error must propagate");

        let store = AuthStore::load(dir.path()).expect("load");
        assert_eq!(
            store.get("seed"),
            Some("value"),
            "a refused mutation was written anyway"
        );
        // The hold released, so the next writer proceeds rather than deadlocking.
        update(dir.path(), |store| -> std::io::Result<_> {
            store.set("after", "ok");
            Ok((Save::Yes, ()))
        })
        .expect("the hold was not released");
    }

    /// The page that removes a key must be able to SEE one whose provider has
    /// been deleted from the catalog — otherwise the secret sits on disk with
    /// no way to remove it.
    #[test]
    fn ids_enumerates_entries_the_catalog_no_longer_knows() {
        let mut store = AuthStore::default();
        store.set("provider/deleted-custom", "key");
        store.set("some-model", "key");
        let ids: Vec<_> = store.ids().collect();
        assert!(
            ids.contains(&"provider/deleted-custom"),
            "a stored provider-wide key was not enumerable: {ids:?}"
        );
    }

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
