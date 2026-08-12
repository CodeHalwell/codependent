//! `workflow.query` — the agent's read of a durable workflow run's **graph
//! state** (rubric 5): nodes, their lifecycle states, the dependency edges
//! between them, and measured per-node cost.
//!
//! Until now the DAG was a purely human surface: the TUI could render a run's
//! nodes but an agent had no way to ask "what has already run, what is blocked,
//! and what depends on what". This tool closes that half — the same
//! `WorkflowRunSnapshot` projection the `ReadWorkflowRun` command replies with,
//! reached through the pool-erased
//! [`WorkflowQueryChannel`](crate::blackboard::WorkflowQueryChannel) seam so this
//! crate stays free of `sqlx` and the workflow domain types (ADR-009), exactly as
//! `blackboard.*` reaches its store.
//!
//! Unlike `blackboard.*` this tool is offered **outside** a workflow run too: it
//! is a read of Codypendent's own coordination state scoped to the run's
//! repository, so a plain chat agent asked "how did the last /fix-ci go?" can
//! answer. Inside a workflow node the run's own id is the default subject, so an
//! agent can inspect its siblings' progress without being told the id.
//!
//! It reads and never writes, touches no file/command/network, and so its
//! [`ProposedAction`] is policy-allowed unconditionally — recorded purely so the
//! access is traced like every other tool call.

use codypendent_protocol::ProposedAction;
use serde_json::Value;

/// The `workflow.query` tool: read a run's graph state, or list the repository's
/// recent runs.
pub struct WorkflowQueryTool;

impl WorkflowQueryTool {
    /// The stable dotted tool name.
    pub const NAME: &'static str = "workflow.query";

    /// The action policy evaluates: a read of Codypendent's own durable workflow
    /// store, never the filesystem or a remote.
    #[must_use]
    pub fn proposed_action(workflow_run_id: &str) -> ProposedAction {
        ProposedAction::WorkflowQuery {
            workflow_run_id: workflow_run_id.to_string(),
        }
    }
}

/// The parsed arguments of a `workflow.query` call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkflowQueryInput {
    /// The run to inspect. `None` means "the ambient run" inside a workflow node,
    /// and "list this repository's recent runs" from a plain chat run.
    pub workflow_run_id: Option<String>,
}

/// Parse `workflow.query` arguments. Everything is optional: a bare call inside a
/// workflow node reads that node's own run, and a bare call from chat lists the
/// repository's recent runs. `run_id` is accepted as an alias because a model that
/// has just seen a run id in prose reaches for the shorter name.
#[must_use]
pub fn parse_workflow_query(args: &Value) -> WorkflowQueryInput {
    let workflow_run_id = args
        .get("workflow_run_id")
        .or_else(|| args.get("run_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    WorkflowQueryInput { workflow_run_id }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_bare_call_names_no_run() {
        // The ambient-run case: inside a workflow node this reads the node's own
        // run; from chat it lists the repository's runs.
        assert_eq!(parse_workflow_query(&json!({})).workflow_run_id, None);
        // A blank string is the same as absent — a model that filled the field
        // with "" must not send the daemon looking for a run named "".
        assert_eq!(
            parse_workflow_query(&json!({ "workflow_run_id": "  " })).workflow_run_id,
            None
        );
    }

    #[test]
    fn an_explicit_run_is_taken_under_either_name() {
        assert_eq!(
            parse_workflow_query(&json!({ "workflow_run_id": "wfrun-abc" })).workflow_run_id,
            Some("wfrun-abc".to_string())
        );
        assert_eq!(
            parse_workflow_query(&json!({ "run_id": "wfrun-xyz" })).workflow_run_id,
            Some("wfrun-xyz".to_string())
        );
    }

    #[test]
    fn the_proposed_action_names_the_run_it_reads() {
        assert_eq!(
            WorkflowQueryTool::proposed_action("wfrun-abc"),
            ProposedAction::WorkflowQuery {
                workflow_run_id: "wfrun-abc".to_string()
            }
        );
    }
}
