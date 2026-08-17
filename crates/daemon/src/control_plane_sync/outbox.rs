//! Durable outbound synchronization queue (`control_plane_outbox`).
//!
//! Conforms to M7 design:
//! - Enqueue happens in the SAME transaction as the local authoritative write.
//! - Redaction happens at ENQUEUE time, never at send time.
//! - Monotonic sequence is assigned per pairing.
//! - Idempotent deduplication via UNIQUE constraints.

use chrono::{DateTime, Utc};
use codypendent_control_plane_protocol::PublicationClass;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use super::error::ControlPlaneSyncError;

/// A row in the durable `control_plane_outbox` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub id: String,
    pub pairing_id: String,
    pub delta_kind: String,
    pub subject_id: String,
    pub payload: serde_json::Value,
    pub class: PublicationClass,
    pub payload_hash: String,
    pub sequence: i64,
    pub created_at: DateTime<Utc>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub remote_receipt: Option<String>,
    pub attempts: i64,
    pub last_error: Option<String>,
}

fn parse_publication_class(s: &str) -> PublicationClass {
    match s {
        "private-local" => PublicationClass::PrivateLocal,
        "metadata-shared" => PublicationClass::MetadataShared,
        "content-shared" => PublicationClass::ContentShared,
        "organization-knowledge" => PublicationClass::OrganizationKnowledge,
        "public-marketplace" => PublicationClass::PublicMarketplace,
        _ => PublicationClass::Unknown,
    }
}

/// Compute SHA-256 hex digest of a JSON value.
#[must_use]
pub fn compute_payload_hash(payload: &serde_json::Value) -> String {
    let serialized = serde_json::to_string(payload).expect("serialize payload");
    let mut hasher = Sha256::new();
    hasher.update(serialized.as_bytes());
    hex::encode(hasher.finalize())
}

/// Redact payload based on effective publication class (Design §8.3 Gotcha 14).
pub fn redact_payload_for_class(
    delta_kind: &str,
    mut payload: serde_json::Value,
    class: PublicationClass,
) -> serde_json::Value {
    if class < PublicationClass::ContentShared {
        // Redact content fields for metadata-shared or narrower
        match delta_kind {
            "session-summary" => {
                if let Some(obj) = payload.as_object_mut() {
                    obj.remove("title");
                    obj.insert("title".to_string(), serde_json::Value::Null);
                }
            }
            "artifact-summary" => {
                if let Some(obj) = payload.as_object_mut() {
                    obj.remove("preview");
                    obj.remove("content");
                }
            }
            _ => {}
        }
    }
    payload
}

/// Enqueue a delta into `control_plane_outbox` inside a SQLite transaction or executor.
pub async fn enqueue_delta<'a, E>(
    executor: E,
    pairing_id: &str,
    max_publication_class: PublicationClass,
    delta_kind: &str,
    subject_id: &str,
    raw_payload: serde_json::Value,
    requested_class: PublicationClass,
) -> Result<Option<String>, ControlPlaneSyncError>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    // Compute effective publication class via intersection
    let effective_class = max_publication_class.intersect(requested_class);
    if effective_class == PublicationClass::PrivateLocal
        || effective_class == PublicationClass::Unknown
    {
        // Data marked private-local must never be published to outbox
        return Ok(None);
    }

    let redacted_payload = redact_payload_for_class(delta_kind, raw_payload, effective_class);
    let payload_str = serde_json::to_string(&redacted_payload)?;
    let payload_hash = compute_payload_hash(&redacted_payload);
    let outbox_id = Uuid::now_v7().to_string();
    let now = Utc::now().to_rfc3339();

    // Sequence assignment: monotonic sequence per pairing claimed inside the insert
    let rows_affected = sqlx::query(
        r#"
        INSERT INTO control_plane_outbox (
            id, pairing_id, delta_kind, subject_id, payload, class,
            payload_hash, sequence, created_at, attempts
        )
        SELECT
            ?, ?, ?, ?, ?, ?, ?,
            COALESCE((SELECT MAX(sequence) FROM control_plane_outbox WHERE pairing_id = ?), 0) + 1,
            ?, 0
        WHERE NOT EXISTS (
            SELECT 1 FROM control_plane_outbox
            WHERE pairing_id = ? AND delta_kind = ? AND subject_id = ? AND payload_hash = ?
        )
        "#,
    )
    .bind(&outbox_id)
    .bind(pairing_id)
    .bind(delta_kind)
    .bind(subject_id)
    .bind(&payload_str)
    .bind(effective_class.as_str())
    .bind(&payload_hash)
    .bind(pairing_id)
    .bind(&now)
    .bind(pairing_id)
    .bind(delta_kind)
    .bind(subject_id)
    .bind(&payload_hash)
    .execute(executor)
    .await?
    .rows_affected();

    if rows_affected > 0 {
        Ok(Some(outbox_id))
    } else {
        // Idempotent duplicate: already enqueued
        Ok(None)
    }
}

/// Helper to enqueue a session summary delta.
// Arguments mirror the delta row this enqueues; a struct would rename them, not reduce them.
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_session_summary<'a, E>(
    executor: E,
    pairing_id: &str,
    max_class: PublicationClass,
    session_id: &str,
    repository_id: Option<&str>,
    state: &str,
    started_at: DateTime<Utc>,
    last_activity_at: Option<DateTime<Utc>>,
    title: Option<&str>,
    requested_class: PublicationClass,
) -> Result<Option<String>, ControlPlaneSyncError>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    let payload = serde_json::json!({
        "session_id": session_id,
        "repository_id": repository_id,
        "state": state,
        "started_at": started_at.to_rfc3339(),
        "last_activity_at": last_activity_at.map(|t| t.to_rfc3339()),
        "title": title,
    });

    enqueue_delta(
        executor,
        pairing_id,
        max_class,
        "session-summary",
        session_id,
        payload,
        requested_class,
    )
    .await
}

/// Helper to enqueue a run summary delta.
// Arguments mirror the delta row this enqueues; a struct would rename them, not reduce them.
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_run_summary<'a, E>(
    executor: E,
    pairing_id: &str,
    max_class: PublicationClass,
    run_id: &str,
    session_id: &str,
    repository_id: Option<&str>,
    state: &str,
    started_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
    status: Option<&str>,
    requested_class: PublicationClass,
) -> Result<Option<String>, ControlPlaneSyncError>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    let payload = serde_json::json!({
        "run_id": run_id,
        "session_id": session_id,
        "repository_id": repository_id,
        "state": state,
        "started_at": started_at.to_rfc3339(),
        "completed_at": completed_at.map(|t| t.to_rfc3339()),
        "status": status,
    });

    enqueue_delta(
        executor,
        pairing_id,
        max_class,
        "run-summary",
        run_id,
        payload,
        requested_class,
    )
    .await
}

/// Helper to enqueue an artifact summary delta.
// Arguments mirror the delta row this enqueues; a struct would rename them, not reduce them.
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_artifact_summary<'a, E>(
    executor: E,
    pairing_id: &str,
    max_class: PublicationClass,
    artifact_id: &str,
    repository_id: Option<&str>,
    name: &str,
    content_hash: &str,
    byte_length: i64,
    media_type: &str,
    requested_class: PublicationClass,
) -> Result<Option<String>, ControlPlaneSyncError>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    let payload = serde_json::json!({
        "artifact_id": artifact_id,
        "repository_id": repository_id,
        "name": name,
        "content_hash": content_hash,
        "byte_length": byte_length,
        "media_type": media_type,
    });

    enqueue_delta(
        executor,
        pairing_id,
        max_class,
        "artifact-summary",
        artifact_id,
        payload,
        requested_class,
    )
    .await
}

/// Helper to enqueue a published graph facts batch delta.
pub async fn enqueue_graph_batch<'a, E>(
    executor: E,
    pairing_id: &str,
    max_class: PublicationClass,
    batch_id: &str,
    repository_id: &str,
    facts_payload: serde_json::Value,
    requested_class: PublicationClass,
) -> Result<Option<String>, ControlPlaneSyncError>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    let payload = serde_json::json!({
        "batch_id": batch_id,
        "repository_id": repository_id,
        "facts": facts_payload,
    });

    enqueue_delta(
        executor,
        pairing_id,
        max_class,
        "graph-batch",
        batch_id,
        payload,
        requested_class,
    )
    .await
}

/// Helper to enqueue an audit event delta.
// Arguments mirror the delta row this enqueues; a struct would rename them, not reduce them.
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_audit_event<'a, E>(
    executor: E,
    pairing_id: &str,
    max_class: PublicationClass,
    event_id: &str,
    action: &str,
    actor_kind: &str,
    target_kind: &str,
    target_id: &str,
    digest: &str,
    detail: serde_json::Value,
    requested_class: PublicationClass,
) -> Result<Option<String>, ControlPlaneSyncError>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    let payload = serde_json::json!({
        "event_id": event_id,
        "action": action,
        "actor_kind": actor_kind,
        "target_kind": target_kind,
        "target_id": target_id,
        "digest": digest,
        "detail": detail,
    });

    enqueue_delta(
        executor,
        pairing_id,
        max_class,
        "approval-decision",
        event_id,
        payload,
        requested_class,
    )
    .await
}

/// Helper to enqueue a tombstone delta.
pub async fn enqueue_tombstone<'a, E>(
    executor: E,
    pairing_id: &str,
    max_class: PublicationClass,
    subject_kind: &str,
    subject_key: &str,
    reason: &str,
) -> Result<Option<String>, ControlPlaneSyncError>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    let payload = serde_json::json!({
        "subject_kind": subject_kind,
        "subject_key": subject_key,
        "reason": reason,
    });

    enqueue_delta(
        executor,
        pairing_id,
        max_class,
        "tombstone",
        subject_key,
        payload,
        PublicationClass::MetadataShared,
    )
    .await
}

/// Read pending deltas from outbox ordered by sequence ASC.
pub async fn fetch_pending_deltas(
    pool: &SqlitePool,
    pairing_id: &str,
    limit: i64,
) -> Result<Vec<OutboxEntry>, ControlPlaneSyncError> {
    let rows = sqlx::query(
        r#"
        SELECT
            id, pairing_id, delta_kind, subject_id, payload, class,
            payload_hash, sequence, created_at, acknowledged_at,
            remote_receipt, attempts, last_error
        FROM control_plane_outbox
        WHERE pairing_id = ? AND acknowledged_at IS NULL
        ORDER BY sequence ASC
        LIMIT ?
        "#,
    )
    .bind(pairing_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row.get("id");
        let pairing_id: String = row.get("pairing_id");
        let delta_kind: String = row.get("delta_kind");
        let subject_id: String = row.get("subject_id");
        let payload_str: String = row.get("payload");
        let class_str: String = row.get("class");
        let payload_hash: String = row.get("payload_hash");
        let sequence: i64 = row.get("sequence");
        let created_at_str: String = row.get("created_at");
        let acknowledged_at_str: Option<String> = row.get("acknowledged_at");
        let remote_receipt: Option<String> = row.get("remote_receipt");
        let attempts: i64 = row.get("attempts");
        let last_error: Option<String> = row.get("last_error");

        let payload: serde_json::Value = serde_json::from_str(&payload_str)?;
        let class = parse_publication_class(&class_str);
        let created_at = DateTime::parse_from_rfc3339(&created_at_str)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let acknowledged_at = acknowledged_at_str.and_then(|s| {
            DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|d| d.with_timezone(&Utc))
        });

        entries.push(OutboxEntry {
            id,
            pairing_id,
            delta_kind,
            subject_id,
            payload,
            class,
            payload_hash,
            sequence,
            created_at,
            acknowledged_at,
            remote_receipt,
            attempts,
            last_error,
        });
    }

    Ok(entries)
}

/// Mark an outbox entry as successfully acknowledged by the control plane.
pub async fn acknowledge_receipt(
    pool: &SqlitePool,
    pairing_id: &str,
    sequence: i64,
    receipt_id: &str,
    accepted_at: DateTime<Utc>,
) -> Result<(), ControlPlaneSyncError> {
    sqlx::query(
        r#"
        UPDATE control_plane_outbox
        SET acknowledged_at = ?, remote_receipt = ?, last_error = NULL
        WHERE pairing_id = ? AND sequence = ?
        "#,
    )
    .bind(accepted_at.to_rfc3339())
    .bind(receipt_id)
    .bind(pairing_id)
    .bind(sequence)
    .execute(pool)
    .await?;

    Ok(())
}

/// Record an error attempt for an outbox row.
pub async fn record_attempt_error(
    pool: &SqlitePool,
    outbox_id: &str,
    error_msg: &str,
) -> Result<(), ControlPlaneSyncError> {
    sqlx::query(
        r#"
        UPDATE control_plane_outbox
        SET attempts = attempts + 1, last_error = ?
        WHERE id = ?
        "#,
    )
    .bind(error_msg)
    .bind(outbox_id)
    .execute(pool)
    .await?;

    Ok(())
}
