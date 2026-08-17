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
 * Kind of delta payload contained in an outbound synchronization batch.
 */
export type SyncDeltaKind =
  | (
      | "session-summary"
      | "run-summary"
      | "inbox-entry"
      | "graph-batch"
      | "tombstone"
      | "approval-decision"
      | "usage-aggregate"
    )
  | "unknown";
/**
 * Lifecycle state of a shared session projection.
 */
export type SharedSessionState = ("running" | "completed" | "failed" | "pending-approval" | "cancelled") | "unknown";
/**
 * Reason for a durable tombstone.
 */
export type TombstoneReason = ("deleted" | "narrowed" | "revoked") | "unknown";

export interface SyncCatalog {
  batch_response: SyncBatchResponse;
  cursor: SyncCursor;
  delta: SyncDelta;
  delta_kind: SyncDeltaKind;
  envelope: SyncEnvelope;
  receipt: SyncReceipt;
  rejection: SyncRejection;
  shared_session: SharedSession;
  shared_session_state: SharedSessionState;
  tombstone: Tombstone;
  tombstone_reason: TombstoneReason;
}
/**
 * Control-plane response to an outbound synchronization batch.
 */
export interface SyncBatchResponse {
  latest_sequence: number;
  receipts: SyncReceipt[];
  rejected_deltas?: SyncRejection[];
}
/**
 * Receipt returned by the control plane confirming durable acceptance of a delta.
 */
export interface SyncReceipt {
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
 * Explicit rejection of a delta during batch synchronization.
 */
export interface SyncRejection {
  code: string;
  reason: string;
  sequence: number;
}
/**
 * Synchronization stream resume cursor.
 */
export interface SyncCursor {
  cursor: string;
  pairing_id: string;
  stream: string;
  updated_at: string;
}
/**
 * An individual synchronized delta emitted by a local daemon.
 */
export interface SyncDelta {
  class: PublicationClass;
  created_at: string;
  id: string;
  kind: SyncDeltaKind;
  payload: JsonValue;
  payload_hash: string;
  /**
   * Repository this delta is scoped to, when the delta kind is repository-scoped. The control plane never trusts this value beyond selecting a projection target: the effective organization always comes from the authenticated daemon row.
   */
  repository_id?: string | null;
  /**
   * Monotonic sequence number per paired daemon instance.
   */
  sequence: number;
  subject_id: string;
}
/**
 * Outbound batch synchronization envelope sent by a daemon to the control plane.
 */
export interface SyncEnvelope {
  daemon_id: string;
  deltas: SyncDelta[];
  organization_id: string;
  protocol_version: ProtocolVersion;
  sent_at: string;
}
/**
 * Control plane wire protocol version.
 */
interface ProtocolVersion {
  major: number;
  minor: number;
}
/**
 * Shared session projection synchronized from a daemon.
 */
export interface SharedSession {
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
 * Durable tombstone recording the deletion or revocation of an entity.
 */
export interface Tombstone {
  applied_at?: string | null;
  created_at: string;
  id: string;
  organization_id: string;
  reason: TombstoneReason;
  subject_key: string;
  subject_kind: string;
}

type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
