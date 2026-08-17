/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

/**
 * Lifecycle state of a user account.
 */
type UserState = ("active" | "suspended" | "deleted") | "unknown";
/**
 * Supported external authentication identity providers.
 */
type IdentityProvider = ("github" | "oidc") | "unknown";

export interface AuthCatalog {
  auth_token_response: AuthTokenResponse;
  oauth_callback_request: OAuthCallbackRequest;
  oauth_init_request: OAuthInitRequest;
  oauth_init_response: OAuthInitResponse;
  refresh_token_request: RefreshTokenRequest;
  revoke_token_request: RevokeTokenRequest;
}
/**
 * Token response returned upon successful authentication.
 */
export interface AuthTokenResponse {
  access_token: string;
  expires_in: number;
  refresh_token?: string | null;
  token_type: string;
  user?: User | null;
}
/**
 * User entity in the control plane.
 */
interface User {
  created_at: string;
  display_name: string;
  id: string;
  primary_email?: string | null;
  state: UserState;
  updated_at: string;
}
/**
 * Request to complete OAuth callback and exchange authorization code.
 */
export interface OAuthCallbackRequest {
  code: string;
  code_verifier: string;
  state: string;
}
/**
 * Request to initiate an OAuth 2.0 / OIDC login or link flow.
 */
export interface OAuthInitRequest {
  code_challenge: string;
  code_challenge_method: string;
  provider: IdentityProvider;
  redirect_uri: string;
  state: string;
}
/**
 * Response returned with the external authorization URL.
 */
export interface OAuthInitResponse {
  authorization_url: string;
  state: string;
}
/**
 * Request to rotate a refresh token.
 */
export interface RefreshTokenRequest {
  refresh_token: string;
}
/**
 * Request to revoke an active token or session.
 */
export interface RevokeTokenRequest {
  token: string;
}
