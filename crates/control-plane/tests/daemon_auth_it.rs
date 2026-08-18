//! Daemon authentication and authorization invariants.
//!
//! These tests run entirely against the in-memory store: no PostgreSQL is
//! required, so they execute in CI where `DATABASE_URL` is unset.
//!
//! Invariants under test:
//!   * A daemon JWT is only a pointer to a `daemons` row. Every authority-bearing
//!     field is read from that row on each request, never from the claims.
//!   * A revoked, suspended or unknown daemon is refused, and the refusal is
//!     indistinguishable from a garbage token.
//!   * A daemon's authority is bounded by the pairing user's current grants.

use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    Router,
};
use chrono::{Duration, Utc};
use codypendent_control_plane::{
    auth::create_daemon_token, build_router, AppState, ControlPlaneConfig, Daemon,
    MemoryStorageDriver, MemoryStore, Organization, Repository, RoleGrant, Store,
};
use codypendent_control_plane_protocol::{
    ids::{DaemonId, OrganizationId, RepositoryId, Sha256Digest},
    sync::{SyncDelta, SyncDeltaKind, SyncEnvelope},
    PublicationClass, CONTROL_PLANE_PROTOCOL_V1,
};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use uuid::Uuid;

/// Explicit signing secret: there is no default secret to fall back on.
const TEST_JWT_SECRET: &str = "ctrl-plane-unit-test-signing-key-0123456789abcdef";

struct Fixture {
    app: Router,
    store: Arc<MemoryStore>,
    jwt_secret: String,
    org_id: Uuid,
    repo_id: Uuid,
    user_id: Uuid,
    daemon_id: Uuid,
}

struct FixtureSpec {
    daemon_state: &'static str,
    daemon_revoked: bool,
    daemon_max_class: &'static str,
    /// Role granted to the pairing user, if any.
    pairing_role: Option<&'static str>,
    /// When true the pairing user's grant is already expired.
    pairing_grant_expired: bool,
}

impl Default for FixtureSpec {
    fn default() -> Self {
        Self {
            daemon_state: "active",
            daemon_revoked: false,
            daemon_max_class: "metadata-shared",
            pairing_role: Some("contributor"),
            pairing_grant_expired: false,
        }
    }
}

async fn fixture(spec: FixtureSpec) -> Fixture {
    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET)
        .expect("test signing secret must be accepted");
    let jwt_secret = config.jwt_secret.clone();
    let store = Arc::new(MemoryStore::new());
    let storage = Arc::new(MemoryStorageDriver::new());

    let org_id = Uuid::now_v7();
    let repo_id = Uuid::now_v7();
    let user_id = Uuid::now_v7();
    let daemon_id = Uuid::now_v7();
    let now = Utc::now();

    store
        .create_organization(Organization {
            id: org_id,
            slug: "tenant-a".to_string(),
            display_name: "Tenant A".to_string(),
            max_publication_class: "organization-knowledge".to_string(),
            max_classification: "internal".to_string(),
            data_residency: None,
            retention_days: None,
            policy_version: 1,
            created_at: now,
        })
        .await
        .expect("organization must be stored");

    store
        .create_repository(Repository {
            id: repo_id,
            organization_id: org_id,
            federated_id: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .to_string(),
            display_name: "Core".to_string(),
            max_publication_class: "organization-knowledge".to_string(),
            max_classification: "internal".to_string(),
            policy_version: 1,
            created_at: now,
        })
        .await
        .expect("repository must be stored");

    if let Some(role) = spec.pairing_role {
        store
            .create_role_grant(RoleGrant {
                id: Uuid::now_v7(),
                organization_id: org_id,
                user_id: Some(user_id),
                team_id: None,
                repository_id: None,
                role: role.to_string(),
                action_scope: None,
                granted_by: user_id,
                granted_at: now - Duration::days(2),
                expires_at: spec.pairing_grant_expired.then(|| now - Duration::hours(1)),
                revoked_at: None,
            })
            .await
            .expect("grant must be stored");
    }

    store
        .register_daemon(Daemon {
            id: daemon_id,
            organization_id: org_id,
            paired_by: user_id,
            display_name: "Workstation".to_string(),
            consent_manifest_hash: vec![0u8; 32],
            max_publication_class: spec.daemon_max_class.to_string(),
            accepts_remote_approvals: false,
            accepts_runner_dispatch: false,
            state: spec.daemon_state.to_string(),
            paired_at: Some(now),
            revoked_at: spec.daemon_revoked.then_some(now),
            last_seen_at: Some(now),
            created_at: now,
        })
        .await
        .expect("daemon must be stored");

    let app = build_router(AppState::new(config, store.clone(), storage));

    Fixture {
        app,
        store,
        jwt_secret,
        org_id,
        repo_id,
        user_id,
        daemon_id,
    }
}

impl Fixture {
    fn daemon_token(&self) -> String {
        create_daemon_token(
            self.daemon_id,
            self.org_id,
            self.user_id,
            "metadata-shared".to_string(),
            &self.jwt_secret,
            3600,
        )
        .expect("token must mint")
    }

    async fn pull(&self, token: &str) -> (StatusCode, Vec<u8>) {
        let req = Request::builder()
            .uri(format!(
                "/v1/sync/pull?repository_id={}&stream=sync&after_id=0",
                self.repo_id
            ))
            .method("GET")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (status, body.to_vec())
    }

    async fn upload(&self, token: &str, org_id: Uuid) -> StatusCode {
        let content = b"payload".to_vec();
        let req = Request::builder()
            .uri(format!("/v1/organizations/{org_id}/objects/upload"))
            .method("POST")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header("x-content-sha256", hex::encode(Sha256::digest(&content)))
            .body(Body::from(content))
            .unwrap();
        self.app.clone().oneshot(req).await.unwrap().status()
    }
}

// ---------------------------------------------------------------------------
// Defect 2: daemon JWTs must be checked against the daemons row.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn active_daemon_with_granted_pairing_user_is_admitted() {
    let f = fixture(FixtureSpec::default()).await;
    let (status, _) = f.pull(&f.daemon_token()).await;
    assert_eq!(status, StatusCode::OK, "baseline daemon access must work");
}

#[tokio::test]
async fn revoked_daemon_jwt_is_rejected() {
    let f = fixture(FixtureSpec {
        daemon_state: "revoked",
        daemon_revoked: true,
        ..FixtureSpec::default()
    })
    .await;

    let (status, body) = f.pull(&f.daemon_token()).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a revoked daemon must lose authority immediately, not at token expiry"
    );

    // Revocation must not be observable: identical response to a bogus token.
    let (garbage_status, garbage_body) = f.pull("not-even-a-token").await;
    assert_eq!(garbage_status, status);
    assert_eq!(
        garbage_body, body,
        "revoked and never-existed must be indistinguishable"
    );
}

#[tokio::test]
async fn suspended_daemon_jwt_is_rejected() {
    let f = fixture(FixtureSpec {
        daemon_state: "suspended",
        ..FixtureSpec::default()
    })
    .await;

    let (status, _) = f.pull(&f.daemon_token()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn daemon_with_revocation_timestamp_but_active_state_is_rejected() {
    let f = fixture(FixtureSpec {
        daemon_state: "active",
        daemon_revoked: true,
        ..FixtureSpec::default()
    })
    .await;

    let (status, _) = f.pull(&f.daemon_token()).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "an inconsistent row must fail closed"
    );
}

#[tokio::test]
async fn jwt_naming_an_unknown_daemon_is_rejected() {
    let f = fixture(FixtureSpec::default()).await;

    // Correctly signed, unexpired, well-formed — but no such daemons row.
    let token = create_daemon_token(
        Uuid::now_v7(),
        f.org_id,
        f.user_id,
        "public-marketplace".to_string(),
        &f.jwt_secret,
        3600,
    )
    .unwrap();

    let (status, _) = f.pull(&token).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn daemon_jwt_claiming_a_foreign_organization_is_rejected() {
    let f = fixture(FixtureSpec::default()).await;
    let victim_org = Uuid::now_v7();

    // The forged claim names another tenant. Authority comes from the row, and
    // a token minted for a different organization is refused outright.
    let token = create_daemon_token(
        f.daemon_id,
        victim_org,
        f.user_id,
        "public-marketplace".to_string(),
        &f.jwt_secret,
        3600,
    )
    .unwrap();

    let (status, _) = f.pull(&token).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    assert_eq!(
        f.upload(&token, victim_org).await,
        StatusCode::UNAUTHORIZED,
        "a claims-only organization must never grant write access to that tenant"
    );
}

#[tokio::test]
async fn daemon_publication_ceiling_comes_from_the_row_not_the_claims() {
    let f = fixture(FixtureSpec {
        daemon_max_class: "metadata-shared",
        ..FixtureSpec::default()
    })
    .await;

    // Claims assert the widest possible class; the row says metadata-shared.
    let token = create_daemon_token(
        f.daemon_id,
        f.org_id,
        f.user_id,
        "public-marketplace".to_string(),
        &f.jwt_secret,
        3600,
    )
    .unwrap();

    let payload = serde_json::json!({ "title": "Secret Session Title", "state": "completed" });

    // Serialized from the protocol's own batched envelope: the route accepts the
    // wire contract a real daemon emits, not a flat hand-rolled body.
    let envelope = SyncEnvelope {
        protocol_version: CONTROL_PLANE_PROTOCOL_V1,
        daemon_id: DaemonId::from_uuid(f.daemon_id),
        organization_id: OrganizationId::from_uuid(f.org_id),
        sent_at: Utc::now(),
        deltas: vec![SyncDelta {
            id: "delta-1".to_string(),
            sequence: 1,
            kind: SyncDeltaKind::SessionSummary,
            repository_id: Some(RepositoryId::from_uuid(f.repo_id)),
            subject_id: "sess_1".to_string(),
            payload_hash: Sha256Digest::from_bytes(&serde_json::to_vec(&payload).unwrap()),
            payload,
            class: PublicationClass::PublicMarketplace,
            created_at: Utc::now(),
        }],
    };

    let req = Request::builder()
        .uri("/v1/sync/push")
        .method("POST")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&envelope).unwrap()))
        .unwrap();

    let res = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let sessions = f
        .store
        .list_shared_sessions(f.org_id, Some(f.repo_id), 10)
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].class, "metadata-shared",
        "the daemons row, not the token claims, sets the publication ceiling"
    );
    assert_eq!(
        sessions[0].title, None,
        "content must stay redacted at metadata-shared"
    );
}

// ---------------------------------------------------------------------------
// Defect 3: daemon authority is bounded by the pairing user's current grants.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn daemon_of_an_ungranted_pairing_user_has_no_authority() {
    let f = fixture(FixtureSpec {
        pairing_role: None,
        ..FixtureSpec::default()
    })
    .await;

    let (status, body) = f.pull(&f.daemon_token()).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a daemon must not hold blanket authority in its organization"
    );

    // Identical to a repository that does not exist at all.
    let f2 = fixture(FixtureSpec::default()).await;
    let missing_repo_req = Request::builder()
        .uri(format!(
            "/v1/sync/pull?repository_id={}&stream=sync&after_id=0",
            Uuid::now_v7()
        ))
        .method("GET")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", f2.daemon_token()),
        )
        .body(Body::empty())
        .unwrap();
    let res = f2.app.clone().oneshot(missing_repo_req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let absent_body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        absent_body.to_vec(),
        body,
        "unauthorized and absent must be byte-identical"
    );
}

#[tokio::test]
async fn daemon_authority_is_re_evaluated_not_frozen_at_pairing_time() {
    // The pairing user held contributor when the daemon was paired; the grant
    // has since lapsed. The daemon must lose authority on the very next request.
    let f = fixture(FixtureSpec {
        pairing_role: Some("contributor"),
        pairing_grant_expired: true,
        ..FixtureSpec::default()
    })
    .await;

    let (status, _) = f.pull(&f.daemon_token()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn daemon_of_an_observer_cannot_write() {
    let f = fixture(FixtureSpec {
        pairing_role: Some("observer"),
        ..FixtureSpec::default()
    })
    .await;
    let token = f.daemon_token();

    let (read_status, _) = f.pull(&token).await;
    assert_eq!(
        read_status,
        StatusCode::OK,
        "observer-level read is allowed"
    );

    assert_eq!(
        f.upload(&token, f.org_id).await,
        StatusCode::NOT_FOUND,
        "a daemon must not exceed the pairing user's role"
    );
}

#[tokio::test]
async fn daemon_of_an_admin_is_still_capped_at_contributor() {
    let f = fixture(FixtureSpec {
        pairing_role: Some("organization-admin"),
        ..FixtureSpec::default()
    })
    .await;
    let token = f.daemon_token();

    assert_eq!(
        f.upload(&token, f.org_id).await,
        StatusCode::OK,
        "contributor-level writes are allowed"
    );

    // Audit reading requires ManageOrganization; the daemon must not inherit it.
    let audit_req = Request::builder()
        .uri(format!("/v1/organizations/{}/audit", f.org_id))
        .method("GET")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let res = f.app.clone().oneshot(audit_req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::NOT_FOUND,
        "a daemon must never inherit organization-management authority"
    );
}
