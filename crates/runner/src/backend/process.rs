//! Process sandbox execution backend reusing `crates/sandbox` OS confinement.
//!
//! Follows the fail-closed security posture in `docs/superpowers/implementation/M8-self-hosted-runners.md` §2.1 and §6.1.

use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::watch;

use codypendent_sandbox::{enforcing_executor, SandboxCommand, SandboxExecutor, SandboxProfile};

use crate::backend::{ExecutionOutcome, RunnerBackend};
use crate::policy::RunnerPolicy;
use crate::types::{JobSpec, RunnerError};
use crate::workspace::WorkspaceGuard;

/// Backend that executes commands inside a platform-native sandbox (Seatbelt / bubblewrap).
pub struct ProcessSandboxBackend {
    executor: Result<Box<dyn SandboxExecutor>, String>,
    policy: RunnerPolicy,
}

impl Default for ProcessSandboxBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessSandboxBackend {
    /// Initialize the process sandbox backend using the enforcing OS executor.
    #[must_use]
    pub fn new() -> Self {
        let executor = enforcing_executor().map_err(|e| e.to_string());
        Self {
            executor,
            policy: RunnerPolicy::default(),
        }
    }

    /// Construct a backend with an injected sandbox executor (for testing).
    #[must_use]
    pub fn with_executor(executor: Box<dyn SandboxExecutor>) -> Self {
        Self {
            executor: Ok(executor),
            policy: RunnerPolicy::default(),
        }
    }

    /// Construct a backend with an injected executor and immutable local policy.
    #[must_use]
    pub fn with_executor_and_policy(
        executor: Box<dyn SandboxExecutor>,
        policy: RunnerPolicy,
    ) -> Self {
        Self {
            executor: Ok(executor),
            policy,
        }
    }

    /// Whether an enforcing sandbox is actually available on this host.
    ///
    /// This reports the same condition [`RunnerBackend::execute`] fails closed on, so a
    /// caller can skip work it knows would be refused. It is never a licence to run
    /// unconfined: `execute` re-checks and still refuses.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.executor
            .as_ref()
            .is_ok_and(|exec| exec.capability_report().available)
    }
}

#[async_trait]
impl RunnerBackend for ProcessSandboxBackend {
    fn name(&self) -> &'static str {
        "process-sandbox"
    }

    fn is_available(&self) -> bool {
        match &self.executor {
            Ok(exec) => exec.capability_report().available,
            Err(_) => false,
        }
    }

    async fn execute(
        &self,
        job: &JobSpec,
        workspace: &WorkspaceGuard,
        mut cancel_rx: watch::Receiver<bool>,
    ) -> Result<ExecutionOutcome, RunnerError> {
        // Fail closed immediately if sandbox is unavailable
        let executor = match &self.executor {
            Ok(exec) => {
                let report = exec.capability_report();
                if !report.available {
                    return Err(RunnerError::SandboxUnavailable(
                        "sandbox executor reported unavailable; refusing to run unconfined"
                            .to_string(),
                    ));
                }
                exec
            }
            Err(err) => {
                return Err(RunnerError::SandboxUnavailable(format!(
                    "{err}; refusing to run unconfined"
                )));
            }
        };

        // Check if cancelled before starting
        if *cancel_rx.borrow() {
            return Err(RunnerError::Cancelled(
                "job cancelled before launch".to_string(),
            ));
        }

        // Validate command
        if job.argv.is_empty() {
            return Err(RunnerError::InvalidCommand("empty argv".to_string()));
        }

        // Validate resource caps: zero is never unlimited
        job.resources.validate()?;

        // Fail-closed network rule (§6.3 option 1): network allowlists require a broker
        if !job.sandbox.network_allowlist.is_empty() {
            return Err(RunnerError::UnsupportedCapability(
                "host:port network allowlists require a broker; refusing unrestricted outbound access"
                    .to_string(),
            ));
        }

        let resolved_policy = self.policy.resolve(job, workspace.path())?;

        // Fail-closed environment rule. `SandboxCommand` has no per-command environment:
        // the executor clears the environment and re-adds only the names in the profile's
        // `env_allowlist`, taking their values from the daemon's own environment. Arbitrary
        // caller-supplied values cannot be delivered, so refuse rather than drop them —
        // a job that silently ran without its environment would be worse than one refused.
        if !job.env.is_empty() {
            let mut names: Vec<&str> = job.env.keys().map(String::as_str).collect();
            names.sort_unstable();
            return Err(RunnerError::UnsupportedCapability(format!(
                "per-job environment values are not deliverable under the process sandbox \
                 (it clears the environment and re-adds only allowlisted names); \
                 refusing job that sets: {}",
                names.join(", ")
            )));
        }

        let profile = SandboxProfile::new(
            "codypendent-runner-job",
            resolved_policy.env_allowlist,
            resolved_policy
                .read_paths
                .iter()
                .map(|path| path.host.to_string_lossy().into_owned())
                .collect(),
            resolved_policy
                .write_paths
                .iter()
                .map(|path| path.host.to_string_lossy().into_owned())
                .collect(),
            vec![], // network_allowlist must be empty
            job.sandbox.brokered_secrets.clone(),
            job.sandbox.allow_subprocess,
            job.resources.memory_mb,
            job.resources.cpu_seconds,
            job.resources.wall_seconds,
            job.resources.maximum_output_mb,
        );

        let program_str = &job.argv[0];
        let program = if program_str.starts_with('/') || program_str.starts_with('.') {
            PathBuf::from(program_str)
        } else {
            // Find absolute path of binary
            resolve_binary_path(program_str).unwrap_or_else(|| PathBuf::from(program_str))
        };

        let args = job.argv[1..].to_vec();
        let working_dir = resolved_policy.working_directory.host;

        let command = SandboxCommand::new(
            program,
            args,
            working_dir,
            format!("runner:job:{}", job.input_manifest_hash),
        );

        let start_time = Instant::now();

        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation_signal = cancelled.clone();
        let cancellation_watcher = tokio::spawn(async move {
            if *cancel_rx.borrow() {
                cancellation_signal.store(true, Ordering::Release);
                return;
            }
            if cancel_rx.changed().await.is_ok() && *cancel_rx.borrow() {
                cancellation_signal.store(true, Ordering::Release);
            }
        });

        // Run synchronously on a blocking thread so the sandbox's synchronous
        // executor never stalls the reactor. The enforcing executor polls the
        // host-owned atomic and sweeps the process group when a lease is revoked.
        // NOTE: `block_in_place` PANICS on a current-thread runtime — the daemon
        // and runner agent both run multi-threaded, and tests driving this path
        // must ask for `#[tokio::test(flavor = "multi_thread")]`.
        let result = tokio::task::block_in_place(|| {
            executor.run_cancellable(&profile, &command, &cancelled)
        });
        cancellation_watcher.abort();
        let outcome = match result {
            Err(codypendent_sandbox::SandboxError::Cancelled) => {
                return Err(RunnerError::Cancelled(
                    "process sandbox cancelled mid-execution".to_string(),
                ));
            }
            other => other?,
        };

        let duration = start_time.elapsed();

        Ok(ExecutionOutcome {
            // `None` means the process was killed (timeout or resource cap). `-1` is the
            // crate's existing convention for that — see the container backend — and keeps
            // the `exit_code == 0` success test in `agent.rs` correctly false.
            exit_code: outcome.exit_code.unwrap_or(-1),
            stdout: outcome.stdout.text.into_bytes(),
            stderr: outcome.stderr.text.into_bytes(),
            duration,
            truncated: outcome.output_truncated,
        })
    }
}

fn resolve_binary_path(name: &str) -> Option<PathBuf> {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
