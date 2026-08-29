import type {
  AuthTokenResponse as WireAuthTokenResponse,
  CreateOrganizationRequest as WireCreateOrganizationRequest,
  Organization as WireOrganization,
  PublishedObject as WirePublishedObject,
  RegisterRepositoryRequest as WireRegisterRepositoryRequest,
  Repository as WireRepository,
  SharedSession as WireSharedSession,
  AuditRecord as WireAuditRecord,
} from "./generated/index.js";
import type {
  AuditRecord,
  AuthTokens,
  CreateOrganizationRequest,
  Organization,
  PublishedObject,
  RegisterRepositoryRequest,
  Repository,
  SharedSession,
} from "./types/index.js";

/**
 * The organization table has no `updated_at` column yet, so the deployed route
 * honestly returns the protocol organization minus that field.
 */
export type OrganizationWireResponse = Omit<WireOrganization, "updated_at">;

/** The deployed create route has not implemented residency/retention inputs. */
export type CreateOrganizationWireRequest = Pick<
  WireCreateOrganizationRequest,
  "slug" | "display_name" | "max_publication_class" | "max_classification"
>;

/**
 * The public React-facing model remains camelCase for compatibility. These
 * functions are the only place where the generated, snake_case wire contract
 * crosses that boundary.
 */
export function authTokensFromWire(wire: WireAuthTokenResponse): AuthTokens {
  if (!wire.refresh_token) {
    throw new Error("refresh response did not include a replacement refresh token");
  }
  return {
    accessToken: wire.access_token,
    refreshToken: wire.refresh_token,
    tokenType: "Bearer",
    expiresIn: wire.expires_in,
  };
}

export function createOrganizationToWire(
  value: CreateOrganizationRequest,
): CreateOrganizationWireRequest {
  return {
    slug: value.slug,
    display_name: value.displayName,
    max_publication_class: value.maxPublicationClass ?? null,
    max_classification: value.maxClassification ?? null,
  };
}

export function organizationFromWire(wire: OrganizationWireResponse): Organization {
  return {
    id: wire.id,
    slug: wire.slug,
    displayName: wire.display_name,
    maxPublicationClass: wire.max_publication_class,
    maxClassification: wire.max_classification,
    dataResidency: wire.data_residency ?? null,
    retentionDays: wire.retention_days ?? null,
    policyVersion: wire.policy_version,
    createdAt: wire.created_at,
  };
}

export function publishedObjectFromWire(wire: WirePublishedObject): PublishedObject {
  return {
    id: wire.id,
    organizationId: wire.organization_id,
    repositoryId: wire.repository_id ?? null,
    contentHash: wire.content_hash,
    byteLength: wire.byte_length,
    mediaType: wire.media_type,
    class: wire.class,
    encryption: wire.encryption,
    state: wire.state,
    uploadedByDaemon: wire.uploaded_by_daemon ?? null,
    createdAt: wire.created_at,
  };
}

export function registerRepositoryToWire(
  value: RegisterRepositoryRequest,
): WireRegisterRepositoryRequest {
  return {
    federated_id: value.federatedId,
    display_name: value.displayName,
    max_publication_class: value.maxPublicationClass ?? null,
    max_classification: value.maxClassification ?? null,
  };
}

export function repositoryFromWire(wire: WireRepository): Repository {
  return {
    id: wire.id,
    organizationId: wire.organization_id,
    federatedId: wire.federated_id,
    displayName: wire.display_name,
    maxPublicationClass: wire.max_publication_class,
    maxClassification: wire.max_classification,
    policyVersion: wire.policy_version,
    createdAt: wire.created_at,
  };
}

export function sharedSessionFromWire(wire: WireSharedSession): SharedSession {
  return {
    id: wire.id,
    organizationId: wire.organization_id,
    repositoryId: wire.repository_id,
    daemonId: wire.daemon_id,
    remoteSessionKey: wire.remote_session_key,
    class: wire.class,
    title: wire.title ?? null,
    state: wire.state,
    startedAt: wire.started_at,
    lastActivityAt: wire.last_activity_at ?? null,
    tombstonedAt: wire.tombstoned_at ?? null,
    updatedAt: wire.updated_at,
  };
}

export function auditRecordFromWire(wire: WireAuditRecord): AuditRecord {
  return {
    id: wire.id,
    organizationId: wire.organization_id,
    actorKind: wire.actor_kind,
    actorId: wire.actor_id ?? null,
    action: wire.action,
    targetKind: wire.target_kind,
    targetId: wire.target_id,
    actionDigest: wire.action_digest,
    correlationId: wire.correlation_id ?? null,
    prevHash: wire.prev_hash ?? null,
    recordHash: wire.record_hash,
    detail:
      wire.detail && typeof wire.detail === "object" && !Array.isArray(wire.detail)
        ? wire.detail
        : {},
    occurredAt: wire.occurred_at,
  };
}
