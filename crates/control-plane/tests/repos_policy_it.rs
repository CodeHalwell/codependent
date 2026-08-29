//! Route-level repository catalog policy-ceiling regressions.

use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    Router,
};
use chrono::Utc;
use codypendent_control_plane::{
    auth::create_user_token, build_router, AppState, ControlPlaneConfig, Membership,
    MemoryStorageDriver, MemoryStore, Organization, Repository, RoleGrant, Store, User,
};
use tower::ServiceExt;
use uuid::Uuid;

const TEST_JWT_SECRET: &str = "ctrl-plane-unit-test-signing-key-0123456789abcdef";

fn get_request(token: &str, uri: String) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .method("GET")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> serde_json::Value {
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

async fn seed_authorized_organization(
    store: &MemoryStore,
    user_id: Uuid,
    slug: &str,
    publication_ceiling: &str,
    classification_ceiling: &str,
) -> Uuid {
    let organization_id = Uuid::now_v7();
    let now = Utc::now();
    store
        .create_organization(Organization {
            id: organization_id,
            slug: slug.to_string(),
            display_name: slug.to_string(),
            max_publication_class: publication_ceiling.to_string(),
            max_classification: classification_ceiling.to_string(),
            data_residency: None,
            retention_days: None,
            policy_version: 2,
            created_at: now,
        })
        .await
        .unwrap();
    store
        .add_membership(Membership {
            organization_id,
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
            organization_id,
            user_id: Some(user_id),
            team_id: None,
            repository_id: None,
            role: "observer".to_string(),
            action_scope: None,
            granted_by: user_id,
            granted_at: now,
            expires_at: None,
            revoked_at: None,
        })
        .await
        .unwrap();
    organization_id
}

async fn seed_repository(
    store: &MemoryStore,
    organization_id: Uuid,
    federated_byte: char,
    publication_ceiling: &str,
    classification_ceiling: &str,
) -> Uuid {
    let repository_id = Uuid::now_v7();
    store
        .create_repository(Repository {
            id: repository_id,
            organization_id,
            federated_id: federated_byte.to_string().repeat(64),
            display_name: "Repository".to_string(),
            max_publication_class: publication_ceiling.to_string(),
            max_classification: classification_ceiling.to_string(),
            policy_version: 1,
            created_at: Utc::now(),
        })
        .await
        .unwrap();
    repository_id
}

#[tokio::test]
async fn repository_catalog_intersects_current_org_ceilings_and_rejects_malformed_storage() {
    let config = ControlPlaneConfig::from_env_with_jwt_secret(TEST_JWT_SECRET).unwrap();
    let store = Arc::new(MemoryStore::new());
    let app: Router = build_router(AppState::new(
        config.clone(),
        store.clone(),
        Arc::new(MemoryStorageDriver::new()),
    ));
    let user_id = Uuid::now_v7();
    let now = Utc::now();
    store
        .create_user(User {
            id: user_id,
            display_name: "Catalog reader".to_string(),
            primary_email: Some("catalog@example.test".to_string()),
            state: "active".to_string(),
            created_at: now,
            updated_at: now,
        })
        .await
        .unwrap();
    let token = create_user_token(
        user_id,
        Some("catalog@example.test".to_string()),
        "Catalog reader".to_string(),
        &config.jwt_secret,
        3600,
    )
    .unwrap();

    let organization_id =
        seed_authorized_organization(&store, user_id, "narrowed-org", "metadata-shared", "public")
            .await;
    let stale_wide_repository =
        seed_repository(&store, organization_id, 'a', "public-marketplace", "secret").await;

    let list = app
        .clone()
        .oneshot(get_request(
            &token,
            format!("/v1/organizations/{organization_id}/repositories"),
        ))
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let listed = response_json(list).await;
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["max_publication_class"], "metadata-shared");
    assert_eq!(listed[0]["max_classification"], "public");

    let get = app
        .clone()
        .oneshot(get_request(
            &token,
            format!("/v1/organizations/{organization_id}/repositories/{stale_wide_repository}"),
        ))
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);
    let fetched = response_json(get).await;
    assert_eq!(fetched["max_publication_class"], "metadata-shared");
    assert_eq!(fetched["max_classification"], "public");

    let malformed_repository =
        seed_repository(&store, organization_id, 'b', "future-sharing", "internal").await;
    let malformed_get = app
        .clone()
        .oneshot(get_request(
            &token,
            format!("/v1/organizations/{organization_id}/repositories/{malformed_repository}"),
        ))
        .await
        .unwrap();
    assert_eq!(malformed_get.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(response_json(malformed_get).await["type"], "internal_error");

    let malformed_list = app
        .clone()
        .oneshot(get_request(
            &token,
            format!("/v1/organizations/{organization_id}/repositories"),
        ))
        .await
        .unwrap();
    assert_eq!(malformed_list.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        response_json(malformed_list).await["type"],
        "internal_error"
    );

    let malformed_org_id = seed_authorized_organization(
        &store,
        user_id,
        "malformed-org",
        "metadata-shared",
        "future-classification",
    )
    .await;
    let valid_repository =
        seed_repository(&store, malformed_org_id, 'c', "metadata-shared", "internal").await;
    let malformed_org_get = app
        .oneshot(get_request(
            &token,
            format!("/v1/organizations/{malformed_org_id}/repositories/{valid_repository}"),
        ))
        .await
        .unwrap();
    assert_eq!(
        malformed_org_get.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        response_json(malformed_org_get).await["type"],
        "internal_error"
    );
}
