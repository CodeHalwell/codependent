//! `workspace.read_file` — a line-numbered excerpt of a file, confined to the
//! granted read scope.

use std::path::PathBuf;

use codypendent_daemon::policy::PathScope;
use codypendent_protocol::ProposedAction;

use super::{secure_fs, CapabilityKind, ToolError};

/// Default line ceiling when no explicit range is requested.
const DEFAULT_MAX_LINES: usize = 200;
/// Upper bound on the bytes the reader will produce, so a single pathological
/// line (e.g. a minified multi-hundred-MB file) can never be buffered whole.
/// Far larger than any real source file; content beyond it is not read.
const MAX_READ_BYTES: u64 = 64 * 1024 * 1024;

/// Typed input for [`ReadFile::execute`].
#[derive(Debug, Clone)]
pub struct ReadFileInput {
    /// The file to read.
    pub path: PathBuf,
    /// An optional inclusive 1-based `(start, end)` line range. When absent, the
    /// first [`DEFAULT_MAX_LINES`] lines are returned.
    pub range: Option<(usize, usize)>,
}

/// A line-numbered excerpt of a file.
#[derive(Debug, Clone)]
pub struct FileExcerpt {
    /// The file the excerpt came from.
    pub path: PathBuf,
    /// First line included (1-based).
    pub start_line: usize,
    /// Last line included (1-based, inclusive).
    pub end_line: usize,
    /// Total lines in the file.
    pub total_lines: usize,
    /// Whether the file has content beyond the returned excerpt.
    pub truncated: bool,
    /// The excerpt, each line prefixed with its 1-based number.
    pub content: String,
}

/// The `workspace.read_file` tool.
pub struct ReadFile;

impl ReadFile {
    /// The stable tool name.
    pub const NAME: &'static str = "workspace.read_file";

    /// Capability classes this tool draws on.
    pub fn required_capabilities() -> &'static [CapabilityKind] {
        &[CapabilityKind::FileRead]
    }

    /// The [`ProposedAction`] the middleware evaluates before granting.
    pub fn proposed_action(input: &ReadFileInput) -> ProposedAction {
        ProposedAction::ReadFiles {
            paths: vec![input.path.to_string_lossy().into_owned()],
        }
    }

    /// Read an excerpt of `input.path`, refusing any path outside `scope`.
    ///
    /// The path is canonicalized *once*, the scope check runs on that resolved
    /// path, and the very same resolved path is then opened and streamed — so a
    /// traversal or a symlink swapped in between the check and the open cannot
    /// redirect the read out of scope (no TOCTOU gap). The file is read line by
    /// line through a [`tokio::io::BufReader`], retaining only the excerpt window
    /// in memory, so an enormous file is never buffered whole. At most
    /// [`DEFAULT_MAX_LINES`] lines are returned unless an explicit range is given.
    pub async fn execute(
        input: &ReadFileInput,
        scope: &PathScope,
    ) -> Result<FileExcerpt, ToolError> {
        use tokio::io::AsyncBufReadExt;

        // Open descriptor-relative to the authorized root, refusing symlinks at
        // every component. Subsequent reads use this handle, never the pathname.
        let path = input.path.clone();
        let scope_for_open = scope.clone();
        let scoped_res =
            tokio::task::spawn_blocking(move || secure_fs::open_read(&path, &scope_for_open))
                .await
                .map_err(|error| {
                    ToolError::Other(anyhow::anyhow!("read worker failed: {error}"))
                })?;

        let scoped = match scoped_res {
            Ok(s) => s,
            Err(ToolError::Io(ref err)) if err.kind() == std::io::ErrorKind::NotFound => {
                let p = input.path.clone();
                let s = scope.clone();
                let suggestions = tokio::task::spawn_blocking(move || did_you_mean(&p, &s))
                    .await
                    .unwrap_or_default();
                return Err(ToolError::FileNotFound {
                    path: input.path.clone(),
                    suggestions,
                });
            }
            Err(other) => return Err(other),
        };

        // Validate an explicit range before touching the file (unchanged errors).
        if let Some((start, end)) = input.range {
            if start == 0 {
                return Err(ToolError::InvalidRange {
                    start,
                    end,
                    reason: "line numbers are 1-based".to_string(),
                });
            }
            if end < start {
                return Err(ToolError::InvalidRange {
                    start,
                    end,
                    reason: "end precedes start".to_string(),
                });
            }
        }

        // The inclusive window we retain: the requested span, or the first
        // DEFAULT_MAX_LINES lines by default. Only these lines are held in memory.
        let (want_start, want_end) = match input.range {
            Some((start, end)) => (start, end),
            None => (1, DEFAULT_MAX_LINES),
        };

        // Refuse non-regular files: a FIFO/device inside the scope would block
        // the read forever (a pipe never reaches EOF while a writer can appear).
        let metadata = scoped.file.metadata()?;
        if !metadata.is_file() {
            return Err(ToolError::Other(anyhow::anyhow!(
                "not a regular file: {}",
                scoped.path.display()
            )));
        }

        // Stream line by line, keeping only the window and counting the total, so
        // the excerpt semantics (total_lines, truncation) stay exact without the
        // whole file ever residing in memory. The reader is byte-bounded so one
        // enormous newline-free line cannot be buffered whole either.
        let file = tokio::fs::File::from_std(scoped.file);
        let bounded = tokio::io::AsyncReadExt::take(file, MAX_READ_BYTES);
        let mut lines = tokio::io::BufReader::new(bounded).lines();
        let mut total = 0usize;
        let mut window: Vec<String> = Vec::new();
        while let Some(line) = lines.next_line().await? {
            total += 1;
            if total >= want_start && total <= want_end {
                window.push(line);
            }
            // Past the window we only keep counting (nothing is retained).
        }

        let (start, end) = if total == 0 || want_start > total {
            (0, 0)
        } else {
            (want_start, want_end.min(total))
        };

        // Emit the retained lines whose absolute number falls in [start, end].
        // The window's first entry is line `want_start`.
        let mut content = String::new();
        if start > 0 {
            for (offset, line) in window.iter().enumerate() {
                let number = want_start + offset;
                if number >= start && number <= end {
                    content.push_str(&format!("{number:>6}\t{line}\n"));
                }
            }
        }

        Ok(FileExcerpt {
            path: input.path.clone(),
            start_line: start,
            end_line: end,
            total_lines: total,
            truncated: end < total || start > 1,
            content,
        })
    }
}

/// Up to 3 entries of `path`'s parent whose lowercase name contains the
/// requested leaf's lowercase name or vice versa. The parent is re-checked
/// against `scope` and read with std::fs::read_dir on the RESOLVED parent;
/// any error yields no suggestions (the not-found error stands alone).
fn did_you_mean(path: &std::path::Path, scope: &PathScope) -> Vec<String> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let parent_to_check = if parent.as_os_str().is_empty() {
        std::path::Path::new(".")
    } else {
        parent
    };
    let (resolved_parent, verdict) = scope.resolve(parent_to_check);
    if verdict != codypendent_daemon::policy::ScopeVerdict::Allowed {
        return Vec::new();
    }
    let Some(leaf) = path.file_name().and_then(|f| f.to_str()) else {
        return Vec::new();
    };
    let leaf_lower = leaf.to_lowercase();
    let Ok(entries) = std::fs::read_dir(&resolved_parent) else {
        return Vec::new();
    };

    let mut suggestions = Vec::new();
    for entry in entries.flatten() {
        if let Ok(file_name) = entry.file_name().into_string() {
            let name_lower = file_name.to_lowercase();
            if name_lower != leaf_lower && is_similar(&name_lower, &leaf_lower) {
                suggestions.push(file_name);
            }
        }
    }
    suggestions.sort();
    suggestions.truncate(3);
    suggestions
}

fn is_similar(a: &str, b: &str) -> bool {
    if a.contains(b) || b.contains(a) {
        return true;
    }
    levenshtein_distance(a, b) <= 2
}

fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();
    let mut dp = vec![vec![0; n + 1]; m + 1];

    for (i, row) in dp.iter_mut().enumerate().take(m + 1) {
        row[0] = i;
    }
    if let Some(first_row) = dp.first_mut() {
        for (j, cell) in first_row.iter_mut().enumerate().take(n + 1) {
            *cell = j;
        }
    }

    for i in 1..=m {
        for j in 1..=n {
            if a_chars[i - 1] == b_chars[j - 1] {
                dp[i][j] = dp[i - 1][j - 1];
            } else {
                dp[i][j] = 1 + dp[i - 1][j].min(dp[i][j - 1]).min(dp[i - 1][j - 1]);
            }
        }
    }
    dp[m][n]
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_daemon::policy::PathScope;

    #[tokio::test]
    async fn not_found_suggests_similar_sibling_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        let scope = PathScope::new(vec![root.clone()], vec![]);
        let err = ReadFile::execute(
            &ReadFileInput {
                path: root.join("src/mian.rs"),
                range: None,
            },
            &scope,
        )
        .await
        .unwrap_err();

        match err {
            ToolError::FileNotFound {
                ref path,
                ref suggestions,
            } => {
                assert_eq!(path, &root.join("src/mian.rs"));
                assert_eq!(suggestions, &vec!["main.rs".to_string()]);
                let msg = err.to_string();
                assert!(msg.contains("file not found:"));
                assert!(msg.contains("— did you mean: main.rs?"));
            }
            other => panic!("expected FileNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn not_found_out_of_scope_yields_no_suggestions() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_root = outside.path().canonicalize().unwrap();
        std::fs::write(outside_root.join("main.rs"), "fn main() {}").unwrap();

        let scope = PathScope::new(vec![root], vec![]);
        let err = ReadFile::execute(
            &ReadFileInput {
                path: outside_root.join("mian.rs"),
                range: None,
            },
            &scope,
        )
        .await
        .unwrap_err();

        // Either path out of scope or file not found without suggestions
        match err {
            ToolError::PathOutOfScope(_) => {}
            ToolError::FileNotFound { suggestions, .. } => {
                assert!(suggestions.is_empty());
            }
            other => panic!("unexpected error {other:?}"),
        }
    }
}
