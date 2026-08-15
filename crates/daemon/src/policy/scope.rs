//! Capabilities and the scopes they carry (STEP 1.5).
//!
//! A [`Capability`] is the unit a run is granted before a tool executes. The
//! Phase 1 subset covers filesystem reads/writes, command execution, network
//! connections, and Git commit/push. Each scope is *checked after
//! canonicalization*: a path is resolved (its `..` segments and symlinks
//! collapsed) before it is compared against an allowed root, so neither
//! traversal nor a planted symlink can smuggle a path out of scope. Deny always
//! wins over allow, even inside an allowed root.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The default fallback for a network destination that is not on the allow
/// list. `deny` is the Phase 1 built-in default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkDefault {
    /// Permit destinations not otherwise listed.
    Allow,
    /// Reject destinations not otherwise listed.
    Deny,
}

/// A time-limited, invocation-scoped capability. The Phase 1 subset of the
/// [Chapter 11](../../docs/docs/11-security-and-governance.md) capability model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Capability {
    /// Read files within a [`PathScope`].
    FileRead(PathScope),
    /// Write files within a [`PathScope`].
    FileWrite(PathScope),
    /// Execute an allow-listed program within a [`CommandScope`].
    CommandExecute(CommandScope),
    /// Open a network connection permitted by a [`NetworkScope`].
    NetworkConnect(NetworkScope),
    /// Create a Git commit in the run's repository.
    GitCommit,
    /// Push to a Git remote.
    GitPush,
    /// Call a tool on an operator-declared MCP server (PR B — MCP client). A
    /// marker: the MCP bridge executes the call itself, so the grant carries no
    /// path/command/network scope — it exists so the approval and audit record
    /// names the server the call went to.
    McpToolCall {
        /// The server name from the trusted `mcp.toml`.
        server: String,
    },
    /// Create or run a persisted agent council. Marker capability: execution
    /// stays inside Codypendent but may change config and incur model cost.
    CouncilManage,
    /// Create or run a durable workflow. Marker capability: the compiler and
    /// workflow host enforce the concrete manifest/repository boundaries while
    /// this grant records the fresh human approval.
    WorkflowManage,
    /// Restore a run's worktree to a recorded filesystem checkpoint (Adoption 04).
    /// Marker capability: always requires explicit single-use approval.
    RestoreCheckpoint,
}

/// The verdict of checking a single path against a [`PathScope`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeVerdict {
    /// Inside an allowed root and not denied.
    Allowed,
    /// Not under any allowed root.
    OutsideRoots,
    /// Matched the deny list (deny wins even inside an allowed root).
    Denied,
}

/// A set of canonical allowed root directories plus a canonical deny list.
///
/// The `roots` and `deny` paths are already canonicalized (built at evaluation
/// time from the merged policy with `$REPOSITORY`/`$WORKTREE`/`$HOME`
/// expanded). A candidate path is canonicalized on the fly in [`classify`]
/// before comparison.
///
/// [`classify`]: PathScope::classify
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathScope {
    /// Canonical directories a path must fall under to be in scope.
    pub roots: Vec<PathBuf>,
    /// Canonical directories that are denied even inside an allowed root.
    pub deny: Vec<PathBuf>,
}

impl PathScope {
    /// Build a scope from already-canonical roots and deny entries.
    pub fn new(roots: Vec<PathBuf>, deny: Vec<PathBuf>) -> Self {
        Self { roots, deny }
    }

    /// Canonicalize `path` and classify it against this scope. Deny wins: a
    /// path under a deny entry is [`ScopeVerdict::Denied`] even when it is also
    /// under an allowed root.
    pub fn classify(&self, path: &Path) -> ScopeVerdict {
        self.classify_canonical(&canonicalize_lenient(path))
    }

    /// Resolve `path` to its lenient-canonical absolute form and classify that
    /// **same** resolved path against this scope, returning both together.
    ///
    /// This is the no-TOCTOU seam for callers that must later act (e.g. write)
    /// on exactly the path that was checked: [`classify`] alone recomputes the
    /// canonical path internally and hands the caller nothing, so a caller that
    /// re-derives the path itself (even by calling `classify` and then
    /// re-canonicalizing) opens a check/act gap. `resolve` closes that gap by
    /// canonicalizing once and returning the resolved [`PathBuf`] alongside its
    /// [`ScopeVerdict`] — the caller acts on the returned path, never a
    /// re-derived one.
    ///
    /// A non-existent leaf (e.g. a new file to be created) resolves via
    /// [`canonicalize_lenient`]: the existing ancestor prefix is fully
    /// canonicalized (collapsing `..` and symlinks), and the not-yet-created
    /// remainder is appended — so a traversal or symlinked parent cannot escape
    /// containment even for a path that does not yet exist.
    ///
    /// Note: this does not by itself guard against a symlink planted *at the
    /// leaf* between this call and a later filesystem write — callers that
    /// write to the returned path must additionally check
    /// `symlink_metadata` on that same path immediately before writing.
    ///
    /// [`classify`]: PathScope::classify
    pub fn resolve(&self, path: &Path) -> (PathBuf, ScopeVerdict) {
        let canonical = canonicalize_lenient(path);
        let verdict = self.classify_canonical(&canonical);
        (canonical, verdict)
    }

    /// The shared classification core: deny-wins, then root-containment, both
    /// via component-wise [`is_within`]. Takes an already-canonicalized path so
    /// [`classify`] and [`resolve`] never canonicalize twice.
    ///
    /// [`classify`]: PathScope::classify
    /// [`resolve`]: PathScope::resolve
    fn classify_canonical(&self, canonical: &Path) -> ScopeVerdict {
        if self.deny.iter().any(|d| is_within(canonical, d)) {
            return ScopeVerdict::Denied;
        }
        if self.roots.iter().any(|r| is_within(canonical, r)) {
            ScopeVerdict::Allowed
        } else {
            ScopeVerdict::OutsideRoots
        }
    }

    /// Whether `path` is allowed by this scope (convenience over [`classify`]).
    ///
    /// [`classify`]: PathScope::classify
    pub fn allows(&self, path: &Path) -> bool {
        matches!(self.classify(path), ScopeVerdict::Allowed)
    }
}

/// The programs a run may execute and the wall-clock ceiling for each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandScope {
    /// Executables permitted to run. A bare command name (no path separator,
    /// e.g. `cargo`) is resolved from the daemon's trusted `PATH` by the shell
    /// tool; a path-bearing entry (e.g. `/usr/bin/cargo`) pins that exact
    /// binary. Matching is by exact string equality — never by basename.
    pub allowed_programs: Vec<String>,
    /// Maximum wall-clock seconds a single command may run.
    pub maximum_seconds: u64,
}

impl CommandScope {
    /// Whether `program` is allow-listed.
    ///
    /// Matching is by **exact string equality**, deliberately NOT by basename.
    /// A bare command name (no path separator, e.g. `cargo`) matches a bare
    /// allow-list entry and is then resolved from the daemon's trusted `PATH` by
    /// the shell tool; a path-bearing program (e.g. `./cargo`, `/tmp/cargo`,
    /// `/usr/bin/cargo`) is allowed only if that exact string is configured.
    ///
    /// Matching a path by its final component would let `./cargo`, `/tmp/cargo`,
    /// or a model-planted `cargo` in the worktree impersonate an allow-listed
    /// `cargo` and execute arbitrary code — the shell tool checks this on the
    /// model-supplied program string before it spawns anything.
    pub fn allows_program(&self, program: &str) -> bool {
        self.allowed_programs.iter().any(|entry| entry == program)
    }
}

/// The network destinations a run may reach and the fallback for the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkScope {
    /// Explicitly permitted `host:port` destinations.
    pub allow: Vec<String>,
    /// What to do with a destination not on `allow`.
    pub default: NetworkDefault,
}

impl NetworkScope {
    /// Whether `destination` (a `host:port` string) may be reached.
    pub fn allows(&self, destination: &str) -> bool {
        if self.allow.iter().any(|d| d == destination) {
            return true;
        }
        matches!(self.default, NetworkDefault::Allow)
    }
}

/// Canonicalize `path`, resolving `..` and symlinks. When the full path does
/// not exist, canonicalize the nearest existing ancestor (which resolves any
/// symlinks and `..` in the existing prefix) and re-append the remainder,
/// collapsing `.`/`..` in that remainder lexically. This lets a not-yet-created
/// leaf still be checked against a scope while a symlinked or `..`-laden prefix
/// is fully resolved first.
pub(crate) fn canonicalize_lenient(path: &Path) -> PathBuf {
    if let Ok(resolved) = std::fs::canonicalize(path) {
        return resolved;
    }
    let mut existing = path;
    while let Some(parent) = existing.parent() {
        if let Ok(base) = std::fs::canonicalize(parent) {
            let remainder = path.strip_prefix(parent).unwrap_or_else(|_| Path::new(""));
            let mut result = base;
            for component in remainder.components() {
                match component {
                    Component::ParentDir => {
                        result.pop();
                    }
                    Component::Normal(segment) => result.push(segment),
                    Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
                }
            }
            return result;
        }
        existing = parent;
    }
    path.to_path_buf()
}

/// Component-wise containment: `candidate` is `root` or lives under it. Uses
/// path components (never raw string prefixes) so `/foobar` is not "under"
/// `/foo`.
pub(crate) fn is_within(candidate: &Path, root: &Path) -> bool {
    candidate == root || candidate.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    #[test]
    fn is_within_is_component_wise() {
        assert!(is_within(Path::new("/foo/bar"), Path::new("/foo")));
        assert!(is_within(Path::new("/foo"), Path::new("/foo")));
        assert!(!is_within(Path::new("/foobar"), Path::new("/foo")));
        assert!(!is_within(Path::new("/foo"), Path::new("/foo/bar")));
    }

    #[test]
    fn lenient_resolves_parent_dir_in_missing_tail() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        // `<root>/exists/nope/../other` — only `<root>/exists` exists.
        std::fs::create_dir(root.join("exists")).unwrap();
        let messy = root.join("exists/nope/../other");
        let resolved = canonicalize_lenient(&messy);
        assert_eq!(resolved, root.join("exists/other"));
    }

    #[test]
    fn lenient_resolves_symlink_prefix() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let real = root.join("real");
        std::fs::create_dir(&real).unwrap();
        let link = root.join("link");
        symlink(&real, &link).unwrap();
        // A not-yet-created file under the symlink resolves through it.
        let resolved = canonicalize_lenient(&link.join("new.txt"));
        assert_eq!(resolved, real.join("new.txt"));
    }

    #[test]
    fn command_scope_requires_exact_program_match() {
        let scope = CommandScope {
            allowed_programs: vec!["cargo".to_string()],
            maximum_seconds: 900,
        };
        // A bare allow-listed name is permitted (resolved from a trusted PATH).
        assert!(scope.allows_program("cargo"));
        // Path-bearing impersonators are rejected — matching an allow-listed
        // basename would let these run arbitrary code as `cargo`.
        assert!(!scope.allows_program("./cargo"));
        assert!(!scope.allows_program("../cargo"));
        assert!(!scope.allows_program("/tmp/cargo"));
        assert!(!scope.allows_program("/usr/bin/cargo"));
        // A non-listed program is rejected.
        assert!(!scope.allows_program("rm"));
    }

    #[test]
    fn command_scope_allows_an_exact_configured_path() {
        // An admin may pin a specific binary by its full path.
        let scope = CommandScope {
            allowed_programs: vec!["/usr/bin/cargo".to_string()],
            maximum_seconds: 900,
        };
        assert!(scope.allows_program("/usr/bin/cargo"));
        // ...but only that exact path — not the bare name or another location.
        assert!(!scope.allows_program("cargo"));
        assert!(!scope.allows_program("/tmp/cargo"));
    }

    #[test]
    fn resolve_allows_a_new_nonexistent_leaf_under_the_root() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = PathScope::new(vec![root.clone()], vec![]);

        let target = root.join("new_file.txt");
        assert!(!target.exists());
        let (resolved, verdict) = scope.resolve(&target);

        assert_eq!(verdict, ScopeVerdict::Allowed);
        assert_eq!(resolved, target);
    }

    #[test]
    fn resolve_allows_an_existing_file_under_the_root() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = PathScope::new(vec![root.clone()], vec![]);

        let target = root.join("existing.txt");
        std::fs::write(&target, b"hello").unwrap();
        let (resolved, verdict) = scope.resolve(&target);

        assert_eq!(verdict, ScopeVerdict::Allowed);
        assert_eq!(resolved, target);
    }

    #[test]
    fn resolve_rejects_a_relative_escape_outside_the_root() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(root.join("inside")).unwrap();
        let scope = PathScope::new(vec![root.join("inside")], vec![]);

        // `<root>/inside/../outside` escapes the allowed root via `..`.
        let escape = root.join("inside/../outside");
        let (resolved, verdict) = scope.resolve(&escape);

        assert_eq!(verdict, ScopeVerdict::OutsideRoots);
        assert_eq!(resolved, root.join("outside"));
    }

    #[test]
    fn resolve_rejects_an_absolute_path_outside_the_root() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = PathScope::new(vec![root.join("inside")], vec![]);

        let outside = tempdir().unwrap();
        let outside_root = std::fs::canonicalize(outside.path()).unwrap();
        let (resolved, verdict) = scope.resolve(&outside_root.join("file.txt"));

        assert_eq!(verdict, ScopeVerdict::OutsideRoots);
        assert_eq!(resolved, outside_root.join("file.txt"));
    }

    #[test]
    fn resolve_denies_a_path_under_a_denied_subpath() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(root.join("secret")).unwrap();
        let scope = PathScope::new(vec![root.clone()], vec![root.join("secret")]);

        let (resolved, verdict) = scope.resolve(&root.join("secret/file.txt"));

        assert_eq!(verdict, ScopeVerdict::Denied);
        assert_eq!(resolved, root.join("secret/file.txt"));
    }

    #[test]
    fn resolve_resolves_a_symlinked_parent_before_the_root_check() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let real = root.join("real");
        std::fs::create_dir(&real).unwrap();
        let link = root.join("link");
        symlink(&real, &link).unwrap();
        let scope = PathScope::new(vec![real.clone()], vec![]);

        // A not-yet-created file addressed through the symlinked parent still
        // resolves to (and is classified against) the real, symlink-free path.
        let (resolved, verdict) = scope.resolve(&link.join("new.txt"));

        assert_eq!(verdict, ScopeVerdict::Allowed);
        assert_eq!(resolved, real.join("new.txt"));
    }

    #[test]
    fn resolve_returns_the_same_path_a_caller_checks_for_a_leaf_symlink() {
        // Demonstrates the no-TOCTOU seam write tools rely on: `resolve` hands
        // back the exact path a caller must then pass to `symlink_metadata`
        // (never a re-derived path) to refuse a symlink planted at the leaf.
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = PathScope::new(vec![root.clone()], vec![]);

        let elsewhere = tempdir().unwrap();
        let outside_target = elsewhere.path().join("secret.txt");
        std::fs::write(&outside_target, b"outside").unwrap();
        let leaf = root.join("planted_link");
        symlink(&outside_target, &leaf).unwrap();

        // `resolve` follows the symlink during canonicalization (it exists),
        // so a leaf symlink pointing outside the root is itself classified as
        // OutsideRoots — the scope check already catches this case.
        let (resolved, verdict) = scope.resolve(&leaf);
        assert_eq!(verdict, ScopeVerdict::OutsideRoots);

        // The caller can additionally confirm — on the SAME resolved path,
        // no re-derivation — whether the *original* leaf was a symlink, which
        // is the check a write tool performs immediately before writing to
        // guard against a symlink planted after this call returns.
        let leaf_metadata = std::fs::symlink_metadata(&leaf).unwrap();
        assert!(leaf_metadata.file_type().is_symlink());
        assert_eq!(resolved, std::fs::canonicalize(&outside_target).unwrap());
    }
}
