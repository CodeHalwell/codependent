# G-knowledge, from G-cli — what `codypendent graph build` needs back from the scan

I own `crates/cli/**`, `crates/daemon/**`, `crates/protocol/**`, `crates/codypendentd/**`
**except** `crates/codypendentd/src/scan.rs`. I am adding `codypendent graph
{build,status,show}`. `graph build` must print *why* a graph is empty, which is
information only the scan walk has. Everything below is in files you own.

## 1. `scan::scan_repository` must return a summary (scan.rs:74)

Today it returns `anyhow::Result<()>` and the counts only reach a `tracing::info!`.
A tracing field is not reachable from a command reply. Please return a struct.
The shape I am coding the daemon against (name it as you like, but please keep
these facts available — I map them into `codypendent_protocol::CodeGraphScanReport`):

```rust
/// What one `scan_repository` walk saw. Every count is about FILES, except
/// `nodes`/`edges`, which are rows written.
#[derive(Debug, Clone, Default)]
pub struct ScanSummary {
    /// The checkout the walk actually resolved (after `discover_repository_root`).
    pub root: PathBuf,
    /// The revision every node was stamped with.
    pub revision: GitRevision,
    /// Files parsed and folded successfully.
    pub files_scanned: usize,
    /// `code_nodes` rows written across the whole scan.
    pub nodes: usize,
    /// `code_edges` rows written across the whole scan.
    pub edges: usize,
    /// Per-language breakdown, one entry per grammar that matched at least one
    /// file. `language` is the same string that lands in `code_nodes.language`.
    pub by_language: Vec<LanguageScan>,
    /// Files a grammar-less extension excluded, keyed by extension
    /// (`"go" -> 12`). THIS IS THE HEADLINE OF THE FEATURE: on the reporter's
    /// mixed repo, Python and TSX contributed nothing and said nothing.
    /// Files with no extension count under the empty string.
    pub skipped_unsupported: Vec<(String, usize)>,
    /// Files excluded by `.gitignore` / `target/` / dot-directories.
    pub skipped_ignored: usize,
    /// Files that matched a grammar but could not be read or parsed.
    pub skipped_unreadable: usize,
    /// `SCAN_FILE_CAP` as it was for this walk, and whether the walk hit it.
    pub cap: usize,
    pub cap_hit: bool,
}

#[derive(Debug, Clone)]
pub struct LanguageScan {
    pub language: String,
    pub files: usize,
    pub nodes: usize,
    pub edges: usize,
}
```

`Default` + `Clone` please, so the daemon can build an empty one on a failure
path without constructing every field.

If any field is genuinely not cheap to produce, return it as `None`/absent
rather than a wrong zero — a zero that means "not measured" is exactly the bug
being fixed here. I will render "not measured" distinctly.

**`cap_hit` matters even when it is false.** `SCAN_FILE_CAP` is 2000 and
`collect_rust_paths` applies the cap to *candidates before the ignore filter*
(scan.rs:176-179). So on a repo with >2000 candidate paths the graph is
truncated and nothing says so. Please set `cap_hit` when the candidate walk
broke out on the cap, not when `files_scanned == cap`.

## 2. `codegraph::supported_languages()` — the grammar roster, public

`graph build` prints, for an empty graph: "no grammar covers `.go` (14 files);
grammars available: rust, python, typescript, tsx, javascript". I need the
right-hand list from you rather than hard-coding a copy that drifts the first
time you add a grammar (this is a rule-4 "fix the class" concern: a second copy
of the roster IS the next silent-empty bug).

```rust
/// Every language the code graph can fold, with the file extensions that
/// select it. The ONE roster; `language_for` must be derived from it.
#[must_use]
pub fn supported_languages() -> &'static [SupportedLanguage];

pub struct SupportedLanguage {
    /// The value written to `code_nodes.language`.
    pub language: &'static str,
    /// Lowercase, no leading dot: `["ts", "mts"]`.
    pub extensions: &'static [&'static str],
}
```

## 3. Please do NOT make `scan_repository` fail on an unsupported-only repo

A repository with zero supported files must return `Ok(summary)` with
`files_scanned == 0`, not an error. `graph build` renders that case as a full
explanation ("47 files, none in a language with a grammar; the .go/.rb files
listed below need a grammar"), which is the reporter's actual complaint. An
`Err` collapses it back to an opaque failure.

## 4. `arm_watcher`'s `is_candidate_path` (scan.rs:410) still hardcodes `ext != "rs"`

If the extractor widens and this does not, an edit to a `.py` file during a
session never folds and every later turn's repository map describes the
pre-edit tree — the F2 bug, re-opened for the new languages. Please drive it off
the same roster as §2. Same for `SCAN_FILE_CAP`'s doc comment, which says
"`*.rs` files".

## 5. Nothing else of yours changes for me

I am not asking for a new table, a migration, or a query. `codegraph::nodes` /
`codegraph::edges` are enough for `graph status` / `graph show`; I read
`code_nodes` / `code_edges` directly for the grouped counts.

— G-cli
