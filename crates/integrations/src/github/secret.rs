//! The token broker (Phase 3 STEP 3.1).
//!
//! Personal mode uses the developer's own GitHub credential. That credential is
//! a secret and must never enter model context, logs, or the database
//! (Chapter 11). [`GitHubToken`] is therefore opaque: it does not implement
//! `Display` or `Serialize`, its manual `Debug` prints only `<redacted>`, and
//! the raw value is reachable only through [`GitHubToken::expose`], which is
//! documented for a single caller — setting the `Authorization` header.

use std::borrow::Cow;
use std::fmt;

use tokio::process::Command;

use crate::github::GitHubError;

/// The canonical environment variable reference for GitHub credentials.
pub const GITHUB_TOKEN_ENV_REF: &str = "env:GITHUB_TOKEN";

/// An opaque GitHub token. The inner value never appears in `Debug`, is not
/// serializable, and is reachable only via [`GitHubToken::expose`].
pub struct GitHubToken(TokenSource);

#[derive(Clone, PartialEq, Eq)]
enum TokenSource {
    Reference(String),
    Direct(String),
}

impl GitHubToken {
    /// Wrap a raw token value. Prefer [`GitHubToken::discover`] in production;
    /// this exists for tests and callers that already hold a vetted token.
    pub fn new(token: impl Into<String>) -> Self {
        Self(TokenSource::Direct(token.into()))
    }

    /// Create a token from a brokered reference (e.g. `env:GITHUB_TOKEN`).
    pub fn from_reference(reference: impl Into<String>) -> Self {
        Self(TokenSource::Reference(reference.into()))
    }

    /// Return the reference string if this token is backed by a reference.
    pub fn reference(&self) -> Option<&str> {
        match &self.0 {
            TokenSource::Reference(r) => Some(r.as_str()),
            TokenSource::Direct(_) => None,
        }
    }

    /// Borrow the raw token, solely to set the `Authorization` header.
    ///
    /// Callers MUST NOT log, store, serialize, or otherwise propagate the
    /// returned value. There is deliberately no other accessor.
    pub fn expose(&self) -> Cow<'_, str> {
        match &self.0 {
            TokenSource::Direct(token) => Cow::Borrowed(token.as_str()),
            TokenSource::Reference(reference) => {
                let var_name = reference.strip_prefix("env:").unwrap_or(reference);
                match std::env::var(var_name) {
                    Ok(value) if !value.trim().is_empty() => Cow::Owned(value.trim().to_string()),
                    _ => Cow::Borrowed(""),
                }
            }
        }
    }

    /// Create a token reference pointing to the `GITHUB_TOKEN` environment variable.
    /// Returns [`GitHubError::MissingToken`] if the variable is unset or empty.
    pub fn from_env() -> Result<GitHubToken, GitHubError> {
        let var_name = "GITHUB_TOKEN";
        match std::env::var(var_name) {
            Ok(value) if !value.trim().is_empty() => Ok(GitHubToken(TokenSource::Reference(
                GITHUB_TOKEN_ENV_REF.to_string(),
            ))),
            _ => Err(GitHubError::MissingToken(var_name.to_string())),
        }
    }

    /// Read the token by shelling out to `gh auth token`. Returns an error if
    /// the `gh` CLI is absent, exits non-zero, or prints nothing.
    pub async fn from_gh_cli() -> Result<GitHubToken, GitHubError> {
        let output = Command::new("gh").args(["auth", "token"]).output().await?;
        if !output.status.success() {
            return Err(GitHubError::MissingToken("gh auth token".to_string()));
        }
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if token.is_empty() {
            return Err(GitHubError::MissingToken("gh auth token".to_string()));
        }
        Ok(GitHubToken::new(token))
    }

    /// Discover a token the way the Phase 3 docs prescribe: prefer the `gh` CLI
    /// (so the daemon inherits the developer's existing session), then fall back
    /// to `GITHUB_TOKEN` as a brokered reference.
    pub async fn discover() -> Result<GitHubToken, GitHubError> {
        match Self::from_gh_cli().await {
            Ok(token) => Ok(token),
            Err(_) => Self::from_env(),
        }
    }
}

impl fmt::Debug for GitHubToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GitHubToken(\"<redacted>\")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_reveals_the_secret() {
        let token = GitHubToken::new("ghp_SUPERSECRETVALUE");
        let rendered = format!("{token:?}");
        assert!(
            !rendered.contains("ghp_SUPERSECRETVALUE"),
            "Debug leaked the token value: {rendered}"
        );
        assert!(rendered.contains("redacted"));

        let ref_token = GitHubToken::from_reference("env:GITHUB_TOKEN");
        let rendered_ref = format!("{ref_token:?}");
        assert!(rendered_ref.contains("redacted"));
    }

    #[test]
    fn expose_returns_the_raw_value() {
        let token = GitHubToken::new("ghp_abc");
        assert_eq!(token.expose(), "ghp_abc");
        assert!(token.reference().is_none());
    }

    #[test]
    fn from_env_returns_reference_and_resolves_at_call_time() {
        let var = "GITHUB_TOKEN";
        std::env::set_var(var, "ghp_env_secret");
        let token = GitHubToken::from_env().expect("from_env succeeds when set");
        assert_eq!(token.reference(), Some("env:GITHUB_TOKEN"));
        assert_eq!(token.expose(), "ghp_env_secret");

        // Verify resolution happens at call time by updating env
        std::env::set_var(var, "ghp_rotated_secret");
        assert_eq!(token.expose(), "ghp_rotated_secret");

        std::env::remove_var(var);
        assert!(GitHubToken::from_env().is_err());
    }

    #[test]
    fn custom_env_reference_resolves_at_call_time() {
        let var = "CODYPENDENT_TEST_GH_TOKEN_REF";
        let token = GitHubToken::from_reference(format!("env:{var}"));
        assert_eq!(token.reference(), Some(format!("env:{var}").as_str()));

        std::env::set_var(var, "ghp_custom_val");
        assert_eq!(token.expose(), "ghp_custom_val");

        std::env::remove_var(var);
        assert_eq!(token.expose(), "");
    }
}
