/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

/**
 * Explicit purpose to which a workload credential is cryptographically bound.
 */
export type CredentialPurpose = ("sync" | "pairing" | "runner-job") | "unknown";

export interface WorkloadCatalog {
  credential: WorkloadCredential;
  purpose: CredentialPurpose;
  service_token: ServiceAccountToken;
}
/**
 * Workload credential record (e.g. for daemons or remote runners).
 */
export interface WorkloadCredential {
  audience: string;
  daemon_id?: string | null;
  expires_at: string;
  id: string;
  issued_at: string;
  purpose: CredentialPurpose;
  revoked_at?: string | null;
}
/**
 * Service account or workload authentication token representation.
 */
export interface ServiceAccountToken {
  expires_at: string;
  purpose: CredentialPurpose;
  token: string;
  token_type: string;
}
