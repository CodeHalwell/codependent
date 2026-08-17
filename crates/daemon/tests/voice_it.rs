//! Voice input over a real Unix socket (voice v1, rubric 8): `PutArtifact`
//! upload, a `SubmitUserInput` whose `InputEnvelope` carries the stored audio,
//! the transcription seam behind the classification gate, and the refusal path
//! when the operator's off-device ceiling forbids sending audio away.
//!
//! **This container has no audio hardware and no network.** Every test here
//! drives a fixture WAV (a bare 44-byte RIFF header — enough for the
//! content-addressed store, which is byte-agnostic) through a *fake*
//! [`Transcriber`]. Nothing in this file is evidence that a real microphone,
//! recorder binary, or hosted speech-to-text provider behaves as expected.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use codypendent_daemon::executor::{RunExecutor, RunLaunch};
use codypendent_daemon::transcription::{
    Transcriber, Transcription, TranscriptionFuture, TranscriptionRequest,
};
use codypendent_daemon::{db, instance, server};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::input::{
    AudioArtifact, InputBlock, InputEnvelope, InputSource, OffDevicePolicy, ScopeLevel,
    TranscriptionMode, DEFAULT_MEDIA_CLASSIFICATION,
};
use codypendent_protocol::{
    read_envelope, write_envelope, AgentMode, ArtifactRef, Catchup, ClientCapabilities,
    ClientHello, ClientId, ClientRole, Command, CommandBody, CommandId, DataClassification,
    Envelope, EventBody, ModelId, Payload, SessionId, Subscription, WorkspaceId, PROTOCOL_V1,
};
use tokio::net::UnixStream;
use tokio::task::JoinHandle;

type ServerTask = JoinHandle<anyhow::Result<()>>;

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

/// A [`Transcriber`] that returns canned text and records what it was asked to
/// transcribe. Its `mode`/`policy` are the two knobs the daemon's classification
/// gate reads, so a test picks them to exercise either side of the gate.
struct FakeTranscriber {
    mode: TranscriptionMode,
    policy: OffDevicePolicy,
    text: String,
    /// Byte lengths handed to `transcribe`, in order — empty proves the gate
    /// refused BEFORE any audio reached the transcriber.
    seen: Arc<Mutex<Vec<usize>>>,
}

impl Transcriber for FakeTranscriber {
    fn mode(&self) -> TranscriptionMode {
        self.mode
    }

    fn off_device_policy(&self) -> OffDevicePolicy {
        self.policy
    }

    fn transcribe(&self, request: TranscriptionRequest) -> TranscriptionFuture<'_> {
        self.seen
            .lock()
            .expect("seen lock")
            .push(request.bytes.len());
        let text = self.text.clone();
        Box::pin(async move {
            Ok(Transcription {
                text,
                model: Some(ModelId("whisper-large-v3-turbo".to_string())),
                duration_ms: None,
            })
        })
    }
}

/// Records launches instead of running them, and carries the transcription seam
/// into the server the way the `codypendentd` assembly does.
#[derive(Clone)]
struct VoiceExecutor {
    launches: Arc<Mutex<Vec<RunLaunch>>>,
    transcriber: Option<Arc<dyn Transcriber>>,
}

impl RunExecutor for VoiceExecutor {
    fn spawn_run(&self, launch: RunLaunch) {
        self.launches.lock().expect("launches lock").push(launch);
    }

    fn transcriber(&self) -> Option<Arc<dyn Transcriber>> {
        self.transcriber.clone()
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

async fn start_server(
    tmp: &tempfile::TempDir,
    executor: Arc<dyn RunExecutor>,
) -> (RuntimePaths, ServerTask) {
    let paths = RuntimePaths::from_data_dir(tmp.path().to_path_buf());
    paths.ensure_directories().expect("create directories");
    let pool = db::open_database(&paths.data_dir.join("codypendent.db"))
        .await
        .expect("open db");
    let boot = instance::record_boot(&pool).await.expect("record boot");
    let task = tokio::spawn(server::run_with_executor(
        pool,
        paths.clone(),
        boot,
        Some(executor),
    ));
    (paths, task)
}

async fn connect(paths: &RuntimePaths) -> UnixStream {
    loop {
        match UnixStream::connect(&paths.socket_path).await {
            Ok(stream) => break stream,
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
}

/// Send a request and read frames until the one CORRELATED to it arrives.
/// An attached connection also carries pushed session events (presence,
/// `RunStarted`, the transcription note), which interleave freely with replies.
async fn send_recv(stream: &mut UnixStream, request: &Envelope) -> Envelope {
    write_envelope(stream, request).await.expect("write frame");
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), read_envelope(stream))
            .await
            .expect("read timed out")
            .expect("read frame")
            .expect("server must reply");
        if frame.correlation_id == Some(request.message_id) {
            return frame;
        }
    }
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
        client_name: "voice-it".to_string(),
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
        "expected ServerHello, got {:?}",
        reply.payload
    );
}

/// Create a session and attach to it in `role`, returning the session id.
async fn open_session(
    stream: &mut UnixStream,
    client_id: ClientId,
    role: ClientRole,
    key: &str,
) -> SessionId {
    let reply = send_recv(
        stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::CreateSession {
                    workspace: WorkspaceId::new(),
                    title: "voice".to_string(),
                    repository: None,
                    internal: false,
                    parent_session_id: None,
                    parent_run_id: None,
                },
                &format!("{key}-create"),
            )),
        ),
    )
    .await;
    let session_id = reply
        .session_id
        .expect("CreateSession reply carries the created session id");
    let reply = send_recv(
        stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::AttachSession {
                    session_id,
                    last_seen_sequence: None,
                    subscriptions: vec![Subscription::SessionSummary],
                    requested_role: role,
                    repository: None,
                },
                &format!("{key}-attach"),
            )),
        ),
    )
    .await;
    assert!(
        matches!(reply.payload, Payload::Catchup { .. }),
        "attach must succeed, got {:?}",
        reply.payload
    );
    session_id
}

/// A bare 44-byte RIFF/WAVE header, no samples. NOT a recording — this
/// container has no capture device; the store only cares that bytes exist.
fn fixture_wav() -> Vec<u8> {
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&36u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&32_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&0u32.to_le_bytes());
    wav
}

/// Upload the fixture WAV and return the minted ref.
async fn put_fixture(stream: &mut UnixStream, client_id: ClientId, key: &str) -> ArtifactRef {
    let reply = send_recv(
        stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::PutArtifact {
                    media_type: "audio/wav".to_string(),
                    bytes_base64: base64::engine::general_purpose::STANDARD.encode(fixture_wav()),
                    sensitivity: DEFAULT_MEDIA_CLASSIFICATION,
                },
                key,
            )),
        ),
    )
    .await;
    match reply.payload {
        Payload::ArtifactStored { artifact, .. } => artifact,
        other => panic!("expected ArtifactStored, got {other:?}"),
    }
}

fn voice_envelope(original: ArtifactRef) -> InputEnvelope {
    InputEnvelope {
        source: InputSource::Voice,
        blocks: vec![InputBlock::Audio(AudioArtifact {
            original,
            transcript: None,
            duration_ms: Some(4_000),
            sample_rate_hz: Some(16_000),
        })],
        scope: ScopeLevel::Session,
        attachments: Vec::new(),
    }
}

fn fake(
    mode: TranscriptionMode,
    policy: OffDevicePolicy,
    text: &str,
) -> (Arc<dyn Transcriber>, Arc<Mutex<Vec<usize>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let transcriber = Arc::new(FakeTranscriber {
        mode,
        policy,
        text: text.to_string(),
        seen: seen.clone(),
    });
    (transcriber, seen)
}

fn executor(transcriber: Option<Arc<dyn Transcriber>>) -> (Arc<VoiceExecutor>, VoiceExecutor) {
    let executor = VoiceExecutor {
        launches: Arc::new(Mutex::new(Vec::new())),
        transcriber,
    };
    (Arc::new(executor.clone()), executor)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_voice_note_is_uploaded_transcribed_and_becomes_the_runs_objective() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (transcriber, seen) = fake(
        // Local transcription is permitted under ANY ceiling.
        TranscriptionMode::Local,
        OffDevicePolicy::restrictive(),
        "fix the flaky test",
    );
    let (injected, handle) = executor(Some(transcriber));
    let (paths, task) = start_server(&tmp, injected).await;

    let mut stream = connect(&paths).await;
    let client_id = ClientId::new();
    handshake(&mut stream, client_id).await;
    let session_id = open_session(&mut stream, client_id, ClientRole::Controller, "voice").await;

    // 1. Upload the captured audio; the daemon mints a content-addressed ref.
    let stored = put_fixture(&mut stream, client_id, "put-1").await;
    assert_eq!(stored.media_type, "audio/wav");
    assert_eq!(stored.byte_length, fixture_wav().len() as u64);
    assert_eq!(
        stored.sensitivity,
        DataClassification::Confidential,
        "captured media defaults to Confidential so it cannot leave by accident"
    );

    // 2. Submit it with NO text — the push-to-talk shape.
    let reply = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::SubmitUserInput {
                    session_id,
                    text: String::new(),
                    mode: AgentMode::Build,
                    model: None,
                    envelope: Some(voice_envelope(stored.clone())),
                },
                "voice-input",
            )),
        ),
    )
    .await;
    let created_run = match reply.payload {
        Payload::CommandAccepted { created_run, .. } => {
            created_run.expect("a follow-up launches its own run")
        }
        other => panic!("expected CommandAccepted, got {other:?}"),
    };

    // The transcriber saw exactly the bytes the store holds.
    assert_eq!(*seen.lock().expect("seen lock"), vec![fixture_wav().len()]);

    // 3. The TRANSCRIPT is what the run executes.
    let launches = handle.launches.lock().expect("launches lock");
    assert_eq!(launches.len(), 1, "one run launched");
    assert_eq!(launches[0].objective, "fix the flaky test");
    assert_eq!(launches[0].run_id, created_run);
    drop(launches);

    task.abort();
}

#[tokio::test]
async fn a_retried_voice_command_replays_without_retranscribing_or_relaunching() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (transcriber, seen) = fake(
        TranscriptionMode::Local,
        OffDevicePolicy::restrictive(),
        "retry-safe transcript",
    );
    let (injected, handle) = executor(Some(transcriber));
    let (paths, task) = start_server(&tmp, injected).await;

    let mut stream = connect(&paths).await;
    let client_id = ClientId::new();
    handshake(&mut stream, client_id).await;
    let session_id = open_session(&mut stream, client_id, ClientRole::Controller, "voice").await;
    let stored = put_fixture(&mut stream, client_id, "put-once").await;
    let body = CommandBody::SubmitUserInput {
        session_id,
        text: String::new(),
        mode: AgentMode::Build,
        model: None,
        envelope: Some(voice_envelope(stored)),
    };

    let first = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(body.clone(), "voice-retry-key")),
        ),
    )
    .await;
    let second = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(body, "voice-retry-key")),
        ),
    )
    .await;
    let accepted = |payload: Payload| match payload {
        Payload::CommandAccepted {
            command_id,
            created_run,
            ..
        } => (command_id, created_run),
        other => panic!("expected CommandAccepted, got {other:?}"),
    };
    assert_eq!(accepted(first.payload), accepted(second.payload));
    assert_eq!(seen.lock().expect("seen lock").len(), 1);
    assert_eq!(handle.launches.lock().expect("launches lock").len(), 1);

    task.abort();
}

#[tokio::test]
async fn retried_artifact_upload_returns_the_original_ref_and_command_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (injected, _handle) = executor(None);
    let (paths, task) = start_server(&tmp, injected).await;

    let mut stream = connect(&paths).await;
    let client_id = ClientId::new();
    handshake(&mut stream, client_id).await;
    open_session(&mut stream, client_id, ClientRole::Controller, "voice").await;
    let body = CommandBody::PutArtifact {
        media_type: "audio/wav".to_string(),
        bytes_base64: base64::engine::general_purpose::STANDARD.encode(fixture_wav()),
        sensitivity: DEFAULT_MEDIA_CLASSIFICATION,
    };
    let first_request = command(body.clone(), "artifact-retry-key");
    let first_command_id = first_request.command_id;
    let first = send_recv(
        &mut stream,
        &Envelope::request(client_id, Payload::Command(first_request)),
    )
    .await;
    let second = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(body, "artifact-retry-key")),
        ),
    )
    .await;
    let stored = |payload: Payload| match payload {
        Payload::ArtifactStored {
            command_id,
            artifact,
        } => (command_id, artifact),
        other => panic!("expected ArtifactStored, got {other:?}"),
    };
    let first = stored(first.payload);
    let second = stored(second.payload);
    assert_eq!(first.0, first_command_id);
    assert_eq!(first, second, "retry must replay the original occurrence");

    let conflict = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::PutArtifact {
                    media_type: "audio/wav".to_string(),
                    bytes_base64: base64::engine::general_purpose::STANDARD
                        .encode(b"different bytes"),
                    sensitivity: DEFAULT_MEDIA_CLASSIFICATION,
                },
                "artifact-retry-key",
            )),
        ),
    )
    .await;
    assert!(matches!(
        conflict.payload,
        Payload::CommandRejected(ref error) if error.code == "artifact.idempotency-conflict"
    ));

    task.abort();
}

#[tokio::test]
async fn a_transcription_appends_a_note_naming_the_duration_and_model() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (transcriber, _) = fake(
        TranscriptionMode::Local,
        OffDevicePolicy::restrictive(),
        "ship it",
    );
    let (injected, _handle) = executor(Some(transcriber));
    let (paths, task) = start_server(&tmp, injected).await;

    let mut stream = connect(&paths).await;
    let client_id = ClientId::new();
    handshake(&mut stream, client_id).await;
    let session_id = open_session(&mut stream, client_id, ClientRole::Controller, "voice").await;
    let stored = put_fixture(&mut stream, client_id, "put-1").await;

    send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::SubmitUserInput {
                    session_id,
                    text: String::new(),
                    mode: AgentMode::Build,
                    model: None,
                    envelope: Some(voice_envelope(stored)),
                },
                "voice-input",
            )),
        ),
    )
    .await;

    // Re-attach from scratch and read the ledger back: the note is durable,
    // not merely broadcast.
    let mut reader = connect(&paths).await;
    let reader_id = ClientId::new();
    handshake(&mut reader, reader_id).await;
    let reply = send_recv(
        &mut reader,
        &Envelope::request(
            reader_id,
            Payload::Command(command(
                CommandBody::AttachSession {
                    session_id,
                    last_seen_sequence: Some(0),
                    subscriptions: vec![Subscription::SessionSummary],
                    requested_role: ClientRole::Observer,
                    repository: None,
                },
                "reader-attach",
            )),
        ),
    )
    .await;
    let Payload::Catchup {
        catchup: Catchup::Events { events, .. },
    } = reply.payload
    else {
        panic!("expected an event catch-up");
    };
    let note = events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::NoteAppended { text, run_id } if text.starts_with("transcribed") => {
                Some((text.clone(), *run_id))
            }
            _ => None,
        })
        .expect("a transcription note is on the ledger");
    assert_eq!(
        note.0,
        "transcribed 4.0 s of audio (model whisper-large-v3-turbo)"
    );
    assert!(
        note.1.is_some(),
        "the note is run-scoped so it lands on the right transcript"
    );

    task.abort();
}

#[tokio::test]
async fn a_restrictive_ceiling_refuses_to_send_audio_off_device() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (transcriber, seen) = fake(
        // A hosted transcriber under a ceiling that stops at Internal, while
        // captured media defaults to Confidential.
        TranscriptionMode::Remote,
        OffDevicePolicy::restrictive(),
        "must never be produced",
    );
    let (injected, handle) = executor(Some(transcriber));
    let (paths, task) = start_server(&tmp, injected).await;

    let mut stream = connect(&paths).await;
    let client_id = ClientId::new();
    handshake(&mut stream, client_id).await;
    let session_id = open_session(&mut stream, client_id, ClientRole::Controller, "voice").await;
    let stored = put_fixture(&mut stream, client_id, "put-1").await;

    let reply = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::SubmitUserInput {
                    session_id,
                    text: String::new(),
                    mode: AgentMode::Build,
                    model: None,
                    envelope: Some(voice_envelope(stored)),
                },
                "voice-input",
            )),
        ),
    )
    .await;
    match reply.payload {
        Payload::CommandRejected(error) => {
            assert_eq!(error.code, "voice.off-device-forbidden");
            assert!(!error.retryable, "a policy refusal is not retryable");
        }
        other => panic!("expected a classification refusal, got {other:?}"),
    }
    assert!(
        seen.lock().expect("seen lock").is_empty(),
        "the gate must refuse BEFORE any audio reaches the transcriber"
    );
    assert!(
        handle.launches.lock().expect("launches lock").is_empty(),
        "a refused submission launches no run"
    );

    task.abort();
}

#[tokio::test]
async fn a_permissive_ceiling_admits_remote_transcription() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (transcriber, seen) = fake(
        TranscriptionMode::Remote,
        OffDevicePolicy::permissive(),
        "run the tests",
    );
    let (injected, handle) = executor(Some(transcriber));
    let (paths, task) = start_server(&tmp, injected).await;

    let mut stream = connect(&paths).await;
    let client_id = ClientId::new();
    handshake(&mut stream, client_id).await;
    let session_id = open_session(&mut stream, client_id, ClientRole::Controller, "voice").await;
    let stored = put_fixture(&mut stream, client_id, "put-1").await;

    let reply = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::SubmitUserInput {
                    session_id,
                    text: String::new(),
                    mode: AgentMode::Build,
                    model: None,
                    envelope: Some(voice_envelope(stored)),
                },
                "voice-input",
            )),
        ),
    )
    .await;
    assert!(
        matches!(reply.payload, Payload::CommandAccepted { .. }),
        "Confidential audio is within a permissive ceiling, got {:?}",
        reply.payload
    );
    assert_eq!(seen.lock().expect("seen lock").len(), 1);
    assert_eq!(
        handle.launches.lock().expect("launches lock")[0].objective,
        "run the tests"
    );

    task.abort();
}

#[tokio::test]
async fn a_daemon_with_no_transcriber_refuses_audio_but_still_serves_text() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (injected, handle) = executor(None);
    let (paths, task) = start_server(&tmp, injected).await;

    let mut stream = connect(&paths).await;
    let client_id = ClientId::new();
    handshake(&mut stream, client_id).await;
    let session_id = open_session(&mut stream, client_id, ClientRole::Controller, "voice").await;
    let stored = put_fixture(&mut stream, client_id, "put-1").await;

    let reply = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::SubmitUserInput {
                    session_id,
                    text: String::new(),
                    mode: AgentMode::Build,
                    model: None,
                    envelope: Some(voice_envelope(stored)),
                },
                "voice-input",
            )),
        ),
    )
    .await;
    match reply.payload {
        Payload::CommandRejected(error) => {
            assert_eq!(error.code, "voice.transport-unavailable");
            assert!(error.retryable, "configuring a transcriber makes it work");
        }
        other => panic!("expected voice.transport-unavailable, got {other:?}"),
    }

    // The ordinary text path is completely unaffected by the missing seam.
    let reply = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::SubmitUserInput {
                    session_id,
                    text: "just type it".to_string(),
                    mode: AgentMode::Build,
                    model: None,
                    envelope: None,
                },
                "text-input",
            )),
        ),
    )
    .await;
    assert!(
        matches!(reply.payload, Payload::CommandAccepted { .. }),
        "plain text still works, got {:?}",
        reply.payload
    );
    assert_eq!(
        handle.launches.lock().expect("launches lock")[0].objective,
        "just type it"
    );

    task.abort();
}

#[tokio::test]
async fn an_observer_may_not_upload_artifacts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (transcriber, _) = fake(
        TranscriptionMode::Local,
        OffDevicePolicy::restrictive(),
        "unused",
    );
    let (injected, _handle) = executor(Some(transcriber));
    let (paths, task) = start_server(&tmp, injected).await;

    let mut stream = connect(&paths).await;
    let client_id = ClientId::new();
    handshake(&mut stream, client_id).await;
    open_session(&mut stream, client_id, ClientRole::Observer, "voice").await;

    let reply = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::PutArtifact {
                    media_type: "audio/wav".to_string(),
                    bytes_base64: base64::engine::general_purpose::STANDARD.encode(fixture_wav()),
                    sensitivity: DEFAULT_MEDIA_CLASSIFICATION,
                },
                "observer-put",
            )),
        ),
    )
    .await;
    match reply.payload {
        Payload::CommandRejected(error) => assert_eq!(error.code, "artifact.role-denied"),
        other => panic!("expected artifact.role-denied, got {other:?}"),
    }

    task.abort();
}

#[tokio::test]
async fn malformed_upload_bytes_are_refused_legibly() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (injected, _handle) = executor(None);
    let (paths, task) = start_server(&tmp, injected).await;

    let mut stream = connect(&paths).await;
    let client_id = ClientId::new();
    handshake(&mut stream, client_id).await;
    open_session(&mut stream, client_id, ClientRole::Controller, "voice").await;

    let reply = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::PutArtifact {
                    media_type: "audio/wav".to_string(),
                    bytes_base64: "not base64!!".to_string(),
                    sensitivity: DEFAULT_MEDIA_CLASSIFICATION,
                },
                "bad-put",
            )),
        ),
    )
    .await;
    match reply.payload {
        Payload::CommandRejected(error) => {
            assert_eq!(error.code, "artifact.malformed-base64");
            assert!(!error.retryable);
        }
        other => panic!("expected artifact.malformed-base64, got {other:?}"),
    }

    task.abort();
}

#[tokio::test]
async fn identical_uploads_share_a_blob_but_never_a_classification() {
    // The store's RULE 1: the same bytes dedup to one blob, but each occurrence
    // is its own ref with its own id and classification. Uploading the same WAV
    // twice — once Confidential, once Secret — must NOT let the second inherit
    // the first's laxer classification, or the gate could be walked around.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (injected, _handle) = executor(None);
    let (paths, task) = start_server(&tmp, injected).await;

    let mut stream = connect(&paths).await;
    let client_id = ClientId::new();
    handshake(&mut stream, client_id).await;
    open_session(&mut stream, client_id, ClientRole::Controller, "voice").await;

    let first = put_fixture(&mut stream, client_id, "put-1").await;
    let reply = send_recv(
        &mut stream,
        &Envelope::request(
            client_id,
            Payload::Command(command(
                CommandBody::PutArtifact {
                    media_type: "audio/wav".to_string(),
                    bytes_base64: base64::engine::general_purpose::STANDARD.encode(fixture_wav()),
                    sensitivity: DataClassification::Secret,
                },
                "put-2",
            )),
        ),
    )
    .await;
    let second = match reply.payload {
        Payload::ArtifactStored { artifact, .. } => artifact,
        other => panic!("expected ArtifactStored, got {other:?}"),
    };

    assert_eq!(first.sha256, second.sha256, "identical bytes, one blob");
    assert_ne!(first.id, second.id, "each occurrence is its own ref");
    assert_eq!(first.sensitivity, DataClassification::Confidential);
    assert_eq!(
        second.sensitivity,
        DataClassification::Secret,
        "a later occurrence never inherits an earlier row's classification"
    );

    task.abort();
}
