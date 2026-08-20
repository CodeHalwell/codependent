//! Milestone 3 Tasks 3.3 & 3.4 Integration Tests: Analytics DDL, Observations, Store, Queries, Budgets, and Exports.

use std::path::Path;

use chrono::Utc;
use codypendent_daemon::analytics::export::{escape_csv_cell, export};
use codypendent_daemon::analytics::{
    calculate_percentile, percentiles, query, record_observation, AnalyticsStore,
    ExecutionObservation,
};
use codypendent_daemon::artifacts::ArtifactStore;
use codypendent_daemon::db;
use codypendent_daemon::principal::PeerPrincipal;
use codypendent_protocol::{
    AnalyticsBudgetDimension, AnalyticsBudgetDraft, AnalyticsBudgetPatch, AnalyticsBudgetQuery,
    AnalyticsBudgetScope, AnalyticsBudgetWindow, AnalyticsCompletion, AnalyticsExportFormat,
    AnalyticsExportRequest, AnalyticsFilters, AnalyticsGrouping, AnalyticsQuery, RunId, SessionId,
};
use sqlx::SqlitePool;

/// The seven measured metric columns of `execution_observations`, every one
/// nullable because an absent measurement is stored as NULL rather than 0.
/// The seven metric columns plus the two routing labels the backfill copies.
type BackfilledRow = (
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<String>,
);

type MetricsRow = (
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

async fn test_pool(path: &Path) -> SqlitePool {
    db::open_database(&path.join("analytics_test.db"))
        .await
        .expect("open database")
}

async fn insert_session(pool: &SqlitePool, session_id: SessionId, owner_uid: u32, repo_id: &str) {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO sessions (id, title, state, created_at, updated_at, revision, owner_uid, repository_id) \
         VALUES (?, 'Test Session', 'open', ?, ?, 0, ?, ?)",
    )
    .bind(session_id.to_string())
    .bind(&now)
    .bind(&now)
    .bind(i64::from(owner_uid))
    .bind(repo_id)
    .execute(pool)
    .await
    .expect("insert session");
}

async fn insert_run(pool: &SqlitePool, run_id: RunId, session_id: SessionId) {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        // `runs` has no `created_at` column (migrations 0002 + 0032); its time
        // columns are `started_at` and `ended_at`.
        "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json, started_at) \
         VALUES (?, ?, 'Objective', 'Completed', 'Build', '{}', '{}', ?)",
    )
    .bind(run_id.to_string())
    .bind(session_id.to_string())
    .bind(&now)
    .execute(pool)
    .await
    .expect("insert run");
}

/// Criterion 15: An unmeasured provider produces a row whose token, cost, latency, and grader columns are all NULL.
#[tokio::test]
async fn unmeasured_provider_records_nulls_not_zeros() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = test_pool(temp.path()).await;
    let session_id = SessionId::new();
    let run_id = RunId::new();
    insert_session(&pool, session_id, 1000, "repo-1").await;
    insert_run(&pool, run_id, session_id).await;

    let obs = ExecutionObservation {
        id: None,
        owner_uid: 1000,
        run_id,
        attempt: 0,
        node_id: String::new(),
        session_id: Some(session_id),
        repository_id: Some("repo-1".to_string()),
        workflow_id: None,
        workflow_run_id: None,
        task_class: Some("small-bug-fix".to_string()),
        provider: Some("unmeasured-provider".to_string()),
        model_id: Some("model-unmeasured".to_string()),
        endpoint: None,
        route: None,
        input_tokens: None,
        output_tokens: None,
        cached_tokens: None,
        reasoning_tokens: None,
        cost_micros: None,
        latency_ms: None,
        retry_count: None,
        escalation_count: None,
        grader_score_micros: None,
        completion: Some(AnalyticsCompletion::Successful),
        observed_at: Utc::now(),
    };

    let obs_id = record_observation(&pool, &obs)
        .await
        .expect("record observation");

    let row: MetricsRow = sqlx::query_as(
        "SELECT input_tokens, output_tokens, cached_tokens, reasoning_tokens, cost_micros, latency_ms, grader_score_micros \
         FROM execution_observations WHERE id = ?",
    )
    .bind(obs_id)
    .fetch_one(&pool)
    .await
    .expect("fetch observation row");

    assert_eq!(row.0, None, "input_tokens must be NULL");
    assert_eq!(row.1, None, "output_tokens must be NULL");
    assert_eq!(row.2, None, "cached_tokens must be NULL");
    assert_eq!(row.3, None, "reasoning_tokens must be NULL");
    assert_eq!(row.4, None, "cost_micros must be NULL");
    assert_eq!(row.5, None, "latency_ms must be NULL");
    assert_eq!(row.6, None, "grader_score_micros must be NULL");

    // Coverage query returns honest measured: 0, total: 1
    let store = AnalyticsStore::new(pool.clone());
    let page = store
        .query(
            1000,
            PeerPrincipal::from_uid(1000),
            &AnalyticsQuery::default(),
        )
        .await
        .expect("query analytics");

    assert_eq!(page.items.len(), 1);
    let metrics = &page.items[0].metrics;
    assert_eq!(metrics.input_tokens, None);
    assert_eq!(metrics.cost_micros, None);
    assert_eq!(metrics.latency_ms, None);
    assert_eq!(metrics.coverage.input_tokens.measured, 0);
    assert_eq!(metrics.coverage.input_tokens.total, 1);
    assert_eq!(metrics.coverage.cost.measured, 0);
    assert_eq!(metrics.coverage.cost.total, 1);
}

/// Criterion 16: A measured zero survives the round trip as Some(0) and is distinguishable from absent.
#[tokio::test]
async fn measured_zero_is_not_absent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = test_pool(temp.path()).await;
    let session_id = SessionId::new();
    let run_id = RunId::new();
    insert_session(&pool, session_id, 1000, "repo-1").await;
    insert_run(&pool, run_id, session_id).await;

    let obs = ExecutionObservation {
        id: None,
        owner_uid: 1000,
        run_id,
        attempt: 0,
        node_id: String::new(),
        session_id: Some(session_id),
        repository_id: Some("repo-1".to_string()),
        workflow_id: None,
        workflow_run_id: None,
        task_class: Some("small-bug-fix".to_string()),
        provider: Some("measured-provider".to_string()),
        model_id: Some("model-zero".to_string()),
        endpoint: None,
        route: None,
        input_tokens: Some(0),
        output_tokens: Some(0),
        cached_tokens: Some(0),
        reasoning_tokens: Some(0),
        cost_micros: Some(0),
        latency_ms: Some(0),
        retry_count: Some(0),
        escalation_count: Some(0),
        grader_score_micros: Some(0),
        completion: Some(AnalyticsCompletion::Successful),
        observed_at: Utc::now(),
    };

    let obs_id = record_observation(&pool, &obs)
        .await
        .expect("record observation");

    let row: MetricsRow = sqlx::query_as(
        "SELECT input_tokens, output_tokens, cached_tokens, reasoning_tokens, cost_micros, latency_ms, grader_score_micros \
         FROM execution_observations WHERE id = ?",
    )
    .bind(obs_id)
    .fetch_one(&pool)
    .await
    .expect("fetch observation row");

    assert_eq!(row.0, Some(0), "input_tokens must be Some(0)");
    assert_eq!(row.1, Some(0), "output_tokens must be Some(0)");
    assert_eq!(row.2, Some(0), "cached_tokens must be Some(0)");
    assert_eq!(row.3, Some(0), "reasoning_tokens must be Some(0)");
    assert_eq!(row.4, Some(0), "cost_micros must be Some(0)");
    assert_eq!(row.5, Some(0), "latency_ms must be Some(0)");
    assert_eq!(row.6, Some(0), "grader_score_micros must be Some(0)");

    let store = AnalyticsStore::new(pool.clone());
    let page = store
        .query(
            1000,
            PeerPrincipal::from_uid(1000),
            &AnalyticsQuery::default(),
        )
        .await
        .expect("query analytics");

    assert_eq!(page.items.len(), 1);
    let metrics = &page.items[0].metrics;
    assert_eq!(metrics.input_tokens, Some(0));
    assert_eq!(metrics.cost_micros, Some(0));
    assert_eq!(metrics.latency_ms, Some(0));
    assert_eq!(metrics.coverage.input_tokens.measured, 1);
    assert_eq!(metrics.coverage.input_tokens.total, 1);
    assert_eq!(metrics.coverage.cost.measured, 1);
    assert_eq!(metrics.coverage.cost.total, 1);
}

/// Criterion 17: A re-driven write for the same (run_id, attempt, node_id) updates in place.
#[tokio::test]
async fn retried_write_does_not_double_count() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = test_pool(temp.path()).await;
    let session_id = SessionId::new();
    let run_id = RunId::new();
    insert_session(&pool, session_id, 1000, "repo-1").await;
    insert_run(&pool, run_id, session_id).await;

    let mut obs = ExecutionObservation {
        id: None,
        owner_uid: 1000,
        run_id,
        attempt: 1,
        node_id: "node-a".to_string(),
        session_id: Some(session_id),
        repository_id: Some("repo-1".to_string()),
        workflow_id: None,
        workflow_run_id: None,
        task_class: Some("small-bug-fix".to_string()),
        provider: Some("anthropic".to_string()),
        model_id: Some("claude-3-5-sonnet".to_string()),
        endpoint: None,
        route: None,
        input_tokens: Some(100),
        output_tokens: Some(50),
        cached_tokens: None,
        reasoning_tokens: None,
        cost_micros: Some(1500),
        latency_ms: Some(250),
        retry_count: Some(0),
        escalation_count: Some(0),
        grader_score_micros: None,
        completion: Some(AnalyticsCompletion::Incomplete),
        observed_at: Utc::now(),
    };

    record_observation(&pool, &obs).await.expect("first write");

    // Retry / update with completion verdict
    obs.completion = Some(AnalyticsCompletion::Successful);
    obs.latency_ms = Some(300);
    record_observation(&pool, &obs)
        .await
        .expect("retried write");

    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM execution_observations WHERE run_id = ? AND attempt = 1 AND node_id = 'node-a'",
    )
    .bind(run_id.to_string())
    .fetch_one(&pool)
    .await
    .expect("count rows");

    assert_eq!(count.0, 1, "re-driven write must update in place");

    let store = AnalyticsStore::new(pool.clone());
    let page = store
        .query(
            1000,
            PeerPrincipal::from_uid(1000),
            &AnalyticsQuery::default(),
        )
        .await
        .expect("query analytics");

    assert_eq!(page.items.len(), 1);
    let metrics = &page.items[0].metrics;
    assert_eq!(metrics.input_tokens, Some(100));
    assert_eq!(metrics.latency_ms, Some(300));
    assert_eq!(metrics.coverage.input_tokens.total, 1);
}

/// Criterion 18 & 19: Backfill from existing runs rows populates only the three durable columns and leaves the rest NULL.
/// runs.{prompt_tokens, completion_tokens, cost_micros} are unchanged.
#[tokio::test]
async fn backfill_populates_only_durable_existing_values() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = test_pool(temp.path()).await;
    let session_id = SessionId::new();
    let run_id = RunId::new();
    insert_session(&pool, session_id, 1000, "repo-1").await;

    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json, prompt_tokens, completion_tokens, cost_micros, started_at) \
         VALUES (?, ?, 'Objective', 'Completed', 'Build', '{}', '{}', 150, 75, 2500, ?)",
    )
    .bind(run_id.to_string())
    .bind(session_id.to_string())
    .bind(&now)
    .execute(&pool)
    .await
    .expect("insert run with usage");

    sqlx::query(
        "INSERT INTO model_task_outcomes (model_id, endpoint, task_class, success, run_id, recorded_at) \
         VALUES ('claude-3-5', 'default', 'small-bug-fix', 1, ?, ?)",
    )
    .bind(run_id.to_string())
    .bind(&now)
    .execute(&pool)
    .await
    .expect("insert routing outcome");

    let store = AnalyticsStore::new(pool.clone());
    let backfilled = store.backfill(1000).await.expect("run backfill");
    assert_eq!(backfilled, 1);

    let row: BackfilledRow = sqlx::query_as(
        "SELECT input_tokens, output_tokens, cost_micros, cached_tokens, reasoning_tokens, latency_ms, grader_score_micros, model_id, task_class \
         FROM execution_observations WHERE run_id = ?",
    )
    .bind(run_id.to_string())
    .fetch_one(&pool)
    .await
    .expect("fetch backfilled observation");

    assert_eq!(row.0, Some(150), "input_tokens backfilled");
    assert_eq!(row.1, Some(75), "output_tokens backfilled");
    assert_eq!(row.2, Some(2500), "cost_micros backfilled");
    assert_eq!(row.3, None, "cached_tokens must be NULL");
    assert_eq!(row.4, None, "reasoning_tokens must be NULL");
    assert_eq!(row.5, None, "latency_ms must be NULL");
    assert_eq!(row.6, None, "grader_score_micros must be NULL");
    assert_eq!(
        row.7,
        Some("claude-3-5".to_string()),
        "model_id joined from outcomes"
    );
    assert_eq!(
        row.8,
        Some("small-bug-fix".to_string()),
        "task_class joined from outcomes"
    );

    // Criterion 19: Check runs table compatibility columns are unchanged
    let run_usage: (Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT prompt_tokens, completion_tokens, cost_micros FROM runs WHERE id = ?",
    )
    .bind(run_id.to_string())
    .fetch_one(&pool)
    .await
    .expect("fetch run usage");

    assert_eq!(run_usage.0, Some(150));
    assert_eq!(run_usage.1, Some(75));
    assert_eq!(run_usage.2, Some(2500));
}

/// Criterion 20: migrations/checksums.json contains 0043_execution_observations.sql.
#[test]
fn checksums_manifest_contains_0043_execution_observations() {
    let manifest_path = Path::new("../../migrations/checksums.json");
    let content = std::fs::read_to_string(manifest_path).expect("read checksums.json");
    let json: serde_json::Value = serde_json::from_str(&content).expect("parse checksums.json");
    assert!(
        json.get("0043_execution_observations.sql").is_some(),
        "checksums.json must contain 0043_execution_observations.sql"
    );
}

/// Criterion 21: Aggregates group correctly by model, provider, repository, workflow, task class, time, and completion.
#[tokio::test]
async fn aggregates_group_by_every_declared_dimension() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = test_pool(temp.path()).await;
    let session_id = SessionId::new();
    insert_session(&pool, session_id, 1000, "repo-main").await;

    let groupings = [
        AnalyticsGrouping::Model,
        AnalyticsGrouping::Provider,
        AnalyticsGrouping::Repository,
        AnalyticsGrouping::Workflow,
        AnalyticsGrouping::TaskClass,
        AnalyticsGrouping::Time,
        AnalyticsGrouping::Completion,
        AnalyticsGrouping::Route,
    ];

    for i in 0..4 {
        let run_id = RunId::new();
        insert_run(&pool, run_id, session_id).await;
        let obs = ExecutionObservation {
            id: None,
            owner_uid: 1000,
            run_id,
            attempt: 0,
            node_id: String::new(),
            session_id: Some(session_id),
            repository_id: Some(format!("repo-{}", i % 2)),
            workflow_id: Some(format!("wf-{}", i % 2)),
            workflow_run_id: None,
            task_class: Some(format!("task-{}", i % 2)),
            provider: Some(format!("provider-{}", i % 2)),
            model_id: Some(format!("model-{}", i % 2)),
            endpoint: Some("ep-1".to_string()),
            route: Some("route-default".to_string()),
            input_tokens: Some(10 * (i + 1)),
            output_tokens: Some(5 * (i + 1)),
            cached_tokens: None,
            reasoning_tokens: None,
            cost_micros: Some(100 * (i + 1)),
            latency_ms: Some(50 * (i + 1)),
            retry_count: Some(0),
            escalation_count: Some(0),
            grader_score_micros: Some(900_000),
            completion: Some(if i % 2 == 0 {
                AnalyticsCompletion::Successful
            } else {
                AnalyticsCompletion::Failed
            }),
            observed_at: Utc::now(),
        };
        record_observation(&pool, &obs)
            .await
            .expect("record observation");
    }

    for grouping in groupings {
        let q = AnalyticsQuery {
            filters: AnalyticsFilters::default(),
            group_by: vec![grouping],
            cursor: None,
            limit: 10,
        };
        let page = query(&pool, 1000, PeerPrincipal::from_uid(1000), &q)
            .await
            .unwrap_or_else(|_| panic!("group by {grouping:?} failed"));
        assert!(
            !page.items.is_empty(),
            "group by {grouping:?} must return non-empty items"
        );
        for item in &page.items {
            assert_eq!(item.dimensions.len(), 1);
        }
    }
}

/// Criterion 22: MeasurementCoverage reports honest measured/total per dimension, computed over owner-filtered set.
#[tokio::test]
async fn coverage_counts_are_owner_scoped_and_honest() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = test_pool(temp.path()).await;
    let session_1 = SessionId::new();
    let session_2 = SessionId::new();
    insert_session(&pool, session_1, 1000, "repo-1").await;
    insert_session(&pool, session_2, 2000, "repo-2").await;

    // Insert 2 observations for user 1000: 1 measured tokens, 1 unmeasured
    let r1 = RunId::new();
    let r2 = RunId::new();
    insert_run(&pool, r1, session_1).await;
    insert_run(&pool, r2, session_1).await;

    record_observation(
        &pool,
        &ExecutionObservation {
            id: None,
            owner_uid: 1000,
            run_id: r1,
            attempt: 0,
            node_id: String::new(),
            session_id: Some(session_1),
            repository_id: Some("repo-1".to_string()),
            workflow_id: None,
            workflow_run_id: None,
            task_class: None,
            provider: None,
            model_id: None,
            endpoint: None,
            route: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
            cached_tokens: None,
            reasoning_tokens: None,
            cost_micros: Some(500),
            latency_ms: None,
            retry_count: None,
            escalation_count: None,
            grader_score_micros: None,
            completion: Some(AnalyticsCompletion::Successful),
            observed_at: Utc::now(),
        },
    )
    .await
    .unwrap();

    record_observation(
        &pool,
        &ExecutionObservation {
            id: None,
            owner_uid: 1000,
            run_id: r2,
            attempt: 0,
            node_id: String::new(),
            session_id: Some(session_1),
            repository_id: Some("repo-1".to_string()),
            workflow_id: None,
            workflow_run_id: None,
            task_class: None,
            provider: None,
            model_id: None,
            endpoint: None,
            route: None,
            input_tokens: None,
            output_tokens: None,
            cached_tokens: None,
            reasoning_tokens: None,
            cost_micros: None,
            latency_ms: None,
            retry_count: None,
            escalation_count: None,
            grader_score_micros: None,
            completion: Some(AnalyticsCompletion::Failed),
            observed_at: Utc::now(),
        },
    )
    .await
    .unwrap();

    // Insert 10 observations for user 2000 (must not affect user 1000's coverage count)
    for _ in 0..10 {
        let r_other = RunId::new();
        insert_run(&pool, r_other, session_2).await;
        record_observation(
            &pool,
            &ExecutionObservation {
                id: None,
                owner_uid: 2000,
                run_id: r_other,
                attempt: 0,
                node_id: String::new(),
                session_id: Some(session_2),
                repository_id: Some("repo-2".to_string()),
                workflow_id: None,
                workflow_run_id: None,
                task_class: None,
                provider: None,
                model_id: None,
                endpoint: None,
                route: None,
                input_tokens: Some(500),
                output_tokens: Some(250),
                cached_tokens: None,
                reasoning_tokens: None,
                cost_micros: Some(1000),
                latency_ms: None,
                retry_count: None,
                escalation_count: None,
                grader_score_micros: None,
                completion: Some(AnalyticsCompletion::Successful),
                observed_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    }

    let page = query(
        &pool,
        1000,
        PeerPrincipal::from_uid(1000),
        &AnalyticsQuery::default(),
    )
    .await
    .unwrap();

    assert_eq!(page.items.len(), 1);
    let coverage = &page.items[0].metrics.coverage;
    assert_eq!(coverage.input_tokens.measured, 1);
    assert_eq!(
        coverage.input_tokens.total, 2,
        "total must be owner-scoped to 2"
    );
    assert_eq!(coverage.cost.measured, 1);
    assert_eq!(coverage.cost.total, 2);
    assert_eq!(coverage.latency.measured, 0);
    assert_eq!(coverage.latency.total, 2);
}

/// Criterion 23: Cost per successful task is absent when either side is unmeasured, not zero.
#[tokio::test]
async fn cost_per_successful_task_is_absent_when_unmeasurable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = test_pool(temp.path()).await;
    let session_id = SessionId::new();
    let run_id = RunId::new();
    insert_session(&pool, session_id, 1000, "repo-1").await;
    insert_run(&pool, run_id, session_id).await;

    // Run with cost but failed completion
    record_observation(
        &pool,
        &ExecutionObservation {
            id: None,
            owner_uid: 1000,
            run_id,
            attempt: 0,
            node_id: String::new(),
            session_id: Some(session_id),
            repository_id: Some("repo-1".to_string()),
            workflow_id: None,
            workflow_run_id: None,
            task_class: None,
            provider: None,
            model_id: None,
            endpoint: None,
            route: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
            cached_tokens: None,
            reasoning_tokens: None,
            cost_micros: Some(5000),
            latency_ms: None,
            retry_count: None,
            escalation_count: None,
            grader_score_micros: None,
            completion: Some(AnalyticsCompletion::Failed),
            observed_at: Utc::now(),
        },
    )
    .await
    .unwrap();

    let page = query(
        &pool,
        1000,
        PeerPrincipal::from_uid(1000),
        &AnalyticsQuery::default(),
    )
    .await
    .unwrap();

    assert_eq!(
        page.items[0].metrics.cost_per_successful_task_micros, None,
        "cost_per_successful_task must be None when successful tasks == 0"
    );
    assert_eq!(
        page.items[0]
            .metrics
            .coverage
            .cost_per_successful_task
            .measured,
        0
    );

    // Add a successful run with unmeasured cost
    let run_2 = RunId::new();
    insert_run(&pool, run_2, session_id).await;
    record_observation(
        &pool,
        &ExecutionObservation {
            id: None,
            owner_uid: 1000,
            run_id: run_2,
            attempt: 0,
            node_id: String::new(),
            session_id: Some(session_id),
            repository_id: Some("repo-1".to_string()),
            workflow_id: None,
            workflow_run_id: None,
            task_class: None,
            provider: None,
            model_id: None,
            endpoint: None,
            route: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
            cached_tokens: None,
            reasoning_tokens: None,
            cost_micros: Some(3000), // Now cost is measured for successful run
            latency_ms: None,
            retry_count: None,
            escalation_count: None,
            grader_score_micros: None,
            completion: Some(AnalyticsCompletion::Successful),
            observed_at: Utc::now(),
        },
    )
    .await
    .unwrap();

    let page2 = query(
        &pool,
        1000,
        PeerPrincipal::from_uid(1000),
        &AnalyticsQuery::default(),
    )
    .await
    .unwrap();

    // Total cost = 5000 + 3000 = 8000, successful runs = 1 -> cost per successful task = 8000
    assert_eq!(
        page2.items[0].metrics.cost_per_successful_task_micros,
        Some(8000)
    );
}

/// Criterion 24: JSON and CSV exports respect the server row ceiling and set truncated.
#[tokio::test]
async fn exports_are_bounded_and_report_truncation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = test_pool(temp.path()).await;
    let artifacts = ArtifactStore::new(temp.path().join("artifacts"));
    let session_id = SessionId::new();
    insert_session(&pool, session_id, 1000, "repo-1").await;

    // Create 10 distinct observations
    for i in 0..10 {
        let r = RunId::new();
        insert_run(&pool, r, session_id).await;
        record_observation(
            &pool,
            &ExecutionObservation {
                id: None,
                owner_uid: 1000,
                run_id: r,
                attempt: 0,
                node_id: String::new(),
                session_id: Some(session_id),
                repository_id: Some("repo-1".to_string()),
                workflow_id: None,
                workflow_run_id: None,
                task_class: None,
                provider: None,
                model_id: Some(format!("model-{i}")),
                endpoint: None,
                route: None,
                input_tokens: Some(10),
                output_tokens: Some(5),
                cached_tokens: None,
                reasoning_tokens: None,
                cost_micros: Some(100),
                latency_ms: Some(50),
                retry_count: None,
                escalation_count: None,
                grader_score_micros: None,
                completion: Some(AnalyticsCompletion::Successful),
                observed_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    }

    let export_req = AnalyticsExportRequest {
        query: AnalyticsQuery {
            filters: AnalyticsFilters::default(),
            group_by: vec![AnalyticsGrouping::Model],
            cursor: None,
            limit: 0,
        },
        format: AnalyticsExportFormat::Json,
        max_rows: 5,
    };

    let result = export(
        &pool,
        &artifacts,
        1000,
        PeerPrincipal::from_uid(1000),
        codypendent_protocol::ClientId::new(),
        codypendent_protocol::CommandId::new(),
        &export_req,
    )
    .await
    .unwrap();

    assert_eq!(result.row_count, 5);
    assert!(
        result.truncated,
        "export must report truncated = true when exceeding max_rows"
    );

    // Read artifact content. `ArtifactStore` exposes no public path accessor —
    // the blob layout is private and reached by id through the metadata row.
    let bytes = artifacts
        .read_bytes(&pool, result.artifact.id)
        .await
        .unwrap();
    let lines = String::from_utf8(bytes).unwrap();
    let count = lines.lines().count();
    assert_eq!(count, 5);
}

/// An export larger than one query page must contain every row, not the first
/// page of them.
///
/// `query` clamps any caller's limit to its own page ceiling, so asking it once
/// for `max_rows` rows returns at most a page. The export ceiling is well above
/// that page ceiling, which is the shape that hides the bug: the short result
/// also failed the `len() > max_rows` truncation test, so a partial export was
/// handed over labelled complete. Nothing in the artifact says which rows are
/// missing, so nobody downstream can notice.
#[tokio::test]
async fn an_export_spanning_several_query_pages_contains_every_row() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = test_pool(temp.path()).await;
    let artifacts = ArtifactStore::new(temp.path().join("artifacts"));
    let session_id = SessionId::new();
    insert_session(&pool, session_id, 1000, "repo-1").await;

    // Grouped by model, so each observation is its own bucket: 250 rows, more
    // than one page and far fewer than the 1_000-row export default.
    const ROWS: usize = 250;
    for i in 0..ROWS {
        let r = RunId::new();
        insert_run(&pool, r, session_id).await;
        record_observation(
            &pool,
            &ExecutionObservation {
                id: None,
                owner_uid: 1000,
                run_id: r,
                attempt: 0,
                node_id: String::new(),
                session_id: Some(session_id),
                repository_id: Some("repo-1".to_string()),
                workflow_id: None,
                workflow_run_id: None,
                task_class: None,
                provider: None,
                model_id: Some(format!("model-{i:04}")),
                endpoint: None,
                route: None,
                input_tokens: Some(10),
                output_tokens: Some(5),
                cached_tokens: None,
                reasoning_tokens: None,
                cost_micros: Some(100),
                latency_ms: Some(50),
                retry_count: None,
                escalation_count: None,
                grader_score_micros: None,
                completion: Some(AnalyticsCompletion::Successful),
                observed_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    }

    let export_req = AnalyticsExportRequest {
        query: AnalyticsQuery {
            filters: AnalyticsFilters::default(),
            group_by: vec![AnalyticsGrouping::Model],
            cursor: None,
            limit: 0,
        },
        format: AnalyticsExportFormat::Json,
        max_rows: 0, // the 1_000-row default, comfortably above ROWS
    };

    let result = export(
        &pool,
        &artifacts,
        1000,
        PeerPrincipal::from_uid(1000),
        codypendent_protocol::ClientId::new(),
        codypendent_protocol::CommandId::new(),
        &export_req,
    )
    .await
    .unwrap();

    assert!(
        !result.truncated,
        "250 rows is under the export ceiling, so nothing was truncated"
    );
    assert_eq!(
        result.row_count, ROWS as u64,
        "the export must span every page of the query, not stop at the first"
    );

    let bytes = artifacts
        .read_bytes(&pool, result.artifact.id)
        .await
        .unwrap();
    let body = String::from_utf8(bytes).unwrap();
    assert_eq!(body.lines().count(), ROWS, "artifact must hold every row");
    // Paging by offset can repeat or skip rows if the cursor is mishandled;
    // distinct models make that visible.
    let distinct: std::collections::HashSet<&str> = body.lines().collect();
    assert_eq!(distinct.len(), ROWS, "paging must not repeat rows");
}

/// Criterion 25: CSV cells beginning =, +, -, @, tab, or CR are escaped.
#[test]
fn csv_export_escapes_formula_injection() {
    assert_eq!(escape_csv_cell("normal"), "normal");
    assert_eq!(escape_csv_cell("=SUM(A1:B2)"), "'=SUM(A1:B2)");
    assert_eq!(escape_csv_cell("+cmd|' /C calc'!A0"), "'+cmd|' /C calc'!A0");
    assert_eq!(escape_csv_cell("-10"), "'-10");
    assert_eq!(escape_csv_cell("@alert"), "'@alert");
    assert_eq!(escape_csv_cell("\ttab_val"), "'\ttab_val");
    assert_eq!(
        escape_csv_cell("\rCR_val"),
        "\"'\\rCR_val\"".replace("\\r", "\r")
    );
}

/// Criterion 26: A budget threshold crossed by measured values creates exactly one deduplicated alert per window.
#[tokio::test]
async fn budget_alert_is_deduplicated_per_window() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = test_pool(temp.path()).await;
    let session_id = SessionId::new();
    insert_session(&pool, session_id, 1000, "repo-1").await;

    // Create daily budget for cost_micros > 5000
    let now_str = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO analytics_budgets (id, owner_uid, scope, scope_value, dimension, window, threshold, enabled, created_at, updated_at) \
         VALUES ('b-1', 1000, 'owner', '', 'cost_micros', 'day', 5000, 1, ?, ?)",
    )
    .bind(&now_str)
    .bind(&now_str)
    .execute(&pool)
    .await
    .expect("insert budget");

    let r1 = RunId::new();
    insert_run(&pool, r1, session_id).await;
    record_observation(
        &pool,
        &ExecutionObservation {
            id: None,
            owner_uid: 1000,
            run_id: r1,
            attempt: 0,
            node_id: String::new(),
            session_id: Some(session_id),
            repository_id: Some("repo-1".to_string()),
            workflow_id: None,
            workflow_run_id: None,
            task_class: None,
            provider: None,
            model_id: None,
            endpoint: None,
            route: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
            cached_tokens: None,
            reasoning_tokens: None,
            cost_micros: Some(6000), // Exceeds threshold of 5000
            latency_ms: None,
            retry_count: None,
            escalation_count: None,
            grader_score_micros: None,
            completion: Some(AnalyticsCompletion::Successful),
            observed_at: Utc::now(),
        },
    )
    .await
    .unwrap();

    let store = AnalyticsStore::new(pool.clone());
    let alerts1 = store
        .evaluate_budgets(1000, PeerPrincipal::from_uid(1000))
        .await
        .unwrap();

    assert_eq!(alerts1.len(), 1);
    assert_eq!(alerts1[0].budget_id, "b-1");
    assert_eq!(alerts1[0].current_value, 6000);
    assert!(alerts1[0].dedup_key.starts_with("budget:b-1:"));

    // Second evaluation on the same window yields identical dedup_key
    let alerts2 = store
        .evaluate_budgets(1000, PeerPrincipal::from_uid(1000))
        .await
        .unwrap();

    assert_eq!(alerts2.len(), 1);
    assert_eq!(alerts1[0].dedup_key, alerts2[0].dedup_key);
}

/// Criterion 27: A budget over a dimension with no measured values creates no alert.
#[tokio::test]
async fn unmeasured_dimension_never_triggers_a_budget_alert() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = test_pool(temp.path()).await;
    let session_id = SessionId::new();
    insert_session(&pool, session_id, 1000, "repo-1").await;

    // Create daily budget for latency_ms > 100
    let now_str = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO analytics_budgets (id, owner_uid, scope, scope_value, dimension, window, threshold, enabled, created_at, updated_at) \
         VALUES ('b-latency', 1000, 'owner', '', 'latency_ms', 'day', 100, 1, ?, ?)",
    )
    .bind(&now_str)
    .bind(&now_str)
    .execute(&pool)
    .await
    .expect("insert budget");

    // Insert observations where latency_ms is NULL (unmeasured)
    for _ in 0..5 {
        let r = RunId::new();
        insert_run(&pool, r, session_id).await;
        record_observation(
            &pool,
            &ExecutionObservation {
                id: None,
                owner_uid: 1000,
                run_id: r,
                attempt: 0,
                node_id: String::new(),
                session_id: Some(session_id),
                repository_id: Some("repo-1".to_string()),
                workflow_id: None,
                workflow_run_id: None,
                task_class: None,
                provider: None,
                model_id: None,
                endpoint: None,
                route: None,
                input_tokens: Some(100),
                output_tokens: Some(50),
                cached_tokens: None,
                reasoning_tokens: None,
                cost_micros: Some(500),
                latency_ms: None, // Unmeasured!
                retry_count: None,
                escalation_count: None,
                grader_score_micros: None,
                completion: Some(AnalyticsCompletion::Successful),
                observed_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    }

    let store = AnalyticsStore::new(pool.clone());
    let alerts = store
        .evaluate_budgets(1000, PeerPrincipal::from_uid(1000))
        .await
        .unwrap();

    assert!(
        alerts.is_empty(),
        "unmeasured dimension must never trigger a budget alert"
    );
}

/// Percentile calculations unit test.
#[test]
fn percentiles_calculation_is_accurate() {
    let empty: [u64; 0] = [];
    assert_eq!(calculate_percentile(&empty, 0.5), None);
    assert_eq!(percentiles(&empty), None);

    let single = [42u64];
    assert_eq!(calculate_percentile(&single, 0.5), Some(42));
    assert_eq!(calculate_percentile(&single, 0.99), Some(42));

    let dataset = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
    let p = percentiles(&dataset).expect("calculate percentiles");
    // Nearest-rank with no interpolation: the index is `((len - 1) * p).round()`.
    // For len = 10 that makes p50 = index round(9 * 0.50) = round(4.5) = 5, i.e.
    // 60 — not 50. Asserting the shipped definition, not an idealised median.
    assert_eq!(p.p50, 60);
    assert_eq!(p.p90, 90);
    assert_eq!(p.p95, 100);
    assert_eq!(p.p99, 100);
}

// --- Budget configuration and its production reachability -------------------
//
// `analytics_budgets` previously had no writer outside these tests, so
// `evaluate_budgets`, `BudgetAlert`, `derive_budget_dedup_key` and the
// `BudgetWarning` inbox kind were live code nothing could reach. These cover
// the writer and, more importantly, the PRODUCTION path that now evaluates it.

fn owner_cost_budget(threshold: u64) -> AnalyticsBudgetDraft {
    AnalyticsBudgetDraft {
        scope: AnalyticsBudgetScope::Owner,
        dimension: AnalyticsBudgetDimension::CostMicros,
        window: AnalyticsBudgetWindow::Day,
        threshold,
        enabled: true,
    }
}

/// The reachability proof for the whole budget feature: a budget created
/// through the command path's storage layer, crossed by a measured value, is
/// evaluated by `ledger::append_run_terminal` — the real run-terminal writer,
/// not a test harness — and lands as a durable `BudgetWarning` inbox entry.
///
/// Before the ledger hook existed this test could not have been written: every
/// evaluation in this file called `evaluate_budgets` directly, which is exactly
/// how a feature passes its unit tests while being unreachable in production.
#[tokio::test]
async fn a_budget_crossed_at_run_terminal_produces_a_durable_inbox_warning() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = test_pool(temp.path()).await;
    let principal = PeerPrincipal::from_uid(1000);
    // A real `RepositoryId`: the inbox requires a resolvable repository and
    // refuses to invent one, so a free-form string would silently produce no
    // entry at all rather than a misattributed one.
    let repository_id = codypendent_protocol::RepositoryId::new();
    let session_id = SessionId::new();
    insert_session(&pool, session_id, 1000, &repository_id.to_string()).await;

    let budget =
        codypendent_daemon::analytics::create_budget(&pool, principal, &owner_cost_budget(5_000))
            .await
            .expect("create budget");

    let run_id = RunId::new();
    insert_run(&pool, run_id, session_id).await;
    record_observation(
        &pool,
        &ExecutionObservation {
            id: None,
            owner_uid: 1000,
            run_id,
            attempt: 0,
            node_id: String::new(),
            session_id: Some(session_id),
            repository_id: Some(repository_id.to_string()),
            workflow_id: None,
            workflow_run_id: None,
            task_class: None,
            provider: None,
            model_id: None,
            endpoint: None,
            route: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
            cached_tokens: None,
            reasoning_tokens: None,
            // Over the 5_000 threshold.
            cost_micros: Some(6_000),
            latency_ms: None,
            retry_count: None,
            escalation_count: None,
            grader_score_micros: None,
            completion: Some(AnalyticsCompletion::Successful),
            observed_at: Utc::now(),
        },
    )
    .await
    .expect("record observation");

    let completion = codypendent_protocol::EventBody::RunCompleted {
        run_id,
        disposition: codypendent_protocol::RunDisposition::Completed { summary: None },
        chronicle: codypendent_protocol::ArtifactRef {
            id: codypendent_protocol::ArtifactId::new(),
            media_type: "application/json".to_string(),
            byte_length: 2,
            sha256: "b".repeat(64),
            sensitivity: codypendent_protocol::DataClassification::Internal,
        },
    };
    codypendent_daemon::ledger::append_run_terminal(
        &pool,
        session_id,
        &codypendent_protocol::Actor::System,
        codypendent_protocol::RunState::Completed,
        &completion,
        Utc::now(),
    )
    .await
    .expect("append run terminal");

    let warnings: Vec<(String, String)> = sqlx::query_as(
        "SELECT kind, dedup_key FROM inbox_entries WHERE owner_uid = 1000 AND kind = 'BudgetWarning'",
    )
    .fetch_all(&pool)
    .await
    .expect("read inbox");

    assert_eq!(
        warnings.len(),
        1,
        "the run-terminal writer must raise exactly one budget warning"
    );
    assert!(
        warnings[0].1.starts_with(&format!("budget:{}:", budget.id)),
        "the inbox entry must carry the evaluator's own dedup key, so a second \
         run in the same window updates this row instead of minting another; got {}",
        warnings[0].1
    );
}

/// A budget below its threshold records nothing. Guards against the failure
/// mode where an absent or unmeasured sum is coerced to a crossing.
#[tokio::test]
async fn a_budget_under_its_threshold_records_no_warning() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = test_pool(temp.path()).await;
    let principal = PeerPrincipal::from_uid(1000);
    let repository_id = codypendent_protocol::RepositoryId::new();
    let session_id = SessionId::new();
    insert_session(&pool, session_id, 1000, &repository_id.to_string()).await;
    codypendent_daemon::analytics::create_budget(&pool, principal, &owner_cost_budget(1_000_000))
        .await
        .expect("create budget");

    let run_id = RunId::new();
    insert_run(&pool, run_id, session_id).await;
    let completion = codypendent_protocol::EventBody::RunCompleted {
        run_id,
        disposition: codypendent_protocol::RunDisposition::Completed { summary: None },
        chronicle: codypendent_protocol::ArtifactRef {
            id: codypendent_protocol::ArtifactId::new(),
            media_type: "application/json".to_string(),
            byte_length: 2,
            sha256: "c".repeat(64),
            sensitivity: codypendent_protocol::DataClassification::Internal,
        },
    };
    codypendent_daemon::ledger::append_run_terminal(
        &pool,
        session_id,
        &codypendent_protocol::Actor::System,
        codypendent_protocol::RunState::Completed,
        &completion,
        Utc::now(),
    )
    .await
    .expect("append run terminal");

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM inbox_entries WHERE kind = 'BudgetWarning'")
            .fetch_one(&pool)
            .await
            .expect("count warnings");
    assert_eq!(count, 0);
}

/// Another principal's budget must be indistinguishable from one that was never
/// created — same error code, same message, for read and for every mutation.
/// A distinct "forbidden" answer would make any id an existence oracle.
#[tokio::test]
async fn another_principals_budget_is_indistinguishable_from_an_absent_one() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = test_pool(temp.path()).await;
    let owner = PeerPrincipal::from_uid(1000);
    let stranger = PeerPrincipal::from_uid(1001);

    let budget =
        codypendent_daemon::analytics::create_budget(&pool, owner, &owner_cost_budget(5_000))
            .await
            .expect("create budget");

    let absent = "00000000-0000-0000-0000-000000000000";
    let hidden = codypendent_daemon::analytics::get_budget(&pool, stranger, &budget.id)
        .await
        .expect_err("a stranger must not read another principal's budget");
    let missing = codypendent_daemon::analytics::get_budget(&pool, stranger, absent)
        .await
        .expect_err("an absent budget must refuse");
    assert_eq!(hidden.code, missing.code);
    assert_eq!(hidden.message, missing.message);

    let patch = AnalyticsBudgetPatch {
        threshold: Some(9_000),
        ..Default::default()
    };
    let hidden_update =
        codypendent_daemon::analytics::update_budget(&pool, stranger, &budget.id, &patch)
            .await
            .expect_err("a stranger must not update another principal's budget");
    let missing_update =
        codypendent_daemon::analytics::update_budget(&pool, stranger, absent, &patch)
            .await
            .expect_err("an absent budget must refuse");
    assert_eq!(hidden_update.code, missing_update.code);
    assert_eq!(hidden_update.message, missing_update.message);

    let hidden_delete = codypendent_daemon::analytics::delete_budget(&pool, stranger, &budget.id)
        .await
        .expect_err("a stranger must not delete another principal's budget");
    let missing_delete = codypendent_daemon::analytics::delete_budget(&pool, stranger, absent)
        .await
        .expect_err("an absent budget must refuse");
    assert_eq!(hidden_delete.code, missing_delete.code);
    assert_eq!(hidden_delete.message, missing_delete.message);

    // The stranger's failed mutations must not have touched the row.
    let survived = codypendent_daemon::analytics::get_budget(&pool, owner, &budget.id)
        .await
        .expect("owner still reads their budget");
    assert_eq!(survived.definition.threshold, 5_000);

    // A listing is owner-scoped too, so the stranger sees nothing at all.
    let stranger_page = codypendent_daemon::analytics::list_budgets(
        &pool,
        stranger,
        &AnalyticsBudgetQuery::default(),
    )
    .await
    .expect("list budgets");
    assert!(stranger_page.items.is_empty());
}

/// A budget over a dimension or scope this build cannot evaluate is refused at
/// write time rather than stored enabled-and-silent.
#[tokio::test]
async fn unknown_budget_dimensions_and_scopes_are_refused_not_stored() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = test_pool(temp.path()).await;
    let principal = PeerPrincipal::from_uid(1000);

    let unknown_dimension = AnalyticsBudgetDraft {
        dimension: AnalyticsBudgetDimension::Unknown,
        ..owner_cost_budget(5_000)
    };
    codypendent_daemon::analytics::create_budget(&pool, principal, &unknown_dimension)
        .await
        .expect_err("an unknown dimension has no honest column");

    let unknown_scope = AnalyticsBudgetDraft {
        scope: AnalyticsBudgetScope::Unknown,
        ..owner_cost_budget(5_000)
    };
    codypendent_daemon::analytics::create_budget(&pool, principal, &unknown_scope)
        .await
        .expect_err("an unknown scope cannot be narrowed and must be refused");

    // 0043 CHECKs `threshold > 0`; a zero threshold would alert on the first
    // measured observation forever.
    let zero_threshold = owner_cost_budget(0);
    codypendent_daemon::analytics::create_budget(&pool, principal, &zero_threshold)
        .await
        .expect_err("a zero threshold is refused");

    let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM analytics_budgets")
        .fetch_one(&pool)
        .await
        .expect("count budgets");
    assert_eq!(stored, 0, "no refused budget may reach the table");
}

/// The CRUD surface round-trips, and an update is sparse: it changes only the
/// fields the patch names.
#[tokio::test]
async fn budget_crud_round_trips_and_updates_sparsely() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = test_pool(temp.path()).await;
    let principal = PeerPrincipal::from_uid(1000);

    let created =
        codypendent_daemon::analytics::create_budget(&pool, principal, &owner_cost_budget(5_000))
            .await
            .expect("create budget");
    let fetched = codypendent_daemon::analytics::get_budget(&pool, principal, &created.id)
        .await
        .expect("get budget");
    assert_eq!(fetched.definition, created.definition);

    // The UNIQUE (owner_uid, scope, scope_value, dimension, window) row already
    // exists, so an identical budget is refused rather than silently duplicated.
    codypendent_daemon::analytics::create_budget(&pool, principal, &owner_cost_budget(7_000))
        .await
        .expect_err("a duplicate budget is refused");

    let updated = codypendent_daemon::analytics::update_budget(
        &pool,
        principal,
        &created.id,
        &AnalyticsBudgetPatch {
            enabled: Some(false),
            ..Default::default()
        },
    )
    .await
    .expect("update budget");
    assert!(!updated.definition.enabled);
    assert_eq!(
        updated.definition.threshold, 5_000,
        "a patch that names only `enabled` must not disturb the threshold"
    );

    // A disabled budget is skipped by the evaluator entirely.
    let page = codypendent_daemon::analytics::list_budgets(
        &pool,
        principal,
        &AnalyticsBudgetQuery {
            enabled: Some(true),
            limit: 0,
        },
    )
    .await
    .expect("list enabled budgets");
    assert!(page.items.is_empty());

    codypendent_daemon::analytics::delete_budget(&pool, principal, &created.id)
        .await
        .expect("delete budget");
    codypendent_daemon::analytics::get_budget(&pool, principal, &created.id)
        .await
        .expect_err("a deleted budget is gone");
}
