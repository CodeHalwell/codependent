//! Runner agent daemon coordinating lease claim, execution, logs, artifacts, and attestations.

use chrono::Utc;
use sha2::{Digest, Sha256};
use std::fs;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::attestation::sign_attestation;
use crate::backend::RunnerBackend;
use crate::client::{ControlPlaneClient, ObjectStoreClient};
use crate::identity::RunnerIdentity;
use crate::log_streamer::LogStreamer;
use crate::materialize::{MaterializeLimits, Materializer};
use crate::types::{
    AttestationOutput, AttestationStatement, AttestationVerifyResult, ClaimRequest, JobClaim,
    OutputUpload, ReleaseRequest, RenewRequest, RunnerError,
};
use crate::workspace::{WorkspaceGuard, WorkspaceManager};

/// The runner agent daemon.
pub struct RunnerAgent {
    pub identity: RunnerIdentity,
    pub control_plane: Arc<dyn ControlPlaneClient>,
    pub object_store: Arc<dyn ObjectStoreClient>,
    pub workspace_manager: WorkspaceManager,
    pub backend: Arc<dyn RunnerBackend>,
    pub materialize_limits: MaterializeLimits,
}

impl RunnerAgent {
    /// Create a new runner agent daemon.
    #[must_use]
    pub fn new(
        identity: RunnerIdentity,
        control_plane: Arc<dyn ControlPlaneClient>,
        object_store: Arc<dyn ObjectStoreClient>,
        workspace_manager: WorkspaceManager,
        backend: Arc<dyn RunnerBackend>,
    ) -> Self {
        Self {
            identity,
            control_plane,
            object_store,
            workspace_manager,
            backend,
            materialize_limits: MaterializeLimits::default(),
        }
    }

    /// Register with control plane and run the claim loop until shutdown is signaled.
    pub async fn run(&self, mut shutdown_rx: watch::Receiver<bool>) -> Result<(), RunnerError> {
        info!(
            runner_id = %self.identity.id,
            name = %self.identity.name,
            backend = %self.backend.name(),
            "Registering runner agent with control plane"
        );

        self.control_plane.register(&self.identity).await?;

        let claim_req = ClaimRequest {
            runner_id: self.identity.id,
            organization_id: self.identity.organization_id,
            capabilities: self.identity.capabilities.clone(),
            max_concurrency: 1,
        };

        while !*shutdown_rx.borrow() {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    info!("Shutdown signaled; stopping runner agent claim loop");
                    break;
                }
                claim_res = self.control_plane.claim(&claim_req) => {
                    match claim_res {
                        Ok(Some(claim)) => {
                            info!(job_id = %claim.job_id, attempt_id = %claim.attempt_id, "Claimed job lease");
                            if let Err(e) = self.execute_claim(claim).await {
                                error!(error = %e, "Failed to execute claimed job");
                            }
                        }
                        Ok(None) => {
                            // Queue empty; sleep briefly before polling again
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                        Err(e) => {
                            warn!(error = %e, "Error claiming job; retrying after backoff");
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Execute a single claimed job lease end-to-end.
    pub async fn execute_claim(
        &self,
        claim: JobClaim,
    ) -> Result<AttestationVerifyResult, RunnerError> {
        let started_at = Utc::now().to_rfc3339();

        // 1. Prepare isolated workspace
        let mut workspace = self
            .workspace_manager
            .create_workspace(claim.job_id, claim.attempt_id)?;

        // 2. Setup cancellation watch and background lease renewal
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let lease_id = claim.lease_id;
        let lease_token = claim.lease_token.clone();
        let control_plane = self.control_plane.clone();

        let heartbeat_handle = tokio::spawn(async move {
            let mut generation = claim.lease_generation;
            let renew_interval = Duration::from_secs(10);

            loop {
                tokio::time::sleep(renew_interval).await;
                let req = RenewRequest {
                    lease_id,
                    generation,
                    lease_token: lease_token.clone(),
                };

                match control_plane.renew_lease(&req).await {
                    Ok(resp) => {
                        generation = resp.new_generation;
                        if resp.cancel_requested {
                            let _ = cancel_tx.send(true);
                            break;
                        }
                    }
                    Err(_) => {
                        // Lease renewal failed or expired; self-terminate execution
                        let _ = cancel_tx.send(true);
                        break;
                    }
                }
            }
        });

        // 3. Materialize input bundle safely
        let materialize_res = self.materialize_inputs(&claim, &workspace).await;

        let mut log_streamer =
            LogStreamer::new(claim.attempt_id, claim.job_spec.resources.maximum_output_mb);

        let (exit_code, result_state) = match materialize_res {
            Ok(_) => {
                // 4. Execute command through backend
                match self
                    .backend
                    .execute(&claim.job_spec, &workspace, cancel_rx)
                    .await
                {
                    Ok(outcome) => {
                        for chunk in log_streamer.ingest_stdout(&outcome.stdout) {
                            let _ = self.control_plane.stream_logs(chunk).await;
                        }
                        for chunk in log_streamer.ingest_stderr(&outcome.stderr) {
                            let _ = self.control_plane.stream_logs(chunk).await;
                        }
                        for chunk in log_streamer.flush() {
                            let _ = self.control_plane.stream_logs(chunk).await;
                        }

                        let res = if outcome.exit_code == 0 {
                            "succeeded"
                        } else {
                            "failed"
                        };
                        (outcome.exit_code, res)
                    }
                    Err(RunnerError::Cancelled(_)) => {
                        let _ = log_streamer.ingest_stderr(b"Job cancelled mid-execution");
                        for chunk in log_streamer.flush() {
                            let _ = self.control_plane.stream_logs(chunk).await;
                        }
                        (-1, "cancelled")
                    }
                    Err(e) => {
                        let err_msg = format!("Backend execution error: {e}");
                        let _ = log_streamer.ingest_stderr(err_msg.as_bytes());
                        for chunk in log_streamer.flush() {
                            let _ = self.control_plane.stream_logs(chunk).await;
                        }
                        (-1, "failed")
                    }
                }
            }
            Err(e) => {
                let err_msg = format!("Input materialization error: {e}");
                let _ = log_streamer.ingest_stderr(err_msg.as_bytes());
                for chunk in log_streamer.flush() {
                    let _ = self.control_plane.stream_logs(chunk).await;
                }
                (-1, "failed")
            }
        };

        let ended_at = Utc::now().to_rfc3339();

        // Abort heartbeat task
        heartbeat_handle.abort();

        // 5. Gather and upload declared outputs
        let mut attestation_outputs = Vec::new();
        if result_state == "succeeded" {
            for out_decl in &claim.job_spec.outputs {
                let target_path = workspace.output_dir.join(&out_decl.path);
                let file_path = if target_path.exists() {
                    target_path
                } else {
                    workspace.source_dir.join(&out_decl.path)
                };

                if file_path.exists() && file_path.is_file() {
                    if let Ok(bytes) = fs::read(&file_path) {
                        let hash = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
                        let byte_length = bytes.len() as u64;
                        let object_key = format!(
                            "artifacts/{}/{}/{}",
                            claim.job_id, claim.attempt_id, out_decl.name
                        );

                        let _ = self.object_store.upload_artifact(&object_key, &bytes).await;

                        let upload_req = OutputUpload {
                            attempt_id: claim.attempt_id,
                            name: out_decl.name.clone(),
                            content_hash: hash.clone(),
                            byte_length,
                            media_type: out_decl.media_type.clone(),
                            object_key,
                            classification: "public".to_string(),
                        };

                        let _ = self.control_plane.upload_output(&upload_req).await;

                        attestation_outputs.push(AttestationOutput {
                            name: out_decl.name.clone(),
                            content_hash: hash,
                            byte_length,
                        });
                    }
                }
            }
        }

        // 6. Sign and submit attestation
        let statement = AttestationStatement {
            job_id: claim.job_id,
            job_spec_hash: claim.job_spec_hash.clone(),
            attempt_id: claim.attempt_id,
            attempt_number: claim.attempt_number,
            lease_id: claim.lease_id,
            lease_generation: claim.lease_generation,
            runner_id: self.identity.id,
            image_digest: self
                .identity
                .capabilities
                .image_digest
                .clone()
                .unwrap_or_else(|| "none".to_string()),
            input_manifest_hash: claim.input_manifest_hash.clone(),
            outputs: attestation_outputs,
            started_at,
            ended_at,
            exit_code,
            result: result_state.to_string(),
        };

        let attestation = sign_attestation(statement, &self.identity);
        let verify_result = self.control_plane.submit_attestation(&attestation).await?;

        // 7. Teardown workspace and release lease
        let _ = workspace.teardown();

        let release_req = ReleaseRequest {
            lease_id: claim.lease_id,
            attempt_id: claim.attempt_id,
            reason: result_state.to_string(),
            lease_token: claim.lease_token,
        };
        let _ = self.control_plane.release_lease(&release_req).await;

        Ok(verify_result)
    }

    async fn materialize_inputs(
        &self,
        claim: &JobClaim,
        workspace: &WorkspaceGuard,
    ) -> Result<(), RunnerError> {
        if claim.input_manifest_hash.is_empty() || claim.input_manifest_hash == "none" {
            return Ok(());
        }

        let bundle_bytes = self
            .object_store
            .fetch_input_bundle(&claim.input_manifest_hash)
            .await?;

        let materializer = Materializer::new(self.materialize_limits.clone());
        materializer.materialize_bytes(&bundle_bytes, &workspace.source_dir, None)?;

        Ok(())
    }
}
