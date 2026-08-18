//! What one appended event costs, and what the projections that ride along with
//! it add on top.
//!
//! # Why this function
//!
//! `ledger::append_next_event` is the single write path for the event store —
//! eleven call sites across the server, command handler and fork machinery — so
//! every durable thing the daemon does passes through it. One run is hundreds to
//! thousands of appends, and a streamed model response is one append PER
//! FRAGMENT, which makes this the daemon's per-token cost.
//!
//! # What it actually does per call
//!
//! 1. `BEGIN IMMEDIATE` — takes the database's write lock.
//! 2. One `INSERT ... SELECT` whose sequence comes from a correlated
//!    `MAX(sequence) WHERE session_id = ?` subquery.
//! 3. `session_library::index_event_sources` — the search projection, which
//!    writes one `session_search_sources` row per indexable field of the body.
//! 4. `COMMIT`.
//!
//! Two things there are worth a number rather than an assumption. The subquery
//! looks like it should get more expensive as the ledger grows; `events` is keyed
//! `PRIMARY KEY (session_id, sequence)`, so in principle SQLite answers it with a
//! reverse scan of one index and the depth does not matter — `depth/*` below is
//! there to confirm that rather than trust it. And the projection is not free
//! even for a plain streamed fragment: `ModelStreamDelta` produces a transcript
//! source row, so the per-token append is two inserts, not one. `body/*`
//! separates a body that indexes from one that does not.
//!
//! # Repeatability
//!
//! Each benchmark opens its own database in its own temporary directory with
//! production settings (WAL, `synchronous = NORMAL`) and drops it at the end.
//! Nothing is shared between benchmarks and nothing survives the run.
//!
//! Depth is held EXACT rather than allowed to drift: an iteration appends a
//! fixed batch, and the untimed setup deletes the batch again, so the hundredth
//! sample measures the same ledger depth as the first. Without that, a
//! multi-second measurement window would append tens of thousands of rows and
//! the "at depth 1 000" number would quietly become an average over a range.
//!
//! This bench does touch the filesystem, which the pure-CPU benches in the other
//! crates do not — that is unavoidable, because SQLite write cost IS the thing
//! being measured. It stays deterministic in the ways that matter: no network,
//! no model calls, no wall-clock branching, fixed identifiers, fixed payloads.

use chrono::{DateTime, TimeZone, Utc};
use codypendent_daemon::db::open_database;
use codypendent_daemon::ledger::{append_next_event, append_run_terminal, create_session};
use codypendent_protocol::artifact::{ArtifactRef, DataClassification};
use codypendent_protocol::events::{Actor, EventBody};
use codypendent_protocol::ids::ArtifactId;
use codypendent_protocol::ids::{RunId, SessionId};
use codypendent_protocol::run::RunDisposition;
use codypendent_protocol::run::RunState;
use criterion::{criterion_group, BatchSize, Criterion};
use sqlx::SqlitePool;
use std::hint::black_box;
use tokio::runtime::Runtime;

/// How many appends one timed iteration performs. Large enough that the
/// per-iteration timer overhead is negligible against a transaction commit,
/// small enough that the untimed cleanup afterwards stays cheap.
const BATCH: usize = 64;

fn runtime() -> Runtime {
    // Current-thread: a multi-threaded runtime would fold the scheduler's
    // work-stealing into every sample for no benefit — there is one task here.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

fn fixed_time() -> DateTime<Utc> {
    Utc.timestamp_opt(1_765_000_000, 0).single().expect("fixed")
}

fn session_id() -> SessionId {
    SessionId(uuid::Uuid::from_u128(
        0x51_0000_0000_0000_0000_0000_0000_0001,
    ))
}

fn run_id() -> RunId {
    RunId(uuid::Uuid::from_u128(
        0x2a_0000_0000_0000_0000_0000_0000_0001,
    ))
}

/// A streamed model fragment — the daemon's most frequent event by a wide
/// margin, and one the search projection DOES index.
fn stream_delta() -> EventBody {
    EventBody::ModelStreamDelta {
        run_id: run_id(),
        // Multi-byte text: the projection hashes the content and SQLite stores
        // it as UTF-8, so an ASCII-only corpus would understate both.
        text: "設定を検証しています… ✅ ok".to_owned(),
    }
}

/// A body the search projection does NOT index, so the difference between this
/// and `stream_delta` is the projection's own cost rather than the insert's.
fn state_change() -> EventBody {
    EventBody::RunStateChanged {
        run_id: run_id(),
        state: RunState::Running,
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    pool: SqlitePool,
}

/// A fresh database, migrated, with one open session and `depth` events already
/// in the ledger.
async fn fixture(depth: usize) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = open_database(&dir.path().join("bench.db"))
        .await
        .expect("open database");
    create_session(&pool, session_id(), "ledger benchmark")
        .await
        .expect("create session");
    for _ in 0..depth {
        append_next_event(
            &pool,
            session_id(),
            &Actor::System,
            &state_change(),
            fixed_time(),
        )
        .await
        .expect("seed");
    }
    Fixture { _dir: dir, pool }
}

/// Append `BATCH` events. This is the timed routine.
async fn append_batch(pool: &SqlitePool, body: &EventBody) {
    for _ in 0..BATCH {
        append_next_event(
            pool,
            session_id(),
            &Actor::System,
            black_box(body),
            fixed_time(),
        )
        .await
        .expect("append");
    }
}

/// Return the ledger to `depth`, so every sample starts from the same place.
/// Untimed: criterion runs this in `iter_batched`'s setup.
async fn truncate_to(pool: &SqlitePool, depth: usize) {
    let keep = depth as i64;
    // Source rows first: they carry a foreign key onto (session_id, sequence).
    sqlx::query("DELETE FROM session_search_sources WHERE session_id = ? AND event_sequence > ?")
        .bind(session_id().to_string())
        .bind(keep)
        .execute(pool)
        .await
        .expect("trim sources");
    sqlx::query("DELETE FROM events WHERE session_id = ? AND sequence > ?")
        .bind(session_id().to_string())
        .bind(keep)
        .execute(pool)
        .await
        .expect("trim events");
}

/// Does append cost grow with how many events the session already holds?
fn bench_depth(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("ledger/append/depth");
    // Each sample is BATCH real transactions against a real file.
    group.sample_size(10);

    for depth in [0usize, 1_000, 10_000] {
        let fixture = rt.block_on(fixture(depth));
        group.bench_function(format!("{depth}_events"), |b| {
            b.iter_batched(
                || rt.block_on(truncate_to(&fixture.pool, depth)),
                |()| rt.block_on(append_batch(&fixture.pool, &stream_delta())),
                BatchSize::PerIteration,
            )
        });
    }

    group.finish();
}

/// What does the search projection add to an append?
fn bench_body(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("ledger/append/body");
    group.sample_size(10);

    const DEPTH: usize = 1_000;
    for (name, body) in [
        ("indexed/stream_delta", stream_delta()),
        ("not_indexed/state_changed", state_change()),
    ] {
        let fixture = rt.block_on(fixture(DEPTH));
        group.bench_function(name, |b| {
            b.iter_batched(
                || rt.block_on(truncate_to(&fixture.pool, DEPTH)),
                |()| rt.block_on(append_batch(&fixture.pool, &body)),
                BatchSize::PerIteration,
            )
        });
    }

    group.finish();
}

/// How many run completions one timed iteration performs.
const RUNS_PER_BATCH: usize = 16;

fn nth_run_id(i: usize) -> RunId {
    RunId(uuid::Uuid::from_u128(
        0x2a_0000_0000_0000_0000_0000_0001_0000 + i as u128,
    ))
}

/// Insert `count` runs in the `Running` state, ready to be completed.
async fn seed_runs(pool: &SqlitePool, count: usize) {
    for i in 0..count {
        sqlx::query(
            "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json) \
             VALUES (?, ?, ?, 'Running', 'Build', 'hosted-default', '{}')",
        )
        .bind(nth_run_id(i).to_string())
        .bind(session_id().to_string())
        .bind("benchmark run")
        .execute(pool)
        .await
        .expect("seed run");
    }
}

async fn clear_runs(pool: &SqlitePool) {
    sqlx::query("DELETE FROM runs WHERE session_id = ?")
        .bind(session_id().to_string())
        .execute(pool)
        .await
        .expect("clear runs");
}

fn completion(run: RunId) -> EventBody {
    EventBody::RunCompleted {
        run_id: run,
        disposition: RunDisposition::Completed {
            summary: Some("done".to_owned()),
        },
        chronicle: ArtifactRef {
            id: ArtifactId(uuid::Uuid::from_u128(
                0xc0de_0000_0000_0000_0000_0000_0000_0001,
            )),
            media_type: "application/json".to_owned(),
            byte_length: 2_048,
            sha256: "0".repeat(64),
            sensitivity: DataClassification::Internal,
        },
    }
}

/// The per-RUN cost, as opposed to the per-event cost above.
///
/// The brief for this harness assumed analytics observations and inbox
/// producers run on every append. They do not: `append_next_event` calls only
/// the session-library indexer. The run-state projection, the analytics owner
/// resolution and `inbox::resolve_run_entries` hang off `append_run_terminal`,
/// which fires ONCE per run. That makes them a fixed cost per run rather than a
/// per-event one, and this is what that fixed cost actually is.
fn bench_run_terminal(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("ledger/append");
    group.sample_size(10);

    const DEPTH: usize = 1_000;
    let fixture = rt.block_on(fixture(DEPTH));

    group.bench_function("run_terminal", |b| {
        b.iter_batched(
            || {
                rt.block_on(async {
                    truncate_to(&fixture.pool, DEPTH).await;
                    clear_runs(&fixture.pool).await;
                    seed_runs(&fixture.pool, RUNS_PER_BATCH).await;
                })
            },
            |()| {
                rt.block_on(async {
                    for i in 0..RUNS_PER_BATCH {
                        append_run_terminal(
                            &fixture.pool,
                            session_id(),
                            &Actor::System,
                            RunState::Completed,
                            black_box(&completion(nth_run_id(i))),
                            fixed_time(),
                        )
                        .await
                        .expect("terminal");
                    }
                })
            },
            BatchSize::PerIteration,
        )
    });

    group.finish();
}

criterion_group!(benches, bench_depth, bench_body, bench_run_terminal);
fn main() {
    // The A/B harness below shares this file's fixtures, so it lives behind an
    // env var rather than in a second bench target: same corpus, same helpers,
    // no duplicated setup that could drift from what criterion measures.
    if let Ok(which) = std::env::var("CODY_AB") {
        ab_main(&which);
        return;
    }
    benches();
    writepath();
    Criterion::default().configure_from_args().final_summary();
}

// ---------------------------------------------------------------------------
// Write-path profiling (second pass).
//
// The first pass measured the per-append cost at a FIXED ledger depth and the
// per-run cost at a FIXED ledger depth. Neither answers the question the brief
// actually poses: does anything on this path do work proportional to how much
// the session already holds? `bench_depth` answered that for the per-EVENT
// path (flat). The per-RUN path was never varied, and `append_run_terminal`
// contains this guard:
//
//     SELECT EXISTS(SELECT 1 FROM events
//                   WHERE session_id = ? AND json_valid(body)
//                     AND json_extract(body,'$.type') = 'RunCompleted'
//                     AND json_extract(body,'$.run_id') = ?)
//
// `EXPLAIN QUERY PLAN` reports `SEARCH events USING INDEX
// sqlite_autoindex_events_1 (session_id=?)` — NOT a covering index, so it seeks
// to the session and then walks every event row of that session, pulling `body`
// off the table page and running two JSON parses over it. In the normal case
// (this run has not completed before) the answer is false, which means it walks
// ALL of them. `depth/*` below measures whether that is real.
// ---------------------------------------------------------------------------

/// The per-run terminal cost as a function of how many events the session
/// already holds. If the `already_completed` guard's scan is real, this grows
/// linearly; if SQLite is answering it some cheaper way, it is flat like
/// `bench_depth`.
fn bench_run_terminal_depth(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("ledger/run_terminal/depth");
    group.sample_size(10);

    for depth in [0usize, 1_000, 10_000] {
        let fixture = rt.block_on(fixture(depth));
        group.bench_function(format!("{depth}_events"), |b| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        truncate_to(&fixture.pool, depth).await;
                        clear_runs(&fixture.pool).await;
                        seed_runs(&fixture.pool, RUNS_PER_BATCH).await;
                    })
                },
                |()| {
                    rt.block_on(async {
                        for i in 0..RUNS_PER_BATCH {
                            append_run_terminal(
                                &fixture.pool,
                                session_id(),
                                &Actor::System,
                                RunState::Completed,
                                black_box(&completion(nth_run_id(i))),
                                fixed_time(),
                            )
                            .await
                            .expect("terminal");
                        }
                    })
                },
                BatchSize::PerIteration,
            )
        });
    }

    group.finish();
}

/// How many streamed fragments one simulated run emits. A real coding run is
/// hundreds to low thousands; 500 keeps a criterion sample under a second while
/// staying in the right order of magnitude.
const RUN_FRAGMENTS: usize = 500;
/// Tool calls per simulated run — each is a started/completed pair.
const RUN_TOOL_CALLS: usize = 20;

fn tool_started(run: RunId, i: usize) -> EventBody {
    EventBody::ToolStarted {
        run_id: run,
        tool: "workspace.read_file".to_owned(),
        args_digest: format!("{i:064x}"),
        label: Some(format!("crates/daemon/src/ledger_{i}.rs")),
    }
}

fn tool_completed(run: RunId) -> EventBody {
    EventBody::ToolCompleted {
        run_id: run,
        tool: "workspace.read_file".to_owned(),
        outcome: codypendent_protocol::run::ToolOutcome::Succeeded,
        artifact: None,
    }
}

fn run_started(run: RunId) -> EventBody {
    EventBody::RunStarted {
        run_id: run,
        objective: "検証: profile the ledger write path ✅".to_owned(),
        mode: codypendent_protocol::run::AgentMode::Build,
    }
}

/// Every durable write ONE run performs, start to finish. This is the number
/// the brief asks for: what a realistic agent run costs in SQLite.
async fn one_run(pool: &SqlitePool, run: RunId) {
    append_next_event(
        pool,
        session_id(),
        &Actor::System,
        &run_started(run),
        fixed_time(),
    )
    .await
    .expect("run started");
    for _ in 0..RUN_FRAGMENTS {
        append_next_event(
            pool,
            session_id(),
            &Actor::System,
            &stream_delta_for(run),
            fixed_time(),
        )
        .await
        .expect("delta");
    }
    for i in 0..RUN_TOOL_CALLS {
        append_next_event(
            pool,
            session_id(),
            &Actor::System,
            &tool_started(run, i),
            fixed_time(),
        )
        .await
        .expect("tool started");
        append_next_event(
            pool,
            session_id(),
            &Actor::System,
            &tool_completed(run),
            fixed_time(),
        )
        .await
        .expect("tool completed");
    }
    append_run_terminal(
        pool,
        session_id(),
        &Actor::System,
        RunState::Completed,
        &completion(run),
        fixed_time(),
    )
    .await
    .expect("terminal");
}

fn stream_delta_for(run: RunId) -> EventBody {
    EventBody::ModelStreamDelta {
        run_id: run,
        text: "設定を検証しています… ✅ ok".to_owned(),
    }
}

/// One whole run's write cost, measured at two ledger depths so the per-run
/// component that scales with session length is visible in the total.
fn bench_realistic_run(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("ledger/realistic_run");
    group.sample_size(10);

    for depth in [0usize, 10_000] {
        let fixture = rt.block_on(fixture(depth));
        group.bench_function(format!("depth_{depth}"), |b| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        truncate_to(&fixture.pool, depth).await;
                        clear_runs(&fixture.pool).await;
                        seed_runs(&fixture.pool, 1).await;
                    })
                },
                |()| rt.block_on(one_run(&fixture.pool, black_box(nth_run_id(0)))),
                BatchSize::PerIteration,
            )
        });
    }

    group.finish();
}

criterion_group!(writepath, bench_run_terminal_depth, bench_realistic_run);

// ---------------------------------------------------------------------------
// A/B harness for a CONTENDED machine.
//
// Criterion reports a mean with a confidence interval, which is the right thing
// on a quiet box and misleading on a loaded one: contention only ever adds time,
// so it inflates the mean and widens the interval without moving the floor. This
// harness reports the MINIMUM of many repetitions instead — the sample that got
// the fewest interruptions is the one closest to the uncontended cost — and it
// INTERLEAVES the two variants (ABABAB) inside a single process against a single
// fixture, so any drift in machine load lands on both arms equally.
//
// The index arm is a true A/B: `DROP INDEX` / `CREATE INDEX` toggles the thing
// under test at runtime, so both arms run the same binary, the same fixture and
// the same rows within seconds of each other. Nothing else differs.
//
// Run with `CODY_AB=index`, `CODY_AB=append` or `CODY_AB=run`.
// ---------------------------------------------------------------------------

const AB_INDEX_SQL: &str = "CREATE INDEX IF NOT EXISTS idx_events_run_completed \
     ON events (session_id, json_extract(body, '$.run_id')) \
     WHERE json_valid(body) AND json_extract(body, '$.type') = 'RunCompleted'";

async fn set_index(pool: &SqlitePool, present: bool) {
    if present {
        sqlx::query(AB_INDEX_SQL)
            .execute(pool)
            .await
            .expect("create index");
    } else {
        sqlx::query("DROP INDEX IF EXISTS idx_events_run_completed")
            .execute(pool)
            .await
            .expect("drop index");
    }
}

fn report(label: &str, samples: &mut Vec<f64>, ops: usize) {
    samples.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
    let min = samples[0];
    let median = samples[samples.len() / 2];
    eprintln!(
        "{label:<34} min {:>9.3} ms  ({:>8.1} us/op)   median {:>9.3} ms",
        min,
        min * 1000.0 / ops as f64,
        median
    );
}

/// Does the partial expression index on `events` actually change what
/// `append_run_terminal` costs, and by how much, at a realistic session depth?
fn ab_index(rt: &Runtime) {
    const DEPTH: usize = 10_000;
    const REPS: usize = 15;
    let fixture = rt.block_on(fixture(DEPTH));
    let mut without = Vec::new();
    let mut with = Vec::new();

    for _ in 0..REPS {
        for present in [false, true] {
            rt.block_on(async {
                set_index(&fixture.pool, present).await;
                truncate_to(&fixture.pool, DEPTH).await;
                clear_runs(&fixture.pool).await;
                seed_runs(&fixture.pool, RUNS_PER_BATCH).await;
            });
            let start = std::time::Instant::now();
            rt.block_on(async {
                for i in 0..RUNS_PER_BATCH {
                    append_run_terminal(
                        &fixture.pool,
                        session_id(),
                        &Actor::System,
                        RunState::Completed,
                        black_box(&completion(nth_run_id(i))),
                        fixed_time(),
                    )
                    .await
                    .expect("terminal");
                }
            });
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            if present {
                with.push(elapsed)
            } else {
                without.push(elapsed)
            }
        }
    }

    eprintln!(
        "\n== append_run_terminal, {RUNS_PER_BATCH} run completions, session depth {DEPTH} =="
    );
    report(
        "without idx_events_run_completed",
        &mut without,
        RUNS_PER_BATCH,
    );
    report(
        "with    idx_events_run_completed",
        &mut with,
        RUNS_PER_BATCH,
    );
}

/// The per-append cost, for comparing a patched build against a pristine one.
/// Reported as a minimum so the comparison survives a loaded machine.
fn ab_append(rt: &Runtime) {
    const DEPTH: usize = 1_000;
    const REPS: usize = 30;
    let fixture = rt.block_on(fixture(DEPTH));
    let mut indexed = Vec::new();
    let mut plain = Vec::new();

    for _ in 0..REPS {
        for (samples, body) in [(&mut indexed, stream_delta()), (&mut plain, state_change())] {
            rt.block_on(truncate_to(&fixture.pool, DEPTH));
            let start = std::time::Instant::now();
            rt.block_on(append_batch(&fixture.pool, &body));
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
        }
    }

    eprintln!("\n== append_next_event, {BATCH} appends, session depth {DEPTH} ==");
    report("body indexed (ModelStreamDelta)", &mut indexed, BATCH);
    report("body not indexed (RunStateChanged)", &mut plain, BATCH);
}

/// One whole run's durable writes, at two session depths, INTERLEAVED.
///
/// The depths must alternate rather than run one after the other: on a loaded
/// machine a sequential loop charges whatever the load happened to be during
/// the second arm to "depth", and the comparison stops meaning anything. Both
/// fixtures are built up front and then alternated sample by sample.
fn ab_run(rt: &Runtime) {
    const REPS: usize = 9;
    const DEEP: usize = 10_000;
    let shallow = rt.block_on(fixture(0));
    let deep = rt.block_on(fixture(DEEP));
    let mut s_samples = Vec::new();
    let mut d_samples = Vec::new();

    for _ in 0..REPS {
        for (fixture, depth, samples) in [
            (&shallow, 0usize, &mut s_samples),
            (&deep, DEEP, &mut d_samples),
        ] {
            rt.block_on(async {
                truncate_to(&fixture.pool, depth).await;
                clear_runs(&fixture.pool).await;
                seed_runs(&fixture.pool, 1).await;
            });
            let start = std::time::Instant::now();
            rt.block_on(one_run(&fixture.pool, black_box(nth_run_id(0))));
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
        }
    }

    let ops = 1 + RUN_FRAGMENTS + 2 * RUN_TOOL_CALLS + 1;
    eprintln!(
        "\n== one run ({RUN_FRAGMENTS} fragments + {RUN_TOOL_CALLS} tool calls + terminal), \
         {ops} durable writes, interleaved =="
    );
    report("session depth 0", &mut s_samples, ops);
    report(&format!("session depth {DEEP}"), &mut d_samples, ops);
}

/// The `write_source_entry` change, isolated and run through the real sqlx
/// stack rather than a standalone SQLite driver.
///
/// The only thing that changed in that function is whether the run/artifact
/// foreign-key resolutions are separate `SELECT`s or correlated subqueries
/// inside the `INSERT`. Both forms are issued here against the same pool, the
/// same rows and the same transaction shape, alternating, so the difference is
/// the statement count and nothing else.
async fn wse_old(pool: &SqlitePool, n: usize) {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.expect("begin");
    for i in 0..n {
        let resolved: Option<String> = sqlx::query_scalar("SELECT id FROM runs WHERE id = ?")
            .bind(nth_run_id(0).to_string())
            .fetch_optional(&mut *tx)
            .await
            .expect("resolve run");
        sqlx::query(
            "INSERT INTO session_search_sources \
             (session_id, source_type, source_id, content_hash, indexed_at, \
              event_sequence, run_id, artifact_id) \
             VALUES (?, 'transcript', ?, 'h', 't', NULL, ?, NULL) \
             ON CONFLICT(session_id, source_type, source_id) DO UPDATE SET \
             content_hash = excluded.content_hash",
        )
        .bind(session_id().to_string())
        .bind(format!("ab:old:{i}"))
        .bind(resolved)
        .execute(&mut *tx)
        .await
        .expect("insert");
    }
    tx.commit().await.expect("commit");
}

async fn wse_new(pool: &SqlitePool, n: usize) {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.expect("begin");
    for i in 0..n {
        sqlx::query(
            "INSERT INTO session_search_sources \
             (session_id, source_type, source_id, content_hash, indexed_at, \
              event_sequence, run_id, artifact_id) \
             VALUES (?, 'transcript', ?, 'h', 't', NULL, \
                     (SELECT id FROM runs WHERE id = ?), \
                     (SELECT id FROM artifacts WHERE id = ?)) \
             ON CONFLICT(session_id, source_type, source_id) DO UPDATE SET \
             content_hash = excluded.content_hash",
        )
        .bind(session_id().to_string())
        .bind(format!("ab:new:{i}"))
        .bind(nth_run_id(0).to_string())
        .bind(None::<String>)
        .execute(&mut *tx)
        .await
        .expect("insert");
    }
    tx.commit().await.expect("commit");
}

fn ab_wse(rt: &Runtime) {
    const ENTRIES: usize = 2_000;
    const REPS: usize = 15;
    let fixture = rt.block_on(fixture(0));
    rt.block_on(seed_runs(&fixture.pool, 1));
    let mut old = Vec::new();
    let mut new = Vec::new();

    for _ in 0..REPS {
        for which in [0u8, 1] {
            rt.block_on(async {
                sqlx::query("DELETE FROM session_search_sources WHERE source_id LIKE 'ab:%'")
                    .execute(&fixture.pool)
                    .await
                    .expect("clear");
            });
            let start = std::time::Instant::now();
            if which == 0 {
                rt.block_on(wse_old(&fixture.pool, ENTRIES));
            } else {
                rt.block_on(wse_new(&fixture.pool, ENTRIES));
            }
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            if which == 0 {
                old.push(elapsed)
            } else {
                new.push(elapsed)
            }
        }
    }

    eprintln!("\n== write_source_entry, {ENTRIES} source rows, via sqlx ==");
    report("3-statement form (before)", &mut old, ENTRIES);
    report("1-statement form (after)", &mut new, ENTRIES);
}

/// The counter-measurement for migration 0050: a partial expression index is
/// re-evaluated on EVERY insert into `events`, and `events` is the daemon's
/// hottest write path. This toggles the index and appends through the real
/// `append_next_event`, so the tax is measured where it is actually paid.
fn ab_tax(rt: &Runtime) {
    const DEPTH: usize = 1_000;
    const REPS: usize = 25;
    let fixture = rt.block_on(fixture(DEPTH));
    let mut without = Vec::new();
    let mut with = Vec::new();

    for _ in 0..REPS {
        for present in [false, true] {
            rt.block_on(async {
                set_index(&fixture.pool, present).await;
                truncate_to(&fixture.pool, DEPTH).await;
            });
            let start = std::time::Instant::now();
            rt.block_on(append_batch(&fixture.pool, &stream_delta()));
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            if present {
                with.push(elapsed)
            } else {
                without.push(elapsed)
            }
        }
    }

    eprintln!("\n== append_next_event, {BATCH} appends, index maintenance tax ==");
    report("without idx_events_run_completed", &mut without, BATCH);
    report("with    idx_events_run_completed", &mut with, BATCH);
}

fn ab_main(which: &str) {
    let rt = runtime();
    match which {
        "index" => ab_index(&rt),
        "append" => ab_append(&rt),
        "run" => ab_run(&rt),
        "wse" => ab_wse(&rt),
        "tax" => ab_tax(&rt),
        other => panic!("unknown CODY_AB={other}"),
    }
}
