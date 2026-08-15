//! Server-side pending-prompt queue (Adoption 06).
//!
//! Durable queue of user prompts awaiting execution or live steering.
//! Ported from Cline's `pending-prompt-service.ts` and backed by SQLite so
//! the queue survives client reconnects and daemon restarts.

use chrono::Utc;
use codypendent_protocol::{AgentMode, PendingPromptView, PromptDelivery, PromptId, SessionId};
use sqlx::{Row, SqliteConnection, SqlitePool};

/// In-memory representation of a `pending_prompts` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueEntry {
    pub id: PromptId,
    pub session_id: SessionId,
    pub position: i64,
    pub text: String,
    pub mode: AgentMode,
    pub delivery: PromptDelivery,
}

fn delivery_to_str(delivery: PromptDelivery) -> &'static str {
    match delivery {
        PromptDelivery::Steer => "steer",
        PromptDelivery::Queue | PromptDelivery::Unknown => "queue",
        _ => "queue",
    }
}

fn str_to_delivery(s: &str) -> PromptDelivery {
    match s {
        "steer" => PromptDelivery::Steer,
        _ => PromptDelivery::Queue,
    }
}

fn mode_to_str(mode: AgentMode) -> &'static str {
    match mode {
        AgentMode::Ask => "Ask",
        AgentMode::Explore => "Explore",
        AgentMode::Plan => "Plan",
        AgentMode::Build => "Build",
        AgentMode::Review => "Review",
        AgentMode::Unknown => "Build",
        _ => "Build",
    }
}

fn str_to_mode(s: &str) -> AgentMode {
    match s {
        "Ask" => AgentMode::Ask,
        "Explore" => AgentMode::Explore,
        "Plan" => AgentMode::Plan,
        "Build" => AgentMode::Build,
        "Review" => AgentMode::Review,
        _ => AgentMode::Build,
    }
}

/// Retrieve the full ordered queue snapshot for a session using a connection.
pub async fn snapshot(
    conn: &mut SqliteConnection,
    session_id: SessionId,
) -> anyhow::Result<Vec<PendingPromptView>> {
    let session_str = session_id.to_string();
    let rows = sqlx::query(
        "SELECT id, text, mode, delivery FROM pending_prompts \
         WHERE session_id = ? ORDER BY position ASC",
    )
    .bind(session_str)
    .fetch_all(&mut *conn)
    .await?;

    let mut views = Vec::with_capacity(rows.len());
    for row in rows {
        let id_str: String = row.get("id");
        let id = match id_str.parse::<PromptId>() {
            Ok(id) => id,
            Err(_) => continue,
        };
        let text: String = row.get("text");
        let mode_str: String = row.get("mode");
        let delivery_str: String = row.get("delivery");
        views.push(PendingPromptView {
            id,
            text,
            mode: str_to_mode(&mode_str),
            delivery: str_to_delivery(&delivery_str),
        });
    }
    Ok(views)
}

/// Retrieve the full ordered queue snapshot for a session using a pool.
pub async fn snapshot_pool(
    pool: &SqlitePool,
    session_id: SessionId,
) -> anyhow::Result<Vec<PendingPromptView>> {
    let mut conn = pool.acquire().await?;
    snapshot(&mut conn, session_id).await
}

/// Enqueue a prompt. Dedupes by exact prompt text: re-submitting an existing
/// prompt updates it (steer delivery wins and moves to the front).
/// Otherwise inserts a new prompt (steer to the front, queue to the back).
pub async fn enqueue(
    tx: &mut SqliteConnection,
    session_id: SessionId,
    text: &str,
    mode: AgentMode,
    delivery: PromptDelivery,
) -> anyhow::Result<Vec<PendingPromptView>> {
    let session_str = session_id.to_string();
    let mode_str = mode_to_str(mode);
    let delivery_str = delivery_to_str(delivery);

    let existing = sqlx::query(
        "SELECT id, position, delivery FROM pending_prompts \
         WHERE session_id = ? AND text = ?",
    )
    .bind(&session_str)
    .bind(text)
    .fetch_optional(&mut *tx)
    .await?;

    if let Some(row) = existing {
        let id_str: String = row.get("id");
        let old_pos: i64 = row.get("position");
        let old_del_str: String = row.get("delivery");
        let old_delivery = str_to_delivery(&old_del_str);

        let final_delivery =
            if delivery == PromptDelivery::Steer || old_delivery == PromptDelivery::Steer {
                PromptDelivery::Steer
            } else {
                PromptDelivery::Queue
            };

        let new_pos =
            if final_delivery == PromptDelivery::Steer && old_delivery != PromptDelivery::Steer {
                let min_pos: Option<i64> = sqlx::query_scalar(
                    "SELECT MIN(position) FROM pending_prompts WHERE session_id = ?",
                )
                .bind(&session_str)
                .fetch_one(&mut *tx)
                .await?;
                min_pos.unwrap_or(0) - 1
            } else {
                old_pos
            };

        sqlx::query(
            "UPDATE pending_prompts SET position = ?, mode = ?, delivery = ? \
             WHERE id = ?",
        )
        .bind(new_pos)
        .bind(mode_str)
        .bind(delivery_to_str(final_delivery))
        .bind(&id_str)
        .execute(&mut *tx)
        .await?;
    } else {
        let new_pos = if delivery == PromptDelivery::Steer {
            let min_pos: Option<i64> = sqlx::query_scalar(
                "SELECT MIN(position) FROM pending_prompts WHERE session_id = ?",
            )
            .bind(&session_str)
            .fetch_one(&mut *tx)
            .await?;
            min_pos.unwrap_or(0) - 1
        } else {
            let max_pos: Option<i64> = sqlx::query_scalar(
                "SELECT MAX(position) FROM pending_prompts WHERE session_id = ?",
            )
            .bind(&session_str)
            .fetch_one(&mut *tx)
            .await?;
            max_pos.map_or(0, |p| p + 1)
        };

        let id = PromptId::new();
        let now = Utc::now().to_rfc3339();

        sqlx::query(
            "INSERT INTO pending_prompts (id, session_id, position, text, mode, delivery, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(&session_str)
        .bind(new_pos)
        .bind(text)
        .bind(mode_str)
        .bind(delivery_str)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }

    snapshot(tx, session_id).await
}

/// Edit a queued prompt in place.
/// Newly-steer moves to front, formerly-steer-now-queue moves to back, otherwise keeps position.
/// Returns `None` if the prompt_id was not found.
pub async fn update(
    tx: &mut SqliteConnection,
    session_id: SessionId,
    prompt_id: PromptId,
    text: Option<&str>,
    delivery: Option<PromptDelivery>,
) -> anyhow::Result<Option<Vec<PendingPromptView>>> {
    let session_str = session_id.to_string();
    let prompt_str = prompt_id.to_string();

    let existing = sqlx::query(
        "SELECT position, text, mode, delivery FROM pending_prompts \
         WHERE id = ? AND session_id = ?",
    )
    .bind(&prompt_str)
    .bind(&session_str)
    .fetch_optional(&mut *tx)
    .await?;

    let row = match existing {
        Some(r) => r,
        None => return Ok(None),
    };

    let old_pos: i64 = row.get("position");
    let old_text: String = row.get("text");
    let old_del_str: String = row.get("delivery");
    let old_delivery = str_to_delivery(&old_del_str);

    let new_text = match text {
        Some(t) => {
            if t.trim().is_empty() {
                anyhow::bail!("prompt cannot be empty");
            }
            t
        }
        None => &old_text,
    };

    let (new_pos, new_delivery) = match delivery {
        Some(d) => {
            if d == PromptDelivery::Steer && old_delivery != PromptDelivery::Steer {
                let min_pos: Option<i64> = sqlx::query_scalar(
                    "SELECT MIN(position) FROM pending_prompts WHERE session_id = ?",
                )
                .bind(&session_str)
                .fetch_one(&mut *tx)
                .await?;
                (min_pos.unwrap_or(0) - 1, d)
            } else if d != PromptDelivery::Steer && old_delivery == PromptDelivery::Steer {
                let max_pos: Option<i64> = sqlx::query_scalar(
                    "SELECT MAX(position) FROM pending_prompts WHERE session_id = ?",
                )
                .bind(&session_str)
                .fetch_one(&mut *tx)
                .await?;
                (max_pos.map_or(0, |p| p + 1), d)
            } else {
                (old_pos, d)
            }
        }
        None => (old_pos, old_delivery),
    };

    sqlx::query(
        "UPDATE pending_prompts SET position = ?, text = ?, delivery = ? \
         WHERE id = ? AND session_id = ?",
    )
    .bind(new_pos)
    .bind(new_text)
    .bind(delivery_to_str(new_delivery))
    .bind(&prompt_str)
    .bind(&session_str)
    .execute(&mut *tx)
    .await?;

    let snap = snapshot(tx, session_id).await?;
    Ok(Some(snap))
}

/// Promote a queued prompt to steer (moves to front).
pub async fn promote(
    tx: &mut SqliteConnection,
    session_id: SessionId,
    prompt_id: PromptId,
) -> anyhow::Result<Option<Vec<PendingPromptView>>> {
    update(tx, session_id, prompt_id, None, Some(PromptDelivery::Steer)).await
}

/// Delete a prompt from the queue. Returns `None` if not found.
pub async fn delete(
    tx: &mut SqliteConnection,
    session_id: SessionId,
    prompt_id: PromptId,
) -> anyhow::Result<Option<Vec<PendingPromptView>>> {
    let session_str = session_id.to_string();
    let prompt_str = prompt_id.to_string();

    let res = sqlx::query("DELETE FROM pending_prompts WHERE id = ? AND session_id = ?")
        .bind(&prompt_str)
        .bind(&session_str)
        .execute(&mut *tx)
        .await?;

    if res.rows_affected() == 0 {
        Ok(None)
    } else {
        let snap = snapshot(tx, session_id).await?;
        Ok(Some(snap))
    }
}

/// Pop the first `delivery == Steer` row.
pub async fn consume_steer(
    tx: &mut SqliteConnection,
    session_id: SessionId,
) -> anyhow::Result<Option<(QueueEntry, Vec<PendingPromptView>)>> {
    let session_str = session_id.to_string();
    let row = sqlx::query(
        "SELECT id, position, text, mode, delivery FROM pending_prompts \
         WHERE session_id = ? AND delivery = 'steer' \
         ORDER BY position ASC LIMIT 1",
    )
    .bind(&session_str)
    .fetch_optional(&mut *tx)
    .await?;

    if let Some(r) = row {
        let id_str: String = r.get("id");
        let id = id_str.parse::<PromptId>()?;
        let position: i64 = r.get("position");
        let text: String = r.get("text");
        let mode_str: String = r.get("mode");
        let delivery_str: String = r.get("delivery");

        sqlx::query("DELETE FROM pending_prompts WHERE id = ?")
            .bind(&id_str)
            .execute(&mut *tx)
            .await?;

        let entry = QueueEntry {
            id,
            session_id,
            position,
            text,
            mode: str_to_mode(&mode_str),
            delivery: str_to_delivery(&delivery_str),
        };
        let snap = snapshot(tx, session_id).await?;
        Ok(Some((entry, snap)))
    } else {
        Ok(None)
    }
}

/// Pop the front row regardless of delivery.
pub async fn shift_next(
    tx: &mut SqliteConnection,
    session_id: SessionId,
) -> anyhow::Result<Option<(QueueEntry, Vec<PendingPromptView>)>> {
    let session_str = session_id.to_string();
    let row = sqlx::query(
        "SELECT id, position, text, mode, delivery FROM pending_prompts \
         WHERE session_id = ? \
         ORDER BY position ASC LIMIT 1",
    )
    .bind(&session_str)
    .fetch_optional(&mut *tx)
    .await?;

    if let Some(r) = row {
        let id_str: String = r.get("id");
        let id = id_str.parse::<PromptId>()?;
        let position: i64 = r.get("position");
        let text: String = r.get("text");
        let mode_str: String = r.get("mode");
        let delivery_str: String = r.get("delivery");

        sqlx::query("DELETE FROM pending_prompts WHERE id = ?")
            .bind(&id_str)
            .execute(&mut *tx)
            .await?;

        let entry = QueueEntry {
            id,
            session_id,
            position,
            text,
            mode: str_to_mode(&mode_str),
            delivery: str_to_delivery(&delivery_str),
        };
        let snap = snapshot(tx, session_id).await?;
        Ok(Some((entry, snap)))
    } else {
        Ok(None)
    }
}

/// Put an entry back at the front of the queue after a failed apply.
pub async fn requeue_front(
    tx: &mut SqliteConnection,
    entry: &QueueEntry,
) -> anyhow::Result<Vec<PendingPromptView>> {
    let session_str = entry.session_id.to_string();
    let min_pos: Option<i64> =
        sqlx::query_scalar("SELECT MIN(position) FROM pending_prompts WHERE session_id = ?")
            .bind(&session_str)
            .fetch_one(&mut *tx)
            .await?;
    let new_pos = min_pos.unwrap_or(0) - 1;
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO pending_prompts (id, session_id, position, text, mode, delivery, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET position = excluded.position",
    )
    .bind(entry.id.to_string())
    .bind(&session_str)
    .bind(new_pos)
    .bind(&entry.text)
    .bind(mode_to_str(entry.mode))
    .bind(delivery_to_str(entry.delivery))
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    snapshot(tx, entry.session_id).await
}

/// Discard all queued prompts for a session (e.g. on run cancellation).
pub async fn clear(
    tx: &mut SqliteConnection,
    session_id: SessionId,
) -> anyhow::Result<Vec<PendingPromptView>> {
    let session_str = session_id.to_string();
    sqlx::query("DELETE FROM pending_prompts WHERE session_id = ?")
        .bind(&session_str)
        .execute(&mut *tx)
        .await?;

    Ok(Vec::new())
}

use crate::commands::{ApplyContext, CommandProcessor};
use crate::executor::{RunExecutor, RunLaunch};
use crate::principal::PeerPrincipal;
use crate::projections;
use crate::server::resolve_run_repository;
use crate::subscriptions::SubscriptionHub;
use codypendent_protocol::{
    Actor, ClientId, ClientRole, Command, CommandBody, CommandId, EventBody, RunState, SessionEvent,
};
use std::sync::Arc;

/// Watches sessions with non-empty queues and drains them per the §4 rules.
/// One task per watched session, subscribed to the SubscriptionHub; tasks
/// stop when their queue empties. `notify(session_id)` is called after every
/// queue mutation and at startup for every session with rows.
#[derive(Clone)]
pub struct PromptQueueDrainer {
    notify_tx: tokio::sync::mpsc::UnboundedSender<SessionId>,
}

impl PromptQueueDrainer {
    pub fn new(
        pool: SqlitePool,
        commands: CommandProcessor,
        subscriptions: SubscriptionHub,
        executor: Option<Arc<dyn RunExecutor>>,
        daemon_uid: u32,
    ) -> Self {
        let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel::<SessionId>();
        let drainer_pool = pool.clone();
        let drainer_cmds = commands.clone();
        let drainer_subs = subscriptions.clone();
        let drainer_exec = executor.clone();

        tokio::spawn(async move {
            let active_sessions =
                Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new()));

            while let Some(session_id) = notify_rx.recv().await {
                let mut set = active_sessions.lock().await;
                if set.insert(session_id) {
                    let pool = drainer_pool.clone();
                    let cmds = drainer_cmds.clone();
                    let subs = drainer_subs.clone();
                    let exec = drainer_exec.clone();
                    let set_clone = active_sessions.clone();

                    tokio::spawn(async move {
                        let mut sub_rx = subs.subscribe(session_id);
                        loop {
                            let drain_res = drain_prompt_queue_once(
                                &pool,
                                &cmds,
                                &subs,
                                exec.as_ref(),
                                daemon_uid,
                                session_id,
                            )
                            .await;

                            if let Err(e) = drain_res {
                                tracing::warn!(session_id = %session_id, error = %e, "prompt queue drain error");
                            }

                            let is_empty = match snapshot_pool(&pool, session_id).await {
                                Ok(prompts) => prompts.is_empty(),
                                Err(_) => true,
                            };

                            if is_empty {
                                break;
                            }

                            tokio::select! {
                                ev = sub_rx.recv() => {
                                    match ev {
                                        Ok(event) => {
                                            if let EventBody::RunStateChanged { state, .. } =
                                                &event.body
                                            {
                                                match *state {
                                                    // A run reaching a terminal
                                                    // state frees the session:
                                                    // re-drain so the next queued
                                                    // prompt starts. This MUST
                                                    // include `Failed` — it used to
                                                    // `break` here, so every prompt
                                                    // queued behind a run that
                                                    // happened to fail sat
                                                    // unexecuted until the user
                                                    // touched the queue or the
                                                    // daemon restarted.
                                                    RunState::Completed
                                                    | RunState::Failed => continue,
                                                    // Cancellation clears the queue
                                                    // by intent.
                                                    RunState::Cancelled => {
                                                        let tx = pool.begin().await.ok();
                                                        if let Some(mut tx) = tx {
                                                            let _ =
                                                                clear(&mut tx, session_id).await;
                                                            let _ = tx.commit().await;
                                                        }
                                                        break;
                                                    }
                                                    _ => {}
                                                }
                                            }
                                        }
                                        // We fell behind the broadcast and may have
                                        // missed the run's terminal transition.
                                        // Re-sync by re-draining rather than dying:
                                        // `drain_prompt_queue_once` re-reads the
                                        // projection, so a run that finished while
                                        // we lagged still releases its queued
                                        // prompts.
                                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                            continue;
                                        }
                                        // The session's fan-out is gone; nothing
                                        // more will ever arrive.
                                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                            break;
                                        }
                                    }
                                }
                            }
                        }

                        set_clone.lock().await.remove(&session_id);
                    });
                }
            }
        });

        Self { notify_tx }
    }

    pub fn notify(&self, session_id: SessionId) {
        let _ = self.notify_tx.send(session_id);
    }
}

pub async fn drain_prompt_queue_once(
    pool: &SqlitePool,
    commands: &CommandProcessor,
    subscriptions: &SubscriptionHub,
    executor: Option<&Arc<dyn RunExecutor>>,
    daemon_uid: u32,
    session_id: SessionId,
) -> anyhow::Result<()> {
    let projection = projections::session_projection(pool, session_id).await?;
    let active_run_id = projection.active_runs.first().copied();

    if let Some(active_run) = active_run_id {
        if let Some(exec) = executor {
            loop {
                // 1. Consume the next steer and PERSIST that consumption (queue
                //    deletion + the `PendingPromptsChanged` reflecting it) BEFORE
                //    any external delivery. Delivering first and committing after
                //    is the bug (F3): if the commit failed or the daemon crashed
                //    after `steer_run` but before commit, SQLite would roll the
                //    deletion back while the agent had already acted on the steer,
                //    and the next drain / recovery would consume and deliver the
                //    SAME steer again — repeating its edits/tool calls. Committing
                //    the consumption first makes delivery at-most-once: a failure
                //    after this point can never re-deliver.
                let mut tx = pool.begin().await?;
                let Some((steer_entry, remaining)) = consume_steer(&mut tx, session_id).await?
                else {
                    tx.rollback().await?;
                    break;
                };
                let seq = crate::commands::next_sequence(&mut *tx, session_id).await?;
                let actor = Actor::System;
                let consumed_body = EventBody::PendingPromptsChanged { prompts: remaining };
                crate::commands::append_event(
                    &mut *tx,
                    session_id,
                    seq,
                    &actor,
                    &consumed_body,
                    &Utc::now().to_rfc3339(),
                    None,
                )
                .await?;
                tx.commit().await?;
                subscriptions.publish(
                    session_id,
                    SessionEvent {
                        sequence: u64::try_from(seq)?,
                        occurred_at: Utc::now(),
                        causation_id: None,
                        correlation_id: None,
                        actor: actor.clone(),
                        body: consumed_body,
                    },
                );

                // 2. Now deliver to the live run.
                if exec.steer_run(active_run, steer_entry.text.clone()) {
                    // Delivered: journal `SteeringQueued`. A crash between the
                    // delivery and this append merely loses an informational
                    // event, never re-delivers (the prompt is already consumed).
                    let mut tx = pool.begin().await?;
                    let seq = crate::commands::next_sequence(&mut *tx, session_id).await?;
                    let body = EventBody::SteeringQueued { run_id: active_run };
                    crate::commands::append_event(
                        &mut *tx,
                        session_id,
                        seq,
                        &actor,
                        &body,
                        &Utc::now().to_rfc3339(),
                        None,
                    )
                    .await?;
                    tx.commit().await?;
                    subscriptions.publish(
                        session_id,
                        SessionEvent {
                            sequence: u64::try_from(seq)?,
                            occurred_at: Utc::now(),
                            causation_id: None,
                            correlation_id: None,
                            actor: actor.clone(),
                            body,
                        },
                    );
                } else {
                    // The run is no longer steerable in this process. The steer
                    // was consumed but not applied — durably requeue it to the
                    // front so it is retried, then stop draining.
                    let mut tx = pool.begin().await?;
                    let restored = requeue_front(&mut tx, &steer_entry).await?;
                    let seq = crate::commands::next_sequence(&mut *tx, session_id).await?;
                    let body = EventBody::PendingPromptsChanged { prompts: restored };
                    crate::commands::append_event(
                        &mut *tx,
                        session_id,
                        seq,
                        &actor,
                        &body,
                        &Utc::now().to_rfc3339(),
                        None,
                    )
                    .await?;
                    tx.commit().await?;
                    subscriptions.publish(
                        session_id,
                        SessionEvent {
                            sequence: u64::try_from(seq)?,
                            occurred_at: Utc::now(),
                            causation_id: None,
                            correlation_id: None,
                            actor,
                            body,
                        },
                    );
                    break;
                }
            }
        }
    } else {
        // PEEK the front prompt without removing it. The `SubmitUserInput` must be
        // applied — and thus durably recorded — BEFORE the prompt leaves the queue.
        // Removing first (the F4 bug) and then crashing before `commands.apply`
        // recorded the command loses the prompt entirely: startup would see neither
        // a queued prompt nor a `SubmitUserInput`. Applying first guarantees that
        // after any crash the prompt is either STILL queued (apply not yet
        // committed) or ALREADY a recorded command (apply committed) — never
        // neither. The `prompt-queue:{id}` idempotency key makes a re-apply after
        // such a crash return the SAME run instead of starting a second one, and
        // the removal below is idempotent (it no-ops if a prior drain removed it).
        let front = snapshot_pool(pool, session_id).await?.into_iter().next();
        if let Some(front) = front {
            let prompt_id = front.id;
            let text = front.text.clone();
            let mode = front.mode;

            let cmd_body = CommandBody::SubmitUserInput {
                session_id,
                text: text.clone(),
                mode,
                model: None,
                envelope: None,
            };
            let cmd = Command {
                command_id: CommandId::new(),
                idempotency_key: format!("prompt-queue:{prompt_id}"),
                expected_revision: None,
                body: cmd_body.clone(),
            };
            let ctx = ApplyContext {
                client_id: ClientId::new(),
                role: ClientRole::Contributor,
                principal: PeerPrincipal::from_uid(daemon_uid),
            };

            match commands.apply(pool, ctx, cmd).await {
                Ok(outcome) => {
                    // The command is durably recorded. NOW remove the prompt and
                    // emit `PendingPromptsChanged`. A crash before this leaves the
                    // prompt queued; the next drain re-applies idempotently (same
                    // run) and completes the removal.
                    let mut tx = pool.begin().await?;
                    if let Some(remaining) = delete(&mut tx, session_id, prompt_id).await? {
                        let seq = crate::commands::next_sequence(&mut *tx, session_id).await?;
                        let actor = Actor::System;
                        let body = EventBody::PendingPromptsChanged { prompts: remaining };
                        crate::commands::append_event(
                            &mut *tx,
                            session_id,
                            seq,
                            &actor,
                            &body,
                            &Utc::now().to_rfc3339(),
                            None,
                        )
                        .await?;
                        tx.commit().await?;
                        subscriptions.publish(
                            session_id,
                            SessionEvent {
                                sequence: u64::try_from(seq)?,
                                occurred_at: Utc::now(),
                                causation_id: None,
                                correlation_id: None,
                                actor,
                                body,
                            },
                        );
                    } else {
                        // A concurrent drain already removed it — nothing to emit.
                        tx.rollback().await?;
                    }

                    if let (true, Some(run_id), Some(exec)) =
                        (outcome.newly_applied, outcome.created_run, executor)
                    {
                        let provenance = crate::commands::session_run_provenance(pool, session_id)
                            .await
                            .unwrap_or_default();
                        exec.spawn_run(RunLaunch {
                            session_id,
                            run_id,
                            objective: text,
                            mode,
                            repository: resolve_run_repository(provenance.repository.as_deref()),
                            model: provenance.model,
                            prior: Vec::new(),
                        });
                    }
                }
                Err(err) => {
                    // The prompt was never removed, so it stays queued for the next
                    // drain — no data loss, nothing to requeue.
                    tracing::error!(error = ?err, "failed to apply queued prompt as SubmitUserInput; leaving it queued");
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    async fn test_pool(dir: &Path) -> SqlitePool {
        crate::db::open_database(&dir.join("test.db"))
            .await
            .expect("open database")
    }

    #[tokio::test]
    async fn enqueue_dedupes_by_text_and_steer_goes_front() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = test_pool(tmp.path()).await;
        let session_id = SessionId::new();

        crate::ledger::create_session(&pool, session_id, "Test Session")
            .await
            .unwrap();

        let mut tx = pool.begin().await.unwrap();

        // Queue A
        let s1 = enqueue(
            &mut tx,
            session_id,
            "Prompt A",
            AgentMode::Build,
            PromptDelivery::Queue,
        )
        .await
        .unwrap();
        assert_eq!(s1.len(), 1);
        assert_eq!(s1[0].text, "Prompt A");
        assert_eq!(s1[0].delivery, PromptDelivery::Queue);

        // Queue B
        let s2 = enqueue(
            &mut tx,
            session_id,
            "Prompt B",
            AgentMode::Build,
            PromptDelivery::Queue,
        )
        .await
        .unwrap();
        assert_eq!(s2.len(), 2);
        assert_eq!(s2[0].text, "Prompt A");
        assert_eq!(s2[1].text, "Prompt B");

        // Steer C -> goes to front
        let s3 = enqueue(
            &mut tx,
            session_id,
            "Prompt C",
            AgentMode::Explore,
            PromptDelivery::Steer,
        )
        .await
        .unwrap();
        assert_eq!(s3.len(), 3);
        assert_eq!(s3[0].text, "Prompt C");
        assert_eq!(s3[0].delivery, PromptDelivery::Steer);
        assert_eq!(s3[1].text, "Prompt A");
        assert_eq!(s3[2].text, "Prompt B");

        // Re-queue B with Steer -> dedupes B and moves to front
        let s4 = enqueue(
            &mut tx,
            session_id,
            "Prompt B",
            AgentMode::Plan,
            PromptDelivery::Steer,
        )
        .await
        .unwrap();
        assert_eq!(s4.len(), 3);
        assert_eq!(s4[0].text, "Prompt B");
        assert_eq!(s4[0].delivery, PromptDelivery::Steer);
        assert_eq!(s4[1].text, "Prompt C");
        assert_eq!(s4[2].text, "Prompt A");

        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn update_repositions_on_delivery_change() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = test_pool(tmp.path()).await;
        let session_id = SessionId::new();

        crate::ledger::create_session(&pool, session_id, "Test Session")
            .await
            .unwrap();

        let mut tx = pool.begin().await.unwrap();

        let _s1 = enqueue(
            &mut tx,
            session_id,
            "Prompt 1",
            AgentMode::Build,
            PromptDelivery::Queue,
        )
        .await
        .unwrap();
        let s2 = enqueue(
            &mut tx,
            session_id,
            "Prompt 2",
            AgentMode::Build,
            PromptDelivery::Queue,
        )
        .await
        .unwrap();

        let id2 = s2[1].id;

        // Promote Prompt 2 to Steer -> moves to front
        let u1 = promote(&mut tx, session_id, id2).await.unwrap().unwrap();
        assert_eq!(u1.len(), 2);
        assert_eq!(u1[0].id, id2);
        assert_eq!(u1[0].text, "Prompt 2");
        assert_eq!(u1[0].delivery, PromptDelivery::Steer);
        assert_eq!(u1[1].text, "Prompt 1");

        // Demote Prompt 2 back to Queue -> moves to back
        let u2 = update(&mut tx, session_id, id2, None, Some(PromptDelivery::Queue))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(u2.len(), 2);
        assert_eq!(u2[0].text, "Prompt 1");
        assert_eq!(u2[1].id, id2);
        assert_eq!(u2[1].text, "Prompt 2");
        assert_eq!(u2[1].delivery, PromptDelivery::Queue);

        // Edit text only -> keeps position
        let u3 = update(&mut tx, session_id, id2, Some("Prompt 2 Edited"), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(u3.len(), 2);
        assert_eq!(u3[0].text, "Prompt 1");
        assert_eq!(u3[1].text, "Prompt 2 Edited");

        // Update to blank is refused
        assert!(update(&mut tx, session_id, id2, Some("   "), None)
            .await
            .is_err());

        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn consume_steer_and_shift_next_and_requeue_front() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = test_pool(tmp.path()).await;
        let session_id = SessionId::new();

        crate::ledger::create_session(&pool, session_id, "Test Session")
            .await
            .unwrap();

        let mut tx = pool.begin().await.unwrap();

        enqueue(
            &mut tx,
            session_id,
            "Queue 1",
            AgentMode::Build,
            PromptDelivery::Queue,
        )
        .await
        .unwrap();
        enqueue(
            &mut tx,
            session_id,
            "Steer 2",
            AgentMode::Build,
            PromptDelivery::Steer,
        )
        .await
        .unwrap();
        enqueue(
            &mut tx,
            session_id,
            "Queue 3",
            AgentMode::Build,
            PromptDelivery::Queue,
        )
        .await
        .unwrap();

        // consume_steer pops Steer 2
        let (steer_entry, after_steer) = consume_steer(&mut tx, session_id).await.unwrap().unwrap();
        assert_eq!(steer_entry.text, "Steer 2");
        assert_eq!(after_steer.len(), 2);
        assert_eq!(after_steer[0].text, "Queue 1");
        assert_eq!(after_steer[1].text, "Queue 3");

        // No more steer entries
        assert!(consume_steer(&mut tx, session_id).await.unwrap().is_none());

        // shift_next pops Queue 1
        let (next_entry, after_shift) = shift_next(&mut tx, session_id).await.unwrap().unwrap();
        assert_eq!(next_entry.text, "Queue 1");
        assert_eq!(after_shift.len(), 1);
        assert_eq!(after_shift[0].text, "Queue 3");

        // requeue_front puts Queue 1 back at the front
        let after_requeue = requeue_front(&mut tx, &next_entry).await.unwrap();
        assert_eq!(after_requeue.len(), 2);
        assert_eq!(after_requeue[0].text, "Queue 1");
        assert_eq!(after_requeue[1].text, "Queue 3");

        // clear empties queue
        let cleared = clear(&mut tx, session_id).await.unwrap();
        assert!(cleared.is_empty());
        assert!(shift_next(&mut tx, session_id).await.unwrap().is_none());

        tx.commit().await.unwrap();
    }

    /// A `RunExecutor` that records every `steer_run` call and returns a fixed
    /// result, so a test can assert delivery happened at-most-once.
    struct RecordingExecutor {
        steers: std::sync::Mutex<Vec<(codypendent_protocol::RunId, String)>>,
        steer_returns: bool,
    }

    impl RunExecutor for RecordingExecutor {
        fn spawn_run(&self, _launch: RunLaunch) {}
        fn steer_run(&self, run_id: codypendent_protocol::RunId, text: String) -> bool {
            self.steers.lock().unwrap().push((run_id, text));
            self.steer_returns
        }
    }

    async fn seed_running_run(
        pool: &SqlitePool,
        session_id: SessionId,
    ) -> codypendent_protocol::RunId {
        let run_id = codypendent_protocol::RunId::new();
        sqlx::query(
            "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json) \
             VALUES (?, ?, ?, 'Running', 'Build', 'hosted-default', '{}')",
        )
        .bind(run_id.to_string())
        .bind(session_id.to_string())
        .bind("obj")
        .execute(pool)
        .await
        .unwrap();
        run_id
    }

    /// F3: steering consumption is durable BEFORE external delivery, so a failure
    /// after delivery cannot re-deliver. After one drain the steer is gone from
    /// the queue, and a second drain (modelling recovery) delivers nothing again.
    #[tokio::test]
    async fn steer_is_consumed_before_delivery_and_never_redelivered() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = test_pool(tmp.path()).await;
        let session_id = SessionId::new();
        crate::ledger::create_session(&pool, session_id, "S")
            .await
            .unwrap();
        let run_id = seed_running_run(&pool, session_id).await;

        let mut tx = pool.begin().await.unwrap();
        enqueue(
            &mut tx,
            session_id,
            "steer this",
            AgentMode::Build,
            PromptDelivery::Steer,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let commands = CommandProcessor::default();
        let subs = commands.subscriptions().clone();
        let recorder = Arc::new(RecordingExecutor {
            steers: std::sync::Mutex::new(Vec::new()),
            steer_returns: true,
        });
        let exec: Arc<dyn RunExecutor> = recorder.clone();

        drain_prompt_queue_once(&pool, &commands, &subs, Some(&exec), 1000, session_id)
            .await
            .unwrap();

        // Delivered exactly once, and the queue is now empty (consumption durable).
        {
            let steers = recorder.steers.lock().unwrap();
            assert_eq!(steers.len(), 1);
            assert_eq!(steers[0].0, run_id);
        }
        assert!(snapshot_pool(&pool, session_id).await.unwrap().is_empty());

        // A second drain (recovery re-run) must NOT re-deliver the same steer.
        drain_prompt_queue_once(&pool, &commands, &subs, Some(&exec), 1000, session_id)
            .await
            .unwrap();
        assert_eq!(
            recorder.steers.lock().unwrap().len(),
            1,
            "steer must not be re-delivered on recovery"
        );
    }

    /// F4: the queued prompt is applied as a `SubmitUserInput` (and durably
    /// recorded) BEFORE it leaves the queue, so a crash between apply and removal
    /// never loses it — it is recoverable (still queued AND recorded), and a
    /// recovery drain reconciles it idempotently without a duplicate run.
    #[tokio::test]
    async fn queued_prompt_is_recorded_before_removal_and_reconciles_idempotently() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = test_pool(tmp.path()).await;
        let session_id = SessionId::new();
        crate::ledger::create_session(&pool, session_id, "S")
            .await
            .unwrap();

        let mut tx = pool.begin().await.unwrap();
        let snap = enqueue(
            &mut tx,
            session_id,
            "do the thing",
            AgentMode::Build,
            PromptDelivery::Queue,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let prompt_id = snap[0].id;

        let commands = CommandProcessor::default();
        let subs = commands.subscriptions().clone();

        // Simulate the drain applying the SubmitUserInput and then CRASHING before
        // the prompt is removed: apply the command with the prompt's idempotency
        // key directly, and leave the prompt in the queue.
        let cmd = Command {
            command_id: CommandId::new(),
            idempotency_key: format!("prompt-queue:{prompt_id}"),
            expected_revision: None,
            body: CommandBody::SubmitUserInput {
                session_id,
                text: "do the thing".to_string(),
                mode: AgentMode::Build,
                model: None,
                envelope: None,
            },
        };
        let ctx = ApplyContext {
            client_id: ClientId::new(),
            role: ClientRole::Contributor,
            principal: PeerPrincipal::from_uid(1000),
        };
        let outcome = commands.apply(&pool, ctx, cmd).await.unwrap();
        let run_id = outcome.created_run.expect("run created");

        // Recoverable: the prompt is STILL queued AND the command is recorded —
        // never neither.
        let still = snapshot_pool(&pool, session_id).await.unwrap();
        assert_eq!(still.len(), 1);
        assert_eq!(still[0].id, prompt_id);
        let (status,): (String,) =
            sqlx::query_as("SELECT status FROM commands WHERE idempotency_key = ?")
                .bind(format!("prompt-queue:{prompt_id}"))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "applied");

        // Model the launched run having finished after restart so the drainer's
        // no-active-run branch reconciles the still-queued prompt.
        sqlx::query("UPDATE runs SET state = 'Completed' WHERE id = ?")
            .bind(run_id.to_string())
            .execute(&pool)
            .await
            .unwrap();

        // Recovery drain: idempotent re-apply (SAME run) + completes the removal.
        drain_prompt_queue_once(&pool, &commands, &subs, None, 1000, session_id)
            .await
            .unwrap();

        assert!(
            snapshot_pool(&pool, session_id).await.unwrap().is_empty(),
            "prompt removed after reconciliation"
        );
        let (runs,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM runs WHERE session_id = ?")
            .bind(session_id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(runs, 1, "reconciliation must not create a duplicate run");
    }
}
