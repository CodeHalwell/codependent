//! The bounded code-graph warm-up scan, shared by startup and per-run launch.
//!
//! Session attach and run launch warm a checkout in the background. Both paths
//! want the same bounded, failure-tolerant walk, so it lives here rather than in
//! the server or executor.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use codypendent_knowledge::codegraph::{Language, ScanSummary};
use codypendent_knowledge::{codegraph, GitRevision};
use codypendent_protocol::RepositoryId;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tokio::sync::{mpsc, OwnedMutexGuard};
use tracing::{debug, info, warn};

/// The upper bound on files folded into the code graph in one scan. The scan is
/// capped so a very large tree never delays the socket opening (startup) or a
/// run's first note — but the cap must comfortably cover a real workspace: the
/// `code_nodes` table is cleared and rebuilt from this scan on every boot, so a
/// cap smaller than the repository silently truncates the *authoritative* graph
/// (and, with an unsorted walk, truncates it differently on every boot).
pub const SCAN_FILE_CAP: usize = 2000;

/// Serialize every mutation of one repository's code graph.
///
/// Two independent paths trigger a warm-up for the same checkout — the server's
/// `CreateSession` hook and the executor's `spawn_run` — and `codypendent run`
/// issues both back to back. Before this lock they both observed "not folded"
/// and both ran [`clear_repository`](codegraph::clear_repository) + a full
/// rebuild concurrently, which produced `database is locked` (so the revision
/// guard was never recorded and the repository re-scanned forever) and let a run
/// read the repository map *between* another scanner's clear and its rebuild —
/// a torn graph handed to the model (2026-08-13 review, F6). The live watcher
/// adds a third writer, so the lock is the graph's single writer gate: hold it
/// across a full scan AND across an incremental batch.
///
/// A `tokio` mutex, not a `std` one: it is deliberately held across the awaits
/// of the scan. The registry it lives in is keyed by repository, so two
/// different checkouts served by one daemon still scan in parallel.
pub async fn lock_repository(repository: RepositoryId) -> OwnedMutexGuard<()> {
    static LOCKS: OnceLock<Mutex<HashMap<RepositoryId, Arc<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    let lock = {
        // The `std` mutex guards only the map lookup and is dropped before the
        // await below — never held across one.
        let mut registry = LOCKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .expect("code-graph lock registry");
        Arc::clone(registry.entry(repository).or_default())
    };
    lock.lock_owned().await
}

/// Fold up to [`SCAN_FILE_CAP`] of `root`'s source files into the code graph for
/// `repository`, so the repository map is populated. Which files those are is
/// [`codegraph::language_for`]'s decision, never this module's. Best-effort: a
/// per-file parse/read failure is logged and skipped, never propagated — a
/// warm-up must not block or fail its caller.
///
/// The repository's prior graph is cleared first so symbols removed since the
/// last scan (files deleted outright, which a per-file reparse never revisits)
/// do not linger. The code graph is derived and regenerable, so wiping and
/// rebuilding is safe.
///
/// Returns a [`ScanSummary`] — files seen, folded per language, skipped as
/// unsupported, and whether the cap truncated the walk. It used to return `()`,
/// which meant a repository in a language the graph could not parse walked
/// thousands of files, folded none, reported success, and left an empty graph
/// with nothing anywhere saying why.
///
/// **The caller must hold [`lock_repository`] for `repository`.** This function
/// does not take it itself: the executor's warm-up re-checks its revision guard
/// under the same guard, and a lock taken here could not cover that check.
pub async fn scan_repository(
    pool: &SqlitePool,
    repository: RepositoryId,
    root: &Path,
) -> anyhow::Result<ScanSummary> {
    let Some(root) = discover_repository_root(root) else {
        anyhow::bail!("cannot scan {}: not a git repository", root.display());
    };
    let revision = head_revision(&root);

    // The walk is blocking std::fs work — off the async runtime so a large tree
    // does not stall this worker's other tasks.
    let walk_root = root;
    let (files, mut summary) =
        tokio::task::spawn_blocking(move || collect_sources(&walk_root, SCAN_FILE_CAP))
            .await
            .map_err(|error| anyhow::anyhow!("code-graph walker failed: {error}"))??;
    for (relative, source, _) in &files {
        codegraph::validate_file_graph(repository, relative, source)?;
    }
    // Destructive replacement begins only after the entire filesystem walk and
    // parse preflight succeeded. Any later database failure is returned so the
    // caller removes its in-process success marker and retries.
    codegraph::clear_repository(pool, repository).await?;
    for (relative, source, language) in files {
        match codegraph::upsert_file_graph(pool, repository, &revision, &relative, &source).await {
            Ok(delta) => {
                summary.record_folded(language);
                summary.nodes += delta.nodes.len();
                summary.edges += delta.edges.len();
            }
            Err(error) => return Err(error.into()),
        }
    }

    // A scan that folded nothing is the failure this summary exists to expose —
    // it must not look like a successful scan of an empty repository.
    if summary.found_nothing_to_fold() || summary.truncated_by_cap {
        warn!(
            repository = %repository,
            revision = %revision.0,
            summary = %summary.headline(),
            "code-graph scan produced an incomplete graph"
        );
    } else {
        info!(
            repository = %repository,
            revision = %revision.0,
            summary = %summary.headline(),
            "code-graph scan complete"
        );
    }
    Ok(summary)
}

/// Resolve `root` to the checkout's top-level directory. An ordinary directory
/// is deliberately not treated as a repository: recursively indexing a home or
/// projects directory folds unrelated checkouts into one enormous graph.
#[must_use]
pub fn discover_repository_root(root: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8(output.stdout).ok()?.trim());
    if path.as_os_str().is_empty() {
        return None;
    }
    Some(path.canonicalize().unwrap_or(path))
}

/// The working tree's `HEAD` commit as a [`GitRevision`], or the `"workdir"`
/// placeholder when Git is unavailable or `root` is not a repository. Shelling
/// out keeps this free of a Git library dependency.
///
/// Also the executor's re-scan gate: a run whose checkout has moved to a
/// revision the daemon has not folded warms the graph again, so a long-lived
/// daemon does not keep serving a repository map from whatever the tree looked
/// like at its first run.
///
/// Crate-visible because the `/update-docs` sweep labels its staleness findings
/// with the same revision the scan resolved links at (`crate::docs_job`).
#[must_use]
pub fn head_revision(root: &Path) -> GitRevision {
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    GitRevision(head.unwrap_or_else(|| "workdir".to_string()))
}

/// Collect up to `cap` `(repo-relative-path, source, language)` triples for the
/// source files under `root`, skipping `target/`, hidden (dot-prefixed)
/// directories, and anything the checkout's `.gitignore` rules exclude. A plain
/// iterative walk (no `walkdir` dependency); unreadable entries are skipped. The
/// traversal is **sorted** (per-directory, names ascending) so the cap — if it
/// ever bites — truncates the same files on every boot instead of rebuilding a
/// different graph per `read_dir` ordering.
///
/// Paths are collected first and read only after the ignore filter, so a tree
/// full of generated output is not read into memory just to be discarded.
/// The cap is applied to *candidates*, before the filter: a repository whose
/// first `SCAN_FILE_CAP` sorted paths are all ignored yields fewer files than
/// the cap, which is the conservative direction (never more work than the cap).
///
/// Returns the [`ScanSummary`] fields the walk itself knows (seen, supported,
/// unsupported, ignored, cap); the caller fills in what the fold produced.
#[allow(clippy::type_complexity)]
fn collect_sources(
    root: &Path,
    cap: usize,
) -> std::io::Result<(Vec<(String, String, Language)>, ScanSummary)> {
    let (candidates, mut summary) = collect_source_paths(root, cap)?;
    let paths: Vec<String> = candidates.iter().map(|(path, _)| path.clone()).collect();
    let ignored = ignored_paths(root, &paths);
    let mut out = Vec::with_capacity(candidates.len());
    for (relative, language) in candidates {
        if ignored.contains(&relative) {
            summary.files_skipped_ignored += 1;
            continue;
        }
        // A file that vanished between the walk and the read is not an error —
        // the tree is live. Only a genuine read failure of a present file is.
        match std::fs::read_to_string(root.join(&relative)) {
            Ok(source) => out.push((relative, source, language)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                summary.files_skipped_unreadable += 1;
                continue;
            }
            Err(error) => return Err(error),
        }
    }
    Ok((out, summary))
}

/// The repo-relative source paths under `root`, sorted and capped, each with the
/// language [`codegraph::language_for`] selected for it. Split out of
/// [`collect_sources`] so the ignore filter runs on paths alone.
///
/// **The extension test is `codegraph::language_for` and nothing else.** It was
/// `ext == "rs"` here and again in [`is_candidate_path`], two lists that agreed
/// with the parser only by coincidence; when the parser grew Python and
/// TypeScript they would have kept rejecting both.
fn collect_source_paths(
    root: &Path,
    cap: usize,
) -> std::io::Result<(Vec<(String, Language)>, ScanSummary)> {
    let mut out = Vec::new();
    let mut summary = ScanSummary {
        file_cap: cap,
        ..ScanSummary::default()
    };
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= cap {
            summary.truncated_by_cap = true;
            break;
        }
        let entries = std::fs::read_dir(&dir)?;
        let mut entries: Vec<_> = entries.collect::<Result<_, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        let mut subdirs = Vec::new();
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Skip hidden dirs/files and the build output tree.
            if name.starts_with('.') || name == "target" {
                continue;
            }
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                subdirs.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            summary.files_seen += 1;
            let Some(language) = codegraph::language_for(&path) else {
                summary.record_unsupported(&path);
                continue;
            };
            summary.files_supported += 1;
            if out.len() >= cap {
                summary.truncated_by_cap = true;
                break;
            }
            out.push((
                path.strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned(),
                language,
            ));
        }
        // LIFO stack: push in reverse so subdirectories pop in ascending order.
        for subdir in subdirs.into_iter().rev() {
            stack.push(subdir);
        }
    }
    Ok((out, summary))
}

/// Which of `relative` the checkout's ignore rules exclude, asked of Git itself.
///
/// Reimplementing `.gitignore` (nested files, negations, `core.excludesFile`,
/// `.git/info/exclude`) is a well-known source of quiet divergence, and this
/// crate already shells out to `git` for `rev-parse`. Best-effort by design: no
/// Git, a non-repository, or any spawn failure yields an EMPTY set, so the
/// caller behaves exactly as it did before this filter existed rather than
/// indexing nothing.
///
/// `--stdin -z` on both directions so a path containing a newline or a quote is
/// passed and returned verbatim. Stdin is written from a helper thread: the
/// payload can exceed a pipe buffer, and writing it inline while the child
/// writes its own output back would deadlock.
fn ignored_paths(root: &Path, relative: &[String]) -> HashSet<String> {
    if relative.is_empty() {
        return HashSet::new();
    }
    let mut payload = Vec::new();
    for path in relative {
        payload.extend_from_slice(path.as_bytes());
        payload.push(0);
    }
    let child = Command::new("git")
        .current_dir(root)
        .args(["check-ignore", "--stdin", "-z"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return HashSet::new();
    };
    let Some(mut stdin) = child.stdin.take() else {
        return HashSet::new();
    };
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&payload);
    });
    let output = child.wait_with_output();
    let _ = writer.join();
    let Ok(output) = output else {
        return HashSet::new();
    };
    // Exit 1 means "nothing was ignored"; 128 means Git could not answer. Both
    // leave an empty set, which is the same as "ignore nothing".
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|chunk| !chunk.is_empty())
        .filter_map(|chunk| std::str::from_utf8(chunk).ok())
        .map(str::to_owned)
        .collect()
}

// --------------------------------------------------------------------------
// The live code graph — a debounced, incremental filesystem watcher
// --------------------------------------------------------------------------

/// How long the watcher waits for the filesystem to go quiet before folding a
/// batch. An editor save is several events (write, rename, chmod) and a `git
/// checkout` is thousands; without a debounce every keystroke-triggered
/// autosave would be its own reparse.
pub const WATCH_DEBOUNCE: Duration = Duration::from_millis(400);

/// Hard ceiling on one debounce window, so a tool that writes continuously
/// (`cargo watch`, a formatter loop) cannot postpone the fold forever by never
/// letting the tree fall quiet.
pub const WATCH_MAX_WINDOW: Duration = Duration::from_secs(3);

/// How many distinct changed files one batch folds individually. Past this a
/// batch is a branch switch or a rebase, not an edit, and one full rescan is
/// both cheaper and more correct (it also retires files deleted by the switch).
pub const WATCH_BATCH_CAP: usize = 64;

/// Minimum interval between two overflow-triggered full rescans, so a long
/// rebase cannot chain them back to back.
pub const WATCH_FULL_RESCAN_COOLDOWN: Duration = Duration::from_secs(30);

/// Depth of the channel from the notify thread to the debouncer. Bounded: the
/// notify callback must never block or grow without limit, so an overrun is
/// dropped and recorded, and the batch it belongs to degrades to a full rescan.
const WATCH_CHANNEL_CAP: usize = 4096;

/// A live per-repository code-graph watcher. Dropping it stops the background
/// task and, with it, the underlying `notify` watcher and its thread.
pub struct RepositoryWatcher {
    repository: RepositoryId,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for RepositoryWatcher {
    fn drop(&mut self) {
        self.task.abort();
        debug!(repository = %self.repository, "code-graph watcher stopped");
    }
}

/// Arm a debounced, incremental code-graph watcher over `root`.
///
/// This is the wire outcome 14 needs: without it the graph only ever moves when
/// git `HEAD` moves, so a file edited during a session — including by the
/// agent's own `edit_file` — never enters the graph and every later turn's
/// repository map describes the pre-edit tree (2026-08-13 review, F2).
///
/// Bounded on every axis, because this runs for the daemon's whole life against
/// a tree a build system is also writing to:
///
/// * `target/`, `.git/`, any other dot-directory, and every `.gitignore`d
///   top-level directory are never WATCHED at all — one `inotify` watch is
///   taken per directory, so filtering their events afterwards would still
///   register thousands of kernel watches for build output;
/// * of what is watched, only files [`codegraph::language_for`] recognises reach
///   the channel (filtered in the notify callback, before the queue);
/// * `.gitignore` is applied per batch, by asking Git (see [`ignored_paths`]);
/// * events are debounced by [`WATCH_DEBOUNCE`] and capped at [`WATCH_MAX_WINDOW`];
/// * a batch over [`WATCH_BATCH_CAP`] files (a branch switch) collapses to ONE
///   full rescan, rate-limited by [`WATCH_FULL_RESCAN_COOLDOWN`];
/// * everything else is an incremental per-file reparse — never a
///   clear-and-rebuild, which at editor-save frequency would be ruinous.
pub fn arm_watcher(
    pool: SqlitePool,
    repository: RepositoryId,
    root: &Path,
) -> anyhow::Result<RepositoryWatcher> {
    // Watch the checkout's TOP LEVEL, and derive relative paths from it — the
    // same root `scan_repository` resolves. Watching the run's directory instead
    // would stamp `source_path` values the full scan never writes, so a reparse
    // would create a second set of nodes rather than updating the first.
    let root = discover_repository_root(root).unwrap_or_else(|| root.to_path_buf());
    let (tx, rx) = mpsc::channel::<PathBuf>(WATCH_CHANNEL_CAP);
    let filter_root = root.clone();
    // The notify callback runs on notify's own thread: `try_send` is the only
    // correct send here — it never blocks that thread and never queues without
    // bound. A full channel therefore DROPS the path, which would be a silently
    // missed edit; the drop is counted instead, and the debouncer turns any
    // non-zero count into a full rescan.
    let dropped = Arc::new(AtomicUsize::new(0));
    let dropped_by_notify = Arc::clone(&dropped);
    let mut watcher = codegraph::watcher(move |paths: Vec<PathBuf>| {
        for path in paths {
            if is_candidate_path(&filter_root, &path) && tx.try_send(path).is_err() {
                dropped_by_notify.fetch_add(1, Ordering::Relaxed);
            }
        }
    })?;
    let watched = arm_source_subtrees(&mut watcher, &root, &mut HashSet::new());

    let task = tokio::spawn(async move {
        // The watcher lives exactly as long as this task: aborting the task on
        // `RepositoryWatcher::drop` drops it here, which joins notify's thread.
        watch_loop(pool, repository, root, rx, dropped, watcher, watched).await;
    });
    info!(%repository, "code-graph watcher armed");
    Ok(RepositoryWatcher { repository, task })
}

/// True for a path the code graph could possibly hold: a file in a language
/// [`codegraph::language_for`] recognises that is not inside `target/`, `.git/`,
/// or any other dot-directory **within the checkout**. Cheap — it runs on
/// notify's thread for every raw event, including the thousands a build produces.
///
/// The language test goes through `language_for` for the same reason
/// [`collect_source_paths`] does: the watcher's idea of "worth folding" and the
/// parser's idea of "can parse" must be one question. They were two, and the
/// watcher's answer was `*.rs`.
///
/// The scan is deliberately relative to `root`: the checkout's own ancestors are
/// none of this filter's business. Testing the absolute path instead rejected
/// every file in a repository whose path happened to contain a dot-directory —
/// `/tmp/.tmpXk9/src/lib.rs`, or anything under `~/.local/share`, or a checkout
/// inside a dot-prefixed workspace directory — and the watcher then silently
/// folded nothing at all.
fn is_candidate_path(root: &Path, path: &Path) -> bool {
    if codegraph::language_for(path).is_none() {
        return false;
    }
    let relative = path.strip_prefix(root).unwrap_or(path);
    !relative.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        name == "target" || (name.starts_with('.') && name != "." && name != "..")
    })
}

/// Watch `root` itself (non-recursively) plus every immediate child directory
/// that could hold source, recursively — skipping `target/`, `.git/`, any other
/// dot-directory, and anything `.gitignore` excludes.
///
/// One `inotify` watch is taken per directory, so a recursive watch on the root
/// would register thousands for build output that can only ever produce events
/// [`is_candidate_path`] discards. `already` carries the set arming has covered,
/// so re-calling this only adds newly appeared directories. Returns the updated
/// set. Best-effort per directory: one unwatchable path is logged and skipped,
/// never fatal.
fn arm_source_subtrees(
    watcher: &mut codegraph::GraphWatcher,
    root: &Path,
    already: &mut HashSet<PathBuf>,
) -> HashSet<PathBuf> {
    // The root is watched non-recursively so a NEW top-level directory (a new
    // crate) is noticed at all; the sweep below then arms it on the next batch.
    if already.insert(root.to_path_buf()) {
        if let Err(error) = watcher.watch_subtree(root, false) {
            warn!(root = %root.display(), %error, "could not watch the repository root");
        }
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return std::mem::take(already);
    };
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "target" {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            candidates.push((name.into_owned(), entry.path()));
        }
    }
    let relative: Vec<String> = candidates.iter().map(|(name, _)| name.clone()).collect();
    let ignored = ignored_paths(root, &relative);
    for (name, path) in candidates {
        if ignored.contains(&name) || !already.insert(path.clone()) {
            continue;
        }
        if let Err(error) = watcher.watch_subtree(&path, true) {
            warn!(path = %path.display(), %error, "could not watch a source subtree");
        }
    }
    std::mem::take(already)
}

/// Drain events, debounce them into batches, and fold each batch into the graph.
#[allow(clippy::too_many_arguments)]
async fn watch_loop(
    pool: SqlitePool,
    repository: RepositoryId,
    root: PathBuf,
    mut rx: mpsc::Receiver<PathBuf>,
    dropped: Arc<AtomicUsize>,
    mut watcher: codegraph::GraphWatcher,
    mut watched: HashSet<PathBuf>,
) {
    let mut last_full_rescan = Instant::now() - WATCH_FULL_RESCAN_COOLDOWN;
    // The bytes last folded for each path, so a repeated event for an unchanged
    // file costs a hash instead of a parse and a transaction. Belt and braces
    // over the event-kind filter: any future event storm degrades to a read.
    let mut folded: HashMap<String, [u8; 32]> = HashMap::new();
    while let Some(first) = rx.recv().await {
        let mut pending: HashSet<PathBuf> = HashSet::new();
        pending.insert(first);
        // Collect until the tree goes quiet for WATCH_DEBOUNCE, or the window
        // ceiling is reached — whichever comes first.
        let window_ends = Instant::now() + WATCH_MAX_WINDOW;
        let closed = loop {
            let quiet = WATCH_DEBOUNCE.min(window_ends.saturating_duration_since(Instant::now()));
            if quiet.is_zero() {
                break false;
            }
            match tokio::time::timeout(quiet, rx.recv()).await {
                Ok(Some(path)) => {
                    pending.insert(path);
                }
                Ok(None) => break true,
                Err(_elapsed) => break false,
            }
        };

        // Two reasons to prefer one full rebuild over per-file folds: a batch
        // this large is a branch switch or a rebase (rebuilding is both cheaper
        // and retires files the switch deleted), or the notify queue overran and
        // this batch is missing paths it will never see again.
        let lost = dropped.swap(0, Ordering::Relaxed);
        let wants_rebuild = lost > 0 || pending.len() > WATCH_BATCH_CAP;

        // One writer at a time: the same gate a full warm-up holds, so a batch
        // can never interleave with a clear-and-rebuild (F6).
        let guard = lock_repository(repository).await;
        let rebuilt = if wants_rebuild && last_full_rescan.elapsed() >= WATCH_FULL_RESCAN_COOLDOWN {
            info!(
                %repository,
                changed = pending.len(),
                dropped_events = lost,
                "code-graph watcher: bulk change, rebuilding"
            );
            match scan_repository(&pool, repository, &root).await {
                Ok(_summary) => {
                    last_full_rescan = Instant::now();
                    true
                }
                Err(error) => {
                    warn!(%repository, %error, "code-graph rebuild failed");
                    false
                }
            }
        } else {
            false
        };
        let deferred_rebuild = wants_rebuild && !rebuilt;
        if !rebuilt {
            // Within the rescan cooldown, or the rebuild failed. Fold what this
            // batch DOES name rather than discarding it — bounded per-file work
            // that is still correct for every path the queue delivered.
            if wants_rebuild {
                warn!(
                    %repository,
                    changed = pending.len(),
                    dropped_events = lost,
                    "code-graph watcher: rebuild deferred by the cooldown; folding this batch incrementally"
                );
            }
            apply_batch(&pool, repository, &root, pending, &mut folded).await;
        }
        drop(guard);

        // A new top-level directory (a crate added mid-session) is not covered
        // by any recursive watch yet — the root's own non-recursive watch is what
        // reported it. One `read_dir` of the root per batch closes that gap.
        watched = arm_source_subtrees(&mut watcher, &root, &mut watched);

        // A dropped notify event names no path, so folding the paths that did
        // arrive cannot repair the graph. Previously a cooldown-deferred rebuild
        // forgot `lost` here and waited for an unrelated future filesystem event;
        // if the tree then stayed quiet the graph remained stale forever. Wait
        // outside the repository lock and perform the promised authoritative
        // rebuild even when no further event arrives. Events accumulated during
        // the wait remain in the bounded channel and are folded afterwards.
        if deferred_rebuild {
            let remaining = WATCH_FULL_RESCAN_COOLDOWN.saturating_sub(last_full_rescan.elapsed());
            if !remaining.is_zero() {
                tokio::time::sleep(remaining).await;
            }
            let guard = lock_repository(repository).await;
            match scan_repository(&pool, repository, &root).await {
                Ok(_summary) => {
                    last_full_rescan = Instant::now();
                    info!(%repository, "code-graph watcher: completed deferred rebuild");
                }
                Err(error) => {
                    // Preserve the need for a rebuild. The next queued event (or
                    // producer overflow) will retry instead of treating the
                    // missing paths as repaired.
                    dropped.fetch_add(lost.max(1), Ordering::Relaxed);
                    warn!(%repository, %error, "code-graph deferred rebuild failed");
                }
            }
            drop(guard);
        }

        if closed {
            break;
        }
    }
}

/// Reparse (or retire) each changed path, incrementally.
async fn apply_batch(
    pool: &SqlitePool,
    repository: RepositoryId,
    root: &Path,
    pending: HashSet<PathBuf>,
    folded: &mut HashMap<String, [u8; 32]>,
) {
    let mut relative: Vec<String> = pending
        .iter()
        .filter_map(|path| repo_relative(root, path))
        .collect();
    relative.sort();
    if relative.is_empty() {
        return;
    }
    // `.gitignore` is asked once per batch, not once per file.
    let ignore_root = root.to_path_buf();
    let probe = relative.clone();
    let ignored = tokio::task::spawn_blocking(move || ignored_paths(&ignore_root, &probe))
        .await
        .unwrap_or_default();

    // Every node this batch writes is stamped `<head>+workdir`, so the graph
    // says out loud that it is describing an uncommitted tree rather than
    // claiming the symbol was seen at that commit. Nothing filters on the
    // revision column; the TUI's edge table and `graph.*` answers print it.
    let revision = working_tree_revision(root);
    let mut reparsed = 0usize;
    let mut retired = 0usize;
    for path in relative {
        if ignored.contains(&path) {
            continue;
        }
        let absolute = root.join(&path);
        match std::fs::read_to_string(&absolute) {
            Ok(source) => {
                let digest: [u8; 32] = Sha256::digest(source.as_bytes()).into();
                if folded.get(&path) == Some(&digest) {
                    continue;
                }
                match codegraph::upsert_file_graph(pool, repository, &revision, &path, &source)
                    .await
                {
                    Ok(_) => {
                        // Bounded: a repository past the scan cap would otherwise
                        // grow this map without limit over the daemon's life.
                        if folded.len() >= SCAN_FILE_CAP {
                            folded.clear();
                        }
                        folded.insert(path.clone(), digest);
                        reparsed += 1;
                    }
                    Err(error) => warn!(%repository, path, %error, "code-graph reparse failed"),
                }
            }
            // Gone (deleted or renamed away): retire what it defined, which a
            // reparse can never do because nothing reparses a missing file.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                folded.remove(&path);
                match codegraph::remove_file_graph(pool, repository, &path).await {
                    Ok(removed) if removed > 0 => retired += 1,
                    Ok(_) => {}
                    Err(error) => warn!(%repository, path, %error, "code-graph retire failed"),
                }
            }
            Err(error) => debug!(%repository, path, %error, "unreadable changed file; skipped"),
        }
    }
    if reparsed > 0 || retired > 0 {
        info!(
            %repository,
            revision = %revision.0,
            reparsed,
            retired,
            "code-graph updated from the working tree"
        );
    }
}

/// `<HEAD>+workdir` — the revision an incremental, uncommitted fold is stamped
/// with. A checkout with no resolvable `HEAD` already reports `"workdir"`, which
/// is left alone rather than doubled.
fn working_tree_revision(root: &Path) -> GitRevision {
    let head = head_revision(root);
    if head.0 == "workdir" {
        head
    } else {
        GitRevision(format!("{}+workdir", head.0))
    }
}

/// The repo-relative form of a watched path, matching exactly what the full scan
/// stores in `code_nodes.source_path`. `None` when the path is not under `root`
/// (a symlinked tree, or an event for a sibling directory).
fn repo_relative(root: &Path, path: &Path) -> Option<String> {
    if let Ok(stripped) = path.strip_prefix(root) {
        return Some(stripped.to_string_lossy().into_owned());
    }
    // notify reports the path the OS handed it, which can differ from the
    // canonical root by a symlinked ancestor (`/tmp` → `/private/tmp`).
    // Canonicalizing the PARENT recovers the match without resolving the file
    // itself — which would fail for the delete events that matter most.
    let canonical_parent = path.parent()?.canonicalize().ok()?;
    let name = path.file_name()?;
    let stripped = canonical_parent.strip_prefix(root).ok()?.join(name);
    Some(stripped.to_string_lossy().into_owned())
}

// --------------------------------------------------------------------------
// The `graph.*` query seam
// --------------------------------------------------------------------------

/// Backs the `graph.callers_of` / `graph.blast_radius` / `graph.tests_covering`
/// tools, and any CLI/TUI pane that asks the same questions.
///
/// The queries themselves live in `codypendent_knowledge::codegraph` (bounded,
/// rendered, and tested there); this is only the assembly seam — the pool the
/// graph lives in, plus [`repository_id_for`], so a caller that knows a
/// repository ROOT (which is all a run knows) resolves to the SAME identity the
/// scan wrote under. Deriving it any other way is how the TUI ended up querying
/// an id the daemon never stored under (2026-08-13 review, F5).
#[derive(Clone)]
pub struct PoolCodeGraph {
    pool: SqlitePool,
}

impl PoolCodeGraph {
    /// Bind the seam to the daemon's pool.
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl codegraph::CodeGraphQueries for PoolCodeGraph {
    async fn ask(
        &self,
        repository_root: &Path,
        question: codegraph::GraphQuestion,
    ) -> Result<codegraph::GraphAnswer, String> {
        let repository = repository_id_for(repository_root);
        codegraph::answer(&self.pool, repository, &question)
            .await
            .map_err(|error| format!("code-graph query failed: {error}"))
    }
}

/// Canonicalize `root` (falling back to the path as-given) and derive the stable
/// [`RepositoryId`] the knowledge fabric attributes work under. Kept here so the
/// startup scan and the per-run executor derive identity identically.
#[must_use]
pub fn repository_id_for(root: &Path) -> RepositoryId {
    let canonical = discover_repository_root(root)
        .unwrap_or_else(|| root.canonicalize().unwrap_or_else(|_| PathBuf::from(root)));
    codypendent_knowledge::stable_repository_id(&canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo(path: &Path) {
        let status = Command::new("git")
            .current_dir(path)
            .args(["init", "--quiet"])
            .status()
            .expect("run git init");
        assert!(status.success());
    }

    #[test]
    fn repository_id_is_stable_per_root_and_distinct_across_roots() {
        // The per-run identity (issue #6 item 1) must be deterministic for one
        // checkout — so a run resolves to the same repository across launches —
        // and distinct for different checkouts served by one daemon.
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert_eq!(
            repository_id_for(a.path()),
            repository_id_for(a.path()),
            "same root → same repository id"
        );
        assert_ne!(
            repository_id_for(a.path()),
            repository_id_for(b.path()),
            "different roots → different repository ids"
        );
    }

    #[test]
    fn candidate_paths_are_judged_relative_to_the_checkout() {
        // The checkout's own ancestors are none of the filter's business. Judging
        // the ABSOLUTE path rejected every file in a repository living under a
        // dot-directory — `/tmp/.tmpXk9/…`, `~/.local/share/…`, a checkout inside
        // a dot-prefixed workspace — and the watcher then folded nothing at all.
        let root = Path::new("/tmp/.tmpXk9/repo");
        assert!(is_candidate_path(root, &root.join("src/lib.rs")));
        assert!(is_candidate_path(root, &root.join("crates/a/src/mod.rs")));

        // Inside the checkout the rules still bite.
        assert!(!is_candidate_path(
            root,
            &root.join("target/debug/build.rs")
        ));
        assert!(!is_candidate_path(root, &root.join(".git/hooks/x.rs")));
        assert!(!is_candidate_path(root, &root.join(".cargo/config.rs")));
        assert!(!is_candidate_path(root, &root.join("src/nested")));
        assert!(!is_candidate_path(root, &root.join("README.md")));
    }

    #[test]
    fn the_watcher_accepts_every_language_the_parser_handles() {
        // The watcher's filter and the parser's grammar table must be ONE list.
        // This test is that equality, written so it fails the day a language is
        // added to `language_for` and not to the watcher — which cannot happen
        // while `is_candidate_path` calls it, and is exactly what happened when
        // the two were maintained separately.
        let root = Path::new("/tmp/.tmpXk9/repo");
        for extension in codegraph::supported_extensions() {
            let path = root.join(format!("src/thing.{extension}"));
            assert!(
                is_candidate_path(root, &path),
                "the watcher rejects .{extension}, which the parser handles"
            );
        }
        // And a language nothing parses is still rejected, cheaply, before the
        // path even reaches the queue.
        for extension in ["md", "go", "java", "lock", "json"] {
            assert!(
                !is_candidate_path(root, &root.join(format!("src/thing.{extension}"))),
                ".{extension} must not reach the fold queue"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_live_edit_to_a_python_file_reaches_the_graph() {
        // Outcome 14 for the languages the user actually writes in. The filter
        // test above proves the gate; this drives the whole live path — notify
        // callback, debounce, `apply_batch`, `upsert_file_graph` — against an
        // uncommitted `.py` edit, which before the widening never reached the
        // channel at all.
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/api.py"), "def handler():\n    return 1\n").unwrap();
        init_repo(&root);

        let data = tempfile::tempdir().unwrap();
        let pool = codypendent_knowledge::db::open(&data.path().join("graph.db"))
            .await
            .unwrap();
        let repository = repository_id_for(&root);
        {
            let _guard = lock_repository(repository).await;
            scan_repository(&pool, repository, &root).await.unwrap();
        }
        assert!(
            codegraph::find_symbols(&pool, repository, "uncommitted_python_symbol", 5)
                .await
                .unwrap()
                .is_empty()
        );

        let _watcher = arm_watcher(pool.clone(), repository, &root).expect("arm the watcher");
        std::fs::write(
            root.join("src/api.py"),
            "def handler():\n    return 1\n\n\ndef uncommitted_python_symbol():\n    return handler()\n",
        )
        .unwrap();

        // Generous against WATCH_DEBOUNCE (400 ms) so a loaded box does not
        // flake; the assertion fails on timeout rather than passing silently.
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let found = !codegraph::find_symbols(&pool, repository, "uncommitted_python_symbol", 5)
                .await
                .unwrap()
                .is_empty();
            if found {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the watcher never folded the edited Python file"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    #[test]
    fn the_walk_offers_every_language_the_parser_handles_and_counts_the_rest() {
        // The full-scan gate, the other half of the same class. Before this it
        // was `ext == "rs"`, so a Python/TypeScript repository walked its whole
        // tree and offered the parser nothing.
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        for extension in codegraph::supported_extensions() {
            std::fs::write(root.join(format!("src/thing.{extension}")), "").unwrap();
        }
        std::fs::write(root.join("src/notes.md"), "").unwrap();
        std::fs::write(root.join("go.mod"), "").unwrap();
        std::fs::write(root.join("src/main.go"), "").unwrap();

        let (paths, summary) = collect_source_paths(root, SCAN_FILE_CAP).unwrap();
        let found: HashSet<String> = paths.iter().map(|(path, _)| path.clone()).collect();
        for extension in codegraph::supported_extensions() {
            let expected = format!("src/thing.{extension}");
            assert!(
                found.contains(&expected),
                "the walk skipped {expected}; found {found:?}"
            );
        }
        assert_eq!(
            summary.files_supported,
            codegraph::supported_extensions().len()
        );
        // Everything else is COUNTED, not silently dropped: that count is the
        // only thing that can explain an empty graph to a user.
        assert_eq!(summary.files_skipped_unsupported, 3);
        assert_eq!(summary.unsupported_by_extension.get("go"), Some(&1));
        assert_eq!(summary.unsupported_by_extension.get("mod"), Some(&1));
        assert_eq!(summary.unsupported_by_extension.get("md"), Some(&1));
        assert!(!summary.truncated_by_cap);
    }

    #[test]
    fn a_truncated_walk_says_so() {
        // `SCAN_FILE_CAP` used to truncate in silence, so a repository larger
        // than the cap got a partial graph presented as a complete one.
        let repo = tempfile::tempdir().unwrap();
        for index in 0..6 {
            std::fs::write(repo.path().join(format!("f{index}.rs")), "").unwrap();
        }
        let (paths, summary) = collect_source_paths(repo.path(), 3).unwrap();
        assert_eq!(paths.len(), 3);
        assert!(summary.truncated_by_cap);
        assert_eq!(summary.file_cap, 3);
        assert!(
            summary.headline().contains("TRUNCATED"),
            "{}",
            summary.headline()
        );
    }

    #[tokio::test]
    async fn a_mixed_language_repository_folds_every_language_not_only_rust() {
        // The user-visible bug, end to end through the production scan: a repo
        // with a Python file, a TSX file and a Rust file produced ONE row —
        // `('src/lib.rs', 2)` — and an empty graph plus silence for a project
        // with no Rust in it at all.
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().canonicalize().unwrap();
        seed_mixed_repository(&root);

        let data = tempfile::tempdir().unwrap();
        let pool = codypendent_knowledge::db::open(&data.path().join("graph.db"))
            .await
            .unwrap();
        let repository = repository_id_for(&root);
        let _guard = lock_repository(repository).await;
        let summary = scan_repository(&pool, repository, &root).await.unwrap();

        // Grouped exactly the way the live check reads the database back.
        let rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT source_path, language, count(*) FROM code_nodes \
             GROUP BY source_path, language ORDER BY source_path",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        println!("code_nodes by source_path: {rows:#?}");
        println!("summary: {}", summary.headline());

        let paths: Vec<&str> = rows.iter().map(|(path, _, _)| path.as_str()).collect();
        assert_eq!(paths, ["src/app.tsx", "src/lib.rs", "src/main.py"]);
        for (path, language, count) in &rows {
            assert!(*count > 1, "{path} folded to {count} node(s) — file only");
            let expected = match path.as_str() {
                "src/app.tsx" => "tsx",
                "src/lib.rs" => "rust",
                _ => "python",
            };
            assert_eq!(language, expected, "{path}");
        }

        assert_eq!(summary.files_folded, 3);
        assert_eq!(summary.folded_by_language.get("python"), Some(&1));
        assert_eq!(summary.folded_by_language.get("tsx"), Some(&1));
        assert_eq!(summary.folded_by_language.get("rust"), Some(&1));
        assert!(!summary.found_nothing_to_fold());

        // Every language contributes real relations, not just a File node.
        let calls: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM code_edges e JOIN code_nodes n ON e.from_node = n.id \
             WHERE e.relation = 'calls' AND n.source_path = ?",
        )
        .bind("src/main.py")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(calls > 0, "python produced no Calls edges");
    }

    #[tokio::test]
    async fn a_repository_with_no_parsable_source_reports_why() {
        // The silence is as much the bug as the missing grammars: an all-Go
        // repository must come back saying "1 file seen, none parsable", not
        // "scan complete".
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().canonicalize().unwrap();
        std::fs::write(root.join("main.go"), "package main\n").unwrap();
        std::fs::write(root.join("README.md"), "# hi\n").unwrap();
        init_repo(&root);

        let data = tempfile::tempdir().unwrap();
        let pool = codypendent_knowledge::db::open(&data.path().join("graph.db"))
            .await
            .unwrap();
        let repository = repository_id_for(&root);
        let _guard = lock_repository(repository).await;
        let summary = scan_repository(&pool, repository, &root).await.unwrap();

        assert!(summary.found_nothing_to_fold());
        assert_eq!(summary.files_folded, 0);
        assert_eq!(summary.files_seen, 2);
        assert_eq!(summary.files_skipped_unsupported, 2);
        let headline = summary.headline();
        println!("headline: {headline}");
        assert!(headline.contains("NO supported source found"), "{headline}");
        assert!(headline.contains(".go 1"), "{headline}");
    }

    /// A committed repository holding one Python, one TSX and one Rust file,
    /// each defining a function that calls another.
    fn seed_mixed_repository(root: &Path) {
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/main.py"),
            "from .service import dispatch\n\n\n\
             def handler(request):\n    return route(request)\n\n\n\
             def route(request):\n    return dispatch(request)\n\n\n\
             class Router:\n    def decide(self, request):\n        return handler(request)\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/app.tsx"),
            "import { useState } from \"react\";\n\n\
             export function greet(name: string): string {\n  return format(name);\n}\n\n\
             function format(name: string): string {\n  return `hi ${name}`;\n}\n\n\
             export interface Props { name: string }\n\n\
             export const App = (props: Props) => {\n\
             \x20 const [n] = useState(props.name);\n  return <div>{greet(n)}</div>;\n};\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn boot() -> u32 {\n    classify()\n}\n\npub fn classify() -> u32 { 1 }\n",
        )
        .unwrap();
        init_repo(root);
    }

    #[test]
    fn ignored_paths_asks_git_and_degrades_to_an_empty_set() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        std::fs::write(repo.path().join(".gitignore"), "generated/\n").unwrap();
        std::fs::create_dir_all(repo.path().join("generated")).unwrap();
        std::fs::write(repo.path().join("generated/a.rs"), "").unwrap();
        std::fs::write(repo.path().join("kept.rs"), "").unwrap();

        let probe = vec!["generated/a.rs".to_string(), "kept.rs".to_string()];
        let ignored = ignored_paths(repo.path(), &probe);
        assert!(ignored.contains("generated/a.rs"), "{ignored:?}");
        assert!(!ignored.contains("kept.rs"), "{ignored:?}");

        // Outside a checkout Git cannot answer; the filter must then exclude
        // nothing rather than excluding everything.
        let plain = tempfile::tempdir().unwrap();
        assert!(ignored_paths(plain.path(), &probe).is_empty());
        assert!(ignored_paths(repo.path(), &[]).is_empty());
    }

    #[test]
    fn a_working_tree_fold_is_stamped_so_the_revision_says_uncommitted() {
        let repo = tempfile::tempdir().unwrap();
        // No commits yet: `head_revision` reports the `workdir` placeholder, which
        // must not be doubled into `workdir+workdir`.
        assert_eq!(working_tree_revision(repo.path()).0, "workdir");
    }

    #[test]
    fn repository_discovery_rejects_an_ordinary_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(discover_repository_root(dir.path()), None);
    }

    #[test]
    fn repository_identity_normalizes_subdirectories_to_the_checkout_root() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let nested = repo.path().join("crates").join("demo");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(
            discover_repository_root(&nested),
            repo.path().canonicalize().ok()
        );
        assert_eq!(repository_id_for(&nested), repository_id_for(repo.path()));
    }
}
