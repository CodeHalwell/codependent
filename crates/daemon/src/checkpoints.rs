//! Checkpoint broker & journaling (Adoption 04).
//!
//! Owns per-turn worktree checkpoints, journaling of checkpoint lifecycle
//! events (`CheckpointRecorded`, `CheckpointRestored`), and the approval-gated
//! transactional restore mechanism.

use std::path::{Path, PathBuf};

use chrono::Utc;
use codypendent_protocol::{
    Actor, ApprovalDecision, ApprovalId, CheckpointId, EventBody, ProposedAction, Risk, RiskLevel,
    RunId, RunState, SessionEvent, SessionId,
};
use sqlx::SqlitePool;

use crate::approvals::ApprovalBroker;
use crate::policy::Capability;
use crate::subscriptions::SubscriptionHub;
use crate::worktrees::{RunCheckpoint, WorktreeError};

#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("checkpoint {0} not found")]
    NotFound(CheckpointId),
    #[error("checkpoint belongs to run {expected}, not {actual}")]
    RunMismatch { expected: RunId, actual: RunId },
    #[error("run {0} is currently active; refusing restore")]
    RunActive(RunId),
    #[error("worktree {0} no longer exists")]
    WorktreeMissing(PathBuf),
    #[error("restore rejected or expired")]
    Rejected,
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error(transparent)]
    Approval(#[from] crate::approvals::ApprovalError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Record a checkpoint for a turn, append `EventBody::CheckpointRecorded`,
/// and publish the event.
pub async fn record_checkpoint(
    pool: &SqlitePool,
    subscriptions: &SubscriptionHub,
    session_id: SessionId,
    repository: &Path,
    worktree: &Path,
    run_id: RunId,
    ordinal: u32,
) -> Result<Option<RunCheckpoint>, CheckpointError> {
    let created =
        crate::worktrees::create_run_checkpoint(pool, repository, worktree, run_id, ordinal)
            .await?;
    if let Some(ref cp) = created {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let mut tx = pool.begin().await?;
        let seq = next_sequence(&mut tx, session_id).await?;
        let actor = Actor::System;
        let body = EventBody::CheckpointRecorded {
            run_id: cp.run_id,
            checkpoint_id: cp.id,
            ordinal: cp.ordinal,
            kind: cp.kind,
            commit: cp.commit_sha.clone(),
            base_commit: cp.base_commit.clone(),
        };
        append_event(&mut tx, session_id, seq, &actor, &body, &now_str).await?;
        tx.commit().await?;

        let event = SessionEvent {
            sequence: seq as u64,
            occurred_at: now,
            actor,
            body,
            causation_id: None,
            correlation_id: None,
        };
        subscriptions.publish(session_id, event);
    }
    Ok(created)
}

/// Validate that a restore is permitted and register its approval, WITHOUT
/// awaiting the decision.
///
/// Returns the pending [`ApprovalId`]. The caller must **not** block its
/// connection command loop on the decision: a single-connection client delivers
/// the approving `ResolveApproval` over the same serial connection, so awaiting
/// the decision inline deadlocks it. Spawn [`complete_restore`] off the command
/// loop with the returned id instead.
///
/// The pre-flight guards (run settled, worktree present) run here so the caller
/// can surface `RunActive` / `WorktreeMissing` synchronously before the approval
/// is ever raised.
pub async fn prepare_restore(
    pool: &SqlitePool,
    approvals: &ApprovalBroker,
    session_id: SessionId,
    checkpoint: &RunCheckpoint,
) -> Result<ApprovalId, CheckpointError> {
    // 1. Ensure the run is settled. Restore runs `git reset --hard` + `git
    //    clean -fd` on the worktree, so it must only ever touch a run that has
    //    reached a terminal/settled state. We default-deny: restore is allowed
    //    ONLY for the terminal states (Completed/Failed/Cancelled). Any
    //    non-terminal, unknown, or future state — including a run row that is
    //    missing — is treated as active and refused, so a newly added
    //    non-terminal `RunState` can never silently become restorable.
    let run_row: Option<(String,)> = sqlx::query_as("SELECT state FROM runs WHERE id = ?")
        .bind(checkpoint.run_id.to_string())
        .fetch_optional(pool)
        .await?;
    let state = run_row
        .map(|(s,)| crate::projections::run_state_from_db(&s))
        .unwrap_or(RunState::Unknown);
    let settled = matches!(
        state,
        RunState::Completed | RunState::Failed | RunState::Cancelled
    );
    if !settled {
        return Err(CheckpointError::RunActive(checkpoint.run_id));
    }

    // 2. Ensure worktree exists
    if !checkpoint.worktree_path.exists() {
        return Err(CheckpointError::WorktreeMissing(
            checkpoint.worktree_path.clone(),
        ));
    }

    // 3. Propose action & risk
    let action = ProposedAction::RestoreCheckpoint {
        run_id: checkpoint.run_id.to_string(),
        ordinal: checkpoint.ordinal,
        worktree: checkpoint.worktree_path.to_string_lossy().into_owned(),
        commit: checkpoint.commit_sha.clone(),
    };
    let risk = Risk {
        level: RiskLevel::High,
        reasons: vec![format!(
            "Restore worktree to checkpoint turn {} ({}) — destructive for uncheckpointed changes",
            checkpoint.ordinal, checkpoint.commit_sha
        )],
    };

    let approval_id = approvals
        .request(
            pool,
            session_id,
            checkpoint.run_id,
            None,
            action,
            risk,
            vec![Capability::RestoreCheckpoint],
            None,
        )
        .await?;
    Ok(approval_id)
}

/// Await the decision on a restore approval and, on approve, run the
/// transactional restore, journaling `CheckpointRestored` either way.
///
/// This is the half that blocks on the human decision. It runs OFF the
/// connection command loop (spawned) so the client can still deliver the
/// approval over the same connection.
pub async fn complete_restore(
    pool: &SqlitePool,
    approvals: &ApprovalBroker,
    subscriptions: &SubscriptionHub,
    session_id: SessionId,
    checkpoint: RunCheckpoint,
    approval_id: ApprovalId,
) -> Result<bool, CheckpointError> {
    let decision = approvals.await_decision(approval_id).await?;

    match decision {
        ApprovalDecision::Approve => {
            let restore_res = crate::worktrees::restore_checkpoint_transactional(&checkpoint).await;
            let restored = restore_res.is_ok();

            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let mut tx = pool.begin().await?;
            let seq = next_sequence(&mut tx, session_id).await?;
            let actor = Actor::System;
            let body = EventBody::CheckpointRestored {
                run_id: checkpoint.run_id,
                checkpoint_id: checkpoint.id,
                restored,
            };
            append_event(&mut tx, session_id, seq, &actor, &body, &now_str).await?;
            tx.commit().await?;

            let event = SessionEvent {
                sequence: seq as u64,
                occurred_at: now,
                actor,
                body,
                causation_id: None,
                correlation_id: None,
            };
            subscriptions.publish(session_id, event);

            match restore_res {
                Ok(()) => Ok(true),
                Err(e) => Err(CheckpointError::Worktree(e)),
            }
        }
        ApprovalDecision::Reject | ApprovalDecision::Unknown | _ => {
            let now = Utc::now();
            let now_str = now.to_rfc3339();
            let mut tx = pool.begin().await?;
            let seq = next_sequence(&mut tx, session_id).await?;
            let actor = Actor::System;
            let body = EventBody::CheckpointRestored {
                run_id: checkpoint.run_id,
                checkpoint_id: checkpoint.id,
                restored: false,
            };
            append_event(&mut tx, session_id, seq, &actor, &body, &now_str).await?;
            tx.commit().await?;

            let event = SessionEvent {
                sequence: seq as u64,
                occurred_at: now,
                actor,
                body,
                causation_id: None,
                correlation_id: None,
            };
            subscriptions.publish(session_id, event);
            Ok(false)
        }
    }
}

/// Request an approval-gated transactional restore and await it to completion.
///
/// Convenience over [`prepare_restore`] + [`complete_restore`] for callers that
/// are already off the connection command loop (an out-of-band task, tests). A
/// per-connection command handler must **not** call this — it awaits the human
/// decision inline and would deadlock a single-connection client; use the two
/// halves and spawn the second instead.
pub async fn request_restore(
    pool: &SqlitePool,
    approvals: &ApprovalBroker,
    subscriptions: &SubscriptionHub,
    session_id: SessionId,
    checkpoint: RunCheckpoint,
) -> Result<bool, CheckpointError> {
    let approval_id = prepare_restore(pool, approvals, session_id, &checkpoint).await?;
    complete_restore(
        pool,
        approvals,
        subscriptions,
        session_id,
        checkpoint,
        approval_id,
    )
    .await
}

async fn next_sequence(
    tx: &mut sqlx::SqliteConnection,
    session_id: SessionId,
) -> Result<i64, CheckpointError> {
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
) -> Result<(), CheckpointError> {
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
    use codypendent_protocol::ApprovalScope;
    use tempfile::tempdir;

    async fn test_pool(dir: &Path) -> SqlitePool {
        crate::db::open_database(&dir.join("test.db"))
            .await
            .expect("open database")
    }

    fn init_repo(dir: &Path) -> PathBuf {
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git failed: {:?}", out);
        };
        git(&["init"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        std::fs::write(repo.join("file.txt"), "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "initial"]);
        repo
    }

    #[tokio::test]
    async fn record_and_restore_checkpoint_flow() {
        let root = tempdir().unwrap();
        let pool = test_pool(root.path()).await;
        let repo = init_repo(root.path());
        let session_id = SessionId::new();
        let run_id = RunId::new();
        let now = Utc::now().to_rfc3339();

        sqlx::query("INSERT INTO sessions (id, title, created_at, updated_at) VALUES (?, ?, ?, ?)")
            .bind(session_id.to_string())
            .bind("test")
            .bind(&now)
            .bind(&now)
            .execute(&pool)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(run_id.to_string())
        .bind(session_id.to_string())
        .bind("test")
        .bind("Completed")
        .bind("Build")
        .bind("hosted-default")
        .bind("{}")
        .execute(&pool)
        .await
        .unwrap();

        let hub = SubscriptionHub::new();
        let mut sub = hub.subscribe(session_id);

        let mgr = crate::worktrees::WorktreeManager::new();
        let lease = mgr.allocate(&pool, &repo, run_id).await.unwrap();

        // 1. Record checkpoint
        let cp = record_checkpoint(
            &pool,
            &hub,
            session_id,
            &repo,
            &lease.worktree_path,
            run_id,
            1,
        )
        .await
        .unwrap()
        .expect("checkpoint");

        let event1 = sub.recv().await.unwrap();
        assert!(matches!(event1.body, EventBody::CheckpointRecorded { .. }));

        // 2. Mutate worktree
        std::fs::write(lease.worktree_path.join("file.txt"), "modified\n").unwrap();

        // 3. Request restore with approval broker
        let approvals = ApprovalBroker::new();
        let approvals_clone = approvals.clone();
        let pool_clone = pool.clone();

        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let pending = approvals_clone.reload_pending(&pool_clone).await.unwrap();
            if let Some(p) = pending.first() {
                approvals_clone
                    .resolve(
                        &pool_clone,
                        p.approval_id,
                        ApprovalDecision::Approve,
                        ApprovalScope::Once,
                        "test-user".to_string(),
                    )
                    .await
                    .unwrap();
            }
        });

        let restored = request_restore(&pool, &approvals, &hub, session_id, cp)
            .await
            .unwrap();
        assert!(restored);

        let event2 = sub.recv().await.unwrap();
        assert!(matches!(
            event2.body,
            EventBody::CheckpointRestored { restored: true, .. }
        ));

        let content = std::fs::read_to_string(lease.worktree_path.join("file.txt")).unwrap();
        assert_eq!(content, "base\n");
    }

    /// A restore against a run that is still in a non-terminal state must be
    /// refused with `RunActive` before it can touch the worktree. Regression
    /// for the guard that compared against `"WaitingForInput"` (which the
    /// projection never writes — it writes `"WaitingForUserInput"`) and omitted
    /// `Queued`/`Preparing`/`Recovering`, letting a destructive restore run on a
    /// still-attached run.
    #[tokio::test]
    async fn restore_refused_for_non_terminal_run_states() {
        let root = tempdir().unwrap();
        let pool = test_pool(root.path()).await;
        let repo = init_repo(root.path());
        let session_id = SessionId::new();
        let now = Utc::now().to_rfc3339();

        sqlx::query("INSERT INTO sessions (id, title, created_at, updated_at) VALUES (?, ?, ?, ?)")
            .bind(session_id.to_string())
            .bind("test")
            .bind(&now)
            .bind(&now)
            .execute(&pool)
            .await
            .unwrap();

        let approvals = ApprovalBroker::new();
        let hub = SubscriptionHub::new();

        // Each non-terminal state must be refused. These are exactly the DB
        // strings the projection writes (see `run_state_to_db`).
        for state in [
            "WaitingForUserInput",
            "Queued",
            "Preparing",
            "Recovering",
            "Running",
            "Paused",
            "WaitingForApproval",
        ] {
            let run_id = RunId::new();
            sqlx::query(
                "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(run_id.to_string())
            .bind(session_id.to_string())
            .bind("test")
            .bind(state)
            .bind("Build")
            .bind("hosted-default")
            .bind("{}")
            .execute(&pool)
            .await
            .unwrap();

            let cp = RunCheckpoint {
                id: CheckpointId::new(),
                run_id,
                ordinal: 1,
                kind: codypendent_protocol::CheckpointKind::Commit,
                commit_sha: "deadbeef".to_string(),
                base_commit: "cafebabe".to_string(),
                worktree_path: repo.clone(),
                repository_path: repo.clone(),
                created_at: Utc::now(),
            };

            let result = request_restore(&pool, &approvals, &hub, session_id, cp).await;
            assert!(
                matches!(result, Err(CheckpointError::RunActive(id)) if id == run_id),
                "state {state} should be refused with RunActive, got {result:?}"
            );
        }
    }
}
