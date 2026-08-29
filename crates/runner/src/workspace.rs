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

        // `Path::exists` follows symlinks and reports a dangling one as absent.
        // Inspect the directory entry itself so a stale attempt-root symlink is
        // always unlinked before `create_dir_all` can resolve through it.
        match fs::symlink_metadata(&attempt_dir) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                reset_permissions_and_remove(&attempt_dir).map_err(|error| {
                    RunnerError::Workspace(format!(
                        "failed to clear stale workspace entry: {error}"
                    ))
                })?
            }
            Ok(_) => {
                let quarantine_path = quarantine_workspace(&attempt_dir).map_err(|error| {
                    RunnerError::Workspace(format!(
                        "existing attempt workspace may contain resumable execution state and could not be quarantined: {error}"
                    ))
                })?;
                return Err(RunnerError::Workspace(format!(
                    "existing attempt workspace was not erased; quarantined at {}",
                    quarantine_path.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(RunnerError::Workspace(format!(
                    "failed to inspect workspace path: {error}"
                )))
            }
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

    /// Re-open an existing attempt workspace after a durable upload journal
    /// proves execution already completed. Never creates or follows a root link.
    pub(crate) fn resume_workspace(
        &self,
        job_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<WorkspaceGuard, RunnerError> {
        let attempt_dir = self
            .base_dir
            .join(job_id.to_string())
            .join(attempt_id.to_string());
        let metadata = fs::symlink_metadata(&attempt_dir).map_err(|error| {
            RunnerError::Workspace(format!("failed to inspect resumable workspace: {error}"))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(RunnerError::Workspace(
                "resumable workspace root is not a real directory".to_string(),
            ));
        }
        let canonical_base = fs::canonicalize(&self.base_dir).map_err(|error| {
            RunnerError::Workspace(format!("failed to resolve runner workspace base: {error}"))
        })?;
        let canonical_attempt = fs::canonicalize(&attempt_dir).map_err(|error| {
            RunnerError::Workspace(format!("failed to resolve resumable workspace: {error}"))
        })?;
        if !canonical_attempt.starts_with(&canonical_base) {
            return Err(RunnerError::Workspace(
                "resumable workspace resolves outside the runner base".to_string(),
            ));
        }

        Ok(WorkspaceGuard {
            source_dir: attempt_dir.join("src"),
            output_dir: attempt_dir.join("out"),
            temp_dir: attempt_dir.join("tmp"),
            workspace_dir: attempt_dir,
            is_cleaned: false,
        })
    }

    pub(crate) fn upload_journal_path(&self, job_id: Uuid, attempt_id: Uuid) -> PathBuf {
        self.base_dir
            .join(".runner-state")
            .join(job_id.to_string())
            .join(format!("{attempt_id}.json"))
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
            match reset_permissions_and_remove(&self.workspace_dir) {
                Ok(()) => self.is_cleaned = true,
                Err(cleanup_error) => {
                    return match quarantine_workspace(&self.workspace_dir) {
                        Ok(quarantine_path) => {
                            self.is_cleaned = true;
                            Err(RunnerError::Workspace(format!(
                                "failed to wipe workspace: {cleanup_error}; quarantined at {}",
                                quarantine_path.display()
                            )))
                        }
                        Err(quarantine_error) => Err(RunnerError::Workspace(format!(
                            "failed to wipe workspace: {cleanup_error}; quarantine also failed: {quarantine_error}"
                        ))),
                    };
                }
            }
        }
        Ok(())
    }

    /// Keep this workspace intact because durable state will resume harvesting.
    pub(crate) fn preserve_for_resume(&mut self) {
        self.is_cleaned = true;
    }
}

impl Drop for WorkspaceGuard {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

fn reset_permissions_and_remove(dir: &Path) -> std::io::Result<()> {
    let metadata = match fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };

    // A stale workspace root may itself have been replaced with a symlink.
    // Remove the directory entry, never the tree it points at.
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return fs::remove_file(dir);
    }

    #[cfg(unix)]
    {
        // Only directories need owner access for recursive unlinking. Open every
        // directory with O_NOFOLLOW and chmod the descriptor itself, so a
        // workspace symlink can never redirect permission changes outside it.
        make_tree_writable(dir)?;
    }

    fs::remove_dir_all(dir)
}

#[cfg(unix)]
fn make_tree_writable(dir: &Path) -> std::io::Result<()> {
    use rustix::fs::{open, Mode, OFlags};

    let parent = dir.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace directory has no parent",
        )
    })?;
    let name = dir.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace directory has no file name",
        )
    })?;
    let parent = open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let directory = open_directory_for_cleanup(&parent, name)?;
    make_open_tree_writable(&std::fs::File::from(directory))
}

#[cfg(unix)]
fn make_open_tree_writable(directory: &std::fs::File) -> std::io::Result<()> {
    use rustix::fs::{fchmod, statat, AtFlags, Dir, FileType, Mode};

    fchmod(directory, Mode::from_raw_mode(0o700)).map_err(std::io::Error::from)?;
    let mut entries = Dir::read_from(directory).map_err(std::io::Error::from)?;
    while let Some(entry) = entries.read() {
        let entry = entry.map_err(std::io::Error::from)?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let stat =
            statat(directory, name, AtFlags::SYMLINK_NOFOLLOW).map_err(std::io::Error::from)?;
        if FileType::from_raw_mode(stat.st_mode).is_dir() {
            let child = open_directory_for_cleanup(directory, name)?;
            make_open_tree_writable(&std::fs::File::from(child))?;
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_directory_for_cleanup(
    parent: &impl std::os::fd::AsFd,
    name: impl rustix::path::Arg + Copy,
) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;

    use rustix::fs::{openat, Mode, OFlags};

    // O_PATH obtains a stable handle without requiring read permission. Chmod
    // through procfs addresses that exact inode, so a swapped symlink cannot
    // redirect permission repair outside the workspace.
    let handle = openat(
        parent,
        name,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let proc_path = PathBuf::from(format!("/proc/self/fd/{}", handle.as_raw_fd()));
    fs::set_permissions(proc_path, fs::Permissions::from_mode(0o700))?;
    openat(
        &handle,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn open_directory_for_cleanup(
    parent: &impl std::os::fd::AsFd,
    name: impl rustix::path::Arg + Copy,
) -> std::io::Result<std::os::fd::OwnedFd> {
    use rustix::fs::{chmodat, openat, AtFlags, Mode, OFlags};

    chmodat(
        parent,
        name,
        Mode::from_raw_mode(0o700),
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(std::io::Error::from)?;
    openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)
}

fn quarantine_workspace(workspace_dir: &Path) -> std::io::Result<PathBuf> {
    let attempt = workspace_dir.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace directory has no attempt id",
        )
    })?;
    let job_dir = workspace_dir.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace directory has no job parent",
        )
    })?;
    let job = job_dir.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace directory has no job id",
        )
    })?;
    let base = job_dir.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace directory has no runner base",
        )
    })?;
    let quarantine_root = base.join(".quarantine");
    ensure_real_directory(&quarantine_root)?;
    let canonical_base = fs::canonicalize(base)?;
    let canonical_quarantine = fs::canonicalize(&quarantine_root)?;
    if !canonical_quarantine.starts_with(&canonical_base) || canonical_quarantine == canonical_base
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "quarantine root resolves outside the runner workspace base",
        ));
    }

    let quarantine_dir = quarantine_root.join(job);
    ensure_real_directory(&quarantine_dir)?;
    let quarantine_path = quarantine_dir.join(attempt);
    if fs::symlink_metadata(&quarantine_path).is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "quarantine target already exists: {}",
                quarantine_path.display()
            ),
        ));
    }
    fs::rename(workspace_dir, &quarantine_path)?;
    Ok(quarantine_path)
}

fn ensure_real_directory(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("expected a real directory at {}", path.display()),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
            let metadata = fs::symlink_metadata(path)?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                Ok(())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "directory path was replaced during creation: {}",
                        path.display()
                    ),
                ))
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[cfg(unix)]
    #[test]
    fn teardown_removes_workspace_symlink_without_touching_its_target() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let base = TempDir::new().unwrap();
        let outside = base.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let secret = outside.join("secret-key");
        fs::write(&secret, b"outside-data").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();

        let manager = WorkspaceManager::new(base.path().join("workspaces"));
        let mut workspace = manager
            .create_workspace(Uuid::now_v7(), Uuid::now_v7())
            .unwrap();
        let nested = workspace.output_dir.join("nested");
        fs::create_dir(&nested).unwrap();
        symlink(&outside, nested.join("escape")).unwrap();

        workspace.teardown().unwrap();

        assert_eq!(fs::read(&secret).unwrap(), b"outside-data");
        assert_eq!(
            fs::metadata(&secret).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!workspace.path().exists());
    }

    #[cfg(unix)]
    #[test]
    fn stale_workspace_root_symlink_is_unlinked_not_followed() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let base = TempDir::new().unwrap();
        let workspaces = base.path().join("workspaces");
        let job_id = Uuid::now_v7();
        let attempt_id = Uuid::now_v7();
        let job_dir = workspaces.join(job_id.to_string());
        fs::create_dir_all(&job_dir).unwrap();

        let outside = base.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let secret = outside.join("secret-key");
        fs::write(&secret, b"outside-data").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&outside, job_dir.join(attempt_id.to_string())).unwrap();

        let manager = WorkspaceManager::new(&workspaces);
        let mut workspace = manager.create_workspace(job_id, attempt_id).unwrap();
        assert!(workspace.path().is_dir());
        assert!(!outside.join("src").exists());
        assert!(!outside.join("out").exists());
        assert!(!outside.join("tmp").exists());
        workspace.teardown().unwrap();

        assert_eq!(fs::read(&secret).unwrap(), b"outside-data");
        assert_eq!(
            fs::metadata(&secret).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn dangling_workspace_root_symlink_is_removed_before_creation() {
        use std::os::unix::fs::symlink;

        let base = TempDir::new().unwrap();
        let workspaces = base.path().join("workspaces");
        let job_id = Uuid::now_v7();
        let attempt_id = Uuid::now_v7();
        let job_dir = workspaces.join(job_id.to_string());
        fs::create_dir_all(&job_dir).unwrap();
        let outside = base.path().join("must-not-be-created");
        symlink(&outside, job_dir.join(attempt_id.to_string())).unwrap();

        let manager = WorkspaceManager::new(&workspaces);
        let mut workspace = manager.create_workspace(job_id, attempt_id).unwrap();
        assert!(workspace.path().is_dir());
        assert!(!outside.exists());
        workspace.teardown().unwrap();
    }

    #[test]
    fn existing_attempt_directory_is_quarantined_instead_of_erased() {
        let base = TempDir::new().unwrap();
        let workspaces = base.path().join("workspaces");
        let job_id = Uuid::now_v7();
        let attempt_id = Uuid::now_v7();
        let attempt = workspaces
            .join(job_id.to_string())
            .join(attempt_id.to_string());
        fs::create_dir_all(&attempt).unwrap();
        fs::write(attempt.join("completed-output"), b"must survive").unwrap();

        let manager = WorkspaceManager::new(&workspaces);
        let error = match manager.create_workspace(job_id, attempt_id) {
            Ok(_) => panic!("existing workspace must not be erased"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            RunnerError::Workspace(message) if message.contains("quarantined")
        ));
        let quarantined = workspaces
            .join(".quarantine")
            .join(job_id.to_string())
            .join(attempt_id.to_string())
            .join("completed-output");
        assert_eq!(fs::read(quarantined).unwrap(), b"must survive");
    }

    #[cfg(unix)]
    #[test]
    fn quarantine_refuses_a_redirected_quarantine_root() {
        use std::os::unix::fs::symlink;

        let base = TempDir::new().unwrap();
        let workspaces = base.path().join("workspaces");
        let outside = base.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(&workspaces).unwrap();
        symlink(&outside, workspaces.join(".quarantine")).unwrap();

        let job_id = Uuid::now_v7();
        let attempt_id = Uuid::now_v7();
        let attempt = workspaces
            .join(job_id.to_string())
            .join(attempt_id.to_string());
        fs::create_dir_all(&attempt).unwrap();
        fs::write(attempt.join("sensitive"), b"must remain controlled").unwrap();

        let manager = WorkspaceManager::new(&workspaces);
        let error = match manager.create_workspace(job_id, attempt_id) {
            Ok(_) => panic!("redirected quarantine root must be refused"),
            Err(error) => error,
        };
        assert!(matches!(error, RunnerError::Workspace(_)));
        assert_eq!(
            fs::read(attempt.join("sensitive")).unwrap(),
            b"must remain controlled"
        );
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn teardown_recovers_mode_zero_directories_without_following_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let base = TempDir::new().unwrap();
        let outside = base.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let secret = outside.join("secret");
        fs::write(&secret, b"outside-data").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();

        let manager = WorkspaceManager::new(base.path().join("workspaces"));
        let mut workspace = manager
            .create_workspace(Uuid::now_v7(), Uuid::now_v7())
            .unwrap();
        let locked = workspace.output_dir.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("artifact"), b"sensitive").unwrap();
        symlink(&outside, locked.join("escape")).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        workspace.teardown().unwrap();

        assert!(!workspace.path().exists());
        assert_eq!(fs::read(&secret).unwrap(), b"outside-data");
        assert_eq!(
            fs::metadata(&secret).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
