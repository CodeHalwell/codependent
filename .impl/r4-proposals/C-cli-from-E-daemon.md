# `graph build` blames the parser for files the file CAP dropped

`crates/cli/src/commands.rs:2949` in `render_scan_report`:

```rust
let unparsed = report
    .files_supported
    .saturating_sub(report.files_folded)
    .saturating_sub(report.files_ignored);
if unparsed > 0 {
    out.push_str(&format!(
        "  ! {unparsed} file(s) matched a grammar but produced nothing — unreadable, or\n\
         \x20   the parser rejected them. See the daemon log for the per-file reason.\n"
    ));
}
```

`files_supported` counts every file a grammar claimed, including the ones the
`SCAN_FILE_CAP` walk never got to fold. So on a repository larger than the cap
the difference is the **cap**, not a parse failure, and the command prints a
confidently wrong diagnosis two lines above the correct one:

```
  walked        2101 file(s); 2101 matched a grammar
  folded        2000 file(s) -> 4002 node(s), 4002 edge(s)
  ! 101 file(s) matched a grammar but produced nothing — unreadable, or
    the parser rejected them. See the daemon log for the per-file reason.
  ! The 2000-file scan cap was reached. This graph is a TRUNCATION of the
    repository, not the repository.
```

(Real output, `codypendent graph build` against a 2101-file checkout, this
branch.) Nothing was unreadable and the parser rejected nothing.

This is pre-existing — the old walk incremented `files_supported` for the file
that tripped the cap and then broke, so it mis-reported exactly one phantom
parse failure. The round-5 walk fix ("ignored paths must not spend the cap")
evaluates a whole probe chunk before the cap bites, so the same wrong inference
now shows a much larger number. The count itself is correct and should stay:
2101 files really did match a grammar.

`CodeGraphScanReport` already carries `cap_hit: bool`, so the fix is local to
the renderer — subtract what the cap dropped before blaming the parser:

```rust
    // Ignored files are subtracted first because they are a SUBSET of the
    // supported ones … [existing comment unchanged]
    //
    // And a capped walk leaves supported-but-unfolded candidates that no parser
    // ever saw: attributing those to the parser reads as a repository-wide parse
    // failure when the only thing that happened is that the scan stopped. The
    // cap gets its own line below; this one must not claim them too.
    let unparsed = if report.cap_hit {
        0
    } else {
        report
            .files_supported
            .saturating_sub(report.files_folded)
            .saturating_sub(report.files_ignored)
    };
```

Suppressing rather than adjusting the number: once the cap has bitten there is
no way to tell a genuinely unreadable file from one the walk never reached, and
"n file(s) the parser rejected" is only worth printing when it is certainly
true. The `cap_hit` line immediately below already tells the user the graph is
a truncation.

A regression test in the same file's `mod tests` (where
`an_empty_status_points_at_graph_build_and_disowns_index_rebuild` and friends
live) would be: a report with `files_supported: 2101, files_folded: 2000,
cap_hit: true` renders the TRUNCATED line and does **not** contain "the parser
rejected them"; with `cap_hit: false` it still does.

— E-daemon (`crates/codypendentd`, `crates/knowledge`). No daemon-side change is
needed: the daemon fills `files_supported`, `files_folded`, `files_ignored` and
`cap_hit` from the scan's own `ScanSummary`, and all four are accurate.
