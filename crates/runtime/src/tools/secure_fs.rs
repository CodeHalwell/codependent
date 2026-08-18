//! Descriptor-relative filesystem access for runtime tools.
//!
//! Scope classification alone cannot close a check/use race: an attacker can
//! replace an ancestor or leaf with a symlink after canonicalization. These
//! helpers anchor traversal at an already-authorized root directory and open
//! every component with `O_NOFOLLOW`, so the object acted on is the one reached
//! through the authorized directory handles.

#[cfg(unix)]
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use codypendent_daemon::policy::{PathScope, ScopeVerdict};

use super::ToolError;

pub(crate) struct ScopedFile {
    pub path: PathBuf,
    pub file: std::fs::File,
    pub created: bool,
    /// The authorized directory handle the leaf was opened through, kept open so
    /// a caller can act on the leaf's *directory entry* — creating a sibling,
    /// renaming over it — with the same anchoring the open had.
    /// [`replace_contents`] is the only such caller.
    #[cfg(unix)]
    dir: rustix::fd::OwnedFd,
    /// The leaf's own name within [`Self::dir`].
    #[cfg(unix)]
    leaf: OsString,
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
            .map_err(|error| leaf_open_error(error, &resolved))?,
            false,
        ),
        Access::ReadWrite => (
            openat(
                &dir,
                leaf,
                OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .map_err(|error| leaf_open_error(error, &resolved))?,
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
            Err(error) => return Err(leaf_open_error(error, &resolved)),
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
        dir,
        leaf: leaf.clone(),
    })
}

/// Replace `scoped`'s contents with `bytes` **atomically**: write a fresh
/// sibling file, fsync it, and rename it over the leaf.
///
/// Truncating in place (`set_len(0)` then `write_all`) destroys the user's file
/// for any failure in between — a full disk, an I/O error, a killed daemon —
/// leaving a half-written or empty file and no copy of what was there. A rename
/// over the leaf is atomic for every reader: the file is either entirely the old
/// contents or entirely the new one, and a failure at any point before the
/// rename leaves the original untouched.
///
/// The temporary is created, renamed, and (on failure) removed **through the
/// same authorized directory handle** the leaf was opened through, so this adds
/// no path-resolution step an attacker could win by swapping an ancestor for a
/// symlink — the property `open_scoped` exists to provide. The original file's
/// permission bits are carried onto the replacement, since a rename installs a
/// new inode.
#[cfg(unix)]
pub(crate) fn replace_contents(scoped: &ScopedFile, bytes: &[u8]) -> Result<(), ToolError> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    use rustix::fs::{fsync, openat, renameat, unlinkat, AtFlags, Mode, OFlags};

    let mode = scoped.file.metadata()?.permissions().mode() & 0o7777;
    // `.` prefixed and uuid-suffixed: hidden from listings, and unique enough
    // that `O_EXCL` never collides with a live temporary of a concurrent edit.
    let temp_name = OsString::from(format!(
        ".codypendent-edit-{}.tmp",
        uuid::Uuid::now_v7().simple()
    ));
    let temp = openat(
        &scoped.dir,
        &temp_name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::from_raw_mode(0o600),
    )
    .map_err(errno_to_io)?;
    let mut temp = std::fs::File::from(temp);

    // Anything that fails from here on removes the temporary and leaves the
    // original file exactly as it was.
    let write = (|| -> Result<(), ToolError> {
        temp.write_all(bytes)?;
        temp.set_permissions(std::fs::Permissions::from_mode(mode))?;
        // Durable BEFORE the rename: a rename that lands ahead of the data
        // would, after a crash, publish an empty file over the real one.
        temp.sync_all()?;
        renameat(&scoped.dir, &temp_name, &scoped.dir, &scoped.leaf).map_err(errno_to_io)?;
        Ok(())
    })();
    if let Err(error) = write {
        let _ = unlinkat(&scoped.dir, &temp_name, AtFlags::empty());
        return Err(error);
    }
    // The rename itself is only durable once the directory entry is synced. A
    // failure here means the new contents may not survive a power loss, but the
    // file is one of the two whole versions either way, so it is not fatal.
    let _ = fsync(&scoped.dir);
    Ok(())
}

/// The non-Unix stub. Unreachable in practice: [`open_scoped`] refuses on any
/// platform without descriptor-relative traversal, so no [`ScopedFile`] exists
/// here to replace.
#[cfg(not(unix))]
pub(crate) fn replace_contents(_scoped: &ScopedFile, _bytes: &[u8]) -> Result<(), ToolError> {
    Err(ToolError::Other(anyhow::anyhow!(
        "secure descriptor-relative filesystem tools are unsupported on this platform"
    )))
}

#[cfg(unix)]
fn leaf_open_error(error: rustix::io::Errno, path: &Path) -> ToolError {
    use rustix::io::Errno;

    if matches!(error, Errno::LOOP | Errno::ISDIR) {
        ToolError::NotRegularFile(path.to_path_buf())
    } else {
        ToolError::Io(errno_to_io(error))
    }
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
