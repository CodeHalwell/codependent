use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
};
use codypendent_control_plane::{
    auth::hash_token,
    build_router,
    store::{User, UserRefreshToken},
    AppState, ControlPlaneConfig, MemoryStorageDriver, MemoryStore, Store,
};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

/// Explicit signing secret: there is no default secret to fall back on.
const TEST_JWT_SECRET: &str = "ctrl-plane-unit-test-signing-key-0123456789abcdef";

/// `POST /v1/auth/login` used to create a brand-new user and mint an access and
/// refresh token for an entirely unauthenticated caller: anyone who could reach
/// the port received a valid session. There is no identity provider — no
/// OAuth/OIDC exchange, no PKCE, no `state`/`nonce`, and the `auth_flows` table
/// is unused — so the endpoint must refuse rather than mint authority.
#[tokio::test]
async fn login_refuses_to_mint_credentials_for_an_unauthenticated_caller() {
    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET)
        .expect("test signing secret must be accepted");
    let store = Arc::new(MemoryStore::new());
    let storage = Arc::new(MemoryStorageDriver::new());
    let state = AppState::new(config, store.clone(), storage);
    let app = build_router(state);

    let req = Request::builder()
        .uri("/v1/auth/login")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "display_name": "Mallory",
                "primary_email": "mallory@example.com"
            }))
            .unwrap(),
        ))
        .unwrap();

    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::NOT_IMPLEMENTED,
        "login must refuse: no identity provider is configured"
    );

    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(resp["type"], "not_implemented");

    // No principal is obtainable through this endpoint.
    assert!(resp.get("access_token").is_none());
    assert!(resp.get("refresh_token").is_none());
    assert!(resp.get("user").is_none());
}

#[tokio::test]
async fn refresh_token_rotation_and_replay_detection() {
    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET)
        .expect("test signing secret must be accepted");
    let store = Arc::new(MemoryStore::new());
    let storage = Arc::new(MemoryStorageDriver::new());
    let state = AppState::new(config, store.clone(), storage);
    let app = build_router(state);

    // 1. Seed an already-authenticated session directly: no route mints one.
    let now = chrono::Utc::now();
    let user_id = Uuid::now_v7();
    store
        .create_user(User {
            id: user_id,
            display_name: "Alice Cooper".to_string(),
            primary_email: Some("alice@example.com".to_string()),
            state: "active".to_string(),
            created_at: now,
            updated_at: now,
        })
        .await
        .unwrap();

    let refresh_token = format!("cprt_{}", Uuid::now_v7());
    store
        .save_refresh_token(UserRefreshToken {
            id: Uuid::now_v7(),
            user_id,
            token_hash: hash_token(&refresh_token),
            rotated_from: None,
            issued_at: now,
            expires_at: now + chrono::Duration::days(30),
            revoked_at: None,
            user_agent_digest: None,
        })
        .await
        .unwrap();
    let refresh_token = refresh_token.as_str();

    // 2. Normal refresh rotation
    let refresh_body = serde_json::json!({
        "refresh_token": refresh_token
    });
    let req = Request::builder()
        .uri("/v1/auth/refresh")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&refresh_body).unwrap()))
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let refresh_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let new_refresh_token = refresh_resp["refresh_token"].as_str().unwrap();
    assert_ne!(refresh_token, new_refresh_token);

    // 3. Replay of old refresh token (theft detection)
    let req_replay = Request::builder()
        .uri("/v1/auth/refresh")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&refresh_body).unwrap()))
        .unwrap();

    let res_replay = app.clone().oneshot(req_replay).await.unwrap();
    assert_eq!(res_replay.status(), StatusCode::UNAUTHORIZED);

    // 4. Assert that new refresh token is now ALSO revoked due to chain revocation
    let req_new = Request::builder()
        .uri("/v1/auth/refresh")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "refresh_token": new_refresh_token
            }))
            .unwrap(),
        ))
        .unwrap();

    let res_new = app.oneshot(req_new).await.unwrap();
    assert_eq!(res_new.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn daemon_pairing_challenge_flow() {
    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET)
        .expect("test signing secret must be accepted");
    let store = Arc::new(MemoryStore::new());
    let storage = Arc::new(MemoryStorageDriver::new());
    let state = AppState::new(config.clone(), store.clone(), storage);
    let app = build_router(state);

    // 1. Create a user and organization
    let user_token = codypendent_control_plane::auth::create_user_token(
        Uuid::now_v7(),
        Some("bob@example.com".to_string()),
        "Bob".to_string(),
        &config.jwt_secret,
        3600,
    )
    .unwrap();

    let create_org_req = Request::builder()
        .uri("/v1/organizations")
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {user_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "slug": "acme-corp",
                "display_name": "Acme Corporation"
            }))
            .unwrap(),
        ))
        .unwrap();

    let res = app.clone().oneshot(create_org_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let org: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let org_id = org["id"].as_str().unwrap();

    // 2. User starts a pairing challenge
    let challenge_req = Request::builder()
        .uri("/v1/auth/pairing/challenge")
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {user_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "organization_id": org_id,
                "requested_scope": { "sync": true }
            }))
            .unwrap(),
        ))
        .unwrap();

    let res = app.clone().oneshot(challenge_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let challenge_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let pairing_code = challenge_resp["pairing_code"].as_str().unwrap();

    // 3. Daemon completes pairing challenge
    let complete_req = Request::builder()
        .uri("/v1/auth/pairing/complete")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "pairing_code": pairing_code,
                "display_name": "Bob's Workstation",
                "consent_manifest": "metadata-only consent manifest v1",
                "max_publication_class": "metadata-shared",
                "accepts_remote_approvals": false,
                "accepts_runner_dispatch": false
            }))
            .unwrap(),
        ))
        .unwrap();

    let res = app.clone().oneshot(complete_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let complete_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let daemon_token = complete_resp["token"].as_str().unwrap();
    assert!(daemon_token.starts_with("cp_daemon_"));

    // 4. Second attempt to use same pairing code must fail (single-use)
    let reuse_req = Request::builder()
        .uri("/v1/auth/pairing/complete")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "pairing_code": pairing_code,
                "display_name": "Attacker Workstation",
                "consent_manifest": "attacker manifest",
                "max_publication_class": "public-marketplace"
            }))
            .unwrap(),
        ))
        .unwrap();

    let res_reuse = app.oneshot(reuse_req).await.unwrap();
    assert_eq!(res_reuse.status(), StatusCode::UNAUTHORIZED);
}

/// `link_identity` used to surface the unique violation on
/// `(provider, issuer, subject)` as 409, which proved to the caller that another
/// user had already linked that identity. The refusal must disclose nothing.
#[tokio::test]
async fn link_identity_does_not_disclose_another_users_identity() {
    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET)
        .expect("test signing secret must be accepted");
    let store = Arc::new(MemoryStore::new());
    let storage = Arc::new(MemoryStorageDriver::new());
    let state = AppState::new(config.clone(), store.clone(), storage);
    let app = build_router(state);

    let link_request = |token: &str, subject: &str| {
        Request::builder()
            .uri("/v1/auth/link")
            .method("POST")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "provider": "github",
                    "issuer": "https://github.com",
                    "subject": subject,
                }))
                .unwrap(),
            ))
            .unwrap()
    };

    let alice = codypendent_control_plane::auth::create_user_token(
        Uuid::now_v7(),
        Some("alice@example.com".to_string()),
        "Alice".to_string(),
        &config.jwt_secret,
        3600,
    )
    .unwrap();
    let mallory = codypendent_control_plane::auth::create_user_token(
        Uuid::now_v7(),
        Some("mallory@example.com".to_string()),
        "Mallory".to_string(),
        &config.jwt_secret,
        3600,
    )
    .unwrap();

    // Alice links her identity.
    let res = app
        .clone()
        .oneshot(link_request(&alice, "gh-user-1"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Re-linking her own identity is idempotent, not a conflict.
    let res_again = app
        .clone()
        .oneshot(link_request(&alice, "gh-user-1"))
        .await
        .unwrap();
    assert_eq!(res_again.status(), StatusCode::OK);

    // Mallory probes for Alice's identity.
    let res_probe = app
        .clone()
        .oneshot(link_request(&mallory, "gh-user-1"))
        .await
        .unwrap();
    assert_ne!(
        res_probe.status(),
        StatusCode::CONFLICT,
        "a 409 proves another user has already linked this identity"
    );
    assert_eq!(res_probe.status(), StatusCode::NOT_FOUND);
    let probe_body: serde_json::Value =
        serde_json::from_slice(&to_bytes(res_probe.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(probe_body["type"], "not_found");

    // An identity nobody has linked still succeeds for Mallory, so the refusal
    // above is about the identity's owner and not about Mallory herself.
    let res_fresh = app
        .oneshot(link_request(&mallory, "gh-user-2"))
        .await
        .unwrap();
    assert_eq!(res_fresh.status(), StatusCode::OK);
}

/// Suspension must end the session, not merely stop new logins.
///
/// A refresh token lives 30 days, and `refresh` loaded the user only "to get
/// latest display name / email" — it never consulted `state`. So an account
/// suspended a moment after issuing one went on minting access tokens for the
/// rest of the month. `UserState::is_active` is documented as "whether the
/// account may act" and had no production caller anywhere in the workspace.
#[tokio::test]
async fn a_suspended_user_cannot_refresh_into_a_fresh_access_token() {
    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET)
        .expect("test signing secret must be accepted");
    let store = Arc::new(MemoryStore::new());
    let storage = Arc::new(MemoryStorageDriver::new());
    let state = AppState::new(config, store.clone(), storage);
    let app = build_router(state);

    let now = chrono::Utc::now();
    let user_id = Uuid::now_v7();
    store
        .create_user(User {
            id: user_id,
            display_name: "Suspended Sam".to_string(),
            primary_email: Some("sam@example.com".to_string()),
            state: "suspended".to_string(),
            created_at: now,
            updated_at: now,
        })
        .await
        .unwrap();

    let refresh_token = format!("cprt_{}", Uuid::now_v7());
    store
        .save_refresh_token(UserRefreshToken {
            id: Uuid::now_v7(),
            user_id,
            token_hash: hash_token(&refresh_token),
            rotated_from: None,
            issued_at: now,
            expires_at: now + chrono::Duration::days(30),
            revoked_at: None,
            user_agent_digest: None,
        })
        .await
        .unwrap();

    let req = Request::builder()
        .uri("/v1/auth/refresh")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({ "refresh_token": refresh_token })).unwrap(),
        ))
        .unwrap();

    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "a suspended account must not refresh"
    );
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        !parsed
            .as_object()
            .is_some_and(|o| o.contains_key("access_token")),
        "no access token may be minted for a suspended account: {parsed}"
    );
}
