//! Integration tests for daemon control plane synchronization.

use std::sync::Arc;

use axum::{
    extract::Query,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use codypendent_control_plane_protocol::{
    DataClassification, PolicyRestrictions, PolicySnapshot, PolicyUpdateEvent, PublicationClass,
    Sha256Digest, StreamEvent, StreamEventPayload, StreamKind,
};
use codypendent_daemon::{
    control_plane_sync::{
        compute_effective_policy, enqueue_artifact_summary, enqueue_run_summary,
        enqueue_session_summary, enqueue_tombstone, fetch_pending_deltas, get_pairing,
        get_stream_cursor, has_inbound_receipt, list_active_pairings, record_inbound_receipt,
        record_pairing, revoke_pairing, set_stream_cursor, store_policy_snapshot,
        ControlPlaneCredential, ControlPlanePairing, InboundReceipt, LocalConsentManifest,
        PairingState, SyncDeltaPushRequest, SyncDeltaPushResponse, SyncEngine,
    },
    db,
};
use tokio::net::TcpListener;
use uuid::Uuid;

async fn setup_test_db() -> (tempfile::TempDir, sqlx::SqlitePool) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("codypendent-test.db");
    let pool = db::open_database(&db_path).await.expect("open db");
    (tmp, pool)
}

#[tokio::test]
async fn pairing_lifecycle_and_consent_manifest() {
    let (_tmp, pool) = setup_test_db().await;

    let manifest = LocalConsentManifest {
        organization_id: "org_123".to_string(),
        organization_display_name: "Acme Corp".to_string(),
        endpoint: "https://control-plane.acme.corp".to_string(),
        max_publication_class: PublicationClass::MetadataShared,
        accepts_remote_approvals: false,
        accepts_runner_dispatch: false,
        allowed_repositories: vec!["repo_abc".to_string()],
        created_at: Utc::now(),
    };

    let manifest_json = serde_json::to_string(&manifest).unwrap();
    let manifest_hash = manifest.compute_hash();
    assert_eq!(manifest_hash.len(), 64);

    let pairing_id = Uuid::now_v7().to_string();
    let pairing = ControlPlanePairing {
        id: pairing_id.clone(),
        owner_uid: 501,
        endpoint: "https://control-plane.acme.corp".to_string(),
        organization_id: "org_123".to_string(),
        organization_display_name: "Acme Corp".to_string(),
        consent_manifest: manifest_json,
        consent_manifest_hash: manifest_hash,
        max_publication_class: PublicationClass::MetadataShared,
        accepts_remote_approvals: false,
        accepts_runner_dispatch: false,
        state: PairingState::Active,
        paired_at: Some(Utc::now()),
        expires_at: None,
        revoked_at: None,
        revoked_reason: None,
        created_at: Utc::now(),
    };

    let cred = ControlPlaneCredential {
        pairing_id: pairing_id.clone(),
        credential_ref: format!("keychain:codypendent.control-plane.{pairing_id}"),
        credential_hash: "0000000000000000000000000000000000000000000000000000000000000000"
            .to_string(),
        audience: "https://control-plane.acme.corp".to_string(),
        purpose: "sync".to_string(),
        issued_at: Utc::now(),
        expires_at: Utc::now() + chrono::Duration::days(30),
        rotated_at: None,
    };

    record_pairing(&pool, &pairing, &cred).await.unwrap();

    let fetched = get_pairing(&pool, &pairing_id)
        .await
        .unwrap()
        .expect("pairing exists");
    assert_eq!(fetched.id, pairing_id);
    assert_eq!(fetched.state, PairingState::Active);
    assert_eq!(
        fetched.max_publication_class,
        PublicationClass::MetadataShared
    );

    let active = list_active_pairings(&pool).await.unwrap();
    assert_eq!(active.len(), 1);

    // Revoke pairing
    revoke_pairing(&pool, &pairing_id, "user revoked access")
        .await
        .unwrap();
    let revoked = get_pairing(&pool, &pairing_id).await.unwrap().unwrap();
    assert_eq!(revoked.state, PairingState::Revoked);
    assert_eq!(
        revoked.revoked_reason.as_deref(),
        Some("user revoked access")
    );

    let active_after = list_active_pairings(&pool).await.unwrap();
    assert_eq!(active_after.len(), 0);
}

#[tokio::test]
async fn outbox_enqueue_redaction_and_monotonic_sequence() {
    let (_tmp, pool) = setup_test_db().await;
    let pairing_id = Uuid::now_v7().to_string();

    let pairing = ControlPlanePairing {
        id: pairing_id.clone(),
        owner_uid: 501,
        endpoint: "https://control-plane.test".to_string(),
        organization_id: "org_test".to_string(),
        organization_display_name: "Test Org".to_string(),
        consent_manifest: "{}".to_string(),
        consent_manifest_hash: "1111111111111111111111111111111111111111111111111111111111111111"
            .to_string(),
        max_publication_class: PublicationClass::MetadataShared,
        accepts_remote_approvals: false,
        accepts_runner_dispatch: false,
        state: PairingState::Active,
        paired_at: Some(Utc::now()),
        expires_at: None,
        revoked_at: None,
        revoked_reason: None,
        created_at: Utc::now(),
    };
    let cred = ControlPlaneCredential {
        pairing_id: pairing_id.clone(),
        credential_ref: "keychain:test".to_string(),
        credential_hash: "2222222222222222222222222222222222222222222222222222222222222222"
            .to_string(),
        audience: "https://control-plane.test".to_string(),
        purpose: "sync".to_string(),
        issued_at: Utc::now(),
        expires_at: Utc::now() + chrono::Duration::days(1),
        rotated_at: None,
    };
    record_pairing(&pool, &pairing, &cred).await.unwrap();

    // 1. Session summary with requested class ContentShared, but pairing max is MetadataShared
    let _outbox_id1 = enqueue_session_summary(
        &pool,
        &pairing_id,
        pairing.max_publication_class,
        "sess_001",
        Some("repo_001"),
        "completed",
        Utc::now(),
        Some(Utc::now()),
        Some("Top Secret Title"),
        PublicationClass::ContentShared,
    )
    .await
    .unwrap()
    .expect("enqueued delta 1");

    // 2. PrivateLocal data must NOT be enqueued (returns None)
    let outbox_id_private = enqueue_session_summary(
        &pool,
        &pairing_id,
        pairing.max_publication_class,
        "sess_private",
        Some("repo_001"),
        "active",
        Utc::now(),
        None,
        Some("Local Title"),
        PublicationClass::PrivateLocal,
    )
    .await
    .unwrap();
    assert!(
        outbox_id_private.is_none(),
        "PrivateLocal delta must not be enqueued to outbox"
    );

    // 3. Enqueue run summary, artifact summary, graph batch, audit event, tombstone
    // Captured so the duplicate below can replay the IDENTICAL payload: the
    // outbox deduplicates on `payload_hash`, so fresh timestamps would make it
    // a genuinely different delta rather than a duplicate.
    let run_started_at = Utc::now();
    let run_completed_at = Utc::now();
    let _outbox_id2 = enqueue_run_summary(
        &pool,
        &pairing_id,
        pairing.max_publication_class,
        "run_001",
        "sess_001",
        Some("repo_001"),
        "succeeded",
        run_started_at,
        Some(run_completed_at),
        Some("ok"),
        PublicationClass::MetadataShared,
    )
    .await
    .unwrap()
    .expect("enqueued delta 2");

    let _outbox_id3 = enqueue_artifact_summary(
        &pool,
        &pairing_id,
        pairing.max_publication_class,
        "art_001",
        Some("repo_001"),
        "report.pdf",
        "sha256:abcd",
        1024,
        "application/pdf",
        PublicationClass::MetadataShared,
    )
    .await
    .unwrap()
    .expect("enqueued delta 3");

    let _outbox_id4 = enqueue_tombstone(
        &pool,
        &pairing_id,
        pairing.max_publication_class,
        "session",
        "sess_old",
        "deleted",
    )
    .await
    .unwrap()
    .expect("enqueued delta 4");

    // Verify pending deltas
    let pending = fetch_pending_deltas(&pool, &pairing_id, 10).await.unwrap();
    assert_eq!(pending.len(), 4);

    // Verify sequence monotonicity (1, 2, 3, 4)
    assert_eq!(pending[0].sequence, 1);
    assert_eq!(pending[1].sequence, 2);
    assert_eq!(pending[2].sequence, 3);
    assert_eq!(pending[3].sequence, 4);

    // Verify redaction at enqueue time: session title is NULL under metadata-shared
    assert_eq!(pending[0].class, PublicationClass::MetadataShared);
    assert_eq!(pending[0].payload["title"], serde_json::Value::Null);

    // Verify duplicate enqueue is deduplicated safely
    let dup_id = enqueue_run_summary(
        &pool,
        &pairing_id,
        pairing.max_publication_class,
        "run_001",
        "sess_001",
        Some("repo_001"),
        "succeeded",
        run_started_at,
        Some(run_completed_at),
        Some("ok"),
        PublicationClass::MetadataShared,
    )
    .await
    .unwrap();
    assert!(dup_id.is_none(), "Duplicate outbox item should be ignored");
}

#[tokio::test]
async fn inbound_receipt_and_stream_cursor_idempotency() {
    let (_tmp, pool) = setup_test_db().await;
    let pairing_id = Uuid::now_v7().to_string();

    let pairing = ControlPlanePairing {
        id: pairing_id.clone(),
        owner_uid: 501,
        endpoint: "https://control-plane.test".to_string(),
        organization_id: "org_test".to_string(),
        organization_display_name: "Test Org".to_string(),
        consent_manifest: "{}".to_string(),
        consent_manifest_hash: "1111111111111111111111111111111111111111111111111111111111111111"
            .to_string(),
        max_publication_class: PublicationClass::MetadataShared,
        accepts_remote_approvals: false,
        accepts_runner_dispatch: false,
        state: PairingState::Active,
        paired_at: Some(Utc::now()),
        expires_at: None,
        revoked_at: None,
        revoked_reason: None,
        created_at: Utc::now(),
    };
    let cred = ControlPlaneCredential {
        pairing_id: pairing_id.clone(),
        credential_ref: "keychain:test".to_string(),
        credential_hash: "2222222222222222222222222222222222222222222222222222222222222222"
            .to_string(),
        audience: "https://control-plane.test".to_string(),
        purpose: "sync".to_string(),
        issued_at: Utc::now(),
        expires_at: Utc::now() + chrono::Duration::days(1),
        rotated_at: None,
    };
    record_pairing(&pool, &pairing, &cred).await.unwrap();

    let remote_msg_id = "msg_456";
    assert!(!has_inbound_receipt(&pool, &pairing_id, remote_msg_id)
        .await
        .unwrap());

    let receipt = InboundReceipt {
        pairing_id: pairing_id.clone(),
        remote_message_id: remote_msg_id.to_string(),
        message_kind: "approval".to_string(),
        local_effect_id: Some("approval_effect_1".to_string()),
        outcome_hash: "3333333333333333333333333333333333333333333333333333333333333333"
            .to_string(),
        received_at: Utc::now(),
    };

    record_inbound_receipt(&pool, &receipt).await.unwrap();
    assert!(has_inbound_receipt(&pool, &pairing_id, remote_msg_id)
        .await
        .unwrap());

    // Test stream cursor
    assert!(get_stream_cursor(&pool, &pairing_id, "approvals")
        .await
        .unwrap()
        .is_none());
    set_stream_cursor(&pool, &pairing_id, "approvals", "42")
        .await
        .unwrap();
    assert_eq!(
        get_stream_cursor(&pool, &pairing_id, "approvals")
            .await
            .unwrap()
            .as_deref(),
        Some("42")
    );
}

#[tokio::test]
async fn policy_snapshot_and_effective_narrowing() {
    let (_tmp, pool) = setup_test_db().await;
    let pairing_id = Uuid::now_v7().to_string();

    let pairing = ControlPlanePairing {
        id: pairing_id.clone(),
        owner_uid: 501,
        endpoint: "https://control-plane.test".to_string(),
        organization_id: "org_test".to_string(),
        organization_display_name: "Test Org".to_string(),
        consent_manifest: "{}".to_string(),
        consent_manifest_hash: "1111111111111111111111111111111111111111111111111111111111111111"
            .to_string(),
        max_publication_class: PublicationClass::ContentShared,
        accepts_remote_approvals: false,
        accepts_runner_dispatch: false,
        state: PairingState::Active,
        paired_at: Some(Utc::now()),
        expires_at: None,
        revoked_at: None,
        revoked_reason: None,
        created_at: Utc::now(),
    };
    let cred = ControlPlaneCredential {
        pairing_id: pairing_id.clone(),
        credential_ref: "keychain:test".to_string(),
        credential_hash: "2222222222222222222222222222222222222222222222222222222222222222"
            .to_string(),
        audience: "https://control-plane.test".to_string(),
        purpose: "sync".to_string(),
        issued_at: Utc::now(),
        expires_at: Utc::now() + chrono::Duration::days(1),
        rotated_at: None,
    };
    record_pairing(&pool, &pairing, &cred).await.unwrap();

    let mut restrictions = PolicyRestrictions::default();
    restrictions
        .denied_providers
        .push("untrusted-provider".to_string());
    restrictions
        .denied_models
        .push("experimental-999".to_string());
    restrictions.denied_regions.push("eu-west-9".to_string());
    restrictions
        .denied_integrations
        .push("unvetted-plugin".to_string());

    let snapshot = PolicySnapshot {
        policy_version: 5,
        max_publication_class: PublicationClass::MetadataShared, // Narrower than local ContentShared
        max_classification: DataClassification::Internal,
        restrictions,
        received_at: Utc::now(),
        payload_hash: Sha256Digest(
            "4444444444444444444444444444444444444444444444444444444444444444".to_string(),
        ),
    };

    store_policy_snapshot(&pool, &pairing_id, &snapshot)
        .await
        .unwrap();

    let effective = compute_effective_policy(
        &pool,
        &pairing_id,
        PublicationClass::ContentShared,
        DataClassification::Confidential,
    )
    .await
    .unwrap();

    // Strictest (narrowest) wins: MetadataShared < ContentShared
    assert_eq!(
        effective.publication_class,
        PublicationClass::MetadataShared
    );
    // Strictest classification wins: Internal < Confidential
    assert_eq!(effective.classification, DataClassification::Internal);

    // Check restrictions
    assert!(!effective.is_provider_allowed("untrusted-provider"));
    assert!(effective.is_provider_allowed("openai"));
    assert!(!effective.is_model_allowed("experimental-999"));
    assert!(effective.is_model_allowed("claude-3-5-sonnet"));
    assert!(!effective.is_region_allowed("eu-west-9"));
    assert!(effective.is_region_allowed("us-east-1"));
    assert!(!effective.is_integration_allowed("unvetted-plugin"));
    assert!(effective.is_integration_allowed("github"));
}

#[tokio::test]
async fn sync_engine_offline_and_mock_server_sync() {
    let (_tmp, pool) = setup_test_db().await;

    // 1. Unpaired test: sync_all_active_once does zero work
    let engine = SyncEngine::new(pool.clone());
    let synced_count = engine.sync_all_active_once().await.unwrap();
    assert_eq!(
        synced_count, 0,
        "Unpaired daemon must perform 0 sync operations"
    );

    // 2. Setup mock control plane server
    let pushed_deltas_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let pushed_counter = pushed_deltas_count.clone();

    let mock_app = Router::new()
        .route(
            "/v1/sync/push",
            post(move |Json(req): Json<SyncDeltaPushRequest>| {
                let counter = pushed_counter.clone();
                async move {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Json(SyncDeltaPushResponse {
                        receipt_id: Uuid::now_v7(),
                        daemon_sequence: req.daemon_sequence,
                        accepted_at: Utc::now(),
                        duplicate: false,
                    })
                }
            }),
        )
        .route(
            "/v1/sync/events",
            get(
                |Query(params): Query<std::collections::HashMap<String, String>>| async move {
                    let stream = params.get("stream").map(|s| s.as_str()).unwrap_or("sync");
                    if stream == "policy" {
                        let event = StreamEvent {
                            id: 1,
                            organization_id: codypendent_control_plane_protocol::OrganizationId(
                                Uuid::now_v7(),
                            ),
                            repository_id: None,
                            stream: StreamKind::Policy,
                            payload: StreamEventPayload::PolicyUpdate(PolicyUpdateEvent {
                                policy_version: 2,
                                max_publication_class: PublicationClass::MetadataShared,
                                max_classification: DataClassification::Internal,
                            }),
                            created_at: Utc::now(),
                        };
                        Json(vec![event])
                    } else {
                        Json(vec![])
                    }
                },
            ),
        );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, mock_app).await.unwrap();
    });

    let endpoint = format!("http://{}", addr);

    // 3. Register pairing with mock server endpoint
    let pairing_id = Uuid::now_v7().to_string();
    let pairing = ControlPlanePairing {
        id: pairing_id.clone(),
        owner_uid: 501,
        endpoint: endpoint.clone(),
        organization_id: "org_mock".to_string(),
        organization_display_name: "Mock Org".to_string(),
        consent_manifest: "{}".to_string(),
        consent_manifest_hash: "5555555555555555555555555555555555555555555555555555555555555555"
            .to_string(),
        max_publication_class: PublicationClass::MetadataShared,
        accepts_remote_approvals: false,
        accepts_runner_dispatch: false,
        state: PairingState::Active,
        paired_at: Some(Utc::now()),
        expires_at: None,
        revoked_at: None,
        revoked_reason: None,
        created_at: Utc::now(),
    };
    let cred = ControlPlaneCredential {
        pairing_id: pairing_id.clone(),
        credential_ref: "keychain:mock".to_string(),
        credential_hash: "6666666666666666666666666666666666666666666666666666666666666666"
            .to_string(),
        audience: endpoint.clone(),
        purpose: "sync".to_string(),
        issued_at: Utc::now(),
        expires_at: Utc::now() + chrono::Duration::days(1),
        rotated_at: None,
    };
    record_pairing(&pool, &pairing, &cred).await.unwrap();
    engine
        .set_pairing_token(&pairing_id, "mock_token_123")
        .await;

    // Enqueue two outbox items
    enqueue_session_summary(
        &pool,
        &pairing_id,
        PublicationClass::MetadataShared,
        "sess_mock_1",
        None,
        "active",
        Utc::now(),
        None,
        None,
        PublicationClass::MetadataShared,
    )
    .await
    .unwrap();

    enqueue_run_summary(
        &pool,
        &pairing_id,
        PublicationClass::MetadataShared,
        "run_mock_1",
        "sess_mock_1",
        None,
        "running",
        Utc::now(),
        None,
        None,
        PublicationClass::MetadataShared,
    )
    .await
    .unwrap();

    // 4. Run sync cycle
    let summary = engine.sync_pairing_once(&pairing_id).await.unwrap();
    assert_eq!(summary.pushed_deltas, 2);
    assert_eq!(summary.acknowledged_deltas, 2);
    assert_eq!(summary.pulled_events, 1);
    assert_eq!(
        pushed_deltas_count.load(std::sync::atomic::Ordering::SeqCst),
        2
    );

    // Verify outbox is now empty of pending items
    let pending_after = fetch_pending_deltas(&pool, &pairing_id, 10).await.unwrap();
    assert_eq!(pending_after.len(), 0);

    // Verify inbound policy event was applied
    let policy_snapshot =
        codypendent_daemon::control_plane_sync::get_policy_snapshot(&pool, &pairing_id)
            .await
            .unwrap()
            .expect("policy snapshot stored from inbound stream");
    assert_eq!(policy_snapshot.policy_version, 2);

    server_handle.abort();
}
