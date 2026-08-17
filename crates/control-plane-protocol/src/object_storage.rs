//! Object storage metadata and presigned URL contracts.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{DaemonId, OrganizationId, PublishedObjectId, RepositoryId, Sha256Digest};
use crate::publication::PublicationClass;

/// Lifecycle state of a published object in object storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ObjectState {
    /// Upload in-flight. Invisible to all read paths and counts until completed.
    #[default]
    Uploading,
    /// Upload completed and content hash verified. Visible to authorized reads.
    Available,
    /// Object tombstoned / deleted.
    Tombstoned,
    /// Unrecognized or newer state. Never served and never counted.
    #[serde(other)]
    Unknown,
}

impl ObjectState {
    /// Whether the object may be served to a reader or counted. Only `Available` qualifies.
    #[must_use]
    pub fn is_readable(self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Encryption mode for stored objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ObjectEncryption {
    #[default]
    None,
    Envelope,
    /// Unrecognized or newer encryption mode. The object must not be decrypted or served.
    #[serde(other)]
    Unknown,
}

/// Published object metadata record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PublishedObject {
    pub id: PublishedObjectId,
    pub organization_id: OrganizationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<RepositoryId>,
    /// Content address (SHA-256 digest of object bytes).
    pub content_hash: Sha256Digest,
    pub byte_length: u64,
    pub media_type: String,
    pub class: PublicationClass,
    pub encryption: ObjectEncryption,
    pub state: ObjectState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uploaded_by_daemon: Option<DaemonId>,
    pub created_at: DateTime<Utc>,
}

/// Request for a presigned upload URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PresignedUploadRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<RepositoryId>,
    pub content_hash: Sha256Digest,
    pub byte_length: u64,
    pub media_type: String,
    pub class: PublicationClass,
    #[serde(default)]
    pub encryption: ObjectEncryption,
}

/// Response containing a presigned direct upload URL and required headers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PresignedUploadResponse {
    pub object_id: PublishedObjectId,
    pub upload_url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub expires_at: DateTime<Utc>,
}

/// Confirmation that an upload has finished and is ready for verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CompleteUploadRequest {
    pub object_id: PublishedObjectId,
    pub content_hash: Sha256Digest,
    pub actual_byte_length: u64,
}

/// Request for a presigned download URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PresignedDownloadRequest {
    pub object_id: PublishedObjectId,
}

/// Response containing a presigned download URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PresignedDownloadResponse {
    pub download_url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub byte_length: u64,
    pub media_type: String,
    pub content_hash: Sha256Digest,
    pub expires_at: DateTime<Utc>,
}
