//! Executing a skill's behaviour — `scripts/` through the OS sandbox and
//! `[entrypoints] module` through the WASM host (STEP 6.3 / 6.4).
//!
//! Phase 2 recorded a skill's `scripts/` but refused to run them. STEP 6.2 built
//! the OS sandbox ([`codypendent_sandbox::executor`]) and STEP 6.3 adds the WASM
//! guest runtime ([`codypendent_sandbox::wasm`]), so a skill's declared
//! `[permissions]` and `[limits]` lower into a closed
//! [`SandboxProfile`](codypendent_sandbox::SandboxProfile) and a named script or
//! module runs confined — output captured, sanitized, and origin-labeled.
//!
//! # The single production entry point
//!
//! [`SkillRunner`] is it. It takes a [`RegistryItem`] rather than a bare path so
//! everything the registry knows is enforced before anything executes:
//!
//! * the item is `Active` (a draft or deprecated skill never runs);
//! * the item is `executable` (it actually carries a script or a module);
//! * the package on disk still hashes to the item's recorded `content_hash`, so
//!   a package cannot be swapped after the user approved it;
//! * the entrypoint resolves *inside* the package directory;
//! * the manifest's `[limits]` become the profile's resource caps;
//! * `$REPOSITORY` / `$WORKTREE` / `$HOME` in `[permissions]` are substituted
//!   against a caller-supplied context, and an **unresolved** placeholder is an
//!   error rather than a verbatim path.
//!
//! # The capability model
//!
//! A skill's manifest is a **ceiling**, not a grant. The WASM path is
//! constructed around a [`RunPolicyGate`], so every privileged act a guest
//! attempts is re-checked against the daemon's deny-first run policy — the same
//! engine every model-proposed side effect passes. See
//! `.impl/threat-models/12-executable-skills.md` §0 for why the dependency is
//! inverted rather than the two capability enums being converted.
//!
//! # Permission → profile
//!
//! | `[permissions]`   | profile field         |
//! |-------------------|-----------------------|
//! | `filesystem_read` | `read_paths`          |
//! | `filesystem_write`| `write_paths`         |
//! | `commands` (any)  | `allow_subprocess`    |
//!
//! `network` and `secrets` are refused at package load
//! ([`ManifestError::UnenforceableCapability`]) because no executor can honour
//! them yet, so they never reach a profile.
//!
//! The script's own package directory is always granted read so the sandbox can
//! read the script file. **Executing** it is a separate grant: a script that runs
//! through an interpreter (a `#!/bin/sh` shebang) needs a `commands` capability
//! (which grants subprocess), because the sandbox scopes `exec` to the script
//! image alone unless subprocess is allowed. Granting the interpreter `exec` is
//! deliberately *not* done, as that would weaken the sandbox.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use codypendent_sandbox::{
    enforcing_executor, RunPolicyGate, SandboxCommand, SandboxError, SandboxExecutor,
    SandboxOutcome, SandboxProfile, WasmError, WasmHost, WasmLimits, WasmOutcome, ENV_ALLOWLIST,
};

use crate::manifest::{hash_package, ManifestError, SkillManifest, SkillResourceLimits};
use crate::types::{CapabilityRequest, Provenance, RegistryItem, RegistryStatus};

/// A failure resolving or running a skill's behaviour.
#[derive(Debug, thiserror::Error)]
pub enum SkillExecError {
    /// The named script does not exist under the skill's package directory.
    #[error("skill script `{0}` not found under the package directory")]
    ScriptNotFound(String),
    /// The resolved path escapes the skill package directory (a `../` or
    /// symlink traversal) — refused so a skill can only run its own bundled code.
    #[error("skill entrypoint `{path}` escapes the package directory")]
    ScriptEscapesPackage {
        /// The offending relative path.
        path: String,
    },
    /// The script is not an executable file (no execute bit).
    #[error("skill script `{0}` is not an executable file (chmod +x and add a shebang)")]
    ScriptNotExecutable(String),
    /// A `[permissions]` value contains a placeholder with no value in the
    /// caller's context.
    ///
    /// This fails **loudly**. Before, an unsubstituted `$REPOSITORY` was passed
    /// to the OS layer verbatim, where it silently matched nothing — so the
    /// shipped skill had no effective filesystem grant at all and nobody could
    /// tell why.
    #[error("skill permission `{value}` contains the unresolved placeholder `{placeholder}`")]
    UnresolvedPlaceholder {
        /// The whole declared value.
        value: String,
        /// The placeholder that could not be resolved.
        placeholder: String,
    },
    /// A `[permissions]` path is not absolute after substitution. Every
    /// downstream check requires an absolute path and silently refuses a
    /// relative one, so it is caught here instead.
    #[error("skill permission `{0}` is not an absolute path after substitution")]
    RelativePermission(String),
    /// The registry item is not in a state that may execute.
    #[error("skill `{name}` is not runnable: {reason}")]
    NotRunnable {
        /// The item's registry name.
        name: String,
        /// Why it was refused.
        reason: String,
    },
    /// The package on disk no longer matches the hash the registry recorded.
    /// A skill's contents are what the user approved; substituting them
    /// afterwards is exactly the attack the content hash exists to catch.
    #[error("skill `{name}` package contents changed since registration (expected {expected}, found {found})")]
    ContentHashMismatch {
        /// The item's registry name.
        name: String,
        /// The hash recorded at registration.
        expected: String,
        /// The hash of what is on disk now.
        found: String,
    },
    /// Re-reading or re-validating the package's `skill.toml` failed.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// The sandbox refused or failed the run (fails closed).
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
    /// The WASM host refused or terminated the guest (fails closed).
    #[error(transparent)]
    Wasm(#[from] WasmError),
    /// An I/O error resolving a path.
    #[error("resolving skill entrypoint: {0}")]
    Io(#[from] std::io::Error),
}

/// The values `$REPOSITORY` / `$WORKTREE` / `$HOME` resolve to for one
/// execution. Supplied by the caller because they are properties of the *run*,
/// not of the package.
#[derive(Debug, Clone)]
pub struct PlaceholderContext {
    repository: PathBuf,
    worktree: PathBuf,
    home: Option<PathBuf>,
}

impl PlaceholderContext {
    /// Build a context. Both roots must be absolute; a relative root would
    /// produce a grant every downstream check silently ignores.
    pub fn new(
        repository: impl Into<PathBuf>,
        worktree: impl Into<PathBuf>,
    ) -> Result<Self, SkillExecError> {
        let (repository, worktree) = (repository.into(), worktree.into());
        for root in [&repository, &worktree] {
            if !root.is_absolute() {
                return Err(SkillExecError::RelativePermission(
                    root.display().to_string(),
                ));
            }
        }
        Ok(Self {
            repository,
            worktree,
            home: None,
        })
    }

    /// Supply `$HOME`. Absent by default: a skill that wants the user's home
    /// directory should have to be given it explicitly.
    #[must_use]
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }

    /// The value for `name`, or `None` when this context does not define it.
    ///
    /// The table is **exhaustive and closed**: an unknown `$NAME` resolves to
    /// `None` and becomes an error, so a manifest cannot smuggle in a
    /// placeholder the host does not understand and have it survive as a
    /// literal path segment.
    fn lookup(&self, name: &str) -> Option<&Path> {
        match name {
            "REPOSITORY" => Some(&self.repository),
            "WORKTREE" => Some(&self.worktree),
            "HOME" => self.home.as_deref(),
            _ => None,
        }
    }
}

/// Substitute every `$NAME` in `value` from `ctx`, refusing any that does not
/// resolve.
///
/// Only whole `$NAME` tokens are recognised (`$REPOSITORY/src` substitutes;
/// `$$` and a bare `$` are literals). An unresolved placeholder is an error, so
/// a typo produces a refusal rather than an ineffective grant.
pub fn substitute_placeholders(
    value: &str,
    ctx: &PlaceholderContext,
) -> Result<String, SkillExecError> {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        let end = tail
            .find(|c: char| !(c.is_ascii_uppercase() || c == '_'))
            .unwrap_or(tail.len());
        let name = &tail[..end];
        if name.is_empty() {
            // A bare `$` is a literal.
            out.push('$');
            rest = tail;
            continue;
        }
        let Some(resolved) = ctx.lookup(name) else {
            return Err(SkillExecError::UnresolvedPlaceholder {
                value: value.to_string(),
                placeholder: format!("${name}"),
            });
        };
        out.push_str(&resolved.to_string_lossy());
        rest = &tail[end..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Substitute a permission path and require the result be absolute.
fn permission_path(value: &str, ctx: &PlaceholderContext) -> Result<String, SkillExecError> {
    let resolved = substitute_placeholders(value, ctx)?;
    if !Path::new(&resolved).is_absolute() {
        return Err(SkillExecError::RelativePermission(resolved));
    }
    Ok(resolved)
}

/// Lower a skill's flattened `[permissions]` and resolved `[limits]` into a
/// closed [`SandboxProfile`].
///
/// `label` becomes the profile's plugin/origin tag (e.g. `skill:rust.fix-ci`).
/// Any [`Command`](CapabilityRequest::Command) permission grants
/// `allow_subprocess`. Resource caps come from the manifest, not from constants
/// here: a skill's `[limits]` used to be parsed and discarded, so a declared
/// 1800-second budget ran under a hardcoded 60-second clock.
///
/// `network` and `secrets` never reach this function — [`crate::load_package`]
/// refuses them, since no executor can enforce either. They are matched
/// exhaustively anyway so a future capability cannot be silently dropped.
pub fn profile_for_permissions(
    label: impl Into<String>,
    permissions: &[CapabilityRequest],
    limits: &SkillResourceLimits,
    ctx: &PlaceholderContext,
) -> Result<SandboxProfile, SkillExecError> {
    let mut read_paths = Vec::new();
    let mut write_paths = Vec::new();
    let mut network_allowlist = Vec::new();
    let mut brokered_secrets = Vec::new();
    let mut allow_subprocess = false;
    for perm in permissions {
        match perm {
            CapabilityRequest::FilesystemRead(p) => read_paths.push(permission_path(p, ctx)?),
            CapabilityRequest::FilesystemWrite(p) => write_paths.push(permission_path(p, ctx)?),
            CapabilityRequest::Network(n) => network_allowlist.push(n.clone()),
            CapabilityRequest::Secret(s) => brokered_secrets.push(s.clone()),
            // A declared command capability means the script may spawn subprocesses.
            CapabilityRequest::Command(_) => allow_subprocess = true,
        }
    }
    Ok(SandboxProfile {
        plugin: label.into(),
        env_allowlist: ENV_ALLOWLIST.iter().map(|s| (*s).to_string()).collect(),
        read_paths,
        write_paths,
        network_allowlist,
        brokered_secrets,
        allow_subprocess,
        memory_mb: limits.memory_mb,
        cpu_seconds: limits.cpu_seconds,
        wall_seconds: limits.wall_seconds,
        maximum_output_mb: limits.output_mb,
    })
}

/// Resolve `relpath` inside `skill_dir`, refusing anything that escapes.
fn resolve_within(skill_dir: &Path, relpath: &str) -> Result<(PathBuf, PathBuf), SkillExecError> {
    let root = skill_dir.canonicalize()?;
    let resolved = root
        .join(relpath)
        .canonicalize()
        .map_err(|_| SkillExecError::ScriptNotFound(relpath.to_string()))?;
    if !resolved.starts_with(&root) {
        return Err(SkillExecError::ScriptEscapesPackage {
            path: relpath.to_string(),
        });
    }
    Ok((root, resolved))
}

/// Grant read on the package root so the sandbox can read the entrypoint file
/// (and any bundled references it reads) and use the package as its cwd. This
/// grants read, not exec.
fn with_package_read(profile: &SandboxProfile, root: &Path) -> SandboxProfile {
    let mut confined = profile.clone();
    let root_str = root.to_string_lossy().into_owned();
    if !confined.read_paths.iter().any(|p| p == &root_str) {
        confined.read_paths.push(root_str);
    }
    confined
}

/// Run a skill's script through the sandbox `executor`.
///
/// Fails closed: a missing/non-executable script, a path escaping the package,
/// an unavailable/unsupported sandbox, or (for a shebang script) a missing
/// subprocess grant all refuse to run or cannot launch.
pub fn run_script(
    executor: &dyn SandboxExecutor,
    skill_dir: &Path,
    script_relpath: &str,
    args: Vec<String>,
    profile: &SandboxProfile,
) -> Result<SandboxOutcome, SkillExecError> {
    let (root, script) = resolve_within(skill_dir, script_relpath)?;
    if !is_executable_file(&script) {
        return Err(SkillExecError::ScriptNotExecutable(
            script_relpath.to_string(),
        ));
    }
    let confined = with_package_read(profile, &root);
    let origin = if confined.plugin.is_empty() {
        "skill".to_string()
    } else {
        confined.plugin.clone()
    };
    let command = SandboxCommand::new(script, args, root, origin);
    Ok(executor.run(&confined, &command)?)
}

/// Run a skill's WebAssembly module through `host`.
///
/// `gate` is the run policy the guest's privileged calls are checked against.
/// There is deliberately no variant without one: a WASM host gated only on the
/// package's own manifest would be a second, weaker policy path.
pub fn run_module(
    host: &WasmHost,
    skill_dir: &Path,
    module_relpath: &str,
    input: &[u8],
    profile: &SandboxProfile,
    gate: Arc<dyn RunPolicyGate>,
) -> Result<WasmOutcome, SkillExecError> {
    let (root, module) = resolve_within(skill_dir, module_relpath)?;
    let confined = with_package_read(profile, &root);
    let origin = if confined.plugin.is_empty() {
        "skill".to_string()
    } else {
        confined.plugin.clone()
    };
    let bytes = std::fs::read(&module)?;
    Ok(host.run(&bytes, input, &confined, gate, &origin)?)
}

/// Whether `path` is a regular file with an execute bit.
fn is_executable_file(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(path)
            .map(|m| m.is_file())
            .unwrap_or(false)
    }
}

/// Which of a skill's behaviours to run.
#[derive(Debug, Clone)]
pub enum SkillInvocation {
    /// A script under the package's `scripts/` entrypoint.
    Script {
        /// Path relative to the package directory (e.g. `scripts/fix.sh`).
        relpath: String,
        /// Arguments, passed verbatim.
        args: Vec<String>,
    },
    /// The package's WebAssembly module.
    Module {
        /// Path relative to the package directory (e.g. `skill.wasm`).
        relpath: String,
        /// Bytes the guest pulls with `codypendent::input`.
        input: Vec<u8>,
    },
}

/// The result of a skill execution, tagged by which runtime produced it.
#[derive(Debug)]
pub enum SkillRunOutcome {
    /// A confined OS process.
    Script(SandboxOutcome),
    /// A metered WASM guest.
    Module(WasmOutcome),
}

impl SkillRunOutcome {
    /// A one-line audit summary, whichever runtime ran.
    #[must_use]
    pub fn audit_summary(&self) -> String {
        match self {
            SkillRunOutcome::Script(outcome) => outcome.audit_summary(),
            SkillRunOutcome::Module(outcome) => outcome.audit_summary(),
        }
    }

    /// Whether the behaviour completed successfully.
    #[must_use]
    pub fn success(&self) -> bool {
        match self {
            SkillRunOutcome::Script(outcome) => outcome.success(),
            SkillRunOutcome::Module(outcome) => outcome.success(),
        }
    }
}

/// The production entry point for executing a registered skill's behaviour.
///
/// Holds the two things a run needs and a caller must not be able to vary
/// per-invocation: the OS sandbox backend and the run-policy gate.
pub struct SkillRunner {
    executor: Box<dyn SandboxExecutor>,
    gate: Arc<dyn RunPolicyGate>,
}

impl SkillRunner {
    /// Build a runner over an explicit executor — the seam tests and alternative
    /// hosts use.
    #[must_use]
    pub fn new(executor: Box<dyn SandboxExecutor>, gate: Arc<dyn RunPolicyGate>) -> Self {
        Self { executor, gate }
    }

    /// Build a runner over this platform's enforcing sandbox, or fail closed.
    pub fn enforcing(gate: Arc<dyn RunPolicyGate>) -> Result<Self, SkillExecError> {
        Ok(Self::new(enforcing_executor()?, gate))
    }

    /// What the active backend actually enforces on this host.
    ///
    /// The typed capability report existed with no caller outside its own
    /// tests, so a host with no `bwrap` produced a runtime `ToolUnavailable`
    /// instead of an install-time diagnostic. Callers render this when a skill
    /// is installed and when a run is refused.
    #[must_use]
    pub fn capability_diagnostic(&self) -> String {
        self.executor.capability_report().diagnostic()
    }

    /// Whether the backend enforces the four exit-criterion properties.
    #[must_use]
    pub fn enforces_exit_criteria(&self) -> bool {
        self.executor.capability_report().enforces_exit_criteria()
    }

    /// Run `invocation` for the registered skill `item`.
    ///
    /// Every precondition is checked before anything executes; see the module
    /// docs for the list and why each is there.
    pub fn run(
        &self,
        item: &RegistryItem,
        invocation: &SkillInvocation,
        ctx: &PlaceholderContext,
    ) -> Result<SkillRunOutcome, SkillExecError> {
        let dir = self.package_dir(item)?;

        // The package's contents are what the user saw and approved. Re-hash
        // before running so a package swapped after registration is refused
        // rather than executed under the old approval.
        let found = hash_package(&dir)?;
        if found != item.content_hash {
            return Err(SkillExecError::ContentHashMismatch {
                name: item.name.clone(),
                expected: item.content_hash.clone(),
                found,
            });
        }

        let raw = std::fs::read_to_string(dir.join("skill.toml"))?;
        let manifest: SkillManifest = toml::from_str(&raw).map_err(ManifestError::from)?;
        let limits = manifest.limits.resolve()?;
        let label = format!("skill:{}", item.name);
        let profile = profile_for_permissions(label, &item.permissions, &limits, ctx)?;

        match invocation {
            SkillInvocation::Script { relpath, args } => {
                let declared = manifest.entrypoints.scripts.as_deref().ok_or_else(|| {
                    SkillExecError::NotRunnable {
                        name: item.name.clone(),
                        reason: "the package declares no `scripts` entrypoint".into(),
                    }
                })?;
                // A script must live under the declared scripts entrypoint, not
                // merely inside the package: the manifest names where behaviour
                // lives, and a caller must not be able to run some other file
                // the package happens to ship.
                if !relpath.starts_with(declared.trim_end_matches('/')) {
                    return Err(SkillExecError::ScriptEscapesPackage {
                        path: relpath.clone(),
                    });
                }
                let outcome = run_script(
                    self.executor.as_ref(),
                    &dir,
                    relpath,
                    args.clone(),
                    &profile,
                )?;
                Ok(SkillRunOutcome::Script(outcome))
            }
            SkillInvocation::Module { relpath, input } => {
                let declared = manifest.entrypoints.module.as_deref().ok_or_else(|| {
                    SkillExecError::NotRunnable {
                        name: item.name.clone(),
                        reason: "the package declares no `module` entrypoint".into(),
                    }
                })?;
                if relpath != declared {
                    return Err(SkillExecError::ScriptEscapesPackage {
                        path: relpath.clone(),
                    });
                }
                let host = WasmHost::new(WasmLimits::from_profile(&profile))?;
                let outcome = run_module(
                    &host,
                    &dir,
                    relpath,
                    input,
                    &profile,
                    Arc::clone(&self.gate),
                )?;
                Ok(SkillRunOutcome::Module(outcome))
            }
        }
    }

    /// The package directory for a runnable item, refusing anything the
    /// registry says must not execute.
    fn package_dir(&self, item: &RegistryItem) -> Result<PathBuf, SkillExecError> {
        if item.status != RegistryStatus::Active {
            return Err(SkillExecError::NotRunnable {
                name: item.name.clone(),
                reason: format!("status is {:?}, not Active", item.status),
            });
        }
        if !item.executable {
            return Err(SkillExecError::NotRunnable {
                name: item.name.clone(),
                reason: "the item carries no executable behaviour".into(),
            });
        }
        match &item.provenance {
            Provenance::Package { path } => Ok(PathBuf::from(path)),
            other => Err(SkillExecError::NotRunnable {
                name: item.name.clone(),
                reason: format!("provenance {other:?} is not a package on disk"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_sandbox::RefusingSandbox;

    fn ctx() -> PlaceholderContext {
        PlaceholderContext::new("/srv/repo", "/srv/repo/.worktrees/w1").unwrap()
    }

    #[test]
    fn placeholders_are_substituted_not_passed_through() {
        let c = ctx();
        assert_eq!(
            substitute_placeholders("$REPOSITORY/src", &c).unwrap(),
            "/srv/repo/src"
        );
        assert_eq!(
            substitute_placeholders("$WORKTREE", &c).unwrap(),
            "/srv/repo/.worktrees/w1"
        );
        // Two in one value.
        assert_eq!(
            substitute_placeholders("$REPOSITORY:$WORKTREE", &c).unwrap(),
            "/srv/repo:/srv/repo/.worktrees/w1"
        );
    }

    #[test]
    fn an_unresolved_placeholder_is_an_error_not_a_literal_path() {
        // The old behaviour: `$REPOSITORY` reached the OS layer verbatim, where
        // every check silently ignored it, so the shipped skill had no effective
        // filesystem grant and nothing said so.
        let c = ctx();
        let err = substitute_placeholders("$NOT_A_THING/x", &c).unwrap_err();
        match err {
            SkillExecError::UnresolvedPlaceholder { placeholder, .. } => {
                assert_eq!(placeholder, "$NOT_A_THING");
            }
            other => panic!("expected an unresolved-placeholder error, got {other}"),
        }
        // $HOME is absent unless the caller supplies it.
        assert!(substitute_placeholders("$HOME/.ssh", &c).is_err());
        assert_eq!(
            substitute_placeholders("$HOME/.ssh", &ctx().with_home("/home/u")).unwrap(),
            "/home/u/.ssh"
        );
    }

    #[test]
    fn a_bare_dollar_is_a_literal() {
        let c = ctx();
        assert_eq!(substitute_placeholders("a$ b", &c).unwrap(), "a$ b");
        assert_eq!(substitute_placeholders("100$", &c).unwrap(), "100$");
    }

    #[test]
    fn permissions_lower_into_a_closed_profile_with_manifest_limits() {
        let perms = vec![
            CapabilityRequest::FilesystemRead("$REPOSITORY".into()),
            CapabilityRequest::FilesystemWrite("$WORKTREE/target".into()),
            CapabilityRequest::Command("cargo".into()),
        ];
        let limits = SkillResourceLimits {
            memory_mb: 256,
            cpu_seconds: 90,
            wall_seconds: 1800,
            output_mb: 16,
            maximum_iterations: Some(20),
        };
        let p = profile_for_permissions("skill:rust.fix-ci", &perms, &limits, &ctx()).unwrap();
        assert_eq!(p.plugin, "skill:rust.fix-ci");
        assert_eq!(p.read_paths, ["/srv/repo"]);
        assert_eq!(p.write_paths, ["/srv/repo/.worktrees/w1/target"]);
        assert!(p.allow_subprocess, "a command permission grants subprocess");
        // The manifest's ceilings, not the old hardcoded 128/30/60/8.
        assert_eq!(p.memory_mb, 256);
        assert_eq!(p.cpu_seconds, 90);
        assert_eq!(p.wall_seconds, 1800);
        assert_eq!(p.maximum_output_mb, 16);
    }

    #[test]
    fn a_relative_permission_is_refused_rather_than_silently_ineffective() {
        let perms = vec![CapabilityRequest::FilesystemRead("relative/path".into())];
        let err =
            profile_for_permissions("skill:x", &perms, &SkillResourceLimits::default(), &ctx())
                .unwrap_err();
        assert!(matches!(err, SkillExecError::RelativePermission(_)));
    }

    #[test]
    fn no_command_permission_means_no_subprocess() {
        let perms = vec![CapabilityRequest::FilesystemRead("$REPOSITORY".into())];
        let p = profile_for_permissions("skill:x", &perms, &SkillResourceLimits::default(), &ctx())
            .unwrap();
        assert!(!p.allow_subprocess);
        assert_eq!(p.wall_seconds, crate::manifest::DEFAULT_SKILL_WALL_SECONDS);
    }

    #[test]
    fn a_missing_script_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let profile =
            profile_for_permissions("skill:x", &[], &SkillResourceLimits::default(), &ctx())
                .unwrap();
        let err = run_script(
            &RefusingSandbox,
            dir.path(),
            "scripts/does-not-exist.sh",
            Vec::new(),
            &profile,
        )
        .unwrap_err();
        assert!(matches!(err, SkillExecError::ScriptNotFound(_)));
    }

    #[test]
    fn a_non_executable_script_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("scripts")).unwrap();
        std::fs::write(
            dir.path().join("scripts").join("noexec.sh"),
            "#!/bin/sh\necho hi\n",
        )
        .unwrap();
        let profile =
            profile_for_permissions("skill:x", &[], &SkillResourceLimits::default(), &ctx())
                .unwrap();
        let err = run_script(
            &RefusingSandbox,
            dir.path(),
            "scripts/noexec.sh",
            Vec::new(),
            &profile,
        )
        .unwrap_err();
        assert!(matches!(err, SkillExecError::ScriptNotExecutable(_)));
    }

    #[test]
    fn an_executable_script_reaches_the_executor_then_fails_closed_off_backend() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("scripts")).unwrap();
        let script = dir.path().join("scripts").join("run.sh");
        std::fs::write(&script, "#!/bin/sh\necho ran\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let profile =
            profile_for_permissions("skill:x", &[], &SkillResourceLimits::default(), &ctx())
                .unwrap();
        let err = run_script(
            &RefusingSandbox,
            dir.path(),
            "scripts/run.sh",
            Vec::new(),
            &profile,
        )
        .unwrap_err();
        // Reached the executor and was refused (fail closed), not a resolution error.
        assert!(matches!(err, SkillExecError::Sandbox(_)));
    }

    #[test]
    fn a_script_outside_the_package_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let profile =
            profile_for_permissions("skill:x", &[], &SkillResourceLimits::default(), &ctx())
                .unwrap();
        // `../` traversal out of the package resolves elsewhere; refused.
        let err = run_script(
            &RefusingSandbox,
            dir.path(),
            "../../../bin/sh",
            Vec::new(),
            &profile,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            SkillExecError::ScriptNotFound(_) | SkillExecError::ScriptEscapesPackage { .. }
        ));
    }
}
