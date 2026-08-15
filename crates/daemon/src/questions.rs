//! Question broker (adoption 03): durable parking for `user.ask`.
//!
//! Deliberately a sibling of [`crate::approvals::ApprovalBroker`] — same
//! persist-then-publish, same watch-channel waiters, same restart story.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use codypendent_protocol::{
    Actor, EventBody, QuestionId, QuestionOutcome, QuestionPrompt, RunId, SessionEvent, SessionId,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::watch;

use crate::subscriptions::SubscriptionHub;

/// What the parked run receives when the question resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionReply {
    Answered(Vec<Vec<String>>),
    Rejected { feedback: Option<String> },
}

/// The lifecycle state of a question row (`pending | answered | rejected | expired`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuestionState {
    Pending,
    Answered,
    Rejected,
    Expired,
}

impl QuestionState {
    #[allow(dead_code)]
    fn as_db(self) -> &'static str {
        match self {
            QuestionState::Pending => "pending",
            QuestionState::Answered => "answered",
            QuestionState::Rejected => "rejected",
            QuestionState::Expired => "expired",
        }
    }
}

/// A `pending` question re-surfaced on daemon restart by
/// [`QuestionBroker::reload_pending`].
#[derive(Debug, Clone)]
pub struct PendingQuestion {
    pub question_id: QuestionId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub questions: Vec<QuestionPrompt>,
    pub asked_at: DateTime<Utc>,
}

/// A structured question-broker error.
#[derive(Debug, thiserror::Error)]
pub enum QuestionError {
    #[error("no question with id {question_id}")]
    NotFound { question_id: QuestionId },
    #[error("question {question_id} is already resolved (state {state})")]
    AlreadyResolved {
        question_id: QuestionId,
        state: String,
    },
    #[error("unsupported question outcome")]
    UnsupportedOutcome,
    #[error("question {question_id} waiter dropped before a reply")]
    WaiterGone { question_id: QuestionId },
    #[error("corrupt question row: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
}

/// The registry of live waiters, shared by every clone of a broker.
type Waiters = Arc<Mutex<HashMap<QuestionId, watch::Sender<Option<QuestionReply>>>>>;

#[derive(Debug, Clone, Default)]
pub struct QuestionBroker {
    waiters: Waiters,
    subscriptions: Option<SubscriptionHub>,
}

impl QuestionBroker {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_subscriptions(mut self, subscriptions: SubscriptionHub) -> Self {
        self.subscriptions = Some(subscriptions);
        self
    }

    /// Persist a `pending` question row + append `QuestionAsked` in one BEGIN IMMEDIATE
    /// transaction, register the waiter BEFORE publishing, publish post-commit.
    pub async fn ask(
        &self,
        pool: &SqlitePool,
        session_id: SessionId,
        run_id: RunId,
        questions: Vec<QuestionPrompt>,
    ) -> Result<QuestionId, QuestionError> {
        let question_id = QuestionId::new();
        let questions_json = serde_json::to_string(&questions)?;
        let now = Utc::now();
        let now_str = now.to_rfc3339();

        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;

        sqlx::query(
            "INSERT INTO questions \
             (id, run_id, questions_json, state, asked_at) \
             VALUES (?, ?, ?, 'pending', ?)",
        )
        .bind(question_id.to_string())
        .bind(run_id.to_string())
        .bind(&questions_json)
        .bind(&now_str)
        .execute(&mut *tx)
        .await?;

        let requested_seq = next_sequence(&mut tx, session_id).await?;
        let requested = EventBody::QuestionAsked {
            question_id,
            run_id,
            questions,
        };
        append_event(
            &mut tx,
            session_id,
            requested_seq,
            &Actor::System,
            &requested,
            &now_str,
        )
        .await?;

        tx.commit().await?;

        // Register waiter before publishing (race guard).
        self.register_waiter(question_id, None).await;

        if let Some(hub) = &self.subscriptions {
            if let Ok(sequence) = u64::try_from(requested_seq) {
                hub.publish(
                    session_id,
                    SessionEvent {
                        sequence,
                        occurred_at: now,
                        causation_id: None,
                        correlation_id: None,
                        actor: Actor::System,
                        body: requested,
                    },
                );
            }
        }

        Ok(question_id)
    }

    /// Block until this question is resolved, returning the reply.
    pub async fn await_reply(
        &self,
        question_id: QuestionId,
    ) -> Result<QuestionReply, QuestionError> {
        let mut rx = {
            let guard = self.waiters.lock().expect("waiters mutex poisoned");
            match guard.get(&question_id) {
                Some(sender) => sender.subscribe(),
                None => return Err(QuestionError::NotFound { question_id }),
            }
        };

        loop {
            let current = rx.borrow_and_update().clone();
            if let Some(reply) = current {
                self.waiters
                    .lock()
                    .expect("waiters mutex poisoned")
                    .remove(&question_id);
                return Ok(reply);
            }
            if rx.changed().await.is_err() {
                self.waiters
                    .lock()
                    .expect("waiters mutex poisoned")
                    .remove(&question_id);
                return Err(QuestionError::WaiterGone { question_id });
            }
        }
    }

    /// Flip the pending row and append `QuestionResolved` INSIDE the caller's
    /// transaction (the command write path), returning the exact event to
    /// publish.
    pub(crate) async fn resolve_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        question_id: QuestionId,
        outcome: QuestionOutcome,
        resolved_by: String,
        now: DateTime<Utc>,
    ) -> Result<SessionEvent, QuestionError> {
        let now_str = now.to_rfc3339();

        let existing: Option<(String, String)> = sqlx::query_as(
            "SELECT q.state, r.session_id FROM questions q \
             JOIN runs r ON q.run_id = r.id WHERE q.id = ?",
        )
        .bind(question_id.to_string())
        .fetch_optional(&mut **tx)
        .await?;

        let (current_state, session_id_str) =
            existing.ok_or(QuestionError::NotFound { question_id })?;
        let session_id = SessionId::from_str(&session_id_str).map_err(|e| {
            QuestionError::Corrupt(format!("invalid session_id {session_id_str}: {e}"))
        })?;

        if current_state != "pending" {
            return Err(QuestionError::AlreadyResolved {
                question_id,
                state: current_state,
            });
        }

        let (state_db, answers_json, feedback) = match &outcome {
            QuestionOutcome::Answered { answers } => {
                let json = serde_json::to_string(answers)?;
                ("answered", Some(json), None)
            }
            QuestionOutcome::Rejected { feedback } => ("rejected", None, feedback.clone()),
            _ => return Err(QuestionError::UnsupportedOutcome),
        };

        let result = sqlx::query(
            "UPDATE questions \
             SET state = ?, answers_json = ?, feedback = ?, resolved_by = ?, resolved_at = ? \
             WHERE id = ? AND state = 'pending'",
        )
        .bind(state_db)
        .bind(&answers_json)
        .bind(&feedback)
        .bind(&resolved_by)
        .bind(&now_str)
        .bind(question_id.to_string())
        .execute(&mut **tx)
        .await?;

        if result.rows_affected() != 1 {
            return Err(QuestionError::AlreadyResolved {
                question_id,
                state: current_state,
            });
        }

        let resolved_seq = next_sequence(tx, session_id).await?;
        let resolved = EventBody::QuestionResolved {
            question_id,
            outcome,
        };
        append_event(
            tx,
            session_id,
            resolved_seq,
            &Actor::System,
            &resolved,
            &now_str,
        )
        .await?;

        Ok(SessionEvent {
            sequence: u64::try_from(resolved_seq)
                .map_err(|_| QuestionError::Corrupt("sequence overflow".to_string()))?,
            occurred_at: now,
            causation_id: None,
            correlation_id: None,
            actor: Actor::System,
            body: resolved,
        })
    }

    /// Standalone resolve (tests, recovery): tx + commit + wake.
    pub async fn resolve(
        &self,
        pool: &SqlitePool,
        question_id: QuestionId,
        outcome: QuestionOutcome,
        resolved_by: String,
    ) -> Result<SessionEvent, QuestionError> {
        let reply = match &outcome {
            QuestionOutcome::Answered { answers } => QuestionReply::Answered(answers.clone()),
            QuestionOutcome::Rejected { feedback } => QuestionReply::Rejected {
                feedback: feedback.clone(),
            },
            _ => return Err(QuestionError::UnsupportedOutcome),
        };

        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        let event = self
            .resolve_in_tx(&mut tx, question_id, outcome, resolved_by, Utc::now())
            .await?;
        tx.commit().await?;
        self.wake(question_id, reply).await;
        Ok(event)
    }

    /// Restart resurfacing — mirror of `ApprovalBroker::reload_pending`.
    pub async fn reload_pending(
        &self,
        pool: &SqlitePool,
    ) -> Result<Vec<PendingQuestion>, QuestionError> {
        let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT q.id, q.run_id, r.session_id, q.questions_json, q.asked_at \
             FROM questions q \
             JOIN runs r ON q.run_id = r.id \
             WHERE q.state = 'pending' \
             ORDER BY q.asked_at ASC",
        )
        .fetch_all(pool)
        .await?;

        let mut pending = Vec::with_capacity(rows.len());
        for (id_str, run_id_str, session_id_str, questions_json, asked_at_str) in rows {
            let question_id = QuestionId::from_str(&id_str)
                .map_err(|e| QuestionError::Corrupt(format!("bad question_id {id_str}: {e}")))?;
            let run_id = RunId::from_str(&run_id_str)
                .map_err(|e| QuestionError::Corrupt(format!("bad run_id {run_id_str}: {e}")))?;
            let session_id = SessionId::from_str(&session_id_str).map_err(|e| {
                QuestionError::Corrupt(format!("bad session_id {session_id_str}: {e}"))
            })?;
            let questions: Vec<QuestionPrompt> = serde_json::from_str(&questions_json)?;
            let asked_at = DateTime::parse_from_rfc3339(&asked_at_str)
                .map_err(|e| QuestionError::Corrupt(format!("bad asked_at {asked_at_str}: {e}")))?
                .with_timezone(&Utc);

            self.register_waiter_if_absent(question_id);
            pending.push(PendingQuestion {
                question_id,
                run_id,
                session_id,
                questions,
                asked_at,
            });
        }
        Ok(pending)
    }

    /// Expire pending questions whose run is terminal.
    pub async fn expire_orphaned(
        &self,
        pool: &SqlitePool,
        now: DateTime<Utc>,
    ) -> Result<Vec<QuestionId>, QuestionError> {
        // `runs.state` is written PascalCase by the projection (`run_state_to_db`
        // — `Completed` / `Failed` / `Cancelled`), so the lowercase literals this
        // used before never matched a single row: orphaned questions were never
        // expired and resurfaced as answerable modals on every boot. Match the
        // exact terminal strings the projection writes. (There is no `expired`
        // run state; the terminal run states are exactly these three.)
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT q.id, r.session_id FROM questions q \
             JOIN runs r ON q.run_id = r.id \
             WHERE q.state = 'pending' \
               AND r.state IN ('Completed', 'Failed', 'Cancelled')",
        )
        .fetch_all(pool)
        .await?;

        if rows.is_empty() {
            return Ok(Vec::new());
        }

        let now_str = now.to_rfc3339();
        let mut expired = Vec::with_capacity(rows.len());

        for (id_str, session_id_str) in rows {
            let question_id = QuestionId::from_str(&id_str)
                .map_err(|e| QuestionError::Corrupt(format!("bad question_id {id_str}: {e}")))?;
            let session_id = SessionId::from_str(&session_id_str).map_err(|e| {
                QuestionError::Corrupt(format!("bad session_id {session_id_str}: {e}"))
            })?;

            let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
            let res = sqlx::query(
                "UPDATE questions \
                 SET state = 'expired', resolved_by = 'daemon:expired-orphaned', resolved_at = ? \
                 WHERE id = ? AND state = 'pending'",
            )
            .bind(&now_str)
            .bind(question_id.to_string())
            .execute(&mut *tx)
            .await?;

            if res.rows_affected() == 1 {
                let resolved_seq = next_sequence(&mut tx, session_id).await?;
                let resolved = EventBody::QuestionResolved {
                    question_id,
                    outcome: QuestionOutcome::Rejected { feedback: None },
                };
                append_event(
                    &mut tx,
                    session_id,
                    resolved_seq,
                    &Actor::System,
                    &resolved,
                    &now_str,
                )
                .await?;

                tx.commit().await?;

                self.wake(question_id, QuestionReply::Rejected { feedback: None })
                    .await;

                if let Some(hub) = &self.subscriptions {
                    if let Ok(sequence) = u64::try_from(resolved_seq) {
                        hub.publish(
                            session_id,
                            SessionEvent {
                                sequence,
                                occurred_at: now,
                                causation_id: None,
                                correlation_id: None,
                                actor: Actor::System,
                                body: resolved,
                            },
                        );
                    }
                }

                expired.push(question_id);
            }
        }

        Ok(expired)
    }

    pub fn forget_waiter(&self, question_id: QuestionId) {
        self.waiters
            .lock()
            .expect("waiters mutex poisoned")
            .remove(&question_id);
    }

    pub(crate) async fn wake(&self, question_id: QuestionId, reply: QuestionReply) {
        let mut guard = self.waiters.lock().expect("waiters mutex poisoned");
        guard
            .entry(question_id)
            .or_insert_with(|| watch::channel(None).0)
            .send_replace(Some(reply));
    }

    async fn register_waiter(&self, question_id: QuestionId, initial: Option<QuestionReply>) {
        let mut guard = self.waiters.lock().expect("waiters mutex poisoned");
        guard
            .entry(question_id)
            .or_insert_with(|| watch::channel(initial.clone()).0)
            .send_replace(initial);
    }

    fn register_waiter_if_absent(&self, question_id: QuestionId) {
        let mut guard = self.waiters.lock().expect("waiters mutex poisoned");
        guard
            .entry(question_id)
            .or_insert_with(|| watch::channel(None).0);
    }
}

async fn next_sequence(
    tx: &mut sqlx::SqliteConnection,
    session_id: SessionId,
) -> Result<i64, QuestionError> {
    let row: (i64,) =
        sqlx::query_as("SELECT COALESCE(MAX(sequence), 0) + 1 FROM events WHERE session_id = ?")
            .bind(session_id.to_string())
            .fetch_one(tx)
            .await?;
    Ok(row.0)
}

async fn append_event(
    tx: &mut sqlx::SqliteConnection,
    session_id: SessionId,
    sequence: i64,
    actor: &Actor,
    body: &EventBody,
    occurred_at: &str,
) -> Result<(), QuestionError> {
    sqlx::query(
        "INSERT INTO events \
         (session_id, sequence, occurred_at, actor, body, causation_id, correlation_id, schema_version) \
         VALUES (?, ?, ?, ?, ?, NULL, NULL, 1)",
    )
    .bind(session_id.to_string())
    .bind(sequence)
    .bind(occurred_at)
    .bind(serde_json::to_string(actor)?)
    .bind(serde_json::to_string(body)?)
    .execute(tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_protocol::QuestionOption;
    use std::path::Path;
    use tempfile::tempdir;

    async fn test_pool(dir: &Path) -> SqlitePool {
        crate::db::open_database(&dir.join("test.db"))
            .await
            .expect("open database")
    }

    async fn seed_session_and_run(pool: &SqlitePool) -> (SessionId, RunId) {
        let session_id = SessionId::new();
        crate::ledger::create_session(pool, session_id, "question-test")
            .await
            .expect("create session");

        let run_id = RunId::new();
        sqlx::query(
            "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(run_id.to_string())
        .bind(session_id.to_string())
        .bind("diagnose")
        .bind("Running")
        .bind("Build")
        .bind("hosted-default")
        .bind("{}")
        .execute(pool)
        .await
        .expect("insert run");

        (session_id, run_id)
    }

    fn sample_questions() -> Vec<QuestionPrompt> {
        vec![QuestionPrompt {
            question: "Choose format:".to_string(),
            header: "Format".to_string(),
            options: vec![
                QuestionOption {
                    label: "JSON".to_string(),
                    description: "Machine readable".to_string(),
                },
                QuestionOption {
                    label: "YAML".to_string(),
                    description: "Human readable".to_string(),
                },
            ],
            multiple: false,
            custom: true,
        }]
    }

    #[tokio::test]
    async fn answer_round_trip_wakes_the_waiter() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let (session_id, run_id) = seed_session_and_run(&pool).await;
        let broker = QuestionBroker::new();

        let q_id = broker
            .ask(&pool, session_id, run_id, sample_questions())
            .await
            .unwrap();

        let broker_clone = broker.clone();
        let join = tokio::spawn(async move { broker_clone.await_reply(q_id).await });

        let event = broker
            .resolve(
                &pool,
                q_id,
                QuestionOutcome::Answered {
                    answers: vec![vec!["JSON".to_string()]],
                },
                "user:test".to_string(),
            )
            .await
            .unwrap();

        assert!(matches!(event.body, EventBody::QuestionResolved { .. }));
        let reply = join.await.unwrap().unwrap();
        assert_eq!(
            reply,
            QuestionReply::Answered(vec![vec!["JSON".to_string()]])
        );
    }

    #[tokio::test]
    async fn reject_with_feedback_carries_the_feedback() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let (session_id, run_id) = seed_session_and_run(&pool).await;
        let broker = QuestionBroker::new();

        let q_id = broker
            .ask(&pool, session_id, run_id, sample_questions())
            .await
            .unwrap();

        broker
            .resolve(
                &pool,
                q_id,
                QuestionOutcome::Rejected {
                    feedback: Some("not now".to_string()),
                },
                "user:test".to_string(),
            )
            .await
            .unwrap();

        let reply = broker.await_reply(q_id).await.unwrap();
        assert_eq!(
            reply,
            QuestionReply::Rejected {
                feedback: Some("not now".to_string())
            }
        );
    }

    #[tokio::test]
    async fn restart_re_surfaces_pending_and_still_resolves() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let (session_id, run_id) = seed_session_and_run(&pool).await;
        let broker1 = QuestionBroker::new();

        let q_id = broker1
            .ask(&pool, session_id, run_id, sample_questions())
            .await
            .unwrap();

        // Simulate new process instance
        let broker2 = QuestionBroker::new();
        let pending = broker2.reload_pending(&pool).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].question_id, q_id);

        broker2
            .resolve(
                &pool,
                q_id,
                QuestionOutcome::Answered {
                    answers: vec![vec!["YAML".to_string()]],
                },
                "user:test".to_string(),
            )
            .await
            .unwrap();

        let reply = broker2.await_reply(q_id).await.unwrap();
        assert_eq!(
            reply,
            QuestionReply::Answered(vec![vec!["YAML".to_string()]])
        );
    }

    #[tokio::test]
    async fn orphaned_question_is_expired_on_boot() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let (session_id, run_id) = seed_session_and_run(&pool).await;
        let broker = QuestionBroker::new();

        let q_id = broker
            .ask(&pool, session_id, run_id, sample_questions())
            .await
            .unwrap();

        // Mark run completed — using the SAME PascalCase string the projection
        // (`run_state_to_db`) actually writes. The lowercase `'completed'` this
        // test used before masked the bug: it matched the (equally lowercase,
        // and wrong) query, so both agreed on a value the real projection never
        // produces.
        sqlx::query("UPDATE runs SET state = 'Completed' WHERE id = ?")
            .bind(run_id.to_string())
            .execute(&pool)
            .await
            .unwrap();

        let expired = broker.expire_orphaned(&pool, Utc::now()).await.unwrap();
        assert_eq!(expired, vec![q_id]);

        let state: (String,) = sqlx::query_as("SELECT state FROM questions WHERE id = ?")
            .bind(q_id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state.0, "expired");
    }

    /// The complement of the fix: a question whose run is still `Running` (the
    /// PascalCase value the projection writes) must NOT be expired.
    #[tokio::test]
    async fn question_for_a_running_run_is_not_expired() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        // `seed_session_and_run` inserts the run in state "Running".
        let (session_id, run_id) = seed_session_and_run(&pool).await;
        let broker = QuestionBroker::new();

        let q_id = broker
            .ask(&pool, session_id, run_id, sample_questions())
            .await
            .unwrap();

        let expired = broker.expire_orphaned(&pool, Utc::now()).await.unwrap();
        assert!(expired.is_empty());

        let state: (String,) = sqlx::query_as("SELECT state FROM questions WHERE id = ?")
            .bind(q_id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state.0, "pending");
    }

    #[tokio::test]
    async fn double_resolve_reports_already_resolved() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let (session_id, run_id) = seed_session_and_run(&pool).await;
        let broker = QuestionBroker::new();

        let q_id = broker
            .ask(&pool, session_id, run_id, sample_questions())
            .await
            .unwrap();

        broker
            .resolve(
                &pool,
                q_id,
                QuestionOutcome::Answered {
                    answers: vec![vec!["JSON".to_string()]],
                },
                "user:test".to_string(),
            )
            .await
            .unwrap();

        let second = broker
            .resolve(
                &pool,
                q_id,
                QuestionOutcome::Answered {
                    answers: vec![vec!["JSON".to_string()]],
                },
                "user:test".to_string(),
            )
            .await;

        assert!(matches!(second, Err(QuestionError::AlreadyResolved { .. })));
    }

    #[tokio::test]
    async fn resolving_a_missing_question_is_not_found() {
        let dir = tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let broker = QuestionBroker::new();
        let missing = QuestionId::new();

        let res = broker
            .resolve(
                &pool,
                missing,
                QuestionOutcome::Answered { answers: vec![] },
                "user".to_string(),
            )
            .await;

        assert!(matches!(res, Err(QuestionError::NotFound { .. })));
    }
}
