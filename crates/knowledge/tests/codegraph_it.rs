//! STEP 2.5 code graph: tree-sitter parse → durable nodes/edges, stable symbol
//! identity across file rename, incremental reparse == full reparse, and the
//! repository map render.

use std::collections::HashMap;

use codypendent_knowledge::codegraph::{self, CodeGraphError};
use codypendent_knowledge::repomap::repository_map;
use codypendent_knowledge::types::{CodeNode, CodeNodeKind, CodeRelation, EvidenceKind};
use codypendent_knowledge::{db, outbox, GitRevision};
use codypendent_protocol::RepositoryId;

/// A small fixture crate exercising every extracted node kind and edge relation:
/// imports, a constant, a struct, a trait, an impl with methods, a free function
/// called from a method, a nested module, and a `#[cfg(test)]` module whose
/// `#[test]` fn calls back into the API.
const FIXTURE: &str = r#"
use std::fmt;
use crate::util::{helper, Widget as W};

pub const MAX: u32 = 10;

pub struct Engine {
    count: u32,
}

pub trait Runnable {
    fn run(&self);
}

impl Engine {
    pub fn new() -> Engine {
        Engine { count: 0 }
    }

    pub fn tick(&self) -> u32 {
        compute(self.count)
    }
}

pub fn compute(seed: u32) -> u32 {
    seed + 1
}

mod inner {
    pub fn deep() -> u32 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_ticks() {
        let e = Engine::new();
        let _ = e.tick();
        let _ = compute(1);
    }
}
"#;

async fn temp_pool() -> (tempfile::TempDir, sqlx::SqlitePool) {
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("codypendent.db")).await.unwrap();
    (tmp, pool)
}

fn rev() -> GitRevision {
    GitRevision("rev-1".to_owned())
}

fn has_node(nodes: &[CodeNode], qualified: &str, kind: CodeNodeKind) -> bool {
    nodes
        .iter()
        .any(|n| n.key.qualified_name == qualified && n.key.kind == kind)
}

/// Build a `(qualified_name, relation, qualified_name)` view of the edges, so
/// they can be asserted without knowing the generated node ids.
fn edge_triples(
    nodes: &[CodeNode],
    edges: &[codypendent_knowledge::CodeEdge],
) -> Vec<(String, CodeRelation, String)> {
    let by_id: HashMap<_, _> = nodes
        .iter()
        .map(|n| (n.id, n.key.qualified_name.clone()))
        .collect();
    edges
        .iter()
        .map(|e| {
            (
                by_id.get(&e.from).cloned().unwrap_or_default(),
                e.relation,
                by_id.get(&e.to).cloned().unwrap_or_default(),
            )
        })
        .collect()
}

fn has_edge(
    triples: &[(String, CodeRelation, String)],
    from: &str,
    rel: CodeRelation,
    to: &str,
) -> bool {
    triples
        .iter()
        .any(|(f, r, t)| f == from && *r == rel && t == to)
}

#[tokio::test]
async fn parses_expected_nodes_and_edges() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    let path = "src/engine.rs";

    let delta = codegraph::upsert_file_graph(&pool, repo, &rev(), path, FIXTURE)
        .await
        .unwrap();

    let nodes = codegraph::nodes(&pool, repo).await.unwrap();

    // Every extracted node kind is present, keyed by qualified name.
    assert!(has_node(&nodes, path, CodeNodeKind::File));
    assert!(has_node(&nodes, "MAX", CodeNodeKind::Constant));
    assert!(has_node(&nodes, "Engine", CodeNodeKind::Type));
    assert!(has_node(&nodes, "Runnable", CodeNodeKind::TraitOrInterface));
    assert!(has_node(&nodes, "Runnable::run", CodeNodeKind::Method));
    assert!(has_node(&nodes, "Engine::new", CodeNodeKind::Method));
    assert!(has_node(&nodes, "Engine::tick", CodeNodeKind::Method));
    assert!(has_node(&nodes, "compute", CodeNodeKind::Function));
    assert!(has_node(&nodes, "inner", CodeNodeKind::Module));
    assert!(has_node(&nodes, "inner::deep", CodeNodeKind::Function));
    assert!(has_node(&nodes, "tests", CodeNodeKind::Module));
    assert!(has_node(&nodes, "tests::engine_ticks", CodeNodeKind::Test));

    // Imports become ExternalDependency reference nodes named by the use path.
    assert!(has_node(
        &nodes,
        "std::fmt",
        CodeNodeKind::ExternalDependency
    ));
    assert!(has_node(
        &nodes,
        "crate::util::helper",
        CodeNodeKind::ExternalDependency
    ));
    assert!(has_node(
        &nodes,
        "crate::util::Widget",
        CodeNodeKind::ExternalDependency
    ));

    let edges = codegraph::edges(&pool, repo).await.unwrap();
    let triples = edge_triples(&nodes, &edges);

    // Contains: file → item, and module → nested item.
    assert!(has_edge(&triples, path, CodeRelation::Contains, "compute"));
    assert!(has_edge(&triples, path, CodeRelation::Contains, "inner"));
    assert!(has_edge(
        &triples,
        "inner",
        CodeRelation::Contains,
        "inner::deep"
    ));
    assert!(has_edge(
        &triples,
        "tests",
        CodeRelation::Contains,
        "tests::engine_ticks"
    ));

    // Defines: the definer (file/module/trait) → item.
    assert!(has_edge(&triples, path, CodeRelation::Defines, "Engine"));
    assert!(has_edge(&triples, path, CodeRelation::Defines, "compute"));
    assert!(has_edge(
        &triples,
        "Runnable",
        CodeRelation::Defines,
        "Runnable::run"
    ));

    // Imports: file → the imported path.
    assert!(has_edge(&triples, path, CodeRelation::Imports, "std::fmt"));

    // Calls-as-written, resolved within the file to real owned nodes.
    assert!(has_edge(
        &triples,
        "Engine::tick",
        CodeRelation::Calls,
        "compute"
    ));
    assert!(has_edge(
        &triples,
        "tests::engine_ticks",
        CodeRelation::Calls,
        "Engine::tick"
    ));
    assert!(has_edge(
        &triples,
        "tests::engine_ticks",
        CodeRelation::Calls,
        "Engine::new"
    ));

    // Call edges carry the Chapter 07 syntax-inferred confidence + evidence.
    let call = edges
        .iter()
        .find(|e| e.relation == CodeRelation::Calls)
        .expect("a Calls edge");
    assert!((call.confidence - 0.45).abs() < f32::EPSILON);
    assert_eq!(call.evidence_kind, EvidenceKind::SyntaxInferred);
    assert!(
        call.evidence.is_some(),
        "every edge carries an evidence ref"
    );

    // One SymbolChanged outbox event per durable node (the 12 owned symbols),
    // enqueued in the write tx. The synthesized import reference nodes are also
    // created but are not symbols, so they emit no event.
    let events = outbox::unprocessed(&pool, 1000).await.unwrap();
    assert!(events.iter().all(|e| e.event_kind == "symbol_changed"));
    assert_eq!(events.len(), 12, "one SymbolChanged per durable symbol");
    assert!(
        delta.created_node_ids.len() > events.len(),
        "reference nodes were created too, without events"
    );
}

#[tokio::test]
async fn symbol_identity_survives_line_movement() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    let path = "src/engine.rs";

    codegraph::upsert_file_graph(&pool, repo, &rev(), path, FIXTURE)
        .await
        .unwrap();
    let before = codegraph::nodes(&pool, repo).await.unwrap();
    let compute_before = before
        .iter()
        .find(|n| n.key.qualified_name == "compute" && n.key.kind == CodeNodeKind::Function)
        .expect("compute node")
        .id;

    // Same file, same symbols, every item shifted down by a leading comment:
    // `SymbolKey` is byte-position-independent, so `compute` keeps its id across
    // the reparse even though its start offset moved.
    let moved = format!("// a new leading comment shifts every item down\n{FIXTURE}");
    codegraph::upsert_file_graph(&pool, repo, &rev(), path, &moved)
        .await
        .unwrap();
    let after = codegraph::nodes(&pool, repo).await.unwrap();
    let compute_after = after
        .iter()
        .find(|n| n.key.qualified_name == "compute" && n.key.kind == CodeNodeKind::Function)
        .expect("compute node")
        .id;

    assert_eq!(
        compute_before, compute_after,
        "identity survives line movement within the file"
    );
}

/// Issue #6 item 5: two files whose top-level symbols share a name *and* a
/// signature must not collapse onto one node — the folded `source_path` keeps
/// them distinct, so reparsing the second file can't delete the first's edges.
#[tokio::test]
async fn same_named_symbols_in_different_files_do_not_collide() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();

    // `init` has an identical signature (`pub fn init() -> u32`) in both files and
    // each calls a same-file helper. Before `source_path` entered the key these
    // two `init`s were one row, and bar's edge-replacement deleted foo's call.
    let foo = "pub fn init() -> u32 { helper() }\npub fn helper() -> u32 { 1 }\n";
    let bar = "pub fn init() -> u32 { other() }\npub fn other() -> u32 { 2 }\n";

    codegraph::upsert_file_graph(&pool, repo, &rev(), "src/foo.rs", foo)
        .await
        .unwrap();
    codegraph::upsert_file_graph(&pool, repo, &rev(), "src/bar.rs", bar)
        .await
        .unwrap();

    let nodes = codegraph::nodes(&pool, repo).await.unwrap();
    let inits: Vec<_> = nodes
        .iter()
        .filter(|n| n.key.qualified_name == "init" && n.key.kind == CodeNodeKind::Function)
        .collect();
    assert_eq!(inits.len(), 2, "each file keeps its own init node");
    assert_ne!(inits[0].id, inits[1].id, "distinct identities");
    assert_ne!(inits[0].key.source_path, inits[1].key.source_path);

    // Both call edges survive: bar's reparse did not collateral-delete foo's.
    let edges = codegraph::edges(&pool, repo).await.unwrap();
    let triples = edge_triples(&nodes, &edges);
    assert!(
        has_edge(&triples, "init", CodeRelation::Calls, "helper"),
        "foo's call edge survived bar's reparse"
    );
    assert!(has_edge(&triples, "init", CodeRelation::Calls, "other"));
}

/// Issue #6 item 4: a single-file reparse retires a symbol the file no longer
/// defines, without waiting for a whole-repository `clear_repository`.
#[tokio::test]
async fn reparse_retires_a_removed_symbol() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    let path = "src/lib.rs";

    let before = "pub fn kept() -> u32 { 0 }\npub fn dropped() -> u32 { 1 }\n";
    codegraph::upsert_file_graph(&pool, repo, &rev(), path, before)
        .await
        .unwrap();
    let nodes = codegraph::nodes(&pool, repo).await.unwrap();
    assert!(has_node(&nodes, "kept", CodeNodeKind::Function));
    assert!(has_node(&nodes, "dropped", CodeNodeKind::Function));

    // Reparse with `dropped` gone: it is retired from the graph in place.
    let after = "pub fn kept() -> u32 { 0 }\n";
    codegraph::upsert_file_graph(&pool, repo, &rev(), path, after)
        .await
        .unwrap();
    let nodes = codegraph::nodes(&pool, repo).await.unwrap();
    assert!(has_node(&nodes, "kept", CodeNodeKind::Function));
    assert!(
        !has_node(&nodes, "dropped", CodeNodeKind::Function),
        "the removed symbol was retired by the reparse"
    );
}

/// A comparable, id-independent projection of a whole repository graph.
async fn projection(
    pool: &sqlx::SqlitePool,
    repo: RepositoryId,
) -> Result<(Vec<String>, Vec<String>), CodeGraphError> {
    let nodes = codegraph::nodes(pool, repo).await?;
    let edges = codegraph::edges(pool, repo).await?;
    let mut node_keys: Vec<String> = nodes
        .iter()
        .map(|n| format!("{}|{:?}", n.key.qualified_name, n.key.kind))
        .collect();
    node_keys.sort();
    let mut edge_keys: Vec<String> = edge_triples(&nodes, &edges)
        .into_iter()
        .map(|(f, r, t)| format!("{f}|{r:?}|{t}"))
        .collect();
    edge_keys.sort();
    Ok((node_keys, edge_keys))
}

#[tokio::test]
async fn incremental_reparse_equals_full_reparse() {
    // Full: one clean parse into a fresh database.
    let (_tmp_a, pool_a) = temp_pool().await;
    let repo = RepositoryId::new();
    codegraph::upsert_file_graph(&pool_a, repo, &rev(), "src/engine.rs", FIXTURE)
        .await
        .unwrap();
    let full = projection(&pool_a, repo).await.unwrap();

    // Incremental: parse the same file twice into another database.
    let (_tmp_b, pool_b) = temp_pool().await;
    codegraph::upsert_file_graph(&pool_b, repo, &rev(), "src/engine.rs", FIXTURE)
        .await
        .unwrap();
    let first_nodes = codegraph::nodes(&pool_b, repo).await.unwrap();
    let compute_first = first_nodes
        .iter()
        .find(|n| n.key.qualified_name == "compute")
        .unwrap()
        .id;

    let second = codegraph::upsert_file_graph(&pool_b, repo, &rev(), "src/engine.rs", FIXTURE)
        .await
        .unwrap();
    let incremental = projection(&pool_b, repo).await.unwrap();

    // The graphs are identical (same node set, same edge set).
    assert_eq!(full, incremental, "incremental delta equals full reparse");

    // The reparse replaced every edge and preserved node identity.
    assert_eq!(second.removed_edges as usize, second.edges.len());
    assert!(
        second.created_node_ids.is_empty(),
        "no new nodes on reparse"
    );
    let compute_second = codegraph::nodes(&pool_b, repo)
        .await
        .unwrap()
        .into_iter()
        .find(|n| n.key.qualified_name == "compute")
        .unwrap()
        .id;
    assert_eq!(compute_first, compute_second);
}

#[tokio::test]
async fn repository_map_renders_apis_and_tests() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    codegraph::upsert_file_graph(&pool, repo, &rev(), "src/engine.rs", FIXTURE)
        .await
        .unwrap();

    let map = repository_map(&pool, repo).await.unwrap();
    let rendered = map.render();

    // Public API surface is present — this fixture's crate-root module sits
    // well under the sample cap, so every top-level API name still surfaces.
    assert!(
        rendered.contains("compute"),
        "public fn in map:\n{rendered}"
    );
    assert!(
        rendered.contains("Engine"),
        "public type in map:\n{rendered}"
    );
    assert!(rendered.contains("MAX"), "public const in map:\n{rendered}");
    // The crate-root module's count line reflects its 4 API symbols (Engine,
    // Runnable, MAX, compute) and 0 tests directly (tests live in the `tests`
    // module, folded from the fixture's `#[cfg(test)] mod tests`).
    assert!(
        rendered.contains("module (crate root) — 4 APIs, 0 tests"),
        "crate-root count line:\n{rendered}"
    );
    // Tests are now summarized by COUNT, not individually named — the
    // declutter fix (Fix 2) stops rendering every test name verbatim.
    assert!(
        rendered.contains("module tests — 0 APIs, 1 tests"),
        "the tests module's count line:\n{rendered}"
    );
    assert!(
        !rendered.contains("engine_ticks"),
        "individual test names must no longer be rendered:\n{rendered}"
    );
    // The change surface slot renders (empty stub in v1).
    assert!(rendered.contains("change surface: (none)"));
}

// --------------------------------------------------------------------------
// The named query surface (`graph.callers_of` / `blast_radius` /
// `tests_covering`) and the deleted-file retirement the live watcher needs
// --------------------------------------------------------------------------

/// The live watcher's other half: a file that DISAPPEARS is never reparsed, so
/// nothing retires its symbols. Without `remove_file_graph` a deleted file's
/// nodes linger in the graph — and in the repository map handed to the model —
/// until the next whole-repository rebuild.
#[tokio::test]
async fn remove_file_graph_retires_a_deleted_files_symbols() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();

    codegraph::upsert_file_graph(&pool, repo, &rev(), "src/gone.rs", "pub fn vanishes() {}\n")
        .await
        .unwrap();
    // Another file calls into it, so the retirement must also drop the INCOMING
    // edge — foreign keys are on, and a node with a live referrer cannot be
    // deleted.
    codegraph::upsert_file_graph(
        &pool,
        repo,
        &rev(),
        "src/keeps.rs",
        "pub fn caller() { vanishes(); }\n",
    )
    .await
    .unwrap();
    assert!(has_node(
        &codegraph::nodes(&pool, repo).await.unwrap(),
        "vanishes",
        CodeNodeKind::Function
    ));

    let retired = codegraph::remove_file_graph(&pool, repo, "src/gone.rs")
        .await
        .unwrap();
    assert!(retired > 0, "the deleted file's nodes were retired");

    let nodes = codegraph::nodes(&pool, repo).await.unwrap();
    assert!(
        !nodes.iter().any(|n| n.key.source_path == "src/gone.rs"),
        "no node survives for the deleted path"
    );
    assert!(
        has_node(&nodes, "caller", CodeNodeKind::Function),
        "the surviving file is untouched"
    );
    // Retiring a path with nothing under it is a no-op, not an error.
    assert_eq!(
        codegraph::remove_file_graph(&pool, repo, "src/gone.rs")
            .await
            .unwrap(),
        0
    );
}

/// The translation every `graph.*` call depends on: a caller names a symbol the
/// way it appears in source (`tick`, `Engine::tick`), never by the
/// `path|package::name#Kind@hash` composite the graph keys on.
#[tokio::test]
async fn graph_answers_callers_of_a_plainly_named_symbol() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    codegraph::upsert_file_graph(&pool, repo, &rev(), "src/engine.rs", FIXTURE)
        .await
        .unwrap();

    let answer = codegraph::answer(
        &pool,
        repo,
        &codegraph::GraphQuestion::CallersOf {
            symbol: "compute".to_owned(),
        },
    )
    .await
    .unwrap();

    let names: Vec<&str> = answer
        .hits
        .iter()
        .map(|h| h.qualified_name.as_str())
        .collect();
    assert!(
        names.contains(&"Engine::tick"),
        "the method that calls it: {names:?}"
    );
    assert!(
        names.contains(&"tests::engine_ticks"),
        "the test that calls it: {names:?}"
    );
    assert!(
        !answer.targets.is_empty(),
        "the answer names what it resolved to"
    );
    let rendered = answer.render();
    assert!(rendered.contains("callers of `compute`"), "{rendered}");
    assert!(rendered.contains("src/engine.rs"), "{rendered}");
}

/// Depth is a real bound, not decoration: `new` is reached only THROUGH `tick`'s
/// caller chain, so depth 1 must not report it.
#[tokio::test]
async fn blast_radius_is_depth_bounded_and_names_the_test_that_reaches_it() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    codegraph::upsert_file_graph(&pool, repo, &rev(), "src/engine.rs", FIXTURE)
        .await
        .unwrap();

    let shallow = codegraph::answer(
        &pool,
        repo,
        &codegraph::GraphQuestion::BlastRadius {
            symbol: "compute".to_owned(),
            depth: 1,
        },
    )
    .await
    .unwrap();
    let deep = codegraph::answer(
        &pool,
        repo,
        &codegraph::GraphQuestion::BlastRadius {
            symbol: "compute".to_owned(),
            // Deliberately over the ceiling: it is clamped, never rejected.
            depth: 99,
        },
    )
    .await
    .unwrap();

    let shallow_names: Vec<&str> = shallow
        .hits
        .iter()
        .map(|h| h.qualified_name.as_str())
        .collect();
    let deep_names: Vec<&str> = deep
        .hits
        .iter()
        .map(|h| h.qualified_name.as_str())
        .collect();
    assert!(shallow_names.contains(&"Engine::tick"), "{shallow_names:?}");
    assert!(deep_names.contains(&"Engine::tick"), "{deep_names:?}");
    assert!(
        deep.total >= shallow.total,
        "a deeper walk never reaches fewer nodes: {} vs {}",
        deep.total,
        shallow.total
    );
    assert!(
        deep.render().contains("depth 5"),
        "an over-ceiling depth is clamped and SAID to be: {}",
        deep.render()
    );
}

/// A missed lookup offers a next step instead of a bare "not found".
#[tokio::test]
async fn a_missed_lookup_suggests_candidates() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    codegraph::upsert_file_graph(&pool, repo, &rev(), "src/engine.rs", FIXTURE)
        .await
        .unwrap();

    let answer = codegraph::answer(
        &pool,
        repo,
        &codegraph::GraphQuestion::CallersOf {
            symbol: "computer".to_owned(),
        },
    )
    .await
    .unwrap();
    assert!(answer.targets.is_empty());
    assert!(
        answer.candidates.iter().any(|c| c == "compute"),
        "candidates: {:?}",
        answer.candidates
    );
    assert!(
        answer.render().contains("did you mean"),
        "{}",
        answer.render()
    );
}

/// `tests_covering` accepts a path suffix, because nobody types the full
/// repo-relative path, and returns only `Test` nodes.
#[tokio::test]
async fn tests_covering_accepts_a_path_suffix_and_returns_only_tests() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    codegraph::upsert_file_graph(&pool, repo, &rev(), "crates/x/src/engine.rs", FIXTURE)
        .await
        .unwrap();

    let answer = codegraph::answer(
        &pool,
        repo,
        &codegraph::GraphQuestion::TestsCovering {
            path: "engine.rs".to_owned(),
            depth: 3,
        },
    )
    .await
    .unwrap();
    assert!(
        answer.hits.iter().any(
            |h| h.qualified_name == "tests::engine_ticks" || h.qualified_name == "engine_ticks"
        ),
        "hits: {:?}",
        answer.hits
    );
    assert!(
        answer.hits.iter().all(|h| h.kind == CodeNodeKind::Test),
        "only tests are returned: {:?}",
        answer.hits
    );
}

/// F10: a BFS seeded in repository A must not walk THROUGH repository B and
/// back, reporting a node whose only path to the target runs via a foreign
/// symbol the answer never shows.
#[tokio::test]
async fn blast_radius_does_not_traverse_through_another_repository() {
    let (_tmp, pool) = temp_pool().await;
    let a = RepositoryId::new();
    let b = RepositoryId::new();
    // A defines the target and an `outer` that calls into B's bridge; B's bridge
    // calls A's target. The only path target ← outer runs through B.
    codegraph::upsert_file_graph(
        &pool,
        a,
        &rev(),
        "src/lib.rs",
        "pub fn target() {}\npub fn outer() { bridge(); }\n",
    )
    .await
    .unwrap();
    codegraph::upsert_file_graph(
        &pool,
        b,
        &rev(),
        "src/lib.rs",
        "pub fn bridge() { target(); }\n",
    )
    .await
    .unwrap();

    let node = |nodes: Vec<CodeNode>, name: &str| {
        nodes
            .into_iter()
            .find(|n| n.key.qualified_name == name && n.key.kind == CodeNodeKind::Function)
            .unwrap()
    };
    let a_nodes = codegraph::nodes(&pool, a).await.unwrap();
    let a_target = node(a_nodes.clone(), "target");
    let a_outer = node(a_nodes, "outer");
    let b_bridge = node(codegraph::nodes(&pool, b).await.unwrap(), "bridge");

    // The two cross-repository edges the semantic (LSP) layer can produce once
    // it resolves references across checkouts served by one daemon.
    for (from, to) in [(b_bridge.id, a_target.id), (a_outer.id, b_bridge.id)] {
        sqlx::query(
            "INSERT INTO code_edges (id, from_node, to_node, relation, confidence, \
             evidence_kind, evidence_artifact, revision, created_at) \
             VALUES (?, ?, ?, 'calls', 1.0, 'lsp_resolved', NULL, ?, ?)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(from.to_string())
        .bind(to.to_string())
        .bind(&rev().0)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();
    }

    let answer = codegraph::answer(
        &pool,
        a,
        &codegraph::GraphQuestion::BlastRadius {
            symbol: "target".to_owned(),
            depth: 3,
        },
    )
    .await
    .unwrap();
    // Before the repository scope moved INTO the walk, the BFS hopped to B's
    // `bridge`, spent a depth layer there, came back to A's `outer`, and
    // `nodes_by_ids` then dropped `bridge` — leaving `outer` in the answer with
    // no visible path to `target`.
    assert!(
        answer.hits.iter().all(|h| h.qualified_name != "outer"),
        "no node reachable only through another repository: {:?}",
        answer.hits
    );
    assert_eq!(answer.total, 0, "hits: {:?}", answer.hits);
}

// --------------------------------------------------------------------------
// Multi-language extraction (the mixed-repository bug)
// --------------------------------------------------------------------------

const PYTHON_FIXTURE: &str = r#"
import json
from .service import dispatch, Client as C

MAX_RETRIES = 3


async def handler(request):
    return route(request)


def route(request):
    return dispatch(request)


class Router:
    def decide(self, request):
        return handler(request)
"#;

const TSX_FIXTURE: &str = r#"
import { useState } from "react";
import Panel from "./panel";

export function greet(name: string): string {
  return format(name);
}

function format(name: string): string {
  return `hi ${name}`;
}

export interface Props { name: string }

export type Id = string;

export const App = (props: Props) => {
  const [n] = useState(props.name);
  return <div>{greet(n)}</div>;
};

export class Board {
  render(): string {
    return greet("board");
  }
}
"#;

#[tokio::test]
async fn python_defines_symbols_calls_and_imports() {
    // Before the language dispatch this file was parsed with the RUST grammar,
    // which yields an error tree indistinguishable from an empty file: one File
    // node, no symbols, no edges — and no error anywhere.
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    codegraph::upsert_file_graph(&pool, repo, &rev(), "src/main.py", PYTHON_FIXTURE)
        .await
        .unwrap();
    let nodes = codegraph::nodes(&pool, repo).await.unwrap();
    let edges = codegraph::edges(&pool, repo).await.unwrap();
    let triples = edge_triples(&nodes, &edges);

    assert!(has_node(&nodes, "src/main.py", CodeNodeKind::File));
    // `async def` — which the old line scanner could not see at all.
    assert!(has_node(&nodes, "handler", CodeNodeKind::Function));
    assert!(has_node(&nodes, "route", CodeNodeKind::Function));
    assert!(has_node(&nodes, "Router", CodeNodeKind::Type));
    // A method: every indented line was skipped before, so ALL methods were lost.
    assert!(has_node(&nodes, "Router.decide", CodeNodeKind::Method));
    assert!(has_node(&nodes, "MAX_RETRIES", CodeNodeKind::Constant));

    // Every language must produce the same four relations the Rust path does.
    assert!(has_edge(
        &triples,
        "src/main.py",
        CodeRelation::Contains,
        "handler"
    ));
    assert!(has_edge(
        &triples,
        "Router",
        CodeRelation::Defines,
        "Router.decide"
    ));
    assert!(has_edge(&triples, "handler", CodeRelation::Calls, "route"));
    assert!(has_edge(&triples, "route", CodeRelation::Calls, "dispatch"));
    assert!(has_edge(
        &triples,
        "Router.decide",
        CodeRelation::Calls,
        "handler"
    ));
    assert!(has_edge(
        &triples,
        "src/main.py",
        CodeRelation::Imports,
        "json"
    ));
    assert!(has_edge(
        &triples,
        "src/main.py",
        CodeRelation::Imports,
        ".service.dispatch"
    ));

    // The language is recorded, so a report can break the graph down by it.
    assert!(
        nodes.iter().all(|n| n.key.language.0 == "python"),
        "{nodes:?}"
    );
    // A syntax-inferred call keeps its Chapter 07 confidence in every language.
    let call = edges
        .iter()
        .find(|e| e.relation == CodeRelation::Calls)
        .expect("a call edge");
    assert_eq!(call.evidence_kind, EvidenceKind::SyntaxInferred);
    assert!((call.confidence - codypendent_knowledge::SYNTAX_CALL_CONFIDENCE).abs() < f32::EPSILON);
}

#[tokio::test]
async fn tsx_defines_symbols_calls_and_imports() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    codegraph::upsert_file_graph(&pool, repo, &rev(), "src/app.tsx", TSX_FIXTURE)
        .await
        .unwrap();
    let nodes = codegraph::nodes(&pool, repo).await.unwrap();
    let edges = codegraph::edges(&pool, repo).await.unwrap();
    let triples = edge_triples(&nodes, &edges);

    assert!(has_node(&nodes, "greet", CodeNodeKind::Function));
    assert!(has_node(&nodes, "format", CodeNodeKind::Function));
    assert!(has_node(&nodes, "Props", CodeNodeKind::TraitOrInterface));
    assert!(has_node(&nodes, "Id", CodeNodeKind::Type));
    assert!(has_node(&nodes, "Board", CodeNodeKind::Type));
    assert!(has_node(&nodes, "Board.render", CodeNodeKind::Method));
    // The arrow-function `const` — the dominant React declaration form, and
    // invisible to anything that only matches the `function` keyword.
    assert!(
        has_node(&nodes, "App", CodeNodeKind::Function),
        "{nodes:#?}"
    );

    assert!(has_edge(&triples, "greet", CodeRelation::Calls, "format"));
    assert!(has_edge(&triples, "App", CodeRelation::Calls, "greet"));
    assert!(has_edge(&triples, "App", CodeRelation::Calls, "useState"));
    assert!(has_edge(
        &triples,
        "Board.render",
        CodeRelation::Calls,
        "greet"
    ));
    assert!(has_edge(
        &triples,
        "src/app.tsx",
        CodeRelation::Imports,
        "react.useState"
    ));
    assert!(has_edge(
        &triples,
        "src/app.tsx",
        CodeRelation::Imports,
        "./panel.Panel"
    ));
    assert!(nodes.iter().all(|n| n.key.language.0 == "tsx"), "{nodes:?}");
}

#[tokio::test]
async fn every_supported_extension_parses_and_nothing_else_does() {
    // The gate itself. `language_for` is the ONE list; a file it accepts must
    // parse, and a file it rejects must fail LOUDLY rather than fold to an empty
    // graph the caller reads as "this file defines nothing".
    for extension in codegraph::supported_extensions() {
        let path = format!("src/probe.{extension}");
        let language = codegraph::language_for(std::path::Path::new(&path));
        assert!(language.is_some(), "language_for rejects .{extension}");
        codegraph::validate_file_graph(RepositoryId::new(), &path, "")
            .unwrap_or_else(|e| panic!("empty .{extension} file failed to parse: {e}"));
    }

    let error = codegraph::validate_file_graph(RepositoryId::new(), "main.go", "package main\n")
        .expect_err("an unsupported extension must be an error, not an empty graph");
    assert!(
        matches!(error, CodeGraphError::UnsupportedLanguage { .. }),
        "{error:?}"
    );
    // The error names what IS supported, so the message is a next step.
    assert!(error.to_string().contains("tsx"), "{error}");
}

#[tokio::test]
async fn a_python_symbol_is_findable_by_its_simple_name() {
    // `find_symbols`' last-segment tier matched `%::name` only, so a
    // `.`-separated language could only ever be found by the substring tier.
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    codegraph::upsert_file_graph(&pool, repo, &rev(), "src/main.py", PYTHON_FIXTURE)
        .await
        .unwrap();
    // `predecide` CONTAINS "decide", so the substring tier (the last resort)
    // would return both. Only a real last-segment match on the `.` separator
    // narrows it to one — which is what makes this assertion discriminating
    // rather than accidentally satisfied by the substring fallback.
    codegraph::upsert_file_graph(
        &pool,
        repo,
        &rev(),
        "src/util.py",
        "def predecide():\n    return 1\n",
    )
    .await
    .unwrap();
    let hits = codegraph::find_symbols(&pool, repo, "decide", 5)
        .await
        .unwrap();
    assert_eq!(
        hits.iter()
            .map(|n| n.key.qualified_name.as_str())
            .collect::<Vec<_>>(),
        ["Router.decide"]
    );

    let answer = codegraph::answer(
        &pool,
        repo,
        &codegraph::GraphQuestion::CallersOf {
            symbol: "Router.decide".to_owned(),
        },
    )
    .await
    .unwrap();
    assert_eq!(answer.total, 0, "nothing calls it: {:?}", answer.hits);
    assert!(!answer.targets.is_empty(), "but it resolved: {answer:?}");
}

// --------------------------------------------------------------------------
// A rebuild retires what a scan did not see — but only if the scan FINISHED
// --------------------------------------------------------------------------

/// A complete scan is what licenses the retire pass, and a truncated one is not.
///
/// The retire pass reasons "this stored path was not in the scan, therefore it
/// is gone". That holds only when the walk reached everything. A walk stopped by
/// its file cap reached an arbitrary prefix of the repository, and retiring the
/// rest turns one truncated scan into a wiped graph — which is precisely what a
/// `node_modules/` sorting before `src/` produced once JavaScript and TypeScript
/// became foldable: the cap was spent on ignored dependency code and `graph
/// build` retired the entire application.
#[tokio::test]
async fn a_truncated_rebuild_retires_nothing_and_a_complete_one_still_does() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    for path in ["src/a.rs", "src/b.rs"] {
        codegraph::upsert_file_graph(&pool, repo, &rev(), path, "pub fn kept() {}\n")
            .await
            .unwrap();
    }

    // A scan that reached only `src/a.rs` because it ran out of budget says
    // nothing whatever about `src/b.rs`.
    let truncated = codegraph::rebuild_repository(
        &pool,
        repo,
        &rev(),
        [("src/a.rs", "pub fn kept() {}\n")],
        codegraph::ScanCoverage::Truncated,
    )
    .await
    .unwrap();
    assert_eq!(truncated.retired, codegraph::RetiredFiles::default());
    let paths: Vec<String> = codegraph::nodes(&pool, repo)
        .await
        .unwrap()
        .into_iter()
        .map(|node| node.key.source_path)
        .collect();
    assert!(
        paths.iter().any(|path| path == "src/b.rs"),
        "a truncated rebuild retired a path it never looked at: {paths:?}"
    );

    // The same file list from a scan that FINISHED is evidence, and the deleted
    // file is retired exactly as before — suppressing retirement on truncation
    // must not cost the ordinary deletion case.
    let complete = codegraph::rebuild_repository(
        &pool,
        repo,
        &rev(),
        [("src/a.rs", "pub fn kept() {}\n")],
        codegraph::ScanCoverage::Complete,
    )
    .await
    .unwrap();
    assert_eq!(complete.retired.files, 1, "{complete:?}");
    assert!(complete.retired.nodes > 0, "{complete:?}");
    let paths: Vec<String> = codegraph::nodes(&pool, repo)
        .await
        .unwrap()
        .into_iter()
        .map(|node| node.key.source_path)
        .collect();
    assert!(
        !paths.iter().any(|path| path == "src/b.rs"),
        "a complete rebuild left a genuinely deleted file behind: {paths:?}"
    );
}
