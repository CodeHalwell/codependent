//! Workload credentials, service accounts, and token authorization models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{DaemonId, WorkloadCredentialId};

/// Explicit purpose to which a workload credential is cryptographically bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum CredentialPurpose {
    Sync,
    Pairing,
    RunnerJob,
    /// Unrecognized or newer purpose. Purpose binding fails closed: a credential whose
    /// purpose does not parse authorizes nothing.
    #[serde(other)]
    Unknown,
}

impl CredentialPurpose {
    /// Whether a credential carrying this purpose may be used at an endpoint requiring `required`.
    /// `Unknown` never matches, on either side.
    #[must_use]
    pub fn authorizes(self, required: CredentialPurpose) -> bool {
        self != Self::Unknown && required != Self::Unknown && self == required
    }
}

/// Workload credential record (e.g. for daemons or remote runners).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct WorkloadCredential {
    pub id: WorkloadCredentialId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_id: Option<DaemonId>,
    pub audience: String,
    pub purpose: CredentialPurpose,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

/// Service account or workload authentication token representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ServiceAccountToken {
    pub token: String,
    pub token_type: String,
    pub purpose: CredentialPurpose,
    pub expires_at: DateTime<Utc>,
}
