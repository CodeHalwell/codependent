//! Immutable, runner-local policy ceiling for untrusted job specifications.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use codypendent_sandbox::ENV_ALLOWLIST;

use crate::types::{JobSpec, RunnerError};

/// Guest-visible root of the per-attempt workspace.
pub const GUEST_WORKSPACE_ROOT: &str = "/workspace";

/// Policy configured by the runner owner, never by a claimed job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerPolicy {
    env_allowlist: HashSet<String>,
}

impl Default for RunnerPolicy {
    fn default() -> Self {
        Self::new(ENV_ALLOWLIST.iter().copied())
    }
}

impl RunnerPolicy {
    /// Construct a local policy with a closed environment-name allowlist.
    #[must_use]
    pub fn new(env_allowlist: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            env_allowlist: env_allowlist.into_iter().map(Into::into).collect(),
        }
    }

    /// Resolve the wire policy beneath a concrete attempt workspace.
    pub(crate) fn resolve(
        &self,
        job: &JobSpec,
        workspace_root: &Path,
    ) -> Result<ResolvedRunnerPolicy, RunnerError> {
        let workspace_root = std::fs::canonicalize(workspace_root).map_err(|error| {
            RunnerError::Workspace(format!(
                "failed to resolve attempt workspace for local policy: {error}"
            ))
        })?;

        let read_paths = resolve_grants(&job.sandbox.read_paths, &workspace_root, "read_paths")?;
        let write_paths = resolve_grants(&job.sandbox.write_paths, &workspace_root, "write_paths")?;
        let working_directory = resolve_guest_path(
            job.working_directory.as_deref().unwrap_or("/workspace/src"),
            &workspace_root,
            "working_directory",
        )?;

        let requested_env: Vec<&str> = if job.sandbox.env_allowlist.is_empty() {
            self.env_allowlist.iter().map(String::as_str).collect()
        } else {
            job.sandbox
                .env_allowlist
                .iter()
                .map(String::as_str)
                .collect()
        };
        let mut env_allowlist = Vec::with_capacity(requested_env.len());
        let mut seen = HashSet::with_capacity(requested_env.len());
        for name in requested_env {
            if !valid_env_name(name) || !self.env_allowlist.contains(name) {
                return Err(RunnerError::UnauthorizedScope(format!(
                    "environment name {name:?} is outside the runner-local allowlist"
                )));
            }
            if seen.insert(name) {
                env_allowlist.push(name.to_string());
            }
        }
        env_allowlist.sort_unstable();

        for name in job.env.keys() {
            if !env_allowlist.iter().any(|allowed| allowed == name) {
                return Err(RunnerError::UnauthorizedScope(format!(
                    "job environment value {name:?} is outside the effective local policy"
                )));
            }
        }

        let effective_read_paths = if read_paths.is_empty() && write_paths.is_empty() {
            vec![ResolvedWorkspacePath::root(&workspace_root)]
        } else {
            read_paths
        };
        let effective_write_paths =
            if job.sandbox.read_paths.is_empty() && job.sandbox.write_paths.is_empty() {
                vec![ResolvedWorkspacePath::root(&workspace_root)]
            } else {
                write_paths
            };

        if !effective_read_paths
            .iter()
            .chain(effective_write_paths.iter())
            .any(|grant| working_directory.host.starts_with(&grant.host))
        {
            return Err(RunnerError::UnauthorizedScope(format!(
                "working directory {:?} is outside the job's effective workspace grants",
                working_directory.guest
            )));
        }

        Ok(ResolvedRunnerPolicy {
            read_paths: effective_read_paths,
            write_paths: effective_write_paths,
            working_directory,
            env_allowlist,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedRunnerPolicy {
    pub read_paths: Vec<ResolvedWorkspacePath>,
    pub write_paths: Vec<ResolvedWorkspacePath>,
    pub working_directory: ResolvedWorkspacePath,
    pub env_allowlist: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedWorkspacePath {
    pub host: PathBuf,
    pub guest: String,
}

impl ResolvedWorkspacePath {
    fn root(workspace_root: &Path) -> Self {
        Self {
            host: workspace_root.to_path_buf(),
            guest: GUEST_WORKSPACE_ROOT.to_string(),
        }
    }
}

fn resolve_grants(
    grants: &[String],
    workspace_root: &Path,
    field: &str,
) -> Result<Vec<ResolvedWorkspacePath>, RunnerError> {
    let mut resolved = Vec::with_capacity(grants.len());
    let mut seen = HashSet::with_capacity(grants.len());
    for grant in grants {
        let path = resolve_guest_path(grant, workspace_root, field)?;
        if seen.insert(path.host.clone()) {
            resolved.push(path);
        }
    }
    Ok(resolved)
}

fn resolve_guest_path(
    raw: &str,
    workspace_root: &Path,
    field: &str,
) -> Result<ResolvedWorkspacePath, RunnerError> {
    let has_empty_component = if raw.starts_with('/') {
        raw.split('/').skip(1).any(str::is_empty)
    } else {
        raw.split('/').any(str::is_empty)
    };
    if raw.is_empty() || raw.contains('\\') || has_empty_component {
        return Err(invalid_workspace_path(field, raw));
    }

    let raw_path = Path::new(raw);
    let relative = if raw_path.is_absolute() {
        raw_path
            .strip_prefix(GUEST_WORKSPACE_ROOT)
            .map_err(|_| invalid_workspace_path(field, raw))?
    } else {
        raw_path
    };
    if relative.components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir | Component::CurDir
        )
    }) {
        return Err(invalid_workspace_path(field, raw));
    }

    let candidate = workspace_root.join(relative);
    let host = std::fs::canonicalize(&candidate).map_err(|error| {
        RunnerError::UnauthorizedScope(format!(
            "{field} path {raw:?} does not resolve inside the attempt workspace: {error}"
        ))
    })?;
    if !host.starts_with(workspace_root) {
        return Err(invalid_workspace_path(field, raw));
    }
    let guest = if relative.as_os_str().is_empty() {
        GUEST_WORKSPACE_ROOT.to_string()
    } else {
        format!(
            "{GUEST_WORKSPACE_ROOT}/{}",
            relative.to_string_lossy().trim_start_matches('/')
        )
    };

    Ok(ResolvedWorkspacePath { host, guest })
}

fn invalid_workspace_path(field: &str, raw: &str) -> RunnerError {
    RunnerError::UnauthorizedScope(format!(
        "{field} path {raw:?} must be a normalized guest path beneath {GUEST_WORKSPACE_ROOT}"
    ))
}

fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'_'))
        && bytes.all(|byte| matches!(byte, b'A'..=b'Z' | b'0'..=b'9' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ResourceSpec, SandboxSpec, WorkspaceLayout};
    use tempfile::TempDir;

    fn job() -> JobSpec {
        JobSpec {
            argv: vec!["/bin/true".to_string()],
            env: Default::default(),
            working_directory: None,
            workspace_layout: WorkspaceLayout::default(),
            input_manifest_hash: "none".to_string(),
            sandbox: SandboxSpec::default(),
            resources: ResourceSpec::default(),
            outputs: vec![],
            max_attempts: 1,
        }
    }

    fn workspace() -> TempDir {
        let workspace = TempDir::new().unwrap();
        for name in ["src", "out", "tmp"] {
            std::fs::create_dir(workspace.path().join(name)).unwrap();
        }
        workspace
    }

    #[test]
    fn host_paths_and_workspace_symlink_escapes_are_refused() {
        let workspace = workspace();
        let policy = RunnerPolicy::default();
        for path in ["/", "/etc", "/workspace/../etc", "../outside"] {
            let mut request = job();
            request.sandbox.read_paths = vec![path.to_string()];
            assert!(matches!(
                policy.resolve(&request, workspace.path()),
                Err(RunnerError::UnauthorizedScope(_))
            ));
        }

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/", workspace.path().join("escape")).unwrap();
            let mut request = job();
            request.sandbox.write_paths = vec!["/workspace/escape".to_string()];
            assert!(matches!(
                policy.resolve(&request, workspace.path()),
                Err(RunnerError::UnauthorizedScope(_))
            ));
        }
    }

    #[test]
    fn wire_environment_can_only_narrow_the_local_allowlist() {
        let workspace = workspace();
        let policy = RunnerPolicy::new(["PATH", "LANG"]);
        let mut request = job();
        request.sandbox.env_allowlist = vec!["LANG".to_string()];
        let resolved = policy.resolve(&request, workspace.path()).unwrap();
        assert_eq!(resolved.env_allowlist, vec!["LANG"]);

        request.sandbox.env_allowlist = vec!["AWS_SECRET_ACCESS_KEY".to_string()];
        assert!(matches!(
            policy.resolve(&request, workspace.path()),
            Err(RunnerError::UnauthorizedScope(_))
        ));
    }
}
