//! Runner execution backends (OS Process Sandbox and Hardened Container).

pub mod container;
pub mod process;

use async_trait::async_trait;
use std::time::Duration;
use tokio::sync::watch;

use crate::types::{JobSpec, RunnerError};
use crate::workspace::WorkspaceGuard;

/// The outcome of executing a runner job attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionOutcome {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub duration: Duration,
    pub truncated: bool,
}

/// Common trait implemented by runner execution backends.
#[async_trait]
pub trait RunnerBackend: Send + Sync {
    /// Human-readable backend name (e.g. "process-sandbox", "hardened-container").
    fn name(&self) -> &'static str;

    /// Whether this backend is operational on the current host.
    fn is_available(&self) -> bool;

    /// Execute `job` within the given isolated `workspace`.
    async fn execute(
        &self,
        job: &JobSpec,
        workspace: &WorkspaceGuard,
        cancel_rx: watch::Receiver<bool>,
    ) -> Result<ExecutionOutcome, RunnerError>;
}
