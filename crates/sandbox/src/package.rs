//! Safe package extraction, verification against tampering, and filesystem isolation (Milestone 5).
//!
//! Enforces:
//! - Content bounds: maximum archive bytes, maximum file count, maximum file size, total uncompressed size.
//! - Path normalization: no empty or absolute paths, no `..` or escaping components, depth/length limits.
//! - Entry types: only regular files and directories (symlinks, hardlinks, FIFOs, devices are rejected).
//! - Archive sanity: duplicate normalized paths rejected, empty packages rejected.
//! - Compression ratio defense: refuses archives whose uncompressed-to-compressed ratio exceeds [`MAX_COMPRESSION_RATIO`].
//! - Content sealing: atomic write-once, directory synchronization, and read-only tree freezing.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;
use tar::Archive;
use uuid::Uuid;

use crate::verify::checksum_of;

/// Maximum compressed package archive size (10 MB).
pub const MAX_PACKAGE_ARCHIVE_BYTES: usize = 10 * 1024 * 1024;
/// Maximum regular files in a package.
pub const MAX_PACKAGE_FILES: usize = 10_000;
/// Maximum single uncompressed file size (64 MB).
pub const MAX_PACKAGE_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum total uncompressed package size (256 MB).
pub const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum total entries in an archive (files + directories).
pub const MAX_ARCHIVE_ENTRIES: usize = 20_000;
/// Maximum directory entries in an archive.
pub const MAX_ARCHIVE_DIRECTORIES: usize = 10_000;
/// Maximum archive path length in bytes.
pub const MAX_ARCHIVE_PATH_BYTES: usize = 4_096;
/// Maximum archive path nesting depth.
pub const MAX_ARCHIVE_PATH_DEPTH: usize = 64;
/// Maximum allowed uncompressed-to-compressed ratio (100:1) to prevent decompression bombs.
pub const MAX_COMPRESSION_RATIO: u64 = 100;

/// Errors that can occur during safe package handling.
#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("package I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid package path: {0}")]
    InvalidPath(String),
    #[error("package limit exceeded: {0}")]
    LimitExceeded(String),
    #[error("compression ratio exceeded: {ratio}:1 > {max}:1")]
    CompressionRatioExceeded { ratio: u64, max: u64 },
    #[error("invalid package archive: {0}")]
    Invalid(String),
    #[error("package verification failed (tampered content or digest mismatch)")]
    Authentication,
    #[error("package contains no regular files")]
    EmptyPackage,
}

/// Normalize an archive relative path and verify it is non-empty, non-absolute,
/// contains only normal components (no `..` or `.`), and stays within length and depth bounds.
pub fn normalized_path(path: &Path) -> Result<PathBuf, PackageError> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(PackageError::InvalidPath(
            "archive path is empty or absolute".into(),
        ));
    }
    if path.as_os_str().as_encoded_bytes().len() > MAX_ARCHIVE_PATH_BYTES
        || path.components().count() > MAX_ARCHIVE_PATH_DEPTH
    {
        return Err(PackageError::LimitExceeded(
            "archive path exceeds host length/depth limits".into(),
        ));
    }
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => output.push(value),
            _ => {
                return Err(PackageError::InvalidPath(
                    "archive path is not normalized".into(),
                ))
            }
        }
    }
    if output.as_os_str().is_empty() {
        return Err(PackageError::InvalidPath(
            "archive path normalized to empty".into(),
        ));
    }
    Ok(output)
}

/// Safely extract a `.tar.gz` package artifact into `root`.
///
/// Fails closed if:
/// - The archive exceeds [`MAX_PACKAGE_ARCHIVE_BYTES`].
/// - Total entries exceed [`MAX_ARCHIVE_ENTRIES`] or directories exceed [`MAX_ARCHIVE_DIRECTORIES`].
/// - Total regular files exceed [`MAX_PACKAGE_FILES`].
/// - Any file exceeds [`MAX_PACKAGE_FILE_BYTES`] or total uncompressed bytes exceed [`MAX_PACKAGE_BYTES`].
/// - The compression ratio exceeds [`MAX_COMPRESSION_RATIO`].
/// - Any path contains `..`, absolute components, or non-normal elements.
/// - Any duplicate path is encountered.
/// - Any entry is not a regular file or directory (symlinks/hardlinks/devices are refused).
/// - The archive contains zero regular files.
pub fn extract_package(artifact: &[u8], root: &Path) -> Result<(), PackageError> {
    if artifact.len() > MAX_PACKAGE_ARCHIVE_BYTES {
        return Err(PackageError::LimitExceeded(format!(
            "archive exceeds maximum size ({} > {})",
            artifact.len(),
            MAX_PACKAGE_ARCHIVE_BYTES
        )));
    }

    let decoder = GzDecoder::new(Cursor::new(artifact));
    let mut archive = Archive::new(decoder);
    let mut seen = HashSet::new();
    let mut files = 0_usize;
    let mut directories = 0_usize;
    let mut entries = 0_usize;
    let mut total = 0_u64;

    for entry in archive
        .entries()
        .map_err(|error| PackageError::Invalid(error.to_string()))?
    {
        let mut entry = entry.map_err(|error| PackageError::Invalid(error.to_string()))?;
        entries += 1;
        if entries > MAX_ARCHIVE_ENTRIES {
            return Err(PackageError::LimitExceeded(
                "archive exceeds total entry limit".into(),
            ));
        }

        let entry_path = entry
            .path()
            .map_err(|error| PackageError::Invalid(error.to_string()))?;
        let relative = normalized_path(&entry_path)?;

        if !seen.insert(relative.clone()) {
            return Err(PackageError::Invalid(format!(
                "duplicate archive path `{}`",
                relative.display()
            )));
        }

        let target = root.join(&relative);
        if entry.header().entry_type().is_dir() {
            directories += 1;
            if directories > MAX_ARCHIVE_DIRECTORIES {
                return Err(PackageError::LimitExceeded(
                    "archive exceeds directory entry limit".into(),
                ));
            }
            create_private_dir(&target)?;
            continue;
        }

        if !entry.header().entry_type().is_file() {
            return Err(PackageError::Invalid(format!(
                "archive entry `{}` is not a regular file",
                relative.display()
            )));
        }

        files += 1;
        if files > MAX_PACKAGE_FILES || entry.size() > MAX_PACKAGE_FILE_BYTES {
            return Err(PackageError::LimitExceeded(
                "archive exceeds package file limits".into(),
            ));
        }

        total = total
            .checked_add(entry.size())
            .ok_or_else(|| PackageError::LimitExceeded("package size overflow".into()))?;

        if total > MAX_PACKAGE_BYTES {
            return Err(PackageError::LimitExceeded(
                "archive exceeds uncompressed package limit".into(),
            ));
        }

        let compressed_len = artifact.len() as u64;
        if compressed_len > 0 && total > compressed_len.saturating_mul(MAX_COMPRESSION_RATIO) {
            let ratio = total / compressed_len;
            return Err(PackageError::CompressionRatioExceeded {
                ratio,
                max: MAX_COMPRESSION_RATIO,
            });
        }

        if let Some(parent) = target.parent() {
            create_private_dir(parent)?;
        }

        let mut file = private_new_file(&target)?;
        let copied = std::io::copy(
            &mut entry.by_ref().take(MAX_PACKAGE_FILE_BYTES + 1),
            &mut file,
        )
        .map_err(|source| PackageError::Io {
            path: target.clone(),
            source,
        })?;

        if copied != entry.size() {
            return Err(PackageError::Invalid(format!(
                "archive entry `{}` size mismatch",
                relative.display()
            )));
        }

        file.sync_all().map_err(|source| PackageError::Io {
            path: target,
            source,
        })?;
    }

    if files == 0 {
        return Err(PackageError::EmptyPackage);
    }

    Ok(())
}

/// Verify that an existing extracted package directory matches the artifact.
pub fn verify_existing_package(root: &Path, artifact: &[u8]) -> Result<(), PackageError> {
    let temporary = root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| PackageError::Invalid("invalid package root".into()))?
        .join("tmp")
        .join(Uuid::now_v7().to_string());
    create_private_dir(&temporary)?;
    extract_package(artifact, &temporary)?;
    let expected = directory_seal(&temporary)?;
    let actual = directory_seal(root)?;
    let _ = std::fs::remove_dir_all(&temporary);
    if expected != actual {
        return Err(PackageError::Authentication);
    }
    Ok(())
}

/// Compute a sorted seal of relative paths and checksums for all files in `root`.
pub fn directory_seal(root: &Path) -> Result<Vec<(PathBuf, String)>, PackageError> {
    fn visit(
        root: &Path,
        directory: &Path,
        output: &mut Vec<(PathBuf, String)>,
    ) -> Result<(), PackageError> {
        for entry in std::fs::read_dir(directory).map_err(|source| PackageError::Io {
            path: directory.to_path_buf(),
            source,
        })? {
            let entry = entry.map_err(|source| PackageError::Io {
                path: directory.to_path_buf(),
                source,
            })?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|source| PackageError::Io {
                path: path.clone(),
                source,
            })?;
            if metadata.file_type().is_symlink() {
                return Err(PackageError::Authentication);
            }
            if metadata.is_dir() {
                visit(root, &path, output)?;
            } else if metadata.is_file() {
                let bytes = std::fs::read(&path).map_err(|source| PackageError::Io {
                    path: path.clone(),
                    source,
                })?;
                output.push((
                    path.strip_prefix(root)
                        .map_err(|_| PackageError::Authentication)?
                        .to_path_buf(),
                    checksum_of(&bytes),
                ));
            } else {
                return Err(PackageError::Authentication);
            }
        }
        Ok(())
    }
    let mut output = Vec::new();
    visit(root, root, &mut output)?;
    output.sort();
    Ok(output)
}

/// Freeze a package tree by making directories and files read-only (mode 0500 / 0400 on unix).
pub fn freeze_package_tree(root: &Path) -> Result<(), PackageError> {
    fn visit(path: &Path) -> Result<(), PackageError> {
        for entry in std::fs::read_dir(path).map_err(|source| PackageError::Io {
            path: path.to_path_buf(),
            source,
        })? {
            let entry = entry.map_err(|source| PackageError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            let child = entry.path();
            let metadata =
                std::fs::symlink_metadata(&child).map_err(|source| PackageError::Io {
                    path: child.clone(),
                    source,
                })?;
            if metadata.file_type().is_symlink() {
                return Err(PackageError::Authentication);
            }
            if metadata.is_dir() {
                visit(&child)?;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = if metadata.is_dir() { 0o500 } else { 0o400 };
                std::fs::set_permissions(&child, std::fs::Permissions::from_mode(mode)).map_err(
                    |source| PackageError::Io {
                        path: child,
                        source,
                    },
                )?;
            }
        }
        Ok(())
    }
    visit(root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o500)).map_err(
            |source| PackageError::Io {
                path: root.to_path_buf(),
                source,
            },
        )?;
    }
    Ok(())
}

/// Create a private directory with mode 0700 on Unix.
pub fn create_private_dir(path: &Path) -> Result<(), PackageError> {
    std::fs::create_dir_all(path).map_err(|source| PackageError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(
            |source| PackageError::Io {
                path: path.to_path_buf(),
                source,
            },
        )?;
    }
    Ok(())
}

/// Create a new file exclusively with mode 0600 on Unix.
pub fn private_new_file(path: &Path) -> Result<File, PackageError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path).map_err(|source| PackageError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Atomically write content-addressed bytes to `path` once. If `path` already exists,
/// verify its contents match `bytes`.
pub fn atomic_write_once(path: &Path, bytes: &[u8]) -> Result<(), PackageError> {
    if path.exists() {
        let existing = std::fs::read(path).map_err(|source| PackageError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if existing == bytes {
            return Ok(());
        }
        return Err(PackageError::Authentication);
    }
    let mut file = private_new_file(path)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| PackageError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    sync_directory(path.parent().expect("content-addressed file has parent"))
}

/// Atomically replace `path` with `bytes` using a temporary file and sync.
pub fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), PackageError> {
    let parent = path
        .parent()
        .ok_or_else(|| PackageError::Invalid("record has no parent".into()))?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::now_v7()));
    let mut file = private_new_file(&temporary)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| PackageError::Io {
            path: temporary.clone(),
            source,
        })?;
    std::fs::rename(&temporary, path).map_err(|source| PackageError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    sync_directory(parent)
}

/// Sync a directory to disk.
pub fn sync_directory(path: &Path) -> Result<(), PackageError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| PackageError::Io {
            path: path.to_path_buf(),
            source,
        })
}
