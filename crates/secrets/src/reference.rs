//! Secret references and their accepted digest verification.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::backend::SecretBackendKind;

/// An opaque, stable handle to credential material held by some backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretReference {
    pub id: String,
    pub owner_uid: u32,
    pub name: String,
    pub backend: SecretBackendKind,
    pub locator: String,
    pub capability: String,
    pub organization_id: Option<String>,
    pub repository_id: Option<String>,
    pub accepted_digest: String,
    pub created_at: String,
    pub rotated_at: Option<String>,
    pub revoked_at: Option<String>,
    pub revoked_reason: Option<String>,
}

impl SecretReference {
    /// Compute the canonical digest of the reference as accepted by the operator.
    #[must_use]
    pub fn compute_digest(
        owner_uid: u32,
        name: &str,
        backend: SecretBackendKind,
        locator: &str,
        capability: &str,
        organization_id: Option<&str>,
        repository_id: Option<&str>,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"codypendent-secret-reference-v1\n");
        hasher.update(format!("{owner_uid}\n").as_bytes());
        hasher.update(format!("{name}\n").as_bytes());
        hasher.update(format!("{}\n", backend.as_str()).as_bytes());
        hasher.update(format!("{locator}\n").as_bytes());
        hasher.update(format!("{capability}\n").as_bytes());
        hasher.update(format!("{}\n", organization_id.unwrap_or("")).as_bytes());
        hasher.update(format!("{}\n", repository_id.unwrap_or("")).as_bytes());
        format!("sha256:{}", hex::encode(hasher.finalize()))
    }

    /// Whether this reference has been revoked.
    #[must_use]
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    /// Verify whether the reference's fields still match its accepted digest.
    #[must_use]
    pub fn matches_digest(&self) -> bool {
        let computed = Self::compute_digest(
            self.owner_uid,
            &self.name,
            self.backend,
            &self.locator,
            &self.capability,
            self.organization_id.as_deref(),
            self.repository_id.as_deref(),
        );
        computed == self.accepted_digest
    }
}
