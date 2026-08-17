/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

/**
 * Granular actions protected by control-plane RBAC.
 */
export type RbacAction =
  | (
      | "read-metadata"
      | "read-content"
      | "write-content"
      | "approve-action"
      | "manage-repositories"
      | "manage-team"
      | "manage-organization"
      | "dispatch-runner"
      | "read-audit-logs"
    )
  | "unknown";
/**
 * Standard control-plane roles.
 *
 * Ordering is by privilege, ascending, with `Unknown` lowest — but it is implemented via [`ControlPlaneRole::privilege_rank`] rather than derived. `#[serde(other)]` must sit on the **last** variant, while the fail-closed invariant needs `Unknown` to rank **below** every named role; a derived `Ord` cannot satisfy both, and deriving it after moving `Unknown` last would silently invert the ranking into "unknown outranks everything".
 */
export type ControlPlaneRole =
  "observer" | "contributor" | "approver" | "maintainer" | "organization-admin" | "unknown";

export interface RbacCatalog {
  action: RbacAction;
  action_scope: ActionScope;
  create_grant: CreateRoleGrantRequest;
  grant: RoleGrant;
  revoke_grant: RevokeRoleGrantRequest;
  role: ControlPlaneRole;
}
/**
 * Explicit scope constraints for scoped grants (e.g. Approver role).
 */
export interface ActionScope {
  /**
   * Specific action types permitted (e.g. "ExecuteCommand", "WriteFile").
   */
  action_kinds?: string[] | null;
  /**
   * Maximum risk level allowed for auto-delegated approval.
   */
  max_risk_level?: string | null;
  /**
   * Repositories to which this approval grant is restricted.
   */
  repositories?: string[] | null;
}
/**
 * Request to create a new role grant.
 */
export interface CreateRoleGrantRequest {
  action_scope?: ActionScope | null;
  expires_at?: string | null;
  repository_id?: string | null;
  role: ControlPlaneRole;
  team_id?: string | null;
  user_id?: string | null;
}
/**
 * Role grant record binding a user or team to a role within an organization (and optional repository scope).
 */
export interface RoleGrant {
  /**
   * Required for Approver role; optional for others.
   */
  action_scope?: ActionScope | null;
  expires_at?: string | null;
  granted_at: string;
  granted_by: string;
  id: string;
  organization_id: string;
  /**
   * Optional repository scope (None = organization-wide).
   */
  repository_id?: string | null;
  revoked_at?: string | null;
  role: ControlPlaneRole;
  team_id?: string | null;
  /**
   * Exactly one of user_id or team_id must be set.
   */
  user_id?: string | null;
}
/**
 * Request to revoke an existing role grant.
 */
export interface RevokeRoleGrantRequest {
  grant_id: string;
}
