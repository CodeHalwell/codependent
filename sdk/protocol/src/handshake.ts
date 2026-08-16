/** Mirrors `crates/protocol/src/handshake.rs`. */

import type { ClientCapabilities } from "./capabilities.js";
import type { DaemonInstanceId, DocumentId, RunId } from "./ids.js";
import type { ProtocolVersion } from "./version.js";

/** `#[serde(transparent)]` newtype over `String`. */
export type ResumeToken = string;

export interface ClientHello {
  client_name: string;
  client_version: string;
  supported_protocols: ProtocolVersion[];
  capabilities: ClientCapabilities;
  resume_token?: ResumeToken;
}

export interface ServerHello {
  selected_protocol: ProtocolVersion;
  daemon_version: string;
  daemon_instance: DaemonInstanceId;
  heartbeat_interval_ms: number;
  resume_token?: ResumeToken;
  /** `#[serde(default)]` with no skip — always present. */
  build_id: string;
}

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type ClientRole =
  | { type: "Observer" }
  | { type: "Contributor" }
  | { type: "Controller" }
  | { type: "Approver" }
  | { type: "Unknown" };

/** `#[serde(tag = "type")]` + `#[serde(other)] Unknown`. */
export type Subscription =
  | { type: "SessionSummary" }
  | { type: "RunTrace"; run_id: RunId }
  | { type: "AgentActivity" }
  | { type: "RepositoryStatus" }
  | { type: "BudgetState" }
  | { type: "Document"; document_id: DocumentId }
  | { type: "Blackboard"; workflow_run_id: string }
  | { type: "Workflow"; workflow_run_id: string }
  | { type: "Unknown" };
