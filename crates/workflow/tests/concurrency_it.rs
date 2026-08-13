//! Outcome 15: the ready frontier really runs concurrently, and the durable
//! record proves it.
//!
//! The review measured the old behaviour directly — three independent
//! isolated-worktree agent nodes with `maximum_agents: 3` produced
//! `w1 t=…891.033`, `w2 t=…891.113`, `w3 t=…891.183`, strictly ordered and never
//! overlapping, so the wall-clock for three "parallel" workers was the sum rather
//! than the max. This test asserts the opposite from the same evidence the review
//! used: the `started_at` / `ended_at` timestamps `workflow_nodes` persists.
//!
//! Reading the timestamps out of SQLite (rather than instrumenting the executor)
//! is deliberate: it is the same durable record the graph view, the board, and a
//! post-mortem read, so a regression here is visible to a user, not only to a
//! test double.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use codypendent_workflow::{
    compile_yaml, db, NodeContext, NodeExecutor, NodeOutcome, NodeState, WorkflowDriver,
    WorkflowRunState, WorkflowStore,
};
use serde_json::json;

/// Three independent workers and a join, the canonical delegation shape.
fn manifest(maximum_agents: u32) -> String {
    format!(
        "\
schema_version: 1
id: fanout
version: 1
budget:
  maximum_agents: {maximum_agents}
steps:
  - id: w1
    tool: repository.test
  - id: w2
    tool: repository.test
  - id: w3
    tool: repository.test
  - id: synth
    depends_on: [w1, w2, w3]
    tool: repository.test
"
    )
}

/// Each node occupies real wall time, so overlapping timestamps mean overlapping
/// execution and not merely two writes in the same millisecond.
struct SlowExecutor {
    duration: Duration,
    peak: Mutex<(usize, usize)>,
}

impl SlowExecutor {
    fn new(duration: Duration) -> Self {
        Self {
            duration,
            peak: Mutex::new((0, 0)),
        }
    }

    fn peak(&self) -> usize {
        self.peak.lock().unwrap().1
    }
}

#[async_trait]
impl NodeExecutor for SlowExecutor {
    async fn execute(&self, _ctx: NodeContext<'_>) -> NodeOutcome {
        {
            let mut peak = self.peak.lock().unwrap();
            peak.0 += 1;
            peak.1 = peak.1.max(peak.0);
        }
        tokio::time::sleep(self.duration).await;
        self.peak.lock().unwrap().0 -= 1;
        NodeOutcome::completed()
    }
}

/// `(node_id, started_at, ended_at)` straight out of the durable node table.
async fn intervals(
    pool: &sqlx::SqlitePool,
    run_id: &str,
) -> Vec<(String, DateTime<Utc>, DateTime<Utc>)> {
    let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT node_id, started_at, ended_at FROM workflow_nodes \
         WHERE workflow_run_id = ? ORDER BY node_id",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(id, started, ended)| {
            let parse = |value: Option<String>, what: &str| {
                DateTime::parse_from_rfc3339(&value.unwrap_or_else(|| panic!("{id} has no {what}")))
                    .unwrap()
                    .with_timezone(&Utc)
            };
            let started = parse(started, "started_at");
            let ended = parse(ended, "ended_at");
            (id, started, ended)
        })
        .collect()
}

fn overlaps(
    a: &(String, DateTime<Utc>, DateTime<Utc>),
    b: &(String, DateTime<Utc>, DateTime<Utc>),
) -> bool {
    a.1 < b.2 && b.1 < a.2
}

#[tokio::test]
async fn independent_workers_overlap_in_the_durable_record() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("codypendent.db")).await.unwrap();
    let compiled = compile_yaml(&manifest(3)).unwrap();
    let run_id = WorkflowStore::new()
        .create_run(&pool, &compiled, None, &json!({}), None)
        .await
        .unwrap();

    let executor = SlowExecutor::new(Duration::from_millis(200));
    let state = WorkflowDriver::new()
        .run(&pool, &run_id, &compiled, &executor)
        .await
        .unwrap();
    assert_eq!(state, WorkflowRunState::Completed);
    assert_eq!(executor.peak(), 3, "three workers ran at once");

    let intervals = intervals(&pool, &run_id).await;
    let worker = |id: &str| {
        intervals
            .iter()
            .find(|(node, _, _)| node == id)
            .unwrap_or_else(|| panic!("{id} has no row"))
            .clone()
    };
    let (w1, w2, w3) = (worker("w1"), worker("w2"), worker("w3"));

    // Every pair of independent workers overlaps. Under the old sequential
    // frontier each interval ended before the next began, so all three of these
    // fail.
    assert!(overlaps(&w1, &w2), "w1 {w1:?} must overlap w2 {w2:?}");
    assert!(overlaps(&w1, &w3), "w1 {w1:?} must overlap w3 {w3:?}");
    assert!(overlaps(&w2, &w3), "w2 {w2:?} must overlap w3 {w3:?}");

    // The join still runs strictly after all three — concurrency must not weaken
    // the dependency contract.
    let synth = worker("synth");
    for worker in [&w1, &w2, &w3] {
        assert!(
            worker.2 <= synth.1,
            "synth {synth:?} must start after {worker:?} ends"
        );
    }

    // Three workers of 200ms each finish in well under their 600ms sum.
    let span = synth.1 - w1.1.min(w2.1).min(w3.1);
    assert!(
        span.num_milliseconds() < 500,
        "three 200ms workers should not take their sum: {span}"
    );

    let snapshot = WorkflowStore::new()
        .snapshot(&pool, &run_id)
        .await
        .unwrap()
        .unwrap();
    assert!(snapshot
        .nodes
        .iter()
        .all(|node| node.state == NodeState::Completed));
}

/// `maximum_agents` is the enforcement point, not documentation: with a cap of 2
/// the third worker waits for a free slot, so exactly one of the three pairs
/// fails to overlap.
#[tokio::test]
async fn maximum_agents_serialises_the_excess_worker() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("codypendent.db")).await.unwrap();
    let compiled = compile_yaml(&manifest(2)).unwrap();
    assert_eq!(compiled.max_concurrency(), 2);
    let run_id = WorkflowStore::new()
        .create_run(&pool, &compiled, None, &json!({}), None)
        .await
        .unwrap();

    let executor = SlowExecutor::new(Duration::from_millis(200));
    let state = WorkflowDriver::new()
        .run(&pool, &run_id, &compiled, &executor)
        .await
        .unwrap();
    assert_eq!(state, WorkflowRunState::Completed);
    assert_eq!(executor.peak(), 2, "the cap is enforced, not advisory");

    let intervals = intervals(&pool, &run_id).await;
    let workers: Vec<_> = intervals
        .iter()
        .filter(|(id, _, _)| id.starts_with('w'))
        .cloned()
        .collect();
    // At least one pair must NOT overlap: three mutually overlapping intervals
    // would mean three workers ran at once under a cap of 2. (Pairs at the
    // hand-off boundary can brush against each other by a fraction of a
    // millisecond, so counting overlapping pairs exactly would be flaky; the
    // property that matters is that no THREE were ever live together.)
    let disjoint = workers
        .iter()
        .enumerate()
        .flat_map(|(i, a)| workers[i + 1..].iter().map(move |b| (a, b)))
        .filter(|(a, b)| !overlaps(a, b))
        .count();
    assert!(
        disjoint >= 1,
        "with a cap of 2 the third worker must wait for a free slot: {workers:?}"
    );
    // The excess worker starts only after one of the first two has finished.
    let last = workers
        .iter()
        .max_by_key(|(_, started, _)| *started)
        .unwrap();
    assert!(
        workers
            .iter()
            .any(|worker| worker.0 != last.0 && worker.2 <= last.1),
        "the last worker started before any slot was freed: {workers:?}"
    );
}
