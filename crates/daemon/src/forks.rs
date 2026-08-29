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
use sqlx::{SqliteConnection, SqlitePool};

use crate::ledger;
use crate::worktrees::RunCheckpoint;

/// Everything ForkSession does after validation, in order.
///
/// `fork` is the id the new session will be created under — supplied by the
/// caller (the server records it durably on the command's reservation BEFORE
/// forking), so recovery can complete a fork whose `applied` finalize was
/// skipped by re-driving with the SAME id and getting back the SAME session.
///
/// **Idempotent + atomic.** Every write happens in one transaction, so a crash
/// leaves either no fork or a complete one — never a partial orphan. And if the
/// `fork` session already exists (a prior attempt committed before crashing), it
/// returns that id immediately instead of forking a second time.
pub async fn fork_session(
    pool: &SqlitePool,
    source: SessionId,
    checkpoint: RunCheckpoint,
    name: Option<String>,
    owner_uid: Option<u32>,
    fork: SessionId,
) -> Result<SessionId, CodypendentError> {
    // Idempotency guard: an existing fork row means the whole (atomic) fork
    // already committed. This is what lets `resume_received` complete a fork
    // whose command was reserved but never finalized `applied`.
    if ledger::session_exists(pool, fork)
        .await
        .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?
    {
        reconcile_fork_outbox(pool, fork).await?;
        return Ok(fork);
    }

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

    // 3. Derive fork title (reads only; the writes are all deferred to the tx).
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
    let (workspace_id, repository_id, repository): (
        Option<String>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as("SELECT workspace_id, repository_id, repository FROM sessions WHERE id = ?")
        .bind(source.to_string())
        .fetch_one(pool)
        .await
        .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;

    // 4. Every write in ONE transaction so the fork is all-or-nothing: the
    //    session row, the copied events, the cloned run rows, and the marker
    //    event commit together. A crash mid-fork rolls back cleanly (leaving no
    //    orphan), and a committed fork is complete — which makes the existence
    //    check at the top a reliable idempotency signal.
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;

    if let Err(e) = sqlx::query(
        "INSERT INTO sessions (id, workspace_id, title, state, created_at, updated_at, revision, \
         repository_id, repository, \
         owner_uid, forked_from_session_id, forked_at_sequence, \
         fork_base_commit, fork_checkpoint_sha, fork_checkpoint_kind) \
         VALUES (?, ?, ?, 'open', ?, ?, 0, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(fork.to_string())
    .bind(&workspace_id)
    .bind(&title)
    .bind(&now)
    .bind(&now)
    .bind(&repository_id)
    .bind(&repository)
    .bind(owner_uid.map(|u| u as i64))
    .bind(source.to_string())
    .bind(cut as i64)
    .bind(&checkpoint.base_commit)
    .bind(&checkpoint.commit_sha)
    .bind(kind_str)
    .execute(&mut *tx)
    .await
    {
        // A concurrent delivery (or recovery re-drive) racing on this same fork id
        // may have committed the fork between our existence check and this insert.
        // If the fork now exists, this is the idempotent no-op it is meant to be.
        let _ = tx.rollback().await;
        if ledger::session_exists(pool, fork)
            .await
            .map_err(|err| CodypendentError::new("fork.copy-failed", err.to_string(), false))?
        {
            return Ok(fork);
        }
        return Err(CodypendentError::new(
            "fork.copy-failed",
            e.to_string(),
            false,
        ));
    }

    // 5. Copy events remapped (inside the tx).
    let copied = ledger::copy_events_remapped(&mut tx, fork, &head, &id_map)
        .await
        .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;

    // 6. Clone run rows (inside the tx).
    clone_run_rows(&mut tx, fork, &id_map)
        .await
        .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;

    // 7. Marker event (sequence copied + 1), inside the tx.
    let marker = SessionEvent {
        sequence: copied + 1,
        occurred_at: Utc::now(),
        causation_id: None,
        correlation_id: None,
        actor: Actor::System,
        body: EventBody::SessionForked {
            from_session: source,
            checkpoint: checkpoint.id,
        },
    };
    ledger::append_event_conn(&mut tx, fork, &marker)
        .await
        .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;

    crate::control_plane_sync::outbox::enqueue_session_snapshot(&mut tx, &fork.to_string())
        .await
        .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;
    for run_id in id_map.values() {
        crate::control_plane_sync::outbox::enqueue_run_snapshot(&mut tx, &run_id.to_string())
            .await
            .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;
    }

    tx.commit()
        .await
        .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;

    Ok(fork)
}

async fn reconcile_fork_outbox(pool: &SqlitePool, fork: SessionId) -> Result<(), CodypendentError> {
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;
    crate::control_plane_sync::outbox::enqueue_session_snapshot(&mut tx, &fork.to_string())
        .await
        .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;
    let run_ids: Vec<String> = sqlx::query_scalar("SELECT id FROM runs WHERE session_id = ?")
        .bind(fork.to_string())
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;
    for run_id in run_ids {
        crate::control_plane_sync::outbox::enqueue_run_snapshot(&mut tx, &run_id)
            .await
            .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;
    }
    tx.commit()
        .await
        .map_err(|e| CodypendentError::new("fork.copy-failed", e.to_string(), false))?;
    Ok(())
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
    conn: &mut SqliteConnection,
    fork: SessionId,
    id_map: &HashMap<RunId, RunId>,
) -> anyhow::Result<()> {
    for (old_id, new_id) in id_map {
        let row: Option<RunRowData> = sqlx::query_as(
            "SELECT objective, state, mode, model_policy, budget_json, started_at, ended_at, prompt_tokens, completion_tokens, cost_micros \
             FROM runs WHERE id = ?",
        )
        .bind(old_id.to_string())
        .fetch_optional(&mut *conn)
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
            .execute(&mut *conn)
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane_sync::{
        fetch_pending_deltas, record_pairing, ControlPlaneCredential, ControlPlanePairing,
        LocalConsentManifest, PairingState,
    };
    use codypendent_control_plane_protocol::PublicationClass;
    use codypendent_protocol::{AgentMode, CheckpointId, CheckpointKind, ModelId};
    use uuid::Uuid;

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
        let fork_id = fork_session(&pool, source, cp, None, Some(1000), SessionId::new())
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

        let fork_id = fork_session(&pool, source, cp, None, Some(1000), SessionId::new())
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

        let err = fork_session(&pool, source, cp, None, None, SessionId::new())
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

        let err = fork_session(&pool, source, cp, None, None, SessionId::new())
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

    /// A re-driven fork with a supplied id that already exists is an idempotent
    /// no-op returning that same id (no duplicate session, no partial rewrite).
    #[tokio::test]
    async fn re_forking_an_existing_id_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = test_pool(tmp.path()).await;
        let (source, _run, cp) = seed_forkable_source(&pool, &tmp).await;
        let repository_id = "fork-local-repository";
        sqlx::query(
            "UPDATE sessions SET owner_uid = 7, repository_id = ?, repository = ? WHERE id = ?",
        )
        .bind(repository_id)
        .bind(tmp.path().to_string_lossy().as_ref())
        .bind(source.to_string())
        .execute(&pool)
        .await
        .unwrap();
        let manifest = LocalConsentManifest {
            organization_id: "fork-org".to_string(),
            organization_display_name: "Fork Org".to_string(),
            endpoint: "https://control-plane.test".to_string(),
            max_publication_class: PublicationClass::MetadataShared,
            accepts_remote_approvals: false,
            accepts_runner_dispatch: false,
            allowed_repositories: vec![repository_id.to_string()],
            created_at: Utc::now(),
        };
        let pairing_id = Uuid::now_v7().to_string();
        record_pairing(
            &pool,
            &ControlPlanePairing {
                id: pairing_id.clone(),
                owner_uid: 7,
                endpoint: manifest.endpoint.clone(),
                organization_id: manifest.organization_id.clone(),
                organization_display_name: manifest.organization_display_name.clone(),
                consent_manifest: serde_json::to_string(&manifest).unwrap(),
                consent_manifest_hash: manifest.compute_hash(),
                max_publication_class: PublicationClass::MetadataShared,
                accepts_remote_approvals: false,
                accepts_runner_dispatch: false,
                state: PairingState::Active,
                paired_at: Some(Utc::now()),
                expires_at: None,
                revoked_at: None,
                revoked_reason: None,
                created_at: Utc::now(),
            },
            &ControlPlaneCredential {
                pairing_id: pairing_id.clone(),
                credential_ref: "keychain:fork-test".to_string(),
                credential_hash: "abababababababababababababababababababababababababababababababab"
                    .to_string(),
                audience: "control-plane".to_string(),
                purpose: "sync".to_string(),
                issued_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::days(1),
                rotated_at: None,
            },
        )
        .await
        .unwrap();

        let fork_id = SessionId::new();
        let first = fork_session(&pool, source, cp.clone(), None, Some(7), fork_id)
            .await
            .unwrap();
        assert_eq!(first, fork_id);
        let fork_scope: (Option<String>, Option<String>) =
            sqlx::query_as("SELECT repository_id, repository FROM sessions WHERE id = ?")
                .bind(fork_id.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(fork_scope.0.as_deref(), Some(repository_id));
        assert_eq!(
            fork_scope.1.as_deref(),
            Some(tmp.path().to_string_lossy().as_ref())
        );
        assert!(fetch_pending_deltas(&pool, &pairing_id, 20)
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.delta_kind == "session-summary"
                && entry.subject_id == fork_id.to_string()));

        let events_after_first = ledger::load_events(&pool, fork_id).await.unwrap().len();
        sqlx::query("DELETE FROM control_plane_outbox WHERE pairing_id = ?")
            .bind(&pairing_id)
            .execute(&pool)
            .await
            .unwrap();

        // Re-drive with the same id (as recovery does) — must not fork again.
        let second = fork_session(&pool, source, cp, None, Some(7), fork_id)
            .await
            .unwrap();
        assert_eq!(second, fork_id);
        assert!(fetch_pending_deltas(&pool, &pairing_id, 20)
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.delta_kind == "session-summary"
                && entry.subject_id == fork_id.to_string()));
        assert_eq!(
            ledger::load_events(&pool, fork_id).await.unwrap().len(),
            events_after_first,
            "a re-drive must not duplicate the fork's events"
        );

        let (forks,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM sessions WHERE forked_from_session_id = ?")
                .bind(source.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(forks, 1, "exactly one fork session exists");
    }

    /// F5: a fork whose `applied` finalize was skipped (crash simulated by a
    /// lingering `received` command row carrying the pre-recorded fork id) is
    /// completed on recovery/replay — returning the SAME forked session id, with
    /// no orphan and no permanent `fork.in-progress`.
    #[tokio::test]
    async fn recovery_completes_a_fork_whose_finalize_was_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = test_pool(tmp.path()).await;
        let (source, _run, cp) = seed_forkable_source(&pool, &tmp).await;

        // The server pre-generates the fork id and records it on the reservation
        // BEFORE forking. Simulate a crash AFTER that reservation but BEFORE the
        // `applied` finalize (and before the fork itself ran): a `received`
        // command row whose `result_json` carries the intended fork id.
        let fork_id = SessionId::new();
        let command_id = codypendent_protocol::CommandId::new();
        let body = codypendent_protocol::CommandBody::ForkSession {
            session_id: source,
            checkpoint: cp.id,
            name: None,
        };
        let reserved_outcome = crate::commands::CommandOutcome {
            command_id,
            created_session: Some(fork_id),
            created_run: None,
            last_sequence: None,
            newly_applied: true,
        };
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO commands \
             (id, idempotency_key, session_id, client_id, body, status, result_json, received_at) \
             VALUES (?, ?, ?, ?, ?, 'received', ?, ?)",
        )
        .bind(command_id.to_string())
        .bind("fork:crash-test")
        .bind(source.to_string())
        .bind(codypendent_protocol::ClientId::new().to_string())
        .bind(serde_json::to_string(&body).unwrap())
        .bind(serde_json::to_string(&reserved_outcome).unwrap())
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();

        let commands = crate::commands::CommandProcessor::default();

        // Recovery / retry path: replay the reserved command.
        let outcome = commands
            .replay_existing(&pool, "fork:crash-test")
            .await
            .unwrap()
            .expect("a reserved fork is replayable");
        assert_eq!(
            outcome.created_session,
            Some(fork_id),
            "recovery returns the SAME forked session id"
        );

        // The command row is now `applied` (no permanent `fork.in-progress`).
        let (status,): (String,) =
            sqlx::query_as("SELECT status FROM commands WHERE idempotency_key = ?")
                .bind("fork:crash-test")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "applied");

        // The fork exists and points at its source; exactly one, no orphan.
        assert!(ledger::session_exists(&pool, fork_id).await.unwrap());
        let (forks,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM sessions WHERE forked_from_session_id = ?")
                .bind(source.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(forks, 1, "no orphan or duplicate fork");

        // A second replay is idempotent: same id, still one fork.
        let again = commands
            .replay_existing(&pool, "fork:crash-test")
            .await
            .unwrap()
            .expect("still replayable");
        assert_eq!(again.created_session, Some(fork_id));
        let (forks,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM sessions WHERE forked_from_session_id = ?")
                .bind(source.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(forks, 1, "replay must not create a second fork");
    }

    /// Seed a source session with one run + `RunStarted` at the head and an
    /// ordinal-1 checkpoint persisted in `run_checkpoints`, returning
    /// (source, run_id, checkpoint).
    async fn seed_forkable_source(
        pool: &SqlitePool,
        tmp: &tempfile::TempDir,
    ) -> (SessionId, RunId, RunCheckpoint) {
        let source = SessionId::new();
        ledger::create_session(pool, source, "Task").await.unwrap();

        let run_id = RunId::new();
        sqlx::query(
            "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json) \
             VALUES (?, ?, 'o', 'Completed', 'Build', 'hosted-default', '{}')",
        )
        .bind(run_id.to_string())
        .bind(source.to_string())
        .execute(pool)
        .await
        .unwrap();

        ledger::append_next_event(
            pool,
            source,
            &Actor::Client {
                client_id: codypendent_protocol::ClientId::new(),
            },
            &EventBody::RunStarted {
                run_id,
                objective: "o".into(),
                mode: AgentMode::Build,
            },
            Utc::now(),
        )
        .await
        .unwrap();

        let cp = RunCheckpoint {
            id: CheckpointId::new(),
            run_id,
            ordinal: 1,
            kind: CheckpointKind::Commit,
            commit_sha: "c".repeat(40),
            base_commit: "b".repeat(40),
            worktree_path: tmp.path().join("wt"),
            repository_path: tmp.path().join("repo"),
            created_at: Utc::now(),
        };
        crate::worktrees::insert_checkpoint(pool, &cp)
            .await
            .unwrap();
        (source, run_id, cp)
    }
}
