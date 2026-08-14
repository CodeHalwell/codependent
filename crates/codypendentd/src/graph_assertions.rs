//! The `graph.assert_edge` write seam — the assembly half of the agent lever on
//! code-graph construction.
//!
//! The runtime declares what it needs
//! ([`CodeGraphAssertions`](codypendent_runtime::tools::CodeGraphAssertions)) and
//! the knowledge crate owns the engine
//! ([`codegraph::assert_agent_edges`]); this is only the binding, and it is
//! deliberately thin — the resolution of a model-written name to a node, the
//! confidence rule that stops an assertion displacing a compiler-resolved fact,
//! and the per-assertion disposition all live in the store, tested there.
//!
//! # Why it is a separate type from [`PoolCodeGraph`](crate::scan::PoolCodeGraph)
//!
//! `CodeGraphQueries` is documented, dispositioned by policy, and reasoned about
//! everywhere as a pure READ of a derived projection. Hanging a write method off
//! it would make each of those statements quietly false. Two seams, one pool,
//! one identity derivation ([`repository_id_for`](crate::scan::repository_id_for)),
//! which is the part that actually has to agree.

use std::path::Path;

use async_trait::async_trait;
use codypendent_knowledge::codegraph::{self, AgentEdgeAssertion, AssertionResult};
use codypendent_knowledge::EvidenceRef;
use codypendent_runtime::tools::{CodeGraphAssertions, EdgeAssertionOutcome, EdgeAssertionRequest};
use sqlx::SqlitePool;

/// Backs `graph.assert_edge`: folds an agent's claims into the repository's code
/// graph at `EvidenceKind::AgentAsserted`.
#[derive(Clone)]
pub struct PoolCodeGraphAssertions {
    pool: SqlitePool,
}

impl PoolCodeGraphAssertions {
    /// Bind the seam to the daemon's pool.
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl CodeGraphAssertions for PoolCodeGraphAssertions {
    async fn assert_edges(
        &self,
        request: EdgeAssertionRequest<'_>,
    ) -> Result<Vec<EdgeAssertionOutcome>, String> {
        // The SAME derivation the startup scan wrote the graph under. The run
        // hands us its durable repository identity (never its worktree — see
        // `RunContext::repository_identity`); resolving it any other way is how a
        // caller ends up writing under an id nothing else ever reads.
        let repository = crate::scan::repository_id_for(request.repository);
        let revision = crate::scan::head_revision(request.repository);

        let assertions: Vec<AgentEdgeAssertion> = request
            .edges
            .iter()
            .map(|edge| AgentEdgeAssertion {
                from_symbol: edge.from.clone(),
                to_symbol: edge.to.clone(),
                relation: edge.relation,
                // The rationale travels ONTO the edge row, with the run that
                // said it. An agent-asserted edge whose provenance is only "some
                // run" cannot be reviewed by the user it was asserted for.
                evidence: Some(EvidenceRef::AgentAssertion {
                    session_id: request.session_id,
                    run_id: request.run_id,
                    rationale: edge.rationale.clone(),
                }),
            })
            .collect();

        let results = codegraph::assert_agent_edges(&self.pool, repository, &revision, &assertions)
            .await
            .map_err(|error| error.to_string())?;
        Ok(results.iter().map(project).collect())
    }
}

/// One store disposition as the runtime's own. A pure renaming across the crate
/// boundary: the runtime cannot name `codegraph`'s enum in its tool signatures
/// without making the knowledge crate part of the tool contract, and the two
/// must stay free to gain variants independently.
fn project(result: &AssertionResult) -> EdgeAssertionOutcome {
    match result {
        AssertionResult::Applied => EdgeAssertionOutcome::Applied,
        AssertionResult::Superseded {
            previous,
            previous_confidence,
        } => EdgeAssertionOutcome::Superseded {
            previous: *previous,
            previous_confidence: *previous_confidence,
        },
        AssertionResult::Outranked {
            existing,
            existing_confidence,
        } => EdgeAssertionOutcome::Outranked {
            existing: *existing,
            existing_confidence: *existing_confidence,
        },
        AssertionResult::Unresolved { symbol, candidates } => EdgeAssertionOutcome::Unresolved {
            symbol: symbol.clone(),
            candidates: candidates.clone(),
        },
        AssertionResult::Ambiguous { symbol, candidates } => EdgeAssertionOutcome::Ambiguous {
            symbol: symbol.clone(),
            candidates: candidates.clone(),
        },
    }
}

/// The repository identity an assertion is written under, exposed for the
/// integration test that proves a Build run's assertion lands under the checkout
/// the scan folded — not under the worktree the run executes in.
#[must_use]
pub fn assertion_repository_id(root: &Path) -> codypendent_protocol::RepositoryId {
    crate::scan::repository_id_for(root)
}
