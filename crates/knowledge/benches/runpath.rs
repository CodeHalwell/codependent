//! The run path: everything between the user pressing Enter and the first model
//! token, as far as the knowledge fabric is responsible for it.
//!
//! # What the executor actually does before the model is called
//!
//! `RuntimeExecutor::spawn_run` runs, in order: `repository_id_for` (git),
//! `ensure_scanned` (git + a possible scan), `build_run_seed` → `emit_context`
//! → **`assemble_context`**, then the worktree bind and the model call. Only the
//! `assemble_context` step lives in this crate, and it is the only step of the
//! four whose cost is a function of how big the repository's graph has grown.
//!
//! # The claim under test
//!
//! `ContextAssembler` exists to cache "the retrieval authority" — the registry
//! item list and the derived hashing indexes — behind a `RegistryStamp`, and
//! `emit_context`'s doc comment treats the cached path as the cheap one and the
//! semantic path as the expensive one *because it "sources the registry per
//! call, so it does not use the stamped cache"*.
//!
//! That framing is only true if the registry is what dominates. Both paths call
//! the same `assemble_with` core, and step 1 of that core is
//! `repomap::repository_map`, which is **not cached by either path**: it issues
//! `SELECT … FROM code_nodes WHERE repository = ?` with no LIMIT, materialises
//! every row into a `CodeNode` (three `String`s each), groups them through a
//! `BTreeMap<String, ModuleEntry>` keyed by a freshly allocated module path, and
//! sorts every group — in order to render a summary bounded to 50 modules of 8
//! symbols each.
//!
//! So the benches below are laid out to separate the two: the same assembly at
//! several graph sizes, with the registry held constant. If the registry were
//! the dominant term the curve would be flat.
//!
//! # Why these sizes
//!
//! The scan bug that motivated this harness produced a graph of 510,904 nodes.
//! `graph_nodes` therefore runs 1k / 10k / 100k / 500k, which brackets that
//! report, and the small end is a normal repository so the numbers say what the
//! ordinary case pays as well as the pathological one.
//!
//! # Determinism
//!
//! Every fixture is generated in-process from an index: a real migrated SQLite
//! database in a tempdir (WAL + `synchronous=NORMAL`, the daemon's settings, via
//! this crate's own `db::open`), rows inserted by the same statement shape the
//! fold writes, a fixed `RepositoryId`, a fixed objective string. No repository
//! is read, no network, no model, no clock-dependent behaviour beyond the
//! `created_at` the schema requires. The graph is built ONCE per size and reused
//! across every sample: these benches measure the READ path, which is what a run
//! pays, and rebuilding per iteration would measure the write path instead.

use std::path::Path;

use codypendent_knowledge::context::{assemble_context, ContextAssembler};
use codypendent_knowledge::{register_builtins, repomap, Scope};
use codypendent_protocol::RepositoryId;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use sqlx::SqlitePool;
use std::hint::black_box;

fn repo() -> RepositoryId {
    RepositoryId(uuid::Uuid::from_u128(
        0x5c5c_0000_0000_0000_0000_0000_0000_0007,
    ))
}

/// The objective a run opens with — fixed, so retrieval scores the same query
/// every sample.
const OBJECTIVE: &str =
    "add a bounded LIMIT to the repository map query and prove it with a benchmark";

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("bench runtime")
}

/// Insert `count` code-graph nodes for `repository`, shaped like a real fold:
/// symbols distributed over `modules` module paths, in the kind mix a Rust
/// repository produces (functions and methods dominate, with types, constants
/// and tests behind them, and one `module` node heading each module).
///
/// Written with a multi-row INSERT inside one transaction rather than through
/// `rebuild_repository`, because the fixture is the READ path's input: what
/// matters is that the rows are byte-identical in shape to what the fold writes
/// (same columns, same `kind` scalars, same qualified-name structure), not how
/// they got there.
async fn seed_graph(pool: &SqlitePool, repository: RepositoryId, count: usize, modules: usize) {
    const KINDS: [&str; 6] = [
        "function",
        "method",
        "type",
        "constant",
        "test",
        "trait_or_interface",
    ];
    /// Rows per INSERT. The fixture is untimed setup, but at half a million rows
    /// a statement-per-row is minutes rather than seconds, and this harness has
    /// to be cheap enough to re-run.
    const CHUNK: usize = 500;
    const COLUMNS: &str = "INSERT INTO code_nodes (id, repository, language, package, \
         source_path, qualified_name, kind, signature_hash, symbol_key, revision, created_at) VALUES ";
    let repo_key = repository.to_string();
    let mut tx = pool.begin().await.expect("begin");

    // `total` rows: one `module` head per module path (so the map's grouping
    // sees the heads a real fold writes), then `count` symbols spread over them.
    let total = modules + count;
    let mut written = 0usize;
    while written < total {
        let rows = CHUNK.min(total - written);
        let values = std::iter::repeat_n(
            "(?, ?, 'rust', NULL, ?, ?, ?, ?, ?, 'benchrev', '2026-01-01T00:00:00Z')",
            rows,
        )
        .collect::<Vec<_>>()
        .join(", ");
        let sql = format!("{COLUMNS}{values}");
        let mut query = sqlx::query(&sql);
        for offset in 0..rows {
            let index = written + offset;
            if index < modules {
                let m = index;
                let qualified = format!("crate::area{}::module{}", m % 32, m);
                query = query
                    .bind(
                        uuid::Uuid::from_u128(
                            0xA000_0000_0000_0000_0000_0000_0000_0000u128 + m as u128,
                        )
                        .to_string(),
                    )
                    .bind(repo_key.clone())
                    .bind(format!("src/area{}/module{}.rs", m % 32, m))
                    .bind(qualified.clone())
                    .bind("module")
                    .bind(Option::<String>::None)
                    .bind(format!("mod:{qualified}"));
            } else {
                let i = index - modules;
                let m = i % modules;
                let qualified = format!("crate::area{}::module{}::symbol_{}", m % 32, m, i);
                query = query
                    .bind(
                        uuid::Uuid::from_u128(
                            0xB000_0000_0000_0000_0000_0000_0000_0000u128 + i as u128,
                        )
                        .to_string(),
                    )
                    .bind(repo_key.clone())
                    .bind(format!("src/area{}/module{}.rs", m % 32, m))
                    .bind(qualified.clone())
                    .bind(KINDS[i % KINDS.len()])
                    // A real fold stores a 64-hex SHA-256 here; the read path
                    // decodes it, so the fixture pays the same per-row cost.
                    .bind(Some(format!("{i:064x}")))
                    .bind(format!("sym:{qualified}"));
            }
        }
        query.execute(&mut *tx).await.expect("insert node chunk");
        written += rows;
    }
    tx.commit().await.expect("commit graph fixture");
}

/// A migrated database in `dir` with the built-in registry registered (what a
/// daemon has after startup) and a graph of `nodes` symbols over `modules`
/// modules.
async fn build_fixture(dir: &Path, nodes: usize, modules: usize) -> SqlitePool {
    let pool = codypendent_knowledge::db::open(&dir.join("codypendent.db"))
        .await
        .expect("open migrated database");
    register_builtins(&pool).await.expect("register builtins");
    seed_graph(&pool, repo(), nodes, modules).await;
    pool
}

/// Every size this file benches, built ONCE per process and shared by every
/// group. Five groups over four sizes would otherwise mean twenty fixtures, and
/// the 500k one alone is half a million rows — setup that dwarfs the
/// measurement, on a machine the measurement already needs to be quiet.
///
/// The tempdirs are leaked deliberately: they must outlive every group, and the
/// process is a benchmark binary that exits immediately afterwards.
fn fixtures(rt: &tokio::runtime::Runtime) -> &'static [(usize, SqlitePool)] {
    static FIXTURES: std::sync::OnceLock<Vec<(usize, SqlitePool)>> = std::sync::OnceLock::new();
    FIXTURES.get_or_init(|| {
        SIZES
            .iter()
            .map(|&(nodes, modules)| {
                let dir = Box::leak(Box::new(tempfile::tempdir().expect("fixture tempdir")));
                (
                    nodes,
                    rt.block_on(build_fixture(dir.path(), nodes, modules)),
                )
            })
            .collect()
    })
}

/// `(symbol nodes, module paths)`. 1k is an ordinary repository; 510,904 is the
/// graph the scan bug actually produced.
const SIZES: [(usize, usize); 4] = [(1_000, 40), (10_000, 200), (100_000, 800), (500_000, 2_000)];

/// The fixture for `nodes`, from the shared set.
fn pool_for(rt: &tokio::runtime::Runtime, nodes: usize) -> &'static SqlitePool {
    &fixtures(rt)
        .iter()
        .find(|(size, _)| *size == nodes)
        .expect("a fixture for this size")
        .1
}

/// The scopes `emit_context` passes: System + the local user scope + this
/// repository.
fn scopes() -> Vec<Scope> {
    vec![
        Scope::System,
        codypendent_knowledge::local_user_scope(),
        Scope::Repository(repo()),
    ]
}

/// **The headline.** One `emit_context` at four graph sizes, registry held
/// constant. A flat curve would mean the registry dominates and the stamped
/// cache is the thing that matters; a rising curve means the uncached repository
/// map is.
fn context_by_graph_size(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("runpath/emit_context");
    group.sample_size(10);
    for (nodes, _) in SIZES {
        let pool = pool_for(&rt, nodes);
        let assembler = ContextAssembler::new();
        // Warm the stamped cache exactly as the second run of a daemon's life
        // finds it, so this measures steady state and not first-call index build.
        rt.block_on(assembler.assemble(pool, repo(), OBJECTIVE, &scopes()))
            .expect("warm the assembler");
        group.bench_with_input(
            BenchmarkId::new("cached_assembler", nodes),
            &nodes,
            |b, _| {
                let scopes = scopes();
                b.iter(|| {
                    rt.block_on(async {
                        let manifest = assembler
                            .assemble(pool, repo(), OBJECTIVE, &scopes)
                            .await
                            .expect("assemble");
                        black_box(manifest.render())
                    })
                });
            },
        );
        // The semantic branch of `emit_context` with no embedder: same core,
        // registry sourced and indexes rebuilt per call. The delta between this
        // and the line above IS what the stamped cache buys.
        group.bench_with_input(BenchmarkId::new("uncached", nodes), &nodes, |b, _| {
            let scopes = scopes();
            b.iter(|| {
                rt.block_on(async {
                    let manifest = assemble_context(pool, repo(), OBJECTIVE, &scopes)
                        .await
                        .expect("assemble");
                    black_box(manifest.render())
                })
            });
        });
    }
    group.finish();
}

/// Step 1 of `assemble_with` on its own, so the breakdown attributes the cost
/// rather than inferring it.
fn repository_map_by_graph_size(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("runpath/repository_map");
    group.sample_size(10);
    for (nodes, _) in SIZES {
        let pool = pool_for(&rt, nodes);
        group.bench_with_input(BenchmarkId::from_parameter(nodes), &nodes, |b, _| {
            b.iter(|| {
                rt.block_on(async {
                    let map = repomap::repository_map(pool, repo()).await.expect("map");
                    black_box(map.render())
                })
            });
        });
    }
    group.finish();
}

/// The raw `SELECT` underneath the map, with no grouping — separating "SQLite
/// hands us the rows" from "we fold them", so an optimisation aimed at the wrong
/// half is visible as such.
fn graph_read_by_size(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("runpath/codegraph_nodes");
    group.sample_size(10);
    for nodes in [10_000usize, 100_000, 500_000] {
        let pool = pool_for(&rt, nodes);
        group.bench_with_input(BenchmarkId::from_parameter(nodes), &nodes, |b, _| {
            b.iter(|| {
                rt.block_on(async {
                    black_box(
                        codypendent_knowledge::codegraph::nodes(pool, repo())
                            .await
                            .expect("nodes")
                            .len(),
                    )
                })
            });
        });
    }
    group.finish();
}

/// **Attribution probe.** `repository_map` needs exactly two of the eight
/// columns `codegraph::nodes` selects, and needs no ordering at all (it groups
/// through a `BTreeMap` and sorts within each group). This prices the three
/// shapes against each other so a change is aimed at whichever half actually
/// costs:
///
/// - `nodes_full` — today: 8 columns, `ORDER BY created_at, id` (no index covers
///   that pair, so SQLite builds a temp B-tree over every row), and one
///   `CodeNode` per row = a UUID parse plus five `String` allocations, all but
///   two of them discarded immediately;
/// - `projection_ordered` — 2 columns, same `ORDER BY`;
/// - `projection_unordered` — 2 columns, no `ORDER BY`.
fn graph_read_shape(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("runpath/graph_read_shape");
    group.sample_size(10);
    for nodes in [100_000usize, 500_000] {
        let pool = pool_for(&rt, nodes);
        let key = repo().to_string();
        group.bench_with_input(BenchmarkId::new("nodes_full", nodes), &nodes, |b, _| {
            b.iter(|| {
                rt.block_on(async {
                    black_box(
                        codypendent_knowledge::codegraph::nodes(pool, repo())
                            .await
                            .expect("nodes")
                            .len(),
                    )
                })
            });
        });
        group.bench_with_input(
            BenchmarkId::new("projection_ordered", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    rt.block_on(async {
                        let rows: Vec<(String, String)> = sqlx::query_as(
                            "SELECT qualified_name, kind FROM code_nodes WHERE repository = ? \
                             ORDER BY created_at ASC, id ASC",
                        )
                        .bind(&key)
                        .fetch_all(pool)
                        .await
                        .expect("projection");
                        black_box(rows.len())
                    })
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("projection_unordered", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    rt.block_on(async {
                        let rows: Vec<(String, String)> = sqlx::query_as(
                            "SELECT qualified_name, kind FROM code_nodes WHERE repository = ?",
                        )
                        .bind(&key)
                        .fetch_all(pool)
                        .await
                        .expect("projection");
                        black_box(rows.len())
                    })
                });
            },
        );
    }
    group.finish();
}

/// **The A/B for the repository-map change, measured side by side.**
///
/// `legacy` reproduces what `repomap::repository_map` did before this change —
/// `codegraph::nodes` (eight columns, `ORDER BY created_at, id`, a full
/// `CodeNode` per row) folded into the same `BTreeMap` grouping — and `current`
/// calls the shipped function. Both render.
///
/// They run in the same criterion group over the same fixture so the comparison
/// survives a machine that is not quiet: whatever the absolute numbers, the two
/// lines saw the same conditions. The grouping half is duplicated here on
/// purpose; a bench that called only the new function would have no before to
/// put beside its after.
fn repository_map_ab(c: &mut Criterion) {
    use codypendent_knowledge::types::CodeNodeKind;
    use std::collections::BTreeMap;

    let rt = runtime();
    let mut group = c.benchmark_group("runpath/repository_map_ab");
    group.sample_size(10);
    for nodes in [10_000usize, 100_000, 500_000] {
        let pool = pool_for(&rt, nodes);

        group.bench_with_input(BenchmarkId::new("legacy", nodes), &nodes, |b, _| {
            b.iter(|| {
                rt.block_on(async {
                    let all = codypendent_knowledge::codegraph::nodes(pool, repo())
                        .await
                        .expect("nodes");
                    // The old grouping, verbatim in shape: every node, keyed by
                    // an owned module path, counted per kind.
                    let mut grouped: BTreeMap<String, (Vec<String>, Vec<String>)> = BTreeMap::new();
                    for node in &all {
                        let qualified = &node.key.qualified_name;
                        let simple = qualified
                            .rsplit("::")
                            .next()
                            .unwrap_or(qualified)
                            .to_owned();
                        let key = match qualified.rfind("::") {
                            Some(at) => &qualified[..at],
                            None => "",
                        };
                        match node.key.kind {
                            CodeNodeKind::Module => {
                                grouped.entry(key.to_owned()).or_default();
                            }
                            CodeNodeKind::Test => {
                                grouped.entry(key.to_owned()).or_default().1.push(simple)
                            }
                            CodeNodeKind::Type
                            | CodeNodeKind::TraitOrInterface
                            | CodeNodeKind::Function
                            | CodeNodeKind::Method
                            | CodeNodeKind::Constant => {
                                grouped.entry(key.to_owned()).or_default().0.push(simple)
                            }
                            _ => {}
                        }
                    }
                    for entry in grouped.values_mut() {
                        entry.0.sort();
                        entry.1.sort();
                    }
                    black_box(grouped.len())
                })
            });
        });

        group.bench_with_input(BenchmarkId::new("current", nodes), &nodes, |b, _| {
            b.iter(|| {
                rt.block_on(async {
                    let map = repomap::repository_map(pool, repo()).await.expect("map");
                    black_box(map.render())
                })
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    context_by_graph_size,
    repository_map_by_graph_size,
    graph_read_by_size,
    graph_read_shape,
    repository_map_ab
);
criterion_main!(benches);
