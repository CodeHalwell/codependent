//! The credential-provider seam. Resolves auth material from the environment at
//! CALL TIME and never stores it (Chapter 11 secrets invariant). The trait is
//! `async` so the follow-up CloudIam/OAuth impls (token refresh, request signing)
//! slot in without changing this seam; the `ApiKey` impl resolves synchronously.

use async_trait::async_trait;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::model::AuthMethod;

/// The concrete auth material a [`CredentialProvider`] resolved. Deliberately not
/// an HTTP `HeaderMap` — this leaf crate has no `http`/`reqwest` dep, and the
/// wired OpenAI-compatible path only needs the key string; a raw-HTTP adapter
/// (follow-up) can derive a header from `header`+`prefix`+`value`.
#[derive(Clone, PartialEq, Eq)]
pub enum ResolvedCredential {
    /// No credential (local endpoints).
    None,
    /// A resolved API key: inject `value` under `header` with `prefix`.
    ApiKey {
        header: String,
        prefix: String,
        value: String,
    },
    /// A short-lived delegated bearer token. Its value is always redacted.
    BearerToken {
        value: String,
        expires_at: SystemTime,
    },
}

// `Debug` is hand-written to REDACT the key `value` — a derived `Debug` would
// print the secret, so a stray `debug!("{cred:?}")` anywhere downstream would
// leak it into logs. The header/prefix (non-secret) stay visible for diagnosis.
impl std::fmt::Debug for ResolvedCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.write_str("ResolvedCredential::None"),
            Self::ApiKey { header, prefix, .. } => f
                .debug_struct("ResolvedCredential::ApiKey")
                .field("header", header)
                .field("prefix", prefix)
                .field("value", &"<redacted>")
                .finish(),
            Self::BearerToken { expires_at, .. } => f
                .debug_struct("ResolvedCredential::BearerToken")
                .field("value", &"<redacted>")
                .field("expires_at", expires_at)
                .finish(),
        }
    }
}

/// A failure resolving a credential.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CredentialError {
    /// None of the configured env-var NAMEs is set. Names the first, per the rule
    /// that secrets are identified (never guessed) in error output.
    #[error("environment variable `{var}` for the API key is not set")]
    MissingEnv { var: String },
    /// A credential method whose signing/refresh is a follow-up (CloudIam/OAuth).
    #[error("unsupported credential configuration: {reason}")]
    UnsupportedConfiguration { reason: String },
    #[error("delegated token provider failed ({class})")]
    TokenProvider { class: &'static str },
}

/// Resolves the auth material to inject for one request, reading secrets from the
/// environment at call time.
#[async_trait]
pub trait CredentialProvider: Send + Sync {
    async fn resolve(&self) -> Result<ResolvedCredential, CredentialError>;
}

/// Request metadata passed to an injected, non-interactive token source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRequest {
    pub kind: &'static str,
    pub variant_or_client_id: String,
    pub scopes: Vec<String>,
}

/// A token and its absolute expiry. Debug intentionally never exposes value.
#[derive(Clone, PartialEq, Eq)]
pub struct DelegatedToken {
    value: String,
    expires_at: SystemTime,
}

impl DelegatedToken {
    pub fn new(value: impl Into<String>, expires_at: SystemTime) -> Self {
        Self {
            value: value.into(),
            expires_at,
        }
    }
}

impl std::fmt::Debug for DelegatedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DelegatedToken")
            .field("value", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[async_trait]
pub trait TokenProvider: Send + Sync {
    async fn token(&self, request: &TokenRequest) -> Result<DelegatedToken, CredentialError>;
}

const REFRESH_SKEW: Duration = Duration::from_secs(30);

struct DelegatedCredential {
    request: TokenRequest,
    provider: Arc<dyn TokenProvider>,
    cached: tokio::sync::Mutex<Option<DelegatedToken>>,
}

impl DelegatedCredential {
    async fn resolve(&self) -> Result<ResolvedCredential, CredentialError> {
        // Hold the async mutex across refresh. This deliberately serializes the
        // slow path and rechecks the cache after waiting, so a refresh is
        // single-flight rather than one provider call per concurrent waiter.
        let mut cached = self.cached.lock().await;
        if let Some(token) = cached
            .as_ref()
            .filter(|token| {
                token
                    .expires_at
                    .duration_since(SystemTime::now())
                    .unwrap_or_default()
                    > REFRESH_SKEW
            })
            .cloned()
        {
            return Ok(ResolvedCredential::BearerToken {
                value: token.value,
                expires_at: token.expires_at,
            });
        }
        let token = self.provider.token(&self.request).await.map_err(|_| {
            // A TokenProvider is untrusted and may put credentials in its raw
            // error. Preserve classification, never its arbitrary string.
            CredentialError::TokenProvider {
                class: "acquisition",
            }
        })?;
        if token.value.trim().is_empty() || token.expires_at <= SystemTime::now() {
            return Err(CredentialError::TokenProvider {
                class: "invalid-token",
            });
        }
        *cached = Some(token.clone());
        Ok(ResolvedCredential::BearerToken {
            value: token.value,
            expires_at: token.expires_at,
        })
    }
}

/// Helper to resolve a secret reference at call time. Supports `env:<VAR_NAME>`
/// references (and bare `<VAR_NAME>` for backwards compatibility).
pub fn resolve_secret_reference(reference: &str) -> Option<String> {
    let var_name = reference.strip_prefix("env:").unwrap_or(reference);
    std::env::var(var_name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The wired API-key credential: the first `env` reference or name that is set wins.
pub struct ApiKeyCredential {
    pub env: Vec<String>,
    pub header: String,
    pub prefix: String,
}

#[async_trait]
impl CredentialProvider for ApiKeyCredential {
    async fn resolve(&self) -> Result<ResolvedCredential, CredentialError> {
        for reference in &self.env {
            if let Some(value) = resolve_secret_reference(reference) {
                return Ok(ResolvedCredential::ApiKey {
                    header: self.header.clone(),
                    prefix: self.prefix.clone(),
                    value,
                });
            }
        }
        match self.env.first() {
            Some(first) => Err(CredentialError::MissingEnv { var: first.clone() }),
            None => Ok(ResolvedCredential::None),
        }
    }
}

/// No-auth credential (local endpoints; ACP carries no HTTP credential).
pub struct NoneCredential;

#[async_trait]
impl CredentialProvider for NoneCredential {
    async fn resolve(&self) -> Result<ResolvedCredential, CredentialError> {
        Ok(ResolvedCredential::None)
    }
}

/// Trait-shaped stub: cloud-IAM signing/refresh is a follow-up.
pub struct CloudIamCredential(DelegatedCredential);

impl CloudIamCredential {
    pub fn new(
        variant: impl Into<String>,
        scopes: Vec<String>,
        provider: Arc<dyn TokenProvider>,
    ) -> Self {
        Self(DelegatedCredential {
            request: TokenRequest {
                kind: "cloud-iam",
                variant_or_client_id: variant.into(),
                scopes,
            },
            provider,
            cached: tokio::sync::Mutex::new(None),
        })
    }
}

#[async_trait]
impl CredentialProvider for CloudIamCredential {
    async fn resolve(&self) -> Result<ResolvedCredential, CredentialError> {
        self.0.resolve().await
    }
}

/// Trait-shaped stub: subscription OAuth is reserved and not wired (ToS-gated).
pub struct OAuthCredential(DelegatedCredential);

impl OAuthCredential {
    pub fn new(
        client_id: impl Into<String>,
        scopes: Vec<String>,
        provider: Arc<dyn TokenProvider>,
    ) -> Self {
        Self(DelegatedCredential {
            request: TokenRequest {
                kind: "oauth",
                variant_or_client_id: client_id.into(),
                scopes,
            },
            provider,
            cached: tokio::sync::Mutex::new(None),
        })
    }
}

#[async_trait]
impl CredentialProvider for OAuthCredential {
    async fn resolve(&self) -> Result<ResolvedCredential, CredentialError> {
        self.0.resolve().await
    }
}

/// Build the credential provider for an auth method (a provider offers its methods
/// in preference order; the caller picks one — typically the first).
pub fn credential_for(method: &AuthMethod) -> Box<dyn CredentialProvider> {
    match method {
        AuthMethod::None | AuthMethod::Acp { .. } => Box::new(NoneCredential),
        AuthMethod::ApiKey {
            env,
            header,
            prefix,
        } => Box::new(ApiKeyCredential {
            env: env.clone(),
            header: header.clone(),
            prefix: prefix.clone(),
        }),
        AuthMethod::CloudIam { .. } => Box::new(UnsupportedCredential("cloud IAM requires an injected token provider")),
        AuthMethod::OAuth { .. } => Box::new(UnsupportedCredential("OAuth requires a pre-authorized injected token provider; interactive browser login is unsupported")),
    }
}

struct UnsupportedCredential(&'static str);

#[async_trait]
impl CredentialProvider for UnsupportedCredential {
    async fn resolve(&self) -> Result<ResolvedCredential, CredentialError> {
        Err(CredentialError::UnsupportedConfiguration {
            reason: self.0.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AuthMethod;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime};

    struct StubTokenProvider {
        calls: Mutex<usize>,
        tokens: Mutex<Vec<DelegatedToken>>,
    }

    #[async_trait]
    impl TokenProvider for StubTokenProvider {
        async fn token(&self, _request: &TokenRequest) -> Result<DelegatedToken, CredentialError> {
            *self.calls.lock().unwrap() += 1;
            Ok(self.tokens.lock().unwrap().remove(0))
        }
    }

    #[test]
    fn debug_redacts_the_api_key_value() {
        let cred = ResolvedCredential::ApiKey {
            header: "Authorization".to_string(),
            prefix: "Bearer ".to_string(),
            value: "sk-secret-12345".to_string(),
        };
        let dbg = format!("{cred:?}");
        assert!(
            !dbg.contains("sk-secret-12345"),
            "the key value must never appear in Debug: {dbg}"
        );
        assert!(dbg.contains("<redacted>"));
        assert!(dbg.contains("Authorization")); // non-secret header stays visible
    }

    #[tokio::test]
    async fn api_key_resolves_the_first_set_env_var() {
        // A deliberately unique name that IS set for this test only.
        let var = "CODYPENDENT_TEST_PROVIDERS_KEY_7c1f";
        std::env::set_var(var, "sk-secret");
        let auth = AuthMethod::ApiKey {
            env: vec![
                "CODYPENDENT_TEST_PROVIDERS_UNSET_a1".to_string(),
                var.to_string(),
            ],
            header: "Authorization".to_string(),
            prefix: "Bearer ".to_string(),
        };
        let resolved = credential_for(&auth).resolve().await.expect("resolves");
        assert_eq!(
            resolved,
            ResolvedCredential::ApiKey {
                header: "Authorization".to_string(),
                prefix: "Bearer ".to_string(),
                value: "sk-secret".to_string(),
            }
        );
        std::env::remove_var(var);
    }

    #[tokio::test]
    async fn api_key_missing_env_errors_naming_the_variable() {
        let var = "CODYPENDENT_TEST_PROVIDERS_NEVER_SET_9f3c";
        assert!(std::env::var(var).is_err(), "precondition: {var} unset");
        let auth = AuthMethod::ApiKey {
            env: vec![var.to_string()],
            header: "Authorization".to_string(),
            prefix: "Bearer ".to_string(),
        };
        let err = credential_for(&auth)
            .resolve()
            .await
            .expect_err("must error");
        match &err {
            CredentialError::MissingEnv { var: v } => assert_eq!(v, var),
            other => panic!("expected MissingEnv, got {other:?}"),
        }
        assert!(err.to_string().contains(var), "message names the variable");
    }

    #[tokio::test]
    async fn none_and_acp_resolve_to_no_credential() {
        assert_eq!(
            credential_for(&AuthMethod::None).resolve().await.unwrap(),
            ResolvedCredential::None
        );
        let acp = AuthMethod::Acp {
            command: "gemini".into(),
            args: vec!["--acp".into()],
            env: Default::default(),
        };
        assert_eq!(
            credential_for(&acp).resolve().await.unwrap(),
            ResolvedCredential::None
        );
    }

    #[tokio::test]
    async fn delegated_credentials_cache_valid_tokens_and_refresh_expiring_tokens() {
        let provider = Arc::new(StubTokenProvider {
            calls: Mutex::new(0),
            tokens: Mutex::new(vec![
                DelegatedToken::new("first-secret", SystemTime::now() + Duration::from_secs(5)),
                DelegatedToken::new(
                    "second-secret",
                    SystemTime::now() + Duration::from_secs(3600),
                ),
            ]),
        });
        let credential = CloudIamCredential::new("gcp_adc", vec!["scope".into()], provider.clone());
        let first = credential.resolve().await.unwrap();
        let second = credential.resolve().await.unwrap();
        assert_eq!(
            *provider.calls.lock().unwrap(),
            2,
            "near-expiry token refreshes"
        );
        assert_ne!(first, second);
        assert!(!format!("{first:?}").contains("first-secret"));
        assert_eq!(credential.resolve().await.unwrap(), second);
        assert_eq!(*provider.calls.lock().unwrap(), 2, "valid token is cached");
    }

    #[tokio::test]
    async fn oauth_uses_injected_provider_and_unsupported_config_is_explicit() {
        let provider = Arc::new(StubTokenProvider {
            calls: Mutex::new(0),
            tokens: Mutex::new(vec![DelegatedToken::new(
                "oauth-secret",
                SystemTime::now() + Duration::from_secs(3600),
            )]),
        });
        let credential = OAuthCredential::new("client", vec!["scope".into()], provider);
        assert!(matches!(
            credential.resolve().await.unwrap(),
            ResolvedCredential::BearerToken { .. }
        ));

        let cloud = AuthMethod::CloudIam {
            variant: "aws_sigv4".into(),
            env: Default::default(),
            scopes: vec![],
        };
        assert!(matches!(
            credential_for(&cloud).resolve().await,
            Err(CredentialError::UnsupportedConfiguration { .. })
        ));
        let oauth = AuthMethod::OAuth {
            authorize_url: "x".into(),
            token_url: "y".into(),
            client_id: "z".into(),
            scopes: vec![],
            pkce: true,
        };
        assert!(matches!(
            credential_for(&oauth).resolve().await,
            Err(CredentialError::UnsupportedConfiguration { .. })
        ));
    }

    #[tokio::test]
    async fn api_key_resolves_env_reference_at_call_time() {
        let var = "CODYPENDENT_TEST_BROKERED_ENV_VAR_8e2b";
        let auth = AuthMethod::ApiKey {
            env: vec![format!("env:{var}")],
            header: "Authorization".to_string(),
            prefix: "Bearer ".to_string(),
        };
        let cred = credential_for(&auth);

        // Before setting: MissingEnv error
        assert!(cred.resolve().await.is_err());

        // Set value: resolves
        std::env::set_var(var, "sk-brokered-secret");
        let resolved = cred.resolve().await.expect("resolves with env: prefix");
        assert_eq!(
            resolved,
            ResolvedCredential::ApiKey {
                header: "Authorization".to_string(),
                prefix: "Bearer ".to_string(),
                value: "sk-brokered-secret".to_string(),
            }
        );

        // Rotate value: call-time resolution returns new value
        std::env::set_var(var, "sk-rotated-secret");
        let rotated = cred.resolve().await.expect("resolves rotated value");
        assert_eq!(
            rotated,
            ResolvedCredential::ApiKey {
                header: "Authorization".to_string(),
                prefix: "Bearer ".to_string(),
                value: "sk-rotated-secret".to_string(),
            }
        );

        std::env::remove_var(var);
    }
}
