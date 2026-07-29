//! Computes `CODYPENDENT_BUILD_ID`, a per-build identifier baked into the
//! binary as `codypendent_protocol::BUILD_ID` (see `src/lib.rs`).
//!
//! Precedence:
//! 1. `CODYPENDENT_BUILD_ID` env var, if set and non-empty — verbatim
//!    (reproducible-build pipelines get full control).
//! 2. Git-derived: `"{CARGO_PKG_VERSION}+{short_hash}[-dirty]"`, where
//!    `short_hash` is `git rev-parse --short=12 HEAD` and `-dirty` is
//!    appended when `git status --porcelain --untracked-files=no` is
//!    non-empty.
//! 3. Fallback: bare `CARGO_PKG_VERSION` when git is unavailable or this
//!    is not a git checkout (source tarball, `cargo install` from a
//!    registry).
//!
//! Reproducibility constraints this script upholds: no network access (git
//! commands are local-only), no dependency on a clean working tree (a dirty
//! tree just gets a `-dirty` suffix), and no embedded timestamp or
//! build-host data — the id is a pure function of (HEAD, tree dirtiness),
//! or the explicit override.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Re-run whenever the override env var changes, even if nothing else
    // about the source tree does.
    println!("cargo:rerun-if-env-changed=CODYPENDENT_BUILD_ID");

    let pkg_version = env!("CARGO_PKG_VERSION");

    let build_id = std::env::var("CODYPENDENT_BUILD_ID")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| git_derived_id(pkg_version))
        .unwrap_or_else(|| pkg_version.to_string());

    println!("cargo:rustc-env=CODYPENDENT_BUILD_ID={build_id}");

    register_rerun_triggers();
}

/// `"{pkg_version}+{short_hash}[-dirty]"`, or `None` if git is unavailable,
/// this is not a git checkout, or `HEAD` cannot be resolved. Best-effort:
/// any git failure here just falls through to the bare-version fallback.
fn git_derived_id(pkg_version: &str) -> Option<String> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let hash = run_git(&manifest_dir, &["rev-parse", "--short=12", "HEAD"])?;
    let hash = hash.trim();
    if hash.is_empty() {
        return None;
    }
    let dirty = run_git(
        &manifest_dir,
        &["status", "--porcelain", "--untracked-files=no"],
    )
    .is_some_and(|s| !s.trim().is_empty());
    Some(format!(
        "{pkg_version}+{hash}{}",
        if dirty { "-dirty" } else { "" }
    ))
}

/// Runs `git <args>` in `cwd`, returning stdout on success. `None` on any
/// failure (git missing, not a repo, non-UTF8 output, ...) — deliberately
/// swallowed since this id is best-effort.
fn run_git(cwd: &str, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// Tells cargo to re-run this script when `HEAD` moves (a checkout/commit
/// changes which commit `BUILD_ID` should reflect). Resolved relative to
/// `CARGO_MANIFEST_DIR` (`crates/protocol`) so it works regardless of the
/// invoking working directory. A missed rerun in a fast dev loop only risks
/// a stale-but-harmless id; a clean/release build always re-evaluates.
fn register_rerun_triggers() {
    let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") else {
        return;
    };
    let git_dir = Path::new(&manifest_dir).join("../../.git");
    let head = git_dir.join("HEAD");
    if !head.exists() {
        return;
    }
    println!("cargo:rerun-if-changed={}", head.display());
    if let Some(resolved) = resolved_ref_path(&git_dir, &head) {
        println!("cargo:rerun-if-changed={}", resolved.display());
    }
}

/// If `HEAD` is a symbolic ref (`ref: refs/heads/main`), the path to the
/// file that actually holds the current commit hash — so a commit on the
/// checked-out branch (which only touches that ref file, not `HEAD` itself)
/// still triggers a rebuild.
fn resolved_ref_path(git_dir: &Path, head: &Path) -> Option<PathBuf> {
    let contents = std::fs::read_to_string(head).ok()?;
    let rest = contents.trim().strip_prefix("ref:")?.trim();
    Some(git_dir.join(rest))
}
