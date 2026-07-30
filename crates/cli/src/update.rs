//! `codypendent update` — self-update by installing the latest GitHub release
//! over the running binary, then picking up the new build via the idle-guarded
//! auto-restart. Mirrors `install.sh` (target detection → `gh release download`
//! → tar → macOS quarantine clear → `install`), but in-process. The repo is
//! private, so downloads authenticate through `gh` exactly as the installer.
//!
//! The pure decision ([`decide_update`]) and target detection
//! ([`detect_target`]) are unit-tested; the effectful driver shells out to the
//! same tools `install.sh` is proven against.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::BUILD_ID;

use crate::commands;

const REPO: &str = "CodeHalwell/codypendent";

/// Whether the installed build already matches the latest release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatePlan {
    /// The running build's git sha is a prefix of the latest release's commit.
    UpToDate,
    /// A newer (or different) build is available.
    Available,
}

/// Decide whether an update is available by comparing the running binary's
/// embedded git sha (the `{version}+{sha}[-dirty]` in [`BUILD_ID`]) against the
/// latest release's commit sha. A build with no embedded sha (a bare version)
/// can't be matched, so it always reads as `Available` (offer the install).
pub fn decide_update(build_id: &str, latest_commit_sha: &str) -> UpdatePlan {
    let build_sha = build_id
        .split_once('+')
        .map(|(_, sha)| sha.trim_end_matches("-dirty"))
        .unwrap_or("");
    if !build_sha.is_empty()
        && !latest_commit_sha.is_empty()
        && latest_commit_sha.starts_with(build_sha)
    {
        UpdatePlan::UpToDate
    } else {
        UpdatePlan::Available
    }
}

/// The release asset target triple for this machine, or `None` on an
/// unsupported platform (Windows). Matches `install.sh`'s `case` exactly.
pub fn detect_target(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        _ => None,
    }
}

/// `codypendent update [--check] [<tag>]`.
///
/// Returns whether an update is available, so the caller (the binary) can map
/// `--check` + available to exit code 2 for scripts; the library never exits.
pub async fn run(paths: &RuntimePaths, check: bool, tag: Option<String>) -> anyhow::Result<bool> {
    require_gh()?;
    let tag = match tag {
        Some(tag) => tag,
        None => latest_release_tag()?,
    };
    let commit = release_commit(&tag)?;
    let plan = decide_update(BUILD_ID, &commit);

    if check {
        match plan {
            UpdatePlan::UpToDate => println!("up to date: {tag} (build {BUILD_ID})"),
            UpdatePlan::Available => {
                println!("update available: {tag} (installed build {BUILD_ID})")
            }
        }
        return Ok(plan == UpdatePlan::Available);
    }
    if plan == UpdatePlan::UpToDate {
        println!("already up to date: {tag} (build {BUILD_ID})");
        return Ok(false);
    }

    let target = detect_target(std::env::consts::OS, std::env::consts::ARCH)
        .context("no prebuilt release for this platform (Windows is unsupported)")?;
    let dest_dir = install_dir()?;
    println!(
        "codypendent: installing {tag} ({target}) → {}",
        dest_dir.display()
    );
    download_and_install(&tag, target, &dest_dir)?;
    println!("codypendent: installed {tag}");

    // Pick up the new build immediately via the idle-guarded restart (never
    // kills an active run): the running daemon is the OLD build, so this is
    // exactly the auto-restart path, triggered explicitly.
    match commands::restart_daemon_if_idle(paths).await? {
        commands::IdleRestartOutcome::Restarted => {
            println!("codypendent: daemon restarted on the new build")
        }
        commands::IdleRestartOutcome::RefusedActive(active) => println!(
            "codypendent: installed — the daemon will load the new build once the current \
             run(s) finish ({active} active), or on next launch"
        ),
    }
    // The update was just installed — nothing is available any more.
    Ok(false)
}

fn require_gh() -> anyhow::Result<()> {
    which("gh").context(
        "GitHub CLI (`gh`) is required to download releases from the private repo — \
         install it from https://cli.github.com and run `gh auth login`",
    )?;
    Ok(())
}

/// The newest release tag (rolling builds are prereleases, so ask for the
/// latest of ALL releases — exactly `install.sh`).
fn latest_release_tag() -> anyhow::Result<String> {
    let out = capture(
        "gh",
        &[
            "release",
            "list",
            "-R",
            REPO,
            "-L",
            "1",
            "--json",
            "tagName",
            "--jq",
            ".[0].tagName",
        ],
    )?;
    let tag = out.trim().to_string();
    if tag.is_empty() {
        bail!("no releases found on {REPO}");
    }
    Ok(tag)
}

/// The commit a release tag points at (its `targetCommitish`).
fn release_commit(tag: &str) -> anyhow::Result<String> {
    let out = capture(
        "gh",
        &[
            "release",
            "view",
            tag,
            "-R",
            REPO,
            "--json",
            "targetCommitish",
            "--jq",
            ".targetCommitish",
        ],
    )?;
    Ok(out.trim().to_string())
}

/// The directory the running binary lives in — where the new one is installed.
fn install_dir() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("resolving the running binary's path")?;
    Ok(exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".")))
}

/// Download the release tarball, extract it, clear the macOS quarantine, and
/// install the `codypendent` (and optional `codypendentd`) binaries over
/// `dest_dir` — `sudo install` only when `dest_dir` is not writable, exactly as
/// `install.sh`.
fn download_and_install(tag: &str, target: &str, dest_dir: &Path) -> anyhow::Result<()> {
    let tmp = tempfile::tempdir().context("creating a temp dir for the download")?;
    let tmp_path = tmp.path();
    let asset = format!("codypendent-{target}.tar.gz");

    run_ok(
        "gh",
        &[
            "release",
            "download",
            tag,
            "-R",
            REPO,
            "-p",
            &asset,
            "-D",
            &tmp_path.to_string_lossy(),
            "--clobber",
        ],
    )
    .with_context(|| format!("downloading {asset} for {tag}"))?;

    run_ok(
        "tar",
        &[
            "-xzf",
            &tmp_path.join(&asset).to_string_lossy(),
            "-C",
            &tmp_path.to_string_lossy(),
        ],
    )
    .context("extracting the release tarball")?;

    let src = tmp_path.join(format!("codypendent-{target}"));
    let bin = src.join("codypendent");
    if !bin.exists() {
        bail!("release tarball {asset} did not contain a codypendent binary");
    }

    // macOS: clear the Gatekeeper quarantine so the fresh binary runs.
    if std::env::consts::OS == "macos" {
        let _ = Command::new("xattr")
            .args(["-dr", "com.apple.quarantine"])
            .arg(&src)
            .status();
    }

    let mut bins = vec![bin];
    let daemon_bin = src.join("codypendentd");
    if daemon_bin.exists() {
        bins.push(daemon_bin);
    }

    let writable = dest_dir
        .metadata()
        .map(|m| !m.permissions().readonly())
        .unwrap_or(false)
        && is_dir_writable(dest_dir);
    let (program, prefix): (&str, &[&str]) = if writable {
        ("install", &[])
    } else {
        println!(
            "codypendent: {} is not writable — using sudo",
            dest_dir.display()
        );
        ("sudo", &["install"])
    };
    // `install -m 0755 <bins...> <dest_dir>/` (via `sudo` when not writable).
    let mut args: Vec<String> = prefix.iter().map(|s| s.to_string()).collect();
    args.push("-m".into());
    args.push("0755".into());
    for b in &bins {
        args.push(b.to_string_lossy().to_string());
    }
    args.push(format!("{}/", dest_dir.display()));
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_ok(program, &arg_refs).context("installing the new binary")?;
    Ok(())
}

/// A best-effort writability probe for a directory (create + remove a probe
/// file), so we only escalate to `sudo` when we truly cannot write.
fn is_dir_writable(dir: &Path) -> bool {
    let probe = dir.join(".codypendent-update-write-probe");
    match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

// --- small process helpers ------------------------------------------------

fn which(program: &str) -> anyhow::Result<()> {
    // `output()` (not `status()`) so the probed path never leaks to the user.
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {program}"))
        .output()
        .with_context(|| format!("checking for {program}"))?;
    if out.status.success() {
        Ok(())
    } else {
        bail!("`{program}` not found on PATH")
    }
}

fn capture(program: &str, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !out.status.success() {
        bail!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn run_ok(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("running {program}"))?;
    if status.success() {
        Ok(())
    } else {
        bail!("{program} {} exited with {status}", args.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_update_matches_by_embedded_git_sha() {
        // Installed build's sha is a prefix of the latest release commit → up to date.
        assert_eq!(
            decide_update("0.1.0+d2a51066de31", "d2a51066de31abc0000"),
            UpdatePlan::UpToDate
        );
        // A different commit → available.
        assert_eq!(
            decide_update("0.1.0+d2a51066de31", "833223dd1551ffff"),
            UpdatePlan::Available
        );
        // `-dirty` suffix is stripped before comparing.
        assert_eq!(
            decide_update("0.1.0+d2a51066de31-dirty", "d2a51066de31aaaa"),
            UpdatePlan::UpToDate
        );
        // No embedded sha (bare version) can't be matched → always available.
        assert_eq!(
            decide_update("0.1.0", "d2a51066de31"),
            UpdatePlan::Available
        );
        // Empty latest commit → available (can't confirm up-to-date).
        assert_eq!(
            decide_update("0.1.0+d2a51066de31", ""),
            UpdatePlan::Available
        );
    }

    #[test]
    fn detect_target_covers_the_supported_platforms() {
        assert_eq!(
            detect_target("macos", "aarch64"),
            Some("aarch64-apple-darwin")
        );
        assert_eq!(
            detect_target("macos", "x86_64"),
            Some("x86_64-apple-darwin")
        );
        assert_eq!(
            detect_target("linux", "x86_64"),
            Some("x86_64-unknown-linux-gnu")
        );
        assert_eq!(detect_target("windows", "x86_64"), None);
        assert_eq!(detect_target("linux", "aarch64"), None);
    }
}
