//! Control plane and object storage clients for runner communication.

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::identity::RunnerIdentity;
use crate::types::{
    Attestation, AttestationVerifyResult, ClaimRequest, JobClaim, JobSpec, LogChunk, OutputUpload,
    ReleaseRequest, RenewRequest, RenewResponse, RunnerError,
};

/// Client interface for communicating with the Codypendent control plane.
#[async_trait]
pub trait ControlPlaneClient: Send + Sync {
    /// Register runner capabilities and identity with the control plane.
    async fn register(&self, identity: &RunnerIdentity) -> Result<(), RunnerError>;

    /// Claim the next eligible queued job.
    async fn claim(&self, req: &ClaimRequest) -> Result<Option<JobClaim>, RunnerError>;

    /// Renew an active job lease with matching generation.
    async fn renew_lease(&self, req: &RenewRequest) -> Result<RenewResponse, RunnerError>;

    /// Stream a chunk of log output.
    async fn stream_logs(&self, chunk: LogChunk) -> Result<(), RunnerError>;

    /// Register/upload an output artifact.
    async fn upload_output(&self, req: &OutputUpload) -> Result<(), RunnerError>;

    /// Submit signed execution attestation.
    async fn submit_attestation(
        &self,
        attestation: &Attestation,
    ) -> Result<AttestationVerifyResult, RunnerError>;

    /// Release a lease upon job completion or termination.
    async fn release_lease(&self, req: &ReleaseRequest) -> Result<(), RunnerError>;

    /// Request a brokered secret for an active lease.
    async fn request_secret(
        &self,
        lease_token: &str,
        secret_name: &str,
    ) -> Result<String, RunnerError>;
}

/// Client interface for content-addressed object storage.
#[async_trait]
pub trait ObjectStoreClient: Send + Sync {
    /// Fetch input bundle archive by hash (`sha256:<hex>`).
    async fn fetch_input_bundle(&self, hash: &str) -> Result<Vec<u8>, RunnerError>;

    /// Upload artifact bytes to object storage.
    async fn upload_artifact(&self, key: &str, data: &[u8]) -> Result<(), RunnerError>;
}

/// In-memory control plane implementation for testing and local integration.
#[derive(Clone, Default)]
pub struct InMemoryControlPlane {
    inner: Arc<RwLock<InMemoryControlPlaneState>>,
}

#[derive(Default)]
struct InMemoryControlPlaneState {
    runners: HashMap<Uuid, RunnerIdentity>,
    queued_jobs: Vec<QueuedJobEntry>,
    active_leases: HashMap<Uuid, StoredLease>, // lease_id -> lease
    token_to_lease: HashMap<String, Uuid>,
    logs: Vec<LogChunk>,
    outputs: HashMap<(Uuid, String), OutputUpload>, // (attempt_id, name) -> output
    attestations: HashMap<Uuid, Attestation>,       // attempt_id -> attestation
    brokered_secrets: HashMap<String, String>,      // secret_name -> value
    cancelled_jobs: HashMap<Uuid, bool>,
}

struct QueuedJobEntry {
    job_id: Uuid,
    organization_id: Uuid,
    repository_id: Uuid,
    job_spec: JobSpec,
    job_spec_hash: String,
    input_manifest_hash: String,
    data_classification: String,
}

/// The in-memory control-plane double stores the whole lease row so the shape
/// matches the real server's; several columns are only ever written here.
#[allow(dead_code)]
struct StoredLease {
    lease_id: Uuid,
    job_id: Uuid,
    attempt_id: Uuid,
    attempt_number: u32,
    runner_id: Uuid,
    repository_id: Uuid,
    generation: u64,
    lease_token: String,
    expires_at: DateTime<Utc>,
    active: bool,
    declared_secrets: Vec<String>,
}

impl InMemoryControlPlane {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a secret to the control plane secret broker.
    pub async fn add_secret(&self, name: impl Into<String>, value: impl Into<String>) {
        let mut state = self.inner.write().await;
        state.brokered_secrets.insert(name.into(), value.into());
    }

    /// Queue a job to be claimed by runners.
    pub async fn queue_job(
        &self,
        job_id: Uuid,
        organization_id: Uuid,
        repository_id: Uuid,
        job_spec: JobSpec,
    ) {
        self.queue_job_with_classification(
            job_id,
            organization_id,
            repository_id,
            job_spec,
            "internal",
        )
        .await;
    }

    /// Queue a job with the classification every produced artifact must inherit.
    pub async fn queue_job_with_classification(
        &self,
        job_id: Uuid,
        organization_id: Uuid,
        repository_id: Uuid,
        job_spec: JobSpec,
        data_classification: impl Into<String>,
    ) {
        let job_spec_hash = job_spec.compute_hash();
        let input_manifest_hash = job_spec.input_manifest_hash.clone();
        let data_classification = data_classification.into();
        let mut state = self.inner.write().await;
        state.queued_jobs.push(QueuedJobEntry {
            job_id,
            organization_id,
            repository_id,
            job_spec,
            job_spec_hash,
            input_manifest_hash,
            data_classification,
        });
    }

    /// Mark a job as cancelled.
    pub async fn cancel_job(&self, job_id: Uuid) {
        let mut state = self.inner.write().await;
        state.cancelled_jobs.insert(job_id, true);
    }

    /// Read all streamed logs.
    pub async fn get_logs(&self) -> Vec<LogChunk> {
        let state = self.inner.read().await;
        state.logs.clone()
    }

    /// Read submitted outputs.
    pub async fn get_outputs(&self) -> Vec<OutputUpload> {
        let state = self.inner.read().await;
        state.outputs.values().cloned().collect()
    }

    /// Read submitted attestations.
    pub async fn get_attestations(&self) -> Vec<Attestation> {
        let state = self.inner.read().await;
        state.attestations.values().cloned().collect()
    }
}

#[async_trait]
impl ControlPlaneClient for InMemoryControlPlane {
    async fn register(&self, identity: &RunnerIdentity) -> Result<(), RunnerError> {
        let mut state = self.inner.write().await;
        state.runners.insert(identity.id, identity.clone());
        Ok(())
    }

    async fn claim(&self, req: &ClaimRequest) -> Result<Option<JobClaim>, RunnerError> {
        let mut state = self.inner.write().await;

        let index = state
            .queued_jobs
            .iter()
            .position(|j| j.organization_id == req.organization_id);

        if let Some(idx) = index {
            let entry = state.queued_jobs.remove(idx);

            // Check if cancelled before claim (§4.3: cancel consumed at claim)
            if state
                .cancelled_jobs
                .get(&entry.job_id)
                .copied()
                .unwrap_or(false)
            {
                return Ok(None);
            }

            let attempt_id = Uuid::now_v7();
            let lease_id = Uuid::now_v7();
            let lease_token = format!("lease_token_{}", Uuid::now_v7());
            let expires_at = Utc::now() + ChronoDuration::seconds(60);

            let stored_lease = StoredLease {
                lease_id,
                job_id: entry.job_id,
                attempt_id,
                attempt_number: 1,
                runner_id: req.runner_id,
                repository_id: entry.repository_id,
                generation: 1,
                lease_token: lease_token.clone(),
                expires_at,
                active: true,
                declared_secrets: entry.job_spec.sandbox.brokered_secrets.clone(),
            };

            state.active_leases.insert(lease_id, stored_lease);
            state.token_to_lease.insert(lease_token.clone(), lease_id);

            let claim = JobClaim {
                job_id: entry.job_id,
                attempt_id,
                attempt_number: 1,
                lease_id,
                lease_generation: 1,
                expires_at,
                job_spec: entry.job_spec,
                job_spec_hash: entry.job_spec_hash,
                input_manifest_hash: entry.input_manifest_hash,
                data_classification: entry.data_classification,
                lease_token,
                presigned_urls: HashMap::new(),
            };

            Ok(Some(claim))
        } else {
            Ok(None)
        }
    }

    async fn renew_lease(&self, req: &RenewRequest) -> Result<RenewResponse, RunnerError> {
        let mut state = self.inner.write().await;
        // Reborrow the guard once so `active_leases` and `cancelled_jobs` are seen as
        // disjoint fields; going through `DerefMut` twice would overlap the borrows.
        let state = &mut *state;

        let lease = state.active_leases.get_mut(&req.lease_id).ok_or_else(|| {
            RunnerError::UnauthorizedScope("lease not found or expired".to_string())
        })?;

        if !lease.active {
            return Err(RunnerError::UnauthorizedScope(
                "lease is released".to_string(),
            ));
        }

        if lease.lease_token != req.lease_token {
            return Err(RunnerError::UnauthorizedScope(
                "invalid lease token".to_string(),
            ));
        }

        // Generation check: stale generation is refused!
        if req.generation != lease.generation {
            return Err(RunnerError::StaleGeneration {
                requested: req.generation,
                current: lease.generation,
            });
        }

        let is_cancelled = state
            .cancelled_jobs
            .get(&lease.job_id)
            .copied()
            .unwrap_or(false);

        lease.generation += 1;
        lease.expires_at = Utc::now() + ChronoDuration::seconds(60);

        Ok(RenewResponse {
            renewed: true,
            new_generation: lease.generation,
            new_expires_at: lease.expires_at,
            cancel_requested: is_cancelled,
        })
    }

    async fn stream_logs(&self, chunk: LogChunk) -> Result<(), RunnerError> {
        let mut state = self.inner.write().await;
        state.logs.push(chunk);
        Ok(())
    }

    async fn upload_output(&self, req: &OutputUpload) -> Result<(), RunnerError> {
        let mut state = self.inner.write().await;
        // Resumable upload: the same registration is idempotent, but a changed
        // hash or metadata for the same attempt/name is an integrity conflict.
        if let Some(existing) = state.outputs.get(&(req.attempt_id, req.name.clone())) {
            if existing == req {
                return Ok(());
            }
            return Err(RunnerError::Output(format!(
                "conflicting upload registration for output {:?}",
                req.name
            )));
        }
        state
            .outputs
            .insert((req.attempt_id, req.name.clone()), req.clone());
        Ok(())
    }

    async fn submit_attestation(
        &self,
        attestation: &Attestation,
    ) -> Result<AttestationVerifyResult, RunnerError> {
        let mut state = self.inner.write().await;

        let lease = match state.active_leases.get(&attestation.lease_id) {
            Some(l) => l,
            None => {
                return Ok(AttestationVerifyResult {
                    verified: false,
                    verify_result: "lease-mismatch".to_string(),
                    quarantined: true,
                });
            }
        };

        if lease.attempt_id != attestation.attempt_id || lease.job_id != attestation.job_id {
            return Ok(AttestationVerifyResult {
                verified: false,
                verify_result: "lease-mismatch".to_string(),
                quarantined: true,
            });
        }

        let runner = match state.runners.get(&attestation.runner_id) {
            Some(r) => r,
            None => {
                return Ok(AttestationVerifyResult {
                    verified: false,
                    verify_result: "unknown-signer".to_string(),
                    quarantined: true,
                });
            }
        };

        match crate::attestation::verify_attestation(attestation, &runner.pubkey_bytes()) {
            Ok(_) => {
                state
                    .attestations
                    .insert(attestation.attempt_id, attestation.clone());
                Ok(AttestationVerifyResult {
                    verified: true,
                    verify_result: "verified".to_string(),
                    quarantined: false,
                })
            }
            Err(e) => Ok(AttestationVerifyResult {
                verified: false,
                verify_result: e.to_string(),
                quarantined: true,
            }),
        }
    }

    async fn release_lease(&self, req: &ReleaseRequest) -> Result<(), RunnerError> {
        let mut state = self.inner.write().await;
        if let Some(lease) = state.active_leases.get_mut(&req.lease_id) {
            if lease.lease_token == req.lease_token {
                lease.active = false;
            }
        }
        Ok(())
    }

    async fn request_secret(
        &self,
        lease_token: &str,
        secret_name: &str,
    ) -> Result<String, RunnerError> {
        let state = self.inner.read().await;

        let lease_id = state
            .token_to_lease
            .get(lease_token)
            .ok_or_else(|| RunnerError::UnauthorizedScope("invalid lease token".to_string()))?;

        let lease = state
            .active_leases
            .get(lease_id)
            .ok_or_else(|| RunnerError::UnauthorizedScope("lease not found".to_string()))?;

        // Compromised runner defense: runner can only request secrets declared in its job spec!
        if !lease.declared_secrets.iter().any(|s| s == secret_name) {
            return Err(RunnerError::SecretAccessDenied(format!(
                "secret `{secret_name}` was not declared in job's brokered_secrets"
            )));
        }

        state
            .brokered_secrets
            .get(secret_name)
            .cloned()
            .ok_or_else(|| RunnerError::ControlPlane(format!("secret `{secret_name}` not found")))
    }
}

/// In-memory object store implementation for testing.
#[derive(Clone, Default)]
pub struct InMemoryObjectStore {
    inner: Arc<RwLock<HashMap<String, Vec<u8>>>>,
}

impl InMemoryObjectStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a bundle with a specific hash.
    pub async fn put_bundle(&self, hash: impl Into<String>, data: Vec<u8>) {
        let mut map = self.inner.write().await;
        map.insert(hash.into(), data);
    }
}

#[async_trait]
impl ObjectStoreClient for InMemoryObjectStore {
    async fn fetch_input_bundle(&self, hash: &str) -> Result<Vec<u8>, RunnerError> {
        let map = self.inner.read().await;
        map.get(hash)
            .cloned()
            .ok_or_else(|| RunnerError::ObjectStore(format!("bundle with hash `{hash}` not found")))
    }

    async fn upload_artifact(&self, key: &str, data: &[u8]) -> Result<(), RunnerError> {
        let mut map = self.inner.write().await;
        map.insert(key.to_string(), data.to_vec());
        Ok(())
    }
}
