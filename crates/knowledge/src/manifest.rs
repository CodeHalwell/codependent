//! The skill-package loader (Chapter 05, STEP 2.2).
//!
//! A skill is a directory (`SKILL.md`, `skill.toml`, optional `tests/`,
//! `references/`, `scripts/`, `assets/`). [`load_package`] parses its
//! `skill.toml` with **exactly** the key shapes of [`specs/skill.toml`], rejects
//! packages that declare an entrypoint that is not on disk or a scope that does
//! not match the tier it is being registered under, content-hashes every file in
//! the directory, and folds the result into a [`RegistryItem`] the registry can
//! store.
//!
//! Two rules from the spec are enforced structurally here:
//! - **Unknown keys are rejected.** Every manifest struct carries
//!   `#[serde(deny_unknown_fields)]`, so a stray top-level or nested key fails to
//!   parse rather than being silently ignored.
//! - **Skill behaviours are executable (STEP 6.4).** The Phase-2 restriction that
//!   marked a script-bearing skill non-[`executable`](RegistryItem::executable) is
//!   lifted now that the OS sandbox exists: skill `scripts/` run confined through
//!   [`crate::skill_exec`] under a profile derived from the skill's
//!   `[permissions]`. Registered skills are `executable = true`; the run itself is
//!   still gated by the sandbox executor, which fails closed where no backend is
//!   available.
//!
//! [`specs/skill.toml`]: ../../../docs/specs/skill.toml

use std::path::Path;

use chrono::Utc;
use codypendent_protocol::RegistryItemId;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::types::{
    CapabilityRequest, Provenance, RegistryDependency, RegistryItem, RegistryItemKind,
    RegistryStatus, RiskClass, Scope, TrustMetadata, TrustTier, Version,
};

/// The parsed `skill.toml`. Field names and types mirror
/// [`specs/skill.toml`](../../../docs/specs/skill.toml) exactly; unknown keys are
/// rejected so a typo never disappears into a silently-ignored field.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillManifest {
    /// Manifest format version (currently `1`).
    pub schema_version: u32,
    /// The stable identity slug, e.g. `"rust.fix-ci"` — this becomes the registry
    /// item's [`name`](RegistryItem::name), the value shadowing and dependency
    /// references resolve against.
    pub id: String,
    /// The human-readable display title, e.g. `"Fix Rust CI"`.
    pub name: String,
    /// Semantic version string (`MAJOR.MINOR.PATCH`).
    pub version: String,
    /// The scope tier the skill targets (`"repository"`, `"user"`, …); must equal
    /// the tier of the [`Scope`] it is registered under.
    pub scope: String,
    /// Lifecycle status (`draft | active | modified | deprecated`).
    pub status: String,
    /// One-line summary shown on the skill's card.
    pub description: String,
    /// Task intents the skill answers, for retrieval.
    #[serde(default)]
    pub intents: Vec<String>,
    /// Languages the skill applies to (kept as keywords).
    #[serde(default)]
    pub languages: Vec<String>,
    /// Tools the skill needs — hard dependencies.
    #[serde(default)]
    pub required_tools: Vec<String>,
    /// Tools the skill can use if present — soft dependencies.
    #[serde(default)]
    pub optional_tools: Vec<String>,
    /// The `[permissions]` table, flattened into [`CapabilityRequest`]s.
    #[serde(default)]
    pub permissions: SkillPermissions,
    /// The `[limits]` table. Parsed to validate the manifest; **not persisted** —
    /// budget enforcement is Phase 5, so nothing downstream reads it yet.
    #[serde(default)]
    pub limits: SkillLimits,
    /// The `[entrypoints]` table naming the package's files/dirs.
    #[serde(default)]
    pub entrypoints: SkillEntrypoints,
    /// The `[trust]` table (publisher + signature policy).
    pub trust: SkillTrust,
}

/// The `[permissions]` table — each field a list of capability targets.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPermissions {
    #[serde(default)]
    pub filesystem_read: Vec<String>,
    #[serde(default)]
    pub filesystem_write: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub network: Vec<String>,
    #[serde(default)]
    pub secrets: Vec<String>,
}

/// The `[limits]` table.
///
/// These used to be parsed for validation and thrown away, so a skill declaring
/// `maximum_duration_seconds = 1800` actually ran under a hardcoded 60-second
/// wall clock. [`SkillLimits::resolve`] now turns the table into
/// [`SkillResourceLimits`], which is lowered into the sandbox profile and the
/// WASM store — a ceiling that terminates, not a comment.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillLimits {
    /// Agent-loop iteration ceiling. Carried through to
    /// [`SkillResourceLimits`] for the caller that drives the loop; this crate
    /// does not run the loop, so it does not enforce it here.
    pub maximum_iterations: Option<u32>,
    /// Wall-clock ceiling for one execution. **Enforced**: the sandbox kills the
    /// process group past this, and the WASM host refuses to refuel past it.
    pub maximum_duration_seconds: Option<u64>,
    /// Model-spend ceiling. Still **not** enforced here — budget accounting is
    /// not this crate's, and a skill's script/module spends no model tokens.
    /// Kept so the manifest shape is stable and the value reaches a future
    /// budget owner rather than being silently dropped.
    pub maximum_cost_usd: Option<f64>,
    /// CPU-time ceiling. **Enforced**: `prlimit --cpu` for a script, a fuel
    /// budget for a WASM guest.
    pub maximum_cpu_seconds: Option<u64>,
    /// Address-space ceiling in MiB. **Enforced**: `prlimit --as` for a script,
    /// the linear-memory cap for a WASM guest.
    pub maximum_memory_mb: Option<u64>,
    /// Captured-output ceiling in MiB. **Enforced**: output past it is truncated
    /// and flagged.
    pub maximum_output_mb: Option<u64>,
}

/// Conservative defaults for a skill that declares no explicit `[limits]`.
/// These are the values `skill_exec` used to hardcode; they are the *default*
/// now, not the only possibility.
pub const DEFAULT_SKILL_MEMORY_MB: u64 = 128;
pub const DEFAULT_SKILL_CPU_SECONDS: u64 = 30;
pub const DEFAULT_SKILL_WALL_SECONDS: u64 = 60;
pub const DEFAULT_SKILL_OUTPUT_MB: u64 = 8;

/// Hard ceilings no manifest may exceed. A package asking for more is **refused
/// at load**, not clamped: silently granting less than a manifest asks for
/// produces a skill that mysteriously fails halfway, whereas refusing tells its
/// author to fix the manifest.
pub const MAX_SKILL_MEMORY_MB: u64 = 4096;
pub const MAX_SKILL_CPU_SECONDS: u64 = 3600;
pub const MAX_SKILL_WALL_SECONDS: u64 = 3600;
pub const MAX_SKILL_OUTPUT_MB: u64 = 64;

/// The resolved, validated resource ceilings for one skill execution — the
/// shape the sandbox profile and the WASM store are built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkillResourceLimits {
    /// Address-space ceiling (MiB).
    pub memory_mb: u64,
    /// CPU-time ceiling (seconds).
    pub cpu_seconds: u64,
    /// Wall-clock ceiling (seconds).
    pub wall_seconds: u64,
    /// Captured-output ceiling (MiB).
    pub output_mb: u64,
    /// Agent-loop iteration ceiling, for the caller that drives the loop.
    pub maximum_iterations: Option<u32>,
}

impl Default for SkillResourceLimits {
    fn default() -> Self {
        Self {
            memory_mb: DEFAULT_SKILL_MEMORY_MB,
            cpu_seconds: DEFAULT_SKILL_CPU_SECONDS,
            wall_seconds: DEFAULT_SKILL_WALL_SECONDS,
            output_mb: DEFAULT_SKILL_OUTPUT_MB,
            maximum_iterations: None,
        }
    }
}

impl SkillLimits {
    /// Resolve the declared table into enforceable ceilings, refusing a value
    /// that is zero (which would read as "unlimited" downstream) or above the
    /// hard ceiling.
    pub fn resolve(&self) -> Result<SkillResourceLimits, ManifestError> {
        let defaults = SkillResourceLimits::default();
        let mut resolved = SkillResourceLimits {
            memory_mb: self.maximum_memory_mb.unwrap_or(defaults.memory_mb),
            cpu_seconds: self.maximum_cpu_seconds.unwrap_or(defaults.cpu_seconds),
            wall_seconds: self
                .maximum_duration_seconds
                .unwrap_or(defaults.wall_seconds),
            output_mb: self.maximum_output_mb.unwrap_or(defaults.output_mb),
            maximum_iterations: self.maximum_iterations,
        };
        for (field, value, ceiling) in [
            ("maximum_memory_mb", resolved.memory_mb, MAX_SKILL_MEMORY_MB),
            (
                "maximum_cpu_seconds",
                resolved.cpu_seconds,
                MAX_SKILL_CPU_SECONDS,
            ),
            (
                "maximum_duration_seconds",
                resolved.wall_seconds,
                MAX_SKILL_WALL_SECONDS,
            ),
            ("maximum_output_mb", resolved.output_mb, MAX_SKILL_OUTPUT_MB),
        ] {
            if value == 0 {
                return Err(ManifestError::InvalidLimit {
                    field,
                    value,
                    detail: "must be greater than zero (zero is not `unlimited`)".into(),
                });
            }
            if value > ceiling {
                return Err(ManifestError::InvalidLimit {
                    field,
                    value,
                    detail: format!("exceeds the {ceiling} ceiling"),
                });
            }
        }
        if matches!(resolved.maximum_iterations, Some(0)) {
            return Err(ManifestError::InvalidLimit {
                field: "maximum_iterations",
                value: 0,
                detail: "must be greater than zero".into(),
            });
        }
        // A CPU budget above the wall clock can never be reached, which makes
        // the manifest read as if it grants more than it does. Pull it down so
        // the two ceilings tell the same story.
        resolved.cpu_seconds = resolved.cpu_seconds.min(resolved.wall_seconds);
        Ok(resolved)
    }
}

/// The `[entrypoints]` table. Every declared path must exist on disk under the
/// package directory, or [`load_package`] rejects the package.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillEntrypoints {
    /// The instructions file (`SKILL.md`).
    pub instructions: Option<String>,
    /// The tests directory.
    pub tests: Option<String>,
    /// The references directory.
    pub references: Option<String>,
    /// The scripts directory. Its contents run confined through the OS sandbox
    /// (STEP 6.4).
    pub scripts: Option<String>,
    /// A WebAssembly module (STEP 6.3) run by
    /// [`WasmHost`](codypendent_sandbox::WasmHost). A guest gets no ambient
    /// capabilities: WASI is not linked, and every privileged act it attempts is
    /// re-checked against the run policy.
    pub module: Option<String>,
}

impl SkillEntrypoints {
    /// Every declared entrypoint path, in declaration order.
    fn declared(&self) -> impl Iterator<Item = &String> {
        [
            self.instructions.as_ref(),
            self.tests.as_ref(),
            self.references.as_ref(),
            self.scripts.as_ref(),
            self.module.as_ref(),
        ]
        .into_iter()
        .flatten()
    }

    /// Whether the package carries anything that can actually be executed.
    #[must_use]
    pub fn has_behaviour(&self) -> bool {
        self.scripts.is_some() || self.module.is_some()
    }
}

/// The `[trust]` table.
///
/// Everything in it is a **claim the package makes about itself**, recorded so
/// an operator can read it — never a decision input. See [`PACKAGE_TRUST_TIER`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillTrust {
    /// Publisher identity, as the package states it. Unverified: nothing signs
    /// it and nothing checks it against the operator's trusted-publisher store,
    /// so it is stored as text and read by no decision.
    pub publisher: String,
    /// Whether a signature is required before the item may run. Refused at load
    /// when `true`, because no skill path verifies a signature — see
    /// [`ManifestError::UnenforceableCapability`].
    #[serde(default)]
    pub signature_required: bool,
}

/// The trust tier every package loaded from disk gets, whatever its manifest
/// says.
///
/// Trust is a property of **how an item arrived**, not of what it says about
/// itself. `[trust] publisher` is package-authored — the 2026-08-13 review
/// installed a cloned package that wrote `publisher = "local-user"` and was
/// recorded `trust_tier = first_party`, which is the tier
/// [`crate::context`] renders as `first-party` on a disclosed card. The
/// prompt-injection labelling that exists to mark author-controlled text as
/// less authoritative was therefore bypassed by one line of TOML.
///
/// So: a package on disk is `Community`, full stop.
///
/// * [`TrustTier::FirstParty`] is reserved for items constructed in code
///   ([`crate::builtin`]) — there is no manifest for those, so nothing can
///   claim it.
/// * [`TrustTier::Verified`] awaits a verified signature. `crates/sandbox`
///   has the machinery ([`verify_artifact`] against the trusted-publisher
///   store) but no skill path calls it, so no package can reach that tier
///   today and pretending otherwise would be the same defect again.
///
/// The registration [`Scope`] is deliberately *not* used to soften this: the
/// scope a package is installed under is itself derived from the manifest's own
/// `scope` field (`crate::skills::install_package`), so keying trust on it
/// would hand the same self-promotion back through a different field.
///
/// [`verify_artifact`]: https://docs.rs/codypendent-sandbox
pub const PACKAGE_TRUST_TIER: TrustTier = TrustTier::Community;

/// A failure loading or validating a skill package.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// Reading `skill.toml` or walking the package directory failed.
    #[error("reading skill package: {0}")]
    Io(#[from] std::io::Error),
    /// `skill.toml` did not parse — a syntax error, a missing required key, or an
    /// **unknown key** (rejected by `deny_unknown_fields`).
    #[error("parsing skill.toml: {0}")]
    Toml(#[from] toml::de::Error),
    /// A declared entrypoint path does not exist under the package directory.
    #[error("declared entrypoint `{path}` does not exist under the package directory")]
    MissingEntrypoint { path: String },

    #[error("declared entrypoint `{path}` escapes the package directory")]
    EscapingEntrypoint { path: String },
    /// The manifest's `scope` string does not match the tier of the [`Scope`] the
    /// package is being registered under.
    #[error("manifest scope `{declared}` does not match the registration scope `{expected}`")]
    ScopeMismatch { declared: String, expected: String },
    /// The `version` string is not a plain `MAJOR.MINOR.PATCH`.
    #[error("invalid version `{0}` (expected MAJOR.MINOR.PATCH)")]
    InvalidVersion(String),
    /// The `status` string is not one of `draft | active | modified | deprecated`.
    #[error("unknown status `{0}` (expected draft|active|modified|deprecated)")]
    UnknownStatus(String),
    /// The package's total file bytes exceed [`MAX_PACKAGE_BYTES`] — refused so a
    /// community package with a huge asset cannot exhaust daemon memory or stall
    /// registration.
    #[error("skill package exceeds the {limit}-byte size ceiling (at least {seen} bytes)")]
    PackageTooLarge { seen: u64, limit: u64 },
    /// A `[limits]` value is zero, or above the hard ceiling. Refused at load so
    /// the package's author sees it, rather than the skill quietly running under
    /// a limit it did not ask for.
    #[error("`[limits] {field} = {value}` is not usable: {detail}")]
    InvalidLimit {
        /// The offending key.
        field: &'static str,
        /// The value as declared.
        value: u64,
        /// Why it was refused.
        detail: String,
    },
    /// The manifest declares something no executor can currently enforce.
    /// Refused at **load** rather than at the first run: the 2026-08-13 review
    /// found the one shipped skill package structurally unrunnable for exactly
    /// this reason, and discovering that at install time is the difference
    /// between a fixable error and a mystery.
    #[error("`[{table}] {capability}` cannot be enforced: {detail}")]
    UnenforceableCapability {
        /// The manifest table the key lives in (`permissions`, `trust`, …).
        /// Carried separately so a key outside `[permissions]` does not report
        /// itself as living there.
        table: &'static str,
        /// The key.
        capability: &'static str,
        /// Why no executor can honour it yet.
        detail: String,
    },
}

/// Load and validate the skill package at `dir`, folding it into a
/// [`RegistryItem`] registered under `scope`.
///
/// Validation, in order: the manifest parses (unknown keys rejected); every
/// declared entrypoint exists on disk under `dir`; the manifest `scope` string
/// equals `scope.tier()`; the `version` is well-formed; the `status` is known.
/// The item's `content_hash` is taken over **all** files in `dir` (recursively,
/// path-sorted for determinism), so any later file change without a version bump
/// is detectable. `[permissions]` is flattened into [`CapabilityRequest`]s and
/// the item's [`RiskClass`] derived from them. Skill behaviours are
/// [`executable`](RegistryItem::executable) (STEP 6.4): a skill's `scripts/` now
/// run confined through [`crate::skill_exec`], so the Phase-2 non-executable flag
/// no longer applies.
pub fn load_package(dir: &Path, scope: Scope) -> Result<RegistryItem, ManifestError> {
    let raw = std::fs::read_to_string(dir.join("skill.toml"))?;
    let manifest: SkillManifest = toml::from_str(&raw)?;

    // Every declared entrypoint must exist AND stay within the package. A `../`
    // or absolute entrypoint could otherwise validate — and later silently
    // change — a file outside `dir` that `hash_package` never hashes (so the
    // change would go undetected as `Modified`).
    let package_root = dir.canonicalize()?;
    for path in manifest.entrypoints.declared() {
        let Ok(resolved) = dir.join(path).canonicalize() else {
            return Err(ManifestError::MissingEntrypoint { path: path.clone() });
        };
        if !resolved.starts_with(&package_root) {
            return Err(ManifestError::EscapingEntrypoint { path: path.clone() });
        }
    }

    // The manifest's tier must match the scope it is being registered under.
    if manifest.scope != scope.tier() {
        return Err(ManifestError::ScopeMismatch {
            declared: manifest.scope.clone(),
            expected: scope.tier().to_string(),
        });
    }

    let version = Version(manifest.version.clone());
    if !version.is_valid() {
        return Err(ManifestError::InvalidVersion(manifest.version.clone()));
    }
    let status = parse_status(&manifest.status)?;

    // `[limits]` are resolved here, not at run time, so a package that asks for
    // an unusable ceiling is refused while its author is still looking at it.
    let _limits = manifest.limits.resolve()?;

    // A network allowlist has no enforcer: `validate_enforceable_profile` refuses
    // any non-empty `network_allowlist` on both platforms because a `host:port`
    // grant needs a broker that does not exist. Accepting the declaration and
    // failing at the first run is how the shipped `fix-ci` package came to be
    // installable but unrunnable — so it is refused here instead.
    if !manifest.permissions.network.is_empty() {
        return Err(ManifestError::UnenforceableCapability {
            table: "permissions",
            capability: "network",
            detail: "host:port grants require an outbound broker, which does not exist yet; \
                     no sandbox backend will admit a non-empty network allowlist"
                .into(),
        });
    }
    if !manifest.permissions.secrets.is_empty() {
        return Err(ManifestError::UnenforceableCapability {
            table: "permissions",
            capability: "secrets",
            detail: "brokered secrets require the secrets daemon, which does not exist yet; \
                     no executor reads `brokered_secrets`"
                .into(),
        });
    }
    // Same rule, applied to the one other declaration nothing honours. A
    // package asserting `signature_required = true` believes it will not run
    // unverified; the field was parsed, stored, and read by nothing, so it
    // would have run anyway. Refusing at load tells its author, exactly as for
    // `network`/`secrets`, rather than leaving a protection that is only a
    // string in a database.
    if manifest.trust.signature_required {
        return Err(ManifestError::UnenforceableCapability {
            table: "trust",
            capability: "signature_required",
            detail: "no skill path verifies a signature: `[trust] signature` is never populated \
                     and the trusted-publisher store is not consulted for skills, so requiring \
                     one would be recorded and not enforced"
                .into(),
        });
    }

    let content_hash = hash_package(dir)?;

    let permissions = flatten_permissions(&manifest.permissions);
    let risk = RiskClass::from_permissions(&permissions);

    // STEP 6.4: the Phase-2 "scripts recorded but not runnable" restriction is
    // lifted — the OS sandbox ([`crate::skill_exec`]) confines skill scripts and
    // the WASM host runs `[entrypoints] module`. But the flag is a statement
    // about THIS package, not about the platform: a skill that ships neither a
    // script nor a module has no behaviour to execute, and saying otherwise was
    // how every skill came to be marked executable regardless of content.
    let executable = manifest.entrypoints.has_behaviour();

    // Derived from how the package arrived (on disk, unsigned), never from what
    // it says about itself. `publisher` is carried through as an unverified
    // claim so an operator can see who the package says wrote it.
    let trust = TrustMetadata {
        publisher: manifest.trust.publisher.clone(),
        signature_required: manifest.trust.signature_required,
        signature: None,
        tier: PACKAGE_TRUST_TIER,
    };

    // Required tools are hard dependencies; optional tools are soft.
    let dependencies = manifest
        .required_tools
        .iter()
        .map(|target| RegistryDependency {
            target: target.clone(),
            optional: false,
        })
        .chain(
            manifest
                .optional_tools
                .iter()
                .map(|target| RegistryDependency {
                    target: target.clone(),
                    optional: true,
                }),
        )
        .collect();

    // Languages plus the human title are kept as lexical keywords (RegistryItem
    // has no separate display-title field; `name` carries the stable id).
    let mut keywords = manifest.languages.clone();
    keywords.push(manifest.name.clone());

    let now = Utc::now();
    Ok(RegistryItem {
        id: RegistryItemId::new(),
        kind: RegistryItemKind::Skill,
        name: manifest.id.clone(),
        version,
        scope,
        description: manifest.description.clone(),
        intents: manifest.intents.clone(),
        keywords,
        examples: Vec::new(),
        input_schema: None,
        output_schema: None,
        dependencies,
        permissions,
        risk,
        provenance: Provenance::Package {
            path: dir.display().to_string(),
        },
        trust,
        status,
        content_hash,
        executable,
        created_at: now,
        updated_at: now,
    })
}

/// Flatten a `[permissions]` table into the registry's capability list.
fn flatten_permissions(permissions: &SkillPermissions) -> Vec<CapabilityRequest> {
    let mut out = Vec::new();
    out.extend(
        permissions
            .filesystem_read
            .iter()
            .cloned()
            .map(CapabilityRequest::FilesystemRead),
    );
    out.extend(
        permissions
            .filesystem_write
            .iter()
            .cloned()
            .map(CapabilityRequest::FilesystemWrite),
    );
    out.extend(
        permissions
            .commands
            .iter()
            .cloned()
            .map(CapabilityRequest::Command),
    );
    out.extend(
        permissions
            .network
            .iter()
            .cloned()
            .map(CapabilityRequest::Network),
    );
    out.extend(
        permissions
            .secrets
            .iter()
            .cloned()
            .map(CapabilityRequest::Secret),
    );
    out
}

/// Map the manifest `status` string to a [`RegistryStatus`].
fn parse_status(status: &str) -> Result<RegistryStatus, ManifestError> {
    match status {
        "draft" => Ok(RegistryStatus::Draft),
        "active" => Ok(RegistryStatus::Active),
        "modified" => Ok(RegistryStatus::Modified),
        "deprecated" => Ok(RegistryStatus::Deprecated),
        other => Err(ManifestError::UnknownStatus(other.to_string())),
    }
}

/// Ceiling on a package's total file bytes. A skill package is instructions,
/// references, scripts, and small assets — far below this; anything larger is
/// refused rather than read (see [`ManifestError::PackageTooLarge`]).
const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;

/// Content-hash every file in the package directory.
///
/// Walks `dir` recursively, sorts files by their normalized relative path (so the
/// digest is independent of directory-read order and platform separators), and
/// folds each path and its bytes — length-prefixed so no path/content boundary is
/// ambiguous — into one SHA-256, hex-encoded. Any file added, removed, or edited
/// changes the digest. Files are streamed through the hasher one chunk at a time
/// — never all held in memory at once — and the total is capped at
/// [`MAX_PACKAGE_BYTES`].
pub fn hash_package(dir: &Path) -> Result<String, ManifestError> {
    let mut files = Vec::new();
    collect_files(dir, dir, &mut files)?;
    files.sort();

    let mut hasher = Sha256::new();
    let mut total: u64 = 0;
    let mut chunk = vec![0u8; 64 * 1024];
    for (relative, path) in files {
        let size = std::fs::metadata(&path)?.len();
        total = total.saturating_add(size);
        if total > MAX_PACKAGE_BYTES {
            return Err(ManifestError::PackageTooLarge {
                seen: total,
                limit: MAX_PACKAGE_BYTES,
            });
        }
        hasher.update((relative.len() as u64).to_le_bytes());
        hasher.update(relative.as_bytes());
        hasher.update(size.to_le_bytes());
        let mut file = std::fs::File::open(&path)?;
        loop {
            let n = std::io::Read::read(&mut file, &mut chunk)?;
            if n == 0 {
                break;
            }
            hasher.update(&chunk[..n]);
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Recursively gather `(normalized-relative-path, absolute-path)` for every
/// regular file under `dir`. Paths only — bytes are streamed at hash time.
fn collect_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, std::path::PathBuf)>,
) -> Result<(), ManifestError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(root, &path, out)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((relative, path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_protocol::RepositoryId;

    /// Write a minimal, valid skill package into `dir`, with `extra` spliced in
    /// before `[trust]` so a test can vary one table at a time.
    fn write_package(dir: &Path, extra: &str) {
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("scripts").join("run.sh"), "#!/bin/sh\n").unwrap();
        std::fs::write(dir.join("SKILL.md"), "# Demo\n").unwrap();
        let manifest = format!(
            "schema_version = 1\n\
             id = \"demo.skill\"\n\
             name = \"Demo\"\n\
             version = \"0.1.0\"\n\
             scope = \"repository\"\n\
             status = \"active\"\n\
             description = \"A demo skill.\"\n\
             \n{extra}\n\
             [entrypoints]\n\
             instructions = \"SKILL.md\"\n\
             scripts = \"scripts/\"\n\
             \n\
             [trust]\n\
             publisher = \"local-user\"\n\
             signature_required = false\n"
        );
        std::fs::write(dir.join("skill.toml"), manifest).unwrap();
    }

    fn load(extra: &str) -> Result<RegistryItem, ManifestError> {
        let dir = tempfile::tempdir().unwrap();
        write_package(dir.path(), extra);
        load_package(dir.path(), Scope::Repository(RepositoryId::new()))
    }

    #[test]
    fn absent_limits_resolve_to_the_conservative_defaults() {
        let resolved = SkillLimits::default().resolve().unwrap();
        assert_eq!(resolved.memory_mb, DEFAULT_SKILL_MEMORY_MB);
        assert_eq!(resolved.wall_seconds, DEFAULT_SKILL_WALL_SECONDS);
        assert_eq!(resolved.output_mb, DEFAULT_SKILL_OUTPUT_MB);
        assert_eq!(resolved.cpu_seconds, DEFAULT_SKILL_CPU_SECONDS);
    }

    #[test]
    fn a_declared_duration_becomes_the_enforced_wall_clock() {
        // The defect this closes: `maximum_duration_seconds = 1800` was parsed
        // and discarded, so the skill actually ran under a hardcoded 60s clock.
        let limits = SkillLimits {
            maximum_duration_seconds: Some(1800),
            ..Default::default()
        };
        let resolved = limits.resolve().unwrap();
        assert_eq!(resolved.wall_seconds, 1800);
        assert_ne!(
            resolved.wall_seconds, DEFAULT_SKILL_WALL_SECONDS,
            "the manifest value must win over the default"
        );
    }

    #[test]
    fn a_cpu_budget_above_the_wall_clock_is_pulled_down_to_it() {
        // A CPU cap that can never be reached makes the manifest read as if it
        // grants more than it does.
        let limits = SkillLimits {
            maximum_duration_seconds: Some(30),
            maximum_cpu_seconds: Some(3000),
            ..Default::default()
        };
        assert_eq!(limits.resolve().unwrap().cpu_seconds, 30);
    }

    #[test]
    fn a_zero_limit_is_refused_rather_than_read_as_unlimited() {
        for limits in [
            SkillLimits {
                maximum_duration_seconds: Some(0),
                ..Default::default()
            },
            SkillLimits {
                maximum_memory_mb: Some(0),
                ..Default::default()
            },
            SkillLimits {
                maximum_output_mb: Some(0),
                ..Default::default()
            },
            SkillLimits {
                maximum_iterations: Some(0),
                ..Default::default()
            },
        ] {
            assert!(matches!(
                limits.resolve().unwrap_err(),
                ManifestError::InvalidLimit { .. }
            ));
        }
    }

    #[test]
    fn a_limit_above_the_hard_ceiling_is_refused_not_clamped() {
        let limits = SkillLimits {
            maximum_memory_mb: Some(MAX_SKILL_MEMORY_MB + 1),
            ..Default::default()
        };
        let err = limits.resolve().unwrap_err();
        match err {
            ManifestError::InvalidLimit { field, .. } => assert_eq!(field, "maximum_memory_mb"),
            other => panic!("expected InvalidLimit, got {other}"),
        }
    }

    #[test]
    fn a_bad_limit_refuses_the_whole_package_at_load() {
        let err = load("[limits]\nmaximum_duration_seconds = 0\n").unwrap_err();
        assert!(matches!(err, ManifestError::InvalidLimit { .. }));
    }

    #[test]
    fn a_network_permission_is_refused_at_load_not_at_the_first_run() {
        // The shipped `fix-ci` package used to install cleanly and then fail on
        // every run, because no backend admits a non-empty network allowlist.
        let err = load("[permissions]\nnetwork = [\"api.github.com:443\"]\n").unwrap_err();
        match err {
            ManifestError::UnenforceableCapability { capability, .. } => {
                assert_eq!(capability, "network");
            }
            other => panic!("expected UnenforceableCapability, got {other}"),
        }
    }

    #[test]
    fn a_secrets_permission_is_refused_at_load() {
        let err = load("[permissions]\nsecrets = [\"github-token\"]\n").unwrap_err();
        assert!(matches!(
            err,
            ManifestError::UnenforceableCapability {
                capability: "secrets",
                ..
            }
        ));
    }

    #[test]
    fn a_package_cannot_promote_itself_to_first_party_trust() {
        // The 2026-08-13 review: a cloned package writing `publisher =
        // "local-user"` was recorded `trust_tier = first_party`, which is the
        // tier `context.rs` renders as `first-party` on a disclosed card — so
        // the prompt-injection labelling was bypassed by one line of TOML.
        // `publisher` is package-authored; trust comes from how the package
        // arrived, and every package arrives the same way: unsigned, on disk.
        for publisher in ["local-user", "codypendent", "anyone-at-all"] {
            let dir = tempfile::tempdir().unwrap();
            write_package(dir.path(), "[permissions]\n");
            let raw = std::fs::read_to_string(dir.path().join("skill.toml")).unwrap();
            std::fs::write(
                dir.path().join("skill.toml"),
                raw.replace(
                    "publisher = \"local-user\"",
                    &format!("publisher = \"{publisher}\""),
                ),
            )
            .unwrap();
            let item = load_package(dir.path(), Scope::Repository(RepositoryId::new())).unwrap();
            assert_eq!(
                item.trust.tier,
                TrustTier::Community,
                "`publisher = {publisher:?}` must not move the tier"
            );
            assert_ne!(item.trust.tier, TrustTier::FirstParty);
            // The claim is still recorded, so an operator can see it — it is
            // just not a decision input.
            assert_eq!(item.trust.publisher, publisher);
        }
    }

    #[test]
    fn a_user_scoped_package_is_no_more_trusted_than_a_repository_scoped_one() {
        // The scope a package installs under is derived from the manifest's own
        // `scope` field, so keying trust on it would hand the self-promotion
        // back through a different attacker-controlled key.
        let dir = tempfile::tempdir().unwrap();
        write_package(dir.path(), "[permissions]\n");
        let repository = load_package(dir.path(), Scope::Repository(RepositoryId::new())).unwrap();

        let user_dir = tempfile::tempdir().unwrap();
        write_package(user_dir.path(), "[permissions]\n");
        let raw = std::fs::read_to_string(user_dir.path().join("skill.toml")).unwrap();
        std::fs::write(
            user_dir.path().join("skill.toml"),
            raw.replace("scope = \"repository\"", "scope = \"user\""),
        )
        .unwrap();
        let user = load_package(
            user_dir.path(),
            Scope::User(codypendent_protocol::UserId("u".into())),
        )
        .unwrap();

        assert_eq!(repository.trust.tier, user.trust.tier);
        assert_eq!(user.trust.tier, TrustTier::Community);
    }

    #[test]
    fn requiring_a_signature_nobody_verifies_is_refused_at_load() {
        // Same rule as `network`/`secrets`: a declaration no executor honours
        // is refused while its author is looking at it, rather than recorded as
        // a protection that does not exist.
        let dir = tempfile::tempdir().unwrap();
        write_package(dir.path(), "[permissions]\n");
        let raw = std::fs::read_to_string(dir.path().join("skill.toml")).unwrap();
        std::fs::write(
            dir.path().join("skill.toml"),
            raw.replace("signature_required = false", "signature_required = true"),
        )
        .unwrap();
        let err = load_package(dir.path(), Scope::Repository(RepositoryId::new())).unwrap_err();
        assert!(
            matches!(
                err,
                ManifestError::UnenforceableCapability {
                    table: "trust",
                    capability: "signature_required",
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn executable_reflects_the_package_not_the_platform() {
        // With a `scripts/` entrypoint there is behaviour to run.
        let item = load("[permissions]\ncommands = [\"cargo\"]\n").unwrap();
        assert!(item.executable);

        // Without one there is not. Marking every skill executable regardless of
        // contents is what made the flag meaningless.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("SKILL.md"), "# Docs only\n").unwrap();
        std::fs::write(
            dir.path().join("skill.toml"),
            "schema_version = 1\n\
             id = \"demo.docs\"\n\
             name = \"Docs\"\n\
             version = \"0.1.0\"\n\
             scope = \"repository\"\n\
             status = \"active\"\n\
             description = \"Instructions only.\"\n\
             \n\
             [entrypoints]\n\
             instructions = \"SKILL.md\"\n\
             \n\
             [trust]\n\
             publisher = \"local-user\"\n\
             signature_required = false\n",
        )
        .unwrap();
        let item = load_package(dir.path(), Scope::Repository(RepositoryId::new())).unwrap();
        assert!(
            !item.executable,
            "a skill with no script and no module has no behaviour to execute"
        );
    }

    #[test]
    fn a_declared_module_entrypoint_must_exist_and_makes_the_skill_executable() {
        let dir = tempfile::tempdir().unwrap();
        write_package(dir.path(), "[permissions]\n");
        let raw = std::fs::read_to_string(dir.path().join("skill.toml")).unwrap();
        std::fs::write(
            dir.path().join("skill.toml"),
            raw.replace("scripts = \"scripts/\"", "module = \"skill.wasm\""),
        )
        .unwrap();
        // Declared but absent ⇒ refused, like every other entrypoint.
        assert!(matches!(
            load_package(dir.path(), Scope::Repository(RepositoryId::new())).unwrap_err(),
            ManifestError::MissingEntrypoint { .. }
        ));

        std::fs::write(dir.path().join("skill.wasm"), b"\0asm\x01\0\0\0").unwrap();
        let item = load_package(dir.path(), Scope::Repository(RepositoryId::new())).unwrap();
        assert!(item.executable);
    }
}
