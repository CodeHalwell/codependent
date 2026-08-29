use std::sync::Arc;

use chrono::Utc;
use codypendent_codypendentd::control_plane_credentials::{
    persist_control_plane_token, rehydrate_control_plane_credentials,
};
use codypendent_control_plane::{
    auth::hash_token,
    build_router,
    store::{StreamEvent as StoredStreamEvent, WorkloadCredential},
    AppState, ControlPlaneConfig, Daemon, Membership, MemoryStorageDriver, MemoryStore,
    Organization, Repository, RoleGrant, Store, User,
};
use codypendent_control_plane_protocol::{
    DataClassification, PolicyUpdateEvent, PublicationClass, StreamEventPayload,
};
use codypendent_daemon::{
    control_plane_sync::{
        enqueue_session_summary, fetch_pending_deltas, get_policy_snapshot,
        get_repository_stream_cursor, record_pairing, ControlPlaneCredential, ControlPlanePairing,
        LocalConsentManifest, PairingState, SyncEngine,
    },
    db,
};
use tokio::net::TcpListener;
use uuid::Uuid;

const TEST_JWT_SECRET: &str = "daemon-sync-e2e-signing-key-0123456789abcdef";

#[tokio::test]
async fn startup_rehydration_pushes_and_pulls_against_the_real_router_and_honors_backoff() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = db::open_database(&temp.path().join("daemon.db"))
        .await
        .expect("local daemon database");

    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET)
        .expect("test control-plane config");
    let store = Arc::new(MemoryStore::new());
    let state = AppState::new(config, store.clone(), Arc::new(MemoryStorageDriver::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let endpoint = format!("http://{}", listener.local_addr().expect("address"));
    let server = tokio::spawn(async move {
        axum::serve(listener, build_router(state))
            .await
            .expect("serve control plane");
    });

    let organization_id = Uuid::now_v7();
    let repository_id = Uuid::now_v7();
    let daemon_id = Uuid::now_v7();
    let user_id = Uuid::now_v7();
    let now = Utc::now();
    store
        .create_organization(Organization {
            id: organization_id,
            slug: "e2e-org".to_string(),
            display_name: "E2E Org".to_string(),
            max_publication_class: "content-shared".to_string(),
            max_classification: "internal".to_string(),
            data_residency: None,
            retention_days: None,
            policy_version: 1,
            created_at: now,
        })
        .await
        .expect("organization");
    store
        .create_repository(Repository {
            id: repository_id,
            organization_id,
            federated_id: "a".repeat(64),
            display_name: "E2E Repository".to_string(),
            max_publication_class: "content-shared".to_string(),
            max_classification: "internal".to_string(),
            policy_version: 1,
            created_at: now,
        })
        .await
        .expect("repository");
    store
        .create_user(User {
            id: user_id,
            display_name: "Pairing User".to_string(),
            primary_email: None,
            state: "active".to_string(),
            created_at: now,
            updated_at: now,
        })
        .await
        .expect("user");
    store
        .add_membership(Membership {
            organization_id,
            user_id,
            state: "active".to_string(),
            joined_at: Some(now),
            created_at: now,
        })
        .await
        .expect("membership");
    store
        .create_role_grant(RoleGrant {
            id: Uuid::now_v7(),
            organization_id,
            user_id: Some(user_id),
            team_id: None,
            repository_id: None,
            role: "contributor".to_string(),
            action_scope: None,
            granted_by: user_id,
            granted_at: now,
            expires_at: None,
            revoked_at: None,
        })
        .await
        .expect("role grant");
    store
        .register_daemon(Daemon {
            id: daemon_id,
            organization_id,
            paired_by: user_id,
            display_name: "E2E Daemon".to_string(),
            consent_manifest_hash: vec![0; 32],
            max_publication_class: "metadata-shared".to_string(),
            accepts_remote_approvals: false,
            accepts_runner_dispatch: false,
            state: "active".to_string(),
            paired_at: Some(now),
            revoked_at: None,
            last_seen_at: Some(now),
            created_at: now,
        })
        .await
        .expect("daemon");

    let token = format!("cp_daemon_{}", Uuid::now_v7());
    let expires_at = now + chrono::Duration::days(30);
    store
        .save_workload_credential(WorkloadCredential {
            id: Uuid::now_v7(),
            daemon_id,
            audience: "control-plane".to_string(),
            purpose: "sync".to_string(),
            token_hash: hash_token(&token),
            rotated_from: None,
            issued_at: now,
            expires_at,
            revoked_at: None,
        })
        .await
        .expect("workload credential");
    store
        .append_stream_event(StoredStreamEvent {
            id: 0,
            organization_id,
            repository_id: None,
            stream: "policy".to_string(),
            payload: serde_json::to_value(StreamEventPayload::PolicyUpdate(PolicyUpdateEvent {
                policy_version: 7,
                max_publication_class: PublicationClass::MetadataShared,
                max_classification: DataClassification::Internal,
            }))
            .expect("serialize policy event"),
            created_at: now,
        })
        .await
        .expect("organization policy event");

    let pairing_id = daemon_id.to_string();
    let consent = LocalConsentManifest {
        organization_id: organization_id.to_string(),
        organization_display_name: "E2E Org".to_string(),
        endpoint: endpoint.clone(),
        max_publication_class: PublicationClass::MetadataShared,
        accepts_remote_approvals: false,
        accepts_runner_dispatch: false,
        allowed_repositories: vec!["a".repeat(64)],
        created_at: now,
    };
    let persisted = persist_control_plane_token(temp.path(), &pairing_id, &token)
        .expect("persist workload token in owner-only AuthStore");
    record_pairing(
        &pool,
        &ControlPlanePairing {
            id: pairing_id.clone(),
            owner_uid: 501,
            endpoint,
            organization_id: organization_id.to_string(),
            organization_display_name: "E2E Org".to_string(),
            consent_manifest: serde_json::to_string(&consent).expect("serialize consent"),
            consent_manifest_hash: consent.compute_hash(),
            max_publication_class: PublicationClass::MetadataShared,
            accepts_remote_approvals: false,
            accepts_runner_dispatch: false,
            state: PairingState::Active,
            paired_at: Some(now),
            expires_at: Some(expires_at),
            revoked_at: None,
            revoked_reason: None,
            created_at: now,
        },
        &ControlPlaneCredential {
            pairing_id: pairing_id.clone(),
            credential_ref: persisted.credential_ref,
            credential_hash: persisted.credential_hash,
            audience: "control-plane".to_string(),
            purpose: "sync".to_string(),
            issued_at: now,
            expires_at,
            rotated_at: None,
        },
    )
    .await
    .expect("local pairing");

    let repository_key = repository_id.to_string();
    enqueue_session_summary(
        &pool,
        &pairing_id,
        PublicationClass::MetadataShared,
        "session-1",
        Some(&repository_key),
        "completed",
        now,
        Some(now),
        Some("redacted below content-shared"),
        PublicationClass::MetadataShared,
    )
    .await
    .expect("enqueue first delta")
    .expect("first delta is new");

    // A new engine has an empty process-local cache, exactly as it does after
    // daemon restart. It must refuse before network I/O even though an active
    // pairing and pending delta exist.
    let unhydrated = SyncEngine::new(pool.clone());
    let error = unhydrated
        .sync_pairing_once(&pairing_id)
        .await
        .expect_err("an unhydrated engine must fail closed");
    assert!(matches!(
        error,
        codypendent_daemon::control_plane_sync::ControlPlaneSyncError::CredentialUnavailable(_)
    ));
    assert_eq!(
        fetch_pending_deltas(&pool, &pairing_id, 10)
            .await
            .expect("pending after refused unhydrated attempt")
            .len(),
        1
    );

    // Rehydration is therefore required for this request to carry any
    // credentials at all.
    let engine = SyncEngine::new(pool.clone());
    let rehydrated = rehydrate_control_plane_credentials(&pool, temp.path(), &engine)
        .await
        .expect("rehydrate startup credentials");
    assert_eq!(rehydrated.loaded, 1);
    assert_eq!(rehydrated.unavailable, 0);

    assert_eq!(engine.sync_all_active_once().await.expect("first sync"), 1);
    assert!(fetch_pending_deltas(&pool, &pairing_id, 10)
        .await
        .expect("pending after push")
        .is_empty());
    assert_eq!(
        get_repository_stream_cursor(&pool, &pairing_id, &repository_key, "sync")
            .await
            .expect("sync cursor")
            .as_deref(),
        Some("2"),
        "the event written by push is pulled back through the real Axum route"
    );
    assert_eq!(
        get_repository_stream_cursor(&pool, &pairing_id, "", "policy")
            .await
            .expect("organization policy cursor")
            .as_deref(),
        Some("1")
    );
    let policy = get_policy_snapshot(&pool, &pairing_id)
        .await
        .expect("policy snapshot lookup")
        .expect("organization policy event applied");
    assert_eq!(policy.policy_version, 7);
    assert_eq!(policy.max_classification, DataClassification::Internal);

    enqueue_session_summary(
        &pool,
        &pairing_id,
        PublicationClass::MetadataShared,
        "session-2",
        Some(&repository_key),
        "completed",
        now,
        Some(now),
        None,
        PublicationClass::MetadataShared,
    )
    .await
    .expect("enqueue second delta")
    .expect("second delta is new");
    engine.record_failure_backoff(&pairing_id).await;
    assert_eq!(
        engine
            .sync_all_active_once()
            .await
            .expect("backoff-gated sync"),
        0,
        "a recorded pairing deadline prevents the startup worker from reaching Axum"
    );
    assert_eq!(
        fetch_pending_deltas(&pool, &pairing_id, 10)
            .await
            .expect("pending while deferred")
            .len(),
        1
    );

    engine.reset_backoff(&pairing_id).await;
    assert_eq!(engine.sync_all_active_once().await.expect("retry sync"), 1);
    assert!(fetch_pending_deltas(&pool, &pairing_id, 10)
        .await
        .expect("pending after retry")
        .is_empty());

    server.abort();
}
