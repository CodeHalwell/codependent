//! The run prelude: the fixed cost `spawn_run` pays before it has even decided
//! whether there is any work to do.
//!
//! # What is being measured
//!
//! `RuntimeExecutor::spawn_run` opens with two calls that both shell out to
//! `git`, on every run, warm graph or cold:
//!
//! 1. `scan::repository_id_for(&launch.repository)` → `discover_repository_root`
//!    → `resolve_scan_root` → `git rev-parse --show-toplevel`;
//! 2. `ensure_scanned(...)` → `scan::head_revision(root)` → `git rev-parse HEAD`,
//!    whose result is the key of the in-process "already folded" map. On a warm
//!    graph the function returns immediately after that call, so `head_revision`
//!    IS the warm path.
//!
//! `ensure_scanned`'s own doc comment says a run at an already-folded revision
//! "still costs nothing but the `rev-parse` the run's identity derivation
//! already pays". That sentence contains two claims — that the warm path costs
//! one `rev-parse`, and that the cost is negligible — and it is wrong on the
//! first: identity derivation pays `--show-toplevel`, `ensure_scanned` pays
//! `HEAD`, and they are separate processes. This bench prices both, plus
//! `working_tree_dirty` (`git status --porcelain`), which the fold's revision
//! stamp adds on the cold path and which is the one whose cost scales with the
//! size of the checkout rather than being a fixed process spawn.
//!
//! # Determinism
//!
//! Each fixture is a repository created in a tempdir by this bench: `git init`,
//! one commit, a fixed number of files. Nothing reads the developer's own
//! checkout, so the numbers do not depend on where this workspace lives or how
//! dirty it happens to be. Git's own object cache is warm after the setup
//! commit, which is the state a daemon serving repeated runs is always in.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use codypendent_codypendentd::scan;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::hint::black_box;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

/// A committed repository of `files` small source files under `src/`.
fn repository(dir: &Path, files: usize) -> PathBuf {
    git(dir, &["init", "--quiet"]);
    git(dir, &["config", "user.email", "bench@example.invalid"]);
    git(dir, &["config", "user.name", "bench"]);
    std::fs::create_dir_all(dir.join("src")).expect("src");
    for i in 0..files {
        std::fs::write(
            dir.join("src").join(format!("file{i}.rs")),
            format!("pub fn symbol_{i}() -> u64 {{ {i} }}\n"),
        )
        .expect("write source");
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "--quiet", "-m", "bench fixture"]);
    dir.to_path_buf()
}

/// The two `git` calls every run pays before any work is decided, priced
/// separately so the doc comment's "one rev-parse" can be checked against two.
fn run_prelude_git(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = repository(dir.path(), 200);
    let mut group = c.benchmark_group("runprelude/git");

    group.bench_function("repository_id_for", |b| {
        b.iter(|| black_box(scan::repository_id_for(&root)));
    });
    group.bench_function("head_revision", |b| {
        b.iter(|| black_box(scan::head_revision(&root)));
    });
    group.finish();
}

/// `git status --porcelain`, the extra call the fold's revision stamp makes, at
/// three checkout sizes. Unlike the two above it is not a fixed process spawn —
/// it walks the index.
fn dirty_probe_by_repo_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("runprelude/working_tree_dirty");
    for files in [10usize, 200, 2_000] {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = repository(dir.path(), files);
        group.bench_with_input(BenchmarkId::from_parameter(files), &files, |b, _| {
            b.iter(|| black_box(scan::working_tree_dirty(&root)));
        });
    }
    group.finish();
}

/// The OTHER unbounded read on the run path, and the one that grows with the
/// session rather than the repository.
///
/// `spawn_run` → `build_run_seed` → `reconstruct_prior` calls
/// `ledger::load_events(session_id)`, which is `SELECT … FROM events WHERE
/// session_id = ? ORDER BY sequence ASC` with no window: the FULL history of the
/// session, JSON-decoded body by body, on every continuation run. The daemon
/// already has a windowed reader beside it (`load_events_between`, whose own doc
/// comment says a client "one event behind on a 100k-event session must not pay
/// a full-history read per reconnect") — the run path does not use it.
///
/// A streamed model response is one event per fragment, so session depth here is
/// counted in fragments, not in turns. The sizes below are one short run, a
/// working session, and a long-lived one.
///
/// This measures the LOAD only. The fold that consumes it, `continuation_prior`,
/// is `pub(crate)` in `codypendent-codypendentd` and cannot be reached from a
/// bench, so its cost is not included in these numbers and is not claimed.
fn continuation_prior_load(c: &mut Criterion) {
    use codypendent_daemon::db::open_database;
    use codypendent_daemon::ledger::{append_next_event, create_session, load_events};
    use codypendent_protocol::events::{Actor, EventBody};
    use codypendent_protocol::ids::{RunId, SessionId};

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("bench runtime");
    let session = SessionId(uuid::Uuid::from_u128(
        0x51_0000_0000_0000_0000_0000_0000_00a1,
    ));
    let run = RunId(uuid::Uuid::from_u128(
        0x51_0000_0000_0000_0000_0000_0000_00a2,
    ));
    let at = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("fixed timestamp")
        .with_timezone(&chrono::Utc);

    let mut group = c.benchmark_group("runprelude/load_events");
    group.sample_size(10);
    for depth in [200usize, 2_000, 20_000] {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = rt.block_on(async {
            let pool = open_database(&dir.path().join("bench.db"))
                .await
                .expect("open database");
            create_session(&pool, session, "run-path benchmark")
                .await
                .expect("create session");
            for i in 0..depth {
                let body = EventBody::ModelStreamDelta {
                    run_id: run,
                    // A fragment of prose, the shape a streamed response
                    // actually appends — the body is JSON-decoded on the way
                    // back out, so its size is part of the cost.
                    text: format!("fragment {i} of the model's streamed response, "),
                };
                append_next_event(&pool, session, &Actor::System, &body, at)
                    .await
                    .expect("seed event");
            }
            pool
        });
        group.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |b, _| {
            b.iter(|| {
                rt.block_on(async {
                    black_box(load_events(&pool, session).await.expect("load").len())
                })
            });
        });
        rt.block_on(pool.close());
    }
    group.finish();
}

criterion_group!(
    benches,
    run_prelude_git,
    dirty_probe_by_repo_size,
    continuation_prior_load
);
criterion_main!(benches);
