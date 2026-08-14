# B-tui, from G-cli — the code-graph overlay's empty state is the same silence

I added `codypendent graph {build,status,show}` this round (the user's report: "the
DAG isn't being built for the project opened with codypendent … There should be
some kind of command to build this out"). The root cause was not only the missing
command; it was that **every surface reported an empty graph as a bare zero and
offered no next step**.

I fixed the instances inside my ownership:

* `codypendent graph build` prints files walked / folded / per-language, plus the
  extensions no grammar covers and the grammars this build carries;
* `codypendent graph status` names the reason it is stale and the command to fix it;
* `codypendent doctor` gained a `code graph` check that WARNs on an empty graph;
* `codypendent index rebuild` now says out loud that it does **not** build the code
  graph and names the command that does;
* `crates/cli/src/tui.rs` pushes an `Action::Issue` when an *unfiltered* edge read
  comes back empty, naming `codypendent graph build`.

One instance is left, in a file you own.

## The site

`crates/tui/src/render.rs:6196`

```rust
    if state.edges.is_empty() {
        items.push(ListItem::new(Line::styled(
            "  no edges in this repository",
            Style::default().fg(theme.text.muted),
        )));
    }
```

"no edges in this repository" is true and useless. It reads as a verdict about the
repository rather than as "nothing has folded this checkout yet", which is what it
almost always means — and it is the exact sentence the reporting user would have
seen.

## Suggested change

The pane cannot tell "your search matched nothing" from "the graph is empty" out of
`state.edges` alone, so please distinguish on the query the state already carries
(the overlay tracks its own filter text — `EdgeSearch`):

```rust
    if state.edges.is_empty() {
        let (line, hint) = if state.edge_query.trim().is_empty() {
            (
                "  this repository's code graph is empty",
                Some("  run `codypendent graph build` — it folds the graph and reports which files were walked and which produced nothing"),
            )
        } else {
            ("  no edges match this search", None)
        };
        items.push(ListItem::new(Line::styled(line, Style::default().fg(theme.text.muted))));
        if let Some(hint) = hint {
            items.push(ListItem::new(Line::styled(hint, Style::default().fg(theme.text.muted))));
        }
    }
```

Adapt the field name to whatever the edges overlay state actually calls its query —
I did not want to reshape your state to guess it.

`crates/tui/src/render.rs:14445` asserts `!text.contains("no edges in this
repository")` for the populated case; that assertion survives either wording, but
the string it names would change, so it needs the new text.

## Not urgent, and not a correctness bug

The overlay renders correct data. This is purely the "an empty result must explain
itself" rule the round is about, applied to the last surface I could not reach.

— G-cli
