//! Runner protocol: wire types, registration, heartbeats, job leases, execution events, and attestation.
//!
//! Defined in M8 §1, §3, §4 and specs §7.4, §11, §12.

use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::ids::{OrganizationId, RunnerAttemptId, RunnerId, RunnerJobId, RunnerLeaseId, UserId};

/// Deployment kind of the runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RunnerKind {
    Container,
    Kubernetes,
    Microvm,
    Macos,
    /// Unrecognized or newer runner kind. Never eligible for dispatch.
    #[serde(other)]
    Unknown,
}

impl fmt::Display for RunnerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Container => write!(f, "container"),
            Self::Kubernetes => write!(f, "kubernetes"),
            Self::Microvm => write!(f, "microvm"),
            Self::Macos => write!(f, "macos"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// The sandbox confinement backend reported by a runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum SandboxBackend {
    Seatbelt,
    Bubblewrap,
    None,
    /// Unrecognized or newer backend. Treated as providing no confinement.
    #[serde(other)]
    Unknown,
}

impl SandboxBackend {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Seatbelt => "seatbelt",
            Self::Bubblewrap => "bubblewrap",
            Self::None => "none",
            Self::Unknown => "unknown",
        }
    }

    /// Whether this backend provides real confinement (fails closed on 'none' and 'unknown').
    #[must_use]
    pub const fn is_enforceable(&self) -> bool {
        !matches!(self, Self::None | Self::Unknown)
    }
}

impl fmt::Display for SandboxBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Operational status of a registered runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RunnerStatus {
    Online,
    Idle,
    Busy,
    Draining,
    Offline,
    Revoked,
    /// Unrecognized or newer status. Never treated as dispatchable.
    #[serde(other)]
    Unknown,
}

impl RunnerStatus {
    /// Whether the control plane may hand this runner new work.
    #[must_use]
    pub const fn accepts_work(self) -> bool {
        matches!(self, Self::Online | Self::Idle)
    }
}

/// Advertised capabilities of a runner.
///
/// Note (M8 §4.2): Advertised capabilities are scheduling hints used to filter eligibility;
/// security-relevant facts are verified cryptographically via attestation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RunnerCapabilities {
    /// Available tool names and versions.
    #[serde(default)]
    pub tools: BTreeMap<String, String>,
    /// Container image digest if fixed ('sha256:<hex>').
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    /// Policy and environment labels.
    #[serde(default)]
    pub policy_labels: Vec<String>,
    /// Maximum concurrent jobs this runner advertises.
    #[serde(default = "default_concurrency")]
    pub max_concurrency: u32,
    /// Arbitrary additional metadata.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

const fn default_concurrency() -> u32 {
    1
}

/// Runner registration message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RunnerRegistration {
    pub runner_id: RunnerId,
    pub organization_id: OrganizationId,
    pub name: String,
    pub kind: RunnerKind,
    pub os: String,
    pub arch: String,
    pub sandbox_backend: SandboxBackend,
    pub capabilities: RunnerCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Hex-encoded 32-byte Ed25519 public key.
    pub attestation_pubkey: String,
    #[serde(default = "default_concurrency")]
    pub max_concurrency: u32,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_runner_status")]
    pub status: RunnerStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<DateTime<Utc>>,
}

fn default_runner_status() -> RunnerStatus {
    RunnerStatus::Online
}

/// Runner metrics included in heartbeats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RunnerMetrics {
    pub active_leases: u32,
    pub cpu_usage_pct: Option<u32>,
    pub memory_used_mb: Option<u64>,
}

/// Heartbeat message sent periodically by a runner holding a lease.
///
/// Heartbeat IS the lease renewal mechanism (M8 §4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RunnerHeartbeat {
    pub lease_id: RunnerLeaseId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<RunnerAttemptId>,
    pub runner_id: RunnerId,
    /// Monotonic lease generation (must match current lease generation).
    pub generation: u64,
    /// Plaintext lease token presented to authenticate the heartbeat.
    pub lease_token: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<RunnerMetrics>,
}

/// Control-plane response to a runner heartbeat / renewal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct HeartbeatResponse {
    pub lease_id: RunnerLeaseId,
    pub new_generation: u64,
    pub expires_at: DateTime<Utc>,
    pub cancel_requested: bool,
}

/// Request by a runner to claim queued work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct JobClaimRequest {
    pub runner_id: RunnerId,
    pub organization_id: OrganizationId,
    pub os: String,
    pub arch: String,
    pub sandbox_backend: SandboxBackend,
    pub capabilities: RunnerCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_jobs: Option<u32>,
}

/// Response to a job claim request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct JobClaimResponse {
    pub lease: Option<JobLease>,
}

/// Active lease granted to a runner for an attempt on a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct JobLease {
    pub lease_id: RunnerLeaseId,
    pub job_id: RunnerJobId,
    pub attempt_id: RunnerAttemptId,
    pub attempt_number: u32,
    pub runner_id: RunnerId,
    pub generation: u64,
    /// Opaque, high-entropy lease secret token.
    pub lease_token: String,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub job_spec: JobSpec,
    pub job_spec_hash: String,
    pub input_manifest_hash: String,
    pub data_classification: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_micro_usd: Option<u64>,
}

/// Specification of a job to be executed by a runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct JobSpec {
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_layout: Option<String>,
    pub input_manifest_ref: String,
    pub sandbox: SandboxSpec,
    pub resource: ResourceSpec,
    #[serde(default)]
    pub outputs: Vec<OutputDeclaration>,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
}

const fn default_max_attempts() -> u32 {
    1
}

/// Sandbox confinement specification on the wire.
///
/// A serializable projection of `SandboxProfile` (M8 §2.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SandboxSpec {
    #[serde(default)]
    pub write_paths: Vec<String>,
    #[serde(default)]
    pub read_paths: Vec<String>,
    #[serde(default)]
    pub env_allowlist: Vec<String>,
    #[serde(default)]
    pub brokered_secrets: Vec<String>,
    #[serde(default)]
    pub allow_subprocess: bool,
    pub memory_mb: u64,
    pub cpu_seconds: u64,
    pub wall_seconds: u64,
    pub maximum_output_mb: u64,
    #[serde(default)]
    pub network_allowlist: Vec<String>,
}

/// Resource specification for container / VM limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ResourceSpec {
    pub cpu_cores: u32,
    pub memory_mb: u64,
    pub disk_mb: u64,
    pub wall_time_secs: u64,
}

/// Declared output requirement in a JobSpec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct OutputDeclaration {
    pub name: String,
    pub media_type: String,
    #[serde(default)]
    pub optional: bool,
}

/// Registered output uploaded by a runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct OutputRegistration {
    pub attempt_id: RunnerAttemptId,
    pub name: String,
    pub content_hash: String,
    pub byte_length: u64,
    pub media_type: String,
    pub object_key: String,
    pub classification: String,
}

/// Log stream type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum LogStream {
    Stdout,
    Stderr,
    /// Unrecognized or newer stream name.
    #[serde(other)]
    Unknown,
}

/// Live log chunk streamed from runner to control plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct LogChunk {
    pub attempt_id: RunnerAttemptId,
    pub sequence: u64,
    pub stream: LogStream,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_key: Option<String>,
    pub byte_length: usize,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_at: Option<DateTime<Utc>>,
}

/// Lifecycle state of a runner job attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RunnerAttemptState {
    Claimed,
    Executing,
    Uploading,
    Verified,
    Rejected,
    Expired,
    Cancelled,
    /// Unrecognized or newer attempt state. Never treated as verified.
    #[serde(other)]
    Unknown,
}

impl RunnerAttemptState {
    /// Whether outputs from this attempt may be published. Only `Verified` qualifies.
    #[must_use]
    pub const fn is_publishable(self) -> bool {
        matches!(self, Self::Verified)
    }
}

/// Terminal state of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum JobTerminalState {
    Succeeded,
    Failed,
    Cancelled,
    Quarantined,
    /// Unrecognized or newer terminal state. Never treated as success.
    #[serde(other)]
    Unknown,
}

impl JobTerminalState {
    /// Whether the job completed successfully. Only `Succeeded` qualifies.
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Succeeded)
    }
}

/// Execution event streamed by a runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct JobExecutionEvent {
    pub lease_id: RunnerLeaseId,
    pub attempt_id: RunnerAttemptId,
    pub sequence: u64,
    pub timestamp: DateTime<Utc>,
    pub kind: JobExecutionEventKind,
}

/// Kind of execution event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "data", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum JobExecutionEventKind {
    Log(LogChunk),
    StatusUpdate {
        state: RunnerAttemptState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    OutputDeclared(OutputRegistration),
    Finished {
        exit_code: Option<i32>,
        result: JobTerminalState,
    },
    /// An execution-event kind emitted by a newer runner. Without this arm the
    /// whole frame failed to deserialize, which takes the surrounding
    /// [`JobExecutionEvent`] — and therefore the attempt's event stream — down
    /// with it. `Unknown` carries no state and no terminal result, so it can
    /// never be read as a status transition or as a finished-successfully
    /// event; a consumer must ignore it and infer no effect from it.
    #[serde(other)]
    Unknown,
}

/// Cancellation request for a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct JobCancellation {
    pub job_id: RunnerJobId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<RunnerLeaseId>,
    pub reason: String,
    pub requested_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<UserId>,
    #[serde(default)]
    pub force: bool,
}

/// Response to a job cancellation request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct JobCancellationResponse {
    pub job_id: RunnerJobId,
    pub cancelled: bool,
    pub current_state: String,
}

/// Single output in the canonical attestation statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RunnerAttestationOutput {
    pub name: String,
    pub content_hash: String,
    pub byte_length: u64,
}

/// Canonical attestation statement structure (M8 §3.5).
///
/// Must not contain optional or map-typed fields for deterministic canonical serialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RunnerAttestationStatement {
    pub job_id: RunnerJobId,
    pub job_spec_hash: String,
    pub attempt_id: RunnerAttemptId,
    pub attempt_number: u32,
    pub lease_id: RunnerLeaseId,
    pub lease_generation: u64,
    pub runner_id: RunnerId,
    pub image_digest: String,
    pub input_manifest_hash: String,
    pub outputs: Vec<RunnerAttestationOutput>,
    pub started_at: String,
    pub ended_at: String,
    pub exit_code: Option<i32>,
    pub result: String,
}

/// Domain separation tag for runner attestation signatures (v1).
pub const ATTESTATION_SCHEME_V1: &str = "codypendent-runner-attestation-v1";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AttestationVerificationError {
    #[error("unsupported or foreign attestation scheme: expected {expected}, got {actual}")]
    ForeignSchemeTag { expected: String, actual: String },
    #[error("invalid signer public key: {0}")]
    InvalidPublicKey(String),
    #[error("invalid signature format: {0}")]
    InvalidSignature(String),
    #[error("signature verification failed")]
    SignatureMismatch,
    #[error("serialization error: {0}")]
    SerializationError(String),
}

impl RunnerAttestationStatement {
    /// Return deterministic canonical JSON bytes of the statement.
    ///
    /// Outputs are guaranteed to be sorted by `name`.
    #[must_use]
    pub fn canonical_statement_bytes(&self) -> Vec<u8> {
        let mut sorted = self.clone();
        sorted.outputs.sort_by(|a, b| a.name.cmp(&b.name));
        serde_json::to_vec(&sorted).expect("attestation statement serializes deterministically")
    }

    /// Compute the 32-byte signing digest using the canonical v1 scheme.
    ///
    /// `digest = SHA256( b"codypendent-runner-attestation-v1" || be_u64(len(canonical)) || canonical )`
    #[must_use]
    pub fn compute_digest(&self) -> [u8; 32] {
        self.compute_digest_with_scheme(ATTESTATION_SCHEME_V1)
    }

    /// Compute the 32-byte signing digest using an explicit scheme tag.
    #[must_use]
    pub fn compute_digest_with_scheme(&self, scheme: &str) -> [u8; 32] {
        let canonical = self.canonical_statement_bytes();
        let mut hasher = Sha256::new();
        hasher.update(scheme.as_bytes());
        hasher.update((canonical.len() as u64).to_be_bytes());
        hasher.update(&canonical);
        hasher.finalize().into()
    }

    /// Sign the statement using an Ed25519 signing key.
    #[must_use]
    pub fn sign(&self, signing_key: &SigningKey) -> Signature {
        let digest = self.compute_digest();
        signing_key.sign(&digest)
    }

    /// Verify this statement's signature against a 32-byte Ed25519 public key.
    pub fn verify_signature(
        &self,
        pubkey_bytes: &[u8; 32],
        signature_bytes: &[u8; 64],
    ) -> Result<(), AttestationVerificationError> {
        self.verify_with_scheme(ATTESTATION_SCHEME_V1, pubkey_bytes, signature_bytes)
    }

    /// Verify signature under a specific scheme tag, failing closed if it does not match v1.
    pub fn verify_with_scheme(
        &self,
        scheme: &str,
        pubkey_bytes: &[u8; 32],
        signature_bytes: &[u8; 64],
    ) -> Result<(), AttestationVerificationError> {
        if scheme != ATTESTATION_SCHEME_V1 {
            return Err(AttestationVerificationError::ForeignSchemeTag {
                expected: ATTESTATION_SCHEME_V1.to_string(),
                actual: scheme.to_string(),
            });
        }
        let verifying_key = VerifyingKey::from_bytes(pubkey_bytes)
            .map_err(|e| AttestationVerificationError::InvalidPublicKey(e.to_string()))?;
        let signature = Signature::from_bytes(signature_bytes);
        let digest = self.compute_digest_with_scheme(scheme);
        verifying_key
            .verify_strict(&digest, &signature)
            .map_err(|_| AttestationVerificationError::SignatureMismatch)
    }
}

/// Attestation submission from runner to control plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RunnerAttestationSubmission {
    pub attempt_id: RunnerAttemptId,
    pub job_id: RunnerJobId,
    pub lease_id: RunnerLeaseId,
    pub runner_id: RunnerId,
    pub scheme: String,
    pub statement: RunnerAttestationStatement,
    /// Hex-encoded 64-byte Ed25519 signature.
    pub signature: String,
    /// Hex-encoded 32-byte Ed25519 public key.
    pub signer_pubkey: String,
}

/// Reason code for placing outputs / attempt into quarantine (M8 §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RunnerQuarantineReason {
    AttestationInvalid,
    HashMismatch,
    UndeclaredOutput,
    RevokedImage,
    RevokedKey,
    LeaseMismatch,
    Oversized,
    /// Unrecognized or newer quarantine reason. Quarantine still applies.
    #[serde(other)]
    Unknown,
}

impl fmt::Display for RunnerQuarantineReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AttestationInvalid => write!(f, "attestation-invalid"),
            Self::HashMismatch => write!(f, "hash-mismatch"),
            Self::UndeclaredOutput => write!(f, "undeclared-output"),
            Self::RevokedImage => write!(f, "revoked-image"),
            Self::RevokedKey => write!(f, "revoked-key"),
            Self::LeaseMismatch => write!(f, "lease-mismatch"),
            Self::Oversized => write!(f, "oversized"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}
