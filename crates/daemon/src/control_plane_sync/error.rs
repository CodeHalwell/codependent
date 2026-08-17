//! Error types for control plane synchronization.

use thiserror::Error;

/// Error encountered during control plane pairing, outbound/inbound sync, or policy enforcement.
#[derive(Debug, Error)]
pub enum ControlPlaneSyncError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("network or http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("no active pairing found for organization or daemon")]
    Unpaired,

    #[error("pairing has been revoked: {0}")]
    Revoked(String),

    #[error("pairing has expired")]
    Expired,

    #[error("invalid consent manifest: {0}")]
    InvalidConsentManifest(String),

    #[error("policy violation: {0}")]
    PolicyViolation(String),

    #[error("remote control plane rejected request: {0}")]
    RemoteRejected(String),

    #[error("unsupported publication class: {0}")]
    UnsupportedPublicationClass(String),

    #[error("pairing conflict: {0}")]
    PairingConflict(String),
}
