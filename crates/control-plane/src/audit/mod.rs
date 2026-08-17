use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditRecord {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub actor_kind: String, // 'user' | 'daemon' | 'system'
    pub actor_id: Option<Uuid>,
    pub action: String,
    pub target_kind: String,
    pub target_id: String,
    pub action_digest: Vec<u8>,
    pub correlation_id: Option<Uuid>,
    pub prev_hash: Option<Vec<u8>>,
    pub record_hash: Vec<u8>,
    pub detail: serde_json::Value,
    pub occurred_at: DateTime<Utc>,
}

#[allow(clippy::too_many_arguments)]
pub fn compute_record_hash(
    prev_hash: Option<&[u8]>,
    organization_id: Uuid,
    actor_kind: &str,
    actor_id: Option<Uuid>,
    action: &str,
    target_kind: &str,
    target_id: &str,
    action_digest: &[u8],
    detail: &serde_json::Value,
    occurred_at: DateTime<Utc>,
) -> Vec<u8> {
    let mut hasher = Sha256::new();

    // 1. Previous hash (or zeroes if genesis)
    if let Some(prev) = prev_hash {
        hasher.update(prev);
    } else {
        hasher.update([0u8; 32]);
    }

    // 2. Organization ID
    hasher.update(organization_id.as_bytes());

    // 3. Actor details
    hasher.update(actor_kind.as_bytes());
    if let Some(actor) = actor_id {
        hasher.update(actor.as_bytes());
    } else {
        hasher.update([0u8; 16]);
    }

    // 4. Action and target
    hasher.update(action.as_bytes());
    hasher.update(target_kind.as_bytes());
    hasher.update(target_id.as_bytes());

    // 5. Action digest
    hasher.update(action_digest);

    // 6. Canonical detail JSON
    let canonical_detail = serde_json::to_vec(detail).unwrap_or_default();
    hasher.update(&canonical_detail);

    // 7. Occurred at timestamp RFC3339
    let ts_str = occurred_at.to_rfc3339();
    hasher.update(ts_str.as_bytes());

    hasher.finalize().to_vec()
}

pub fn compute_action_digest(action_payload: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(action_payload);
    hasher.finalize().to_vec()
}

#[derive(Debug, thiserror::Error)]
pub enum AuditVerificationError {
    #[error("hash mismatch at record {id}: expected {expected}, got {actual}")]
    HashMismatch {
        id: Uuid,
        expected: String,
        actual: String,
    },
    #[error("broken chain link at record {id}: prev_hash does not match previous record")]
    BrokenLink { id: Uuid },
}

pub fn verify_audit_chain(records: &[AuditRecord]) -> Result<(), AuditVerificationError> {
    let mut expected_prev_hash: Option<Vec<u8>> = None;

    for record in records {
        // Check link to previous
        if record.prev_hash != expected_prev_hash {
            return Err(AuditVerificationError::BrokenLink { id: record.id });
        }

        // Verify hash computation
        let calculated_hash = compute_record_hash(
            record.prev_hash.as_deref(),
            record.organization_id,
            &record.actor_kind,
            record.actor_id,
            &record.action,
            &record.target_kind,
            &record.target_id,
            &record.action_digest,
            &record.detail,
            record.occurred_at,
        );

        if calculated_hash != record.record_hash {
            return Err(AuditVerificationError::HashMismatch {
                id: record.id,
                expected: hex::encode(calculated_hash),
                actual: hex::encode(&record.record_hash),
            });
        }

        expected_prev_hash = Some(record.record_hash.clone());
    }

    Ok(())
}
