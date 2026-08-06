//! The append-only event ledger.
//!
//! Phase 0 provides create/append/load/count. Later phases add commands with
//! idempotency keys, the crash-consistency write path, projections, and
//! subscriptions — the storage shape here is already the durable ordering
//! authority they build on.

use chrono::{DateTime, Utc};
use codypendent_protocol::{Actor, EventBody, RunId, RunState, SessionEvent, SessionId};
use sqlx::SqlitePool;

/// Insert a session row in state `open`.
pub async fn create_session(
    pool: &SqlitePool,
    session_id: SessionId,
    title: &str,
) -> anyhow::Result<()> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO sessions (id, title, state, created_at, updated_at, revision) \
         VALUES (?, ?, 'open', ?, ?, 0)",
    )
    .bind(session_id.to_string())
    .bind(title)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Append one event. The caller supplies `event.sequence`; the UNIQUE primary
/// key (session_id, sequence) makes duplicate appends fail loudly instead of
/// silently forking history.
pub async fn append_event(
    pool: &SqlitePool,
    session_id: SessionId,
    event: &SessionEvent,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO events \
         (session_id, sequence, occurred_at, actor, body, causation_id, correlation_id, schema_version) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 1)",
    )
    .bind(session_id.to_string())
    .bind(i64::try_from(event.sequence)?)
    .bind(event.occurred_at.to_rfc3339())
    .bind(serde_json::to_string(&event.actor)?)
    .bind(serde_json::to_string(&event.body)?)
    .bind(event.causation_id.map(|id| id.to_string()))
    .bind(event.correlation_id.map(|id| id.to_string()))
    .execute(pool)
    .await?;
    Ok(())
}

/// Row shape of the `events` table used by `load_events`:
/// (sequence, occurred_at, actor, body, causation_id, correlation_id).
type EventRow = (i64, String, String, String, Option<String>, Option<String>);

/// Load every event for a session in sequence order.
pub async fn load_events(
    pool: &SqlitePool,
    session_id: SessionId,
) -> anyhow::Result<Vec<SessionEvent>> {
    let rows: Vec<EventRow> = sqlx::query_as(
        "SELECT sequence, occurred_at, actor, body, causation_id, correlation_id \
         FROM events WHERE session_id = ? ORDER BY sequence ASC",
    )
    .bind(session_id.to_string())
    .fetch_all(pool)
    .await?;
    rows_to_events(rows)
}

/// Load the events with `after < sequence <= through`, in sequence order. The
/// window is filtered in SQL — the `(session_id, sequence)` primary key serves
/// it — so an attach catch-up reads only the gap: a client one event behind on
/// a 100k-event session must not pay a full-history read per reconnect.
pub async fn load_events_between(
    pool: &SqlitePool,
    session_id: SessionId,
    after: u64,
    through: u64,
) -> anyhow::Result<Vec<SessionEvent>> {
    let rows: Vec<EventRow> = sqlx::query_as(
        "SELECT sequence, occurred_at, actor, body, causation_id, correlation_id \
         FROM events WHERE session_id = ? AND sequence > ? AND sequence <= ? \
         ORDER BY sequence ASC",
    )
    .bind(session_id.to_string())
    .bind(i64::try_from(after).unwrap_or(i64::MAX))
    .bind(i64::try_from(through).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await?;
    rows_to_events(rows)
}

/// Whether `session_id` exists in the sessions table. The attach path uses
/// this to reject a session id the daemon has never seen — an empty catch-up
/// must mean "empty session", never "typo'd id".
pub async fn session_exists(pool: &SqlitePool, session_id: SessionId) -> anyhow::Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM sessions WHERE id = ?")
        .bind(session_id.to_string())
        .fetch_optional(pool)
        .await?;
    Ok(row.is_some())
}

/// Decode raw event rows into [`SessionEvent`]s.
fn rows_to_events(rows: Vec<EventRow>) -> anyhow::Result<Vec<SessionEvent>> {
    let mut events = Vec::with_capacity(rows.len());
    for (sequence, occurred_at, actor, body, causation_id, correlation_id) in rows {
        events.push(SessionEvent {
            sequence: u64::try_from(sequence)?,
            occurred_at: DateTime::parse_from_rfc3339(&occurred_at)?.with_timezone(&Utc),
            causation_id: causation_id.map(|id| id.parse()).transpose()?,
            correlation_id: correlation_id.map(|id| id.parse()).transpose()?,
            actor: serde_json::from_str(&actor)?,
            body: serde_json::from_str(&body)?,
        });
    }
    Ok(events)
}

/// Load only the single most recent event for a session (the highest sequence),
/// or `None` if it has none. Cheaper than [`load_events`] when the caller needs
/// just the latest event rather than the whole history.
pub async fn load_last_event(
    pool: &SqlitePool,
    session_id: SessionId,
) -> anyhow::Result<Option<SessionEvent>> {
    let row: Option<EventRow> = sqlx::query_as(
        "SELECT sequence, occurred_at, actor, body, causation_id, correlation_id \
         FROM events WHERE session_id = ? ORDER BY sequence DESC LIMIT 1",
    )
    .bind(session_id.to_string())
    .fetch_optional(pool)
    .await?;

    match row {
        None => Ok(None),
        Some((sequence, occurred_at, actor, body, causation_id, correlation_id)) => {
            Ok(Some(SessionEvent {
                sequence: u64::try_from(sequence)?,
                occurred_at: DateTime::parse_from_rfc3339(&occurred_at)?.with_timezone(&Utc),
                causation_id: causation_id.map(|id| id.parse()).transpose()?,
                correlation_id: correlation_id.map(|id| id.parse()).transpose()?,
                actor: serde_json::from_str(&actor)?,
                body: serde_json::from_str(&body)?,
            }))
        }
    }
}

/// Atomically claim the next sequence for `session_id` and append an event,
/// returning the persisted [`SessionEvent`].
///
/// The sequence is computed *inside* the INSERT (`COALESCE(MAX(sequence),0)+1`
/// via `INSERT … SELECT … RETURNING`), so the read and the write happen under a
/// single write lock — concurrent appenders on the same session (a live run and
/// a client command such as steering, cancel, or approval resolution) can never
/// claim the same number and trip the `(session_id, sequence)` uniqueness
/// constraint. Prefer this over a separate [`next_sequence`] + [`append_event`],
/// which race. Actor/body are `System`-friendly: no causation/correlation ids.
pub async fn append_next_event(
    pool: &SqlitePool,
    session_id: SessionId,
    actor: &Actor,
    body: &EventBody,
    occurred_at: DateTime<Utc>,
) -> anyhow::Result<SessionEvent> {
    let (sequence,): (i64,) = sqlx::query_as(
        "INSERT INTO events \
         (session_id, sequence, occurred_at, actor, body, causation_id, correlation_id, schema_version) \
         SELECT ?, COALESCE(MAX(sequence), 0) + 1, ?, ?, ?, NULL, NULL, 1 \
         FROM events WHERE session_id = ? \
         RETURNING sequence",
    )
    .bind(session_id.to_string())
    .bind(occurred_at.to_rfc3339())
    .bind(serde_json::to_string(actor)?)
    .bind(serde_json::to_string(body)?)
    .bind(session_id.to_string())
    .fetch_one(pool)
    .await?;
    Ok(SessionEvent {
        sequence: u64::try_from(sequence)?,
        occurred_at,
        causation_id: None,
        correlation_id: None,
        actor: actor.clone(),
        body: body.clone(),
    })
}

/// Atomically append a `RunStateChanged` event and update the run projection.
/// The conditional projection update also makes terminal states authoritative:
/// a late runtime completion can no longer overwrite a concurrently accepted
/// cancellation.
pub async fn append_run_state_changed(
    pool: &SqlitePool,
    session_id: SessionId,
    actor: &Actor,
    run_id: RunId,
    state: RunState,
    occurred_at: DateTime<Utc>,
) -> anyhow::Result<SessionEvent> {
    let legal_from: &[RunState] = match state {
        RunState::Preparing => &[RunState::Queued],
        RunState::Running => &[
            RunState::Preparing,
            RunState::WaitingForApproval,
            RunState::WaitingForUserInput,
            RunState::Paused,
        ],
        RunState::WaitingForApproval => &[RunState::Running],
        RunState::WaitingForUserInput => &[RunState::Running],
        RunState::Paused => &[RunState::Running],
        RunState::Recovering => &[
            RunState::Preparing,
            RunState::Running,
            RunState::WaitingForApproval,
            RunState::WaitingForUserInput,
            RunState::Paused,
        ],
        RunState::Completed | RunState::Failed | RunState::Cancelled => &[
            RunState::Queued,
            RunState::Preparing,
            RunState::Running,
            RunState::WaitingForApproval,
            RunState::WaitingForUserInput,
            RunState::Paused,
            RunState::Recovering,
        ],
        RunState::Unknown => &[],
        _ => &[],
    };
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let affected =
        crate::projections::set_run_state_if_legal(&mut *tx, run_id, legal_from, state).await?;
    if affected != 1 {
        let current = crate::projections::load_run_state(&mut *tx, run_id).await?;
        return Err(anyhow::anyhow!(
            "refused stale runtime transition for {run_id}: {current:?} -> {state:?}"
        ));
    }
    let body = EventBody::RunStateChanged { run_id, state };
    let (sequence,): (i64,) = sqlx::query_as(
        "INSERT INTO events \
         (session_id, sequence, occurred_at, actor, body, causation_id, correlation_id, schema_version) \
         SELECT ?, COALESCE(MAX(sequence), 0) + 1, ?, ?, ?, NULL, NULL, 1 \
         FROM events WHERE session_id = ? RETURNING sequence",
    )
    .bind(session_id.to_string())
    .bind(occurred_at.to_rfc3339())
    .bind(serde_json::to_string(actor)?)
    .bind(serde_json::to_string(&body)?)
    .bind(session_id.to_string())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(SessionEvent {
        sequence: u64::try_from(sequence)?,
        occurred_at,
        causation_id: None,
        correlation_id: None,
        actor: actor.clone(),
        body,
    })
}

/// The next sequence number for a session (1-based).
pub async fn next_sequence(pool: &SqlitePool, session_id: SessionId) -> anyhow::Result<u64> {
    let (max,): (i64,) =
        sqlx::query_as("SELECT COALESCE(MAX(sequence), 0) FROM events WHERE session_id = ?")
            .bind(session_id.to_string())
            .fetch_one(pool)
            .await?;
    Ok(u64::try_from(max)? + 1)
}

pub async fn session_count(pool: &SqlitePool) -> anyhow::Result<i64> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sessions")
        .fetch_one(pool)
        .await?;
    Ok(count)
}

/// Count of in-flight work — both session runs and durable **workflow** runs —
/// whose `state` is **not** terminal. For `runs` the terminal `RunState`s are
/// `Completed`/`Failed`/`Cancelled` (the DB strings written by
/// [`crate::projections::run_state_to_db`]); for `workflow_runs` they are the
/// lowercase `completed`/`failed`/`cancelled` (see
/// `codypendent_workflow::store::WorkflowRunState`). Every other state —
/// `Queued`/`Preparing`/`Running`/`WaitingForApproval`/`WaitingForUserInput`/
/// `Paused`/`Recovering`/`Unknown` for runs, and `pending`/`running`/`paused`
/// for workflow runs — counts as active.
///
/// Both are included because the auto-restart idle gate must mean *no work is
/// in flight*: a restart while EITHER a session run or a workflow run is live
/// would disrupt in-memory state a stopped daemon cannot recover, so this must
/// never undercount. `workflow_runs` is created by the same migrations
/// (`0010_workflow_runs.sql`) on the same pool, so the two counts sum in one
/// query.
pub async fn active_run_count(pool: &SqlitePool) -> anyhow::Result<i64> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT \
           (SELECT COUNT(*) FROM runs \
              WHERE state NOT IN ('Completed', 'Failed', 'Cancelled')) \
         + (SELECT COUNT(*) FROM workflow_runs \
              WHERE state NOT IN ('completed', 'failed', 'cancelled'))",
    )
    .fetch_one(pool)
    .await?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_protocol::{RunId, SessionId};

    async fn test_pool(dir: &std::path::Path) -> SqlitePool {
        crate::db::open_database(&dir.join("test.db"))
            .await
            .expect("open database")
    }

    /// Insert a session, then a run in `state`, under a fresh id.
    async fn seed_run(pool: &SqlitePool, state: &str) {
        let session_id = SessionId::new();
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO sessions (id, title, state, created_at, updated_at, revision) \
             VALUES (?, ?, 'open', ?, ?, 0)",
        )
        .bind(session_id.to_string())
        .bind("active-run-count-test")
        .bind(&now)
        .bind(&now)
        .execute(pool)
        .await
        .expect("insert session");

        sqlx::query(
            "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(RunId::new().to_string())
        .bind(session_id.to_string())
        .bind("diagnose")
        .bind(state)
        .bind("Build")
        .bind("hosted-default")
        .bind("{}")
        .execute(pool)
        .await
        .expect("insert run");
    }

    #[tokio::test]
    async fn active_run_count_is_zero_with_no_runs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pool = test_pool(tmp.path()).await;
        assert_eq!(active_run_count(&pool).await.expect("count"), 0);
    }

    #[tokio::test]
    async fn terminal_only_runs_do_not_count_as_active() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pool = test_pool(tmp.path()).await;
        seed_run(&pool, "Completed").await;
        seed_run(&pool, "Failed").await;
        seed_run(&pool, "Cancelled").await;
        assert_eq!(active_run_count(&pool).await.expect("count"), 0);
    }

    #[tokio::test]
    async fn every_non_terminal_state_counts_as_active() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pool = test_pool(tmp.path()).await;
        // A settled terminal baseline...
        seed_run(&pool, "Completed").await;
        seed_run(&pool, "Failed").await;
        seed_run(&pool, "Cancelled").await;
        assert_eq!(active_run_count(&pool).await.expect("count"), 0);

        // ...then one non-terminal run at a time, each incrementing the count.
        // Includes waiting/paused states, which must count as active (never
        // undercount: a restart would disrupt them).
        let non_terminal = [
            "Queued",
            "Preparing",
            "Running",
            "WaitingForApproval",
            "WaitingForUserInput",
            "Paused",
            "Recovering",
        ];
        for (i, state) in non_terminal.iter().enumerate() {
            seed_run(&pool, state).await;
            assert_eq!(
                active_run_count(&pool).await.expect("count"),
                (i + 1) as i64,
                "state {state} must count as active"
            );
        }
    }
}
