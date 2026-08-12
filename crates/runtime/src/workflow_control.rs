//! Runtime-facing seam for authoring and starting durable workflows.
//!
//! The runtime deliberately does not depend on `codypendent-workflow` (doing so
//! would invert the workspace layering).  These draft types are the small,
//! typed contract produced by the model-facing tools.  The assembly maps them
//! to the canonical workflow model, compiles them, persists user workflows, and
//! starts runs through the daemon's existing `WorkflowStarter` seam.

use std::collections::BTreeMap;

use async_trait::async_trait;
use codypendent_protocol::SessionId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDraft {
    pub id: String,
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub inputs: BTreeMap<String, WorkflowInputDraft>,
    #[serde(default)]
    pub budget: WorkflowBudgetDraft,
    pub steps: Vec<WorkflowStepDraft>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration_reason: Option<WorkflowOrchestrationReasonDraft>,
}

const fn default_version() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowInputDraft {
    #[serde(rename = "type")]
    pub input_type: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowBudgetDraft {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_duration_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_agents: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStepDraft {
    pub id: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<WorkflowAgentDraft>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub with: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkflowWorkspaceDraft>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<WorkflowApprovalDraft>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<WorkflowRetryDraft>,
    #[serde(default)]
    pub outputs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowAgentDraft {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_policy: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkflowWorkspaceDraft {
    SharedWorktree,
    IsolatedWorktree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkflowApprovalDraft {
    BeforeWrite,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRetryDraft {
    pub attempts: u32,
    #[serde(default)]
    pub backoff_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkflowOrchestrationReasonDraft {
    Parallelism,
    IndependentReview,
    AccessSeparation,
    Specialist,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WorkflowRunTarget {
    Named(String),
    Inline(WorkflowDraft),
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowCreateRequest {
    pub workflow: WorkflowDraft,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowCreated {
    pub workflow_id: String,
    pub version: u32,
    /// A durable, human-openable handle. The assembly returns the manifest path.
    pub handle: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowRunRequest {
    pub target: WorkflowRunTarget,
    pub inputs: Value,
    /// Always copied from the active run, never accepted from tool arguments.
    pub repository: String,
    /// Attribution to the chat session that launched the durable workflow.
    pub session_id: SessionId,
    /// Stable across a retried delivery of the same tool call.
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowStarted {
    pub workflow_id: String,
    pub workflow_run_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkflowControlError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Backend(String),
}

#[async_trait]
pub trait WorkflowControlChannel: Send + Sync {
    async fn create(
        &self,
        request: WorkflowCreateRequest,
    ) -> Result<WorkflowCreated, WorkflowControlError>;

    async fn run(
        &self,
        request: WorkflowRunRequest,
    ) -> Result<WorkflowStarted, WorkflowControlError>;
}
