//! `task.create` / `task.update` / `task.move` / `task.list` — the natural-language
//! backlog tools (rubric 10).
//!
//! These are what makes "break this feature into backlog cards" work end to end:
//! the agent turns prose into durable `task` cards on the **repository's** board,
//! the same rows the kanban pane renders and a human moves. They are deliberately
//! *not* the `blackboard.*` tools:
//!
//! * scope — a blackboard post targets the agent's own workflow run and exists
//!   only inside one; a task card targets the repository, so these are offered
//!   from a plain chat run too (that is the whole point of a backlog);
//! * shape — a card carries a column, an assignee, and a position, which a typed
//!   workflow artifact has no notion of.
//!
//! The store is reached through the pool-erased
//! [`TaskBoardChannel`](crate::blackboard::TaskBoardChannel) seam, exactly as
//! `blackboard.*` reaches [`BlackboardChannel`](crate::blackboard::BlackboardChannel)
//! — the assembly implements both over one `BlackboardStore`, so an agent-created
//! card and a human-created one are the same durable row under the same
//! supersession discipline.
//!
//! A board write touches no file, command, network, or remote — it is internal
//! coordination state, like a blackboard post — so its [`ProposedAction`] is
//! policy-allowed without an approval gate, and recorded so every board write is
//! traced and attributable.

use codypendent_protocol::ProposedAction;
use serde_json::{Map, Value};

/// Longest accepted card title. A title is a one-line column entry in every
/// client, so an unbounded string would let one write distort the whole board.
const MAX_TITLE_LEN: usize = 200;

/// The `task.create` tool: add a card to the repository's board.
pub struct TaskCreateTool;

impl TaskCreateTool {
    /// The stable dotted tool name.
    pub const NAME: &'static str = "task.create";
}

/// The `task.update` tool: edit a card's body, assignee, or position.
pub struct TaskUpdateTool;

impl TaskUpdateTool {
    /// The stable dotted tool name.
    pub const NAME: &'static str = "task.update";
}

/// The `task.move` tool: move a card to another column.
pub struct TaskMoveTool;

impl TaskMoveTool {
    /// The stable dotted tool name.
    pub const NAME: &'static str = "task.move";
}

/// The `task.list` tool: read the repository's live board.
pub struct TaskListTool;

impl TaskListTool {
    /// The stable dotted tool name.
    pub const NAME: &'static str = "task.list";
}

/// The action policy evaluates for a board **write**. `repository` is
/// server-derived from the run context — never model-supplied — and `summary` is a
/// short human rendering so the trace reads without decoding the payload.
#[must_use]
pub fn task_write_action(repository: &str, summary: String) -> ProposedAction {
    ProposedAction::TaskWrite {
        repository: repository.to_string(),
        summary,
    }
}

/// The action policy evaluates for a board **read**.
#[must_use]
pub fn task_read_action(repository: &str) -> ProposedAction {
    ProposedAction::TaskRead {
        repository: repository.to_string(),
    }
}

/// The parsed arguments of a `task.create` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCreateInput {
    /// The card's one-line title.
    pub title: String,
    /// A longer body, if the model wrote one.
    pub description: Option<String>,
    /// The starting column; the board defaults a card with none to `todo`.
    pub status: Option<String>,
    /// Who the card is assigned to, if anyone.
    pub assignee: Option<String>,
}

impl TaskCreateInput {
    /// The card body to store: a stable `{ title, description? }` shape every
    /// client renders, rather than whatever the model happened to nest.
    #[must_use]
    pub fn payload(&self) -> Value {
        let mut payload = Map::new();
        payload.insert("title".to_string(), Value::String(self.title.clone()));
        if let Some(description) = &self.description {
            payload.insert(
                "description".to_string(),
                Value::String(description.clone()),
            );
        }
        Value::Object(payload)
    }
}

/// Parse `task.create` arguments. Only `title` is required — a backlog card is a
/// line of prose, and demanding more would make the tool harder to call than
/// writing the sentence.
pub fn parse_task_create(args: &Value) -> Result<TaskCreateInput, String> {
    let title = required_text(args, "title", TaskCreateTool::NAME)?;
    if title.chars().count() > MAX_TITLE_LEN {
        return Err(format!(
            "task.create `title` is longer than {MAX_TITLE_LEN} characters — put the detail in `description`"
        ));
    }
    Ok(TaskCreateInput {
        title,
        description: optional_text(args, "description"),
        status: optional_text(args, "status"),
        assignee: optional_text(args, "assignee"),
    })
}

/// The parsed arguments of a `task.update` or `task.move` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskUpdateInput {
    /// The card to revise.
    pub item_id: String,
    /// The new column, when moving.
    pub status: Option<String>,
    /// The new assignee, when re-assigning.
    pub assignee: Option<String>,
    /// The new within-column position; absent appends when the column changed.
    pub ordinal: Option<i64>,
    /// A replacement title, when editing.
    pub title: Option<String>,
    /// A replacement description, when editing.
    pub description: Option<String>,
}

impl TaskUpdateInput {
    /// The replacement body, or `None` when this call changes only board fields —
    /// so a pure move carries the card's existing text forward untouched rather
    /// than blanking it.
    #[must_use]
    pub fn payload(&self) -> Option<Value> {
        if self.title.is_none() && self.description.is_none() {
            return None;
        }
        let mut payload = Map::new();
        if let Some(title) = &self.title {
            payload.insert("title".to_string(), Value::String(title.clone()));
        }
        if let Some(description) = &self.description {
            payload.insert(
                "description".to_string(),
                Value::String(description.clone()),
            );
        }
        Some(Value::Object(payload))
    }

    /// A short human rendering of this write for the recorded action.
    #[must_use]
    pub fn summary(&self, verb: &str) -> String {
        match &self.status {
            Some(status) => format!("{verb} {} to {status}", self.item_id),
            None => format!("{verb} {}", self.item_id),
        }
    }
}

/// Parse `task.update` arguments: the card id plus whatever is being changed.
pub fn parse_task_update(args: &Value) -> Result<TaskUpdateInput, String> {
    parse_update_like(args, TaskUpdateTool::NAME, false)
}

/// Parse `task.move` arguments — the same shape as `task.update`, but `status` is
/// required: a move with no destination is a no-op the agent should not spend a
/// turn on.
pub fn parse_task_move(args: &Value) -> Result<TaskUpdateInput, String> {
    parse_update_like(args, TaskMoveTool::NAME, true)
}

fn parse_update_like(
    args: &Value,
    tool: &str,
    status_required: bool,
) -> Result<TaskUpdateInput, String> {
    let item_id = args
        .get("item_id")
        .or_else(|| args.get("id"))
        .or_else(|| args.get("task_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{tool} requires the card's `item_id`"))?
        .to_string();
    let status = optional_text(args, "status").or_else(|| optional_text(args, "column"));
    if status_required && status.is_none() {
        return Err(format!("{tool} requires a destination `status` column"));
    }
    Ok(TaskUpdateInput {
        item_id,
        status,
        assignee: optional_text(args, "assignee"),
        ordinal: args.get("ordinal").and_then(Value::as_i64),
        title: optional_text(args, "title"),
        description: optional_text(args, "description"),
    })
}

/// The parsed arguments of a `task.list` call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TaskListInput {
    /// Show only one column, when given.
    pub status: Option<String>,
}

/// Parse `task.list` arguments. Everything is optional: a bare call reads the
/// whole board.
#[must_use]
pub fn parse_task_list(args: &Value) -> TaskListInput {
    TaskListInput {
        status: optional_text(args, "status").or_else(|| optional_text(args, "column")),
    }
}

fn required_text(args: &Value, key: &str, tool: &str) -> Result<String, String> {
    optional_text(args, key).ok_or_else(|| format!("{tool} requires a non-empty `{key}`"))
}

/// A trimmed, non-empty string field, or `None`. A model that fills an optional
/// field with `""` means "not set", not "set to empty".
fn optional_text(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn create_needs_only_a_title_and_builds_a_stable_payload() {
        let input = parse_task_create(&json!({ "title": "  wire the DAG viewer  " }))
            .expect("a title is enough");
        assert_eq!(input.title, "wire the DAG viewer");
        assert_eq!(input.status, None);
        assert_eq!(input.payload(), json!({ "title": "wire the DAG viewer" }));

        let full = parse_task_create(&json!({
            "title": "wire the DAG viewer",
            "description": "edges on the wire",
            "status": "doing",
            "assignee": "dana",
        }))
        .expect("full card");
        assert_eq!(full.status.as_deref(), Some("doing"));
        assert_eq!(full.assignee.as_deref(), Some("dana"));
        assert_eq!(
            full.payload(),
            json!({ "title": "wire the DAG viewer", "description": "edges on the wire" })
        );
    }

    #[test]
    fn create_refuses_a_missing_or_oversized_title() {
        assert!(parse_task_create(&json!({ "description": "no title" })).is_err());
        assert!(parse_task_create(&json!({ "title": "   " })).is_err());
        let long = "x".repeat(MAX_TITLE_LEN + 1);
        let err = parse_task_create(&json!({ "title": long })).unwrap_err();
        assert!(err.contains("description"), "{err}");
    }

    #[test]
    fn a_pure_move_carries_the_card_body_forward() {
        // The bug this guards: an update that changes only the column must NOT
        // synthesize an empty payload and blank the card's text.
        let moved =
            parse_task_move(&json!({ "item_id": "0192-card", "status": "doing" })).expect("a move");
        assert_eq!(moved.status.as_deref(), Some("doing"));
        assert_eq!(moved.payload(), None);
        assert_eq!(moved.summary("move"), "move 0192-card to doing");
    }

    #[test]
    fn a_move_without_a_destination_is_refused() {
        let err = parse_task_move(&json!({ "item_id": "0192-card" })).unwrap_err();
        assert!(err.contains("status"), "{err}");
        // …but the same arguments are a legal (if inert) update.
        assert!(parse_task_update(&json!({ "item_id": "0192-card" })).is_ok());
    }

    #[test]
    fn the_card_id_is_accepted_under_the_names_a_model_reaches_for() {
        for key in ["item_id", "id", "task_id"] {
            let parsed = parse_task_update(&json!({ key: "0192-card", "assignee": "dana" }))
                .unwrap_or_else(|e| panic!("{key}: {e}"));
            assert_eq!(parsed.item_id, "0192-card");
        }
        assert!(parse_task_update(&json!({ "status": "done" })).is_err());
    }

    #[test]
    fn an_edit_replaces_only_the_fields_it_names() {
        let edited = parse_task_update(&json!({ "id": "c1", "title": "renamed" })).expect("edit");
        assert_eq!(edited.payload(), Some(json!({ "title": "renamed" })));
        assert_eq!(parse_task_list(&json!({})).status, None);
        assert_eq!(
            parse_task_list(&json!({ "column": "review" }))
                .status
                .as_deref(),
            Some("review")
        );
    }
}
