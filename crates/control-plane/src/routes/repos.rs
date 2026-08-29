use axum::{
    extract::{Path, State},
    Json,
};
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use codypendent_control_plane_protocol::{
    ids::{AuditRecordId, FederatedRepositoryId, OrganizationId, RepositoryId},
    repository::Repository as WireRepository,
};

use crate::{
    audit::{compute_action_digest, uncomputed_digest, AuditRecord},
    auth::{AuthPrincipal, Principal},
    authz::{
        authorize_organization_action, authorize_repository_action, parse_data_classification,
        parse_publication_class, Action, DataClassification, PublicationClass,
    },
    error::ControlPlaneError,
    state::AppState,
    store::Repository,
};

/// Project a stored repository row onto the protocol type.
///
/// The row carries `max_publication_class` and `max_classification` as free-form
/// text and `federated_id` as an unvalidated `String`; the protocol has a closed
/// enum for each class and a validated 64-hex newtype for the identity. A stored
/// federated id that is not a SHA-256 hex string is a corrupted row — refused,
/// never re-rendered into something a client would treat as a content address.
fn repository_to_wire(
    row: Repository,
    organization_publication_ceiling: PublicationClass,
    organization_classification_ceiling: DataClassification,
) -> Result<WireRepository, ControlPlaneError> {
    let federated_id = FederatedRepositoryId::new(row.federated_id).map_err(|e| {
        ControlPlaneError::Internal(format!(
            "stored repository has a malformed federated id: {e}"
        ))
    })?;
    let policy_version = u64::try_from(row.policy_version).map_err(|_| {
        ControlPlaneError::Internal("stored repository policy version is negative".to_string())
    })?;

    let repository_publication_ceiling = parse_publication_class(&row.max_publication_class);
    let repository_classification_ceiling = parse_data_classification(&row.max_classification);
    if organization_publication_ceiling == PublicationClass::Unknown
        || repository_publication_ceiling == PublicationClass::Unknown
        || organization_classification_ceiling == DataClassification::Unknown
        || repository_classification_ceiling == DataClassification::Unknown
    {
        return Err(ControlPlaneError::Internal(
            "stored repository or organization has an unrecognized policy ceiling".to_string(),
        ));
    }

    Ok(WireRepository {
        id: RepositoryId::from_uuid(row.id),
        organization_id: OrganizationId::from_uuid(row.organization_id),
        federated_id,
        display_name: row.display_name,
        // Repository rows may predate a later organization-policy narrowing.
        // Publish only the current effective intersection so this authenticated
        // catalog is a send-time ceiling, never a stale wider promise.
        max_publication_class: repository_publication_ceiling
            .intersect(organization_publication_ceiling),
        max_classification: repository_classification_ceiling
            .intersect(organization_classification_ceiling),
        policy_version,
        created_at: row.created_at,
    })
}

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
) -> Result<Json<WireRepository>, ControlPlaneError> {
    authorize_organization_action(
        state.store.as_ref(),
        &principal,
        org_id,
        Action::ManageRepository,
    )
    .await?;

    // Validated by the protocol's own newtype rather than by a length check: a
    // 64-character string that is not lowercase hex was accepted before, stored,
    // and then handed back as a federated identity no other node could reproduce.
    let federated_id = FederatedRepositoryId::new(req.federated_id).map_err(|_| {
        ControlPlaneError::BadRequest(
            "federated_id must be a 64-character lowercase hex string".to_string(),
        )
    })?;

    let org = state
        .store
        .get_organization(org_id)
        .await?
        .ok_or_else(|| ControlPlaneError::not_found("organization", "no such organization"))?;

    // Invariant: Repository ceiling must never widen organization ceiling (Design §12.3)
    // `intersect` collapses an unrecognized class on either side to
    // `private-local`, so a ceiling this build cannot name can only narrow.
    let org_ceiling = parse_publication_class(&org.max_publication_class);
    let requested_ceiling = req
        .max_publication_class
        .as_deref()
        .map(parse_publication_class)
        .unwrap_or(PublicationClass::MetadataShared);

    let effective_class = org_ceiling.intersect(requested_ceiling);

    // The sensitivity ceiling narrows the same way, but it cannot be computed
    // with `intersect` alone: `DataClassification::intersect` answers `Secret`
    // when either side is `Unknown` — correct for "how sensitive is this data",
    // and exactly backwards for a ceiling, where it would *widen* the repository
    // to the most permissive setting there is. An unnameable classification on
    // either side is refused instead.
    let requested_classification = req
        .max_classification
        .as_deref()
        .map_or(DataClassification::Internal, parse_data_classification);
    if requested_classification == DataClassification::Unknown {
        return Err(ControlPlaneError::BadRequest(
            "max_classification is not a recognized classification".to_string(),
        ));
    }
    let org_classification = parse_data_classification(&org.max_classification);
    if org_classification == DataClassification::Unknown {
        return Err(ControlPlaneError::Internal(
            "organization classification ceiling is not recognized by this build".to_string(),
        ));
    }
    let effective_classification = requested_classification.intersect(org_classification);

    let repo_id = Uuid::now_v7();
    let now = Utc::now();

    let repo = Repository {
        id: repo_id,
        organization_id: org_id,
        federated_id: federated_id.as_str().to_string(),
        display_name: req.display_name,
        max_publication_class: effective_class.as_str().to_string(),
        max_classification: effective_classification.as_str().to_string(),
        policy_version: 1,
        created_at: now,
    };

    let repo = state.store.create_repository(repo).await?;

    let actor_id = match &principal {
        Principal::User { id, .. } => Some(*id),
        Principal::Daemon { paired_by, .. } => Some(*paired_by),
    };

    let audit = AuditRecord {
        id: AuditRecordId::new(),
        organization_id: OrganizationId::from_uuid(org_id),
        actor_kind: principal.audit_actor_kind(),
        actor_id: actor_id.map(|id| id.to_string()),
        action: "repository.register".to_string(),
        target_kind: "repository".to_string(),
        target_id: repo_id.to_string(),
        action_digest: compute_action_digest(repo.federated_id.as_bytes()),
        correlation_id: None,
        prev_hash: None,
        record_hash: uncomputed_digest(),
        detail: serde_json::json!({
            "federated_id": repo.federated_id,
            "display_name": repo.display_name,
            "max_publication_class": repo.max_publication_class,
        }),
        occurred_at: now,
    };
    state.store.append_audit_record(audit).await?;

    Ok(Json(repository_to_wire(
        repo,
        org_ceiling,
        org_classification,
    )?))
}

pub async fn list_repositories(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path(org_id): Path<Uuid>,
) -> Result<Json<Vec<WireRepository>>, ControlPlaneError> {
    authorize_organization_action(state.store.as_ref(), &principal, org_id, Action::Read).await?;

    let user_id = match principal {
        Principal::User { id, .. } => id,
        Principal::Daemon { paired_by, .. } => paired_by,
    };

    let repos = state
        .store
        .list_authorized_repositories(org_id, user_id)
        .await?;
    let organization = state
        .store
        .get_organization(org_id)
        .await?
        .ok_or_else(|| ControlPlaneError::not_found("organization", "no such organization"))?;
    let organization_publication_ceiling =
        parse_publication_class(&organization.max_publication_class);
    let organization_classification_ceiling =
        parse_data_classification(&organization.max_classification);

    Ok(Json(
        repos
            .into_iter()
            .map(|repo| {
                repository_to_wire(
                    repo,
                    organization_publication_ceiling,
                    organization_classification_ceiling,
                )
            })
            .collect::<Result<Vec<_>, _>>()?,
    ))
}

pub async fn get_repository(
    State(state): State<AppState>,
    AuthPrincipal(principal): AuthPrincipal,
    Path((org_id, repo_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<WireRepository>, ControlPlaneError> {
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
    let organization = state
        .store
        .get_organization(org_id)
        .await?
        .ok_or_else(|| ControlPlaneError::not_found("organization", "no such organization"))?;

    Ok(Json(repository_to_wire(
        repo,
        parse_publication_class(&organization.max_publication_class),
        parse_data_classification(&organization.max_classification),
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(federated_id: &str, class: &str, classification: &str) -> Repository {
        Repository {
            id: Uuid::now_v7(),
            organization_id: Uuid::now_v7(),
            federated_id: federated_id.to_string(),
            display_name: "Repo".to_string(),
            max_publication_class: class.to_string(),
            max_classification: classification.to_string(),
            policy_version: 1,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn ceilings_are_published_as_protocol_enums_not_free_text() {
        let wire = repository_to_wire(
            row(&"a".repeat(64), "content-shared", "confidential"),
            PublicationClass::MetadataShared,
            DataClassification::Internal,
        )
        .expect("a well-formed row must project");
        assert_eq!(wire.max_publication_class, PublicationClass::MetadataShared);
        assert_eq!(wire.max_classification, DataClassification::Internal);

        let json = serde_json::to_value(&wire).expect("the wire type must serialize");
        assert_eq!(
            json.get("max_publication_class").unwrap(),
            "metadata-shared"
        );
        assert_eq!(json.get("federated_id").unwrap(), &"a".repeat(64));
    }

    #[test]
    fn unrecognized_ceilings_rank_most_restrictive_rather_than_nearest() {
        assert!(repository_to_wire(
            row(&"b".repeat(64), "galaxy-shared", "cosmic"),
            PublicationClass::PublicMarketplace,
            DataClassification::Secret,
        )
        .is_err());
    }

    #[test]
    fn a_row_whose_federated_id_is_not_a_digest_is_refused() {
        for bad in ["fed-1", &"A".repeat(64), &"a".repeat(63)] {
            assert!(
                repository_to_wire(
                    row(bad, "metadata-shared", "internal"),
                    PublicationClass::MetadataShared,
                    DataClassification::Internal,
                )
                .is_err(),
                "{bad} must not be published as a federated identity"
            );
        }
    }
}
