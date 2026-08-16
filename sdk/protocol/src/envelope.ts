/** Mirrors `crates/protocol/src/envelope.rs`. */

import type { ArtifactRef } from "./artifact.js";
import type { BlackboardItemView } from "./blackboard.js";
import type { Catchup } from "./catchup.js";
import type { CodeGraphPage, CodeGraphScanReport, CodeGraphStatusView } from "./codegraph.js";
import type { Command, FileMatchWire, SessionSummary, UiPluginLifecycleStatus } from "./commands.js";
import type { DocumentLeaseGrant, DocumentSync } from "./document.js";
import type { CodypendentError, ProtocolError } from "./error.js";
import type { SessionEvent } from "./events.js";
import type { ClientHello, ServerHello } from "./handshake.js";
import type {
  ApprovalId,
  ArtifactId,
  ClientId,
  CommandId,
  DaemonInstanceId,
  DocumentId,
  MemoryId,
  MessageId,
  RunId,
  SessionId,
  Timestamp,
  WorkspaceId,
} from "./ids.js";
import type { MemoryEvidence, MemoryView } from "./memory.js";
import type { ProtocolVersion } from "./version.js";
import type { WorkflowEvent, WorkflowRunSnapshot } from "./workflow.js";

export interface DaemonStatus {
  daemon_version: string;
  protocol_version: ProtocolVersion;
  instance_id: DaemonInstanceId;
  pid: number;
  started_at: Timestamp;
  uptime_seconds: number;
  boot_count: number;
  database_path: string;
  socket_path: string;
  session_count: number;
  /** `#[serde(default)]` with no skip — always present. */
  build_id: string;
  /** `#[serde(default)]` with no skip — always present. */
  active_run_count: number;
  /** `skip_serializing_if = "Vec::is_empty"`. */
  integration_issues?: string[];
}

/** The transport frame. Only `payload` is mandatory beyond the routing header. */
export interface Envelope {
  protocol_version: ProtocolVersion;
  message_id: MessageId;
  correlation_id?: MessageId;
  client_id: ClientId;
  workspace_id?: WorkspaceId;
  session_id?: SessionId;
  sequence?: number;
  payload: Payload;
}

/**
 * `#[serde(tag = "type")]` + `#[serde(other)] Unknown`.
 *
 * Rust newtype variants (`Payload::Event(SessionEvent)`,
 * `Payload::ClientHello(ClientHello)`, …) are internally tagged, which means
 * the inner struct's fields sit ALONGSIDE `type` rather than nested under a
 * key — those variants are modeled here as intersections.
 */
export type Payload =
  // --- liveness / lifecycle ---
  | { type: "Ping" }
  | { type: "Pong" }
  | { type: "DaemonStatusRequest" }
  | ({ type: "DaemonStatusResponse" } & DaemonStatus)
  | { type: "Shutdown" }
  | { type: "ShutdownAck" }
  | { type: "ShutdownIfIdle" }
  | { type: "ShutdownRefused"; active_run_count: number }
  | ({ type: "Error" } & ProtocolError)
  // --- handshake and command flow ---
  | ({ type: "ClientHello" } & ClientHello)
  | ({ type: "ServerHello" } & ServerHello)
  | ({ type: "Command" } & Command)
  | { type: "CommandAccepted"; command_id: CommandId; sequence?: number; created_run?: RunId }
  | ({ type: "CommandRejected" } & CodypendentError)
  // --- documents ---
  | { type: "DocumentLeaseGranted"; command_id: CommandId; grant: DocumentLeaseGrant }
  | { type: "DocumentCreated"; command_id: CommandId; document_id: DocumentId }
  | ({ type: "DocumentSync" } & DocumentSync)
  | {
      type: "DocumentPublishRequested";
      command_id: CommandId;
      approval_id: ApprovalId;
      target: string;
      changed_files: string[];
      git_action: string;
    }
  | {
      type: "DocsCheckCompleted";
      command_id: CommandId;
      documents_checked: number;
      links_resolved: number;
      stale_findings: number;
      suggestions_filed: number;
    }
  // --- sessions ---
  | { type: "SessionForked"; command_id: CommandId; session_id: SessionId }
  | { type: "SessionList"; command_id: CommandId; sessions: SessionSummary[] }
  | {
      type: "FileSearchResults";
      command_id: CommandId;
      query: string;
      matches: FileMatchWire[];
      /** `#[serde(default)]` with no skip — always present. */
      truncated: boolean;
    }
  | {
      type: "SessionEventsPage";
      command_id: CommandId;
      session_id: SessionId;
      events: SessionEvent[];
      through: number;
      has_more: boolean;
    }
  | ({ type: "Event" } & SessionEvent)
  | { type: "Catchup"; catchup: Catchup }
  // --- workflows ---
  | { type: "WorkflowRunStarted"; command_id: CommandId; workflow_run_id: string }
  | { type: "WorkflowRunSnapshot"; command_id: CommandId; snapshot: WorkflowRunSnapshot }
  | { type: "WorkflowEvent"; event: WorkflowEvent }
  // --- promotion / UI plugins ---
  | { type: "PromotionProposed"; command_id: CommandId; candidate_id: string }
  | { type: "UiPluginLifecycle"; command_id: CommandId; plugins: UiPluginLifecycleStatus[] }
  // --- artifacts ---
  | { type: "ArtifactStored"; command_id: CommandId; artifact: ArtifactRef }
  | {
      type: "ArtifactChunk";
      artifact_id: ArtifactId;
      offset: number;
      bytes_base64: string;
      eof: boolean;
      sha256: string;
    }
  // --- curated memory ---
  | { type: "Memory"; command_id: CommandId; memory: MemoryView }
  | { type: "MemoryForgotten"; command_id: CommandId; forgotten: MemoryId[] }
  | { type: "MemoryEvidence"; command_id: CommandId; evidence: MemoryEvidence }
  // --- blackboard ---
  | { type: "BlackboardItems"; command_id: CommandId; items: BlackboardItemView[] }
  | ({ type: "BlackboardPosted" } & BlackboardItemView)
  | { type: "BlackboardItemApplied"; command_id: CommandId; item: BlackboardItemView }
  // --- code graph ---
  | { type: "CodeGraphBuilt"; command_id: CommandId; report: CodeGraphScanReport }
  | { type: "CodeGraphStatus"; command_id: CommandId; status: CodeGraphStatusView }
  | { type: "CodeGraphPage"; command_id: CommandId; page: CodeGraphPage }
  /**
   * The remote-UI channel (`Payload::RemoteUi { message: Box<UiWireMessage> }`).
   * `UiWireMessage` lives in `crates/protocol/src/remote_ui.rs`, has no golden
   * vector, and is not modeled here — the field is left opaque rather than
   * guessed at.
   */
  | { type: "RemoteUi"; message: unknown }
  /** Forward-compatibility fallback (`#[serde(other)]`). */
  | { type: "Unknown" };

/** Every `Payload` tag this build knows. Kept exhaustive by `tags.ts`. */
export type PayloadTag = Payload["type"];
