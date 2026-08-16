//! codypendent-providers — the provider/auth data model, credential-provider
//! trait, and curated built-in catalog. A daemon-free, network-free leaf crate.

pub mod catalog;
pub mod credential;
pub mod model;
pub mod retry;

pub use catalog::{builtin_providers, Catalog, CatalogError};
pub use credential::{
    credential_for, CloudIamCredential, CredentialError, CredentialProvider, DelegatedToken,
    OAuthCredential, ResolvedCredential, TokenProvider, TokenRequest,
};
pub use model::{AuthMethod, Model, Protocol, Provider, ProvidersFile};
pub use retry::{
    delay_ms, entropy_jitter, parse_retry_after_hint, retryable, RetryDecision, RETRY_MAX_RETRIES,
};
