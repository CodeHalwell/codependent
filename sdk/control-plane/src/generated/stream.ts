/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

/**
 * Structured payload for stream events.
 */
export type StreamEventPayload =
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
 * Data sensitivity classification hierarchy.
 */
type DataClassification = "public" | "internal" | "confidential" | "secret" | "unknown";
/**
 * Publication class hierarchy. Defines how far data may be synchronized or shared.
 */
type PublicationClass =
  "private-local" | "metadata-shared" | "content-shared" | "organization-knowledge" | "public-marketplace" | "unknown";
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
export type StreamKind =
  ("notifications" | "approvals" | "schedules" | "runner-events" | "policy") | "sessions" | "sync" | "unknown";

export interface StreamCatalog {
  approval_request: ApprovalRequestEvent;
  event: StreamEvent;
  kind: StreamKind;
  notification: NotificationEvent;
  payload: StreamEventPayload;
  policy_update: PolicyUpdateEvent;
  runner_status: RunnerStatusEvent;
  schedule_trigger: ScheduleTriggerEvent;
  subscribe_request: StreamSubscribeRequest;
}
/**
 * Remote approval request delivery event.
 */
export interface ApprovalRequestEvent {
  action_digest: string;
  approval_id: string;
  repository_id: string;
  requested_action: string;
  risk_level: string;
}
/**
 * Durable event in a resumable stream.
 */
export interface StreamEvent {
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
 * Notification event.
 */
export interface NotificationEvent {
  body: string;
  category: string;
  id: string;
  read: boolean;
  title: string;
}
/**
 * Organization policy update event.
 */
export interface PolicyUpdateEvent {
  max_classification: DataClassification;
  max_publication_class: PublicationClass;
  policy_version: number;
}
/**
 * Runner execution status event.
 */
export interface RunnerStatusEvent {
  attempt: number;
  details?: string | null;
  job_id: string;
  status: string;
}
/**
 * Schedule trigger event.
 */
export interface ScheduleTriggerEvent {
  schedule_id: string;
  scheduled_time: string;
  target: string;
}
/**
 * Client subscription request to open a stream.
 */
export interface StreamSubscribeRequest {
  from_cursor?: number | null;
  repository_id?: string | null;
  stream: StreamKind;
}

type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
