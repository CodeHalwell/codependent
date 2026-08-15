//! File indexer and fuzzy matcher for workspace file path search (Adoption 11 M2).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ignore::WalkBuilder;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

const CACHE_TTL: Duration = Duration::from_secs(30);
const MAX_ENTRIES: usize = 50_000;

#[derive(Debug, Clone)]
struct CachedWalk {
    paths: Arc<Vec<String>>,
    built_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatch {
    pub path: String,
    pub indices: Vec<u32>,
    pub score: u32,
}

#[derive(Debug, Default)]
pub struct FileIndex {
    roots: Mutex<HashMap<PathBuf, CachedWalk>>,
}

impl FileIndex {
    pub fn new() -> Self {
        Self {
            roots: Mutex::new(HashMap::new()),
        }
    }

    /// Walk `root` with `ignore::WalkBuilder` (gitignore on, hidden off, max 50_000 entries),
    /// returning relative UTF-8 paths.
    fn walk_root(root: &Path) -> Vec<String> {
        let mut builder = WalkBuilder::new(root);
        builder
            .standard_filters(true)
            .hidden(false)
            .require_git(false)
            .parents(true)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true);

        let mut paths = Vec::new();
        for entry in builder.build().flatten() {
            if entry.file_type().is_some_and(|ft| ft.is_file()) {
                if let Ok(rel) = entry.path().strip_prefix(root) {
                    if let Some(s) = rel.to_str() {
                        if !s.is_empty() {
                            paths.push(s.to_string());
                            if paths.len() >= MAX_ENTRIES {
                                break;
                            }
                        }
                    }
                }
            }
        }
        paths.sort();
        paths
    }

    fn get_or_walk(&self, root: &Path) -> Arc<Vec<String>> {
        let mut map = self.roots.lock().unwrap();
        if let Some(cached) = map.get(root) {
            if cached.built_at.elapsed() < CACHE_TTL {
                return cached.paths.clone();
            }
        }
        let paths = Arc::new(Self::walk_root(root));
        map.insert(
            root.to_path_buf(),
            CachedWalk {
                paths: paths.clone(),
                built_at: Instant::now(),
            },
        );
        paths
    }

    /// Pre-warm the cache in the background.
    pub fn prewarm(self: &Arc<Self>, root: PathBuf) {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let _ = this.get_or_walk(&root);
        });
    }

    /// Query the index with fuzzy matching.
    pub async fn query(&self, root: &Path, query: &str, limit: usize) -> (Vec<FileMatch>, bool) {
        let root = root.to_path_buf();
        let query = query.to_string();
        let paths = self.get_or_walk(&root);
        let truncated = paths.len() >= MAX_ENTRIES;

        tokio::task::spawn_blocking(move || {
            if query.trim().is_empty() {
                let matches = paths
                    .iter()
                    .take(limit)
                    .map(|p| FileMatch {
                        path: p.clone(),
                        indices: Vec::new(),
                        score: 0,
                    })
                    .collect();
                return (matches, truncated);
            }

            let mut matcher = Matcher::new(Config::DEFAULT);
            let pattern = Pattern::parse(&query, CaseMatching::Smart, Normalization::Smart);

            let mut scored: Vec<FileMatch> = Vec::new();
            let mut indices_buf = Vec::new();

            for path in paths.iter() {
                let mut buf = Vec::new();
                let haystack = Utf32Str::new(path, &mut buf);
                indices_buf.clear();
                if let Some(score) = pattern.indices(haystack, &mut matcher, &mut indices_buf) {
                    scored.push(FileMatch {
                        path: path.clone(),
                        indices: indices_buf.clone(),
                        score,
                    });
                }
            }

            // Score descending, tie-break shorter path first, then lexicographical
            scored.sort_by(|a, b| {
                b.score
                    .cmp(&a.score)
                    .then_with(|| a.path.len().cmp(&b.path.len()))
                    .then_with(|| a.path.cmp(&b.path))
            });

            scored.truncate(limit);
            (scored, truncated)
        })
        .await
        .unwrap_or_else(|_| (Vec::new(), false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fuzzy_search_matches_and_ranks_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("crates/tui/src")).unwrap();
        std::fs::write(root.join("crates/tui/src/palette.rs"), "fn foo() {}").unwrap();
        std::fs::write(root.join("crates/tui/src/render.rs"), "fn bar() {}").unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]").unwrap();

        let index = Arc::new(FileIndex::new());
        let (matches, truncated) = index.query(root, "pal", 10).await;

        assert!(!truncated);
        assert!(!matches.is_empty());
        assert_eq!(matches[0].path, "crates/tui/src/palette.rs");
        assert!(!matches[0].indices.is_empty());
    }

    #[tokio::test]
    async fn prewarm_caches_results_without_re_walking() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();

        let index = Arc::new(FileIndex::new());
        index.prewarm(root.to_path_buf());

        // Yield to allow background task to run
        tokio::time::sleep(Duration::from_millis(50)).await;

        let (matches, _) = index.query(root, "", 10).await;
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].path, "main.rs");
    }
}
