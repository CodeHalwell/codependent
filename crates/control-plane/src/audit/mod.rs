//! Audit records, as defined by the control-plane protocol.
//!
//! This module owns **no** record type and **no** hash function of its own. It
//! re-exports the protocol's and adds only the adapters the PostgreSQL schema
//! needs, because the columns are `bytea`/`uuid` while the wire (and every
//! client) is hex strings.
//!
//! The reason for the strictness: this crate previously carried a second
//! `AuditRecord` and a second `compute_record_hash` that hashed `actor_kind` as
//! a raw `&str` where the protocol hashes its JSON encoding, and hashed a
//! 32-zero-byte genesis where the protocol hashes `b"GENESIS"`. A chain written
//! by one could not be verified by the other, and nothing in either crate's
//! tests could notice. There is now exactly one function —
//! [`AuditRecord::compute_hash`] — and both stores call it.

use codypendent_control_plane_protocol::ids::Sha256Digest;
use uuid::Uuid;

pub use codypendent_control_plane_protocol::audit::{
    verify_audit_chain, AuditActorKind, AuditChainError, AuditQuery, AuditRecord,
};

use crate::error::ControlPlaneError;

/// Digest of the exact action being audited.
///
/// Mirrors the local approvals convention (`crates/daemon/src/approvals.rs`) so
/// that a local and a remote record of the same action are comparable.
#[must_use]
pub fn compute_action_digest(action_payload: &[u8]) -> Sha256Digest {
    Sha256Digest::from_bytes(action_payload)
}

/// The value a caller puts in `prev_hash`/`record_hash` when drafting a record.
///
/// Only [`crate::store::Store::append_audit_record`] can compute those two
/// fields: they depend on the tail of the organization's chain, which is only
/// readable under the store's lock. This placeholder is deliberately **not** a
/// valid digest — it is the empty string, so [`digest_to_bytes`] refuses it and
/// a draft that somehow skipped the store can never be written to the chain or
/// mistaken for a computed hash.
#[must_use]
pub fn uncomputed_digest() -> Sha256Digest {
    Sha256Digest(String::new())
}

/// Wire form (64 lowercase hex characters) of a digest column read from
/// PostgreSQL, which stores it as raw `bytea`.
///
/// A row whose digest is not 32 bytes is a corrupted row, not a shorter hash:
/// it is refused rather than rendered into a digest that cannot be reproduced.
pub(crate) fn digest_from_bytes(raw: &[u8]) -> Result<Sha256Digest, ControlPlaneError> {
    Sha256Digest::new(hex::encode(raw)).map_err(|e| {
        ControlPlaneError::Internal(format!("audit record carries a malformed digest: {e}"))
    })
}

/// Storage form (`bytea`) of a wire digest.
pub(crate) fn digest_to_bytes(digest: &Sha256Digest) -> Result<Vec<u8>, ControlPlaneError> {
    // Re-validate rather than trusting the newtype: `Sha256Digest` is
    // `#[serde(transparent)]` over `String`, so a deserialized one has never
    // been checked, and `uncomputed_digest()` is intentionally invalid.
    let validated = Sha256Digest::new(digest.0.clone()).map_err(|e| {
        ControlPlaneError::Internal(format!("refusing to store a malformed digest: {e}"))
    })?;
    hex::decode(validated.as_str()).map_err(|e| {
        ControlPlaneError::Internal(format!("refusing to store a malformed digest: {e}"))
    })
}

/// `audit_records.actor_kind` is `text` with a CHECK constraint listing exactly
/// `user`, `daemon` and `system`.
///
/// [`AuditActorKind::Unknown`] has no representation there, so it is refused
/// before the statement is sent instead of surfacing as an opaque constraint
/// violation. Fail closed: an actor this build cannot name is not audited as
/// though it were one it can.
pub(crate) fn actor_kind_to_db_str(
    kind: AuditActorKind,
) -> Result<&'static str, ControlPlaneError> {
    match kind {
        AuditActorKind::User => Ok("user"),
        AuditActorKind::Daemon => Ok("daemon"),
        AuditActorKind::System => Ok("system"),
        _ => Err(ControlPlaneError::Internal(
            "audit actor kind is not representable in the audit_records schema".to_string(),
        )),
    }
}

/// Inverse of [`actor_kind_to_db_str`]. An unrecognized stored value decodes to
/// [`AuditActorKind::Unknown`] and is retained verbatim in the hash chain, so an
/// older reader can still verify a record written by a newer writer.
pub(crate) fn actor_kind_from_db_str(raw: &str) -> AuditActorKind {
    serde_json::from_value(serde_json::Value::String(raw.to_string()))
        .unwrap_or(AuditActorKind::Unknown)
}

/// `audit_records.actor_id` is a `uuid` column while the protocol carries the
/// actor as an opaque string. Every actor this service records is a
/// control-plane user or daemon id, so a non-UUID actor is an internal
/// invariant break rather than a client input.
pub(crate) fn actor_id_to_db_uuid(
    actor_id: Option<&str>,
) -> Result<Option<Uuid>, ControlPlaneError> {
    match actor_id {
        None => Ok(None),
        Some(id) => Uuid::parse_str(id).map(Some).map_err(|_| {
            ControlPlaneError::Internal(
                "audit actor id is not representable in the audit_records schema".to_string(),
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_uncomputed_digest_can_never_be_stored() {
        let err = digest_to_bytes(&uncomputed_digest())
            .expect_err("a placeholder hash must never reach the chain");
        assert!(matches!(err, ControlPlaneError::Internal(_)));
    }

    #[test]
    fn digests_round_trip_through_the_bytea_representation() {
        let digest = compute_action_digest(b"organization.create");
        let raw = digest_to_bytes(&digest).expect("a real digest must be storable");
        assert_eq!(raw.len(), 32);
        assert_eq!(digest_from_bytes(&raw).unwrap(), digest);
    }

    #[test]
    fn a_short_digest_column_is_refused_rather_than_reinterpreted() {
        assert!(digest_from_bytes(&[0u8; 16]).is_err());
    }

    #[test]
    fn actor_kinds_round_trip_and_unknown_is_refused_by_the_writer() {
        for kind in [
            AuditActorKind::User,
            AuditActorKind::Daemon,
            AuditActorKind::System,
        ] {
            let raw = actor_kind_to_db_str(kind).expect("named kinds are representable");
            assert_eq!(actor_kind_from_db_str(raw), kind);
        }
        assert!(actor_kind_to_db_str(AuditActorKind::Unknown).is_err());
        assert_eq!(actor_kind_from_db_str("runner"), AuditActorKind::Unknown);
    }

    #[test]
    fn a_non_uuid_actor_id_is_refused_instead_of_silently_dropped() {
        assert_eq!(actor_id_to_db_uuid(None).unwrap(), None);
        let id = Uuid::now_v7();
        assert_eq!(
            actor_id_to_db_uuid(Some(&id.to_string())).unwrap(),
            Some(id)
        );
        assert!(actor_id_to_db_uuid(Some("not-a-uuid")).is_err());
    }
}
