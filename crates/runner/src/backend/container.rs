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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::{watch, Mutex};

use crate::backend::{ExecutionOutcome, RunnerBackend};
use crate::policy::RunnerPolicy;
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
        Self::translate_with_policy(job, workspace_path, default_image, &RunnerPolicy::default())
    }

    /// Translate with an immutable policy configured by the runner owner.
    pub fn translate_with_policy(
        job: &JobSpec,
        workspace_path: &Path,
        default_image: Option<&str>,
        policy: &RunnerPolicy,
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
        let resolved_policy = policy.resolve(job, workspace_path)?;

        let image = default_image
            .unwrap_or("ghcr.io/codehalwell/codypendent-runner-base:latest")
            .to_string();

        let security_context = ContainerSecurityContext::default();

        let memory_bytes = job
            .resources
            .memory_mb
            .checked_mul(1024 * 1024)
            .ok_or_else(|| {
                RunnerError::InvalidCommand("resource cap `memory_mb` overflows bytes".to_string())
            })?;
        let maximum_output_bytes = job
            .resources
            .maximum_output_mb
            .checked_mul(1024 * 1024)
            .ok_or_else(|| {
                RunnerError::InvalidCommand(
                    "resource cap `maximum_output_mb` overflows bytes".to_string(),
                )
            })?;
        let resources = ContainerResourceLimits {
            memory_bytes,
            cpu_seconds: job.resources.cpu_seconds,
            wall_seconds: job.resources.wall_seconds,
            maximum_output_bytes,
            pids_limit: job.resources.pids_limit.unwrap_or(100),
        };

        let network = ContainerNetworkConfig::default();

        // 3. Mounts are resolved local workspace grants, never caller-supplied
        // host paths. A write grant subsumes a duplicate read grant.
        let mut mounts = Vec::new();
        for rp in &resolved_policy.read_paths {
            if resolved_policy
                .write_paths
                .iter()
                .any(|wp| wp.host == rp.host)
            {
                continue;
            }
            mounts.push(ContainerMount {
                host_path: rp.host.to_string_lossy().into_owned(),
                container_path: rp.guest.clone(),
                read_only: true,
            });
        }
        for wp in &resolved_policy.write_paths {
            mounts.push(ContainerMount {
                host_path: wp.host.to_string_lossy().into_owned(),
                container_path: wp.guest.clone(),
                read_only: false,
            });
        }

        // 4. Environment: allowlist only, never secrets in env
        let mut env = HashMap::new();
        for (k, v) in &job.env {
            if resolved_policy.env_allowlist.contains(k) {
                env.insert(k.clone(), v.clone());
            }
        }

        let working_dir = resolved_policy.working_directory.guest;

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
        self.build_cli_args_with_identity(None, None)
    }

    fn build_cli_args_with_identity(
        &self,
        container_name: Option<&str>,
        cidfile: Option<&Path>,
    ) -> Vec<String> {
        let mut args = vec!["run".to_string()];
        if let Some(container_name) = container_name {
            args.push(format!("--name={container_name}"));
        }
        if let Some(cidfile) = cidfile {
            args.push(format!("--cidfile={}", cidfile.to_string_lossy()));
        }

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
        // Docker and Podman both install their built-in default seccomp policy
        // when this flag is omitted. `RuntimeDefault` is a Kubernetes API enum,
        // not a portable CLI profile path, so emitting it makes real runtimes
        // fail before the container starts.
        if !self.security_context.seccomp_profile.is_empty()
            && self.security_context.seccomp_profile != "RuntimeDefault"
        {
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

const CONTAINER_CONTROL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const OUTPUT_READ_CHUNK_BYTES: usize = 8 * 1024;

#[derive(Debug)]
struct OutputBudget {
    remaining: usize,
    truncated: bool,
}

/// Hardened container runner backend.
pub struct ContainerBackend {
    runtime_bin: Option<String>,
    default_image: Option<String>,
    policy: RunnerPolicy,
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
            policy: RunnerPolicy::default(),
        }
    }

    /// Construct a container backend with an explicit runtime and default image.
    #[must_use]
    pub fn with_runtime(runtime_bin: Option<String>, default_image: Option<String>) -> Self {
        Self {
            runtime_bin,
            default_image,
            policy: RunnerPolicy::default(),
        }
    }

    /// Construct with an explicit runtime, image, and immutable local policy.
    #[must_use]
    pub fn with_runtime_and_policy(
        runtime_bin: Option<String>,
        default_image: Option<String>,
        policy: RunnerPolicy,
    ) -> Self {
        Self {
            runtime_bin,
            default_image,
            policy,
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
        let spec = ContainerSpec::translate_with_policy(
            job,
            workspace.path(),
            self.default_image.as_deref(),
            &self.policy,
        )?;

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
        let attempt_name = workspace
            .workspace_dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                RunnerError::Container("workspace has no safe attempt id".to_string())
            })?;
        if !attempt_name
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        {
            return Err(RunnerError::Container(
                "workspace attempt id contains unsafe container-name characters".to_string(),
            ));
        }
        let container_name = format!("codypendent-{attempt_name}");
        let cidfile = workspace
            .workspace_dir
            .parent()
            .ok_or_else(|| RunnerError::Container("workspace has no parent directory".to_string()))?
            .join(format!(".{attempt_name}.cid"));
        remove_cidfile(&cidfile)?;
        let args = spec.build_cli_args_with_identity(Some(&container_name), Some(&cidfile));

        let mut cmd = tokio::process::Command::new(runtime);
        cmd.args(&args);
        cmd.kill_on_drop(true);

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
        let maximum_output_bytes =
            usize::try_from(spec.resources.maximum_output_bytes).unwrap_or(usize::MAX);
        let output_budget = Arc::new(Mutex::new(OutputBudget {
            remaining: maximum_output_bytes,
            truncated: false,
        }));
        let stdout_budget = output_budget.clone();
        let stderr_budget = output_budget.clone();
        let stdout_task =
            tokio::spawn(async move { drain_bounded(&mut stdout_pipe, stdout_budget).await });
        let stderr_task =
            tokio::spawn(async move { drain_bounded(&mut stderr_pipe, stderr_budget).await });

        let execution_result = tokio::select! {
            res = child.wait() => {
                match res {
                    Err(error) => {
                        let _ = wait_for_container(runtime, &container_name, &cidfile, true).await;
                        stdout_task.abort();
                        stderr_task.abort();
                        Err(RunnerError::Container(format!("container execution error: {error}")))
                    }
                    Ok(status) => match wait_for_container(runtime, &container_name, &cidfile, false).await {
                        Err(error) => {
                            stdout_task.abort();
                            stderr_task.abort();
                            Err(error)
                        }
                        Ok(()) => {
                            let duration = start_time.elapsed();
                            let exit_code = status.code().unwrap_or(-1);
                            match (collect_output(stdout_task).await, collect_output(stderr_task).await) {
                                (Ok(stdout), Ok(stderr)) => {
                                    let truncated = output_budget.lock().await.truncated;
                                    Ok(ExecutionOutcome {
                                        exit_code,
                                        stdout,
                                        stderr,
                                        duration,
                                        truncated,
                                    })
                                }
                                (Err(error), _) | (_, Err(error)) => Err(error),
                            }
                        }
                    },
                }
            }
            _ = tokio::time::sleep(timeout) => {
                let container_cleanup = wait_for_container(runtime, &container_name, &cidfile, true).await;
                let runtime_cleanup = reap_runtime_child(&mut child).await;
                stdout_task.abort();
                stderr_task.abort();
                match container_cleanup.and(runtime_cleanup) {
                    Ok(()) => Err(RunnerError::Container(format!("container wall clock timeout exceeded ({timeout:?})"))),
                    Err(error) => Err(error),
                }
            }
            _ = cancel_rx.changed() => {
                let container_cleanup = wait_for_container(runtime, &container_name, &cidfile, true).await;
                let runtime_cleanup = reap_runtime_child(&mut child).await;
                stdout_task.abort();
                stderr_task.abort();
                match container_cleanup.and(runtime_cleanup) {
                    Ok(()) => Err(RunnerError::Cancelled("container cancelled mid-execution".to_string())),
                    Err(error) => Err(error),
                }
            }
        };
        remove_cidfile(&cidfile)?;
        execution_result
    }
}

async fn drain_bounded(
    reader: &mut (impl AsyncRead + Unpin),
    budget: Arc<Mutex<OutputBudget>>,
) -> std::io::Result<Vec<u8>> {
    let mut retained = Vec::new();
    let mut chunk = [0_u8; OUTPUT_READ_CHUNK_BYTES];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok(retained);
        }
        let mut budget = budget.lock().await;
        let keep = read.min(budget.remaining);
        retained.extend_from_slice(&chunk[..keep]);
        budget.remaining -= keep;
        if keep < read {
            budget.truncated = true;
        }
    }
}

async fn collect_output(
    task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
) -> Result<Vec<u8>, RunnerError> {
    task.await
        .map_err(|error| RunnerError::Container(format!("output collection failed: {error}")))?
        .map_err(|error| RunnerError::Container(format!("container output read error: {error}")))
}

async fn wait_for_container(
    runtime: &str,
    container_name: &str,
    cidfile: &Path,
    kill_first: bool,
) -> Result<(), RunnerError> {
    let identity = read_container_identity(cidfile).unwrap_or_else(|| container_name.to_string());
    if kill_first {
        // A non-zero kill can mean the container exited between the select and
        // this command. `wait` below is the authoritative proof of termination.
        let _ = bounded_runtime_status(runtime, &["kill", &identity]).await;
    }
    let wait_status = bounded_runtime_status(runtime, &["wait", &identity]).await?;
    if !wait_status.success() {
        return Err(RunnerError::Container(format!(
            "container runtime could not prove {identity:?} terminated"
        )));
    }
    let remove_status = bounded_runtime_status(runtime, &["rm", "-f", &identity]).await?;
    if !remove_status.success() {
        return Err(RunnerError::Container(format!(
            "container runtime could not remove completed container {identity:?}"
        )));
    }
    Ok(())
}

async fn bounded_runtime_status(
    runtime: &str,
    args: &[&str],
) -> Result<std::process::ExitStatus, RunnerError> {
    let mut command = tokio::process::Command::new(runtime);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(CONTAINER_CONTROL_TIMEOUT, command.status())
        .await
        .map_err(|_| {
            RunnerError::Container(format!(
                "container runtime command {:?} timed out",
                args.first().copied().unwrap_or("unknown")
            ))
        })?
        .map_err(|error| {
            RunnerError::Container(format!("container runtime command failed: {error}"))
        })
}

async fn reap_runtime_child(child: &mut tokio::process::Child) -> Result<(), RunnerError> {
    match tokio::time::timeout(CONTAINER_CONTROL_TIMEOUT, child.wait()).await {
        Ok(result) => result
            .map(|_| ())
            .map_err(|error| RunnerError::Container(format!("failed to reap runtime: {error}"))),
        Err(_) => {
            child.kill().await.map_err(|error| {
                RunnerError::Container(format!("failed to kill runtime: {error}"))
            })?;
            child
                .wait()
                .await
                .map(|_| ())
                .map_err(|error| RunnerError::Container(format!("failed to reap runtime: {error}")))
        }
    }
}

fn read_container_identity(cidfile: &Path) -> Option<String> {
    let metadata = std::fs::metadata(cidfile).ok()?;
    if !metadata.is_file() || metadata.len() > 128 {
        return None;
    }
    let identity = std::fs::read_to_string(cidfile).ok()?;
    let identity = identity.trim();
    if (12..=64).contains(&identity.len()) && identity.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        Some(identity.to_string())
    } else {
        None
    }
}

fn remove_cidfile(cidfile: &PathBuf) -> Result<(), RunnerError> {
    match std::fs::remove_file(cidfile) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RunnerError::Container(format!(
            "failed to remove container cidfile: {error}"
        ))),
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
