//! The key broker (PR C1, extended in D1).
//!
//! The Tavily API key is a secret and must never enter model context, logs, or
//! the database (Chapter 11). [`TavilyKey`] is therefore opaque, exactly like
//! [`crate::github::GitHubToken`]: it does not implement `Display` or
//! `Serialize`, its manual `Debug` prints only `<redacted>`, and the raw value
//! is reachable only through [`TavilyKey::expose`], documented for a single
//! caller — setting the `Authorization` header.
//!
//! **Why a direct env read and not the `codypendent_providers::credential`
//! seam** (`AuthMethod::ApiKey` + `credential_for`): that seam lives in the
//! providers crate — which this crate does not depend on — and is shaped for
//! MODEL clients (it yields a `ResolvedCredential` for chat-client auth
//! injection). This crate's own non-model credential precedent,
//! [`crate::github::GitHubToken::from_env`], reads `GITHUB_TOKEN` directly.
//! Mirroring the in-crate pattern is the more honest fit and adds no
//! dependency; the key is still resolved BY NAME at discovery time, never
//! stored anywhere or logged.
//!
//! **Why the `auth.json` lookup is a minimal direct read** (D1): the file's
//! owner is `codypendent_runtime::auth::AuthStore`, but this crate must NOT
//! depend on the runtime crate (the runtime depends on integrations — a
//! cycle). So the precedence lookup parses `<data_dir>/auth.json` as a bare
//! `serde_json::Value` and plucks the one reserved entry, duplicating none of
//! `AuthStore`'s behavior (no writes, no mode handling, no error semantics):
//! a missing OR corrupt file simply falls through to the env var, never an
//! error — web search is a best-effort boot-time feature and a corrupt
//! `auth.json` (which the runtime's own loaders surface elsewhere) must not
//! also disable the env fallback.

use std::borrow::Cow;
use std::fmt;
use std::path::Path;

use super::SearchError;

/// The environment variable the Tavily key is read from, by name.
pub const TAVILY_API_KEY_ENV: &str = "TAVILY_API_KEY";

/// The canonical environment variable reference for Tavily credentials.
pub const TAVILY_API_KEY_ENV_REF: &str = "env:TAVILY_API_KEY";

/// The reserved `auth.json` entry id for the Tavily key (D1). The `auth.json`
/// map is keyed by model id; the `integrations/` prefix keeps this entry
/// collision-proof against any model id a user could configure, so the
/// `/keys` flow and this discovery read the same slot the runtime's
/// model-key lookup never will.
pub const TAVILY_AUTH_ID: &str = "integrations/tavily";

/// An opaque Tavily API key. The inner value never appears in `Debug`, is not
/// serializable, and is reachable only via [`TavilyKey::expose`].
pub struct TavilyKey(KeySource);

#[derive(Clone, PartialEq, Eq)]
enum KeySource {
    Reference(String),
    Literal(String),
}

impl TavilyKey {
    /// Wrap a raw key value. Prefer [`TavilyKey::discover`] in production;
    /// this exists for tests and callers that already hold a vetted key.
    pub fn new(key: impl Into<String>) -> Self {
        Self(KeySource::Literal(key.into()))
    }

    /// Create a key from a brokered reference (e.g. `env:TAVILY_API_KEY`).
    pub fn from_reference(reference: impl Into<String>) -> Self {
        Self(KeySource::Reference(reference.into()))
    }

    /// Return the reference string if this key is backed by a reference.
    pub fn reference(&self) -> Option<&str> {
        match &self.0 {
            KeySource::Reference(r) => Some(r.as_str()),
            KeySource::Literal(_) => None,
        }
    }

    /// Borrow the raw key, solely to set the `Authorization` header.
    ///
    /// Callers MUST NOT log, store, serialize, or otherwise propagate the
    /// returned value. There is deliberately no other accessor.
    pub fn expose(&self) -> Cow<'_, str> {
        match &self.0 {
            KeySource::Literal(key) => Cow::Borrowed(key.as_str()),
            KeySource::Reference(reference) => {
                let var_name = reference.strip_prefix("env:").unwrap_or(reference);
                match std::env::var(var_name) {
                    Ok(value) if !value.trim().is_empty() => Cow::Owned(value.trim().to_string()),
                    _ => Cow::Borrowed(""),
                }
            }
        }
    }

    /// Discover the key, in precedence order (D1):
    ///
    /// 1. `<data_dir>/auth.json` under the reserved [`TAVILY_AUTH_ID`] entry —
    ///    the key the `/keys` TUI flow saves (a client cannot mutate the
    ///    daemon's env, so the file is how an in-app key reaches the daemon;
    ///    it takes effect on the next daemon boot).
    /// 2. The [`TAVILY_API_KEY_ENV`] environment variable as a brokered reference
    ///    ([`TAVILY_API_KEY_ENV_REF`]).
    ///
    /// Returns [`SearchError::MissingKey`] (naming the variable, never a
    /// value) if neither source yields a non-blank key.
    pub fn discover(data_dir: &Path) -> Result<TavilyKey, SearchError> {
        if let Some(key) = key_from_auth_json(data_dir) {
            return Ok(TavilyKey::new(key));
        }
        match std::env::var(TAVILY_API_KEY_ENV) {
            Ok(value) if !value.trim().is_empty() => {
                Ok(TavilyKey::from_reference(TAVILY_API_KEY_ENV_REF))
            }
            _ => Err(SearchError::MissingKey(TAVILY_API_KEY_ENV.to_string())),
        }
    }
}

/// The `auth.json` half of [`TavilyKey::discover`]: the reserved entry's
/// `api_key`, or `None`. Deliberately a minimal direct read (see the module
/// docs — this crate cannot depend on the runtime's `AuthStore`): a missing
/// file, an unreadable file, corrupt JSON, a missing entry, or a blank value
/// all yield `None` (fall through to the env var), never an error.
fn key_from_auth_json(data_dir: &Path) -> Option<String> {
    let bytes = std::fs::read(data_dir.join("auth.json")).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let key = value.get(TAVILY_AUTH_ID)?.get("api_key")?.as_str()?.trim();
    if key.is_empty() {
        None
    } else {
        Some(key.to_string())
    }
}

impl fmt::Debug for TavilyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TavilyKey(\"<redacted>\")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_reveals_the_secret() {
        let key = TavilyKey::new("tvly-SUPERSECRETVALUE");
        let rendered = format!("{key:?}");
        assert!(
            !rendered.contains("tvly-SUPERSECRETVALUE"),
            "Debug leaked the key value: {rendered}"
        );
        assert!(rendered.contains("redacted"));

        let ref_key = TavilyKey::from_reference("env:TAVILY_API_KEY");
        let rendered_ref = format!("{ref_key:?}");
        assert!(rendered_ref.contains("redacted"));
    }

    #[test]
    fn discover_prefers_auth_json_then_env_then_missing() {
        // Every discovery case lives in this ONE test: the process environment
        // is global mutable state, so two tests racing `set_var`/`remove_var`
        // on the SAME variable would flake (the `discovery.rs` convention).
        let dir = tempfile::tempdir().expect("tempdir");
        let auth_path = dir.path().join("auth.json");

        // 1. Both sources absent → MissingKey naming the env var (never a value).
        std::env::remove_var(TAVILY_API_KEY_ENV);
        match TavilyKey::discover(dir.path()) {
            Err(SearchError::MissingKey(var)) => assert_eq!(var, TAVILY_API_KEY_ENV),
            other => panic!("absent must be MissingKey, got {other:?}"),
        }

        // 2. Env fallback: no auth.json, env set → the env key reference that resolves.
        std::env::set_var(TAVILY_API_KEY_ENV, "tvly-env-key");
        let key = TavilyKey::discover(dir.path()).expect("env resolves");
        assert_eq!(key.reference(), Some("env:TAVILY_API_KEY"));
        assert_eq!(key.expose(), "tvly-env-key");

        // 3. A blank env value counts as absent.
        std::env::set_var(TAVILY_API_KEY_ENV, "   ");
        assert!(
            TavilyKey::discover(dir.path()).is_err(),
            "a blank env value counts as absent"
        );
        std::env::set_var(TAVILY_API_KEY_ENV, "tvly-env-key");

        // 4. File wins over env: an auth.json with the reserved entry is
        //    preferred even when the env var is also set.
        std::fs::write(
            &auth_path,
            format!("{{\"{TAVILY_AUTH_ID}\": {{\"api_key\": \"tvly-file-key\"}}}}"),
        )
        .expect("write auth.json");
        let key = TavilyKey::discover(dir.path()).expect("file resolves");
        assert_eq!(key.reference(), None);
        assert_eq!(key.expose(), "tvly-file-key", "the file beats the env var");

        // 5. A blank file entry falls through to the env var reference.
        std::fs::write(
            &auth_path,
            format!("{{\"{TAVILY_AUTH_ID}\": {{\"api_key\": \"  \"}}}}"),
        )
        .expect("write blank-entry auth.json");
        let key = TavilyKey::discover(dir.path()).expect("env fallback resolves");
        assert_eq!(key.reference(), Some("env:TAVILY_API_KEY"));
        assert_eq!(key.expose(), "tvly-env-key");

        // 6. A corrupt file falls through to the env var, never an error.
        std::fs::write(&auth_path, b"{ not json").expect("write corrupt auth.json");
        let key = TavilyKey::discover(dir.path()).expect("corrupt file falls through");
        assert_eq!(key.reference(), Some("env:TAVILY_API_KEY"));
        assert_eq!(key.expose(), "tvly-env-key");

        // 7. File present, env absent → the file key.
        std::fs::write(
            &auth_path,
            format!("{{\"{TAVILY_AUTH_ID}\": {{\"api_key\": \"tvly-file-key\"}}}}"),
        )
        .expect("write auth.json");
        std::env::remove_var(TAVILY_API_KEY_ENV);
        let key = TavilyKey::discover(dir.path()).expect("file resolves alone");
        assert_eq!(key.reference(), None);
        assert_eq!(key.expose(), "tvly-file-key");

        // 8. An auth.json without the reserved entry (e.g. only model keys)
        //    counts as absent.
        std::fs::write(&auth_path, "{\"groq/llama\": {\"api_key\": \"sk-other\"}}")
            .expect("write unrelated auth.json");
        assert!(
            TavilyKey::discover(dir.path()).is_err(),
            "an unrelated entry must not resolve"
        );

        // 9. Call-time resolution with rotated env variable
        std::env::set_var(TAVILY_API_KEY_ENV, "tvly-initial");
        let key = TavilyKey::from_reference("env:TAVILY_API_KEY");
        assert_eq!(key.expose(), "tvly-initial");
        std::env::set_var(TAVILY_API_KEY_ENV, "tvly-rotated");
        assert_eq!(key.expose(), "tvly-rotated");

        std::env::remove_var(TAVILY_API_KEY_ENV);
    }
}
