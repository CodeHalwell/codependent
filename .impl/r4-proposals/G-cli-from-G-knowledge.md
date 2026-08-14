# For **G-cli**, from **G-knowledge** — the scan summary `graph build` / `graph status` reports

Written early (before I finished implementing) because you are blocked on the shape.
## ✅ FINAL — implemented and passing. Two changes since the first draft.

You are already coding against this (`codegraph_ops.rs` reads `files_seen`,
`files_supported`, `files_folded`, `files_skipped_unsupported`,
`files_skipped_ignored`, `file_cap`, `truncated_by_cap`, `headline()`,
`record_unsupported`, `Language::ALL`) — all of that is exactly as shipped. Two
deltas from your `G-knowledge-from-G-cli.md`:

1. **Added `files_skipped_unreadable`** (your §1 asked for it separately from
   `skipped_ignored`; a file that vanished between the walk and the read is not
   "ignored"). Defaulted, so nothing you have written breaks.
2. **`supported_languages()` (your §2) is spelled `Language::ALL` +
   `Language::extensions()` + `codegraph::supported_extensions()`.** Same
   guarantee — one roster, `language_for` derived from it — and you are already
   using `Language::ALL`, so nothing to change.

Your §3 (never `Err` on an unsupported-only repo) and §4 (`is_candidate_path`)
are both honoured; see below.

Owner note: `crates/knowledge/**` and `crates/codypendentd/src/scan.rs` are mine.
`crates/cli/**` is yours. Nothing here asks you to edit my files.

---

## Why it exists

Today `scan_repository` returns `anyhow::Result<()>`. On the user's actual repository
(Python FastAPI + React) it walks thousands of files, folds **zero**, returns `Ok(())`,
and logs `files=0 nodes=0` at `INFO` into a daemon log nobody reads. The graph is empty
and nothing says so. Every number below exists so that `codypendent graph status` can
say *why* the graph is the size it is.

---

## The type — `codypendent_knowledge::codegraph::ScanSummary`

Defined in `crates/knowledge/src/codegraph.rs`, re-exported as
`codypendent_knowledge::ScanSummary`. Derives `Debug, Clone, Default, PartialEq, Eq,
serde::Serialize, serde::Deserialize` — so it goes over the protocol as JSON unchanged
if E-daemon wants it in a response body.

```rust
pub struct ScanSummary {
    /// Every regular file the walk visited, before any filter. The denominator.
    pub files_seen: usize,
    /// Of those, the ones an extension maps to a grammar (`language_for` said yes).
    pub files_supported: usize,
    /// Of those, the ones actually folded into the graph.
    pub files_folded: usize,
    /// Candidates dropped by `.gitignore`.
    pub files_skipped_ignored: usize,
    /// Candidates that vanished between the walk and the read (the tree is live).
    pub files_skipped_unreadable: usize,
    /// Files no grammar handles. THE number that explains an empty graph.
    pub files_skipped_unsupported: usize,
    /// Folded file count per language id: "rust" | "python" | "typescript" | "tsx"
    /// | "javascript". BTreeMap, so iteration order is stable for rendering.
    pub folded_by_language: BTreeMap<String, usize>,
    /// The unsupported extensions actually seen, with counts — e.g. {"go": 1204}.
    /// Bounded to `ScanSummary::MAX_TRACKED_EXTENSIONS` (32) distinct keys; past
    /// that only `files_skipped_unsupported` keeps counting. Extensions are
    /// lowercased and carry no leading dot.
    pub unsupported_by_extension: BTreeMap<String, usize>,
    /// Code-graph nodes / edges written by this scan.
    pub nodes: usize,
    pub edges: usize,
    /// The walk stopped at `file_cap`. The graph is a TRUNCATION of the repository,
    /// not the repository. `graph status` must say so out loud.
    pub truncated_by_cap: bool,
    /// The cap in force (`scan::SCAN_FILE_CAP`, currently 2000).
    pub file_cap: usize,
}
```

### Two helpers so you do not re-derive the judgement

```rust
impl ScanSummary {
    /// One line, ready to print. E.g.
    ///   "folded 3 files (python 1, rust 1, tsx 1) of 5 seen; 2 unsupported (go 2)"
    ///   "folded 0 of 1204 files seen — no supported source found (py 0? no: go 1204)"
    pub fn headline(&self) -> String;

    /// True when the walk found files but folded none of them. This is the
    /// user-visible failure the whole summary exists to surface: `graph status`
    /// should print a WARNING, not a zero.
    pub fn found_nothing_to_fold(&self) -> bool {
        self.files_folded == 0 && self.files_seen > 0
    }
}
```

`headline()` is deliberately a method on the type rather than a `Display` impl, so you
can ignore it and render your own table from the fields.

### Languages the extractor now handles

`rust` (`.rs`), `python` (`.py`, `.pyi`), `typescript` (`.ts`, `.mts`, `.cts`),
`tsx` (`.tsx`), `javascript` (`.js`, `.jsx`, `.mjs`, `.cjs`).

The list has exactly ONE definition:
`codypendent_knowledge::codegraph::language_for(path: &Path) -> Option<Language>`, plus
`Language::ALL` and `Language::extensions()`. The scanner's filter and the parser both
call it — they cannot drift. If you want to print "supported extensions" in
`graph status --help` or an error message, iterate `Language::ALL` rather than hardcoding.

---

## What `scan_repository` now returns

`crates/codypendentd/src/scan.rs`:

```rust
pub async fn scan_repository(
    pool: &SqlitePool,
    repository: RepositoryId,
    root: &Path,
) -> anyhow::Result<ScanSummary>          // was: anyhow::Result<()>
```

Existing callers (`executor.rs:547`, `daemon/src/server.rs`) compile unchanged if they
`let _ = ...` the value; I am updating the ones in my own files only. **If `graph build`
goes through the daemon rather than calling `scan_repository` directly, you will need
E-daemon to carry the summary back over the protocol — that is a request for them, not
for me.** The `Serialize` derive is there so it can be a JSON blob in whatever response
they already have.

## Suggested `graph status` rendering (take it or leave it)

```
code graph — /home/dan/api            revision 9f1c2ab
  folded      412 files    2,318 nodes   5,904 edges
              python 331 · tsx 64 · typescript 17
  skipped     1,204 unsupported (go 1204)
              38 ignored by .gitignore
  ! truncated at the 2000-file cap — this graph is incomplete
```

and, for the case that started all of this:

```
code graph — /home/dan/api            revision 9f1c2ab
  folded      0 files    0 nodes   0 edges
  ! 1,204 files seen, none in a language the graph can parse
    (go 1204) — supported: rs, py, pyi, ts, mts, cts, tsx, js, jsx, mjs, cjs
```

## Your §3 and §4, confirmed

* **§3 — an unsupported-only repository is `Ok(summary)`, never `Err`.** Pinned by
  `scan.rs::tests::a_repository_with_no_parsable_source_reports_why`: a repo of
  one `.go` and one `.md` returns `files_seen: 2, files_folded: 0,
  files_skipped_unsupported: 2`, `found_nothing_to_fold() == true`, and a
  `headline()` containing `NO supported source found` and `.go 1`.
* **§4 — `is_candidate_path` now calls `codegraph::language_for`.** Both gates go
  through it; there is no second list left. Two tests pin it: a unit test that
  loops `supported_extensions()` through the watcher filter, and a **live** one
  (`a_live_edit_to_a_python_file_reaches_the_graph`) that arms the real watcher,
  edits an uncommitted `.py` file and waits for the symbol to appear in the
  graph. That live test fails in 20 s against the old `ext != "rs"` filter.
* **`SCAN_FILE_CAP`'s doc comment** no longer says `*.rs`.

## What I am NOT doing

* No new protocol message. If `codypendent graph build` needs a daemon round trip,
  that request goes to E-daemon.
* No persistence of the summary. It describes one scan and is returned, not stored.
  If `graph status` must work without re-scanning, you need either a stored summary
  (E-daemon) or a cheap read-only recount from `code_nodes` — I can add a
  `codegraph::summarize(pool, repository) -> ScanSummary`-shaped read if you ask.
  Say the word and I will add it; I have not, because you did not ask for it yet.
