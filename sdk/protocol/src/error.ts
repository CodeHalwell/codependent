/** Mirrors `crates/protocol/src/error.rs`. */

import type { CorrelationId, JsonValue } from "./ids.js";

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type UserAction =
  | { type: "Retry" }
  | { type: "Reauthenticate" }
  | { type: "GrantApproval" }
  | { type: "AdjustPolicy" }
  | { type: "ReconfigureModel" }
  | { type: "ContactSupport" }
  | { type: "Unknown" };

export interface CodypendentError {
  code: string;
  message: string;
  retryable: boolean;
  user_action?: UserAction;
  /** `skip_serializing_if = "Value::is_null"` — absent, never `null`. */
  details?: JsonValue;
  correlation_id: CorrelationId;
}

/** The transport-level error carried by `Payload::Error`. */
export interface ProtocolError {
  code: string;
  message: string;
  retryable: boolean;
}
