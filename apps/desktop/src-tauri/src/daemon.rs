//! The desktop client's connection to `codypendentd`.
//!
//! This is the whole reason the shell has a Rust side (adoption 14 §4.1): a
//! webview cannot open a Unix domain socket, so the connection lives here and
//! the webview only ever sees JSON the shared protocol crate produced. The
//! framing, handshake, and command envelopes are NOT reimplemented — they come
//! from `codypendent_council::connection::Connection`, the same reference
//! client `codypendent run` and the TUI use.
//!
//! Everything in this module is deliberately free of `tauri` types: the sink
//! events are pushed into is a trait, so the transport is exercised in tests
//! against a real Unix socket without a webview (see the tests at the bottom).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use codypendent_council::connection::Connection;
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{
    read_envelope, write_envelope, AgentMode, Catchup, ClientId, ClientRole, CommandBody, Envelope,
    MessageId, Payload, RunId, SessionEvent, SessionId, Subscription, WorkspaceId,
};
use serde::Serialize;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{oneshot, Mutex};

/// How long a command waits for its correlated reply before the client gives
/// up. A daemon that has stopped answering is a disconnect, and the UI must be
/// told that rather than spinning forever on a promise that never settles.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// The client name/version this shell announces in its `ClientHello`.
const CLIENT_NAME: &str = "codypendent-desktop";

/// What the webview learns about a connection that actually completed its
/// handshake. Every field here is something the daemon said — there is no
/// "connected" state the shell can invent on its own.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionInfo {
    pub socket_path: String,
    pub protocol_version: String,
    pub daemon_version: String,
    pub daemon_instance: String,
    pub build_id: String,
}

/// A frame pushed to the webview. Session events are forwarded verbatim (the
/// protocol crate serializes them, tagged with its own event names), so the
/// TypeScript side reads daemon state rather than a shell-invented projection.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DaemonFrame {
    /// One durable session event, live or replayed from attach catch-up.
    Event {
        session_id: Option<SessionId>,
        event: Box<SessionEvent>,
    },
    /// The daemon answered an attach with a snapshot instead of an event
    /// replay (the client was too far behind). Forwarded as-is so the webview
    /// can render pending approvals rather than silently dropping them.
    Catchup {
        session_id: SessionId,
        snapshot: Box<Catchup>,
    },
    /// The socket closed or failed. The UI must fall back to a disconnected
    /// state on this frame; it is the only honest thing to show afterwards.
    Disconnected { reason: String },
}

/// Where forwarded frames go. Implemented by the Tauri channel in `bridge`,
/// and by a plain `Vec` collector in tests.
pub trait FrameSink: Send + Sync + 'static {
    fn emit(&self, frame: DaemonFrame);
}

/// The run a submitted objective actually created, as reported by the daemon.
#[derive(Debug, Clone, Serialize)]
pub struct RunHandle {
    pub session_id: SessionId,
    /// `None` when the daemon accepted `StartRun` without naming the run in
    /// its reply (older daemons); the `RunStarted` event still carries it.
    pub run_id: Option<RunId>,
}

/// One session row, as the daemon lists it.
#[derive(Debug, Clone, Serialize)]
pub struct SessionRow {
    pub session_id: SessionId,
    pub title: String,
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
}

/// The socket the daemon is expected on, resolved exactly the way every other
/// client resolves it (`CODYPENDENT_SOCKET`, then the platform data dir).
pub fn socket_path() -> anyhow::Result<PathBuf> {
    Ok(RuntimePaths::resolve()
        .context("resolving the codypendentd socket path")?
        .socket_path)
}

/// A live, handshaken connection to the daemon.
///
/// The socket is split: a reader task owns the read half and either completes
/// an in-flight command or forwards the envelope to the webview; commands take
/// the writer under a mutex. That split is what lets a streaming run push
/// events at the UI while the UI is still awaiting a command reply.
pub struct DaemonClient {
    writer: Arc<Mutex<OwnedWriteHalf>>,
    client_id: ClientId,
    inflight: Arc<Mutex<HashMap<MessageId, oneshot::Sender<Envelope>>>>,
    workspace: WorkspaceId,
    repository: Option<String>,
}

impl DaemonClient {
    /// Connect to `socket` and complete the protocol handshake. An error here
    /// is the normal case on a machine with no daemon running, and the caller
    /// is expected to surface it to the operator verbatim.
    pub async fn connect<S: FrameSink>(
        socket: &Path,
        repository: Option<String>,
        sink: Arc<S>,
    ) -> anyhow::Result<(Arc<Self>, ConnectionInfo)> {
        let mut connection = Connection::connect(socket).await?;
        let hello = connection
            .handshake(CLIENT_NAME, env!("CARGO_PKG_VERSION"), None)
            .await?;

        let info = ConnectionInfo {
            socket_path: socket.display().to_string(),
            protocol_version: hello.selected_protocol.to_string(),
            daemon_version: hello.daemon_version.clone(),
            daemon_instance: hello.daemon_instance.to_string(),
            build_id: hello.build_id.clone(),
        };

        let (reader, writer, buffered, client_id) = connection.into_split();
        let writer = Arc::new(Mutex::new(writer));
        let inflight = Arc::new(Mutex::new(HashMap::new()));

        let client = Arc::new(Self {
            writer: Arc::clone(&writer),
            client_id,
            inflight: Arc::clone(&inflight),
            workspace: WorkspaceId::new(),
            repository,
        });

        tokio::spawn(read_loop(
            reader, buffered, writer, client_id, inflight, sink,
        ));

        Ok((client, info))
    }

    /// Sessions the daemon knows about.
    pub async fn list_sessions(&self) -> anyhow::Result<Vec<SessionRow>> {
        let reply = self
            .send_command(CommandBody::ListSessions {
                workspace: None,
                limit: Some(50),
            })
            .await?;
        match reply.payload {
            Payload::SessionList { sessions, .. } => Ok(sessions
                .into_iter()
                .map(|session| SessionRow {
                    session_id: session.session_id,
                    title: session.title,
                    state: session.state,
                    created_at: session.created_at.to_rfc3339(),
                    updated_at: session.updated_at.to_rfc3339(),
                })
                .collect()),
            Payload::CommandRejected(error) => {
                bail!("ListSessions rejected: {} ({})", error.message, error.code)
            }
            other => bail!("unexpected reply to ListSessions: {other:?}"),
        }
    }

    /// Create a session, attach to it as a controller, and start a run for
    /// `objective` — the same three commands `codypendent run` sends
    /// (`crates/cli/src/commands.rs::run_over_connection`). Attach catch-up is
    /// replayed into the sink so the transcript starts from daemon state.
    pub async fn start_objective<S: FrameSink>(
        &self,
        objective: String,
        mode: AgentMode,
        sink: &Arc<S>,
    ) -> anyhow::Result<RunHandle> {
        let create_reply = self
            .send_command(CommandBody::CreateSession {
                workspace: self.workspace,
                title: objective.clone(),
                repository: self.repository.clone(),
            })
            .await?;
        let session_id = match &create_reply.payload {
            Payload::CommandAccepted { .. } => create_reply.session_id.ok_or_else(|| {
                anyhow!(
                    "daemon accepted CreateSession but its reply carried no session_id; \
                     the desktop client cannot bind to the session it just created"
                )
            })?,
            Payload::CommandRejected(error) => {
                bail!("CreateSession rejected: {} ({})", error.message, error.code)
            }
            other => bail!("unexpected reply to CreateSession: {other:?}"),
        };

        self.attach(session_id, sink).await?;

        let start_reply = self
            .send_command(CommandBody::StartRun {
                session_id,
                objective,
                mode,
                repository: self.repository.clone(),
                model: None,
            })
            .await?;
        let run_id = match start_reply.payload {
            Payload::CommandAccepted { created_run, .. } => created_run,
            Payload::CommandRejected(error) => {
                bail!("StartRun rejected: {} ({})", error.message, error.code)
            }
            other => bail!("unexpected reply to StartRun: {other:?}"),
        };

        Ok(RunHandle { session_id, run_id })
    }

    /// Attach to an existing session and replay its catch-up into the sink.
    pub async fn attach<S: FrameSink>(
        &self,
        session_id: SessionId,
        sink: &Arc<S>,
    ) -> anyhow::Result<()> {
        let reply = self
            .send_command(CommandBody::AttachSession {
                session_id,
                last_seen_sequence: None,
                subscriptions: vec![Subscription::SessionSummary, Subscription::AgentActivity],
                requested_role: ClientRole::Controller,
                repository: self.repository.clone(),
            })
            .await?;
        match reply.payload {
            Payload::Catchup { catchup } => {
                replay_catchup(session_id, catchup, sink);
                Ok(())
            }
            Payload::CommandRejected(error) => {
                bail!("AttachSession rejected: {} ({})", error.message, error.code)
            }
            other => bail!("unexpected reply to AttachSession: {other:?}"),
        }
    }

    /// Cancel a run. This is a real `CancelRun` command — the UI never clears
    /// its own transcript and calls that a cancellation.
    pub async fn cancel_run(&self, run_id: RunId) -> anyhow::Result<()> {
        let reply = self.send_command(CommandBody::CancelRun { run_id }).await?;
        match reply.payload {
            Payload::CommandAccepted { .. } => Ok(()),
            Payload::CommandRejected(error) => {
                bail!("CancelRun rejected: {} ({})", error.message, error.code)
            }
            other => bail!("unexpected reply to CancelRun: {other:?}"),
        }
    }

    /// Send one command and await its correlated reply.
    async fn send_command(&self, body: CommandBody) -> anyhow::Result<Envelope> {
        let command_id = codypendent_protocol::CommandId::new();
        let command = codypendent_protocol::Command {
            command_id,
            idempotency_key: command_id.to_string(),
            expected_revision: None,
            body,
        };
        let envelope = Envelope::request(self.client_id, Payload::Command(command));
        let message_id = envelope.message_id;

        let (tx, rx) = oneshot::channel();
        self.inflight.lock().await.insert(message_id, tx);

        let write = {
            let mut writer = self.writer.lock().await;
            write_envelope(&mut *writer, &envelope).await
        };
        if let Err(error) = write {
            self.inflight.lock().await.remove(&message_id);
            return Err(anyhow::Error::from(error).context("writing a command to the daemon"));
        }

        match tokio::time::timeout(COMMAND_TIMEOUT, rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => {
                self.inflight.lock().await.remove(&message_id);
                bail!("the daemon connection closed before replying")
            }
            Err(_) => {
                self.inflight.lock().await.remove(&message_id);
                bail!(
                    "the daemon did not reply within {} seconds",
                    COMMAND_TIMEOUT.as_secs()
                )
            }
        }
    }
}

/// Replay an attach-time catch-up into the sink: event-by-event when the
/// daemon replayed, or the snapshot frame when it sent a projection instead.
fn replay_catchup<S: FrameSink>(session_id: SessionId, catchup: Catchup, sink: &Arc<S>) {
    match catchup {
        Catchup::Events { events, .. } => {
            for event in events {
                sink.emit(DaemonFrame::Event {
                    session_id: Some(session_id),
                    event: Box::new(event),
                });
            }
        }
        snapshot @ Catchup::Snapshot { .. } => sink.emit(DaemonFrame::Catchup {
            session_id,
            snapshot: Box::new(snapshot),
        }),
        // A catch-up kind a newer daemon invented (RULE 1): nothing to replay,
        // and inventing transcript content for it would be exactly the lie
        // this module exists to avoid.
        _ => {}
    }
}

/// The single reader for the connection: complete in-flight commands, answer
/// heartbeats, forward everything else to the webview, and — when the socket
/// ends — say so.
async fn read_loop<S: FrameSink>(
    mut reader: OwnedReadHalf,
    buffered: VecDeque<Envelope>,
    writer: Arc<Mutex<OwnedWriteHalf>>,
    client_id: ClientId,
    inflight: Arc<Mutex<HashMap<MessageId, oneshot::Sender<Envelope>>>>,
    sink: Arc<S>,
) {
    // Envelopes the handshake buffered (live events that outraced a reply)
    // must be folded before the wire is read, or they are lost.
    for envelope in buffered {
        dispatch(envelope, &inflight, &sink).await;
    }

    let reason = loop {
        match read_envelope(&mut reader).await {
            Ok(Some(envelope)) => {
                if matches!(envelope.payload, Payload::Ping) {
                    let pong = Envelope::request(client_id, Payload::Pong);
                    let mut writer = writer.lock().await;
                    if write_envelope(&mut *writer, &pong).await.is_err() {
                        break "the daemon connection dropped while answering a heartbeat"
                            .to_string();
                    }
                    continue;
                }
                dispatch(envelope, &inflight, &sink).await;
            }
            Ok(None) => break "the daemon closed the connection".to_string(),
            Err(error) => break format!("the daemon connection failed: {error}"),
        }
    };

    inflight.lock().await.clear();
    sink.emit(DaemonFrame::Disconnected { reason });
}

/// Route one envelope: correlated reply, forwarded event, or ignored.
async fn dispatch<S: FrameSink>(
    envelope: Envelope,
    inflight: &Arc<Mutex<HashMap<MessageId, oneshot::Sender<Envelope>>>>,
    sink: &Arc<S>,
) {
    if let Some(correlation) = envelope.correlation_id {
        let waiter = inflight.lock().await.remove(&correlation);
        if let Some(waiter) = waiter {
            let _ = waiter.send(envelope);
            return;
        }
    }
    if let Payload::Event(event) = envelope.payload {
        sink.emit(DaemonFrame::Event {
            session_id: envelope.session_id,
            event: Box::new(event),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex as StdMutex;

    use codypendent_protocol::{
        Actor, ClientCapabilities, CodypendentError, DaemonInstanceId, EventBody, ServerHello,
        PROTOCOL_V1,
    };
    use tokio::net::UnixListener;

    #[derive(Default)]
    struct Collector {
        frames: StdMutex<Vec<DaemonFrame>>,
    }

    impl Collector {
        fn frames(&self) -> Vec<DaemonFrame> {
            self.frames.lock().expect("collector lock").clone()
        }
    }

    impl FrameSink for Collector {
        fn emit(&self, frame: DaemonFrame) {
            self.frames.lock().expect("collector lock").push(frame);
        }
    }

    /// Commands a fake daemon observed, so a test can assert that submitting an
    /// objective really put `StartRun` on the wire.
    #[derive(Default)]
    struct Observed {
        commands: StdMutex<Vec<CommandBody>>,
    }

    fn server_hello() -> ServerHello {
        ServerHello {
            selected_protocol: PROTOCOL_V1,
            daemon_version: "0.9.0-test".to_string(),
            daemon_instance: DaemonInstanceId::new(),
            heartbeat_interval_ms: 15_000,
            resume_token: None,
            build_id: "test-build".to_string(),
        }
    }

    fn event(sequence: u64, body: EventBody) -> SessionEvent {
        SessionEvent {
            sequence,
            occurred_at: chrono::Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor: Actor::System,
            body,
        }
    }

    /// A minimal daemon: handshake, accept every command, and emit one model
    /// delta after a `StartRun` so the client has a real event to forward.
    async fn serve(listener: UnixListener, observed: Arc<Observed>) {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut session: Option<SessionId> = None;
        while let Ok(Some(request)) = read_envelope(&mut stream).await {
            match request.payload.clone() {
                Payload::ClientHello(_) => {
                    let reply = Envelope::reply_to(&request, Payload::ServerHello(server_hello()));
                    write_envelope(&mut stream, &reply).await.expect("hello");
                }
                Payload::Command(command) => {
                    observed
                        .commands
                        .lock()
                        .expect("observed lock")
                        .push(command.body.clone());
                    match command.body {
                        CommandBody::CreateSession { .. } => {
                            let id = SessionId::new();
                            session = Some(id);
                            let mut reply = Envelope::reply_to(
                                &request,
                                Payload::CommandAccepted {
                                    command_id: command.command_id,
                                    sequence: Some(1),
                                    created_run: None,
                                },
                            );
                            reply.session_id = Some(id);
                            write_envelope(&mut stream, &reply).await.expect("created");
                        }
                        CommandBody::AttachSession { .. } => {
                            let reply = Envelope::reply_to(
                                &request,
                                Payload::Catchup {
                                    catchup: Catchup::Events {
                                        from: 1,
                                        through: 1,
                                        events: vec![event(
                                            1,
                                            EventBody::SessionCreated {
                                                title: "replayed".to_string(),
                                            },
                                        )],
                                    },
                                },
                            );
                            write_envelope(&mut stream, &reply).await.expect("catchup");
                        }
                        CommandBody::StartRun { .. } => {
                            let run_id = RunId::new();
                            let reply = Envelope::reply_to(
                                &request,
                                Payload::CommandAccepted {
                                    command_id: command.command_id,
                                    sequence: Some(2),
                                    created_run: Some(run_id),
                                },
                            );
                            write_envelope(&mut stream, &reply).await.expect("started");

                            let mut live = Envelope::request(
                                request.client_id,
                                Payload::Event(event(
                                    3,
                                    EventBody::ModelStreamDelta {
                                        run_id,
                                        text: "real daemon output".to_string(),
                                    },
                                )),
                            );
                            live.session_id = session;
                            write_envelope(&mut stream, &live).await.expect("event");
                        }
                        CommandBody::CancelRun { .. } => {
                            let reply = Envelope::reply_to(
                                &request,
                                Payload::CommandAccepted {
                                    command_id: command.command_id,
                                    sequence: Some(4),
                                    created_run: None,
                                },
                            );
                            write_envelope(&mut stream, &reply)
                                .await
                                .expect("cancelled");
                        }
                        _ => {
                            let reply = Envelope::reply_to(
                                &request,
                                Payload::CommandRejected(CodypendentError::new(
                                    "test.unsupported",
                                    "the test daemon does not implement this command",
                                    false,
                                )),
                            );
                            write_envelope(&mut stream, &reply).await.expect("rejected");
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn socket_in(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("daemon.sock")
    }

    #[tokio::test]
    async fn connect_fails_when_no_daemon_listens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sink = Arc::new(Collector::default());
        let result = DaemonClient::connect(&socket_in(&dir), None, sink).await;
        assert!(
            result.is_err(),
            "connecting to an absent daemon must fail, never report a connection"
        );
    }

    #[tokio::test]
    async fn submitting_an_objective_sends_real_commands_and_forwards_real_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_in(&dir);
        let listener = UnixListener::bind(&path).expect("bind");
        let observed = Arc::new(Observed::default());
        tokio::spawn(serve(listener, Arc::clone(&observed)));

        let sink = Arc::new(Collector::default());
        let (client, info) = DaemonClient::connect(&path, None, Arc::clone(&sink))
            .await
            .expect("connect");
        assert_eq!(info.daemon_version, "0.9.0-test");

        let handle = client
            .start_objective("ship the thing".to_string(), AgentMode::Build, &sink)
            .await
            .expect("start objective");
        assert!(
            handle.run_id.is_some(),
            "the daemon named the run it created"
        );

        let commands = observed.commands.lock().expect("observed lock").clone();
        let started = commands
            .iter()
            .find_map(|command| match command {
                CommandBody::StartRun { objective, .. } => Some(objective.clone()),
                _ => None,
            })
            .expect("a StartRun command reached the daemon");
        assert_eq!(started, "ship the thing");

        // The live event the daemon emitted must arrive; nothing else may.
        for _ in 0..50 {
            if sink
                .frames()
                .iter()
                .any(|frame| matches!(frame, DaemonFrame::Event { event, .. } if matches!(event.body, EventBody::ModelStreamDelta { .. })))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let frames = sink.frames();
        let deltas: Vec<String> = frames
            .iter()
            .filter_map(|frame| match frame {
                DaemonFrame::Event { event, .. } => match &event.body {
                    EventBody::ModelStreamDelta { text, .. } => Some(text.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["real daemon output".to_string()]);

        let replayed = frames.iter().any(|frame| {
            matches!(frame, DaemonFrame::Event { event, .. }
                if matches!(event.body, EventBody::SessionCreated { .. }))
        });
        assert!(replayed, "attach catch-up is replayed into the transcript");

        client
            .cancel_run(handle.run_id.expect("run id"))
            .await
            .expect("cancel");
        let cancelled = observed
            .commands
            .lock()
            .expect("observed lock")
            .iter()
            .any(|command| matches!(command, CommandBody::CancelRun { .. }));
        assert!(cancelled, "cancel sends a real CancelRun command");
    }

    #[tokio::test]
    async fn a_closed_socket_reports_a_disconnect() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_in(&dir);
        let listener = UnixListener::bind(&path).expect("bind");
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let request = read_envelope(&mut stream)
                .await
                .expect("read")
                .expect("hello");
            let reply = Envelope::reply_to(&request, Payload::ServerHello(server_hello()));
            write_envelope(&mut stream, &reply).await.expect("hello");
            drop(stream);
        });

        let sink = Arc::new(Collector::default());
        let (_client, _info) = DaemonClient::connect(&path, None, Arc::clone(&sink))
            .await
            .expect("connect");

        for _ in 0..50 {
            if sink
                .frames()
                .iter()
                .any(|frame| matches!(frame, DaemonFrame::Disconnected { .. }))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("a dropped socket must surface as a Disconnected frame");
    }

    #[tokio::test]
    async fn the_handshake_advertises_this_client() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = socket_in(&dir);
        let listener = UnixListener::bind(&path).expect("bind");
        let seen: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
        let recorder = Arc::clone(&seen);
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let request = read_envelope(&mut stream)
                .await
                .expect("read")
                .expect("hello");
            if let Payload::ClientHello(hello) = &request.payload {
                *recorder.lock().expect("recorder") = Some(hello.client_name.clone());
                assert_eq!(hello.capabilities, ClientCapabilities::default());
            }
            let reply = Envelope::reply_to(&request, Payload::ServerHello(server_hello()));
            write_envelope(&mut stream, &reply).await.expect("hello");
            // Hold the socket open so the client is still connected when the
            // assertion below runs.
            tokio::time::sleep(Duration::from_millis(200)).await;
        });

        let sink = Arc::new(Collector::default());
        let (_client, _info) = DaemonClient::connect(&path, None, sink)
            .await
            .expect("connect");
        assert_eq!(
            seen.lock().expect("recorder").clone(),
            Some(CLIENT_NAME.to_string())
        );
    }
}
