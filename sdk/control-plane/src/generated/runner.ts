/**
 * Generated from the authoritative Rust protocol schema.
 * Do not edit by hand; run `npm run generate`.
 */

/**
 * Lifecycle state of a runner job attempt.
 */
export type RunnerAttemptState =
  ("claimed" | "executing" | "uploading" | "verified" | "rejected" | "expired" | "cancelled") | "unknown";
/**
 * The sandbox confinement backend reported by a runner.
 */
export type SandboxBackend = ("seatbelt" | "bubblewrap" | "none") | "unknown";
/**
 * Kind of execution event.
 */
export type JobExecutionEventKind =
  | {
      data: LogChunk;
      type: "log";
    }
  | {
      data: {
        detail?: string | null;
        state: RunnerAttemptState;
      };
      type: "status-update";
    }
  | {
      data: OutputRegistration;
      type: "output-declared";
    }
  | {
      data: {
        exit_code?: number | null;
        result: JobTerminalState;
      };
      type: "finished";
    };
/**
 * Log stream type.
 */
export type LogStream = ("stdout" | "stderr") | "unknown";
/**
 * Terminal state of a job.
 */
export type JobTerminalState = ("succeeded" | "failed" | "cancelled" | "quarantined") | "unknown";
/**
 * Reason code for placing outputs / attempt into quarantine (M8 §3.4).
 */
export type RunnerQuarantineReason =
  | (
      | "attestation-invalid"
      | "hash-mismatch"
      | "undeclared-output"
      | "revoked-image"
      | "revoked-key"
      | "lease-mismatch"
      | "oversized"
    )
  | "unknown";
/**
 * Deployment kind of the runner.
 */
export type RunnerKind = ("container" | "kubernetes" | "microvm" | "macos") | "unknown";
/**
 * Operational status of a registered runner.
 */
export type RunnerStatus = ("online" | "idle" | "busy" | "draining" | "offline" | "revoked") | "unknown";

export interface RunnerCatalog {
  attempt_state: RunnerAttemptState;
  attestation_output: RunnerAttestationOutput;
  attestation_statement: RunnerAttestationStatement;
  attestation_submission: RunnerAttestationSubmission;
  cancellation_request: JobCancellation;
  cancellation_response: JobCancellationResponse;
  capabilities: RunnerCapabilities;
  claim_request: JobClaimRequest;
  claim_response: JobClaimResponse;
  execution_event: JobExecutionEvent;
  heartbeat_request: RunnerHeartbeat;
  heartbeat_response: HeartbeatResponse;
  job_spec: JobSpec;
  lease: JobLease;
  log_chunk: LogChunk;
  log_stream: LogStream;
  metrics: RunnerMetrics;
  output_declaration: OutputDeclaration;
  output_registration: OutputRegistration;
  quarantine_reason: RunnerQuarantineReason;
  registration: RunnerRegistration;
  resource_spec: ResourceSpec;
  runner_kind: RunnerKind;
  runner_status: RunnerStatus;
  sandbox_backend: SandboxBackend;
  sandbox_spec: SandboxSpec;
  terminal_state: JobTerminalState;
}
/**
 * Single output in the canonical attestation statement.
 */
export interface RunnerAttestationOutput {
  byte_length: number;
  content_hash: string;
  name: string;
}
/**
 * Canonical attestation statement structure (M8 §3.5).
 *
 * Must not contain optional or map-typed fields for deterministic canonical serialization.
 */
export interface RunnerAttestationStatement {
  attempt_id: string;
  attempt_number: number;
  ended_at: string;
  exit_code?: number | null;
  image_digest: string;
  input_manifest_hash: string;
  job_id: string;
  job_spec_hash: string;
  lease_generation: number;
  lease_id: string;
  outputs: RunnerAttestationOutput[];
  result: string;
  runner_id: string;
  started_at: string;
}
/**
 * Attestation submission from runner to control plane.
 */
export interface RunnerAttestationSubmission {
  attempt_id: string;
  job_id: string;
  lease_id: string;
  runner_id: string;
  scheme: string;
  /**
   * Hex-encoded 64-byte Ed25519 signature.
   */
  signature: string;
  /**
   * Hex-encoded 32-byte Ed25519 public key.
   */
  signer_pubkey: string;
  statement: RunnerAttestationStatement;
}
/**
 * Cancellation request for a job.
 */
export interface JobCancellation {
  force?: boolean;
  job_id: string;
  lease_id?: string | null;
  reason: string;
  requested_at: string;
  requested_by?: string | null;
}
/**
 * Response to a job cancellation request.
 */
export interface JobCancellationResponse {
  cancelled: boolean;
  current_state: string;
  job_id: string;
}
/**
 * Advertised capabilities of a runner.
 *
 * Note (M8 §4.2): Advertised capabilities are scheduling hints used to filter eligibility; security-relevant facts are verified cryptographically via attestation.
 */
export interface RunnerCapabilities {
  /**
   * Container image digest if fixed ('sha256:<hex>').
   */
  image_digest?: string | null;
  /**
   * Maximum concurrent jobs this runner advertises.
   */
  max_concurrency?: number;
  /**
   * Arbitrary additional metadata.
   */
  metadata?: {
    [k: string]: string | undefined;
  };
  /**
   * Policy and environment labels.
   */
  policy_labels?: string[];
  /**
   * Available tool names and versions.
   */
  tools?: {
    [k: string]: string | undefined;
  };
}
/**
 * Request by a runner to claim queued work.
 */
export interface JobClaimRequest {
  arch: string;
  capabilities: RunnerCapabilities;
  max_jobs?: number | null;
  organization_id: string;
  os: string;
  region?: string | null;
  runner_id: string;
  sandbox_backend: SandboxBackend;
}
/**
 * Response to a job claim request.
 */
export interface JobClaimResponse {
  lease?: JobLease | null;
}
/**
 * Active lease granted to a runner for an attempt on a job.
 */
export interface JobLease {
  acquired_at: string;
  attempt_id: string;
  attempt_number: number;
  budget_micro_usd?: number | null;
  data_classification: string;
  expires_at: string;
  generation: number;
  input_manifest_hash: string;
  job_id: string;
  job_spec: JobSpec;
  job_spec_hash: string;
  lease_id: string;
  /**
   * Opaque, high-entropy lease secret token.
   */
  lease_token: string;
  runner_id: string;
}
/**
 * Specification of a job to be executed by a runner.
 */
export interface JobSpec {
  argv: string[];
  env?: {
    [k: string]: string | undefined;
  };
  input_manifest_ref: string;
  max_attempts?: number;
  outputs?: OutputDeclaration[];
  resource: ResourceSpec;
  sandbox: SandboxSpec;
  working_dir?: string | null;
  workspace_layout?: string | null;
}
/**
 * Declared output requirement in a JobSpec.
 */
export interface OutputDeclaration {
  media_type: string;
  name: string;
  optional?: boolean;
}
/**
 * Resource specification for container / VM limits.
 */
export interface ResourceSpec {
  cpu_cores: number;
  disk_mb: number;
  memory_mb: number;
  wall_time_secs: number;
}
/**
 * Sandbox confinement specification on the wire.
 *
 * A serializable projection of `SandboxProfile` (M8 §2.1).
 */
export interface SandboxSpec {
  allow_subprocess?: boolean;
  brokered_secrets?: string[];
  cpu_seconds: number;
  env_allowlist?: string[];
  maximum_output_mb: number;
  memory_mb: number;
  network_allowlist?: string[];
  read_paths?: string[];
  wall_seconds: number;
  write_paths?: string[];
}
/**
 * Execution event streamed by a runner.
 */
export interface JobExecutionEvent {
  attempt_id: string;
  kind: JobExecutionEventKind;
  lease_id: string;
  sequence: number;
  timestamp: string;
}
/**
 * Live log chunk streamed from runner to control plane.
 */
export interface LogChunk {
  attempt_id: string;
  body?: number[] | null;
  byte_length: number;
  object_key?: string | null;
  received_at?: string | null;
  sequence: number;
  stream: LogStream;
  truncated?: boolean;
}
/**
 * Registered output uploaded by a runner.
 */
export interface OutputRegistration {
  attempt_id: string;
  byte_length: number;
  classification: string;
  content_hash: string;
  media_type: string;
  name: string;
  object_key: string;
}
/**
 * Heartbeat message sent periodically by a runner holding a lease.
 *
 * Heartbeat IS the lease renewal mechanism (M8 §4.4).
 */
export interface RunnerHeartbeat {
  attempt_id?: string | null;
  /**
   * Monotonic lease generation (must match current lease generation).
   */
  generation: number;
  lease_id: string;
  /**
   * Plaintext lease token presented to authenticate the heartbeat.
   */
  lease_token: string;
  metrics?: RunnerMetrics | null;
  runner_id: string;
  timestamp: string;
}
/**
 * Runner metrics included in heartbeats.
 */
export interface RunnerMetrics {
  active_leases: number;
  cpu_usage_pct?: number | null;
  memory_used_mb?: number | null;
}
/**
 * Control-plane response to a runner heartbeat / renewal.
 */
export interface HeartbeatResponse {
  cancel_requested: boolean;
  expires_at: string;
  lease_id: string;
  new_generation: number;
}
/**
 * Runner registration message.
 */
export interface RunnerRegistration {
  arch: string;
  /**
   * Hex-encoded 32-byte Ed25519 public key.
   */
  attestation_pubkey: string;
  capabilities: RunnerCapabilities;
  kind: RunnerKind;
  last_seen_at?: string | null;
  max_concurrency?: number;
  name: string;
  organization_id: string;
  os: string;
  region?: string | null;
  registered_at?: string | null;
  runner_id: string;
  sandbox_backend: SandboxBackend;
  status?: RunnerStatus & string;
  tags?: string[];
}
