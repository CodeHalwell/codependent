//! Object-route tenant scoping. Runs entirely against `MemoryStore` and the
//! in-memory storage driver; no live PostgreSQL is required.

use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
};
use codypendent_control_plane::{
    auth::{create_daemon_token, create_user_token},
    build_router,
    store::PublishedObject,
    AppState, ControlPlaneConfig, Daemon, Membership, MemoryStorageDriver, MemoryStore,
    ObjectStorageDriver, Organization, RoleGrant, Store, User,
};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use uuid::Uuid;

/// Explicit signing secret: there is no default secret to fall back on.
const TEST_JWT_SECRET: &str = "ctrl-plane-unit-test-signing-key-0123456789abcdef";

async fn seed_object_uploader(
    store: &MemoryStore,
    org_max_publication_class: &str,
) -> (Uuid, Uuid, chrono::DateTime<chrono::Utc>) {
    let org_id = Uuid::now_v7();
    let user_id = Uuid::now_v7();
    let now = chrono::Utc::now();
    store
        .create_organization(Organization {
            id: org_id,
            slug: format!("object-policy-{org_id}"),
            display_name: "Object policy".to_string(),
            max_publication_class: org_max_publication_class.to_string(),
            max_classification: "internal".to_string(),
            data_residency: None,
            retention_days: None,
            policy_version: 1,
            created_at: now,
        })
        .await
        .unwrap();
    store
        .create_user(User {
            id: user_id,
            display_name: "Uploader".to_string(),
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
            granted_at: now,
            expires_at: None,
            revoked_at: None,
        })
        .await
        .unwrap();
    (org_id, user_id, now)
}

fn upload_request(org_id: Uuid, token: &str, body: &'static [u8]) -> Request<Body> {
    Request::builder()
        .uri(format!("/v1/organizations/{org_id}/objects/upload"))
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn upload_refuses_an_organization_that_does_not_allow_off_device_publication() {
    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET).unwrap();
    let store = Arc::new(MemoryStore::new());
    let storage = Arc::new(MemoryStorageDriver::new());
    let (org_id, user_id, _) = seed_object_uploader(&store, "private-local").await;
    let token = create_user_token(
        user_id,
        None,
        "Uploader".to_string(),
        &config.jwt_secret,
        3600,
    )
    .unwrap();
    let app = build_router(AppState::new(config, store, storage.clone()));

    let response = app
        .oneshot(upload_request(org_id, &token, b"must stay local"))
        .await
        .unwrap();
    let status = response.status();
    let response_body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "{}",
        String::from_utf8_lossy(&response_body)
    );
    let key = format!(
        "{org_id}/{}",
        hex::encode(Sha256::digest(b"must stay local"))
    );
    assert!(storage.head_object(&key).await.is_err());
}

#[tokio::test]
async fn upload_refuses_a_daemon_whose_live_pairing_ceiling_is_private_local() {
    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET).unwrap();
    let store = Arc::new(MemoryStore::new());
    let storage = Arc::new(MemoryStorageDriver::new());
    let (org_id, user_id, now) = seed_object_uploader(&store, "content-shared").await;
    let daemon_id = Uuid::now_v7();
    store
        .register_daemon(Daemon {
            id: daemon_id,
            organization_id: org_id,
            paired_by: user_id,
            display_name: "Local-only daemon".to_string(),
            consent_manifest_hash: vec![0; 32],
            max_publication_class: "private-local".to_string(),
            accepts_remote_approvals: false,
            accepts_runner_dispatch: false,
            state: "active".to_string(),
            paired_at: Some(now),
            revoked_at: None,
            last_seen_at: Some(now),
            created_at: now,
        })
        .await
        .unwrap();
    let token = create_daemon_token(
        daemon_id,
        org_id,
        user_id,
        "private-local".to_string(),
        &config.jwt_secret,
        3600,
    )
    .unwrap();
    let app = build_router(AppState::new(config, store, storage.clone()));

    let response = app
        .oneshot(upload_request(org_id, &token, b"daemon-local"))
        .await
        .unwrap();
    let status = response.status();
    let response_body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "{}",
        String::from_utf8_lossy(&response_body)
    );
    let key = format!("{org_id}/{}", hex::encode(Sha256::digest(b"daemon-local")));
    assert!(storage.head_object(&key).await.is_err());
}

/// The `{org_id}/` prefix is the only thing separating one tenant's object keys
/// from another's, and the key comes straight from the request body.
#[tokio::test]
async fn presign_refuses_keys_that_escape_the_organization_prefix() {
    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET)
        .expect("test signing secret must be accepted");
    let store = Arc::new(MemoryStore::new());
    let storage = Arc::new(MemoryStorageDriver::new());
    let state = AppState::new(config.clone(), store.clone(), storage);
    let app = build_router(state);

    let org_id = Uuid::now_v7();
    let user_id = Uuid::now_v7();
    let now = chrono::Utc::now();

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
            created_at: now,
        })
        .await
        .unwrap();
    store
        .create_user(User {
            id: user_id,
            display_name: "Member".to_string(),
            primary_email: Some("member@acme.test".to_string()),
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
            granted_at: now,
            expires_at: None,
            revoked_at: None,
        })
        .await
        .unwrap();

    let token = create_user_token(
        user_id,
        Some("member@acme.test".to_string()),
        "Member".to_string(),
        &config.jwt_secret,
        3600,
    )
    .unwrap();

    let presign = |key: &str| {
        Request::builder()
            .uri(format!("/v1/organizations/{org_id}/objects/presign"))
            .method("POST")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "key": key,
                    "method": "GET",
                }))
                .unwrap(),
            ))
            .unwrap()
    };

    for key in [
        "../00000000-0000-0000-0000-000000000000/secret",
        "/../other-tenant/secret",
        "nested/../../other-tenant/secret",
        "./secret",
        r"..\other-tenant\secret",
        "",
    ] {
        let res = app.clone().oneshot(presign(key)).await.unwrap();
        assert_eq!(
            res.status(),
            StatusCode::BAD_REQUEST,
            "key {key:?} must not be presignable"
        );
    }

    let content_hash = vec![0_u8; 32];
    let content_hash_hex = hex::encode(&content_hash);
    store
        .record_published_object(PublishedObject {
            id: Uuid::now_v7(),
            organization_id: org_id,
            repository_id: None,
            content_hash,
            byte_length: 0,
            media_type: "application/octet-stream".to_string(),
            class: "metadata-shared".to_string(),
            encryption: "none".to_string(),
            state: "available".to_string(),
            uploaded_by_daemon: None,
            created_at: now,
        })
        .await
        .unwrap();

    // Only a verified metadata row's canonical content address works.
    let res = app.oneshot(presign(&content_hash_hex)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

/// Direct presigned writes are unavailable because object storage cannot verify
/// caller-supplied bytes or atomically publish their metadata. Reads remain
/// available for live, canonical content addresses.
#[tokio::test]
async fn presign_disables_unverified_puts() {
    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET)
        .expect("test signing secret must be accepted");
    let store = Arc::new(MemoryStore::new());
    let storage = Arc::new(MemoryStorageDriver::new());
    let state = AppState::new(config.clone(), store.clone(), storage);
    let app = build_router(state);

    let org_id = Uuid::now_v7();
    let observer_id = Uuid::now_v7();
    let now = chrono::Utc::now();

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
            created_at: now,
        })
        .await
        .unwrap();
    store
        .create_user(User {
            id: observer_id,
            display_name: "Observer".to_string(),
            primary_email: Some("observer@acme.test".to_string()),
            state: "active".to_string(),
            created_at: now,
            updated_at: now,
        })
        .await
        .unwrap();
    store
        .add_membership(Membership {
            organization_id: org_id,
            user_id: observer_id,
            state: "active".to_string(),
            joined_at: Some(now),
            created_at: now,
        })
        .await
        .unwrap();

    store
        .create_role_grant(RoleGrant {
            id: Uuid::now_v7(),
            organization_id: org_id,
            user_id: Some(observer_id),
            team_id: None,
            repository_id: None,
            role: "observer".to_string(),
            action_scope: None,
            granted_by: observer_id,
            granted_at: now,
            expires_at: None,
            revoked_at: None,
        })
        .await
        .unwrap();

    let token = create_user_token(
        observer_id,
        Some("observer@acme.test".to_string()),
        "Observer".to_string(),
        &config.jwt_secret,
        3600,
    )
    .unwrap();

    let content_hash = vec![0_u8; 32];
    let content_hash_hex = hex::encode(&content_hash);
    store
        .record_published_object(PublishedObject {
            id: Uuid::now_v7(),
            organization_id: org_id,
            repository_id: None,
            content_hash,
            byte_length: 0,
            media_type: "application/octet-stream".to_string(),
            class: "metadata-shared".to_string(),
            encryption: "none".to_string(),
            state: "available".to_string(),
            uploaded_by_daemon: None,
            created_at: now,
        })
        .await
        .unwrap();

    let presign = |method: &str| {
        Request::builder()
            .uri(format!("/v1/organizations/{org_id}/objects/presign"))
            .method("POST")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "key": content_hash_hex,
                    "method": method,
                }))
                .unwrap(),
            ))
            .unwrap()
    };

    // Reading stays open to observers.
    let res = app.clone().oneshot(presign("GET")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "observer GET presign");

    // Writing does not: callers must use the verified upload route.
    let res = app.clone().oneshot(presign("PUT")).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "observer PUT presign"
    );

    // A verb with no sibling route to mirror is refused, not weakest-gated.
    let res = app.oneshot(presign("DELETE")).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "DELETE presign");
}
