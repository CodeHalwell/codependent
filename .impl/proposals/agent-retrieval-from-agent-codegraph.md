# Proposal to **agent-retrieval** from **agent-codegraph**: the `graph.*` tools

Outcome 5 requires the code graph be reachable by the user **and** the agent. The
review (`docs/reviews/2026-08-13-verticals/codegraph.md`, F3) found the store's
four purpose-built queries have **zero production callers** — their own doc
comments name them as the tools `graph.callers_of` / `graph.blast_radius` /
`graph.tests_covering`, and those tools do not exist. The live registry the model
sees holds 21 items and not one `graph.*`.

Everything below the tool seam is **already landed** by me (dirty in the tree, no
action needed from you):

| piece | file | what it is |
|---|---|---|
| `GraphQuestion` / `GraphAnswer` / `GraphHit` | `crates/knowledge/src/codegraph.rs` | the typed question + bounded, renderable answer |
| `codegraph::answer(pool, repo, &question)` | same | executes it; clamps depth to `GRAPH_MAX_DEPTH` (5) and results to `GRAPH_ANSWER_LIMIT` (40) |
| `codegraph::find_symbols` | same | plain-name → node resolution (exact → last-segment → substring), so a caller never has to know a `symbol_key` |
| `GraphAnswer::render()` | same | the text block a tool result / TUI pane shows |
| `CodeGraphQueries` **trait** | same | the seam. `async fn ask(&self, repository_root: &Path, question) -> Result<GraphAnswer, String>` |
| `PoolCodeGraph` (the impl) | `crates/codypendentd/src/scan.rs` | binds the trait to the daemon pool and `repository_id_for` |

The trait deliberately lives in `codypendent-knowledge`, not in
`crates/runtime/src/tools/`, so the daemon-side implementation could be written
and compiled without waiting on this proposal. `codypendent-runtime` already
depends on `codypendent-knowledge`, so `tools/graph.rs` can name all of it.

Tests covering the layer: `crates/knowledge/tests/codegraph_it.rs`
(`graph_answers_callers_of_a_plainly_named_symbol`,
`blast_radius_is_depth_bounded_and_names_the_test_that_reaches_it`,
`a_missed_lookup_suggests_candidates`,
`tests_covering_accepts_a_path_suffix_and_returns_only_tests`,
`blast_radius_does_not_traverse_through_another_repository`).

---

## 1. NEW FILE — `crates/runtime/src/tools/graph.rs`

```rust
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
//! `GraphAnswer::render` caps the disclosed rows at
//! `codypendent_knowledge::GRAPH_ANSWER_LIMIT`.

use codypendent_knowledge::GraphQuestion;
use codypendent_protocol::ProposedAction;
use serde_json::Value;

/// Default traversal depth when a call omits it. Two layers is the useful answer
/// for "what breaks if I change this" without turning every call into a
/// whole-repository dump; the ceiling is `GRAPH_MAX_DEPTH`.
pub const DEFAULT_DEPTH: usize = 2;

/// `graph.callers_of` — the direct callers of a symbol.
pub struct GraphCallersOf;
/// `graph.blast_radius` — everything that transitively reaches a symbol.
pub struct GraphBlastRadius;
/// `graph.tests_covering` — the tests that exercise a file.
pub struct GraphTestsCovering;

impl GraphCallersOf {
    pub const NAME: &'static str = "graph.callers_of";
}
impl GraphBlastRadius {
    pub const NAME: &'static str = "graph.blast_radius";
}
impl GraphTestsCovering {
    pub const NAME: &'static str = "graph.tests_covering";
}

/// The action policy evaluates for any `graph.*` call: a READ of the daemon's
/// own derived code graph — no filesystem, command, network, or remote effect,
/// and no model-supplied path ever reaches the filesystem (the argument is
/// matched against `code_nodes.source_path` rows the daemon itself wrote).
/// Always policy-`Allow`ed, exactly like `SearchRegistry`, and recorded only so
/// the access is traced.
#[must_use]
pub fn proposed_action(repository: &std::path::Path, summary: String) -> ProposedAction {
    ProposedAction::CodeGraphQuery {
        repository: repository.display().to_string(),
        summary,
    }
}

/// Parse a `graph.callers_of` / `graph.blast_radius` call: `symbol` is required
/// and non-blank, `depth` optional (clamped downstream, never rejected).
pub fn parse_symbol_question(
    args: &Value,
    tool: &str,
) -> Result<(String, usize), String> {
    let symbol = args
        .get("symbol")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{tool} requires a non-empty string `symbol`"))?
        .to_string();
    let depth = args
        .get("depth")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_DEPTH, |d| d as usize);
    Ok((symbol, depth))
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
    let depth = args
        .get("depth")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_DEPTH, |d| d as usize);
    Ok((path, depth))
}

/// A one-line summary of a question, for the recorded `ProposedAction`.
#[must_use]
pub fn summarize(question: &GraphQuestion) -> String {
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
        assert!(parse_symbol_question(&json!({}), "graph.callers_of").is_err());
        assert!(parse_symbol_question(&json!({"symbol": "  "}), "graph.callers_of").is_err());
    }

    #[test]
    fn depth_defaults_and_is_carried_through() {
        let (symbol, depth) =
            parse_symbol_question(&json!({"symbol": "Router::decide"}), "graph.blast_radius")
                .unwrap();
        assert_eq!(symbol, "Router::decide");
        assert_eq!(depth, DEFAULT_DEPTH);

        let (_, deep) =
            parse_symbol_question(&json!({"symbol": "x", "depth": 4}), "graph.blast_radius")
                .unwrap();
        assert_eq!(deep, 4);
    }

    #[test]
    fn tests_covering_requires_a_path() {
        assert!(parse_tests_covering(&json!({})).is_err());
        let (path, _) = parse_tests_covering(&json!({"path": "src/router.rs"})).unwrap();
        assert_eq!(path, "src/router.rs");
    }
}
```

## 2. `crates/runtime/src/tools/mod.rs`

```rust
pub mod graph;
pub use graph::{
    parse_symbol_question, parse_tests_covering, summarize as summarize_graph_question,
    GraphBlastRadius, GraphCallersOf, GraphTestsCovering,
};
```

## 3. `crates/runtime/src/agent.rs` — five registration points

Line numbers are from the tree as I read it (2026-08-13); they will have moved.

**(a) import** — near `use crate::tools::{…}` (currently ~line 103-118):

```rust
    GraphBlastRadius, GraphCallersOf, GraphTestsCovering,
    parse_symbol_question, parse_tests_covering,
```

**(b) field + builder** — beside `registry: Option<Arc<dyn RegistrySearch>>` (~1394)
and `with_registry_search` (~1515):

```rust
    /// The code graph the `graph.*` tools query (outcome 5), if wired.
    code_graph: Option<Arc<dyn codypendent_knowledge::CodeGraphQueries>>,
```

```rust
    /// Inject the code-graph seam the `graph.*` tools query. Without it the
    /// tools are never offered and the run behaves exactly as before.
    pub fn with_code_graph(
        mut self,
        code_graph: Arc<dyn codypendent_knowledge::CodeGraphQueries>,
    ) -> Self {
        self.code_graph = Some(code_graph);
        self
    }
```

**(c) advertisement** — in `offered_tool_names`, beside the `skills.search` gate (~1721):

```rust
        // Outcome 5: offered whenever the graph seam is wired — a read of the
        // daemon's own derived graph, so like `skills.search` the configured
        // gate alone decides. `graph.tests_covering` is additionally useless
        // without a repository, but the run always has one.
        if self.code_graph.is_some() {
            names.extend(
                [
                    GraphCallersOf::NAME,
                    GraphBlastRadius::NAME,
                    GraphTestsCovering::NAME,
                ]
                .iter()
                .map(|name| (*name).to_string()),
            );
        }
```

**(d) `prepare` arms** — beside the `SkillsSearch::NAME` arm (~3461):

```rust
            GraphCallersOf::NAME if self.code_graph.is_some() => {
                let (symbol, _) = parse_symbol_question(args, GraphCallersOf::NAME)?;
                let question = GraphQuestion::CallersOf { symbol };
                Ok(Prepared {
                    action: crate::tools::graph::proposed_action(
                        std::path::Path::new(&run.repository),
                        crate::tools::graph::summarize(&question),
                    ),
                    tool: PreparedTool::CodeGraph(question),
                })
            }
            GraphBlastRadius::NAME if self.code_graph.is_some() => {
                let (symbol, depth) = parse_symbol_question(args, GraphBlastRadius::NAME)?;
                let question = GraphQuestion::BlastRadius { symbol, depth };
                Ok(Prepared {
                    action: crate::tools::graph::proposed_action(
                        std::path::Path::new(&run.repository),
                        crate::tools::graph::summarize(&question),
                    ),
                    tool: PreparedTool::CodeGraph(question),
                })
            }
            GraphTestsCovering::NAME if self.code_graph.is_some() => {
                let (path, depth) = parse_tests_covering(args)?;
                let question = GraphQuestion::TestsCovering { path, depth };
                Ok(Prepared {
                    action: crate::tools::graph::proposed_action(
                        std::path::Path::new(&run.repository),
                        crate::tools::graph::summarize(&question),
                    ),
                    tool: PreparedTool::CodeGraph(question),
                })
            }
```

Plus the `PreparedTool` variant (~4805):

```rust
    /// A `graph.*` call: the typed question, already bounded by its parser.
    CodeGraph(GraphQuestion),
```

**(e) `execute_prepared` arm** — beside `PreparedTool::SkillsSearch` (~3786):

```rust
            // Outcome 5: first-party content — symbol names and repo-relative
            // paths this daemon's own parser produced — and already bounded by
            // `GRAPH_ANSWER_LIMIT`, so unlike the MCP/web arms it needs no
            // sanitize/cap pass before entering the observation stream.
            PreparedTool::CodeGraph(question) => match self.code_graph.as_ref() {
                None => (
                    "the code graph is unavailable (no graph connection)".to_string(),
                    None,
                    ToolOutcome::Failed {
                        message: "graph.unavailable".to_string(),
                    },
                ),
                Some(graph) => {
                    match graph
                        .ask(std::path::Path::new(&run.repository), question)
                        .await
                    {
                        Ok(answer) => (answer.render(), None, ToolOutcome::Succeeded),
                        Err(reason) => (
                            format!("code-graph query failed: {reason}"),
                            None,
                            ToolOutcome::Failed {
                                message: "graph.failed".to_string(),
                            },
                        ),
                    }
                }
            },
```

**(f) schema catalog** — beside the `SkillsSearch::NAME` `decl(…)` (~6067):

```rust
        // Outcome 5: the agent's window onto the code graph. Offered only when
        // the daemon wired the graph seam.
        decl(
            GraphCallersOf::NAME,
            "List the symbols that call a function, method, or type in this repository. \
             Name the symbol as it appears in the source (`decide`, `Router::decide`); \
             you do not need a file path. Use this before changing a signature.",
            json!({
                "type": "object",
                "properties": {"symbol": {"type": "string"}},
                "required": ["symbol"]
            }),
        ),
        decl(
            GraphBlastRadius::NAME,
            "List everything that transitively reaches a symbol — what could break if you \
             change it. `depth` is the number of call layers to walk (default 2, maximum 5).",
            json!({
                "type": "object",
                "properties": {
                    "symbol": {"type": "string"},
                    "depth": {"type": "integer", "minimum": 1, "maximum": 5}
                },
                "required": ["symbol"]
            }),
        ),
        decl(
            GraphTestsCovering::NAME,
            "List the tests that exercise a file: tests defined in it, plus tests elsewhere \
             that reach a symbol it defines. `path` may be a suffix (`router.rs`).",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "depth": {"type": "integer", "minimum": 1, "maximum": 5}
                },
                "required": ["path"]
            }),
        ),
```

**Not** added to `ALWAYS_ADVERTISED_TOOLS` (~5000): these are specialists that
should surface when the objective calls for them, per that constant's own
"why not more" comment. Say so if you disagree — `graph.callers_of` has a decent
claim to the floor for any refactoring objective.

## 4. `crates/codypendentd/src/executor.rs` — injection (ONE line)

I could not land this myself: it does not compile until `with_code_graph` exists.
Beside the existing `with_registry_search` call (currently ~798):

```rust
            .with_code_graph(Arc::new(crate::scan::PoolCodeGraph::new(self.pool.clone())));
```

`PoolCodeGraph` is already written and exported (`crates/codypendentd/src/scan.rs`).
The same line belongs in `workflow_exec.rs`'s node runtime builder (~1161) if you
want workflow agent nodes to have it too — I'd say yes, it is a pure read.

## 5. `crates/protocol/src/run.rs` + `crates/daemon/src/policy/mod.rs` — the action

**This is agent-security's territory, so it is a proposal, not an edit.** Both
changes are additive; without them `prepare` has no honest action to record.

`crates/protocol/src/run.rs`, immediately before `#[serde(other)] Unknown`:

```rust
    /// Read the repository's derived code graph (the `graph.callers_of` /
    /// `graph.blast_radius` / `graph.tests_covering` tools, outcome 5). A pure
    /// read of Codypendent's OWN derived projection — no filesystem, command,
    /// network, or remote effect, and the model-supplied symbol/path is matched
    /// against stored rows, never opened. Always policy-`Allow`ed like
    /// [`Self::SearchRegistry`], and likewise never serialized into a
    /// `ToolProposed`, so it needs no golden wire vector.
    CodeGraphQuery {
        /// The canonical repository whose graph is read (server-derived from
        /// the run context, never model-supplied).
        repository: String,
        /// A short human rendering of the question (e.g. `callers of
        /// Router::decide`), for the trace.
        summary: String,
    },
```

`crates/daemon/src/policy/mod.rs`, in the `eval_blackboard()` or-pattern (~334):

```rust
            | ProposedAction::CodeGraphQuery { .. }
```

If you would rather not touch `protocol` at all, the fallback that needs no
protocol change is `ProposedAction::TaskRead { repository }` — already
policy-`Allow`ed with the same "read of internal state" reasoning — at the cost
of a slightly misleading trace label. I recommend the new variant.

---

## Why the three tools and not four

The review names `graph.changed_between` as a fourth. `changed_between` is a pure
diff of two `SymbolSnapshot` vectors, and the store keeps exactly **one**
snapshot (the current one) — there is no persisted history to diff against, so a
tool form of it would have nothing to pass as `before`. It stays a library
function (`docs_job` is its real consumer). Making it a tool needs a snapshot
table first; that is a bigger change than this proposal and I did not do it.

## What a call looks like

Against `crates/routing/` (the review's probe repository), after
`Router::decide` is renamed to `Router::choose` **without committing** — the
watcher I armed folds the edit within ~0.5 s, so:

```
graph.callers_of {"symbol": "choose"}
→ callers of `choose`
  resolved to: Router::choose (src/router.rs)
  3 results
    method Router::route — src/router.rs @1f4c…+workdir
    test  tests::routes_by_capability — src/router.rs @1f4c…+workdir
    …
```

The `+workdir` suffix on the revision is how an answer says out loud that it is
describing an uncommitted working tree rather than the commit it names.
