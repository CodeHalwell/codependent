//! Code-graph seam (dependency inversion) — the daemon half of
//! `codypendent graph {build,status,show}`.
//!
//! `BuildCodeGraph`/`ReadCodeGraphStatus`/`ReadCodeGraph` act on the
//! syntax-layer code graph, which lives *outside* the session ledger and inside
//! `codypendent-knowledge` — a crate this one cannot name (the daemon is below
//! knowledge in the graph). So, exactly like
//! [`MemoryGateway`](crate::memory::MemoryGateway), the daemon declares the
//! seam and the `codypendentd` assembly fills it. The default-`None`
//! [`ServerState::code_graph`](crate::server::ServerState::code_graph) leaves it
//! unwired, and the lib-only / test server then rejects every graph command with
//! `graph.transport-unavailable`.
//!
//! # What this seam does NOT decide
//!
//! Which repository is in view. Every request carries a filesystem **path**, and
//! the implementation resolves it to a checkout with the daemon's own single
//! source of truth (`scan::repository_id_for`) before it touches a row. A client
//! cannot name a repository identity, so it cannot aim a query at a checkout it
//! did not name a path inside.
//!
//! That resolution is also why the *by-id* read is not a hole. `graph show
//! --node <id>` names a `code_nodes.id` directly, and the temptation is to fetch
//! it by primary key and filter the list path only. Four consecutive reviews of
//! this codebase found the same class of defect — a gate enforced where a list
//! is built and not where a row is fetched — so
//! [`CodeGraphReadRequest`](CodeGraphReadRequest) carries the repository into
//! **both** paths, and the implementation must answer "not in this repository"
//! and "no such node" with one identical [`node_not_found`] rejection.

use std::future::Future;
use std::pin::Pin;

use codypendent_protocol::{
    CodeGraphPage, CodeGraphQuery, CodeGraphScanReport, CodeGraphStatusView, CodypendentError,
};

/// Fold a repository's graph now. `repository` is a directory; the
/// implementation resolves it to the enclosing checkout.
#[derive(Debug, Clone)]
pub struct BuildCodeGraphRequest {
    pub repository: String,
}

/// Describe the stored graph without re-scanning.
#[derive(Debug, Clone)]
pub struct CodeGraphStatusRequest {
    pub repository: String,
}

/// List nodes/edges under a filter. The repository is separate from the query
/// precisely so it cannot be filtered away.
#[derive(Debug, Clone)]
pub struct CodeGraphReadRequest {
    pub repository: String,
    pub query: CodeGraphQuery,
}

/// The future a build returns. Boxed so the trait stays object-safe without an
/// `async-trait` dependency, matching [`crate::memory`]'s futures.
pub type CodeGraphBuildFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CodeGraphScanReport, CodypendentError>> + Send + 'a>>;

/// The future a status read returns.
pub type CodeGraphStatusFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CodeGraphStatusView, CodypendentError>> + Send + 'a>>;

/// The future a filtered read returns.
pub type CodeGraphReadFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CodeGraphPage, CodypendentError>> + Send + 'a>>;

/// The daemon's seam for the syntax-layer code graph.
///
/// Every method surfaces the underlying error verbatim as a `CommandRejected`.
/// A node the caller may not see and a node that does not exist must both
/// surface as [`node_not_found`] — the implementation is where that collapse
/// happens, and it is the whole point of the seam.
pub trait CodeGraphGateway: Send + Sync {
    fn build(&self, request: BuildCodeGraphRequest) -> CodeGraphBuildFuture<'_>;
    fn status(&self, request: CodeGraphStatusRequest) -> CodeGraphStatusFuture<'_>;
    fn read(&self, request: CodeGraphReadRequest) -> CodeGraphReadFuture<'_>;
}

/// The one rejection a by-id read may answer with.
///
/// Deliberately carries no id and no distinction: an id in another checkout, an
/// id that never existed, and a malformed id all produce this exact value, so
/// naming an id can never confirm it exists somewhere else. Constructed here
/// rather than at each call site so the three cannot drift apart.
#[must_use]
pub fn node_not_found() -> CodypendentError {
    CodypendentError::new(
        "graph.node-not-found",
        "no such node in this repository's code graph".to_string(),
        false,
    )
}

/// The refusal for a directory that is not inside a Git checkout. The code
/// graph anchors on checkouts by design — recursively folding a home or
/// projects directory would merge unrelated repositories into one graph.
#[must_use]
pub fn not_a_repository(path: &str) -> CodypendentError {
    CodypendentError::new(
        "graph.not-a-repository",
        format!("{path} is not inside a Git repository, so it has no code graph"),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The by-id refusal must be indistinguishable from the not-there refusal:
    /// same code, same message, no id echoed back. If a future edit adds the id
    /// to the message, `graph show --node <id>` becomes an oracle that confirms
    /// another checkout's node ids one probe at a time.
    #[test]
    fn the_node_refusal_leaks_nothing_about_which_case_it_was() {
        let a = node_not_found();
        let b = node_not_found();
        assert_eq!(a.code, b.code);
        assert_eq!(a.message, b.message);
        assert!(
            !a.message.contains(char::is_numeric),
            "the refusal must not echo an id: {}",
            a.message
        );
    }
}
