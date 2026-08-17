/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

/**
 * Actor kind responsible for an audited action.
 */
export type AuditActorKind = ("user" | "daemon" | "system") | "unknown";

export interface AuditCatalog {
  actor_kind: AuditActorKind;
  query: AuditQuery;
  record: AuditRecord;
}
/**
 * Query parameters for filtering audit logs.
 */
export interface AuditQuery {
  action?: string | null;
  actor_id?: string | null;
  cursor?: string | null;
  from_time?: string | null;
  limit?: number | null;
  target_id?: string | null;
  target_kind?: string | null;
  to_time?: string | null;
}
/**
 * Immutable audit record with tamper-evident cryptographic hash chaining.
 */
export interface AuditRecord {
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

type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
