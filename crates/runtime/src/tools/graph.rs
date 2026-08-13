//! `graph.callers_of` / `graph.blast_radius` / `graph.tests_covering` — the
//! agent's window onto the repository's code graph (outcome 5).
//!
//! The graph has always been parsed, stored, and queryable; until now the model's
//! only exposure to it was the static `RepositoryMap` text folded once into a
//! run's opening note. It could not ask "who calls this", "what breaks if I
//! change this", or "which tests cover this file" — the three questions the store
//! was built to answer (2026-08-13 review, F3).
//!
//! Like `skills.search` and the MCP bridge, this module holds only argument
//! parsing: the queries, their bounds, and their rendering live in
//! `codypendent_knowledge::codegraph`, and the seam
//! ([`CodeGraphQueries`](codypendent_knowledge::CodeGraphQueries)) is implemented
//! in the daemon assembly, which owns the pool. Without an injected
//! implementation the tools are never offered.
//!
//! Every result is first-party: node names and paths this daemon parsed out of
//! the repository itself. There is no untrusted content to sanitize here, unlike
//! the MCP/web/skill-document arms — a symbol name is bounded by the parser, and
//! `GraphAnswer::render` caps the disclosed rows.

use std::path::Path;

use codypendent_knowledge::GraphQuestion;
use codypendent_protocol::ProposedAction;
use serde_json::Value;

/// Default traversal depth when a call omits it. Two layers is the useful answer
/// for "what breaks if I change this" without turning every call into a
/// whole-repository dump; the ceiling is enforced downstream.
pub const DEFAULT_DEPTH: usize = 2;

/// `graph.callers_of` — the direct callers of a symbol.
pub struct GraphCallersOf;
/// `graph.blast_radius` — everything that transitively reaches a symbol.
pub struct GraphBlastRadius;
/// `graph.tests_covering` — the tests that exercise a file.
pub struct GraphTestsCovering;

impl GraphCallersOf {
    /// The stable dotted tool name.
    pub const NAME: &'static str = "graph.callers_of";
}
impl GraphBlastRadius {
    /// The stable dotted tool name.
    pub const NAME: &'static str = "graph.blast_radius";
}
impl GraphTestsCovering {
    /// The stable dotted tool name.
    pub const NAME: &'static str = "graph.tests_covering";
}

/// The action policy evaluates for any `graph.*` call: a READ of the daemon's
/// own derived code graph — no filesystem, command, network, or remote effect,
/// and no model-supplied path ever reaches the filesystem (the argument is
/// matched against `code_nodes.source_path` rows the daemon itself wrote).
/// Always policy-`Allow`ed, exactly like `SearchRegistry`, and recorded only so
/// the access is traced.
#[must_use]
pub fn graph_proposed_action(repository: &Path, summary: String) -> ProposedAction {
    ProposedAction::CodeGraphQuery {
        repository: repository.display().to_string(),
        summary,
    }
}

/// Parse a `graph.callers_of` / `graph.blast_radius` call: `symbol` is required
/// and non-blank, `depth` optional (clamped downstream, never rejected here — a
/// model that asks for depth 50 gets the ceiling, not a failed call).
pub fn parse_symbol_question(args: &Value, tool: &str) -> Result<(String, usize), String> {
    let symbol = args
        .get("symbol")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{tool} requires a non-empty string `symbol`"))?
        .to_string();
    Ok((symbol, parse_depth(args)))
}

/// Parse a `graph.tests_covering` call: `path` is required and non-blank.
pub fn parse_tests_covering(args: &Value) -> Result<(String, usize), String> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("graph.tests_covering requires a non-empty string `path`")?
        .to_string();
    Ok((path, parse_depth(args)))
}

fn parse_depth(args: &Value) -> usize {
    args.get("depth")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_DEPTH, |depth| {
            usize::try_from(depth).unwrap_or(DEFAULT_DEPTH)
        })
}

/// A one-line summary of a question, for the recorded [`ProposedAction`].
#[must_use]
pub fn summarize_graph_question(question: &GraphQuestion) -> String {
    match question {
        GraphQuestion::CallersOf { symbol } => format!("callers of {symbol}"),
        GraphQuestion::BlastRadius { symbol, depth } => {
            format!("blast radius of {symbol} (depth {depth})")
        }
        GraphQuestion::TestsCovering { path, depth } => {
            format!("tests covering {path} (depth {depth})")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_requires_a_non_empty_symbol() {
        assert!(parse_symbol_question(&json!({}), GraphCallersOf::NAME).is_err());
        assert!(parse_symbol_question(&json!({"symbol": "  "}), GraphCallersOf::NAME).is_err());
    }

    #[test]
    fn depth_defaults_and_is_carried_through() {
        let (symbol, depth) =
            parse_symbol_question(&json!({"symbol": "Router::decide"}), GraphBlastRadius::NAME)
                .expect("parses");
        assert_eq!(symbol, "Router::decide");
        assert_eq!(depth, DEFAULT_DEPTH);

        let (_, deep) =
            parse_symbol_question(&json!({"symbol": "x", "depth": 4}), GraphBlastRadius::NAME)
                .expect("parses");
        assert_eq!(deep, 4);
    }

    /// An absurd depth is clamped downstream, not refused here: the model asked a
    /// legitimate question badly, and a failed call teaches it nothing.
    #[test]
    fn an_unparseable_depth_falls_back_to_the_default() {
        let (_, depth) = parse_symbol_question(
            &json!({"symbol": "x", "depth": u64::MAX}),
            GraphBlastRadius::NAME,
        )
        .expect("parses");
        assert!(depth >= DEFAULT_DEPTH);
    }

    #[test]
    fn tests_covering_requires_a_path() {
        assert!(parse_tests_covering(&json!({})).is_err());
        let (path, _) = parse_tests_covering(&json!({"path": "src/router.rs"})).expect("parses");
        assert_eq!(path, "src/router.rs");
    }

    #[test]
    fn the_summary_names_the_subject_and_the_depth() {
        assert_eq!(
            summarize_graph_question(&GraphQuestion::CallersOf {
                symbol: "decide".to_string()
            }),
            "callers of decide"
        );
        assert_eq!(
            summarize_graph_question(&GraphQuestion::BlastRadius {
                symbol: "decide".to_string(),
                depth: 3
            }),
            "blast radius of decide (depth 3)"
        );
    }
}
