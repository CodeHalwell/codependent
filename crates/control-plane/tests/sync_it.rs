use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
};
use codypendent_control_plane::{
    auth::create_daemon_token, build_router, store::Daemon, AppState, ControlPlaneConfig,
    Membership, MemoryStorageDriver, MemoryStore, Organization, Repository, RoleGrant, Store, User,
};
use codypendent_control_plane_protocol::{
    ids::{DaemonId, OrganizationId, RepositoryId, Sha256Digest},
    sync::{SyncDelta, SyncDeltaKind, SyncEnvelope},
    PublicationClass, CONTROL_PLANE_PROTOCOL_V1,
};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

/// Explicit signing secret: there is no default secret to fall back on.
const TEST_JWT_SECRET: &str = "ctrl-plane-unit-test-signing-key-0123456789abcdef";

/// A one-delta envelope, built from the protocol types rather than hand-rolled
/// JSON: the point of the route is that a daemon serializing `SyncEnvelope`
/// is accepted verbatim.
fn envelope(daemon_id: Uuid, org_id: Uuid, deltas: Vec<SyncDelta>) -> SyncEnvelope {
    SyncEnvelope {
        protocol_version: CONTROL_PLANE_PROTOCOL_V1,
        daemon_id: DaemonId::from_uuid(daemon_id),
        organization_id: OrganizationId::from_uuid(org_id),
        sent_at: chrono::Utc::now(),
        deltas,
    }
}

fn session_delta(
    sequence: u64,
    repo_id: Uuid,
    subject_id: &str,
    class: PublicationClass,
    payload: serde_json::Value,
) -> SyncDelta {
    let payload_hash = Sha256Digest::from_bytes(&serde_json::to_vec(&payload).unwrap());
    SyncDelta {
        id: format!("delta-{sequence}"),
        sequence,
        kind: SyncDeltaKind::SessionSummary,
        repository_id: Some(RepositoryId::from_uuid(repo_id)),
        subject_id: subject_id.to_string(),
        payload,
        class,
        payload_hash,
        created_at: chrono::Utc::now(),
    }
}

fn push_request(token: &str, envelope: &SyncEnvelope) -> Request<Body> {
    Request::builder()
        .uri("/v1/sync/push")
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(envelope).unwrap()))
        .unwrap()
}

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
    let now = chrono::Utc::now();
    store
        .create_user(User {
            id: user_id,
            display_name: "Pairing user".to_string(),
            primary_email: None,
            state: "active".to_string(),
            created_at: now,
            updated_at: now,
        })
        .await
        .unwrap();
    store
        .add_membership(Membership {
            organization_id: org_id,
            user_id,
            state: "active".to_string(),
            joined_at: Some(now),
            created_at: now,
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

    let push_req = push_request(
        &daemon_token,
        &envelope(
            daemon_id,
            org_id,
            vec![session_delta(
                1,
                repo_id,
                "sess_123",
                PublicationClass::ContentShared,
                payload.clone(),
            )],
        ),
    );

    let res = app.clone().oneshot(push_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let push_json: serde_json::Value =
        serde_json::from_slice(&to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap();
    let receipts = push_json["receipts"].as_array().unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0]["duplicate"], false);
    assert_eq!(receipts[0]["daemon_sequence"], 1);
    // The receipt reports the class the control plane actually stored, which is
    // narrower than the one the daemon asked for.
    assert_eq!(receipts[0]["class"], "metadata-shared");
    assert!(push_json["rejected_deltas"].as_array().unwrap().is_empty());
    assert_eq!(push_json["latest_sequence"], 1);

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

    // A syntactically valid digest is still not an integrity proof unless it
    // actually covers the payload accepted by the control plane.
    let mut forged = session_delta(
        2,
        repo_id,
        "sess_forged",
        PublicationClass::MetadataShared,
        serde_json::json!({ "state": "completed" }),
    );
    forged.payload_hash = Sha256Digest("0".repeat(64));
    let forged_res = app
        .clone()
        .oneshot(push_request(
            &daemon_token,
            &envelope(daemon_id, org_id, vec![forged]),
        ))
        .await
        .unwrap();
    assert_eq!(forged_res.status(), StatusCode::OK);
    let forged_json: serde_json::Value =
        serde_json::from_slice(&to_bytes(forged_res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert!(forged_json["receipts"].as_array().unwrap().is_empty());
    assert_eq!(forged_json["rejected_deltas"][0]["code"], "malformed-delta");
    assert_eq!(forged_json["latest_sequence"], 1);
    assert!(store
        .list_shared_sessions(org_id, Some(repo_id), 10)
        .await
        .unwrap()
        .iter()
        .all(|session| session.remote_session_key != "sess_forged"));

    // 2. Redelivering the same sequence is idempotent, and the receipt returned
    //    is the one that was actually written the first time — not a freshly
    //    minted id for an effect that did not happen.
    let dup_push_req = push_request(
        &daemon_token,
        &envelope(
            daemon_id,
            org_id,
            vec![session_delta(
                1,
                repo_id,
                "sess_123",
                PublicationClass::ContentShared,
                serde_json::json!({ "state": "completed" }),
            )],
        ),
    );

    let res_dup = app.clone().oneshot(dup_push_req).await.unwrap();
    assert_eq!(res_dup.status(), StatusCode::OK);
    let dup_json: serde_json::Value =
        serde_json::from_slice(&to_bytes(res_dup.into_body(), usize::MAX).await.unwrap()).unwrap();
    let dup_receipts = dup_json["receipts"].as_array().unwrap();
    assert_eq!(dup_receipts.len(), 1);
    assert_eq!(dup_receipts[0]["duplicate"], true);
    assert_eq!(
        dup_receipts[0]["id"], receipts[0]["id"],
        "a replay must report the receipt that was already stored"
    );
    assert_eq!(dup_receipts[0]["class"], "metadata-shared");

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
