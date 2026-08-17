//! Inbox Integration Tests (Milestone 3, Tasks 3.1 & 3.2).
//!
//! Covers:
//! - Deduplication on replayed source events
//! - State transitions (Unread -> Acknowledged -> Dismissed) preserving resolved_at invariance
//! - Resolution sweep as the exclusive writer of `resolved_at`
//! - Idempotent mutation replay
//! - Non-overlapping keyset cursor pagination
//! - Strict owner isolation across queries, counts, and cursors
//! - Safe repository filtering
//! - Deep link generation across all entry kinds
//! - Transactional production with underlying source records
//! - At-most-once delivery attempts and policy suppression

use chrono::Utc;
use codypendent_daemon::{
    approvals::ApprovalBroker,
    commands::CommandProcessor,
    db,
    inbox::{self, DeliveryState, InboxStore},
    principal::PeerPrincipal,
    questions::QuestionBroker,
};
use codypendent_protocol::{
    Actor, ApprovalId, ArtifactId, ArtifactRef, ClientId, ClientRole, Command, CommandBody,
    CommandId, DataClassification, EventBody, InboxDeepLink, InboxEntryId, InboxEntryKind,
    InboxEntryState, InboxListFilters, InboxListQuery, InboxMutation, InboxSourceIdentity,
    PluginId, ProposedAction, QuestionId, QuestionOption, QuestionPrompt, RepositoryId, Risk,
    RiskLevel, RunDisposition, RunId, RunState, SessionId, WorkflowId,
};
use sqlx::SqlitePool;
use tempfile::TempDir;

async fn setup_test_db(temp: &TempDir) -> SqlitePool {
    db::open_database(&temp.path().join("test.db"))
        .await
        .expect("open test database")
}

async fn insert_session(
    pool: &SqlitePool,
    session_id: SessionId,
    owner_uid: u32,
    repo_id: RepositoryId,
) {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO sessions (id, title, state, created_at, updated_at, revision, owner_uid, repository_id) \
         VALUES (?, 'test session', 'open', ?, ?, 0, ?, ?)",
    )
    .bind(session_id.to_string())
    .bind(&now)
    .bind(&now)
    .bind(i64::from(owner_uid))
    .bind(repo_id.to_string())
    .execute(pool)
    .await
    .expect("insert session");
}

async fn insert_run(pool: &SqlitePool, session_id: SessionId, run_id: RunId) {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        // `runs` has no `created_at` column, and `model_policy`/`budget_json`
        // are NOT NULL without defaults, so both must be supplied. `state` and
        // `mode` are the exact strings `projections::run_state_from_db` /
        // `agent_mode_to_db` round-trip — lowercase parses back as `Unknown`.
        "INSERT INTO runs (id, session_id, objective, mode, state, model_policy, budget_json, started_at) \
         VALUES (?, ?, 'test objective', 'Build', 'Running', '{}', '{}', ?)",
    )
    .bind(run_id.to_string())
    .bind(session_id.to_string())
    .bind(&now)
    .execute(pool)
    .await
    .expect("insert run");
}

#[tokio::test]
async fn replayed_source_upserts_one_entry_on_the_dedup_key() {
    let temp = TempDir::new().unwrap();
    let pool = setup_test_db(&temp).await;
    let store = InboxStore::new();

    let owner_uid = 1000;
    let principal = PeerPrincipal::from_uid(owner_uid);
    let repo_id = RepositoryId::new();
    let session_id = SessionId::new();
    let run_id = RunId::new();
    let approval_id = ApprovalId::new();

    // `inbox_entries.session_id` and `.run_id` are real foreign keys, so the
    // source rows must exist before an entry can be produced.
    insert_session(&pool, session_id, owner_uid, repo_id).await;
    insert_run(&pool, session_id, run_id).await;

    let mut conn = pool.acquire().await.unwrap();
    let first = inbox::produce_approval_request(
        &mut conn,
        owner_uid,
        repo_id,
        session_id,
        run_id,
        approval_id,
        "Initial Title".to_string(),
        "Initial Summary".to_string(),
        Utc::now(),
    )
    .await
    .expect("produce initial");

    let second = inbox::produce_approval_request(
        &mut conn,
        owner_uid,
        repo_id,
        session_id,
        run_id,
        approval_id,
        "Updated Title".to_string(),
        "Updated Summary".to_string(),
        Utc::now(),
    )
    .await
    .expect("produce replayed");

    assert_eq!(
        first.id, second.id,
        "same dedup key must update existing entry id"
    );
    assert_eq!(second.title, "Updated Title");

    let count = store
        .count(&pool, 0, principal, &InboxListFilters::default())
        .await
        .expect("count entries");
    assert_eq!(count, 1, "dedup must yield exactly 1 row");
}

#[tokio::test]
async fn acknowledgement_never_resolves_the_source() {
    let temp = TempDir::new().unwrap();
    let pool = setup_test_db(&temp).await;
    let store = InboxStore::new();

    let owner_uid = 1000;
    let principal = PeerPrincipal::from_uid(owner_uid);
    let repo_id = RepositoryId::new();
    let session_id = SessionId::new();
    let run_id = RunId::new();
    let approval_id = ApprovalId::new();

    // `inbox_entries.session_id` and `.run_id` are real foreign keys, so the
    // source rows must exist before an entry can be produced.
    insert_session(&pool, session_id, owner_uid, repo_id).await;
    insert_run(&pool, session_id, run_id).await;

    let mut conn = pool.acquire().await.unwrap();
    let entry = inbox::produce_approval_request(
        &mut conn,
        owner_uid,
        repo_id,
        session_id,
        run_id,
        approval_id,
        "Title".to_string(),
        "Summary".to_string(),
        Utc::now(),
    )
    .await
    .expect("produce");

    assert_eq!(entry.state, InboxEntryState::Unread);
    assert!(entry.acknowledged_at.is_none());
    assert!(entry.dismissed_at.is_none());
    assert!(entry.resolved_at.is_none());

    // Acknowledge
    let acknowledged = store
        .mutate(
            &pool,
            principal,
            &InboxMutation::Acknowledge { entry_id: entry.id },
            Utc::now(),
        )
        .await
        .expect("acknowledge");

    assert_eq!(acknowledged.state, InboxEntryState::Acknowledged);
    assert!(acknowledged.acknowledged_at.is_some());
    assert!(acknowledged.dismissed_at.is_none());
    assert!(
        acknowledged.resolved_at.is_none(),
        "acknowledgement must never set resolved_at"
    );

    // Dismiss
    let dismissed = store
        .mutate(
            &pool,
            principal,
            &InboxMutation::Dismiss { entry_id: entry.id },
            Utc::now(),
        )
        .await
        .expect("dismiss");

    assert_eq!(dismissed.state, InboxEntryState::Dismissed);
    assert!(dismissed.acknowledged_at.is_some());
    assert!(dismissed.dismissed_at.is_some());
    assert!(
        dismissed.resolved_at.is_none(),
        "dismissal must never set resolved_at"
    );
}

#[tokio::test]
async fn source_resolution_is_the_only_writer_of_resolved_at() {
    let temp = TempDir::new().unwrap();
    let pool = setup_test_db(&temp).await;
    let store = InboxStore::new();

    let owner_uid = 1000;
    let principal = PeerPrincipal::from_uid(owner_uid);
    let repo_id = RepositoryId::new();
    let session_id = SessionId::new();
    let run_id = RunId::new();
    let approval_id = ApprovalId::new();

    // `inbox_entries.session_id` and `.run_id` are real foreign keys, so the
    // source rows must exist before an entry can be produced.
    insert_session(&pool, session_id, owner_uid, repo_id).await;
    insert_run(&pool, session_id, run_id).await;

    let mut conn = pool.acquire().await.unwrap();
    let entry = inbox::produce_approval_request(
        &mut conn,
        owner_uid,
        repo_id,
        session_id,
        run_id,
        approval_id,
        "Approval".to_string(),
        "Summary".to_string(),
        Utc::now(),
    )
    .await
    .expect("produce");

    // Source resolution
    let resolved_count = inbox::resolve_approval_entry(&mut conn, approval_id, Utc::now())
        .await
        .expect("resolve");
    assert_eq!(resolved_count, 1);

    let page = store
        .list(&pool, 0, principal, &InboxListQuery::default())
        .await
        .expect("list");
    let resolved_entry = page.items.into_iter().find(|i| i.id == entry.id).unwrap();
    assert_eq!(resolved_entry.state, InboxEntryState::Resolved);
    assert!(resolved_entry.resolved_at.is_some());

    // Subsequent mutate does not clobber Resolved state
    let post_mutate = store
        .mutate(
            &pool,
            principal,
            &InboxMutation::Acknowledge { entry_id: entry.id },
            Utc::now(),
        )
        .await
        .expect("mutate after resolved");
    assert_eq!(post_mutate.state, InboxEntryState::Resolved);
    assert!(post_mutate.resolved_at.is_some());
}

#[tokio::test]
async fn mutation_is_idempotent_under_replay() {
    let temp = TempDir::new().unwrap();
    let pool = setup_test_db(&temp).await;
    let processor = CommandProcessor::default();

    let owner_uid = 1000;
    let principal = PeerPrincipal::from_uid(owner_uid);
    let repo_id = RepositoryId::new();
    let session_id = SessionId::new();
    let run_id = RunId::new();
    let approval_id = ApprovalId::new();

    // `inbox_entries.session_id` and `.run_id` are real foreign keys, so the
    // source rows must exist before an entry can be produced.
    insert_session(&pool, session_id, owner_uid, repo_id).await;
    insert_run(&pool, session_id, run_id).await;

    let mut conn = pool.acquire().await.unwrap();
    let entry = inbox::produce_approval_request(
        &mut conn,
        owner_uid,
        repo_id,
        session_id,
        run_id,
        approval_id,
        "Title".to_string(),
        "Summary".to_string(),
        Utc::now(),
    )
    .await
    .expect("produce");
    drop(conn);

    let cmd = Command {
        command_id: CommandId::new(),
        idempotency_key: "inbox-mutate-1".to_string(),
        expected_revision: None,
        body: CommandBody::MutateInbox {
            mutation: InboxMutation::Acknowledge { entry_id: entry.id },
        },
    };

    let ctx = codypendent_daemon::commands::ApplyContext {
        client_id: ClientId::new(),
        principal,
        role: ClientRole::Contributor,
    };

    let first = processor
        .apply(&pool, ctx.clone(), cmd.clone())
        .await
        .expect("first apply");
    let second = processor
        .apply(&pool, ctx, cmd)
        .await
        .expect("replayed apply");

    assert_eq!(first.command_id, second.command_id);
    let outcome = inbox::inbox_mutation_response(&pool, "inbox-mutate-1")
        .await
        .expect("lookup outcome");
    assert_eq!(outcome.id, entry.id);
    assert_eq!(outcome.state, InboxEntryState::Acknowledged);
}

#[tokio::test]
async fn cursor_paging_has_no_overlap_or_gap() {
    let temp = TempDir::new().unwrap();
    let pool = setup_test_db(&temp).await;
    let store = InboxStore::new();

    let owner_uid = 1000;
    let principal = PeerPrincipal::from_uid(owner_uid);
    let repo_id = RepositoryId::new();
    let session_id = SessionId::new();
    let run_id = RunId::new();

    // Satisfy the `inbox_entries` foreign keys onto `sessions`/`runs`.
    insert_session(&pool, session_id, owner_uid, repo_id).await;
    insert_run(&pool, session_id, run_id).await;

    let mut conn = pool.acquire().await.unwrap();
    let mut created_ids = Vec::new();
    for i in 0..10 {
        let approval_id = ApprovalId::new();
        let entry = inbox::produce_approval_request(
            &mut conn,
            owner_uid,
            repo_id,
            session_id,
            run_id,
            approval_id,
            format!("Title {i}"),
            format!("Summary {i}"),
            Utc::now() + chrono::Duration::seconds(i),
        )
        .await
        .expect("produce");
        created_ids.push(entry.id);
    }
    drop(conn);

    // Page 1 (limit 4)
    let page1 = store
        .list(
            &pool,
            0,
            principal,
            &InboxListQuery {
                limit: Some(4),
                cursor: None,
                filters: InboxListFilters::default(),
            },
        )
        .await
        .expect("page 1");
    assert_eq!(page1.items.len(), 4);
    assert!(page1.next_cursor.is_some());

    // Page 2 (limit 4)
    let page2 = store
        .list(
            &pool,
            0,
            principal,
            &InboxListQuery {
                limit: Some(4),
                cursor: page1.next_cursor,
                filters: InboxListFilters::default(),
            },
        )
        .await
        .expect("page 2");
    assert_eq!(page2.items.len(), 4);
    assert!(page2.next_cursor.is_some());

    // Page 3 (limit 4)
    let page3 = store
        .list(
            &pool,
            0,
            principal,
            &InboxListQuery {
                limit: Some(4),
                cursor: page2.next_cursor,
                filters: InboxListFilters::default(),
            },
        )
        .await
        .expect("page 3");
    assert_eq!(page3.items.len(), 2);
    assert!(page3.next_cursor.is_none());

    let mut collected = Vec::new();
    collected.extend(page1.items.into_iter().map(|e| e.id));
    collected.extend(page2.items.into_iter().map(|e| e.id));
    collected.extend(page3.items.into_iter().map(|e| e.id));

    assert_eq!(collected.len(), 10);
    // Ensure all 10 unique IDs are present
    let mut deduped = collected.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(deduped.len(), 10, "paging must have no duplicates or gaps");
}

#[tokio::test]
async fn owner_isolation_covers_items_cursors_and_counts() {
    let temp = TempDir::new().unwrap();
    let pool = setup_test_db(&temp).await;
    let store = InboxStore::new();

    let owner1 = 1000;
    let owner2 = 1001;
    let p1 = PeerPrincipal::from_uid(owner1);
    let p2 = PeerPrincipal::from_uid(owner2);
    let repo_id = RepositoryId::new();
    let session_id = SessionId::new();
    let run_id = RunId::new();

    // Satisfy the `inbox_entries` foreign keys onto `sessions`/`runs`. The
    // session is owned by `owner1`; owner-scoping is asserted on the entries
    // themselves, which carry their own `owner_uid`.
    insert_session(&pool, session_id, owner1, repo_id).await;
    insert_run(&pool, session_id, run_id).await;

    let mut conn = pool.acquire().await.unwrap();
    let _entry1 = inbox::produce_approval_request(
        &mut conn,
        owner1,
        repo_id,
        session_id,
        run_id,
        ApprovalId::new(),
        "Owner1 Title".to_string(),
        "Summary".to_string(),
        Utc::now(),
    )
    .await
    .expect("produce owner1");

    let _entry2 = inbox::produce_approval_request(
        &mut conn,
        owner2,
        repo_id,
        session_id,
        run_id,
        ApprovalId::new(),
        "Owner2 Title".to_string(),
        "Summary".to_string(),
        Utc::now(),
    )
    .await
    .expect("produce owner2");
    drop(conn);

    let count1 = store
        .count(&pool, 0, p1, &InboxListFilters::default())
        .await
        .unwrap();
    let count2 = store
        .count(&pool, 0, p2, &InboxListFilters::default())
        .await
        .unwrap();
    assert_eq!(count1, 1);
    assert_eq!(count2, 1);

    let page1 = store
        .list(&pool, 0, p1, &InboxListQuery::default())
        .await
        .unwrap();
    assert_eq!(page1.items.len(), 1);
    assert_eq!(page1.items[0].title, "Owner1 Title");

    // Owner 2 using a cursor generated for Owner 1 must be rejected
    let query1 = InboxListQuery {
        limit: Some(1),
        cursor: None,
        filters: InboxListFilters::default(),
    };
    let p1_page = store.list(&pool, 0, p1, &query1).await.unwrap();
    if let Some(cursor) = p1_page.next_cursor {
        let p2_query = InboxListQuery {
            limit: Some(1),
            cursor: Some(cursor),
            filters: InboxListFilters::default(),
        };
        let err = store
            .list(&pool, 0, p2, &p2_query)
            .await
            .expect_err("foreign cursor must fail");
        assert!(matches!(err, inbox::InboxError::InvalidCursor));
    }
}

#[tokio::test]
async fn repository_filter_narrows_and_never_becomes_an_oracle() {
    let temp = TempDir::new().unwrap();
    let pool = setup_test_db(&temp).await;
    let store = InboxStore::new();

    let owner_uid = 1000;
    let principal = PeerPrincipal::from_uid(owner_uid);
    let repo1 = RepositoryId::new();
    let repo2 = RepositoryId::new();
    let unowned_repo = RepositoryId::new();

    // One session/run per repository, inserted up front: `inbox_entries` has
    // real foreign keys onto `sessions`/`runs`, so the ids cannot be minted
    // inline at the produce call.
    let session1 = SessionId::new();
    let run1 = RunId::new();
    let session2 = SessionId::new();
    let run2 = RunId::new();
    insert_session(&pool, session1, owner_uid, repo1).await;
    insert_run(&pool, session1, run1).await;
    insert_session(&pool, session2, owner_uid, repo2).await;
    insert_run(&pool, session2, run2).await;

    let mut conn = pool.acquire().await.unwrap();
    inbox::produce_approval_request(
        &mut conn,
        owner_uid,
        repo1,
        session1,
        run1,
        ApprovalId::new(),
        "Repo1 Title".to_string(),
        "Summary".to_string(),
        Utc::now(),
    )
    .await
    .unwrap();

    inbox::produce_approval_request(
        &mut conn,
        owner_uid,
        repo2,
        session2,
        run2,
        ApprovalId::new(),
        "Repo2 Title".to_string(),
        "Summary".to_string(),
        Utc::now(),
    )
    .await
    .unwrap();
    drop(conn);

    let mut query = InboxListQuery::default();
    query.filters.repository_ids = vec![repo1];
    let page = store.list(&pool, 0, principal, &query).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].repository_id, repo1);

    // Filter by an unowned repository returns an empty list, never an error
    let mut unowned_query = InboxListQuery::default();
    unowned_query.filters.repository_ids = vec![unowned_repo];
    let empty_page = store
        .list(&pool, 0, principal, &unowned_query)
        .await
        .unwrap();
    assert!(empty_page.items.is_empty());
}

#[tokio::test]
async fn deep_links_resolve_for_every_kind() {
    let temp = TempDir::new().unwrap();
    let pool = setup_test_db(&temp).await;

    let owner_uid = 1000;
    let repo_id = RepositoryId::new();
    let session_id = SessionId::new();
    let run_id = RunId::new();
    let approval_id = ApprovalId::new();
    let question_id = QuestionId::new();
    let workflow_id = WorkflowId::new();
    let plugin_id = PluginId::new();

    // Satisfy the `inbox_entries` foreign keys onto `sessions`/`runs`.
    insert_session(&pool, session_id, owner_uid, repo_id).await;
    insert_run(&pool, session_id, run_id).await;

    let mut conn = pool.acquire().await.unwrap();
    let now = Utc::now();

    // 1. ApprovalRequest
    let e1 = inbox::produce_approval_request(
        &mut conn,
        owner_uid,
        repo_id,
        session_id,
        run_id,
        approval_id,
        "T1".into(),
        "S1".into(),
        now,
    )
    .await
    .unwrap();
    assert_eq!(e1.deep_link, InboxDeepLink::Approval { approval_id });

    // 2. AgentQuestion
    let e2 = inbox::produce_agent_question(
        &mut conn,
        owner_uid,
        repo_id,
        session_id,
        run_id,
        question_id,
        "T2".into(),
        "S2".into(),
        now,
    )
    .await
    .unwrap();
    assert_eq!(e2.deep_link, InboxDeepLink::Question { question_id });

    // 3. RunCompleted
    let e3 = inbox::produce_run_terminal(
        &mut conn,
        owner_uid,
        repo_id,
        session_id,
        run_id,
        None,
        InboxEntryKind::RunCompleted,
        "T3".into(),
        "S3".into(),
        now,
    )
    .await
    .unwrap();
    assert_eq!(e3.deep_link, InboxDeepLink::Run { session_id, run_id });

    // 4. RunFailed
    let e4 = inbox::produce_run_terminal(
        &mut conn,
        owner_uid,
        repo_id,
        session_id,
        run_id,
        None,
        InboxEntryKind::RunFailed,
        "T4".into(),
        "S4".into(),
        now,
    )
    .await
    .unwrap();
    assert_eq!(e4.deep_link, InboxDeepLink::Run { session_id, run_id });

    // 5. BudgetWarning
    let e5 = inbox::produce_budget_warning(
        &mut conn,
        owner_uid,
        repo_id,
        "budget-1".into(),
        "2026-08-01",
        Some(session_id),
        Some(run_id),
        None,
        "T5".into(),
        "S5".into(),
        now,
    )
    .await
    .unwrap();
    assert_eq!(
        e5.deep_link,
        InboxDeepLink::Repository {
            repository_id: repo_id
        }
    );

    // 6. WorkflowBlocked
    let e6 = inbox::produce_workflow_blocked(
        &mut conn,
        owner_uid,
        repo_id,
        workflow_id,
        "wf-run-1",
        "node-1",
        Some(session_id),
        Some(run_id),
        "T6".into(),
        "S6".into(),
        now,
    )
    .await
    .unwrap();
    assert_eq!(e6.deep_link, InboxDeepLink::Workflow { workflow_id });

    // 7. PluginPermissionChanged
    let e7 = inbox::produce_plugin_permission_changed(
        &mut conn,
        owner_uid,
        repo_id,
        plugin_id,
        "perm-hash",
        Some(session_id),
        "T7".into(),
        "S7".into(),
        now,
    )
    .await
    .unwrap();
    assert_eq!(e7.deep_link, InboxDeepLink::Plugin { plugin_id });

    // 8. RunnerFailed
    let e8 = inbox::produce_runner_failed(
        &mut conn,
        owner_uid,
        repo_id,
        "runner-1".into(),
        "job-1",
        Some(session_id),
        Some(run_id),
        "T8".into(),
        "S8".into(),
        now,
    )
    .await
    .unwrap();
    assert_eq!(
        e8.deep_link,
        InboxDeepLink::Repository {
            repository_id: repo_id
        }
    );
}

#[tokio::test]
async fn entries_are_produced_transactionally_with_their_source() {
    let temp = TempDir::new().unwrap();
    let pool = setup_test_db(&temp).await;
    let store = InboxStore::new();

    let owner_uid = 1000;
    let principal = PeerPrincipal::from_uid(owner_uid);
    let repo_id = RepositoryId::new();
    let session_id = SessionId::new();
    let run_id = RunId::new();
    let approval_id = ApprovalId::new();
    let _question_id = QuestionId::new();

    insert_session(&pool, session_id, owner_uid, repo_id).await;
    insert_run(&pool, session_id, run_id).await;

    let broker = ApprovalBroker::new();
    let action = ProposedAction::ExecuteCommand {
        program: "cargo".to_string(),
        args: vec!["test".to_string()],
        environment: Default::default(),
        cwd: None,
    };
    let risk = Risk {
        level: RiskLevel::High,
        reasons: vec!["runs code".to_string()],
    };

    // Broker produces approval request & inbox entry transactionally
    broker
        .request_with_id_and_reuse(
            &pool,
            approval_id,
            session_id,
            run_id,
            None,
            action,
            risk,
            vec![],
            None,
            false,
        )
        .await
        .expect("request approval");

    let count_approvals = store
        .count(&pool, 0, principal, &InboxListFilters::default())
        .await
        .unwrap();
    assert_eq!(count_approvals, 1);

    // Questions broker produces agent question & inbox entry transactionally
    let questions_broker = QuestionBroker::new();
    questions_broker
        .ask(
            &pool,
            session_id,
            run_id,
            vec![QuestionPrompt {
                question: "Choose option?".to_string(),
                header: "Choose".to_string(),
                options: vec![
                    QuestionOption {
                        label: "A".to_string(),
                        description: String::new(),
                    },
                    QuestionOption {
                        label: "B".to_string(),
                        description: String::new(),
                    },
                ],
                multiple: false,
                custom: false,
            }],
        )
        .await
        .expect("ask question");

    let count_total = store
        .count(&pool, 0, principal, &InboxListFilters::default())
        .await
        .unwrap();
    assert_eq!(count_total, 2);

    // Terminal run resolves previous entries and produces RunCompleted
    // `append_run_terminal` derives the run id and disposition from the
    // `RunCompleted` body itself; it takes the projected `RunState` separately
    // and rejects any mismatch between the two.
    let events = codypendent_daemon::ledger::append_run_terminal(
        &pool,
        session_id,
        &Actor::System,
        RunState::Completed,
        &EventBody::RunCompleted {
            run_id,
            disposition: RunDisposition::Completed {
                summary: Some("done".to_string()),
            },
            chronicle: ArtifactRef {
                id: ArtifactId::new(),
                media_type: "application/json".to_string(),
                byte_length: 0,
                sha256: "0".repeat(64),
                sensitivity: DataClassification::Internal,
            },
        },
        Utc::now(),
    )
    .await
    .expect("append run terminal");
    assert!(!events.is_empty());

    let page = store
        .list(&pool, 0, principal, &InboxListQuery::default())
        .await
        .unwrap();
    let run_completed = page
        .items
        .iter()
        .find(|e| e.kind == InboxEntryKind::RunCompleted);
    assert!(run_completed.is_some(), "terminal entry produced");

    // Pre-existing approval and question for this run must be resolved
    for entry in &page.items {
        if entry.kind != InboxEntryKind::RunCompleted {
            assert_eq!(entry.state, InboxEntryState::Resolved);
            assert!(entry.resolved_at.is_some());
        }
    }
}

#[tokio::test]
async fn each_entry_notifies_once_across_reconnect() {
    let temp = TempDir::new().unwrap();
    let pool = setup_test_db(&temp).await;

    let owner_uid = 1000;
    let repo_id = RepositoryId::new();

    let mut conn = pool.acquire().await.unwrap();
    // `produce_entry` mints the entry id itself, and
    // `inbox_delivery_attempts.entry_id` is a foreign key onto it, so the
    // attempts below must target the produced entry rather than a fresh id.
    let entry = inbox::produce_entry(
        &mut conn,
        owner_uid,
        repo_id,
        InboxEntryKind::ApprovalRequest,
        "Title".into(),
        "Summary".into(),
        InboxSourceIdentity::Approval {
            approval_id: ApprovalId::new(),
        },
        "dedup-1".into(),
        InboxDeepLink::Approval {
            approval_id: ApprovalId::new(),
        },
        None,
        None,
        None,
        Utc::now(),
    )
    .await
    .unwrap();

    // First attempt -> succeeds (delivered)
    let delivered1 = inbox::record_delivery_attempt(
        &mut conn,
        entry.id,
        "websocket",
        Some("client-1"),
        DeliveryState::Delivered,
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    assert!(delivered1, "first delivery must record");

    // Second attempt across reconnect for the same delivered entry -> deduplicated (returns false)
    let delivered2 = inbox::record_delivery_attempt(
        &mut conn,
        entry.id,
        "websocket",
        Some("client-2"),
        DeliveryState::Delivered,
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    assert!(!delivered2, "duplicate delivery must be suppressed");
}

#[tokio::test]
async fn native_acknowledgement_does_not_decide_the_source() {
    let temp = TempDir::new().unwrap();
    let pool = setup_test_db(&temp).await;
    let store = InboxStore::new();

    let owner_uid = 1000;
    let principal = PeerPrincipal::from_uid(owner_uid);
    let repo_id = RepositoryId::new();
    let session_id = SessionId::new();
    let run_id = RunId::new();
    let approval_id = ApprovalId::new();

    insert_session(&pool, session_id, owner_uid, repo_id).await;
    insert_run(&pool, session_id, run_id).await;

    let broker = ApprovalBroker::new();
    let action = ProposedAction::ExecuteCommand {
        program: "echo".to_string(),
        args: vec!["hello".to_string()],
        environment: Default::default(),
        cwd: None,
    };
    let risk = Risk {
        level: RiskLevel::Low,
        reasons: vec![],
    };

    broker
        .request_with_id_and_reuse(
            &pool,
            approval_id,
            session_id,
            run_id,
            None,
            action,
            risk,
            vec![],
            None,
            false,
        )
        .await
        .expect("request approval");

    let page = store
        .list(&pool, 0, principal, &InboxListQuery::default())
        .await
        .unwrap();
    let entry = page
        .items
        .into_iter()
        .find(|e| e.kind == InboxEntryKind::ApprovalRequest)
        .unwrap();

    // Acknowledge the inbox entry
    store
        .mutate(
            &pool,
            principal,
            &InboxMutation::Acknowledge { entry_id: entry.id },
            Utc::now(),
        )
        .await
        .expect("acknowledge");

    // The underlying approval in the database MUST still be pending
    let (approval_state,): (String,) = sqlx::query_as("SELECT state FROM approvals WHERE id = ?")
        .bind(approval_id.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        approval_state, "pending",
        "acknowledgement must not decide the approval"
    );
}

#[tokio::test]
async fn policy_disabled_adapters_deliver_nothing() {
    let temp = TempDir::new().unwrap();
    let pool = setup_test_db(&temp).await;

    let entry_id = InboxEntryId::new();
    let mut conn = pool.acquire().await.unwrap();

    // Email adapter disabled by policy
    let email_recorded = inbox::record_delivery_attempt(
        &mut conn,
        entry_id,
        "email",
        None,
        DeliveryState::Delivered,
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    assert!(
        !email_recorded,
        "email adapter must be suppressed by policy"
    );

    // Chat adapter disabled by policy
    let chat_recorded = inbox::record_delivery_attempt(
        &mut conn,
        entry_id,
        "chat",
        None,
        DeliveryState::Delivered,
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    assert!(!chat_recorded, "chat adapter must be suppressed by policy");

    // Verify 0 rows in database
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM inbox_delivery_attempts")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
