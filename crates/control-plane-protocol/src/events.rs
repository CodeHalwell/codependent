//! Real-time stream events and resumable event log payloads.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{OrganizationId, RepositoryId, RunnerJobId, Sha256Digest};
use crate::publication::{DataClassification, PublicationClass};

/// The distinct event streams supported by the control plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum StreamKind {
    Notifications,
    Approvals,
    Schedules,
    RunnerEvents,
    Policy,
    /// Projected shared-session activity.
    Sessions,
    /// Outbound synchronization echo stream.
    Sync,
    /// Unrecognized or newer stream name. A subscriber must not open it.
    #[serde(other)]
    Unknown,
}

/// Durable event in a resumable stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct StreamEvent {
    /// Monotonic log sequence ID serving as resume cursor.
    pub id: u64,
    pub organization_id: OrganizationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<RepositoryId>,
    pub stream: StreamKind,
    pub payload: StreamEventPayload,
    pub created_at: DateTime<Utc>,
}

/// Structured payload for stream events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum StreamEventPayload {
    Notification(NotificationEvent),
    ApprovalRequest(ApprovalRequestEvent),
    ScheduleTrigger(ScheduleTriggerEvent),
    RunnerStatus(RunnerStatusEvent),
    PolicyUpdate(PolicyUpdateEvent),
    /// Payload type emitted by a newer control plane. Consumers must ignore it rather
    /// than fail the whole stream, and must never infer an effect from it.
    #[serde(other)]
    Unknown,
}

/// Notification event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct NotificationEvent {
    pub id: String,
    pub title: String,
    pub body: String,
    pub category: String,
    pub read: bool,
}

/// Remote approval request delivery event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ApprovalRequestEvent {
    pub approval_id: String,
    pub repository_id: RepositoryId,
    pub requested_action: String,
    pub action_digest: Sha256Digest,
    pub risk_level: String,
}

/// Schedule trigger event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ScheduleTriggerEvent {
    pub schedule_id: String,
    pub scheduled_time: DateTime<Utc>,
    pub target: String,
}

/// Runner execution status event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RunnerStatusEvent {
    pub job_id: RunnerJobId,
    pub status: String,
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// Organization policy update event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PolicyUpdateEvent {
    pub policy_version: u64,
    pub max_publication_class: PublicationClass,
    pub max_classification: DataClassification,
}

/// Client subscription request to open a stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct StreamSubscribeRequest {
    pub stream: StreamKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_cursor: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<RepositoryId>,
}
