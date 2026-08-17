use axum::{
    extract::{Path, State},
    Json,
};
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    audit::{compute_action_digest, AuditRecord},
    auth::{AuthPrincipal, Principal},
    authz::{authorize_organization_action, authorize_repository_action, Action, PublicationClass},
    error::ControlPlaneError,
    state::AppState,
    store::Repository,
};

#[derive(Debug, Deserialize)]
pub struct RegisterRepositoryRequest {
    pub federated_id: String,
    pub display_name: String,
    pub max_publication_class: Option<String>,
    pub max_classification: Option<String>,
}

pub async fn register_repository(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path(org_id): Path<Uuid>,
    Json(req): Json<RegisterRepositoryRequest>,
) -> Result<Json<Repository>, ControlPlaneError> {
    authorize_organization_action(
        state.store.as_ref(),
        &principal,
        org_id,
        Action::ManageRepository,
    )
    .await?;

    if req.federated_id.len() != 64 {
        return Err(ControlPlaneError::BadRequest(
            "federated_id must be a 64-character hex string".to_string(),
        ));
    }

    let org = state
        .store
        .get_organization(org_id)
        .await?
        .ok_or_else(|| ControlPlaneError::not_found("organization", "no such organization"))?;

    // Invariant: Repository ceiling must never widen organization ceiling (Design §12.3)
    let org_ceiling = PublicationClass::from_str(&org.max_publication_class);
    let requested_ceiling = req
        .max_publication_class
        .as_deref()
        .map(PublicationClass::from_str)
        .unwrap_or(PublicationClass::MetadataShared);

    let effective_class = org_ceiling.intersect(requested_ceiling);

    let repo_id = Uuid::now_v7();
    let now = Utc::now();

    let repo = Repository {
        id: repo_id,
        organization_id: org_id,
        federated_id: req.federated_id,
        display_name: req.display_name,
        max_publication_class: effective_class.as_str().to_string(),
        max_classification: req
            .max_classification
            .unwrap_or_else(|| "internal".to_string()),
        policy_version: 1,
        created_at: now,
    };

    let repo = state.store.create_repository(repo).await?;

    let actor_id = match &principal {
        Principal::User { id, .. } => Some(*id),
        Principal::Daemon { paired_by, .. } => Some(*paired_by),
    };

    let audit = AuditRecord {
        id: Uuid::now_v7(),
        organization_id: org_id,
        actor_kind: principal.kind_str().to_string(),
        actor_id,
        action: "repository.register".to_string(),
        target_kind: "repository".to_string(),
        target_id: repo_id.to_string(),
        action_digest: compute_action_digest(repo.federated_id.as_bytes()),
        correlation_id: None,
        prev_hash: None,
        record_hash: vec![],
        detail: serde_json::json!({
            "federated_id": repo.federated_id,
            "display_name": repo.display_name,
            "max_publication_class": repo.max_publication_class,
        }),
        occurred_at: now,
    };
    state.store.append_audit_record(audit).await?;

    Ok(Json(repo))
}

pub async fn list_repositories(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path(org_id): Path<Uuid>,
) -> Result<Json<Vec<Repository>>, ControlPlaneError> {
    authorize_organization_action(state.store.as_ref(), &principal, org_id, Action::Read).await?;

    let user_id = match principal {
        Principal::User { id, .. } => id,
        Principal::Daemon { paired_by, .. } => paired_by,
    };

    let repos = state
        .store
        .list_authorized_repositories(org_id, user_id)
        .await?;

    Ok(Json(repos))
}

pub async fn get_repository(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path((org_id, repo_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Repository>, ControlPlaneError> {
    authorize_repository_action(
        state.store.as_ref(),
        &principal,
        org_id,
        repo_id,
        Action::Read,
    )
    .await?;

    // Tenant-scoped in the query: `get_repository` is the unscoped variant and
    // must not appear on a request path.
    let repo = state
        .store
        .get_repository_in_org(org_id, repo_id)
        .await?
        .ok_or_else(|| ControlPlaneError::not_found("repository", "no such repository"))?;

    Ok(Json(repo))
}
