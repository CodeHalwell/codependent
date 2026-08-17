/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

export interface ControlPlaneIdCatalog {
  audit_record_id: string;
  challenge_id: string;
  correlation_id: string;
  daemon_id: string;
  federated_repository_id: string;
  grant_id: string;
  identity_id: string;
  organization_id: string;
  published_object_id: string;
  refresh_token_id: string;
  repository_id: string;
  runner_job_id: string;
  runner_lease_id: string;
  sha256_digest: string;
  shared_session_id: string;
  sync_receipt_id: string;
  team_id: string;
  timestamp: string;
  tombstone_id: string;
  user_id: string;
  workload_credential_id: string;
  workspace_id: string;
}

export type AuditRecordId = ControlPlaneIdCatalog["audit_record_id"];
export type ChallengeId = ControlPlaneIdCatalog["challenge_id"];
export type CorrelationId = ControlPlaneIdCatalog["correlation_id"];
export type DaemonId = ControlPlaneIdCatalog["daemon_id"];
export type FederatedRepositoryId = ControlPlaneIdCatalog["federated_repository_id"];
export type GrantId = ControlPlaneIdCatalog["grant_id"];
export type IdentityId = ControlPlaneIdCatalog["identity_id"];
export type OrganizationId = ControlPlaneIdCatalog["organization_id"];
export type PublishedObjectId = ControlPlaneIdCatalog["published_object_id"];
export type RefreshTokenId = ControlPlaneIdCatalog["refresh_token_id"];
export type RepositoryId = ControlPlaneIdCatalog["repository_id"];
export type RunnerJobId = ControlPlaneIdCatalog["runner_job_id"];
export type RunnerLeaseId = ControlPlaneIdCatalog["runner_lease_id"];
export type Sha256Digest = ControlPlaneIdCatalog["sha256_digest"];
export type SharedSessionId = ControlPlaneIdCatalog["shared_session_id"];
export type SyncReceiptId = ControlPlaneIdCatalog["sync_receipt_id"];
export type TeamId = ControlPlaneIdCatalog["team_id"];
export type Timestamp = ControlPlaneIdCatalog["timestamp"];
export type TombstoneId = ControlPlaneIdCatalog["tombstone_id"];
export type UserId = ControlPlaneIdCatalog["user_id"];
export type WorkloadCredentialId = ControlPlaneIdCatalog["workload_credential_id"];
export type WorkspaceId = ControlPlaneIdCatalog["workspace_id"];

export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
