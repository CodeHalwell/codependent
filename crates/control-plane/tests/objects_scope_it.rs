//! Object-route tenant scoping. Runs entirely against `MemoryStore` and the
//! in-memory storage driver; no live PostgreSQL is required.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use codypendent_control_plane::{
    auth::create_user_token, build_router, AppState, ControlPlaneConfig, MemoryStorageDriver,
    MemoryStore, Organization, RoleGrant, Store,
};
use tower::ServiceExt;
use uuid::Uuid;

/// Explicit signing secret: there is no default secret to fall back on.
const TEST_JWT_SECRET: &str = "ctrl-plane-unit-test-signing-key-0123456789abcdef";

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

    // A plain relative key inside the tenant still works.
    let res = app.oneshot(presign("deadbeef")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

/// The verb decides the gate. A presigned PUT is an upload — it takes the same
/// Contributor gate as `upload_object` — and gating it as a read handed every
/// Observer a signed write into the org's bucket space (bypassing
/// `upload_object`'s content-hash check as well). A verb with no sibling route
/// is refused outright rather than mapped to the weakest gate.
#[tokio::test]
async fn presign_gates_put_as_an_upload_not_a_read() {
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

    let presign = |method: &str| {
        Request::builder()
            .uri(format!("/v1/organizations/{org_id}/objects/presign"))
            .method("POST")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "key": "deadbeef",
                    "method": method,
                }))
                .unwrap(),
            ))
            .unwrap()
    };

    // Reading stays open to observers.
    let res = app.clone().oneshot(presign("GET")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "observer GET presign");

    // Writing does not: same non-disclosing not-found the upload route gives.
    let res = app.clone().oneshot(presign("PUT")).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND, "observer PUT presign");

    // A verb with no sibling route to mirror is refused, not weakest-gated.
    let res = app.oneshot(presign("DELETE")).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "DELETE presign");
}
