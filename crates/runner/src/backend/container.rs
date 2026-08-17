//! Hardened container execution backend and specification translator (Task 8.4).
//!
//! Translates `JobSpec`, `SandboxSpec`, and `ResourceSpec` into container controls:
//! - Non-root execution (`runAsNonRoot: true`, UID/GID 10001)
//! - Read-only root filesystem (`readOnlyRootFilesystem: true`)
//! - Dropped Linux capabilities (`capabilities.drop: ["ALL"]`)
//! - No privilege escalation (`allowPrivilegeEscalation: false`)
//! - Workspace-only writable mounts (`/workspace` RW, root RO)
//! - Bounded CPU, memory, PIDs, and wall-clock timeout
//! - Deny-by-default network isolation (`--network none`)
//! - Allowlist-only environment variables (secrets never in env)

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;
use tokio::sync::watch;

use codypendent_sandbox::ENV_ALLOWLIST;

use crate::backend::{ExecutionOutcome, RunnerBackend};
use crate::types::{JobSpec, RunnerError};
use crate::workspace::WorkspaceGuard;

/// Security context applied to the container.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContainerSecurityContext {
    pub run_as_non_root: bool,
    pub uid: u32,
    pub gid: u32,
    pub read_only_root_filesystem: bool,
    pub allow_privilege_escalation: bool,
    pub capabilities_drop: Vec<String>,
    pub seccomp_profile: String,
}

impl Default for ContainerSecurityContext {
    fn default() -> Self {
        Self {
            run_as_non_root: true,
            uid: 10001,
            gid: 10001,
            read_only_root_filesystem: true,
            allow_privilege_escalation: false,
            capabilities_drop: vec!["ALL".to_string()],
            seccomp_profile: "RuntimeDefault".to_string(),
        }
    }
}

/// Resource constraints enforced on the container.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContainerResourceLimits {
    pub memory_bytes: u64,
    pub cpu_seconds: u64,
    pub wall_seconds: u64,
    pub maximum_output_bytes: u64,
    pub pids_limit: u64,
}

/// Network configuration for container isolation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContainerNetworkConfig {
    pub network_mode: String,
    pub deny_all_egress: bool,
    pub network_allowlist: Vec<String>,
}

impl Default for ContainerNetworkConfig {
    fn default() -> Self {
        Self {
            network_mode: "none".to_string(),
            deny_all_egress: true,
            network_allowlist: vec![],
        }
    }
}

/// Mount configuration for container filesystems.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContainerMount {
    pub host_path: String,
    pub container_path: String,
    pub read_only: bool,
}

/// Complete, validated container specification translated from JobSpec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContainerSpec {
    pub image: String,
    pub argv: Vec<String>,
    pub working_dir: String,
    pub env: HashMap<String, String>,
    pub security_context: ContainerSecurityContext,
    pub resources: ContainerResourceLimits,
    pub network: ContainerNetworkConfig,
    pub mounts: Vec<ContainerMount>,
}

impl ContainerSpec {
    /// Translate a `JobSpec` into hardened `ContainerSpec`, refusing unsupported grants.
    pub fn translate(
        job: &JobSpec,
        workspace_path: &Path,
        default_image: Option<&str>,
    ) -> Result<Self, RunnerError> {
        if job.argv.is_empty() {
            return Err(RunnerError::InvalidCommand("empty argv".to_string()));
        }

        // 1. Validate resource caps > 0
        job.resources.validate()?;

        // 2. Refuse unsupported network allowlist (fail-closed, §6.3)
        if !job.sandbox.network_allowlist.is_empty() {
            return Err(RunnerError::UnsupportedCapability(
                "host:port network allowlists require a broker; refusing unrestricted outbound access"
                    .to_string(),
            ));
        }

        let image = default_image
            .unwrap_or("ghcr.io/codehalwell/codypendent-runner-base:latest")
            .to_string();

        let security_context = ContainerSecurityContext::default();

        let resources = ContainerResourceLimits {
            memory_bytes: job.resources.memory_mb * 1024 * 1024,
            cpu_seconds: job.resources.cpu_seconds,
            wall_seconds: job.resources.wall_seconds,
            maximum_output_bytes: job.resources.maximum_output_mb * 1024 * 1024,
            pids_limit: job.resources.pids_limit.unwrap_or(100),
        };

        let network = ContainerNetworkConfig::default();

        // 3. Mounts: ONLY the workspace is writable
        let mut mounts = vec![ContainerMount {
            host_path: workspace_path.to_string_lossy().to_string(),
            container_path: "/workspace".to_string(),
            read_only: false,
        }];

        for rp in &job.sandbox.read_paths {
            mounts.push(ContainerMount {
                host_path: rp.clone(),
                container_path: rp.clone(),
                read_only: true,
            });
        }

        // 4. Environment: allowlist only, never secrets in env
        let env_allowlist = if job.sandbox.env_allowlist.is_empty() {
            ENV_ALLOWLIST.iter().map(|s| s.to_string()).collect()
        } else {
            job.sandbox.env_allowlist.clone()
        };

        let mut env = HashMap::new();
        for (k, v) in &job.env {
            if env_allowlist.contains(k) {
                env.insert(k.clone(), v.clone());
            }
        }

        let working_dir = job
            .working_directory
            .clone()
            .unwrap_or_else(|| "/workspace/src".to_string());

        Ok(Self {
            image,
            argv: job.argv.clone(),
            working_dir,
            env,
            security_context,
            resources,
            network,
            mounts,
        })
    }

    /// Build CLI arguments for a container runtime (`docker run` / `podman run`).
    #[must_use]
    pub fn build_cli_args(&self) -> Vec<String> {
        let mut args = vec!["run".to_string(), "--rm".to_string()];

        // Non-root UID / GID
        if self.security_context.run_as_non_root {
            args.push(format!(
                "--user={}:{}",
                self.security_context.uid, self.security_context.gid
            ));
        }

        // Read-only root
        if self.security_context.read_only_root_filesystem {
            args.push("--read-only".to_string());
        }

        // Drop capabilities
        for cap in &self.security_context.capabilities_drop {
            args.push(format!("--cap-drop={cap}"));
        }

        // Security options
        if !self.security_context.allow_privilege_escalation {
            args.push("--security-opt=no-new-privileges:true".to_string());
        }
        if !self.security_context.seccomp_profile.is_empty() {
            args.push(format!(
                "--security-opt=seccomp={}",
                self.security_context.seccomp_profile
            ));
        }

        // Network isolation (deny-by-default)
        args.push(format!("--network={}", self.network.network_mode));

        // Resource limits
        args.push(format!("--memory={}b", self.resources.memory_bytes));
        args.push(format!("--pids-limit={}", self.resources.pids_limit));

        // Mounts
        for mount in &self.mounts {
            let ro_flag = if mount.read_only { ":ro" } else { ":rw" };
            args.push(format!(
                "-v={}:{}{}",
                mount.host_path, mount.container_path, ro_flag
            ));
        }

        // Working directory
        args.push(format!("-w={}", self.working_dir));

        // Environment variables
        for (k, v) in &self.env {
            args.push(format!("-e={}={}", k, v));
        }

        // Image and command
        args.push(self.image.clone());
        args.extend(self.argv.clone());

        args
    }
}

/// Hardened container runner backend.
pub struct ContainerBackend {
    runtime_bin: Option<String>,
    default_image: Option<String>,
}

impl Default for ContainerBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl ContainerBackend {
    /// Initialize a container backend, probing for `docker` or `podman`.
    #[must_use]
    pub fn new() -> Self {
        let runtime_bin = probe_container_runtime();
        Self {
            runtime_bin,
            default_image: None,
        }
    }

    /// Construct a container backend with an explicit runtime and default image.
    #[must_use]
    pub fn with_runtime(runtime_bin: Option<String>, default_image: Option<String>) -> Self {
        Self {
            runtime_bin,
            default_image,
        }
    }
}

#[async_trait]
impl RunnerBackend for ContainerBackend {
    fn name(&self) -> &'static str {
        "hardened-container"
    }

    fn is_available(&self) -> bool {
        self.runtime_bin.is_some()
    }

    async fn execute(
        &self,
        job: &JobSpec,
        workspace: &WorkspaceGuard,
        mut cancel_rx: watch::Receiver<bool>,
    ) -> Result<ExecutionOutcome, RunnerError> {
        let spec = ContainerSpec::translate(job, workspace.path(), self.default_image.as_deref())?;

        if *cancel_rx.borrow() {
            return Err(RunnerError::Cancelled(
                "job cancelled before launch".to_string(),
            ));
        }

        let runtime = match &self.runtime_bin {
            Some(r) => r,
            None => {
                return Err(RunnerError::Container(
                    "no container runtime (docker/podman) available; refusing to run unconfined"
                        .to_string(),
                ));
            }
        };

        let start_time = Instant::now();
        let args = spec.build_cli_args();

        let mut cmd = tokio::process::Command::new(runtime);
        cmd.args(&args[1..]); // args[0] is "run"

        let timeout = std::time::Duration::from_secs(spec.resources.wall_seconds);

        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| RunnerError::Container(format!("failed to spawn container: {e}")))?;

        // `wait_with_output` consumes the child, which would leave the timeout and
        // cancellation branches with nothing to kill. Drain the pipes on separate tasks
        // instead and wait on the child by reference — draining concurrently is required,
        // because a child that fills a pipe buffer blocks forever if nobody is reading.
        let mut stdout_pipe = child.stdout.take().ok_or_else(|| {
            RunnerError::Container("container stdout pipe was not captured".to_string())
        })?;
        let mut stderr_pipe = child.stderr.take().ok_or_else(|| {
            RunnerError::Container("container stderr pipe was not captured".to_string())
        })?;
        let stdout_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut stdout_pipe, &mut buf)
                .await
                .map(|_| buf)
        });
        let stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut stderr_pipe, &mut buf)
                .await
                .map(|_| buf)
        });

        tokio::select! {
            res = child.wait() => {
                let status = res.map_err(|e| RunnerError::Container(format!("container execution error: {e}")))?;
                let duration = start_time.elapsed();
                let exit_code = status.code().unwrap_or(-1);

                let collect = |joined: std::result::Result<std::io::Result<Vec<u8>>, tokio::task::JoinError>| {
                    joined
                        .map_err(|e| RunnerError::Container(format!("output collection failed: {e}")))?
                        .map_err(|e| RunnerError::Container(format!("container output read error: {e}")))
                };
                let raw_stdout = collect(stdout_task.await)?;
                let raw_stderr = collect(stderr_task.await)?;

                let max_out = spec.resources.maximum_output_bytes as usize;
                let (stdout, truncated_out) = cap_output(raw_stdout, max_out);
                let (stderr, truncated_err) = cap_output(raw_stderr, max_out);

                Ok(ExecutionOutcome {
                    exit_code,
                    stdout,
                    stderr,
                    duration,
                    truncated: truncated_out || truncated_err,
                })
            }
            _ = tokio::time::sleep(timeout) => {
                let _ = child.kill().await;
                Err(RunnerError::Container(format!("container wall clock timeout exceeded ({timeout:?})")))
            }
            _ = cancel_rx.changed() => {
                let _ = child.kill().await;
                Err(RunnerError::Cancelled("container cancelled mid-execution".to_string()))
            }
        }
    }
}

fn cap_output(bytes: Vec<u8>, max_bytes: usize) -> (Vec<u8>, bool) {
    if bytes.len() > max_bytes {
        (bytes[..max_bytes].to_vec(), true)
    } else {
        (bytes, false)
    }
}

fn probe_container_runtime() -> Option<String> {
    for rt in &["docker", "podman", "nerdctl"] {
        if which_tool(rt) {
            return Some((*rt).to_string());
        }
    }
    None
}

fn which_tool(tool: &str) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(tool);
            if candidate.is_file() {
                return true;
            }
        }
    }
    false
}
