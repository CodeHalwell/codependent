//! Measured execution observations, owner-scoped aggregation, and budgets.
//!
//! # Invariants (conventions §8, M3 guide §3, §4, §6):
//! 1. **Absent measurements stay absent**: Every metric is [`Option`]. `None`
//!    means "not measured" and is stored as SQL `NULL`, never a fabricated 0.
//! 2. **Honest coverage**: `MeasurementCoverage { measured, total }` is computed
//!    strictly over the owner-filtered set — never unfiltered or zero-coerced.
//! 3. **Owner isolation**: Authorization is part of every query's seek predicate
//!    before grouping, counting, or pagination. Cursors are principal-bound.
//! 4. **No Oracle leakage**: Filters on unowned repositories or workflows narrow
//!    to empty results rather than returning an error.

pub mod export;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, Datelike, Utc};
use codypendent_protocol::{
    AnalyticsBucket, AnalyticsBudget, AnalyticsBudgetDimension, AnalyticsBudgetDraft,
    AnalyticsBudgetPage, AnalyticsBudgetPatch, AnalyticsBudgetQuery, AnalyticsBudgetScope,
    AnalyticsBudgetWindow, AnalyticsCompletion, AnalyticsDimensionCoverage, AnalyticsGrouping,
    AnalyticsMetrics, AnalyticsPage, AnalyticsQuery, CodypendentError, MeasurementCoverage,
    PageCursor, RunId, SessionId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{QueryBuilder, Row, Sqlite, SqliteConnection, SqlitePool};

use crate::principal::PeerPrincipal;

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 200;
const CURSOR_VERSION: u8 = 1;

/// Errors arising from analytics queries, exports, or storage.
#[derive(Debug, thiserror::Error)]
pub enum AnalyticsError {
    #[error("invalid or stale analytics cursor")]
    InvalidCursor,
    #[error("unsupported grouping requested")]
    UnsupportedGrouping,
    #[error("unsupported export format requested")]
    UnsupportedFormat,
    #[error("database query failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("invalid analytics data: {0}")]
    InvalidData(String),
}

/// A normalized, measured execution observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionObservation {
    pub id: Option<i64>,
    pub owner_uid: u32,
    pub run_id: RunId,
    pub attempt: i64,
    pub node_id: String,
    pub session_id: Option<SessionId>,
    pub repository_id: Option<String>,
    pub workflow_id: Option<String>,
    pub workflow_run_id: Option<String>,
    pub task_class: Option<String>,
    pub provider: Option<String>,
    pub model_id: Option<String>,
    pub endpoint: Option<String>,
    pub route: Option<String>,

    // Measured metrics — NULL means NOT MEASURED
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub cost_micros: Option<u64>,
    pub latency_ms: Option<u64>,
    pub retry_count: Option<u64>,
    pub escalation_count: Option<u64>,
    pub grader_score_micros: Option<u64>,
    pub completion: Option<AnalyticsCompletion>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetAlert {
    pub budget_id: String,
    pub owner_uid: u32,
    pub dimension: String,
    pub window: String,
    pub window_start: DateTime<Utc>,
    pub threshold: u64,
    pub current_value: u64,
    pub dedup_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Percentiles {
    pub p50: u64,
    pub p90: u64,
    pub p95: u64,
    pub p99: u64,
}

/// Calculate a given percentile (0.0 to 1.0) from a slice of values.
pub fn calculate_percentile(values: &[u64], percentile: f64) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let clamped_p = percentile.clamp(0.0, 1.0);
    let index = ((sorted.len() - 1) as f64 * clamped_p).round() as usize;
    Some(sorted[index])
}

/// Calculate standard percentiles (p50, p90, p95, p99) from a slice of values.
pub fn percentiles(values: &[u64]) -> Option<Percentiles> {
    if values.is_empty() {
        return None;
    }
    Some(Percentiles {
        p50: calculate_percentile(values, 0.50)?,
        p90: calculate_percentile(values, 0.90)?,
        p95: calculate_percentile(values, 0.95)?,
        p99: calculate_percentile(values, 0.99)?,
    })
}

#[derive(Debug, Serialize, Deserialize)]
struct AnalyticsCursor {
    version: u8,
    query_hash: String,
    offset: usize,
}

fn query_hash(principal_uid: u32, query: &AnalyticsQuery) -> Result<String, AnalyticsError> {
    let payload = serde_json::to_vec(&(principal_uid, &query.filters, &query.group_by))
        .map_err(|error| AnalyticsError::InvalidData(error.to_string()))?;
    Ok(hex::encode(Sha256::digest(payload)))
}

fn encode_cursor(offset: usize, query_hash: &str) -> Result<PageCursor, AnalyticsError> {
    let cursor = AnalyticsCursor {
        version: CURSOR_VERSION,
        query_hash: query_hash.to_string(),
        offset,
    };
    let bytes = serde_json::to_vec(&cursor)
        .map_err(|error| AnalyticsError::InvalidData(error.to_string()))?;
    Ok(PageCursor(URL_SAFE_NO_PAD.encode(bytes)))
}

fn decode_cursor(cursor: &PageCursor, expected_query_hash: &str) -> Result<usize, AnalyticsError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(&cursor.0)
        .map_err(|_| AnalyticsError::InvalidCursor)?;
    let decoded: AnalyticsCursor =
        serde_json::from_slice(&bytes).map_err(|_| AnalyticsError::InvalidCursor)?;
    if decoded.version != CURSOR_VERSION || decoded.query_hash != expected_query_hash {
        return Err(AnalyticsError::InvalidCursor);
    }
    Ok(decoded.offset)
}

fn completion_to_db(completion: Option<AnalyticsCompletion>) -> Option<&'static str> {
    match completion {
        Some(AnalyticsCompletion::Successful) => Some("successful"),
        Some(AnalyticsCompletion::Failed) => Some("failed"),
        Some(AnalyticsCompletion::Cancelled) => Some("cancelled"),
        Some(AnalyticsCompletion::Incomplete) => Some("incomplete"),
        _ => None,
    }
}

/// The AnalyticsStore manages observation recording, aggregation queries, and budget evaluations.
#[derive(Debug, Clone)]
pub struct AnalyticsStore {
    pool: SqlitePool,
}

impl AnalyticsStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Record an execution observation. Updates in place on duplicate (run_id, attempt, node_id).
    pub async fn record_observation(
        &self,
        obs: &ExecutionObservation,
    ) -> Result<i64, AnalyticsError> {
        record_observation(&self.pool, obs).await
    }

    /// Backfill observations from historical runs and model_task_outcomes.
    pub async fn backfill(&self, daemon_uid: u32) -> Result<usize, AnalyticsError> {
        backfill(&self.pool, daemon_uid).await
    }

    /// Query aggregate analytics buckets for the given principal.
    pub async fn query(
        &self,
        daemon_uid: u32,
        principal: PeerPrincipal,
        query_def: &AnalyticsQuery,
    ) -> Result<AnalyticsPage, AnalyticsError> {
        query(&self.pool, daemon_uid, principal, query_def).await
    }

    /// Evaluate active budgets for the given principal.
    pub async fn evaluate_budgets(
        &self,
        daemon_uid: u32,
        principal: PeerPrincipal,
    ) -> Result<Vec<BudgetAlert>, AnalyticsError> {
        evaluate_budgets(&self.pool, daemon_uid, principal).await
    }
}

/// Record an execution observation inside an existing transaction.
pub async fn record_observation_in_tx(
    tx: &mut SqliteConnection,
    obs: &ExecutionObservation,
) -> Result<i64, AnalyticsError> {
    let to_i64 = |val: Option<u64>| -> Result<Option<i64>, AnalyticsError> {
        val.map(i64::try_from)
            .transpose()
            .map_err(|e| AnalyticsError::InvalidData(format!("metric out of range: {e}")))
    };

    let input_tokens = to_i64(obs.input_tokens)?;
    let output_tokens = to_i64(obs.output_tokens)?;
    let cached_tokens = to_i64(obs.cached_tokens)?;
    let reasoning_tokens = to_i64(obs.reasoning_tokens)?;
    let cost_micros = to_i64(obs.cost_micros)?;
    let latency_ms = to_i64(obs.latency_ms)?;
    let retry_count = to_i64(obs.retry_count)?;
    let escalation_count = to_i64(obs.escalation_count)?;
    let grader_score_micros = to_i64(obs.grader_score_micros)?;
    let completion_str = completion_to_db(obs.completion);

    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO execution_observations (
            owner_uid, run_id, attempt, node_id, session_id,
            repository_id, workflow_id, workflow_run_id, task_class,
            provider, model_id, endpoint, route,
            input_tokens, output_tokens, cached_tokens, reasoning_tokens,
            cost_micros, latency_ms, retry_count, escalation_count,
            grader_score_micros, completion, observed_at
         ) VALUES (
            ?, ?, ?, ?, ?,
            ?, ?, ?, ?,
            ?, ?, ?, ?,
            ?, ?, ?, ?,
            ?, ?, ?, ?,
            ?, ?, ?
         )
         ON CONFLICT (run_id, attempt, node_id) DO UPDATE SET
            owner_uid = excluded.owner_uid,
            session_id = COALESCE(excluded.session_id, execution_observations.session_id),
            repository_id = COALESCE(excluded.repository_id, execution_observations.repository_id),
            workflow_id = COALESCE(excluded.workflow_id, execution_observations.workflow_id),
            workflow_run_id = COALESCE(excluded.workflow_run_id, execution_observations.workflow_run_id),
            task_class = COALESCE(excluded.task_class, execution_observations.task_class),
            provider = COALESCE(excluded.provider, execution_observations.provider),
            model_id = COALESCE(excluded.model_id, execution_observations.model_id),
            endpoint = COALESCE(excluded.endpoint, execution_observations.endpoint),
            route = COALESCE(excluded.route, execution_observations.route),
            input_tokens = COALESCE(excluded.input_tokens, execution_observations.input_tokens),
            output_tokens = COALESCE(excluded.output_tokens, execution_observations.output_tokens),
            cached_tokens = COALESCE(excluded.cached_tokens, execution_observations.cached_tokens),
            reasoning_tokens = COALESCE(excluded.reasoning_tokens, execution_observations.reasoning_tokens),
            cost_micros = COALESCE(excluded.cost_micros, execution_observations.cost_micros),
            latency_ms = COALESCE(excluded.latency_ms, execution_observations.latency_ms),
            retry_count = COALESCE(excluded.retry_count, execution_observations.retry_count),
            escalation_count = COALESCE(excluded.escalation_count, execution_observations.escalation_count),
            grader_score_micros = COALESCE(excluded.grader_score_micros, execution_observations.grader_score_micros),
            completion = COALESCE(excluded.completion, execution_observations.completion),
            observed_at = excluded.observed_at
         RETURNING id",
    )
    .bind(i64::from(obs.owner_uid))
    .bind(obs.run_id.to_string())
    .bind(obs.attempt)
    .bind(&obs.node_id)
    .bind(obs.session_id.map(|s| s.to_string()))
    .bind(&obs.repository_id)
    .bind(&obs.workflow_id)
    .bind(&obs.workflow_run_id)
    .bind(&obs.task_class)
    .bind(&obs.provider)
    .bind(&obs.model_id)
    .bind(&obs.endpoint)
    .bind(&obs.route)
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(cached_tokens)
    .bind(reasoning_tokens)
    .bind(cost_micros)
    .bind(latency_ms)
    .bind(retry_count)
    .bind(escalation_count)
    .bind(grader_score_micros)
    .bind(completion_str)
    .bind(obs.observed_at.to_rfc3339())
    .fetch_one(tx)
    .await?;

    Ok(id)
}

/// Record an execution observation into the database.
pub async fn record_observation(
    pool: &SqlitePool,
    obs: &ExecutionObservation,
) -> Result<i64, AnalyticsError> {
    let mut conn = pool.acquire().await?;
    record_observation_in_tx(&mut conn, obs).await
}

/// Backfill observations from historical runs and model_task_outcomes.
///
/// Backfills only values present in durable existing records:
/// `runs.{prompt_tokens, completion_tokens, cost_micros}` (0032) and
/// `model_task_outcomes` (0025). All other fields remain NULL.
pub async fn backfill(pool: &SqlitePool, daemon_uid: u32) -> Result<usize, AnalyticsError> {
    let rows_affected = sqlx::query(
        "INSERT OR IGNORE INTO execution_observations (
            owner_uid,
            run_id,
            attempt,
            node_id,
            session_id,
            repository_id,
            task_class,
            model_id,
            endpoint,
            input_tokens,
            output_tokens,
            cost_micros,
            observed_at
        )
        SELECT
            COALESCE(s.owner_uid, ?),
            r.id,
            0,
            '',
            r.session_id,
            s.repository_id,
            mto.task_class,
            mto.model_id,
            mto.endpoint,
            r.prompt_tokens,
            r.completion_tokens,
            r.cost_micros,
            -- `runs` has never had a `created_at` column (see 0002_phase1.sql and
            -- 0032_ledger.sql); naming it here made every backfill fail at
            -- runtime with `no such column: r.created_at`.
            COALESCE(r.ended_at, r.started_at, datetime('now'))
        FROM runs r
        JOIN sessions s ON s.id = r.session_id
        LEFT JOIN (
            SELECT run_id, task_class, model_id, endpoint
            FROM model_task_outcomes
            GROUP BY run_id
        ) mto ON mto.run_id = r.id
        WHERE r.prompt_tokens IS NOT NULL
           OR r.completion_tokens IS NOT NULL
           OR r.cost_micros IS NOT NULL",
    )
    .bind(i64::from(daemon_uid))
    .execute(pool)
    .await?
    .rows_affected();

    Ok(rows_affected as usize)
}

fn map_grouping_to_col(grouping: AnalyticsGrouping) -> Result<&'static str, AnalyticsError> {
    match grouping {
        AnalyticsGrouping::Model => Ok("COALESCE(model_id, '')"),
        AnalyticsGrouping::Provider => Ok("COALESCE(provider, '')"),
        AnalyticsGrouping::Repository => Ok("COALESCE(repository_id, '')"),
        AnalyticsGrouping::Workflow => Ok("COALESCE(workflow_id, '')"),
        AnalyticsGrouping::TaskClass => Ok("COALESCE(task_class, '')"),
        AnalyticsGrouping::Time => Ok("strftime('%Y-%m-%d', observed_at)"),
        AnalyticsGrouping::Completion => Ok("COALESCE(completion, '')"),
        AnalyticsGrouping::Route => Ok("COALESCE(route, '')"),
        AnalyticsGrouping::Unknown | _ => Err(AnalyticsError::UnsupportedGrouping),
    }
}

/// Execute an owner-scoped aggregate analytics query.
pub async fn query(
    pool: &SqlitePool,
    _daemon_uid: u32,
    principal: PeerPrincipal,
    query_def: &AnalyticsQuery,
) -> Result<AnalyticsPage, AnalyticsError> {
    // Validate groupings
    for grouping in &query_def.group_by {
        if matches!(grouping, AnalyticsGrouping::Unknown) {
            return Err(AnalyticsError::UnsupportedGrouping);
        }
    }

    let q_hash = query_hash(principal.uid(), query_def)?;
    let offset = if let Some(cursor) = &query_def.cursor {
        decode_cursor(cursor, &q_hash)?
    } else {
        0
    };

    let limit = if query_def.limit == 0 {
        DEFAULT_PAGE_SIZE
    } else {
        query_def.limit.min(MAX_PAGE_SIZE)
    };

    let mut sql = QueryBuilder::<Sqlite>::new("SELECT ");

    // Dimension expressions
    for (i, grouping) in query_def.group_by.iter().enumerate() {
        let col = map_grouping_to_col(*grouping)?;
        sql.push(col);
        sql.push(format!(" AS dim_{i}, "));
    }

    sql.push(
        "COUNT(*) AS total_count, \
         COUNT(input_tokens) AS count_input_tokens, \
         SUM(input_tokens) AS sum_input_tokens, \
         COUNT(output_tokens) AS count_output_tokens, \
         SUM(output_tokens) AS sum_output_tokens, \
         COUNT(cached_tokens) AS count_cached_tokens, \
         SUM(cached_tokens) AS sum_cached_tokens, \
         COUNT(reasoning_tokens) AS count_reasoning_tokens, \
         SUM(reasoning_tokens) AS sum_reasoning_tokens, \
         COUNT(cost_micros) AS count_cost_micros, \
         SUM(cost_micros) AS sum_cost_micros, \
         COUNT(latency_ms) AS count_latency, \
         SUM(latency_ms) AS sum_latency_ms, \
         COUNT(grader_score_micros) AS count_grader_score, \
         AVG(grader_score_micros) AS avg_grader_score_micros, \
         COUNT(retry_count) AS count_retry, \
         SUM(retry_count) AS sum_retry_count, \
         COUNT(escalation_count) AS count_escalation, \
         SUM(escalation_count) AS sum_escalation_count, \
         COUNT(completion) AS count_completion, \
         SUM(CASE WHEN completion = 'successful' THEN 1 ELSE 0 END) AS count_successful \
         FROM execution_observations \
         WHERE owner_uid = ",
    );
    sql.push_bind(i64::from(principal.uid()));

    // Apply filters
    if !query_def.filters.models.is_empty() {
        sql.push(" AND model_id IN (");
        let mut sep = sql.separated(", ");
        for m in &query_def.filters.models {
            sep.push_bind(m);
        }
        sep.push_unseparated(")");
    }
    if !query_def.filters.providers.is_empty() {
        sql.push(" AND provider IN (");
        let mut sep = sql.separated(", ");
        for p in &query_def.filters.providers {
            sep.push_bind(p);
        }
        sep.push_unseparated(")");
    }
    if !query_def.filters.repositories.is_empty() {
        sql.push(" AND repository_id IN (");
        let mut sep = sql.separated(", ");
        for r in &query_def.filters.repositories {
            sep.push_bind(r);
        }
        sep.push_unseparated(")");
    }
    if !query_def.filters.workflows.is_empty() {
        sql.push(" AND workflow_id IN (");
        let mut sep = sql.separated(", ");
        for w in &query_def.filters.workflows {
            sep.push_bind(w);
        }
        sep.push_unseparated(")");
    }
    if !query_def.filters.task_classes.is_empty() {
        sql.push(" AND task_class IN (");
        let mut sep = sql.separated(", ");
        for tc in &query_def.filters.task_classes {
            sep.push_bind(tc);
        }
        sep.push_unseparated(")");
    }
    if !query_def.filters.routes.is_empty() {
        sql.push(" AND route IN (");
        let mut sep = sql.separated(", ");
        for r in &query_def.filters.routes {
            sep.push_bind(r);
        }
        sep.push_unseparated(")");
    }
    if let Some(time) = &query_def.filters.time {
        if let Some(from) = time.from {
            sql.push(" AND observed_at >= ");
            sql.push_bind(from.to_rfc3339());
        }
        if let Some(until) = time.until {
            sql.push(" AND observed_at < ");
            sql.push_bind(until.to_rfc3339());
        }
    }
    if !query_def.filters.completions.is_empty() {
        let valid_completions: Vec<&'static str> = query_def
            .filters
            .completions
            .iter()
            .filter_map(|c| completion_to_db(Some(*c)))
            .collect();
        if !valid_completions.is_empty() {
            sql.push(" AND completion IN (");
            let mut sep = sql.separated(", ");
            for c in valid_completions {
                sep.push_bind(c);
            }
            sep.push_unseparated(")");
        }
    }

    if !query_def.group_by.is_empty() {
        sql.push(" GROUP BY ");
        for (i, grouping) in query_def.group_by.iter().enumerate() {
            if i > 0 {
                sql.push(", ");
            }
            let col = map_grouping_to_col(*grouping)?;
            sql.push(col);
        }
        sql.push(" ORDER BY ");
        for i in 0..query_def.group_by.len() {
            if i > 0 {
                sql.push(", ");
            }
            sql.push(format!("dim_{i} ASC"));
        }
    }

    sql.push(" LIMIT ");
    sql.push_bind(i64::from(limit + 1));
    sql.push(" OFFSET ");
    sql.push_bind(i64::try_from(offset).map_err(|e| AnalyticsError::InvalidData(e.to_string()))?);

    let rows = sql.build().fetch_all(pool).await?;

    let mut items = Vec::new();
    let num_dims = query_def.group_by.len();

    for row in rows.iter().take(limit as usize) {
        let total_count: i64 = row.try_get("total_count").unwrap_or(0);
        if query_def.group_by.is_empty() && total_count == 0 {
            continue;
        }

        let mut dimensions = Vec::with_capacity(num_dims);
        for i in 0..num_dims {
            let dim: String = row.try_get(format!("dim_{i}").as_str()).unwrap_or_default();
            dimensions.push(dim);
        }

        let count_input_tokens: i64 = row.try_get("count_input_tokens").unwrap_or(0);
        let sum_input_tokens: Option<i64> = row.try_get("sum_input_tokens").unwrap_or(None);
        let count_output_tokens: i64 = row.try_get("count_output_tokens").unwrap_or(0);
        let sum_output_tokens: Option<i64> = row.try_get("sum_output_tokens").unwrap_or(None);
        let count_cached_tokens: i64 = row.try_get("count_cached_tokens").unwrap_or(0);
        let sum_cached_tokens: Option<i64> = row.try_get("sum_cached_tokens").unwrap_or(None);
        let count_reasoning_tokens: i64 = row.try_get("count_reasoning_tokens").unwrap_or(0);
        let sum_reasoning_tokens: Option<i64> = row.try_get("sum_reasoning_tokens").unwrap_or(None);
        let count_cost_micros: i64 = row.try_get("count_cost_micros").unwrap_or(0);
        let sum_cost_micros: Option<i64> = row.try_get("sum_cost_micros").unwrap_or(None);
        let count_latency: i64 = row.try_get("count_latency").unwrap_or(0);
        let sum_latency_ms: Option<i64> = row.try_get("sum_latency_ms").unwrap_or(None);
        let count_grader_score: i64 = row.try_get("count_grader_score").unwrap_or(0);
        let avg_grader_score_micros: Option<f64> =
            row.try_get("avg_grader_score_micros").unwrap_or(None);
        let count_retry: i64 = row.try_get("count_retry").unwrap_or(0);
        let sum_retry_count: Option<i64> = row.try_get("sum_retry_count").unwrap_or(None);
        let count_escalation: i64 = row.try_get("count_escalation").unwrap_or(0);
        let sum_escalation_count: Option<i64> = row.try_get("sum_escalation_count").unwrap_or(None);
        let count_completion: i64 = row.try_get("count_completion").unwrap_or(0);
        let count_successful: i64 = row.try_get("count_successful").unwrap_or(0);

        let input_tokens = if count_input_tokens > 0 {
            sum_input_tokens.and_then(|v| u64::try_from(v).ok())
        } else {
            None
        };
        let output_tokens = if count_output_tokens > 0 {
            sum_output_tokens.and_then(|v| u64::try_from(v).ok())
        } else {
            None
        };
        let cached_tokens = if count_cached_tokens > 0 {
            sum_cached_tokens.and_then(|v| u64::try_from(v).ok())
        } else {
            None
        };
        let reasoning_tokens = if count_reasoning_tokens > 0 {
            sum_reasoning_tokens.and_then(|v| u64::try_from(v).ok())
        } else {
            None
        };
        let cost_micros = if count_cost_micros > 0 {
            sum_cost_micros.and_then(|v| u64::try_from(v).ok())
        } else {
            None
        };
        let latency_ms = if count_latency > 0 {
            sum_latency_ms.and_then(|v| u64::try_from(v).ok())
        } else {
            None
        };
        let grader_score_micros = if count_grader_score > 0 {
            avg_grader_score_micros.map(|v| v.round().max(0.0) as u64)
        } else {
            None
        };
        let cost_per_successful_task_micros = sum_cost_micros
            .filter(|_| count_cost_micros > 0 && count_successful > 0)
            .map(|sum| (sum as u64) / (count_successful as u64));
        let retry_count = if count_retry > 0 {
            sum_retry_count.and_then(|v| u64::try_from(v).ok())
        } else {
            None
        };
        let escalation_count = if count_escalation > 0 {
            sum_escalation_count.and_then(|v| u64::try_from(v).ok())
        } else {
            None
        };
        let completion_count = if count_completion > 0 {
            Some(count_completion as u64)
        } else {
            None
        };

        let coverage = AnalyticsDimensionCoverage {
            input_tokens: MeasurementCoverage {
                measured: count_input_tokens as u64,
                total: total_count as u64,
            },
            output_tokens: MeasurementCoverage {
                measured: count_output_tokens as u64,
                total: total_count as u64,
            },
            cached_tokens: MeasurementCoverage {
                measured: count_cached_tokens as u64,
                total: total_count as u64,
            },
            reasoning_tokens: MeasurementCoverage {
                measured: count_reasoning_tokens as u64,
                total: total_count as u64,
            },
            cost: MeasurementCoverage {
                measured: count_cost_micros as u64,
                total: total_count as u64,
            },
            latency: MeasurementCoverage {
                measured: count_latency as u64,
                total: total_count as u64,
            },
            grader_score: MeasurementCoverage {
                measured: count_grader_score as u64,
                total: total_count as u64,
            },
            cost_per_successful_task: MeasurementCoverage {
                measured: if count_cost_micros > 0 && count_successful > 0 {
                    count_successful as u64
                } else {
                    0
                },
                total: total_count as u64,
            },
            retry_count: MeasurementCoverage {
                measured: count_retry as u64,
                total: total_count as u64,
            },
            escalation_count: MeasurementCoverage {
                measured: count_escalation as u64,
                total: total_count as u64,
            },
            completion_count: MeasurementCoverage {
                measured: count_completion as u64,
                total: total_count as u64,
            },
        };

        items.push(AnalyticsBucket {
            dimensions,
            metrics: AnalyticsMetrics {
                input_tokens,
                output_tokens,
                cached_tokens,
                reasoning_tokens,
                cost_micros,
                latency_ms,
                grader_score_micros,
                cost_per_successful_task_micros,
                retry_count,
                escalation_count,
                completion_count,
                coverage,
            },
        });
    }

    let next_cursor = if rows.len() > limit as usize {
        Some(encode_cursor(offset + limit as usize, &q_hash)?)
    } else {
        None
    };

    Ok(AnalyticsPage { items, next_cursor })
}

/// Evaluate active budgets for the given principal.
///
/// If measured usage exceeds threshold, creates an alert with stable dedup_key.
/// If no values are measured for the dimension in the window, no alert is created.
pub async fn evaluate_budgets(
    pool: &SqlitePool,
    _daemon_uid: u32,
    principal: PeerPrincipal,
) -> Result<Vec<BudgetAlert>, AnalyticsError> {
    let mut conn = pool.acquire().await?;
    evaluate_budgets_in(&mut conn, principal.uid()).await
}

/// The evaluator's single implementation, over an arbitrary connection so the
/// run-terminal writer can evaluate inside the SAME transaction that recorded
/// the observation. Evaluating on a separate pool connection would read a
/// snapshot that does not yet contain the run that just crossed the threshold.
pub async fn evaluate_budgets_in(
    conn: &mut SqliteConnection,
    owner_uid: u32,
) -> Result<Vec<BudgetAlert>, AnalyticsError> {
    let rows: Vec<(String, String, String, String, String, i64)> = sqlx::query_as(
        "SELECT id, scope, scope_value, dimension, window, threshold \
         FROM analytics_budgets \
         WHERE owner_uid = ? AND enabled = 1",
    )
    .bind(i64::from(owner_uid))
    .fetch_all(&mut *conn)
    .await?;

    let now = Utc::now();
    let mut alerts = Vec::new();

    for (id, scope, scope_value, dimension, window, threshold) in rows {
        let window_start = match window.as_str() {
            "day" => now.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc(),
            "week" => {
                let days = now.weekday().num_days_from_monday();
                (now.date_naive() - chrono::Duration::days(days as i64))
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
            }
            "month" => now
                .date_naive()
                .with_day(1)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc(),
            _ => continue,
        };

        let dim_col = match dimension.as_str() {
            "cost_micros" => "cost_micros",
            "input_tokens" => "input_tokens",
            "output_tokens" => "output_tokens",
            "latency_ms" => "latency_ms",
            _ => continue,
        };

        let mut sql = QueryBuilder::<Sqlite>::new("SELECT COUNT(");
        sql.push(dim_col);
        sql.push(") AS count_measured, SUM(");
        sql.push(dim_col);
        sql.push(") AS sum_measured FROM execution_observations WHERE owner_uid = ");
        sql.push_bind(i64::from(owner_uid));
        sql.push(" AND observed_at >= ");
        sql.push_bind(window_start.to_rfc3339());

        match scope.as_str() {
            // Owner scope narrows nothing beyond the owner predicate above.
            "owner" => {}
            "repository" => {
                sql.push(" AND repository_id = ");
                sql.push_bind(&scope_value);
            }
            "workflow" => {
                sql.push(" AND workflow_id = ");
                sql.push_bind(&scope_value);
            }
            "model" => {
                sql.push(" AND model_id = ");
                sql.push_bind(&scope_value);
            }
            // Fail closed. A scope this build does not know must not fall
            // through to "no narrowing", which would evaluate the threshold
            // against the owner's ENTIRE volume and alert on the wrong basis.
            _ => continue,
        }

        let query_row = sql.build().fetch_one(&mut *conn).await?;
        let count_measured: i64 = query_row.try_get("count_measured").unwrap_or(0);
        let sum_measured: Option<i64> = query_row.try_get("sum_measured").unwrap_or(None);

        if count_measured > 0 {
            if let Some(sum_val) = sum_measured {
                let current_value = sum_val.max(0) as u64;
                if current_value > threshold as u64 {
                    let dedup_key = format!("budget:{id}:{}", window_start.to_rfc3339());
                    alerts.push(BudgetAlert {
                        budget_id: id,
                        owner_uid,
                        dimension,
                        window,
                        window_start,
                        threshold: threshold as u64,
                        current_value,
                        dedup_key,
                    });
                }
            }
        }
    }

    Ok(alerts)
}

// --- Budget configuration (the writer `analytics_budgets` never had) ---------
//
// Before this, `analytics_budgets` had no INSERT outside integration tests, so
// `evaluate_budgets` above, `BudgetAlert`, `derive_budget_dedup_key` and the
// `BudgetWarning` inbox kind were all live code nothing could ever reach. These
// functions are the missing half.
//
// Ownership is the connection's kernel-derived principal, never a wire field,
// and every statement carries `owner_uid = ?` in its predicate so a by-id read
// or mutation of another principal's budget is a miss, not a denial.

/// Default and maximum rows a single budget listing returns.
const DEFAULT_BUDGET_PAGE: u32 = 50;
const MAX_BUDGET_PAGE: u32 = 200;

fn budget_not_found() -> CodypendentError {
    // Deliberately identical for "not yours" and "not there": the ownership
    // gate in `server::authorize_command` answers the same error, so the pair
    // is indistinguishable and no id is an existence oracle.
    CodypendentError::new(
        "analytics.budget-not-found",
        "analytics budget is unavailable",
        false,
    )
}

fn budget_database_error(error: impl std::fmt::Display) -> CodypendentError {
    CodypendentError::new("analytics.database-error", error.to_string(), true)
}

fn budget_invalid_request(message: impl Into<String>) -> CodypendentError {
    CodypendentError::new("analytics.invalid-budget", message.into(), false)
}

/// Project a wire scope onto 0043's `(scope, scope_value)` pair.
///
/// `Unknown` is refused rather than stored: the column has a `CHECK`, and a
/// budget whose scope this build cannot evaluate would sit enabled and silent.
fn project_budget_scope(
    scope: &AnalyticsBudgetScope,
) -> Result<(&'static str, String), CodypendentError> {
    match scope {
        AnalyticsBudgetScope::Owner => Ok(("owner", String::new())),
        AnalyticsBudgetScope::Repository { repository_id } => {
            if repository_id.trim().is_empty() {
                return Err(budget_invalid_request(
                    "repository scope needs a repository",
                ));
            }
            Ok(("repository", repository_id.clone()))
        }
        AnalyticsBudgetScope::Workflow { workflow_id } => {
            if workflow_id.trim().is_empty() {
                return Err(budget_invalid_request("workflow scope needs a workflow"));
            }
            Ok(("workflow", workflow_id.clone()))
        }
        AnalyticsBudgetScope::Model { model_id } => {
            if model_id.trim().is_empty() {
                return Err(budget_invalid_request("model scope needs a model"));
            }
            Ok(("model", model_id.clone()))
        }
        _ => Err(budget_invalid_request("unsupported budget scope")),
    }
}

fn budget_scope_from_db(scope: &str, value: &str) -> Option<AnalyticsBudgetScope> {
    match scope {
        "owner" => Some(AnalyticsBudgetScope::Owner),
        "repository" => Some(AnalyticsBudgetScope::Repository {
            repository_id: value.to_string(),
        }),
        "workflow" => Some(AnalyticsBudgetScope::Workflow {
            workflow_id: value.to_string(),
        }),
        "model" => Some(AnalyticsBudgetScope::Model {
            model_id: value.to_string(),
        }),
        _ => None,
    }
}

fn project_budget_dimension(
    dimension: AnalyticsBudgetDimension,
) -> Result<&'static str, CodypendentError> {
    match dimension {
        AnalyticsBudgetDimension::CostMicros => Ok("cost_micros"),
        AnalyticsBudgetDimension::InputTokens => Ok("input_tokens"),
        AnalyticsBudgetDimension::OutputTokens => Ok("output_tokens"),
        AnalyticsBudgetDimension::LatencyMs => Ok("latency_ms"),
        // Fail closed: an unmeasured or unknown dimension has no honest column.
        _ => Err(budget_invalid_request(
            "budgets are only supported over measured dimensions",
        )),
    }
}

fn budget_dimension_from_db(dimension: &str) -> Option<AnalyticsBudgetDimension> {
    match dimension {
        "cost_micros" => Some(AnalyticsBudgetDimension::CostMicros),
        "input_tokens" => Some(AnalyticsBudgetDimension::InputTokens),
        "output_tokens" => Some(AnalyticsBudgetDimension::OutputTokens),
        "latency_ms" => Some(AnalyticsBudgetDimension::LatencyMs),
        _ => None,
    }
}

fn project_budget_window(window: AnalyticsBudgetWindow) -> Result<&'static str, CodypendentError> {
    match window {
        AnalyticsBudgetWindow::Day => Ok("day"),
        AnalyticsBudgetWindow::Week => Ok("week"),
        AnalyticsBudgetWindow::Month => Ok("month"),
        _ => Err(budget_invalid_request("unsupported budget window")),
    }
}

fn budget_window_from_db(window: &str) -> Option<AnalyticsBudgetWindow> {
    match window {
        "day" => Some(AnalyticsBudgetWindow::Day),
        "week" => Some(AnalyticsBudgetWindow::Week),
        "month" => Some(AnalyticsBudgetWindow::Month),
        _ => None,
    }
}

fn project_budget_threshold(threshold: u64) -> Result<i64, CodypendentError> {
    if threshold == 0 {
        return Err(budget_invalid_request("budget threshold must be positive"));
    }
    i64::try_from(threshold).map_err(|_| budget_invalid_request("budget threshold out of range"))
}

type BudgetRow = (
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    String,
    String,
);

const BUDGET_COLUMNS: &str =
    "id, scope, scope_value, dimension, window, threshold, enabled, created_at, updated_at";

fn parse_budget_row(row: BudgetRow) -> Result<AnalyticsBudget, CodypendentError> {
    let (id, scope, scope_value, dimension, window, threshold, enabled, created_at, updated_at) =
        row;
    // A row this build cannot interpret is reported as unavailable rather than
    // guessed at — the same answer an absent row gets.
    let scope = budget_scope_from_db(&scope, &scope_value).ok_or_else(budget_not_found)?;
    let dimension = budget_dimension_from_db(&dimension).ok_or_else(budget_not_found)?;
    let window = budget_window_from_db(&window).ok_or_else(budget_not_found)?;
    let threshold = u64::try_from(threshold).map_err(|_| budget_not_found())?;
    let parse_time = |value: &str| -> Result<DateTime<Utc>, CodypendentError> {
        DateTime::parse_from_rfc3339(value)
            .map(|t| t.with_timezone(&Utc))
            .map_err(|_| budget_not_found())
    };
    Ok(AnalyticsBudget {
        id,
        definition: AnalyticsBudgetDraft {
            scope,
            dimension,
            window,
            threshold,
            enabled: enabled != 0,
        },
        created_at: parse_time(&created_at)?,
        updated_at: parse_time(&updated_at)?,
    })
}

/// Create a budget owned by `principal`.
pub async fn create_budget(
    pool: &SqlitePool,
    principal: PeerPrincipal,
    draft: &AnalyticsBudgetDraft,
) -> Result<AnalyticsBudget, CodypendentError> {
    let (scope, scope_value) = project_budget_scope(&draft.scope)?;
    let dimension = project_budget_dimension(draft.dimension)?;
    let window = project_budget_window(draft.window)?;
    let threshold = project_budget_threshold(draft.threshold)?;

    let id = uuid::Uuid::now_v7().to_string();
    let now = Utc::now();
    let now_str = now.to_rfc3339();

    let inserted = sqlx::query(
        "INSERT INTO analytics_budgets \
         (id, owner_uid, scope, scope_value, dimension, window, threshold, enabled, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(i64::from(principal.uid()))
    .bind(scope)
    .bind(&scope_value)
    .bind(dimension)
    .bind(window)
    .bind(threshold)
    .bind(i64::from(draft.enabled))
    .bind(&now_str)
    .bind(&now_str)
    .execute(pool)
    .await;

    if let Err(error) = inserted {
        // 0043's UNIQUE (owner_uid, scope, scope_value, dimension, window). The
        // collision is with a row this principal already owns — the predicate
        // leads with `owner_uid` — so saying so leaks nothing.
        if matches!(&error, sqlx::Error::Database(db) if db.is_unique_violation()) {
            return Err(budget_invalid_request(
                "a budget for that scope, dimension and window already exists",
            ));
        }
        return Err(budget_database_error(error));
    }

    Ok(AnalyticsBudget {
        id,
        definition: AnalyticsBudgetDraft {
            scope: draft.scope.clone(),
            dimension: draft.dimension,
            window: draft.window,
            threshold: draft.threshold,
            enabled: draft.enabled,
        },
        created_at: now,
        updated_at: now,
    })
}

/// Read one budget owned by `principal`.
pub async fn get_budget(
    pool: &SqlitePool,
    principal: PeerPrincipal,
    id: &str,
) -> Result<AnalyticsBudget, CodypendentError> {
    let row: Option<BudgetRow> = sqlx::query_as(&format!(
        "SELECT {BUDGET_COLUMNS} FROM analytics_budgets WHERE id = ? AND owner_uid = ?"
    ))
    .bind(id)
    .bind(i64::from(principal.uid()))
    .fetch_optional(pool)
    .await
    .map_err(budget_database_error)?;

    match row {
        Some(row) => parse_budget_row(row),
        None => Err(budget_not_found()),
    }
}

/// List budgets owned by `principal`.
pub async fn list_budgets(
    pool: &SqlitePool,
    principal: PeerPrincipal,
    query: &AnalyticsBudgetQuery,
) -> Result<AnalyticsBudgetPage, CodypendentError> {
    let limit = if query.limit == 0 {
        DEFAULT_BUDGET_PAGE
    } else {
        query.limit.min(MAX_BUDGET_PAGE)
    };
    // Fetch one past the ceiling so `truncated` is measured, not guessed.
    let probe = i64::from(limit) + 1;

    let mut sql = QueryBuilder::<Sqlite>::new("SELECT ");
    sql.push(BUDGET_COLUMNS);
    sql.push(" FROM analytics_budgets WHERE owner_uid = ");
    sql.push_bind(i64::from(principal.uid()));
    if let Some(enabled) = query.enabled {
        sql.push(" AND enabled = ");
        sql.push_bind(i64::from(enabled));
    }
    sql.push(" ORDER BY created_at ASC, id ASC LIMIT ");
    sql.push_bind(probe);

    let rows = sql
        .build_query_as::<BudgetRow>()
        .fetch_all(pool)
        .await
        .map_err(budget_database_error)?;

    let truncated = rows.len() > limit as usize;
    let items = rows
        .into_iter()
        .take(limit as usize)
        .map(parse_budget_row)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AnalyticsBudgetPage { items, truncated })
}

/// Apply a sparse patch to a budget owned by `principal`.
pub async fn update_budget(
    pool: &SqlitePool,
    principal: PeerPrincipal,
    id: &str,
    patch: &AnalyticsBudgetPatch,
) -> Result<AnalyticsBudget, CodypendentError> {
    if patch.threshold.is_none() && patch.enabled.is_none() {
        return Err(budget_invalid_request("patch changes nothing"));
    }
    let threshold = patch.threshold.map(project_budget_threshold).transpose()?;
    let now_str = Utc::now().to_rfc3339();

    let mut sql = QueryBuilder::<Sqlite>::new("UPDATE analytics_budgets SET updated_at = ");
    sql.push_bind(now_str);
    if let Some(threshold) = threshold {
        sql.push(", threshold = ");
        sql.push_bind(threshold);
    }
    if let Some(enabled) = patch.enabled {
        sql.push(", enabled = ");
        sql.push_bind(i64::from(enabled));
    }
    sql.push(" WHERE id = ");
    sql.push_bind(id);
    sql.push(" AND owner_uid = ");
    sql.push_bind(i64::from(principal.uid()));

    let result = sql
        .build()
        .execute(pool)
        .await
        .map_err(budget_database_error)?;
    if result.rows_affected() == 0 {
        return Err(budget_not_found());
    }
    get_budget(pool, principal, id).await
}

/// Delete a budget owned by `principal`.
pub async fn delete_budget(
    pool: &SqlitePool,
    principal: PeerPrincipal,
    id: &str,
) -> Result<(), CodypendentError> {
    let result = sqlx::query("DELETE FROM analytics_budgets WHERE id = ? AND owner_uid = ?")
        .bind(id)
        .bind(i64::from(principal.uid()))
        .execute(pool)
        .await
        .map_err(budget_database_error)?;
    if result.rows_affected() == 0 {
        return Err(budget_not_found());
    }
    Ok(())
}
