//! Client-facing views over the syntax-layer code graph — the wire half of
//! `codypendent graph {build,status,show}`.
//!
//! # Why these types exist at all
//!
//! Before them the code graph was folded only as a *side effect* of opening a
//! session or starting a run, its counts went to a `tracing::info!` nobody
//! reads, and there was no command that built it or described it. On a mixed
//! repository (Python + TSX + Rust) the graph came out holding two nodes from
//! one file and said nothing about the thousands of files it had walked past.
//! An empty graph that explains nothing is the defect these types close: every
//! field below exists so a client can answer **"why is my graph this size?"**
//! from one command's reply.
//!
//! # Projections, not mirrors
//!
//! The authoritative graph types live in `codypendent-knowledge` (`CodeNode`,
//! `CodeEdge`, `SymbolKey`, `LanguageId`). Protocol is a leaf crate and must
//! not grow a second copy of that type graph — see [`crate::memory`]'s module
//! doc for the duplication this codebase has already paid for. So these are
//! flat projections: stored scalars as strings, counts as `u64`, and no
//! reconstruction of an id a client cannot type.
//!
//! # Every count comes from the walk that happened
//!
//! [`CodeGraphScanReport`]'s file counts and its
//! [`not_folded`](CodeGraphScanReport::not_folded) histogram are the extractor's
//! own tallies from the scan being reported, and
//! [`grammars`](CodeGraphScanReport::grammars) is the extractor's own roster.
//! Nothing here re-derives either. A second walk, or a copied list of
//! "supported extensions", would be a second source of truth — and a second
//! source of truth is exactly how a widened extractor keeps confidently
//! reporting the languages it used to have.

use serde::{Deserialize, Serialize};

/// One language's contribution to the graph, on the scan path and the stored
/// path alike. `language` is the stored `code_nodes.language` scalar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeGraphLanguageCount {
    pub language: String,
    /// Distinct source files this language contributed.
    pub files: u64,
    pub nodes: u64,
    pub edges: u64,
}

/// A labelled tally — a node kind, or a revision the graph was stamped at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeGraphTally {
    pub label: String,
    pub count: u64,
}

/// Files an extension contributed to the walk that no grammar covers.
/// `extension` is lowercase and carries no leading dot; a file with no
/// extension at all is not tallied here (it has nothing to tally under) but
/// still counts in [`CodeGraphScanReport::files_unsupported`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeGraphSkippedExtension {
    pub extension: String,
    pub files: u64,
}

/// One grammar this build carries, and the extensions that select it. Sent so a
/// client can answer "what *would* have been folded?" without keeping its own
/// copy of the roster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeGraphGrammar {
    pub language: String,
    /// Lowercase, no leading dot.
    pub extensions: Vec<String>,
}

/// What one on-demand `graph build` did — the report that makes an empty graph
/// self-explanatory.
///
/// Additive by default: a field a future daemon does not compute must be
/// absent-or-zero in a way the client renders as "not measured" rather than as
/// a fact. Today every field is measured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeGraphScanReport {
    /// The checkout the daemon resolved and folded — never the directory the
    /// client happened to be standing in.
    pub repository_root: String,
    /// The revision every node written by this scan was stamped with.
    pub revision: String,
    /// Every file the walk visited, before any filter. The denominator.
    pub files_walked: u64,
    /// Of those, the ones whose extension maps to a grammar — the candidates.
    pub files_supported: u64,
    /// Of those, the ones actually folded into the graph. `files_supported`
    /// minus this is the count that matched a grammar and still yielded
    /// nothing: unreadable, or a parse the extractor rejected.
    pub files_folded: u64,
    /// Files no grammar covers. On a repository whose graph is empty, this
    /// number and [`not_folded`](Self::not_folded) **are** the explanation.
    pub files_unsupported: u64,
    /// Files the ignore rules excluded, or that vanished between the walk and
    /// the read.
    pub files_ignored: u64,
    /// Rows in `code_nodes` for this repository after the fold.
    pub nodes: u64,
    /// Rows in `code_edges` for this repository after the fold.
    pub edges: u64,
    /// Per-language breakdown of what landed, most nodes first.
    pub by_language: Vec<CodeGraphLanguageCount>,
    /// The unsupported extensions, most files first. **Bounded** by the
    /// extractor, so its `files` may sum to less than
    /// [`files_unsupported`](Self::files_unsupported) — a tree with a very long
    /// tail of one-off extensions is counted in full but named in part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_folded: Vec<CodeGraphSkippedExtension>,
    /// Every grammar this build has. Sent with the report because the useful
    /// answer to "why did nothing fold?" is the pair "these extensions were
    /// seen" and "these are the ones that would have worked".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grammars: Vec<CodeGraphGrammar>,
    /// The scan's own per-repository file cap.
    pub file_cap: u64,
    /// The fold reached the cap, so the graph is a **truncation** of the
    /// repository rather than the repository.
    pub cap_hit: bool,
    /// How long the fold took, so a slow scan is visible rather than inferred.
    pub elapsed_ms: u64,
}

/// What the stored graph holds for one repository right now, with no re-scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeGraphStatusView {
    pub repository_root: String,
    pub nodes: u64,
    pub edges: u64,
    /// Distinct `source_path` values across the repository's nodes.
    pub files: u64,
    pub by_language: Vec<CodeGraphLanguageCount>,
    /// Node kinds (`function`, `type`, `file`, …), most nodes first.
    pub by_kind: Vec<CodeGraphTally>,
    /// The revisions the stored nodes are stamped at, most nodes first. More
    /// than one entry means the graph is a mix — usually a full scan at a
    /// commit plus incremental `<head>+workdir` folds on top.
    pub revisions: Vec<CodeGraphTally>,
    /// The checkout's current `HEAD` (or `workdir` where Git cannot answer).
    pub head_revision: String,
    /// The working tree has uncommitted changes to tracked files.
    pub working_tree_dirty: bool,
    /// The graph does not describe the current working tree.
    pub stale: bool,
    /// Why, in one sentence, when `stale`. Absent when the graph is current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
}

/// One node, projected for display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeGraphNodeView {
    /// The stored `code_nodes.id`. Naming it back in a
    /// [`CodeGraphQuery::node_id`] is scoped to the same repository — see that
    /// field's documentation.
    pub id: String,
    pub language: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub qualified_name: String,
    pub kind: String,
    pub revision: String,
}

/// One edge, projected for display with both endpoints already named (a client
/// must not have to issue a second query to render an edge).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeGraphEdgeView {
    pub from_id: String,
    pub from_name: String,
    pub to_id: String,
    pub to_name: String,
    pub relation: String,
    pub confidence: f32,
    pub evidence_kind: String,
    pub revision: String,
}

/// The filter `graph show` applies. Every field narrows; absent fields do not.
///
/// **Scoping is not one of these fields.** The repository is carried by the
/// command, resolved by the daemon from a filesystem path with its own single
/// source of truth, and applied to every branch of this query — including
/// [`node_id`](Self::node_id). There is no way to spell "some other checkout"
/// here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeGraphQuery {
    /// Repo-relative path prefix (`crates/cli/`), matched against
    /// `code_nodes.source_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Exact stored language scalar (`rust`, `python`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Exact stored node-kind scalar (`function`, `type`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Case-insensitive substring of the qualified name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Exactly one node, by its stored id.
    ///
    /// This is the direct-by-id path, and it carries the **same** repository
    /// gate the list path does. A node id belonging to another checkout is
    /// answered identically to an id that does not exist anywhere
    /// (`graph.node-not-found`), so naming an id can never confirm that it
    /// exists elsewhere. A filter that is enforced only where a list is built
    /// is not a filter; it is an enumeration oracle with extra steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// Include the edges incident to the selected nodes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_edges: bool,
    /// Include the nodes themselves. Both false is treated as both true — a
    /// query that selects nothing is a client bug, not a legal request.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_nodes: bool,
    /// Maximum rows of each kind. 0 asks for the server default; the server
    /// clamps any request to its own ceiling.
    #[serde(default, skip_serializing_if = "u32_is_zero")]
    pub limit: u32,
}

/// One page of `graph show` results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeGraphPage {
    pub nodes: Vec<CodeGraphNodeView>,
    pub edges: Vec<CodeGraphEdgeView>,
    /// Nodes matching the filter **before** the limit, so a client can say
    /// "showing 50 of 812" rather than implying it showed everything.
    pub total_nodes: u64,
    pub total_edges: u64,
    /// The limit actually applied after the server's clamp.
    pub limit: u32,
}

fn u32_is_zero(value: &u32) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The empty optionals stay off the wire, so a report from a daemon that
    /// measured nothing extra is byte-identical to one from before the field
    /// existed (the additive-compatibility property every protocol type here
    /// holds).
    #[test]
    fn a_scan_report_round_trips_and_omits_its_empty_optionals() {
        let report = CodeGraphScanReport {
            repository_root: "/home/user/api".to_string(),
            revision: "9f1c2ab".to_string(),
            files_walked: 1204,
            files_supported: 0,
            files_folded: 0,
            files_unsupported: 1204,
            files_ignored: 0,
            nodes: 0,
            edges: 0,
            by_language: Vec::new(),
            not_folded: Vec::new(),
            grammars: Vec::new(),
            file_cap: 2000,
            cap_hit: false,
            elapsed_ms: 12,
        };
        let json = serde_json::to_string(&report).expect("serialize");
        assert!(!json.contains("not_folded"), "{json}");
        assert!(!json.contains("grammars"), "{json}");
        let back: CodeGraphScanReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, report);
    }

    /// A query with nothing set serializes to `{}` — the "no filter" request —
    /// and survives the round trip. This is what pins the default: a client
    /// that sends an empty object must not accidentally mean "node_id = null
    /// matches everything" on a future daemon.
    #[test]
    fn an_empty_query_is_an_empty_object() {
        let query = CodeGraphQuery::default();
        assert_eq!(serde_json::to_string(&query).expect("serialize"), "{}");
        let back: CodeGraphQuery = serde_json::from_str("{}").expect("deserialize");
        assert_eq!(back, query);
        assert!(back.node_id.is_none());
    }

    /// A status view keeps its stale reason only when it is stale.
    #[test]
    fn a_current_status_omits_its_stale_reason() {
        let view = CodeGraphStatusView {
            repository_root: "/home/user/api".to_string(),
            nodes: 12,
            edges: 4,
            files: 3,
            by_language: vec![CodeGraphLanguageCount {
                language: "rust".to_string(),
                files: 3,
                nodes: 12,
                edges: 4,
            }],
            by_kind: vec![CodeGraphTally {
                label: "function".to_string(),
                count: 9,
            }],
            revisions: vec![CodeGraphTally {
                label: "9f1c2ab".to_string(),
                count: 12,
            }],
            head_revision: "9f1c2ab".to_string(),
            working_tree_dirty: false,
            stale: false,
            stale_reason: None,
        };
        let json = serde_json::to_string(&view).expect("serialize");
        assert!(!json.contains("stale_reason"), "{json}");
        assert_eq!(
            serde_json::from_str::<CodeGraphStatusView>(&json).expect("deserialize"),
            view
        );
    }
}
