//! Error types for the federation crate.

use thiserror::Error;

/// Errors arising from federation identity, publication, and query operations.
#[derive(Debug, Error)]
pub enum FederationError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Federated identity not found for repository: {0}")]
    IdentityNotFound(String),

    #[error("Publication policy not found for repository: {0}")]
    PolicyNotFound(String),

    #[error("Node not found: {0}")]
    NodeNotFound(String),

    #[error("Invalid pagination cursor")]
    InvalidCursor,

    #[error("Cannot seal batch while unacknowledged tombstones are pending")]
    UnacknowledgedTombstonesPending,

    #[error("Publication batch not found: {0}")]
    BatchNotFound(String),

    #[error("Publication batch is already sealed: {0}")]
    BatchAlreadySealed(String),

    #[error("Batch hash mismatch: calculated {calculated}, expected {expected}")]
    BatchHashMismatch {
        calculated: String,
        expected: String,
    },

    #[error("Invalid remote URL: {0}")]
    InvalidRemoteUrl(String),

    #[error("Federation error: {0}")]
    Other(String),
}

/// Convenience result type for federation operations.
pub type Result<T> = std::result::Result<T, FederationError>;
