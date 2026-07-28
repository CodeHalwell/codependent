//! `workspace.edit_file` — targeted, exact search/replace edits confined to
//! the run's granted write scope (the run's disposable worktree).
//!
//! This is the structured-argument alternative to `git.apply_patch`
//! (`crates/runtime/src/tools/git.rs`) for a **targeted** change to an
//! existing file: the model supplies one or more `{search, replace}` pairs
//! instead of reproducing an exact unified diff (context lines and `@@`
//! offsets a weak model is poor at getting byte-perfect). Robust for editing
//! large files without reproducing them whole.
//!
//! ## Containment (the security boundary)
//!
//! Mirrors [`WriteFile`](super::WriteFile) exactly: [`PathScope::resolve`]
//! canonicalizes `path` and classifies that *same* resolved path in one call
//! (no-TOCTOU seam), and [`EditFile::execute`] acts only on that resolved
//! [`PathBuf`]. Immediately before reading (and again the guard is checked
//! only once, since nothing else touches the path in between), a
//! `symlink_metadata` of the exact resolved path refuses a symlink or
//! directory found there (the leaf-swap guard).
//!
//! ## Match semantics — exact, unique, sequential, atomic
//!
//! See [`EditFile::execute`] for the full contract: each `search` must match
//! **exactly once** in the buffer as it stands *after* all earlier edits have
//! been applied; a failure to match uniquely fails the whole call and writes
//! nothing.

use std::path::PathBuf;

use codypendent_daemon::policy::{PathScope, ScopeVerdict};
use serde_json::Value;

use super::{CapabilityKind, ToolError};

/// Largest file `edit_file` will hold in memory to search. Mirrors
/// `read_file.rs`'s `MAX_READ_BYTES` ceiling — far larger than any real
/// source file; a file beyond this is refused rather than silently
/// truncated (truncating could corrupt a match that spans the cut).
const MAX_EDIT_BYTES: u64 = 64 * 1024 * 1024;

/// A single search/replace pair within a `workspace.edit_file` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdit {
    /// The exact literal text to find. Must be non-empty.
    pub search: String,
    /// The text to substitute in place of the single matched occurrence.
    pub replace: String,
}

/// Typed input for [`EditFile::execute`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditFileInput {
    /// The file to edit. Must already exist (new files use `workspace.write_file`).
    pub path: PathBuf,
    /// One or more edits, applied in order. Must be non-empty.
    pub edits: Vec<FileEdit>,
}

/// The result of a successful `workspace.edit_file` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditFileOutcome {
    /// The resolved path that was actually written — the same [`PathBuf`]
    /// [`PathScope::resolve`] classified, not a re-derived one.
    pub path: PathBuf,
    /// The number of edits applied (== `input.edits.len()` on success, since
    /// the call is all-or-nothing).
    pub edits_applied: usize,
}

impl EditFileOutcome {
    /// The honest, model- and user-facing observation: what actually
    /// happened, never a fabricated success.
    #[must_use]
    pub fn observation(&self) -> String {
        format!(
            "applied {} edit(s) to {}",
            self.edits_applied,
            self.path.display()
        )
    }
}

/// The `workspace.edit_file` tool.
pub struct EditFile;

impl EditFile {
    /// The stable tool name.
    pub const NAME: &'static str = "workspace.edit_file";

    /// Capability classes this tool draws on.
    pub fn required_capabilities() -> &'static [CapabilityKind] {
        &[CapabilityKind::FileWrite]
    }

    /// Apply `input.edits` to `input.path` in order, confined to `scope`.
    ///
    /// Resolves `input.path` once via [`PathScope::resolve`] and acts only on
    /// the returned resolved path (mirrors [`WriteFile::execute`](super::WriteFile::execute)).
    /// A verdict other than `Allowed` refuses without touching the
    /// filesystem. Before reading, `symlink_metadata` on that same resolved
    /// path refuses a symlink or directory (the leaf-swap guard). The whole
    /// file is read into memory, bounded at [`MAX_EDIT_BYTES`] — a larger
    /// file fails with [`ToolError::FileTooLarge`] rather than being
    /// silently truncated.
    ///
    /// Edits are then applied **sequentially against the evolving buffer**:
    /// edit `i+1` matches against the buffer *after* edit `i` was applied,
    /// so a later edit can target text a prior edit just produced. For each
    /// edit, `search` must appear as an exact, non-overlapping substring
    /// **exactly once** in the current buffer:
    /// - empty `search` → [`ToolError::EmptySearch`] (checked before matching);
    /// - `0` occurrences → [`ToolError::SearchNotFound`];
    /// - `>1` occurrences → [`ToolError::SearchAmbiguous`];
    /// - exactly `1` → the occurrence is replaced with `replace`.
    ///
    /// The whole sequence is computed **in memory**; the filesystem is
    /// touched only once, via a single `tokio::fs::write`, and only after
    /// every edit has matched uniquely. If any edit fails, `execute` returns
    /// that edit's error (the first/lowest-index failure) and **writes
    /// nothing** — no partial edit can ever land on disk.
    pub async fn execute(
        input: &EditFileInput,
        scope: &PathScope,
    ) -> Result<EditFileOutcome, ToolError> {
        let (resolved, verdict) = scope.resolve(&input.path);
        match verdict {
            ScopeVerdict::Allowed => {}
            ScopeVerdict::Denied => return Err(ToolError::PathDenied(resolved)),
            ScopeVerdict::OutsideRoots => return Err(ToolError::PathOutOfScope(resolved)),
        }

        // Leaf guard on the exact resolved path — never re-derived.
        let metadata = tokio::fs::symlink_metadata(&resolved).await?;
        if metadata.file_type().is_symlink() || metadata.is_dir() {
            return Err(ToolError::NotRegularFile(resolved));
        }
        if metadata.len() > MAX_EDIT_BYTES {
            return Err(ToolError::FileTooLarge {
                path: resolved,
                cap: MAX_EDIT_BYTES,
            });
        }

        let bytes = tokio::fs::read(&resolved).await?;
        let mut buffer = String::from_utf8(bytes).map_err(|e| {
            ToolError::Other(anyhow::anyhow!(
                "{}: file is not valid UTF-8: {e}",
                resolved.display()
            ))
        })?;

        // Compute the full result in memory; nothing is written until every
        // edit has matched uniquely (atomicity).
        for (zero_based, edit) in input.edits.iter().enumerate() {
            let index = zero_based + 1;
            if edit.search.is_empty() {
                return Err(ToolError::EmptySearch { index });
            }
            let count = buffer.matches(edit.search.as_str()).count();
            match count {
                0 => {
                    return Err(ToolError::SearchNotFound {
                        path: resolved,
                        index,
                    })
                }
                1 => {
                    let pos = buffer.find(edit.search.as_str()).expect("count == 1");
                    buffer.replace_range(pos..pos + edit.search.len(), &edit.replace);
                }
                n => {
                    return Err(ToolError::SearchAmbiguous {
                        path: resolved,
                        index,
                        count: n,
                    })
                }
            }
        }

        tokio::fs::write(&resolved, buffer.as_bytes()).await?;

        Ok(EditFileOutcome {
            edits_applied: input.edits.len(),
            path: resolved,
        })
    }
}

/// Parse `workspace.edit_file` arguments: `path` (required string) and
/// `edits` (required non-empty array of `{search, replace}` objects, both
/// required strings; `search` must be non-empty).
pub fn parse_edit_file(args: &Value) -> Result<EditFileInput, String> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or("workspace.edit_file requires a string `path`")?;
    let edits_value = args
        .get("edits")
        .and_then(Value::as_array)
        .ok_or("workspace.edit_file requires an array `edits`")?;
    if edits_value.is_empty() {
        return Err("workspace.edit_file requires at least one edit".to_string());
    }

    let mut edits = Vec::with_capacity(edits_value.len());
    for (zero_based, entry) in edits_value.iter().enumerate() {
        let index = zero_based + 1;
        let search = entry
            .get("search")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("edit {index}: requires a string `search`"))?;
        if search.is_empty() {
            return Err(format!("edit {index}: search text must not be empty"));
        }
        let replace = entry
            .get("replace")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("edit {index}: requires a string `replace`"))?;
        edits.push(FileEdit {
            search: search.to_string(),
            replace: replace.to_string(),
        });
    }

    Ok(EditFileInput {
        path: PathBuf::from(path),
        edits,
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
    async fn unique_match_is_replaced() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = scope_for(&root);
        std::fs::write(root.join("f.txt"), "hello world").unwrap();

        let input = EditFileInput {
            path: root.join("f.txt"),
            edits: vec![FileEdit {
                search: "world".to_string(),
                replace: "there".to_string(),
            }],
        };
        let outcome = EditFile::execute(&input, &scope).await.unwrap();

        assert_eq!(outcome.edits_applied, 1);
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "hello there"
        );
        assert_eq!(
            outcome.observation(),
            format!("applied 1 edit(s) to {}", outcome.path.display())
        );
    }

    #[tokio::test]
    async fn search_not_found_leaves_file_unchanged() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = scope_for(&root);
        std::fs::write(root.join("f.txt"), "hello world").unwrap();

        let input = EditFileInput {
            path: root.join("f.txt"),
            edits: vec![FileEdit {
                search: "nope".to_string(),
                replace: "x".to_string(),
            }],
        };
        let err = EditFile::execute(&input, &scope).await.unwrap_err();

        match &err {
            ToolError::SearchNotFound { index, .. } => assert_eq!(*index, 1),
            other => panic!("expected SearchNotFound, got {other:?}"),
        }
        assert_eq!(
            err.to_string(),
            format!(
                "edit 1: search text not found in {}",
                root.join("f.txt").display()
            )
        );
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "hello world"
        );
    }

    #[tokio::test]
    async fn ambiguous_match_leaves_file_unchanged() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = scope_for(&root);
        std::fs::write(root.join("f.txt"), "aa bb aa").unwrap();

        let input = EditFileInput {
            path: root.join("f.txt"),
            edits: vec![FileEdit {
                search: "aa".to_string(),
                replace: "cc".to_string(),
            }],
        };
        let err = EditFile::execute(&input, &scope).await.unwrap_err();

        match &err {
            ToolError::SearchAmbiguous { index, count, .. } => {
                assert_eq!(*index, 1);
                assert_eq!(*count, 2);
            }
            other => panic!("expected SearchAmbiguous, got {other:?}"),
        }
        assert_eq!(
            err.to_string(),
            format!(
                "edit 1: search text is ambiguous (2 matches) — include more surrounding context so it is unique"
            )
        );
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "aa bb aa"
        );
    }

    #[tokio::test]
    async fn empty_search_is_rejected() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = scope_for(&root);
        std::fs::write(root.join("f.txt"), "hello world").unwrap();

        let input = EditFileInput {
            path: root.join("f.txt"),
            edits: vec![FileEdit {
                search: String::new(),
                replace: "x".to_string(),
            }],
        };
        let err = EditFile::execute(&input, &scope).await.unwrap_err();

        assert!(matches!(err, ToolError::EmptySearch { index: 1 }));
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "hello world"
        );
    }

    #[tokio::test]
    async fn multiple_edits_apply_sequentially_against_the_evolving_buffer() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = scope_for(&root);
        std::fs::write(root.join("f.txt"), "hello world").unwrap();

        // Edit 2's search text ("there friend") does not exist in the
        // original file — it only exists after edit 1 has run. This proves
        // edits apply sequentially against the evolving buffer, not all
        // against the original.
        let input = EditFileInput {
            path: root.join("f.txt"),
            edits: vec![
                FileEdit {
                    search: "world".to_string(),
                    replace: "there friend".to_string(),
                },
                FileEdit {
                    search: "there friend".to_string(),
                    replace: "there, friend!".to_string(),
                },
            ],
        };
        let outcome = EditFile::execute(&input, &scope).await.unwrap();

        assert_eq!(outcome.edits_applied, 2);
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "hello there, friend!"
        );
    }

    #[tokio::test]
    async fn atomicity_a_later_failing_edit_leaves_the_file_byte_for_byte_unchanged() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = scope_for(&root);
        let original = "one two three";
        std::fs::write(root.join("f.txt"), original).unwrap();

        // Edit 1 would succeed in isolation (replacing "one" with "1"), but
        // edit 2 is ambiguous ("t" appears many times), so the whole call
        // must fail and edit 1 must NOT be persisted.
        let input = EditFileInput {
            path: root.join("f.txt"),
            edits: vec![
                FileEdit {
                    search: "one".to_string(),
                    replace: "1".to_string(),
                },
                FileEdit {
                    search: "t".to_string(),
                    replace: "T".to_string(),
                },
            ],
        };
        let err = EditFile::execute(&input, &scope).await.unwrap_err();

        assert!(matches!(err, ToolError::SearchAmbiguous { index: 2, .. }));
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            original,
            "edit 1 must not have been partially persisted"
        );
    }

    #[tokio::test]
    async fn relative_escape_is_denied_and_nothing_is_written() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(root.join("inside")).unwrap();
        std::fs::write(root.join("outside.txt"), "secret").unwrap();
        let scope = PathScope::new(vec![root.join("inside")], vec![]);

        let input = EditFileInput {
            path: root.join("inside/../outside.txt"),
            edits: vec![FileEdit {
                search: "secret".to_string(),
                replace: "pwned".to_string(),
            }],
        };
        let err = EditFile::execute(&input, &scope).await.unwrap_err();

        assert!(matches!(err, ToolError::PathOutOfScope(_)));
        assert_eq!(
            std::fs::read_to_string(root.join("outside.txt")).unwrap(),
            "secret"
        );
    }

    #[tokio::test]
    async fn denied_subpath_is_refused_and_nothing_is_written() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(root.join("secret")).unwrap();
        std::fs::write(root.join("secret/file.txt"), "classified").unwrap();
        let scope = PathScope::new(vec![root.clone()], vec![root.join("secret")]);

        let input = EditFileInput {
            path: root.join("secret/file.txt"),
            edits: vec![FileEdit {
                search: "classified".to_string(),
                replace: "leaked".to_string(),
            }],
        };
        let err = EditFile::execute(&input, &scope).await.unwrap_err();

        assert!(matches!(err, ToolError::PathDenied(_)));
        assert_eq!(
            std::fs::read_to_string(root.join("secret/file.txt")).unwrap(),
            "classified"
        );
    }

    #[tokio::test]
    async fn leaf_symlink_is_refused_and_nothing_is_written() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = scope_for(&root);

        let leaf = root.join("planted_link.txt");
        symlink(root.join("does_not_exist.txt"), &leaf).unwrap();

        let input = EditFileInput {
            path: leaf.clone(),
            edits: vec![FileEdit {
                search: "x".to_string(),
                replace: "y".to_string(),
            }],
        };
        let err = EditFile::execute(&input, &scope).await.unwrap_err();

        assert!(matches!(err, ToolError::NotRegularFile(_)));
        assert!(std::fs::symlink_metadata(&leaf)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[tokio::test]
    async fn leaf_directory_is_refused_and_nothing_is_written() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = scope_for(&root);
        std::fs::create_dir(root.join("a_directory")).unwrap();

        let input = EditFileInput {
            path: root.join("a_directory"),
            edits: vec![FileEdit {
                search: "x".to_string(),
                replace: "y".to_string(),
            }],
        };
        let err = EditFile::execute(&input, &scope).await.unwrap_err();

        assert!(matches!(err, ToolError::NotRegularFile(_)));
    }

    #[tokio::test]
    async fn missing_file_is_an_io_error() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = scope_for(&root);

        let input = EditFileInput {
            path: root.join("does_not_exist.txt"),
            edits: vec![FileEdit {
                search: "x".to_string(),
                replace: "y".to_string(),
            }],
        };
        let err = EditFile::execute(&input, &scope).await.unwrap_err();

        assert!(matches!(err, ToolError::Io(_)));
    }

    #[tokio::test]
    async fn file_over_the_cap_is_refused_and_nothing_is_written() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let scope = scope_for(&root);

        // Sparse file: seek past the cap and write one byte, so the file's
        // reported length exceeds MAX_EDIT_BYTES without actually writing
        // 64MiB of data to disk.
        let path = root.join("huge.bin");
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut file = std::fs::File::create(&path).unwrap();
            file.seek(SeekFrom::Start(MAX_EDIT_BYTES + 1)).unwrap();
            file.write_all(b"x").unwrap();
        }

        let input = EditFileInput {
            path: path.clone(),
            edits: vec![FileEdit {
                search: "x".to_string(),
                replace: "y".to_string(),
            }],
        };
        let err = EditFile::execute(&input, &scope).await.unwrap_err();

        match &err {
            ToolError::FileTooLarge { cap, .. } => assert_eq!(*cap, MAX_EDIT_BYTES),
            other => panic!("expected FileTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn parses_required_fields() {
        let args = serde_json::json!({
            "path": "src/lib.rs",
            "edits": [{"search": "foo", "replace": "bar"}]
        });
        let parsed = parse_edit_file(&args).unwrap();
        assert_eq!(parsed.path, PathBuf::from("src/lib.rs"));
        assert_eq!(parsed.edits.len(), 1);
        assert_eq!(parsed.edits[0].search, "foo");
        assert_eq!(parsed.edits[0].replace, "bar");
    }

    #[test]
    fn parses_multiple_edits() {
        let args = serde_json::json!({
            "path": "f.txt",
            "edits": [
                {"search": "a", "replace": "b"},
                {"search": "c", "replace": "d"}
            ]
        });
        let parsed = parse_edit_file(&args).unwrap();
        assert_eq!(parsed.edits.len(), 2);
    }

    #[test]
    fn missing_path_is_rejected() {
        let args = serde_json::json!({"edits": [{"search": "a", "replace": "b"}]});
        assert!(parse_edit_file(&args).is_err());
    }

    #[test]
    fn missing_edits_is_rejected() {
        let args = serde_json::json!({"path": "f.txt"});
        assert!(parse_edit_file(&args).is_err());
    }

    #[test]
    fn empty_edits_array_is_rejected() {
        let args = serde_json::json!({"path": "f.txt", "edits": []});
        assert!(parse_edit_file(&args).is_err());
    }

    #[test]
    fn empty_search_in_an_edit_is_rejected_at_parse_time() {
        let args = serde_json::json!({
            "path": "f.txt",
            "edits": [{"search": "", "replace": "b"}]
        });
        assert!(parse_edit_file(&args).is_err());
    }

    #[test]
    fn missing_replace_is_rejected() {
        let args = serde_json::json!({
            "path": "f.txt",
            "edits": [{"search": "a"}]
        });
        assert!(parse_edit_file(&args).is_err());
    }
}
