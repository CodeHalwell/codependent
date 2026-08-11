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
use codypendent_daemon::remote_ui_plugins::verify_bundled_runtime_root;
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
/// install `codypendent`, its mandatory UI worker launcher, and optional `codypendentd` over
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
    let ui_launcher = src.join("codypendent-ui-worker-launcher");
    if !ui_launcher.exists() {
        bail!("release tarball {asset} did not contain codypendent-ui-worker-launcher");
    }
    bins.push(ui_launcher);
    let daemon_bin = src.join("codypendentd");
    if daemon_bin.exists() {
        bins.push(daemon_bin);
    }

    let runtime_source = src.join("node-runtime");
    if !runtime_source.join("bin/node").is_file() {
        bail!("release tarball {asset} did not contain the pinned Node runtime");
    }
    verify_bundled_runtime_root(&runtime_source)
        .context("validating the release's pinned Node runtime seal")?;

    let runtime_parent = dest_dir
        .parent()
        .unwrap_or(dest_dir)
        .join("lib/codypendent");

    let writable = dest_dir
        .metadata()
        .map(|m| !m.permissions().readonly())
        .unwrap_or(false)
        && is_dir_writable(dest_dir);
    let runtime_writable =
        ensure_local_directory(&runtime_parent) && is_dir_writable(&runtime_parent);
    let privileged = !(writable && runtime_writable);
    let (program, prefix): (&str, &[&str]) = if !privileged {
        ("install", &[])
    } else {
        println!("codypendent: installation directories are not writable — using sudo");
        ("sudo", &["install"])
    };
    let swap = RuntimeSwap::begin(&runtime_source, &runtime_parent, privileged)
        .context("staging the pinned Remote UI runtime")?;
    // `install -m 0755 <bins...> <dest_dir>/` (via `sudo` when not writable).
    let mut args: Vec<String> = prefix.iter().map(|s| s.to_string()).collect();
    args.push("-m".into());
    args.push("0755".into());
    for b in &bins {
        args.push(b.to_string_lossy().to_string());
    }
    args.push(format!("{}/", dest_dir.display()));
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Err(install_error) = run_ok(program, &arg_refs).context("installing the new binary") {
        swap.rollback()
            .context("rolling back the pinned Remote UI runtime after install failure")?;
        return Err(install_error);
    }
    swap.commit()
        .context("committing the pinned Remote UI runtime update")?;
    Ok(())
}

fn ensure_local_directory(path: &Path) -> bool {
    path.is_dir() || std::fs::create_dir_all(path).is_ok()
}

/// A same-filesystem staged runtime replacement. The previous sealed runtime
/// remains at a private backup name until every binary has installed, so a
/// failed update restores the exact old tree instead of leaving an unusable
/// binary/runtime mix.
struct RuntimeSwap {
    current: PathBuf,
    backup: PathBuf,
    privileged: bool,
}

impl RuntimeSwap {
    fn begin(source: &Path, parent: &Path, privileged: bool) -> anyhow::Result<Self> {
        verify_bundled_runtime_root(source).context("source runtime seal is invalid")?;
        if privileged {
            run_ok("sudo", &["mkdir", "-p", &parent.to_string_lossy()])?;
        } else {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let suffix = std::process::id().to_string();
        let current = parent.join("node-runtime");
        let stage = parent.join(format!("node-runtime.new.{suffix}"));
        let backup = parent.join(format!("node-runtime.old.{suffix}"));
        remove_path_if_present(&stage, privileged)?;
        remove_path_if_present(&backup, privileged)?;
        copy_tree(source, &stage, privileged)?;
        if let Err(error) = verify_bundled_runtime_root(&stage) {
            let _ = remove_path_if_present(&stage, privileged);
            return Err(error).context("staged runtime seal is invalid");
        }
        if path_exists(&current) {
            move_path(&current, &backup, privileged)?;
        }
        if let Err(error) = move_path(&stage, &current, privileged) {
            if path_exists(&backup) {
                let _ = move_path(&backup, &current, privileged);
            }
            return Err(error).context("activating staged runtime");
        }
        Ok(Self {
            current,
            backup,
            privileged,
        })
    }

    fn rollback(self) -> anyhow::Result<()> {
        remove_path_if_present(&self.current, self.privileged)?;
        if path_exists(&self.backup) {
            move_path(&self.backup, &self.current, self.privileged)?;
        }
        Ok(())
    }

    fn commit(self) -> anyhow::Result<()> {
        remove_path_if_present(&self.backup, self.privileged)
    }
}

fn path_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn copy_tree(source: &Path, destination: &Path, privileged: bool) -> anyhow::Result<()> {
    if privileged {
        return run_ok(
            "sudo",
            &[
                "cp",
                "-R",
                &source.to_string_lossy(),
                &destination.to_string_lossy(),
            ],
        );
    }
    copy_tree_local(source, destination)
}

fn copy_tree_local(source: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::create_dir(destination)
        .with_context(|| format!("creating runtime stage {}", destination.display()))?;
    for entry in std::fs::read_dir(source)
        .with_context(|| format!("reading runtime source {}", source.display()))?
    {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&source_path)?;
        if metadata.is_dir() {
            copy_tree_local(&source_path, &destination_path)?;
        } else if metadata.file_type().is_symlink() {
            let target = std::fs::read_link(&source_path)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &destination_path)?;
            #[cfg(not(unix))]
            bail!("release Node runtime symlinks are unsupported on this platform");
        } else if metadata.is_file() {
            std::fs::copy(&source_path, &destination_path)?;
            std::fs::set_permissions(&destination_path, metadata.permissions())?;
        } else {
            bail!(
                "unsupported entry in pinned runtime: {}",
                source_path.display()
            );
        }
    }
    Ok(())
}

fn move_path(source: &Path, destination: &Path, privileged: bool) -> anyhow::Result<()> {
    if privileged {
        run_ok(
            "sudo",
            &[
                "mv",
                &source.to_string_lossy(),
                &destination.to_string_lossy(),
            ],
        )
    } else {
        std::fs::rename(source, destination)
            .with_context(|| format!("moving {} to {}", source.display(), destination.display()))
    }
}

fn remove_path_if_present(path: &Path, privileged: bool) -> anyhow::Result<()> {
    if !path_exists(path) {
        return Ok(());
    }
    if privileged {
        run_ok("sudo", &["rm", "-rf", "--", &path.to_string_lossy()])
    } else if std::fs::symlink_metadata(path)?.is_dir() {
        std::fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))
    } else {
        std::fs::remove_file(path).with_context(|| format!("removing {}", path.display()))
    }
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

    fn sealed_runtime(root: &Path, marker: &str) {
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(root.join("bin/node"), b"node").unwrap();
        std::fs::write(root.join("lib/version"), marker.as_bytes()).unwrap();
        let entries = [
            ("bin/node", b"node".as_slice()),
            ("lib/version", marker.as_bytes()),
        ]
        .into_iter()
        .map(|(path, bytes)| {
            serde_json::json!({
                "kind": "file",
                "path": path,
                "digest": codypendent_sandbox::checksum_of(bytes)
                    .strip_prefix("sha256:")
                    .unwrap(),
            })
        })
        .collect::<Vec<_>>();
        std::fs::write(
            root.join(".codypendent-runtime-seal.json"),
            serde_json::to_vec(&entries).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn runtime_update_rejects_a_release_without_a_seal() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("release-runtime");
        std::fs::create_dir_all(source.join("bin")).unwrap();
        std::fs::write(source.join("bin/node"), b"node").unwrap();
        let parent = temp.path().join("install/lib/codypendent");
        assert!(RuntimeSwap::begin(&source, &parent, false).is_err());
        assert!(!parent.join("node-runtime").exists());
    }

    #[test]
    fn runtime_update_replaces_and_commits_the_versioned_tree() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("install/lib/codypendent");
        let current = parent.join("node-runtime");
        sealed_runtime(&current, "old");
        let source = temp.path().join("release-runtime");
        sealed_runtime(&source, "new");

        RuntimeSwap::begin(&source, &parent, false)
            .unwrap()
            .commit()
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(current.join("lib/version")).unwrap(),
            "new"
        );
        verify_bundled_runtime_root(&current).unwrap();
    }

    #[test]
    fn failed_binary_install_rolls_back_the_previous_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("install/lib/codypendent");
        let current = parent.join("node-runtime");
        sealed_runtime(&current, "old");
        let source = temp.path().join("release-runtime");
        sealed_runtime(&source, "new");

        let swap = RuntimeSwap::begin(&source, &parent, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(current.join("lib/version")).unwrap(),
            "new"
        );
        swap.rollback().unwrap();

        assert_eq!(
            std::fs::read_to_string(current.join("lib/version")).unwrap(),
            "old"
        );
        verify_bundled_runtime_root(&current).unwrap();
    }
}
