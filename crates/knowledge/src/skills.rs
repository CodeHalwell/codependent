//! Skill-package directory ingestion (2026-08-11 review, "activate skills").
//!
//! [`Registry::register_package`] existed with zero production callers — no
//! daemon or CLI path ever loaded a skill directory into the governed registry,
//! so retrieval had nothing to disclose beyond the built-ins. This module is
//! that missing bridge: a **scan** over the two well-known skill roots
//! (`<data_dir>/skills/*/` for the operator's own packages,
//! `<repository>/.codypendent/skills/*/` for repo-local ones), and an
//! **install** the CLI (`codypendent skill add <dir>`) copies a validated
//! package through.
//!
//! ## Scope anchoring
//!
//! `register_package` demands the concrete [`Scope`] up front and validates it
//! against the manifest's declared tier, so the scan peeks each package's
//! `skill.toml` first and maps the tier onto the only anchors a local daemon
//! actually has:
//!
//! - `"user"` → [`local_user_scope`] — the daemon is per-user, so one stable
//!   local identity covers every package the operator installs globally;
//! - `"repository"` → the caller's anchor repository (the checkout whose
//!   `.codypendent/skills/` is being scanned, or the daemon's startup
//!   repository for a data-dir package that declares repository scope).
//!
//! Any other tier (system is reserved for built-ins; workspace/organization
//! have no local anchor yet) is skipped with a legible per-package failure —
//! never a scan-wide error, matching the daemon's "context is an aid, not a
//! gate" startup ethos. Registration itself is idempotent: `register_package`
//! reuses the existing identity's id and flags a content-hash change at an
//! unchanged version as `Modified`, so re-scanning on every boot is safe.

use std::path::{Path, PathBuf};

use codypendent_protocol::{RepositoryId, UserId};
use sqlx::SqlitePool;

use crate::codegraph::stable_repository_id;
use crate::manifest::SkillManifest;
use crate::registry::{Registry, RegistryError};
use crate::types::{RegistryItem, RegistryStatus, Scope};

/// The stable key of the local operator's user scope. The daemon serves exactly
/// one OS user, so their globally-installed skills all live under this one
/// identity — and the run-context assembler queries it (alongside System + the
/// run's repository) so those skills are actually retrievable.
pub const LOCAL_USER_KEY: &str = "local";

/// The [`Scope`] a `<data_dir>/skills/` package declaring `scope = "user"` is
/// registered under — and the scope the executor widens its context query with.
/// One function, two callers, so registration and retrieval can never disagree
/// on the key.
#[must_use]
pub fn local_user_scope() -> Scope {
    Scope::User(UserId(LOCAL_USER_KEY.to_string()))
}

/// Derive the stable [`RepositoryId`] for `root`, exactly as the daemon's scan
/// does (`codypendentd::scan::repository_id_for`): resolve the Git toplevel
/// (canonicalized) when `root` is inside a checkout, else canonicalize the path
/// itself, then hash it via [`stable_repository_id`]. The CLI installs skills
/// against this identity, so a package registered by `codypendent skill add`
/// resolves to the SAME repository a later daemon run attributes its context
/// to — a mismatch here would make the installed skill silently invisible.
#[must_use]
pub fn anchor_repository_id(root: &Path) -> RepositoryId {
    let canonical = git_toplevel(root)
        .unwrap_or_else(|| root.canonicalize().unwrap_or_else(|_| root.to_path_buf()));
    stable_repository_id(&canonical)
}

/// The canonicalized `git rev-parse --show-toplevel` for `root`, or `None`
/// outside a checkout. Shelling out mirrors the daemon's discovery so the two
/// derivations agree byte-for-byte.
fn git_toplevel(root: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8(output.stdout).ok()?.trim());
    if path.as_os_str().is_empty() {
        return None;
    }
    Some(path.canonicalize().unwrap_or(path))
}

/// A failure ingesting one skill package directory.
#[derive(Debug, thiserror::Error)]
pub enum SkillInstallError {
    /// Reading the package directory or copying it into the skills root failed.
    #[error("skill package I/O: {0}")]
    Io(#[from] std::io::Error),
    /// The package's `skill.toml` did not parse during the tier peek.
    #[error("parsing skill.toml: {0}")]
    Toml(#[from] toml::de::Error),
    /// The declared scope tier has no local registration anchor.
    #[error(
        "scope `{0}` cannot be registered locally (supported: \"user\", \"repository\"; \
         \"system\" is reserved for built-ins)"
    )]
    UnsupportedScope(String),
    /// The manifest `id` is not usable as a directory name under the skills
    /// root. `load_package` never constrains the id, so an install that derived
    /// its destination directly from it would let `id = "../../.ssh"` place —
    /// and `remove_dir_all` — files anywhere the daemon can write.
    #[error(
        "skill id `{0}` is not a safe directory name (allowed: letters, digits, \
         `.`, `-`, `_`; no path separators)"
    )]
    UnsafeId(String),
    /// Loading/validating/registering the package failed.
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

/// The outcome of scanning one skills root: every package that registered, and
/// every directory that failed with why. A scan itself never fails — a broken
/// package must not block the daemon (or its sibling packages) from starting.
#[derive(Debug, Default)]
pub struct SkillScanOutcome {
    /// Successfully registered items, in directory order.
    pub registered: Vec<RegistryItem>,
    /// `(package directory, reason)` for each package that did not register.
    pub failures: Vec<(PathBuf, String)>,
}

/// Scan `root` for skill packages — each immediate subdirectory holding a
/// `skill.toml` — and register every valid one, anchoring repository-tier
/// manifests to `anchor_repository` (see the module docs). A missing or empty
/// `root` is a clean no-op, so the daemon can probe both well-known roots
/// unconditionally on every boot.
pub async fn scan_skill_root(
    pool: &SqlitePool,
    root: &Path,
    anchor_repository: RepositoryId,
) -> SkillScanOutcome {
    let mut outcome = SkillScanOutcome::default();
    let Ok(entries) = std::fs::read_dir(root) else {
        return outcome; // absent root — nothing installed, nothing to do
    };
    // Sorted so registration order (and any Modified-flag races on duplicate
    // identities) is deterministic across boots, like the code-graph walk.
    let mut package_dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join("skill.toml").is_file())
        .collect();
    package_dirs.sort();

    for dir in package_dirs {
        match register_dir(pool, &dir, anchor_repository).await {
            Ok(item) => outcome.registered.push(item),
            Err(error) => outcome.failures.push((dir, error.to_string())),
        }
    }
    outcome
}

/// Register the package at `dir` under the scope its own manifest declares,
/// resolved against the local anchors. The peek re-reads `skill.toml`, but
/// `register_package` is the sole validator — this only extracts the tier.
async fn register_dir(
    pool: &SqlitePool,
    dir: &Path,
    anchor_repository: RepositoryId,
) -> Result<RegistryItem, SkillInstallError> {
    let scope = declared_scope(dir, anchor_repository)?;
    Ok(Registry::new().register_package(pool, dir, scope).await?)
}

/// Peek the package's declared scope tier and map it onto a concrete local
/// [`Scope`] (see the module docs for the anchoring rules).
fn declared_scope(dir: &Path, anchor_repository: RepositoryId) -> Result<Scope, SkillInstallError> {
    let raw = std::fs::read_to_string(dir.join("skill.toml"))?;
    let manifest: SkillManifest = toml::from_str(&raw)?;
    match manifest.scope.as_str() {
        "user" => Ok(local_user_scope()),
        "repository" => Ok(Scope::Repository(anchor_repository)),
        other => Err(SkillInstallError::UnsupportedScope(other.to_string())),
    }
}

/// Install the package at `source` into `skills_root` (the data-dir skills
/// directory) and register it — the engine under `codypendent skill add <dir>`.
///
/// Order is validate → copy → register, each step honest about partial state:
///
/// 1. **Validate in place** via [`crate::manifest::load_package`] under the
///    scope the manifest declares, so a broken package is rejected before the
///    skills root is touched at all.
/// 2. **Copy** the whole package to a temporary sibling inside `skills_root`,
///    then swap it into `<skills_root>/<id>/` — the previous install (if any)
///    is removed only after the fresh copy fully landed, so a failed copy never
///    destroys a working install.
/// 3. **Register** the *installed* copy (not `source`), so the registry row's
///    provenance names the path the daemon's startup scan will re-verify on
///    every boot.
///
/// Returns the registered item and the installed path.
pub async fn install_package(
    pool: &SqlitePool,
    source: &Path,
    skills_root: &Path,
    anchor_repository: RepositoryId,
) -> Result<(RegistryItem, PathBuf), SkillInstallError> {
    let scope = declared_scope(source, anchor_repository)?;
    // Full validation (entrypoints, version, status, size ceiling, hash) before
    // any filesystem mutation; the blocking walk runs off the async runtime
    // exactly as `register_package` runs it.
    let source_owned = source.to_path_buf();
    let scope_for_load = scope.clone();
    let validated = tokio::task::spawn_blocking(move || {
        crate::manifest::load_package(&source_owned, scope_for_load)
    })
    .await
    .map_err(|join| RegistryError::Corrupt(format!("package load task failed: {join}")))?
    .map_err(RegistryError::from)?;

    // The destination directory is named by the manifest's own `id`, which
    // `load_package` never constrains — so it is checked here before it can
    // reach `join`/`remove_dir_all` (see [`SkillInstallError::UnsafeId`]).
    let dir_name = safe_install_dir_name(&validated.name)?;
    // Create the root first so it canonicalizes: comparing a canonical source
    // against a non-canonical destination could otherwise miss the "already
    // installed" case and delete the very directory being installed from.
    std::fs::create_dir_all(skills_root)?;
    let skills_root = skills_root.canonicalize()?;
    let destination = skills_root.join(dir_name);
    let source_canonical = source.canonicalize()?;
    if source_canonical == destination {
        // `skill add` pointed at the already-installed copy: just (re)register.
        let item = Registry::new()
            .register_package(pool, &destination, scope)
            .await?;
        return Ok((item, destination));
    }

    // Copy to a temporary sibling, then swap — never leave a half-copied
    // package where the startup scan would find it. The staging name is
    // dot-prefixed so a concurrent scan (which only walks package directories
    // holding a `skill.toml`) would in any case ignore it.
    let staging = skills_root.join(format!(".{dir_name}.installing"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    copy_dir(&source_canonical, &staging)?;
    if destination.exists() {
        std::fs::remove_dir_all(&destination)?;
    }
    std::fs::rename(&staging, &destination)?;

    let item = Registry::new()
        .register_package(pool, &destination, scope)
        .await?;
    Ok((item, destination))
}

/// The directory name a package with manifest id `id` installs under, rejecting
/// anything that is not a single, plain path component. Skill ids are dotted
/// names (`rust.fix-ci`), so the allowed set is letters, digits, `.`, `-` and
/// `_`; `.` and `..` are refused outright even though their characters pass.
fn safe_install_dir_name(id: &str) -> Result<&str, SkillInstallError> {
    let plain = !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if plain {
        Ok(id)
    } else {
        Err(SkillInstallError::UnsafeId(id.to_string()))
    }
}

/// The operator's global skills root under a resolved `data_dir` — the
/// directory `codypendent skill add` installs into and the daemon scans on
/// every boot. One function so the installer and the scanner can never disagree
/// on where packages live.
#[must_use]
pub fn user_skills_root(data_dir: &Path) -> PathBuf {
    data_dir.join("skills")
}

/// A checkout's repo-local skills root — packages committed alongside the code
/// they serve, scanned (never written) by the daemon when it warms a repository.
#[must_use]
pub fn repository_skills_root(repository_root: &Path) -> PathBuf {
    repository_root.join(".codypendent").join("skills")
}

/// Whether `status` will actually be retrievable: the funnel hard-filters
/// everything but [`RegistryStatus::Active`], so callers surface a warning for
/// anything else at install time instead of leaving the operator to discover a
/// silently never-disclosed skill (the exact failure the 2026-08-11 review
/// found shipped).
#[must_use]
pub fn is_retrievable_status(status: RegistryStatus) -> bool {
    status == RegistryStatus::Active
}

/// Recursively copy a directory tree (regular files + directories only;
/// symlinks are skipped — a package must be self-contained, and following links
/// out of it would copy files its content hash never covered).
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = to.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a minimal valid skill package at `dir`.
    fn write_package(dir: &Path, id: &str, scope: &str, status: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let manifest = format!(
            "schema_version = 1\n\
             id = \"{id}\"\n\
             name = \"Test Skill\"\n\
             version = \"0.1.0\"\n\
             scope = \"{scope}\"\n\
             status = \"{status}\"\n\
             description = \"A test skill.\"\n\
             \n\
             [entrypoints]\n\
             instructions = \"SKILL.md\"\n\
             \n\
             [trust]\n\
             publisher = \"local-user\"\n\
             signature_required = false\n"
        );
        std::fs::write(dir.join("skill.toml"), manifest).unwrap();
        std::fs::write(dir.join("SKILL.md"), "# Test\n").unwrap();
    }

    async fn temp_pool() -> (tempfile::TempDir, SqlitePool) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::db::open(&tmp.path().join("t.db")).await.unwrap();
        (tmp, pool)
    }

    #[tokio::test]
    async fn scan_registers_user_and_repository_tier_packages() {
        let (_tmp, pool) = temp_pool().await;
        let root = tempfile::tempdir().unwrap();
        let repo = RepositoryId::new();

        write_package(
            &root.path().join("one"),
            "test.user-skill",
            "user",
            "active",
        );
        write_package(
            &root.path().join("two"),
            "test.repo-skill",
            "repository",
            "active",
        );

        let outcome = scan_skill_root(&pool, root.path(), repo).await;
        assert!(
            outcome.failures.is_empty(),
            "no failures expected: {:?}",
            outcome.failures
        );
        assert_eq!(outcome.registered.len(), 2);
        // The user-tier package landed under the stable local user scope; the
        // repository-tier one anchored to the caller's repository.
        let user = outcome
            .registered
            .iter()
            .find(|item| item.name == "test.user-skill")
            .unwrap();
        assert_eq!(user.scope, local_user_scope());
        let repo_item = outcome
            .registered
            .iter()
            .find(|item| item.name == "test.repo-skill")
            .unwrap();
        assert_eq!(repo_item.scope, Scope::Repository(repo));
    }

    #[tokio::test]
    async fn scan_is_idempotent_and_a_broken_package_never_blocks_its_siblings() {
        let (_tmp, pool) = temp_pool().await;
        let root = tempfile::tempdir().unwrap();
        let repo = RepositoryId::new();

        write_package(&root.path().join("good"), "test.good", "user", "active");
        // A package whose declared entrypoint is missing on disk: rejected by
        // `load_package`, collected as a per-package failure.
        let broken = root.path().join("broken");
        write_package(&broken, "test.broken", "user", "active");
        std::fs::remove_file(broken.join("SKILL.md")).unwrap();
        // A tier with no local anchor: skipped legibly, not a scan error.
        write_package(&root.path().join("ws"), "test.ws", "workspace", "active");

        let outcome = scan_skill_root(&pool, root.path(), repo).await;
        assert_eq!(outcome.registered.len(), 1, "only the good package lands");
        assert_eq!(outcome.registered[0].name, "test.good");
        assert_eq!(outcome.failures.len(), 2, "{:?}", outcome.failures);
        assert!(
            outcome
                .failures
                .iter()
                .any(|(dir, reason)| dir.ends_with("ws") && reason.contains("workspace")),
            "the unsupported tier names itself: {:?}",
            outcome.failures
        );

        // Re-scan: same identity re-registers in place (id stable, still one row
        // per identity) — safe to run on every daemon boot.
        let first_id = outcome.registered[0].id;
        let again = scan_skill_root(&pool, root.path(), repo).await;
        assert_eq!(again.registered.len(), 1);
        assert_eq!(again.registered[0].id, first_id, "identity is stable");
    }

    #[tokio::test]
    async fn scan_of_a_missing_root_is_a_clean_no_op() {
        let (_tmp, pool) = temp_pool().await;
        let outcome = scan_skill_root(
            &pool,
            Path::new("/definitely/not/a/real/skills/root"),
            RepositoryId::new(),
        )
        .await;
        assert!(outcome.registered.is_empty());
        assert!(outcome.failures.is_empty());
    }

    #[tokio::test]
    async fn install_copies_into_the_skills_root_and_registers_the_installed_copy() {
        let (_tmp, pool) = temp_pool().await;
        let source = tempfile::tempdir().unwrap();
        let skills_root_dir = tempfile::tempdir().unwrap();
        let skills_root = skills_root_dir.path().join("skills");
        write_package(source.path(), "test.installed", "user", "active");

        let (item, installed) =
            install_package(&pool, source.path(), &skills_root, RepositoryId::new())
                .await
                .unwrap();

        assert_eq!(item.name, "test.installed");
        assert_eq!(
            installed,
            skills_root
                .join("test.installed")
                .canonicalize()
                .expect("installed package path canonicalizes")
        );
        assert!(installed.join("skill.toml").is_file(), "package was copied");
        // Provenance names the INSTALLED path — the one the startup scan re-walks
        // — not the transient source directory.
        match &item.provenance {
            crate::types::Provenance::Package { path } => {
                assert!(
                    path.contains("test.installed"),
                    "provenance should name the installed copy: {path}"
                );
            }
            other => panic!("expected package provenance, got {other:?}"),
        }
        // The startup scan finds the installed copy and re-registers the same
        // identity (the id survives).
        let rescan = scan_skill_root(&pool, &skills_root, RepositoryId::new()).await;
        assert_eq!(rescan.registered.len(), 1);
        assert_eq!(rescan.registered[0].id, item.id);
    }

    #[tokio::test]
    async fn install_replaces_a_previous_install_without_duplicating_rows() {
        let (_tmp, pool) = temp_pool().await;
        let skills_root_dir = tempfile::tempdir().unwrap();
        let skills_root = skills_root_dir.path().join("skills");
        let repo = RepositoryId::new();

        let v1 = tempfile::tempdir().unwrap();
        write_package(v1.path(), "test.replace", "user", "active");
        let (first, _) = install_package(&pool, v1.path(), &skills_root, repo)
            .await
            .unwrap();

        // A changed package at the SAME version: the copy replaces the install
        // and the registry flags the unchanged-version hash change as Modified.
        let v2 = tempfile::tempdir().unwrap();
        write_package(v2.path(), "test.replace", "user", "active");
        std::fs::write(v2.path().join("SKILL.md"), "# Edited\n").unwrap();
        let (second, installed) = install_package(&pool, v2.path(), &skills_root, repo)
            .await
            .unwrap();

        assert_eq!(second.id, first.id, "identity survives re-install");
        assert_eq!(second.status, RegistryStatus::Modified);
        assert_eq!(
            std::fs::read_to_string(installed.join("SKILL.md")).unwrap(),
            "# Edited\n",
            "the installed copy is the new content"
        );
        let all = Registry::new().list(&pool).await.unwrap();
        assert_eq!(all.len(), 1, "one row per identity");
    }

    #[tokio::test]
    async fn install_rejects_an_invalid_package_before_touching_the_root() {
        let (_tmp, pool) = temp_pool().await;
        let source = tempfile::tempdir().unwrap();
        let skills_root_dir = tempfile::tempdir().unwrap();
        let skills_root = skills_root_dir.path().join("skills");
        write_package(source.path(), "test.broken", "user", "active");
        std::fs::remove_file(source.path().join("SKILL.md")).unwrap();

        let error = install_package(&pool, source.path(), &skills_root, RepositoryId::new())
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("SKILL.md"),
            "the entrypoint failure is legible: {error}"
        );
        assert!(
            !skills_root.join("test.broken").exists(),
            "an invalid package must never land in the skills root"
        );
    }

    /// A manifest whose `id` is a traversal must never reach the filesystem:
    /// `load_package` does not constrain the id, so the install is the only
    /// place that can stop `id = "../escaped"` from writing (and deleting)
    /// outside the skills root.
    #[tokio::test]
    async fn install_refuses_a_package_id_that_escapes_the_skills_root() {
        let (_tmp, pool) = temp_pool().await;
        let source = tempfile::tempdir().unwrap();
        let skills_root_dir = tempfile::tempdir().unwrap();
        let skills_root = skills_root_dir.path().join("skills");
        write_package(source.path(), "../escaped", "user", "active");

        let error = install_package(&pool, source.path(), &skills_root, RepositoryId::new())
            .await
            .unwrap_err();
        assert!(
            matches!(error, SkillInstallError::UnsafeId(ref id) if id == "../escaped"),
            "the traversal must be named and refused, got: {error}"
        );
        assert!(
            !skills_root_dir.path().join("escaped").exists(),
            "nothing may be written outside the skills root"
        );
    }

    #[test]
    fn safe_install_dir_name_admits_ids_and_refuses_separators() {
        assert_eq!(safe_install_dir_name("rust.fix-ci").unwrap(), "rust.fix-ci");
        assert_eq!(safe_install_dir_name("a_b.C9").unwrap(), "a_b.C9");
        for bad in ["", ".", "..", "../x", "a/b", "a\\b", "a b"] {
            assert!(
                safe_install_dir_name(bad).is_err(),
                "`{bad}` must not be a package directory name"
            );
        }
    }

    #[test]
    fn the_well_known_roots_are_spelled_once() {
        assert_eq!(
            user_skills_root(Path::new("/data")),
            Path::new("/data/skills")
        );
        assert_eq!(
            repository_skills_root(Path::new("/repo")),
            Path::new("/repo/.codypendent/skills")
        );
    }

    #[test]
    fn anchor_repository_id_is_stable_and_distinct() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert_eq!(
            anchor_repository_id(a.path()),
            anchor_repository_id(a.path())
        );
        assert_ne!(
            anchor_repository_id(a.path()),
            anchor_repository_id(b.path())
        );
    }
}
