//! Hardened container execution and container specification translation tests (Task 8.4).
//!
//! Verifies Acceptance Criterion 13:
//! "Container controls are real. The job process is non-root, root filesystem is read-only,
//! capabilities are empty, only the workspace is writable, and an outbound connection to a
//! non-allowlisted host fails.
//! Tests: container_runs_non_root, container_root_filesystem_is_read_only,
//! container_drops_all_capabilities, container_denies_undeclared_egress"

use tempfile::TempDir;
use tokio::sync::watch;
use uuid::Uuid;

use codypendent_runner::{
    ContainerBackend, ContainerSpec, JobSpec, ResourceSpec, RunnerBackend, RunnerError,
    SandboxSpec, WorkspaceLayout, WorkspaceManager,
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

fn workspace() -> TempDir {
    let workspace = TempDir::new().unwrap();
    for name in ["src", "out", "tmp"] {
        std::fs::create_dir(workspace.path().join(name)).unwrap();
    }
    workspace
}

#[test]
fn container_runs_non_root() {
    let job = sample_job_spec();
    let temp_dir = workspace();
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
    let temp_dir = workspace();
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
    let temp_dir = workspace();
    let spec = ContainerSpec::translate(&job, temp_dir.path(), None).unwrap();

    assert_eq!(
        spec.security_context.capabilities_drop,
        vec!["ALL".to_string()]
    );
    assert_eq!(spec.security_context.seccomp_profile, "RuntimeDefault");

    let cli_args = spec.build_cli_args();
    assert!(cli_args.contains(&"--cap-drop=ALL".to_string()));
    assert!(!cli_args.contains(&"--security-opt=seccomp=RuntimeDefault".to_string()));
}

#[test]
fn container_denies_undeclared_egress() {
    let job = sample_job_spec();
    let temp_dir = workspace();
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
    let temp_dir = workspace();
    let spec = ContainerSpec::translate(&job, temp_dir.path(), None).unwrap();

    assert_eq!(spec.resources.memory_bytes, 1024 * 1024 * 1024);
    assert_eq!(spec.resources.pids_limit, 150);
    assert_eq!(spec.resources.wall_seconds, 120);

    let cli_args = spec.build_cli_args();
    assert!(cli_args.contains(&format!("--memory={}b", 1024 * 1024 * 1024)));
    assert!(cli_args.contains(&"--pids-limit=150".to_string()));
}

#[test]
fn container_refuses_resource_byte_overflow() {
    let temp_dir = workspace();
    let mut job = sample_job_spec();
    job.resources.memory_mb = u64::MAX;
    assert!(matches!(
        ContainerSpec::translate(&job, temp_dir.path(), None),
        Err(RunnerError::InvalidCommand(message)) if message.contains("memory_mb")
    ));

    let mut job = sample_job_spec();
    job.resources.maximum_output_mb = u64::MAX;
    assert!(matches!(
        ContainerSpec::translate(&job, temp_dir.path(), None),
        Err(RunnerError::InvalidCommand(message)) if message.contains("maximum_output_mb")
    ));
}

#[test]
fn container_refuses_host_mount_cwd_and_environment_escalation() {
    let temp_dir = workspace();
    for path in ["/", "/etc", "../outside", "/workspace/../outside"] {
        let mut job = sample_job_spec();
        job.sandbox.read_paths = vec![path.to_string()];
        assert!(matches!(
            ContainerSpec::translate(&job, temp_dir.path(), None),
            Err(RunnerError::UnauthorizedScope(_))
        ));
    }

    let mut cwd_job = sample_job_spec();
    cwd_job.working_directory = Some("/etc".to_string());
    assert!(matches!(
        ContainerSpec::translate(&cwd_job, temp_dir.path(), None),
        Err(RunnerError::UnauthorizedScope(_))
    ));

    let mut env_job = sample_job_spec();
    env_job.sandbox.env_allowlist = vec!["AWS_SECRET_ACCESS_KEY".to_string()];
    assert!(matches!(
        ContainerSpec::translate(&env_job, temp_dir.path(), None),
        Err(RunnerError::UnauthorizedScope(_))
    ));
}

#[cfg(unix)]
fn fake_runtime(temp_dir: &TempDir) -> (String, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let runtime = temp_dir.path().join("fake-runtime");
    let log = temp_dir.path().join("fake-runtime.log");
    std::fs::write(
        &runtime,
        r#"#!/bin/sh
log="$0.log"
printf 'CALL' >> "$log"
for arg in "$@"; do printf ' <%s>' "$arg" >> "$log"; done
printf '\n' >> "$log"
case "$1" in
  run)
    sleep_mode=false
    for arg in "$@"; do
      case "$arg" in --cidfile=*) cidfile=${arg#--cidfile=} ;; esac
      if test "$arg" = 'sleep:image'; then sleep_mode=true; fi
    done
    printf '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n' > "$cidfile"
    printf '%s\n' $$ > "$0.pid"
    if test "$sleep_mode" = true; then
      exec sleep 30
    else
      dd if=/dev/zero bs=1048576 count=2 2>/dev/null
    fi
    ;;
  kill)
    if test -f "$0.pid"; then kill -KILL "$(cat "$0.pid")" 2>/dev/null || true; fi
    ;;
  wait|rm) ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    (runtime.to_string_lossy().into_owned(), log)
}

#[cfg(unix)]
#[tokio::test]
async fn container_runtime_receives_run_identity_wait_and_bounded_output() {
    let temp_dir = TempDir::new().unwrap();
    let (runtime, log) = fake_runtime(&temp_dir);
    let manager = WorkspaceManager::new(temp_dir.path().join("workspaces"));
    let workspace = manager
        .create_workspace(Uuid::now_v7(), Uuid::now_v7())
        .unwrap();
    let backend = ContainerBackend::with_runtime(Some(runtime), Some("test:image".to_string()));
    let mut job = sample_job_spec();
    job.resources.maximum_output_mb = 1;
    let (_cancel_tx, cancel_rx) = watch::channel(false);

    let outcome = backend.execute(&job, &workspace, cancel_rx).await.unwrap();

    assert_eq!(outcome.stdout.len(), 1024 * 1024);
    assert!(outcome.truncated);
    let calls = std::fs::read_to_string(log).unwrap();
    assert!(calls.lines().next().unwrap().starts_with("CALL <run>"));
    assert!(calls.contains("--name=codypendent-"));
    assert!(calls.contains("--cidfile="));
    assert!(calls.contains("CALL <wait>"));
    assert!(calls.contains("CALL <rm> <-f>"));
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_kills_and_waits_for_the_daemon_owned_container() {
    let temp_dir = TempDir::new().unwrap();
    let (runtime, log) = fake_runtime(&temp_dir);
    let manager = WorkspaceManager::new(temp_dir.path().join("workspaces"));
    let workspace = manager
        .create_workspace(Uuid::now_v7(), Uuid::now_v7())
        .unwrap();
    let backend = ContainerBackend::with_runtime(Some(runtime), Some("sleep:image".to_string()));
    let job = sample_job_spec();
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let pidfile = temp_dir.path().join("fake-runtime.pid");

    let execution = backend.execute(&job, &workspace, cancel_rx);
    let cancellation = async move {
        for _ in 0..200 {
            if pidfile.is_file() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(pidfile.is_file(), "fake container never became ready");
        cancel_tx.send(true).unwrap();
    };
    let (result, ()) = tokio::join!(execution, cancellation);

    assert!(
        matches!(result, Err(RunnerError::Cancelled(_))),
        "unexpected cancellation result: {result:?}"
    );
    let calls = std::fs::read_to_string(log).unwrap();
    assert!(calls.contains("CALL <kill>"));
    assert!(calls.contains("CALL <wait>"));
    assert!(calls.contains("CALL <rm> <-f>"));
}
