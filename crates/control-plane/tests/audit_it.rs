use codypendent_control_plane::{
    audit::{compute_action_digest, verify_audit_chain, AuditRecord},
    MemoryStore, Store,
};
use uuid::Uuid;

#[tokio::test]
async fn audit_hash_chain_verification_and_tamper_detection() {
    let store = MemoryStore::new();
    let org_id = Uuid::now_v7();
    let user_id = Uuid::now_v7();

    // 1. Append 3 audit records
    for i in 1..=3 {
        let action = format!("action_{i}");
        let record = AuditRecord {
            id: Uuid::now_v7(),
            organization_id: org_id,
            actor_kind: "user".to_string(),
            actor_id: Some(user_id),
            action: action.clone(),
            target_kind: "resource".to_string(),
            target_id: format!("res_{i}"),
            action_digest: compute_action_digest(action.as_bytes()),
            correlation_id: None,
            prev_hash: None,
            record_hash: vec![],
            detail: serde_json::json!({ "step": i }),
            occurred_at: chrono::Utc::now() + chrono::Duration::seconds(i),
        };
        store.append_audit_record(record).await.unwrap();
    }

    let mut records = store.list_audit_records(org_id, 10).await.unwrap();
    records.reverse(); // Chronological order
    assert_eq!(records.len(), 3);

    // 2. Legitimate chain verifies
    assert!(verify_audit_chain(&records).is_ok());

    // 3. Tampering with any record's details or reordering fails verification
    let mut tampered_records = records.clone();
    tampered_records[1].detail = serde_json::json!({ "hacked": true });
    assert!(
        verify_audit_chain(&tampered_records).is_err(),
        "Tampering with record payload must break verification"
    );

    // 4. Deleting a middle record breaks the chain link
    let mut deleted_records = records.clone();
    deleted_records.remove(1);
    assert!(
        verify_audit_chain(&deleted_records).is_err(),
        "Deleting a record must break prev_hash chain link"
    );
}
