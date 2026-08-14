//! The agent lever on the code graph, end to end through the REAL assembly seam:
//! a run asserts a relation the parser cannot see, and the graph holds it.
//!
//! Drives [`PoolCodeGraphAssertions`] — the exact type
//! `RuntimeExecutor::spawn_run` injects into the agent runtime — over a real
//! pool and a real scanned checkout, so what this proves is what a run gets. The
//! `graph.assert_edge` tool's own argument parsing, bounds and rendering are
//! unit-tested in `codypendent_runtime::tools::graph`; what can only be proved
//! here is the pair that spans crates:
//!
//! 1. an assertion is written under the repository identity the SCAN folded the
//!    graph under, even when the run executes in a worktree elsewhere — the
//!    2026-08-13 review's F1 trap, which every knowledge-scoped call has fallen
//!    into at least once;
//! 2. an assertion cannot displace a higher-confidence edge, which is the
//!    property that makes it safe to let a model write here at all.

use std::path::Path;
use std::process::Command;

use codypendent_codypendentd::graph_assertions::PoolCodeGraphAssertions;
use codypendent_codypendentd::scan;
use codypendent_knowledge::db;
use codypendent_knowledge::{codegraph, CodeRelation, EvidenceKind, EvidenceRef};
use codypendent_protocol::{RunId, SessionId};
use codypendent_runtime::tools::{AssertedEdge, CodeGraphAssertions, EdgeAssertionRequest};
use sqlx::SqlitePool;

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// A checkout whose route table dispatches by NAME — the shape a parser cannot
/// follow, and therefore the shape this feature exists for.
fn seed_repository(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/routes.rs"),
        "pub fn table() -> Vec<(&'static str, &'static str)> {\n\
         \x20   vec![(\"POST /charge\", \"handle_charge\")]\n\
         }\n\
         \n\
         pub fn handle_charge(body: &str) -> u32 { body.len() as u32 }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/services.rs"),
        "pub struct ChargeService;\n\
         \n\
         impl ChargeService {\n\
         \x20   pub fn run(amount: u32) -> u32 { amount }\n\
         }\n",
    )
    .unwrap();
    git(root, &["init", "--quiet"]);
    git(root, &["config", "user.email", "probe@example.invalid"]);
    git(root, &["config", "user.name", "probe"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "--quiet", "-m", "seed"]);
}

fn edge(from: &str, to: &str, rationale: &str) -> AssertedEdge {
    AssertedEdge {
        from: from.to_string(),
        to: to.to_string(),
        relation: CodeRelation::Calls,
        rationale: rationale.to_string(),
    }
}

/// Every non-syntax edge in `repository`, as `(from, to, kind, confidence,
/// rationale)`. Read through the store's own `edges`/`nodes` accessors so the
/// test cannot drift from the schema.
async fn asserted_edges(
    pool: &SqlitePool,
    repository: codypendent_protocol::RepositoryId,
) -> Vec<(String, String, EvidenceKind, f32, Option<String>)> {
    let nodes = codegraph::nodes(pool, repository).await.expect("nodes");
    let name = |id: codypendent_protocol::CodeNodeId| {
        nodes
            .iter()
            .find(|node| node.id == id)
            .map_or_else(|| id.to_string(), |node| node.key.qualified_name.clone())
    };
    codegraph::edges(pool, repository)
        .await
        .expect("edges")
        .into_iter()
        .filter(|edge| edge.evidence_kind != EvidenceKind::SyntaxInferred)
        .map(|edge| {
            let rationale = match &edge.evidence {
                Some(EvidenceRef::AgentAssertion { rationale, .. }) => Some(rationale.clone()),
                _ => None,
            };
            (
                name(edge.from),
                name(edge.to),
                edge.evidence_kind,
                edge.confidence,
                rationale,
            )
        })
        .collect()
}

#[tokio::test]
async fn an_agent_assertion_lands_under_the_scanned_repository_and_carries_its_reason() {
    let checkout = tempfile::tempdir().expect("tempdir");
    seed_repository(checkout.path());
    let data = tempfile::tempdir().expect("tempdir");
    let pool = db::open(&data.path().join("codypendent.db"))
        .await
        .expect("open database");
    let root = checkout.path().canonicalize().expect("canonical checkout");
    let repository = scan::repository_id_for(&root);

    // The warm-up fold, exactly as the daemon runs it.
    {
        let _guard = scan::lock_repository(repository).await;
        scan::scan_repository(&pool, repository, &root)
            .await
            .expect("scan the checkout");
    }
    assert!(
        !codegraph::nodes(&pool, repository)
            .await
            .expect("nodes")
            .is_empty(),
        "the fold produced symbols to assert about"
    );

    // The trap: a Build run executes in a linked worktree OUTSIDE the checkout,
    // which resolves to a DIFFERENT RepositoryId and is deleted when the run
    // ends. The seam is handed the run's durable identity, and the assertion
    // must be readable from the checkout the scan wrote under.
    let worktree = tempfile::tempdir().expect("tempdir");
    assert_ne!(
        repository,
        scan::repository_id_for(worktree.path()),
        "the worktree really is a different identity — otherwise this proves nothing"
    );

    let seam = PoolCodeGraphAssertions::new(pool.clone());
    let edges = vec![
        edge(
            "handle_charge",
            "ChargeService::run",
            "src/routes.rs dispatches POST /charge to this handler by name",
        ),
        edge(
            "handle_charge",
            "NoSuchService",
            "a name the graph has never seen",
        ),
    ];
    let outcomes = seam
        .assert_edges(EdgeAssertionRequest {
            repository: &root,
            session_id: SessionId::new(),
            run_id: RunId::new(),
            edges: &edges,
        })
        .await
        .expect("the seam answers");

    assert_eq!(
        outcomes.len(),
        edges.len(),
        "one disposition per input edge"
    );
    assert!(outcomes[0].recorded(), "the real relation is written");
    assert!(
        !outcomes[1].recorded(),
        "an endpoint the graph has never seen is refused, never invented: {:?}",
        outcomes[1]
    );

    let stored = asserted_edges(&pool, repository).await;
    assert_eq!(
        stored.len(),
        1,
        "exactly the asserted edge, under the SCANNED repository: {stored:?}"
    );
    let (from, to, kind, confidence, rationale) = &stored[0];
    assert_eq!(
        (from.as_str(), to.as_str()),
        ("handle_charge", "ChargeService::run")
    );
    assert_eq!(
        *kind,
        EvidenceKind::AgentAsserted,
        "distinguishable from a parsed edge"
    );
    assert!((*confidence - codypendent_knowledge::AGENT_ASSERTED_CONFIDENCE).abs() < f32::EPSILON);
    assert_eq!(
        rationale.as_deref(),
        Some("src/routes.rs dispatches POST /charge to this handler by name"),
        "a user can see WHY it was asserted, not just that it was"
    );

    // Nothing landed under the worktree identity — the failure mode this guards
    // reports as an empty result everywhere, never as an error.
    assert!(
        asserted_edges(&pool, scan::repository_id_for(worktree.path()))
            .await
            .is_empty(),
        "no row may be written under the throwaway worktree id"
    );

    // And the graph now ANSWERS with it: the assertion is not a side table.
    let answer = codegraph::answer(
        &pool,
        repository,
        &codegraph::GraphQuestion::CallersOf {
            symbol: "ChargeService::run".to_string(),
        },
    )
    .await
    .expect("query");
    assert!(
        answer
            .hits
            .iter()
            .any(|hit| hit.qualified_name == "handle_charge"),
        "the next graph question sees what the agent taught it: {answer:?}"
    );
}

#[tokio::test]
async fn an_assertion_never_displaces_a_more_confident_edge() {
    let checkout = tempfile::tempdir().expect("tempdir");
    seed_repository(checkout.path());
    let data = tempfile::tempdir().expect("tempdir");
    let pool = db::open(&data.path().join("codypendent.db"))
        .await
        .expect("open database");
    let root = checkout.path().canonicalize().expect("canonical checkout");
    let repository = scan::repository_id_for(&root);
    let revision = scan::head_revision(&root);
    {
        let _guard = scan::lock_repository(repository).await;
        scan::scan_repository(&pool, repository, &root)
            .await
            .expect("scan");
    }

    // A resolved fact for the triple the agent is about to claim. Written through
    // the same semantic path an LSP/compiler pass uses, so this is the real
    // contest and not a hand-built row.
    let nodes = codegraph::nodes(&pool, repository).await.expect("nodes");
    let key = |name: &str| {
        nodes
            .iter()
            .find(|node| node.key.qualified_name == name)
            .unwrap_or_else(|| panic!("{name} is in the graph"))
            .key
            .stable_key()
    };
    let semantic = codegraph::SemanticEdge {
        from_symbol_key: key("handle_charge"),
        to_symbol_key: key("ChargeService::run"),
        relation: CodeRelation::Calls,
        evidence_kind: EvidenceKind::CompilerResolved,
        confidence: codypendent_knowledge::COMPILER_RESOLVED_CONFIDENCE,
        evidence: None,
    };
    codegraph::upsert_semantic_edges(&pool, repository, &revision, &[semantic])
        .await
        .expect("seed the resolved edge");

    let seam = PoolCodeGraphAssertions::new(pool.clone());
    let claim = vec![edge(
        "handle_charge",
        "ChargeService::run",
        "the model's reading of the route table",
    )];
    let outcomes = seam
        .assert_edges(EdgeAssertionRequest {
            repository: &root,
            session_id: SessionId::new(),
            run_id: RunId::new(),
            edges: &claim,
        })
        .await
        .expect("the seam answers");

    assert!(
        !outcomes[0].recorded(),
        "a model's claim must not overwrite a compiler-resolved fact: {:?}",
        outcomes[0]
    );

    let stored = asserted_edges(&pool, repository).await;
    assert_eq!(
        stored.len(),
        1,
        "no second row was appended beside it: {stored:?}"
    );
    assert_eq!(
        stored[0].2,
        EvidenceKind::CompilerResolved,
        "the resolved edge is still the one the graph holds"
    );
}
