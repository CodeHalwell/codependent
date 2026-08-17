//! Outbound synchronization envelopes, delta streams, receipts, and tombstones.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{
    DaemonId, OrganizationId, RepositoryId, Sha256Digest, SharedSessionId, SyncReceiptId,
    TombstoneId,
};
use crate::publication::PublicationClass;
use crate::version::ProtocolVersion;

/// Kind of delta payload contained in an outbound synchronization batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum SyncDeltaKind {
    SessionSummary,
    RunSummary,
    InboxEntry,
    GraphBatch,
    Tombstone,
    ApprovalDecision,
    UsageAggregate,
    /// Unrecognized or newer delta kind. Receivers must reject the delta rather than
    /// guess at a projection (fail closed).
    #[serde(other)]
    Unknown,
}

impl SyncDeltaKind {
    /// Whether a receiver may project this delta. `Unknown` is never projectable.
    #[must_use]
    pub fn is_projectable(self) -> bool {
        !matches!(self, Self::Unknown)
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionSummary => "session-summary",
            Self::RunSummary => "run-summary",
            Self::InboxEntry => "inbox-entry",
            Self::GraphBatch => "graph-batch",
            Self::Tombstone => "tombstone",
            Self::ApprovalDecision => "approval-decision",
            Self::UsageAggregate => "usage-aggregate",
            Self::Unknown => "unknown",
        }
    }
}

/// An individual synchronized delta emitted by a local daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SyncDelta {
    pub id: String,
    /// Monotonic sequence number per paired daemon instance.
    pub sequence: u64,
    pub kind: SyncDeltaKind,
    /// Repository this delta is scoped to, when the delta kind is repository-scoped.
    /// The control plane never trusts this value beyond selecting a projection target:
    /// the effective organization always comes from the authenticated daemon row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<RepositoryId>,
    pub subject_id: String,
    /// Serialized payload already redacted to `class`.
    pub payload: serde_json::Value,
    pub class: PublicationClass,
    pub payload_hash: Sha256Digest,
    pub created_at: DateTime<Utc>,
}

/// Outbound batch synchronization envelope sent by a daemon to the control plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SyncEnvelope {
    pub protocol_version: ProtocolVersion,
    pub daemon_id: DaemonId,
    pub organization_id: OrganizationId,
    pub sent_at: DateTime<Utc>,
    pub deltas: Vec<SyncDelta>,
}

/// Receipt returned by the control plane confirming durable acceptance of a delta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SyncReceipt {
    pub id: SyncReceiptId,
    pub daemon_id: DaemonId,
    pub daemon_sequence: u64,
    pub delta_kind: SyncDeltaKind,
    pub payload_hash: Sha256Digest,
    /// The class the control plane actually stored, after intersecting the requested class
    /// with the daemon's pairing ceiling. May be narrower than the class the daemon sent.
    pub class: PublicationClass,
    pub accepted_at: DateTime<Utc>,
    /// True when this sequence had already been durably accepted and the delta was replayed.
    #[serde(default)]
    pub duplicate: bool,
}

/// Explicit rejection of a delta during batch synchronization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SyncRejection {
    pub sequence: u64,
    pub code: String,
    pub reason: String,
}

/// Control-plane response to an outbound synchronization batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SyncBatchResponse {
    pub receipts: Vec<SyncReceipt>,
    pub latest_sequence: u64,
    #[serde(default)]
    pub rejected_deltas: Vec<SyncRejection>,
}

/// Reason for a durable tombstone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum TombstoneReason {
    Deleted,
    Narrowed,
    Revoked,
    /// Unrecognized or newer reason. Treated as a full deletion (most restrictive outcome).
    #[serde(other)]
    Unknown,
}

/// Durable tombstone recording the deletion or revocation of an entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct Tombstone {
    pub id: TombstoneId,
    pub organization_id: OrganizationId,
    pub subject_kind: String,
    pub subject_key: String,
    pub reason: TombstoneReason,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_at: Option<DateTime<Utc>>,
}

/// Lifecycle state of a shared session projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum SharedSessionState {
    #[default]
    Running,
    Completed,
    Failed,
    PendingApproval,
    Cancelled,
    /// Unrecognized or newer state. Never treated as terminal or as approved.
    #[serde(other)]
    Unknown,
}

impl SharedSessionState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::PendingApproval => "pending-approval",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }
}

/// Shared session projection synchronized from a daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SharedSession {
    pub id: SharedSessionId,
    pub organization_id: OrganizationId,
    pub repository_id: RepositoryId,
    pub daemon_id: DaemonId,
    pub remote_session_key: String,
    pub class: PublicationClass,
    /// Only populated at `content-shared` or wider; redacted to `None` below that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub state: SharedSessionState,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tombstoned_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

/// Synchronization stream resume cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SyncCursor {
    pub pairing_id: String,
    pub stream: String,
    pub cursor: String,
    pub updated_at: DateTime<Utc>,
}
