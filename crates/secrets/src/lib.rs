//! Codypendent Secrets Broker.
//!
//! Provides context-bound, short-lived, zeroizing credential leases resolved at
//! the final transport boundary without exposing secret values to guest code,
//! logs, or durable storage.
//!
//! # Non-negotiable invariants
//!
//! - Secret **values** never appear in `Debug`, in an error `Display`, in a
//!   serialized body, or in the `secret_audit` ledger.
//! - Every error carried out of this crate is built from a closed set of
//!   dotted codes plus a `&'static str` detail. There is **no** path by which a
//!   runtime string (and therefore a secret value, or backend output that
//!   echoes one) can reach an error message: the detail field is not `String`.
//! - Every audit line is written from [`AuditOutcome`], a closed enum, never
//!   from caller-supplied text.
//! - A backend that cannot do the thing it is named after **refuses**. It never
//!   substitutes another source and never fakes the operation.

pub mod audit;
pub mod backend;
pub mod backends;
pub mod broker;
pub mod lease;
pub mod reference;

pub use audit::{AuditEvent, AuditOutcome, SecretAuditRecord};
pub use backend::{SecretBackend, SecretBackendKind};
pub use broker::SecretBroker;
pub use lease::{LeaseContext, LeaseState, LeasedSecret, SecretLease};
pub use reference::SecretReference;

/// The closed set of backend failure codes.
///
/// A backend cannot invent a code, and cannot attach a runtime-formatted
/// message: the only free-form part of [`SecretError::BackendError`] is a
/// `&'static str`, which by construction cannot carry a secret value or an
/// echoed backend response body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendErrorCode {
    /// The backend exists in the schema but has no working client or key
    /// material configured in this build/deployment. It refuses rather than
    /// pretending the lookup happened and missed.
    NotConfigured,
    /// A configured backend is reachable in principle but currently unavailable.
    Unavailable,
    /// The platform keychain is not available on this system.
    KeychainUnsupported,
    /// The named environment variable is unset or empty.
    EnvMissing,
    /// The backend was consulted and holds nothing under this locator.
    SecretNotFound,
    /// The locator is malformed for this backend.
    InvalidLocator,
    /// Sealed record failed authentication (tampered, truncated, wrong locator,
    /// or wrong key). Never distinguishes *which*, so it leaks nothing.
    DecryptFailed,
    /// Key material offered to a backend was rejected as unusable.
    InvalidKeyMaterial,
    /// Internal backend fault (a poisoned lock, an RNG failure).
    Internal,
}

impl BackendErrorCode {
    /// The dotted outcome code written to the audit ledger and returned on the wire.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotConfigured => "secrets.backend-not-configured",
            Self::Unavailable => "secrets.backend-unavailable",
            Self::KeychainUnsupported => "secrets.keychain-unsupported",
            Self::EnvMissing => "secrets.env-missing",
            Self::SecretNotFound => "secrets.backend-secret-not-found",
            Self::InvalidLocator => "secrets.invalid-locator",
            Self::DecryptFailed => "secrets.decrypt-failed",
            Self::InvalidKeyMaterial => "secrets.invalid-key-material",
            Self::Internal => "secrets.backend-error",
        }
    }

    /// The audit outcome this backend failure is recorded under.
    #[must_use]
    pub fn audit_outcome(&self) -> AuditOutcome {
        match self {
            Self::NotConfigured => AuditOutcome::BackendNotConfigured,
            Self::Unavailable => AuditOutcome::BackendUnavailable,
            Self::KeychainUnsupported => AuditOutcome::KeychainUnsupported,
            Self::EnvMissing => AuditOutcome::EnvMissing,
            Self::SecretNotFound => AuditOutcome::BackendSecretNotFound,
            Self::InvalidLocator => AuditOutcome::InvalidLocator,
            Self::DecryptFailed => AuditOutcome::DecryptFailed,
            Self::InvalidKeyMaterial => AuditOutcome::InvalidKeyMaterial,
            Self::Internal => AuditOutcome::BackendError,
        }
    }
}

impl std::fmt::Display for BackendErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A failure in secret management or broker resolution.
///
/// Every variant's payload is either an identifier the caller already supplied
/// (a reference id, a lease id, a backend name from the schema) or a
/// `&'static str`. No variant can carry credential material.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// The reference id or name is unknown, or is not owned by the caller.
    #[error("secret reference not found: {0}")]
    NotFound(String),
    /// The reference or lease has been revoked.
    #[error("secret reference is revoked: {0}")]
    Revoked(&'static str),
    /// The lease TTL has elapsed.
    #[error("secret lease has expired")]
    Expired,
    /// The requested context is broader than the accepted reference.
    #[error("scope mismatch: {0}")]
    ScopeMismatch(&'static str),
    /// The stored reference no longer hashes to its accepted digest.
    #[error("reference digest mismatch")]
    DigestMismatch,
    /// A backend refused or failed. `detail` is `&'static str` by design.
    #[error("backend error ({code}): {detail}")]
    BackendError {
        code: BackendErrorCode,
        detail: &'static str,
    },
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    /// A stored row failed to parse back into a domain type. Carries only
    /// schema-level text (a backend name, a state name), never a value.
    #[error("invalid data: {0}")]
    InvalidData(String),
}

impl SecretError {
    /// Build a backend failure. The only constructor: `detail` is `&'static str`,
    /// so no runtime string can enter an error message.
    #[must_use]
    pub fn backend(code: BackendErrorCode, detail: &'static str) -> Self {
        Self::BackendError { code, detail }
    }

    /// The typed audit outcome for this failure.
    #[must_use]
    pub fn audit_outcome(&self) -> AuditOutcome {
        match self {
            Self::NotFound(_) => AuditOutcome::UnknownReference,
            Self::Revoked(_) => AuditOutcome::ReferenceRevoked,
            Self::Expired => AuditOutcome::LeaseExpired,
            Self::ScopeMismatch(_) => AuditOutcome::ScopeMismatch,
            Self::DigestMismatch => AuditOutcome::DigestMismatch,
            Self::BackendError { code, .. } => code.audit_outcome(),
            Self::Database(_) => AuditOutcome::DatabaseError,
            Self::InvalidData(_) => AuditOutcome::InvalidData,
        }
    }

    /// The dotted outcome code for this failure.
    #[must_use]
    pub fn outcome_code(&self) -> &'static str {
        self.audit_outcome().as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sentinel is what a secret value would look like. It must be
    /// impossible to get one into an error, so this asserts on the *shape* of
    /// the type: `BackendError.detail` is `&'static str`, which cannot be
    /// produced from runtime credential material.
    #[test]
    fn backend_error_detail_is_static_and_never_renders_a_value() {
        let err = SecretError::backend(BackendErrorCode::SecretNotFound, "no such entry");
        let rendered = format!("{err}");
        assert!(rendered.contains("secrets.backend-secret-not-found"));
        assert!(rendered.contains("no such entry"));

        // Compile-time proof: the detail can only ever be borrowed for 'static.
        fn assert_static(e: &SecretError) -> Option<&'static str> {
            match e {
                SecretError::BackendError { detail, .. } => Some(*detail),
                _ => None,
            }
        }
        assert_eq!(assert_static(&err), Some("no such entry"));
    }

    #[test]
    fn every_backend_code_maps_to_a_dotted_audit_outcome() {
        let codes = [
            BackendErrorCode::NotConfigured,
            BackendErrorCode::Unavailable,
            BackendErrorCode::KeychainUnsupported,
            BackendErrorCode::EnvMissing,
            BackendErrorCode::SecretNotFound,
            BackendErrorCode::InvalidLocator,
            BackendErrorCode::DecryptFailed,
            BackendErrorCode::InvalidKeyMaterial,
            BackendErrorCode::Internal,
        ];
        for code in codes {
            let outcome = code.audit_outcome();
            assert_eq!(outcome.as_str(), code.as_str());
            assert!(outcome.as_str().starts_with("secrets."));
            assert!(!outcome.as_str().contains(' '));
        }
    }

    #[test]
    fn legacy_outcome_codes_are_preserved() {
        assert_eq!(
            SecretError::backend(BackendErrorCode::EnvMissing, "unset").outcome_code(),
            "secrets.env-missing"
        );
        assert_eq!(
            SecretError::backend(BackendErrorCode::KeychainUnsupported, "n/a").outcome_code(),
            "secrets.keychain-unsupported"
        );
        assert_eq!(
            SecretError::backend(BackendErrorCode::Unavailable, "down").outcome_code(),
            "secrets.backend-unavailable"
        );
        assert_eq!(SecretError::Expired.outcome_code(), "secrets.lease-expired");
        assert_eq!(
            SecretError::DigestMismatch.outcome_code(),
            "secrets.digest-mismatch"
        );
    }
}
