//! Session forking (Adoption 05 / Phase 5 STEP 5.6).
//!
//! Clones a session's event ledger up to a checkpoint boundary into a fresh
//! session with remapped run ids, cloned run projection rows, and recorded fork
//! origin so worktrees carve from the checkpointed state.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use codypendent_protocol::{
    Actor, CheckpointKind, CodypendentError, EventBody, RunId, SessionEvent, SessionId,
};
use sqlx::SqlitePool;

use crate::ledger;
use crate::worktrees::RunCheckpoint;

/// Everything ForkSession does after validation, in order.
pub async fn fork_session(
    pool: &SqlitePool,
    source: SessionId,
    checkpoint: RunCheckpoint,
    name: Option<String>,
    owner_uid: Option<u32>,
) -> Result<SessionId, CodypendentError> {
    // 1. Cut point: only ordinal-1 (run-launch) checkpoints are forkable.
    if checkpoint.ordinal != 1 {
        return Err(CodypendentError::new(
            "fork.mid-run-checkpoint",
            "only ordinal-1 (run-launch) checkpoints may be forked",
            false,
        ));
    }

    let events = ledger::load_events(pool, source)
        .await
        .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;

    let cut = events
        .iter()
        .find(|e| {
            matches!(&e.body, EventBody::RunStarted { run_id, .. } if *run_id == checkpoint.run_id)
        })
        .map(|e| e.sequence.saturating_sub(1))
        .ok_or_else(|| {
            CodypendentError::new(
                "checkpoint.not-found",
                format!(
                    "checkpoint run {} not found in session {source}",
                    checkpoint.run_id
                ),
                false,
            )
        })?;

    let head: Vec<SessionEvent> = events.into_iter().filter(|e| e.sequence <= cut).collect();

    // Guard: ensure no unresolved approvals in the fork head
    let mut pending_approvals = HashSet::new();
    for e in &head {
        match &e.body {
            EventBody::ApprovalRequested { approval_id, .. } => {
                pending_approvals.insert(*approval_id);
            }
            EventBody::ApprovalResolved { approval_id, .. } => {
                pending_approvals.remove(approval_id);
            }
            _ => {}
        }
    }
    if !pending_approvals.is_empty() {
        return Err(CodypendentError::new(
            "fork.copy-failed",
            "pending approval in fork head",
            false,
        ));
    }

    // 2. Id map: one fresh RunId per RunStarted in the head.
    let mut id_map = HashMap::new();
    for event in &head {
        if let EventBody::RunStarted { run_id, .. } = &event.body {
            id_map.insert(*run_id, RunId::new());
        }
    }

    // 3. Derive fork title
    let fork = SessionId::new();
    let title = derive_fork_title(pool, source, name).await?;
    let now = Utc::now().to_rfc3339();
    let kind_str = match checkpoint.kind {
        CheckpointKind::Stash => "stash",
        CheckpointKind::Commit => "commit",
        CheckpointKind::Unknown => "unknown",
        _ => "unknown",
    };

    // Carry the source session's workspace into the fork. Omitting it left the
    // fork with a NULL `workspace_id`, so it vanished from every
    // workspace-scoped `ListSessions` (which filters `WHERE workspace_id = ?`) —
    // the fork existed but the operator could never see it.
    let workspace_id: Option<String> =
        sqlx::query_scalar("SELECT workspace_id FROM sessions WHERE id = ?")
            .bind(source.to_string())
            .fetch_optional(pool)
            .await
            .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?
            .flatten();

    sqlx::query(
        "INSERT INTO sessions (id, workspace_id, title, state, created_at, updated_at, revision, \
         owner_uid, forked_from_session_id, forked_at_sequence, \
         fork_base_commit, fork_checkpoint_sha, fork_checkpoint_kind) \
         VALUES (?, ?, ?, 'open', ?, ?, 0, ?, ?, ?, ?, ?, ?)",
    )
    .bind(fork.to_string())
    .bind(&workspace_id)
    .bind(&title)
    .bind(&now)
    .bind(&now)
    .bind(owner_uid.map(|u| u as i64))
    .bind(source.to_string())
    .bind(cut as i64)
    .bind(&checkpoint.base_commit)
    .bind(&checkpoint.commit_sha)
    .bind(kind_str)
    .execute(pool)
    .await
    .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;

    // 4. Copy events remapped
    let copied = ledger::copy_events_remapped(pool, fork, &head, &id_map)
        .await
        .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;

    // 5. Clone run rows
    clone_run_rows(pool, fork, &id_map)
        .await
        .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;

    // 6. Marker event (sequence copied + 1)
    ledger::append_next_event(
        pool,
        fork,
        &Actor::System,
        &EventBody::SessionForked {
            from_session: source,
            checkpoint: checkpoint.id,
        },
        Utc::now(),
    )
    .await
    .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;

    let _ = copied;
    Ok(fork)
}

pub async fn derive_fork_title(
    pool: &SqlitePool,
    source: SessionId,
    name: Option<String>,
) -> Result<String, CodypendentError> {
    if let Some(n) = name {
        return Ok(n);
    }
    let row: Option<(Option<String>,)> = sqlx::query_as("SELECT title FROM sessions WHERE id = ?")
        .bind(source.to_string())
        .fetch_optional(pool)
        .await
        .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;

    let base_title = match row {
        Some((Some(t),)) if !t.trim().is_empty() => t,
        _ => "Session".to_string(),
    };

    let cand = format!("{base_title} (fork)");
    let exists: Option<(i64,)> = sqlx::query_as("SELECT COUNT(*) FROM sessions WHERE title = ?")
        .bind(&cand)
        .fetch_optional(pool)
        .await
        .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;

    if exists.map(|(c,)| c).unwrap_or(0) == 0 {
        return Ok(cand);
    }

    let mut i = 2;
    loop {
        let cand = format!("{base_title} (fork #{i})");
        let exists: Option<(i64,)> =
            sqlx::query_as("SELECT COUNT(*) FROM sessions WHERE title = ?")
                .bind(&cand)
                .fetch_optional(pool)
                .await
                .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;
        if exists.map(|(c,)| c).unwrap_or(0) == 0 {
            return Ok(cand);
        }
        i += 1;
        if i > 10000 {
            return Ok(cand);
        }
    }
}

type RunRowData = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

async fn clone_run_rows(
    pool: &SqlitePool,
    fork: SessionId,
    id_map: &HashMap<RunId, RunId>,
) -> anyhow::Result<()> {
    for (old_id, new_id) in id_map {
        let row: Option<RunRowData> = sqlx::query_as(
            "SELECT objective, state, mode, model_policy, budget_json, started_at, ended_at, prompt_tokens, completion_tokens, cost_micros \
             FROM runs WHERE id = ?",
        )
        .bind(old_id.to_string())
        .fetch_optional(pool)
        .await?;

        if let Some((
            objective,
            state,
            mode,
            model_policy,
            budget_json,
            started_at,
            ended_at,
            prompt_tokens,
            completion_tokens,
            cost_micros,
        )) = row
        {
            sqlx::query(
                "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json, started_at, ended_at, prompt_tokens, completion_tokens, cost_micros) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(new_id.to_string())
            .bind(fork.to_string())
            .bind(objective)
            .bind(state)
            .bind(mode)
            .bind(model_policy)
            .bind(budget_json)
            .bind(started_at)
            .bind(ended_at)
            .bind(prompt_tokens)
            .bind(completion_tokens)
            .bind(cost_micros)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_protocol::{AgentMode, CheckpointId, CheckpointKind, ModelId};

    async fn test_pool(dir: &std::path::Path) -> SqlitePool {
        crate::db::open_database(&dir.join("test.db"))
            .await
            .expect("open db")
    }

    #[tokio::test]
    async fn fork_copies_history_up_to_the_checkpoint_with_remapped_run_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = test_pool(tmp.path()).await;

        let source = SessionId::new();
        ledger::create_session(&pool, source, "My Task")
            .await
            .unwrap();

        let run1 = RunId::new();
        let run2 = RunId::new();

        // Seed run 1
        sqlx::query(
            "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json) \
             VALUES (?, ?, 'run 1 objective', 'Completed', 'Build', 'hosted-default', '{}')",
        )
        .bind(run1.to_string())
        .bind(source.to_string())
        .execute(&pool)
        .await
        .unwrap();

        let client_id = codypendent_protocol::ClientId::new();
        ledger::append_next_event(
            &pool,
            source,
            &Actor::Client { client_id },
            &EventBody::RunStarted {
                run_id: run1,
                objective: "run 1 objective".into(),
                mode: AgentMode::Build,
            },
            Utc::now(),
        )
        .await
        .unwrap();

        ledger::append_next_event(
            &pool,
            source,
            &Actor::Agent {
                agent_id: codypendent_protocol::AgentId::new(),
                run_id: run1,
                model: ModelId("claude-3-5-sonnet".to_string()),
            },
            &EventBody::NoteAppended {
                text: "run 1 note".into(),
                run_id: Some(run1),
            },
            Utc::now(),
        )
        .await
        .unwrap();

        // Seed run 2
        sqlx::query(
            "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json) \
             VALUES (?, ?, 'run 2 objective', 'Running', 'Build', 'hosted-default', '{}')",
        )
        .bind(run2.to_string())
        .bind(source.to_string())
        .execute(&pool)
        .await
        .unwrap();

        ledger::append_next_event(
            &pool,
            source,
            &Actor::Client { client_id },
            &EventBody::RunStarted {
                run_id: run2,
                objective: "run 2 objective".into(),
                mode: AgentMode::Build,
            },
            Utc::now(),
        )
        .await
        .unwrap();

        let cp = RunCheckpoint {
            id: CheckpointId::new(),
            run_id: run2,
            ordinal: 1,
            kind: CheckpointKind::Commit,
            commit_sha: "c2".repeat(20),
            base_commit: "b2".repeat(20),
            worktree_path: tmp.path().join("wt2"),
            repository_path: tmp.path().join("repo"),
            created_at: Utc::now(),
        };

        let source_events_before = ledger::load_events(&pool, source).await.unwrap();
        assert_eq!(source_events_before.len(), 3);

        // Fork before run 2
        let fork_id = fork_session(&pool, source, cp, None, Some(1000))
            .await
            .expect("fork succeeded");

        // Rule 1: Source session untouched
        let source_events_after = ledger::load_events(&pool, source).await.unwrap();
        assert_eq!(source_events_before, source_events_after);

        // Rule 2: Fork ledger length = 2 (copied run 1 events) + 1 (SessionForked marker) = 3
        let fork_events = ledger::load_events(&pool, fork_id).await.unwrap();
        assert_eq!(fork_events.len(), 3);

        // Rule 3: Remapped run ids
        let fork_run1 = match &fork_events[0].body {
            EventBody::RunStarted { run_id, .. } => *run_id,
            other => panic!("expected RunStarted, got {other:?}"),
        };
        assert_ne!(fork_run1, run1);
        assert_ne!(fork_run1, run2);

        // Actor is also remapped
        match &fork_events[1].actor {
            Actor::Agent { run_id, .. } => assert_eq!(*run_id, fork_run1),
            other => panic!("expected Agent actor, got {other:?}"),
        }

        // Marker event at end
        match &fork_events[2].body {
            EventBody::SessionForked { from_session, .. } => assert_eq!(*from_session, source),
            other => panic!("expected SessionForked, got {other:?}"),
        }

        // Check cloned run row in runs table
        let (cloned_obj,): (String,) = sqlx::query_as("SELECT objective FROM runs WHERE id = ?")
            .bind(fork_run1.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(cloned_obj, "run 1 objective");
    }

    #[tokio::test]
    async fn fork_inherits_source_workspace_id() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = test_pool(tmp.path()).await;
        let source = SessionId::new();
        ledger::create_session(&pool, source, "Task").await.unwrap();

        // Stamp a workspace on the source session; the fork must land in it too
        // so a workspace-scoped `ListSessions` (WHERE workspace_id = ?) shows it.
        let workspace = "workspace-abc";
        sqlx::query("UPDATE sessions SET workspace_id = ? WHERE id = ?")
            .bind(workspace)
            .bind(source.to_string())
            .execute(&pool)
            .await
            .unwrap();

        let run1 = RunId::new();
        sqlx::query(
            "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json) \
             VALUES (?, ?, 'o', 'Completed', 'Build', 'hosted-default', '{}')",
        )
        .bind(run1.to_string())
        .bind(source.to_string())
        .execute(&pool)
        .await
        .unwrap();

        let client_id = codypendent_protocol::ClientId::new();
        ledger::append_next_event(
            &pool,
            source,
            &Actor::Client { client_id },
            &EventBody::RunStarted {
                run_id: run1,
                objective: "o".into(),
                mode: AgentMode::Build,
            },
            Utc::now(),
        )
        .await
        .unwrap();

        let cp = RunCheckpoint {
            id: CheckpointId::new(),
            run_id: run1,
            ordinal: 1,
            kind: CheckpointKind::Commit,
            commit_sha: "c".repeat(40),
            base_commit: "b".repeat(40),
            worktree_path: tmp.path().join("wt"),
            repository_path: tmp.path().join("repo"),
            created_at: Utc::now(),
        };

        let fork_id = fork_session(&pool, source, cp, None, Some(1000))
            .await
            .expect("fork succeeded");

        let (ws,): (Option<String>,) =
            sqlx::query_as("SELECT workspace_id FROM sessions WHERE id = ?")
                .bind(fork_id.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(ws.as_deref(), Some(workspace));
    }

    #[tokio::test]
    async fn a_mid_run_checkpoint_is_not_forkable() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = test_pool(tmp.path()).await;
        let source = SessionId::new();
        ledger::create_session(&pool, source, "Task").await.unwrap();

        let cp = RunCheckpoint {
            id: CheckpointId::new(),
            run_id: RunId::new(),
            ordinal: 2,
            kind: CheckpointKind::Commit,
            commit_sha: "c".repeat(40),
            base_commit: "b".repeat(40),
            worktree_path: tmp.path().join("wt"),
            repository_path: tmp.path().join("repo"),
            created_at: Utc::now(),
        };

        let err = fork_session(&pool, source, cp, None, None)
            .await
            .unwrap_err();
        assert_eq!(err.code, "fork.mid-run-checkpoint");
    }

    #[tokio::test]
    async fn a_foreign_checkpoint_answers_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = test_pool(tmp.path()).await;
        let source = SessionId::new();
        ledger::create_session(&pool, source, "Task").await.unwrap();

        let cp = RunCheckpoint {
            id: CheckpointId::new(),
            run_id: RunId::new(), // not in source session
            ordinal: 1,
            kind: CheckpointKind::Commit,
            commit_sha: "c".repeat(40),
            base_commit: "b".repeat(40),
            worktree_path: tmp.path().join("wt"),
            repository_path: tmp.path().join("repo"),
            created_at: Utc::now(),
        };

        let err = fork_session(&pool, source, cp, None, None)
            .await
            .unwrap_err();
        assert_eq!(err.code, "checkpoint.not-found");
    }

    #[tokio::test]
    async fn fork_title_derivation_auto_increments() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = test_pool(tmp.path()).await;
        let source = SessionId::new();
        ledger::create_session(&pool, source, "Build App")
            .await
            .unwrap();

        let title1 = derive_fork_title(&pool, source, None).await.unwrap();
        assert_eq!(title1, "Build App (fork)");

        let fork1 = SessionId::new();
        ledger::create_session(&pool, fork1, &title1).await.unwrap();

        let title2 = derive_fork_title(&pool, source, None).await.unwrap();
        assert_eq!(title2, "Build App (fork #2)");

        let fork2 = SessionId::new();
        ledger::create_session(&pool, fork2, &title2).await.unwrap();

        let title3 = derive_fork_title(&pool, source, None).await.unwrap();
        assert_eq!(title3, "Build App (fork #3)");
    }
}
