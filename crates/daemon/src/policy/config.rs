//! Policy files, the layered merge, and the built-in defaults (STEP 1.5).
//!
//! Three layers stack, broadest authority first:
//!
//! 1. built-in [`MergedPolicy::builtin_defaults`] (the System baseline);
//! 2. `<config_dir>/codypendent/policy.toml` (the User layer);
//! 3. `<repo>/.codypendent/policy.toml` (the Repository layer, narrowest).
//!
//! Each file layer is a [`RawPolicy`] applied over the accumulating
//! [`MergedPolicy`] by either [`MergedPolicy::apply_untrusted_overlay`] (a
//! repo-local file — may only *restrict* a security scope, never widen it:
//! allowed roots and allow-lists intersect; deny lists union; approval
//! requirements ratchet toward the more restrictive value) or
//! [`MergedPolicy::apply_trusted_overlay`] (the user's own global config —
//! may *widen or narrow* the shell allow-list, network allow-list, and
//! `fs_read` scope, and may relax git/network approval dispositions;
//! `fs_write` stays narrow-only even here, since worktree confinement is the
//! floor no file may widen). Preferences that carry no security weight are
//! simply overridden.
//!
//! Every section that appears in `docs/specs/policy.toml` is modeled with
//! `#[serde(deny_unknown_fields)]`, so an unknown key is a load *error*, not a
//! silently-ignored warning (guide RULE 4).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::scope::NetworkDefault;

/// An approval disposition shared by the `[git]` and `[plugins]` sections.
/// Ordered from least to most restrictive by [`ApprovalAction::rank`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalAction {
    /// Permit without prompting.
    Allow,
    /// Permit once a human approves this occurrence.
    Approval,
    /// Always require a fresh approval; a prior approval never carries over.
    AlwaysApproval,
    /// Never permit.
    Deny,
}

impl ApprovalAction {
    fn rank(self) -> u8 {
        match self {
            ApprovalAction::Allow => 0,
            ApprovalAction::Approval => 1,
            ApprovalAction::AlwaysApproval => 2,
            ApprovalAction::Deny => 3,
        }
    }

    /// The stricter of two dispositions.
    fn more_restrictive(self, other: Self) -> Self {
        if self.rank() >= other.rank() {
            self
        } else {
            other
        }
    }
}

/// The effective, fully-resolved policy after the three layers are merged.
/// Path fields still hold unexpanded `$REPOSITORY`/`$WORKTREE`/`$HOME` strings;
/// they are expanded and canonicalized per evaluation (variables resolve at
/// evaluation time). Serialized deterministically to derive a stable
/// `PolicyVersion` hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MergedPolicy {
    pub schema_version: u32,
    pub fs_read: Vec<String>,
    pub fs_write: Vec<String>,
    pub fs_deny: Vec<String>,
    pub shell_allowed_programs: Vec<String>,
    pub shell_interpreter_requires_approval: bool,
    pub shell_maximum_seconds: u64,
    pub network_allow: Vec<String>,
    pub network_default: NetworkDefault,
    pub git_commit: ApprovalAction,
    pub git_push: ApprovalAction,
    pub git_force_push: ApprovalAction,
    pub git_delete_branch: ApprovalAction,
}

impl MergedPolicy {
    /// The built-in defaults used when no policy file exists (guide item 6):
    /// read = repository; write = worktree only; shell allow-list requiring
    /// approval; network denied; git commit/push require approval. The deny
    /// list guards `.git` and common secret stores so deny-precedence holds
    /// even with no file present.
    pub fn builtin_defaults() -> Self {
        Self {
            schema_version: 1,
            fs_read: vec!["$REPOSITORY".to_string()],
            fs_write: vec!["$WORKTREE".to_string()],
            fs_deny: vec![
                "$REPOSITORY/.git".to_string(),
                "$WORKTREE/.git".to_string(),
                "$HOME/.ssh".to_string(),
                "$HOME/.config".to_string(),
            ],
            shell_allowed_programs: vec![
                // Original Rust baseline.
                "cargo".to_string(),
                "git".to_string(),
                "rg".to_string(),
                "rustfmt".to_string(),
                // Curated, language-agnostic, read-only exploration set (agent &
                // tool fixes spec, FIX 2a): without these, basic exploration of a
                // non-Rust repository denied `ls`/`find` outright and the agent
                // fell back to repeated file reads. `find` is deliberately
                // excluded (`-delete`/`-exec`/`-execdir`/`-ok` can remove files or
                // run arbitrary programs, bypassing this allow-list), as is every
                // interpreter/code-execution multiplexer. Allow-listing a program
                // here does not auto-run it — every allow-listed command still
                // requires approval (see `eval_command`).
                "ls".to_string(),
                "cat".to_string(),
                "head".to_string(),
                "tail".to_string(),
                "wc".to_string(),
                "grep".to_string(),
                "tree".to_string(),
                "pwd".to_string(),
                "file".to_string(),
                "which".to_string(),
                "sort".to_string(),
                "uniq".to_string(),
            ],
            shell_interpreter_requires_approval: true,
            shell_maximum_seconds: 900,
            network_allow: Vec::new(),
            network_default: NetworkDefault::Deny,
            git_commit: ApprovalAction::Approval,
            git_push: ApprovalAction::Approval,
            git_force_push: ApprovalAction::Deny,
            git_delete_branch: ApprovalAction::AlwaysApproval,
        }
    }

    /// Apply a narrower file layer over this policy, enforcing the merge
    /// invariant. Only fields the overlay sets are touched; each is narrowed,
    /// never widened. Used for an **untrusted** source (a repo-local
    /// `.codypendent/policy.toml`): the file may only claw back authority the
    /// broader layers already granted, never add a program, root, endpoint,
    /// or relax an approval disposition.
    pub fn apply_untrusted_overlay(&mut self, raw: &RawPolicy) {
        if let Some(schema_version) = raw.schema_version {
            self.schema_version = schema_version;
        }
        if let Some(fs) = &raw.filesystem {
            if let Some(read) = &fs.read {
                self.fs_read = intersect_roots(&self.fs_read, read);
            }
            if let Some(write) = &fs.write {
                self.fs_write = intersect_roots(&self.fs_write, write);
            }
            if let Some(deny) = &fs.deny {
                // Deny accumulates: a narrower layer can add denials but never
                // remove one a broader layer imposed.
                union_in_place(&mut self.fs_deny, deny);
            }
        }
        if let Some(shell) = &raw.shell {
            if let Some(programs) = &shell.allowed_programs {
                self.shell_allowed_programs =
                    intersect_exact(&self.shell_allowed_programs, programs);
            }
            if let Some(requires) = shell.shell_interpreter_requires_approval {
                // Requiring approval is stricter; once required it stays required.
                self.shell_interpreter_requires_approval |= requires;
            }
            if let Some(seconds) = shell.maximum_seconds {
                self.shell_maximum_seconds = self.shell_maximum_seconds.min(seconds);
            }
        }
        if let Some(network) = &raw.network {
            if let Some(allow) = &network.allow {
                self.network_allow = intersect_exact(&self.network_allow, allow);
            }
            if let Some(default) = network.default {
                // Deny is stricter than Allow.
                if matches!(default, NetworkDefault::Deny) {
                    self.network_default = NetworkDefault::Deny;
                }
            }
        }
        if let Some(git) = &raw.git {
            if let Some(commit) = git.commit {
                self.git_commit = self.git_commit.more_restrictive(commit);
            }
            if let Some(push) = git.push {
                self.git_push = self.git_push.more_restrictive(push);
            }
            if let Some(force_push) = git.force_push {
                self.git_force_push = self.git_force_push.more_restrictive(force_push);
            }
            if let Some(delete_branch) = git.delete_branch {
                self.git_delete_branch = self.git_delete_branch.more_restrictive(delete_branch);
            }
        }
        // `scope`, `data`, `plugins`, and `memory` are parsed (and validated for
        // unknown keys) but not enforced in Phase 1.
    }

    /// Apply a **trusted** file layer over this policy — the user's own
    /// global config, never a repo-local file. Unlike
    /// [`Self::apply_untrusted_overlay`], this path may *widen* the shell
    /// allow-list, network allow-list, and `fs_read` scope, and may *relax*
    /// git/network approval dispositions (the overlay value replaces the
    /// accumulated one in either direction). `fs_write` is the one exception:
    /// it stays **narrow-only** even from a trusted source, because worktree
    /// confinement (`fs_write = $WORKTREE`) is the floor that guarantees a
    /// run cannot write outside its isolated worktree — no config file, not
    /// even the user's own, may widen it (Decision 2a).
    ///
    /// Widening the shell/network allow-lists only changes which programs or
    /// endpoints are *considered* — it never bypasses the approval gate.
    /// Every allow-listed program still requires a human approval at
    /// evaluation time (`eval_command`); widening can move a program from
    /// `Deny` to `RequireApproval`, never to auto-run.
    ///
    /// Not yet called by [`super::PolicyEngine::load`] — routing the trusted
    /// global config through this path (vs. the untrusted path) is a
    /// separate follow-up task; this method is built and unit-tested ahead
    /// of that wiring.
    #[allow(dead_code)]
    pub fn apply_trusted_overlay(&mut self, raw: &RawPolicy) {
        if let Some(schema_version) = raw.schema_version {
            self.schema_version = schema_version;
        }
        if let Some(fs) = &raw.filesystem {
            if let Some(read) = &fs.read {
                // Widen: a trusted global may extend read scope.
                self.fs_read = union_roots(&self.fs_read, read);
            }
            if let Some(write) = &fs.write {
                // Narrow-only floor (Decision 2a): even the trusted global
                // config cannot widen where writes may land.
                self.fs_write = intersect_roots(&self.fs_write, write);
            }
            if let Some(deny) = &fs.deny {
                // Deny accumulates regardless of trust: adding a denial only
                // tightens, so it is always safe to union.
                union_in_place(&mut self.fs_deny, deny);
            }
        }
        if let Some(shell) = &raw.shell {
            if let Some(programs) = &shell.allowed_programs {
                // Widen: the trusted global may add programs (e.g. `pytest`)
                // while keeping the built-in/broader set (union, not
                // replace). Adding a program here only makes it eligible for
                // `RequireApproval`, never for auto-run.
                union_in_place(&mut self.shell_allowed_programs, programs);
            }
            if let Some(requires) = shell.shell_interpreter_requires_approval {
                // Either direction: a trusted global may relax or tighten.
                self.shell_interpreter_requires_approval = requires;
            }
            if let Some(seconds) = shell.maximum_seconds {
                // Either direction: a trusted global may relax or tighten.
                self.shell_maximum_seconds = seconds;
            }
        }
        if let Some(network) = &raw.network {
            if let Some(allow) = &network.allow {
                // Widen: trusted global may add network endpoints.
                union_in_place(&mut self.network_allow, allow);
            }
            if let Some(default) = network.default {
                // Either direction: a trusted global may relax network
                // default to `Allow`.
                self.network_default = default;
            }
        }
        if let Some(git) = &raw.git {
            if let Some(commit) = git.commit {
                // Either direction: a trusted global may relax approval.
                self.git_commit = commit;
            }
            if let Some(push) = git.push {
                self.git_push = push;
            }
            if let Some(force_push) = git.force_push {
                self.git_force_push = force_push;
            }
            if let Some(delete_branch) = git.delete_branch {
                self.git_delete_branch = delete_branch;
            }
        }
        // `scope`, `data`, `plugins`, and `memory` are parsed (and validated for
        // unknown keys) but not enforced in Phase 1.
    }
}

/// Region intersection of two allowed-root lists. A root from `overlay` is kept
/// only where it lies within a `base` root (the narrower region wins); where a
/// `base` root lies within an `overlay` root, the `base` root is kept (still no
/// widening). Disjoint pairs contribute nothing. The result is therefore always
/// a subset of the region `base` already permitted — a narrower layer can never
/// add a root the broader layer did not allow.
fn intersect_roots(base: &[String], overlay: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for narrow in overlay {
        for broad in base {
            let kept = if raw_within(narrow, broad) {
                Some(narrow)
            } else if raw_within(broad, narrow) {
                Some(broad)
            } else {
                None
            };
            if let Some(root) = kept {
                if !out.iter().any(|existing| existing == root) {
                    out.push(root.clone());
                }
            }
        }
    }
    out
}

/// Set intersection preserving `base` order (used for exact-match lists such as
/// shell programs and network destinations).
fn intersect_exact(base: &[String], overlay: &[String]) -> Vec<String> {
    base.iter()
        .filter(|item| overlay.iter().any(|o| o == *item))
        .cloned()
        .collect()
}

/// Region union of two allowed-root lists — the widening dual of
/// [`intersect_roots`]. Every `base` root is kept; an `overlay` root is
/// appended unless it already lies within a root already in the result (so a
/// redundant sub-root contributes nothing new). Used only by the **trusted**
/// widen path (`apply_trusted_overlay`) for `fs_read`; `fs_write` never calls
/// this — it stays on `intersect_roots` even for a trusted source.
#[allow(dead_code)] // called by apply_trusted_overlay, not yet wired into `load`
fn union_roots(base: &[String], overlay: &[String]) -> Vec<String> {
    let mut out: Vec<String> = base.to_vec();
    for candidate in overlay {
        let already_covered = out.iter().any(|existing| raw_within(candidate, existing));
        if !already_covered {
            out.push(candidate.clone());
        }
    }
    out
}

/// Append entries of `extra` to `base` that are not already present.
fn union_in_place(base: &mut Vec<String>, extra: &[String]) {
    for item in extra {
        if !base.iter().any(|existing| existing == item) {
            base.push(item.clone());
        }
    }
}

/// Component-wise containment on unexpanded root strings: `inner` is `outer` or
/// lies under it. `$REPOSITORY`, `$WORKTREE`, and `$HOME` are treated as
/// ordinary leading components, so `$WORKTREE/src` is within `$WORKTREE` while
/// `/tmp/x` is not.
fn raw_within(inner: &str, outer: &str) -> bool {
    let inner = Path::new(inner);
    let outer = Path::new(outer);
    inner == outer || inner.starts_with(outer)
}

/// One policy file, as parsed. Every section is optional; a layer that omits a
/// section leaves the accumulated value untouched. `deny_unknown_fields` makes a
/// stray key a hard error at every level.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawPolicy {
    #[serde(default)]
    pub schema_version: Option<u32>,
    // `scope`, `data`, `plugins`, and `memory` are modeled so the spec file
    // parses and so unknown keys inside them are rejected, but they carry no
    // Phase 1 enforcement — hence parsed-but-unread.
    #[serde(default)]
    #[allow(dead_code)]
    pub scope: Option<RawScope>,
    #[serde(default)]
    #[allow(dead_code)]
    pub data: Option<RawData>,
    #[serde(default)]
    pub filesystem: Option<RawFilesystem>,
    #[serde(default)]
    pub shell: Option<RawShell>,
    #[serde(default)]
    pub network: Option<RawNetwork>,
    #[serde(default)]
    pub git: Option<RawGit>,
    #[serde(default)]
    #[allow(dead_code)]
    pub plugins: Option<RawPlugins>,
    #[serde(default)]
    #[allow(dead_code)]
    pub memory: Option<RawMemory>,
}

impl RawPolicy {
    /// Parse a policy file's contents. Unknown keys fail here.
    pub fn parse(contents: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(contents)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // parsed for validation; not enforced in Phase 1
pub struct RawScope {
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // parsed for validation; not enforced in Phase 1
pub struct RawData {
    #[serde(default)]
    pub classification: Option<String>,
    #[serde(default)]
    pub remote_models_allowed: Option<Vec<String>>,
    #[serde(default)]
    pub local_models_allowed: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawFilesystem {
    #[serde(default)]
    pub read: Option<Vec<String>>,
    #[serde(default)]
    pub write: Option<Vec<String>>,
    #[serde(default)]
    pub deny: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawShell {
    #[serde(default)]
    pub allowed_programs: Option<Vec<String>>,
    #[serde(default)]
    pub shell_interpreter_requires_approval: Option<bool>,
    #[serde(default)]
    pub maximum_seconds: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawNetwork {
    #[serde(default)]
    pub allow: Option<Vec<String>>,
    #[serde(default)]
    pub default: Option<NetworkDefault>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawGit {
    #[serde(default)]
    pub commit: Option<ApprovalAction>,
    #[serde(default)]
    pub push: Option<ApprovalAction>,
    #[serde(default)]
    pub force_push: Option<ApprovalAction>,
    #[serde(default)]
    pub delete_branch: Option<ApprovalAction>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // parsed for validation; not enforced in Phase 1
pub struct RawPlugins {
    #[serde(default)]
    pub unsigned: Option<ApprovalAction>,
    #[serde(default)]
    pub native_process: Option<ApprovalAction>,
    #[serde(default)]
    pub permission_expansion: Option<ApprovalAction>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // parsed for validation; not enforced in Phase 1
pub struct RawMemory {
    #[serde(default)]
    pub cross_repository: Option<bool>,
    #[serde(default)]
    pub retain_days: Option<i64>,
    #[serde(default)]
    pub secrets: Option<String>,
}

/// A failure loading or parsing a policy file.
#[derive(Debug, thiserror::Error)]
pub enum PolicyLoadError {
    /// The file existed but could not be read.
    #[error("failed to read policy file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The file's contents were not valid policy TOML (includes unknown keys).
    #[error("failed to parse policy file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

/// Read and parse a policy file. `Ok(None)` if the path does not exist (so a
/// caller may pass conventional locations without pre-checking); a parse error
/// — including an unknown key — is returned as [`PolicyLoadError::Parse`].
pub fn load_layer(path: &Path) -> Result<Option<RawPolicy>, PolicyLoadError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(PolicyLoadError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let raw = RawPolicy::parse(&contents).map_err(|source| PolicyLoadError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersect_roots_never_widens() {
        // A narrower layer that lists a root outside the broader region does
        // not gain it; only the in-region roots survive.
        let base = vec!["$WORKTREE".to_string()];
        let overlay = vec!["$WORKTREE/src".to_string(), "/tmp/evil".to_string()];
        let merged = intersect_roots(&base, &overlay);
        assert_eq!(merged, vec!["$WORKTREE/src".to_string()]);
        assert!(!merged.iter().any(|r| r == "/tmp/evil"));
    }

    #[test]
    fn intersect_roots_keeps_broader_when_overlay_is_wider() {
        let base = vec!["$REPOSITORY".to_string()];
        let overlay = vec!["$REPOSITORY".to_string(), "/etc".to_string()];
        let merged = intersect_roots(&base, &overlay);
        assert_eq!(merged, vec!["$REPOSITORY".to_string()]);
    }

    /// Property: for any broader/narrower pair drawn from a small alphabet, every
    /// root in the merged result lies within some broader root — the effective
    /// scope is always a subset of the broader region, never a union.
    #[test]
    fn intersect_roots_result_is_always_within_base() {
        let alphabet = [
            "$WORKTREE",
            "$WORKTREE/src",
            "$WORKTREE/src/lib",
            "$REPOSITORY",
            "/tmp/evil",
            "/etc",
        ];
        // Exhaustively enumerate base and overlay as 2-element selections.
        for &b0 in &alphabet {
            for &b1 in &alphabet {
                let base = vec![b0.to_string(), b1.to_string()];
                for &o0 in &alphabet {
                    for &o1 in &alphabet {
                        let overlay = vec![o0.to_string(), o1.to_string()];
                        let merged = intersect_roots(&base, &overlay);
                        for root in &merged {
                            assert!(
                                base.iter().any(|b| raw_within(root, b)),
                                "merged root {root} escaped base {base:?} (overlay {overlay:?})"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn deny_accumulates_and_dedups() {
        let mut merged = MergedPolicy::builtin_defaults();
        let raw =
            RawPolicy::parse("[filesystem]\ndeny = [\"$WORKTREE/secrets\", \"$WORKTREE/.git\"]")
                .unwrap();
        let before = merged.fs_deny.len();
        merged.apply_untrusted_overlay(&raw);
        // New entry added once; the already-present `$WORKTREE/.git` not duped.
        assert!(merged.fs_deny.iter().any(|d| d == "$WORKTREE/secrets"));
        assert_eq!(
            merged
                .fs_deny
                .iter()
                .filter(|d| *d == "$WORKTREE/.git")
                .count(),
            1
        );
        assert_eq!(merged.fs_deny.len(), before + 1);
    }

    #[test]
    fn shell_allow_list_only_narrows() {
        let mut merged = MergedPolicy::builtin_defaults();
        // Overlay tries to add `npm` and keep `cargo`.
        let raw = RawPolicy::parse("[shell]\nallowed_programs = [\"cargo\", \"npm\"]").unwrap();
        merged.apply_untrusted_overlay(&raw);
        assert_eq!(merged.shell_allowed_programs, vec!["cargo".to_string()]);
    }

    /// FIX 2 (agent & tool fixes spec, §2a): the built-in default was Rust-only
    /// (`cargo`/`git`/`rg`/`rustfmt`), so basic exploration in a non-Rust repo
    /// (evidence: a Python repo review) denied `ls`/`find` outright. The default
    /// now also carries a curated, language-agnostic, read-only exploration set —
    /// while keeping the original four and excluding `find` (`-delete`/`-exec`
    /// can write/exec, bypassing the allow-list) and every interpreter/multiplexer.
    #[test]
    fn builtin_default_shell_allow_list_includes_curated_read_only_set() {
        let defaults = MergedPolicy::builtin_defaults();
        let expected = [
            // Original Rust baseline — unchanged.
            "cargo", "git", "rg", "rustfmt",
            // Curated read-only exploration set (FIX 2a).
            "ls", "cat", "head", "tail", "wc", "grep", "tree", "pwd", "file", "which", "sort",
            "uniq",
        ];
        for program in expected {
            assert!(
                defaults.shell_allowed_programs.iter().any(|p| p == program),
                "expected `{program}` in the default shell allow-list, got {:?}",
                defaults.shell_allowed_programs
            );
        }
        assert_eq!(
            defaults.shell_allowed_programs.len(),
            expected.len(),
            "no extra programs beyond the curated set: {:?}",
            defaults.shell_allowed_programs
        );
        // `find` is deliberately excluded: `-delete`/`-exec`/`-execdir`/`-ok` can
        // remove files or run arbitrary programs, bypassing the allow-list.
        assert!(!defaults.shell_allowed_programs.iter().any(|p| p == "find"));
    }

    #[test]
    fn git_and_network_ratchet_toward_restriction() {
        let mut merged = MergedPolicy::builtin_defaults();
        // Overlay tries to relax commit to `allow` — must not weaken approval.
        let raw =
            RawPolicy::parse("[git]\ncommit = \"allow\"\n[network]\ndefault = \"allow\"").unwrap();
        merged.apply_untrusted_overlay(&raw);
        assert_eq!(merged.git_commit, ApprovalAction::Approval);
        assert_eq!(merged.network_default, NetworkDefault::Deny);
    }

    /// PF1: a **trusted** global config may WIDEN the shell allow-list — the
    /// concrete `pytest` need. The merged set must contain both `pytest` and
    /// the built-in baseline (union, not replace).
    #[test]
    fn trusted_overlay_widens_shell_allow_list() {
        let mut merged = MergedPolicy::builtin_defaults();
        let raw = RawPolicy::parse("[shell]\nallowed_programs = [\"pytest\"]").unwrap();
        merged.apply_trusted_overlay(&raw);
        assert!(merged.shell_allowed_programs.iter().any(|p| p == "pytest"));
        // Built-ins survive — this is a union, not a replace.
        for builtin in ["cargo", "git", "rg", "rustfmt"] {
            assert!(
                merged.shell_allowed_programs.iter().any(|p| p == builtin),
                "expected `{builtin}` to survive the trusted widen, got {:?}",
                merged.shell_allowed_programs
            );
        }
    }

    /// PF1 regression guard: an **untrusted** repo-local file must still only
    /// narrow, even post-split. A repo cannot add `pytest` to the allow-list.
    #[test]
    fn untrusted_overlay_still_only_narrows_shell_allow_list() {
        let mut merged = MergedPolicy::builtin_defaults();
        let raw = RawPolicy::parse("[shell]\nallowed_programs = [\"cargo\", \"pytest\"]").unwrap();
        merged.apply_untrusted_overlay(&raw);
        assert_eq!(merged.shell_allowed_programs, vec!["cargo".to_string()]);
        assert!(!merged.shell_allowed_programs.iter().any(|p| p == "pytest"));
    }

    /// PF1: a trusted global config may WIDEN `fs_read` (e.g. to read a
    /// sibling vendored directory outside the repository root).
    #[test]
    fn trusted_overlay_widens_fs_read() {
        let mut merged = MergedPolicy::builtin_defaults();
        assert_eq!(merged.fs_read, vec!["$REPOSITORY".to_string()]);
        let raw = RawPolicy::parse("[filesystem]\nread = [\"/opt/vendored\"]").unwrap();
        merged.apply_trusted_overlay(&raw);
        assert!(merged.fs_read.iter().any(|r| r == "$REPOSITORY"));
        assert!(merged.fs_read.iter().any(|r| r == "/opt/vendored"));
    }

    /// PF1 SECURITY FLOOR (Decision 2a): `fs_write` never widens, even from
    /// the trusted global config. A trusted overlay naming `$HOME` must not
    /// add it to the write scope — worktree confinement is the one invariant
    /// no file, trusted or not, may relax.
    #[test]
    fn trusted_overlay_does_not_widen_fs_write() {
        let mut merged = MergedPolicy::builtin_defaults();
        assert_eq!(merged.fs_write, vec!["$WORKTREE".to_string()]);
        let raw = RawPolicy::parse("[filesystem]\nwrite = [\"$HOME\"]").unwrap();
        merged.apply_trusted_overlay(&raw);
        // `$HOME` is disjoint from `$WORKTREE`, so the narrow-only intersect
        // drops it entirely rather than widening the write scope.
        assert!(!merged.fs_write.iter().any(|r| r == "$HOME"));
        assert!(merged.fs_write.is_empty() || merged.fs_write == vec!["$WORKTREE".to_string()]);
    }

    /// PF1: a trusted global config may WIDEN the network allow-list and
    /// relax `network.default` and `git.commit` — the opposite of the
    /// untrusted ratchet proven by `git_and_network_ratchet_toward_restriction`.
    #[test]
    fn trusted_overlay_widens_network_and_relaxes_git() {
        let mut merged = MergedPolicy::builtin_defaults();
        let raw = RawPolicy::parse(
            "[network]\nallow = [\"example.com\"]\ndefault = \"allow\"\n[git]\ncommit = \"allow\"",
        )
        .unwrap();
        merged.apply_trusted_overlay(&raw);
        assert!(merged.network_allow.iter().any(|n| n == "example.com"));
        assert_eq!(merged.network_default, NetworkDefault::Allow);
        assert_eq!(merged.git_commit, ApprovalAction::Allow);
    }

    #[test]
    fn spec_file_parses_with_every_section() {
        let spec = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/specs/policy.toml");
        let contents = std::fs::read_to_string(spec).expect("read spec policy");
        let raw = RawPolicy::parse(&contents).expect("spec policy must parse");
        assert!(raw.filesystem.is_some());
        assert!(raw.shell.is_some());
        assert!(raw.git.is_some());
        assert!(raw.plugins.is_some());
        assert!(raw.memory.is_some());
    }

    #[test]
    fn unknown_key_is_a_parse_error() {
        assert!(RawPolicy::parse("bogus_top_level = 1").is_err());
        assert!(RawPolicy::parse("[filesystem]\nbogus = 1").is_err());
    }
}
