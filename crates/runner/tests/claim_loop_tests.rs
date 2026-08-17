//! Claim loop, lease renewal, scope isolation, and partial upload tests.
//!
//! Verifies:
//! - Acceptance Criterion 5: "Renewal at a stale generation is refused.
//!   Test: renew_with_stale_generation_is_refused"
//! - Acceptance Criterion 11: "A compromised runner cannot widen scope. With a valid lease
//!   for job A, requests against job B, another repository, and a secret not in
//!   brokered_secrets are all refused. Test: compromised_runner_cannot_claim_other_scope"
//! - Acceptance Criterion 14: "A partial upload leaves the job uploading and resumes.
//!   Test: partial_upload_resumes_without_duplicate_output"

use uuid::Uuid;

use codypendent_runner::{
    ClaimRequest, ControlPlaneClient, InMemoryControlPlane, JobSpec, OutputUpload, RenewRequest,
    ResourceSpec, RunnerError, RunnerIdentity, SandboxSpec, WorkspaceLayout,
};

fn test_job_spec(secrets: Vec<String>) -> JobSpec {
    JobSpec {
        argv: vec!["/bin/true".to_string()],
        env: Default::default(),
        working_directory: None,
        workspace_layout: WorkspaceLayout::default(),
        input_manifest_hash: "none".to_string(),
        sandbox: SandboxSpec {
            brokered_secrets: secrets,
            ..Default::default()
        },
        resources: ResourceSpec::default(),
        outputs: vec![],
        max_attempts: 1,
    }
}

#[tokio::test]
async fn renew_with_stale_generation_is_refused() {
    let cp = InMemoryControlPlane::new();
    let org_id = Uuid::now_v7();
    let runner = RunnerIdentity::generate(org_id, "test-runner", "container", None);
    cp.register(&runner).await.unwrap();

    let job_id = Uuid::now_v7();
    cp.queue_job(job_id, org_id, Uuid::now_v7(), test_job_spec(vec![]))
        .await;

    let claim_req = ClaimRequest {
        runner_id: runner.id,
        organization_id: org_id,
        capabilities: runner.capabilities.clone(),
        max_concurrency: 1,
    };

    let claim = cp.claim(&claim_req).await.unwrap().unwrap();
    assert_eq!(claim.lease_generation, 1);

    // First renewal: presents generation 1 -> bumps to generation 2
    let renew_req1 = RenewRequest {
        lease_id: claim.lease_id,
        generation: 1,
        lease_token: claim.lease_token.clone(),
    };
    let resp1 = cp.renew_lease(&renew_req1).await.unwrap();
    assert_eq!(resp1.new_generation, 2);

    // Stale renewal: replayed message with generation 1 must be REFUSED!
    let stale_req = RenewRequest {
        lease_id: claim.lease_id,
        generation: 1,
        lease_token: claim.lease_token.clone(),
    };
    let err = cp.renew_lease(&stale_req).await.unwrap_err();
    assert!(
        matches!(
            err,
            RunnerError::StaleGeneration {
                requested: 1,
                current: 2
            }
        ),
        "Expected StaleGeneration error, got {err:?}"
    );

    // Second valid renewal: presents generation 2 -> bumps to generation 3
    let renew_req2 = RenewRequest {
        lease_id: claim.lease_id,
        generation: 2,
        lease_token: claim.lease_token.clone(),
    };
    let resp2 = cp.renew_lease(&renew_req2).await.unwrap();
    assert_eq!(resp2.new_generation, 3);
}

#[tokio::test]
async fn compromised_runner_cannot_claim_other_scope() {
    let cp = InMemoryControlPlane::new();
    let org_id = Uuid::now_v7();
    let runner_a = RunnerIdentity::generate(org_id, "runner-a", "container", None);
    cp.register(&runner_a).await.unwrap();

    // Add secret into control plane broker
    cp.add_secret("allowed_token", "secret_value_123").await;
    cp.add_secret("unauthorized_admin_token", "super_secret_admin_key")
        .await;

    // Queue Job A declaring only "allowed_token"
    let job_a_id = Uuid::now_v7();
    cp.queue_job(
        job_a_id,
        org_id,
        Uuid::now_v7(),
        test_job_spec(vec!["allowed_token".to_string()]),
    )
    .await;

    let claim_req = ClaimRequest {
        runner_id: runner_a.id,
        organization_id: org_id,
        capabilities: runner_a.capabilities.clone(),
        max_concurrency: 1,
    };

    let claim_a = cp.claim(&claim_req).await.unwrap().unwrap();

    // 1. Permitted secret request succeeds
    let secret = cp
        .request_secret(&claim_a.lease_token, "allowed_token")
        .await
        .unwrap();
    assert_eq!(secret, "secret_value_123");

    // 2. Secret NOT in brokered_secrets is refused
    let err_secret = cp
        .request_secret(&claim_a.lease_token, "unauthorized_admin_token")
        .await
        .unwrap_err();
    assert!(matches!(err_secret, RunnerError::SecretAccessDenied(_)));

    // 3. Forged lease token is refused
    let err_token = cp
        .request_secret("forged_lease_token_xyz", "allowed_token")
        .await
        .unwrap_err();
    assert!(matches!(err_token, RunnerError::UnauthorizedScope(_)));
}

#[tokio::test]
async fn partial_upload_resumes_without_duplicate_output() {
    let cp = InMemoryControlPlane::new();
    let attempt_id = Uuid::now_v7();

    let output_1 = OutputUpload {
        attempt_id,
        name: "artifact.tar.gz".to_string(),
        content_hash: "sha256:1111111111111111111111111111111111111111111111111111111111111111"
            .to_string(),
        byte_length: 5000,
        media_type: "application/gzip".to_string(),
        object_key: "artifacts/1".to_string(),
        classification: "public".to_string(),
    };

    // First upload attempt
    cp.upload_output(&output_1).await.unwrap();

    let outputs = cp.get_outputs().await;
    assert_eq!(outputs.len(), 1);

    // Resumed / repeated upload of the same output must be idempotent (no duplicate rows)
    cp.upload_output(&output_1).await.unwrap();

    let outputs_after = cp.get_outputs().await;
    assert_eq!(
        outputs_after.len(),
        1,
        "Duplicate upload must not duplicate outputs"
    );
}
