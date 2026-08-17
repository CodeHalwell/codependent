//! Inbound synchronization receipts, stream cursors, policy snapshots, and remote object mappings.

use chrono::{DateTime, Utc};
use codypendent_control_plane_protocol::{
    DataClassification, PolicyRestrictions, PolicySnapshot, PublicationClass,
};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use super::error::ControlPlaneSyncError;

/// Record of an inbound message receipt for idempotency prevention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundReceipt {
    pub pairing_id: String,
    pub remote_message_id: String,
    pub message_kind: String,
    pub local_effect_id: Option<String>,
    pub outcome_hash: String,
    pub received_at: DateTime<Utc>,
}

/// Stored policy snapshot on the local daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicySnapshotRecord {
    pub pairing_id: String,
    pub policy_version: i64,
    pub max_publication_class: PublicationClass,
    pub max_classification: DataClassification,
    pub restrictions: PolicyRestrictions,
    pub received_at: DateTime<Utc>,
    pub payload_hash: String,
}

/// Effective combined policy computed via intersection (`local.strictest(remote)`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectivePolicy {
    pub publication_class: PublicationClass,
    pub classification: DataClassification,
    pub restrictions: PolicyRestrictions,
}

impl EffectivePolicy {
    /// Check whether a provider is permitted under the combined policy.
    #[must_use]
    pub fn is_provider_allowed(&self, provider: &str) -> bool {
        self.restrictions.is_provider_allowed(provider)
    }

    /// Check whether a model ID is permitted under the combined policy.
    #[must_use]
    pub fn is_model_allowed(&self, model: &str) -> bool {
        self.restrictions.is_model_allowed(model)
    }

    /// Check whether a region is permitted under the combined policy.
    #[must_use]
    pub fn is_region_allowed(&self, region: &str) -> bool {
        self.restrictions.is_region_allowed(region)
    }

    /// Check whether an integration is permitted under the combined policy.
    #[must_use]
    pub fn is_integration_allowed(&self, integration: &str) -> bool {
        self.restrictions.is_integration_allowed(integration)
    }
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
    match s {
        "public" => DataClassification::Public,
        "internal" => DataClassification::Internal,
        "confidential" => DataClassification::Confidential,
        "secret" => DataClassification::Secret,
        _ => DataClassification::Unknown,
    }
}

/// Check whether an inbound message has already been processed.
pub async fn has_inbound_receipt(
    pool: &SqlitePool,
    pairing_id: &str,
    remote_message_id: &str,
) -> Result<bool, ControlPlaneSyncError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM control_plane_inbound_receipts WHERE pairing_id = ? AND remote_message_id = ?"
    )
    .bind(pairing_id)
    .bind(remote_message_id)
    .fetch_one(pool)
    .await?;

    Ok(count > 0)
}

/// Record an inbound receipt to ensure idempotent handling of redeliveries.
pub async fn record_inbound_receipt<'a, E>(
    executor: E,
    receipt: &InboundReceipt,
) -> Result<(), ControlPlaneSyncError>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    sqlx::query(
        r#"
        INSERT INTO control_plane_inbound_receipts (
            pairing_id, remote_message_id, message_kind, local_effect_id, outcome_hash, received_at
        ) VALUES (?, ?, ?, ?, ?, ?)
        ON CONFLICT(pairing_id, remote_message_id) DO NOTHING
        "#,
    )
    .bind(&receipt.pairing_id)
    .bind(&receipt.remote_message_id)
    .bind(&receipt.message_kind)
    .bind(&receipt.local_effect_id)
    .bind(&receipt.outcome_hash)
    .bind(receipt.received_at.to_rfc3339())
    .execute(executor)
    .await?;

    Ok(())
}

/// Retrieve the latest resume cursor for a stream.
pub async fn get_stream_cursor(
    pool: &SqlitePool,
    pairing_id: &str,
    stream: &str,
) -> Result<Option<String>, ControlPlaneSyncError> {
    let cursor: Option<String> = sqlx::query_scalar(
        "SELECT cursor FROM control_plane_sync_cursors WHERE pairing_id = ? AND stream = ?",
    )
    .bind(pairing_id)
    .bind(stream)
    .fetch_optional(pool)
    .await?;

    Ok(cursor)
}

/// Persist an updated resume cursor for a stream.
pub async fn set_stream_cursor<'a, E>(
    executor: E,
    pairing_id: &str,
    stream: &str,
    cursor: &str,
) -> Result<(), ControlPlaneSyncError>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        r#"
        INSERT INTO control_plane_sync_cursors (pairing_id, stream, cursor, updated_at)
        VALUES (?, ?, ?, ?)
        ON CONFLICT(pairing_id, stream) DO UPDATE SET
            cursor = excluded.cursor,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(pairing_id)
    .bind(stream)
    .bind(cursor)
    .bind(now)
    .execute(executor)
    .await?;

    Ok(())
}

/// Store a narrowed policy snapshot received from the control plane.
pub async fn store_policy_snapshot(
    pool: &SqlitePool,
    pairing_id: &str,
    snapshot: &PolicySnapshot,
) -> Result<(), ControlPlaneSyncError> {
    let restrictions_json = serde_json::to_string(&snapshot.restrictions)?;
    let now = snapshot.received_at.to_rfc3339();
    let hash = snapshot.payload_hash.0.clone();

    sqlx::query(
        r#"
        INSERT INTO control_plane_policy_snapshot (
            pairing_id, policy_version, max_publication_class, max_classification,
            restrictions, received_at, payload_hash
        ) VALUES (?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(pairing_id) DO UPDATE SET
            policy_version = excluded.policy_version,
            max_publication_class = excluded.max_publication_class,
            max_classification = excluded.max_classification,
            restrictions = excluded.restrictions,
            received_at = excluded.received_at,
            payload_hash = excluded.payload_hash
        "#,
    )
    .bind(pairing_id)
    .bind(snapshot.policy_version as i64)
    .bind(snapshot.max_publication_class.as_str())
    .bind(snapshot.max_classification.as_str())
    .bind(&restrictions_json)
    .bind(now)
    .bind(hash)
    .execute(pool)
    .await?;

    Ok(())
}

/// Fetch the stored policy snapshot for a pairing.
pub async fn get_policy_snapshot(
    pool: &SqlitePool,
    pairing_id: &str,
) -> Result<Option<PolicySnapshotRecord>, ControlPlaneSyncError> {
    let row = sqlx::query("SELECT * FROM control_plane_policy_snapshot WHERE pairing_id = ?")
        .bind(pairing_id)
        .fetch_optional(pool)
        .await?;

    match row {
        Some(r) => {
            let pairing_id: String = r.get("pairing_id");
            let policy_version: i64 = r.get("policy_version");
            let max_class_str: String = r.get("max_publication_class");
            let max_classif_str: String = r.get("max_classification");
            let restrictions_str: String = r.get("restrictions");
            let received_at_str: String = r.get("received_at");
            let payload_hash: String = r.get("payload_hash");

            let max_publication_class = parse_publication_class(&max_class_str);
            let max_classification = parse_data_classification(&max_classif_str);
            let restrictions: PolicyRestrictions =
                serde_json::from_str(&restrictions_str).unwrap_or_default();
            let received_at = DateTime::parse_from_rfc3339(&received_at_str)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            Ok(Some(PolicySnapshotRecord {
                pairing_id,
                policy_version,
                max_publication_class,
                max_classification,
                restrictions,
                received_at,
                payload_hash,
            }))
        }
        None => Ok(None),
    }
}

/// Compute effective policy by intersecting local ceilings with remote snapshot (`local.strictest(remote)`).
/// If no snapshot exists, local policy rules without remote restrictions.
pub async fn compute_effective_policy(
    pool: &SqlitePool,
    pairing_id: &str,
    local_max_class: PublicationClass,
    local_max_classification: DataClassification,
) -> Result<EffectivePolicy, ControlPlaneSyncError> {
    let remote_snapshot = get_policy_snapshot(pool, pairing_id).await?;

    if let Some(snapshot) = remote_snapshot {
        let effective_class = local_max_class.intersect(snapshot.max_publication_class);
        let effective_classification =
            local_max_classification.intersect(snapshot.max_classification);
        Ok(EffectivePolicy {
            publication_class: effective_class,
            classification: effective_classification,
            restrictions: snapshot.restrictions,
        })
    } else {
        Ok(EffectivePolicy {
            publication_class: local_max_class,
            classification: local_max_classification,
            restrictions: PolicyRestrictions::default(),
        })
    }
}

/// Record a local ID to remote ID mapping in `control_plane_remote_objects`.
pub async fn record_remote_object(
    pool: &SqlitePool,
    pairing_id: &str,
    local_kind: &str,
    local_id: &str,
    remote_id: &str,
    class: PublicationClass,
) -> Result<(), ControlPlaneSyncError> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        r#"
        INSERT INTO control_plane_remote_objects (
            pairing_id, local_kind, local_id, remote_id, class, published_at
        ) VALUES (?, ?, ?, ?, ?, ?)
        ON CONFLICT(pairing_id, local_kind, local_id) DO UPDATE SET
            remote_id = excluded.remote_id,
            class = excluded.class,
            published_at = excluded.published_at
        "#,
    )
    .bind(pairing_id)
    .bind(local_kind)
    .bind(local_id)
    .bind(remote_id)
    .bind(class.as_str())
    .bind(now)
    .execute(pool)
    .await?;

    Ok(())
}

/// Lookup remote ID by local ID.
pub async fn get_remote_id(
    pool: &SqlitePool,
    pairing_id: &str,
    local_kind: &str,
    local_id: &str,
) -> Result<Option<String>, ControlPlaneSyncError> {
    let remote_id: Option<String> = sqlx::query_scalar(
        "SELECT remote_id FROM control_plane_remote_objects WHERE pairing_id = ? AND local_kind = ? AND local_id = ?"
    )
    .bind(pairing_id)
    .bind(local_kind)
    .bind(local_id)
    .fetch_optional(pool)
    .await?;

    Ok(remote_id)
}
