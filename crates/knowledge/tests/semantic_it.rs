//! STEP 4.5 semantic layer + revision-aware queries: an LSP-resolved edge
//! supersedes its syntax-inferred counterpart; blast-radius/callers/tests-covering
//! walk a known call chain; changed_between detects signature changes; the
//! LanguageAdapter parses and degrades to syntax-only without a language server.

use codypendent_knowledge::adapter::{
    on_path, LanguageAdapter, ParseInput, RustAdapter, ScriptAdapter, SemanticCapability, Workspace,
};
use codypendent_knowledge::codegraph::{
    self, blast_radius, callers_of, changed_between, tests_covering, SemanticEdge, SymbolSnapshot,
};
use codypendent_knowledge::types::{
    CodeNode, CodeNodeKind, CodeRelation, EvidenceKind, AGENT_ASSERTED_CONFIDENCE,
    COMPILER_RESOLVED_CONFIDENCE, LSP_RESOLVED_CONFIDENCE,
};
use codypendent_knowledge::{db, GitRevision};
use codypendent_protocol::RepositoryId;

async fn temp_pool() -> (tempfile::TempDir, sqlx::SqlitePool) {
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("codypendent.db")).await.unwrap();
    (tmp, pool)
}

/// A single-file call chain: `driver` → `tick` → `compute`.
const CHAIN: &str = r#"
pub fn compute(x: u32) -> u32 { x + 1 }
pub fn tick() -> u32 { compute(1) }
pub fn driver() -> u32 { tick() }
"#;

fn key_of<'a>(nodes: &'a [CodeNode], name: &str) -> &'a CodeNode {
    nodes
        .iter()
        .find(|n| n.key.qualified_name == name)
        .unwrap_or_else(|| panic!("no node named {name}"))
}

#[tokio::test]
async fn callers_and_blast_radius_walk_the_chain() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    let rev = GitRevision("rev1".into());
    codegraph::upsert_file_graph(&pool, repo, &rev, "src/lib.rs", CHAIN)
        .await
        .unwrap();
    let nodes = codegraph::nodes(&pool, repo).await.unwrap();
    let compute_key = key_of(&nodes, "compute").key.stable_key();

    // Direct callers of `compute` are exactly `tick`.
    let direct = callers_of(&pool, repo, &compute_key).await.unwrap();
    assert_eq!(direct.len(), 1);
    assert_eq!(direct[0].key.qualified_name, "tick");

    // Blast radius grows with depth: {tick} at 1, {tick, driver} at 2.
    let r1 = blast_radius(&pool, repo, &compute_key, 1).await.unwrap();
    assert_eq!(r1.len(), 1);
    let r2 = blast_radius(&pool, repo, &compute_key, 2).await.unwrap();
    let names: Vec<&str> = r2.iter().map(|n| n.key.qualified_name.as_str()).collect();
    assert!(
        names.contains(&"tick") && names.contains(&"driver"),
        "{names:?}"
    );
}

#[tokio::test]
async fn lsp_edge_supersedes_the_syntax_edge() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    let rev = GitRevision("rev1".into());
    codegraph::upsert_file_graph(&pool, repo, &rev, "src/lib.rs", CHAIN)
        .await
        .unwrap();
    let nodes = codegraph::nodes(&pool, repo).await.unwrap();
    let tick = key_of(&nodes, "tick");
    let compute = key_of(&nodes, "compute");

    // Before: the syntax layer inferred a low-confidence tick → compute call.
    let before: Vec<_> = codegraph::edges(&pool, repo)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.from == tick.id && e.to == compute.id)
        .collect();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].evidence_kind, EvidenceKind::SyntaxInferred);

    // Fold in the LSP-resolved edge for the same (from, to, relation).
    let outcome = codegraph::upsert_semantic_edges(
        &pool,
        repo,
        &rev,
        &[SemanticEdge {
            from_symbol_key: tick.key.stable_key(),
            to_symbol_key: compute.key.stable_key(),
            relation: CodeRelation::Calls,
            evidence_kind: EvidenceKind::LspResolved,
            confidence: LSP_RESOLVED_CONFIDENCE,
            evidence: None,
        }],
    )
    .await
    .unwrap();
    assert_eq!(outcome.applied, 1);
    assert_eq!(outcome.skipped_unresolved, 0);
    assert_eq!(outcome.skipped_outranked, 0);

    // After: exactly one tick → compute edge remains — the resolved one, at LSP
    // confidence. The syntax edge was superseded, not duplicated.
    let after: Vec<_> = codegraph::edges(&pool, repo)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.from == tick.id && e.to == compute.id)
        .collect();
    assert_eq!(after.len(), 1, "superseded, not duplicated");
    assert_eq!(after[0].evidence_kind, EvidenceKind::LspResolved);
    assert!((after[0].confidence - LSP_RESOLVED_CONFIDENCE).abs() < f32::EPSILON);
}

#[tokio::test]
async fn semantic_edge_with_missing_endpoint_is_skipped() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    let rev = GitRevision("rev1".into());
    codegraph::upsert_file_graph(&pool, repo, &rev, "src/lib.rs", CHAIN)
        .await
        .unwrap();
    let outcome = codegraph::upsert_semantic_edges(
        &pool,
        repo,
        &rev,
        &[SemanticEdge {
            from_symbol_key: "does|not::exist#Function@".into(),
            to_symbol_key: "also|missing#Function@".into(),
            relation: CodeRelation::Calls,
            evidence_kind: EvidenceKind::LspResolved,
            confidence: LSP_RESOLVED_CONFIDENCE,
            evidence: None,
        }],
    )
    .await
    .unwrap();
    assert_eq!(outcome.applied, 0);
    assert_eq!(outcome.skipped_unresolved, 1);
}

#[tokio::test]
async fn tests_covering_follows_a_resolved_cross_file_edge() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    let rev = GitRevision("rev1".into());
    // The implementation and the test live in different files; the syntax layer
    // cannot link them, but an LSP-resolved edge can.
    codegraph::upsert_file_graph(
        &pool,
        repo,
        &rev,
        "src/lib.rs",
        "pub fn charge() -> u32 { 0 }",
    )
    .await
    .unwrap();
    codegraph::upsert_file_graph(
        &pool,
        repo,
        &rev,
        "tests/charge.rs",
        "#[test]\nfn charge_works() { assert_eq!(0, 0); }",
    )
    .await
    .unwrap();
    let nodes = codegraph::nodes(&pool, repo).await.unwrap();
    let charge = key_of(&nodes, "charge");
    let test = key_of(&nodes, "charge_works");
    assert_eq!(test.key.kind, CodeNodeKind::Test);

    codegraph::upsert_semantic_edges(
        &pool,
        repo,
        &rev,
        &[SemanticEdge {
            from_symbol_key: test.key.stable_key(),
            to_symbol_key: charge.key.stable_key(),
            relation: CodeRelation::Calls,
            evidence_kind: EvidenceKind::LspResolved,
            confidence: LSP_RESOLVED_CONFIDENCE,
            evidence: None,
        }],
    )
    .await
    .unwrap();

    let covering = tests_covering(&pool, repo, "src/lib.rs", 3).await.unwrap();
    assert_eq!(covering.len(), 1);
    assert_eq!(covering[0].key.qualified_name, "charge_works");
}

#[test]
fn changed_between_detects_added_removed_and_modified() {
    let sym = |name: &str, sig: Option<&str>| SymbolSnapshot {
        qualified_name: name.into(),
        kind: CodeNodeKind::Function,
        source_path: "src/lib.rs".into(),
        signature_hash: sig.map(str::to_string),
    };
    let before = vec![
        sym("stable", Some("a")),
        sym("gone", Some("b")),
        sym("changed", Some("c")),
    ];
    let after = vec![
        sym("stable", Some("a")),
        sym("changed", Some("c2")),
        sym("fresh", Some("d")),
    ];

    let delta = changed_between(&before, &after);
    assert_eq!(
        delta
            .added
            .iter()
            .map(|s| s.qualified_name.as_str())
            .collect::<Vec<_>>(),
        ["fresh"]
    );
    assert_eq!(
        delta
            .removed
            .iter()
            .map(|s| s.qualified_name.as_str())
            .collect::<Vec<_>>(),
        ["gone"]
    );
    assert_eq!(delta.modified.len(), 1);
    assert_eq!(delta.modified[0].1.qualified_name, "changed");
}

#[tokio::test]
async fn rust_adapter_parses_and_degrades_without_a_language_server() {
    let adapter = RustAdapter;
    let out = adapter
        .parse(ParseInput {
            path: "src/lib.rs".into(),
            source: CHAIN.into(),
        })
        .await
        .unwrap();
    let names: Vec<&str> = out
        .symbols
        .iter()
        .map(|s| s.qualified_name.as_str())
        .collect();
    assert!(names.contains(&"compute") && names.contains(&"tick") && names.contains(&"driver"));

    // The syntax parse works regardless of tooling (graceful degradation). The
    // reported capability is exactly whether rust-analyzer is on PATH — LSP when
    // present, SyntaxOnly when absent; never a failure. A binary that cannot be
    // installed anywhere is always absent.
    let expected = if on_path("rust-analyzer") {
        SemanticCapability::LspResolved
    } else {
        SemanticCapability::SyntaxOnly
    };
    assert_eq!(adapter.capability(), expected);
    assert!(!on_path("codypendent-no-such-language-server"));
}

#[tokio::test]
async fn python_and_typescript_adapters_parse_with_the_graph_grammar() {
    // These adapters used to run their own line scanners, which skipped every
    // indented line (so every method), missed `async def`, `interface`, `type`
    // and arrow functions, and gave every symbol `signature_hash: None` — so a
    // signature change could never be detected. They now run the same
    // tree-sitter walk the graph persists, so an adapter and the graph can never
    // disagree about what a file defines. The file node leads, as it does for
    // Rust.
    let py = ScriptAdapter::python();
    let out = py
        .parse(ParseInput {
            path: "m.py".into(),
            source: "async def charge(x):\n    return x\n\n\nclass Wallet:\n    def top_up(self):\n        return charge(1)\n".into(),
        })
        .await
        .unwrap();
    let names: Vec<&str> = out
        .symbols
        .iter()
        .map(|s| s.qualified_name.as_str())
        .collect();
    assert_eq!(names, ["m.py", "charge", "Wallet", "Wallet.top_up"]);
    assert!(
        out.symbols
            .iter()
            .filter(|s| s.qualified_name != "m.py")
            .any(|s| s.signature_hash.is_some()),
        "a signature change must be observable: {:?}",
        out.symbols
    );
    // Capability reflects whether pyright is present; the syntax scan works either
    // way (graceful degradation).
    let expected = if on_path("pyright") {
        SemanticCapability::LspResolved
    } else {
        SemanticCapability::SyntaxOnly
    };
    assert_eq!(py.capability(), expected);

    let ts = ScriptAdapter::typescript();
    let out = ts
        .parse(ParseInput {
            path: "m.ts".into(),
            source: "export function charge(x: number) {}\nexport class Wallet {}\n\
                     export interface Ledger { total: number }\n\
                     export const refund = (x: number) => charge(-x);\n"
                .into(),
        })
        .await
        .unwrap();
    let names: Vec<&str> = out
        .symbols
        .iter()
        .map(|s| s.qualified_name.as_str())
        .collect();
    // `interface` and the arrow-function `const` were invisible to the old
    // scanner; in React/TypeScript code the arrow form is most of the file.
    assert_eq!(names, ["m.ts", "charge", "Wallet", "Ledger", "refund"]);
}

#[tokio::test]
async fn hierarchical_map_folds_bottom_up_with_evidence() {
    use codypendent_knowledge::repomap::{hierarchical_map, MapLevel};
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    let rev = GitRevision("rev1".into());
    codegraph::upsert_file_graph(&pool, repo, &rev, "src/lib.rs", CHAIN)
        .await
        .unwrap();

    let map = hierarchical_map(&pool, repo).await.unwrap();
    assert_eq!(map.level, MapLevel::Workspace);
    // workspace → package → module, each recording the evidence beneath it.
    assert_eq!(map.evidence.symbol_count, 3);
    assert_eq!(map.evidence.revision.as_deref(), Some("rev1"));
    let package = &map.children[0];
    assert_eq!(package.level, MapLevel::Package);
    assert_eq!(package.evidence.symbol_count, 3);
    let module = &package.children[0];
    assert_eq!(module.level, MapLevel::Module);
    assert_eq!(module.evidence.symbol_count, 3);
    let symbols: Vec<&str> = module.children.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(symbols, ["compute", "driver", "tick"]);
}

#[tokio::test]
async fn rust_adapter_reads_cargo_metadata() {
    // A minimal crate in a temp dir: `cargo metadata --no-deps` needs no network.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"fixture-pkg\"\nversion = \"0.3.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/lib.rs"), "pub fn f() {}\n").unwrap();

    let adapter = RustAdapter;
    let meta = adapter
        .build_metadata(&Workspace::new(tmp.path()))
        .await
        .unwrap();
    assert!(meta
        .packages
        .iter()
        .any(|p| p.name == "fixture-pkg" && p.version == "0.3.1"));
}

#[tokio::test]
async fn reparse_retiring_a_symbol_removes_incoming_semantic_edges() {
    // A cross-file LSP edge points INTO a symbol; reparsing that symbol's file to
    // remove it must retire the node AND drop the now-stale incoming edge. With
    // foreign keys enabled, leaving the edge behind would fail the retire delete.
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    let rev = GitRevision("rev1".into());
    codegraph::upsert_file_graph(
        &pool,
        repo,
        &rev,
        "src/a.rs",
        "pub fn target() -> u32 { 0 }",
    )
    .await
    .unwrap();
    codegraph::upsert_file_graph(
        &pool,
        repo,
        &rev,
        "tests/b.rs",
        "#[test]\nfn covers() { assert_eq!(0, 0); }",
    )
    .await
    .unwrap();
    let nodes = codegraph::nodes(&pool, repo).await.unwrap();
    let target = key_of(&nodes, "target");
    let covers = key_of(&nodes, "covers");
    codegraph::upsert_semantic_edges(
        &pool,
        repo,
        &rev,
        &[SemanticEdge {
            from_symbol_key: covers.key.stable_key(),
            to_symbol_key: target.key.stable_key(),
            relation: CodeRelation::Calls,
            evidence_kind: EvidenceKind::LspResolved,
            confidence: LSP_RESOLVED_CONFIDENCE,
            evidence: None,
        }],
    )
    .await
    .unwrap();
    let target_id = target.id;

    // Reparse src/a.rs, removing `target`. Must succeed (no FK violation).
    let rev2 = GitRevision("rev2".into());
    codegraph::upsert_file_graph(
        &pool,
        repo,
        &rev2,
        "src/a.rs",
        "pub fn other() -> u32 { 1 }",
    )
    .await
    .unwrap();

    let nodes = codegraph::nodes(&pool, repo).await.unwrap();
    assert!(
        nodes.iter().all(|n| n.key.qualified_name != "target"),
        "the removed symbol was retired"
    );
    let edges = codegraph::edges(&pool, repo).await.unwrap();
    assert!(
        edges.iter().all(|e| e.to != target_id),
        "the stale incoming semantic edge was removed"
    );
}

#[test]
fn changed_between_is_file_scoped() {
    let sym = |name: &str, path: &str, sig: Option<&str>| SymbolSnapshot {
        qualified_name: name.into(),
        kind: CodeNodeKind::Function,
        source_path: path.into(),
        signature_hash: sig.map(str::to_string),
    };
    // The same qualified name in two files; a.rs::init is removed, b.rs::init is
    // unchanged. Keying by name alone would hide the removal.
    let before = vec![
        sym("init", "a.rs", Some("x")),
        sym("init", "b.rs", Some("y")),
    ];
    let after = vec![sym("init", "b.rs", Some("y"))];
    let delta = changed_between(&before, &after);
    assert_eq!(delta.removed.len(), 1);
    assert_eq!(delta.removed[0].source_path, "a.rs");
    assert!(delta.added.is_empty());
    assert!(delta.modified.is_empty());
}

// --------------------------------------------------------------------------
// Agent-asserted edges — a model's claim must never outrank a machine's fact
// --------------------------------------------------------------------------

/// A route handler and the service it dispatches to via a table the parser
/// cannot follow — exactly the shape `graph.assert_edge` exists for.
const DISPATCH: &str = r#"
pub fn handle_create_user() -> u32 { 0 }
pub fn user_service_create() -> u32 { 1 }
pub fn caller() -> u32 { handle_create_user() }
"#;

fn assertion(from: &str, to: &str) -> codegraph::AgentEdgeAssertion {
    codegraph::AgentEdgeAssertion {
        from_symbol: from.to_owned(),
        to_symbol: to.to_owned(),
        relation: CodeRelation::Calls,
        evidence: None,
    }
}

async fn edges_between(
    pool: &sqlx::SqlitePool,
    repo: RepositoryId,
    from: &CodeNode,
    to: &CodeNode,
) -> Vec<codypendent_knowledge::CodeEdge> {
    codegraph::edges(pool, repo)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.from == from.id && e.to == to.id)
        .collect()
}

#[tokio::test]
async fn an_agent_assertion_records_an_edge_the_parser_cannot_see() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    let rev = GitRevision("rev1".into());
    codegraph::upsert_file_graph(&pool, repo, &rev, "src/api.rs", DISPATCH)
        .await
        .unwrap();
    let nodes = codegraph::nodes(&pool, repo).await.unwrap();
    let handler = key_of(&nodes, "handle_create_user");
    let service = key_of(&nodes, "user_service_create");

    assert!(edges_between(&pool, repo, handler, service)
        .await
        .is_empty());

    let results = codegraph::assert_agent_edges(
        &pool,
        repo,
        &rev,
        &[assertion("handle_create_user", "user_service_create")],
    )
    .await
    .unwrap();
    assert_eq!(results, vec![codegraph::AssertionResult::Applied]);

    let recorded = edges_between(&pool, repo, handler, service).await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].evidence_kind, EvidenceKind::AgentAsserted);
    assert!(
        (recorded[0].confidence - AGENT_ASSERTED_CONFIDENCE).abs() < f32::EPSILON,
        "{:?}",
        recorded[0]
    );

    // Re-asserting is idempotent: equal confidence is not STRICTLY lower, so the
    // existing row is kept rather than deleted-and-reinserted or duplicated.
    let again = codegraph::assert_agent_edges(
        &pool,
        repo,
        &rev,
        &[assertion("handle_create_user", "user_service_create")],
    )
    .await
    .unwrap();
    assert!(
        matches!(
            again.as_slice(),
            [codegraph::AssertionResult::Outranked {
                existing: EvidenceKind::AgentAsserted,
                ..
            }]
        ),
        "{again:?}"
    );
    assert_eq!(edges_between(&pool, repo, handler, service).await.len(), 1);
}

#[tokio::test]
async fn an_agent_assertion_cannot_overwrite_a_resolved_fact() {
    // THE safety property. The old code deleted any edge for the triple before
    // inserting, unconditionally — so an agent's 0.40 guess erased a
    // compiler-resolved 0.98 fact and the graph then reported the guess.
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    let rev = GitRevision("rev1".into());
    codegraph::upsert_file_graph(&pool, repo, &rev, "src/api.rs", DISPATCH)
        .await
        .unwrap();
    let nodes = codegraph::nodes(&pool, repo).await.unwrap();
    let handler = key_of(&nodes, "handle_create_user");
    let service = key_of(&nodes, "user_service_create");

    codegraph::upsert_semantic_edges(
        &pool,
        repo,
        &rev,
        &[SemanticEdge {
            from_symbol_key: handler.key.stable_key(),
            to_symbol_key: service.key.stable_key(),
            relation: CodeRelation::Calls,
            evidence_kind: EvidenceKind::CompilerResolved,
            confidence: COMPILER_RESOLVED_CONFIDENCE,
            evidence: None,
        }],
    )
    .await
    .unwrap();

    let results = codegraph::assert_agent_edges(
        &pool,
        repo,
        &rev,
        &[assertion("handle_create_user", "user_service_create")],
    )
    .await
    .unwrap();
    assert!(
        matches!(
            results.as_slice(),
            [codegraph::AssertionResult::Outranked {
                existing: EvidenceKind::CompilerResolved,
                ..
            }]
        ),
        "{results:?}"
    );

    let survived = edges_between(&pool, repo, handler, service).await;
    assert_eq!(survived.len(), 1, "no weaker duplicate beside the fact");
    assert_eq!(survived[0].evidence_kind, EvidenceKind::CompilerResolved);
    assert!((survived[0].confidence - COMPILER_RESOLVED_CONFIDENCE).abs() < f32::EPSILON);
}

#[tokio::test]
async fn an_agent_assertion_cannot_overwrite_what_the_parser_saw() {
    // `AGENT_ASSERTED_CONFIDENCE` sits below `SYNTAX_CALL_CONFIDENCE`, so a
    // model's reading cannot displace even tree-sitter. The assertion is
    // reported as outranked rather than silently dropped.
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    let rev = GitRevision("rev1".into());
    codegraph::upsert_file_graph(&pool, repo, &rev, "src/api.rs", DISPATCH)
        .await
        .unwrap();
    let nodes = codegraph::nodes(&pool, repo).await.unwrap();
    let caller = key_of(&nodes, "caller");
    let handler = key_of(&nodes, "handle_create_user");
    assert_eq!(edges_between(&pool, repo, caller, handler).await.len(), 1);

    let results = codegraph::assert_agent_edges(
        &pool,
        repo,
        &rev,
        &[assertion("caller", "handle_create_user")],
    )
    .await
    .unwrap();
    assert!(
        matches!(
            results.as_slice(),
            [codegraph::AssertionResult::Outranked {
                existing: EvidenceKind::SyntaxInferred,
                ..
            }]
        ),
        "{results:?}"
    );
    let survived = edges_between(&pool, repo, caller, handler).await;
    assert_eq!(survived.len(), 1);
    assert_eq!(survived[0].evidence_kind, EvidenceKind::SyntaxInferred);
}

#[tokio::test]
async fn an_unresolvable_or_ambiguous_endpoint_is_named_back_not_dropped() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    let rev = GitRevision("rev1".into());
    codegraph::upsert_file_graph(&pool, repo, &rev, "src/api.rs", DISPATCH)
        .await
        .unwrap();
    // Two files defining the same simple name make that name ambiguous.
    codegraph::upsert_file_graph(
        &pool,
        repo,
        &rev,
        "src/other.rs",
        "pub fn caller() -> u32 { 0 }\n",
    )
    .await
    .unwrap();

    let results = codegraph::assert_agent_edges(
        &pool,
        repo,
        &rev,
        &[
            assertion("handle_create_user", "no_such_symbol_anywhere"),
            assertion("caller", "user_service_create"),
        ],
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 2, "one result per assertion, in order");
    match &results[0] {
        codegraph::AssertionResult::Unresolved { symbol, .. } => {
            // The model must be told WHICH name it got wrong, or it can only
            // resend everything and hope.
            assert_eq!(symbol, "no_such_symbol_anywhere");
        }
        other => panic!("{other:?}"),
    }
    match &results[1] {
        codegraph::AssertionResult::Ambiguous { symbol, candidates } => {
            assert_eq!(symbol, "caller");
            assert_eq!(candidates.len(), 2, "{candidates:?}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_reparse_keeps_edges_it_did_not_produce() {
    // A reparse is authoritative only over the layer it produces. It used to
    // delete EVERY edge out of the file, so with the live watcher armed an
    // agent's assertion — and every LSP edge already shipped — evaporated the
    // next time the file was saved.
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    let rev = GitRevision("rev1".into());
    codegraph::upsert_file_graph(&pool, repo, &rev, "src/api.rs", DISPATCH)
        .await
        .unwrap();
    let nodes = codegraph::nodes(&pool, repo).await.unwrap();
    let handler = key_of(&nodes, "handle_create_user");
    let service = key_of(&nodes, "user_service_create");
    codegraph::assert_agent_edges(
        &pool,
        repo,
        &rev,
        &[assertion("handle_create_user", "user_service_create")],
    )
    .await
    .unwrap();
    assert_eq!(edges_between(&pool, repo, handler, service).await.len(), 1);

    // The same file, saved again with an unrelated edit.
    codegraph::upsert_file_graph(
        &pool,
        repo,
        &rev,
        "src/api.rs",
        &format!("{DISPATCH}\npub fn extra() -> u32 {{ 2 }}\n"),
    )
    .await
    .unwrap();

    let survived = edges_between(&pool, repo, handler, service).await;
    assert_eq!(survived.len(), 1, "the assertion did not survive a reparse");
    assert_eq!(survived[0].evidence_kind, EvidenceKind::AgentAsserted);
    // And the syntax layer is still replaced wholesale, not accumulated.
    let caller = key_of(&nodes, "caller");
    assert_eq!(edges_between(&pool, repo, caller, handler).await.len(), 1);
}
