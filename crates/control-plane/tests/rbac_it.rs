use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
};
use codypendent_control_plane::{
    auth::create_user_token, build_router, AppState, ControlPlaneConfig, MemoryStorageDriver,
    MemoryStore,
};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

/// Explicit signing secret: there is no default secret to fall back on.
const TEST_JWT_SECRET: &str = "ctrl-plane-unit-test-signing-key-0123456789abcdef";

#[tokio::test]
async fn rbac_matrix_and_nondisclosure_invariants() {
    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET)
        .expect("test signing secret must be accepted");
    let store = Arc::new(MemoryStore::new());
    let storage = Arc::new(MemoryStorageDriver::new());
    let state = AppState::new(config.clone(), store.clone(), storage);
    let app = build_router(state);

    let admin_id = Uuid::now_v7();
    let observer_id = Uuid::now_v7();
    let stranger_id = Uuid::now_v7();

    let admin_token = create_user_token(
        admin_id,
        Some("admin@org.com".into()),
        "Admin".into(),
        &config.jwt_secret,
        3600,
    )
    .unwrap();
    let observer_token = create_user_token(
        observer_id,
        Some("obs@org.com".into()),
        "Observer".into(),
        &config.jwt_secret,
        3600,
    )
    .unwrap();
    let stranger_token = create_user_token(
        stranger_id,
        Some("stranger@other.com".into()),
        "Stranger".into(),
        &config.jwt_secret,
        3600,
    )
    .unwrap();

    // 1. Admin creates Organization
    let create_org_req = Request::builder()
        .uri("/v1/organizations")
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "slug": "secure-org",
                "display_name": "Secure Org"
            }))
            .unwrap(),
        ))
        .unwrap();

    let res = app.clone().oneshot(create_org_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let org_json: serde_json::Value =
        serde_json::from_slice(&to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap();
    let org_id = org_json["id"].as_str().unwrap();

    // 2. Admin registers repository
    let federated_id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let reg_repo_req = Request::builder()
        .uri(format!("/v1/organizations/{org_id}/repositories"))
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "federated_id": federated_id,
                "display_name": "Core Engine"
            }))
            .unwrap(),
        ))
        .unwrap();

    let res = app.clone().oneshot(reg_repo_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let repo_json: serde_json::Value =
        serde_json::from_slice(&to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap();
    let repo_id = repo_json["id"].as_str().unwrap();

    // 3. Admin adds observer to org
    let add_mem_req = Request::builder()
        .uri(format!("/v1/organizations/{org_id}/members"))
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "user_id": observer_id,
                "role": "observer"
            }))
            .unwrap(),
        ))
        .unwrap();

    let res = app.clone().oneshot(add_mem_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 4. Observer can read repository
    let read_repo_req = Request::builder()
        .uri(format!("/v1/organizations/{org_id}/repositories/{repo_id}"))
        .method("GET")
        .header(header::AUTHORIZATION, format!("Bearer {observer_token}"))
        .body(Body::empty())
        .unwrap();

    let res = app.clone().oneshot(read_repo_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 5. Observer CANNOT register repositories (Observer < Maintainer)
    let obs_reg_req = Request::builder()
        .uri(format!("/v1/organizations/{org_id}/repositories"))
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {observer_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "federated_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "display_name": "Disallowed Repo"
            }))
            .unwrap(),
        ))
        .unwrap();

    let res = app.clone().oneshot(obs_reg_req).await.unwrap();
    // Non-disclosure: returns 404 (not 403)
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // 6. Stranger accessing existing repository receives 404
    let stranger_real_repo_req = Request::builder()
        .uri(format!("/v1/organizations/{org_id}/repositories/{repo_id}"))
        .method("GET")
        .header(header::AUTHORIZATION, format!("Bearer {stranger_token}"))
        .body(Body::empty())
        .unwrap();

    let res_real = app.clone().oneshot(stranger_real_repo_req).await.unwrap();
    assert_eq!(res_real.status(), StatusCode::NOT_FOUND);
    let real_body = to_bytes(res_real.into_body(), usize::MAX).await.unwrap();

    // 7. Stranger accessing non-existent repository receives byte-equivalent 404
    let random_fake_repo_id = Uuid::now_v7();
    let stranger_fake_repo_req = Request::builder()
        .uri(format!(
            "/v1/organizations/{org_id}/repositories/{random_fake_repo_id}"
        ))
        .method("GET")
        .header(header::AUTHORIZATION, format!("Bearer {stranger_token}"))
        .body(Body::empty())
        .unwrap();

    let res_fake = app.clone().oneshot(stranger_fake_repo_req).await.unwrap();
    assert_eq!(res_fake.status(), StatusCode::NOT_FOUND);
    let fake_body = to_bytes(res_fake.into_body(), usize::MAX).await.unwrap();

    assert_eq!(
        real_body, fake_body,
        "Inaccessible and non-existent resource responses must be identical"
    );

    // 8. Cross-tenant duplicate federated_id succeeds in a second organization without 409
    let org2_req = Request::builder()
        .uri("/v1/organizations")
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {stranger_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "slug": "other-org",
                "display_name": "Other Org"
            }))
            .unwrap(),
        ))
        .unwrap();

    let res = app.clone().oneshot(org2_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let org2_json: serde_json::Value =
        serde_json::from_slice(&to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap();
    let org2_id = org2_json["id"].as_str().unwrap();

    let reg_same_repo_req = Request::builder()
        .uri(format!("/v1/organizations/{org2_id}/repositories"))
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {stranger_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "federated_id": federated_id,
                "display_name": "Same Repo in Second Org"
            }))
            .unwrap(),
        ))
        .unwrap();

    let res = app.oneshot(reg_same_repo_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "Registering identical federated_id in distinct organization must succeed without collision oracle");
}
