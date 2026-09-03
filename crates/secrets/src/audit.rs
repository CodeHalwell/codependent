//! Append-only, non-secret audit log records.
//!
//! The `secret_audit` schema states the rule in a comment: `outcome_code` is
//! "A DOTTED CODE ONLY … Never a rendered message and never backend output".
//! A comment cannot enforce anything, so the rule is made structural here:
//! [`AuditOutcome`] is a closed enum and it is the **only** thing
//! [`crate::SecretBroker::record_audit`] accepts. There is no `&str` overload,
//! so no caller — in this crate or in the daemon — can write a rendered
//! message, a backend response body, or secret-adjacent text into the ledger.

use serde::{Deserialize, Serialize};

/// The lifecycle event of an audited secret operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEvent {
    Issued,
    Used,
    Denied,
    Expired,
    Rotated,
    Revoked,
    BackendError,
}

impl AuditEvent {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Issued => "issued",
            Self::Used => "used",
            Self::Denied => "denied",
            Self::Expired => "expired",
            Self::Rotated => "rotated",
            Self::Revoked => "revoked",
            Self::BackendError => "backend_error",
        }
    }

    #[must_use]
    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "issued" => Some(Self::Issued),
            "used" => Some(Self::Used),
            "denied" => Some(Self::Denied),
            "expired" => Some(Self::Expired),
            "rotated" => Some(Self::Rotated),
            "revoked" => Some(Self::Revoked),
            "backend_error" => Some(Self::BackendError),
            _ => None,
        }
    }
}

/// The closed set of dotted outcome codes an audit line may carry.
///
/// Every variant renders to a fixed, dotted, space-free token. Nothing here is
/// derived from runtime data, so an audit row cannot become a channel for
/// credential material or for echoed backend output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuditOutcome {
    // --- reference lifecycle ---
    /// A new reference was registered.
    ReferenceCreated,
    /// A reference's locator/backend was rotated.
    ReferenceRotated,
    /// A reference was revoked by its owner (the successful operation).
    ReferenceRevocationApplied,
    // --- issue / use ---
    /// A lease was issued.
    Issued,
    /// Material was resolved and handed to a transport boundary.
    Used,
    // --- denials ---
    /// No reference matches the requested name/capability for this principal.
    UnknownReference,
    /// The reference exists but has been revoked (a denial).
    ReferenceRevoked,
    /// The reference row referred to by a lease no longer exists.
    ReferenceNotFound,
    /// The stored reference no longer hashes to its accepted digest.
    DigestMismatch,
    /// The requested context is broader than the accepted reference.
    ScopeMismatch,
    /// The lease id is unknown.
    LeaseNotFound,
    /// The lease was revoked.
    LeaseRevoked,
    /// The lease TTL elapsed.
    LeaseExpired,
    // --- backend failures ---
    /// The backend has no client or key material configured in this build.
    BackendNotConfigured,
    /// The backend is registered but currently unreachable, or not registered.
    BackendUnavailable,
    /// The platform keychain is unavailable on this system.
    KeychainUnsupported,
    /// The named environment variable is unset or empty.
    EnvMissing,
    /// The backend was consulted and holds nothing under this locator.
    BackendSecretNotFound,
    /// The locator is malformed for this backend.
    InvalidLocator,
    /// A sealed record failed authentication.
    DecryptFailed,
    /// Key material offered to a backend was rejected.
    InvalidKeyMaterial,
    /// Any other backend fault.
    BackendError,
    // --- broker faults ---
    /// A database operation failed.
    DatabaseError,
    /// A stored row failed to parse back into a domain type.
    InvalidData,
}

impl AuditOutcome {
    /// The dotted code persisted in `secret_audit.outcome_code`.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReferenceCreated => "secrets.reference-created",
            Self::ReferenceRotated => "secrets.rotated",
            Self::ReferenceRevocationApplied => "secrets.revoked",
            Self::Issued => "secrets.issued",
            Self::Used => "secrets.used",
            Self::UnknownReference => "secrets.unknown-reference",
            Self::ReferenceRevoked => "secrets.reference-revoked",
            Self::ReferenceNotFound => "secrets.reference-not-found",
            Self::DigestMismatch => "secrets.digest-mismatch",
            Self::ScopeMismatch => "secrets.scope-mismatch",
            Self::LeaseNotFound => "secrets.lease-not-found",
            Self::LeaseRevoked => "secrets.lease-revoked",
            Self::LeaseExpired => "secrets.lease-expired",
            Self::BackendNotConfigured => "secrets.backend-not-configured",
            Self::BackendUnavailable => "secrets.backend-unavailable",
            Self::KeychainUnsupported => "secrets.keychain-unsupported",
            Self::EnvMissing => "secrets.env-missing",
            Self::BackendSecretNotFound => "secrets.backend-secret-not-found",
            Self::InvalidLocator => "secrets.invalid-locator",
            Self::DecryptFailed => "secrets.decrypt-failed",
            Self::InvalidKeyMaterial => "secrets.invalid-key-material",
            Self::BackendError => "secrets.backend-error",
            Self::DatabaseError => "secrets.database-error",
            Self::InvalidData => "secrets.invalid-data",
        }
    }

    /// Every outcome this crate can write. Used by the invariant tests.
    #[must_use]
    pub fn all() -> &'static [AuditOutcome] {
        &[
            Self::ReferenceCreated,
            Self::ReferenceRotated,
            Self::ReferenceRevocationApplied,
            Self::Issued,
            Self::Used,
            Self::UnknownReference,
            Self::ReferenceRevoked,
            Self::ReferenceNotFound,
            Self::DigestMismatch,
            Self::ScopeMismatch,
            Self::LeaseNotFound,
            Self::LeaseRevoked,
            Self::LeaseExpired,
            Self::BackendNotConfigured,
            Self::BackendUnavailable,
            Self::KeychainUnsupported,
            Self::EnvMissing,
            Self::BackendSecretNotFound,
            Self::InvalidLocator,
            Self::DecryptFailed,
            Self::InvalidKeyMaterial,
            Self::BackendError,
            Self::DatabaseError,
            Self::InvalidData,
        ]
    }
}

impl std::fmt::Display for AuditOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A non-secret audit log record.
///
/// `outcome_code` is a `String` on the **read** path only, because rows written
/// by older builds must still deserialize. Everything this crate writes comes
/// from [`AuditOutcome`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretAuditRecord {
    pub id: String,
    pub reference_id: Option<String>,
    pub lease_id: Option<String>,
    pub event: AuditEvent,
    pub principal_uid: u32,
    pub job_id: Option<String>,
    pub capability: Option<String>,
    pub outcome_code: String,
    pub requested_name: Option<String>,
    pub occurred_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_outcome_is_a_dotted_code_and_not_a_message() {
        for outcome in AuditOutcome::all() {
            let code = outcome.as_str();
            assert!(code.starts_with("secrets."), "{code} is not namespaced");
            assert!(!code.contains(' '), "{code} looks like a rendered message");
            assert!(
                code.bytes()
                    .all(|c| c.is_ascii_lowercase() || c == b'.' || c == b'-'),
                "{code} contains characters a dotted code may not have"
            );
        }
    }

    #[test]
    fn outcome_codes_are_unique() {
        let codes: HashSet<&str> = AuditOutcome::all().iter().map(|o| o.as_str()).collect();
        assert_eq!(codes.len(), AuditOutcome::all().len());
    }
}
