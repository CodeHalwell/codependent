//! Workspace isolation and clean teardown between job attempts.

use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::types::RunnerError;

/// Workspace manager creating per-attempt isolated directories.
#[derive(Debug, Clone)]
pub struct WorkspaceManager {
    base_dir: PathBuf,
}

impl WorkspaceManager {
    /// Create a workspace manager with a base root path.
    #[must_use]
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    /// Allocate and initialize a fresh, isolated workspace for a job attempt.
    pub fn create_workspace(
        &self,
        job_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<WorkspaceGuard, RunnerError> {
        let attempt_dir = self
            .base_dir
            .join(job_id.to_string())
            .join(attempt_id.to_string());

        // Ensure clean state if anything lingered
        if attempt_dir.exists() {
            let _ = reset_permissions_and_remove(&attempt_dir);
        }

        let source_dir = attempt_dir.join("src");
        let output_dir = attempt_dir.join("out");
        let temp_dir = attempt_dir.join("tmp");

        fs::create_dir_all(&source_dir)
            .map_err(|e| RunnerError::Workspace(format!("failed to create source dir: {e}")))?;
        fs::create_dir_all(&output_dir)
            .map_err(|e| RunnerError::Workspace(format!("failed to create output dir: {e}")))?;
        fs::create_dir_all(&temp_dir)
            .map_err(|e| RunnerError::Workspace(format!("failed to create temp dir: {e}")))?;

        Ok(WorkspaceGuard {
            workspace_dir: attempt_dir,
            source_dir,
            output_dir,
            temp_dir,
            is_cleaned: false,
        })
    }
}

/// A RAII guard ensuring isolated workspaces are completely wiped on teardown or drop.
pub struct WorkspaceGuard {
    pub workspace_dir: PathBuf,
    pub source_dir: PathBuf,
    pub output_dir: PathBuf,
    pub temp_dir: PathBuf,
    is_cleaned: bool,
}

impl WorkspaceGuard {
    /// The root workspace path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.workspace_dir
    }

    /// Explicitly teardown and wipe the workspace directory.
    pub fn teardown(&mut self) -> Result<(), RunnerError> {
        if !self.is_cleaned {
            reset_permissions_and_remove(&self.workspace_dir)
                .map_err(|e| RunnerError::Workspace(format!("failed to wipe workspace: {e}")))?;
            self.is_cleaned = true;
        }
        Ok(())
    }
}

impl Drop for WorkspaceGuard {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

fn reset_permissions_and_remove(dir: &Path) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    #[cfg(unix)]
    {
        // Recursively fix permissions in case a process created read-only files (chmod 0400)
        let _ = make_tree_writable(dir);
    }

    fs::remove_dir_all(dir)
}

#[cfg(unix)]
fn make_tree_writable(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if dir.is_dir() {
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let _ = make_tree_writable(&path);
                } else {
                    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o644));
                }
            }
        }
    }
    Ok(())
}
