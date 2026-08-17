//! Daemon instances, consent manifests, and pairing protocol contracts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ids::{DaemonId, FederatedRepositoryId, OrganizationId, Sha256Digest, UserId};
use crate::publication::PublicationClass;

/// Lifecycle state of a paired daemon instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DaemonState {
    #[default]
    Pending,
    Active,
    Revoked,
    Expired,
    /// Unrecognized or newer state. Never treated as active.
    #[serde(other)]
    Unknown,
}

impl DaemonState {
    /// Whether the daemon may push or pull. Only the explicit `Active` state qualifies.
    #[must_use]
    pub fn is_operational(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// Registered daemon instance in the control plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct Daemon {
    pub id: DaemonId,
    pub organization_id: OrganizationId,
    pub paired_by: UserId,
    pub display_name: String,
    pub consent_manifest_hash: Sha256Digest,
    pub max_publication_class: PublicationClass,
    pub accepts_remote_approvals: bool,
    pub accepts_runner_dispatch: bool,
    pub state: DaemonState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paired_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Consent manifest presented to the human on the local machine during pairing.
/// A cryptographic digest is verified on every reconnection to prevent scope expansion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ConsentManifest {
    pub organization_id: OrganizationId,
    pub organization_display_name: String,
    pub endpoint: String,
    pub allowed_repositories: Vec<FederatedRepositoryId>,
    pub max_publication_class: PublicationClass,
    pub accepts_remote_approvals: bool,
    pub accepts_runner_dispatch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl ConsentManifest {
    /// Compute a deterministic SHA-256 digest of the canonical serialized manifest.
    #[must_use]
    pub fn compute_hash(&self) -> Sha256Digest {
        let canonical_json = serde_json::to_string(self).expect("serialize consent manifest");
        let mut hasher = Sha256::new();
        hasher.update(canonical_json.as_bytes());
        let result = hasher.finalize();
        Sha256Digest(hex::encode(result))
    }
}

/// Scope requested for a daemon pairing challenge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PairingScope {
    pub max_publication_class: PublicationClass,
    pub accepts_remote_approvals: bool,
    pub accepts_runner_dispatch: bool,
    #[serde(default)]
    pub repositories: Vec<FederatedRepositoryId>,
}

/// Single-use short-lived pairing challenge initiated by a user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PairingChallenge {
    pub code_hash: Sha256Digest,
    pub organization_id: OrganizationId,
    pub initiated_by: UserId,
    pub requested_scope: PairingScope,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_id: Option<DaemonId>,
}

/// Request to initiate a new daemon pairing challenge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct InitiatePairingRequest {
    pub organization_id: OrganizationId,
    pub requested_scope: PairingScope,
}

/// Response returned when a pairing challenge is created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct InitiatePairingResponse {
    /// Human-friendly pairing code to enter on the daemon CLI or UI.
    pub challenge_code: String,
    /// Direct pairing verification URL.
    pub verification_uri: String,
    pub expires_at: DateTime<Utc>,
    pub poll_interval_seconds: u32,
}

/// Request sent by a daemon to exchange a verified challenge code for permanent credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ExchangePairingCodeRequest {
    pub challenge_code: String,
    pub daemon_display_name: String,
    pub consent_manifest: ConsentManifest,
}

/// Response returned upon successful challenge code exchange.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ExchangePairingCodeResponse {
    pub daemon_id: DaemonId,
    pub organization_id: OrganizationId,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
    pub max_publication_class: PublicationClass,
}

/// Request to revoke a paired daemon instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RevokeDaemonRequest {
    pub daemon_id: DaemonId,
    pub reason: String,
}
