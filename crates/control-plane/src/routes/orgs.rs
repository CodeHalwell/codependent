use axum::{
    extract::{Path, State},
    Json,
};
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    audit::compute_action_digest,
    audit::AuditRecord,
    auth::{AuthPrincipal, Principal},
    authz::{authorize_organization_action, Action},
    error::ControlPlaneError,
    state::AppState,
    store::{Membership, Organization, RoleGrant},
};

#[derive(Debug, Deserialize)]
pub struct CreateOrgRequest {
    pub slug: String,
    pub display_name: String,
    pub max_publication_class: Option<String>,
    pub max_classification: Option<String>,
}

pub async fn create_organization(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Json(req): Json<CreateOrgRequest>,
) -> Result<Json<Organization>, ControlPlaneError> {
    let user_id = match principal {
        Principal::User { id, .. } => id,
        _ => {
            return Err(ControlPlaneError::forbidden(
                "organization",
                "only users can create organizations",
            ))
        }
    };

    let org_id = Uuid::now_v7();
    let now = Utc::now();

    let org = Organization {
        id: org_id,
        slug: req.slug,
        display_name: req.display_name,
        max_publication_class: req
            .max_publication_class
            .unwrap_or_else(|| "metadata-shared".to_string()),
        max_classification: req
            .max_classification
            .unwrap_or_else(|| "internal".to_string()),
        data_residency: None,
        retention_days: None,
        policy_version: 1,
        created_at: now,
    };

    let org = state.store.create_organization(org).await?;

    // Creator gets active membership and organization-admin role grant
    let membership = Membership {
        organization_id: org_id,
        user_id,
        state: "active".to_string(),
        joined_at: Some(now),
        created_at: now,
    };
    state.store.add_membership(membership).await?;

    let grant = RoleGrant {
        id: Uuid::now_v7(),
        organization_id: org_id,
        user_id: Some(user_id),
        team_id: None,
        repository_id: None,
        role: "organization-admin".to_string(),
        action_scope: None,
        granted_by: user_id,
        granted_at: now,
        expires_at: None,
        revoked_at: None,
    };
    state.store.create_role_grant(grant).await?;

    // Record audit event
    let audit = AuditRecord {
        id: Uuid::now_v7(),
        organization_id: org_id,
        actor_kind: "user".to_string(),
        actor_id: Some(user_id),
        action: "organization.create".to_string(),
        target_kind: "organization".to_string(),
        target_id: org_id.to_string(),
        action_digest: compute_action_digest(org.slug.as_bytes()),
        correlation_id: None,
        prev_hash: None,
        record_hash: vec![],
        detail: serde_json::json!({ "slug": org.slug, "display_name": org.display_name }),
        occurred_at: now,
    };
    state.store.append_audit_record(audit).await?;

    Ok(Json(org))
}

pub async fn list_organizations(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
) -> Result<Json<Vec<Organization>>, ControlPlaneError> {
    let user_id = match principal {
        Principal::User { id, .. } => id,
        Principal::Daemon {
            organization_id, ..
        } => {
            let org = state.store.get_organization(organization_id).await?;
            return Ok(Json(org.into_iter().collect()));
        }
    };

    let orgs = state.store.list_user_organizations(user_id).await?;
    Ok(Json(orgs))
}

pub async fn get_organization(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path(org_id): Path<Uuid>,
) -> Result<Json<Organization>, ControlPlaneError> {
    authorize_organization_action(state.store.as_ref(), &principal, org_id, Action::Read).await?;

    let org = state
        .store
        .get_organization(org_id)
        .await?
        .ok_or_else(|| ControlPlaneError::not_found("organization", "no such organization"))?;

    Ok(Json(org))
}

#[derive(Debug, Deserialize)]
pub struct AddMemberRequest {
    pub user_id: Uuid,
    pub role: String,
}

pub async fn add_member(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path(org_id): Path<Uuid>,
    Json(req): Json<AddMemberRequest>,
) -> Result<Json<serde_json::Value>, ControlPlaneError> {
    let actor_id = match principal {
        Principal::User { id, .. } => id,
        _ => {
            return Err(ControlPlaneError::forbidden(
                "organization",
                "only users can add members",
            ))
        }
    };

    authorize_organization_action(
        state.store.as_ref(),
        &principal,
        org_id,
        Action::ManageOrganization,
    )
    .await?;

    let now = Utc::now();
    let membership = Membership {
        organization_id: org_id,
        user_id: req.user_id,
        state: "active".to_string(),
        joined_at: Some(now),
        created_at: now,
    };
    state.store.add_membership(membership).await?;

    let grant = RoleGrant {
        id: Uuid::now_v7(),
        organization_id: org_id,
        user_id: Some(req.user_id),
        team_id: None,
        repository_id: None,
        role: req.role.clone(),
        action_scope: None,
        granted_by: actor_id,
        granted_at: now,
        expires_at: None,
        revoked_at: None,
    };
    state.store.create_role_grant(grant).await?;

    let audit = AuditRecord {
        id: Uuid::now_v7(),
        organization_id: org_id,
        actor_kind: "user".to_string(),
        actor_id: Some(actor_id),
        action: "organization.member.add".to_string(),
        target_kind: "user".to_string(),
        target_id: req.user_id.to_string(),
        action_digest: compute_action_digest(req.role.as_bytes()),
        correlation_id: None,
        prev_hash: None,
        record_hash: vec![],
        detail: serde_json::json!({ "user_id": req.user_id, "role": req.role }),
        occurred_at: now,
    };
    state.store.append_audit_record(audit).await?;

    Ok(Json(serde_json::json!({ "status": "added" })))
}
