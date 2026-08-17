//! Owner-scoped Session Library queries.
//!
//! Search is a read model over authoritative session metadata. The caller must
//! supply the transport-derived [`PeerPrincipal`]; no owner value from the wire
//! enters this module. Results can originate from titles, durable
//! transcript/tool/patch events, artifact references, changed paths, and typed
//! IDE symbol context, with deterministic ranking and opaque keyset cursors.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::str::FromStr;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use codypendent_protocol::{
    ArtifactId, ArtifactRef, CommandBody, CommandId, EventBody, InputBlock, PageCursor,
    RepositoryId, RunId, RunState, SessionDeepLink, SessionId, SessionSearchPage,
    SessionSearchQuery, SessionSearchResult, SessionSearchScope, SessionSearchSource,
    SessionSummary, WorkspaceId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{QueryBuilder, Sqlite, SqliteConnection, SqlitePool};

use crate::principal::PeerPrincipal;

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 200;
const CURSOR_VERSION: u8 = 1;

/// A Session Library failure safe to translate at the protocol boundary.
#[derive(Debug, thiserror::Error)]
pub enum SessionLibraryError {
    #[error("invalid or stale session search cursor")]
    InvalidCursor,
    #[error("session library database query failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("session library contains invalid data: {0}")]
    InvalidData(String),
}

#[derive(Debug, sqlx::FromRow)]
struct SessionRow {
    id: String,
    workspace_id: Option<String>,
    title: String,
    state: String,
    updated_at: String,
    created_at: String,
    internal: i64,
    parent_session_id: Option<String>,
    parent_run_id: Option<String>,
    pinned: i64,
    archived_at: Option<String>,
    repository_id: Option<String>,
    repository: Option<String>,
    workspace: Option<String>,
    last_activity_at: Option<String>,
    last_run_id: Option<String>,
    run_state: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct EventRow {
    session_id: String,
    sequence: i64,
    body: String,
}

#[derive(Debug, sqlx::FromRow)]
struct CommandRow {
    id: String,
    session_id: String,
    body: String,
}

#[derive(Debug)]
struct SearchSourceEntry {
    source_type: &'static str,
    source_id: String,
    content_hash: String,
    event_sequence: Option<i64>,
    run_id: Option<RunId>,
    artifact_id: Option<ArtifactId>,
}

#[derive(Debug)]
struct Candidate {
    result: SessionSearchResult,
    pinned: bool,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SearchCursor {
    version: u8,
    query_hash: String,
    score_bits: u64,
    pinned: bool,
    updated_at: DateTime<Utc>,
    stable_identity: String,
}

/// Search session metadata and durable content visible to `principal`.
///
/// The owner predicate is part of the SQL query, before ranking, counts, or
/// pagination. A caller therefore cannot infer another user's matches from page
/// length or cursor presence. Rows predating owner metadata resolve to the
/// daemon uid, matching the daemon's central ownership gate.
pub async fn search_sessions(
    pool: &SqlitePool,
    daemon_uid: u32,
    principal: PeerPrincipal,
    query: &SessionSearchQuery,
) -> Result<SessionSearchPage, SessionLibraryError> {
    // Keep authorization, lifecycle visibility, source loading, ranking, and
    // paging on one SQLite snapshot. A concurrent owner/tombstone mutation
    // therefore cannot leak sources selected from stale session metadata.
    let mut tx = pool.begin().await?;
    let query_hash = query_hash(principal.uid(), query)?;
    let cursor = query
        .cursor
        .as_ref()
        .map(|cursor| decode_cursor(cursor, &query_hash))
        .transpose()?;
    let rows = load_visible_sessions(&mut tx, daemon_uid, principal, query).await?;
    let sessions = rows
        .into_iter()
        .map(SessionSummary::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let mut candidates = sessions
        .iter()
        .cloned()
        .filter_map(|session| title_candidate(session, &query.query))
        .collect::<Vec<_>>();
    if !query.query.trim().is_empty() {
        let summaries = sessions
            .iter()
            .map(|session| (session.session_id, session))
            .collect::<HashMap<_, _>>();
        for row in load_visible_events(&mut tx, &sessions).await? {
            let session_id = row.session_id.parse().map_err(|error| {
                SessionLibraryError::InvalidData(format!("invalid event session id: {error}"))
            })?;
            let Some(session) = summaries.get(&session_id) else {
                continue;
            };
            let sequence = u64::try_from(row.sequence).map_err(|error| {
                SessionLibraryError::InvalidData(format!("invalid event sequence: {error}"))
            })?;
            let body: EventBody = serde_json::from_str(&row.body).map_err(|error| {
                SessionLibraryError::InvalidData(format!("invalid event body: {error}"))
            })?;
            candidates.extend(event_candidates(session, sequence, &body, &query.query));
        }
        for row in load_visible_commands(&mut tx, &sessions).await? {
            let session_id = row.session_id.parse().map_err(|error| {
                SessionLibraryError::InvalidData(format!("invalid command session id: {error}"))
            })?;
            let Some(session) = summaries.get(&session_id) else {
                continue;
            };
            let body: CommandBody = serde_json::from_str(&row.body).map_err(|error| {
                SessionLibraryError::InvalidData(format!("invalid command body: {error}"))
            })?;
            candidates.extend(command_candidates(session, &row.id, &body, &query.query));
        }
    }
    candidates.sort_by(compare_candidates);

    if let Some(cursor) = &cursor {
        candidates.retain(|candidate| candidate_is_after(candidate, cursor));
    }

    let limit = usize::try_from(if query.limit == 0 {
        DEFAULT_PAGE_SIZE
    } else {
        query.limit.min(MAX_PAGE_SIZE)
    })
    .unwrap_or(MAX_PAGE_SIZE as usize);
    let has_more = candidates.len() > limit;
    let items = candidates
        .iter()
        .take(limit)
        .map(|candidate| candidate.result.clone())
        .collect::<Vec<_>>();
    let next_cursor = if has_more {
        candidates
            .get(limit.saturating_sub(1))
            .map(|candidate| encode_cursor(candidate, &query_hash))
            .transpose()?
    } else {
        None
    };

    tx.commit().await?;
    Ok(SessionSearchPage { items, next_cursor })
}

/// Record the source-of-truth hash for a session title in the same transaction
/// that creates or renames the session. The text remains in `sessions`; this
/// row is rebuild/freshness bookkeeping for the derived search index.
pub(crate) async fn index_title_source(
    exec: impl sqlx::SqliteExecutor<'_>,
    session_id: SessionId,
    title: &str,
    indexed_at: &str,
) -> Result<(), sqlx::Error> {
    let content_hash = hex::encode(Sha256::digest(title.as_bytes()));
    sqlx::query(
        "INSERT INTO session_search_sources \
         (session_id, source_type, source_id, content_hash, indexed_at) \
         VALUES (?, 'title', 'title', ?, ?) \
         ON CONFLICT(session_id, source_type, source_id) DO UPDATE SET \
         content_hash = excluded.content_hash, indexed_at = excluded.indexed_at",
    )
    .bind(session_id.to_string())
    .bind(content_hash)
    .bind(indexed_at)
    .execute(exec)
    .await?;
    Ok(())
}

/// Index every searchable projection of one newly appended durable event.
///
/// The event row must already exist on `conn`; callers invoke this inside the
/// same transaction as the ledger append. Stable `(source_type, source_id)`
/// keys make retries and deterministic rebuilds safe.
pub(crate) async fn index_event_sources(
    conn: &mut SqliteConnection,
    session_id: SessionId,
    sequence: i64,
    body: &EventBody,
    indexed_at: &str,
) -> Result<usize, sqlx::Error> {
    let entries = event_source_entries(sequence, body);
    for entry in &entries {
        write_source_entry(&mut *conn, session_id, entry, indexed_at).await?;
    }
    Ok(entries.len())
}

/// Index typed IDE context carried by a successfully applied command.
pub(crate) async fn index_command_sources(
    conn: &mut SqliteConnection,
    session_id: SessionId,
    command_id: CommandId,
    body: &CommandBody,
    indexed_at: &str,
) -> Result<usize, sqlx::Error> {
    let entries = command_source_entries(command_id, body);
    for entry in &entries {
        write_source_entry(&mut *conn, session_id, entry, indexed_at).await?;
    }
    Ok(entries.len())
}

/// Atomically reconstruct source bookkeeping from the authoritative session,
/// event, and applied-command rows.
///
/// A crash can therefore interrupt incremental indexing without losing search
/// provenance permanently: the next rebuild either commits the complete,
/// deterministic source set or leaves the previous set untouched.
pub async fn rebuild_search_sources(pool: &SqlitePool) -> Result<usize, SessionLibraryError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let sessions: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id, title, updated_at FROM sessions ORDER BY id")
            .fetch_all(&mut *tx)
            .await?;
    let events: Vec<(String, i64, String, String)> = sqlx::query_as(
        "SELECT session_id, sequence, body, occurred_at FROM events \
         ORDER BY session_id, sequence",
    )
    .fetch_all(&mut *tx)
    .await?;
    let commands: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT id, session_id, body, COALESCE(applied_at, received_at) FROM commands \
         WHERE status = 'applied' AND session_id IS NOT NULL \
         ORDER BY session_id, received_at, id",
    )
    .fetch_all(&mut *tx)
    .await?;

    sqlx::query("DELETE FROM session_search_sources")
        .execute(&mut *tx)
        .await?;
    for (session_id, title, indexed_at) in sessions {
        let session_id = session_id.parse().map_err(|error| {
            SessionLibraryError::InvalidData(format!("invalid session id: {error}"))
        })?;
        index_title_source(&mut *tx, session_id, &title, &indexed_at).await?;
    }
    for (session_id, sequence, body, indexed_at) in events {
        let session_id = session_id.parse().map_err(|error| {
            SessionLibraryError::InvalidData(format!("invalid event session id: {error}"))
        })?;
        let body = serde_json::from_str(&body).map_err(|error| {
            SessionLibraryError::InvalidData(format!("invalid event body: {error}"))
        })?;
        index_event_sources(&mut tx, session_id, sequence, &body, &indexed_at).await?;
    }
    for (command_id, session_id, body, indexed_at) in commands {
        let command_id = command_id.parse().map_err(|error| {
            SessionLibraryError::InvalidData(format!("invalid command id: {error}"))
        })?;
        let session_id = session_id.parse().map_err(|error| {
            SessionLibraryError::InvalidData(format!("invalid command session id: {error}"))
        })?;
        let body = serde_json::from_str(&body).map_err(|error| {
            SessionLibraryError::InvalidData(format!("invalid command body: {error}"))
        })?;
        index_command_sources(&mut tx, session_id, command_id, &body, &indexed_at).await?;
    }
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM session_search_sources")
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    usize::try_from(count).map_err(|error| SessionLibraryError::InvalidData(error.to_string()))
}

async fn write_source_entry(
    conn: &mut SqliteConnection,
    session_id: SessionId,
    entry: &SearchSourceEntry,
    indexed_at: &str,
) -> Result<(), sqlx::Error> {
    // Event fixtures and imported legacy ledgers can name a run/artifact whose
    // projection row is absent. Preserve the searchable source while only
    // attaching optional foreign-key provenance that is actually resolvable.
    let run_id = if let Some(run_id) = entry.run_id {
        sqlx::query_scalar::<_, String>("SELECT id FROM runs WHERE id = ?")
            .bind(run_id.to_string())
            .fetch_optional(&mut *conn)
            .await?
    } else {
        None
    };
    let artifact_id = if let Some(artifact_id) = entry.artifact_id {
        sqlx::query_scalar::<_, String>("SELECT id FROM artifacts WHERE id = ?")
            .bind(artifact_id.to_string())
            .fetch_optional(&mut *conn)
            .await?
    } else {
        None
    };
    sqlx::query(
        "INSERT INTO session_search_sources \
         (session_id, source_type, source_id, content_hash, indexed_at, \
          event_sequence, run_id, artifact_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(session_id, source_type, source_id) DO UPDATE SET \
         content_hash = excluded.content_hash, indexed_at = excluded.indexed_at, \
         event_sequence = excluded.event_sequence, run_id = excluded.run_id, \
         artifact_id = excluded.artifact_id",
    )
    .bind(session_id.to_string())
    .bind(entry.source_type)
    .bind(&entry.source_id)
    .bind(&entry.content_hash)
    .bind(indexed_at)
    .bind(entry.event_sequence)
    .bind(run_id)
    .bind(artifact_id)
    .execute(conn)
    .await?;
    Ok(())
}

fn source_entry(
    source_type: &'static str,
    source_id: String,
    content: &str,
    event_sequence: Option<i64>,
    run_id: Option<RunId>,
    artifact_id: Option<ArtifactId>,
) -> SearchSourceEntry {
    SearchSourceEntry {
        source_type,
        source_id,
        content_hash: hex::encode(Sha256::digest(content.as_bytes())),
        event_sequence,
        run_id,
        artifact_id,
    }
}

fn artifact_source_entry(
    sequence: i64,
    artifact: &ArtifactRef,
    run_id: Option<RunId>,
) -> SearchSourceEntry {
    source_entry(
        "artifact",
        format!("event:{sequence}:artifact:{}", artifact.id),
        &format!(
            "{} {} {}",
            artifact.media_type, artifact.id, artifact.sha256
        ),
        Some(sequence),
        run_id,
        Some(artifact.id),
    )
}

fn event_source_entries(sequence: i64, body: &EventBody) -> Vec<SearchSourceEntry> {
    let mut entries = Vec::new();
    match body {
        EventBody::NoteAppended { text, run_id } => entries.push(source_entry(
            "transcript",
            format!("event:{sequence}:transcript"),
            text,
            Some(sequence),
            *run_id,
            None,
        )),
        EventBody::RunStarted {
            run_id, objective, ..
        } => entries.push(source_entry(
            "transcript",
            format!("event:{sequence}:objective"),
            objective,
            Some(sequence),
            Some(*run_id),
            None,
        )),
        EventBody::ModelStreamDelta { run_id, text } => entries.push(source_entry(
            "transcript",
            format!("event:{sequence}:model"),
            text,
            Some(sequence),
            Some(*run_id),
            None,
        )),
        EventBody::ToolStarted {
            run_id,
            tool,
            label,
            ..
        } => {
            let content = label
                .as_ref()
                .map_or_else(|| tool.clone(), |label| format!("{tool} {label}"));
            entries.push(source_entry(
                "tool",
                format!("event:{sequence}:tool"),
                &content,
                Some(sequence),
                Some(*run_id),
                None,
            ));
            if let Some(path) = label.as_deref().filter(|label| looks_like_path(label)) {
                entries.push(source_entry(
                    "path",
                    format!("event:{sequence}:path:{path}"),
                    path,
                    Some(sequence),
                    Some(*run_id),
                    None,
                ));
            }
        }
        EventBody::ToolCompleted {
            run_id,
            tool,
            artifact,
            ..
        } => {
            entries.push(source_entry(
                "tool",
                format!("event:{sequence}:tool-result"),
                tool,
                Some(sequence),
                Some(*run_id),
                None,
            ));
            if let Some(artifact) = artifact {
                entries.push(artifact_source_entry(sequence, artifact, Some(*run_id)));
            }
        }
        EventBody::PatchProposed {
            run_id,
            artifact,
            files,
            preview,
            ..
        } => {
            entries.push(source_entry(
                "patch",
                format!("event:{sequence}:patch"),
                preview,
                Some(sequence),
                Some(*run_id),
                None,
            ));
            for path in files {
                entries.push(source_entry(
                    "path",
                    format!("event:{sequence}:path:{path}"),
                    path,
                    Some(sequence),
                    Some(*run_id),
                    None,
                ));
            }
            entries.push(artifact_source_entry(sequence, artifact, Some(*run_id)));
        }
        EventBody::RunCompleted {
            run_id, chronicle, ..
        } => entries.push(artifact_source_entry(sequence, chronicle, Some(*run_id))),
        _ => {}
    }
    entries
}

fn command_source_entries(command_id: CommandId, body: &CommandBody) -> Vec<SearchSourceEntry> {
    let mut entries = Vec::new();
    let CommandBody::SubmitUserInput {
        envelope: Some(envelope),
        ..
    } = body
    else {
        return entries;
    };
    for (index, block) in envelope.blocks.iter().enumerate() {
        match block {
            InputBlock::CodeSymbol(symbol) => entries.push(source_entry(
                "symbol",
                format!("command:{command_id}:symbol:{index}"),
                &format!(
                    "{} {} {}",
                    symbol.symbol,
                    symbol.path,
                    symbol.kind.as_deref().unwrap_or_default()
                ),
                None,
                None,
                None,
            )),
            InputBlock::EditorSelection(selection) => entries.push(source_entry(
                "path",
                format!("command:{command_id}:selection:{index}"),
                &selection.path,
                None,
                None,
                None,
            )),
            _ => {}
        }
    }
    entries
}

async fn load_visible_sessions(
    conn: &mut SqliteConnection,
    daemon_uid: u32,
    principal: PeerPrincipal,
    query: &SessionSearchQuery,
) -> Result<Vec<SessionRow>, sqlx::Error> {
    let mut sql = QueryBuilder::<Sqlite>::new(
        "SELECT s.id, s.workspace_id, s.title, s.state, s.updated_at, s.created_at, \
         s.internal, s.parent_session_id, s.parent_run_id, s.pinned, s.archived_at, \
         s.repository_id, s.repository, s.workspace, \
         COALESCE(s.last_activity_at, \
             (SELECT MAX(e.occurred_at) FROM events e WHERE e.session_id = s.id)) \
             AS last_activity_at, \
         COALESCE(s.last_run_id, \
             (SELECT r.id FROM runs r WHERE r.session_id = s.id ORDER BY r.rowid DESC LIMIT 1)) \
             AS last_run_id, \
         COALESCE(s.run_state, \
             (SELECT r.state FROM runs r WHERE r.session_id = s.id ORDER BY r.rowid DESC LIMIT 1)) \
             AS run_state \
         FROM sessions s WHERE COALESCE(s.owner_uid, ",
    );
    sql.push_bind(i64::from(daemon_uid));
    sql.push(") = ");
    sql.push_bind(i64::from(principal.uid()));
    sql.push(" AND s.tombstoned_at IS NULL");
    if query.query.trim().is_empty() {
        sql.push(" AND s.internal = 0 AND s.archived_at IS NULL");
    }

    if !query.filters.repository_ids.is_empty() {
        sql.push(" AND s.repository_id IN (");
        let mut values = sql.separated(", ");
        for repository_id in &query.filters.repository_ids {
            values.push_bind(repository_id.to_string());
        }
        values.push_unseparated(")");
    }
    if let Some(after) = query.filters.created_after {
        sql.push(" AND s.created_at >= ");
        sql.push_bind(after.to_rfc3339());
    }
    if let Some(before) = query.filters.created_before {
        sql.push(" AND s.created_at <= ");
        sql.push_bind(before.to_rfc3339());
    }
    if !query.filters.run_states.is_empty() {
        sql.push(
            " AND COALESCE(s.run_state, \
             (SELECT r.state FROM runs r WHERE r.session_id = s.id \
              ORDER BY r.rowid DESC LIMIT 1)) IN (",
        );
        let mut values = sql.separated(", ");
        for state in &query.filters.run_states {
            values.push_bind(run_state_to_db(*state));
        }
        values.push_unseparated(")");
    }
    if !query.filters.workflow_ids.is_empty() {
        sql.push(
            " AND EXISTS (SELECT 1 FROM workflow_runs wr JOIN runs r ON r.id = wr.run_id \
             WHERE r.session_id = s.id AND wr.workflow_id IN (",
        );
        let mut values = sql.separated(", ");
        for workflow_id in &query.filters.workflow_ids {
            values.push_bind(workflow_id.to_string());
        }
        values.push_unseparated("))");
    }
    if !query.filters.model_ids.is_empty() {
        sql.push(
            " AND EXISTS (SELECT 1 FROM model_task_outcomes mo JOIN runs r ON r.id = mo.run_id \
             WHERE r.session_id = s.id AND mo.model_id IN (",
        );
        let mut values = sql.separated(", ");
        for model_id in &query.filters.model_ids {
            values.push_bind(model_id.to_string());
        }
        values.push_unseparated("))");
    }

    sql.build_query_as::<SessionRow>()
        .fetch_all(&mut *conn)
        .await
}

async fn load_visible_events(
    conn: &mut SqliteConnection,
    sessions: &[SessionSummary],
) -> Result<Vec<EventRow>, sqlx::Error> {
    let mut events = Vec::new();
    // Stay comfortably below SQLite's host-parameter ceiling even for a large
    // library. Ownership has already been applied to `sessions`; only those ids
    // are ever materialized here.
    for chunk in sessions.chunks(400) {
        let mut sql = QueryBuilder::<Sqlite>::new(
            "SELECT session_id, sequence, body FROM events WHERE session_id IN (",
        );
        let mut ids = sql.separated(", ");
        for session in chunk {
            ids.push_bind(session.session_id.to_string());
        }
        ids.push_unseparated(") ORDER BY session_id, sequence");
        events.extend(
            sql.build_query_as::<EventRow>()
                .fetch_all(&mut *conn)
                .await?,
        );
    }
    Ok(events)
}

async fn load_visible_commands(
    conn: &mut SqliteConnection,
    sessions: &[SessionSummary],
) -> Result<Vec<CommandRow>, sqlx::Error> {
    let mut commands = Vec::new();
    for chunk in sessions.chunks(400) {
        let mut sql = QueryBuilder::<Sqlite>::new(
            "SELECT id, session_id, body FROM commands WHERE status = 'applied' \
             AND session_id IN (",
        );
        let mut ids = sql.separated(", ");
        for session in chunk {
            ids.push_bind(session.session_id.to_string());
        }
        ids.push_unseparated(") ORDER BY session_id, received_at, id");
        commands.extend(
            sql.build_query_as::<CommandRow>()
                .fetch_all(&mut *conn)
                .await?,
        );
    }
    Ok(commands)
}

fn title_candidate(session: SessionSummary, raw_query: &str) -> Option<Candidate> {
    let score = title_score(&session.title, raw_query)?;
    let stable_identity = format!("session:{}:title", session.session_id);
    let updated_at = session.updated_at;
    let pinned = session.pinned;
    Some(Candidate {
        result: SessionSearchResult {
            deep_link: SessionDeepLink::Session {
                session_id: session.session_id,
            },
            session,
            source: SessionSearchSource::Title,
            scope: SessionSearchScope::Session,
            stable_identity,
            score,
            excerpt: None,
        },
        pinned,
        updated_at,
    })
}

fn event_candidates(
    session: &SessionSummary,
    sequence: u64,
    body: &EventBody,
    query: &str,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    let event_link = SessionDeepLink::Event {
        session_id: session.session_id,
        sequence,
    };
    match body {
        EventBody::NoteAppended { text, .. } => push_text_candidate(
            &mut candidates,
            session,
            SessionSearchSource::Transcript,
            format!("event:{sequence}:transcript"),
            event_link,
            text,
            query,
            600.0,
        ),
        EventBody::RunStarted { objective, .. } => push_text_candidate(
            &mut candidates,
            session,
            SessionSearchSource::Transcript,
            format!("event:{sequence}:objective"),
            event_link,
            objective,
            query,
            650.0,
        ),
        EventBody::ModelStreamDelta { text, .. } => push_text_candidate(
            &mut candidates,
            session,
            SessionSearchSource::Transcript,
            format!("event:{sequence}:model"),
            event_link,
            text,
            query,
            550.0,
        ),
        EventBody::ToolStarted { tool, label, .. } => {
            let text = label
                .as_ref()
                .map_or_else(|| tool.clone(), |label| format!("{tool} {label}"));
            push_text_candidate(
                &mut candidates,
                session,
                SessionSearchSource::ToolObservation,
                format!("event:{sequence}:tool"),
                event_link.clone(),
                &text,
                query,
                700.0,
            );
            if let Some(path) = label.as_deref().filter(|label| looks_like_path(label)) {
                push_text_candidate(
                    &mut candidates,
                    session,
                    SessionSearchSource::ChangedPath,
                    format!("event:{sequence}:path:{path}"),
                    SessionDeepLink::Path {
                        session_id: session.session_id,
                        path: path.to_string(),
                        line: None,
                        column: None,
                    },
                    path,
                    query,
                    750.0,
                );
            }
        }
        EventBody::ToolCompleted { tool, artifact, .. } => {
            push_text_candidate(
                &mut candidates,
                session,
                SessionSearchSource::ToolObservation,
                format!("event:{sequence}:tool-result"),
                event_link,
                tool,
                query,
                680.0,
            );
            if let Some(artifact) = artifact {
                push_artifact_candidate(&mut candidates, session, sequence, artifact, query);
            }
        }
        EventBody::PatchProposed {
            artifact,
            files,
            preview,
            ..
        } => {
            push_text_candidate(
                &mut candidates,
                session,
                SessionSearchSource::Patch,
                format!("event:{sequence}:patch"),
                event_link,
                preview,
                query,
                800.0,
            );
            for path in files {
                push_text_candidate(
                    &mut candidates,
                    session,
                    SessionSearchSource::ChangedPath,
                    format!("event:{sequence}:path:{path}"),
                    SessionDeepLink::Path {
                        session_id: session.session_id,
                        path: path.clone(),
                        line: None,
                        column: None,
                    },
                    path,
                    query,
                    750.0,
                );
            }
            push_artifact_candidate(&mut candidates, session, sequence, artifact, query);
        }
        EventBody::RunCompleted { chronicle, .. } => {
            push_artifact_candidate(&mut candidates, session, sequence, chronicle, query);
        }
        _ => {}
    }
    candidates
}

fn command_candidates(
    session: &SessionSummary,
    command_id: &str,
    body: &CommandBody,
    query: &str,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    let CommandBody::SubmitUserInput {
        envelope: Some(envelope),
        ..
    } = body
    else {
        return candidates;
    };
    for (index, block) in envelope.blocks.iter().enumerate() {
        match block {
            InputBlock::CodeSymbol(symbol) => {
                let text = format!(
                    "{} {} {}",
                    symbol.symbol,
                    symbol.path,
                    symbol.kind.as_deref().unwrap_or_default()
                );
                push_text_candidate(
                    &mut candidates,
                    session,
                    SessionSearchSource::Symbol,
                    format!("command:{command_id}:symbol:{index}"),
                    SessionDeepLink::Symbol {
                        session_id: session.session_id,
                        symbol: symbol.symbol.clone(),
                        path: Some(symbol.path.clone()),
                    },
                    &text,
                    query,
                    850.0,
                );
            }
            InputBlock::EditorSelection(selection) => push_text_candidate(
                &mut candidates,
                session,
                SessionSearchSource::ChangedPath,
                format!("command:{command_id}:selection:{index}"),
                SessionDeepLink::Path {
                    session_id: session.session_id,
                    path: selection.path.clone(),
                    line: Some(selection.range.start.line),
                    column: Some(selection.range.start.character),
                },
                &selection.path,
                query,
                750.0,
            ),
            _ => {}
        }
    }
    candidates
}

#[allow(clippy::too_many_arguments)]
fn push_text_candidate(
    candidates: &mut Vec<Candidate>,
    session: &SessionSummary,
    source: SessionSearchSource,
    identity_suffix: String,
    deep_link: SessionDeepLink,
    text: &str,
    query: &str,
    base_score: f64,
) {
    let Some(score) = text_score(text, query, base_score) else {
        return;
    };
    candidates.push(Candidate {
        result: SessionSearchResult {
            session: session.clone(),
            source,
            scope: SessionSearchScope::Session,
            stable_identity: format!("session:{}:{identity_suffix}", session.session_id),
            deep_link,
            score,
            excerpt: Some(bounded_excerpt(text)),
        },
        pinned: session.pinned,
        updated_at: session.updated_at,
    });
}

fn push_artifact_candidate(
    candidates: &mut Vec<Candidate>,
    session: &SessionSummary,
    sequence: u64,
    artifact: &ArtifactRef,
    query: &str,
) {
    let text = format!(
        "{} {} {}",
        artifact.media_type, artifact.id, artifact.sha256
    );
    push_text_candidate(
        candidates,
        session,
        SessionSearchSource::Artifact,
        format!("event:{sequence}:artifact:{}", artifact.id),
        SessionDeepLink::Artifact {
            session_id: session.session_id,
            artifact_id: artifact.id,
        },
        &text,
        query,
        500.0,
    );
}

fn text_score(text: &str, query: &str, base: f64) -> Option<f64> {
    let text = text.to_lowercase();
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return None;
    }
    let terms = query.split_whitespace().collect::<Vec<_>>();
    if terms.iter().any(|term| !text.contains(term)) {
        return None;
    }
    let quality = if text == query {
        100.0
    } else if text.starts_with(&query) {
        80.0
    } else if text.contains(&query) {
        60.0
    } else {
        40.0 + terms.len() as f64
    };
    Some(base + quality)
}

fn bounded_excerpt(text: &str) -> String {
    const MAX_EXCERPT_CHARS: usize = 240;
    let mut excerpt = text.chars().take(MAX_EXCERPT_CHARS).collect::<String>();
    if text.chars().count() > MAX_EXCERPT_CHARS {
        excerpt.push('…');
    }
    excerpt
}

fn looks_like_path(label: &str) -> bool {
    label.contains('/') || label.contains('\\')
}

fn title_score(title: &str, query: &str) -> Option<f64> {
    let title = title.to_lowercase();
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Some(0.0);
    }
    let terms = query.split_whitespace().collect::<Vec<_>>();
    if terms.iter().any(|term| !title.contains(term)) {
        return None;
    }
    if title == query {
        return Some(1_000.0);
    }
    if title.starts_with(&query) {
        return Some(800.0);
    }
    if title
        .split(|character: char| !character.is_alphanumeric())
        .any(|token| token == query)
    {
        return Some(700.0);
    }
    Some(500.0 + terms.len() as f64)
}

fn compare_candidates(left: &Candidate, right: &Candidate) -> Ordering {
    right
        .result
        .score
        .total_cmp(&left.result.score)
        .then_with(|| right.pinned.cmp(&left.pinned))
        .then_with(|| right.updated_at.cmp(&left.updated_at))
        .then_with(|| {
            left.result
                .stable_identity
                .cmp(&right.result.stable_identity)
        })
}

fn candidate_is_after(candidate: &Candidate, cursor: &SearchCursor) -> bool {
    let boundary_score = f64::from_bits(cursor.score_bits);
    match candidate.result.score.total_cmp(&boundary_score) {
        Ordering::Greater => false,
        Ordering::Less => true,
        Ordering::Equal => match candidate.pinned.cmp(&cursor.pinned) {
            Ordering::Greater => false,
            Ordering::Less => true,
            Ordering::Equal => match candidate.updated_at.cmp(&cursor.updated_at) {
                Ordering::Greater => false,
                Ordering::Less => true,
                Ordering::Equal => candidate.result.stable_identity > cursor.stable_identity,
            },
        },
    }
}

fn query_hash(
    principal_uid: u32,
    query: &SessionSearchQuery,
) -> Result<String, SessionLibraryError> {
    let payload = serde_json::to_vec(&(principal_uid, &query.query, &query.filters))
        .map_err(|error| SessionLibraryError::InvalidData(error.to_string()))?;
    Ok(hex::encode(Sha256::digest(payload)))
}

fn encode_cursor(
    candidate: &Candidate,
    query_hash: &str,
) -> Result<PageCursor, SessionLibraryError> {
    let cursor = SearchCursor {
        version: CURSOR_VERSION,
        query_hash: query_hash.to_string(),
        score_bits: candidate.result.score.to_bits(),
        pinned: candidate.pinned,
        updated_at: candidate.updated_at,
        stable_identity: candidate.result.stable_identity.clone(),
    };
    let bytes = serde_json::to_vec(&cursor)
        .map_err(|error| SessionLibraryError::InvalidData(error.to_string()))?;
    Ok(PageCursor(URL_SAFE_NO_PAD.encode(bytes)))
}

fn decode_cursor(
    cursor: &PageCursor,
    expected_query_hash: &str,
) -> Result<SearchCursor, SessionLibraryError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(&cursor.0)
        .map_err(|_| SessionLibraryError::InvalidCursor)?;
    let decoded: SearchCursor =
        serde_json::from_slice(&bytes).map_err(|_| SessionLibraryError::InvalidCursor)?;
    if decoded.version != CURSOR_VERSION
        || decoded.query_hash != expected_query_hash
        || !f64::from_bits(decoded.score_bits).is_finite()
    {
        return Err(SessionLibraryError::InvalidCursor);
    }
    Ok(decoded)
}

fn parse_time(value: &str, field: &str) -> Result<DateTime<Utc>, SessionLibraryError> {
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|error| SessionLibraryError::InvalidData(format!("invalid {field}: {error}")))
}

fn parse_optional<T>(value: Option<String>, field: &str) -> Result<Option<T>, SessionLibraryError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    value
        .map(|raw| {
            raw.parse().map_err(|error| {
                SessionLibraryError::InvalidData(format!("invalid {field}: {error}"))
            })
        })
        .transpose()
}

impl TryFrom<SessionRow> for SessionSummary {
    type Error = SessionLibraryError;

    fn try_from(row: SessionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            session_id: row.id.parse().map_err(|error| {
                SessionLibraryError::InvalidData(format!("invalid session id: {error}"))
            })?,
            workspace_id: parse_optional::<WorkspaceId>(row.workspace_id, "workspace id")?,
            title: row.title,
            state: row.state,
            updated_at: parse_time(&row.updated_at, "updated_at")?,
            created_at: parse_time(&row.created_at, "created_at")?,
            internal: row.internal != 0,
            parent_session_id: parse_optional::<SessionId>(
                row.parent_session_id,
                "parent session id",
            )?,
            parent_run_id: parse_optional::<RunId>(row.parent_run_id, "parent run id")?,
            pinned: row.pinned != 0,
            archived_at: row
                .archived_at
                .as_deref()
                .map(|value| parse_time(value, "archived_at"))
                .transpose()?,
            repository_id: parse_optional::<RepositoryId>(row.repository_id, "repository id")?,
            repository: row.repository,
            workspace: row.workspace,
            last_activity_at: row
                .last_activity_at
                .as_deref()
                .map(|value| parse_time(value, "last_activity_at"))
                .transpose()?,
            last_run_id: parse_optional::<RunId>(row.last_run_id, "last run id")?,
            run_state: row
                .run_state
                .as_deref()
                .map(run_state_from_db)
                .transpose()?,
        })
    }
}

fn run_state_to_db(state: RunState) -> &'static str {
    match state {
        RunState::Queued => "Queued",
        RunState::Preparing => "Preparing",
        RunState::Running => "Running",
        RunState::WaitingForApproval => "WaitingForApproval",
        RunState::WaitingForUserInput => "WaitingForUserInput",
        RunState::Paused => "Paused",
        RunState::Recovering => "Recovering",
        RunState::Completed => "Completed",
        RunState::Failed => "Failed",
        RunState::Cancelled => "Cancelled",
        _ => "Unknown",
    }
}

fn run_state_from_db(value: &str) -> Result<RunState, SessionLibraryError> {
    match value {
        "Queued" => Ok(RunState::Queued),
        "Preparing" => Ok(RunState::Preparing),
        "Running" => Ok(RunState::Running),
        "WaitingForApproval" => Ok(RunState::WaitingForApproval),
        "WaitingForUserInput" => Ok(RunState::WaitingForUserInput),
        "Paused" => Ok(RunState::Paused),
        "Recovering" => Ok(RunState::Recovering),
        "Completed" => Ok(RunState::Completed),
        "Failed" => Ok(RunState::Failed),
        "Cancelled" => Ok(RunState::Cancelled),
        "Unknown" => Ok(RunState::Unknown),
        other => Err(SessionLibraryError::InvalidData(format!(
            "invalid run state {other:?}"
        ))),
    }
}
