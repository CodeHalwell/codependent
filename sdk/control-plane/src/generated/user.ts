/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

/**
 * Supported external authentication identity providers.
 */
export type IdentityProvider = ("github" | "oidc") | "unknown";
/**
 * Lifecycle state of a user account.
 */
export type UserState = ("active" | "suspended" | "deleted") | "unknown";

export interface UserCatalog {
  identity: UserIdentity;
  link_request: IdentityLinkRequest;
  link_result: IdentityLinkResult;
  provider: IdentityProvider;
  state: UserState;
  summary: UserSummary;
  update_request: UpdateUserRequest;
  user: User;
}
/**
 * External identity linked to a control-plane user account.
 */
export interface UserIdentity {
  email_at_link?: string | null;
  id: string;
  issuer: string;
  link_audit_id: string;
  linked_at: string;
  provider: IdentityProvider;
  subject: string;
  user_id: string;
}
/**
 * Request to link a new external identity to the current authenticated account.
 */
export interface IdentityLinkRequest {
  auth_code: string;
  code_verifier: string;
  issuer: string;
  provider: IdentityProvider;
  redirect_uri: string;
}
/**
 * Result of an identity linking operation.
 */
export interface IdentityLinkResult {
  identity_id: string;
  linked_at: string;
  provider: IdentityProvider;
  user_id: string;
}
/**
 * Compact user summary for team listings and mentions.
 */
export interface UserSummary {
  display_name: string;
  id: string;
  primary_email?: string | null;
  state: UserState;
}
/**
 * Request to update a user's details.
 */
export interface UpdateUserRequest {
  display_name?: string | null;
  primary_email?: string | null;
}
/**
 * User entity in the control plane.
 */
export interface User {
  created_at: string;
  display_name: string;
  id: string;
  primary_email?: string | null;
  state: UserState;
  updated_at: string;
}
