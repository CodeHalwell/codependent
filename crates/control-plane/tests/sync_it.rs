use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
};
use codypendent_control_plane::{
    auth::create_daemon_token, build_router, store::Daemon, AppState, ControlPlaneConfig,
    MemoryStorageDriver, MemoryStore, Organization, Repository, RoleGrant, Store,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

/// Explicit signing secret: there is no default secret to fall back on.
const TEST_JWT_SECRET: &str = "ctrl-plane-unit-test-signing-key-0123456789abcdef";

#[tokio::test]
async fn sync_push_pull_and_class_redaction() {
    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET)
        .expect("test signing secret must be accepted");
    let store = Arc::new(MemoryStore::new());
    let storage = Arc::new(MemoryStorageDriver::new());
    let state = AppState::new(config.clone(), store.clone(), storage);
    let app = build_router(state);

    let org_id = Uuid::now_v7();
    let repo_id = Uuid::now_v7();
    let daemon_id = Uuid::now_v7();
    let user_id = Uuid::now_v7();

    // The organization and repository must exist: a push names a repository and
    // the effective publication class is organization ∩ repository ∩ daemon.
    store
        .create_organization(Organization {
            id: org_id,
            slug: "acme".to_string(),
            display_name: "Acme".to_string(),
            max_publication_class: "content-shared".to_string(),
            max_classification: "internal".to_string(),
            data_residency: None,
            retention_days: None,
            policy_version: 1,
            created_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    store
        .create_repository(Repository {
            id: repo_id,
            organization_id: org_id,
            federated_id: "a".repeat(64),
            display_name: "Core Engine".to_string(),
            max_publication_class: "content-shared".to_string(),
            max_classification: "internal".to_string(),
            policy_version: 1,
            created_at: chrono::Utc::now(),
        })
        .await
        .unwrap();

    // The daemon borrows its authority from the user who paired it, so that user
    // must actually hold a grant in the organization. Without this the daemon has
    // no authority at all and every request is a 404.
    store
        .create_role_grant(RoleGrant {
            id: Uuid::now_v7(),
            organization_id: org_id,
            user_id: Some(user_id),
            team_id: None,
            repository_id: None,
            role: "contributor".to_string(),
            action_scope: None,
            granted_by: user_id,
            granted_at: chrono::Utc::now(),
            expires_at: None,
            revoked_at: None,
        })
        .await
        .unwrap();

    // Register metadata-shared daemon
    let daemon = Daemon {
        id: daemon_id,
        organization_id: org_id,
        paired_by: user_id,
        display_name: "Test Daemon".to_string(),
        consent_manifest_hash: vec![0u8; 32],
        max_publication_class: "metadata-shared".to_string(),
        accepts_remote_approvals: false,
        accepts_runner_dispatch: false,
        state: "active".to_string(),
        paired_at: Some(chrono::Utc::now()),
        revoked_at: None,
        last_seen_at: Some(chrono::Utc::now()),
        created_at: chrono::Utc::now(),
    };
    store.register_daemon(daemon).await.unwrap();

    let daemon_token = create_daemon_token(
        daemon_id,
        org_id,
        user_id,
        "metadata-shared".to_string(),
        &config.jwt_secret,
        3600,
    )
    .unwrap();

    // 1. Daemon pushes session-summary delta claiming content-shared, but daemon is capped at metadata-shared
    let payload = serde_json::json!({
        "title": "Secret Session Title",
        "state": "completed"
    });
    let payload_bytes = serde_json::to_vec(&payload).unwrap();
    let payload_hash = hex::encode(Sha256::digest(&payload_bytes));

    let push_req = Request::builder()
        .uri("/v1/sync/push")
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {daemon_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "daemon_sequence": 1,
                "delta_kind": "session-summary",
                "repository_id": repo_id,
                "subject_id": "sess_123",
                "class": "content-shared",
                "payload": payload,
                "payload_hash": payload_hash
            }))
            .unwrap(),
        ))
        .unwrap();

    let res = app.clone().oneshot(push_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let push_json: serde_json::Value =
        serde_json::from_slice(&to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(push_json["duplicate"], false);
    assert_eq!(push_json["daemon_sequence"], 1);

    // Verify session in store has title = NULL (redacted because effective class was metadata-shared)
    let sessions = store
        .list_shared_sessions(org_id, Some(repo_id), 10)
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].title, None,
        "Session title must be redacted when publication class is metadata-shared"
    );
    assert_eq!(sessions[0].class, "metadata-shared");

    // 2. Duplicate sync push returns idempotent response with duplicate = true
    let dup_push_req = Request::builder()
        .uri("/v1/sync/push")
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {daemon_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "daemon_sequence": 1,
                "delta_kind": "session-summary",
                "repository_id": repo_id,
                "subject_id": "sess_123",
                "class": "content-shared",
                "payload": serde_json::json!({"state": "completed"}),
                "payload_hash": payload_hash
            }))
            .unwrap(),
        ))
        .unwrap();

    let res_dup = app.clone().oneshot(dup_push_req).await.unwrap();
    assert_eq!(res_dup.status(), StatusCode::OK);
    let dup_json: serde_json::Value =
        serde_json::from_slice(&to_bytes(res_dup.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(dup_json["duplicate"], true);

    // 3. Pull sync events, scoped to the repository the daemon is authorized on
    let pull_req = Request::builder()
        .uri(format!(
            "/v1/sync/pull?repository_id={repo_id}&stream=sync&after_id=0"
        ))
        .method("GET")
        .header(header::AUTHORIZATION, format!("Bearer {daemon_token}"))
        .body(Body::empty())
        .unwrap();

    let res_pull = app.oneshot(pull_req).await.unwrap();
    assert_eq!(res_pull.status(), StatusCode::OK);
    let events: Vec<serde_json::Value> =
        serde_json::from_slice(&to_bytes(res_pull.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(!events.is_empty());
}
