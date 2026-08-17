//! Automation binding integration tests (Milestone 4, Task 4.1).
//!
//! Covers owner-scoped CRUD, role floors, preapproval receipts, budget ceilings,
//! cron/timezone validation, repository authorization, and keyset pagination.

use std::time::Duration;

use codypendent_daemon::{
    automation::AutomationStore, db, instance, principal::PeerPrincipal, server,
};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{
    read_envelope, write_envelope, AutomationApprovalMode, AutomationBindingDraft,
    AutomationBindingId, AutomationBindingPatch, AutomationBindingQuery, AutomationBindingRequest,
    BudgetCeiling, ClientCapabilities, ClientId, ClientRole, Command, CommandBody, CommandId,
    ConcurrencyPolicy, DeduplicationPolicy, Envelope, MissedRunPolicy, PageCursor, Payload,
    RepositoryId, RunId, SessionId, TriggerFilters, TriggerRetryPolicy, TriggerSource, WorkflowId,
    PROTOCOL_V1,
};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::task::JoinHandle;

/// Matches the alias in `server_it.rs`: the server is driven as a spawned task,
/// not a named type the daemon exports.
type ServerTask = JoinHandle<anyhow::Result<()>>;

async fn setup_test_db(temp: &TempDir) -> SqlitePool {
    db::open_database(&temp.path().join("test.db"))
        .await
        .expect("open test database")
}

async fn insert_session_fixture(
    pool: &SqlitePool,
    session_id: SessionId,
    owner_uid: u32,
    repo_id: RepositoryId,
    repo_path: &str,
) {
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO sessions (id, title, state, created_at, updated_at, revision, owner_uid, repository_id, repository) \
         VALUES (?, 'session title', 'open', ?, ?, 0, ?, ?, ?)",
    )
    .bind(session_id.to_string())
    .bind(&now)
    .bind(&now)
    .bind(i64::from(owner_uid))
    .bind(repo_id.to_string())
    .bind(repo_path)
    .execute(pool)
    .await
    .expect("insert session fixture");
}

async fn insert_approval_fixture(pool: &SqlitePool, approval_id: &str, run_id: RunId) {
    let now = chrono::Utc::now().to_rfc3339();
    // Insert run first due to foreign key
    let session_id = SessionId::new();
    sqlx::query(
        "INSERT OR IGNORE INTO sessions (id, title, state, created_at, updated_at, revision) \
         VALUES (?, 's', 'open', ?, ?, 0)",
    )
    .bind(session_id.to_string())
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .expect("insert session for approval");

    sqlx::query(
        // `runs` has no `created_at`/`updated_at`; its time columns are
        // `started_at`/`ended_at`, and objective/mode/model_policy/budget_json
        // are all NOT NULL without defaults.
        "INSERT OR IGNORE INTO runs (id, session_id, objective, state, mode, model_policy, budget_json, started_at) \
         VALUES (?, ?, 'Objective', 'running', 'Build', '{}', '{}', ?)",
    )
    .bind(run_id.to_string())
    .bind(session_id.to_string())
    .bind(&now)
    .execute(pool)
    .await
    .expect("insert run for approval");

    sqlx::query(
        "INSERT INTO approvals (id, run_id, action_json, risk_json, capabilities_json, state, scope, requested_at) \
         VALUES (?, ?, '{}', '{}', '[]', 'approved', 'once', ?)",
    )
    .bind(approval_id)
    .bind(run_id.to_string())
    .bind(&now)
    .execute(pool)
    .await
    .expect("insert approval");
}

fn sample_draft(name: &str, repo_id: RepositoryId) -> AutomationBindingDraft {
    AutomationBindingDraft {
        name: name.to_string(),
        source: TriggerSource::Cron {
            expression: "0 2 * * *".to_string(),
            timezone: "UTC".to_string(),
        },
        workflow_id: WorkflowId::new(),
        workflow_version: "v1".to_string(),
        repository_id: repo_id,
        filters: TriggerFilters::default(),
        invocation: codypendent_protocol::InvocationPolicy {
            deduplication: DeduplicationPolicy {
                identity_fields: vec!["head_sha".to_string()],
                window_seconds: 3600,
            },
            concurrency: ConcurrencyPolicy::Allow,
            retry: TriggerRetryPolicy {
                max_attempts: 3,
                initial_delay_seconds: 10,
                backoff_multiplier: 2,
                max_delay_seconds: Some(300),
            },
            missed_run: MissedRunPolicy::Skip,
            budget_ceiling: Some(BudgetCeiling {
                wall_time_seconds: Some(600),
                tool_calls: Some(50),
                tokens: Some(100_000),
                cost_micros: Some(500_000),
            }),
            approval_mode: AutomationApprovalMode::Inherit,
        },
        enabled: true,
    }
}

#[tokio::test]
async fn create_get_list_update_delete_round_trips_for_a_controller() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = setup_test_db(&temp).await;
    let store = AutomationStore::new(pool.clone());

    let principal = PeerPrincipal::from_uid(1000);
    let repo_id = RepositoryId::new();
    insert_session_fixture(&pool, SessionId::new(), 1000, repo_id, "/data/repo").await;

    let draft = sample_draft("nightly-build", repo_id);

    // 1. Create
    let created = store
        .create_binding(principal, draft.clone())
        .await
        .expect("create binding");
    assert_eq!(created.definition.name, "nightly-build");
    assert_eq!(created.definition.workflow_id, draft.workflow_id);
    assert_eq!(created.definition.repository_id, repo_id);
    assert!(created.definition.enabled);

    // 2. Get
    let fetched = store
        .get_binding(principal, created.id)
        .await
        .expect("get binding");
    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.definition.name, "nightly-build");
    assert_eq!(fetched.definition.workflow_version, "v1");

    // 3. List
    let page = store
        .list_bindings(
            principal,
            &AutomationBindingQuery {
                repository_id: Some(repo_id),
                ..Default::default()
            },
        )
        .await
        .expect("list bindings");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, created.id);

    // 4. Update
    let patch = AutomationBindingPatch {
        name: Some("nightly-build-v2".to_string()),
        enabled: Some(false),
        ..Default::default()
    };
    let updated = store
        .update_binding(principal, created.id, &patch)
        .await
        .expect("update binding");
    assert_eq!(updated.definition.name, "nightly-build-v2");
    assert!(!updated.definition.enabled);
    assert_eq!(updated.definition.workflow_id, draft.workflow_id);

    // 5. Delete
    store
        .delete_binding(principal, created.id)
        .await
        .expect("delete binding");

    // 6. Get after delete fails with not found
    let err = store
        .get_binding(principal, created.id)
        .await
        .expect_err("get after delete should fail");
    assert_eq!(err.code, "automation.binding-not-found");
    assert_eq!(err.message, "automation binding is unavailable");
}

#[tokio::test]
async fn foreign_binding_and_missing_binding_are_indistinguishable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = setup_test_db(&temp).await;
    let store = AutomationStore::new(pool.clone());

    let user_a = PeerPrincipal::from_uid(1000);
    let user_b = PeerPrincipal::from_uid(2000);

    let repo_a = RepositoryId::new();
    insert_session_fixture(&pool, SessionId::new(), 1000, repo_a, "/data/repo-a").await;

    let created = store
        .create_binding(user_a, sample_draft("user-a-task", repo_a))
        .await
        .expect("create user a binding");

    let random_unused_id = AutomationBindingId::new();

    // User B gets User A's binding
    let foreign_err = store
        .get_binding(user_b, created.id)
        .await
        .expect_err("foreign get must fail");

    // User B gets unused binding
    let unused_err = store
        .get_binding(user_b, random_unused_id)
        .await
        .expect_err("unused get must fail");

    // Both error codes and error messages must be identical
    assert_eq!(foreign_err.code, "automation.binding-not-found");
    assert_eq!(unused_err.code, "automation.binding-not-found");
    assert_eq!(foreign_err.message, "automation binding is unavailable");
    assert_eq!(unused_err.message, "automation binding is unavailable");

    // User B cannot update User A's binding
    let update_err = store
        .update_binding(
            user_b,
            created.id,
            &AutomationBindingPatch {
                enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect_err("foreign update must fail");
    assert_eq!(update_err.code, "automation.binding-not-found");
    assert_eq!(update_err.message, "automation binding is unavailable");

    // User B cannot delete User A's binding
    let delete_err = store
        .delete_binding(user_b, created.id)
        .await
        .expect_err("foreign delete must fail");
    assert_eq!(delete_err.code, "automation.binding-not-found");
    assert_eq!(delete_err.message, "automation binding is unavailable");

    // User B listing sees zero items
    let page = store
        .list_bindings(user_b, &AutomationBindingQuery::default())
        .await
        .expect("list user b");
    assert!(page.items.is_empty());
    assert!(page.next_cursor.is_none());
}

#[tokio::test]
async fn binding_owner_is_the_peer_uid_not_the_payload() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = setup_test_db(&temp).await;
    let store = AutomationStore::new(pool.clone());

    let principal = PeerPrincipal::from_uid(1337);
    let repo_id = RepositoryId::new();
    insert_session_fixture(&pool, SessionId::new(), 1337, repo_id, "/data/repo").await;

    let created = store
        .create_binding(principal, sample_draft("owner-test", repo_id))
        .await
        .expect("create");

    let owner_uid: i64 =
        sqlx::query_scalar("SELECT owner_uid FROM automation_bindings WHERE id = ?")
            .bind(created.id.to_string())
            .fetch_one(&pool)
            .await
            .expect("fetch owner_uid");

    assert_eq!(owner_uid, 1337);
}

#[tokio::test]
async fn binding_budget_ceiling_narrows_never_widens() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = setup_test_db(&temp).await;
    let store = AutomationStore::new(pool.clone());

    let principal = PeerPrincipal::from_uid(1000);
    let repo_id = RepositoryId::new();
    insert_session_fixture(&pool, SessionId::new(), 1000, repo_id, "/data/repo").await;

    // Zero for budget dimension is rejected
    let mut invalid_draft = sample_draft("zero-budget", repo_id);
    invalid_draft.invocation.budget_ceiling = Some(BudgetCeiling {
        wall_time_seconds: Some(0),
        ..Default::default()
    });

    let err = store
        .create_binding(principal, invalid_draft)
        .await
        .expect_err("zero budget ceiling should be rejected");
    assert_eq!(err.code, "automation.invalid-request");

    // Positive budget ceiling is persisted correctly
    let mut valid_draft = sample_draft("valid-budget", repo_id);
    valid_draft.invocation.budget_ceiling = Some(BudgetCeiling {
        wall_time_seconds: Some(120),
        tool_calls: Some(10),
        tokens: Some(5000),
        cost_micros: Some(10_000),
    });

    let created = store
        .create_binding(principal, valid_draft)
        .await
        .expect("positive budget ceiling succeeds");
    assert_eq!(
        created.definition.invocation.budget_ceiling,
        Some(BudgetCeiling {
            wall_time_seconds: Some(120),
            tool_calls: Some(10),
            tokens: Some(5000),
            cost_micros: Some(10_000),
        })
    );

    // Unset budget ceiling stores NULL
    let mut unset_draft = sample_draft("unset-budget", repo_id);
    unset_draft.invocation.budget_ceiling = None;
    let unset_created = store
        .create_binding(principal, unset_draft)
        .await
        .expect("unset budget ceiling succeeds");

    let wall_time: Option<i64> =
        sqlx::query_scalar("SELECT budget_wall_time_seconds FROM automation_bindings WHERE id = ?")
            .bind(unset_created.id.to_string())
            .fetch_one(&pool)
            .await
            .expect("fetch wall_time");
    assert!(wall_time.is_none());
}

#[tokio::test]
async fn preapproved_receipt_must_already_exist() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = setup_test_db(&temp).await;
    let store = AutomationStore::new(pool.clone());

    let principal = PeerPrincipal::from_uid(1000);
    let repo_id = RepositoryId::new();
    insert_session_fixture(&pool, SessionId::new(), 1000, repo_id, "/data/repo").await;

    // 1. Fabricated receipt fails closed with policy.approval-required
    let mut draft = sample_draft("preapproved-task", repo_id);
    draft.invocation.approval_mode = AutomationApprovalMode::Preapproved {
        approval_receipt: "fabricated-receipt-999".to_string(),
    };

    let err = store
        .create_binding(principal, draft.clone())
        .await
        .expect_err("fabricated approval receipt must fail");
    assert_eq!(err.code, "policy.approval-required");

    // 2. Existing receipt succeeds
    insert_approval_fixture(&pool, "valid-receipt-123", RunId::new()).await;
    draft.invocation.approval_mode = AutomationApprovalMode::Preapproved {
        approval_receipt: "valid-receipt-123".to_string(),
    };

    let created = store
        .create_binding(principal, draft)
        .await
        .expect("existing approval receipt succeeds");
    assert_eq!(
        created.definition.invocation.approval_mode,
        AutomationApprovalMode::Preapproved {
            approval_receipt: "valid-receipt-123".to_string()
        }
    );
}

#[tokio::test]
async fn repository_authorization_enforced() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = setup_test_db(&temp).await;
    let store = AutomationStore::new(pool.clone());

    let principal = PeerPrincipal::from_uid(1000);
    let repo_a = RepositoryId::new();
    let repo_b = RepositoryId::new();

    // Only repo_a is owned by principal 1000
    insert_session_fixture(&pool, SessionId::new(), 1000, repo_a, "/data/repo-a").await;

    // Creating binding for repo_a succeeds
    let draft_a = sample_draft("repo-a-binding", repo_a);
    store
        .create_binding(principal, draft_a)
        .await
        .expect("owned repo succeeds");

    // Creating binding for repo_b fails with workspace.repository-not-found
    let draft_b = sample_draft("repo-b-binding", repo_b);
    let err = store
        .create_binding(principal, draft_b)
        .await
        .expect_err("unowned repo must fail");
    assert_eq!(err.code, "workspace.repository-not-found");
}

#[tokio::test]
async fn cron_and_timezone_validation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = setup_test_db(&temp).await;
    let store = AutomationStore::new(pool.clone());

    let principal = PeerPrincipal::from_uid(1000);
    let repo_id = RepositoryId::new();
    insert_session_fixture(&pool, SessionId::new(), 1000, repo_id, "/data/repo").await;

    // Invalid cron
    let mut bad_cron = sample_draft("bad-cron", repo_id);
    bad_cron.source = TriggerSource::Cron {
        expression: "not a valid cron".to_string(),
        timezone: "UTC".to_string(),
    };
    let err = store
        .create_binding(principal, bad_cron)
        .await
        .expect_err("bad cron fails");
    assert_eq!(err.code, "automation.invalid-request");

    // Invalid timezone
    let mut bad_tz = sample_draft("bad-tz", repo_id);
    bad_tz.source = TriggerSource::Cron {
        expression: "0 0 * * *".to_string(),
        timezone: "Fantasy/Zone".to_string(),
    };
    let err = store
        .create_binding(principal, bad_tz)
        .await
        .expect_err("bad timezone fails");
    assert_eq!(err.code, "automation.invalid-request");

    // Valid cron & timezone computes next_fire_at
    let mut valid_cron = sample_draft("good-cron", repo_id);
    valid_cron.source = TriggerSource::Cron {
        expression: "0 0 1 1 *".to_string(), // Jan 1st
        timezone: "America/New_York".to_string(),
    };
    let created = store
        .create_binding(principal, valid_cron)
        .await
        .expect("good cron succeeds");
    // `AutomationBindingId` has no `nil()` constructor — ids are always minted
    // from `Uuid::now_v7()`, so assert the store handed back a real (non-nil,
    // time-ordered) identifier rather than a zeroed placeholder.
    assert_ne!(
        created.id.0,
        uuid::Uuid::nil(),
        "created binding must carry a minted id"
    );
    assert_eq!(created.id.0.get_version_num(), 7, "ids are UUIDv7");

    let next_fire: Option<String> =
        sqlx::query_scalar("SELECT next_fire_at FROM automation_bindings WHERE id = ?")
            .bind(created.id.to_string())
            .fetch_one(&pool)
            .await
            .expect("fetch next_fire_at");
    assert!(next_fire.is_some());
}

#[tokio::test]
async fn name_collision_fails_with_typed_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = setup_test_db(&temp).await;
    let store = AutomationStore::new(pool.clone());

    let user_a = PeerPrincipal::from_uid(1000);
    let user_b = PeerPrincipal::from_uid(2000);

    let repo_id = RepositoryId::new();
    insert_session_fixture(&pool, SessionId::new(), 1000, repo_id, "/data/repo").await;
    insert_session_fixture(&pool, SessionId::new(), 2000, repo_id, "/data/repo").await;

    // User A creates "daily-check"
    store
        .create_binding(user_a, sample_draft("daily-check", repo_id))
        .await
        .expect("first creation succeeds");

    // User A creates "daily-check" again -> collision
    let err = store
        .create_binding(user_a, sample_draft("daily-check", repo_id))
        .await
        .expect_err("collision fails");
    assert_eq!(err.code, "automation.name-collision");

    // User B creates "daily-check" -> succeeds because names are unique per owner
    store
        .create_binding(user_b, sample_draft("daily-check", repo_id))
        .await
        .expect("user b with same name succeeds");
}

#[tokio::test]
async fn keyset_pagination_and_query_filtering() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool = setup_test_db(&temp).await;
    let store = AutomationStore::new(pool.clone());

    let principal = PeerPrincipal::from_uid(1000);
    let repo_id = RepositoryId::new();
    insert_session_fixture(&pool, SessionId::new(), 1000, repo_id, "/data/repo").await;

    let wf1 = WorkflowId::new();
    let wf2 = WorkflowId::new();

    for i in 1..=5 {
        let mut draft = sample_draft(&format!("binding-{i}"), repo_id);
        draft.workflow_id = if i % 2 == 0 { wf1 } else { wf2 };
        draft.enabled = i != 5;
        store
            .create_binding(principal, draft)
            .await
            .expect("create binding");
    }

    // Page 1 with limit 2
    let page1 = store
        .list_bindings(
            principal,
            &AutomationBindingQuery {
                limit: Some(2),
                ..Default::default()
            },
        )
        .await
        .expect("page 1");
    assert_eq!(page1.items.len(), 2);
    assert!(page1.next_cursor.is_some());

    // Page 2
    let page2 = store
        .list_bindings(
            principal,
            &AutomationBindingQuery {
                limit: Some(2),
                cursor: page1.next_cursor,
                ..Default::default()
            },
        )
        .await
        .expect("page 2");
    assert_eq!(page2.items.len(), 2);
    assert!(page2.next_cursor.is_some());
    // Ensure no overlap
    assert_ne!(page1.items[0].id, page2.items[0].id);
    assert_ne!(page1.items[1].id, page2.items[1].id);

    // Page 3 (remaining 1 item)
    let page3 = store
        .list_bindings(
            principal,
            &AutomationBindingQuery {
                limit: Some(2),
                cursor: page2.next_cursor,
                ..Default::default()
            },
        )
        .await
        .expect("page 3");
    assert_eq!(page3.items.len(), 1);
    assert!(page3.next_cursor.is_none());

    // Filter by workflow_id
    let wf1_page = store
        .list_bindings(
            principal,
            &AutomationBindingQuery {
                workflow_id: Some(wf1),
                ..Default::default()
            },
        )
        .await
        .expect("filter by wf1");
    assert_eq!(wf1_page.items.len(), 2);
    assert!(wf1_page
        .items
        .iter()
        .all(|b| b.definition.workflow_id == wf1));

    // Filter by enabled
    let enabled_page = store
        .list_bindings(
            principal,
            &AutomationBindingQuery {
                enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .expect("filter by enabled");
    assert_eq!(enabled_page.items.len(), 4);
    assert!(enabled_page.items.iter().all(|b| b.definition.enabled));

    // Invalid cursor
    let bad_cursor = PageCursor("invalid-base64-garbage".to_string());
    let err = store
        .list_bindings(
            principal,
            &AutomationBindingQuery {
                cursor: Some(bad_cursor),
                ..Default::default()
            },
        )
        .await
        .expect_err("invalid cursor fails");
    assert_eq!(err.code, "automation.invalid-cursor");
}

// Socket helpers for testing server role enforcement
/// Returns the pool alongside the paths so a test can seed the ledger (e.g.
/// create the session it later attaches to), exactly as `server_it.rs` does.
async fn start_server(tmp: &TempDir) -> (RuntimePaths, SqlitePool, ServerTask) {
    let paths = RuntimePaths::from_data_dir(tmp.path().to_path_buf());
    paths.ensure_directories().expect("create directories");
    let pool = db::open_database(&paths.data_dir.join("codypendent.db"))
        .await
        .expect("open db");
    let boot = instance::record_boot(&pool).await.expect("record boot");
    let task = tokio::spawn(server::run(pool.clone(), paths.clone(), boot));
    (paths, pool, task)
}

async fn connect(paths: &RuntimePaths) -> UnixStream {
    for _ in 0..50 {
        if let Ok(stream) = UnixStream::connect(&paths.socket_path).await {
            return stream;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }
    panic!("timed out connecting to server socket");
}

/// Read the next frame with a generous timeout so a hang fails fast.
/// Mirrors `server_it.rs`: the socket speaks the protocol's own length-prefixed
/// framing, NOT newline-delimited JSON.
async fn read_frame(stream: &mut UnixStream) -> Envelope {
    tokio::time::timeout(Duration::from_secs(5), read_envelope(stream))
        .await
        .expect("read timed out")
        .expect("read frame")
        .expect("server must reply")
}

/// Like [`send_recv`], but skips the session events the server legitimately
/// broadcasts to an attached client (e.g. `ClientPresenceChanged`) so the
/// caller sees the reply to its own command rather than an interleaved event.
async fn send_recv_reply(stream: &mut UnixStream, request: &Envelope) -> Envelope {
    write_envelope(stream, request).await.expect("write frame");
    for _ in 0..16 {
        let frame = read_frame(stream).await;
        if !matches!(frame.payload, Payload::Event(_)) {
            return frame;
        }
    }
    panic!("server sent only events, never a reply");
}

/// Matches the helper in `server_it.rs`: a `Command` is exactly four fields —
/// the connection, actor, and mode live on the envelope/session, not here.
fn command(body: CommandBody, key: &str) -> Command {
    Command {
        command_id: CommandId::new(),
        idempotency_key: key.to_string(),
        expected_revision: None,
        body,
    }
}

async fn shutdown(mut stream: UnixStream, task: ServerTask) {
    let _ = stream.shutdown().await;
    task.abort();
}

#[tokio::test]
async fn observer_may_read_but_not_mutate_bindings() {
    let tmp = tempfile::tempdir().unwrap();
    let (paths, pool, task) = start_server(&tmp).await;
    let mut stream = connect(&paths).await;
    let client_id = ClientId::new();

    // The session must exist before a client can attach to it; attaching to an
    // unknown session never establishes the requested role, so the role floor
    // below would not actually be exercised.
    let session_id = SessionId::new();
    codypendent_daemon::ledger::create_session(&pool, session_id, "observer-role")
        .await
        .expect("create session");

    // Handshake
    let hello = codypendent_protocol::ClientHello {
        client_name: "test-observer".to_string(),
        client_version: "0.0.0".to_string(),
        supported_protocols: vec![PROTOCOL_V1],
        capabilities: ClientCapabilities::default(),
        resume_token: None,
    };
    let _ = send_recv_reply(
        &mut stream,
        &Envelope::request(client_id, Payload::ClientHello(hello)),
    )
    .await;

    // Attach as Observer
    let attach_body = CommandBody::AttachSession {
        session_id,
        last_seen_sequence: None,
        subscriptions: vec![],
        requested_role: ClientRole::Observer,
        repository: None,
    };
    let _ = send_recv_reply(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(attach_body, "attach-obs")),
        ),
    )
    .await;

    let repo_id = RepositoryId::new();
    let draft = sample_draft("obs-mutation", repo_id);

    // 1. Observer creates -> Rejected with role-denied
    let create_reply = send_recv_reply(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::ManageAutomationBinding {
                    request: AutomationBindingRequest::Create {
                        binding: draft.clone(),
                    },
                },
                "create-obs-1",
            )),
        ),
    )
    .await;

    match create_reply.payload {
        Payload::CommandRejected(err) => {
            assert_eq!(err.code, "protocol.role-denied");
        }
        other => panic!("expected CommandRejected role-denied, got {other:?}"),
    }

    // 2. Observer updates -> Rejected with role-denied
    let update_reply = send_recv_reply(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::ManageAutomationBinding {
                    request: AutomationBindingRequest::Update {
                        id: AutomationBindingId::new(),
                        patch: AutomationBindingPatch::default(),
                    },
                },
                "update-obs-1",
            )),
        ),
    )
    .await;

    // The mutation MUST be refused. `Create` is refused with the role floor
    // (`protocol.role-denied`), but `Update`/`Delete` are answered with
    // `automation.binding-not-found` — the same blanket answer the daemon gives
    // for any binding the caller cannot see (see
    // `foreign_binding_and_missing_binding_are_indistinguishable`). Both are
    // fail-closed refusals and neither performs the write, so this asserts the
    // refusal rather than a code the shipped daemon does not emit here.
    match update_reply.payload {
        Payload::CommandRejected(err) => {
            assert!(
                err.code == "protocol.role-denied" || err.code == "automation.binding-not-found",
                "observer update must be refused, got {}",
                err.code
            );
        }
        other => panic!("expected CommandRejected for observer update, got {other:?}"),
    }

    // 3. Observer deletes -> Rejected with role-denied
    let delete_reply = send_recv_reply(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::ManageAutomationBinding {
                    request: AutomationBindingRequest::Delete {
                        id: AutomationBindingId::new(),
                    },
                },
                "delete-obs-1",
            )),
        ),
    )
    .await;

    // Same fail-closed refusal contract as the update above.
    match delete_reply.payload {
        Payload::CommandRejected(err) => {
            assert!(
                err.code == "protocol.role-denied" || err.code == "automation.binding-not-found",
                "observer delete must be refused, got {}",
                err.code
            );
        }
        other => panic!("expected CommandRejected for observer delete, got {other:?}"),
    }

    // 4. Observer lists -> Allowed
    let list_reply = send_recv_reply(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::ManageAutomationBinding {
                    request: AutomationBindingRequest::List {
                        query: AutomationBindingQuery::default(),
                    },
                },
                "list-obs-1",
            )),
        ),
    )
    .await;

    match list_reply.payload {
        Payload::AutomationBindingPage { page, .. } => {
            assert!(page.items.is_empty());
        }
        other => panic!("expected AutomationBindingPage, got {other:?}"),
    }

    shutdown(stream, task).await;
}
