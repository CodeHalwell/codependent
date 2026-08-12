//! Typed natural-language tools for persisted multi-model councils.

use codypendent_council::CouncilDefinition;
use codypendent_protocol::ProposedAction;
use serde_json::Value;

pub struct CouncilCreateTool;
impl CouncilCreateTool {
    pub const NAME: &'static str = "council.create";
}

pub struct CouncilRunTool;
impl CouncilRunTool {
    pub const NAME: &'static str = "council.run";
}

pub struct CouncilResultTool;
impl CouncilResultTool {
    pub const NAME: &'static str = "council.result";
}

#[derive(Debug, Clone)]
pub struct CouncilCreateInput {
    pub definition: CouncilDefinition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CouncilRunInput {
    pub name: String,
    pub objective: String,
    pub evidence: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CouncilResultInput {
    pub selector: String,
}

pub fn parse_council_create(args: &Value) -> Result<CouncilCreateInput, String> {
    let definition: CouncilDefinition = serde_json::from_value(args.clone()).map_err(|error| {
        format!(
            "council.create needs name, at least two unique members {{model, role}}, chair, and optional rounds/description/evidence: {error}"
        )
    })?;
    Ok(CouncilCreateInput { definition })
}

pub fn parse_council_run(args: &Value) -> Result<CouncilRunInput, String> {
    Ok(CouncilRunInput {
        name: required(args, "name", CouncilRunTool::NAME)?,
        objective: required(args, "objective", CouncilRunTool::NAME)?,
        evidence: args
            .get("evidence")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

pub fn parse_council_result(args: &Value) -> Result<CouncilResultInput, String> {
    let selector = args
        .get("selector")
        .or_else(|| args.get("result_id"))
        .or_else(|| args.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "council.result requires `selector` (a result id or council name)".to_owned()
        })?;
    Ok(CouncilResultInput {
        selector: selector.to_owned(),
    })
}

fn required(args: &Value, field: &str, tool: &str) -> Result<String, String> {
    args.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{tool} requires a non-empty string `{field}`"))
}

#[must_use]
pub fn council_create_action(input: &CouncilCreateInput) -> ProposedAction {
    ProposedAction::CouncilCreate {
        name: input.definition.name.clone(),
        summary: format!(
            "create council `{}` with {} members, chair `{}`, {} round(s){}",
            input.definition.name,
            input.definition.members.len(),
            input.definition.chair,
            input.definition.rounds,
            if input.definition.evidence {
                ", evidence mode"
            } else {
                ""
            }
        ),
    }
}

#[must_use]
pub fn council_run_action(input: &CouncilRunInput) -> ProposedAction {
    ProposedAction::CouncilRun {
        name: input.name.clone(),
        summary: format!(
            "run council `{}` for objective: {}",
            input.name,
            input.objective.chars().take(240).collect::<String>()
        ),
    }
}

#[must_use]
pub fn council_result_action(input: &CouncilResultInput) -> ProposedAction {
    ProposedAction::CouncilResultRead {
        selector: input.selector.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn typed_create_requires_the_full_definition_shape() {
        let input = parse_council_create(&json!({
            "name": "reviewers",
            "members": [
                {"model": "claude", "role": "critic"},
                {"model": "codex", "role": "implementer"}
            ],
            "chair": "claude",
            "rounds": 2,
            "evidence": true
        }))
        .expect("typed definition");
        assert_eq!(input.definition.members.len(), 2);
        assert!(input.definition.evidence);
        assert!(parse_council_create(&json!({"name": "incomplete"})).is_err());
    }

    #[test]
    fn run_preview_is_bounded_and_result_accepts_aliases() {
        let input = parse_council_run(&json!({
            "name": "reviewers",
            "objective": "review the release"
        }))
        .unwrap();
        assert!(matches!(
            council_run_action(&input),
            ProposedAction::CouncilRun { .. }
        ));
        assert_eq!(
            parse_council_result(&json!({"result_id": "result-1"}))
                .unwrap()
                .selector,
            "result-1"
        );
    }
}
