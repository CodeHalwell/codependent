//! Tenant-scoping and publication-ceiling defences on the sync routes.
//!
//! Everything here runs against `MemoryStore`, so no live PostgreSQL is needed
//! (`migrations_it.rs` owns the `DATABASE_URL` probe/skip path for the tests
//! that genuinely require a database).
//!
//! Every push in this file is a serialized protocol [`SyncEnvelope`]. A delta the
//! control plane will not write is refused *inside* the batch — the request is
//! still a 200 carrying a `rejected_deltas` entry — so the assertions below check
//! the rejection and the absence of any stored effect rather than a status code.

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
use codypendent_control_plane_protocol::{
    ids::{DaemonId, OrganizationId, RepositoryId, Sha256Digest},
    sync::{SyncDelta, SyncDeltaKind, SyncEnvelope},
    PublicationClass, CONTROL_PLANE_PROTOCOL_V1,
};
use tower::ServiceExt;
use uuid::Uuid;

/// Explicit signing secret: there is no default secret to fall back on.
const TEST_JWT_SECRET: &str = "ctrl-plane-unit-test-signing-key-0123456789abcdef";

/// The single code every refused delta carries. One code for "another tenant's
/// repository", "no such repository", "no grant" and "the ceilings forbid this
/// class": a distinguishable code would be the existence oracle the uniform 404s
/// exist to prevent.
const REFUSED: &str = "delta-refused";

/// A daemon identity plus the credential that speaks for it. The envelope names
/// its own daemon and organization, and the route refuses a batch whose claims
/// disagree with the credential, so a test cannot build one from the token alone.
struct DaemonFixture {
    token: String,
    daemon_id: Uuid,
    organization_id: Uuid,
}

fn setup() -> (Router, Arc<MemoryStore>, ControlPlaneConfig) {
    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET)
        .expect("test signing secret must be accepted");
    let store = Arc::new(MemoryStore::new());
    let storage = Arc::new(MemoryStorageDriver::new());
    let state = AppState::new(config.clone(), store.clone(), storage);
    (build_router(state), store, config)
}

async fn seed_org(store: &MemoryStore, slug: &str, max_publication_class: &str) -> Uuid {
    let org_id = Uuid::now_v7();
    store
        .create_organization(Organization {
            id: org_id,
            slug: slug.to_string(),
            display_name: slug.to_string(),
            max_publication_class: max_publication_class.to_string(),
            max_classification: "internal".to_string(),
            data_residency: None,
            retention_days: None,
            policy_version: 1,
            created_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    org_id
}

async fn seed_repo(
    store: &MemoryStore,
    org_id: Uuid,
    federated_id: &str,
    max_publication_class: &str,
) -> Uuid {
    let repo_id = Uuid::now_v7();
    store
        .create_repository(Repository {
            id: repo_id,
            organization_id: org_id,
            federated_id: federated_id.to_string(),
            display_name: "Repo".to_string(),
            max_publication_class: max_publication_class.to_string(),
            max_classification: "internal".to_string(),
            policy_version: 1,
            created_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    repo_id
}

async fn seed_daemon(
    store: &MemoryStore,
    config: &ControlPlaneConfig,
    org_id: Uuid,
    max_publication_class: &str,
) -> DaemonFixture {
    let daemon_id = Uuid::now_v7();
    let user_id = Uuid::now_v7();

    // A daemon borrows its authority from the user who paired it, so that user
    // must hold a grant in the organization for the daemon to have any at all.
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
            max_publication_class: max_publication_class.to_string(),
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
        max_publication_class.to_string(),
        &config.jwt_secret,
        3600,
    )
    .unwrap();

    DaemonFixture {
        token,
        daemon_id,
        organization_id: org_id,
    }
}

/// A batch built from the protocol types, exactly as a daemon would serialize it.
fn envelope(daemon: &DaemonFixture, deltas: Vec<SyncDelta>) -> SyncEnvelope {
    SyncEnvelope {
        protocol_version: CONTROL_PLANE_PROTOCOL_V1,
        daemon_id: DaemonId::from_uuid(daemon.daemon_id),
        organization_id: OrganizationId::from_uuid(daemon.organization_id),
        sent_at: chrono::Utc::now(),
        deltas,
    }
}

fn delta(
    sequence: u64,
    kind: SyncDeltaKind,
    repository_id: Option<Uuid>,
    subject_id: &str,
    class: PublicationClass,
    title: &str,
) -> SyncDelta {
    let payload = serde_json::json!({ "title": title, "state": "completed" });
    SyncDelta {
        id: format!("delta-{sequence}"),
        sequence,
        kind,
        repository_id: repository_id.map(RepositoryId::from_uuid),
        subject_id: subject_id.to_string(),
        payload_hash: Sha256Digest::from_bytes(&serde_json::to_vec(&payload).unwrap()),
        payload,
        class,
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

fn get_request(token: &str, uri: String) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .method("GET")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

async fn body_json(res: Response) -> serde_json::Value {
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// The rejection recorded for a single-delta batch that accepted nothing.
async fn sole_rejection(res: Response) -> serde_json::Value {
    let body = body_json(res).await;
    assert!(
        body["receipts"].as_array().unwrap().is_empty(),
        "a refused delta must not produce a receipt: {body}"
    );
    let rejections = body["rejected_deltas"].as_array().unwrap().clone();
    assert_eq!(
        rejections.len(),
        1,
        "expected exactly one rejection: {body}"
    );
    rejections[0].clone()
}

/// `repository_id` arrives in the request body. A daemon must not be able to
/// write a row attributed to another tenant's repository, and the refusal must
/// be indistinguishable from a repository that does not exist at all.
#[tokio::test]
async fn push_refuses_repository_belonging_to_another_tenant() {
    let (app, store, config) = setup();

    let victim_org = seed_org(&store, "victim", "content-shared").await;
    let victim_repo = seed_repo(&store, victim_org, &"b".repeat(64), "content-shared").await;

    let attacker_org = seed_org(&store, "attacker", "content-shared").await;
    seed_repo(&store, attacker_org, &"c".repeat(64), "content-shared").await;
    let attacker = seed_daemon(&store, &config, attacker_org, "content-shared").await;

    let res = app
        .clone()
        .oneshot(push_request(
            &attacker.token,
            &envelope(
                &attacker,
                vec![delta(
                    1,
                    SyncDeltaKind::SessionSummary,
                    Some(victim_repo),
                    "sess_evil",
                    PublicationClass::ContentShared,
                    "Injected",
                )],
            ),
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let cross_tenant = sole_rejection(res).await;
    assert_eq!(cross_tenant["code"], REFUSED);

    // Nothing reached the victim tenant.
    assert!(store
        .list_shared_sessions(victim_org, Some(victim_repo), 10)
        .await
        .unwrap()
        .is_empty());
    assert!(store
        .query_stream_events(victim_org, Some(victim_repo), "sync", 0, 10)
        .await
        .unwrap()
        .is_empty());
    assert!(store
        .query_stream_events(attacker_org, None, "sync", 0, 10)
        .await
        .unwrap()
        .is_empty());

    // A repository that exists nowhere produces the identical rejection, so
    // neither can be used to prove the victim's repository exists.
    let res_absent = app
        .oneshot(push_request(
            &attacker.token,
            &envelope(
                &attacker,
                vec![delta(
                    1,
                    SyncDeltaKind::SessionSummary,
                    Some(Uuid::now_v7()),
                    "sess_evil",
                    PublicationClass::ContentShared,
                    "Injected",
                )],
            ),
        ))
        .await
        .unwrap();

    assert_eq!(res_absent.status(), StatusCode::OK);
    assert_eq!(sole_rejection(res_absent).await, cross_tenant);
}

/// An envelope may not be filed as another daemon or into another tenant. The
/// credential is the fact; the body is only a claim.
#[tokio::test]
async fn push_refuses_an_envelope_that_disagrees_with_its_credential() {
    let (app, store, config) = setup();

    let org_id = seed_org(&store, "acme", "content-shared").await;
    let repo_id = seed_repo(&store, org_id, &"a".repeat(64), "content-shared").await;
    let daemon = seed_daemon(&store, &config, org_id, "content-shared").await;

    let victim_org = seed_org(&store, "victim", "content-shared").await;

    let forged_daemon = DaemonFixture {
        token: daemon.token.clone(),
        daemon_id: Uuid::now_v7(),
        organization_id: org_id,
    };
    let forged_org = DaemonFixture {
        token: daemon.token.clone(),
        daemon_id: daemon.daemon_id,
        organization_id: victim_org,
    };

    for forged in [&forged_daemon, &forged_org] {
        let res = app
            .clone()
            .oneshot(push_request(
                &daemon.token,
                &envelope(
                    forged,
                    vec![delta(
                        1,
                        SyncDeltaKind::SessionSummary,
                        Some(repo_id),
                        "sess_forged",
                        PublicationClass::ContentShared,
                        "Forged",
                    )],
                ),
            ))
            .await
            .unwrap();

        // Collapses into the same refusal every other unauthorized request
        // produces: nothing is looked up, so it cannot confirm the ids exist.
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    assert!(store
        .list_shared_sessions(org_id, Some(repo_id), 10)
        .await
        .unwrap()
        .is_empty());
}

/// A protocol version this build does not implement is refused outright rather
/// than interpreted under contract terms that may have changed.
#[tokio::test]
async fn push_refuses_an_unsupported_protocol_version() {
    let (app, store, config) = setup();

    let org_id = seed_org(&store, "acme", "content-shared").await;
    let repo_id = seed_repo(&store, org_id, &"9".repeat(64), "content-shared").await;
    let daemon = seed_daemon(&store, &config, org_id, "content-shared").await;

    let mut env = envelope(
        &daemon,
        vec![delta(
            1,
            SyncDeltaKind::SessionSummary,
            Some(repo_id),
            "sess_future",
            PublicationClass::ContentShared,
            "From the future",
        )],
    );
    env.protocol_version = codypendent_control_plane_protocol::version::ProtocolVersion::new(
        CONTROL_PLANE_PROTOCOL_V1.major + 1,
        0,
    );

    let res = app
        .oneshot(push_request(&daemon.token, &env))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    assert!(store
        .list_shared_sessions(org_id, Some(repo_id), 10)
        .await
        .unwrap()
        .is_empty());
}

/// An unnamed repository is refused rather than defaulted to "organization-wide".
#[tokio::test]
async fn push_refuses_delta_without_repository() {
    let (app, store, config) = setup();

    let org_id = seed_org(&store, "acme", "content-shared").await;
    let daemon = seed_daemon(&store, &config, org_id, "content-shared").await;

    let res = app
        .oneshot(push_request(
            &daemon.token,
            &envelope(
                &daemon,
                vec![delta(
                    1,
                    SyncDeltaKind::Tombstone,
                    None,
                    "sess_1",
                    PublicationClass::MetadataShared,
                    "No repo",
                )],
            ),
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(sole_rejection(res).await["code"], REFUSED);

    assert!(store
        .list_tombstones(org_id, chrono::DateTime::UNIX_EPOCH)
        .await
        .unwrap()
        .is_empty());
}

/// The repository ceiling binds even when the daemon ceiling is wider — the
/// daemon must not be the only thing consulted.
#[tokio::test]
async fn push_class_is_clamped_by_repository_ceiling() {
    let (app, store, config) = setup();

    let org_id = seed_org(&store, "acme", "organization-knowledge").await;
    let repo_id = seed_repo(&store, org_id, &"d".repeat(64), "metadata-shared").await;
    // Deliberately the widest of the three, so it cannot be the binding one.
    let daemon = seed_daemon(&store, &config, org_id, "organization-knowledge").await;

    let res = app
        .oneshot(push_request(
            &daemon.token,
            &envelope(
                &daemon,
                vec![delta(
                    1,
                    SyncDeltaKind::SessionSummary,
                    Some(repo_id),
                    "sess_repo_cap",
                    PublicationClass::OrganizationKnowledge,
                    "Repository capped",
                )],
            ),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // The receipt reports the class that was actually stored, not the one asked for.
    let body = body_json(res).await;
    assert_eq!(body["receipts"][0]["class"], "metadata-shared");

    let sessions = store
        .list_shared_sessions(org_id, Some(repo_id), 10)
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].class, "metadata-shared",
        "repository ceiling must bind the publication class"
    );
    assert_eq!(
        sessions[0].title, None,
        "content must be redacted once the repository ceiling clamps the class"
    );
}

/// The organization ceiling binds even when both the repository and the daemon
/// are wider.
#[tokio::test]
async fn push_class_is_clamped_by_organization_ceiling() {
    let (app, store, config) = setup();

    let org_id = seed_org(&store, "acme", "metadata-shared").await;
    // Seeded directly so the repository ceiling is wider than the organization's.
    let repo_id = seed_repo(&store, org_id, &"e".repeat(64), "organization-knowledge").await;
    let daemon = seed_daemon(&store, &config, org_id, "organization-knowledge").await;

    let res = app
        .oneshot(push_request(
            &daemon.token,
            &envelope(
                &daemon,
                vec![delta(
                    1,
                    SyncDeltaKind::SessionSummary,
                    Some(repo_id),
                    "sess_org_cap",
                    PublicationClass::OrganizationKnowledge,
                    "Organization capped",
                )],
            ),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let sessions = store
        .list_shared_sessions(org_id, Some(repo_id), 10)
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].class, "metadata-shared",
        "organization ceiling must bind the publication class"
    );
    assert_eq!(sessions[0].title, None);
}

/// An unrecognised class parses to `Unknown`, whose intersection is private-local,
/// and private-local must never be persisted to the control plane.
#[tokio::test]
async fn push_refuses_private_local_publication() {
    let (app, store, config) = setup();

    let org_id = seed_org(&store, "acme", "content-shared").await;
    let repo_id = seed_repo(&store, org_id, &"f".repeat(64), "content-shared").await;
    let daemon = seed_daemon(&store, &config, org_id, "content-shared").await;

    for class in [PublicationClass::PrivateLocal, PublicationClass::Unknown] {
        let res = app
            .clone()
            .oneshot(push_request(
                &daemon.token,
                &envelope(
                    &daemon,
                    vec![delta(
                        1,
                        SyncDeltaKind::SessionSummary,
                        Some(repo_id),
                        "sess_private",
                        class,
                        "Never leaves the machine",
                    )],
                ),
            ))
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK, "class {class:?}");
        assert_eq!(
            sole_rejection(res).await["code"],
            REFUSED,
            "class {class:?}"
        );
    }

    assert!(store
        .list_shared_sessions(org_id, Some(repo_id), 10)
        .await
        .unwrap()
        .is_empty());
}

/// A delta kind this build cannot project is refused with its own code — a
/// property of the delta, disclosing nothing about any resource — and never
/// projected as the nearest known kind.
#[tokio::test]
async fn push_refuses_an_unprojectable_delta_kind() {
    let (app, store, config) = setup();

    let org_id = seed_org(&store, "acme", "content-shared").await;
    let repo_id = seed_repo(&store, org_id, &"8".repeat(64), "content-shared").await;
    let daemon = seed_daemon(&store, &config, org_id, "content-shared").await;

    let res = app
        .oneshot(push_request(
            &daemon.token,
            &envelope(
                &daemon,
                vec![delta(
                    1,
                    SyncDeltaKind::Unknown,
                    Some(repo_id),
                    "sess_unknown_kind",
                    PublicationClass::ContentShared,
                    "From a newer daemon",
                )],
            ),
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        sole_rejection(res).await["code"],
        "unprojectable-delta-kind"
    );

    assert!(store
        .list_shared_sessions(org_id, Some(repo_id), 10)
        .await
        .unwrap()
        .is_empty());
}

/// Pulling must be scoped to one authorized repository instead of draining
/// every repository in the organization.
#[tokio::test]
async fn pull_is_repository_scoped_and_refuses_foreign_repositories() {
    let (app, store, config) = setup();

    let org_id = seed_org(&store, "acme", "content-shared").await;
    let repo_a = seed_repo(&store, org_id, &"1".repeat(64), "content-shared").await;
    let repo_b = seed_repo(&store, org_id, &"2".repeat(64), "content-shared").await;

    let other_org = seed_org(&store, "other", "content-shared").await;
    let other_repo = seed_repo(&store, other_org, &"3".repeat(64), "content-shared").await;

    let daemon = seed_daemon(&store, &config, org_id, "content-shared").await;

    // One envelope, two repositories: the batch is the wire unit.
    let res = app
        .clone()
        .oneshot(push_request(
            &daemon.token,
            &envelope(
                &daemon,
                vec![
                    delta(
                        1,
                        SyncDeltaKind::SessionSummary,
                        Some(repo_a),
                        "sess_a",
                        PublicationClass::ContentShared,
                        "sess_a",
                    ),
                    delta(
                        2,
                        SyncDeltaKind::SessionSummary,
                        Some(repo_b),
                        "sess_b",
                        PublicationClass::ContentShared,
                        "sess_b",
                    ),
                ],
            ),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body["receipts"].as_array().unwrap().len(), 2);
    assert!(body["rejected_deltas"].as_array().unwrap().is_empty());
    assert_eq!(body["latest_sequence"], 2);

    // Scoped pull returns only repo_a's event.
    let res = app
        .clone()
        .oneshot(get_request(
            &daemon.token,
            format!("/v1/sync/pull?repository_id={repo_a}"),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let events = body_json(res).await;
    let events = events.as_array().unwrap();
    assert_eq!(
        events.len(),
        1,
        "pull must not return another repository's events"
    );
    assert_eq!(
        events[0]["repository_id"].as_str().unwrap(),
        repo_a.to_string()
    );

    // An unscoped pull is refused instead of returning the whole organization.
    let res_unscoped = app
        .clone()
        .oneshot(get_request(&daemon.token, "/v1/sync/pull".to_string()))
        .await
        .unwrap();
    assert_eq!(res_unscoped.status(), StatusCode::BAD_REQUEST);

    // Another tenant's repository is refused as not found.
    let res_foreign = app
        .oneshot(get_request(
            &daemon.token,
            format!("/v1/sync/pull?repository_id={other_repo}"),
        ))
        .await
        .unwrap();
    assert_eq!(res_foreign.status(), StatusCode::NOT_FOUND);
}

/// Listing sessions must not treat organization membership as authorization for
/// every repository inside it.
#[tokio::test]
async fn listing_sessions_requires_an_authorized_repository() {
    let (app, store, config) = setup();

    let org_id = seed_org(&store, "acme", "content-shared").await;
    let repo_id = seed_repo(&store, org_id, &"4".repeat(64), "content-shared").await;
    let daemon = seed_daemon(&store, &config, org_id, "content-shared").await;

    let res = app
        .clone()
        .oneshot(get_request(
            &daemon.token,
            format!("/v1/organizations/{org_id}/sessions"),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res_scoped = app
        .oneshot(get_request(
            &daemon.token,
            format!("/v1/organizations/{org_id}/sessions?repository_id={repo_id}"),
        ))
        .await
        .unwrap();
    assert_eq!(res_scoped.status(), StatusCode::OK);
}
