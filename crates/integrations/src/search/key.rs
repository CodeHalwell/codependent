//! The key broker (PR C1).
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

use std::fmt;

use super::SearchError;

/// The environment variable the Tavily key is read from, by name.
pub const TAVILY_API_KEY_ENV: &str = "TAVILY_API_KEY";

/// An opaque Tavily API key. The inner value never appears in `Debug`, is not
/// serializable, and is reachable only via [`TavilyKey::expose`].
pub struct TavilyKey(String);

impl TavilyKey {
    /// Wrap a raw key value. Prefer [`TavilyKey::discover`] in production;
    /// this exists for tests and callers that already hold a vetted key.
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// Borrow the raw key, solely to set the `Authorization` header.
    ///
    /// Callers MUST NOT log, store, serialize, or otherwise propagate the
    /// returned value. There is deliberately no other accessor.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Read the key from the [`TAVILY_API_KEY_ENV`] environment variable.
    /// Returns [`SearchError::MissingKey`] (naming the variable, never a
    /// value) if it is unset or blank.
    pub fn discover() -> Result<TavilyKey, SearchError> {
        match std::env::var(TAVILY_API_KEY_ENV) {
            Ok(value) if !value.trim().is_empty() => Ok(TavilyKey(value.trim().to_string())),
            _ => Err(SearchError::MissingKey(TAVILY_API_KEY_ENV.to_string())),
        }
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
    }

    #[test]
    fn discover_reads_tavily_api_key_by_name_present_and_absent() {
        // Both the present and absent cases live in this ONE test: the
        // process environment is global mutable state, so two tests racing
        // `set_var`/`remove_var` on the SAME variable would flake (the
        // `discovery.rs` convention).
        std::env::remove_var(TAVILY_API_KEY_ENV);
        match TavilyKey::discover() {
            Err(SearchError::MissingKey(var)) => assert_eq!(var, TAVILY_API_KEY_ENV),
            other => panic!("absent must be MissingKey, got {other:?}"),
        }

        std::env::set_var(TAVILY_API_KEY_ENV, "tvly-test-key");
        let key = TavilyKey::discover().expect("present resolves");
        assert_eq!(key.expose(), "tvly-test-key");

        std::env::set_var(TAVILY_API_KEY_ENV, "   ");
        assert!(
            TavilyKey::discover().is_err(),
            "a blank value counts as absent"
        );

        std::env::remove_var(TAVILY_API_KEY_ENV);
    }
}
