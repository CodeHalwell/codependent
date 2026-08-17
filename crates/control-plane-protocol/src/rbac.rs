//! Role-Based Access Control (RBAC) models, actions, and evaluation logic.
//!
//! Control-plane roles govern remote access to control-plane resources only.
//! Note: Remote roles never map to local client roles.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{GrantId, OrganizationId, RepositoryId, TeamId, UserId};

/// Standard control-plane roles.
///
/// Ordering is by privilege, ascending, with `Unknown` lowest — but it is
/// implemented via [`ControlPlaneRole::privilege_rank`] rather than derived.
/// `#[serde(other)]` must sit on the **last** variant, while the fail-closed
/// invariant needs `Unknown` to rank **below** every named role; a derived `Ord`
/// cannot satisfy both, and deriving it after moving `Unknown` last would
/// silently invert the ranking into "unknown outranks everything".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ControlPlaneRole {
    /// Read-only access to metadata.
    Observer,
    /// Read and write access to sessions, code graph, and artifacts.
    Contributor,
    /// Authorized to grant approvals within an explicit action scope.
    Approver,
    /// Administrative control over repositories and teams within the organization.
    Maintainer,
    /// Full administrative control over the entire organization.
    OrganizationAdmin,
    /// Unrecognized or newer role name. Ranks below every named role and
    /// `permits` denies every action for it.
    #[serde(other)]
    Unknown,
}

impl ControlPlaneRole {
    /// Privilege rank, ascending. `Unknown` is 0 so an unrecognized role from a
    /// newer peer can never compare as more privileged than a role this build
    /// understands.
    #[must_use]
    pub fn privilege_rank(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Observer => 1,
            Self::Contributor => 2,
            Self::Approver => 3,
            Self::Maintainer => 4,
            Self::OrganizationAdmin => 5,
        }
    }
}

impl Ord for ControlPlaneRole {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.privilege_rank().cmp(&other.privilege_rank())
    }
}

impl PartialOrd for ControlPlaneRole {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Granular actions protected by control-plane RBAC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RbacAction {
    ReadMetadata,
    ReadContent,
    WriteContent,
    ApproveAction,
    ManageRepositories,
    ManageTeam,
    ManageOrganization,
    DispatchRunner,
    ReadAuditLogs,
    /// Unrecognized or newer action name. No role ever permits it.
    #[serde(other)]
    Unknown,
}

/// Explicit scope constraints for scoped grants (e.g. Approver role).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ActionScope {
    /// Repositories to which this approval grant is restricted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repositories: Option<Vec<RepositoryId>>,
    /// Specific action types permitted (e.g. "ExecuteCommand", "WriteFile").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_kinds: Option<Vec<String>>,
    /// Maximum risk level allowed for auto-delegated approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_risk_level: Option<String>,
}

impl ControlPlaneRole {
    /// Determine whether this role inherently permits the specified action.
    #[must_use]
    pub fn permits(&self, action: RbacAction) -> bool {
        if action == RbacAction::Unknown {
            return false;
        }
        match self {
            // Fail closed: an unrecognized role grants nothing.
            Self::Unknown => false,
            Self::Observer => matches!(action, RbacAction::ReadMetadata),
            Self::Contributor => matches!(
                action,
                RbacAction::ReadMetadata | RbacAction::ReadContent | RbacAction::WriteContent
            ),
            Self::Approver => matches!(
                action,
                RbacAction::ReadMetadata
                    | RbacAction::ReadContent
                    | RbacAction::WriteContent
                    | RbacAction::ApproveAction
            ),
            Self::Maintainer => matches!(
                action,
                RbacAction::ReadMetadata
                    | RbacAction::ReadContent
                    | RbacAction::WriteContent
                    | RbacAction::ApproveAction
                    | RbacAction::ManageRepositories
                    | RbacAction::ManageTeam
                    | RbacAction::DispatchRunner
            ),
            Self::OrganizationAdmin => true,
        }
    }
}

/// Role grant record binding a user or team to a role within an organization (and optional repository scope).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RoleGrant {
    pub id: GrantId,
    pub organization_id: OrganizationId,
    /// Exactly one of user_id or team_id must be set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<UserId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<TeamId>,
    /// Optional repository scope (None = organization-wide).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<RepositoryId>,
    pub role: ControlPlaneRole,
    /// Required for Approver role; optional for others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_scope: Option<ActionScope>,
    pub granted_by: UserId,
    pub granted_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

impl RoleGrant {
    /// Check whether the grant is currently active (not revoked and not expired).
    #[must_use]
    pub fn is_active_at(&self, now: DateTime<Utc>) -> bool {
        if self.revoked_at.is_some() {
            return false;
        }
        if let Some(expires_at) = self.expires_at {
            if now > expires_at {
                return false;
            }
        }
        true
    }

    /// Check whether this grant applies to the given user, considering direct grant or team membership.
    #[must_use]
    pub fn applies_to_user(&self, user_id: &UserId, user_teams: &[TeamId]) -> bool {
        if let Some(ref target_user) = self.user_id {
            if target_user == user_id {
                return true;
            }
        }
        if let Some(ref target_team) = self.team_id {
            if user_teams.contains(target_team) {
                return true;
            }
        }
        false
    }

    /// Check whether this grant applies to a specific repository.
    #[must_use]
    pub fn applies_to_repository(&self, repo_id: &RepositoryId) -> bool {
        match self.repository_id {
            None => true, // Organization-wide
            Some(ref scoped_id) => scoped_id == repo_id,
        }
    }
}

/// Request to create a new role grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CreateRoleGrantRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<UserId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<TeamId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<RepositoryId>,
    pub role: ControlPlaneRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_scope: Option<ActionScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

/// Request to revoke an existing role grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RevokeRoleGrantRequest {
    pub grant_id: GrantId,
}
