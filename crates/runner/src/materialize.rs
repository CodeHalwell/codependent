//! Hostile input materialization and archive security validation.
//!
//! Enforces fail-closed defense against malicious archives:
//! 1. Absolute paths
//! 2. Parent directory traversal (`../`)
//! 3. Symlink escape (targets resolving outside workspace root)
//! 4. Hardlink escape
//! 5. Duplicate entry conflicts
//! 6. Expansion-ratio bombs
//! 7. Single-file and total-size overflows
//! 8. Undeclared entries not in the InputManifest
//! 9. Checksum mismatches against content-addressed hashes

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};
use tar::{Archive, EntryType};

use crate::types::{InputManifest, MaterializeError};

/// Resource and ratio limits for archive decompression and extraction.
#[derive(Debug, Clone)]
pub struct MaterializeLimits {
    pub max_file_size: u64,
    pub max_total_size: u64,
    pub max_files_count: usize,
    pub max_expansion_ratio: u64,
    pub min_compressed_for_ratio: u64,
}

impl Default for MaterializeLimits {
    fn default() -> Self {
        Self {
            max_file_size: 50 * 1024 * 1024,       // 50 MiB per file
            max_total_size: 200 * 1024 * 1024,     // 200 MiB total workspace
            max_files_count: 5000,                 // 5000 files max
            max_expansion_ratio: 100,              // 100:1 ratio ceiling
            min_compressed_for_ratio: 1024 * 1024, // Check ratio after 1 MiB uncompressed
        }
    }
}

/// The hostile input materializer.
pub struct Materializer {
    limits: MaterializeLimits,
}

impl Materializer {
    #[must_use]
    pub fn new(limits: MaterializeLimits) -> Self {
        Self { limits }
    }

    /// Unpack input archive bytes into `destination_dir`, validating against `manifest`.
    ///
    /// If validation fails, destination directory is cleaned up and a `MaterializeError` returned.
    pub fn materialize_bytes(
        &self,
        archive_bytes: &[u8],
        destination_dir: &Path,
        manifest: Option<&InputManifest>,
    ) -> Result<MaterializeReport, MaterializeError> {
        let compressed_size = archive_bytes.len() as u64;

        // Auto-detect gzip compression magic number (0x1f, 0x8b)
        let is_gzip =
            archive_bytes.len() >= 2 && archive_bytes[0] == 0x1f && archive_bytes[1] == 0x8b;

        let result = if is_gzip {
            let gz = GzDecoder::new(Cursor::new(archive_bytes));
            self.unpack_tar(gz, destination_dir, manifest, compressed_size)
        } else {
            self.unpack_tar(
                Cursor::new(archive_bytes),
                destination_dir,
                manifest,
                compressed_size,
            )
        };

        if result.is_err() {
            // Clean up any partially extracted files on failure
            let _ = cleanup_extracted_dir(destination_dir);
        }

        result
    }

    fn unpack_tar<R: Read>(
        &self,
        reader: R,
        destination_dir: &Path,
        manifest: Option<&InputManifest>,
        compressed_size: u64,
    ) -> Result<MaterializeReport, MaterializeError> {
        let mut archive = Archive::new(reader);
        let dest_canonical = destination_dir
            .canonicalize()
            .unwrap_or_else(|_| destination_dir.to_path_buf());

        fs::create_dir_all(&dest_canonical).map_err(|e| {
            MaterializeError::ArchiveFormat(format!("failed to create dest dir: {e}"))
        })?;

        let mut seen_paths: HashSet<String> = HashSet::new();
        let mut total_uncompressed_bytes: u64 = 0;
        let mut files_count: usize = 0;
        let mut extracted_entries = Vec::new();

        let entries = archive
            .entries()
            .map_err(|e| MaterializeError::ArchiveFormat(format!("malformed tar archive: {e}")))?;

        for entry_res in entries {
            let mut entry = entry_res
                .map_err(|e| MaterializeError::ArchiveFormat(format!("corrupt tar entry: {e}")))?;

            let raw_path = entry
                .path()
                .map_err(|e| MaterializeError::ArchiveFormat(format!("invalid entry path: {e}")))?
                .to_path_buf();

            let path_str = raw_path.to_string_lossy().to_string();

            // 1. Absolute path check
            if raw_path.is_absolute() || path_str.starts_with('/') || path_str.starts_with('\\') {
                return Err(MaterializeError::AbsolutePath(path_str));
            }

            // 2. Parent directory traversal check (..)
            for component in raw_path.components() {
                match component {
                    Component::ParentDir => {
                        return Err(MaterializeError::ParentTraversal(path_str));
                    }
                    Component::RootDir | Component::Prefix(_) => {
                        return Err(MaterializeError::AbsolutePath(path_str));
                    }
                    Component::CurDir | Component::Normal(_) => {}
                }
            }

            let normalized_rel = normalize_relative_path(&raw_path);
            if normalized_rel.is_empty() {
                continue;
            }

            // 5. Duplicate entry conflict check
            if !seen_paths.insert(normalized_rel.clone()) {
                return Err(MaterializeError::DuplicateEntry(normalized_rel));
            }

            // Check file count limit
            files_count += 1;
            if files_count > self.limits.max_files_count {
                return Err(MaterializeError::TotalSizeOverflow {
                    total: files_count as u64,
                    limit: self.limits.max_files_count as u64,
                });
            }

            let entry_type = entry.header().entry_type();
            let entry_size = entry.header().size().unwrap_or(0);

            // 7. Single file size overflow check
            if entry_size > self.limits.max_file_size {
                return Err(MaterializeError::SizeOverflow {
                    path: normalized_rel,
                    size: entry_size,
                    limit: self.limits.max_file_size,
                });
            }

            let target_path = dest_canonical.join(&normalized_rel);

            match entry_type {
                EntryType::Regular | EntryType::Continuous => {
                    // 8. Undeclared entry check (if manifest is provided)
                    let manifest_entry = if let Some(m) = manifest {
                        let me = m.find_entry(&normalized_rel).ok_or_else(|| {
                            MaterializeError::UndeclaredEntry(normalized_rel.clone())
                        })?;
                        Some(me)
                    } else {
                        None
                    };

                    // Ensure parent directory exists
                    if let Some(parent) = target_path.parent() {
                        fs::create_dir_all(parent).map_err(|e| {
                            MaterializeError::ArchiveFormat(format!("failed to create dir: {e}"))
                        })?;
                    }

                    // Read content safely, enforcing size limits and computing hash
                    let mut file_bytes = Vec::with_capacity(entry_size as usize);
                    let mut hasher = Sha256::new();
                    let mut buffer = [0u8; 8192];

                    loop {
                        let n = entry.read(&mut buffer).map_err(|e| {
                            MaterializeError::ArchiveFormat(format!("error reading entry: {e}"))
                        })?;
                        if n == 0 {
                            break;
                        }

                        total_uncompressed_bytes += n as u64;

                        // 7. Total size overflow
                        if total_uncompressed_bytes > self.limits.max_total_size {
                            return Err(MaterializeError::TotalSizeOverflow {
                                total: total_uncompressed_bytes,
                                limit: self.limits.max_total_size,
                            });
                        }

                        // 6. Expansion ratio bomb
                        if compressed_size > 0
                            && total_uncompressed_bytes > self.limits.min_compressed_for_ratio
                            && total_uncompressed_bytes
                                > compressed_size * self.limits.max_expansion_ratio
                        {
                            return Err(MaterializeError::ExpansionBomb {
                                compressed: compressed_size,
                                uncompressed: total_uncompressed_bytes,
                            });
                        }

                        hasher.update(&buffer[..n]);
                        file_bytes.extend_from_slice(&buffer[..n]);
                    }

                    let actual_hash = format!("sha256:{}", hex::encode(hasher.finalize()));

                    // 9. Checksum mismatch check
                    if let Some(me) = manifest_entry {
                        if !me.content_hash.eq_ignore_ascii_case(&actual_hash) {
                            return Err(MaterializeError::ChecksumMismatch {
                                path: normalized_rel,
                                expected: me.content_hash.clone(),
                                actual: actual_hash,
                            });
                        }
                    }

                    // Write file to disk
                    let mut out_file = File::create(&target_path).map_err(|e| {
                        MaterializeError::ArchiveFormat(format!("failed to create file: {e}"))
                    })?;
                    out_file.write_all(&file_bytes).map_err(|e| {
                        MaterializeError::ArchiveFormat(format!("failed to write file: {e}"))
                    })?;

                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let mode = if let Some(me) = manifest_entry {
                            if me.executable {
                                0o755
                            } else {
                                me.mode
                            }
                        } else {
                            entry.header().mode().unwrap_or(0o644)
                        };
                        let _ = fs::set_permissions(&target_path, fs::Permissions::from_mode(mode));
                    }

                    extracted_entries.push(ExtractedFile {
                        path: normalized_rel,
                        hash: actual_hash,
                        byte_length: file_bytes.len() as u64,
                    });
                }
                EntryType::Directory => {
                    fs::create_dir_all(&target_path).map_err(|e| {
                        MaterializeError::ArchiveFormat(format!("failed to create dir: {e}"))
                    })?;
                }
                EntryType::Symlink => {
                    let target = entry
                        .link_name()
                        .map_err(|e| {
                            MaterializeError::ArchiveFormat(format!("invalid symlink: {e}"))
                        })?
                        .unwrap_or_default();
                    let target_str = target.to_string_lossy().to_string();

                    // 3. Symlink escape check
                    if target.is_absolute()
                        || target_str.starts_with('/')
                        || target_str.starts_with('\\')
                    {
                        return Err(MaterializeError::SymlinkEscape { target: target_str });
                    }

                    // Check if relative target escapes the destination root
                    let parent_dir = target_path.parent().unwrap_or(&dest_canonical);
                    let resolved = parent_dir.join(&target);
                    let normalized_resolved = normalize_path(&resolved);

                    if !normalized_resolved.starts_with(&dest_canonical) {
                        return Err(MaterializeError::SymlinkEscape { target: target_str });
                    }

                    // Create parent directory
                    if let Some(parent) = target_path.parent() {
                        fs::create_dir_all(parent).map_err(|e| {
                            MaterializeError::ArchiveFormat(format!("failed to create dir: {e}"))
                        })?;
                    }

                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::symlink;
                        let _ = symlink(&target, &target_path);
                    }
                }
                EntryType::Link => {
                    let target = entry
                        .link_name()
                        .map_err(|e| {
                            MaterializeError::ArchiveFormat(format!("invalid hardlink: {e}"))
                        })?
                        .unwrap_or_default();
                    let target_str = target.to_string_lossy().to_string();

                    // 4. Hardlink escape check
                    if target.is_absolute()
                        || target_str.starts_with('/')
                        || target_str.starts_with('\\')
                    {
                        return Err(MaterializeError::HardlinkEscape { target: target_str });
                    }

                    let resolved = dest_canonical.join(&target);
                    let normalized_resolved = normalize_path(&resolved);

                    if !normalized_resolved.starts_with(&dest_canonical) {
                        return Err(MaterializeError::HardlinkEscape { target: target_str });
                    }

                    if let Some(parent) = target_path.parent() {
                        fs::create_dir_all(parent).map_err(|e| {
                            MaterializeError::ArchiveFormat(format!("failed to create dir: {e}"))
                        })?;
                    }

                    let _ = fs::hard_link(&normalized_resolved, &target_path);
                }
                _ => {
                    // Ignore or skip unsupported non-standard entries safely
                }
            }
        }

        // If manifest was provided, verify all declared entries were present
        if let Some(m) = manifest {
            for expected in &m.entries {
                if !seen_paths.contains(&expected.path) {
                    return Err(MaterializeError::MissingDeclaredEntry(
                        expected.path.clone(),
                    ));
                }
            }
        }

        Ok(MaterializeReport {
            files: extracted_entries,
            total_uncompressed_bytes,
        })
    }
}

/// Information about successfully extracted files.
#[derive(Debug, Clone)]
pub struct ExtractedFile {
    pub path: String,
    pub hash: String,
    pub byte_length: u64,
}

/// Report after successful materialization.
#[derive(Debug, Clone)]
pub struct MaterializeReport {
    pub files: Vec<ExtractedFile>,
    pub total_uncompressed_bytes: u64,
}

fn normalize_relative_path(path: &Path) -> String {
    let mut components = Vec::new();
    for c in path.components() {
        if let Component::Normal(s) = c {
            components.push(s.to_string_lossy().to_string());
        }
    }
    components.join("/")
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            _ => out.push(comp),
        }
    }
    out
}

fn cleanup_extracted_dir(dir: &Path) -> std::io::Result<()> {
    if dir.exists() {
        // Reset permissions recursively on unix so read-only files can be removed
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
        }
        fs::remove_dir_all(dir)?;
    }
    Ok(())
}
