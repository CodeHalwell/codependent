//! Immutable, hash-chained audit records and verification.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::ids::{AuditRecordId, CorrelationId, OrganizationId, Sha256Digest};
use crate::page::PageCursor;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuditChainError {
    #[error("broken hash chain at record index {index}: expected prev_hash '{expected}', got '{actual}'")]
    BrokenLink {
        index: usize,
        expected: String,
        actual: String,
    },
    #[error("corrupted record at index {index}: recomputed hash '{computed}' does not match recorded hash '{recorded}'")]
    CorruptedRecord {
        index: usize,
        computed: String,
        recorded: String,
    },
}

/// Actor kind responsible for an audited action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum AuditActorKind {
    User,
    Daemon,
    System,
    /// Unrecognized or newer actor kind. Retained verbatim in the hash chain so that an
    /// old reader can still verify a record written by a newer writer.
    #[serde(other)]
    Unknown,
}

/// Immutable audit record with tamper-evident cryptographic hash chaining.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AuditRecord {
    pub id: AuditRecordId,
    pub organization_id: OrganizationId,
    pub actor_kind: AuditActorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    pub action: String,
    pub target_kind: String,
    pub target_id: String,
    pub action_digest: Sha256Digest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<CorrelationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_hash: Option<Sha256Digest>,
    pub record_hash: Sha256Digest,
    #[serde(default)]
    pub detail: serde_json::Value,
    pub occurred_at: DateTime<Utc>,
}

impl AuditRecord {
    /// Compute the cryptographic record hash for this audit entry given its fields.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn compute_hash(
        organization_id: &OrganizationId,
        actor_kind: AuditActorKind,
        actor_id: Option<&str>,
        action: &str,
        target_kind: &str,
        target_id: &str,
        action_digest: &Sha256Digest,
        correlation_id: Option<&CorrelationId>,
        prev_hash: Option<&Sha256Digest>,
        detail: &serde_json::Value,
        occurred_at: &DateTime<Utc>,
    ) -> Sha256Digest {
        let mut hasher = Sha256::new();
        if let Some(prev) = prev_hash {
            hasher.update(prev.as_bytes());
        } else {
            hasher.update(b"GENESIS");
        }
        hasher.update(organization_id.to_string().as_bytes());
        hasher.update(
            serde_json::to_string(&actor_kind)
                .unwrap_or_default()
                .as_bytes(),
        );
        hasher.update(actor_id.unwrap_or("").as_bytes());
        hasher.update(action.as_bytes());
        hasher.update(target_kind.as_bytes());
        hasher.update(target_id.as_bytes());
        hasher.update(action_digest.as_bytes());
        if let Some(corr) = correlation_id {
            hasher.update(corr.to_string().as_bytes());
        }
        let detail_str = serde_json::to_string(detail).unwrap_or_default();
        hasher.update(detail_str.as_bytes());
        hasher.update(occurred_at.to_rfc3339().as_bytes());
        let result = hasher.finalize();
        Sha256Digest(hex::encode(result))
    }

    /// Recompute the hash of this record to verify its integrity.
    #[must_use]
    pub fn verify_record_hash(&self) -> bool {
        let computed = Self::compute_hash(
            &self.organization_id,
            self.actor_kind,
            self.actor_id.as_deref(),
            &self.action,
            &self.target_kind,
            &self.target_id,
            &self.action_digest,
            self.correlation_id.as_ref(),
            self.prev_hash.as_ref(),
            &self.detail,
            &self.occurred_at,
        );
        computed == self.record_hash
    }
}

/// Verify a chronological chain of audit records for integrity.
pub fn verify_audit_chain(records: &[AuditRecord]) -> Result<(), AuditChainError> {
    for (i, record) in records.iter().enumerate() {
        if !record.verify_record_hash() {
            let computed = AuditRecord::compute_hash(
                &record.organization_id,
                record.actor_kind,
                record.actor_id.as_deref(),
                &record.action,
                &record.target_kind,
                &record.target_id,
                &record.action_digest,
                record.correlation_id.as_ref(),
                record.prev_hash.as_ref(),
                &record.detail,
                &record.occurred_at,
            );
            return Err(AuditChainError::CorruptedRecord {
                index: i,
                computed: computed.0,
                recorded: record.record_hash.0.clone(),
            });
        }
        if i > 0 {
            let prev = &records[i - 1];
            let expected_prev_hash = Some(&prev.record_hash);
            if record.prev_hash.as_ref() != expected_prev_hash {
                return Err(AuditChainError::BrokenLink {
                    index: i,
                    expected: prev.record_hash.0.clone(),
                    actual: record
                        .prev_hash
                        .as_ref()
                        .map(|h| h.0.clone())
                        .unwrap_or_default(),
                });
            }
        }
    }
    Ok(())
}

/// Query parameters for filtering audit logs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AuditQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_time: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_time: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<PageCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}
