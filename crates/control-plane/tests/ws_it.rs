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
    Membership, MemoryStorageDriver, MemoryStore, Organization, Repository, RoleGrant, Store, User,
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

fn ticket_request(
    token: &str,
    organization_id: Option<Uuid>,
    repository_id: Option<Uuid>,
) -> Request<Body> {
    let mut body = serde_json::Map::new();
    if let Some(organization_id) = organization_id {
        body.insert(
            "organization_id".to_string(),
            serde_json::json!(organization_id),
        );
    }
    body.insert(
        "repository_id".to_string(),
        repository_id.map_or(serde_json::Value::Null, |id| serde_json::json!(id)),
    );
    body.insert("stream".to_string(), serde_json::json!("sync"));
    Request::builder()
        .uri("/v1/events/ticket")
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
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

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
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
async fn ws_requires_a_single_use_ticket() {
    let (app, store, config) = setup();
    let (_org_id, _repo_id, _token) = seed_tenant(&store, &config, "acme", &"a".repeat(64)).await;

    let res = app
        .oneshot(ws_request("/v1/events/stream".to_string(), None))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

/// Ticket scope must name a tenant. Repository is deliberately nullable for
/// organization-wide inbox/approval/session streams.
#[tokio::test]
async fn ticket_requires_an_explicit_organization_but_allows_org_wide_scope() {
    let (app, store, config) = setup();
    let (org_id, repo_id, token) = seed_tenant(&store, &config, "acme", &"a".repeat(64)).await;

    let missing_org = app
        .clone()
        .oneshot(ticket_request(&token, None, Some(repo_id)))
        .await
        .unwrap();
    assert_eq!(missing_org.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let org_wide = app
        .oneshot(ticket_request(&token, Some(org_id), None))
        .await
        .unwrap();
    assert_eq!(org_wide.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_ticket_is_consumed_exactly_once_before_upgrade() {
    let (app, store, config) = setup();
    let (org_id, repo_id, token) = seed_tenant(&store, &config, "acme", &"a".repeat(64)).await;
    let issued = app
        .clone()
        .oneshot(ticket_request(&token, Some(org_id), Some(repo_id)))
        .await
        .unwrap();
    assert_eq!(issued.status(), StatusCode::OK);
    let issued = body_json(issued).await;
    let ticket = issued["ticket"].as_str().expect("opaque ticket");
    assert!(ticket.starts_with("cp_ws_"));

    let upgrade_uri = format!("/v1/events/stream?ticket={ticket}");
    // `Router::oneshot` has no Hyper `OnUpgrade` extension, so Axum correctly
    // refuses the synthetic handshake after the route has consumed the ticket.
    // The second request proves that pre-upgrade failures cannot leave a replayable
    // ticket behind; a real socket handshake is covered by client contract tests.
    let upgraded = app
        .clone()
        .oneshot(ws_request(upgrade_uri.clone(), None))
        .await
        .unwrap();
    assert_eq!(upgraded.status(), StatusCode::BAD_REQUEST);

    let replay = app.oneshot(ws_request(upgrade_uri, None)).await.unwrap();
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
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
        .oneshot(ticket_request(
            &attacker_token,
            Some(attacker_org),
            Some(victim_repo),
        ))
        .await
        .unwrap();
    assert_eq!(res_foreign.status(), StatusCode::NOT_FOUND);
    let foreign_body = body_json(res_foreign).await;

    let absent = Uuid::now_v7();
    let res_absent = app
        .oneshot(ticket_request(
            &attacker_token,
            Some(attacker_org),
            Some(absent),
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
        .oneshot(ticket_request(
            &attacker_token,
            Some(victim_org),
            Some(victim_repo),
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
