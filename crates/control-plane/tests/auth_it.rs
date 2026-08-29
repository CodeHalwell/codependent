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
    // A separate browser/session is a different rotation family. Detecting
    // theft in the first family must not let that stolen token become a
    // permanent account-wide logout primitive.
    let independent_refresh_token = format!("cprt_{}", Uuid::now_v7());
    store
        .save_refresh_token(UserRefreshToken {
            id: Uuid::now_v7(),
            user_id,
            token_hash: hash_token(&independent_refresh_token),
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

    let res_new = app.clone().oneshot(req_new).await.unwrap();
    assert_eq!(res_new.status(), StatusCode::UNAUTHORIZED);

    // The independent root remains usable: replay revokes descendants of the
    // stolen token, not unrelated refresh families for the account.
    let independent_req = Request::builder()
        .uri("/v1/auth/refresh")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "refresh_token": independent_refresh_token
            }))
            .unwrap(),
        ))
        .unwrap();
    let independent_res = app.oneshot(independent_req).await.unwrap();
    assert_eq!(independent_res.status(), StatusCode::OK);
}

#[tokio::test]
async fn concurrent_refresh_use_cannot_mint_two_valid_descendants() {
    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET).unwrap();
    let store = Arc::new(MemoryStore::new());
    let state = AppState::new(config, store.clone(), Arc::new(MemoryStorageDriver::new()));
    let app = build_router(state);
    let now = chrono::Utc::now();
    let user_id = Uuid::now_v7();
    store
        .create_user(User {
            id: user_id,
            display_name: "Concurrent Casey".to_string(),
            primary_email: None,
            state: "active".to_string(),
            created_at: now,
            updated_at: now,
        })
        .await
        .unwrap();
    let raw = format!("cprt_{}", Uuid::now_v7());
    store
        .save_refresh_token(UserRefreshToken {
            id: Uuid::now_v7(),
            user_id,
            token_hash: hash_token(&raw),
            rotated_from: None,
            issued_at: now,
            expires_at: now + chrono::Duration::days(30),
            revoked_at: None,
            user_agent_digest: None,
        })
        .await
        .unwrap();

    let request = || {
        Request::builder()
            .uri("/v1/auth/refresh")
            .method("POST")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({"refresh_token": raw.clone()})).unwrap(),
            ))
            .unwrap()
    };
    let (first, second) = tokio::join!(
        app.clone().oneshot(request()),
        app.clone().oneshot(request())
    );
    let mut responses = vec![first.unwrap(), second.unwrap()];
    responses.sort_by_key(|response| response.status());
    assert_eq!(
        responses
            .iter()
            .filter(|response| response.status() == StatusCode::OK)
            .count(),
        1
    );
    assert_eq!(
        responses
            .iter()
            .filter(|response| response.status() == StatusCode::UNAUTHORIZED)
            .count(),
        1
    );

    // Replay detection revokes the whole chain, including the one replacement
    // that won the race. There can never be two usable descendants.
    let successful = responses
        .into_iter()
        .find(|response| response.status() == StatusCode::OK)
        .unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(successful.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    let replacement = body["refresh_token"].as_str().unwrap();
    let replay = Request::builder()
        .uri("/v1/auth/refresh")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({"refresh_token": replacement})).unwrap(),
        ))
        .unwrap();
    assert_eq!(
        app.oneshot(replay).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
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
    let user_id = Uuid::now_v7();
    let now = chrono::Utc::now();
    store
        .create_user(User {
            id: user_id,
            display_name: "Bob".to_string(),
            primary_email: Some("bob@example.com".to_string()),
            state: "active".to_string(),
            created_at: now,
            updated_at: now,
        })
        .await
        .unwrap();
    let user_token = codypendent_control_plane::auth::create_user_token(
        user_id,
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

    // 2. The approved pairing scope cannot exceed the organization's ceiling.
    let expanded_challenge_req = Request::builder()
        .uri("/v1/auth/pairing/challenge")
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {user_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "organization_id": org_id,
                "requested_scope": {
                    "max_publication_class": "public-marketplace",
                    "accepts_remote_approvals": false,
                    "accepts_runner_dispatch": false,
                    "repositories": []
                }
            }))
            .unwrap(),
        ))
        .unwrap();
    let expanded_challenge = app.clone().oneshot(expanded_challenge_req).await.unwrap();
    assert_eq!(expanded_challenge.status(), StatusCode::BAD_REQUEST);

    // 3. User starts a pairing challenge within that ceiling.
    let challenge_req = Request::builder()
        .uri("/v1/auth/pairing/challenge")
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {user_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "organization_id": org_id,
                "requested_scope": {
                    "max_publication_class": "metadata-shared",
                    "accepts_remote_approvals": false,
                    "accepts_runner_dispatch": false,
                    "repositories": []
                }
            }))
            .unwrap(),
        ))
        .unwrap();

    let res = app.clone().oneshot(challenge_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let challenge_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let pairing_code = challenge_resp["challenge_code"].as_str().unwrap();

    // 4. The daemon cannot expand the scope the user approved, and a refused
    // attempt must not consume the one-time challenge.
    let expanded_req = Request::builder()
        .uri("/v1/auth/pairing/complete")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "pairing_code": pairing_code,
                "display_name": "Over-scoped Workstation",
                "consent_manifest": "unexpected expanded scope",
                "max_publication_class": "public-marketplace",
                "accepts_remote_approvals": true,
                "accepts_runner_dispatch": true
            }))
            .unwrap(),
        ))
        .unwrap();
    let expanded = app.clone().oneshot(expanded_req).await.unwrap();
    assert_eq!(expanded.status(), StatusCode::BAD_REQUEST);

    // 5. Daemon completes pairing challenge with the exact approved scope.
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

    // 6. Second attempt to use same pairing code must fail (single-use)
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

/// A caller knowing a provider subject is not proof they control that identity.
/// Until a provider callback proves both sides, the endpoint must never write a
/// link that could hijack the real owner's future login.
#[tokio::test]
async fn link_identity_refuses_unproved_external_identity_claims() {
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

    let now = chrono::Utc::now();
    let user_id = Uuid::now_v7();
    store
        .create_user(User {
            id: user_id,
            display_name: "Mallory".to_string(),
            primary_email: Some("mallory@example.com".to_string()),
            state: "active".to_string(),
            created_at: now,
            updated_at: now,
        })
        .await
        .unwrap();
    let token = codypendent_control_plane::auth::create_user_token(
        user_id,
        Some("mallory@example.com".to_string()),
        "Mallory".to_string(),
        &config.jwt_secret,
        3600,
    )
    .unwrap();

    let res = app
        .oneshot(link_request(&token, "victims-github-subject"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
    assert!(store
        .find_user_identity("github", "https://github.com", "victims-github-subject")
        .await
        .unwrap()
        .is_none());
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
