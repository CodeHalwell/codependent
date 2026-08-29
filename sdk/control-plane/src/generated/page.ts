/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

/**
 * Actor kind responsible for an audited action.
 */
type AuditActorKind = ("user" | "daemon" | "system") | "unknown";
/**
 * Publication class hierarchy. Defines how far data may be synchronized or shared.
 */
type PublicationClass =
  "private-local" | "metadata-shared" | "content-shared" | "organization-knowledge" | "public-marketplace" | "unknown";
/**
 * Lifecycle state of a paired daemon instance.
 */
type DaemonState = ("pending" | "active" | "revoked" | "expired") | "unknown";
/**
 * State of an organization membership.
 */
type MembershipState = ("invited" | "active" | "suspended") | "unknown";
/**
 * Encryption mode for stored objects.
 */
type ObjectEncryption = ("none" | "envelope") | "unknown";
/**
 * Lifecycle state of a published object in object storage.
 */
type ObjectState = "uploading" | "available" | "tombstoned" | "unknown";
/**
 * Data sensitivity classification hierarchy.
 */
type DataClassification = "public" | "internal" | "confidential" | "secret" | "unknown";
/**
 * Standard control-plane roles.
 *
 * Ordering is by privilege, ascending, with `Unknown` lowest — but it is implemented via [`ControlPlaneRole::privilege_rank`] rather than derived. `#[serde(other)]` must sit on the **last** variant, while the fail-closed invariant needs `Unknown` to rank **below** every named role; a derived `Ord` cannot satisfy both, and deriving it after moving `Unknown` last would silently invert the ranking into "unknown outranks everything".
 */
type ControlPlaneRole =
  "observer" | "contributor" | "approver" | "maintainer" | "organization-admin" | "unknown";
/**
 * Lifecycle state of a shared session projection.
 */
type SharedSessionState = ("running" | "completed" | "failed" | "pending-approval" | "cancelled") | "unknown";
/**
 * Structured payload for stream events.
 */
type StreamEventPayload =
  | {
      body: string;
      category: string;
      id: string;
      read: boolean;
      title: string;
      type: "notification";
    }
  | {
      action_digest: string;
      approval_id: string;
      repository_id: string;
      requested_action: string;
      risk_level: string;
      type: "approval-request";
    }
  | {
      schedule_id: string;
      scheduled_time: string;
      target: string;
      type: "schedule-trigger";
    }
  | {
      attempt: number;
      details?: string | null;
      job_id: string;
      status: string;
      type: "runner-status";
    }
  | {
      max_classification: DataClassification;
      max_publication_class: PublicationClass;
      policy_version: number;
      type: "policy-update";
    }
  | {
      class: PublicationClass;
      delta_kind: SyncDeltaKind;
      payload: JsonValue;
      subject_id: string;
      type: "sync-delta";
    }
  | {
      type: "unknown";
    };
/**
 * Kind of delta payload contained in an outbound synchronization batch.
 */
type SyncDeltaKind =
  | (
      | "session-summary"
      | "run-summary"
      | "artifact-summary"
      | "inbox-entry"
      | "graph-batch"
      | "tombstone"
      | "approval-decision"
      | "usage-aggregate"
    )
  | "unknown";
/**
 * The distinct event streams supported by the control plane.
 */
type StreamKind =
  ("notifications" | "approvals" | "schedules" | "runner-events" | "policy") | "sessions" | "sync" | "unknown";
/**
 * Reason for a durable tombstone.
 */
type TombstoneReason = ("deleted" | "narrowed" | "revoked") | "unknown";
/**
 * Lifecycle state of a user account.
 */
type UserState = ("active" | "suspended" | "deleted") | "unknown";

/**
 * Every paginated response the control plane returns. `Page<T>` is generic, so it is unreachable from any other catalog: each list endpoint must be named here explicitly or a client has no type for the envelope it actually receives.
 */
export interface PageCatalog {
  audit_record_page: AuditRecordPage;
  cursor: string;
  daemon_page: DaemonPage;
  membership_page: OrganizationMembershipPage;
  organization_page: OrganizationSummaryPage;
  published_object_page: PublishedObjectPage;
  repository_page: RepositorySummaryPage;
  request: PageRequest;
  role_grant_page: RoleGrantPage;
  shared_session_page: SharedSessionPage;
  stream_event_page: StreamEventPage;
  sync_receipt_page: SyncReceiptPage;
  team_member_page: TeamMemberPage;
  team_page: TeamPage;
  tombstone_page: TombstonePage;
  user_page: UserSummaryPage;
}
/**
 * Standard paginated response envelope.
 *
 * The schema name is `<T>Page` (e.g. `AuditRecordPage`) rather than schemars' default `Page_for_AuditRecord`, because that default becomes `PageFor_AuditRecord` in the generated TypeScript — a name no client author would type.
 */
export interface AuditRecordPage {
  has_more: boolean;
  items: AuditRecord[];
  next_cursor?: string | null;
  /**
   * Bounded count computed strictly inside the authorized set.
   */
  total_count?: number | null;
}
/**
 * Immutable audit record with tamper-evident cryptographic hash chaining.
 */
interface AuditRecord {
  action: string;
  action_digest: string;
  actor_id?: string | null;
  actor_kind: AuditActorKind;
  correlation_id?: string | null;
  detail?: JsonValue;
  id: string;
  occurred_at: string;
  organization_id: string;
  prev_hash?: string | null;
  record_hash: string;
  target_id: string;
  target_kind: string;
}
/**
 * Standard paginated response envelope.
 *
 * The schema name is `<T>Page` (e.g. `AuditRecordPage`) rather than schemars' default `Page_for_AuditRecord`, because that default becomes `PageFor_AuditRecord` in the generated TypeScript — a name no client author would type.
 */
export interface DaemonPage {
  has_more: boolean;
  items: Daemon[];
  next_cursor?: string | null;
  /**
   * Bounded count computed strictly inside the authorized set.
   */
  total_count?: number | null;
}
/**
 * Registered daemon instance in the control plane.
 */
interface Daemon {
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
 * Standard paginated response envelope.
 *
 * The schema name is `<T>Page` (e.g. `AuditRecordPage`) rather than schemars' default `Page_for_AuditRecord`, because that default becomes `PageFor_AuditRecord` in the generated TypeScript — a name no client author would type.
 */
export interface OrganizationMembershipPage {
  has_more: boolean;
  items: OrganizationMembership[];
  next_cursor?: string | null;
  /**
   * Bounded count computed strictly inside the authorized set.
   */
  total_count?: number | null;
}
/**
 * Organization membership binding a user to an organization.
 */
interface OrganizationMembership {
  created_at: string;
  joined_at?: string | null;
  organization_id: string;
  state: MembershipState;
  user_id: string;
}
/**
 * Standard paginated response envelope.
 *
 * The schema name is `<T>Page` (e.g. `AuditRecordPage`) rather than schemars' default `Page_for_AuditRecord`, because that default becomes `PageFor_AuditRecord` in the generated TypeScript — a name no client author would type.
 */
export interface OrganizationSummaryPage {
  has_more: boolean;
  items: OrganizationSummary[];
  next_cursor?: string | null;
  /**
   * Bounded count computed strictly inside the authorized set.
   */
  total_count?: number | null;
}
/**
 * Compact summary of an organization for listings.
 */
interface OrganizationSummary {
  created_at: string;
  display_name: string;
  id: string;
  max_publication_class: PublicationClass;
  member_count: number;
  repository_count: number;
  slug: string;
}
/**
 * Standard paginated response envelope.
 *
 * The schema name is `<T>Page` (e.g. `AuditRecordPage`) rather than schemars' default `Page_for_AuditRecord`, because that default becomes `PageFor_AuditRecord` in the generated TypeScript — a name no client author would type.
 */
export interface PublishedObjectPage {
  has_more: boolean;
  items: PublishedObject[];
  next_cursor?: string | null;
  /**
   * Bounded count computed strictly inside the authorized set.
   */
  total_count?: number | null;
}
/**
 * Published object metadata record.
 */
interface PublishedObject {
  byte_length: number;
  class: PublicationClass;
  /**
   * Content address (SHA-256 digest of object bytes).
   */
  content_hash: string;
  created_at: string;
  encryption: ObjectEncryption;
  id: string;
  media_type: string;
  organization_id: string;
  repository_id?: string | null;
  state: ObjectState;
  uploaded_by_daemon?: string | null;
}
/**
 * Standard paginated response envelope.
 *
 * The schema name is `<T>Page` (e.g. `AuditRecordPage`) rather than schemars' default `Page_for_AuditRecord`, because that default becomes `PageFor_AuditRecord` in the generated TypeScript — a name no client author would type.
 */
export interface RepositorySummaryPage {
  has_more: boolean;
  items: RepositorySummary[];
  next_cursor?: string | null;
  /**
   * Bounded count computed strictly inside the authorized set.
   */
  total_count?: number | null;
}
/**
 * Compact repository summary for listings.
 */
interface RepositorySummary {
  created_at: string;
  display_name: string;
  federated_id: string;
  id: string;
  max_classification: DataClassification;
  max_publication_class: PublicationClass;
  organization_id: string;
  published_object_count: number;
  shared_session_count: number;
}
/**
 * Standard page query request.
 */
export interface PageRequest {
  cursor?: string | null;
  limit?: number | null;
}
/**
 * Standard paginated response envelope.
 *
 * The schema name is `<T>Page` (e.g. `AuditRecordPage`) rather than schemars' default `Page_for_AuditRecord`, because that default becomes `PageFor_AuditRecord` in the generated TypeScript — a name no client author would type.
 */
export interface RoleGrantPage {
  has_more: boolean;
  items: RoleGrant[];
  next_cursor?: string | null;
  /**
   * Bounded count computed strictly inside the authorized set.
   */
  total_count?: number | null;
}
/**
 * Role grant record binding a user or team to a role within an organization (and optional repository scope).
 */
interface RoleGrant {
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
 * Explicit scope constraints for scoped grants (e.g. Approver role).
 */
interface ActionScope {
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
 * Standard paginated response envelope.
 *
 * The schema name is `<T>Page` (e.g. `AuditRecordPage`) rather than schemars' default `Page_for_AuditRecord`, because that default becomes `PageFor_AuditRecord` in the generated TypeScript — a name no client author would type.
 */
export interface SharedSessionPage {
  has_more: boolean;
  items: SharedSession[];
  next_cursor?: string | null;
  /**
   * Bounded count computed strictly inside the authorized set.
   */
  total_count?: number | null;
}
/**
 * Shared session projection synchronized from a daemon.
 */
interface SharedSession {
  class: PublicationClass;
  daemon_id: string;
  id: string;
  last_activity_at?: string | null;
  organization_id: string;
  remote_session_key: string;
  repository_id: string;
  started_at: string;
  state: SharedSessionState;
  /**
   * Only populated at `content-shared` or wider; redacted to `None` below that.
   */
  title?: string | null;
  tombstoned_at?: string | null;
  updated_at: string;
}
/**
 * Standard paginated response envelope.
 *
 * The schema name is `<T>Page` (e.g. `AuditRecordPage`) rather than schemars' default `Page_for_AuditRecord`, because that default becomes `PageFor_AuditRecord` in the generated TypeScript — a name no client author would type.
 */
export interface StreamEventPage {
  has_more: boolean;
  items: StreamEvent[];
  next_cursor?: string | null;
  /**
   * Bounded count computed strictly inside the authorized set.
   */
  total_count?: number | null;
}
/**
 * Durable event in a resumable stream.
 */
interface StreamEvent {
  created_at: string;
  /**
   * Monotonic log sequence ID serving as resume cursor.
   */
  id: number;
  organization_id: string;
  payload: StreamEventPayload;
  repository_id?: string | null;
  stream: StreamKind;
}
/**
 * Standard paginated response envelope.
 *
 * The schema name is `<T>Page` (e.g. `AuditRecordPage`) rather than schemars' default `Page_for_AuditRecord`, because that default becomes `PageFor_AuditRecord` in the generated TypeScript — a name no client author would type.
 */
export interface SyncReceiptPage {
  has_more: boolean;
  items: SyncReceipt[];
  next_cursor?: string | null;
  /**
   * Bounded count computed strictly inside the authorized set.
   */
  total_count?: number | null;
}
/**
 * Receipt returned by the control plane confirming durable acceptance of a delta.
 */
interface SyncReceipt {
  accepted_at: string;
  /**
   * The class the control plane actually stored, after intersecting the requested class with the daemon's pairing ceiling. May be narrower than the class the daemon sent.
   */
  class: PublicationClass;
  daemon_id: string;
  daemon_sequence: number;
  delta_kind: SyncDeltaKind;
  /**
   * True when this sequence had already been durably accepted and the delta was replayed.
   */
  duplicate?: boolean;
  id: string;
  payload_hash: string;
}
/**
 * Standard paginated response envelope.
 *
 * The schema name is `<T>Page` (e.g. `AuditRecordPage`) rather than schemars' default `Page_for_AuditRecord`, because that default becomes `PageFor_AuditRecord` in the generated TypeScript — a name no client author would type.
 */
export interface TeamMemberPage {
  has_more: boolean;
  items: TeamMember[];
  next_cursor?: string | null;
  /**
   * Bounded count computed strictly inside the authorized set.
   */
  total_count?: number | null;
}
/**
 * Team member association.
 */
interface TeamMember {
  joined_at: string;
  team_id: string;
  user_id: string;
}
/**
 * Standard paginated response envelope.
 *
 * The schema name is `<T>Page` (e.g. `AuditRecordPage`) rather than schemars' default `Page_for_AuditRecord`, because that default becomes `PageFor_AuditRecord` in the generated TypeScript — a name no client author would type.
 */
export interface TeamPage {
  has_more: boolean;
  items: Team[];
  next_cursor?: string | null;
  /**
   * Bounded count computed strictly inside the authorized set.
   */
  total_count?: number | null;
}
/**
 * Team or workspace entity within an organization.
 */
interface Team {
  created_at: string;
  display_name: string;
  id: string;
  organization_id: string;
  slug: string;
}
/**
 * Standard paginated response envelope.
 *
 * The schema name is `<T>Page` (e.g. `AuditRecordPage`) rather than schemars' default `Page_for_AuditRecord`, because that default becomes `PageFor_AuditRecord` in the generated TypeScript — a name no client author would type.
 */
export interface TombstonePage {
  has_more: boolean;
  items: Tombstone[];
  next_cursor?: string | null;
  /**
   * Bounded count computed strictly inside the authorized set.
   */
  total_count?: number | null;
}
/**
 * Durable tombstone recording the deletion or revocation of an entity.
 */
interface Tombstone {
  applied_at?: string | null;
  created_at: string;
  id: string;
  organization_id: string;
  reason: TombstoneReason;
  subject_key: string;
  subject_kind: string;
}
/**
 * Standard paginated response envelope.
 *
 * The schema name is `<T>Page` (e.g. `AuditRecordPage`) rather than schemars' default `Page_for_AuditRecord`, because that default becomes `PageFor_AuditRecord` in the generated TypeScript — a name no client author would type.
 */
export interface UserSummaryPage {
  has_more: boolean;
  items: UserSummary[];
  next_cursor?: string | null;
  /**
   * Bounded count computed strictly inside the authorized set.
   */
  total_count?: number | null;
}
/**
 * Compact user summary for team listings and mentions.
 */
interface UserSummary {
  display_name: string;
  id: string;
  primary_email?: string | null;
  state: UserState;
}

type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
