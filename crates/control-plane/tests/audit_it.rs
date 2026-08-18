//! The audit chain the control plane writes is the protocol's audit chain.
//!
//! Runs entirely against `MemoryStore`; no `DATABASE_URL` is needed
//! (`tests/migrations_it.rs` owns the probe/skip path for tests that genuinely
//! require PostgreSQL).

use codypendent_control_plane::{
    audit::{
        compute_action_digest, uncomputed_digest, verify_audit_chain, AuditActorKind, AuditRecord,
    },
    MemoryStore, Store,
};
use codypendent_control_plane_protocol::ids::{AuditRecordId, OrganizationId};
use uuid::Uuid;

fn record(org_id: OrganizationId, actor_id: Uuid, i: i64) -> AuditRecord {
    let action = format!("action_{i}");
    AuditRecord {
        id: AuditRecordId::new(),
        organization_id: org_id,
        actor_kind: AuditActorKind::User,
        actor_id: Some(actor_id.to_string()),
        action: action.clone(),
        target_kind: "resource".to_string(),
        target_id: format!("res_{i}"),
        action_digest: compute_action_digest(action.as_bytes()),
        correlation_id: None,
        prev_hash: None,
        record_hash: uncomputed_digest(),
        detail: serde_json::json!({ "step": i }),
        occurred_at: chrono::Utc::now() + chrono::Duration::seconds(i),
    }
}

#[tokio::test]
async fn audit_hash_chain_verification_and_tamper_detection() {
    let store = MemoryStore::new();
    let org_id = OrganizationId::new();
    let user_id = Uuid::now_v7();

    for i in 1..=3 {
        store
            .append_audit_record(record(org_id, user_id, i))
            .await
            .unwrap();
    }

    let mut records = store
        .list_audit_records(org_id.as_uuid(), 10)
        .await
        .unwrap();
    records.reverse(); // Chronological order
    assert_eq!(records.len(), 3);

    // 2. The legitimate chain verifies under the protocol's verifier — the only
    //    verifier there is. The server used to carry its own, which hashed the
    //    same record differently, so a chain written here could not be checked
    //    by any client.
    assert!(verify_audit_chain(&records).is_ok());

    // 3. Tampering with any record's details fails verification.
    let mut tampered_records = records.clone();
    tampered_records[1].detail = serde_json::json!({ "hacked": true });
    assert!(
        verify_audit_chain(&tampered_records).is_err(),
        "Tampering with record payload must break verification"
    );

    // 4. Deleting a middle record breaks the chain link.
    let mut deleted_records = records.clone();
    deleted_records.remove(1);
    assert!(
        verify_audit_chain(&deleted_records).is_err(),
        "Deleting a record must break prev_hash chain link"
    );
}

/// The chain fields are hex strings on the wire.
///
/// They were `Vec<u8>` server-side, which serde renders as a JSON **array of
/// integers** — so `GET /v1/organizations/:id/audit` returned
/// `"record_hash": [23, 44, ...]` where the protocol, the generated schema and
/// every SDK client expect a 64-character hex string. No Rust test could see it
/// because both ends of the server agreed with each other.
#[tokio::test]
async fn audit_records_serialize_their_hashes_as_hex_strings() {
    let store = MemoryStore::new();
    let org_id = OrganizationId::new();
    let user_id = Uuid::now_v7();

    store
        .append_audit_record(record(org_id, user_id, 1))
        .await
        .unwrap();
    let stored = store
        .append_audit_record(record(org_id, user_id, 2))
        .await
        .unwrap();

    let json = serde_json::to_value(&stored).unwrap();

    for field in ["action_digest", "prev_hash", "record_hash"] {
        let value = json
            .get(field)
            .unwrap_or_else(|| panic!("{field} must be present"));
        let hex = value
            .as_str()
            .unwrap_or_else(|| panic!("{field} must be a hex string, got {value}"));
        assert_eq!(hex.len(), 64, "{field} must be a 64-character sha-256 hex");
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{field} must be lowercase hex"
        );
    }

    // The actor kind is the protocol's kebab-case tag, not a free-form string.
    assert_eq!(json.get("actor_kind").unwrap(), "user");
}

/// A record whose chain fields were never computed must not be mistaken for a
/// linked one: the store overwrites both, and the placeholder is not a digest.
#[tokio::test]
async fn the_store_owns_the_chain_fields() {
    let store = MemoryStore::new();
    let org_id = OrganizationId::new();

    let drafted = record(org_id, Uuid::now_v7(), 1);
    assert_eq!(drafted.record_hash, uncomputed_digest());

    let stored = store.append_audit_record(drafted).await.unwrap();
    assert_ne!(stored.record_hash, uncomputed_digest());
    assert!(
        stored.verify_record_hash(),
        "the stored record must verify against the protocol's hash"
    );
}
