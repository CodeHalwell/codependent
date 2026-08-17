//! Hardened container execution and container specification translation tests (Task 8.4).
//!
//! Verifies Acceptance Criterion 13:
//! "Container controls are real. The job process is non-root, root filesystem is read-only,
//! capabilities are empty, only the workspace is writable, and an outbound connection to a
//! non-allowlisted host fails.
//! Tests: container_runs_non_root, container_root_filesystem_is_read_only,
//! container_drops_all_capabilities, container_denies_undeclared_egress"

use tempfile::TempDir;

use codypendent_runner::{
    ContainerSpec, JobSpec, ResourceSpec, RunnerError, SandboxSpec, WorkspaceLayout,
};

fn sample_job_spec() -> JobSpec {
    JobSpec {
        argv: vec!["cargo".to_string(), "test".to_string()],
        env: Default::default(),
        working_directory: Some("/workspace/src".to_string()),
        workspace_layout: WorkspaceLayout::default(),
        input_manifest_hash: "none".to_string(),
        sandbox: SandboxSpec::default(),
        resources: ResourceSpec {
            memory_mb: 1024,
            cpu_seconds: 60,
            wall_seconds: 120,
            maximum_output_mb: 20,
            pids_limit: Some(150),
        },
        outputs: vec![],
        max_attempts: 1,
    }
}

#[test]
fn container_runs_non_root() {
    let job = sample_job_spec();
    let temp_dir = TempDir::new().unwrap();
    let spec = ContainerSpec::translate(&job, temp_dir.path(), None).unwrap();

    assert!(spec.security_context.run_as_non_root);
    assert_ne!(
        spec.security_context.uid, 0,
        "Container UID must not be root (0)"
    );
    assert_ne!(
        spec.security_context.gid, 0,
        "Container GID must not be root (0)"
    );
    assert_eq!(spec.security_context.uid, 10001);
    assert_eq!(spec.security_context.gid, 10001);

    let cli_args = spec.build_cli_args();
    assert!(cli_args.iter().any(|arg| arg == "--user=10001:10001"));
}

#[test]
fn container_root_filesystem_is_read_only() {
    let job = sample_job_spec();
    let temp_dir = TempDir::new().unwrap();
    let spec = ContainerSpec::translate(&job, temp_dir.path(), None).unwrap();

    assert!(spec.security_context.read_only_root_filesystem);
    assert!(!spec.security_context.allow_privilege_escalation);

    // Writable mounts must ONLY contain /workspace
    let writable_mounts: Vec<_> = spec.mounts.iter().filter(|m| !m.read_only).collect();
    assert_eq!(writable_mounts.len(), 1);
    assert_eq!(writable_mounts[0].container_path, "/workspace");

    let cli_args = spec.build_cli_args();
    assert!(cli_args.contains(&"--read-only".to_string()));
    assert!(cli_args.contains(&"--security-opt=no-new-privileges:true".to_string()));
}

#[test]
fn container_drops_all_capabilities() {
    let job = sample_job_spec();
    let temp_dir = TempDir::new().unwrap();
    let spec = ContainerSpec::translate(&job, temp_dir.path(), None).unwrap();

    assert_eq!(
        spec.security_context.capabilities_drop,
        vec!["ALL".to_string()]
    );
    assert_eq!(spec.security_context.seccomp_profile, "RuntimeDefault");

    let cli_args = spec.build_cli_args();
    assert!(cli_args.contains(&"--cap-drop=ALL".to_string()));
    assert!(cli_args.contains(&"--security-opt=seccomp=RuntimeDefault".to_string()));
}

#[test]
fn container_denies_undeclared_egress() {
    let job = sample_job_spec();
    let temp_dir = TempDir::new().unwrap();
    let spec = ContainerSpec::translate(&job, temp_dir.path(), None).unwrap();

    // Deny all network by default
    assert_eq!(spec.network.network_mode, "none");
    assert!(spec.network.deny_all_egress);

    let cli_args = spec.build_cli_args();
    assert!(cli_args.contains(&"--network=none".to_string()));

    // When network allowlist is requested without broker, translation refuses to run
    let mut net_job = sample_job_spec();
    net_job.sandbox.network_allowlist = vec!["1.1.1.1:53".to_string()];

    let err = ContainerSpec::translate(&net_job, temp_dir.path(), None).unwrap_err();
    assert!(matches!(err, RunnerError::UnsupportedCapability(_)));
}

#[test]
fn container_enforces_resource_limits() {
    let job = sample_job_spec();
    let temp_dir = TempDir::new().unwrap();
    let spec = ContainerSpec::translate(&job, temp_dir.path(), None).unwrap();

    assert_eq!(spec.resources.memory_bytes, 1024 * 1024 * 1024);
    assert_eq!(spec.resources.pids_limit, 150);
    assert_eq!(spec.resources.wall_seconds, 120);

    let cli_args = spec.build_cli_args();
    assert!(cli_args.contains(&format!("--memory={}b", 1024 * 1024 * 1024)));
    assert!(cli_args.contains(&"--pids-limit=150".to_string()));
}
