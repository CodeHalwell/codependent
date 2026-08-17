use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{auth::Principal, error::ControlPlaneError, store::Store};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Observer,
    Contributor,
    Approver,
    Maintainer,
    OrganizationAdmin,
}

impl Role {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "observer" => Some(Role::Observer),
            "contributor" => Some(Role::Contributor),
            "approver" => Some(Role::Approver),
            "maintainer" => Some(Role::Maintainer),
            "organization-admin" => Some(Role::OrganizationAdmin),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Observer => "observer",
            Role::Contributor => "contributor",
            Role::Approver => "approver",
            Role::Maintainer => "maintainer",
            Role::OrganizationAdmin => "organization-admin",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Read,
    SyncPush,
    SyncPull,
    UploadObject,
    DownloadObject,
    Approve,
    ManageRepository,
    ManageOrganization,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PublicationClass {
    PrivateLocal = 0,
    MetadataShared = 1,
    ContentShared = 2,
    OrganizationKnowledge = 3,
    PublicMarketplace = 4,
}

impl PublicationClass {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "private-local" => PublicationClass::PrivateLocal,
            "metadata-shared" => PublicationClass::MetadataShared,
            "content-shared" => PublicationClass::ContentShared,
            "organization-knowledge" => PublicationClass::OrganizationKnowledge,
            "public-marketplace" => PublicationClass::PublicMarketplace,
            _ => PublicationClass::PrivateLocal, // Unknown defaults to strictest (design §8.3)
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            PublicationClass::PrivateLocal => "private-local",
            PublicationClass::MetadataShared => "metadata-shared",
            PublicationClass::ContentShared => "content-shared",
            PublicationClass::OrganizationKnowledge => "organization-knowledge",
            PublicationClass::PublicMarketplace => "public-marketplace",
        }
    }

    pub fn intersect(self, other: Self) -> Self {
        if (self as u8) < (other as u8) {
            self
        } else {
            other
        }
    }
}

/// Highest role a daemon may ever hold, no matter what its pairing user holds.
/// A daemon acts unattended, so it never inherits approval or management authority.
pub const DAEMON_ROLE_CEILING: Role = Role::Contributor;

/// Highest role the given user currently holds in `org_id`, considering
/// organization-wide grants plus (when `repo_id` is given) grants scoped to that
/// repository. `list_user_grants` already excludes revoked and expired grants;
/// grant rows carrying an unrecognised role string are dropped rather than
/// guessed at.
async fn highest_user_role(
    store: &dyn Store,
    org_id: Uuid,
    user_id: Uuid,
    repo_id: Option<Uuid>,
) -> Result<Option<Role>, ControlPlaneError> {
    let grants = store.list_user_grants(org_id, user_id).await?;
    Ok(grants
        .iter()
        .filter(|g| match repo_id {
            Some(repo_id) => g.repository_id.is_none() || g.repository_id == Some(repo_id),
            None => g.repository_id.is_none(),
        })
        .filter_map(|g| Role::from_str(&g.role))
        .max())
}

/// Effective role of a daemon principal.
///
/// Design rule: a daemon's authority is bounded by the authority of the user who
/// paired it, at all times. The pairing user's grants are re-read on every
/// request — never frozen at pairing time — so revoking or downgrading the user
/// immediately downgrades every daemon they paired. The result is additionally
/// capped at [`DAEMON_ROLE_CEILING`]. `None` means "no authority at all".
async fn daemon_effective_role(
    store: &dyn Store,
    daemon_org_id: Uuid,
    paired_by: Uuid,
    org_id: Uuid,
    repo_id: Option<Uuid>,
) -> Result<Option<Role>, ControlPlaneError> {
    if daemon_org_id != org_id {
        return Ok(None);
    }
    // A daemon with no identifiable pairing user has no borrowed authority.
    if paired_by.is_nil() {
        return Ok(None);
    }

    let user_role = highest_user_role(store, org_id, paired_by, repo_id).await?;
    Ok(user_role.map(|role| role.min(DAEMON_ROLE_CEILING)))
}

pub fn check_action_allowed(role: Role, action: Action) -> bool {
    match action {
        Action::Read | Action::SyncPull | Action::DownloadObject => true, // All roles can read
        Action::SyncPush | Action::UploadObject => role >= Role::Contributor,
        Action::Approve => role >= Role::Approver,
        Action::ManageRepository => role >= Role::Maintainer,
        Action::ManageOrganization => role >= Role::OrganizationAdmin,
    }
}

pub async fn authorize_organization_action(
    store: &dyn Store,
    principal: &Principal,
    org_id: Uuid,
    action: Action,
) -> Result<Role, ControlPlaneError> {
    match principal {
        Principal::User { id, .. } => {
            // Check direct membership and role grants (organization-wide only)
            let highest_role = highest_user_role(store, org_id, *id, None).await?;

            if let Some(role) = highest_role {
                if check_action_allowed(role, action) {
                    return Ok(role);
                }
            }

            // Design §5.3: Return not_found to avoid disclosing resource existence
            Err(ControlPlaneError::not_found(
                "organization",
                "no such organization",
            ))
        }
        Principal::Daemon {
            organization_id,
            paired_by,
            ..
        } => {
            // Authority is borrowed from the pairing user and re-derived here on
            // every request; a daemon holds nothing of its own.
            let daemon_role =
                daemon_effective_role(store, *organization_id, *paired_by, org_id, None).await?;

            match daemon_role {
                Some(role) if check_action_allowed(role, action) => Ok(role),
                _ => Err(ControlPlaneError::not_found(
                    "organization",
                    "no such organization",
                )),
            }
        }
    }
}

pub async fn authorize_repository_action(
    store: &dyn Store,
    principal: &Principal,
    org_id: Uuid,
    repo_id: Uuid,
    action: Action,
) -> Result<Role, ControlPlaneError> {
    // Resolve the repository with the organization in the query itself. An
    // authorization decision must never be made about a row this tenant could
    // not have read in the first place, so the scoping cannot be a Rust-side
    // comparison after an unscoped fetch. A repository in another tenant and one
    // that does not exist are both `None` here.
    if store
        .get_repository_in_org(org_id, repo_id)
        .await?
        .is_none()
    {
        return Err(ControlPlaneError::not_found(
            "repository",
            "no such repository",
        ));
    }

    match principal {
        Principal::User { id, .. } => {
            let highest_role = highest_user_role(store, org_id, *id, Some(repo_id)).await?;

            if let Some(role) = highest_role {
                if check_action_allowed(role, action) {
                    return Ok(role);
                }
            }

            Err(ControlPlaneError::not_found(
                "repository",
                "no such repository",
            ))
        }
        Principal::Daemon {
            organization_id,
            paired_by,
            ..
        } => {
            let daemon_role =
                daemon_effective_role(store, *organization_id, *paired_by, org_id, Some(repo_id))
                    .await?;

            match daemon_role {
                Some(role) if check_action_allowed(role, action) => Ok(role),
                _ => Err(ControlPlaneError::not_found(
                    "repository",
                    "no such repository",
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{memory::MemoryStore, Repository, RoleGrant};
    use chrono::Utc;

    fn daemon_principal(org_id: Uuid, paired_by: Uuid) -> Principal {
        Principal::Daemon {
            daemon_id: Uuid::now_v7(),
            organization_id: org_id,
            paired_by,
            max_publication_class: "metadata-shared".to_string(),
        }
    }

    async fn grant(
        store: &MemoryStore,
        org_id: Uuid,
        user_id: Uuid,
        role: &str,
        repo_id: Option<Uuid>,
    ) {
        store
            .create_role_grant(RoleGrant {
                id: Uuid::now_v7(),
                organization_id: org_id,
                user_id: Some(user_id),
                team_id: None,
                repository_id: repo_id,
                role: role.to_string(),
                action_scope: None,
                granted_by: user_id,
                granted_at: Utc::now(),
                expires_at: None,
                revoked_at: None,
            })
            .await
            .expect("grant must be stored");
    }

    fn is_not_found(err: &ControlPlaneError) -> bool {
        matches!(err, ControlPlaneError::NotFound { .. })
    }

    #[tokio::test]
    async fn daemon_whose_pairing_user_has_no_grants_has_no_authority() {
        let store = MemoryStore::new();
        let org_id = Uuid::now_v7();
        let principal = daemon_principal(org_id, Uuid::now_v7());

        // Even a read, which every role may perform, is refused: the daemon
        // borrows all of its authority and the pairing user has none.
        let err = authorize_organization_action(&store, &principal, org_id, Action::Read)
            .await
            .expect_err("daemon without a granted pairing user must be refused");
        assert!(
            is_not_found(&err),
            "must be indistinguishable from absent: {err:?}"
        );
    }

    #[tokio::test]
    async fn daemon_with_nil_pairing_user_has_no_authority() {
        let store = MemoryStore::new();
        let org_id = Uuid::now_v7();
        let principal = daemon_principal(org_id, Uuid::nil());
        grant(&store, org_id, Uuid::nil(), "organization-admin", None).await;

        let err = authorize_organization_action(&store, &principal, org_id, Action::Read)
            .await
            .expect_err("nil pairing user must never carry authority");
        assert!(is_not_found(&err));
    }

    #[tokio::test]
    async fn daemon_authority_is_capped_at_contributor_even_for_admin_pairing_user() {
        let store = MemoryStore::new();
        let org_id = Uuid::now_v7();
        let user_id = Uuid::now_v7();
        grant(&store, org_id, user_id, "organization-admin", None).await;
        let principal = daemon_principal(org_id, user_id);

        let role = authorize_organization_action(&store, &principal, org_id, Action::SyncPush)
            .await
            .expect("contributor-level action must be permitted");
        assert_eq!(role, Role::Contributor);

        for action in [
            Action::Approve,
            Action::ManageRepository,
            Action::ManageOrganization,
        ] {
            let err = authorize_organization_action(&store, &principal, org_id, action)
                .await
                .expect_err("daemon must never inherit approval or management authority");
            assert!(is_not_found(&err));
        }
    }

    #[tokio::test]
    async fn daemon_is_downgraded_with_its_pairing_user() {
        let store = MemoryStore::new();
        let org_id = Uuid::now_v7();
        let user_id = Uuid::now_v7();
        grant(&store, org_id, user_id, "observer", None).await;
        let principal = daemon_principal(org_id, user_id);

        let role = authorize_organization_action(&store, &principal, org_id, Action::Read)
            .await
            .expect("observer-level read must be permitted");
        assert_eq!(role, Role::Observer);

        let err = authorize_organization_action(&store, &principal, org_id, Action::SyncPush)
            .await
            .expect_err("daemon of an observer must not be able to write");
        assert!(is_not_found(&err));
    }

    #[tokio::test]
    async fn daemon_cannot_reach_another_tenant() {
        let store = MemoryStore::new();
        let home_org = Uuid::now_v7();
        let other_org = Uuid::now_v7();
        let user_id = Uuid::now_v7();
        // The pairing user is an admin in the *other* organization.
        grant(&store, other_org, user_id, "organization-admin", None).await;
        let principal = daemon_principal(home_org, user_id);

        let err = authorize_organization_action(&store, &principal, other_org, Action::Read)
            .await
            .expect_err("daemon must not act outside its own organization");
        assert!(is_not_found(&err));
    }

    #[tokio::test]
    async fn daemon_repository_authority_follows_repository_scoped_grants() {
        let store = MemoryStore::new();
        let org_id = Uuid::now_v7();
        let user_id = Uuid::now_v7();
        let granted_repo = Uuid::now_v7();
        let other_repo = Uuid::now_v7();

        for (id, federated_id) in [(granted_repo, "repo-a"), (other_repo, "repo-b")] {
            store
                .create_repository(Repository {
                    id,
                    organization_id: org_id,
                    federated_id: federated_id.to_string(),
                    display_name: federated_id.to_string(),
                    max_publication_class: "metadata-shared".to_string(),
                    max_classification: "internal".to_string(),
                    policy_version: 1,
                    created_at: Utc::now(),
                })
                .await
                .expect("repository must be stored");
        }

        grant(&store, org_id, user_id, "contributor", Some(granted_repo)).await;
        let principal = daemon_principal(org_id, user_id);

        let role =
            authorize_repository_action(&store, &principal, org_id, granted_repo, Action::SyncPush)
                .await
                .expect("daemon may push to the repository its pairing user can push to");
        assert_eq!(role, Role::Contributor);

        let err =
            authorize_repository_action(&store, &principal, org_id, other_repo, Action::SyncPush)
                .await
                .expect_err("daemon must not reach repositories outside the pairing user's grants");
        assert!(is_not_found(&err));

        // A repository-scoped grant confers nothing organization-wide.
        let err = authorize_organization_action(&store, &principal, org_id, Action::Read)
            .await
            .expect_err("repository-scoped grant must not become organization-wide authority");
        assert!(is_not_found(&err));
    }

    #[tokio::test]
    async fn unknown_role_strings_confer_nothing() {
        let store = MemoryStore::new();
        let org_id = Uuid::now_v7();
        let user_id = Uuid::now_v7();
        grant(&store, org_id, user_id, "super-admin", None).await;
        let principal = daemon_principal(org_id, user_id);

        let err = authorize_organization_action(&store, &principal, org_id, Action::Read)
            .await
            .expect_err("unrecognised role strings must not be interpreted");
        assert!(is_not_found(&err));
    }
}
