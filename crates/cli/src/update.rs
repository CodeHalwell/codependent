//! `codypendent update` — self-update by installing the latest GitHub release
//! over the running binary, then picking up the new build via the idle-guarded
//! auto-restart. Mirrors `install.sh` (target detection → `gh release download`
//! → tar → macOS quarantine clear → `install`), but in-process. The repo is
//! private, so downloads authenticate through `gh` exactly as the installer.
//!
//! The pure decision ([`decide_update`]) and target detection
//! ([`detect_target`]) are unit-tested; the effectful driver shells out to the
//! same tools `install.sh` is proven against.
//!
//! # KNOWN GAP: the downloaded binaries are not integrity-verified
//!
//! [`download_and_install`] extracts `codypendent-<target>.tar.gz` and installs
//! the binaries inside it — with `sudo` when the destination is not writable —
//! against no digest, signature or provenance attestation. The bundled Node
//! runtime IS verified (`verify_bundled_runtime_root` checks the
//! `.codypendent-runtime-seal.json` the release workflow writes), which makes
//! the omission for the binaries themselves easy to miss.
//!
//! This is stated rather than fixed because THERE IS NOTHING TO VERIFY AGAINST.
//! `.github/workflows/release.yml`'s `publish` job attaches exactly
//! `dist/*.tar.gz`, `dist/*.AppImage` and `dist/*.vsix`; it publishes no
//! `checksums.txt`, no per-asset `.sha256`, and runs no attestation step. A
//! check added here could only hash the bytes just downloaded and compare them
//! against themselves, which is assurance theatre — it would make the code look
//! verified without any independent value to verify against.
//!
//! Closing it is a release-workflow change first: emit a checksum manifest (or
//! `actions/attest-build-provenance`) at publish time, then verify it here (or
//! with `gh attestation verify`) before anything is extracted or installed.
//! Until then the trust boundary is `gh`'s authenticated TLS session to the
//! private repository, and nothing narrower.

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

/// What `codypendent install` can put on this machine, alongside the CLI that
/// `codypendent update` maintains.
///
/// Both arms install a GitHub release asset through the SAME machinery
/// `update` uses (`gh release download` → extract → macOS quarantine clear),
/// because that path is the one that actually yields a usable artefact on an
/// unsigned build: `gh` never sets `com.apple.quarantine`, a browser always
/// does, and macOS 15 removed the right-click→Open bypass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallTarget {
    /// The Tauri desktop shell (`Codypendent.app` / the Linux AppImage).
    Desktop,
    /// The VS Code-family extension (`.vsix`), installed through the editor's
    /// own CLI. Platform-INDEPENDENT: a `.vsix` is the same file everywhere,
    /// so this arm deliberately never consults [`detect_target`].
    Editor {
        /// The editor's launcher binary (`code`, `cursor`, …).
        binary: &'static str,
        /// Its human name, for messages.
        name: &'static str,
    },
}

/// The release asset carrying the desktop app for a target triple, or `None`
/// when the release does not build a desktop bundle for it.
///
/// Deliberately NARROWER than [`detect_target`]: the CLI tarball is built for
/// three triples, the desktop bundle for two. Intel macOS is absent because CI
/// does not build a desktop bundle for it, and an `install desktop` that
/// downloaded an Apple-Silicon `.app` onto an Intel Mac would be exactly the
/// "looks installed, is not" failure this command exists to avoid.
pub fn desktop_asset_name(target: &str) -> Option<String> {
    match target {
        // A tar.gz of `Codypendent.app`, NOT a .dmg: the release is unsigned,
        // and an unsigned .dmg is only installable through a path that does not
        // set the quarantine bit — which a .dmg's own double-click flow is not.
        "aarch64-apple-darwin" => Some(format!("codypendent-desktop-{target}.tar.gz")),
        // Self-contained AppImage: it carries the GTK/WebKit stack the bare
        // binary would need preinstalled, and needs no root to install.
        "x86_64-unknown-linux-gnu" => Some(format!("codypendent-desktop-{target}.AppImage")),
        _ => None,
    }
}

/// `codypendent install <desktop|vscode|cursor|…> [<tag>]`.
///
/// A LOCAL command: it shells out to `gh`/`tar`/`install`/the editor CLI and
/// never crosses the daemon socket, so it carries no `CommandBody` variant and
/// no role floor.
pub fn install(what: InstallTarget, tag: Option<String>) -> anyhow::Result<()> {
    require_gh()?;
    let tag = match tag {
        Some(tag) => tag,
        None => latest_release_tag()?,
    };
    match what {
        InstallTarget::Desktop => install_desktop(&tag),
        InstallTarget::Editor { binary, name } => install_editor_extension(&tag, binary, name),
    }
}

fn install_desktop(tag: &str) -> anyhow::Result<()> {
    let target = detect_target(std::env::consts::OS, std::env::consts::ARCH)
        .context("no prebuilt release for this platform (Windows is unsupported)")?;
    let asset = desktop_asset_name(target).with_context(|| {
        format!(
            "the release does not carry a desktop bundle for {target} — it is built only for \
             aarch64-apple-darwin (Apple Silicon) and x86_64-unknown-linux-gnu. The `codypendent` \
             CLI itself IS built for {target}: run `codypendent update`."
        )
    })?;

    let tmp = tempfile::tempdir().context("creating a temp dir for the download")?;
    let tmp_path = tmp.path();
    println!("codypendent: downloading {asset} from {tag}");
    download_asset(tag, &asset, tmp_path)?;
    let downloaded = tmp_path.join(&asset);
    if !downloaded.is_file() {
        bail!("release {tag} has no asset named {asset}");
    }

    if std::env::consts::OS == "macos" {
        install_macos_app(tmp_path, &downloaded, &asset)
    } else {
        install_linux_appimage(&downloaded)
    }
}

/// macOS: unpack `Codypendent.app`, strip the quarantine attribute, and copy it
/// into an Applications directory this user can actually write.
fn install_macos_app(tmp_path: &Path, downloaded: &Path, asset: &str) -> anyhow::Result<()> {
    run_ok(
        "tar",
        &[
            "-xzf",
            &downloaded.to_string_lossy(),
            "-C",
            &tmp_path.to_string_lossy(),
        ],
    )
    .context("extracting the desktop bundle")?;

    let app = tmp_path.join(APP_BUNDLE_NAME);
    if !app.is_dir() {
        bail!("desktop bundle {asset} did not contain {APP_BUNDLE_NAME}");
    }
    // The build is NOT notarized. `gh` does not set com.apple.quarantine, but
    // clear it unconditionally so the result launches even if something in the
    // download path did.
    let _ = Command::new("xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(&app)
        .status();

    let dest_dir = applications_dir()?;
    let dest = dest_dir.join(APP_BUNDLE_NAME);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)
            .with_context(|| format!("replacing the existing {}", dest.display()))?;
    }
    run_ok(
        "cp",
        &["-R", &app.to_string_lossy(), &dest.to_string_lossy()],
    )
    .with_context(|| format!("installing {APP_BUNDLE_NAME} into {}", dest_dir.display()))?;

    println!("codypendent: installed {}", dest.display());
    println!(
        "codypendent: NOTE — this build is not code-signed or notarized. It launches because \
         `gh` never sets the macOS quarantine attribute and this command clears any that was \
         set. The SAME bundle downloaded through a web browser is quarantined and Gatekeeper \
         blocks it (macOS 15 removed the right-click → Open bypass), so `codypendent install \
         desktop` is the supported install path. If you ever do end up with a quarantined copy: \
         xattr -dr com.apple.quarantine '{}'",
        dest.display()
    );
    Ok(())
}

/// Linux: the AppImage is a single self-contained executable — chmod it and drop
/// it in the user's own bin directory. No root, no package manager.
fn install_linux_appimage(downloaded: &Path) -> anyhow::Result<()> {
    let dest_dir = user_bin_dir()?;
    std::fs::create_dir_all(&dest_dir)
        .with_context(|| format!("creating {}", dest_dir.display()))?;
    let dest = dest_dir.join("codypendent-desktop.AppImage");
    run_ok(
        "install",
        &[
            "-m",
            "0755",
            &downloaded.to_string_lossy(),
            &dest.to_string_lossy(),
        ],
    )
    .with_context(|| format!("installing the AppImage into {}", dest_dir.display()))?;

    println!("codypendent: installed {}", dest.display());
    println!(
        "codypendent: run it with `{}`. The AppImage is built on Ubuntu 24.04, so it needs \
         glibc 2.39 or newer and FUSE (`--appimage-extract-and-run` works without FUSE). It is \
         unsigned; Linux does not gate on that.",
        dest.display()
    );
    if !path_contains(&dest_dir) {
        println!(
            "codypendent: {} is not on your PATH — add it, or launch the AppImage by full path.",
            dest_dir.display()
        );
    }
    Ok(())
}

/// The `.vsix` is platform-independent, so this never consults
/// [`detect_target`] — refusing to install an editor extension on, say, an
/// aarch64 Linux box would be a refusal with no cause.
fn install_editor_extension(tag: &str, binary: &str, name: &str) -> anyhow::Result<()> {
    which(binary).with_context(|| {
        format!(
            "{name}'s command-line launcher `{binary}` is not on PATH — install it from {name} \
             via the \"Shell Command: Install '{binary}' command in PATH\" palette action, then \
             re-run this"
        )
    })?;

    let tmp = tempfile::tempdir().context("creating a temp dir for the download")?;
    let tmp_path = tmp.path();
    println!("codypendent: downloading the extension bundle from {tag}");
    // Matched by glob: the asset name carries the extension's own version,
    // which moves independently of the workspace version in the tag.
    download_asset(tag, "*.vsix", tmp_path)?;
    let vsix = sole_vsix(tmp_path)?;

    run_ok(
        binary,
        &["--install-extension", &vsix.to_string_lossy(), "--force"],
    )
    .with_context(|| format!("installing the extension into {name}"))?;
    println!(
        "codypendent: installed {} into {name} — reload the window to activate it",
        vsix.file_name().unwrap_or_default().to_string_lossy()
    );
    Ok(())
}

const APP_BUNDLE_NAME: &str = "Codypendent.app";

fn download_asset(tag: &str, pattern: &str, into: &Path) -> anyhow::Result<()> {
    run_ok(
        "gh",
        &[
            "release",
            "download",
            tag,
            "-R",
            REPO,
            "-p",
            pattern,
            "-D",
            &into.to_string_lossy(),
            "--clobber",
        ],
    )
    .with_context(|| format!("downloading {pattern} from release {tag}"))
}

/// Exactly one `.vsix` must have arrived; two would make "which one did we just
/// install?" unanswerable, and zero means the release never carried one.
fn sole_vsix(dir: &Path) -> anyhow::Result<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "vsix"))
        .collect();
    found.sort();
    match found.len() {
        0 => bail!("the release carries no .vsix extension bundle"),
        1 => Ok(found.remove(0)),
        n => bail!("the release carries {n} .vsix bundles; refusing to guess which one to install"),
    }
}

/// `/Applications` when this user can write it, else `~/Applications` — a
/// per-user install is a real install, and is preferable to escalating to
/// `sudo` for a GUI app.
fn applications_dir() -> anyhow::Result<PathBuf> {
    let system = PathBuf::from("/Applications");
    if system.is_dir() && is_dir_writable(&system) {
        return Ok(system);
    }
    let user = home_dir()?.join("Applications");
    std::fs::create_dir_all(&user).with_context(|| format!("creating {}", user.display()))?;
    Ok(user)
}

fn user_bin_dir() -> anyhow::Result<PathBuf> {
    Ok(home_dir()?.join(".local/bin"))
}

fn home_dir() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| !home.as_os_str().is_empty())
        .context("HOME is not set, so there is no per-user install location")
}

fn path_contains(dir: &Path) -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|entry| entry == dir))
        .unwrap_or(false)
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

    #[test]
    fn desktop_asset_is_built_only_for_the_triples_ci_bundles() {
        assert_eq!(
            desktop_asset_name("aarch64-apple-darwin").as_deref(),
            Some("codypendent-desktop-aarch64-apple-darwin.tar.gz")
        );
        assert_eq!(
            desktop_asset_name("x86_64-unknown-linux-gnu").as_deref(),
            Some("codypendent-desktop-x86_64-unknown-linux-gnu.AppImage")
        );
        // Intel macOS gets a CLI tarball but NO desktop bundle. It must refuse
        // rather than hand back the Apple-Silicon asset name — installing that
        // would produce an app bundle that cannot launch.
        assert_eq!(desktop_asset_name("x86_64-apple-darwin"), None);
        assert_eq!(desktop_asset_name("aarch64-unknown-linux-gnu"), None);
        // Every desktop asset name must correspond to a real CLI target triple,
        // so `detect_target` can never hand `install desktop` a triple that
        // silently has no mapping in the other direction.
        for target in ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"] {
            assert!(
                [
                    detect_target("macos", "aarch64"),
                    detect_target("linux", "x86_64")
                ]
                .contains(&Some(target)),
                "{target} is not a triple detect_target can produce"
            );
        }
    }

    #[test]
    fn sole_vsix_refuses_zero_and_ambiguous_downloads() {
        let temp = tempfile::tempdir().unwrap();
        assert!(sole_vsix(temp.path()).is_err());

        std::fs::write(temp.path().join("codypendent-0.6.0.vsix"), b"vsix").unwrap();
        // A non-.vsix sibling (gh writes nothing else, but be explicit) is ignored.
        std::fs::write(temp.path().join("notes.txt"), b"x").unwrap();
        assert_eq!(
            sole_vsix(temp.path()).unwrap(),
            temp.path().join("codypendent-0.6.0.vsix")
        );

        std::fs::write(temp.path().join("codypendent-0.7.0.vsix"), b"vsix").unwrap();
        assert!(sole_vsix(temp.path()).is_err());
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
