//! The one place this shell answers **"which repository is this?"**.
//!
//! A repository's durable identity is its **checkout** — never a subdirectory
//! of it, never the directory the app happened to be launched from. The task
//! board is keyed by that path (`board:{path}` — see
//! [`codypendent_protocol::board_scope_id`]), so handing the raw launch
//! directory to `ReadBlackboard` mints a *second* board per subdirectory:
//! `repo/` shows the cards, `repo/src` shows none, and a card created from one
//! is invisible from the other forever, with nothing reporting a problem. The
//! daemon canonicalizes what it receives but has no notion of a repository, so
//! it cannot rescue a client that hands it a subdirectory.
//!
//! The CLI names this once in `crates/cli/src/repo_anchor.rs` and delegates to
//! the daemon's own `codypendent_codypendentd::scan::discover_repository_root`.
//! This shell cannot take that dependency — `codypendent-codypendentd` pulls
//! tantivy, tree-sitter, loro and sqlx into a webview binary — so the *one*
//! thing it reimplements is the `git rev-parse` call, deliberately including
//! the part that is easy to leave out:
//!
//! **The stripped environment.** `git rev-parse --show-toplevel` does not ask
//! "is this directory a repository" when `GIT_DIR` is set in the environment:
//! it answers with the *current directory*, whatever that directory is. A
//! desktop app launched from a git hook, `git rebase -x`, or a shell where
//! those variables were exported would therefore anchor its board to `$HOME`.
//! The same variable list `crates/codypendentd/src/scan.rs::git_command`
//! removes is removed here.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The repository-location and discovery-scope variables that must not reach
/// the child. Kept byte-identical to `codypendentd::scan::git_command`'s list:
/// a divergence here silently re-opens the `$HOME` hole that list closed.
const AMBIENT_GIT_VARIABLES: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
];

/// The checkout `dir` belongs to, canonicalized: the Git toplevel when `dir` is
/// inside a repository, else `dir` itself canonicalized, else `dir` as given (a
/// path that does not exist on this host keeps its literal spelling rather than
/// failing the caller).
///
/// Blocking — it shells out to `git`. Call it from `spawn_blocking`, or once at
/// connect time as [`crate::daemon::DaemonClient::connect`] does.
#[must_use]
pub fn anchor_repository_path(dir: &Path) -> PathBuf {
    checkout_root(dir).unwrap_or_else(|| dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()))
}

/// The checkout `dir` belongs to, or `None` when `dir` is not inside one.
///
/// The accessor above deliberately falls back to `dir` itself, which is right
/// for *scoping* a board: a directory outside a checkout still needs somewhere
/// to keep its cards. It is wrong for *diagnosing* one, so a caller that must
/// tell "this is a checkout" from "there is no repository here" asks here and
/// gets the un-fallen-back answer.
///
/// The `git` invocation has no timeout: `std::process::Command::output` offers
/// none, and callers run this on a `spawn_blocking` thread, so a wedged `git`
/// (a hung network mount) stalls one blocking thread rather than the Tauri
/// runtime. Revisit with a spawn-and-poll timeout if that ever bites in
/// practice.
#[must_use]
pub fn checkout_root(dir: &Path) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    for key in AMBIENT_GIT_VARIABLES {
        command.env_remove(key);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let printed = String::from_utf8(output.stdout).ok()?;
    let trimmed = printed.trim();
    if trimmed.is_empty() {
        return None;
    }
    let root = PathBuf::from(trimmed);
    // `--show-toplevel` already prints an absolute path; canonicalizing folds
    // the symlinked-`/tmp` case so the client and the daemon agree on spelling.
    Some(root.canonicalize().unwrap_or(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo(path: &Path) {
        let status = Command::new("git")
            .current_dir(path)
            .args(["init", "--quiet"])
            .status()
            .expect("git init");
        assert!(status.success(), "git init failed in {}", path.display());
    }

    /// The invariant this module exists for: a subdirectory anchors to the
    /// checkout. Before anchoring, the board hashed the opened directory, so
    /// `repo/src` and `repo/` were two different boards.
    #[test]
    fn a_subdirectory_anchors_to_the_checkout() {
        let repo = tempfile::tempdir().expect("tempdir");
        init_repo(repo.path());
        let nested = repo.path().join("src").join("deep");
        std::fs::create_dir_all(&nested).expect("mkdir -p");

        let root = anchor_repository_path(repo.path());
        assert_eq!(anchor_repository_path(&nested), root);
    }

    /// Outside a checkout an ordinary directory is its own anchor — a home or
    /// projects directory must never be folded into some enclosing repository.
    #[test]
    fn a_directory_outside_a_checkout_anchors_to_itself() {
        let plain = tempfile::tempdir().expect("tempdir");
        let canonical = plain.path().canonicalize().expect("canonicalize");
        assert_eq!(anchor_repository_path(plain.path()), canonical);
        assert_eq!(checkout_root(plain.path()), None);
    }

    /// The hole this module documents: with `GIT_DIR` inherited, git answers
    /// `--show-toplevel` with the *current* directory even when that directory
    /// is not a repository. The strip must survive that.
    #[test]
    fn an_inherited_git_dir_does_not_make_a_plain_directory_a_checkout() {
        let repo = tempfile::tempdir().expect("tempdir");
        init_repo(repo.path());
        let plain = tempfile::tempdir().expect("tempdir");

        // SAFETY: single-threaded test process mutation of its own environment,
        // restored before returning.
        std::env::set_var("GIT_DIR", repo.path().join(".git"));
        let answer = checkout_root(plain.path());
        std::env::remove_var("GIT_DIR");

        assert_eq!(
            answer, None,
            "an inherited GIT_DIR made a non-repository answer as a checkout"
        );
    }
}
