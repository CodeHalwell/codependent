//! Owner-scoped durable inbox store, query service, and producer helpers.
//!
//! Inbox rows represent human work or notifications produced by durable
//! system events (approvals, questions, terminal run states, budget alerts,
//! workflow blocks, plugin permission changes, runner failures). They are
//! never authored directly by clients.
//!
//! Ownership is derived from the source records (e.g. approval -> run -> session.owner_uid)
//! and queries are strictly scoped to the authenticated [`PeerPrincipal`].

use std::str::FromStr;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use codypendent_protocol::{
    ApprovalId, CodypendentError, InboxDeepLink, InboxEntry, InboxEntryId, InboxEntryKind,
    InboxEntryState, InboxListFilters, InboxListQuery, InboxMutation, InboxPage, InboxSource,
    InboxSourceIdentity, PageCursor, PluginId, QuestionId, RepositoryId, RunId, SessionId,
    WorkflowId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{QueryBuilder, Sqlite, SqliteConnection, SqlitePool};

use crate::commands::CommandOutcome;
use crate::principal::PeerPrincipal;

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 200;
const CURSOR_VERSION: u8 = 1;

/// An Inbox failure safe to translate at the protocol boundary.
#[derive(Debug, thiserror::Error)]
pub enum InboxError {
    #[error("invalid or stale inbox cursor")]
    InvalidCursor,
    #[error("inbox entry not found")]
    NotFound,
    #[error("unsupported inbox mutation: {0}")]
    UnsupportedMutation(String),
    #[error("inbox database query failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("inbox contains invalid data: {0}")]
    InvalidData(String),
}

pub fn internal_error(error: impl std::fmt::Display) -> CodypendentError {
    CodypendentError::new("inbox.mutation-failed", error.to_string(), true)
}

pub fn into_codypendent_error(error: InboxError) -> CodypendentError {
    match error {
        InboxError::InvalidCursor => CodypendentError::new(
            "inbox.invalid-cursor",
            "the inbox cursor is invalid or belongs to a different query",
            false,
        ),
        InboxError::NotFound => {
            CodypendentError::new("inbox.not-found", "inbox entry is unavailable", false)
        }
        InboxError::UnsupportedMutation(msg) => {
            CodypendentError::new("inbox.unsupported-mutation", msg, false)
        }
        InboxError::Database(err) => {
            tracing::warn!(%err, "inbox database operation failed");
            CodypendentError::new("inbox.query-failed", "inbox operation failed", true)
        }
        InboxError::InvalidData(err) => {
            tracing::warn!(%err, "inbox data is corrupted or invalid");
            CodypendentError::new("inbox.query-failed", "inbox data is invalid", false)
        }
    }
}

/// `owner_uid` and `updated_at` are never read in Rust, but they must stay:
/// `FromRow` maps by column, so removing them would desynchronise this struct
/// from the `SELECT` that populates it. `owner_uid` in particular is the
/// authorization predicate — it is filtered in SQL, not compared here.
#[allow(dead_code)]
#[derive(Debug, sqlx::FromRow)]
struct InboxEntryRow {
    id: String,
    owner_uid: i64,
    repository_id: String,
    kind: String,
    state: String,
    title: String,
    summary: String,
    source_identity_json: String,
    dedup_key: String,
    deep_link_json: String,
    session_id: Option<String>,
    run_id: Option<String>,
    workflow_id: Option<String>,
    created_at: String,
    updated_at: String,
    acknowledged_at: Option<String>,
    dismissed_at: Option<String>,
    resolved_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct InboxCursor {
    version: u8,
    query_hash: String,
    created_at: DateTime<Utc>,
    id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedInboxOutcome {
    #[serde(flatten)]
    pub outcome: CommandOutcome,
    pub entry: InboxEntry,
}

pub fn kind_to_db(kind: InboxEntryKind) -> Result<&'static str, InboxError> {
    match kind {
        InboxEntryKind::ApprovalRequest => Ok("ApprovalRequest"),
        InboxEntryKind::AgentQuestion => Ok("AgentQuestion"),
        InboxEntryKind::RunCompleted => Ok("RunCompleted"),
        InboxEntryKind::RunFailed => Ok("RunFailed"),
        InboxEntryKind::BudgetWarning => Ok("BudgetWarning"),
        InboxEntryKind::WorkflowBlocked => Ok("WorkflowBlocked"),
        InboxEntryKind::PluginPermissionChanged => Ok("PluginPermissionChanged"),
        InboxEntryKind::RunnerFailed => Ok("RunnerFailed"),
        InboxEntryKind::Unknown | _ => {
            Err(InboxError::InvalidData("unknown inbox kind".to_string()))
        }
    }
}

pub fn kind_from_db(value: &str) -> Result<InboxEntryKind, InboxError> {
    match value {
        "ApprovalRequest" => Ok(InboxEntryKind::ApprovalRequest),
        "AgentQuestion" => Ok(InboxEntryKind::AgentQuestion),
        "RunCompleted" => Ok(InboxEntryKind::RunCompleted),
        "RunFailed" => Ok(InboxEntryKind::RunFailed),
        "BudgetWarning" => Ok(InboxEntryKind::BudgetWarning),
        "WorkflowBlocked" => Ok(InboxEntryKind::WorkflowBlocked),
        "PluginPermissionChanged" => Ok(InboxEntryKind::PluginPermissionChanged),
        "RunnerFailed" => Ok(InboxEntryKind::RunnerFailed),
        other => Err(InboxError::InvalidData(format!(
            "invalid inbox kind {other:?}"
        ))),
    }
}

pub fn state_to_db(state: InboxEntryState) -> Result<&'static str, InboxError> {
    match state {
        InboxEntryState::Unread => Ok("Unread"),
        InboxEntryState::Acknowledged => Ok("Acknowledged"),
        InboxEntryState::Dismissed => Ok("Dismissed"),
        InboxEntryState::Resolved => Ok("Resolved"),
        InboxEntryState::Unknown | _ => {
            Err(InboxError::InvalidData("unknown inbox state".to_string()))
        }
    }
}

pub fn state_from_db(value: &str) -> Result<InboxEntryState, InboxError> {
    match value {
        "Unread" => Ok(InboxEntryState::Unread),
        "Acknowledged" => Ok(InboxEntryState::Acknowledged),
        "Dismissed" => Ok(InboxEntryState::Dismissed),
        "Resolved" => Ok(InboxEntryState::Resolved),
        other => Err(InboxError::InvalidData(format!(
            "invalid inbox state {other:?}"
        ))),
    }
}

fn parse_time(value: &str, field: &str) -> Result<DateTime<Utc>, InboxError> {
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|error| InboxError::InvalidData(format!("invalid {field}: {error}")))
}

fn parse_optional_time(
    value: Option<String>,
    field: &str,
) -> Result<Option<DateTime<Utc>>, InboxError> {
    value.as_deref().map(|v| parse_time(v, field)).transpose()
}

fn parse_optional<T>(value: Option<String>, field: &str) -> Result<Option<T>, InboxError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    value
        .map(|raw| {
            raw.parse()
                .map_err(|error| InboxError::InvalidData(format!("invalid {field}: {error}")))
        })
        .transpose()
}

impl TryFrom<InboxEntryRow> for InboxEntry {
    type Error = InboxError;

    fn try_from(row: InboxEntryRow) -> Result<Self, Self::Error> {
        let identity: InboxSourceIdentity = serde_json::from_str(&row.source_identity_json)
            .map_err(|error| {
                InboxError::InvalidData(format!("invalid source identity: {error}"))
            })?;
        let deep_link: InboxDeepLink = serde_json::from_str(&row.deep_link_json)
            .map_err(|error| InboxError::InvalidData(format!("invalid deep link: {error}")))?;

        let session_id = parse_optional::<SessionId>(row.session_id, "session_id")?;
        let run_id = parse_optional::<RunId>(row.run_id, "run_id")?;
        let workflow_id = parse_optional::<WorkflowId>(row.workflow_id, "workflow_id")?;

        let source = InboxSource {
            identity,
            dedup_key: row.dedup_key,
            session_id,
            run_id,
            workflow_id,
        };

        Ok(Self {
            id: row.id.parse().map_err(|error| {
                InboxError::InvalidData(format!("invalid inbox entry id: {error}"))
            })?,
            repository_id: row.repository_id.parse().map_err(|error| {
                InboxError::InvalidData(format!("invalid repository id: {error}"))
            })?,
            kind: kind_from_db(&row.kind)?,
            state: state_from_db(&row.state)?,
            title: row.title,
            summary: row.summary,
            source,
            deep_link,
            created_at: parse_time(&row.created_at, "created_at")?,
            acknowledged_at: parse_optional_time(row.acknowledged_at, "acknowledged_at")?,
            dismissed_at: parse_optional_time(row.dismissed_at, "dismissed_at")?,
            resolved_at: parse_optional_time(row.resolved_at, "resolved_at")?,
        })
    }
}

fn query_hash(principal_uid: u32, query: &InboxListQuery) -> Result<String, InboxError> {
    let payload = serde_json::to_vec(&(principal_uid, &query.filters))
        .map_err(|error| InboxError::InvalidData(error.to_string()))?;
    Ok(hex::encode(Sha256::digest(payload)))
}

fn encode_cursor(entry: &InboxEntry, query_hash: &str) -> Result<PageCursor, InboxError> {
    let cursor = InboxCursor {
        version: CURSOR_VERSION,
        query_hash: query_hash.to_string(),
        created_at: entry.created_at,
        id: entry.id.to_string(),
    };
    let bytes =
        serde_json::to_vec(&cursor).map_err(|error| InboxError::InvalidData(error.to_string()))?;
    Ok(PageCursor(URL_SAFE_NO_PAD.encode(bytes)))
}

fn decode_cursor(
    cursor: &PageCursor,
    expected_query_hash: &str,
) -> Result<InboxCursor, InboxError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(&cursor.0)
        .map_err(|_| InboxError::InvalidCursor)?;
    let decoded: InboxCursor =
        serde_json::from_slice(&bytes).map_err(|_| InboxError::InvalidCursor)?;
    if decoded.version != CURSOR_VERSION || decoded.query_hash != expected_query_hash {
        return Err(InboxError::InvalidCursor);
    }
    Ok(decoded)
}

/// Query durable inbox entries visible to `principal`.
pub async fn list_entries(
    pool: &SqlitePool,
    _daemon_uid: u32,
    principal: PeerPrincipal,
    query: &InboxListQuery,
) -> Result<InboxPage, InboxError> {
    let mut conn = pool.acquire().await?;
    let query_hash = query_hash(principal.uid(), query)?;
    let cursor = query
        .cursor
        .as_ref()
        .map(|c| decode_cursor(c, &query_hash))
        .transpose()?;

    let limit = query
        .limit
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);

    let mut sql = QueryBuilder::<Sqlite>::new(
        "SELECT id, owner_uid, repository_id, kind, state, title, summary, \
         source_identity_json, dedup_key, deep_link_json, session_id, run_id, workflow_id, \
         created_at, updated_at, acknowledged_at, dismissed_at, resolved_at \
         FROM inbox_entries WHERE owner_uid = ",
    );
    sql.push_bind(i64::from(principal.uid()));

    if !query.filters.kinds.is_empty() {
        let valid_kinds = query
            .filters
            .kinds
            .iter()
            .filter_map(|k| kind_to_db(*k).ok())
            .collect::<Vec<_>>();
        if valid_kinds.is_empty() {
            return Ok(InboxPage {
                items: Vec::new(),
                next_cursor: None,
            });
        }
        sql.push(" AND kind IN (");
        let mut sep = sql.separated(", ");
        for kind in valid_kinds {
            sep.push_bind(kind);
        }
        sep.push_unseparated(")");
    }

    if !query.filters.states.is_empty() {
        let valid_states = query
            .filters
            .states
            .iter()
            .filter_map(|s| state_to_db(*s).ok())
            .collect::<Vec<_>>();
        if valid_states.is_empty() {
            return Ok(InboxPage {
                items: Vec::new(),
                next_cursor: None,
            });
        }
        sql.push(" AND state IN (");
        let mut sep = sql.separated(", ");
        for state in valid_states {
            sep.push_bind(state);
        }
        sep.push_unseparated(")");
    }

    if !query.filters.repository_ids.is_empty() {
        sql.push(" AND repository_id IN (");
        let mut sep = sql.separated(", ");
        for repo_id in &query.filters.repository_ids {
            sep.push_bind(repo_id.to_string());
        }
        sep.push_unseparated(")");
    }

    if let Some(cursor) = cursor {
        sql.push(" AND (created_at < ");
        sql.push_bind(cursor.created_at.to_rfc3339());
        sql.push(" OR (created_at = ");
        sql.push_bind(cursor.created_at.to_rfc3339());
        sql.push(" AND id < ");
        sql.push_bind(cursor.id);
        sql.push("))");
    }

    sql.push(" ORDER BY created_at DESC, id DESC LIMIT ");
    sql.push_bind(i64::from(limit + 1));

    let rows = sql
        .build_query_as::<InboxEntryRow>()
        .fetch_all(&mut *conn)
        .await?;

    let mut items = rows
        .into_iter()
        .map(InboxEntry::try_from)
        .collect::<Result<Vec<_>, _>>()?;

    let next_cursor = if items.len() > limit as usize {
        items.truncate(limit as usize);
        items
            .last()
            .map(|item| encode_cursor(item, &query_hash))
            .transpose()?
    } else {
        None
    };

    Ok(InboxPage { items, next_cursor })
}

/// Count durable inbox entries matching filters visible to `principal`.
pub async fn count_entries(
    pool: &SqlitePool,
    _daemon_uid: u32,
    principal: PeerPrincipal,
    filters: &InboxListFilters,
) -> Result<u64, InboxError> {
    let mut conn = pool.acquire().await?;
    let mut sql =
        QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM inbox_entries WHERE owner_uid = ");
    sql.push_bind(i64::from(principal.uid()));

    if !filters.kinds.is_empty() {
        let valid_kinds = filters
            .kinds
            .iter()
            .filter_map(|k| kind_to_db(*k).ok())
            .collect::<Vec<_>>();
        if valid_kinds.is_empty() {
            return Ok(0);
        }
        sql.push(" AND kind IN (");
        let mut sep = sql.separated(", ");
        for kind in valid_kinds {
            sep.push_bind(kind);
        }
        sep.push_unseparated(")");
    }

    if !filters.states.is_empty() {
        let valid_states = filters
            .states
            .iter()
            .filter_map(|s| state_to_db(*s).ok())
            .collect::<Vec<_>>();
        if valid_states.is_empty() {
            return Ok(0);
        }
        sql.push(" AND state IN (");
        let mut sep = sql.separated(", ");
        for state in valid_states {
            sep.push_bind(state);
        }
        sep.push_unseparated(")");
    }

    if !filters.repository_ids.is_empty() {
        sql.push(" AND repository_id IN (");
        let mut sep = sql.separated(", ");
        for repo_id in &filters.repository_ids {
            sep.push_bind(repo_id.to_string());
        }
        sep.push_unseparated(")");
    }

    let (count,): (i64,) = sql.build_query_as().fetch_one(&mut *conn).await?;
    Ok(count as u64)
}

/// Apply an idempotent mutation to an inbox entry owned by `principal`.
pub async fn apply_mutation(
    conn: &mut SqliteConnection,
    principal: PeerPrincipal,
    mutation: &InboxMutation,
    occurred_at: DateTime<Utc>,
) -> Result<InboxEntry, InboxError> {
    let now_str = occurred_at.to_rfc3339();
    let principal_uid = i64::from(principal.uid());

    let entry_id = match mutation {
        InboxMutation::Acknowledge { entry_id } => {
            let affected = sqlx::query(
                "UPDATE inbox_entries \
                 SET state = CASE WHEN state = 'Resolved' THEN 'Resolved' ELSE 'Acknowledged' END, \
                     acknowledged_at = COALESCE(acknowledged_at, ?), \
                     updated_at = ? \
                 WHERE id = ? AND owner_uid = ?",
            )
            .bind(&now_str)
            .bind(&now_str)
            .bind(entry_id.to_string())
            .bind(principal_uid)
            .execute(&mut *conn)
            .await?;

            if affected.rows_affected() == 0 {
                return Err(InboxError::NotFound);
            }
            *entry_id
        }
        InboxMutation::Dismiss { entry_id } => {
            let affected = sqlx::query(
                "UPDATE inbox_entries \
                 SET state = CASE WHEN state = 'Resolved' THEN 'Resolved' ELSE 'Dismissed' END, \
                     dismissed_at = COALESCE(dismissed_at, ?), \
                     updated_at = ? \
                 WHERE id = ? AND owner_uid = ?",
            )
            .bind(&now_str)
            .bind(&now_str)
            .bind(entry_id.to_string())
            .bind(principal_uid)
            .execute(&mut *conn)
            .await?;

            if affected.rows_affected() == 0 {
                return Err(InboxError::NotFound);
            }
            *entry_id
        }
        InboxMutation::Unknown | _ => {
            return Err(InboxError::UnsupportedMutation(
                "unknown mutation".to_string(),
            ));
        }
    };

    let row = sqlx::query_as::<_, InboxEntryRow>(
        "SELECT id, owner_uid, repository_id, kind, state, title, summary, \
         source_identity_json, dedup_key, deep_link_json, session_id, run_id, workflow_id, \
         created_at, updated_at, acknowledged_at, dismissed_at, resolved_at \
         FROM inbox_entries WHERE id = ? AND owner_uid = ?",
    )
    .bind(entry_id.to_string())
    .bind(principal_uid)
    .fetch_one(&mut *conn)
    .await?;

    InboxEntry::try_from(row)
}

/// Retrieve the projected result of an applied inbox mutation from command records.
pub async fn inbox_mutation_response(
    pool: &SqlitePool,
    idempotency_key: &str,
) -> Result<InboxEntry, CodypendentError> {
    let row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT result_json FROM commands WHERE idempotency_key = ? AND status = 'applied'",
    )
    .bind(idempotency_key)
    .fetch_optional(pool)
    .await
    .map_err(internal_error)?;
    let (json,) =
        row.ok_or_else(|| internal_error("applied inbox mutation command disappeared"))?;
    let json =
        json.ok_or_else(|| internal_error("applied inbox mutation command missing result_json"))?;
    serde_json::from_str::<PersistedInboxOutcome>(&json)
        .map(|persisted| persisted.entry)
        .map_err(internal_error)
}

// --- Dedup key derivation -----------------------------------------------------

pub fn derive_dedup_key(identity: &InboxSourceIdentity) -> String {
    match identity {
        InboxSourceIdentity::Approval { approval_id } => derive_approval_dedup_key(*approval_id),
        InboxSourceIdentity::Question { question_id } => derive_question_dedup_key(*question_id),
        InboxSourceIdentity::Run { run_id } => derive_run_terminal_dedup_key(*run_id),
        InboxSourceIdentity::Budget { budget_id } => derive_budget_dedup_key(budget_id, ""),
        InboxSourceIdentity::Workflow { workflow_id } => format!("workflow:{workflow_id}"),
        InboxSourceIdentity::Plugin { plugin_id } => format!("plugin:{plugin_id}"),
        InboxSourceIdentity::Runner { runner_id } => format!("runner:{runner_id}"),
        InboxSourceIdentity::Unknown | _ => "unknown".to_string(),
    }
}

pub fn derive_approval_dedup_key(approval_id: ApprovalId) -> String {
    format!("approval:{approval_id}")
}

pub fn derive_question_dedup_key(question_id: QuestionId) -> String {
    format!("question:{question_id}")
}

pub fn derive_run_terminal_dedup_key(run_id: RunId) -> String {
    format!("run:{run_id}:terminal")
}

pub fn derive_budget_dedup_key(budget_id: &str, window_start: &str) -> String {
    if window_start.is_empty() {
        format!("budget:{budget_id}")
    } else {
        format!("budget:{budget_id}:{window_start}")
    }
}

pub fn derive_workflow_blocked_dedup_key(workflow_run_id: &str, node_id: &str) -> String {
    format!("workflow:{workflow_run_id}:{node_id}")
}

pub fn derive_plugin_permission_dedup_key(plugin_id: PluginId, permission_digest: &str) -> String {
    format!("plugin:{plugin_id}:{permission_digest}")
}

pub fn derive_runner_failed_dedup_key(runner_id: &str, job_id: &str) -> String {
    format!("runner:{runner_id}:{job_id}")
}

// --- Producer helpers --------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub async fn produce_entry(
    conn: &mut SqliteConnection,
    owner_uid: u32,
    repository_id: RepositoryId,
    kind: InboxEntryKind,
    title: String,
    summary: String,
    identity: InboxSourceIdentity,
    dedup_key: String,
    deep_link: InboxDeepLink,
    session_id: Option<SessionId>,
    run_id: Option<RunId>,
    workflow_id: Option<WorkflowId>,
    occurred_at: DateTime<Utc>,
) -> Result<InboxEntry, InboxError> {
    let id = InboxEntryId::new();
    let kind_db = kind_to_db(kind)?;
    let source_identity_json =
        serde_json::to_string(&identity).map_err(|e| InboxError::InvalidData(e.to_string()))?;
    let deep_link_json =
        serde_json::to_string(&deep_link).map_err(|e| InboxError::InvalidData(e.to_string()))?;
    let occurred_at_str = occurred_at.to_rfc3339();

    sqlx::query(
        "INSERT INTO inbox_entries (
            id, owner_uid, repository_id, kind, state, title, summary,
            source_identity_json, dedup_key, deep_link_json,
            session_id, run_id, workflow_id,
            created_at, updated_at
        ) VALUES (?, ?, ?, ?, 'Unread', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(owner_uid, dedup_key) DO UPDATE SET
            title = excluded.title,
            summary = excluded.summary,
            deep_link_json = excluded.deep_link_json,
            source_identity_json = excluded.source_identity_json,
            session_id = excluded.session_id,
            run_id = excluded.run_id,
            workflow_id = excluded.workflow_id,
            updated_at = excluded.updated_at",
    )
    .bind(id.to_string())
    .bind(i64::from(owner_uid))
    .bind(repository_id.to_string())
    .bind(kind_db)
    .bind(title)
    .bind(summary)
    .bind(source_identity_json)
    .bind(&dedup_key)
    .bind(deep_link_json)
    .bind(session_id.map(|s| s.to_string()))
    .bind(run_id.map(|r| r.to_string()))
    .bind(workflow_id.map(|w| w.to_string()))
    .bind(&occurred_at_str)
    .bind(&occurred_at_str)
    .execute(&mut *conn)
    .await?;

    let row = sqlx::query_as::<_, InboxEntryRow>(
        "SELECT id, owner_uid, repository_id, kind, state, title, summary, \
         source_identity_json, dedup_key, deep_link_json, session_id, run_id, workflow_id, \
         created_at, updated_at, acknowledged_at, dismissed_at, resolved_at \
         FROM inbox_entries WHERE owner_uid = ? AND dedup_key = ?",
    )
    .bind(i64::from(owner_uid))
    .bind(&dedup_key)
    .fetch_one(&mut *conn)
    .await?;

    InboxEntry::try_from(row)
}

#[allow(clippy::too_many_arguments)]
pub async fn produce_approval_request(
    conn: &mut SqliteConnection,
    owner_uid: u32,
    repository_id: RepositoryId,
    session_id: SessionId,
    run_id: RunId,
    approval_id: ApprovalId,
    title: String,
    summary: String,
    occurred_at: DateTime<Utc>,
) -> Result<InboxEntry, InboxError> {
    let identity = InboxSourceIdentity::Approval { approval_id };
    let dedup_key = derive_approval_dedup_key(approval_id);
    let deep_link = InboxDeepLink::Approval { approval_id };
    produce_entry(
        conn,
        owner_uid,
        repository_id,
        InboxEntryKind::ApprovalRequest,
        title,
        summary,
        identity,
        dedup_key,
        deep_link,
        Some(session_id),
        Some(run_id),
        None,
        occurred_at,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn produce_agent_question(
    conn: &mut SqliteConnection,
    owner_uid: u32,
    repository_id: RepositoryId,
    session_id: SessionId,
    run_id: RunId,
    question_id: QuestionId,
    title: String,
    summary: String,
    occurred_at: DateTime<Utc>,
) -> Result<InboxEntry, InboxError> {
    let identity = InboxSourceIdentity::Question { question_id };
    let dedup_key = derive_question_dedup_key(question_id);
    let deep_link = InboxDeepLink::Question { question_id };
    produce_entry(
        conn,
        owner_uid,
        repository_id,
        InboxEntryKind::AgentQuestion,
        title,
        summary,
        identity,
        dedup_key,
        deep_link,
        Some(session_id),
        Some(run_id),
        None,
        occurred_at,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn produce_run_terminal(
    conn: &mut SqliteConnection,
    owner_uid: u32,
    repository_id: RepositoryId,
    session_id: SessionId,
    run_id: RunId,
    workflow_id: Option<WorkflowId>,
    kind: InboxEntryKind,
    title: String,
    summary: String,
    occurred_at: DateTime<Utc>,
) -> Result<InboxEntry, InboxError> {
    let identity = InboxSourceIdentity::Run { run_id };
    let dedup_key = derive_run_terminal_dedup_key(run_id);
    let deep_link = InboxDeepLink::Run { session_id, run_id };
    produce_entry(
        conn,
        owner_uid,
        repository_id,
        kind,
        title,
        summary,
        identity,
        dedup_key,
        deep_link,
        Some(session_id),
        Some(run_id),
        workflow_id,
        occurred_at,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn produce_budget_warning(
    conn: &mut SqliteConnection,
    owner_uid: u32,
    repository_id: RepositoryId,
    budget_id: String,
    window_start: &str,
    session_id: Option<SessionId>,
    run_id: Option<RunId>,
    workflow_id: Option<WorkflowId>,
    title: String,
    summary: String,
    occurred_at: DateTime<Utc>,
) -> Result<InboxEntry, InboxError> {
    let identity = InboxSourceIdentity::Budget {
        budget_id: budget_id.clone(),
    };
    let dedup_key = derive_budget_dedup_key(&budget_id, window_start);
    let deep_link = InboxDeepLink::Repository { repository_id };
    produce_entry(
        conn,
        owner_uid,
        repository_id,
        InboxEntryKind::BudgetWarning,
        title,
        summary,
        identity,
        dedup_key,
        deep_link,
        session_id,
        run_id,
        workflow_id,
        occurred_at,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn produce_workflow_blocked(
    conn: &mut SqliteConnection,
    owner_uid: u32,
    repository_id: RepositoryId,
    workflow_id: WorkflowId,
    workflow_run_id: &str,
    node_id: &str,
    session_id: Option<SessionId>,
    run_id: Option<RunId>,
    title: String,
    summary: String,
    occurred_at: DateTime<Utc>,
) -> Result<InboxEntry, InboxError> {
    let identity = InboxSourceIdentity::Workflow { workflow_id };
    let dedup_key = derive_workflow_blocked_dedup_key(workflow_run_id, node_id);
    let deep_link = InboxDeepLink::Workflow { workflow_id };
    produce_entry(
        conn,
        owner_uid,
        repository_id,
        InboxEntryKind::WorkflowBlocked,
        title,
        summary,
        identity,
        dedup_key,
        deep_link,
        session_id,
        run_id,
        Some(workflow_id),
        occurred_at,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn produce_plugin_permission_changed(
    conn: &mut SqliteConnection,
    owner_uid: u32,
    repository_id: RepositoryId,
    plugin_id: PluginId,
    permission_digest: &str,
    session_id: Option<SessionId>,
    title: String,
    summary: String,
    occurred_at: DateTime<Utc>,
) -> Result<InboxEntry, InboxError> {
    let identity = InboxSourceIdentity::Plugin { plugin_id };
    let dedup_key = derive_plugin_permission_dedup_key(plugin_id, permission_digest);
    let deep_link = InboxDeepLink::Plugin { plugin_id };
    produce_entry(
        conn,
        owner_uid,
        repository_id,
        InboxEntryKind::PluginPermissionChanged,
        title,
        summary,
        identity,
        dedup_key,
        deep_link,
        session_id,
        None,
        None,
        occurred_at,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn produce_runner_failed(
    conn: &mut SqliteConnection,
    owner_uid: u32,
    repository_id: RepositoryId,
    runner_id: String,
    job_id: &str,
    session_id: Option<SessionId>,
    run_id: Option<RunId>,
    title: String,
    summary: String,
    occurred_at: DateTime<Utc>,
) -> Result<InboxEntry, InboxError> {
    let identity = InboxSourceIdentity::Runner {
        runner_id: runner_id.clone(),
    };
    let dedup_key = derive_runner_failed_dedup_key(&runner_id, job_id);
    let deep_link = InboxDeepLink::Repository { repository_id };
    produce_entry(
        conn,
        owner_uid,
        repository_id,
        InboxEntryKind::RunnerFailed,
        title,
        summary,
        identity,
        dedup_key,
        deep_link,
        session_id,
        run_id,
        None,
        occurred_at,
    )
    .await
}

// --- Resolution sweep helpers ------------------------------------------------

pub async fn resolve_approval_entry(
    conn: &mut SqliteConnection,
    approval_id: ApprovalId,
    resolved_at: DateTime<Utc>,
) -> Result<u64, InboxError> {
    let now_str = resolved_at.to_rfc3339();
    let dedup_key = derive_approval_dedup_key(approval_id);
    let result = sqlx::query(
        "UPDATE inbox_entries \
         SET state = 'Resolved', \
             resolved_at = COALESCE(resolved_at, ?), \
             updated_at = ? \
         WHERE dedup_key = ? AND state != 'Resolved'",
    )
    .bind(&now_str)
    .bind(&now_str)
    .bind(&dedup_key)
    .execute(&mut *conn)
    .await?;
    Ok(result.rows_affected())
}

pub async fn resolve_question_entry(
    conn: &mut SqliteConnection,
    question_id: QuestionId,
    resolved_at: DateTime<Utc>,
) -> Result<u64, InboxError> {
    let now_str = resolved_at.to_rfc3339();
    let dedup_key = derive_question_dedup_key(question_id);
    let result = sqlx::query(
        "UPDATE inbox_entries \
         SET state = 'Resolved', \
             resolved_at = COALESCE(resolved_at, ?), \
             updated_at = ? \
         WHERE dedup_key = ? AND state != 'Resolved'",
    )
    .bind(&now_str)
    .bind(&now_str)
    .bind(&dedup_key)
    .execute(&mut *conn)
    .await?;
    Ok(result.rows_affected())
}

pub async fn resolve_run_entries(
    conn: &mut SqliteConnection,
    run_id: RunId,
    resolved_at: DateTime<Utc>,
) -> Result<u64, InboxError> {
    let now_str = resolved_at.to_rfc3339();
    let result = sqlx::query(
        "UPDATE inbox_entries \
         SET state = 'Resolved', \
             resolved_at = COALESCE(resolved_at, ?), \
             updated_at = ? \
         WHERE run_id = ? AND state != 'Resolved' AND kind NOT IN ('RunCompleted', 'RunFailed')",
    )
    .bind(&now_str)
    .bind(&now_str)
    .bind(run_id.to_string())
    .execute(&mut *conn)
    .await?;
    Ok(result.rows_affected())
}

// --- Delivery attempts (Task 3.2 notifications) -----------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryState {
    Delivered,
    Suppressed,
    Failed,
}

impl DeliveryState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::Suppressed => "suppressed",
            Self::Failed => "failed",
        }
    }
}

pub async fn record_delivery_attempt(
    conn: &mut SqliteConnection,
    entry_id: InboxEntryId,
    adapter: &str,
    client_id: Option<&str>,
    state: DeliveryState,
    detail: Option<&str>,
    attempted_at: DateTime<Utc>,
) -> Result<bool, InboxError> {
    // Policy check: email and chat adapters are disabled by policy and produce no row.
    if adapter == "email" || adapter == "chat" {
        return Ok(false);
    }

    let result = sqlx::query(
        "INSERT INTO inbox_delivery_attempts (entry_id, adapter, client_id, state, attempted_at, detail) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(entry_id.to_string())
    .bind(adapter)
    .bind(client_id)
    .bind(state.as_str())
    .bind(attempted_at.to_rfc3339())
    .bind(detail)
    .execute(&mut *conn)
    .await;

    match result {
        Ok(_) => Ok(true),
        Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
            // Already delivered (UNIQUE constraint on (entry_id, adapter) WHERE state = 'delivered')
            Ok(false)
        }
        Err(err) => Err(InboxError::Database(err)),
    }
}

// --- InboxStore --------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
pub struct InboxStore;

impl InboxStore {
    pub fn new() -> Self {
        Self
    }

    pub async fn list(
        &self,
        pool: &SqlitePool,
        daemon_uid: u32,
        principal: PeerPrincipal,
        query: &InboxListQuery,
    ) -> Result<InboxPage, InboxError> {
        list_entries(pool, daemon_uid, principal, query).await
    }

    pub async fn count(
        &self,
        pool: &SqlitePool,
        daemon_uid: u32,
        principal: PeerPrincipal,
        filters: &InboxListFilters,
    ) -> Result<u64, InboxError> {
        count_entries(pool, daemon_uid, principal, filters).await
    }

    pub async fn mutate(
        &self,
        pool: &SqlitePool,
        principal: PeerPrincipal,
        mutation: &InboxMutation,
        occurred_at: DateTime<Utc>,
    ) -> Result<InboxEntry, InboxError> {
        let mut conn = pool.acquire().await?;
        apply_mutation(&mut conn, principal, mutation, occurred_at).await
    }
}
