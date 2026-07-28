//! `workspace.write_file` — whole-file create-or-overwrite, confined to the
//! run's granted write scope (the run's disposable worktree).
//!
//! This is the structured-argument alternative to `git.apply_patch`
//! (`crates/runtime/src/tools/git.rs`) for a new file or a small full rewrite: the
//! model supplies `{path, content}` directly instead of reproducing an exact
//! unified diff, which a weak model is poor at getting byte-perfect.
//!
//! ## Containment (the security boundary)
//!
//! [`PathScope::resolve`] canonicalizes `path` and classifies that *same*
//! resolved path in one call — the no-TOCTOU seam [`ReadFile`](super::ReadFile)
//! also relies on (see `read_file.rs`'s module docs). [`WriteFile::execute`]
//! then acts on exactly that resolved [`PathBuf`], never a re-derived one, so a
//! traversal (`../..`) or a symlinked *ancestor* cannot redirect the write out
//! of scope.
//!
//! A symlink (or any other non-existent-target swap) planted *at the leaf*
//! between the resolve and the write is a separate race the scope check alone
//! cannot close (a leaf that does not yet exist at resolve time classifies as
//! `Allowed`, and nothing stops something being planted there before the write
//! runs). [`WriteFile::execute`] closes it directly: immediately before
//! writing, it takes `symlink_metadata` of the exact resolved path and refuses
//! ([`ToolError::NotRegularFile`]) if a symlink (or a directory) is there.
//! Write tools only ever write through a plain regular-file path.

use std::path::PathBuf;

use codypendent_daemon::policy::{PathScope, ScopeVerdict};
use serde_json::Value;

use super::{CapabilityKind, ToolError};

/// Typed input for [`WriteFile::execute`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteFileInput {
    /// The file to create or overwrite.
    pub path: PathBuf,
    /// The full new contents. Empty content is valid — it writes an empty
    /// file, it is not treated as "no content given".
    pub content: String,
}

/// The result of a successful `workspace.write_file` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteFileOutcome {
    /// The resolved path that was actually written — the same [`PathBuf`]
    /// [`PathScope::resolve`] classified, not a re-derived one.
    pub path: PathBuf,
    /// Bytes written (the byte length of `content`).
    pub bytes_written: u64,
    /// `true` when nothing existed at `path` before this call (a create);
    /// `false` when an existing regular file was truncated and overwritten.
    pub created: bool,
}

impl WriteFileOutcome {
    /// The honest, model- and user-facing observation: what actually
    /// happened, never a fabricated success. Exactly one of `created
    /// <path> (<n> bytes)` / `overwrote <path> (<n> bytes)`.
    #[must_use]
    pub fn observation(&self) -> String {
        let verb = if self.created { "created" } else { "overwrote" };
        format!(
            "{verb} {} ({} bytes)",
            self.path.display(),
            self.bytes_written
        )
    }
}

/// The `workspace.write_file` tool.
pub struct WriteFile;

impl WriteFile {
    /// The stable tool name.
    pub const NAME: &'static str = "workspace.write_file";

    /// Capability classes this tool draws on.
    pub fn required_capabilities() -> &'static [CapabilityKind] {
        &[CapabilityKind::FileWrite]
    }

    /// Create or overwrite `input.path` with `input.content`, confined to
    /// `scope`.
    ///
    /// Resolves `input.path` once via [`PathScope::resolve`] and acts only on
    /// the returned resolved path (no re-derivation, no TOCTOU gap on the
    /// resolved prefix). A verdict other than `Allowed` refuses without
    /// touching the filesystem. Immediately before writing, `symlink_metadata`
    /// on that same resolved path refuses a symlink or directory found there
    /// (the leaf-swap guard — see the module docs). Parent directories are
    /// created as needed, then the full contents are written in one
    /// `tokio::fs::write` (a truncating overwrite when the target already
    /// existed).
    pub async fn execute(
        input: &WriteFileInput,
        scope: &PathScope,
    ) -> Result<WriteFileOutcome, ToolError> {
        let (resolved, verdict) = scope.resolve(&input.path);
        match verdict {
            ScopeVerdict::Allowed => {}
            ScopeVerdict::Denied => return Err(ToolError::PathDenied(resolved)),
            ScopeVerdict::OutsideRoots => return Err(ToolError::PathOutOfScope(resolved)),
        }

        // Leaf guard + create-vs-overwrite detection from a single stat of
        // the exact resolved path — never re-derived, so this cannot be
        // fooled by something swapped in at a different path.
        let existed = match tokio::fs::symlink_metadata(&resolved).await {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || metadata.is_dir() {
                    return Err(ToolError::NotRegularFile(resolved));
                }
                true
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(ToolError::Io(e)),
        };

        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        tokio::fs::write(&resolved, input.content.as_bytes()).await?;

        Ok(WriteFileOutcome {
            bytes_written: input.content.len() as u64,
            created: !existed,
            path: resolved,
        })
    }
}

/// Parse `workspace.write_file` arguments. Both `path` and `content` are
/// required strings; an empty `content` is accepted (an empty file is a
/// legitimate target, not a malformed call).
pub fn parse_write_file(args: &Value) -> Result<WriteFileInput, String> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or("workspace.write_file requires a string `path`")?;
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .ok_or("workspace.write_file requires a string `content`")?;
    Ok(WriteFileInput {
        path: PathBuf::from(path),
        content: content.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    /// A scope rooted at `root` with no deny entries.
    fn scope_for(root: &std::path::Path) -> PathScope {
        PathScope::new(vec![root.to_path_buf()], vec![])
    }

    #[tokio::test]
    async fn creates_a_new_file_with_parent_dirs_auto_created() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = scope_for(&root);

        let input = WriteFileInput {
            path: root.join("nested/dir/new.txt"),
            content: "hello".to_string(),
        };
        let outcome = WriteFile::execute(&input, &scope).await.unwrap();

        assert!(outcome.created);
        assert_eq!(outcome.bytes_written, 5);
        assert_eq!(
            std::fs::read_to_string(root.join("nested/dir/new.txt")).unwrap(),
            "hello"
        );
        assert_eq!(
            outcome.observation(),
            format!("created {} (5 bytes)", outcome.path.display())
        );
    }

    #[tokio::test]
    async fn overwrites_an_existing_file() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = scope_for(&root);
        std::fs::write(root.join("existing.txt"), "old content").unwrap();

        let input = WriteFileInput {
            path: root.join("existing.txt"),
            content: "new".to_string(),
        };
        let outcome = WriteFile::execute(&input, &scope).await.unwrap();

        assert!(!outcome.created);
        assert_eq!(outcome.bytes_written, 3);
        assert_eq!(
            std::fs::read_to_string(root.join("existing.txt")).unwrap(),
            "new"
        );
        assert_eq!(
            outcome.observation(),
            format!("overwrote {} (3 bytes)", outcome.path.display())
        );
    }

    #[tokio::test]
    async fn empty_content_writes_an_empty_file() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = scope_for(&root);

        let input = WriteFileInput {
            path: root.join("empty.txt"),
            content: String::new(),
        };
        let outcome = WriteFile::execute(&input, &scope).await.unwrap();

        assert!(outcome.created);
        assert_eq!(outcome.bytes_written, 0);
        assert_eq!(
            std::fs::read(root.join("empty.txt")).unwrap(),
            Vec::<u8>::new()
        );
    }

    #[tokio::test]
    async fn relative_escape_is_denied_and_nothing_is_written() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(root.join("inside")).unwrap();
        let scope = PathScope::new(vec![root.join("inside")], vec![]);

        // `<root>/inside/../outside.txt` escapes the allowed root via `..`.
        let input = WriteFileInput {
            path: root.join("inside/../outside.txt"),
            content: "x".to_string(),
        };
        let err = WriteFile::execute(&input, &scope).await.unwrap_err();

        assert!(matches!(err, ToolError::PathOutOfScope(_)));
        assert!(!root.join("outside.txt").exists());
    }

    #[tokio::test]
    async fn absolute_path_outside_the_root_is_denied_and_nothing_is_written() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(root.join("inside")).unwrap();
        let scope = PathScope::new(vec![root.join("inside")], vec![]);

        let elsewhere = tempdir().unwrap();
        let outside = std::fs::canonicalize(elsewhere.path())
            .unwrap()
            .join("file.txt");

        let input = WriteFileInput {
            path: outside.clone(),
            content: "x".to_string(),
        };
        let err = WriteFile::execute(&input, &scope).await.unwrap_err();

        assert!(matches!(err, ToolError::PathOutOfScope(_)));
        assert!(!outside.exists());
    }

    #[tokio::test]
    async fn denied_subpath_is_refused_and_nothing_is_written() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(root.join("secret")).unwrap();
        let scope = PathScope::new(vec![root.clone()], vec![root.join("secret")]);

        let input = WriteFileInput {
            path: root.join("secret/file.txt"),
            content: "x".to_string(),
        };
        let err = WriteFile::execute(&input, &scope).await.unwrap_err();

        assert!(matches!(err, ToolError::PathDenied(_)));
        assert!(!root.join("secret/file.txt").exists());
    }

    #[tokio::test]
    async fn leaf_symlink_is_refused_and_nothing_is_written() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = scope_for(&root);

        // A dangling symlink planted at the write target: its own target
        // does not exist, so a strict `canonicalize` cannot follow through
        // it, and the lenient resolver falls back to the symlink's own path
        // (rather than silently substituting whatever it points at). This
        // mirrors the leaf-swap race the guard exists to close: a target
        // that does not yet exist at `resolve` time classifies as `Allowed`,
        // and something can still be planted at that exact path before the
        // write runs.
        let leaf = root.join("planted_link.txt");
        symlink(root.join("does_not_exist.txt"), &leaf).unwrap();

        let input = WriteFileInput {
            path: leaf.clone(),
            content: "pwned".to_string(),
        };
        let err = WriteFile::execute(&input, &scope).await.unwrap_err();

        assert!(matches!(err, ToolError::NotRegularFile(_)));
        // Nothing was written: the symlink itself is untouched and still
        // dangling (its target still does not exist).
        assert!(std::fs::symlink_metadata(&leaf)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!leaf.exists());
    }

    #[tokio::test]
    async fn leaf_directory_is_refused_and_nothing_is_written() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = scope_for(&root);
        std::fs::create_dir(root.join("a_directory")).unwrap();

        let input = WriteFileInput {
            path: root.join("a_directory"),
            content: "x".to_string(),
        };
        let err = WriteFile::execute(&input, &scope).await.unwrap_err();

        assert!(matches!(err, ToolError::NotRegularFile(_)));
        assert!(root.join("a_directory").is_dir());
    }

    #[test]
    fn parses_required_fields() {
        let args = serde_json::json!({"path": "src/lib.rs", "content": "fn main() {}"});
        let parsed = parse_write_file(&args).unwrap();
        assert_eq!(parsed.path, PathBuf::from("src/lib.rs"));
        assert_eq!(parsed.content, "fn main() {}");
    }

    #[test]
    fn parses_empty_content_as_valid() {
        let args = serde_json::json!({"path": "empty.txt", "content": ""});
        let parsed = parse_write_file(&args).unwrap();
        assert_eq!(parsed.content, "");
    }

    #[test]
    fn missing_path_is_rejected() {
        let args = serde_json::json!({"content": "x"});
        assert!(parse_write_file(&args).is_err());
    }

    #[test]
    fn missing_content_is_rejected() {
        let args = serde_json::json!({"path": "x.txt"});
        assert!(parse_write_file(&args).is_err());
    }

    #[test]
    fn non_string_path_is_rejected() {
        let args = serde_json::json!({"path": 42, "content": "x"});
        assert!(parse_write_file(&args).is_err());
    }
}
