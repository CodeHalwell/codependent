//! Outcome 19: the connection principal, session ownership, and approval
//! authority — driven over a real Unix socket against a real daemon.
//!
//! Every test here reproduces something the 2026-08-13 daemon-core review proved
//! on the wire against the shipped daemon (`docs/reviews/2026-08-13-verticals/
//! daemon-core.md`, F-19-1 and F-19-5): a fresh, never-attached client read
//! another session's entire history, and approved another client's parked
//! `shell.run ls -la`, which then executed.
//!
//! The "other user" is a session row whose `owner_uid` is not this process's
//! uid. That is exactly the state the daemon would be in with a second OS user
//! on the machine, and it needs no privilege to set up — which matters, because
//! CI runs everything as one user.

use std::time::Duration;

use codypendent_daemon::{db, instance, server};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{
    read_envelope, write_envelope, ApprovalDecision, ApprovalId, ApprovalScope, ClientCapabilities,
    ClientHello, ClientId, ClientRole, Command, CommandBody, CommandId, Envelope, EventBody,
    Payload, RunId, SessionId, Subscription, PROTOCOL_V1,
};
use sqlx::SqlitePool;
use tokio::net::UnixStream;
use tokio::task::JoinHandle;

type ServerTask = JoinHandle<anyhow::Result<()>>;

async fn start_server(tmp: &tempfile::TempDir) -> (RuntimePaths, ServerTask, SqlitePool) {
    let paths = RuntimePaths::from_data_dir(tmp.path().to_path_buf());
    paths.ensure_directories().expect("create directories");
    let pool = db::open_database(&paths.data_dir.join("codypendent.db"))
        .await
        .expect("open db");
    let boot = instance::record_boot(&pool).await.expect("record boot");
    let seed_pool = db::open_database(&paths.data_dir.join("codypendent.db"))
        .await
        .expect("open seed pool");
    let task = tokio::spawn(server::run(pool, paths.clone(), boot));
    (paths, task, seed_pool)
}

async fn connect(paths: &RuntimePaths) -> UnixStream {
    for _ in 0..200 {
        if let Ok(stream) = UnixStream::connect(&paths.socket_path).await {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("daemon never accepted a connection");
}

async fn read_frame(stream: &mut UnixStream) -> Envelope {
    tokio::time::timeout(Duration::from_secs(5), read_envelope(stream))
        .await
        .expect("read timed out")
        .expect("read frame")
        .expect("server must reply")
}

/// Send one request and return its reply, skipping the frames an attached
/// connection also receives asynchronously (heartbeat pings, and the presence
/// events an attach itself fans back out).
async fn send_recv(stream: &mut UnixStream, request: &Envelope) -> Envelope {
    write_envelope(stream, request).await.expect("write frame");
    for _ in 0..16 {
        let frame = read_frame(stream).await;
        if !matches!(frame.payload, Payload::Ping | Payload::Event(_)) {
            return frame;
        }
    }
    panic!("no reply arrived among the streamed frames");
}

fn command(body: CommandBody, key: &str) -> Command {
    Command {
        command_id: CommandId::new(),
        idempotency_key: key.to_string(),
        expected_revision: None,
        body,
    }
}

async fn handshake(stream: &mut UnixStream, client_id: ClientId) {
    let hello = ClientHello {
        client_name: "multi-user-it".to_string(),
        client_version: "0.0.0".to_string(),
        supported_protocols: vec![PROTOCOL_V1],
        capabilities: ClientCapabilities::default(),
        resume_token: None,
    };
    let reply = send_recv(
        stream,
        &Envelope::request(client_id, Payload::ClientHello(hello)),
    )
    .await;
    assert!(
        matches!(reply.payload, Payload::ServerHello(_)),
        "handshake must succeed: {:?}",
        reply.payload
    );
}

/// This process's uid, taken from a file it creates — the same trick the daemon
/// uses on its own socket, and the only way to learn it without `libc`.
fn our_uid(tmp: &tempfile::TempDir) -> u32 {
    use std::os::unix::fs::MetadataExt as _;
    let probe = tmp.path().join(".uid-probe");
    std::fs::write(&probe, b"x").expect("write probe");
    std::fs::metadata(&probe).expect("stat probe").uid()
}

/// Seed a session owned by somebody else, with one event, one run, and one
/// pending approval on that run — the exact shape the review approved across.
/// Returns `(session, run, approval)`.
async fn seed_foreign_session(
    pool: &SqlitePool,
    foreign_uid: u32,
) -> (SessionId, RunId, ApprovalId) {
    let session_id = SessionId::new();
    let run_id = RunId::new();
    let approval_id = ApprovalId::new();
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO sessions (id, title, state, created_at, updated_at, revision, owner_uid) \
         VALUES (?, 'another user''s work', 'open', ?, ?, 0, ?)",
    )
    .bind(session_id.to_string())
    .bind(&now)
    .bind(&now)
    .bind(i64::from(foreign_uid))
    .execute(pool)
    .await
    .expect("seed session");

    sqlx::query(
        "INSERT INTO events \
         (session_id, sequence, occurred_at, actor, body, causation_id, correlation_id, schema_version) \
         VALUES (?, 1, ?, ?, ?, NULL, NULL, 1)",
    )
    .bind(session_id.to_string())
    .bind(&now)
    .bind(serde_json::to_string(&codypendent_protocol::Actor::System).unwrap())
    .bind(
        serde_json::to_string(&EventBody::NoteAppended {
            // Stands in for the context manifest the review exfiltrated.
            text: "SECRET: another user's private context".to_string(),
            run_id: None,
        })
        .unwrap(),
    )
    .execute(pool)
    .await
    .expect("seed event");

    sqlx::query(
        "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json) \
         VALUES (?, ?, 'their run', 'AwaitingApproval', 'Build', 'hosted-default', '{}')",
    )
    .bind(run_id.to_string())
    .bind(session_id.to_string())
    .execute(pool)
    .await
    .expect("seed run");

    sqlx::query(
        "INSERT INTO approvals \
         (id, run_id, action_json, risk_json, capabilities_json, state, scope, requested_at) \
         VALUES (?, ?, ?, ?, '[]', 'pending', 'once', ?)",
    )
    .bind(approval_id.to_string())
    .bind(run_id.to_string())
    .bind(r#"{"type":"RunCommand","command":"ls -la"}"#)
    .bind(r#"{"level":"Low"}"#)
    .bind(&now)
    .execute(pool)
    .await
    .expect("seed approval");

    (session_id, run_id, approval_id)
}

async fn approval_state(pool: &SqlitePool, approval_id: ApprovalId) -> (String, Option<String>) {
    sqlx::query_as("SELECT state, resolved_by FROM approvals WHERE id = ?")
        .bind(approval_id.to_string())
        .fetch_one(pool)
        .await
        .expect("read approval")
}

// --- the two attacks the review demonstrated ---------------------------------

/// F-19-5, reproduced and now refused. The review connected a fresh client that
/// had never attached to anything and read all 17 events of somebody else's
/// session, "including the full context manifest … and the model's output".
#[tokio::test]
async fn a_stranger_cannot_read_another_principals_session_events() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let foreign_uid = our_uid(&tmp) + 1;
    let (paths, task, pool) = start_server(&tmp).await;
    let (session_id, _, _) = seed_foreign_session(&pool, foreign_uid).await;

    let mut stream = connect(&paths).await;
    let client_id = ClientId::new();
    handshake(&mut stream, client_id).await;

    let reply = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::ReadSessionEvents {
                    session_id,
                    after_sequence: 0,
                    limit: 0,
                },
                "read-foreign",
            )),
        ),
    )
    .await;

    match reply.payload {
        Payload::CommandRejected(error) => {
            assert_eq!(error.code, "protocol.session-not-found");
        }
        Payload::SessionEventsPage { events, .. } => panic!(
            "THE REVIEW'S DISCLOSURE: a stranger read {} events of another principal's session",
            events.len()
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }

    task.abort();
}

/// F-19-1, the approval-gate bypass, reproduced and now refused. The review
/// parked a `shell.run ls -la`, resolved it "by a *stranger* client over a raw
/// socket", and the daemon executed it. The gate in front of arbitrary command
/// execution has to hold against a principal that does not own the run.
#[tokio::test]
async fn a_stranger_cannot_resolve_another_principals_approval() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let foreign_uid = our_uid(&tmp) + 1;
    let (paths, task, pool) = start_server(&tmp).await;
    let (_, _, approval_id) = seed_foreign_session(&pool, foreign_uid).await;

    let mut stream = connect(&paths).await;
    let client_id = ClientId::new();
    handshake(&mut stream, client_id).await;

    let reply = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::ResolveApproval {
                    approval_id,
                    decision: ApprovalDecision::Approve,
                    scope: ApprovalScope::Once,
                },
                "approve-foreign",
            )),
        ),
    )
    .await;

    match reply.payload {
        Payload::CommandRejected(error) => assert_eq!(error.code, "approval.not-found"),
        Payload::CommandAccepted { .. } => {
            panic!("THE REVIEW'S BYPASS: a stranger approved another principal's parked command")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    // The refusal has to be real, not merely a different reply: the durable row
    // must still be pending, or the parked runtime waiter would still be woken.
    let (state, resolved_by) = approval_state(&pool, approval_id).await;
    assert_eq!(state, "pending", "the approval must remain unresolved");
    assert_eq!(resolved_by, None);

    task.abort();
}

/// Attach is the third door onto the same session. It has to answer exactly as a
/// missing session does.
#[tokio::test]
async fn a_stranger_cannot_attach_to_another_principals_session() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let foreign_uid = our_uid(&tmp) + 1;
    let (paths, task, pool) = start_server(&tmp).await;
    let (session_id, _, _) = seed_foreign_session(&pool, foreign_uid).await;

    let mut stream = connect(&paths).await;
    let client_id = ClientId::new();
    handshake(&mut stream, client_id).await;

    let reply = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::AttachSession {
                    session_id,
                    last_seen_sequence: None,
                    subscriptions: vec![Subscription::SessionSummary],
                    // Asserting the most privileged role must not help: the role
                    // is a self-restriction, never a grant.
                    requested_role: ClientRole::Approver,
                    repository: None,
                },
                "attach-foreign",
            )),
        ),
    )
    .await;

    match reply.payload {
        Payload::Error(error) => assert_eq!(error.code, "protocol.session-not-found"),
        Payload::Catchup { .. } => panic!("a stranger attached to another principal's session"),
        other => panic!("expected a refusal, got {other:?}"),
    }

    task.abort();
}

/// BRIEF rule 2 / F-19-7: a gate that answers differently for "not yours" and
/// "not there" is an enumeration oracle. Both replies must be the same shape,
/// down to the error code and the message template.
#[tokio::test]
async fn a_refusal_is_indistinguishable_from_a_missing_session() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let foreign_uid = our_uid(&tmp) + 1;
    let (paths, task, pool) = start_server(&tmp).await;
    let (foreign_session, _, _) = seed_foreign_session(&pool, foreign_uid).await;
    let never_existed = SessionId::new();

    let mut stream = connect(&paths).await;
    let client_id = ClientId::new();
    handshake(&mut stream, client_id).await;

    let mut replies = Vec::new();
    for (index, session_id) in [foreign_session, never_existed].into_iter().enumerate() {
        let reply = send_recv(
            &mut stream,
            &Envelope::request(
                client_id,
                Payload::Command(command(
                    CommandBody::ReadSessionEvents {
                        session_id,
                        after_sequence: 0,
                        limit: 0,
                    },
                    &format!("oracle-{index}"),
                )),
            ),
        )
        .await;
        match reply.payload {
            Payload::CommandRejected(error) => replies.push((error, session_id)),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    let (existing, existing_id) = &replies[0];
    let (missing, missing_id) = &replies[1];
    assert_eq!(existing.code, missing.code);
    assert_eq!(existing.retryable, missing.retryable);
    // The only difference permitted is the id the caller itself supplied.
    assert_eq!(existing.message, format!("no session {existing_id}"));
    assert_eq!(missing.message, format!("no session {missing_id}"));

    task.abort();
}

// --- ordinary same-user operation is unaffected -------------------------------

/// The other half of the contract: the owning principal must be able to do
/// everything it could before. Create a session over the socket, read its
/// events back, and resolve its own approval — all through the same gates.
#[tokio::test]
async fn the_owning_principal_is_unaffected() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let uid = our_uid(&tmp);
    let (paths, task, pool) = start_server(&tmp).await;

    let mut stream = connect(&paths).await;
    let client_id = ClientId::new();
    handshake(&mut stream, client_id).await;

    // 1. Create a session over the wire.
    let reply = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::CreateSession {
                    title: "mine".to_string(),
                    workspace: codypendent_protocol::WorkspaceId::new(),
                    repository: None,
                },
                "create-own",
            )),
        ),
    )
    .await;
    assert!(
        matches!(reply.payload, Payload::CommandAccepted { .. }),
        "the owner must be able to create a session: {:?}",
        reply.payload
    );
    let session_id = reply.session_id.expect("CreateSession returns its session");

    // 2. The server recorded THIS principal as the owner — not the client id.
    let (owner_uid,): (Option<i64>,) =
        sqlx::query_as("SELECT owner_uid FROM sessions WHERE id = ?")
            .bind(session_id.to_string())
            .fetch_one(&pool)
            .await
            .expect("read owner");
    assert_eq!(
        owner_uid,
        Some(i64::from(uid)),
        "a session must record the connecting principal's uid"
    );

    // 3. Attach and read its history back.
    let reply = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::AttachSession {
                    session_id,
                    last_seen_sequence: None,
                    subscriptions: vec![Subscription::SessionSummary],
                    requested_role: ClientRole::Controller,
                    repository: None,
                },
                "attach-own",
            )),
        ),
    )
    .await;
    assert!(
        matches!(reply.payload, Payload::Catchup { .. }),
        "the owner must be able to attach: {:?}",
        reply.payload
    );

    let reply = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::ReadSessionEvents {
                    session_id,
                    after_sequence: 0,
                    limit: 0,
                },
                "read-own",
            )),
        ),
    )
    .await;
    match reply.payload {
        Payload::SessionEventsPage { events, .. } => {
            assert!(!events.is_empty(), "the owner sees its own SessionCreated");
        }
        other => panic!("the owner must be able to page its own history, got {other:?}"),
    }

    // 4. Resolve an approval on its own run. The identity recorded is the peer
    //    uid, not the UUID this client chose for itself.
    let (run_id, approval_id) = seed_owned_run_and_approval(&pool, session_id).await;
    let reply = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::ResolveApproval {
                    approval_id,
                    decision: ApprovalDecision::Approve,
                    scope: ApprovalScope::Once,
                },
                "approve-own",
            )),
        ),
    )
    .await;
    assert!(
        matches!(reply.payload, Payload::CommandAccepted { .. }),
        "the owner must be able to resolve its own approval: {:?}",
        reply.payload
    );

    let (state, resolved_by) = approval_state(&pool, approval_id).await;
    assert_eq!(state, "approved");
    assert_eq!(
        resolved_by,
        Some(format!("uid:{uid}")),
        "resolved_by must name the OS user the kernel reported, not a client-chosen UUID"
    );
    assert_ne!(
        resolved_by,
        Some(client_id.to_string()),
        "resolved_by must no longer be the client's own envelope id"
    );
    let _ = run_id;

    task.abort();
}

/// The connection principal is not the client id, and cannot be steered by one:
/// two connections claiming wildly different `client_id`s are the same
/// principal, because they are the same OS user.
#[tokio::test]
async fn a_client_cannot_change_its_principal_by_choosing_a_client_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let uid = our_uid(&tmp);
    let (paths, task, pool) = start_server(&tmp).await;

    let mut first = connect(&paths).await;
    let first_id = ClientId::new();
    handshake(&mut first, first_id).await;
    let reply = send_recv(
        &mut first,
        &Envelope::request(
            first_id,
            Payload::Command(command(
                CommandBody::CreateSession {
                    title: "first".to_string(),
                    workspace: codypendent_protocol::WorkspaceId::new(),
                    repository: None,
                },
                "create-first",
            )),
        ),
    )
    .await;
    let session_id = reply.session_id.expect("session created");

    // A *different* client id on a *different* connection — the review's
    // "stranger". Same OS user, so same principal, so it legitimately gets in.
    let mut second = connect(&paths).await;
    let second_id = ClientId::new();
    assert_ne!(first_id, second_id);
    handshake(&mut second, second_id).await;
    let reply = send_recv(
        &mut second,
        &Envelope::request(
            second_id,
            Payload::Command(command(
                CommandBody::ReadSessionEvents {
                    session_id,
                    after_sequence: 0,
                    limit: 0,
                },
                "read-as-peer",
            )),
        ),
    )
    .await;
    assert!(
        matches!(reply.payload, Payload::SessionEventsPage { .. }),
        "a same-uid peer IS the owner and must not be locked out: {:?}",
        reply.payload
    );

    let (owner_uid,): (Option<i64>,) =
        sqlx::query_as("SELECT owner_uid FROM sessions WHERE id = ?")
            .bind(session_id.to_string())
            .fetch_one(&pool)
            .await
            .expect("read owner");
    assert_eq!(owner_uid, Some(i64::from(uid)));

    task.abort();
}

/// Seed a run + pending approval on a session the caller already owns.
async fn seed_owned_run_and_approval(
    pool: &SqlitePool,
    session_id: SessionId,
) -> (RunId, ApprovalId) {
    let run_id = RunId::new();
    let approval_id = ApprovalId::new();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json) \
         VALUES (?, ?, 'my run', 'AwaitingApproval', 'Build', 'hosted-default', '{}')",
    )
    .bind(run_id.to_string())
    .bind(session_id.to_string())
    .execute(pool)
    .await
    .expect("seed run");
    sqlx::query(
        "INSERT INTO approvals \
         (id, run_id, action_json, risk_json, capabilities_json, state, scope, requested_at) \
         VALUES (?, ?, ?, ?, '[]', 'pending', 'once', ?)",
    )
    .bind(approval_id.to_string())
    .bind(run_id.to_string())
    .bind(r#"{"type":"RunCommand","command":"ls -la"}"#)
    .bind(r#"{"level":"Low"}"#)
    .bind(&now)
    .execute(pool)
    .await
    .expect("seed approval");
    (run_id, approval_id)
}
