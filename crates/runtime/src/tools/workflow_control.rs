//! Typed `workflow.create` and `workflow.run` model-facing arguments.

use std::collections::HashSet;

use codypendent_protocol::ProposedAction;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::workflow_control::{WorkflowDraft, WorkflowRunTarget};

const MAX_STEPS: usize = 64;
const MAX_ID_LEN: usize = 96;
const MAX_DESCRIPTION_LEN: usize = 4_000;

pub struct WorkflowCreateTool;

impl WorkflowCreateTool {
    pub const NAME: &'static str = "workflow.create";
}

pub struct WorkflowRunTool;

impl WorkflowRunTool {
    pub const NAME: &'static str = "workflow.run";
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowCreateInput {
    pub workflow: WorkflowDraft,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowRunInput {
    pub target: WorkflowRunTarget,
    pub inputs: Value,
}

/// JSON schema shared by `workflow.create` and the inline target accepted by
/// `workflow.run`. Keeping it beside the parser prevents the advertised tool
/// contract drifting from the deny-unknown-fields Rust types.
pub fn workflow_draft_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "id": {"type": "string", "minLength": 1, "maxLength": MAX_ID_LEN},
            "version": {"type": "integer", "minimum": 1, "default": 1},
            "description": {"type": "string", "maxLength": MAX_DESCRIPTION_LEN},
            "inputs": {
                "type": "object",
                "additionalProperties": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "type": {"type": "string", "minLength": 1},
                        "required": {"type": "boolean", "default": false}
                    },
                    "required": ["type"]
                }
            },
            "budget": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "maximum_cost_usd": {"type": "number", "exclusiveMinimum": 0},
                    "maximum_duration_seconds": {"type": "integer", "minimum": 1},
                    "maximum_agents": {"type": "integer", "minimum": 1}
                }
            },
            "steps": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_STEPS,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "id": {"type": "string", "minLength": 1, "maxLength": MAX_ID_LEN},
                        "depends_on": {"type": "array", "items": {"type": "string"}},
                        "agent": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "role": {"type": "string", "minLength": 1},
                                "model_policy": {"type": "string"}
                            },
                            "required": ["role"]
                        },
                        "tool": {"type": "string", "minLength": 1},
                        "with": {"type": "object"},
                        "skill": {"type": "string"},
                        "workspace": {"type": "string", "enum": ["shared-worktree", "isolated-worktree"]},
                        "approval": {"type": "string", "enum": ["before-write", "always"]},
                        "retry": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "attempts": {"type": "integer", "minimum": 1, "maximum": 10},
                                "backoff_seconds": {"type": "integer", "minimum": 0}
                            },
                            "required": ["attempts"]
                        },
                        "outputs": {"type": "array", "items": {"type": "string"}}
                    },
                    "required": ["id"],
                    "oneOf": [
                        {"required": ["agent"], "not": {"required": ["tool"]}},
                        {"required": ["tool"], "not": {"required": ["agent"]}}
                    ]
                }
            },
            "orchestration_reason": {
                "type": "string",
                "enum": ["parallelism", "independent-review", "access-separation", "specialist"]
            }
        },
        "required": ["id", "steps"]
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    #[serde(default)]
    workflow_id: Option<String>,
    #[serde(default)]
    workflow: Option<WorkflowDraft>,
    #[serde(default = "empty_object")]
    inputs: Value,
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

pub fn parse_workflow_create(args: &Value) -> Result<WorkflowCreateInput, String> {
    let workflow: WorkflowDraft = serde_json::from_value(args.clone())
        .map_err(|error| format!("workflow.create invalid arguments: {error}"))?;
    validate_workflow(&workflow, WorkflowCreateTool::NAME)?;
    Ok(WorkflowCreateInput { workflow })
}

pub fn parse_workflow_run(args: &Value) -> Result<WorkflowRunInput, String> {
    let parsed: RunArgs = serde_json::from_value(args.clone())
        .map_err(|error| format!("workflow.run invalid arguments: {error}"))?;
    if !parsed.inputs.is_object() {
        return Err("workflow.run `inputs` must be an object".to_string());
    }
    let target = match (parsed.workflow_id, parsed.workflow) {
        (Some(id), None) => {
            validate_identifier(&id, "workflow.run `workflow_id`")?;
            WorkflowRunTarget::Named(id)
        }
        (None, Some(workflow)) => {
            validate_workflow(&workflow, WorkflowRunTool::NAME)?;
            WorkflowRunTarget::Inline(workflow)
        }
        (Some(_), Some(_)) => {
            return Err(
                "workflow.run accepts exactly one of `workflow_id` or inline `workflow`"
                    .to_string(),
            )
        }
        (None, None) => {
            return Err(
                "workflow.run requires either `workflow_id` or inline `workflow`".to_string(),
            )
        }
    };
    Ok(WorkflowRunInput {
        target,
        inputs: parsed.inputs,
    })
}

pub fn workflow_create_action(input: &WorkflowCreateInput) -> ProposedAction {
    ProposedAction::WorkflowCreate {
        workflow_id: input.workflow.id.clone(),
        summary: format!(
            "save workflow `{}` v{} with {} step(s)",
            input.workflow.id,
            input.workflow.version,
            input.workflow.steps.len()
        ),
    }
}

pub fn workflow_run_action(input: &WorkflowRunInput) -> ProposedAction {
    let (workflow_id, kind, steps) = match &input.target {
        WorkflowRunTarget::Named(id) => (id.clone(), "named", None),
        WorkflowRunTarget::Inline(workflow) => {
            (workflow.id.clone(), "inline", Some(workflow.steps.len()))
        }
    };
    let summary = match steps {
        Some(steps) => format!("start inline workflow `{workflow_id}` with {steps} step(s)"),
        None => format!("start named workflow `{workflow_id}`"),
    };
    ProposedAction::WorkflowRun {
        workflow_id,
        kind: kind.to_string(),
        summary,
    }
}

fn validate_workflow(workflow: &WorkflowDraft, tool: &str) -> Result<(), String> {
    validate_identifier(&workflow.id, &format!("{tool} `id`"))?;
    if workflow.version == 0 {
        return Err(format!("{tool} `version` must be at least 1"));
    }
    if workflow.steps.is_empty() || workflow.steps.len() > MAX_STEPS {
        return Err(format!("{tool} requires 1..={MAX_STEPS} workflow steps"));
    }
    if workflow
        .description
        .as_ref()
        .is_some_and(|description| description.chars().count() > MAX_DESCRIPTION_LEN)
    {
        return Err(format!(
            "{tool} `description` is longer than {MAX_DESCRIPTION_LEN} characters"
        ));
    }
    if let Some(cost) = workflow.budget.maximum_cost_usd {
        if !cost.is_finite() || cost <= 0.0 {
            return Err(format!(
                "{tool} `budget.maximum_cost_usd` must be finite and greater than zero"
            ));
        }
    }
    if workflow.budget.maximum_duration_seconds == Some(0)
        || workflow.budget.maximum_agents == Some(0)
    {
        return Err(format!("{tool} budget limits must be greater than zero"));
    }

    let mut ids = HashSet::with_capacity(workflow.steps.len());
    for step in &workflow.steps {
        validate_identifier(&step.id, &format!("{tool} step `id`"))?;
        if !ids.insert(step.id.as_str()) {
            return Err(format!("{tool} has duplicate step id `{}`", step.id));
        }
        if step.agent.is_some() == step.tool.is_some() {
            return Err(format!(
                "{tool} step `{}` must declare exactly one of `agent` or `tool`",
                step.id
            ));
        }
        if let Some(agent) = &step.agent {
            validate_text(
                &agent.role,
                &format!("{tool} step `{}` agent role", step.id),
            )?;
        }
        if let Some(name) = &step.tool {
            validate_text(name, &format!("{tool} step `{}` tool", step.id))?;
        }
        if let Some(retry) = step.retry {
            if retry.attempts == 0 || retry.attempts > 10 {
                return Err(format!(
                    "{tool} step `{}` retry attempts must be in 1..=10",
                    step.id
                ));
            }
        }
    }
    for step in &workflow.steps {
        for dependency in &step.depends_on {
            if !ids.contains(dependency.as_str()) {
                return Err(format!(
                    "{tool} step `{}` depends on unknown step `{dependency}`",
                    step.id
                ));
            }
        }
    }
    for (name, input) in &workflow.inputs {
        validate_identifier(name, &format!("{tool} input name"))?;
        validate_text(&input.input_type, &format!("{tool} input `{name}` type"))?;
    }
    Ok(())
}

fn validate_identifier(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty()
        || value.chars().count() > MAX_ID_LEN
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(format!(
            "{field} must be 1..={MAX_ID_LEN} ASCII letters, digits, `.`, `_`, or `-`"
        ));
    }
    Ok(())
}

fn validate_text(value: &str, field: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.contains(['\r', '\n']) {
        return Err(format!("{field} must be non-empty, single-line text"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn one_step() -> Value {
        json!({
            "id": "review-change",
            "description": "Review a change safely",
            "budget": {"maximum_agents": 1},
            "steps": [{
                "id": "review",
                "agent": {"role": "reviewer"},
                "workspace": "shared-worktree",
                "outputs": ["finding"]
            }]
        })
    }

    #[test]
    fn create_parses_a_structured_manifest_and_defaults_version() {
        let parsed = parse_workflow_create(&one_step()).expect("valid workflow");
        assert_eq!(parsed.workflow.id, "review-change");
        assert_eq!(parsed.workflow.version, 1);
        assert_eq!(parsed.workflow.steps.len(), 1);
    }

    #[test]
    fn create_refuses_traversal_and_ambiguous_steps() {
        let mut value = one_step();
        value["id"] = json!("../escape");
        assert!(parse_workflow_create(&value).unwrap_err().contains("ASCII"));

        let mut value = one_step();
        value["steps"][0]["tool"] = json!("repository.test");
        assert!(parse_workflow_create(&value)
            .unwrap_err()
            .contains("exactly one"));
    }

    #[test]
    fn run_requires_exactly_one_named_or_inline_target() {
        assert!(parse_workflow_run(&json!({})).is_err());
        assert!(parse_workflow_run(&json!({
            "workflow_id": "review-change",
            "workflow": one_step()
        }))
        .is_err());
        assert!(matches!(
            parse_workflow_run(&json!({"workflow_id": "review-change"}))
                .expect("named")
                .target,
            WorkflowRunTarget::Named(_)
        ));
        assert!(matches!(
            parse_workflow_run(&json!({"workflow": one_step()}))
                .expect("inline")
                .target,
            WorkflowRunTarget::Inline(_)
        ));
    }

    #[test]
    fn run_inputs_must_be_an_object() {
        let error = parse_workflow_run(&json!({
            "workflow_id": "review-change",
            "inputs": ["not", "an", "object"]
        }))
        .unwrap_err();
        assert!(error.contains("must be an object"));
    }
}
