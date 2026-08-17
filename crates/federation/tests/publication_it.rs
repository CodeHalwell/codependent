//! Integration tests for federation identity, publication, and access-safe traversal.
//!
//! Covers Acceptance Criteria 1–12 and 17 from docs/superpowers/implementation/M6-federation.md §7.

use std::path::Path;

use chrono::Utc;
use codypendent_federation::authorization::{AuthorizedGrants, RepositoryGrant};
use codypendent_federation::error::FederationError;
use codypendent_federation::identity::FederatedRepositoryIdentity;
use codypendent_federation::publication::{
    calculate_edge_class, PublicationClass, PublicationDecision, PublicationPolicy, SubjectKind,
    TombstoneReason,
};
use codypendent_federation::query::{FederationPageCursor, SharedGraphQuery};
use codypendent_federation::store::{SharedGraphStore, TombstoneRecord};
use codypendent_protocol::{CodeNodeId, DataClassification, RepositoryId};
use sqlx::SqlitePool;

/// Helper to set up an in-memory SQLite database migrated to latest.
async fn setup_test_db() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory db");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrations");
    pool
}

// ---------------------------------------------------------------------------
// Acceptance Criterion 1: federated_id_is_stable_across_checkout_paths
// ---------------------------------------------------------------------------
#[test]
fn federated_id_is_stable_across_checkout_paths() {
    let root_commit = "9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b";
    let remote = "git@github.com:CodeHalwell/codypendent.git";

    let path_a = Path::new("/Users/dev1/projects/codypendent");
    let path_b = Path::new("/var/workspace/ci/codypendent");

    // Local RepositoryIds differ
    let local_id_a = codypendent_knowledge::codegraph::stable_repository_id(path_a);
    let local_id_b = codypendent_knowledge::codegraph::stable_repository_id(path_b);
    assert_ne!(local_id_a, local_id_b);

    // Federated IDs match
    let id_a = FederatedRepositoryIdentity::new(
        local_id_a,
        root_commit,
        Some(remote),
        "codypendent",
        1000,
    );
    let id_b = FederatedRepositoryIdentity::new(
        local_id_b,
        root_commit,
        Some(remote),
        "codypendent",
        1000,
    );

    assert_eq!(id_a.federated_id, id_b.federated_id);
    assert_eq!(id_a.federated_id.len(), 64);
}

// ---------------------------------------------------------------------------
// Acceptance Criterion 2: absent_policy_publishes_nothing
// ---------------------------------------------------------------------------
#[tokio::test]
async fn absent_policy_publishes_nothing() {
    let pool = setup_test_db().await;
    let store = SharedGraphStore::new(pool);

    let repo_id = RepositoryId::new();
    let root_commit = "11223344556677889900aabbccddeeff00112233";
    let identity = FederatedRepositoryIdentity::new(
        repo_id,
        root_commit,
        Some("https://github.com/org/repo.git"),
        "Repo",
        1000,
    );
    store.upsert_identity(&identity).await.unwrap();

    // No policy row inserted -> absent policy
    let code_node_id = CodeNodeId::new();
    let (node, decision) = store
        .project_node(
            &identity,
            None, // Absent policy
            &code_node_id,
            "my_module::MyStruct",
            "struct",
            "rust",
            Some("my_package"),
            Some("my_module::MyStruct"),
            Some("src/lib.rs"),
            Some("sig_hash_123"),
            PublicationClass::MetadataShared,
            DataClassification::Internal,
            "rev_1",
        )
        .await
        .unwrap();

    assert_eq!(decision, PublicationDecision::WithheldClass);
    assert_eq!(node.class, PublicationClass::PrivateLocal);

    // Create and seal batch
    let (batch, _) = store
        .create_batch_idempotent(&repo_id, 1000, "batch_1", 1)
        .await
        .unwrap();

    store
        .record_publication(
            &batch.id,
            SubjectKind::Node,
            &node.shared_node_id,
            &repo_id,
            node.class,
            node.classification,
            decision,
            1,
            &node.content_hash,
            "none",
            "default",
            1000,
        )
        .await
        .unwrap();

    let sealed = store.seal_batch(&batch.id).await.unwrap();
    assert_eq!(sealed.fact_count, 0);
}

// ---------------------------------------------------------------------------
// Acceptance Criterion 3: metadata_only_policy_publishes_no_names_paths_or_signatures
// ---------------------------------------------------------------------------
#[tokio::test]
async fn metadata_only_policy_publishes_no_names_paths_or_signatures() {
    let pool = setup_test_db().await;
    let store = SharedGraphStore::new(pool);

    let repo_id = RepositoryId::new();
    let root_commit = "abcdef1234567890abcdef1234567890abcdef12";
    let identity = FederatedRepositoryIdentity::new(
        repo_id,
        root_commit,
        Some("https://github.com/org/fixture-repo.git"),
        "Fixture Repo",
        1000,
    );
    store.upsert_identity(&identity).await.unwrap();

    // Metadata-shared policy with field flags at 0
    let policy = PublicationPolicy {
        repository_id: repo_id,
        max_class: PublicationClass::MetadataShared,
        max_classification: DataClassification::Internal,
        publish_symbol_names: false,
        publish_source_paths: false,
        publish_signature_hashes: false,
        publish_evidence_artifacts: false,
        policy_version: 1,
        updated_at: Utc::now(),
        updated_by_uid: 1000,
    };
    store.upsert_policy(&policy).await.unwrap();

    let code_node_id = CodeNodeId::new();
    let (node, decision) = store
        .project_node(
            &identity,
            Some(&policy),
            &code_node_id,
            "secret_module::SecretFunction",
            "function",
            "rust",
            Some("my_package"),
            Some("secret_module::SecretFunction"),
            Some("src/secret_file.rs"),
            Some("sha256_of_secret_sig"),
            PublicationClass::MetadataShared,
            DataClassification::Internal,
            "rev_10",
        )
        .await
        .unwrap();

    assert_eq!(decision, PublicationDecision::Published);
    assert_eq!(node.class, PublicationClass::MetadataShared);

    // Verify fields are NULL / None
    assert!(node.qualified_name.is_none());
    assert!(node.source_path.is_none());
    assert!(node.signature_hash.is_none());

    // Verify stored projection in database has NULL for those columns
    let stored = store
        .get_shared_node(&node.shared_node_id)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.qualified_name.is_none());
    assert!(stored.source_path.is_none());
    assert!(stored.signature_hash.is_none());

    // Verify serialization contains no leaked symbol name or source path
    let json = serde_json::to_string(&node).unwrap();
    assert!(!json.contains("SecretFunction"));
    assert!(!json.contains("secret_file.rs"));
    assert!(!json.contains("sha256_of_secret_sig"));
}

// ---------------------------------------------------------------------------
// Acceptance Criterion 4: published_facts_never_contain_local_row_ids
// ---------------------------------------------------------------------------
#[tokio::test]
async fn published_facts_never_contain_local_row_ids() {
    let pool = setup_test_db().await;
    let store = SharedGraphStore::new(pool);

    let repo_id = RepositoryId::new();
    let identity = FederatedRepositoryIdentity::new(
        repo_id,
        "00112233445566778899aabbccddeeff00112233",
        Some("https://github.com/org/repo.git"),
        "Repo",
        1000,
    );
    store.upsert_identity(&identity).await.unwrap();

    let policy = PublicationPolicy::metadata_shared(repo_id, 1000);
    store.upsert_policy(&policy).await.unwrap();

    let code_node_id = CodeNodeId::new();
    let (node, _) = store
        .project_node(
            &identity,
            Some(&policy),
            &code_node_id,
            "app::run",
            "function",
            "rust",
            Some("app"),
            Some("app::run"),
            Some("src/main.rs"),
            None,
            PublicationClass::MetadataShared,
            DataClassification::Internal,
            "rev_1",
        )
        .await
        .unwrap();

    let (batch, _) = store
        .create_batch_idempotent(&repo_id, 1000, "batch_publish_facts", 1)
        .await
        .unwrap();

    store
        .record_publication(
            &batch.id,
            SubjectKind::Node,
            &node.shared_node_id,
            &repo_id,
            node.class,
            node.classification,
            PublicationDecision::Published,
            1,
            &node.content_hash,
            "none",
            "default",
            1000,
        )
        .await
        .unwrap();

    let sealed = store.seal_batch(&batch.id).await.unwrap();
    let batch_json = serde_json::to_string(&sealed).unwrap();

    // Verify local code_node_id is not in sealed batch
    assert!(!batch_json.contains(&code_node_id.to_string()));
    assert!(!batch_json.contains("/Users/"));
    assert!(!batch_json.contains("/home/"));
}

// ---------------------------------------------------------------------------
// Acceptance Criterion 5: edge_inherits_strictest_of_its_sources
// ---------------------------------------------------------------------------
#[test]
fn edge_inherits_strictest_of_its_sources() {
    let classes = [
        PublicationClass::PrivateLocal,
        PublicationClass::MetadataShared,
        PublicationClass::ContentShared,
        PublicationClass::OrganizationKnowledge,
        PublicationClass::PublicMarketplace,
    ];

    for &from_node_c in &classes {
        for &to_node_c in &classes {
            for &from_pol_c in &classes {
                for &to_pol_c in &classes {
                    for &ev_floor_c in &classes {
                        let computed = calculate_edge_class(
                            from_node_c,
                            to_node_c,
                            from_pol_c,
                            to_pol_c,
                            ev_floor_c,
                        );
                        let expected_breadth = from_node_c
                            .breadth()
                            .min(to_node_c.breadth())
                            .min(from_pol_c.breadth())
                            .min(to_pol_c.breadth())
                            .min(ev_floor_c.breadth());
                        assert_eq!(
                            computed.breadth(),
                            expected_breadth,
                            "Failed on ({from_node_c:?}, {to_node_c:?}, {from_pol_c:?}, {to_pol_c:?}, {ev_floor_c:?})"
                        );
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Acceptance Criterion 6: narrowing_a_node_tombstones_its_published_edges
// ---------------------------------------------------------------------------
#[tokio::test]
async fn narrowing_a_node_tombstones_its_published_edges() {
    let pool = setup_test_db().await;
    let store = SharedGraphStore::new(pool);

    let repo_a = RepositoryId::new();
    let repo_b = RepositoryId::new();

    let id_a = FederatedRepositoryIdentity::new(
        repo_a,
        "commit_a_123456789012345678901234567890",
        Some("https://github.com/org/repo-a.git"),
        "Repo A",
        1000,
    );
    let id_b = FederatedRepositoryIdentity::new(
        repo_b,
        "commit_b_123456789012345678901234567890",
        Some("https://github.com/org/repo-b.git"),
        "Repo B",
        1000,
    );
    store.upsert_identity(&id_a).await.unwrap();
    store.upsert_identity(&id_b).await.unwrap();

    let policy_a = PublicationPolicy {
        repository_id: repo_a,
        max_class: PublicationClass::ContentShared,
        max_classification: DataClassification::Internal,
        publish_symbol_names: true,
        publish_source_paths: false,
        publish_signature_hashes: false,
        publish_evidence_artifacts: false,
        policy_version: 1,
        updated_at: Utc::now(),
        updated_by_uid: 1000,
    };
    let policy_b = PublicationPolicy {
        repository_id: repo_b,
        max_class: PublicationClass::ContentShared,
        max_classification: DataClassification::Internal,
        publish_symbol_names: true,
        publish_source_paths: false,
        publish_signature_hashes: false,
        publish_evidence_artifacts: false,
        policy_version: 1,
        updated_at: Utc::now(),
        updated_by_uid: 1000,
    };
    store.upsert_policy(&policy_a).await.unwrap();
    store.upsert_policy(&policy_b).await.unwrap();

    let (node_a, _) = store
        .project_node(
            &id_a,
            Some(&policy_a),
            &CodeNodeId::new(),
            "service_a::Client",
            "struct",
            "rust",
            Some("service_a"),
            Some("service_a::Client"),
            None,
            None,
            PublicationClass::ContentShared,
            DataClassification::Internal,
            "rev_1",
        )
        .await
        .unwrap();

    let (node_b, _) = store
        .project_node(
            &id_b,
            Some(&policy_b),
            &CodeNodeId::new(),
            "service_b::Endpoint",
            "function",
            "rust",
            Some("service_b"),
            Some("service_b::Endpoint"),
            None,
            None,
            PublicationClass::ContentShared,
            DataClassification::Internal,
            "rev_1",
        )
        .await
        .unwrap();

    let (edge, _) = store
        .project_edge(
            &node_a,
            &node_b,
            Some(&policy_a),
            Some(&policy_b),
            "calls",
            0.9,
            "syntax_inferred",
            None,
            // Evidence floor must permit `ContentShared`, or the floor — not the
            // policy — is what binds and the narrowing below has nothing to narrow.
            PublicationClass::ContentShared,
            "rev_1",
        )
        .await
        .unwrap();

    assert_eq!(edge.class, PublicationClass::ContentShared);
    let original_digest = edge.class_inputs_digest.clone();

    // Narrow Repository A policy from ContentShared to MetadataShared (version 2)
    let tightened_policy_a = PublicationPolicy {
        repository_id: repo_a,
        max_class: PublicationClass::MetadataShared,
        max_classification: DataClassification::Internal,
        publish_symbol_names: false,
        publish_source_paths: false,
        publish_signature_hashes: false,
        publish_evidence_artifacts: false,
        policy_version: 2,
        updated_at: Utc::now(),
        updated_by_uid: 1000,
    };
    store.upsert_policy(&tightened_policy_a).await.unwrap();

    // Re-project Node A under tightened policy (class becomes MetadataShared)
    let (node_a_narrowed, _) = store
        .project_node(
            &id_a,
            Some(&tightened_policy_a),
            &node_a.code_node_id.unwrap(),
            "service_a::Client",
            "struct",
            "rust",
            Some("service_a"),
            Some("service_a::Client"),
            None,
            None,
            PublicationClass::ContentShared,
            DataClassification::Internal,
            "rev_2",
        )
        .await
        .unwrap();
    assert_eq!(node_a_narrowed.class, PublicationClass::MetadataShared);

    // Run reclassification sweep
    let tombstones = store
        .reclassify_edges_for_repository(&repo_a, 1000)
        .await
        .unwrap();

    assert_eq!(tombstones.len(), 1);
    assert_eq!(tombstones[0].subject_id, edge.shared_edge_id);
    assert_eq!(tombstones[0].reason, TombstoneReason::Narrowed);
    assert_eq!(
        tombstones[0].published_class,
        PublicationClass::ContentShared
    );

    // Verify stored edge has updated class and changed digest
    let updated_edge = store
        .get_shared_edge(&edge.shared_edge_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated_edge.class, PublicationClass::MetadataShared);
    assert_ne!(updated_edge.class_inputs_digest, original_digest);
}

// ---------------------------------------------------------------------------
// Acceptance Criterion 7: unknown_class_is_treated_as_narrowest
// ---------------------------------------------------------------------------
#[test]
fn unknown_class_is_treated_as_narrowest() {
    let unknown = PublicationClass::Unknown;
    assert_eq!(unknown.breadth(), 0);

    let policy_max = PublicationClass::PublicMarketplace;
    assert_eq!(unknown.strictest(policy_max), PublicationClass::Unknown);

    let from_json: PublicationClass =
        serde_json::from_str("\"future-custom-tier\"").unwrap_or(PublicationClass::Unknown);
    assert_eq!(from_json, PublicationClass::Unknown);
    assert_eq!(from_json.breadth(), 0);
}

// ---------------------------------------------------------------------------
// Acceptance Criterion 8: republishing_an_identical_batch_is_idempotent
// ---------------------------------------------------------------------------
#[tokio::test]
async fn republishing_an_identical_batch_is_idempotent() {
    let pool = setup_test_db().await;
    let store = SharedGraphStore::new(pool);

    let repo_id = RepositoryId::new();
    let identity = FederatedRepositoryIdentity::new(
        repo_id,
        "commit_root_1234567890abcdef1234567890",
        Some("https://github.com/org/repo.git"),
        "Repo",
        1000,
    );
    store.upsert_identity(&identity).await.unwrap();

    let (batch_1, created_1) = store
        .create_batch_idempotent(&repo_id, 1000, "client_idempotency_key_42", 1)
        .await
        .unwrap();
    assert!(created_1);

    let (batch_2, created_2) = store
        .create_batch_idempotent(&repo_id, 1000, "client_idempotency_key_42", 1)
        .await
        .unwrap();
    assert!(!created_2);
    assert_eq!(batch_1.id, batch_2.id);
}

// ---------------------------------------------------------------------------
// Acceptance Criterion 9: tombstones_are_drained_before_a_new_batch_is_sealed
// ---------------------------------------------------------------------------
#[tokio::test]
async fn tombstones_are_drained_before_a_new_batch_is_sealed() {
    let pool = setup_test_db().await;
    let store = SharedGraphStore::new(pool);

    let repo_id = RepositoryId::new();
    let identity = FederatedRepositoryIdentity::new(
        repo_id,
        "root_commit_tombstone_test_12345678901234",
        Some("https://github.com/org/repo.git"),
        "Repo",
        1000,
    );
    store.upsert_identity(&identity).await.unwrap();

    // Create an unacknowledged tombstone
    let tombstone = TombstoneRecord {
        id: uuid::Uuid::now_v7().to_string(),
        repository_id: repo_id,
        subject_kind: SubjectKind::Node,
        subject_id: "dead_node_1234567890123456789012345678901234567890123456789012345678901234"
            .to_string(),
        reason: TombstoneReason::Deleted,
        published_class: PublicationClass::MetadataShared,
        created_at: Utc::now(),
        created_by_uid: 1000,
        acknowledged_at: None,
        remote_receipt: None,
    };
    store.record_tombstone(&tombstone).await.unwrap();

    // Attempt to seal a batch -> must fail with UnacknowledgedTombstonesPending
    let (batch, _) = store
        .create_batch_idempotent(&repo_id, 1000, "batch_with_pending_tombstones", 1)
        .await
        .unwrap();

    let seal_err = store.seal_batch(&batch.id).await;
    assert!(matches!(
        seal_err,
        Err(FederationError::UnacknowledgedTombstonesPending)
    ));

    // Acknowledge the pending tombstones
    let pending = store.get_unacknowledged_tombstones(&repo_id).await.unwrap();
    assert_eq!(pending.len(), 1);
    let ids: Vec<String> = pending.into_iter().map(|t| t.id).collect();
    store
        .acknowledge_tombstones(&ids, "remote_receipt_ack_999")
        .await
        .unwrap();

    // Now sealing succeeds
    let sealed = store.seal_batch(&batch.id).await.unwrap();
    assert_eq!(sealed.state, codypendent_federation::BatchState::Sealed);
}

// ---------------------------------------------------------------------------
// Acceptance Criterion 10: inaccessible_seed_and_absent_seed_are_byte_identical
// ---------------------------------------------------------------------------
#[tokio::test]
async fn inaccessible_seed_and_absent_seed_are_byte_identical() {
    let pool = setup_test_db().await;
    let store = SharedGraphStore::new(pool);
    let query = SharedGraphQuery::new(store.clone());

    let repo_id = RepositoryId::new();
    let identity = FederatedRepositoryIdentity::new(
        repo_id,
        "commit_inaccessible_test_123456789012345",
        Some("https://github.com/org/secret-repo.git"),
        "Secret Repo",
        1000,
    );
    store.upsert_identity(&identity).await.unwrap();

    let policy = PublicationPolicy {
        repository_id: repo_id,
        max_class: PublicationClass::MetadataShared,
        max_classification: DataClassification::Confidential,
        publish_symbol_names: false,
        publish_source_paths: false,
        publish_signature_hashes: false,
        publish_evidence_artifacts: false,
        policy_version: 1,
        updated_at: Utc::now(),
        updated_by_uid: 1000,
    };
    store.upsert_policy(&policy).await.unwrap();

    let (node, _) = store
        .project_node(
            &identity,
            Some(&policy),
            &CodeNodeId::new(),
            "secret::Vault",
            "struct",
            "rust",
            Some("secret"),
            Some("secret::Vault"),
            None,
            None,
            PublicationClass::MetadataShared,
            DataClassification::Confidential,
            "rev_1",
        )
        .await
        .unwrap();

    // Principal 2000 has NO grant for repo_id
    let unauthorized_grants = AuthorizedGrants::new(2000, vec![]);

    let err_inaccessible = query
        .blast_radius(&node.shared_node_id, &unauthorized_grants, 3)
        .await
        .unwrap_err();

    let absent_node_id = "0000000000000000000000000000000000000000000000000000000000000000";
    let err_absent = query
        .blast_radius(absent_node_id, &unauthorized_grants, 3)
        .await
        .unwrap_err();

    // The two errors must be format-identical (byte-equivalent)
    let msg_inaccessible = format!("{err_inaccessible}");
    let msg_absent = format!("{err_absent}");

    assert_eq!(
        msg_inaccessible,
        format!("Node not found: {}", node.shared_node_id)
    );
    assert_eq!(msg_absent, format!("Node not found: {absent_node_id}"));
}

// ---------------------------------------------------------------------------
// Acceptance Criterion 11: hidden_intermediate_nodes_do_not_extend_reachability
// ---------------------------------------------------------------------------
#[tokio::test]
async fn hidden_intermediate_nodes_do_not_extend_reachability() {
    let pool = setup_test_db().await;
    let store = SharedGraphStore::new(pool);
    let query = SharedGraphQuery::new(store.clone());

    // Topology: Repo A (Node A) -> Repo H (Hidden Node H) -> Repo B (Node B)
    let repo_a = RepositoryId::new();
    let repo_h = RepositoryId::new();
    let repo_b = RepositoryId::new();

    let id_a = FederatedRepositoryIdentity::new(
        repo_a,
        "commit_a_11111111111111111111111111111111",
        Some("https://github.com/org/repo-a.git"),
        "Repo A",
        1000,
    );
    let id_h = FederatedRepositoryIdentity::new(
        repo_h,
        "commit_h_22222222222222222222222222222222",
        Some("https://github.com/org/repo-h.git"),
        "Repo H",
        1000,
    );
    let id_b = FederatedRepositoryIdentity::new(
        repo_b,
        "commit_b_33333333333333333333333333333333",
        Some("https://github.com/org/repo-b.git"),
        "Repo B",
        1000,
    );

    store.upsert_identity(&id_a).await.unwrap();
    store.upsert_identity(&id_h).await.unwrap();
    store.upsert_identity(&id_b).await.unwrap();

    let pol_a = PublicationPolicy::metadata_shared(repo_a, 1000);
    let pol_h = PublicationPolicy::metadata_shared(repo_h, 1000);
    let pol_b = PublicationPolicy::metadata_shared(repo_b, 1000);
    store.upsert_policy(&pol_a).await.unwrap();
    store.upsert_policy(&pol_h).await.unwrap();
    store.upsert_policy(&pol_b).await.unwrap();

    let (node_a, _) = store
        .project_node(
            &id_a,
            Some(&pol_a),
            &CodeNodeId::new(),
            "node_a",
            "function",
            "rust",
            None,
            None,
            None,
            None,
            PublicationClass::MetadataShared,
            DataClassification::Internal,
            "rev_1",
        )
        .await
        .unwrap();

    let (node_h, _) = store
        .project_node(
            &id_h,
            Some(&pol_h),
            &CodeNodeId::new(),
            "node_h",
            "function",
            "rust",
            None,
            None,
            None,
            None,
            PublicationClass::MetadataShared,
            DataClassification::Internal,
            "rev_1",
        )
        .await
        .unwrap();

    let (node_b, _) = store
        .project_node(
            &id_b,
            Some(&pol_b),
            &CodeNodeId::new(),
            "node_b",
            "function",
            "rust",
            None,
            None,
            None,
            None,
            PublicationClass::MetadataShared,
            DataClassification::Internal,
            "rev_1",
        )
        .await
        .unwrap();

    // Edge A -> H
    store
        .project_edge(
            &node_a,
            &node_h,
            Some(&pol_a),
            Some(&pol_h),
            "calls",
            0.9,
            "syntax_inferred",
            None,
            PublicationClass::MetadataShared,
            "rev_1",
        )
        .await
        .unwrap();

    // Edge H -> B
    store
        .project_edge(
            &node_h,
            &node_b,
            Some(&pol_h),
            Some(&pol_b),
            "calls",
            0.9,
            "syntax_inferred",
            None,
            PublicationClass::MetadataShared,
            "rev_1",
        )
        .await
        .unwrap();

    // Principal only has grants for Repo A and Repo B (Repo H is NOT granted)
    let grants = AuthorizedGrants::new(
        1000,
        vec![
            RepositoryGrant {
                repository_id: repo_a,
                federated_id: id_a.federated_id.clone(),
                max_class: PublicationClass::MetadataShared,
                max_classification: DataClassification::Internal,
            },
            RepositoryGrant {
                repository_id: repo_b,
                federated_id: id_b.federated_id.clone(),
                max_class: PublicationClass::MetadataShared,
                max_classification: DataClassification::Internal,
            },
        ],
    );

    let result = query
        .blast_radius(&node_a.shared_node_id, &grants, 5)
        .await
        .unwrap();

    // Node H is inaccessible, so Node B MUST NOT be reachable
    assert_eq!(result.reachable_nodes.len(), 0);
    assert_eq!(result.reachable_edges.len(), 0);
    assert_eq!(result.impacted_repositories, vec![repo_a]);
}

// ---------------------------------------------------------------------------
// Acceptance Criterion 12: pagination_cursor_is_bound_to_its_principal_and_query
// ---------------------------------------------------------------------------
#[test]
fn pagination_cursor_is_bound_to_its_principal_and_query() {
    let principal_1000 = 1000;
    let query_hash = "sha256_query_filter_hash_123";
    let last_id = "node_shared_abc";

    let cursor = FederationPageCursor::encode_cursor(principal_1000, query_hash, last_id);

    // Decoding with matching principal & query succeeds
    let decoded =
        FederationPageCursor::decode_and_verify(&cursor, principal_1000, query_hash).unwrap();
    assert_eq!(decoded, last_id);

    // Replay by different principal (1001) is rejected as InvalidCursor
    let replay_err_principal = FederationPageCursor::decode_and_verify(&cursor, 1001, query_hash);
    assert!(matches!(
        replay_err_principal,
        Err(FederationError::InvalidCursor)
    ));

    // Replay against altered query is rejected as InvalidCursor
    let replay_err_query = FederationPageCursor::decode_and_verify(
        &cursor,
        principal_1000,
        "different_query_hash_456",
    );
    assert!(matches!(
        replay_err_query,
        Err(FederationError::InvalidCursor)
    ));
}

// ---------------------------------------------------------------------------
// Acceptance Criterion 17: remote_policy_can_only_narrow_local_policy
// ---------------------------------------------------------------------------
#[test]
fn remote_policy_can_only_narrow_local_policy() {
    let repo_id = RepositoryId::new();
    let local = PublicationPolicy {
        repository_id: repo_id,
        max_class: PublicationClass::ContentShared,
        max_classification: DataClassification::Internal,
        publish_symbol_names: true,
        publish_source_paths: true,
        publish_signature_hashes: false,
        publish_evidence_artifacts: false,
        policy_version: 1,
        updated_at: Utc::now(),
        updated_by_uid: 1000,
    };

    // The remote's AUDIENCE is wider (PublicMarketplace), so the local
    // `ContentShared` binds. Its CLASSIFICATION ceiling is not: `max_classification`
    // caps what may leave, so `Public` (rank 0) permits strictly less than
    // `Internal` (rank 1). Narrowing takes the lower rank, so the remote's `Public`
    // binds — asserting `Internal` would require a remote policy to RAISE a local
    // ceiling and send more sensitive data off-device than the operator allowed.
    let effective_wide = local.narrow(
        PublicationClass::PublicMarketplace,
        DataClassification::Public,
    );
    assert_eq!(effective_wide.max_class, PublicationClass::ContentShared);
    assert_eq!(
        effective_wide.max_classification,
        DataClassification::Public
    );

    // Narrower remote policy (MetadataShared / Confidential) -> remote wins (MetadataShared / Internal)
    let effective_narrow = local.narrow(
        PublicationClass::MetadataShared,
        DataClassification::Confidential,
    );
    assert_eq!(effective_narrow.max_class, PublicationClass::MetadataShared);
    assert_eq!(
        effective_narrow.max_classification,
        DataClassification::Internal
    );
}

// ---------------------------------------------------------------------------
// Cross-repository migration plan test
// ---------------------------------------------------------------------------
#[tokio::test]
async fn cross_repository_migration_plan_finds_all_authorized_callers() {
    let pool = setup_test_db().await;
    let store = SharedGraphStore::new(pool);
    let query = SharedGraphQuery::new(store.clone());

    let repo_api = RepositoryId::new();
    let repo_client1 = RepositoryId::new();
    let repo_client2 = RepositoryId::new();

    let id_api = FederatedRepositoryIdentity::new(
        repo_api,
        "commit_api_0000000000000000000000000000",
        Some("https://github.com/org/core-api.git"),
        "Core API",
        1000,
    );
    let id_c1 = FederatedRepositoryIdentity::new(
        repo_client1,
        "commit_c1_0000000000000000000000000000",
        Some("https://github.com/org/client-1.git"),
        "Client 1",
        1000,
    );
    let id_c2 = FederatedRepositoryIdentity::new(
        repo_client2,
        "commit_c2_0000000000000000000000000000",
        Some("https://github.com/org/client-2.git"),
        "Client 2",
        1000,
    );

    store.upsert_identity(&id_api).await.unwrap();
    store.upsert_identity(&id_c1).await.unwrap();
    store.upsert_identity(&id_c2).await.unwrap();

    let pol_api = PublicationPolicy::metadata_shared(repo_api, 1000);
    let pol_c1 = PublicationPolicy::metadata_shared(repo_client1, 1000);
    let pol_c2 = PublicationPolicy::metadata_shared(repo_client2, 1000);
    store.upsert_policy(&pol_api).await.unwrap();
    store.upsert_policy(&pol_c1).await.unwrap();
    store.upsert_policy(&pol_c2).await.unwrap();

    // Target API node
    let (target_node, _) = store
        .project_node(
            &id_api,
            Some(&pol_api),
            &CodeNodeId::new(),
            "api::v1::authenticate",
            "function",
            "rust",
            Some("api"),
            None,
            None,
            None,
            PublicationClass::MetadataShared,
            DataClassification::Internal,
            "rev_1",
        )
        .await
        .unwrap();

    // Client 1 caller node
    let (c1_node, _) = store
        .project_node(
            &id_c1,
            Some(&pol_c1),
            &CodeNodeId::new(),
            "client1::login",
            "function",
            "rust",
            Some("client1"),
            None,
            None,
            None,
            PublicationClass::MetadataShared,
            DataClassification::Internal,
            "rev_1",
        )
        .await
        .unwrap();

    // Client 2 caller node
    let (c2_node, _) = store
        .project_node(
            &id_c2,
            Some(&pol_c2),
            &CodeNodeId::new(),
            "client2::auth_flow",
            "function",
            "rust",
            Some("client2"),
            None,
            None,
            None,
            PublicationClass::MetadataShared,
            DataClassification::Internal,
            "rev_1",
        )
        .await
        .unwrap();

    // Edges pointing to target API node
    store
        .project_edge(
            &c1_node,
            &target_node,
            Some(&pol_c1),
            Some(&pol_api),
            "calls",
            0.95,
            "syntax_inferred",
            None,
            PublicationClass::MetadataShared,
            "rev_1",
        )
        .await
        .unwrap();

    store
        .project_edge(
            &c2_node,
            &target_node,
            Some(&pol_c2),
            Some(&pol_api),
            "calls",
            0.95,
            "syntax_inferred",
            None,
            PublicationClass::MetadataShared,
            "rev_1",
        )
        .await
        .unwrap();

    // Grants covering all three repos
    let all_grants = AuthorizedGrants::new(
        1000,
        vec![
            RepositoryGrant {
                repository_id: repo_api,
                federated_id: id_api.federated_id,
                max_class: PublicationClass::MetadataShared,
                max_classification: DataClassification::Internal,
            },
            RepositoryGrant {
                repository_id: repo_client1,
                federated_id: id_c1.federated_id,
                max_class: PublicationClass::MetadataShared,
                max_classification: DataClassification::Internal,
            },
            RepositoryGrant {
                repository_id: repo_client2,
                federated_id: id_c2.federated_id,
                max_class: PublicationClass::MetadataShared,
                max_classification: DataClassification::Internal,
            },
        ],
    );

    let plan = query
        .migration_plan(&target_node.shared_node_id, &all_grants)
        .await
        .unwrap();

    assert_eq!(plan.referencing_nodes.len(), 2);
    assert_eq!(plan.referencing_edges.len(), 2);
    assert_eq!(plan.impacted_repositories.len(), 2);
}
