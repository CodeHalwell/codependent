//! Durable outbound synchronization queue (`control_plane_outbox`).
//!
//! Conforms to M7 design:
//! - Enqueue happens in the SAME transaction as the local authoritative write.
//! - Redaction happens at ENQUEUE time, never at send time.
//! - Monotonic sequence is assigned per pairing.
//! - Idempotent deduplication via UNIQUE constraints.

use chrono::{DateTime, Utc};
use codypendent_control_plane_protocol::{DataClassification, PublicationClass};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::error::ControlPlaneSyncError;
use super::pairing::LocalConsentManifest;

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
    pub delivery_state: String,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub remote_receipt: Option<String>,
    pub rejected_at: Option<DateTime<Utc>>,
    pub rejection_code: Option<String>,
    pub rejection_reason: Option<String>,
    pub attempts: i64,
    pub last_error: Option<String>,
}

const MAX_OUTBOX_ERROR_BYTES: usize = 1_024;
const MAX_REJECTION_CODE_BYTES: usize = 128;
const MAX_REJECTION_REASON_BYTES: usize = 512;

fn bounded_text(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut boundary = limit;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let mut bounded = value[..boundary].to_string();
    bounded.push('…');
    bounded
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

fn parse_data_classification(s: &str) -> DataClassification {
    match s.to_ascii_lowercase().as_str() {
        "public" => DataClassification::Public,
        "internal" => DataClassification::Internal,
        "confidential" => DataClassification::Confidential,
        "secret" => DataClassification::Secret,
        _ => DataClassification::Unknown,
    }
}

#[derive(Debug)]
struct PublicationTarget {
    pairing_id: String,
    effective_class: PublicationClass,
    max_classification: DataClassification,
    retraction_only: bool,
}

/// Resolve the active, owner-matched pairings that explicitly consented to a
/// repository.
///
/// This query runs on the caller's write connection so target resolution and
/// the outbox append observe the same SQLite snapshot as the authoritative
/// mutation. Invalid consent is skipped (never widened). Graph publication
/// policy is deliberately not consulted here: it governs graph facts only.
async fn publication_targets(
    conn: &mut SqliteConnection,
    owner_uid: i64,
    repository_id: &str,
    only_pairing_id: Option<&str>,
    allow_retraction_after_policy_narrowing: bool,
) -> Result<Vec<PublicationTarget>, ControlPlaneSyncError> {
    let rows = sqlx::query(
        r#"
        SELECT p.id, p.organization_id, p.max_publication_class,
               p.consent_manifest, p.consent_manifest_hash, fi.federated_id,
               rm.remote_id AS consent_remote_id,
               ps.max_publication_class AS remote_max_class,
               ps.max_classification AS remote_max_classification
        FROM control_plane_pairings p
        LEFT JOIN federated_repository_identity fi ON fi.repository_id = ?
        LEFT JOIN control_plane_remote_objects rm
          ON rm.pairing_id = p.id
         AND rm.local_kind = 'repository-consent'
         AND rm.local_id = ?
        LEFT JOIN control_plane_policy_snapshot ps ON ps.pairing_id = p.id
        WHERE p.owner_uid = ? AND p.state = 'active'
          AND (p.expires_at IS NULL OR p.expires_at > ?)
          AND (? IS NULL OR p.id = ?)
        ORDER BY p.created_at ASC, p.id ASC
        "#,
    )
    .bind(repository_id)
    .bind(repository_id)
    .bind(owner_uid)
    .bind(Utc::now().to_rfc3339())
    .bind(only_pairing_id)
    .bind(only_pairing_id)
    .fetch_all(&mut *conn)
    .await?;

    let mut targets = Vec::with_capacity(rows.len());
    for row in rows {
        let organization_id: String = row.get("organization_id");
        let consent_manifest: String = row.get("consent_manifest");
        let Ok(manifest) = serde_json::from_str::<LocalConsentManifest>(&consent_manifest) else {
            continue;
        };
        let consent_manifest_hash: String = row.get("consent_manifest_hash");
        let federated_id: Option<String> = row.get("federated_id");
        let consent_remote_id: Option<String> = row.get("consent_remote_id");
        if manifest.compute_hash() != consent_manifest_hash
            || manifest.organization_id != organization_id
            || !manifest.allowed_repositories.iter().any(|allowed| {
                allowed == repository_id
                    || federated_id.as_ref().is_some_and(|id| allowed == id)
                    || consent_remote_id
                        .as_ref()
                        .is_some_and(|remote_id| allowed == remote_id)
            })
        {
            continue;
        }

        let pairing_class =
            parse_publication_class(row.get::<String, _>("max_publication_class").as_str());
        let remote_class = row
            .get::<Option<String>, _>("remote_max_class")
            .map_or(pairing_class, |class| parse_publication_class(&class));
        let consent_class = pairing_class.intersect(manifest.max_publication_class);
        if !consent_class.allows_off_device() {
            continue;
        }
        let policy_class = consent_class.intersect(remote_class);
        let retraction_only = !policy_class.allows_off_device()
            && allow_retraction_after_policy_narrowing
            && remote_class != PublicationClass::Unknown;
        if !policy_class.allows_off_device() && !retraction_only {
            continue;
        }
        let effective_class = if retraction_only {
            PublicationClass::MetadataShared
        } else {
            policy_class
        };

        let max_classification = row
            .get::<Option<String>, _>("remote_max_classification")
            .map_or(DataClassification::Internal, |classification| {
                parse_data_classification(&classification)
            });
        if max_classification == DataClassification::Unknown {
            continue;
        }
        targets.push(PublicationTarget {
            pairing_id: row.get("id"),
            effective_class,
            max_classification,
            retraction_only,
        });
    }
    Ok(targets)
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

/// Append a deliberate new occurrence even when the same semantic payload was
/// published before. This is reserved for send-time policy repair: a fresh
/// sequence is required to supersede bytes that may already have committed at
/// an older, locally unacknowledged sequence.
async fn force_enqueue_delta_occurrence(
    conn: &mut SqliteConnection,
    pairing_id: &str,
    delta_kind: &str,
    subject_id: &str,
    payload: serde_json::Value,
    class: PublicationClass,
) -> Result<String, ControlPlaneSyncError> {
    let payload = redact_payload_for_class(delta_kind, payload, class);
    let payload_json = serde_json::to_string(&payload)?;
    let payload_hash = compute_payload_hash(&payload);
    let outbox_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO control_plane_outbox \
         (id, pairing_id, delta_kind, subject_id, payload, class, payload_hash, \
          sequence, created_at, attempts) \
         VALUES (?, ?, ?, ?, ?, ?, ?, \
           COALESCE((SELECT MAX(sequence) FROM control_plane_outbox WHERE pairing_id = ?), 0) + 1, \
           ?, 0)",
    )
    .bind(&outbox_id)
    .bind(pairing_id)
    .bind(delta_kind)
    .bind(subject_id)
    .bind(payload_json)
    .bind(class.as_str())
    .bind(payload_hash)
    .bind(pairing_id)
    .bind(Utc::now().to_rfc3339())
    .execute(&mut *conn)
    .await?;
    Ok(outbox_id)
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
        "classification": DataClassification::Internal.as_str(),
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
    repository_id: &str,
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
        "repository_id": repository_id,
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
pub async fn enqueue_tombstone(
    pool: &SqlitePool,
    pairing_id: &str,
    max_class: PublicationClass,
    repository_id: &str,
    subject_kind: &str,
    subject_key: &str,
    reason: &str,
) -> Result<Option<String>, ControlPlaneSyncError> {
    let mut tx = pool.begin().await?;
    let result = enqueue_tombstone_on_connection(
        &mut tx,
        pairing_id,
        max_class,
        repository_id,
        subject_kind,
        subject_key,
        reason,
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

async fn enqueue_tombstone_on_connection(
    conn: &mut SqliteConnection,
    pairing_id: &str,
    max_class: PublicationClass,
    repository_id: &str,
    subject_kind: &str,
    subject_key: &str,
    reason: &str,
) -> Result<Option<String>, ControlPlaneSyncError> {
    let payload = serde_json::json!({
        "repository_id": repository_id,
        "subject_kind": subject_kind,
        "subject_key": subject_key,
        "reason": reason,
    });

    let result = enqueue_delta(
        &mut *conn,
        pairing_id,
        max_class,
        "tombstone",
        subject_key,
        payload,
        PublicationClass::MetadataShared,
    )
    .await?;

    if subject_kind == "session" {
        if let Some(tombstone_id) = result.as_deref() {
            sqlx::query(
                "UPDATE control_plane_outbox SET delivery_state = 'rejected', rejected_at = ?, \
                 rejection_code = 'local-tombstone-superseded', \
                 rejection_reason = 'later session tombstone dominates pending summary', \
                 last_error = 'later session tombstone dominates pending summary' \
                 WHERE pairing_id = ? AND delta_kind = 'session-summary' \
                   AND subject_id = ? AND delivery_state = 'pending' \
                   AND sequence < (SELECT sequence FROM control_plane_outbox WHERE id = ?)",
            )
            .bind(Utc::now().to_rfc3339())
            .bind(pairing_id)
            .bind(subject_key)
            .bind(tombstone_id)
            .execute(&mut *conn)
            .await?;
        }
    }
    Ok(result)
}

/// Enqueue the current authoritative session projection for every eligible
/// pairing. The caller must invoke this before committing the session write.
pub(crate) async fn enqueue_session_snapshot(
    conn: &mut SqliteConnection,
    session_id: &str,
) -> Result<usize, ControlPlaneSyncError> {
    enqueue_session_snapshot_scoped(conn, session_id, None, false).await
}

async fn enqueue_session_snapshot_scoped(
    conn: &mut SqliteConnection,
    session_id: &str,
    only_pairing_id: Option<&str>,
    force_current_occurrence: bool,
) -> Result<usize, ControlPlaneSyncError> {
    let row = sqlx::query(
        "SELECT owner_uid, repository_id, state, created_at, updated_at, revision, \
                last_activity_at, title, tombstoned_at, internal \
         FROM sessions WHERE id = ?",
    )
    .bind(session_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(0);
    };
    let owner_uid: Option<i64> = row.get("owner_uid");
    let repository_id: Option<String> = row.get("repository_id");
    let internal: i64 = row.get("internal");
    let (Some(owner_uid), Some(repository_id)) = (owner_uid, repository_id) else {
        return Ok(0);
    };
    if internal != 0 {
        return Ok(0);
    }

    let tombstoned_at: Option<String> = row.get("tombstoned_at");
    let targets = publication_targets(
        conn,
        owner_uid,
        &repository_id,
        only_pairing_id,
        tombstoned_at.is_some(),
    )
    .await?;
    let local_state: String = row.get("state");
    let shared_state = match local_state.as_str() {
        "open" => "running",
        "closed" => "completed",
        _ => "unknown",
    };
    let mut enqueued = 0;
    for target in targets {
        if tombstoned_at.is_some() && target.retraction_only {
            let was_shared: i64 = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM control_plane_outbox \
                 WHERE pairing_id = ? AND delta_kind = 'session-summary' \
                   AND subject_id = ? AND json_valid(payload) \
                   AND json_extract(payload, '$.repository_id') = ? \
                   AND delivery_state IN ('pending', 'acknowledged'))",
            )
            .bind(&target.pairing_id)
            .bind(session_id)
            .bind(&repository_id)
            .fetch_one(&mut *conn)
            .await?;
            if was_shared == 0 {
                continue;
            }
        }
        let result = if tombstoned_at.is_some() {
            enqueue_tombstone_on_connection(
                &mut *conn,
                &target.pairing_id,
                target.effective_class,
                &repository_id,
                "session",
                session_id,
                "deleted",
            )
            .await?
        } else {
            let payload = serde_json::json!({
                "session_id": session_id,
                "repository_id": repository_id,
                "state": shared_state,
                "revision": row.get::<i64, _>("revision"),
                "started_at": row.get::<String, _>("created_at"),
                "updated_at": row.get::<String, _>("updated_at"),
                "last_activity_at": row.get::<Option<String>, _>("last_activity_at"),
                "title": row.get::<String, _>("title"),
            });
            if force_current_occurrence {
                let effective_class = target
                    .effective_class
                    .intersect(PublicationClass::ContentShared);
                Some(
                    force_enqueue_delta_occurrence(
                        conn,
                        &target.pairing_id,
                        "session-summary",
                        session_id,
                        payload,
                        effective_class,
                    )
                    .await?,
                )
            } else {
                enqueue_delta(
                    &mut *conn,
                    &target.pairing_id,
                    target.effective_class,
                    "session-summary",
                    session_id,
                    payload,
                    PublicationClass::ContentShared,
                )
                .await?
            }
        };
        enqueued += usize::from(result.is_some());
    }
    Ok(enqueued)
}

/// Enqueue the current authoritative run projection inside its write
/// transaction. Usage fields are included in the metadata payload so measured
/// usage is not stranded in SQLite when synchronization is enabled.
pub(crate) async fn enqueue_run_snapshot(
    conn: &mut SqliteConnection,
    run_id: &str,
) -> Result<usize, ControlPlaneSyncError> {
    enqueue_run_snapshot_scoped(conn, run_id, None).await
}

async fn enqueue_run_snapshot_scoped(
    conn: &mut SqliteConnection,
    run_id: &str,
    only_pairing_id: Option<&str>,
) -> Result<usize, ControlPlaneSyncError> {
    let row = sqlx::query(
        "SELECT r.session_id, r.state, r.started_at, r.ended_at, r.prompt_tokens, \
                r.completion_tokens, r.cost_micros, r.sync_revision, s.owner_uid, \
                s.repository_id, s.internal \
         FROM runs r JOIN sessions s ON s.id = r.session_id WHERE r.id = ?",
    )
    .bind(run_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(0);
    };
    let owner_uid: Option<i64> = row.get("owner_uid");
    let repository_id: Option<String> = row.get("repository_id");
    let internal: i64 = row.get("internal");
    let (Some(owner_uid), Some(repository_id)) = (owner_uid, repository_id) else {
        return Ok(0);
    };
    if internal != 0 {
        return Ok(0);
    }

    let targets =
        publication_targets(conn, owner_uid, &repository_id, only_pairing_id, false).await?;
    let payload = serde_json::json!({
        "run_id": run_id,
        "session_id": row.get::<String, _>("session_id"),
        "repository_id": repository_id,
        "state": row.get::<String, _>("state"),
        "started_at": row.get::<Option<String>, _>("started_at"),
        "completed_at": row.get::<Option<String>, _>("ended_at"),
        "prompt_tokens": row.get::<Option<i64>, _>("prompt_tokens"),
        "completion_tokens": row.get::<Option<i64>, _>("completion_tokens"),
        "cost_micros": row.get::<Option<i64>, _>("cost_micros"),
        "sync_revision": row.get::<i64, _>("sync_revision"),
    });
    let mut enqueued = 0;
    for target in targets {
        let result = enqueue_delta(
            &mut *conn,
            &target.pairing_id,
            target.effective_class,
            "run-summary",
            run_id,
            payload.clone(),
            PublicationClass::MetadataShared,
        )
        .await?;
        enqueued += usize::from(result.is_some());
    }
    Ok(enqueued)
}

/// Enqueue artifact metadata when its provenance resolves to a run in an
/// explicitly publishable repository. User uploads and system artifacts with
/// no repository provenance remain local-only.
pub(crate) async fn enqueue_artifact_snapshot(
    conn: &mut SqliteConnection,
    artifact_id: &str,
) -> Result<usize, ControlPlaneSyncError> {
    enqueue_artifact_snapshot_scoped(conn, artifact_id, None).await
}

async fn enqueue_artifact_snapshot_scoped(
    conn: &mut SqliteConnection,
    artifact_id: &str,
    only_pairing_id: Option<&str>,
) -> Result<usize, ControlPlaneSyncError> {
    let row = sqlx::query(
        "SELECT sha256, media_type, byte_length, classification, provenance_json \
         FROM artifacts WHERE id = ?",
    )
    .bind(artifact_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(0);
    };
    let provenance: serde_json::Value =
        serde_json::from_str(row.get::<String, _>("provenance_json").as_str())?;
    let Some(run_id) = provenance
        .pointer("/source/run_id")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(0);
    };
    let scope = sqlx::query(
        "SELECT s.owner_uid, s.repository_id, s.internal \
         FROM runs r JOIN sessions s ON s.id = r.session_id WHERE r.id = ?",
    )
    .bind(run_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(scope) = scope else {
        return Ok(0);
    };
    let owner_uid: Option<i64> = scope.get("owner_uid");
    let repository_id: Option<String> = scope.get("repository_id");
    let internal: i64 = scope.get("internal");
    let (Some(owner_uid), Some(repository_id)) = (owner_uid, repository_id) else {
        return Ok(0);
    };
    if internal != 0 {
        return Ok(0);
    }

    let classification = parse_data_classification(row.get::<String, _>("classification").as_str());
    let targets =
        publication_targets(conn, owner_uid, &repository_id, only_pairing_id, false).await?;
    let payload = serde_json::json!({
        "artifact_id": artifact_id,
        "repository_id": repository_id,
        "name": artifact_id,
        "content_hash": row.get::<String, _>("sha256"),
        "byte_length": row.get::<i64, _>("byte_length"),
        "media_type": row.get::<String, _>("media_type"),
        "classification": classification.as_str(),
    });
    let mut enqueued = 0;
    for target in targets {
        if !classification.permits(target.max_classification) {
            continue;
        }
        let result = enqueue_delta(
            &mut *conn,
            &target.pairing_id,
            target.effective_class,
            "artifact-summary",
            artifact_id,
            payload.clone(),
            PublicationClass::MetadataShared,
        )
        .await?;
        enqueued += usize::from(result.is_some());
    }
    Ok(enqueued)
}

/// Enqueue a sealed graph batch. Graph policy is applied here, independently
/// from the generic repository consent used by session/run/artifact metadata.
pub(crate) async fn enqueue_graph_batch_snapshot(
    conn: &mut SqliteConnection,
    batch_id: &str,
) -> Result<usize, ControlPlaneSyncError> {
    enqueue_graph_batch_snapshot_scoped(conn, batch_id, None).await
}

async fn enqueue_graph_batch_snapshot_scoped(
    conn: &mut SqliteConnection,
    batch_id: &str,
    only_pairing_id: Option<&str>,
) -> Result<usize, ControlPlaneSyncError> {
    let batch = sqlx::query(
        "SELECT b.repository_id, b.owner_uid, b.state, p.max_class, \
                p.max_classification \
         FROM graph_publication_batch b \
         LEFT JOIN graph_publication_policy p ON p.repository_id = b.repository_id \
         WHERE b.id = ?",
    )
    .bind(batch_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(batch) = batch else {
        return Ok(0);
    };
    let state: String = batch.get("state");
    let local_class: Option<String> = batch.get("max_class");
    let local_classification: Option<String> = batch.get("max_classification");
    let (Some(local_class), Some(local_classification)) = (local_class, local_classification)
    else {
        // An absent graph policy is explicitly private-local.
        return Ok(0);
    };
    if state == "building" {
        return Ok(0);
    }

    let repository_id: String = batch.get("repository_id");
    let owner_uid: i64 = batch.get("owner_uid");
    let local_class = parse_publication_class(&local_class);
    let local_classification = parse_data_classification(&local_classification);
    if local_classification == DataClassification::Unknown {
        return Ok(0);
    }
    let rows = sqlx::query(
        "SELECT subject_kind, subject_id, class, classification, content_hash \
         FROM graph_publication \
         WHERE batch_id = ? AND decision = 'published' \
         ORDER BY subject_kind ASC, subject_id ASC",
    )
    .bind(batch_id)
    .fetch_all(&mut *conn)
    .await?;
    let facts: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "subject_kind": row.get::<String, _>("subject_kind"),
                "subject_id": row.get::<String, _>("subject_id"),
                "class": row.get::<String, _>("class"),
                "classification": row.get::<String, _>("classification"),
                "content_hash": row.get::<String, _>("content_hash"),
            })
        })
        .collect();

    let targets =
        publication_targets(conn, owner_uid, &repository_id, only_pairing_id, false).await?;
    let mut enqueued = 0;
    for target in targets {
        let max_class = target.effective_class.intersect(local_class);
        if !max_class.allows_off_device() {
            continue;
        }
        let max_classification = target.max_classification.intersect(local_classification);
        if rows.iter().any(|row| {
            let fact_class = parse_publication_class(row.get::<String, _>("class").as_str());
            let fact_classification =
                parse_data_classification(row.get::<String, _>("classification").as_str());
            !fact_class.permits_in_ceiling(max_class)
                || !fact_classification.permits(max_classification)
        }) {
            // A sealed Merkle batch is indivisible; never silently drop only
            // the facts that a newer remote classification ceiling forbids.
            continue;
        }
        let result = enqueue_graph_batch(
            &mut *conn,
            &target.pairing_id,
            max_class,
            batch_id,
            &repository_id,
            serde_json::Value::Array(facts.clone()),
            max_class,
        )
        .await?;
        enqueued += usize::from(result.is_some());
    }
    Ok(enqueued)
}

/// Reconcile native graph tombstones into the durable control-plane outbox.
/// Global repair considers locally unacknowledged rows; pairing bootstrap also
/// includes retained acknowledged rows because native acknowledgement is not
/// proof that a newly added pairing has ever received the retraction.
pub(crate) async fn enqueue_graph_tombstones_for_repository(
    conn: &mut SqliteConnection,
    repository_id: &str,
) -> Result<usize, ControlPlaneSyncError> {
    enqueue_graph_tombstones_for_repository_scoped(conn, repository_id, None, false).await
}

async fn enqueue_graph_tombstones_for_repository_scoped(
    conn: &mut SqliteConnection,
    repository_id: &str,
    only_pairing_id: Option<&str>,
    include_acknowledged: bool,
) -> Result<usize, ControlPlaneSyncError> {
    let rows = sqlx::query(
        "SELECT id, subject_kind, subject_id, reason, created_at, created_by_uid \
         FROM graph_tombstone \
         WHERE repository_id = ? AND (? = 1 OR acknowledged_at IS NULL) \
         ORDER BY created_at ASC, id ASC",
    )
    .bind(repository_id)
    .bind(i64::from(include_acknowledged))
    .fetch_all(&mut *conn)
    .await?;
    let mut enqueued = 0;
    for row in rows {
        let tombstone_id: String = row.get("id");
        let owner_uid: i64 = row.get("created_by_uid");
        let subject_kind: String = row.get("subject_kind");
        let subject_id: String = row.get("subject_id");
        let reason: String = row.get("reason");
        let created_at: String = row.get("created_at");
        for target in
            publication_targets(conn, owner_uid, repository_id, only_pairing_id, true).await?
        {
            if target.retraction_only {
                let was_shared: i64 = sqlx::query_scalar(
                    "SELECT EXISTS(\
                       SELECT 1 FROM control_plane_outbox o, json_each(o.payload, '$.facts') fact \
                       WHERE o.pairing_id = ? AND o.delta_kind = 'graph-batch' \
                         AND o.delivery_state IN ('pending', 'acknowledged') \
                         AND json_extract(o.payload, '$.repository_id') = ? \
                         AND json_extract(fact.value, '$.subject_kind') = ? \
                         AND json_extract(fact.value, '$.subject_id') = ?\
                     )",
                )
                .bind(&target.pairing_id)
                .bind(repository_id)
                .bind(&subject_kind)
                .bind(&subject_id)
                .fetch_one(&mut *conn)
                .await?;
                if was_shared == 0 {
                    continue;
                }
            }
            let payload = serde_json::json!({
                "repository_id": repository_id,
                "subject_kind": format!("graph-{subject_kind}"),
                "subject_key": subject_id,
                "reason": reason,
                "native_tombstone_id": tombstone_id,
                "native_created_at": created_at,
            });
            let result = enqueue_delta(
                &mut *conn,
                &target.pairing_id,
                target.effective_class,
                "tombstone",
                &subject_id,
                payload,
                PublicationClass::MetadataShared,
            )
            .await?;
            enqueued += usize::from(result.is_some());
            enqueued += supersede_pending_graph_fact_for_tombstone(
                conn,
                &target.pairing_id,
                repository_id,
                &subject_kind,
                &subject_id,
            )
            .await?;
        }
    }
    Ok(enqueued)
}

/// Retire older pending graph batches that still contain a now-tombstoned
/// fact. The original row may already have reached the control plane before a
/// local crash, so its later tombstone must still be sent. A sealed native
/// batch is indivisible: reusing its id/hash for residual facts would forge its
/// provenance and falsely acknowledge the original batch.
async fn supersede_pending_graph_fact_for_tombstone(
    conn: &mut SqliteConnection,
    pairing_id: &str,
    repository_id: &str,
    subject_kind: &str,
    subject_id: &str,
) -> Result<usize, ControlPlaneSyncError> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT o.id \
         FROM control_plane_outbox o, \
              json_each(CASE WHEN json_valid(o.payload) THEN o.payload \
                             ELSE '{\"facts\":[]}' END, '$.facts') fact \
         WHERE o.pairing_id = ? AND o.delta_kind = 'graph-batch' \
           AND o.delivery_state = 'pending' \
           AND json_extract(o.payload, '$.repository_id') = ? \
           AND json_extract(fact.value, '$.subject_kind') = ? \
           AND json_extract(fact.value, '$.subject_id') = ? \
         ORDER BY o.sequence ASC",
    )
    .bind(pairing_id)
    .bind(repository_id)
    .bind(subject_kind)
    .bind(subject_id)
    .fetch_all(&mut *conn)
    .await?;

    for outbox_id in rows {
        sqlx::query(
            "UPDATE control_plane_outbox SET delivery_state = 'rejected', rejected_at = ?, \
             rejection_code = 'local-tombstone-superseded', \
             rejection_reason = 'later graph tombstone removed a pending fact', \
             last_error = 'later graph tombstone removed a pending fact' \
             WHERE id = ? AND delivery_state = 'pending'",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(outbox_id)
        .execute(&mut *conn)
        .await?;
    }
    Ok(0)
}

/// Enqueue the metadata-only audit projection for a terminal approval decision.
pub(crate) async fn enqueue_approval_decision_snapshot(
    conn: &mut SqliteConnection,
    approval_id: &str,
) -> Result<usize, ControlPlaneSyncError> {
    enqueue_approval_decision_snapshot_scoped(conn, approval_id, None).await
}

async fn enqueue_approval_decision_snapshot_scoped(
    conn: &mut SqliteConnection,
    approval_id: &str,
    only_pairing_id: Option<&str>,
) -> Result<usize, ControlPlaneSyncError> {
    let row = sqlx::query(
        "SELECT a.state, a.scope, a.resolved_by, a.action_json, a.run_id, \
                r.session_id, s.owner_uid, s.repository_id, s.internal \
         FROM approvals a \
         JOIN runs r ON r.id = a.run_id \
         JOIN sessions s ON s.id = r.session_id \
         WHERE a.id = ? AND a.state IN ('approved', 'rejected', 'expired')",
    )
    .bind(approval_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(0);
    };
    let owner_uid: Option<i64> = row.get("owner_uid");
    let repository_id: Option<String> = row.get("repository_id");
    let internal: i64 = row.get("internal");
    let (Some(owner_uid), Some(repository_id)) = (owner_uid, repository_id) else {
        return Ok(0);
    };
    if internal != 0 {
        return Ok(0);
    }
    let state: String = row.get("state");
    let scope: String = row.get("scope");
    let resolved_by: Option<String> = row.get("resolved_by");
    let action_json: String = row.get("action_json");
    let digest = hex::encode(Sha256::digest(action_json.as_bytes()));
    let actor_kind = resolved_by.as_deref().map_or("system", |actor| {
        if actor.starts_with("auto:") {
            "system"
        } else {
            "human"
        }
    });
    let detail = serde_json::json!({
        "decision": state,
        "scope": scope,
        "run_id": row.get::<String, _>("run_id"),
        "session_id": row.get::<String, _>("session_id"),
        "repository_id": repository_id,
    });
    let mut enqueued = 0;
    for target in
        publication_targets(conn, owner_uid, &repository_id, only_pairing_id, false).await?
    {
        let result = enqueue_audit_event(
            &mut *conn,
            &target.pairing_id,
            target.effective_class,
            &repository_id,
            approval_id,
            "approval.resolved",
            actor_kind,
            "approval",
            approval_id,
            &digest,
            detail.clone(),
            PublicationClass::MetadataShared,
        )
        .await?;
        enqueued += usize::from(result.is_some());
    }
    Ok(enqueued)
}

/// Rebuild missing outbox rows from every current authoritative projection.
/// This runs before the sync worker starts, closing the crash window for stores
/// (notably the federation store) whose native transaction cannot include the
/// control-plane outbox insert. Every enqueue is content-addressed/idempotent.
pub async fn reconcile_authoritative_writes(
    pool: &SqlitePool,
) -> Result<usize, ControlPlaneSyncError> {
    let has_active_pairing: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM control_plane_pairings \
         WHERE state = 'active' AND (expires_at IS NULL OR expires_at > ?))",
    )
    .bind(Utc::now().to_rfc3339())
    .fetch_one(pool)
    .await?;
    if has_active_pairing == 0 {
        return Ok(0);
    }

    let mut tx = pool.begin().await?;
    let mut enqueued = 0;

    let session_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM sessions \
         WHERE owner_uid IS NOT NULL AND repository_id IS NOT NULL AND internal = 0",
    )
    .fetch_all(&mut *tx)
    .await?;
    for id in session_ids {
        enqueued += enqueue_session_snapshot(&mut tx, &id).await?;
    }

    let run_ids: Vec<String> = sqlx::query_scalar(
        "SELECT r.id FROM runs r JOIN sessions s ON s.id = r.session_id \
         WHERE s.owner_uid IS NOT NULL AND s.repository_id IS NOT NULL AND s.internal = 0",
    )
    .fetch_all(&mut *tx)
    .await?;
    for id in run_ids {
        enqueued += enqueue_run_snapshot(&mut tx, &id).await?;
    }

    let artifact_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM artifacts \
         WHERE CASE WHEN json_valid(provenance_json) \
                    THEN json_extract(provenance_json, '$.source.run_id') IS NOT NULL \
                    ELSE 0 END",
    )
    .fetch_all(&mut *tx)
    .await?;
    for id in artifact_ids {
        enqueued += enqueue_artifact_snapshot(&mut tx, &id).await?;
    }

    let batch_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM graph_publication_batch WHERE state IN ('sealed', 'acknowledged')",
    )
    .fetch_all(&mut *tx)
    .await?;
    for id in batch_ids {
        enqueued += enqueue_graph_batch_snapshot(&mut tx, &id).await?;
    }

    let tombstone_repositories: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT repository_id FROM graph_tombstone")
            .fetch_all(&mut *tx)
            .await?;
    for repository_id in tombstone_repositories {
        enqueued +=
            enqueue_graph_tombstones_for_repository_scoped(&mut tx, &repository_id, None, true)
                .await?;
    }

    let approval_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM approvals WHERE state IN ('approved', 'rejected', 'expired')",
    )
    .fetch_all(&mut *tx)
    .await?;
    for id in approval_ids {
        enqueued += enqueue_approval_decision_snapshot(&mut tx, &id).await?;
    }

    tx.commit().await?;
    Ok(enqueued)
}

/// Rebuild missing outbox rows only for repositories in one pairing's
/// authenticated, consent-qualified repository mapping.
///
/// This runs immediately after refreshing that mapping, so legacy manifests
/// containing control-plane UUIDs can recover authoritative writes that were
/// created before the daemon knew the corresponding local repository alias.
/// Restricting both repository IDs and the target pairing avoids a global
/// projection scan on every synchronization poll.
pub async fn reconcile_authoritative_writes_for_pairing(
    pool: &SqlitePool,
    pairing_id: &str,
) -> Result<usize, ControlPlaneSyncError> {
    reconcile_authoritative_writes_for_pairing_inner(pool, pairing_id, false).await
}

/// Re-project current session authority after the authenticated catalog's
/// effective policy fingerprint changes. A fresh occurrence is mandatory:
/// semantic deduplication against an older identical redacted snapshot would
/// otherwise leave a later content-sharing occurrence authoritative remotely.
pub(crate) async fn reconcile_authoritative_writes_after_policy_refresh(
    pool: &SqlitePool,
    pairing_id: &str,
) -> Result<usize, ControlPlaneSyncError> {
    reconcile_authoritative_writes_for_pairing_inner(pool, pairing_id, true).await
}

async fn reconcile_authoritative_writes_for_pairing_inner(
    pool: &SqlitePool,
    pairing_id: &str,
    force_current_session_occurrence: bool,
) -> Result<usize, ControlPlaneSyncError> {
    let pairing_is_active: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM control_plane_pairings \
         WHERE id = ? AND state = 'active' \
           AND (expires_at IS NULL OR expires_at > ?))",
    )
    .bind(pairing_id)
    .bind(Utc::now().to_rfc3339())
    .fetch_one(pool)
    .await?;
    if pairing_is_active == 0 {
        return Ok(0);
    }

    let repository_ids: Vec<String> = sqlx::query_scalar(
        "SELECT local_id FROM control_plane_remote_objects \
         WHERE pairing_id = ? AND local_kind = 'repository-consent' \
         ORDER BY local_id ASC",
    )
    .bind(pairing_id)
    .fetch_all(pool)
    .await?;
    if repository_ids.is_empty() {
        return Ok(0);
    }

    let mut tx = pool.begin().await?;
    let mut enqueued = 0;
    for repository_id in repository_ids {
        let session_ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM sessions \
             WHERE repository_id = ? AND owner_uid IS NOT NULL AND internal = 0",
        )
        .bind(&repository_id)
        .fetch_all(&mut *tx)
        .await?;
        for id in session_ids {
            enqueued += enqueue_session_snapshot_scoped(
                &mut tx,
                &id,
                Some(pairing_id),
                force_current_session_occurrence,
            )
            .await?;
        }

        let run_ids: Vec<String> = sqlx::query_scalar(
            "SELECT r.id FROM runs r JOIN sessions s ON s.id = r.session_id \
             WHERE s.repository_id = ? AND s.owner_uid IS NOT NULL AND s.internal = 0",
        )
        .bind(&repository_id)
        .fetch_all(&mut *tx)
        .await?;
        for id in run_ids {
            enqueued += enqueue_run_snapshot_scoped(&mut tx, &id, Some(pairing_id)).await?;
        }

        let artifact_ids: Vec<String> = sqlx::query_scalar(
            "SELECT a.id FROM artifacts a \
             JOIN runs r ON r.id = CASE WHEN json_valid(a.provenance_json) \
                                        THEN json_extract(a.provenance_json, '$.source.run_id') \
                                   END \
             JOIN sessions s ON s.id = r.session_id \
             WHERE s.repository_id = ? AND s.owner_uid IS NOT NULL AND s.internal = 0",
        )
        .bind(&repository_id)
        .fetch_all(&mut *tx)
        .await?;
        for id in artifact_ids {
            enqueued += enqueue_artifact_snapshot_scoped(&mut tx, &id, Some(pairing_id)).await?;
        }

        let batch_ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM graph_publication_batch \
             WHERE repository_id = ? AND state IN ('sealed', 'acknowledged')",
        )
        .bind(&repository_id)
        .fetch_all(&mut *tx)
        .await?;
        for id in batch_ids {
            enqueued += enqueue_graph_batch_snapshot_scoped(&mut tx, &id, Some(pairing_id)).await?;
        }

        enqueued += enqueue_graph_tombstones_for_repository_scoped(
            &mut tx,
            &repository_id,
            Some(pairing_id),
            true,
        )
        .await?;

        let approval_ids: Vec<String> = sqlx::query_scalar(
            "SELECT a.id FROM approvals a \
             JOIN runs r ON r.id = a.run_id \
             JOIN sessions s ON s.id = r.session_id \
             WHERE s.repository_id = ? AND a.state IN ('approved', 'rejected', 'expired')",
        )
        .bind(&repository_id)
        .fetch_all(&mut *tx)
        .await?;
        for id in approval_ids {
            enqueued +=
                enqueue_approval_decision_snapshot_scoped(&mut tx, &id, Some(pairing_id)).await?;
        }
    }

    tx.commit().await?;
    Ok(enqueued)
}

/// Dead-letter one bounded page of legacy rows whose payload is not JSON.
/// Returning the affected count lets the engine repeat until decoding the
/// pending page is safe, without loading corrupt payloads into Rust first.
pub(crate) async fn reject_malformed_pending_payloads(
    pool: &SqlitePool,
    pairing_id: &str,
    limit: i64,
) -> Result<usize, ControlPlaneSyncError> {
    let result = sqlx::query(
        r#"
        UPDATE control_plane_outbox
        SET delivery_state = 'rejected',
            rejected_at = ?,
            rejection_code = 'local-invalid-delta',
            rejection_reason = 'outbox payload is not valid JSON',
            attempts = attempts + 1,
            last_error = 'local sync delta is permanently invalid: outbox payload is not valid JSON'
        WHERE id IN (
            SELECT id FROM control_plane_outbox
            WHERE pairing_id = ? AND delivery_state = 'pending' AND NOT json_valid(payload)
            ORDER BY sequence ASC
            LIMIT ?
        )
        "#,
    )
    .bind(Utc::now().to_rfc3339())
    .bind(pairing_id)
    .bind(limit)
    .execute(pool)
    .await?;

    Ok(result.rows_affected() as usize)
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
            payload_hash, sequence, created_at, delivery_state,
            acknowledged_at, remote_receipt, rejected_at, rejection_code,
            rejection_reason, attempts, last_error
        FROM control_plane_outbox
        WHERE pairing_id = ? AND delivery_state = 'pending'
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
        let delivery_state: String = row.get("delivery_state");
        let acknowledged_at_str: Option<String> = row.get("acknowledged_at");
        let remote_receipt: Option<String> = row.get("remote_receipt");
        let rejected_at_str: Option<String> = row.get("rejected_at");
        let rejection_code: Option<String> = row.get("rejection_code");
        let rejection_reason: Option<String> = row.get("rejection_reason");
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
        let rejected_at = rejected_at_str.and_then(|s| {
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
            delivery_state,
            acknowledged_at,
            remote_receipt,
            rejected_at,
            rejection_code,
            rejection_reason,
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
    let mut tx = pool.begin().await?;
    let source = sqlx::query(
        "SELECT delta_kind, subject_id, payload FROM control_plane_outbox \
         WHERE pairing_id = ? AND sequence = ? AND delivery_state = 'pending'",
    )
    .bind(pairing_id)
    .bind(sequence)
    .fetch_optional(&mut *tx)
    .await?;
    let updated = sqlx::query(
        r#"
        UPDATE control_plane_outbox
        SET delivery_state = 'acknowledged',
            acknowledged_at = ?,
            remote_receipt = ?,
            rejected_at = NULL,
            rejection_code = NULL,
            rejection_reason = NULL,
            last_error = NULL
        WHERE pairing_id = ? AND sequence = ? AND delivery_state = 'pending'
        "#,
    )
    .bind(accepted_at.to_rfc3339())
    .bind(receipt_id)
    .bind(pairing_id)
    .bind(sequence)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    if updated == 1 {
        if let Some(source) = source {
            let delta_kind: String = source.get("delta_kind");
            let subject_id: String = source.get("subject_id");
            let payload: serde_json::Value =
                serde_json::from_str(source.get::<String, _>("payload").as_str())?;
            if delta_kind == "graph-batch" {
                sqlx::query(
                    "UPDATE graph_publication_batch \
                     SET state = 'acknowledged', acknowledged_at = ?, remote_receipt = ? \
                     WHERE id = ? AND state = 'sealed'",
                )
                .bind(accepted_at.to_rfc3339())
                .bind(receipt_id)
                .bind(&subject_id)
                .execute(&mut *tx)
                .await?;
            } else if delta_kind == "tombstone" {
                let repository_id = payload
                    .get("repository_id")
                    .and_then(serde_json::Value::as_str);
                let subject_kind = payload
                    .get("subject_kind")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|kind| kind.strip_prefix("graph-"));
                let subject_key = payload
                    .get("subject_key")
                    .and_then(serde_json::Value::as_str);
                let reason = payload.get("reason").and_then(serde_json::Value::as_str);
                let tombstone_id = payload
                    .get("native_tombstone_id")
                    .and_then(serde_json::Value::as_str);
                let created_at = payload
                    .get("native_created_at")
                    .and_then(serde_json::Value::as_str);
                if let (
                    Some(repository_id),
                    Some(subject_kind),
                    Some(subject_key),
                    Some(reason),
                    Some(tombstone_id),
                    Some(created_at),
                ) = (
                    repository_id,
                    subject_kind,
                    subject_key,
                    reason,
                    tombstone_id,
                    created_at,
                ) {
                    sqlx::query(
                        "UPDATE graph_tombstone \
                         SET acknowledged_at = ?, remote_receipt = ? \
                         WHERE id = ? AND repository_id = ? AND subject_kind = ? \
                           AND subject_id = ? AND reason = ? AND created_at = ? \
                           AND acknowledged_at IS NULL",
                    )
                    .bind(accepted_at.to_rfc3339())
                    .bind(receipt_id)
                    .bind(tombstone_id)
                    .bind(repository_id)
                    .bind(subject_kind)
                    .bind(subject_key)
                    .bind(reason)
                    .bind(created_at)
                    .execute(&mut *tx)
                    .await?;
                }
            }
        }
    }

    tx.commit().await?;

    Ok(())
}

/// Record an error attempt for an outbox row.
pub async fn record_attempt_error(
    pool: &SqlitePool,
    outbox_id: &str,
    error_msg: &str,
) -> Result<(), ControlPlaneSyncError> {
    let error_msg = bounded_text(error_msg, MAX_OUTBOX_ERROR_BYTES);
    sqlx::query(
        r#"
        UPDATE control_plane_outbox
        SET attempts = attempts + 1, last_error = ?
        WHERE id = ? AND delivery_state = 'pending'
        "#,
    )
    .bind(&error_msg)
    .bind(outbox_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Move a permanently rejected delta out of the ordered pending queue while
/// retaining bounded remote evidence for audit and operator repair.
pub async fn reject_delta_permanently(
    pool: &SqlitePool,
    pairing_id: &str,
    sequence: i64,
    code: &str,
    reason: &str,
    rejected_at: DateTime<Utc>,
) -> Result<(), ControlPlaneSyncError> {
    let code = bounded_text(code, MAX_REJECTION_CODE_BYTES);
    let reason = bounded_text(reason, MAX_REJECTION_REASON_BYTES);
    let last_error = bounded_text(
        &format!("control plane permanently rejected delta with code {code}"),
        MAX_OUTBOX_ERROR_BYTES,
    );
    sqlx::query(
        r#"
        UPDATE control_plane_outbox
        SET delivery_state = 'rejected',
            rejected_at = ?,
            rejection_code = ?,
            rejection_reason = ?,
            attempts = attempts + 1,
            last_error = ?
        WHERE pairing_id = ? AND sequence = ? AND delivery_state = 'pending'
        "#,
    )
    .bind(rejected_at.to_rfc3339())
    .bind(&code)
    .bind(&reason)
    .bind(&last_error)
    .bind(pairing_id)
    .bind(sequence)
    .execute(pool)
    .await?;

    Ok(())
}

/// Move a locally malformed delta out of the pending queue. These failures
/// are deterministic for the immutable row, so retrying them would only starve
/// later valid entries. The bounded reason is retained for operator repair.
pub async fn reject_delta_locally_invalid(
    pool: &SqlitePool,
    pairing_id: &str,
    sequence: i64,
    reason: &str,
    rejected_at: DateTime<Utc>,
) -> Result<(), ControlPlaneSyncError> {
    let code = "local-invalid-delta";
    let reason = bounded_text(reason, MAX_REJECTION_REASON_BYTES);
    let last_error = bounded_text(
        &format!("local sync delta is permanently invalid: {reason}"),
        MAX_OUTBOX_ERROR_BYTES,
    );
    sqlx::query(
        r#"
        UPDATE control_plane_outbox
        SET delivery_state = 'rejected',
            rejected_at = ?,
            rejection_code = ?,
            rejection_reason = ?,
            attempts = attempts + 1,
            last_error = ?
        WHERE pairing_id = ? AND sequence = ? AND delivery_state = 'pending'
        "#,
    )
    .bind(rejected_at.to_rfc3339())
    .bind(code)
    .bind(&reason)
    .bind(&last_error)
    .bind(pairing_id)
    .bind(sequence)
    .execute(pool)
    .await?;

    Ok(())
}

/// Park a row whose current authenticated repository policy forbids
/// transmission. It is reconsidered after every catalog refresh, so a later
/// policy widening does not lose the immutable local publication decision.
pub(crate) async fn reject_delta_by_local_policy(
    pool: &SqlitePool,
    pairing_id: &str,
    sequence: i64,
    reason: &str,
    rejected_at: DateTime<Utc>,
) -> Result<(), ControlPlaneSyncError> {
    let code = "local-policy-blocked";
    let reason = bounded_text(reason, MAX_REJECTION_REASON_BYTES);
    let last_error = bounded_text(
        &format!("local repository policy refused sync delta: {reason}"),
        MAX_OUTBOX_ERROR_BYTES,
    );
    sqlx::query(
        r#"
        UPDATE control_plane_outbox
        SET delivery_state = 'rejected', rejected_at = ?, rejection_code = ?,
            rejection_reason = ?, last_error = ?
        WHERE pairing_id = ? AND sequence = ? AND delivery_state = 'pending'
        "#,
    )
    .bind(rejected_at.to_rfc3339())
    .bind(code)
    .bind(&reason)
    .bind(&last_error)
    .bind(pairing_id)
    .bind(sequence)
    .execute(pool)
    .await?;
    Ok(())
}

/// Reconsider catalog-policy-blocked rows. They become pending before local
/// policy evaluation, never before catalog authentication, and a crash after
/// this transition is safe because the next cycle evaluates every pending row
/// again before network I/O.
pub(crate) async fn reactivate_policy_blocked_deltas(
    pool: &SqlitePool,
    pairing_id: &str,
) -> Result<usize, ControlPlaneSyncError> {
    let result = sqlx::query(
        r#"
        UPDATE control_plane_outbox
        SET delivery_state = 'pending', rejected_at = NULL,
            rejection_code = NULL, rejection_reason = NULL, last_error = NULL
        WHERE pairing_id = ? AND delivery_state = 'rejected'
          AND rejection_code IN ('local-policy-blocked', 'local-policy-refused')
        "#,
    )
    .bind(pairing_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() as usize)
}

/// Replace a too-wide row with a sanitized row at a fresh sequence. The old
/// sequence may already have committed remotely before a local crash; replaying
/// changed bytes at that sequence would only return the old receipt and never
/// apply the redaction. No local supersession identifier is added to the wire.
pub(crate) async fn narrow_pending_delta_at_publication_ceiling(
    pool: &SqlitePool,
    entry: &OutboxEntry,
    narrowed_class: PublicationClass,
) -> Result<(), ControlPlaneSyncError> {
    let payload =
        redact_payload_for_class(&entry.delta_kind, entry.payload.clone(), narrowed_class);
    let mut tx = pool.begin().await?;

    // Never append a repaired historical projection after a newer occurrence
    // of the same subject. Doing so would make stale state the newest remote
    // projection. The newer pending row will be evaluated in sequence order;
    // an acknowledged row is already the stronger current occurrence.
    let later_projection: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM control_plane_outbox \
         WHERE pairing_id = ? AND delta_kind = ? AND subject_id = ? \
           AND sequence > ? AND delivery_state IN ('pending', 'acknowledged'))",
    )
    .bind(&entry.pairing_id)
    .bind(&entry.delta_kind)
    .bind(&entry.subject_id)
    .bind(entry.sequence)
    .fetch_one(&mut *tx)
    .await?;
    if later_projection != 0 {
        sqlx::query(
            "UPDATE control_plane_outbox SET delivery_state = 'rejected', rejected_at = ?, \
             rejection_code = 'local-policy-superseded', \
             rejection_reason = 'newer subject projection supersedes historical policy repair', \
             last_error = 'newer subject projection supersedes historical policy repair' \
             WHERE id = ? AND delivery_state = 'pending'",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(&entry.id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(());
    }

    // A later retraction dominates an older session projection. Retiring the
    // summary instead of narrowing/sending it avoids creating a replacement
    // whose sequence could follow the tombstone and resurrect deleted state.
    if entry.delta_kind == "session-summary" {
        let later_tombstone: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM control_plane_outbox \
             WHERE pairing_id = ? AND delta_kind = 'tombstone' \
               AND subject_id = ? AND sequence > ? \
               AND delivery_state IN ('pending', 'acknowledged'))",
        )
        .bind(&entry.pairing_id)
        .bind(&entry.subject_id)
        .bind(entry.sequence)
        .fetch_one(&mut *tx)
        .await?;
        if later_tombstone != 0 {
            sqlx::query(
                "UPDATE control_plane_outbox SET delivery_state = 'rejected', rejected_at = ?, \
                 rejection_code = 'local-policy-superseded', \
                 rejection_reason = 'later tombstone dominates session projection', \
                 last_error = 'later tombstone dominates session projection' \
                 WHERE id = ? AND delivery_state = 'pending'",
            )
            .bind(Utc::now().to_rfc3339())
            .bind(&entry.id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(());
        }
    }

    force_enqueue_delta_occurrence(
        &mut tx,
        &entry.pairing_id,
        &entry.delta_kind,
        &entry.subject_id,
        payload,
        narrowed_class,
    )
    .await?;
    sqlx::query(
        "UPDATE control_plane_outbox SET delivery_state = 'rejected', rejected_at = ?, \
         rejection_code = 'local-policy-superseded', \
         rejection_reason = 'sanitized replacement queued at a fresh sequence', \
         last_error = 'sanitized replacement queued at a fresh sequence' \
         WHERE id = ? AND delivery_state = 'pending'",
    )
    .bind(Utc::now().to_rfc3339())
    .bind(&entry.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}
