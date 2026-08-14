//! Daemon-side handlers for `codypendent graph {build,status,show}` — the
//! assembly half of [`codypendent_daemon::codegraph`].
//!
//! # The bug this module closes
//!
//! The code graph was folded only as a side effect of opening a session or
//! starting a run. No command built it, no command described it, and
//! `index rebuild` — whose name reads as "build the index, graph included" —
//! explicitly does not touch it. On a mixed repository the graph came out
//! holding two nodes from one file while thousands of Python and TSX files
//! contributed nothing, and every surface reported that as a plain zero.
//!
//! So the *report* here is the feature, not the fold. [`CodeGraphOps::build`]
//! answers "why is my graph this size?" from two measured sources and no third
//! opinion:
//!
//! * the scan's own [`ScanSummary`] — files seen, files a grammar covered,
//!   files folded, the histogram of extensions no grammar covers, and whether
//!   the file cap truncated the walk. These are the extractor's tallies from
//!   the walk being reported, not a re-derivation of it;
//! * the graph tables themselves, for what the repository now *holds*
//!   (nodes/edges, and the per-language split with edge counts the summary does
//!   not carry).
//!
//! It would have been easy to re-walk the tree here and subtract, or to keep a
//! local list of "extensions we support". Either is a second source of truth for
//! a fact the extractor already owns, and the first time a grammar is added the
//! copy keeps confidently reporting the old languages — a stale claim rendered
//! as a fact, which is the failure mode this command exists to end.
//! [`Language::ALL`] travels on the report for the same reason: the client
//! prints the roster it is told, never one it remembers.
//!
//! # Repository scoping
//!
//! Every request names a filesystem path. [`CodeGraphOps::anchor`] resolves it
//! through the daemon's own [`crate::scan::discover_repository_root`] and hashes
//! the result with [`crate::scan::repository_id_for`] — the same derivation the
//! scan writes under, so a query can never land on an identity nothing was
//! stored beneath (2026-08-13 review, F5). Nothing on the wire lets a client
//! name a repository identity.
//!
//! That scope is applied where the **rows are fetched**, on every path. The
//! by-id read (`graph show --node <id>`) carries `AND repository = ?` in the
//! same statement as the lookup and answers
//! [`node_not_found`](codypendent_daemon::codegraph::node_not_found) for "not
//! yours" and "not there" alike. A gate enforced only where a list is built is
//! not a gate.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use codypendent_daemon::codegraph::{
    node_not_found, not_a_repository, BuildCodeGraphRequest, CodeGraphBuildFuture,
    CodeGraphGateway, CodeGraphReadFuture, CodeGraphReadRequest, CodeGraphStatusFuture,
    CodeGraphStatusRequest,
};
use codypendent_knowledge::{GitRevision, Language, ScanSummary};
use codypendent_protocol::{
    CodeGraphEdgeView, CodeGraphGrammar, CodeGraphLanguageCount, CodeGraphNodeView, CodeGraphPage,
    CodeGraphQuery, CodeGraphScanReport, CodeGraphSkippedExtension, CodeGraphStatusView,
    CodeGraphTally, CodypendentError, RepositoryId,
};
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};
use tracing::{info, warn};

use crate::scan;

/// The largest page `graph show` serves, and the page a request naming no limit
/// gets. Bounded for the reason [`MAX_SESSION_EVENTS_PAGE`] is: a repository
/// graph runs to hundreds of thousands of nodes, and the 16 MiB frame limit is a
/// wall, not a policy. A client narrows its filter or pages instead.
///
/// [`MAX_SESSION_EVENTS_PAGE`]: codypendent_daemon::server
const MAX_GRAPH_PAGE: u32 = 500;

/// The assembly's [`CodeGraphGateway`], over the daemon's pool.
///
/// It shares the executor's two registries by `Arc` rather than keeping its own:
/// an on-demand build must count as *the* fold for this revision (so the next
/// run does not redundantly re-scan) and must arm the live watcher exactly as a
/// session-opened fold does (so an edit after `graph build` still lands). A
/// private copy of either map would give this daemon two disagreeing opinions
/// about what has been folded and two watchers per checkout.
#[derive(Clone)]
pub struct CodeGraphOps {
    pool: SqlitePool,
    scanned: Arc<Mutex<HashMap<RepositoryId, GitRevision>>>,
    watchers: Arc<Mutex<HashMap<RepositoryId, scan::RepositoryWatcher>>>,
}

impl CodeGraphOps {
    #[must_use]
    pub fn new(
        pool: SqlitePool,
        scanned: Arc<Mutex<HashMap<RepositoryId, GitRevision>>>,
        watchers: Arc<Mutex<HashMap<RepositoryId, scan::RepositoryWatcher>>>,
    ) -> Self {
        Self {
            pool,
            scanned,
            watchers,
        }
    }

    /// Resolve a client-supplied directory to `(checkout root, repository id)`.
    ///
    /// The single derivation for all three commands. A path outside a checkout
    /// is refused rather than folded: recursively indexing a home or projects
    /// directory merges unrelated repositories into one graph, and answering
    /// "0 nodes" for it would be the same uninformative silence this module
    /// exists to remove.
    fn anchor(repository: &str) -> Result<(PathBuf, RepositoryId), CodypendentError> {
        let path = PathBuf::from(repository);
        let Some(root) = scan::discover_repository_root(&path) else {
            return Err(not_a_repository(repository));
        };
        let id = scan::repository_id_for(&root);
        Ok((root, id))
    }

    /// Arm the live watcher for a freshly folded checkout, once.
    ///
    /// Mirrors the executor's own `ensure_watching`, against the SAME registry:
    /// the registry entry is the "already watching" flag, so a second build
    /// re-uses the first one's watcher instead of leaking a notify thread per
    /// invocation. A watcher that cannot be armed is logged and skipped — the
    /// build still succeeded, and the graph then simply refreshes only when the
    /// revision moves.
    fn ensure_watching(&self, repository: RepositoryId, root: &Path) {
        let mut watchers = self.watchers.lock().expect("code-graph watcher registry");
        if watchers.contains_key(&repository) {
            return;
        }
        match scan::arm_watcher(self.pool.clone(), repository, root) {
            Ok(watcher) => {
                watchers.insert(repository, watcher);
            }
            Err(error) => warn!(
                %repository,
                %error,
                "could not arm the code-graph watcher after an on-demand build"
            ),
        }
    }
}

impl CodeGraphGateway for CodeGraphOps {
    fn build(&self, request: BuildCodeGraphRequest) -> CodeGraphBuildFuture<'_> {
        let host = self.clone();
        Box::pin(async move {
            let (root, repository) = Self::anchor(&request.repository)?;
            let started = Instant::now();

            // The graph's single writer gate — the same lock the startup scan,
            // the executor's warm-up, and the live watcher take. Without it an
            // on-demand build would race a session-opened one for the same
            // checkout: both clear, both rebuild, `database is locked`, and a
            // reader between one scanner's clear and its rebuild sees a torn
            // graph (2026-08-13 review, F6). Held across the whole scan.
            let guard = scan::lock_repository(repository).await;
            let scan_result = scan::scan_repository(&host.pool, repository, &root).await;
            drop(guard);
            let summary = scan_result.map_err(|error| {
                CodypendentError::new(
                    "graph.scan-failed",
                    format!("could not fold {}: {error}", root.display()),
                    true,
                )
            })?;

            // Record the fold under the SAME map the executor consults, and under
            // the SAME key it compares against (`scan::head_revision`), so the
            // next run for this checkout reuses this graph instead of rebuilding
            // it, and arm the watcher so edits made after the build still land.
            host.scanned
                .lock()
                .expect("scanned map lock")
                .insert(repository, scan::head_revision(&root));
            host.ensure_watching(repository, &root);

            // Per-language files/nodes/edges come from the graph tables rather
            // than the summary's file-only tally: they describe what the
            // repository now HOLDS, which is the question a build report is
            // read to answer, and they carry the edge counts the summary does
            // not track.
            let by_language = language_counts(&host.pool, repository).await?;
            let (nodes, edges) = totals(&host.pool, repository).await?;

            let report = CodeGraphScanReport {
                repository_root: root.display().to_string(),
                // The revision the rows actually CARRY, reported by the scan
                // rather than re-derived here: on a dirty checkout the scan folds
                // working-tree bytes and stamps `<HEAD>+workdir`, and a second
                // `git rev-parse` from this handler would print the bare commit
                // over rows that say otherwise.
                revision: summary.revision.clone(),
                files_walked: summary.files_seen as u64,
                files_supported: summary.files_supported as u64,
                files_folded: summary.files_folded as u64,
                files_unsupported: summary.files_skipped_unsupported as u64,
                files_ignored: summary.files_skipped_ignored as u64,
                nodes,
                edges,
                by_language,
                not_folded: rank_extensions(&summary),
                grammars: grammar_roster(),
                file_cap: summary.file_cap as u64,
                cap_hit: summary.truncated_by_cap,
                elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            };
            info!(
                %repository,
                revision = %report.revision,
                summary = %summary.headline(),
                "on-demand code-graph build complete"
            );
            Ok(report)
        })
    }

    fn status(&self, request: CodeGraphStatusRequest) -> CodeGraphStatusFuture<'_> {
        let host = self.clone();
        Box::pin(async move {
            let (root, repository) = Self::anchor(&request.repository)?;
            let (nodes, edges) = totals(&host.pool, repository).await?;
            let by_language = language_counts(&host.pool, repository).await?;
            let by_kind = tallies(
                &host.pool,
                repository,
                "SELECT kind AS label, COUNT(*) AS count FROM code_nodes \
                 WHERE repository = ? GROUP BY kind ORDER BY count DESC, label ASC",
            )
            .await?;
            let revisions = tallies(
                &host.pool,
                repository,
                "SELECT revision AS label, COUNT(*) AS count FROM code_nodes \
                 WHERE repository = ? GROUP BY revision ORDER BY count DESC, label ASC",
            )
            .await?;
            let files: i64 = sqlx::query_scalar(
                "SELECT COUNT(DISTINCT source_path) FROM code_nodes \
                 WHERE repository = ? AND source_path IS NOT NULL",
            )
            .bind(repository.to_string())
            .fetch_one(&host.pool)
            .await
            .map_err(store_error)?;

            let head = scan::head_revision(&root);
            let working_tree_dirty = scan::working_tree_dirty(&root);
            let (stale, stale_reason) = staleness(nodes, &revisions, &head.0, working_tree_dirty);

            Ok(CodeGraphStatusView {
                repository_root: root.display().to_string(),
                nodes,
                edges,
                files: files.max(0) as u64,
                by_language,
                by_kind,
                revisions,
                head_revision: head.0,
                working_tree_dirty,
                stale,
                stale_reason,
            })
        })
    }

    fn read(&self, request: CodeGraphReadRequest) -> CodeGraphReadFuture<'_> {
        let host = self.clone();
        Box::pin(async move {
            let (_root, repository) = Self::anchor(&request.repository)?;
            let query = request.query;
            let limit = if query.limit == 0 {
                MAX_GRAPH_PAGE
            } else {
                query.limit.min(MAX_GRAPH_PAGE)
            };
            // Asking for neither is a client bug, not a legal "select nothing":
            // answer it as "everything", so an omitted flag never renders as an
            // empty graph.
            let (want_nodes, want_edges) = match (query.include_nodes, query.include_edges) {
                (false, false) => (true, false),
                pair => pair,
            };

            let nodes = select_nodes(&host.pool, repository, &query, limit).await?;
            let total_nodes = count_nodes(&host.pool, repository, &query).await?;
            let (edges, total_edges) = if want_edges {
                let ids: Vec<String> = nodes.iter().map(|node| node.id.clone()).collect();
                select_edges(&host.pool, repository, &ids, limit).await?
            } else {
                (Vec::new(), 0)
            };

            Ok(CodeGraphPage {
                nodes: if want_nodes { nodes } else { Vec::new() },
                edges,
                total_nodes,
                total_edges,
                limit,
            })
        })
    }
}

// ---------------------------------------------------------------------------
// Projecting the scan's own summary onto the wire
// ---------------------------------------------------------------------------

/// The scan's unsupported-extension histogram, ranked most-files-first.
///
/// The summary's map is already bounded by the extractor; this only imposes a
/// **total** order on it (files descending, then extension ascending) so two
/// builds over one tree render identically instead of reordering ties.
fn rank_extensions(summary: &ScanSummary) -> Vec<CodeGraphSkippedExtension> {
    let mut ranked: Vec<CodeGraphSkippedExtension> = summary
        .unsupported_by_extension
        .iter()
        .map(|(extension, files)| CodeGraphSkippedExtension {
            extension: extension.clone(),
            files: *files as u64,
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.files
            .cmp(&a.files)
            .then_with(|| a.extension.cmp(&b.extension))
    });
    ranked
}

/// The grammars this build carries, taken from the extractor's own roster.
///
/// Sent on every build report so the client can print "these extensions were
/// seen, these are the ones that would have worked" without keeping a copy of
/// the roster that goes stale the first time a grammar is added.
fn grammar_roster() -> Vec<CodeGraphGrammar> {
    Language::ALL
        .iter()
        .map(|language| CodeGraphGrammar {
            language: language.as_str().to_string(),
            extensions: language
                .extensions()
                .iter()
                .map(|extension| (*extension).to_string())
                .collect(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Reads over the stored graph. Every one is repository-scoped in SQL.
// ---------------------------------------------------------------------------

/// A store failure, as a wire error. Retryable: a locked database or a busy
/// scan is a "try again", not a permanent refusal.
fn store_error(error: sqlx::Error) -> CodypendentError {
    CodypendentError::new("graph.store-error", error.to_string(), true)
}

/// `(nodes, edges)` for one repository. Edges are scoped by joining `from_node`
/// back to the owning repository — an edge has no repository column of its own.
async fn totals(
    pool: &SqlitePool,
    repository: RepositoryId,
) -> Result<(u64, u64), CodypendentError> {
    let repo = repository.to_string();
    let nodes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM code_nodes WHERE repository = ?")
        .bind(&repo)
        .fetch_one(pool)
        .await
        .map_err(store_error)?;
    let edges: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM code_edges e JOIN code_nodes n ON e.from_node = n.id \
         WHERE n.repository = ?",
    )
    .bind(&repo)
    .fetch_one(pool)
    .await
    .map_err(store_error)?;
    Ok((nodes.max(0) as u64, edges.max(0) as u64))
}

/// Files/nodes/edges per stored language, most nodes first.
async fn language_counts(
    pool: &SqlitePool,
    repository: RepositoryId,
) -> Result<Vec<CodeGraphLanguageCount>, CodypendentError> {
    let repo = repository.to_string();
    let node_rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT language, COUNT(DISTINCT source_path), COUNT(*) FROM code_nodes \
         WHERE repository = ? GROUP BY language",
    )
    .bind(&repo)
    .fetch_all(pool)
    .await
    .map_err(store_error)?;
    let edge_rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT n.language, COUNT(*) FROM code_edges e JOIN code_nodes n ON e.from_node = n.id \
         WHERE n.repository = ? GROUP BY n.language",
    )
    .bind(&repo)
    .fetch_all(pool)
    .await
    .map_err(store_error)?;
    let edges_by_language: HashMap<String, i64> = edge_rows.into_iter().collect();

    let mut counts: Vec<CodeGraphLanguageCount> = node_rows
        .into_iter()
        .map(|(language, files, nodes)| CodeGraphLanguageCount {
            edges: edges_by_language
                .get(&language)
                .copied()
                .unwrap_or_default()
                .max(0) as u64,
            language,
            files: files.max(0) as u64,
            nodes: nodes.max(0) as u64,
        })
        .collect();
    counts.sort_by(|a, b| {
        b.nodes
            .cmp(&a.nodes)
            .then_with(|| a.language.cmp(&b.language))
    });
    Ok(counts)
}

/// Run a `SELECT <label>, <count>` grouped query bound to one repository.
async fn tallies(
    pool: &SqlitePool,
    repository: RepositoryId,
    sql: &str,
) -> Result<Vec<CodeGraphTally>, CodypendentError> {
    let rows: Vec<(String, i64)> = sqlx::query_as(sql)
        .bind(repository.to_string())
        .fetch_all(pool)
        .await
        .map_err(store_error)?;
    Ok(rows
        .into_iter()
        .map(|(label, count)| CodeGraphTally {
            label,
            count: count.max(0) as u64,
        })
        .collect())
}

/// Decide whether the stored graph still describes the working tree, and say why
/// in one sentence when it does not.
///
/// Three ways a graph goes stale, in the order a user cares about them:
/// it is empty; it was folded at a commit that is no longer `HEAD`; or the tree
/// has uncommitted edits and nothing in the graph carries the `+workdir` stamp
/// the live watcher writes for an uncommitted fold.
fn staleness(
    nodes: u64,
    revisions: &[CodeGraphTally],
    head: &str,
    working_tree_dirty: bool,
) -> (bool, Option<String>) {
    if nodes == 0 {
        return (
            true,
            Some("the graph is empty — nothing has been folded for this repository".to_string()),
        );
    }
    let workdir_stamp = format!("{head}+workdir");
    let mut foreign: Vec<&str> = revisions
        .iter()
        .map(|tally| tally.label.as_str())
        .filter(|revision| *revision != head && *revision != workdir_stamp)
        .collect();
    foreign.sort_unstable();
    if let Some(revision) = foreign.first() {
        return (
            true,
            Some(format!(
                "folded at {revision}, but HEAD is now {head} — run `codypendent graph build`"
            )),
        );
    }
    if working_tree_dirty
        && !revisions
            .iter()
            .any(|tally| tally.label.ends_with("+workdir"))
    {
        return (
            true,
            Some(
                "the working tree has uncommitted changes that no fold has covered yet".to_string(),
            ),
        );
    }
    (false, None)
}

/// Bind the repository scope and every narrowing filter onto a node query.
///
/// The repository predicate is pushed FIRST and unconditionally, on both the
/// list path and the by-id path, because it is the only one that is a
/// permission rather than a preference.
fn push_node_filters<'q>(
    builder: &mut QueryBuilder<'q, Sqlite>,
    repository: RepositoryId,
    query: &'q CodeGraphQuery,
) {
    builder
        .push(" WHERE repository = ")
        .push_bind(repository.to_string());
    if let Some(node_id) = &query.node_id {
        builder.push(" AND id = ").push_bind(node_id.as_str());
    }
    if let Some(path) = &query.path {
        builder
            .push(" AND source_path LIKE ")
            .push_bind(format!("{}%", escape_like(path)))
            .push(" ESCAPE '\\'");
    }
    if let Some(language) = &query.language {
        builder
            .push(" AND language = ")
            .push_bind(language.as_str());
    }
    if let Some(kind) = &query.kind {
        builder.push(" AND kind = ").push_bind(kind.as_str());
    }
    if let Some(name) = &query.name {
        builder
            .push(" AND qualified_name LIKE ")
            .push_bind(format!("%{}%", escape_like(name)))
            .push(" ESCAPE '\\'");
    }
}

/// Neutralize `LIKE`'s wildcards in user text, so a path or name containing `%`
/// narrows to itself instead of matching everything.
fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// One page of nodes matching `query`, repository-scoped.
///
/// A `node_id` that matches nothing **in this repository** returns the same
/// [`node_not_found`] as one that exists nowhere: the id is bound into the same
/// `WHERE repository = ? AND id = ?` as the scope, so the two cases are
/// literally the same empty result set and cannot be told apart downstream.
async fn select_nodes(
    pool: &SqlitePool,
    repository: RepositoryId,
    query: &CodeGraphQuery,
    limit: u32,
) -> Result<Vec<CodeGraphNodeView>, CodypendentError> {
    let mut builder = QueryBuilder::new(
        "SELECT id, language, package, source_path, qualified_name, kind, revision FROM code_nodes",
    );
    push_node_filters(&mut builder, repository, query);
    builder
        .push(" ORDER BY source_path ASC, qualified_name ASC, id ASC LIMIT ")
        .push_bind(i64::from(limit));
    let rows = builder.build().fetch_all(pool).await.map_err(store_error)?;
    let nodes: Vec<CodeGraphNodeView> = rows
        .into_iter()
        .map(|row| CodeGraphNodeView {
            id: row.get::<String, _>("id"),
            language: row.get::<String, _>("language"),
            package: row.get::<Option<String>, _>("package"),
            source_path: row.get::<Option<String>, _>("source_path"),
            qualified_name: row.get::<String, _>("qualified_name"),
            kind: row.get::<String, _>("kind"),
            revision: row.get::<String, _>("revision"),
        })
        .collect();
    if query.node_id.is_some() && nodes.is_empty() {
        return Err(node_not_found());
    }
    Ok(nodes)
}

/// How many nodes match `query` before the limit, so a client can say "showing
/// 50 of 812". Shares [`push_node_filters`] with the page itself, so the count
/// and the page can never be scoped differently.
async fn count_nodes(
    pool: &SqlitePool,
    repository: RepositoryId,
    query: &CodeGraphQuery,
) -> Result<u64, CodypendentError> {
    let mut builder = QueryBuilder::new("SELECT COUNT(*) FROM code_nodes");
    push_node_filters(&mut builder, repository, query);
    let row = builder.build().fetch_one(pool).await.map_err(store_error)?;
    Ok(row.get::<i64, _>(0).max(0) as u64)
}

/// The edges incident to `node_ids`, with both endpoints named.
///
/// **Both** endpoints are joined back to `code_nodes` and both are constrained
/// to the repository. Scoping only `from_node` — which is what
/// [`codypendent_knowledge::codegraph::edges`] does for its own whole-repository
/// read, correctly, since there the two cannot differ — would here let an edge
/// name a node outside the caller's repository in `to_name`, which is precisely
/// the leak the by-id gate exists to prevent, arriving by a different door.
async fn select_edges(
    pool: &SqlitePool,
    repository: RepositoryId,
    node_ids: &[String],
    limit: u32,
) -> Result<(Vec<CodeGraphEdgeView>, u64), CodypendentError> {
    if node_ids.is_empty() {
        return Ok((Vec::new(), 0));
    }
    // The count runs first, over the SAME predicate builder as the page, so
    // `total_edges` is the real total rather than "however many the limit let
    // through". Reporting a clamped page size as the total is the same class of
    // confidently-wrong number as the bare zero this whole command family
    // exists to replace.
    let mut counter = QueryBuilder::new(
        "SELECT COUNT(*) FROM code_edges e \
         JOIN code_nodes f ON e.from_node = f.id \
         JOIN code_nodes t ON e.to_node = t.id",
    );
    push_edge_filters(&mut counter, repository, node_ids);
    let total = counter
        .build()
        .fetch_one(pool)
        .await
        .map_err(store_error)?
        .get::<i64, _>(0)
        .max(0) as u64;

    let mut builder = QueryBuilder::new(
        "SELECT e.from_node, f.qualified_name AS from_name, e.to_node, t.qualified_name AS to_name, \
                e.relation, e.confidence, e.evidence_kind, e.revision \
         FROM code_edges e \
         JOIN code_nodes f ON e.from_node = f.id \
         JOIN code_nodes t ON e.to_node = t.id",
    );
    push_edge_filters(&mut builder, repository, node_ids);
    builder
        .push(" ORDER BY f.qualified_name ASC, e.relation ASC, t.qualified_name ASC LIMIT ")
        .push_bind(i64::from(limit));

    let rows = builder.build().fetch_all(pool).await.map_err(store_error)?;
    let edges: Vec<CodeGraphEdgeView> = rows
        .into_iter()
        .map(|row| CodeGraphEdgeView {
            from_id: row.get::<String, _>("from_node"),
            from_name: row.get::<String, _>("from_name"),
            to_id: row.get::<String, _>("to_node"),
            to_name: row.get::<String, _>("to_name"),
            relation: row.get::<String, _>("relation"),
            confidence: row.get::<f64, _>("confidence") as f32,
            evidence_kind: row.get::<String, _>("evidence_kind"),
            revision: row.get::<String, _>("revision"),
        })
        .collect();
    Ok((edges, total))
}

/// Bind the repository scope and the incident-node predicate onto an edge query.
///
/// Shared by the page and its count so the two can never be scoped differently —
/// the same reason [`push_node_filters`] is shared.
///
/// **Both** endpoints are constrained to the repository. Scoping only
/// `from_node` — which is what [`codypendent_knowledge::codegraph::edges`] does
/// for its own whole-repository read, correctly, since there the two cannot
/// differ — would here let an edge name a node outside the caller's repository
/// in `to_name`, which is precisely the leak the by-id gate exists to prevent,
/// arriving through a different door.
fn push_edge_filters(
    builder: &mut QueryBuilder<'_, Sqlite>,
    repository: RepositoryId,
    node_ids: &[String],
) {
    let repo = repository.to_string();
    builder
        .push(" WHERE f.repository = ")
        .push_bind(repo.clone());
    builder.push(" AND t.repository = ").push_bind(repo);
    builder.push(" AND (e.from_node IN (");
    let mut separated = builder.separated(", ");
    for id in node_ids {
        separated.push_bind(id.clone());
    }
    builder.push(") OR e.to_node IN (");
    let mut separated = builder.separated(", ");
    for id in node_ids {
        separated.push_bind(id.clone());
    }
    builder.push("))");
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::process::Command;

    use codypendent_knowledge::codegraph;

    fn init_repo(path: &Path) {
        for args in [
            vec!["init", "--quiet"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "t"],
        ] {
            let status = Command::new("git")
                .current_dir(path)
                .args(&args)
                .status()
                .expect("run git");
            assert!(status.success(), "git {args:?} failed");
        }
    }

    /// A migrated pool on disk. Not `:memory:` — `open_database` treats its
    /// argument as a path, so an in-memory URL would give each connection in the
    /// pool its own empty database and no migrated tables at all.
    async fn pool(dir: &tempfile::TempDir) -> SqlitePool {
        codypendent_daemon::db::open_database(&dir.path().join("codypendent.db"))
            .await
            .expect("open db")
    }

    fn ops(pool: SqlitePool) -> CodeGraphOps {
        CodeGraphOps::new(
            pool,
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
        )
    }

    /// An extension histogram is ranked most-files-first with a total order, so
    /// two builds over one tree render identically instead of reordering ties.
    #[test]
    fn the_extension_histogram_ranks_by_files_then_name() {
        let mut summary = ScanSummary::default();
        for (extension, count) in [("go", 3), ("md", 3), ("toml", 9)] {
            for _ in 0..count {
                summary.record_unsupported(Path::new(&format!("a.{extension}")));
            }
        }
        let ranked = rank_extensions(&summary);
        let order: Vec<&str> = ranked.iter().map(|row| row.extension.as_str()).collect();
        assert_eq!(order, vec!["toml", "go", "md"]);
    }

    /// The roster travels on the report, straight from the extractor. A client
    /// that printed its own list would keep naming the old languages the first
    /// time a grammar is added — the stale-claim-as-fact failure this whole
    /// command family exists to end.
    #[test]
    fn the_grammar_roster_comes_from_the_extractor() {
        let roster = grammar_roster();
        assert_eq!(roster.len(), Language::ALL.len());
        let rust = roster
            .iter()
            .find(|grammar| grammar.language == "rust")
            .expect("rust is in the roster");
        assert_eq!(rust.extensions, vec!["rs".to_string()]);
    }

    /// A path or name carrying `LIKE`'s wildcards must narrow to itself. Without
    /// the escape, `graph show --path %` matched every file in the repository
    /// while claiming to be filtered.
    #[test]
    fn like_wildcards_in_user_text_are_neutralized() {
        assert_eq!(escape_like("a%b_c"), "a\\%b\\_c");
        assert_eq!(escape_like("100\\%"), "100\\\\\\%");
    }

    /// Staleness has to name the reason, because "stale: true" is the same
    /// unhelpful silence as "0 nodes".
    #[test]
    fn staleness_explains_each_of_its_three_causes() {
        let empty = staleness(0, &[], "abc", false);
        assert!(empty.0 && empty.1.expect("reason").contains("empty"));

        let old = staleness(
            5,
            &[CodeGraphTally {
                label: "old".to_string(),
                count: 5,
            }],
            "abc",
            false,
        );
        assert!(old.0);
        assert!(old.1.expect("reason").contains("HEAD is now abc"));

        let dirty = staleness(
            5,
            &[CodeGraphTally {
                label: "abc".to_string(),
                count: 5,
            }],
            "abc",
            true,
        );
        assert!(dirty.0);
        assert!(dirty.1.expect("reason").contains("uncommitted"));

        // A graph carrying the watcher's `+workdir` stamp already describes the
        // uncommitted tree, so a dirty tree alone must not report it stale.
        let folded_dirty = staleness(
            5,
            &[CodeGraphTally {
                label: "abc+workdir".to_string(),
                count: 5,
            }],
            "abc",
            true,
        );
        assert!(!folded_dirty.0, "{folded_dirty:?}");
        assert!(folded_dirty.1.is_none());
    }

    /// A directory outside a checkout is refused rather than answered "empty".
    /// Answering "0 nodes" for a home directory is the same misleading silence
    /// this command family exists to remove.
    #[tokio::test]
    async fn a_directory_outside_a_checkout_is_refused() {
        let plain = tempfile::tempdir().expect("tempdir");
        let data = tempfile::tempdir().expect("tempdir");
        let error = ops(pool(&data).await)
            .status(CodeGraphStatusRequest {
                repository: plain.path().display().to_string(),
            })
            .await
            .expect_err("a non-checkout has no graph");
        assert_eq!(error.code, "graph.not-a-repository");
    }

    /// **The gate.** `graph show --node <id>` must not become a way to read
    /// another repository's graph. A node id that exists — in a different
    /// checkout — is refused with byte-identical bytes to an id that exists
    /// nowhere, so naming an id can never confirm it.
    #[tokio::test]
    async fn a_node_id_from_another_repository_is_refused_identically_to_a_missing_one() {
        let data = tempfile::tempdir().expect("tempdir");
        let pool = pool(&data).await;
        let mine = tempfile::tempdir().expect("tempdir");
        let theirs = tempfile::tempdir().expect("tempdir");
        init_repo(mine.path());
        init_repo(theirs.path());
        let my_id = scan::repository_id_for(mine.path());
        let their_id = scan::repository_id_for(theirs.path());
        assert_ne!(my_id, their_id);

        let revision = GitRevision("r1".to_string());
        codegraph::upsert_file_graph(
            &pool,
            their_id,
            &revision,
            "src/lib.rs",
            "pub fn hidden() {}",
        )
        .await
        .expect("fold the other repository");
        let secret = select_nodes(&pool, their_id, &CodeGraphQuery::default(), 50)
            .await
            .expect("read the other repository directly")
            .into_iter()
            .find(|node| node.qualified_name.contains("hidden"))
            .expect("the other repository really does hold this node");

        let ops = ops(pool);
        let by_real_id = ops
            .read(CodeGraphReadRequest {
                repository: mine.path().display().to_string(),
                query: CodeGraphQuery {
                    node_id: Some(secret.id.clone()),
                    ..CodeGraphQuery::default()
                },
            })
            .await
            .expect_err("another repository's node must not be readable");
        let by_absent_id = ops
            .read(CodeGraphReadRequest {
                repository: mine.path().display().to_string(),
                query: CodeGraphQuery {
                    node_id: Some("00000000-0000-0000-0000-000000000000".to_string()),
                    ..CodeGraphQuery::default()
                },
            })
            .await
            .expect_err("an absent node must not be readable either");

        assert_eq!(by_real_id.code, by_absent_id.code);
        assert_eq!(by_real_id.message, by_absent_id.message);
        assert_eq!(by_real_id.code, "graph.node-not-found");
    }

    /// The same gate on the LIST path: a query with no filters at all sees only
    /// its own repository's rows.
    #[tokio::test]
    async fn an_unfiltered_list_is_still_repository_scoped() {
        let data = tempfile::tempdir().expect("tempdir");
        let pool = pool(&data).await;
        let mine = tempfile::tempdir().expect("tempdir");
        let theirs = tempfile::tempdir().expect("tempdir");
        init_repo(mine.path());
        init_repo(theirs.path());
        let revision = GitRevision("r1".to_string());
        codegraph::upsert_file_graph(
            &pool,
            scan::repository_id_for(theirs.path()),
            &revision,
            "src/lib.rs",
            "pub fn hidden() {}",
        )
        .await
        .expect("fold the other repository");

        let page = ops(pool)
            .read(CodeGraphReadRequest {
                repository: mine.path().display().to_string(),
                query: CodeGraphQuery::default(),
            })
            .await
            .expect("an empty graph is a legal answer");
        assert_eq!(page.total_nodes, 0, "{page:?}");
        assert!(page.nodes.is_empty(), "{page:?}");
    }

    /// The total a page reports must be the real total, not the clamped page
    /// size. "showing 1 of 1" for a graph holding twelve edges is the same
    /// species of confidently-wrong number as the bare zero.
    #[tokio::test]
    async fn an_edge_page_reports_the_true_total_not_the_page_size() {
        let data = tempfile::tempdir().expect("tempdir");
        let pool = pool(&data).await;
        let repo = tempfile::tempdir().expect("tempdir");
        init_repo(repo.path());
        codegraph::upsert_file_graph(
            &pool,
            scan::repository_id_for(repo.path()),
            &GitRevision("r1".to_string()),
            "src/lib.rs",
            "pub fn a() { b(); }\npub fn b() {}\n",
        )
        .await
        .expect("fold");

        let page = ops(pool)
            .read(CodeGraphReadRequest {
                repository: repo.path().display().to_string(),
                query: CodeGraphQuery {
                    include_edges: true,
                    include_nodes: true,
                    limit: 1,
                    ..CodeGraphQuery::default()
                },
            })
            .await
            .expect("read");
        assert_eq!(page.edges.len(), 1, "the page is clamped: {page:?}");
        assert!(
            page.total_edges > 1,
            "the total must be the real total, not the page size: {page:?}"
        );
    }

    /// The edge read is the other door into another repository's rows: an edge
    /// names TWO nodes and both names are rendered verbatim, so the scope has to
    /// hold on `to_node` as well as `from_node`.
    ///
    /// The cross-repository edge is inserted directly, because `upsert_file_graph`
    /// cannot produce one — which is exactly why the assertion needs the direct
    /// insert to be worth anything. `graph.assert_edge` resolves endpoints from
    /// client-supplied symbol keys, so "no writer can create this row" is a
    /// property of today's writers, not of the schema; the read must not depend
    /// on it.
    #[tokio::test]
    async fn an_edge_reaching_out_of_the_repository_is_not_returned() {
        let data = tempfile::tempdir().expect("tempdir");
        let pool = pool(&data).await;
        let mine = tempfile::tempdir().expect("tempdir");
        let theirs = tempfile::tempdir().expect("tempdir");
        init_repo(mine.path());
        init_repo(theirs.path());
        let revision = GitRevision("r1".to_string());
        for (dir, source) in [
            (mine.path(), "pub fn a() {}\n"),
            (theirs.path(), "pub fn hidden() {}\n"),
        ] {
            codegraph::upsert_file_graph(
                &pool,
                scan::repository_id_for(dir),
                &revision,
                "src/lib.rs",
                source,
            )
            .await
            .expect("fold");
        }
        let mine_id = scan::repository_id_for(mine.path());
        let theirs_id = scan::repository_id_for(theirs.path());
        let from: String = sqlx::query_scalar(
            "SELECT id FROM code_nodes WHERE repository = ? AND qualified_name = 'a'",
        )
        .bind(mine_id.to_string())
        .fetch_one(&pool)
        .await
        .expect("my node");
        let to: String = sqlx::query_scalar(
            "SELECT id FROM code_nodes WHERE repository = ? AND qualified_name = 'hidden'",
        )
        .bind(theirs_id.to_string())
        .fetch_one(&pool)
        .await
        .expect("their node");
        sqlx::query(
            "INSERT INTO code_edges (id, from_node, to_node, relation, confidence, \
             evidence_kind, evidence_artifact, revision, created_at) \
             VALUES ('cross', ?, ?, 'calls', 0.45, 'syntax_inferred', NULL, 'r1', '2026-08-14')",
        )
        .bind(&from)
        .bind(&to)
        .execute(&pool)
        .await
        .expect("insert a cross-repository edge");

        let page = ops(pool)
            .read(CodeGraphReadRequest {
                repository: mine.path().display().to_string(),
                query: CodeGraphQuery {
                    include_edges: true,
                    include_nodes: true,
                    ..CodeGraphQuery::default()
                },
            })
            .await
            .expect("read");
        assert!(
            !page
                .edges
                .iter()
                .any(|edge| edge.to_name.contains("hidden") || edge.from_name.contains("hidden")),
            "an edge must never name a node outside this repository: {page:?}"
        );
    }

    /// A build on a checkout holding nothing the extractor can parse must still
    /// come back with the numbers that explain it — files walked, none folded,
    /// the extensions responsible, and the roster that would have worked. This
    /// is the reporter's exact case, and the assertion that a bare `0` is not
    /// an acceptable answer to it.
    #[tokio::test]
    async fn a_build_with_no_foldable_files_reports_what_it_walked() {
        let repo = tempfile::tempdir().expect("tempdir");
        init_repo(repo.path());
        std::fs::create_dir_all(repo.path().join("app")).expect("mkdir");
        std::fs::write(repo.path().join("app/main.go"), "package main\n").expect("write");
        std::fs::write(repo.path().join("app/util.go"), "package main\n").expect("write");
        std::fs::write(repo.path().join("README"), "hello\n").expect("write");

        let data = tempfile::tempdir().expect("tempdir");
        let report = ops(pool(&data).await)
            .build(BuildCodeGraphRequest {
                repository: repo.path().display().to_string(),
            })
            .await
            .expect("an unfoldable repository is not an error");

        assert_eq!(report.nodes, 0);
        assert_eq!(report.files_folded, 0);
        assert_eq!(report.files_supported, 0);
        assert!(report.files_walked >= 3, "{report:?}");
        assert!(report.files_unsupported >= 3, "{report:?}");
        let go = report
            .not_folded
            .iter()
            .find(|row| row.extension == "go")
            .unwrap_or_else(|| panic!("the .go files must be named: {report:?}"));
        assert_eq!(go.files, 2);
        assert!(
            report.grammars.iter().any(|g| g.language == "python"),
            "the report must say what WOULD have folded: {report:?}"
        );
    }

    /// **A build immediately followed by a status must report `current`.**
    ///
    /// The full scan reads the WORKING TREE but used to stamp what it folded
    /// with the bare `HEAD` commit. `graph status` then saw a dirty tree and a
    /// graph carrying no `+workdir` stamp anywhere, and told the user — and the
    /// model — that the graph it had just built was stale, with a remedy of
    /// "run `codypendent graph build`" they had run one second earlier
    /// (2026-08-13 review, codegraph F6).
    #[tokio::test]
    async fn a_build_of_a_dirty_tree_is_not_reported_stale() {
        let repo = tempfile::tempdir().expect("tempdir");
        init_repo(repo.path());
        std::fs::create_dir_all(repo.path().join("src")).expect("mkdir");
        std::fs::write(repo.path().join("src/lib.rs"), "pub fn committed() {}\n").expect("write");
        for args in [vec!["add", "."], vec!["commit", "-qm", "seed"]] {
            assert!(Command::new("git")
                .current_dir(repo.path())
                .args(&args)
                .status()
                .expect("run git")
                .success());
        }
        // An uncommitted edit — the state a developer is in essentially always.
        std::fs::write(
            repo.path().join("src/lib.rs"),
            "pub fn committed() {}\npub fn uncommitted_symbol() {}\n",
        )
        .expect("write");

        let data = tempfile::tempdir().expect("tempdir");
        let ops = ops(pool(&data).await);
        let report = ops
            .build(BuildCodeGraphRequest {
                repository: repo.path().display().to_string(),
            })
            .await
            .expect("build");
        assert!(
            report.revision.ends_with("+workdir"),
            "a fold of an uncommitted tree must say so: {}",
            report.revision
        );

        let status = ops
            .status(CodeGraphStatusRequest {
                repository: repo.path().display().to_string(),
            })
            .await
            .expect("status");
        assert!(status.working_tree_dirty, "{status:?}");
        assert!(
            !status.stale,
            "a graph built one call ago is not stale: {:?}",
            status.stale_reason
        );
        assert_eq!(status.stale_reason, None);
        // The stamp is the commit plus the marker, not some third thing.
        assert_eq!(
            status
                .revisions
                .iter()
                .map(|tally| tally.label.as_str())
                .collect::<Vec<_>>(),
            vec![format!("{}+workdir", status.head_revision).as_str()],
        );
    }

    /// The other half: a CLEAN tree is stamped with the bare commit, so the
    /// revision the graph reports is the one a user can `git show`. Stamping
    /// `+workdir` unconditionally would be the same lie pointing the other way.
    #[tokio::test]
    async fn a_build_of_a_clean_tree_is_stamped_with_the_commit() {
        let repo = tempfile::tempdir().expect("tempdir");
        init_repo(repo.path());
        std::fs::create_dir_all(repo.path().join("src")).expect("mkdir");
        std::fs::write(repo.path().join("src/lib.rs"), "pub fn committed() {}\n").expect("write");
        for args in [vec!["add", "."], vec!["commit", "-qm", "seed"]] {
            assert!(Command::new("git")
                .current_dir(repo.path())
                .args(&args)
                .status()
                .expect("run git")
                .success());
        }

        let data = tempfile::tempdir().expect("tempdir");
        let ops = ops(pool(&data).await);
        let report = ops
            .build(BuildCodeGraphRequest {
                repository: repo.path().display().to_string(),
            })
            .await
            .expect("build");
        let status = ops
            .status(CodeGraphStatusRequest {
                repository: repo.path().display().to_string(),
            })
            .await
            .expect("status");
        assert!(!status.working_tree_dirty, "{status:?}");
        assert_eq!(report.revision, status.head_revision);
        assert!(!status.stale, "{:?}", status.stale_reason);
    }

    /// A repository the extractor does cover folds, and the report attributes
    /// the result per language. The mixed case the user actually reported:
    /// before the extractor widened, only `lib.rs` contributed and the Python
    /// and TSX files vanished silently.
    #[tokio::test]
    async fn a_mixed_repository_folds_every_covered_language() {
        let repo = tempfile::tempdir().expect("tempdir");
        init_repo(repo.path());
        std::fs::create_dir_all(repo.path().join("src")).expect("mkdir");
        std::fs::write(repo.path().join("src/lib.rs"), "pub fn one() {}\n").expect("write");
        std::fs::write(repo.path().join("src/app.py"), "def two():\n    pass\n").expect("write");
        std::fs::write(
            repo.path().join("src/ui.tsx"),
            "export function Three() { return <div/>; }\n",
        )
        .expect("write");
        std::fs::write(repo.path().join("notes.go"), "package main\n").expect("write");

        let data = tempfile::tempdir().expect("tempdir");
        let report = ops(pool(&data).await)
            .build(BuildCodeGraphRequest {
                repository: repo.path().display().to_string(),
            })
            .await
            .expect("build");

        assert_eq!(report.files_folded, 3, "{report:?}");
        let languages: Vec<&str> = report
            .by_language
            .iter()
            .map(|count| count.language.as_str())
            .collect();
        for expected in ["rust", "python", "tsx"] {
            assert!(
                languages.contains(&expected),
                "{expected} must be attributed: {report:?}"
            );
        }
        assert!(report.nodes > 0 && report.edges > 0, "{report:?}");
        assert_eq!(
            report
                .not_folded
                .iter()
                .find(|row| row.extension == "go")
                .map(|row| row.files),
            Some(1),
            "{report:?}"
        );
    }
}
