//! End-to-end integration tests for the runner agent daemon.

use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::watch;
use uuid::Uuid;

use codypendent_runner::{
    InMemoryControlPlane, InMemoryObjectStore, JobSpec, ProcessSandboxBackend, ResourceSpec,
    RunnerAgent, RunnerIdentity, SandboxSpec, WorkspaceLayout, WorkspaceManager,
};

#[tokio::test(flavor = "multi_thread")]
async fn runner_agent_executes_job_and_submits_attestation() {
    let org_id = Uuid::now_v7();
    let runner_identity = RunnerIdentity::generate(org_id, "test-agent-1", "container", None);

    let control_plane = Arc::new(InMemoryControlPlane::new());
    let object_store = Arc::new(InMemoryObjectStore::new());

    let temp_dir = TempDir::new().unwrap();
    let workspace_manager = WorkspaceManager::new(temp_dir.path());

    let backend = Arc::new(ProcessSandboxBackend::new());
    if !backend.is_available() {
        return;
    }

    let agent = RunnerAgent::new(
        runner_identity.clone(),
        control_plane.clone(),
        object_store.clone(),
        workspace_manager,
        backend,
    );

    // Queue a test job
    let job_id = Uuid::now_v7();
    let job_spec = JobSpec {
        argv: vec!["/bin/echo".to_string(), "runner agent e2e".to_string()],
        env: Default::default(),
        working_directory: None,
        workspace_layout: WorkspaceLayout::default(),
        input_manifest_hash: "none".to_string(),
        sandbox: SandboxSpec::default(),
        resources: ResourceSpec::default(),
        outputs: vec![],
        max_attempts: 1,
    };

    control_plane
        .queue_job(job_id, org_id, Uuid::now_v7(), job_spec)
        .await;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let agent_handle = tokio::spawn(async move { agent.run(shutdown_rx).await });

    // Allow agent to claim and execute the job
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let _ = shutdown_tx.send(true);
    let _ = agent_handle.await;

    // Verify logs were streamed
    let logs = control_plane.get_logs().await;
    assert!(!logs.is_empty(), "Logs should have been streamed");

    // Verify attestation was submitted and verified
    let attestations = control_plane.get_attestations().await;
    assert_eq!(
        attestations.len(),
        1,
        "Attestation should have been submitted"
    );
    assert_eq!(attestations[0].job_id, job_id);
}
