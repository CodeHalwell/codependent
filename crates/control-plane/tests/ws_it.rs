//! WebSocket subscription defences.
//!
//! These exercise the pre-upgrade refusals, so no real socket (and no live
//! PostgreSQL) is needed — the handshake headers are present but the request is
//! rejected before `on_upgrade` ever runs.

use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    response::Response,
    Router,
};
use codypendent_control_plane::{
    auth::create_daemon_token, build_router, store::Daemon, AppState, ControlPlaneConfig,
    MemoryStorageDriver, MemoryStore, Organization, Repository, RoleGrant, Store,
};
use tower::ServiceExt;
use uuid::Uuid;

/// Explicit signing secret: there is no default secret to fall back on.
const TEST_JWT_SECRET: &str = "ctrl-plane-unit-test-signing-key-0123456789abcdef";

fn setup() -> (Router, Arc<MemoryStore>, ControlPlaneConfig) {
    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET)
        .expect("test signing secret must be accepted");
    let store = Arc::new(MemoryStore::new());
    let storage = Arc::new(MemoryStorageDriver::new());
    let state = AppState::new(config.clone(), store.clone(), storage);
    (build_router(state), store, config)
}

async fn seed_tenant(
    store: &MemoryStore,
    config: &ControlPlaneConfig,
    slug: &str,
    federated_id: &str,
) -> (Uuid, Uuid, String) {
    let org_id = Uuid::now_v7();
    store
        .create_organization(Organization {
            id: org_id,
            slug: slug.to_string(),
            display_name: slug.to_string(),
            max_publication_class: "content-shared".to_string(),
            max_classification: "internal".to_string(),
            data_residency: None,
            retention_days: None,
            policy_version: 1,
            created_at: chrono::Utc::now(),
        })
        .await
        .unwrap();

    let repo_id = Uuid::now_v7();
    store
        .create_repository(Repository {
            id: repo_id,
            organization_id: org_id,
            federated_id: federated_id.to_string(),
            display_name: "Repo".to_string(),
            max_publication_class: "content-shared".to_string(),
            max_classification: "internal".to_string(),
            policy_version: 1,
            created_at: chrono::Utc::now(),
        })
        .await
        .unwrap();

    let daemon_id = Uuid::now_v7();
    let user_id = Uuid::now_v7();

    // A daemon borrows its authority from the user who paired it.
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

    store
        .register_daemon(Daemon {
            id: daemon_id,
            organization_id: org_id,
            paired_by: user_id,
            display_name: "Test Daemon".to_string(),
            consent_manifest_hash: vec![0u8; 32],
            max_publication_class: "content-shared".to_string(),
            accepts_remote_approvals: false,
            accepts_runner_dispatch: false,
            state: "active".to_string(),
            paired_at: Some(chrono::Utc::now()),
            revoked_at: None,
            last_seen_at: Some(chrono::Utc::now()),
            created_at: chrono::Utc::now(),
        })
        .await
        .unwrap();

    let token = create_daemon_token(
        daemon_id,
        org_id,
        user_id,
        "content-shared".to_string(),
        &config.jwt_secret,
        3600,
    )
    .unwrap();

    (org_id, repo_id, token)
}

/// A well-formed WebSocket handshake, so any refusal comes from the route's own
/// checks rather than from a malformed upgrade.
fn ws_request(uri: String, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .uri(uri)
        .method("GET")
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==");

    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }

    builder.body(Body::empty()).unwrap()
}

async fn body_json(res: Response) -> serde_json::Value {
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// A bearer token in the query string is written to access logs by `TraceLayer`
/// and by every intermediary proxy, so it is refused outright — never accepted
/// as a credential.
#[tokio::test]
async fn ws_refuses_credentials_in_the_query_string() {
    let (app, store, config) = setup();
    let (org_id, repo_id, token) = seed_tenant(&store, &config, "acme", &"a".repeat(64)).await;

    let res = app
        .oneshot(ws_request(
            format!(
                "/v1/events/stream?token={token}&organization_id={org_id}&repository_id={repo_id}"
            ),
            None,
        ))
        .await
        .unwrap();

    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "a query-string token must never authenticate a subscription"
    );
}

/// Even alongside a valid header credential, a query-string token is refused so
/// clients cannot keep leaking one into logs.
#[tokio::test]
async fn ws_refuses_query_string_token_even_with_a_valid_header() {
    let (app, store, config) = setup();
    let (org_id, repo_id, token) = seed_tenant(&store, &config, "acme", &"a".repeat(64)).await;

    let res = app
        .oneshot(ws_request(
            format!(
                "/v1/events/stream?token={token}&organization_id={org_id}&repository_id={repo_id}"
            ),
            Some(&token),
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ws_requires_an_authorization_header() {
    let (app, store, config) = setup();
    let (org_id, repo_id, _token) = seed_tenant(&store, &config, "acme", &"a".repeat(64)).await;

    let res = app
        .oneshot(ws_request(
            format!("/v1/events/stream?organization_id={org_id}&repository_id={repo_id}"),
            None,
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

/// The subscription must name its own tenant and repository; nothing is
/// inferred from the principal.
#[tokio::test]
async fn ws_requires_an_explicit_organization_and_repository() {
    let (app, store, config) = setup();
    let (org_id, repo_id, token) = seed_tenant(&store, &config, "acme", &"a".repeat(64)).await;

    for uri in [
        "/v1/events/stream".to_string(),
        format!("/v1/events/stream?organization_id={org_id}"),
        format!("/v1/events/stream?repository_id={repo_id}"),
    ] {
        let res = app
            .clone()
            .oneshot(ws_request(uri.clone(), Some(&token)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "uri {uri}");
    }
}

/// Subscribing to another tenant's repository is refused, and the refusal is
/// identical to one for a repository that does not exist.
#[tokio::test]
async fn ws_refuses_a_repository_in_another_tenant() {
    let (app, store, config) = setup();
    let (_victim_org, victim_repo, _victim_token) =
        seed_tenant(&store, &config, "victim", &"b".repeat(64)).await;
    let (attacker_org, _attacker_repo, attacker_token) =
        seed_tenant(&store, &config, "attacker", &"c".repeat(64)).await;

    let res_foreign = app
        .clone()
        .oneshot(ws_request(
            format!("/v1/events/stream?organization_id={attacker_org}&repository_id={victim_repo}"),
            Some(&attacker_token),
        ))
        .await
        .unwrap();
    assert_eq!(res_foreign.status(), StatusCode::NOT_FOUND);
    let foreign_body = body_json(res_foreign).await;

    let absent = Uuid::now_v7();
    let res_absent = app
        .oneshot(ws_request(
            format!("/v1/events/stream?organization_id={attacker_org}&repository_id={absent}"),
            Some(&attacker_token),
        ))
        .await
        .unwrap();
    assert_eq!(res_absent.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(res_absent).await,
        foreign_body,
        "an unauthorized repository must be indistinguishable from an absent one"
    );
}

/// A daemon cannot subscribe to a tenant other than its own, even for a
/// repository that really does live in that other tenant.
#[tokio::test]
async fn ws_refuses_an_organization_the_principal_does_not_belong_to() {
    let (app, store, config) = setup();
    let (victim_org, victim_repo, _victim_token) =
        seed_tenant(&store, &config, "victim", &"b".repeat(64)).await;
    let (_attacker_org, _attacker_repo, attacker_token) =
        seed_tenant(&store, &config, "attacker", &"c".repeat(64)).await;

    let res = app
        .oneshot(ws_request(
            format!("/v1/events/stream?organization_id={victim_org}&repository_id={victim_repo}"),
            Some(&attacker_token),
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
