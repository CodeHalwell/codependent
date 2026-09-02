//! The real-crash recovery test (STEP 1.14), living in the crate that builds the
//! `codypendentd` binary so `CARGO_BIN_EXE_codypendentd` is defined.
//!
//! It spawns the actual daemon binary against a temp data dir, creates a run over
//! the socket and parks it (`PauseRun` — a live state), `kill -9`s the child,
//! restarts it, and asserts the paused run remains resumable rather than being
//! terminalized — exercising the assembly binary's `main.rs` startup wiring end
//! to end. Resuming it then drives the run to its ordinary terminal outcome.
//!
//! With the run executor now wired in (this crate injects it), an accepted
//! `StartRun` also begins executing immediately; in a bare data dir with no
//! `models.toml` the resumed run fails cleanly on its own. The important recovery
//! contract is that restart itself preserves the parked `Paused` projection and
//! does not fabricate a terminal event.

use std::path::Path;
use std::process::{Child, Command as StdCommand, Stdio};
use std::str::FromStr;
use std::time::Duration;

use codypendent_daemon::{db, ledger, projections};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{
    read_envelope, write_envelope, AgentMode, ClientCapabilities, ClientHello, ClientId,
    Command as ProtoCommand, CommandBody, CommandId, Envelope, EventBody, Payload, RunId, RunState,
    SessionId, WorkspaceId, PROTOCOL_V1,
};
use sqlx::SqlitePool;
use tokio::net::UnixStream;

/// Spawn the `codypendentd` binary against a temp data dir, with a quiet log and
/// discarded output. The socket resolves under `<data_dir>/run/` (the data-dir
/// override branch of discovery), matching what the test connects to.
fn spawn_daemon(data_dir: &Path) -> Child {
    StdCommand::new(env!("CARGO_BIN_EXE_codypendentd"))
        .env("CODYPENDENT_DATA_DIR", data_dir)
        .env_remove("CODYPENDENT_SOCKET")
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn codypendentd")
}

/// Poll until the daemon SERVES a connection — connected and handshaken — or
/// panic after ~60s.
///
/// Connecting is not on its own proof of readiness. A Unix socket accepts into
/// the kernel's backlog the moment the listener exists, so a connection made
/// while the daemon is still finishing recovery can be dropped underneath the
/// first write. That surfaces as a broken pipe on the hello frame, which is how
/// this test went red on a loaded CI runner minutes after passing locally on
/// the same commit. A connection that does not answer is NOT READY, so it is
/// retried with a fresh one rather than failing the test.
///
/// 60s is a startup detector, not a latency assertion: spawning a daemon is
/// fast idle and slow on a machine saturated by the rest of the suite.
async fn wait_for_socket(paths: &RuntimePaths, client: ClientId) -> UnixStream {
    for _ in 0..1200 {
        if let Ok(mut stream) = UnixStream::connect(&paths.socket_path).await {
            if handshaken(&mut stream, client).await {
                return stream;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "daemon never served a connection at {}",
        paths.socket_path.display()
    );
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

/// Handshake, reporting whether the daemon answered with a `ServerHello`. A
/// handshaken local connection defaults to the `Controller` role, so no
/// explicit attach is needed to create/control.
///
/// Returns false rather than panicking: to the caller above, a connection the
/// daemon never served is indistinguishable from one it has not served YET, and
/// the difference is decided by retrying, not by asserting.
async fn handshaken(stream: &mut UnixStream, client: ClientId) -> bool {
    let hello = ClientHello {
        client_name: "recovery-it".to_string(),
        client_version: "0".to_string(),
        supported_protocols: vec![PROTOCOL_V1],
        capabilities: ClientCapabilities::default(),
        resume_token: None,
    };
    if write_envelope(
        stream,
        &Envelope::request(client, Payload::ClientHello(hello)),
    )
    .await
    .is_err()
    {
        return false;
    }
    // `Ok(None)` is a clean EOF — the daemon closed the connection without
    // answering, which is the very case this retries rather than asserts on.
    matches!(
        tokio::time::timeout(Duration::from_secs(30), read_envelope(stream)).await,
        Ok(Ok(Some(envelope))) if matches!(envelope.payload, Payload::ServerHello(_))
    )
}

/// Send one command and return the first non-heartbeat reply envelope.
async fn send_command(
    stream: &mut UnixStream,
    client: ClientId,
    body: CommandBody,
    key: &str,
) -> Envelope {
    let cmd = ProtoCommand {
        command_id: CommandId::new(),
        idempotency_key: key.to_string(),
        expected_revision: None,
        body,
    };
    write_envelope(stream, &Envelope::request(client, Payload::Command(cmd)))
        .await
        .expect("write command");
    loop {
        let env = read_frame(stream).await;
        if matches!(env.payload, Payload::Ping) {
            continue;
        }
        return env;
    }
}

async fn open_pool(paths: &RuntimePaths) -> SqlitePool {
    db::open_database(&paths.data_dir.join("codypendent.db"))
        .await
        .expect("open db")
}

/// Poll (against a short-lived read pool) for the single run in `session`.
async fn wait_for_run(paths: &RuntimePaths, session: SessionId) -> RunId {
    let pool = open_pool(paths).await;
    let mut found = None;
    for _ in 0..100 {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT id FROM runs WHERE session_id = ? LIMIT 1")
                .bind(session.to_string())
                .fetch_optional(&pool)
                .await
                .unwrap();
        if let Some((id,)) = row {
            found = Some(RunId::from_str(&id).unwrap());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    pool.close().await;
    found.expect("run row appeared")
}

#[tokio::test]
async fn kill9_daemon_preserves_then_resumes_a_parked_run() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();
    let paths = RuntimePaths::from_data_dir(data_dir.clone());

    // Boot the real daemon and drive it over the socket.
    let mut child = spawn_daemon(&data_dir);
    let client = ClientId::new();
    let mut stream = wait_for_socket(&paths, client).await;

    // Create a session (its id rides back on the envelope), start a run, then
    // pause it. A restart must retain that deliberate operator decision rather
    // than converting it into an infrastructure failure.
    let create = send_command(
        &mut stream,
        client,
        CommandBody::CreateSession {
            workspace: WorkspaceId::new(),
            title: "diagnose".to_string(),
            repository: None,
            internal: false,
            parent_session_id: None,
            parent_run_id: None,
        },
        "create",
    )
    .await;
    let session = create.session_id.expect("created session id on envelope");

    let started = send_command(
        &mut stream,
        client,
        CommandBody::StartRun {
            session_id: session,
            objective: "diagnose".to_string(),
            mode: AgentMode::Build,
            repository: None,
            model: None,
        },
        "start",
    )
    .await;
    assert!(matches!(started.payload, Payload::CommandAccepted { .. }));

    let run = wait_for_run(&paths, session).await;
    let paused = send_command(
        &mut stream,
        client,
        CommandBody::PauseRun { run_id: run },
        "pause",
    )
    .await;
    assert!(matches!(paused.payload, Payload::CommandAccepted { .. }));
    drop(stream);

    // Crash the daemon uncleanly.
    let _ = child.kill();
    let _ = child.wait();

    // Restart: recovery runs before the socket reopens.
    let mut child2 = spawn_daemon(&data_dir);
    let client2 = ClientId::new();
    let mut stream2 = wait_for_socket(&paths, client2).await;

    // Restart recovery keeps the run paused and emits no terminal completion.
    let pool = open_pool(&paths).await;
    let mut recovered_state = None;
    for _ in 0..100 {
        if let Some(state) = projections::load_run_state(&pool, run).await.unwrap() {
            if state == RunState::Paused {
                recovered_state = Some(state);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        recovered_state,
        Some(RunState::Paused),
        "restart must preserve an explicitly paused run"
    );

    let events_before_resume = ledger::load_events(&pool, session).await.unwrap();
    assert!(
        !events_before_resume.iter().any(|event| matches!(
            &event.body,
            EventBody::RunCompleted { run_id, .. } if *run_id == run
        )),
        "restart must not fabricate terminal completion for a paused run"
    );

    // The recovered run remains actionable. Resume must durably move it out of
    // Paused; its eventual model-dependent outcome is outside this crash test.
    let resumed = send_command(
        &mut stream2,
        client2,
        CommandBody::ResumeRun { run_id: run },
        "resume",
    )
    .await;
    assert!(matches!(resumed.payload, Payload::CommandAccepted { .. }));

    let mut resumed_state = None;
    for _ in 0..100 {
        if let Some(state) = projections::load_run_state(&pool, run).await.unwrap() {
            if state != RunState::Paused {
                resumed_state = Some(state);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        resumed_state.is_some(),
        "the recovered run must leave Paused"
    );

    let events = ledger::load_events(&pool, session).await.unwrap();
    assert!(
        events.iter().any(|e| matches!(
            &e.body,
            EventBody::RunStateChanged { run_id, state: RunState::Running } if *run_id == run
        )),
        "resuming the recovered run must be durable"
    );
    pool.close().await;

    let _ = child2.kill();
    let _ = child2.wait();
}
