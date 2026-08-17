use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
};
use codypendent_control_plane::{
    auth::create_user_token, build_router, AppState, ControlPlaneConfig, MemoryStorageDriver,
    MemoryStore, RoleGrant, Store,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

/// Explicit signing secret: there is no default secret to fall back on.
const TEST_JWT_SECRET: &str = "ctrl-plane-unit-test-signing-key-0123456789abcdef";

#[tokio::test]
async fn object_storage_upload_download_and_range_reads() {
    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET)
        .expect("test signing secret must be accepted");
    let store = Arc::new(MemoryStore::new());
    let storage = Arc::new(MemoryStorageDriver::new());
    let state = AppState::new(config.clone(), store.clone(), storage);
    let app = build_router(state);

    let user_id = Uuid::now_v7();
    let org_id = Uuid::now_v7();

    // Setup user and grant
    let user_token = create_user_token(
        user_id,
        Some("uploader@org.com".into()),
        "Uploader".into(),
        &config.jwt_secret,
        3600,
    )
    .unwrap();

    let grant = RoleGrant {
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
    };
    store.create_role_grant(grant).await.unwrap();

    let content = b"Hello, Codypendent Content-Addressed Storage!";
    let content_hash_hex = hex::encode(Sha256::digest(content));

    // 1. Wrong hash upload must be rejected with 400
    let bad_upload_req = Request::builder()
        .uri(format!("/v1/organizations/{org_id}/objects/upload"))
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {user_token}"))
        .header(header::CONTENT_TYPE, "text/plain")
        .header(
            "x-content-sha256",
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .body(Body::from(content.to_vec()))
        .unwrap();

    let res_bad = app.clone().oneshot(bad_upload_req).await.unwrap();
    assert_eq!(res_bad.status(), StatusCode::BAD_REQUEST);

    // 2. Correct upload
    let upload_req = Request::builder()
        .uri(format!("/v1/organizations/{org_id}/objects/upload"))
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {user_token}"))
        .header(header::CONTENT_TYPE, "text/plain")
        .header("x-content-sha256", &content_hash_hex)
        .body(Body::from(content.to_vec()))
        .unwrap();

    let res = app.clone().oneshot(upload_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let obj_json: serde_json::Value =
        serde_json::from_slice(&to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(obj_json["state"], "available");
    assert_eq!(obj_json["byte_length"], content.len());

    // 3. Download full object
    let download_req = Request::builder()
        .uri(format!(
            "/v1/organizations/{org_id}/objects/{content_hash_hex}"
        ))
        .method("GET")
        .header(header::AUTHORIZATION, format!("Bearer {user_token}"))
        .body(Body::empty())
        .unwrap();

    let res_dl = app.clone().oneshot(download_req).await.unwrap();
    assert_eq!(res_dl.status(), StatusCode::OK);
    let dl_bytes = to_bytes(res_dl.into_body(), usize::MAX).await.unwrap();
    assert_eq!(dl_bytes.as_ref(), content);

    // 4. Download partial byte range (Range: bytes=0-4 -> "Hello")
    let range_req = Request::builder()
        .uri(format!(
            "/v1/organizations/{org_id}/objects/{content_hash_hex}"
        ))
        .method("GET")
        .header(header::AUTHORIZATION, format!("Bearer {user_token}"))
        .header(header::RANGE, "bytes=0-4")
        .body(Body::empty())
        .unwrap();

    let res_range = app.clone().oneshot(range_req).await.unwrap();
    assert_eq!(res_range.status(), StatusCode::PARTIAL_CONTENT);
    let range_bytes = to_bytes(res_range.into_body(), usize::MAX).await.unwrap();
    assert_eq!(range_bytes.as_ref(), b"Hello");

    // 5. Presigned URL generation
    let presign_req = Request::builder()
        .uri(format!("/v1/organizations/{org_id}/objects/presign"))
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {user_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "key": content_hash_hex,
                "method": "GET",
                "expiry_secs": 1800
            }))
            .unwrap(),
        ))
        .unwrap();

    let res_ps = app.oneshot(presign_req).await.unwrap();
    assert_eq!(res_ps.status(), StatusCode::OK);
    let ps_json: serde_json::Value =
        serde_json::from_slice(&to_bytes(res_ps.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(ps_json["url"].as_str().unwrap().contains(&content_hash_hex));
}
