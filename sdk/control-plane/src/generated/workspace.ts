/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

/**
 * State of an organization membership.
 */
export type MembershipState = ("invited" | "active" | "suspended") | "unknown";

export interface WorkspaceCatalog {
  add_member: AddTeamMemberRequest;
  create_team: CreateTeamRequest;
  membership: OrganizationMembership;
  membership_state: MembershipState;
  team: Team;
  team_member: TeamMember;
  update_team: UpdateTeamRequest;
  workspace: Workspace;
}
/**
 * Add member to team request.
 */
export interface AddTeamMemberRequest {
  user_id: string;
}
/**
 * Request to create a new team or workspace.
 */
export interface CreateTeamRequest {
  display_name: string;
  slug: string;
}
/**
 * Organization membership binding a user to an organization.
 */
export interface OrganizationMembership {
  created_at: string;
  joined_at?: string | null;
  organization_id: string;
  state: MembershipState;
  user_id: string;
}
/**
 * Team or workspace entity within an organization.
 */
export interface Team {
  created_at: string;
  display_name: string;
  id: string;
  organization_id: string;
  slug: string;
}
/**
 * Team member association.
 */
export interface TeamMember {
  joined_at: string;
  team_id: string;
  user_id: string;
}
/**
 * Request to update an existing team or workspace.
 */
export interface UpdateTeamRequest {
  display_name?: string | null;
}
/**
 * Workspace projection (representing team / workspace environment).
 */
export interface Workspace {
  created_at: string;
  display_name: string;
  id: string;
  organization_id: string;
  slug: string;
}
