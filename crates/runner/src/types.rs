//! Protocol and specification types for self-hosted runner execution.
//!
//! Follows the specifications in `docs/superpowers/implementation/M8-self-hosted-runners.md`
//! §3 (Data model) and §4 (Runner protocol).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

/// Error conditions across the runner agent crate.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("sandbox error: {0}")]
    Sandbox(#[from] codypendent_sandbox::SandboxError),

    #[error("sandbox unavailable: {0}; refusing to run unconfined")]
    SandboxUnavailable(String),

    #[error("invalid sandbox command or spec: {0}")]
    InvalidCommand(String),

    #[error("unsupported capability: {0}")]
    UnsupportedCapability(String),

    #[error("materialize error: {0}")]
    Materialize(#[from] MaterializeError),

    #[error("lease expired at {0}")]
    LeaseExpired(DateTime<Utc>),

    #[error("stale lease generation: requested {requested}, current {current}")]
    StaleGeneration { requested: u64, current: u64 },

    #[error("unauthorized scope: {0}")]
    UnauthorizedScope(String),

    #[error("secret access denied for undeclared secret: {0}")]
    SecretAccessDenied(String),

    #[error("job execution cancelled: {0}")]
    Cancelled(String),

    #[error("attestation error: {0}")]
    Attestation(String),

    #[error("control plane error: {0}")]
    ControlPlane(String),

    #[error("object store error: {0}")]
    ObjectStore(String),

    #[error("workspace error: {0}")]
    Workspace(String),

    #[error("output artifact error: {0}")]
    Output(String),

    #[error("transient output transport error: {0}")]
    TransientOutput(String),

    #[error("resumable runner state error: {0}")]
    ResumableState(String),

    #[error("container error: {0}")]
    Container(String),
}

/// Errors during hostile archive unpacking and input materialization.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MaterializeError {
    #[error("absolute path rejected in input archive: {0}")]
    AbsolutePath(String),

    #[error("parent directory traversal (..) rejected in input archive: {0}")]
    ParentTraversal(String),

    #[error("symlink escape rejected: target `{target}` escapes workspace root")]
    SymlinkEscape { target: String },

    #[error("hardlink escape rejected: target `{target}` escapes workspace root")]
    HardlinkEscape { target: String },

    #[error("duplicate entry conflict in archive: {0}")]
    DuplicateEntry(String),

    #[error(
        "expansion ratio bomb detected: extracted {uncompressed} bytes from {compressed} bytes"
    )]
    ExpansionBomb { compressed: u64, uncompressed: u64 },

    #[error("size overflow: file `{path}` size {size} exceeds limit {limit}")]
    SizeOverflow { path: String, size: u64, limit: u64 },

    #[error("total workspace size overflow: extracted {total} bytes exceeds limit {limit}")]
    TotalSizeOverflow { total: u64, limit: u64 },

    #[error("undeclared entry in input archive: `{0}` not in input manifest")]
    UndeclaredEntry(String),

    #[error("checksum mismatch for `{path}`: expected `{expected}`, actual `{actual}`")]
    ChecksumMismatch {
        path: String,
        expected: String,
        actual: String,
    },

    #[error("archive format or decompression error: {0}")]
    ArchiveFormat(String),

    #[error("missing declared entry in archive: `{0}`")]
    MissingDeclaredEntry(String),
}

/// Layout of the workspace on disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceLayout {
    pub root: String,
    pub source_dir: String,
    pub output_dir: String,
    pub temp_dir: String,
}

impl Default for WorkspaceLayout {
    fn default() -> Self {
        Self {
            root: "/workspace".to_string(),
            source_dir: "/workspace/src".to_string(),
            output_dir: "/workspace/out".to_string(),
            temp_dir: "/workspace/tmp".to_string(),
        }
    }
}

/// Complete specification of a leased runner job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobSpec {
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub workspace_layout: WorkspaceLayout,
    pub input_manifest_hash: String,
    pub sandbox: SandboxSpec,
    pub resources: ResourceSpec,
    #[serde(default)]
    pub outputs: Vec<OutputDeclaration>,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
}

fn default_max_attempts() -> u32 {
    1
}

impl JobSpec {
    /// Compute the deterministic `sha256:<hex>` hash of the canonical JobSpec JSON.
    #[must_use]
    pub fn compute_hash(&self) -> String {
        #[derive(Serialize)]
        struct CanonicalJobSpec<'a> {
            argv: &'a [String],
            env: BTreeMap<&'a str, &'a str>,
            working_directory: &'a Option<String>,
            workspace_layout: &'a WorkspaceLayout,
            input_manifest_hash: &'a str,
            sandbox: &'a SandboxSpec,
            resources: &'a ResourceSpec,
            outputs: &'a [OutputDeclaration],
            max_attempts: u32,
        }

        let canonical = CanonicalJobSpec {
            argv: &self.argv,
            env: self
                .env
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect(),
            working_directory: &self.working_directory,
            workspace_layout: &self.workspace_layout,
            input_manifest_hash: &self.input_manifest_hash,
            sandbox: &self.sandbox,
            resources: &self.resources,
            outputs: &self.outputs,
            max_attempts: self.max_attempts,
        };
        let bytes = serde_json::to_vec(&canonical).expect("JobSpec serializes to JSON");
        let digest = Sha256::digest(&bytes);
        format!("sha256:{}", hex::encode(digest))
    }
}

/// Sandbox isolation specification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SandboxSpec {
    #[serde(default)]
    pub read_paths: Vec<String>,
    #[serde(default)]
    pub write_paths: Vec<String>,
    #[serde(default)]
    pub network_allowlist: Vec<String>,
    #[serde(default)]
    pub brokered_secrets: Vec<String>,
    #[serde(default)]
    pub env_allowlist: Vec<String>,
    #[serde(default)]
    pub allow_subprocess: bool,
}

/// Resource limits and constraints for a runner job.
///
/// NOTE: Zero is never "unlimited" anywhere in this codebase; zero means invalid.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceSpec {
    pub memory_mb: u64,
    pub cpu_seconds: u64,
    pub wall_seconds: u64,
    pub maximum_output_mb: u64,
    #[serde(default)]
    pub pids_limit: Option<u64>,
}

impl ResourceSpec {
    /// Validates that resource caps are strictly greater than zero.
    pub fn validate(&self) -> Result<(), RunnerError> {
        if self.memory_mb == 0 {
            return Err(RunnerError::InvalidCommand(
                "resource cap `memory_mb` must be greater than zero".to_string(),
            ));
        }
        if self.cpu_seconds == 0 {
            return Err(RunnerError::InvalidCommand(
                "resource cap `cpu_seconds` must be greater than zero".to_string(),
            ));
        }
        if self.wall_seconds == 0 {
            return Err(RunnerError::InvalidCommand(
                "resource cap `wall_seconds` must be greater than zero".to_string(),
            ));
        }
        if self.maximum_output_mb == 0 {
            return Err(RunnerError::InvalidCommand(
                "resource cap `maximum_output_mb` must be greater than zero".to_string(),
            ));
        }
        if let Some(pids) = self.pids_limit {
            if pids == 0 {
                return Err(RunnerError::InvalidCommand(
                    "resource cap `pids_limit` must be greater than zero".to_string(),
                ));
            }
        }
        Ok(())
    }
}

impl Default for ResourceSpec {
    fn default() -> Self {
        Self {
            memory_mb: 512,
            cpu_seconds: 60,
            wall_seconds: 120,
            maximum_output_mb: 20,
            pids_limit: Some(100),
        }
    }
}

/// Declaration of an expected job output artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutputDeclaration {
    pub name: String,
    pub path: String,
    pub media_type: String,
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

/// Content-addressed input manifest describing input files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct InputManifest {
    pub entries: Vec<InputManifestEntry>,
}

impl InputManifest {
    #[must_use]
    pub fn find_entry(&self, path: &str) -> Option<&InputManifestEntry> {
        self.entries.iter().find(|e| e.path == path)
    }

    #[must_use]
    pub fn compute_hash(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("InputManifest serializes to JSON");
        let digest = Sha256::digest(&bytes);
        format!("sha256:{}", hex::encode(digest))
    }
}

/// A single entry in the input manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InputManifestEntry {
    pub path: String,
    pub content_hash: String, // 'sha256:<hex>'
    pub byte_length: u64,
    #[serde(default = "default_mode")]
    pub mode: u32,
    #[serde(default)]
    pub executable: bool,
}

fn default_mode() -> u32 {
    0o644
}

/// Advertised runner capabilities.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RunnerCapabilities {
    pub tools: HashMap<String, String>,
    #[serde(default)]
    pub image_digest: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub policy_labels: Vec<String>,
}

/// A claimed job lease delivered to the runner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobClaim {
    pub job_id: Uuid,
    pub attempt_id: Uuid,
    pub attempt_number: u32,
    pub lease_id: Uuid,
    pub lease_generation: u64,
    pub expires_at: DateTime<Utc>,
    pub job_spec: JobSpec,
    pub job_spec_hash: String,
    pub input_manifest_hash: String,
    /// Strictest classification inherited by every output artifact.
    /// Missing legacy wire values deserialize to `unknown` and are refused.
    #[serde(default = "default_unknown_classification")]
    pub data_classification: String,
    pub lease_token: String,
    #[serde(default)]
    pub presigned_urls: HashMap<String, String>,
}

fn default_unknown_classification() -> String {
    "unknown".to_string()
}

/// Request sent by runner to claim a queued job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaimRequest {
    pub runner_id: Uuid,
    pub organization_id: Uuid,
    pub capabilities: RunnerCapabilities,
    pub max_concurrency: u32,
}

/// Request to renew a job lease.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenewRequest {
    pub lease_id: Uuid,
    pub generation: u64,
    pub lease_token: String,
}

/// Response returned from a lease renewal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenewResponse {
    pub renewed: bool,
    pub new_generation: u64,
    pub new_expires_at: DateTime<Utc>,
    pub cancel_requested: bool,
}

/// Request to release a lease (upon completion or abort).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseRequest {
    pub lease_id: Uuid,
    pub attempt_id: Uuid,
    pub reason: String,
    pub lease_token: String,
}

/// Live streamed log chunk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogChunk {
    pub attempt_id: Uuid,
    pub sequence: u64,
    pub stream: String, // 'stdout' | 'stderr'
    pub body: Option<Vec<u8>>,
    pub object_key: Option<String>,
    pub byte_length: usize,
    pub truncated: bool,
}

/// Registration of an uploaded output artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutputUpload {
    pub attempt_id: Uuid,
    pub name: String,
    pub content_hash: String, // 'sha256:<hex>'
    pub byte_length: u64,
    pub media_type: String,
    pub object_key: String,
    pub classification: String,
}

/// A signed execution attestation submitted by a runner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attestation {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub job_id: Uuid,
    pub lease_id: Uuid,
    pub runner_id: Uuid,
    pub scheme: String,
    pub statement: Vec<u8>,
    pub statement_digest: [u8; 32],
    pub signature: Vec<u8>,     // Ed25519 64 bytes
    pub signer_pubkey: Vec<u8>, // Ed25519 32 bytes
}

/// The canonical statement signed by the runner (§3.5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttestationStatement {
    pub job_id: Uuid,
    pub job_spec_hash: String,
    pub attempt_id: Uuid,
    pub attempt_number: u32,
    pub lease_id: Uuid,
    pub lease_generation: u64,
    pub runner_id: Uuid,
    pub image_digest: String,
    pub input_manifest_hash: String,
    pub outputs: Vec<AttestationOutput>,
    pub started_at: String, // RFC3339 UTC
    pub ended_at: String,   // RFC3339 UTC
    pub exit_code: i32,
    pub result: String, // 'succeeded' | 'failed' | 'cancelled' | 'quarantined'
}

/// An output artifact recorded in the attestation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct AttestationOutput {
    pub name: String,
    pub content_hash: String,
    pub byte_length: u64,
}

/// Result of attestation verification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttestationVerifyResult {
    pub verified: bool,
    pub verify_result: String, // 'verified' | 'bad-signature' | 'unknown-signer' | etc.
    pub quarantined: bool,
}
