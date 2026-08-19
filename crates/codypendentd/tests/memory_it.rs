//! The client-facing memory triad (outcome 17) over the **real** `codypendentd`
//! socket: inspect, correct, forget, and open-the-source.
//!
//! Chapter 06 promises a user can see, edit, and delete what the fabric
//! remembers. Until this landed, `crates/protocol/src/command.rs` had no memory
//! command at all: the store's `correct`/`forget`/`forget_scope` had zero
//! production callers and the only memory surface read SQLite directly,
//! read-only. This exercises the vertical against the actual daemon process —
//! the assembly's wired `MemoryGateway`, the daemon's connection-level dispatch,
//! and the `MemoryView` projection.
//!
//! It also pins the property that makes the triad safe to expose: a memory
//! outside the caller's visible scopes is refused **identically** to one that
//! never existed, at the *fetch*, for the evidence behind it as well as for the
//! record itself.

use std::path::Path;
use std::process::{Child, Command as StdCommand, Stdio};
use std::time::Duration;

use codypendent_daemon::db;
use codypendent_knowledge::{
    CandidateMemory, Curation, EvidenceRef, MemoryClass, MemoryStore, Revision, Scope,
};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{
    read_envelope, write_envelope, ClientCapabilities, ClientHello, ClientId, Command, CommandBody,
    CommandId, DataClassification, Envelope, MemoryEvidence, MemoryId, MemoryScopeTier, MemoryView,
    Payload, RepositoryId, SessionId, PROTOCOL_V1,
};
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
        client_name: "memory-it".to_string(),
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

/// The reply to a memory command, with heartbeats and stray events skipped.
enum MemoryReply {
    Memory(Box<MemoryView>),
    Forgotten(Vec<MemoryId>),
    Evidence(MemoryEvidence),
    Rejected(String),
}

async fn recv_memory_reply(stream: &mut UnixStream) -> MemoryReply {
    for _ in 0..16 {
        match read_frame(stream).await.payload {
            Payload::Memory { memory, .. } => return MemoryReply::Memory(Box::new(memory)),
            Payload::MemoryForgotten { forgotten, .. } => return MemoryReply::Forgotten(forgotten),
            Payload::MemoryEvidence { evidence, .. } => return MemoryReply::Evidence(evidence),
            Payload::CommandRejected(error) => return MemoryReply::Rejected(error.code),
            Payload::Ping | Payload::Event(_) | Payload::CommandAccepted { .. } => continue,
            other => panic!("expected a memory reply, got {other:?}"),
        }
    }
    panic!("no memory reply arrived");
}

async fn open_pool(paths: &RuntimePaths) -> SqlitePool {
    db::open_database(&paths.data_dir.join("codypendent.db"))
        .await
        .expect("open db")
}

/// Curate one memory in `scope` before the daemon starts, so it opens a database
/// that already holds it. Returns its id.
async fn seed_memory(paths: &RuntimePaths, scope: Scope, statement: &str) -> MemoryId {
    paths.ensure_directories().expect("create directories");
    let pool = open_pool(paths).await;
    let candidate = CandidateMemory {
        class: MemoryClass::Semantic,
        scope: Some(scope),
        statement: statement.to_string(),
        structured_value: None,
        provenance: vec![EvidenceRef::EventRange {
            session_id: SessionId::new(),
            from_sequence: 1,
            to_sequence: 1,
        }],
        confidence: 0.8,
        observed_at: chrono::Utc::now(),
        valid_from: Revision::sequence(1),
        sensitivity: DataClassification::Internal,
        retention: None,
    };
    let id = match MemoryStore::new()
        .curate(&pool, candidate)
        .await
        .expect("curate")
    {
        Curation::Accepted(record) => record.id,
        other => panic!("expected the seed memory to be accepted, got {other:?}"),
    };
    pool.close().await;
    id
}

/// The daemon derives a repository identity from the path the command names, so
/// a memory seeded under that identity is the one a caller naming that path can
/// see — and nothing else is.
fn repository_identity(root: &Path) -> RepositoryId {
    codypendent_codypendentd::scan::repository_id_for(root)
}

#[tokio::test]
async fn a_client_inspects_corrects_and_forgets_its_own_repositorys_memory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("data");
    let repository = tmp.path().join("checkout");
    std::fs::create_dir_all(&repository).expect("checkout dir");
    let repository_arg = repository.to_string_lossy().into_owned();
    let paths = RuntimePaths::from_data_dir(data_dir.clone());

    let mine = seed_memory(
        &paths,
        Scope::Repository(repository_identity(&repository)),
        "the parser is generated from grammar.pest",
    )
    .await;
    // A second checkout's memory, which this client must never be able to see.
    let theirs = seed_memory(
        &paths,
        Scope::Repository(RepositoryId::new()),
        "another checkout's secret",
    )
    .await;

    let _daemon = spawn_daemon(&data_dir);
    let mut stream = wait_for_socket(&paths).await;
    let client = ClientId::new();
    handshake(&mut stream, client).await;

    // 1. Inspect: the memory this checkout owns comes back with its statement.
    send(
        &mut stream,
        client,
        CommandBody::InspectMemory {
            id: mine,
            repository: repository_arg.clone(),
        },
        "inspect-mine",
    )
    .await;
    let view = match recv_memory_reply(&mut stream).await {
        MemoryReply::Memory(view) => *view,
        MemoryReply::Rejected(code) => panic!("inspect rejected: {code}"),
        _ => panic!("expected a memory"),
    };
    assert_eq!(view.id, mine);
    assert_eq!(view.statement, "the parser is generated from grammar.pest");
    assert_eq!(view.scope.tier, "repository");
    assert_eq!(view.evidence.len(), 1, "the provenance ref is addressable");

    // 2. The no-oracle property, over the wire: another checkout's memory and a
    //    memory that never existed produce the SAME rejection code.
    send(
        &mut stream,
        client,
        CommandBody::InspectMemory {
            id: theirs,
            repository: repository_arg.clone(),
        },
        "inspect-theirs",
    )
    .await;
    let out_of_scope = match recv_memory_reply(&mut stream).await {
        MemoryReply::Rejected(code) => code,
        _ => panic!("another checkout's memory must not be readable"),
    };
    send(
        &mut stream,
        client,
        CommandBody::InspectMemory {
            id: MemoryId::new(),
            repository: repository_arg.clone(),
        },
        "inspect-absent",
    )
    .await;
    let absent = match recv_memory_reply(&mut stream).await {
        MemoryReply::Rejected(code) => code,
        _ => panic!("an absent memory must not be readable"),
    };
    assert_eq!(out_of_scope, "memory.not-found");
    assert_eq!(out_of_scope, absent);

    // 3. …and the gate holds where the EVIDENCE is fetched, not only where the
    //    record is. Naming an invisible memory must not yield its source.
    send(
        &mut stream,
        client,
        CommandBody::OpenMemoryEvidence {
            id: theirs,
            repository: repository_arg.clone(),
            evidence_index: 0,
        },
        "evidence-theirs",
    )
    .await;
    match recv_memory_reply(&mut stream).await {
        MemoryReply::Rejected(code) => assert_eq!(code, "memory.not-found"),
        _ => panic!("evidence behind an invisible memory must stay invisible"),
    }

    // 4. Correct: a NEW record that supersedes the old one, with evidence the
    //    daemon supplied (the edit's own receipt), not the caller.
    send(
        &mut stream,
        client,
        CommandBody::CorrectMemory {
            id: mine,
            repository: repository_arg.clone(),
            statement: "the parser is hand-written".to_string(),
            structured_value: None,
            confidence: 0.9,
        },
        "correct-mine",
    )
    .await;
    let corrected = match recv_memory_reply(&mut stream).await {
        MemoryReply::Memory(view) => *view,
        MemoryReply::Rejected(code) => panic!("correct rejected: {code}"),
        _ => panic!("expected the corrected memory"),
    };
    assert_ne!(
        corrected.id, mine,
        "a correction supersedes, never overwrites"
    );
    assert_eq!(corrected.supersedes, vec![mine]);

    // 5. Open the correction's source: real bytes, not an opaque id string.
    send(
        &mut stream,
        client,
        CommandBody::OpenMemoryEvidence {
            id: corrected.id,
            repository: repository_arg.clone(),
            evidence_index: 0,
        },
        "evidence-correction",
    )
    .await;
    match recv_memory_reply(&mut stream).await {
        MemoryReply::Evidence(MemoryEvidence::Artifact {
            media_type,
            bytes_base64,
        }) => {
            assert_eq!(media_type, "application/json");
            assert!(!bytes_base64.is_empty(), "the receipt has real content");
        }
        other => panic!(
            "expected the correction receipt artifact, got a different reply: {}",
            match other {
                MemoryReply::Rejected(code) => code,
                _ => "non-artifact".to_string(),
            }
        ),
    }

    // 6. Forget the repository tier: mine goes, the other checkout's survives.
    send(
        &mut stream,
        client,
        CommandBody::ForgetMemoryScope {
            repository: repository_arg,
            tier: MemoryScopeTier::Repository,
        },
        "forget-scope",
    )
    .await;
    let forgotten = match recv_memory_reply(&mut stream).await {
        MemoryReply::Forgotten(ids) => ids,
        MemoryReply::Rejected(code) => panic!("forget rejected: {code}"),
        _ => panic!("expected a forget audit"),
    };
    // The correction's SUPERSEDED predecessor goes too. A right to forget that
    // left the original statement in the history table would forget nothing.
    let mut forgotten = forgotten;
    forgotten.sort();
    let mut expected = vec![mine, corrected.id];
    expected.sort();
    assert_eq!(forgotten, expected);

    let pool = open_pool(&paths).await;
    assert!(
        MemoryStore::new()
            .get(&pool, corrected.id)
            .await
            .expect("get")
            .is_none(),
        "the forgotten memory is really gone"
    );
    assert!(
        MemoryStore::new()
            .get(&pool, theirs)
            .await
            .expect("get")
            .is_some(),
        "another checkout's memory is untouched by a scoped forget"
    );
    pool.close().await;
}
