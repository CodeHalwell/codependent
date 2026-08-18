//! Organization creation, listing and membership.
//!
//! Request input is validated with the protocol's own types (the slug newtype
//! and both class enums), so nothing unnameable can be stored as a ceiling that
//! every narrowing decision below it then reads as `Unknown`.
//!
//! The **responses** here are still the stored row, not
//! `codypendent_control_plane_protocol::organization::Organization`, and
//! deliberately so: the protocol type has a required `updated_at`, the
//! `organizations` table has no such column, and synthesizing one (from
//! `created_at`, or from "now") would publish a timestamp that records nothing
//! that happened. Projecting this route needs either a migration adding
//! `organizations.updated_at` or an optional field in the protocol; until one
//! exists, the honest shape is the row.

use axum::{
    extract::{Path, State},
    Json,
};
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use codypendent_control_plane_protocol::{
    ids::{AuditRecordId, OrganizationId},
    organization::OrganizationSlug,
};

use crate::{
    audit::{compute_action_digest, uncomputed_digest, AuditActorKind, AuditRecord},
    auth::{AuthPrincipal, Principal},
    authz::{
        authorize_organization_action, parse_data_classification, parse_publication_class, Action,
        DataClassification, PublicationClass,
    },
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

    // Validated by the protocol's own newtype: the slug is the organization's
    // URL-safe identity, and one that fails validation here would be rejected by
    // every client that models it as an `OrganizationSlug`.
    let slug = OrganizationSlug::new(req.slug)
        .map_err(|e| ControlPlaneError::BadRequest(format!("invalid slug: {e}")))?;

    // The organization's ceilings are the top of every narrowing chain beneath
    // it, so they must be classes this build can name. Storing an unrecognized
    // one would read as `Unknown` on every later comparison — correct as a
    // refusal, but silently unusable, and impossible for the operator to see.
    let max_publication_class = req
        .max_publication_class
        .as_deref()
        .map_or(PublicationClass::MetadataShared, parse_publication_class);
    if max_publication_class == PublicationClass::Unknown {
        return Err(ControlPlaneError::BadRequest(
            "max_publication_class is not a recognized publication class".to_string(),
        ));
    }
    let max_classification = req
        .max_classification
        .as_deref()
        .map_or(DataClassification::Internal, parse_data_classification);
    if max_classification == DataClassification::Unknown {
        return Err(ControlPlaneError::BadRequest(
            "max_classification is not a recognized classification".to_string(),
        ));
    }

    let org_id = Uuid::now_v7();
    let now = Utc::now();

    let org = Organization {
        id: org_id,
        slug: slug.as_str().to_string(),
        display_name: req.display_name,
        max_publication_class: max_publication_class.as_str().to_string(),
        max_classification: max_classification.as_str().to_string(),
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
        id: AuditRecordId::new(),
        organization_id: OrganizationId::from_uuid(org_id),
        actor_kind: AuditActorKind::User,
        actor_id: Some(user_id.to_string()),
        action: "organization.create".to_string(),
        target_kind: "organization".to_string(),
        target_id: org_id.to_string(),
        action_digest: compute_action_digest(org.slug.as_bytes()),
        correlation_id: None,
        // Both chain fields belong to the store, which computes them under the
        // per-organization lock. See `audit::uncomputed_digest`.
        prev_hash: None,
        record_hash: uncomputed_digest(),
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
        id: AuditRecordId::new(),
        organization_id: OrganizationId::from_uuid(org_id),
        actor_kind: AuditActorKind::User,
        actor_id: Some(actor_id.to_string()),
        action: "organization.member.add".to_string(),
        target_kind: "user".to_string(),
        target_id: req.user_id.to_string(),
        action_digest: compute_action_digest(req.role.as_bytes()),
        correlation_id: None,
        prev_hash: None,
        record_hash: uncomputed_digest(),
        detail: serde_json::json!({ "user_id": req.user_id, "role": req.role }),
        occurred_at: now,
    };
    state.store.append_audit_record(audit).await?;

    Ok(Json(serde_json::json!({ "status": "added" })))
}
