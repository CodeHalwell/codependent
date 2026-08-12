//! `docs.create` / `docs.read` / `docs.edit` / `docs.suggest` — the four tools an
//! agent authors knowledge-fabric documentation through (rubric #4, doc writer).
//!
//! Before these existed the runtime had no document tool at all: the mutator's
//! own comment said "an agent authors through the runtime, not this path", but
//! that runtime path was never built, so `DocumentAuthor::Agent` was never
//! constructed in production.
//!
//! Like `blackboard.*` and `memory.remember`, a document access needs no
//! path/command scope: it targets only the knowledge fabric's document store —
//! never the filesystem, a command, the network, or a remote — so its
//! [`ProposedAction`] is policy-allowed unconditionally and never reaches the
//! approval gate. It is recorded purely so the access is traced. The REAL gate
//! on an agent's document writes is the document's collaboration mode
//! (organization scope defaults to `Suggest`, so an agent edit lands as a
//! reviewable suggestion), enforced behind the
//! [`DocsChannel`](crate::docs::DocsChannel) seam. Publishing to Git is not
//! reachable from any of these: it stays behind the separate approval-gated
//! `PublishDocument` pipeline.
//!
//! This module owns the tool identities, argument shapes, and proposed actions;
//! the agent-loop layer owns author attribution (built server-side from the run
//! context, never from model-supplied identity).

use codypendent_protocol::ProposedAction;
use serde_json::Value;

/// The `docs.create` tool: draft a new document.
pub struct DocsCreateTool;

/// The `docs.read` tool: read a document as Markdown, or list documents.
pub struct DocsReadTool;

/// The `docs.edit` tool: replace a block's text (mode-gated).
pub struct DocsEditTool;

/// The `docs.suggest` tool: propose a range replacement for human review.
pub struct DocsSuggestTool;

impl DocsCreateTool {
    pub const NAME: &'static str = "docs.create";
}
impl DocsReadTool {
    pub const NAME: &'static str = "docs.read";
}
impl DocsEditTool {
    pub const NAME: &'static str = "docs.edit";
}
impl DocsSuggestTool {
    pub const NAME: &'static str = "docs.suggest";
}

/// The action policy evaluates for a `docs.*` call: a knowledge-fabric document
/// access, traced but never approval-gated (see the module docs). `document_id`
/// is empty for `docs.create` (no document exists yet) and for a `docs.read`
/// listing.
#[must_use]
pub fn docs_proposed_action(document_id: &str, summary: String) -> ProposedAction {
    ProposedAction::DocumentEdit {
        document_id: document_id.to_string(),
        summary,
    }
}

/// The parsed arguments of a `docs.create` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsCreateInput {
    pub title: String,
    pub scope: Option<String>,
    pub markdown: Option<String>,
}

/// The parsed arguments of a `docs.read` call. An absent `document_id` lists the
/// documents this run's repository can see.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocsReadInput {
    pub document_id: Option<String>,
}

/// The parsed arguments of a `docs.edit` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsEditInput {
    pub document_id: String,
    pub block_id: String,
    pub text: String,
}

/// The parsed arguments of a `docs.suggest` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsSuggestInput {
    pub document_id: String,
    pub block_id: String,
    pub range_start: u32,
    pub range_end: u32,
    pub replacement: String,
    pub rationale: Option<String>,
}

/// Parse `docs.create` arguments. `title` is required and non-blank; `scope` and
/// `markdown` are optional (an explicit JSON `null` reads as absent).
pub fn parse_docs_create(args: &Value) -> Result<DocsCreateInput, String> {
    Ok(DocsCreateInput {
        title: required_str(args, "title", DocsCreateTool::NAME)?,
        scope: optional_str(args, "scope"),
        markdown: optional_str(args, "markdown"),
    })
}

/// Parse `docs.read` arguments. Everything is optional: no arguments lists the
/// visible documents.
#[must_use]
pub fn parse_docs_read(args: &Value) -> DocsReadInput {
    DocsReadInput {
        document_id: optional_str(args, "document_id"),
    }
}

/// Parse `docs.edit` arguments. All three are required; `text` may be empty
/// (clearing a block's text is a legitimate edit), unlike `document_id` and
/// `block_id`.
pub fn parse_docs_edit(args: &Value) -> Result<DocsEditInput, String> {
    Ok(DocsEditInput {
        document_id: required_str(args, "document_id", DocsEditTool::NAME)?,
        block_id: required_str(args, "block_id", DocsEditTool::NAME)?,
        text: args
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{} requires a string `text`", DocsEditTool::NAME))?
            .to_string(),
    })
}

/// Parse `docs.suggest` arguments. `range_start`/`range_end` default to `0` (an
/// insertion at the start of the block) so a model that only knows what it wants
/// to add still produces a well-formed suggestion; an inverted range is refused
/// here rather than reaching the store.
pub fn parse_docs_suggest(args: &Value) -> Result<DocsSuggestInput, String> {
    let range_start = optional_u32(args, "range_start");
    let range_end = optional_u32(args, "range_end").max(range_start);
    if args
        .get("range_end")
        .and_then(Value::as_u64)
        .is_some_and(|e| (e as u32) < range_start)
    {
        return Err(format!(
            "{} requires `range_end` >= `range_start`",
            DocsSuggestTool::NAME
        ));
    }
    Ok(DocsSuggestInput {
        document_id: required_str(args, "document_id", DocsSuggestTool::NAME)?,
        block_id: required_str(args, "block_id", DocsSuggestTool::NAME)?,
        range_start,
        range_end,
        replacement: args
            .get("replacement")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{} requires a string `replacement`", DocsSuggestTool::NAME))?
            .to_string(),
        rationale: optional_str(args, "rationale"),
    })
}

fn required_str(args: &Value, key: &str, tool: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{tool} requires a non-empty string `{key}`"))
        .map(str::to_string)
}

/// An optional string argument: absent, JSON `null`, or blank all read as `None`.
fn optional_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

fn optional_u32(args: &Value, key: &str) -> u32 {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|n| n.min(u64::from(u32::MAX)) as u32)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn create_requires_a_non_blank_title() {
        assert!(parse_docs_create(&json!({})).is_err());
        assert!(parse_docs_create(&json!({"title": "   "})).is_err());
        let input = parse_docs_create(&json!({"title": " Runbook "})).expect("parses");
        assert_eq!(input.title, "Runbook");
        assert_eq!(input.scope, None);
        assert_eq!(input.markdown, None);
    }

    #[test]
    fn create_carries_optional_scope_and_markdown() {
        let input = parse_docs_create(&json!({
            "title": "Runbook",
            "scope": "system",
            "markdown": "# Runbook\n\nBody.\n",
        }))
        .expect("parses");
        assert_eq!(input.scope.as_deref(), Some("system"));
        assert_eq!(input.markdown.as_deref(), Some("# Runbook\n\nBody.\n"));
        // An explicit null (and a blank string) read as absent.
        let null = parse_docs_create(&json!({"title": "R", "scope": null, "markdown": "  "}))
            .expect("parses");
        assert_eq!(null.scope, None);
        assert_eq!(null.markdown, None);
    }

    #[test]
    fn read_takes_no_required_arguments() {
        assert_eq!(parse_docs_read(&json!({})).document_id, None);
        assert_eq!(
            parse_docs_read(&json!({"document_id": "doc-1"})).document_id,
            Some("doc-1".to_string())
        );
    }

    #[test]
    fn edit_requires_ids_but_allows_empty_text() {
        assert!(parse_docs_edit(&json!({"block_id": "b", "text": "x"})).is_err());
        assert!(parse_docs_edit(&json!({"document_id": "d", "text": "x"})).is_err());
        assert!(parse_docs_edit(&json!({"document_id": "d", "block_id": "b"})).is_err());
        // Clearing a block is a legitimate edit.
        let cleared =
            parse_docs_edit(&json!({"document_id": "d", "block_id": "b", "text": ""})).expect("ok");
        assert_eq!(cleared.text, "");
    }

    #[test]
    fn suggest_defaults_to_an_insertion_and_refuses_an_inverted_range() {
        let insertion = parse_docs_suggest(&json!({
            "document_id": "d",
            "block_id": "b",
            "replacement": "note",
        }))
        .expect("parses");
        assert_eq!((insertion.range_start, insertion.range_end), (0, 0));
        assert_eq!(insertion.rationale, None);

        let ranged = parse_docs_suggest(&json!({
            "document_id": "d",
            "block_id": "b",
            "range_start": 3,
            "range_end": 9,
            "replacement": "fixed",
            "rationale": "the signature changed",
        }))
        .expect("parses");
        assert_eq!((ranged.range_start, ranged.range_end), (3, 9));
        assert_eq!(ranged.rationale.as_deref(), Some("the signature changed"));

        assert!(parse_docs_suggest(&json!({
            "document_id": "d",
            "block_id": "b",
            "range_start": 9,
            "range_end": 3,
            "replacement": "x",
        }))
        .is_err());
    }

    #[test]
    fn the_proposed_action_names_the_document_and_is_never_a_filesystem_effect() {
        match docs_proposed_action("doc-1", "docs.edit block p".to_string()) {
            ProposedAction::DocumentEdit {
                document_id,
                summary,
            } => {
                assert_eq!(document_id, "doc-1");
                assert_eq!(summary, "docs.edit block p");
            }
            other => panic!("expected DocumentEdit, got {other:?}"),
        }
    }
}
