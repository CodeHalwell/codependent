//! `codypendent-runner` — Self-hosted remote runner agent and sandbox execution engine.
//!
//! Provides:
//! - Workload identity and capability discovery ([`identity`])
//! - Runner agent daemon with WebSocket/long-polling dispatch and heartbeating ([`agent`])
//! - Hostile input materialization and archive security validation ([`materialize`])
//! - Clean workspace isolation and teardown between jobs ([`workspace`])
//! - Secure process sandbox execution reusing `crates/sandbox` with fail-closed OS confinement ([`backend::process`])
//! - Hardened container execution with non-root, read-only root, and network isolation ([`backend::container`])
//! - Bounded live log streaming and output sanitization ([`log_streamer`])
//! - Signed execution attestation under the `codypendent-runner-attestation-v1` scheme ([`attestation`])
//! - Control plane and object storage clients ([`client`])

pub mod agent;
pub mod attestation;
pub mod backend;
pub mod client;
pub mod identity;
pub mod log_streamer;
pub mod materialize;
pub mod types;
pub mod workspace;

pub use agent::RunnerAgent;
pub use attestation::{
    compute_statement_digest, sign_attestation, verify_attestation, AttestationError,
    ATTESTATION_SCHEME_V1,
};
pub use backend::{
    container::{ContainerBackend, ContainerSecurityContext, ContainerSpec},
    process::ProcessSandboxBackend,
    ExecutionOutcome, RunnerBackend,
};
pub use client::{
    ControlPlaneClient, InMemoryControlPlane, InMemoryObjectStore, ObjectStoreClient,
};
pub use identity::{probe_available_tools, probe_sandbox_backend, RunnerIdentity};
pub use log_streamer::LogStreamer;
pub use materialize::{ExtractedFile, MaterializeLimits, MaterializeReport, Materializer};
pub use types::{
    Attestation, AttestationOutput, AttestationStatement, AttestationVerifyResult, ClaimRequest,
    InputManifest, InputManifestEntry, JobClaim, JobSpec, LogChunk, MaterializeError,
    OutputDeclaration, OutputUpload, ReleaseRequest, RenewRequest, RenewResponse, ResourceSpec,
    RunnerCapabilities, RunnerError, SandboxSpec, WorkspaceLayout,
};
pub use workspace::{WorkspaceGuard, WorkspaceManager};
