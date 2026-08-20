//! The Phase-5 blackboard read surface (STEP 5.3 client transport): an **Observer**
//! reading a workflow run's board over the **real** `codypendentd` socket.
//!
//! This exercises the vertical end to end — the assembly binary's wired
//! `BlackboardReader` seam, the daemon's connection-level interception of
//! `ReadBlackboard`, and the `BlackboardItemView` projection — against the actual
//! daemon process. It lives in the crate that builds the `codypendentd` binary so
//! `CARGO_BIN_EXE_codypendentd` is defined (like `docs_sync_it.rs`).
//!
//! It also pins two invariants: an **Observer may read** the board (the read
//! carries no role gate — only the executor writes it, so there is no client post
//! command to gate), and a read item's `author` is the **node identity built
//! server-side** (`{role, node_id, …}`), never the reading client — a client can
//! never appear as an author because no client post path exists.

use std::path::Path;
use std::process::{Child, Command as StdCommand, Stdio};
use std::time::Duration;

use codypendent_daemon::db;
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{
    read_envelope, write_envelope, BlackboardItemView, ClientCapabilities, ClientHello, ClientId,
    ClientRole, Command, CommandBody, CommandId, Envelope, Payload, SessionId, Subscription,
    WorkspaceId, PROTOCOL_V1,
};
use codypendent_runtime::blackboard::TaskBoardChannel;
use codypendent_workflow::{BlackboardStore, NewBlackboardItem};
use serde_json::json;
use sqlx::SqlitePool;
use tokio::net::UnixStream;

/// Owns the spawned daemon process; kills it on drop.
struct Daemon {
    child: Child,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_daemon(data_dir: &Path) -> Daemon {
    let child = StdCommand::new(env!("CARGO_BIN_EXE_codypendentd"))
        .env("CODYPENDENT_DATA_DIR", data_dir)
        .env_remove("CODYPENDENT_SOCKET")
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn codypendentd");
    Daemon { child }
}

async fn wait_for_socket(paths: &RuntimePaths) -> UnixStream {
    // 60s, not 10s. Spawning a daemon — process start, migrations, index open —
    // is fast on an idle machine and slow on a runner already saturated by the
    // rest of the suite. This is a STARTUP detector: when the daemon comes up
    // promptly the bound costs nothing, and 10s was short enough that
    // `blackboard_it` failed here during a full `--workspace` run.
    for _ in 0..1200 {
        if let Ok(stream) = UnixStream::connect(&paths.socket_path).await {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("daemon socket never came up");
}

async fn open_pool(paths: &RuntimePaths) -> SqlitePool {
    db::open_database(&paths.data_dir.join("codypendent.db"))
        .await
        .expect("open db")
}

/// Seed a completed workflow run (so startup recovery ignores it) holding one
/// `finding` on its board, authored as the node executor would attribute it.
/// Runs before the daemon starts, so the daemon opens a DB that already holds it.
async fn seed_board(paths: &RuntimePaths, workflow_run_id: &str) {
    paths.ensure_directories().expect("create directories");
    let pool = open_pool(paths).await;
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO workflow_runs \
         (id, workflow_id, workflow_version, graph_signature, inputs_json, state, \
          created_at, updated_at) \
         VALUES (?, 'review', 1, 'sig', 'null', 'completed', ?, ?)",
    )
    .bind(workflow_run_id)
    .bind(&now)
    .bind(&now)
    .execute(&pool)
    .await
    .expect("seed workflow run");

    BlackboardStore::new()
        .post(
            &pool,
            workflow_run_id,
            NewBlackboardItem {
                kind: codypendent_workflow::BlackboardKind::Finding,
                payload: json!({ "summary": "the parser drops trailing commas" }),
                // The author the executor builds server-side from the run context.
                author: json!({
                    "role": "investigator",
                    "node_id": "inspect",
                    "run_id": "run-xyz",
                    "workflow_run_id": workflow_run_id,
                }),
                confidence: Some(0.9),
                evidence: vec![json!({ "path": "src/parse.rs", "line": 42 })],
                board: Default::default(),
            },
        )
        .await
        .expect("seed finding");
    pool.close().await;
}

async fn read_frame(stream: &mut UnixStream) -> Envelope {
    // A HANG DETECTOR, not a latency assertion: when the daemon answers — which
    // on an idle machine is immediate — this bound costs nothing. Five seconds
    // was comfortable locally and too tight on a loaded CI runner, which is how
    // the v0.12.1 release gate went red on a socket read minutes after the same
    // commit passed the identical command in `ci`.
    tokio::time::timeout(Duration::from_secs(30), read_envelope(stream))
        .await
        .expect("read timed out")
        .expect("read frame")
        .expect("server must reply")
}

async fn send(stream: &mut UnixStream, client: ClientId, body: CommandBody, key: &str) {
    let command = Command {
        command_id: CommandId::new(),
        idempotency_key: key.to_string(),
        expected_revision: None,
        body,
    };
    write_envelope(
        stream,
        &Envelope::request(client, Payload::Command(command)),
    )
    .await
    .expect("write command");
}

async fn handshake(stream: &mut UnixStream, client: ClientId) {
    let hello = ClientHello {
        client_name: "blackboard-it".to_string(),
        client_version: "0".to_string(),
        supported_protocols: vec![PROTOCOL_V1],
        capabilities: ClientCapabilities::default(),
        resume_token: None,
    };
    write_envelope(
        stream,
        &Envelope::request(client, Payload::ClientHello(hello)),
    )
    .await
    .expect("write hello");
    assert!(matches!(
        read_frame(stream).await.payload,
        Payload::ServerHello(_)
    ));
}

/// Create a session and return its id (rides the reply envelope).
async fn create_session(stream: &mut UnixStream, client: ClientId) -> SessionId {
    send(
        stream,
        client,
        CommandBody::CreateSession {
            workspace: WorkspaceId::new(),
            title: "bb".to_string(),
            repository: None,
            internal: false,
            parent_session_id: None,
            parent_run_id: None,
        },
        "create",
    )
    .await;
    loop {
        let env = read_frame(stream).await;
        match env.payload {
            Payload::CommandAccepted { .. } => return env.session_id.expect("session id"),
            Payload::Ping => continue,
            other => panic!("expected CommandAccepted for CreateSession, got {other:?}"),
        }
    }
}

/// Attach `stream` to `session` as an **Observer** (narrowing the connection role),
/// so a subsequent `ReadBlackboard` is issued under the Observer role.
async fn attach_as_observer(stream: &mut UnixStream, client: ClientId, session: SessionId) {
    send(
        stream,
        client,
        CommandBody::AttachSession {
            session_id: session,
            last_seen_sequence: None,
            subscriptions: vec![Subscription::SessionSummary],
            requested_role: ClientRole::Observer,
            repository: None,
        },
        "attach",
    )
    .await;
    loop {
        match read_frame(stream).await.payload {
            Payload::Catchup { .. } => break,
            Payload::Ping => continue,
            other => panic!("expected Catchup on attach, got {other:?}"),
        }
    }
}

/// Read frames until the `BlackboardItems` reply, skipping heartbeats/events.
async fn recv_blackboard_items(stream: &mut UnixStream) -> Vec<BlackboardItemView> {
    for _ in 0..16 {
        match read_frame(stream).await.payload {
            Payload::BlackboardItems { items, .. } => return items,
            Payload::CommandRejected(error) => panic!("read rejected: {}", error.code),
            Payload::Ping | Payload::Event(_) | Payload::CommandAccepted { .. } => continue,
            other => panic!("expected BlackboardItems, got {other:?}"),
        }
    }
    panic!("no BlackboardItems arrived");
}

#[tokio::test]
async fn an_observer_reads_the_board_over_the_socket_and_sees_node_authored_items() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = RuntimePaths::from_data_dir(tmp.path().to_path_buf());
    let workflow_run_id = "wfrun-observed";
    seed_board(&paths, workflow_run_id).await;

    let _daemon = spawn_daemon(tmp.path());
    let mut stream = wait_for_socket(&paths).await;
    let client = ClientId::new();
    handshake(&mut stream, client).await;

    // Become an Observer (the read carries no role gate — an Observer may read).
    let session = create_session(&mut stream, client).await;
    attach_as_observer(&mut stream, client, session).await;

    send(
        &mut stream,
        client,
        CommandBody::ReadBlackboard {
            workflow_run_id: workflow_run_id.to_string(),
            kind: Some("finding".to_string()),
            include_superseded: false,
            board_repository: None,
        },
        "read",
    )
    .await;

    let items = recv_blackboard_items(&mut stream).await;
    assert_eq!(items.len(), 1, "the seeded finding is read back");
    let item = &items[0];
    assert_eq!(item.kind, "finding");
    assert_eq!(item.workflow_run_id, workflow_run_id);
    // The author is the NODE identity the executor built server-side — never the
    // reading client. A client can never appear as an author (no post command).
    assert_eq!(
        item.author.get("node_id").and_then(|v| v.as_str()),
        Some("inspect"),
        "author is the server-built node identity, not the observer"
    );
    assert_eq!(item.confidence, Some(0.9));
}

/// The repository task board over the real socket (rubric 10) — the other half
/// of the NL-backlog vertical.
///
/// An agent's `task.create` writes through `AssemblyTaskBoardChannel` (exercised
/// here directly, because a live model is out of scope for a socket test; the
/// runtime's `a_scripted_agent_fills_the_backlog_and_moves_a_card` drives the
/// same channel through the actual agent loop). This test pins what that write
/// looks like from the *client* side: `ReadBlackboard` at board scope sees the
/// agent's cards over the wire, a Controller can move one with
/// `UpdateBlackboardItem`, and the move is a supersession that a re-read reflects.
#[tokio::test]
async fn an_agents_backlog_cards_are_readable_and_movable_over_the_socket() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = RuntimePaths::from_data_dir(tmp.path().to_path_buf());
    paths.ensure_directories().expect("create directories");
    let repository = tmp.path().join("checkout");
    let repository = repository.to_string_lossy().into_owned();

    // Seed the board exactly as the `task.create` tool does: through the
    // assembly channel, with an agent-built author, before the daemon starts.
    {
        let pool = open_pool(&paths).await;
        let board = codypendent_codypendentd::blackboard::AssemblyTaskBoardChannel::new(
            pool.clone(),
            codypendent_daemon::blackboard::BlackboardHub::new(),
        );
        for (title, status) in [
            ("wire the DAG viewer", None),
            ("column-grouped board pane", Some("doing".to_string())),
        ] {
            board
                .create(
                    &repository,
                    codypendent_runtime::blackboard::TaskCardDraft {
                        payload: json!({ "title": title }),
                        author: json!({ "role": "agent", "run_id": "run-1" }),
                        status,
                        assignee: None,
                        ordinal: None,
                    },
                )
                .await
                .expect("agent card");
        }
        pool.close().await;
    }

    let _daemon = spawn_daemon(tmp.path());
    let mut stream = wait_for_socket(&paths).await;
    let client = ClientId::new();
    handshake(&mut stream, client).await;
    let session = create_session(&mut stream, client).await;

    // An Observer may READ the board — the read carries no role gate.
    attach_as_observer(&mut stream, client, session).await;
    send(
        &mut stream,
        client,
        CommandBody::ReadBlackboard {
            workflow_run_id: String::new(),
            kind: Some("task".to_string()),
            include_superseded: false,
            board_repository: Some(repository.clone()),
        },
        "read-board",
    )
    .await;
    let cards = recv_blackboard_items(&mut stream).await;
    assert_eq!(cards.len(), 2, "the agent's cards are on the board");
    assert!(
        cards
            .iter()
            .all(|card| card.board_scope.as_deref() == Some(repository.as_str())),
        "board cards carry the repository they serve"
    );
    assert!(
        cards
            .iter()
            .all(|card| card.workflow_run_id == codypendent_protocol::board_scope_id(&repository)),
        "the board's synthetic run id is the hub key clients subscribe to"
    );
    assert!(
        cards.iter().all(|card| card.author["role"] == "agent"),
        "attribution is the agent's, built server-side"
    );
    let todo = cards
        .iter()
        .find(|card| card.status.as_deref() == Some("todo"))
        .expect("a todo card");

    // A WRITE is Controller-only: the Observer above is refused.
    send(
        &mut stream,
        client,
        CommandBody::UpdateBlackboardItem {
            scope: codypendent_protocol::BlackboardScope::RepositoryBoard {
                repository: repository.clone(),
            },
            item_id: todo.id.clone(),
            status: Some("review".to_string()),
            assignee: None,
            ordinal: None,
            payload: None,
        },
        "move-denied",
    )
    .await;
    match recv_board_write(&mut stream).await {
        Err(code) => assert_eq!(code, "protocol.role-denied"),
        Ok(item) => panic!("an Observer must not move a card, got {item:?}"),
    }

    // Re-attach as a Controller and move it for real.
    send(
        &mut stream,
        client,
        CommandBody::AttachSession {
            session_id: session,
            last_seen_sequence: None,
            subscriptions: vec![Subscription::SessionSummary],
            requested_role: ClientRole::Controller,
            repository: None,
        },
        "attach-controller",
    )
    .await;
    loop {
        match read_frame(&mut stream).await.payload {
            Payload::Catchup { .. } => break,
            Payload::Ping | Payload::Event(_) => continue,
            other => panic!("expected Catchup, got {other:?}"),
        }
    }
    send(
        &mut stream,
        client,
        CommandBody::UpdateBlackboardItem {
            scope: codypendent_protocol::BlackboardScope::RepositoryBoard {
                repository: repository.clone(),
            },
            item_id: todo.id.clone(),
            status: Some("review".to_string()),
            assignee: Some("dana".to_string()),
            ordinal: None,
            payload: None,
        },
        "move",
    )
    .await;
    let moved = recv_board_write(&mut stream)
        .await
        .expect("a Controller may move a card");
    assert_eq!(moved.status.as_deref(), Some("review"));
    assert_eq!(moved.assignee.as_deref(), Some("dana"));
    assert_eq!(moved.revision, 2, "a move is a supersession");
    assert_eq!(
        moved.payload["title"], "wire the DAG viewer",
        "a pure move carries the card body forward"
    );

    // The live board now shows the replacement, not the moved-from card.
    send(
        &mut stream,
        client,
        CommandBody::ReadBlackboard {
            workflow_run_id: String::new(),
            kind: Some("task".to_string()),
            include_superseded: false,
            board_repository: Some(repository.clone()),
        },
        "read-board-2",
    )
    .await;
    let after = recv_blackboard_items(&mut stream).await;
    assert_eq!(after.len(), 2, "a move replaces, never duplicates");
    assert!(after.iter().any(|card| card.id == moved.id));
    assert!(!after.iter().any(|card| card.id == todo.id));
}

/// Read frames until a board write's reply, returning the stored item or the
/// rejection code.
async fn recv_board_write(stream: &mut UnixStream) -> Result<BlackboardItemView, String> {
    for _ in 0..16 {
        match read_frame(stream).await.payload {
            Payload::BlackboardItemApplied { item, .. } => return Ok(item),
            Payload::CommandRejected(error) => return Err(error.code),
            Payload::Ping | Payload::Event(_) | Payload::CommandAccepted { .. } => continue,
            other => panic!("expected a board write reply, got {other:?}"),
        }
    }
    panic!("no board write reply arrived");
}
