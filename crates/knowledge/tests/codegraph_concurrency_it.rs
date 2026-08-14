//! A repository fold must survive a daemon that keeps writing underneath it.
//!
//! `codypendent graph build` failed on its own checkout — 429 files, a fresh
//! data directory — with `(code: 517) database is locked`. 517 is
//! `SQLITE_BUSY_SNAPSHOT`, not a plain busy timeout: in WAL mode it is returned
//! when a connection holds a *read* snapshot, someone else commits, and the
//! connection then tries to upgrade to a write. Waiting cannot help — the
//! snapshot is unrecoverably stale — so `busy_timeout` never sees it and the
//! transaction must be restarted from the beginning.
//!
//! The collision was self-inflicted. Every folded file appends one
//! `SymbolChanged` row to `index_outbox` inside its own transaction, so a full
//! fold of a real repository queues ten thousand of them; the daemon's
//! retrieval drainer then wakes on its 60-second timer and commits one `UPDATE
//! index_outbox` per row it claims. A fold that runs longer than that timer —
//! any repository bigger than a fixture — meets hundreds of those commits, and
//! the per-file transaction, which reads each symbol's existing id before
//! writing it, dies on the upgrade.
//!
//! These tests reproduce that shape at fixture speed by driving the same
//! competing commits by hand, and pin both halves of the requirement: the fold
//! completes, AND the competing writer keeps making progress throughout (a
//! single transaction wrapped around the whole rebuild would fix the first and
//! break the second — it was measured at 3.4s for 600 files, which starves the
//! run event ledger, the outbox and artifact writes).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use codypendent_knowledge::codegraph::{self, ScanCoverage};
use codypendent_knowledge::{db, GitRevision};
use codypendent_protocol::RepositoryId;
use sqlx::SqlitePool;
use uuid::Uuid;

/// Enough distinct symbols that a fold does real read-then-write work, small
/// enough that the whole test stays sub-second.
fn source(seed: usize) -> String {
    format!(
        r#"
pub const LIMIT_{seed}: u32 = {seed};

pub struct Widget{seed} {{
    count: u32,
}}

impl Widget{seed} {{
    pub fn new() -> Widget{seed} {{
        Widget{seed} {{ count: {seed} }}
    }}

    pub fn tick(&self) -> u32 {{
        compute_{seed}(self.count)
    }}
}}

pub fn compute_{seed}(seed: u32) -> u32 {{
    seed + 1
}}
"#
    )
}

fn files(count: usize) -> Vec<(String, String)> {
    (0..count)
        .map(|i| (format!("src/widget_{i}.rs"), source(i)))
        .collect()
}

async fn temp_pool() -> (tempfile::TempDir, SqlitePool) {
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("codypendent.db")).await.unwrap();
    (tmp, pool)
}

/// The daemon's other writers, in miniature: a task committing one autocommit
/// `UPDATE`/`INSERT` after another on its own pooled connection, exactly as the
/// retrieval drainer's `mark_processed` does. `committed` counts what landed,
/// `busy` what the fold's write lock turned away.
struct Interloper {
    handle: tokio::task::JoinHandle<()>,
    stop: Arc<AtomicUsize>,
    committed: Arc<AtomicU64>,
    busy: Arc<AtomicU64>,
}

impl Interloper {
    fn spawn(pool: SqlitePool) -> Self {
        let stop = Arc::new(AtomicUsize::new(0));
        let committed = Arc::new(AtomicU64::new(0));
        let busy = Arc::new(AtomicU64::new(0));
        let handle = tokio::spawn({
            let stop = Arc::clone(&stop);
            let committed = Arc::clone(&committed);
            let busy = Arc::clone(&busy);
            async move {
                while stop.load(Ordering::Relaxed) == 0 {
                    let result = sqlx::query(
                        "INSERT INTO index_outbox (id, event_kind, entity_id, created_at, \
                         processed_at) VALUES (?, 'memory_changed', ?, '2026-01-01T00:00:00Z', \
                         NULL)",
                    )
                    .bind(Uuid::now_v7().to_string())
                    .bind(Uuid::now_v7().to_string())
                    .execute(&pool)
                    .await;
                    match result {
                        Ok(_) => {
                            committed.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(_) => {
                            busy.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    // Yield rather than sleep: the point is to keep the WAL
                    // header moving under the fold, which is what invalidates a
                    // deferred transaction's snapshot.
                    tokio::task::yield_now().await;
                }
            }
        });
        Self {
            handle,
            stop,
            committed,
            busy,
        }
    }

    async fn finish(self) -> (u64, u64) {
        self.stop.store(1, Ordering::Relaxed);
        self.handle.await.unwrap();
        (
            self.committed.load(Ordering::Relaxed),
            self.busy.load(Ordering::Relaxed),
        )
    }
}

/// **The regression.** A full rebuild while another connection commits between
/// every one of the fold's statements. Against a deferred (`pool.begin()`)
/// per-file transaction this fails within the first few files with
/// `SQLITE_BUSY_SNAPSHOT`; the fold must instead take SQLite's write lock at
/// `BEGIN` and never have a snapshot to invalidate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rebuild_survives_a_concurrent_writer() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    let revision = GitRevision("rev-1".to_owned());
    let files = files(40);

    let interloper = Interloper::spawn(pool.clone());

    let rebuild = codegraph::rebuild_repository(
        &pool,
        repo,
        &revision,
        files.iter().map(|(p, s)| (p.as_str(), s.as_str())),
        ScanCoverage::Complete,
    )
    .await;

    let (committed, busy) = interloper.finish().await;
    let rebuild = rebuild.unwrap_or_else(|error| {
        panic!("rebuild failed under a concurrent writer ({committed} commits landed): {error}")
    });

    assert_eq!(rebuild.folded.len(), files.len());
    let nodes = codegraph::nodes(&pool, repo).await.unwrap();
    assert!(
        nodes.len() >= files.len() * 4,
        "the graph is thin: {} nodes for {} files",
        nodes.len(),
        files.len()
    );

    // The other half of the requirement: the fold must not have held the write
    // lock for the whole rebuild. A single transaction around the walk would
    // pass the assertion above and starve every other writer in the daemon.
    assert!(
        committed > 0,
        "the concurrent writer never got in — {busy} attempts were refused, \
         so the rebuild is holding one long write lock"
    );
}

/// The same collision through the incremental path the live watcher uses. A
/// per-file reparse reads the file's existing symbols before rewriting them, so
/// it has exactly the read-then-write shape a deferred transaction cannot
/// survive; the watcher fires on save, when the daemon is at its busiest.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incremental_reparse_survives_a_concurrent_writer() {
    let (_tmp, pool) = temp_pool().await;
    let repo = RepositoryId::new();
    let revision = GitRevision("rev-1".to_owned());
    let path = "src/widget_0.rs";

    // Seed, so the reparse below takes the "symbol already exists" branch — the
    // one that SELECTs an id and then UPDATEs it.
    codegraph::upsert_file_graph(&pool, repo, &revision, path, &source(0))
        .await
        .unwrap();

    let interloper = Interloper::spawn(pool.clone());
    let next = GitRevision("rev-2".to_owned());
    let mut errors = Vec::new();
    for _ in 0..40 {
        if let Err(error) = codegraph::upsert_file_graph(&pool, repo, &next, path, &source(0)).await
        {
            errors.push(error.to_string());
        }
    }
    let (committed, _) = interloper.finish().await;

    assert!(
        errors.is_empty(),
        "reparse failed under a concurrent writer ({committed} commits landed): {errors:?}"
    );
}
