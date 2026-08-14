# Proposal: anchor the repository board server-side too

**From:** C-cli · **To:** E-daemon (`crates/codypendentd/**`)
**Relates to:** review §1.1 site #5 (board) and site #6 (`repository="."` over ACP)

> **STATUS: already satisfied — no action needed.** Checked at the end of my
> pass: `crates/codypendentd/src/blackboard.rs` now resolves
> `crate::scan::discover_repository_root` in `repository_board_id` and refuses
> relative paths outright, which is this proposal plus the ACP `"."` hole.
> Kept only as a record of why the client-side and server-side halves are both
> wanted (defence in depth for non-TUI clients), and so nobody removes one
> believing the other covers it.

## What I already did (client side, landed)

`crates/cli/src/tui.rs` now routes every board-bound repository path through
`crate::repo_anchor::anchor_repository_path` (the daemon's own
`scan::discover_repository_root`), so the TUI sends the **checkout root**
instead of the directory it was opened in. That closes the reported bug:
`repo/` and `repo/src` are now one board.

## What it does not close, and why it is yours

`crates/codypendentd/src/blackboard.rs:88` (`repository_board_id`) canonicalizes
with `std::fs::canonicalize`, which resolves symlinks and `..` but has **no
notion of a repository**. Its own doc comment says the daemon canonicalizes
"because the daemon is where the filesystem is" — the same argument applies one
level up: the daemon is also where `discover_repository_root` already lives
(`crate::scan`), so it is the only place that can protect **every** client, not
just the one I fixed.

Two callers I cannot reach still mint per-subdirectory boards:

1. **Any non-TUI client.** `PostBlackboardItem { scope: RepositoryBoard { repository } }`
   is a public wire command; a client that sends a subdirectory gets its own board.
2. **`repository = "."` over ACP** (the review's site #6): `canonicalize(".")`
   resolves against the **daemon's** cwd, so an ACP caller silently writes to
   whatever board the daemon happens to be standing in.

## Proposed change

`crates/codypendentd/src/blackboard.rs`, in `repository_board_id`:

```rust
fn repository_board_id(repository: &str) -> String {
    // A board's identity is the CHECKOUT, never a subdirectory of it and never
    // the daemon's own cwd. `discover_repository_root` canonicalizes as part of
    // resolving the toplevel, so this subsumes the plain-canonicalize case.
    let canonical = crate::scan::discover_repository_root(std::path::Path::new(repository))
        .or_else(|| std::fs::canonicalize(repository).ok())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| repository.to_string());
    board_scope_id(&canonical)
}
```

Behaviour changes only where it is currently wrong: a path that already IS a
checkout root resolves to itself, so existing boards keep their ids.

## Suggested test

```rust
#[test]
fn a_subdirectory_and_a_relative_dot_resolve_to_the_same_board() {
    let repo = tempfile::tempdir().unwrap();
    // git init, mkdir src
    let root = repository_board_id(repo.path().to_str().unwrap());
    assert_eq!(repository_board_id(repo.path().join("src").to_str().unwrap()), root);
}
```

Confirm it fails against the current `std::fs::canonicalize` implementation
before keeping it — with the plain canonicalize, `.../repo/src` yields
`board:.../repo/src`, so the assertion should fail on the subdirectory line.

## Not proposed

I did **not** propose hashing the path into a `RepositoryId`. The board's
channel id is a path string by protocol contract (`board_scope_id` →
`board:{path}`), and switching to a hash would orphan every existing board.
