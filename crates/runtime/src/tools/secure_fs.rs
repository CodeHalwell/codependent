//! Descriptor-relative filesystem access for runtime tools.
//!
//! Scope classification alone cannot close a check/use race: an attacker can
//! replace an ancestor or leaf with a symlink after canonicalization. These
//! helpers anchor traversal at an already-authorized root directory and open
//! every component with `O_NOFOLLOW`, so the object acted on is the one reached
//! through the authorized directory handles.

use std::path::{Component, Path, PathBuf};

use codypendent_daemon::policy::{PathScope, ScopeVerdict};

use super::ToolError;

pub(crate) struct ScopedFile {
    pub path: PathBuf,
    pub file: std::fs::File,
    pub created: bool,
}

pub(crate) fn open_read(path: &Path, scope: &PathScope) -> Result<ScopedFile, ToolError> {
    open_scoped(path, scope, Access::Read, false)
}

pub(crate) fn open_edit(path: &Path, scope: &PathScope) -> Result<ScopedFile, ToolError> {
    open_scoped(path, scope, Access::ReadWrite, false)
}

pub(crate) fn open_write(path: &Path, scope: &PathScope) -> Result<ScopedFile, ToolError> {
    open_scoped(path, scope, Access::Write, true)
}

#[derive(Clone, Copy)]
enum Access {
    Read,
    Write,
    ReadWrite,
}

#[cfg(unix)]
fn open_scoped(
    path: &Path,
    scope: &PathScope,
    access: Access,
    create_parents: bool,
) -> Result<ScopedFile, ToolError> {
    use rustix::fs::{mkdirat, open, openat, Mode, OFlags};
    use rustix::io::Errno;

    let (resolved, verdict) = scope.resolve(path);
    match verdict {
        ScopeVerdict::Allowed => {}
        ScopeVerdict::Denied => return Err(ToolError::PathDenied(resolved)),
        ScopeVerdict::OutsideRoots => return Err(ToolError::PathOutOfScope(resolved)),
    }
    let root = scope
        .roots
        .iter()
        .filter(|root| resolved.starts_with(root))
        .max_by_key(|root| root.components().count())
        .ok_or_else(|| ToolError::PathOutOfScope(resolved.clone()))?;
    let relative = resolved
        .strip_prefix(root)
        .map_err(|_| ToolError::PathOutOfScope(resolved.clone()))?;
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(value) => Ok(value.to_os_string()),
            _ => Err(ToolError::PathOutOfScope(resolved.clone())),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let Some((leaf, parents)) = components.split_last() else {
        return Err(ToolError::NotRegularFile(resolved));
    };

    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let mut dir = open(root, dir_flags, Mode::empty()).map_err(errno_to_io)?;
    for component in parents {
        match openat(&dir, component, dir_flags, Mode::empty()) {
            Ok(next) => dir = next,
            Err(Errno::NOENT) if create_parents => {
                match mkdirat(&dir, component, Mode::from_raw_mode(0o755)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(error) => return Err(ToolError::Io(errno_to_io(error))),
                }
                dir = openat(&dir, component, dir_flags, Mode::empty()).map_err(errno_to_io)?;
            }
            Err(error) => return Err(ToolError::Io(errno_to_io(error))),
        }
    }

    let (fd, created) = match access {
        Access::Read => (
            openat(
                &dir,
                leaf,
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .map_err(errno_to_io)?,
            false,
        ),
        Access::ReadWrite => (
            openat(
                &dir,
                leaf,
                OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .map_err(errno_to_io)?,
            false,
        ),
        Access::Write => match openat(
            &dir,
            leaf,
            OFlags::WRONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(fd) => (fd, false),
            Err(Errno::NOENT) => (
                openat(
                    &dir,
                    leaf,
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::CLOEXEC
                        | OFlags::NOFOLLOW,
                    Mode::from_raw_mode(0o600),
                )
                .map_err(errno_to_io)?,
                true,
            ),
            Err(error) => return Err(ToolError::Io(errno_to_io(error))),
        },
    };
    let file = std::fs::File::from(fd);
    if !file.metadata()?.is_file() {
        return Err(ToolError::NotRegularFile(resolved));
    }
    Ok(ScopedFile {
        path: resolved,
        file,
        created,
    })
}

#[cfg(unix)]
fn errno_to_io(error: rustix::io::Errno) -> std::io::Error {
    std::io::Error::from_raw_os_error(error.raw_os_error())
}

#[cfg(not(unix))]
fn open_scoped(
    path: &Path,
    scope: &PathScope,
    _access: Access,
    _create_parents: bool,
) -> Result<ScopedFile, ToolError> {
    let (resolved, verdict) = scope.resolve(path);
    match verdict {
        ScopeVerdict::Denied => Err(ToolError::PathDenied(resolved)),
        ScopeVerdict::OutsideRoots => Err(ToolError::PathOutOfScope(resolved)),
        ScopeVerdict::Allowed => Err(ToolError::Other(anyhow::anyhow!(
            "secure descriptor-relative filesystem tools are unsupported on this platform"
        ))),
    }
}
