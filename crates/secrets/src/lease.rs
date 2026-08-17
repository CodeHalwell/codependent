//! Secret leases, context binding, and zeroizing leased secret material.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use zeroize::Zeroize;

/// The LeaseContext, all 5 axes from design spec §9.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseContext {
    pub principal_uid: u32,
    pub organization_id: Option<String>,
    pub repository_id: Option<String>,
    pub job_id: String,
    pub capability: String,
}

impl LeaseContext {
    /// Construct a new lease context for a given principal, job, and capability.
    pub fn new(
        principal_uid: u32,
        job_id: impl Into<String>,
        capability: impl Into<String>,
    ) -> Self {
        Self {
            principal_uid,
            organization_id: None,
            repository_id: None,
            job_id: job_id.into(),
            capability: capability.into(),
        }
    }

    /// Scope the context to an organization.
    #[must_use]
    pub fn with_org(mut self, org: impl Into<String>) -> Self {
        self.organization_id = Some(org.into());
        self
    }

    /// Scope the context to a repository.
    #[must_use]
    pub fn with_repo(mut self, repo: impl Into<String>) -> Self {
        self.repository_id = Some(repo.into());
        self
    }

    /// Compute the idempotency key for this context and a reference id.
    #[must_use]
    pub fn issue_key(&self, reference_id: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"codypendent-secret-lease-v1\n");
        hasher.update(format!("{reference_id}\n").as_bytes());
        hasher.update(format!("{}\n", self.principal_uid).as_bytes());
        hasher.update(format!("{}\n", self.organization_id.as_deref().unwrap_or("")).as_bytes());
        hasher.update(format!("{}\n", self.repository_id.as_deref().unwrap_or("")).as_bytes());
        hasher.update(format!("{}\n", self.job_id).as_bytes());
        hasher.update(format!("{}\n", self.capability).as_bytes());
        hex::encode(hasher.finalize())
    }
}

/// The lifecycle state of a secret lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    Active,
    Expired,
    Revoked,
    Failed,
}

impl LeaseState {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "expired" => Some(Self::Expired),
            "revoked" => Some(Self::Revoked),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// A short-lived issuance record for credential material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretLease {
    pub id: String,
    pub reference_id: String,
    pub principal_uid: u32,
    pub organization_id: Option<String>,
    pub repository_id: Option<String>,
    pub job_id: String,
    pub capability: String,
    pub issue_key: String,
    pub issued_at: String,
    pub expires_at: String,
    pub backend_lease_handle: Option<String>,
    pub state: LeaseState,
    pub revoked_at: Option<String>,
    pub revoked_reason: Option<String>,
}

impl SecretLease {
    /// Whether this lease is currently active and unexpired.
    #[must_use]
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        if self.state != LeaseState::Active || self.revoked_at.is_some() {
            return false;
        }
        if let Ok(expires) = self.expires_at.parse::<DateTime<Utc>>() {
            now < expires
        } else {
            false
        }
    }
}

/// Opaque secret material that zeroizes its memory on drop and redacts in `Debug`.
///
/// **Invariants**:
/// - Does NOT implement `Clone`.
/// - Does NOT implement `Serialize` or `Deserialize`.
/// - Zeroizes inner buffer upon `Drop`.
/// - Redacts material in `Debug`.
pub struct LeasedSecret {
    bytes: Vec<u8>,
}

impl LeasedSecret {
    /// Construct a leased secret from raw bytes.
    pub fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        Self {
            bytes: bytes.as_ref().to_vec(),
        }
    }

    /// Construct a leased secret from a string slice.
    pub fn from_text(s: &str) -> Self {
        Self {
            bytes: s.as_bytes().to_vec(),
        }
    }

    /// Single documented accessor to borrow the raw secret string for final transport injection.
    pub fn expose_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.bytes)
    }

    /// Single documented accessor to borrow the raw secret bytes for final transport injection.
    pub fn expose(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for LeasedSecret {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

impl fmt::Debug for LeasedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LeasedSecret(\"<redacted>\")")
    }
}
