/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

/**
 * Publication class hierarchy. Defines how far data may be synchronized or shared.
 */
type PublicationClass =
  "private-local" | "metadata-shared" | "content-shared" | "organization-knowledge" | "public-marketplace" | "unknown";
/**
 * Lifecycle state of a paired daemon instance.
 */
export type DaemonState = ("pending" | "active" | "revoked" | "expired") | "unknown";

export interface DaemonCatalog {
  consent_manifest: ConsentManifest;
  daemon: Daemon;
  exchange_request: ExchangePairingCodeRequest;
  exchange_response: ExchangePairingCodeResponse;
  initiate_request: InitiatePairingRequest;
  initiate_response: InitiatePairingResponse;
  pairing_challenge: PairingChallenge;
  pairing_scope: PairingScope;
  revoke_request: RevokeDaemonRequest;
  state: DaemonState;
}
/**
 * Consent manifest presented to the human on the local machine during pairing. A cryptographic digest is verified on every reconnection to prevent scope expansion.
 */
export interface ConsentManifest {
  accepts_remote_approvals: boolean;
  accepts_runner_dispatch: boolean;
  allowed_repositories: string[];
  created_at: string;
  endpoint: string;
  expires_at?: string | null;
  max_publication_class: PublicationClass;
  organization_display_name: string;
  organization_id: string;
}
/**
 * Registered daemon instance in the control plane.
 */
export interface Daemon {
  accepts_remote_approvals: boolean;
  accepts_runner_dispatch: boolean;
  consent_manifest_hash: string;
  created_at: string;
  display_name: string;
  id: string;
  last_seen_at?: string | null;
  max_publication_class: PublicationClass;
  organization_id: string;
  paired_at?: string | null;
  paired_by: string;
  revoked_at?: string | null;
  state: DaemonState;
}
/**
 * Request sent by a daemon to exchange a verified challenge code for permanent credentials.
 */
export interface ExchangePairingCodeRequest {
  challenge_code: string;
  consent_manifest: ConsentManifest;
  daemon_display_name: string;
}
/**
 * Response returned upon successful challenge code exchange.
 */
export interface ExchangePairingCodeResponse {
  access_token: string;
  daemon_id: string;
  expires_at: string;
  max_publication_class: PublicationClass;
  organization_id: string;
  refresh_token: string;
}
/**
 * Request to initiate a new daemon pairing challenge.
 */
export interface InitiatePairingRequest {
  organization_id: string;
  requested_scope: PairingScope;
}
/**
 * Scope requested for a daemon pairing challenge.
 */
export interface PairingScope {
  accepts_remote_approvals: boolean;
  accepts_runner_dispatch: boolean;
  max_publication_class: PublicationClass;
  repositories?: string[];
}
/**
 * Response returned when a pairing challenge is created.
 */
export interface InitiatePairingResponse {
  /**
   * Human-friendly pairing code to enter on the daemon CLI or UI.
   */
  challenge_code: string;
  expires_at: string;
  poll_interval_seconds: number;
  /**
   * Direct pairing verification URL.
   */
  verification_uri: string;
}
/**
 * Single-use short-lived pairing challenge initiated by a user.
 */
export interface PairingChallenge {
  code_hash: string;
  consumed_at?: string | null;
  created_at: string;
  daemon_id?: string | null;
  expires_at: string;
  initiated_by: string;
  organization_id: string;
  requested_scope: PairingScope;
}
/**
 * Request to revoke a paired daemon instance.
 */
export interface RevokeDaemonRequest {
  daemon_id: string;
  reason: string;
}
