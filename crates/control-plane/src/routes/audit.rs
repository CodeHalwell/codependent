use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    audit::AuditRecord,
    auth::AuthPrincipal,
    authz::{authorize_organization_action, Action},
    error::ControlPlaneError,
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    pub limit: Option<usize>,
}

pub async fn list_audit_records(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path(org_id): Path<Uuid>,
    Query(query): Query<AuditQuery>,
) -> Result<Json<Vec<AuditRecord>>, ControlPlaneError> {
    authorize_organization_action(
        state.store.as_ref(),
        &principal,
        org_id,
        Action::ManageOrganization,
    )
    .await?;

    let limit = query.limit.unwrap_or(50).min(200);
    let records = state.store.list_audit_records(org_id, limit).await?;

    Ok(Json(records))
}
