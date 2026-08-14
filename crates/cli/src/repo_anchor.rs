//! The one place this crate answers **"which repository is this?"**.
//!
//! A repository's durable identity is its **checkout** — never a subdirectory
//! of it, never the directory a package happens to live in, never a throwaway
//! worktree. The knowledge fabric already agrees on how to *hash* that identity
//! ([`codypendent_knowledge::anchor_repository_id`]), and the 2026-08-13 review
//! found the remaining defect one level below: every call site re-derived
//! **which path to hand it**, and they disagreed. The board hashed the opened
//! directory (`repo/` → 6 cards, `repo/src` → 0), `skill add` hashed the
//! package directory (an installed skill retrieval never disclosed). Both are
//! silent — a `WHERE scope_key = ?` that matches nothing renders as an empty
//! list, which is a legitimate answer to a legitimate question.
//!
//! So the anchoring is named here once, in both shapes a caller can need:
//!
//! * [`anchor_repository_path`] — the checkout root as a **path**, for the
//!   surfaces whose durable key is a path string (the repository task board's
//!   `board:{path}` channel id).
//! * [`anchor_repository_id`] — the checkout root as a hashed
//!   [`RepositoryId`], for the knowledge-fabric scopes.
//!
//! Both resolve the same root, and [`the test below`](tests) pins that: hashing
//! `anchor_repository_path`'s answer equals `anchor_repository_id`'s. A future
//! divergence between the path axis and the id axis therefore fails the build
//! rather than emptying a list.
//!
//! The resolution itself is the daemon's own
//! [`discover_repository_root`](codypendent_codypendentd::scan::discover_repository_root)
//! — not a third copy of `git rev-parse --show-toplevel`. Client and server
//! cannot disagree about where a checkout starts if they call the same
//! function.

use std::path::{Path, PathBuf};

use codypendent_protocol::RepositoryId;

/// The checkout `dir` belongs to, canonicalized: the Git toplevel when `dir` is
/// inside a repository, else `dir` itself canonicalized, else `dir` as given (a
/// path that does not exist on this host keeps its literal spelling rather than
/// failing the caller — unchanged behaviour for that case).
///
/// Use this wherever a **path** is the durable key. Passing the raw opened
/// directory instead forks the resource once per subdirectory a user happens to
/// `cd` into, permanently and with no error.
#[must_use]
pub fn anchor_repository_path(dir: &Path) -> PathBuf {
    codypendent_codypendentd::scan::discover_repository_root(dir)
        .unwrap_or_else(|| dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()))
}

/// The [`RepositoryId`] of the checkout `dir` belongs to.
///
/// Delegates to [`codypendent_knowledge::anchor_repository_id`] rather than
/// re-hashing: one derivation of the id, one derivation of the path, and a test
/// that they agree.
#[must_use]
pub fn anchor_repository_id(dir: &Path) -> RepositoryId {
    codypendent_knowledge::anchor_repository_id(dir)
}

/// The checkout `dir` belongs to, or `None` when `dir` is not inside one.
///
/// The two accessors above deliberately fall back to `dir` itself, which is
/// right for *scoping* a resource: a note taken outside a checkout still needs
/// somewhere to live. It is wrong for *diagnosing* one. A caller that must tell
/// "this directory is a checkout" from "there is no repository here" — because
/// the remedy it would otherwise print (`codypendent graph build`) refuses a
/// non-checkout with `graph.not-a-repository` — asks here and gets the
/// un-fallen-back answer instead of the hash of an arbitrary directory.
///
/// Same resolver as the rest of this module, so client and server cannot
/// disagree about where a checkout starts.
#[must_use]
pub fn checkout_root(dir: &Path) -> Option<PathBuf> {
    codypendent_codypendentd::scan::discover_repository_root(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::process::Command;

    fn init_repo(path: &Path) {
        let status = Command::new("git")
            .current_dir(path)
            .args(["init", "--quiet"])
            .status()
            .expect("git init");
        assert!(status.success(), "git init failed in {}", path.display());
    }

    /// The invariant this module exists for: a subdirectory anchors to the
    /// checkout, on BOTH axes. Before the repair the board hashed the opened
    /// directory, so this assertion failed on the path axis while the memory
    /// browser (already on `anchor_repository_id`) passed on the id axis —
    /// exactly the disagreement that emptied the board.
    #[test]
    fn a_subdirectory_anchors_to_the_checkout_on_both_axes() {
        let repo = tempfile::tempdir().expect("tempdir");
        init_repo(repo.path());
        let nested = repo.path().join("src").join("deep");
        std::fs::create_dir_all(&nested).expect("mkdir -p");

        let root = anchor_repository_path(repo.path());
        assert_eq!(anchor_repository_path(&nested), root);
        assert_eq!(
            anchor_repository_id(&nested),
            anchor_repository_id(repo.path())
        );
    }

    /// The path axis and the id axis resolve the SAME root. If a future edit
    /// changes one derivation and not the other, every board card and every
    /// knowledge row start disagreeing silently — so it fails here instead.
    #[test]
    fn the_path_axis_and_the_id_axis_resolve_the_same_root() {
        let repo = tempfile::tempdir().expect("tempdir");
        init_repo(repo.path());
        let nested = repo.path().join("crates").join("cli");
        std::fs::create_dir_all(&nested).expect("mkdir -p");

        for dir in [repo.path(), nested.as_path()] {
            assert_eq!(
                codypendent_knowledge::stable_repository_id(&anchor_repository_path(dir)),
                anchor_repository_id(dir),
                "path/id derivations disagree for {}",
                dir.display()
            );
        }
    }

    /// Outside a checkout an ordinary directory is its own anchor — a home or
    /// projects directory must never be folded into some enclosing repository.
    #[test]
    fn a_directory_outside_a_checkout_anchors_to_itself() {
        let plain = tempfile::tempdir().expect("tempdir");
        let canonical = plain.path().canonicalize().expect("canonicalize");
        assert_eq!(anchor_repository_path(plain.path()), canonical);
    }
}
