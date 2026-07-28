//! `memory.remember` — the core tool an agent calls to save a durable fact in
//! its own words (M2, the smarter-memory plan).
//!
//! Unlike the workflow-gated tools (`blackboard.*`, `github.*`), this one is
//! CORE: it is offered to every run, workflow or solo, because remembering a
//! fact for future runs is useful regardless of what kind of run this is.
//!
//! The tool's entire side effect is appending one `NoteAppended` event to the
//! run's own ledger — the same seam the dormant `memory.propose:` marker in
//! [`explicit_proposal_candidates`](codypendent_knowledge::observer) already
//! watches for. No new harvest wiring is needed: the observer turns the note
//! into a `Semantic` candidate, which then flows through `MemoryStore::curate`
//! (redaction, scope, dedup, contradiction, provenance, retention) exactly
//! like every other candidate. The optional `value` argument reaches the
//! candidate's `structured_value` via a single ASCII Record Separator
//! (`\u{1e}`) delimiting the note text — see the observer's parse extension.

use codypendent_protocol::ProposedAction;
use serde_json::Value;

/// The `memory.remember` tool: save a discrete fact, decision, or learning to
/// long-term memory in the agent's own words.
pub struct MemoryRemember;

impl MemoryRemember {
    /// The stable dotted tool name.
    pub const NAME: &'static str = "memory.remember";

    /// ASCII Record Separator delimiting the optional structured value tail in
    /// the emitted `NoteAppended` text.
    pub const RECORD_SEPARATOR: char = '\u{1e}';

    /// The action policy evaluates: recording a memory proposal note on the
    /// run's own ledger — no filesystem/command/network/remote effect. Always
    /// policy-Allowed (see the daemon policy engine's explicit `RecordMemory`
    /// arm).
    #[must_use]
    pub fn proposed_action() -> ProposedAction {
        ProposedAction::RecordMemory
    }

    /// The `NoteAppended` text this call should emit: `memory.propose:
    /// <statement>` when no value is given, or `memory.propose:
    /// <statement>\u{1e}<compact-json-value>` when one is. `Display` on
    /// `serde_json::Value` renders compact JSON, so the observer's
    /// `serde_json::from_str` round-trips it exactly.
    #[must_use]
    pub fn note_text(input: &MemoryRememberInput) -> String {
        match &input.value {
            None => format!("memory.propose: {}", input.statement),
            Some(value) => format!(
                "memory.propose: {}{}{}",
                input.statement,
                Self::RECORD_SEPARATOR,
                value
            ),
        }
    }
}

/// The parsed, model-supplied arguments of a `memory.remember` call.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryRememberInput {
    /// The fact, decision, or learning, in the model's own words — a short,
    /// standalone, one-line statement.
    pub statement: String,
    /// An optional structured value accompanying the statement (e.g.
    /// `{"engine": "postgres"}`). Never itself rendered by retrieval; the
    /// statement alone must stand on its own.
    pub value: Option<Value>,
}

/// Parse `memory.remember` arguments. `statement` is required and must be
/// non-empty (after trimming); `value` is optional, and an explicit JSON
/// `null` is treated the same as an absent field.
pub fn parse_memory_remember(args: &Value) -> Result<MemoryRememberInput, String> {
    let statement = args
        .get("statement")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("memory.remember requires a non-empty string `statement`")?
        .to_string();
    let value = match args.get("value") {
        None | Some(Value::Null) => None,
        Some(other) => Some(other.clone()),
    };
    Ok(MemoryRememberInput { statement, value })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_requires_non_empty_statement() {
        assert!(parse_memory_remember(&json!({})).is_err());
        assert!(parse_memory_remember(&json!({"statement": ""})).is_err());
        assert!(parse_memory_remember(&json!({"statement": "   "})).is_err());
    }

    #[test]
    fn parse_value_optional_and_null_treated_as_absent() {
        let no_value = parse_memory_remember(&json!({"statement": "use ripgrep"})).expect("parses");
        assert_eq!(no_value.statement, "use ripgrep");
        assert_eq!(no_value.value, None);

        let null_value = parse_memory_remember(&json!({"statement": "use ripgrep", "value": null}))
            .expect("parses");
        assert_eq!(null_value.value, None);

        let with_value = parse_memory_remember(&json!({
            "statement": "db is postgres",
            "value": {"engine": "postgres"},
        }))
        .expect("parses");
        assert_eq!(with_value.value, Some(json!({"engine": "postgres"})));
    }

    #[test]
    fn note_text_without_value_has_no_record_separator() {
        let input = MemoryRememberInput {
            statement: "use ripgrep".to_string(),
            value: None,
        };
        assert_eq!(
            MemoryRemember::note_text(&input),
            "memory.propose: use ripgrep"
        );
        assert!(!MemoryRemember::note_text(&input).contains(MemoryRemember::RECORD_SEPARATOR));
    }

    #[test]
    fn note_text_with_value_appends_record_separator_and_compact_json() {
        let input = MemoryRememberInput {
            statement: "db is postgres".to_string(),
            value: Some(json!({"engine": "postgres"})),
        };
        let text = MemoryRemember::note_text(&input);
        assert_eq!(
            text,
            "memory.propose: db is postgres\u{1e}{\"engine\":\"postgres\"}"
        );

        // Round-trips exactly as the observer's split-on-first-`\u{1e}` parses it.
        let marker = "memory.propose:";
        let rest = &text[marker.len()..];
        let (stmt, tail) = rest
            .split_once(MemoryRemember::RECORD_SEPARATOR)
            .expect("has tail");
        assert_eq!(stmt.trim(), "db is postgres");
        let parsed: Value = serde_json::from_str(tail.trim()).expect("valid json");
        assert_eq!(parsed, json!({"engine": "postgres"}));
    }
}
