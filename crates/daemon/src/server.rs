//! Unix-domain-socket protocol server.
//!
//! Phase 0 served the daemon-lifecycle messages (Ping, DaemonStatusRequest,
//! Shutdown). STEP 1.11 grows this into the full session server: a handshake
//! (`ClientHello`/`ServerHello`) with a 15s heartbeat, `AttachSession` with
//! catch-up (missed events vs. a snapshot per the ≤500 rule), per-session event
//! fan-out to subscribed clients, command routing through the crash-consistent
//! write path, and opaque daemon-signed resume tokens.
//!
//! The three lifecycle payloads keep working with **no** handshake — they are
//! connection-level daemon control, not session interaction — so the Phase 0
//! client (and `tests/socket.rs`) is unaffected. Only session interaction
//! (`Command`, including `AttachSession`) requires a prior `ClientHello`.

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use chrono::{DateTime, Utc};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{
    read_envelope, write_envelope, Catchup, ClientId, ClientRole, CommandBody, DaemonStatus,
    Envelope, FrameError, Payload, ProtocolError, ServerHello, SessionEvent, SessionId,
    Subscription, BUILD_ID, PROTOCOL_V1,
};
use codypendent_sandbox::{LifecycleState, UiTarget};
use sqlx::SqlitePool;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, watch, Mutex};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use crate::approvals::ApprovalBroker;
use crate::artifacts::ArtifactStore;
use crate::blackboard::{BlackboardHub, BlackboardReader, ReadBlackboardRequest};
use crate::commands::{ApplyContext, CommandProcessor};
use crate::documents::{
    DocumentHub, DocumentLeaseReleaseRequest, DocumentLeaseRequest, DocumentLeaser,
    DocumentMutationRequest, DocumentMutator, DocumentPublisher, PublishDocumentRequest,
};
use crate::executor::{RunExecutor, RunLaunch};
use crate::instance::InstanceRecord;
use crate::ledger;
use crate::projections;
use crate::promotion::{
    AdvancePromotionRequest, ApprovePromotionRequest, PromotionGateway, ProposePromotionRequest,
    RollbackPromotionRequest,
};
use crate::remote_ui::{
    broker_error, RemoteUiBroker, UiBrokerFrame, UiBrokerTarget, UiMediatedAction,
    UiMediatedSubscription, UiProducerHandle,
};
use crate::remote_ui_plugins::{system_remote_ui_runtime, RemoteUiPluginStore};
use crate::remote_ui_workers::{RemoteUiWorkerService, UiWorkerRequest};
use crate::subscriptions::SubscriptionHub;
use crate::workflow_stream::{ReadWorkflowRunRequest, WorkflowHub, WorkflowReader};
use crate::workflows::{
    CancelWorkflowRequest, PauseWorkflowRequest, ResumeWorkflowRequest, RetryWorkflowNodeRequest,
    StartWorkflowRequest, WorkflowLifecycle, WorkflowStarter,
};

/// Heartbeat cadence advertised in `ServerHello` and used to probe idle clients.
const HEARTBEAT_INTERVAL_MS: u64 = 15_000;
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(HEARTBEAT_INTERVAL_MS);
/// A client silent for this many heartbeat intervals (3 × 15s = 45s) is dropped.
const HEARTBEAT_MISS_LIMIT: u32 = 3;
/// The catch-up cutover: a client at most this many events behind is replayed
/// event-by-event; further behind, it receives a projection snapshot instead.
const CATCHUP_EVENT_LIMIT: u64 = 500;
type RemoteUiRunRow = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// A write half shared between a connection's request/reply path and its
/// per-session event forwarders, so both can frame envelopes onto one socket.
type SharedWriter = Arc<Mutex<OwnedWriteHalf>>;

pub struct ServerState {
    pub pool: SqlitePool,
    pub paths: RuntimePaths,
    pub instance: InstanceRecord,
    pub started_at: DateTime<Utc>,
    pub shutdown: watch::Sender<bool>,
    /// The crash-consistent command write path (persist-before-publish); shares
    /// its [`SubscriptionHub`] with `subscriptions` below.
    pub commands: CommandProcessor,
    /// Per-session event fan-out the server subscribes attached clients to.
    pub subscriptions: SubscriptionHub,
    /// Content-addressed artifact store (`<data_dir>/artifacts`); held here so
    /// the session server owns it for later steps (tool output, chronicles).
    pub artifacts: ArtifactStore,
    /// The per-user secret (32 bytes) that signs resume tokens.
    pub secret: Vec<u8>,
    /// Executes accepted runs. `None` in a lib-only / test embedding (the run
    /// stays `Queued`); the assembly binary injects an implementation that wraps
    /// the runtime agent loop (dependency inversion — see [`crate::executor`]).
    pub executor: Option<Arc<dyn RunExecutor>>,
    /// Per-document CRDT-sync fan-out: a `MutateDocument` that applies publishes
    /// its sync here, and a client's `Subscription::Document` forwarder delivers
    /// from it (Phase 4 STEP 4.3).
    pub documents: DocumentHub,
    /// Applies an accepted `MutateDocument` onto the authoritative collaborative
    /// document. `None` in a lib-only / test embedding (the command is then
    /// rejected `document.transport-unavailable`); the assembly injects a
    /// knowledge-backed implementation (dependency inversion — see
    /// [`crate::documents`]).
    pub mutator: Option<Arc<dyn DocumentMutator>>,
    /// Acquires/releases the block-range edit leases gating `MutateDocument`.
    /// `None` in a lib-only / test embedding (lease commands are then rejected
    /// `document.transport-unavailable`); injected together with `mutator` by the
    /// assembly.
    pub leaser: Option<Arc<dyn DocumentLeaser>>,
    /// Creates a durable run from an accepted `StartWorkflow` (Phase 5 STEP 5.2).
    /// `None` in a lib-only / test embedding (the command is then rejected
    /// `workflow.transport-unavailable`); the assembly injects a
    /// `codypendent-workflow`-backed implementation over the pool.
    pub starter: Option<Arc<dyn WorkflowStarter>>,
    /// Pauses/resumes/retries an existing durable run from the corresponding
    /// lifecycle commands (Phase 5 STEP 5.2). `None` in a lib-only / test embedding
    /// (those commands are then rejected `workflow.transport-unavailable`); injected
    /// together with `starter` by the assembly.
    pub lifecycle: Option<Arc<dyn WorkflowLifecycle>>,
    /// Per-workflow-run blackboard fan-out: the workflow executor publishes each
    /// posted artifact here, and a client's `Subscription::Blackboard` forwarder
    /// delivers from it (Phase 5 STEP 5.3). Reuses the executor's own hub (the
    /// publisher is the agent loop inside the executor), or a fresh empty one in a
    /// lib-only / test embedding.
    pub blackboards: BlackboardHub,
    /// Reads a durable run's board for an accepted `ReadBlackboard` (Phase 5
    /// STEP 5.3). `None` in a lib-only / test embedding (the command is then
    /// rejected `workflow.transport-unavailable`); the assembly injects a
    /// `codypendent-workflow`-backed implementation over the pool.
    pub blackboard_reader: Option<Arc<dyn BlackboardReader>>,
    /// Per-workflow-run node-lifecycle fan-out: the workflow host publishes each
    /// node transition (and run-phase change) here, and a client's
    /// `Subscription::Workflow` forwarder delivers from it (Phase 5 STEP 5.2 / T9).
    /// Reuses the executor's own hub (the publisher is the driver inside the
    /// executor), or a fresh empty one in a lib-only / test embedding.
    pub workflows: WorkflowHub,
    /// Reads a durable run's observability snapshot for an accepted `ReadWorkflowRun`
    /// (Phase 5 STEP 5.2 / T9). `None` in a lib-only / test embedding (the command is
    /// then rejected `workflow.transport-unavailable`); the assembly injects a
    /// `codypendent-workflow`-backed implementation over the pool.
    pub workflow_reader: Option<Arc<dyn WorkflowReader>>,
    /// Computes an accepted `PublishDocument` command's plan, parks its approval,
    /// and (once approved) executes it (Phase 4 STEP 4.4). `None` in a lib-only /
    /// test embedding (the command is then rejected
    /// `document.transport-unavailable`); the assembly injects a
    /// knowledge-backed implementation over the pool, mirroring `mutator`/`leaser`.
    pub publisher: Option<Arc<dyn DocumentPublisher>>,
    /// Drives the evaluation-gated promotion pipeline (Phase 7 STEP 7.5):
    /// propose/advance/approve/rollback. `None` in a lib-only / test embedding
    /// (every promotion command is then rejected
    /// `promotion.transport-unavailable`); the assembly injects a
    /// `codypendent-eval`-backed implementation over the pool.
    pub promotion: Option<Arc<dyn PromotionGateway>>,
    /// Serializes run admission against an idle-guarded shutdown
    /// (`Payload::ShutdownIfIdle`), closing the auto-restart TOCTOU window. Every
    /// write-path command apply is held under a `read()` guard; the idle-guarded
    /// shutdown takes the exclusive `write()` guard, so its
    /// `active_run_count`-check and shutdown signal cannot interleave with a
    /// concurrent `StartRun`/`SubmitUserInput` that would admit a new run.
    pub run_admission: Arc<tokio::sync::RwLock<()>>,
    /// Set once an idle-guarded shutdown has been authorized. A run-admitting
    /// command that was blocked on [`Self::run_admission`] observes this on
    /// acquiring its read guard and is refused (retryable) rather than admitted
    /// into a daemon that is about to exit.
    pub shutting_down: Arc<std::sync::atomic::AtomicBool>,
    /// Repository roots (canonicalized) this server has already fired a
    /// background code-graph scan for. Without this, browsing the code-graph
    /// edges overlay before ever running the agent showed "no edges": the
    /// tree-sitter scan only ran on daemon boot (the daemon's own startup
    /// directory) or the first `StartRun`/`SubmitUserInput` for a repository
    /// (via the executor's own `ensure_scanned`). `CreateSession` and
    /// `AttachSession` now carry the session's repository root too, so
    /// [`maybe_scan_repository`] can warm the graph as soon as a session is
    /// opened — guarded by this set so a session repeatedly created or
    /// re-attached against the same repository fires at most one scan.
    pub scanned_repos: Arc<Mutex<std::collections::HashSet<std::path::PathBuf>>>,
    /// Session-scoped validated Remote UI documents, fan-out, and replay.
    pub remote_ui: RemoteUiBroker,
    /// Durable verified UI packages. `None` means the host runtime or enforcing
    /// sandbox was unavailable; workers then fail closed while normal clients
    /// and core TUI surfaces continue to operate.
    pub remote_ui_plugins: Option<Arc<RemoteUiPluginStore>>,
    /// Supervised verified worker processes, started lazily by session attach.
    pub remote_ui_workers: Option<RemoteUiWorkerService>,
    /// Bounded path from workers into daemon action/projection mediation.
    pub remote_ui_worker_requests: mpsc::Sender<UiWorkerRequest>,
    /// Coalesced invalidation stream for latest-wins IDE context projections.
    pub remote_ui_context_updates: broadcast::Sender<SessionId>,
}

/// Bind the socket, write the pidfile, and serve until Shutdown or SIGTERM /
/// SIGINT. Removes the socket and pidfile on exit.
///
/// This is the executor-less entry point: an accepted `StartRun` is persisted
/// and the run stays `Queued` (nothing executes it). It is what the daemon's own
/// integration tests (`tests/socket.rs`, `tests/server_it.rs`) drive. The
/// assembly binary calls [`run_with_executor`] with a real executor.
pub async fn run(
    pool: SqlitePool,
    paths: RuntimePaths,
    instance: InstanceRecord,
) -> anyhow::Result<()> {
    run_with_executor(pool, paths, instance, None).await
}

/// Like [`run`], but with an injected [`RunExecutor`] that actually executes an
/// accepted `StartRun` (the assembly binary wraps the runtime agent loop).
///
/// When an executor is present, the server binds its command fan-out and
/// approval broker to the executor's ([`RunExecutor::collaborators`]), so a
/// run's events reach attached clients and a client's `ResolveApproval` reaches
/// the runtime awaiting it. With `executor = None` the server creates its own
/// fresh instances and behaves exactly as the pre-executor server did.
pub async fn run_with_executor(
    pool: SqlitePool,
    paths: RuntimePaths,
    instance: InstanceRecord,
    executor: Option<Arc<dyn RunExecutor>>,
) -> anyhow::Result<()> {
    let listener = acquire_socket(&paths).await?;
    run_with_executor_on(listener, pool, paths, instance, executor).await
}

/// Bind the daemon socket (refusing if a live daemon owns it) and write the
/// pidfile. Split out so the assembly binary can claim single-instance
/// exclusivity **before** running startup recovery — a second daemon must never
/// get far enough to fail a live daemon's runs, relaunch its queued runs, or
/// wipe its code graph before discovering the socket is taken.
pub async fn acquire_socket(paths: &RuntimePaths) -> anyhow::Result<UnixListener> {
    prepare_socket(paths).await?;
    let listener = UnixListener::bind(&paths.socket_path)?;
    std::fs::write(&paths.pid_path, std::process::id().to_string())?;
    Ok(listener)
}

/// Like [`run_with_executor`], but on a pre-acquired [`acquire_socket`]
/// listener (the assembly binary acquires it before recovery).
pub async fn run_with_executor_on(
    listener: UnixListener,
    pool: SqlitePool,
    paths: RuntimePaths,
    instance: InstanceRecord,
    executor: Option<Arc<dyn RunExecutor>>,
) -> anyhow::Result<()> {
    info!(socket = %paths.socket_path.display(), pid = std::process::id(), "daemon listening");

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    // One shared fan-out drives both the command processor (publisher) and the
    // server (subscriber); cloning a `SubscriptionHub` shares its channels. When
    // an executor is injected, reuse ITS hub + broker so run events published by
    // the agent loop reach this server's forwarders, and a client's
    // `ResolveApproval` (routed through the command processor) wakes the runtime.
    let (subscriptions, approvals) = executor
        .as_ref()
        .and_then(|e| e.collaborators())
        .unwrap_or_else(|| (SubscriptionHub::new(), ApprovalBroker::new()));

    // The document-transport seam, bundled with the executor by the assembly (as
    // its `collaborators` are). The per-document fan-out is created fresh here —
    // the server owns publishing (after a mutation applies) and subscribing (a
    // client's `Document` forwarder), and the mutator only computes the sync.
    let mutator = executor.as_ref().and_then(|e| e.document_mutator());
    let leaser = executor.as_ref().and_then(|e| e.document_leaser());
    let publisher = executor.as_ref().and_then(|e| e.document_publisher());
    let starter = executor.as_ref().and_then(|e| e.workflow_starter());
    let lifecycle = executor.as_ref().and_then(|e| e.workflow_lifecycle());
    let promotion = executor.as_ref().and_then(|e| e.promotion_gateway());
    let documents = DocumentHub::new();
    // The blackboard read seam, bundled with the executor by the assembly. Unlike
    // the document hub, the per-run blackboard fan-out is REUSED from the executor
    // (not created fresh): the publisher is the agent loop deep inside the executor,
    // so both sides must share one hub — exactly as `collaborators` shares the
    // `SubscriptionHub`. Absent an executor, a fresh empty hub (never published to).
    let blackboard_reader = executor.as_ref().and_then(|e| e.blackboard_reader());
    let blackboards = executor
        .as_ref()
        .and_then(|e| e.blackboard_hub())
        .unwrap_or_default();
    // The workflow observability seams, bundled with the executor exactly like the
    // blackboard ones: the per-run node-lifecycle hub is REUSED from the executor
    // (the publisher is the driver inside it, so both sides must share one hub),
    // and the snapshot reader is pulled out for `ReadWorkflowRun` (T9).
    let workflow_reader = executor.as_ref().and_then(|e| e.workflow_reader());
    let workflows = executor
        .as_ref()
        .and_then(|e| e.workflow_hub())
        .unwrap_or_default();

    // Drive approval expiry: without a periodic caller, `expires_at` deadlines
    // are dead machinery — an approval with a deadline would simply never
    // expire at runtime. The same tick prunes session and document fan-out
    // channels whose last subscriber detached, so neither hub grows for the
    // daemon's lifetime. Aborted when the server stops.
    let expiry_task = {
        let broker = approvals.clone();
        let hub = subscriptions.clone();
        let doc_hub = documents.clone();
        let board_hub = blackboards.clone();
        let workflow_hub = workflows.clone();
        let pool = pool.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                match broker.expire_due(&pool, Utc::now()).await {
                    Ok(0) => {}
                    Ok(n) => info!(expired = n, "expired overdue approvals"),
                    Err(error) => warn!(%error, "approval expiry sweep failed"),
                }
                hub.prune_idle();
                doc_hub.prune_idle();
                board_hub.prune_idle();
                workflow_hub.prune_idle();
            }
        })
    };

    let commands = CommandProcessor::new(subscriptions.clone(), approvals);
    let artifacts = ArtifactStore::new(paths.data_dir.join("artifacts"));
    let secret = load_or_create_secret(&paths.data_dir)?;
    let (remote_ui_worker_requests, remote_ui_request_rx) = mpsc::channel(256);
    let (remote_ui_context_updates, _) = broadcast::channel(256);
    // A process crash can leave a lifecycle idempotency claim without a reply.
    // Store operations are themselves exact-input idempotent, so expired NULL
    // claims may be reclaimed and safely replayed instead of stranding forever.
    let stale_plugin_claim = (Utc::now() - chrono::Duration::minutes(30)).to_rfc3339();
    sqlx::query("DELETE FROM ui_plugin_commands WHERE result_json IS NULL AND created_at < ?")
        .bind(stale_plugin_claim)
        .execute(&pool)
        .await?;
    let (remote_ui_plugins, remote_ui_workers) = match system_remote_ui_runtime() {
        Ok((runtime, supervisor)) => {
            match RemoteUiPluginStore::open(
                &paths.data_dir,
                &paths.config_dir,
                runtime,
                secret.clone(),
            ) {
                Ok(store) => {
                    let store = Arc::new(store);
                    let source: Arc<dyn crate::remote_ui_workers::VerifiedUiLaunchSource> =
                        store.clone();
                    (
                        Some(store),
                        Some(RemoteUiWorkerService::new(supervisor, source)),
                    )
                }
                Err(error) => {
                    warn!(%error, "Remote UI plugin persistence unavailable; component workers fail closed");
                    (None, None)
                }
            }
        }
        Err(error) => {
            warn!(%error, "Remote UI worker runtime unavailable; component workers fail closed");
            (None, None)
        }
    };

    let state = Arc::new(ServerState {
        pool,
        paths: paths.clone(),
        instance,
        started_at: Utc::now(),
        shutdown: shutdown_tx,
        commands,
        subscriptions,
        artifacts,
        secret,
        executor,
        documents,
        mutator,
        leaser,
        starter,
        lifecycle,
        publisher,
        promotion,
        blackboards,
        blackboard_reader,
        workflows,
        workflow_reader,
        run_admission: Arc::new(tokio::sync::RwLock::new(())),
        shutting_down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        scanned_repos: Arc::new(Mutex::new(std::collections::HashSet::new())),
        remote_ui: RemoteUiBroker::default(),
        remote_ui_plugins,
        remote_ui_workers,
        remote_ui_worker_requests,
        remote_ui_context_updates,
    });
    let remote_ui_request_task = tokio::spawn(consume_remote_ui_worker_requests(
        Arc::clone(&state),
        remote_ui_request_rx,
    ));

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                info!("shutdown requested via protocol");
                break;
            }
            _ = tokio::signal::ctrl_c() => {
                info!("SIGINT received");
                break;
            }
            _ = sigterm.recv() => {
                info!("SIGTERM received");
                break;
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _addr)) => {
                        let state = Arc::clone(&state);
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, state).await {
                                warn!(error = %e, "connection ended with error");
                            }
                        });
                    }
                    Err(e) => error!(error = %e, "accept failed"),
                }
            }
        }
    }

    if let Some(workers) = &state.remote_ui_workers {
        workers.shutdown();
    }
    remote_ui_request_task.abort();

    expiry_task.abort();
    let _ = std::fs::remove_file(&paths.socket_path);
    let _ = std::fs::remove_file(&paths.pid_path);
    info!("daemon stopped");
    Ok(())
}

/// Refuse to start if a live daemon already owns the socket; remove the
/// socket file if it is stale (bind would otherwise fail with AddrInUse).
async fn prepare_socket(paths: &RuntimePaths) -> anyhow::Result<()> {
    paths.validate_socket_path()?;
    if paths.socket_path.exists() {
        match UnixStream::connect(&paths.socket_path).await {
            Ok(_) => anyhow::bail!(
                "another daemon is already listening on {}",
                paths.socket_path.display()
            ),
            Err(_) => {
                warn!(socket = %paths.socket_path.display(), "removing stale socket");
                std::fs::remove_file(&paths.socket_path)?;
            }
        }
    }
    Ok(())
}

/// Per-connection mutable state established by the handshake and updated by
/// `AttachSession`.
struct ConnState {
    /// The client's identity — from `ClientHello` (its envelope, or a valid
    /// resume token). `None` until the connection handshakes.
    client_id: Option<ClientId>,
    /// The role applied to commands on this connection. A handshaken local
    /// client defaults to [`ClientRole::Controller`]: the Phase 1 socket is
    /// user-private (0700 dirs, OS peer identity), so the single connecting user
    /// is trusted to create sessions and control their own runs without a prior
    /// attach. An explicit `AttachSession` may narrow (or re-assert) the role —
    /// e.g. an observer-only view. Remote transports (later phases) will default
    /// to `Observer` and require authenticated elevation.
    role: ClientRole,
    /// Whether a `ClientHello` has been seen (session interaction requires it).
    handshaken: bool,
    /// Sessions this connection is attached to, with the role it attached under.
    /// On disconnect a `ClientPresenceChanged { present: false }` is published for
    /// each, so other clients see it leave (Phase 3 STEP 3.7).
    attached: Vec<(SessionId, ClientRole)>,
}

impl ConnState {
    fn new() -> Self {
        Self {
            client_id: None,
            role: ClientRole::Controller,
            handshaken: false,
            attached: Vec::new(),
        }
    }

    /// The identity to stamp on outgoing frames / commands, falling back to a
    /// per-message client id when the connection has not handshaked.
    fn client_id_or(&self, fallback: ClientId) -> ClientId {
        self.client_id.unwrap_or(fallback)
    }
}

/// Serve one connection: a frame-read loop plus a separate heartbeat task.
/// Lifecycle payloads are served without a handshake; session interaction is
/// gated on a prior `ClientHello`. Event forwarders spawned by `AttachSession`
/// write to the same (shared) socket and are aborted when the connection ends.
///
/// The heartbeat runs in its own task (not a `select!` arm of the read loop) so a
/// heartbeat tick can never cancel a `read_envelope` future mid-frame — which
/// would drop the consumed bytes and desynchronize the stream. The read loop only
/// races reads against an idle-shutdown signal the heartbeat task raises, and it
/// stamps a shared last-activity instant the heartbeat task consults to decide
/// when a silent client should be dropped.
async fn handle_connection(stream: UnixStream, state: Arc<ServerState>) -> anyhow::Result<()> {
    let (mut read_half, write_half) = stream.into_split();
    let writer: SharedWriter = Arc::new(Mutex::new(write_half));
    let mut conn = ConnState::new();
    // Keyed by session: a re-attach to the same session on this connection
    // replaces (aborts) the prior forwarder instead of stacking a duplicate
    // that would double-deliver every live event.
    let mut forwarders: std::collections::HashMap<SessionId, JoinHandle<()>> =
        std::collections::HashMap::new();
    // Document forwarders grouped by the session attach that spawned them, so a
    // re-attach to a session replaces that session's whole document set: attaching
    // with a reduced `Document` list aborts the forwarders for the documents it no
    // longer names (mirrors the per-session replacement of `forwarders` above),
    // while another session's document forwarders are left untouched.
    let mut doc_forwarders: std::collections::HashMap<SessionId, Vec<JoinHandle<()>>> =
        std::collections::HashMap::new();
    let mut ui_forwarders: std::collections::HashMap<SessionId, JoinHandle<()>> =
        std::collections::HashMap::new();

    // The read loop stamps this on every frame; the heartbeat task reads it to
    // decide when the client has gone silent. Locked only for the instant swap,
    // never across an `.await`, so a std mutex is the right tool.
    let last_activity = Arc::new(std::sync::Mutex::new(tokio::time::Instant::now()));
    // The heartbeat task raises this to end an idle (or dead-peer) connection.
    let (idle_tx, mut idle_rx) = watch::channel(false);

    let heartbeat = tokio::spawn(heartbeat_loop(
        Arc::clone(&writer),
        Arc::clone(&last_activity),
        idle_tx,
    ));

    let result = loop {
        tokio::select! {
            // Frame reads are never raced against a timer, so a frame is never
            // cancelled mid-parse. The only competing arm is the idle signal,
            // which the heartbeat task raises only once the client has already
            // gone silent — so nothing in flight is lost.
            read = read_envelope(&mut read_half) => {
                let request = match read {
                    Ok(Some(request)) => request,
                    Ok(None) => break Ok(()), // clean end-of-stream
                    Err(e) => break Err(e.into()),
                };
                *last_activity
                    .lock()
                    .expect("last-activity mutex poisoned") = tokio::time::Instant::now();
                match handle_request(
                    &state,
                    &writer,
                    &mut conn,
                    &mut forwarders,
                    &mut doc_forwarders,
                    &mut ui_forwarders,
                    request,
                )
                .await
                {
                    Ok(true) => break Ok(()), // shutdown handled
                    Ok(false) => {}
                    Err(e) => break Err(e),
                }
            }
            // The heartbeat task asked us to end (silent 3 intervals, or the peer
            // vanished mid-ping). A `changed()` error (sender dropped) ends it too.
            _ = idle_rx.changed() => break Ok(()),
        }
    };

    heartbeat.abort();
    // A slow or vanished client must never wedge a forwarder; drop them all —
    // both the session event forwarders and the document sync forwarders.
    for forwarder in forwarders.values() {
        forwarder.abort();
    }
    for handles in doc_forwarders.values() {
        for handle in handles {
            handle.abort();
        }
    }
    for forwarder in ui_forwarders.values() {
        forwarder.abort();
    }
    // Announce this client's departure from every session it was attached to, so
    // the remaining clients see it leave (STEP 3.7).
    if let Some(client_id) = conn.client_id {
        for (session_id, role) in &conn.attached {
            let disconnected = state.remote_ui.disconnect_renderer(*session_id, client_id);
            if disconnected.remaining_total == 0 {
                if let Some(workers) = &state.remote_ui_workers {
                    workers.stop_session(*session_id);
                }
            } else if let Some(workers) = &state.remote_ui_workers {
                for target in disconnected.departed_targets {
                    workers.stop_session_target(*session_id, target);
                }
            }
            publish_presence(&state, *session_id, client_id, *role, false).await;
        }
    }
    result
}

/// The per-connection heartbeat, run as its own task beside the read loop. It
/// pings the client every [`HEARTBEAT_INTERVAL`] via the shared writer and, when
/// the client has been silent for [`HEARTBEAT_MISS_LIMIT`] intervals (or a ping
/// write fails), signals `idle_tx` so the read loop ends the connection. Keeping
/// it off the read path is what guarantees a tick never cancels a frame read.
async fn heartbeat_loop(
    writer: SharedWriter,
    last_activity: Arc<std::sync::Mutex<tokio::time::Instant>>,
    idle_tx: watch::Sender<bool>,
) {
    // Delay the first tick a full interval so an idle-but-fresh connection is
    // not immediately probed.
    let mut ticker = tokio::time::interval_at(
        tokio::time::Instant::now() + HEARTBEAT_INTERVAL,
        HEARTBEAT_INTERVAL,
    );
    let idle_limit = HEARTBEAT_INTERVAL * HEARTBEAT_MISS_LIMIT;
    loop {
        ticker.tick().await;
        let idle = last_activity
            .lock()
            .expect("last-activity mutex poisoned")
            .elapsed();
        if idle >= idle_limit {
            let _ = idle_tx.send(true); // silent for 3 intervals — drop the client
            return;
        }
        let ping = Envelope::request(ClientId::new(), Payload::Ping);
        if send(&writer, &ping).await.is_err() {
            let _ = idle_tx.send(true); // peer gone
            return;
        }
    }
}

/// Warm `repository`'s code graph in the background the first time this server
/// sees a session opened against it (`CreateSession`/`AttachSession` carrying a
/// `repository`), so the code-graph edges overlay is populated as soon as a
/// user opens the TUI on a repo — not only after the first `StartRun` (which
/// reaches the same warm-up through the executor's own `ensure_scanned`).
///
/// Guarded by [`ServerState::scanned_repos`] so a session repeatedly created or
/// re-attached against the same repository fires at most one scan; a `None` or
/// empty `repository` (an older client, or an attach without repo context) is a
/// no-op. Fire-and-forget end to end: [`RunExecutor::ensure_repository_scanned`]
/// is synchronous and spawns its own background task, exactly like
/// `RunExecutor::spawn_run` — this function never blocks the command reply.
async fn maybe_scan_repository(state: &Arc<ServerState>, repository: Option<String>) {
    let Some(root) = repository.filter(|root| !root.is_empty()) else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
    let newly = {
        let mut seen = state.scanned_repos.lock().await;
        seen.insert(canonical)
    };
    if newly {
        if let Some(executor) = state.executor.as_ref() {
            executor.ensure_repository_scanned(root);
        }
    }
}

/// Resolve a run's repository root from an optional per-run repository path,
/// shared by the `StartRun` and continuation (`SubmitUserInput`) launch arms so
/// both resolve identically. `Some(path)` binds the run to exactly that
/// checkout; `None` falls back to the daemon's working directory. That fallback
/// is a genuine last resort: a continuation first recovers the SESSION's real
/// repository from its originating `StartRun` (I-1), so it reaches
/// `current_dir()` only for a session that never had a repository to inherit (an
/// older client that sent none) — never as the silent default it used to be.
fn resolve_run_repository(repository: Option<&str>) -> std::path::PathBuf {
    repository.map(std::path::PathBuf::from).unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    })
}

/// Whether a command, when applied, can admit a NEW non-terminal run (raise
/// `active_run_count`). Both `StartRun` and `SubmitUserInput` route through the
/// same run-creating transaction (`commands.rs`), so both count; every other
/// command either mutates an existing run's state or touches no run at all.
/// Used by the idle-guarded-shutdown gate to refuse only run-admitting commands
/// once a shutdown is authorized — inclusive by design (a spurious retryable in
/// the sub-millisecond shutdown window is safe; missing an admitter is not).
fn admits_run(body: &CommandBody) -> bool {
    matches!(
        body,
        CommandBody::StartRun { .. } | CommandBody::SubmitUserInput { .. }
    )
}

/// Handle one request. Returns `Ok(true)` when a Shutdown was served (the caller
/// should stop reading this connection). Replies are framed onto `writer`.
async fn handle_request(
    state: &Arc<ServerState>,
    writer: &SharedWriter,
    conn: &mut ConnState,
    forwarders: &mut std::collections::HashMap<SessionId, JoinHandle<()>>,
    doc_forwarders: &mut std::collections::HashMap<SessionId, Vec<JoinHandle<()>>>,
    ui_forwarders: &mut std::collections::HashMap<SessionId, JoinHandle<()>>,
    request: Envelope,
) -> anyhow::Result<bool> {
    // Major-version incompatibility is refused structurally; the connection
    // survives (mirrors Phase 0).
    if !request.protocol_version.compatible_with(&PROTOCOL_V1) {
        let reply = Envelope::reply_to(
            &request,
            Payload::Error(ProtocolError {
                code: "protocol.incompatible-version".to_string(),
                message: format!(
                    "daemon speaks {PROTOCOL_V1}, client sent {}",
                    request.protocol_version
                ),
                retryable: false,
            }),
        );
        send(writer, &reply).await?;
        return Ok(false);
    }

    match &request.payload {
        // --- daemon lifecycle: served with NO handshake required ---
        Payload::Ping => {
            send(writer, &Envelope::reply_to(&request, Payload::Pong)).await?;
        }
        // A client's heartbeat reply; the read alone already reset the silence
        // counter, so nothing more is owed.
        Payload::Pong => {}
        Payload::DaemonStatusRequest => {
            let status = status(state).await?;
            send(
                writer,
                &Envelope::reply_to(&request, Payload::DaemonStatusResponse(status)),
            )
            .await?;
        }
        Payload::Shutdown => {
            send(writer, &Envelope::reply_to(&request, Payload::ShutdownAck)).await?;
            let _ = state.shutdown.send(true);
            return Ok(true);
        }
        // The idle-guarded shutdown (protocol v1.3): stop ONLY if no run is
        // active. The exclusive admission guard makes the count-check and the
        // shutdown signal atomic against a concurrent run-admitting command —
        // a `StartRun`/`SubmitUserInput` mid-apply has committed its `Queued`
        // row (so the count sees it → we refuse) or has not yet acquired its
        // read guard (so it blocks here, then observes `shutting_down` and is
        // refused). Either way an in-flight run is never silently killed.
        Payload::ShutdownIfIdle => {
            // Decide UNDER the exclusive guard (atomic against run admission),
            // then RELEASE it before touching the socket — a slow-reading client
            // must not wedge all run admission behind its reply. Setting
            // `shutting_down` while the guard is held is what a blocked admit
            // observes on resume, so the ordering that matters is preserved.
            let active = {
                let _admit = state.run_admission.write().await;
                let active = u64::try_from(ledger::active_run_count(&state.pool).await?)?;
                if active == 0 {
                    state
                        .shutting_down
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
                active
            };
            if active > 0 {
                let reply = Envelope::reply_to(
                    &request,
                    Payload::ShutdownRefused {
                        active_run_count: active,
                    },
                );
                send(writer, &reply).await?;
                return Ok(false);
            }
            send(writer, &Envelope::reply_to(&request, Payload::ShutdownAck)).await?;
            let _ = state.shutdown.send(true);
            return Ok(true);
        }

        // --- handshake ---
        Payload::ClientHello(hello) => {
            let selected_protocol = hello
                .supported_protocols
                .iter()
                .find(|candidate| candidate.compatible_with(&PROTOCOL_V1))
                .map(|candidate| codypendent_protocol::ProtocolVersion {
                    major: PROTOCOL_V1.major,
                    minor: candidate.minor.min(PROTOCOL_V1.minor),
                });
            let Some(selected_protocol) = selected_protocol else {
                send(
                    writer,
                    &Envelope::reply_to(
                        &request,
                        Payload::Error(ProtocolError {
                            code: "protocol.no-common-version".to_string(),
                            message: format!(
                                "daemon speaks {PROTOCOL_V1}; client offered {:?}",
                                hello.supported_protocols
                            ),
                            retryable: false,
                        }),
                    ),
                )
                .await?;
                return Ok(false);
            };
            // A valid resume token restores the prior identity; an invalid or
            // expired one is ignored (proceed as a fresh client, do not drop).
            let client_id = hello
                .resume_token
                .as_ref()
                .and_then(|token| resume::verify_resume_token(&state.secret, &token.0))
                .map(|claims| claims.client_id)
                .unwrap_or(request.client_id);
            conn.client_id = Some(client_id);
            conn.handshaken = true;
            let server_hello = ServerHello {
                selected_protocol,
                daemon_version: env!("CARGO_PKG_VERSION").to_string(),
                daemon_instance: state.instance.instance_id,
                heartbeat_interval_ms: HEARTBEAT_INTERVAL_MS,
                // Issue the token the verify path above consumes: the client
                // stores it opaquely and presents it on its next ClientHello,
                // resuming this identity across a client-process restart.
                resume_token: Some(codypendent_protocol::ResumeToken(
                    resume::mint_resume_token(&state.secret, client_id, 0),
                )),
                // The running daemon's per-build id, so a connecting client
                // can compare it against its own compile-time `BUILD_ID` and
                // decide whether to restart this daemon (daemon-auto-restart).
                build_id: BUILD_ID.to_string(),
            };
            send(
                writer,
                &Envelope::reply_to(&request, Payload::ServerHello(server_hello)),
            )
            .await?;
        }

        // --- session interaction: requires a prior handshake ---
        Payload::Command(command) => {
            if !conn.handshaken {
                let reply = Envelope::reply_to(
                    &request,
                    Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                        "protocol.handshake-required",
                        "send a ClientHello before session commands",
                        false,
                    )),
                );
                send(writer, &reply).await?;
                return Ok(false);
            }

            match &command.body {
                body @ (CommandBody::InstallUiPlugin { .. }
                | CommandBody::SmokeTestUiPlugin { .. }
                | CommandBody::EnableUiPlugin { .. }
                | CommandBody::ListUiPlugins
                | CommandBody::UpdateUiPlugin { .. }
                | CommandBody::ApproveUiPluginUpdate { .. }
                | CommandBody::RejectUiPluginUpdate { .. }
                | CommandBody::RevokeUiPlugin { .. }
                | CommandBody::RemoveTrustedUiPublisher { .. }) => {
                    let reply = handle_ui_plugin_lifecycle(
                        state,
                        conn.client_id.expect("handshaken connection has client id"),
                        command.command_id,
                        &command.idempotency_key,
                        conn.role,
                        body,
                    )
                    .await;
                    send(writer, &Envelope::reply_to(&request, reply)).await?;
                }
                // Attach is a connection-level concern the write path
                // deliberately rejects; intercept it here.
                CommandBody::AttachSession {
                    session_id,
                    last_seen_sequence,
                    subscriptions,
                    requested_role,
                    repository,
                } => {
                    // The requested role binds to the *connection* even when the
                    // attach itself is rejected (unknown session): role is a
                    // connection-level assertion under the Phase 1 local trust
                    // model, not a per-session grant.
                    conn.role = *requested_role;
                    let attached = handle_attach(
                        state,
                        writer,
                        conn,
                        forwarders,
                        doc_forwarders,
                        &request,
                        *session_id,
                        last_seen_sequence.unwrap_or(0),
                        subscriptions.clone(),
                        repository.clone(),
                    )
                    .await?;
                    // Remember the attachment so a detach presence event fires when
                    // this connection ends (STEP 3.7). De-duplicated by session: a
                    // re-attach on the same connection must not queue a second
                    // detach for the same client+session. A rejected attach must
                    // not be remembered — there is nothing to detach from.
                    if attached && !conn.attached.iter().any(|(s, _)| s == session_id) {
                        conn.attached.push((*session_id, *requested_role));
                    }
                    if attached {
                        attach_remote_ui(
                            state,
                            writer,
                            conn.client_id.expect("handshaken connection has client id"),
                            *session_id,
                            ui_forwarders,
                        )
                        .await?;
                    }
                }
                // IDE context is latest-wins, high-frequency projection state, not
                // a ledger command — upsert it directly and acknowledge, mirroring
                // the AttachSession interception above (Phase 3 STEP 3.4).
                CommandBody::UpdateIdeContext { session_id, update } => {
                    // Read-only clients must not overwrite the IDE-context
                    // projection the run read-path uses for provenance labeling.
                    if conn.role == ClientRole::Observer {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "protocol.role-denied",
                                "an Observer may not update IDE context".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
                    let reply = match crate::projections::upsert_ide_context(
                        &state.pool,
                        *session_id,
                        update,
                        chrono::Utc::now(),
                    )
                    .await
                    {
                        Ok(()) => {
                            let _ = state.remote_ui_context_updates.send(*session_id);
                            Envelope::reply_to(
                                &request,
                                Payload::CommandAccepted {
                                    command_id: command.command_id,
                                    sequence: None,
                                    created_run: None,
                                },
                            )
                        }
                        Err(error) => Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "ide.context-store-failed",
                                error.to_string(),
                                true,
                            )),
                        ),
                    };
                    send(writer, &reply).await?;
                }
                // A collaborative-document mutation is applied to the
                // authoritative Loro document (in `codypendent-knowledge`, reached
                // through the assembly's `DocumentMutator` seam), not the session
                // ledger — so, like `AttachSession`/`UpdateIdeContext`, it is
                // intercepted here rather than flowing through the event write
                // path (Phase 4 STEP 4.3).
                CommandBody::MutateDocument {
                    document_id,
                    mutation,
                } => {
                    // Role gate (the seam additionally enforces the document's
                    // collaboration mode and edit leases; this is the coarse role
                    // gate the daemon owns). An Observer may not mutate at all.
                    // Accepting/rejecting a suggestion *resolves* proposed content
                    // — it can apply an edit — so it mirrors `ResolveApproval`'s
                    // split in `commands.rs`: only an Approver or Controller may
                    // resolve. A Contributor may still propose (`Annotate`) and,
                    // where the mode allows, edit directly.
                    let resolves_suggestion = matches!(
                        mutation,
                        codypendent_protocol::DocumentMutation::AcceptSuggestion { .. }
                            | codypendent_protocol::DocumentMutation::RejectSuggestion { .. }
                    );
                    let permitted = if resolves_suggestion {
                        matches!(conn.role, ClientRole::Approver | ClientRole::Controller)
                    } else {
                        conn.role != ClientRole::Observer
                    };
                    if !permitted {
                        let message = if resolves_suggestion {
                            "only an Approver or Controller may resolve a document suggestion"
                        } else {
                            "an Observer may not mutate documents"
                        };
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "protocol.role-denied",
                                message.to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
                    // With no mutator injected (lib-only server / daemon tests)
                    // document transport is not enabled; reject structurally so the
                    // connection survives, mirroring the executor-less run path.
                    let Some(mutator) = state.mutator.as_ref() else {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "document.transport-unavailable",
                                "document transport is not enabled on this daemon".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    };
                    let mutate = DocumentMutationRequest {
                        document_id: *document_id,
                        mutation: mutation.clone(),
                        client_id: conn.client_id_or(request.client_id),
                    };
                    let reply = match mutator.apply_mutation(mutate).await {
                        Ok(sync) => {
                            // The mutation committed inside the seam; only now does
                            // its sync fan out to the document's subscribers
                            // (persist-before-publish, RULE 2). A subscriber's CRDT
                            // merge is idempotent, so a lost or duplicated sync
                            // self-heals — no watermark is needed here.
                            state.documents.publish(*document_id, sync);
                            Envelope::reply_to(
                                &request,
                                Payload::CommandAccepted {
                                    command_id: command.command_id,
                                    sequence: None,
                                    created_run: None,
                                },
                            )
                        }
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                // Edit-lease acquire/release, intercepted at the connection level
                // like `MutateDocument` (leases live outside the session ledger).
                // A lease is a precursor to writing, so — as with a non-resolving
                // `MutateDocument` — an Observer may not take one.
                CommandBody::AcquireDocumentLease { lease, ttl_seconds } => {
                    if conn.role == ClientRole::Observer {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "protocol.role-denied",
                                "an Observer may not acquire a document lease".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
                    let Some(leaser) = state.leaser.as_ref() else {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "document.transport-unavailable",
                                "document transport is not enabled on this daemon".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    };
                    let acquire = DocumentLeaseRequest {
                        document_id: lease.document_id,
                        block_id: lease.block_id.clone(),
                        ttl: ttl_seconds.map(std::time::Duration::from_secs),
                        client_id: conn.client_id_or(request.client_id),
                    };
                    let reply = match leaser.acquire(acquire).await {
                        Ok(grant) => Envelope::reply_to(
                            &request,
                            Payload::DocumentLeaseGranted {
                                command_id: command.command_id,
                                grant,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                CommandBody::ReleaseDocumentLease { lease_id } => {
                    if conn.role == ClientRole::Observer {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "protocol.role-denied",
                                "an Observer may not release a document lease".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
                    let Some(leaser) = state.leaser.as_ref() else {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "document.transport-unavailable",
                                "document transport is not enabled on this daemon".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    };
                    let release = DocumentLeaseReleaseRequest {
                        lease_id: lease_id.clone(),
                        client_id: conn.client_id_or(request.client_id),
                    };
                    let reply = match leaser.release(release).await {
                        Ok(()) => Envelope::reply_to(
                            &request,
                            Payload::CommandAccepted {
                                command_id: command.command_id,
                                sequence: None,
                                created_run: None,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                // Publishing a document is a Git write (Phase 4 STEP 4.4), so it
                // is gated like the other repository-mutating controls
                // (`CancelRun`/`PauseRun`/`ResumeRun`) rather than the looser
                // "any non-Observer" gate `MutateDocument` uses: only a
                // `Controller` may publish. Intercepted here (like
                // `MutateDocument`/`StartWorkflow`) because a document lives
                // outside the session ledger. The seam only PARKS the approval
                // and returns — nothing is written until a human resolves it via
                // the ordinary `ResolveApproval` command, which the assembly's
                // background task is awaiting.
                CommandBody::PublishDocument {
                    document_id,
                    target,
                } => {
                    if conn.role != ClientRole::Controller {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "protocol.role-denied",
                                "only a Controller may publish a document".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
                    let Some(publisher) = state.publisher.as_ref() else {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "document.transport-unavailable",
                                "document transport is not enabled on this daemon".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    };
                    let publish = PublishDocumentRequest {
                        document_id: *document_id,
                        target: target.clone(),
                        client_id: conn.client_id_or(request.client_id),
                    };
                    let reply = match publisher.publish(publish).await {
                        Ok(parked) => Envelope::reply_to(
                            &request,
                            Payload::DocumentPublishRequested {
                                command_id: command.command_id,
                                approval_id: parked.approval_id,
                                target: parked.target_description,
                                changed_files: parked.changed_files,
                                git_action: parked.git_action,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                // A `StartWorkflow` creates a durable run in the workflow store,
                // which lives outside the session ledger — so, like `MutateDocument`,
                // it is intercepted here and applied through the assembly's
                // `WorkflowStarter` seam rather than the event write path (Phase 5
                // STEP 5.2). Driving the created run is a later step.
                CommandBody::StartWorkflow {
                    manifest,
                    workflow_id,
                    inputs,
                    repository,
                } => {
                    // An Observer may not start a run.
                    if conn.role == ClientRole::Observer {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "protocol.role-denied",
                                "an Observer may not start a workflow".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
                    // With no starter injected (lib-only server / daemon tests)
                    // workflow transport is not enabled; reject structurally so the
                    // connection survives, mirroring the executor-less run path.
                    let Some(starter) = state.starter.as_ref() else {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "workflow.transport-unavailable",
                                "workflow transport is not enabled on this daemon".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    };
                    // Admit this durable workflow run under the shared guard,
                    // exactly like a session `StartRun`: it raises
                    // `active_run_count`, so an idle-guarded shutdown must not
                    // check the count and exit while this admission is in flight.
                    // Refuse (retryable) if a shutdown has already been authorized.
                    let _admit = state.run_admission.read().await;
                    if state
                        .shutting_down
                        .load(std::sync::atomic::Ordering::SeqCst)
                    {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "daemon.restarting",
                                "the daemon is restarting to load a newer build; retry in a moment",
                                true,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
                    let start = StartWorkflowRequest {
                        manifest: manifest.clone(),
                        workflow_id: workflow_id.clone(),
                        inputs: inputs.clone(),
                        // Carry the command's idempotency key so a duplicate
                        // delivery resolves to the same durable run (the write
                        // path's idempotency, applied to this intercepted command).
                        idempotency_key: command.idempotency_key.clone(),
                        // The run carries its own repository root so its agent
                        // nodes' isolated worktrees are carved from the right
                        // checkout (Phase 5 T5); persisted raw so recovery reads
                        // it back. An older client that sends none leaves the node
                        // executor to fall back to the daemon's startup repository
                        // — the fallback is applied at node-execution time, never
                        // resolved from a wandering `current_dir()` here.
                        repository: repository.clone(),
                        client_id: conn.client_id_or(request.client_id),
                    };
                    let reply = match starter.start(start).await {
                        Ok(workflow_run_id) => Envelope::reply_to(
                            &request,
                            Payload::WorkflowRunStarted {
                                command_id: command.command_id,
                                workflow_run_id,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                // Workflow lifecycle control (pause / resume / retry-from-node),
                // intercepted at the connection level like `StartWorkflow` — a
                // workflow run lives outside the session ledger. Each requires the
                // `Controller` role (matching agent-run cancel/pause/resume) and the
                // assembly's `WorkflowLifecycle` seam; the seam performs the
                // synchronous state change and drives the run onward in the
                // background, so the reply is a fast accept/reject (Phase 5 STEP 5.2).
                CommandBody::PauseWorkflow { workflow_run_id } => {
                    if let Some(lifecycle) =
                        workflow_control_seam(state, conn, writer, &request, "pause").await?
                    {
                        let reply = match lifecycle
                            .pause(PauseWorkflowRequest {
                                workflow_run_id: workflow_run_id.clone(),
                                client_id: conn.client_id_or(request.client_id),
                            })
                            .await
                        {
                            Ok(()) => Envelope::reply_to(
                                &request,
                                Payload::CommandAccepted {
                                    command_id: command.command_id,
                                    sequence: None,
                                    created_run: None,
                                },
                            ),
                            Err(error) => {
                                Envelope::reply_to(&request, Payload::CommandRejected(error))
                            }
                        };
                        send(writer, &reply).await?;
                    }
                }
                CommandBody::ResumeWorkflow { workflow_run_id } => {
                    if let Some(lifecycle) =
                        workflow_control_seam(state, conn, writer, &request, "resume").await?
                    {
                        let reply = match lifecycle
                            .resume(ResumeWorkflowRequest {
                                workflow_run_id: workflow_run_id.clone(),
                                client_id: conn.client_id_or(request.client_id),
                            })
                            .await
                        {
                            Ok(()) => Envelope::reply_to(
                                &request,
                                Payload::CommandAccepted {
                                    command_id: command.command_id,
                                    sequence: None,
                                    created_run: None,
                                },
                            ),
                            Err(error) => {
                                Envelope::reply_to(&request, Payload::CommandRejected(error))
                            }
                        };
                        send(writer, &reply).await?;
                    }
                }
                CommandBody::RetryWorkflowNode {
                    workflow_run_id,
                    node_id,
                } => {
                    if let Some(lifecycle) =
                        workflow_control_seam(state, conn, writer, &request, "retry").await?
                    {
                        let reply = match lifecycle
                            .retry_node(RetryWorkflowNodeRequest {
                                workflow_run_id: workflow_run_id.clone(),
                                node_id: node_id.clone(),
                                client_id: conn.client_id_or(request.client_id),
                            })
                            .await
                        {
                            Ok(()) => Envelope::reply_to(
                                &request,
                                Payload::CommandAccepted {
                                    command_id: command.command_id,
                                    sequence: None,
                                    created_run: None,
                                },
                            ),
                            Err(error) => {
                                Envelope::reply_to(&request, Payload::CommandRejected(error))
                            }
                        };
                        send(writer, &reply).await?;
                    }
                }
                // Cancel is the missing control (pause/resume/retry existed; T9): a
                // cooperative drain, Controller-gated like the others, through the same
                // `WorkflowLifecycle` seam. The seam performs the synchronous state
                // change (run → Cancelled, pending nodes → Skipped) and interrupts any
                // in-flight node agent run, so the reply is a fast accept/reject.
                CommandBody::CancelWorkflow { workflow_run_id } => {
                    if let Some(lifecycle) =
                        workflow_control_seam(state, conn, writer, &request, "cancel").await?
                    {
                        let reply = match lifecycle
                            .cancel(CancelWorkflowRequest {
                                workflow_run_id: workflow_run_id.clone(),
                                client_id: conn.client_id_or(request.client_id),
                            })
                            .await
                        {
                            Ok(()) => Envelope::reply_to(
                                &request,
                                Payload::CommandAccepted {
                                    command_id: command.command_id,
                                    sequence: None,
                                    created_run: None,
                                },
                            ),
                            Err(error) => {
                                Envelope::reply_to(&request, Payload::CommandRejected(error))
                            }
                        };
                        send(writer, &reply).await?;
                    }
                }
                // A promotion candidate lives in its own durable store outside
                // the session ledger — so, like `StartWorkflow`, it is
                // intercepted here and applied through the assembly's
                // `PromotionGateway` seam rather than the event write path
                // (Phase 7 STEP 7.5). A draft may be authored by anyone
                // (including an agent/grader), so this is gated like
                // `StartWorkflow` — any role but `Observer`.
                CommandBody::ProposePromotion {
                    kind,
                    name,
                    version,
                    requires_permission_review,
                } => {
                    if conn.role == ClientRole::Observer {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "protocol.role-denied",
                                "an Observer may not propose a promotion candidate".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
                    let Some(gateway) = state.promotion.as_ref() else {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "promotion.transport-unavailable",
                                "promotion transport is not enabled on this daemon".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    };
                    let propose = ProposePromotionRequest {
                        kind: kind.clone(),
                        name: name.clone(),
                        version: *version,
                        requires_permission_review: *requires_permission_review,
                        idempotency_key: command.idempotency_key.clone(),
                        client_id: conn.client_id_or(request.client_id),
                    };
                    let reply = match gateway.propose(propose).await {
                        Ok(candidate_id) => Envelope::reply_to(
                            &request,
                            Payload::PromotionProposed {
                                command_id: command.command_id,
                                candidate_id,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                // Promotion advancement consumes trusted eval/canary evidence
                // and may trigger rollback. Keep that authority on an
                // authenticated human Controller, like approve/rollback.
                CommandBody::AdvancePromotion {
                    candidate_id,
                    action,
                } => {
                    if conn.role != ClientRole::Controller {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "protocol.role-denied",
                                "only a Controller may advance a promotion candidate".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
                    let Some(gateway) = state.promotion.as_ref() else {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "promotion.transport-unavailable",
                                "promotion transport is not enabled on this daemon".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    };
                    let advance = AdvancePromotionRequest {
                        candidate_id: candidate_id.clone(),
                        action: action.clone(),
                        client_id: conn.client_id_or(request.client_id),
                    };
                    let reply = match gateway.advance(advance).await {
                        Ok(()) => Envelope::reply_to(
                            &request,
                            Payload::CommandAccepted {
                                command_id: command.command_id,
                                sequence: None,
                                created_run: None,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                // **The human-approval gate (ADR-010, exit criterion 2).** Only a
                // `Controller` may issue this command; over this local-first
                // socket, a `Controller`-role connection IS the human operator
                // (Chapter 03 / `ConnState::role`'s own doc — the single
                // connecting user is trusted with control), the same mapping
                // `apply_resolve_approval` already uses for `resolved_by`. There
                // is no wire field for a caller to *supply* an actor — the
                // `Actor::Human` constructed just below is the ONLY actor this
                // code path can ever hand to the promotion seam, so an
                // agent/system-initiated approve cannot even be expressed here,
                // let alone succeed; `Candidate::approve` then refuses a
                // non-human actor a second, structural time regardless.
                CommandBody::ApprovePromotion { candidate_id } => {
                    if conn.role != ClientRole::Controller {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "protocol.role-denied",
                                "only a Controller may approve a promotion".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
                    let Some(gateway) = state.promotion.as_ref() else {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "promotion.transport-unavailable",
                                "promotion transport is not enabled on this daemon".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    };
                    let client_id = conn.client_id_or(request.client_id);
                    let approve = ApprovePromotionRequest {
                        candidate_id: candidate_id.clone(),
                        // The ONE place an `Actor::Human` is minted for this
                        // command — from the authenticated connection's role,
                        // never from client-supplied data.
                        approver: codypendent_protocol::Actor::Human {
                            user_id: codypendent_protocol::ids::UserId(client_id.to_string()),
                        },
                        client_id,
                    };
                    let reply = match gateway.approve(approve).await {
                        Ok(()) => Envelope::reply_to(
                            &request,
                            Payload::CommandAccepted {
                                command_id: command.command_id,
                                sequence: None,
                                created_run: None,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                // A manual rollback is likewise `Controller`-only — the engine
                // itself does not require a human actor to roll back (stopping a
                // bad change needs no human), but the daemon still reserves the
                // action to the trusted local operator and attributes it as
                // `Actor::Human`, so it is never confused in the audit trail with
                // the system-attributed auto-rollback a canary regression
                // produces on its own (`AdvancePromotion { ObserveCanary }`).
                CommandBody::RollbackPromotion { candidate_id } => {
                    if conn.role != ClientRole::Controller {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "protocol.role-denied",
                                "only a Controller may roll back a promotion".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
                    let Some(gateway) = state.promotion.as_ref() else {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "promotion.transport-unavailable",
                                "promotion transport is not enabled on this daemon".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    };
                    let client_id = conn.client_id_or(request.client_id);
                    let rollback = RollbackPromotionRequest {
                        candidate_id: candidate_id.clone(),
                        actor: codypendent_protocol::Actor::Human {
                            user_id: codypendent_protocol::ids::UserId(client_id.to_string()),
                        },
                        client_id,
                    };
                    let reply = match gateway.rollback(rollback).await {
                        Ok(()) => Envelope::reply_to(
                            &request,
                            Payload::CommandAccepted {
                                command_id: command.command_id,
                                sequence: None,
                                created_run: None,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                // Reading a workflow run's blackboard is intercepted at the
                // connection level like `StartWorkflow` (the board lives in its own
                // durable store outside the session ledger). Unlike the lifecycle
                // commands this is a READ — an Observer may issue it (there is no
                // client-facing post command; only the workflow executor writes the
                // board) — so it carries no role gate, only the transport check
                // (Phase 5 STEP 5.3).
                CommandBody::ReadBlackboard {
                    workflow_run_id,
                    kind,
                    include_superseded,
                } => {
                    let Some(reader) = state.blackboard_reader.as_ref() else {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "workflow.transport-unavailable",
                                "workflow transport is not enabled on this daemon".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    };
                    let read = ReadBlackboardRequest {
                        workflow_run_id: workflow_run_id.clone(),
                        kind: kind.clone(),
                        include_superseded: *include_superseded,
                        client_id: conn.client_id_or(request.client_id),
                    };
                    let reply = match reader.read(read).await {
                        Ok(items) => Envelope::reply_to(
                            &request,
                            Payload::BlackboardItems {
                                command_id: command.command_id,
                                items,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                // Reading a workflow run's observability snapshot is intercepted at the
                // connection level like `ReadBlackboard` (the run lives in its own
                // durable store outside the session ledger). A READ — any client (an
                // Observer included) may issue it — so it carries no role gate, only the
                // transport check (Phase 5 STEP 5.2 / T9). It is the catch-up baseline a
                // client folds a `Subscription::Workflow` live stream on top of.
                CommandBody::ReadWorkflowRun { workflow_run_id } => {
                    let Some(reader) = state.workflow_reader.as_ref() else {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "workflow.transport-unavailable",
                                "workflow transport is not enabled on this daemon".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    };
                    let read = ReadWorkflowRunRequest {
                        workflow_run_id: workflow_run_id.clone(),
                        client_id: conn.client_id_or(request.client_id),
                    };
                    let reply = match reader.read(read).await {
                        Ok(snapshot) => Envelope::reply_to(
                            &request,
                            Payload::WorkflowRunSnapshot {
                                command_id: command.command_id,
                                snapshot,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                // Every other command flows through the crash-consistent write
                // path under the role recorded at attach (role enforcement is
                // inherited from the pipeline).
                _ => {
                    let ctx = ApplyContext {
                        client_id: conn.client_id_or(request.client_id),
                        role: conn.role,
                    };
                    // Hold the shared admission guard across the whole apply so an
                    // idle-guarded shutdown (`ShutdownIfIdle`) cannot check the run
                    // count and signal shutdown while this command is mid-commit.
                    // A run-admitting command that arrives once shutdown is already
                    // authorized is refused (retryable) rather than admitted into a
                    // daemon about to exit — the client retries against the fresh one.
                    let _admit = state.run_admission.read().await;
                    if admits_run(&command.body)
                        && state
                            .shutting_down
                            .load(std::sync::atomic::Ordering::SeqCst)
                    {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "daemon.restarting",
                                "the daemon is restarting to load a newer build; retry in a moment",
                                true,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
                    let reply_envelope = match state
                        .commands
                        .apply(&state.pool, ctx, command.clone())
                        .await
                    {
                        Ok(outcome) => {
                            // A freshly accepted `StartRun` is handed to the
                            // executor so the run actually EXECUTES rather than
                            // sitting `Queued` forever. Fire-and-forget: the
                            // executor spawns its own task and we never await it.
                            // With no executor injected (lib-only / tests) this
                            // is a no-op — the run stays `Queued`, exactly as
                            // before.
                            // Gate on `newly_applied`: a duplicate `StartRun`
                            // delivery replays the recorded outcome (with the same
                            // `created_run`), and launching again would run two
                            // agent loops for one run. A replayed outcome is never
                            // `newly_applied`, so the executor fires exactly once.
                            if let (true, Some(run_id), Some(executor)) = (
                                outcome.newly_applied,
                                outcome.created_run,
                                state.executor.as_ref(),
                            ) {
                                match &command.body {
                                    CommandBody::StartRun {
                                        session_id,
                                        objective,
                                        mode,
                                        repository,
                                        model,
                                    } => {
                                        executor.spawn_run(RunLaunch {
                                            session_id: *session_id,
                                            run_id,
                                            objective: objective.clone(),
                                            mode: *mode,
                                            // The run carries its own repository
                                            // root so a shared daemon attributes it
                                            // to the right checkout (issue #6 item
                                            // 1); an older client that sends none
                                            // falls back to the daemon's working
                                            // directory.
                                            repository: resolve_run_repository(
                                                repository.as_deref(),
                                            ),
                                            // Carry the operator's pinned model
                                            // (STEP MP2) into the run; `None` lets
                                            // the executor resolve/route as before.
                                            // The classification hard filter still
                                            // governs a pin at execution time.
                                            model: model.clone(),
                                            // The reconstructed prior is built by
                                            // the assembly executor from the
                                            // session ledger at run start
                                            // (continuous-session plan, Task 3),
                                            // not carried here — the daemon cannot
                                            // build the runtime's `TurnItem`s.
                                            prior: Vec::new(),
                                        });
                                    }
                                    // A follow-up CONTINUES the conversation: it
                                    // launched its own run (Task 3), so drive that
                                    // run exactly like a `StartRun`. Its objective
                                    // is the user's text; the assembly executor
                                    // seeds its prior transcript from the session
                                    // ledger. `SubmitUserInput` carries no per-run
                                    // repository or model on the wire (a
                                    // session-level command), so the continuation
                                    // INHERITS the session's provenance from its
                                    // originating `StartRun`: the SAME repository
                                    // (I-1 — a shared daemon whose `current_dir()`
                                    // froze at startup must not silently run a
                                    // follow-up against the wrong checkout) and the
                                    // SAME pinned model (I-2 — a pinned session
                                    // stays pinned). Both are recovered from the
                                    // persisted command ledger; a load failure (or
                                    // a session with no `StartRun`) degrades to the
                                    // legacy `current_dir()` / unpinned fallback.
                                    CommandBody::SubmitUserInput {
                                        session_id,
                                        text,
                                        mode,
                                        model,
                                    } => {
                                        let provenance = crate::commands::session_run_provenance(
                                            &state.pool,
                                            *session_id,
                                        )
                                        .await
                                        .unwrap_or_default();
                                        executor.spawn_run(RunLaunch {
                                            session_id: *session_id,
                                            run_id,
                                            objective: text.clone(),
                                            mode: *mode,
                                            repository: resolve_run_repository(
                                                provenance.repository.as_deref(),
                                            ),
                                            // A mid-conversation re-pin runs on
                                            // exactly the model the operator just
                                            // picked (instant switch, same
                                            // session); with none carried, the
                                            // continuation INHERITS the session's
                                            // current model from provenance (I-2)
                                            // — unchanged behavior.
                                            model: model.clone().or(provenance.model),
                                            prior: Vec::new(),
                                        });
                                    }
                                    _ => {}
                                }
                            }
                            // A `CancelRun` must also reach the LIVE runtime loop:
                            // recording `Cancelled` in the projection does not stop
                            // the agent, so signal the executor's per-run
                            // cancellation token. Idempotent and best-effort — a
                            // no-op with no executor injected or an already-finished
                            // run. (No `newly_applied` gate: cancellation is
                            // idempotent, and a re-delivered cancel should still be
                            // free to stop a run the first delivery raced.)
                            if let (Some(executor), CommandBody::CancelRun { run_id }) =
                                (state.executor.as_ref(), &command.body)
                            {
                                executor.cancel_run(*run_id);
                            }
                            if let Some(executor) = state.executor.as_ref() {
                                match &command.body {
                                    CommandBody::PauseRun { run_id } => executor.pause_run(*run_id),
                                    CommandBody::ResumeRun { run_id } => {
                                        executor.resume_run(*run_id)
                                    }
                                    _ => {}
                                }
                            }
                            // A freshly created session that carries its
                            // repository root warms that repository's code
                            // graph in the background, so the code-graph
                            // edges overlay is populated as soon as the
                            // session opens — not only on the first
                            // `StartRun`. Guarded/fire-and-forget like the
                            // executor dispatch above; never blocks this
                            // reply.
                            if let CommandBody::CreateSession { repository, .. } = &command.body {
                                maybe_scan_repository(state, repository.clone()).await;
                            }
                            let mut env = Envelope::reply_to(
                                &request,
                                Payload::CommandAccepted {
                                    command_id: outcome.command_id,
                                    sequence: outcome.last_sequence,
                                    // Bind the issuing client to exactly the run
                                    // its StartRun created (None otherwise).
                                    created_run: outcome.created_run,
                                },
                            );
                            // Surface the created session id so a fresh client
                            // (`codypendent run`) can learn the session it just
                            // created. The `CommandAccepted` payload is
                            // intentionally minimal; the envelope's `session_id`
                            // field carries this connection-level metadata
                            // (Chapter 03).
                            if let Some(created) = outcome.created_session {
                                env.session_id = Some(created);
                            }
                            env
                        }
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply_envelope).await?;
                }
            }
        }

        // Anything else (including a future `Unknown` payload) is refused
        // structurally; the connection survives.
        Payload::RemoteUi { message } => {
            if !conn.handshaken {
                send(
                    writer,
                    &Envelope::reply_to(
                        &request,
                        Payload::Error(ProtocolError {
                            code: "protocol.handshake-required".to_owned(),
                            message: "send a ClientHello before Remote UI traffic".to_owned(),
                            retryable: false,
                        }),
                    ),
                )
                .await?;
                return Ok(false);
            }
            let Some(session_id) = request.session_id else {
                send(
                    writer,
                    &Envelope::reply_to(
                        &request,
                        Payload::RemoteUi {
                            message: Box::new(broker_error(
                                "Remote UI envelope requires a sessionId",
                            )),
                        },
                    ),
                )
                .await?;
                return Ok(false);
            };
            let Some((_, role)) = conn
                .attached
                .iter()
                .find(|(attached, _)| *attached == session_id)
            else {
                send(
                    writer,
                    &Envelope::reply_to(
                        &request,
                        Payload::RemoteUi {
                            message: Box::new(broker_error(
                                "attach to the session before Remote UI traffic",
                            )),
                        },
                    ),
                )
                .await?;
                return Ok(false);
            };
            let client_id = conn.client_id.expect("handshaken connection has client id");
            match state
                .remote_ui
                .handle_renderer(session_id, client_id, *role, (**message).clone())
            {
                Ok(dispatch) => {
                    if dispatch.renderer_negotiated {
                        let target =
                            message.capabilities.as_ref().and_then(
                                |capabilities| match capabilities.client.as_str() {
                                    "terminal" | "tui" => Some(UiTarget::Terminal),
                                    "web" | "browser" | "vscode" | "desktop" => Some(UiTarget::Web),
                                    _ => None,
                                },
                            );
                        if let (Some(workers), Some(target)) = (&state.remote_ui_workers, target) {
                            workers.ensure_session_target(
                                session_id,
                                target,
                                state.remote_ui.clone(),
                                state.remote_ui_worker_requests.clone(),
                            );
                        }
                    }
                    for direct in dispatch.direct {
                        send(
                            writer,
                            &Envelope::reply_to(
                                &request,
                                Payload::RemoteUi {
                                    message: Box::new(direct),
                                },
                            ),
                        )
                        .await?;
                    }
                    for action in dispatch.actions {
                        mediate_remote_ui_action(state, session_id, action).await;
                    }
                    if !dispatch.subscriptions.is_empty() {
                        warn!(
                            count = dispatch.subscriptions.len(),
                            "renderer attempted worker-only Remote UI subscriptions"
                        );
                    }
                }
                Err(error) => {
                    send(
                        writer,
                        &Envelope::reply_to(
                            &request,
                            Payload::RemoteUi {
                                message: Box::new(broker_error(error)),
                            },
                        ),
                    )
                    .await?;
                }
            }
        }
        other => {
            let reply = Envelope::reply_to(
                &request,
                Payload::Error(ProtocolError {
                    code: "protocol.unsupported-payload".to_string(),
                    message: format!("payload not handled in this phase: {other:?}"),
                    retryable: false,
                }),
            );
            send(writer, &reply).await?;
        }
    }
    Ok(false)
}

async fn handle_ui_plugin_lifecycle(
    state: &Arc<ServerState>,
    client_id: ClientId,
    command_id: codypendent_protocol::CommandId,
    idempotency_key: &str,
    role: ClientRole,
    body: &CommandBody,
) -> Payload {
    let read_only = matches!(body, CommandBody::ListUiPlugins);
    if (!read_only && !matches!(role, ClientRole::Controller | ClientRole::Approver))
        || role == ClientRole::Unknown
    {
        return Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
            "plugin.role-denied",
            "Remote UI plugin lifecycle changes require the Controller or Approver role",
            false,
        ));
    }
    let Some(store) = state.remote_ui_plugins.as_ref() else {
        return Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
            "plugin.runtime-unavailable",
            "the enforcing Remote UI worker runtime is unavailable; plugin management fails closed",
            true,
        ));
    };
    let body_hash = {
        use sha2::{Digest as _, Sha256};
        hex::encode(Sha256::digest(serde_json::to_vec(body).unwrap_or_default()))
    };
    match claim_ui_plugin_command(&state.pool, client_id, idempotency_key, &body_hash).await {
        Ok(Some(reply)) => return reply,
        Ok(None) => {}
        Err(error) => return Payload::CommandRejected(error),
    }
    let result = match body {
        CommandBody::InstallUiPlugin {
            manifest_toml,
            artifact_base64,
            allow_unsigned,
        } => decode_ui_plugin_candidate(manifest_toml, artifact_base64).and_then(
            |(manifest, artifact)| {
                let granted = codypendent_sandbox::CapabilitySet::from_spec(&manifest.capabilities);
                let granted_ui = manifest
                    .ui
                    .as_ref()
                    .map(|ui| ui.requested_capabilities.iter().copied().collect())
                    .unwrap_or_default();
                store
                    .install_disabled(manifest, &artifact, *allow_unsigned, granted, granted_ui)
                    .map(|status| vec![status])
                    .map_err(anyhow::Error::from)
            },
        ),
        CommandBody::SmokeTestUiPlugin { plugin_id } => {
            let Some(workers) = state.remote_ui_workers.as_ref() else {
                return Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                    "plugin.runtime-unavailable",
                    "the enforcing Remote UI worker supervisor is unavailable",
                    true,
                ));
            };
            store
                .smoke_test(
                    plugin_id,
                    workers.supervisor(),
                    state.remote_ui.producer_offer(),
                )
                .await
                .map(|status| vec![status])
                .map_err(anyhow::Error::from)
        }
        CommandBody::EnableUiPlugin {
            plugin_id,
            scope,
            session_id,
        } => store
            .enable(plugin_id, scope, *session_id)
            .map(|status| vec![status])
            .map_err(anyhow::Error::from),
        CommandBody::ListUiPlugins => store.list().map_err(anyhow::Error::from),
        CommandBody::UpdateUiPlugin {
            plugin_id,
            manifest_toml,
            artifact_base64,
            allow_unsigned,
        } => match decode_ui_plugin_candidate(manifest_toml, artifact_base64) {
            Err(error) => Err(error),
            Ok((manifest, artifact)) => match state.remote_ui_workers.as_ref() {
                None => Err(anyhow::anyhow!(
                    "the enforcing Remote UI worker supervisor is unavailable"
                )),
                Some(workers) => {
                    match store.update(plugin_id, manifest, &artifact, *allow_unsigned) {
                        Err(error) => Err(anyhow::Error::from(error)),
                        Ok(staged)
                            if staged
                                .update_permission_diff
                                .as_ref()
                                .is_some_and(|diff| !diff.is_empty()) =>
                        {
                            // Permission/resource expansion remains inert until
                            // the exact human approval receipt is supplied.
                            Ok(vec![staged])
                        }
                        Ok(staged) => match staged.update_approval_receipt {
                            None => Err(anyhow::anyhow!(
                                "safe staged update has no internal receipt"
                            )),
                            Some(receipt) => match store
                                .preflight_pending_update(
                                    plugin_id,
                                    &receipt,
                                    workers.supervisor(),
                                    state.remote_ui.producer_offer(),
                                )
                                .await
                            {
                                Ok(()) => store
                                    .commit_safe_update(plugin_id, &receipt)
                                    .map(|status| vec![status])
                                    .map_err(anyhow::Error::from),
                                Err(error) => {
                                    let _ = store.reject_update(plugin_id, &receipt);
                                    Err(anyhow::Error::from(error))
                                }
                            },
                        },
                    }
                }
            },
        },
        CommandBody::ApproveUiPluginUpdate {
            plugin_id,
            approval_receipt,
        } => match state.remote_ui_workers.as_ref() {
            None => Err(anyhow::anyhow!(
                "the enforcing Remote UI worker supervisor is unavailable"
            )),
            Some(workers) => store
                .preflight_pending_update(
                    plugin_id,
                    approval_receipt,
                    workers.supervisor(),
                    state.remote_ui.producer_offer(),
                )
                .await
                .and_then(|()| store.approve_update(plugin_id, approval_receipt))
                .map(|status| vec![status])
                .map_err(anyhow::Error::from),
        },
        CommandBody::RejectUiPluginUpdate {
            plugin_id,
            approval_receipt,
        } => store
            .reject_update(plugin_id, approval_receipt)
            .map(|status| vec![status])
            .map_err(anyhow::Error::from),
        CommandBody::RevokeUiPlugin { plugin_id } => store
            .revoke(plugin_id)
            .map(|status| vec![status])
            .map_err(anyhow::Error::from),
        CommandBody::RemoveTrustedUiPublisher { publisher_id } => {
            match store.remove_trusted_publisher(publisher_id) {
                Err(error) => Err(anyhow::Error::from(error)),
                Ok(removal) if removal.failures.is_empty() => Ok(removal.plugins),
                Ok(removal) => {
                    // Trust is already durably gone. Revoke broker authority and
                    // cancel every pre-resolved worker even if a secondary
                    // record rewrite failed, then report the repair failures.
                    for status in &removal.plugins {
                        if let Err(error) = state.remote_ui.revoke_plugin(&status.id) {
                            tracing::error!(
                                plugin = status.id,
                                %error,
                                "failed to synchronously revoke Remote UI broker authority"
                            );
                        }
                        if let Some(workers) = state.remote_ui_workers.as_ref() {
                            workers.stop_plugin(&status.id);
                        }
                    }
                    let affected = removal
                        .plugins
                        .iter()
                        .map(|status| status.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    Err(anyhow::anyhow!(
                        "publisher trust was removed and workers were revoked for [{affected}], but lifecycle record repair failed: {}",
                        removal.failures.join("; ")
                    ))
                }
            }
        }
        _ => unreachable!("caller filters plugin lifecycle variants"),
    };
    let reply = match result {
        Ok(statuses) => {
            if !read_only {
                for status in &statuses {
                    if status.state != LifecycleState::UpdateBlocked {
                        if let Err(error) = state.remote_ui.revoke_plugin(&status.id) {
                            tracing::error!(
                                plugin = status.id,
                                %error,
                                "failed to synchronously revoke Remote UI broker authority"
                            );
                        }
                    }
                }
                if let Some(workers) = state.remote_ui_workers.as_ref() {
                    for status in &statuses {
                        if status.state != LifecycleState::UpdateBlocked {
                            workers.stop_plugin(&status.id);
                        }
                    }
                    if statuses
                        .iter()
                        .any(|status| status.state == LifecycleState::Enabled)
                    {
                        for (session_id, target) in state.remote_ui.renderer_session_targets() {
                            workers.ensure_session_target(
                                session_id,
                                target,
                                state.remote_ui.clone(),
                                state.remote_ui_worker_requests.clone(),
                            );
                        }
                    }
                }
            }
            Payload::UiPluginLifecycle {
                command_id,
                plugins: statuses.into_iter().map(ui_plugin_status_wire).collect(),
            }
        }
        Err(error) => Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
            "plugin.lifecycle-refused",
            error.to_string(),
            false,
        )),
    };
    if let Err(error) =
        persist_ui_plugin_command_result(&state.pool, idempotency_key, &body_hash, &reply).await
    {
        error!(%error, "could not persist Remote UI plugin command outcome");
        return Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
            "plugin.result-persistence-failed",
            "the lifecycle effect completed but its retry record could not be persisted; inspect plugin list before retrying",
            true,
        ));
    }
    reply
}

async fn claim_ui_plugin_command(
    pool: &SqlitePool,
    client_id: ClientId,
    idempotency_key: &str,
    body_hash: &str,
) -> Result<Option<Payload>, codypendent_protocol::CodypendentError> {
    let inserted = sqlx::query(
        "INSERT OR IGNORE INTO ui_plugin_commands \
         (client_id, idempotency_key, body_hash, result_json, created_at) VALUES (?, ?, ?, NULL, ?)",
    )
    .bind(client_id.to_string())
    .bind(idempotency_key)
    .bind(body_hash)
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await
    .map_err(|error| {
        codypendent_protocol::CodypendentError::new(
            "plugin.idempotency-store-failed",
            error.to_string(),
            true,
        )
    })?
    .rows_affected();
    if inserted == 1 {
        return Ok(None);
    }
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT body_hash, result_json FROM ui_plugin_commands WHERE idempotency_key = ?",
    )
    .bind(idempotency_key)
    .fetch_optional(pool)
    .await
    .map_err(|error| {
        codypendent_protocol::CodypendentError::new(
            "plugin.idempotency-store-failed",
            error.to_string(),
            true,
        )
    })?;
    let Some((stored_hash, result)) = row else {
        return Err(codypendent_protocol::CodypendentError::new(
            "plugin.idempotency-race",
            "plugin lifecycle retry record disappeared",
            true,
        ));
    };
    if stored_hash != body_hash {
        return Err(codypendent_protocol::CodypendentError::new(
            "plugin.idempotency-conflict",
            "idempotency key was already used for a different plugin lifecycle command",
            false,
        ));
    }
    match result {
        Some(result) => serde_json::from_str(&result).map(Some).map_err(|error| {
            codypendent_protocol::CodypendentError::new(
                "plugin.idempotency-corrupt",
                error.to_string(),
                false,
            )
        }),
        // Every lifecycle effect below is serialized by the per-plugin store
        // transaction and exact-input idempotent. Re-drive an identical NULL
        // claim so a daemon crash after the durable effect but before recording
        // its reply can reconcile immediately after reconnect. Concurrent live
        // duplicates are harmless: one effect wins and the journal's first
        // persisted result remains authoritative.
        None => Ok(None),
    }
}

async fn persist_ui_plugin_command_result(
    pool: &SqlitePool,
    idempotency_key: &str,
    body_hash: &str,
    reply: &Payload,
) -> anyhow::Result<()> {
    let result = serde_json::to_string(reply)?;
    let changed = sqlx::query(
        "UPDATE ui_plugin_commands SET result_json = ? \
         WHERE idempotency_key = ? AND body_hash = ? AND result_json IS NULL",
    )
    .bind(result)
    .bind(idempotency_key)
    .bind(body_hash)
    .execute(pool)
    .await?
    .rows_affected();
    if changed != 1 {
        anyhow::bail!("plugin lifecycle command result lost its idempotency claim");
    }
    Ok(())
}

fn decode_ui_plugin_candidate(
    manifest_toml: &str,
    artifact_base64: &str,
) -> anyhow::Result<(codypendent_sandbox::PluginManifest, Vec<u8>)> {
    if manifest_toml.len() > 1024 * 1024 || artifact_base64.len() > 14 * 1024 * 1024 {
        anyhow::bail!("plugin management payload exceeds host bounds");
    }
    let manifest = codypendent_sandbox::parse_manifest(manifest_toml)?;
    let artifact = base64::engine::general_purpose::STANDARD
        .decode(artifact_base64)
        .map_err(|error| anyhow::anyhow!("plugin artifact is not valid base64: {error}"))?;
    Ok((manifest, artifact))
}

fn ui_plugin_status_wire(
    status: crate::remote_ui_plugins::RemoteUiPluginStatus,
) -> codypendent_protocol::UiPluginLifecycleStatus {
    let state = match status.state {
        LifecycleState::InstalledDisabled => "installed-disabled",
        LifecycleState::SmokeTested => "smoke-tested",
        LifecycleState::Enabled => "enabled",
        LifecycleState::UpdateBlocked => "update-blocked",
        LifecycleState::Revoked => "revoked",
    };
    codypendent_protocol::UiPluginLifecycleStatus {
        id: status.id,
        version: status.version,
        state: state.into(),
        enabled_scope: status.enabled_scope,
        update_approval_receipt: status.update_approval_receipt,
        update_permission_diff: status.update_permission_diff,
    }
}

async fn consume_remote_ui_worker_requests(
    state: Arc<ServerState>,
    mut receiver: mpsc::Receiver<UiWorkerRequest>,
) {
    let mut projections =
        std::collections::HashMap::<(SessionId, UiProducerHandle, String), JoinHandle<()>>::new();
    let mut actions = std::collections::HashMap::<
        (SessionId, UiProducerHandle, codypendent_protocol::UiEventId),
        JoinHandle<()>,
    >::new();
    while let Some(request) = receiver.recv().await {
        actions.retain(|_, handle| !handle.is_finished());
        match request {
            UiWorkerRequest::Action { session_id, action } => {
                let key = (
                    session_id,
                    action.producer.clone(),
                    action.invocation.invocation_id.clone(),
                );
                let state = Arc::clone(&state);
                let handle = tokio::spawn(async move {
                    mediate_remote_ui_action(&state, session_id, action).await;
                });
                if let Some(stale) = actions.insert(key, handle) {
                    stale.abort();
                }
            }
            UiWorkerRequest::Subscription {
                session_id,
                subscription,
            } => {
                let key = (
                    session_id,
                    subscription.producer.clone(),
                    subscription.request.subscription_id.clone(),
                );
                let state = Arc::clone(&state);
                let handle = tokio::spawn(async move {
                    if let Err(error) =
                        run_remote_ui_projection(state, session_id, subscription).await
                    {
                        warn!(%session_id, %error, "Remote UI projection mediator stopped");
                    }
                });
                if let Some(stale) = projections.insert(key, handle) {
                    stale.abort();
                }
            }
            UiWorkerRequest::Unsubscription {
                session_id,
                unsubscription,
            } => {
                let key = (
                    session_id,
                    unsubscription.producer,
                    unsubscription.request.subscription_id,
                );
                if let Some(handle) = projections.remove(&key) {
                    handle.abort();
                }
            }
            UiWorkerRequest::Cancellation {
                session_id,
                cancellation,
            } => {
                let key = (
                    session_id,
                    cancellation.producer,
                    cancellation.cancellation.invocation_id,
                );
                if let Some(handle) = actions.remove(&key) {
                    handle.abort();
                }
            }
            UiWorkerRequest::ProducerStopped {
                session_id,
                producer,
            } => {
                let keys = projections
                    .keys()
                    .filter(|(owned_session, owned_producer, _)| {
                        *owned_session == session_id && owned_producer == &producer
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                for key in keys {
                    if let Some(handle) = projections.remove(&key) {
                        handle.abort();
                    }
                }
                let action_keys = actions
                    .keys()
                    .filter(|(owned_session, owned_producer, _)| {
                        *owned_session == session_id && owned_producer == &producer
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                for key in action_keys {
                    if let Some(handle) = actions.remove(&key) {
                        handle.abort();
                    }
                }
            }
        }
    }
    for handle in projections.into_values() {
        handle.abort();
    }
    for handle in actions.into_values() {
        handle.abort();
    }
}

async fn run_remote_ui_projection(
    state: Arc<ServerState>,
    session_id: SessionId,
    subscription: UiMediatedSubscription,
) -> anyhow::Result<()> {
    let request = subscription.request.clone();
    match request.kind.as_str() {
        "workflow" => {
            let resource = request
                .resource_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("workflow subscription requires resourceId"))?;
            authorize_workflow_resource(&state.pool, session_id, resource).await?;
            let mut live = state.workflows.subscribe(resource);
            let mut revision = 1_u64;
            deliver_remote_ui_projection(
                &state,
                session_id,
                &subscription.producer,
                &request.subscription_id,
                Some(revision),
                read_remote_ui_projection(&state, session_id, &request).await?,
            )?;
            loop {
                match live.recv().await {
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        revision = revision.saturating_add(1);
                        let value = read_remote_ui_projection(&state, session_id, &request).await?;
                        deliver_remote_ui_projection(
                            &state,
                            session_id,
                            &subscription.producer,
                            &request.subscription_id,
                            Some(revision),
                            value,
                        )?;
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                }
                revision = revision.saturating_add(1);
                let value = read_remote_ui_projection(&state, session_id, &request).await?;
                deliver_remote_ui_projection(
                    &state,
                    session_id,
                    &subscription.producer,
                    &request.subscription_id,
                    Some(revision),
                    value,
                )?;
            }
        }
        "session" | "run" | "artifact" => {
            // Subscribe before reading the baseline: a persisted event racing
            // the read is either reflected by it or delivered afterward.
            let mut live = state.subscriptions.subscribe(session_id);
            let initial = read_remote_ui_projection(&state, session_id, &request).await?;
            deliver_remote_ui_projection(
                &state,
                session_id,
                &subscription.producer,
                &request.subscription_id,
                None,
                initial,
            )?;
            loop {
                let event = match live.recv().await {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let value = read_remote_ui_projection(&state, session_id, &request).await?;
                        deliver_remote_ui_projection(
                            &state,
                            session_id,
                            &subscription.producer,
                            &request.subscription_id,
                            None,
                            value,
                        )?;
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                };
                let value = read_remote_ui_projection(&state, session_id, &request).await?;
                deliver_remote_ui_projection(
                    &state,
                    session_id,
                    &subscription.producer,
                    &request.subscription_id,
                    Some(event.sequence),
                    value,
                )?;
            }
        }
        "context" => {
            let mut invalidations = state.remote_ui_context_updates.subscribe();
            let mut revision = 1_u64;
            let value = read_remote_ui_projection(&state, session_id, &request).await?;
            deliver_remote_ui_projection(
                &state,
                session_id,
                &subscription.producer,
                &request.subscription_id,
                Some(revision),
                value,
            )?;
            loop {
                match invalidations.recv().await {
                    Ok(changed) if changed != session_id => continue,
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                }
                revision = revision.saturating_add(1);
                let value = read_remote_ui_projection(&state, session_id, &request).await?;
                deliver_remote_ui_projection(
                    &state,
                    session_id,
                    &subscription.producer,
                    &request.subscription_id,
                    Some(revision),
                    value,
                )?;
            }
        }
        "command" => {
            let value = read_remote_ui_projection(&state, session_id, &request).await?;
            deliver_remote_ui_projection(
                &state,
                session_id,
                &subscription.producer,
                &request.subscription_id,
                Some(1),
                value,
            )
        }
        kind => anyhow::bail!("unsupported Remote UI projection kind {kind:?}"),
    }
}

fn deliver_remote_ui_projection(
    state: &ServerState,
    session_id: SessionId,
    producer: &UiProducerHandle,
    subscription_id: &str,
    revision: Option<u64>,
    (removed, value): (bool, serde_json::Value),
) -> anyhow::Result<()> {
    state.remote_ui.deliver_projection(
        session_id,
        producer,
        codypendent_protocol::UiProjectionUpdate {
            subscription_id: subscription_id.to_owned(),
            revision: revision.map(codypendent_protocol::UiRevision),
            removed,
            value: if removed {
                serde_json::Value::Null
            } else {
                value
            },
        },
    )?;
    Ok(())
}

async fn read_remote_ui_projection(
    state: &ServerState,
    session_id: SessionId,
    request: &codypendent_protocol::UiProjectionSubscription,
) -> anyhow::Result<(bool, serde_json::Value)> {
    match request.kind.as_str() {
        "session" => {
            authorize_session_resource(session_id, request.resource_id.as_deref())?;
            let value = projections::session_projection(&state.pool, session_id).await?;
            let updated_at: Option<(String,)> =
                sqlx::query_as("SELECT updated_at FROM sessions WHERE id = ?")
                    .bind(session_id.to_string())
                    .fetch_optional(&state.pool)
                    .await?;
            Ok((
                false,
                serde_json::to_value(codypendent_protocol::UiSessionProjection {
                    id: value.session_id.to_string(),
                    title: Some(value.title),
                    state: if value.closed { "closed" } else { "open" }.into(),
                    active_run_id: value.active_runs.first().map(ToString::to_string),
                    updated_at: updated_at.map(|(value,)| value),
                })?,
            ))
        }
        "context" => {
            authorize_session_resource(session_id, request.resource_id.as_deref())?;
            match projections::load_ide_context(&state.pool, session_id).await? {
                Some(value) => Ok((
                    false,
                    serde_json::to_value(codypendent_protocol::UiContextProjection {
                        active_file: value.active_file,
                        selection: value.selection.map(serde_json::to_value).transpose()?,
                        open_files: value.open_files,
                        dirty_buffers: value
                            .dirty_buffers
                            .into_iter()
                            .map(serde_json::to_value)
                            .collect::<Result<Vec<_>, _>>()?,
                        diagnostics_revision: value.diagnostics_revision,
                    })?,
                )),
                None => Ok((true, serde_json::Value::Null)),
            }
        }
        "run" => {
            let resource = request
                .resource_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("run subscription requires resourceId"))?;
            let run_id = codypendent_protocol::RunId::from_str(resource)?;
            if projections::run_session(&state.pool, run_id).await? != Some(session_id) {
                anyhow::bail!("run resource does not belong to the broker session");
            }
            let row: Option<RemoteUiRunRow> = sqlx::query_as(
                    "SELECT objective, state, mode, model_policy, budget_json, workspace_lease_id, started_at, ended_at \
                     FROM runs WHERE id = ? AND session_id = ?",
                )
                .bind(resource)
                .bind(session_id.to_string())
                .fetch_optional(&state.pool)
                .await?;
            let Some((
                objective,
                state_name,
                mode,
                model_policy,
                budget_json,
                workspace_lease_id,
                started_at,
                completed_at,
            )) = row
            else {
                return Ok((true, serde_json::Value::Null));
            };
            Ok((
                false,
                serde_json::to_value(codypendent_protocol::UiRunProjection {
                    id: resource.to_owned(),
                    session_id: session_id.to_string(),
                    state: state_name,
                    agent_mode: Some(mode),
                    progress: None,
                    cost: None,
                    started_at,
                    completed_at,
                    data: Some(serde_json::json!({
                        "objective": objective,
                        "modelPolicy": model_policy,
                        "budget": serde_json::from_str::<serde_json::Value>(&budget_json)
                            .unwrap_or(serde_json::Value::Null),
                        "workspaceLeaseId": workspace_lease_id,
                    })),
                })?,
            ))
        }
        "artifact" => read_remote_ui_artifact(state, session_id, request).await,
        "workflow" => {
            let resource = request
                .resource_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("workflow subscription requires resourceId"))?;
            authorize_workflow_resource(&state.pool, session_id, resource).await?;
            let Some(reader) = &state.workflow_reader else {
                anyhow::bail!("workflow projection transport is unavailable");
            };
            let snapshot = reader
                .read(ReadWorkflowRunRequest {
                    workflow_run_id: resource.to_owned(),
                    client_id: ClientId::new(),
                })
                .await
                .map_err(|error| anyhow::anyhow!("{}: {}", error.code, error.message))?;
            let phase = serde_json::to_value(snapshot.phase)?
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("workflow phase is not a wire string"))?
                .to_owned();
            let nodes = snapshot
                .nodes
                .into_iter()
                .map(|node| {
                    let state = serde_json::to_value(node.state)?
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("workflow node state is not a wire string"))?
                        .to_owned();
                    Ok(codypendent_protocol::UiWorkflowNodeProjection {
                        workflow_run_id: node.workflow_run_id,
                        node_id: node.node_id,
                        state,
                        attempt: node.attempt,
                        cost: node.cost,
                        error: node.error,
                        warnings: node.warnings,
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            Ok((
                false,
                serde_json::to_value(codypendent_protocol::UiWorkflowProjection {
                    workflow_run_id: snapshot.workflow_run_id,
                    phase,
                    nodes,
                })?,
            ))
        }
        "command" => {
            let command_id = request
                .resource_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("command subscription requires resourceId"))?;
            let title = match command_id {
                "core.run.pause" => Some("Pause run"),
                "core.run.resume" => Some("Resume run"),
                "core.run.cancel" => Some("Cancel run"),
                _ => None,
            };
            match title {
                Some(title) => Ok((
                    false,
                    serde_json::json!({
                        "id": command_id,
                        "title": title,
                        "enabled": true,
                    }),
                )),
                None => Ok((true, serde_json::Value::Null)),
            }
        }
        kind => anyhow::bail!("unsupported Remote UI projection kind {kind:?}"),
    }
}

fn authorize_session_resource(
    session_id: SessionId,
    resource_id: Option<&str>,
) -> anyhow::Result<()> {
    if resource_id.is_some_and(|resource| resource != session_id.to_string().as_str()) {
        anyhow::bail!("projection resource does not belong to the broker session");
    }
    Ok(())
}

async fn authorize_workflow_resource(
    pool: &SqlitePool,
    session_id: SessionId,
    workflow_run_id: &str,
) -> anyhow::Result<()> {
    let owner: Option<(String,)> = sqlx::query_as(
        "SELECT r.session_id FROM workflow_runs w JOIN runs r ON r.id = w.run_id WHERE w.id = ?",
    )
    .bind(workflow_run_id)
    .fetch_optional(pool)
    .await?;
    // Deny-first: no row, or a row owned by another session, both fail.
    if owner.is_none_or(|(owner,)| owner != session_id.to_string()) {
        anyhow::bail!("workflow resource does not belong to the broker session");
    }
    Ok(())
}

async fn read_remote_ui_artifact(
    state: &ServerState,
    session_id: SessionId,
    request: &codypendent_protocol::UiProjectionSubscription,
) -> anyhow::Result<(bool, serde_json::Value)> {
    let resource = request
        .resource_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("artifact subscription requires resourceId"))?;
    let artifact_id = codypendent_protocol::ArtifactId::from_str(resource)?;
    let row: Option<(String, String, i64, String, String)> = sqlx::query_as(
        "SELECT sha256, media_type, byte_length, classification, provenance_json FROM artifacts WHERE id = ?",
    )
    .bind(resource)
    .fetch_optional(&state.pool)
    .await?;
    let Some((sha256, media_type, byte_length, classification, provenance_json)) = row else {
        return Ok((true, serde_json::Value::Null));
    };
    let provenance: crate::artifacts::Provenance = serde_json::from_str(&provenance_json)?;
    let crate::artifacts::ProvenanceSource::ToolOutput { run_id, .. } = &provenance.source else {
        anyhow::bail!("artifact has no session-bound provenance");
    };
    if projections::run_session(&state.pool, *run_id).await? != Some(session_id) {
        anyhow::bail!("artifact resource does not belong to the broker session");
    }
    let range = remote_ui_artifact_range(&request.parameters, byte_length.max(0) as u64)?;
    let include_content = request
        .parameters
        .get("includeContent")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let content = if include_content {
        let mut file = state.artifacts.open(&state.pool, artifact_id).await?;
        file.seek(std::io::SeekFrom::Start(range.offset)).await?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(range.length)
            .read_to_end(&mut bytes)
            .await?;
        Some(base64::engine::general_purpose::STANDARD.encode(bytes))
    } else {
        None
    };
    Ok((
        false,
        serde_json::to_value(codypendent_protocol::UiArtifactProjection {
            id: resource.to_owned(),
            media_type,
            revision: 1,
            value: serde_json::json!({
                "sha256": sha256,
                "byteLength": byte_length,
                "classification": classification,
                "provenance": provenance,
                "contentBase64": content,
                "range": range,
            }),
            schema: None,
            title: None,
        })?,
    ))
}

fn remote_ui_artifact_range(
    parameters: &std::collections::BTreeMap<String, serde_json::Value>,
    total: u64,
) -> anyhow::Result<codypendent_protocol::UiArtifactProjectionRange> {
    const MAX_BYTES: u64 = 1024 * 1024;
    let max_bytes = parameters
        .get("maxBytes")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(MAX_BYTES)
        .min(MAX_BYTES);
    if max_bytes == 0 {
        anyhow::bail!("artifact maxBytes must be greater than zero");
    }
    let has_page = parameters.contains_key("page") || parameters.contains_key("pageSize");
    let has_range = parameters.contains_key("offset") || parameters.contains_key("length");
    if has_page && has_range {
        anyhow::bail!("artifact projection cannot mix page and range addressing");
    }
    let (offset, requested, page, page_size) = if has_page {
        let page = parameters
            .get("page")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let page_size = parameters
            .get("pageSize")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(max_bytes)
            .min(max_bytes);
        if page_size == 0 {
            anyhow::bail!("artifact pageSize must be greater than zero");
        }
        let offset = page
            .checked_mul(page_size)
            .ok_or_else(|| anyhow::anyhow!("artifact page offset overflow"))?;
        (offset, page_size, Some(page), Some(page_size))
    } else {
        let offset = parameters
            .get("offset")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let length = parameters
            .get("length")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(max_bytes)
            .min(max_bytes);
        (offset, length, None, None)
    };
    if offset > total {
        anyhow::bail!("artifact projection offset exceeds artifact length");
    }
    let length = requested.min(total.saturating_sub(offset));
    Ok(codypendent_protocol::UiArtifactProjectionRange {
        offset,
        length,
        total,
        page,
        page_size,
    })
}

async fn mediate_remote_ui_action(
    state: &Arc<ServerState>,
    session_id: SessionId,
    action: UiMediatedAction,
) {
    let invocation_id = action.invocation.invocation_id.clone();
    let result = match remote_ui_command(&action.invocation) {
        Ok(body) => match ensure_remote_ui_command_session(&state.pool, session_id, &body).await {
            Err(error) => Err(error),
            Ok(()) => match action.requester {
                None => Err(codypendent_protocol::CodypendentError::new(
                    "ui.action.user-context-required",
                    "component commands require an attached user renderer",
                    false,
                )),
                Some((client_id, role)) => {
                    let command_id = codypendent_protocol::CommandId::new();
                    let body_digest = {
                        use sha2::{Digest as _, Sha256};
                        hex::encode(Sha256::digest(
                            serde_json::to_vec(&body).unwrap_or_default(),
                        ))
                    };
                    let command = codypendent_protocol::Command {
                        command_id,
                        idempotency_key: format!(
                            "remote-ui:{session_id}:{}:{}:{}:{body_digest}",
                            action.producer.plugin_id(),
                            action.producer.instance_id(),
                            invocation_id
                        ),
                        expected_revision: None,
                        body: body.clone(),
                    };
                    let outcome = state
                        .commands
                        .apply(&state.pool, ApplyContext { client_id, role }, command)
                        .await;
                    if outcome.is_ok() {
                        if let (Some(executor), CommandBody::CancelRun { run_id }) =
                            (state.executor.as_ref(), &body)
                        {
                            executor.cancel_run(*run_id);
                        }
                        if let Some(executor) = state.executor.as_ref() {
                            match body {
                                CommandBody::PauseRun { run_id } => executor.pause_run(run_id),
                                CommandBody::ResumeRun { run_id } => executor.resume_run(run_id),
                                _ => {}
                            }
                        }
                    }
                    outcome
                }
            },
        },
        Err(error) => Err(error),
    };
    let action_result = match result {
        Ok(outcome) => codypendent_protocol::UiActionResult {
            invocation_id,
            status: "succeeded".to_owned(),
            value: serde_json::to_value(outcome).unwrap_or(serde_json::Value::Null),
            error: None,
        },
        Err(error) => codypendent_protocol::UiActionResult {
            invocation_id,
            status: "failed".to_owned(),
            value: serde_json::Value::Null,
            error: Some(codypendent_protocol::UiRemoteError {
                code: error.code,
                message: error.message,
                recoverable: error.retryable,
                document_id: Some(action.invocation.document_id),
                node_id: Some(action.invocation.source_node_id),
                patch_index: None,
                recovery: None,
                fallback: None,
                details: error.details,
            }),
        },
    };
    if let Err(error) = state
        .remote_ui
        .settle_action(session_id, &action.producer, action_result)
    {
        warn!(%error, "could not settle Remote UI action");
    }
}

async fn ensure_remote_ui_command_session(
    pool: &SqlitePool,
    session_id: SessionId,
    body: &CommandBody,
) -> Result<(), codypendent_protocol::CodypendentError> {
    let run_id = match body {
        CommandBody::PauseRun { run_id }
        | CommandBody::ResumeRun { run_id }
        | CommandBody::CancelRun { run_id } => *run_id,
        _ => return Ok(()),
    };
    match projections::run_session(pool, run_id).await {
        Ok(Some(owner)) if owner == session_id => Ok(()),
        Ok(_) => Err(codypendent_protocol::CodypendentError::new(
            "ui.action.cross-session",
            "the requested run does not belong to the Remote UI broker session",
            false,
        )),
        Err(error) => Err(codypendent_protocol::CodypendentError::new(
            "ui.action.lookup-failed",
            error.to_string(),
            true,
        )),
    }
}

fn remote_ui_command(
    invocation: &codypendent_protocol::UiActionInvocation,
) -> Result<CommandBody, codypendent_protocol::CodypendentError> {
    let run_id = || {
        invocation
            .payload
            .get("runId")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                codypendent_protocol::CodypendentError::new(
                    "ui.action.invalid-payload",
                    "run command requires a runId",
                    false,
                )
            })?
            .parse::<codypendent_protocol::RunId>()
            .map_err(|_| {
                codypendent_protocol::CodypendentError::new(
                    "ui.action.invalid-payload",
                    "runId is not a valid run identifier",
                    false,
                )
            })
    };
    match invocation.action_id.as_str() {
        "run.pause" | "core.run.pause" => Ok(CommandBody::PauseRun { run_id: run_id()? }),
        "run.resume" | "core.run.resume" => Ok(CommandBody::ResumeRun { run_id: run_id()? }),
        "run.cancel" | "core.run.cancel" => Ok(CommandBody::CancelRun { run_id: run_id()? }),
        action_id => Err(codypendent_protocol::CodypendentError::new(
            "ui.action.not-authorized",
            format!("Remote UI action {action_id:?} is not a mediated daemon command"),
            false,
        )),
    }
}

async fn attach_remote_ui(
    state: &Arc<ServerState>,
    writer: &SharedWriter,
    client_id: ClientId,
    session_id: SessionId,
    forwarders: &mut std::collections::HashMap<SessionId, JoinHandle<()>>,
) -> anyhow::Result<()> {
    if let Some(previous) = forwarders.remove(&session_id) {
        previous.abort();
    }
    let subscription = state.remote_ui.subscribe_renderer(session_id, client_id)?;
    let writer = Arc::clone(writer);
    let handle = tokio::spawn(forward_remote_ui(
        writer,
        client_id,
        session_id,
        subscription.receiver,
    ));
    forwarders.insert(session_id, handle);
    Ok(())
}

async fn forward_remote_ui(
    writer: SharedWriter,
    client_id: ClientId,
    session_id: SessionId,
    mut receiver: broadcast::Receiver<UiBrokerFrame>,
) {
    loop {
        let message = match receiver.recv().await {
            Ok(frame) => match frame.target {
                UiBrokerTarget::AllRenderers => frame.message,
                UiBrokerTarget::Renderer(target) if target == client_id => frame.message,
                UiBrokerTarget::Renderer(_) | UiBrokerTarget::Producer(_) => continue,
            },
            Err(broadcast::error::RecvError::Lagged(skipped)) => broker_error(format!(
                "Remote UI fan-out dropped {skipped} messages; request a resync"
            )),
            Err(broadcast::error::RecvError::Closed) => break,
        };
        if send(&writer, &remote_ui_envelope(client_id, session_id, message))
            .await
            .is_err()
        {
            break;
        }
    }
}

fn remote_ui_envelope(
    client_id: ClientId,
    session_id: SessionId,
    message: codypendent_protocol::UiWireMessage,
) -> Envelope {
    let mut envelope = Envelope::request(
        client_id,
        Payload::RemoteUi {
            message: Box::new(message),
        },
    );
    envelope.session_id = Some(session_id);
    envelope
}

/// Register a connection's interest in a session: subscribe to its live stream,
/// reply with catch-up (missed events, or a snapshot when too far behind), and
/// spawn a task that forwards matching future events to this client. Returns
/// whether the attach was accepted (`false` = unknown session, error replied).
#[allow(clippy::too_many_arguments)]
async fn handle_attach(
    state: &Arc<ServerState>,
    writer: &SharedWriter,
    conn: &ConnState,
    forwarders: &mut std::collections::HashMap<SessionId, JoinHandle<()>>,
    doc_forwarders: &mut std::collections::HashMap<SessionId, Vec<JoinHandle<()>>>,
    request: &Envelope,
    session_id: SessionId,
    last_seen: u64,
    subscriptions: Vec<Subscription>,
    repository: Option<String>,
) -> anyhow::Result<bool> {
    // Warm this repository's code graph in the background (guarded,
    // fire-and-forget) so the edges overlay is populated as soon as a session
    // is (re-)attached, not only after the first run. Done up front — before
    // the session-existence check below — so a probing re-attach with a
    // remembered id still warms the graph even on the branch that falls
    // through to creating a fresh session.
    maybe_scan_repository(state, repository).await;

    // Reject an attach to a session this daemon has never seen. An empty
    // catch-up here used to make a typo'd id indistinguishable from a valid
    // empty session — the client then bound a blank UI to a dead id whose
    // every `StartRun` rejected `session-not-found`. Clients that probe a
    // remembered id (the TUI's resume flow) treat a non-`Catchup` reply as
    // "gone" and fall through to creating a fresh session.
    if !ledger::session_exists(&state.pool, session_id).await? {
        let reply = Envelope::reply_to(
            request,
            Payload::Error(ProtocolError {
                code: "protocol.session-not-found".to_string(),
                message: format!("no session {session_id}"),
                retryable: false,
            }),
        );
        send(writer, &reply).await?;
        return Ok(false);
    }

    // Subscribe *before* computing catch-up so an event published during the
    // read cannot slip through the gap. An event committed between subscribing
    // and the window read is then delivered twice — once in catch-up, once on
    // the live receiver — so the forwarder drops anything at or below the
    // catch-up watermark (`current_max`) to avoid a double-render on the
    // attach race.
    let receiver = state.subscriptions.subscribe(session_id);

    // Current max sequence (0 for an empty session).
    let current_max = ledger::next_sequence(&state.pool, session_id)
        .await?
        .saturating_sub(1);
    let gap = current_max.saturating_sub(last_seen);

    let catchup = if gap <= CATCHUP_EVENT_LIMIT {
        // Cap replay at `current_max` — the live forwarder's drop watermark. An
        // event committed between reading `current_max` and this window read
        // has sequence > current_max, so it is NOT dropped by the forwarder;
        // excluding it here keeps it delivered exactly once (live), instead of
        // both in catch-up and live. The window is filtered in SQL so the read
        // costs the gap, not the whole session history.
        let events: Vec<SessionEvent> =
            ledger::load_events_between(&state.pool, session_id, last_seen, current_max).await?;
        Catchup::Events {
            from: last_seen + 1,
            through: current_max,
            events,
        }
    } else {
        let projection = projections::session_projection(&state.pool, session_id).await?;
        Catchup::Snapshot {
            through: current_max,
            projection,
        }
    };
    send(
        writer,
        &Envelope::reply_to(request, Payload::Catchup { catchup }),
    )
    .await?;

    let client_id = conn.client_id_or(request.client_id);

    // Reconcile this session's document forwarders. Abort the ones its *previous*
    // attach spawned first, so a re-attach with a reduced (or empty) `Document`
    // set stops delivering syncs for the documents it no longer names — then spawn
    // the new set. Document syncs ride a separate, document-keyed fan-out (not the
    // session hub) and are delivered as `Payload::DocumentSync`; a subscriber's
    // baseline comes from the document read path, this stream carries the
    // post-subscribe updates it merges. Done before the session forwarder below
    // consumes `writer`/`subscriptions`.
    if let Some(previous) = doc_forwarders.remove(&session_id) {
        for handle in previous {
            handle.abort();
        }
    }
    let new_doc_forwarders: Vec<JoinHandle<()>> = subscriptions
        .iter()
        .filter_map(|subscription| match subscription {
            Subscription::Document { document_id } => {
                let receiver = state.documents.subscribe(*document_id);
                Some(tokio::spawn(forward_document_syncs(
                    Arc::clone(writer),
                    receiver,
                    client_id,
                )))
            }
            Subscription::Blackboard { workflow_run_id } => {
                let receiver = state.blackboards.subscribe(workflow_run_id.clone());
                Some(tokio::spawn(forward_blackboard_posts(
                    Arc::clone(writer),
                    receiver,
                    client_id,
                )))
            }
            Subscription::Workflow { workflow_run_id } => {
                let receiver = state.workflows.subscribe(workflow_run_id.clone());
                Some(tokio::spawn(forward_workflow_events(
                    Arc::clone(writer),
                    receiver,
                    client_id,
                )))
            }
            _ => None,
        })
        .collect();
    if !new_doc_forwarders.is_empty() {
        doc_forwarders.insert(session_id, new_doc_forwarders);
    }

    let writer = Arc::clone(writer);
    let handle = tokio::spawn(forward_events(
        writer,
        receiver,
        subscriptions,
        client_id,
        session_id,
        current_max,
    ));
    if let Some(previous) = forwarders.insert(session_id, handle) {
        previous.abort();
    }

    // Announce this client's arrival so other attached clients (e.g. the TUI
    // during a handoff to VS Code) see it join. Emitted after the forwarder is
    // live so the arriving client also receives its own presence event.
    publish_presence(state, session_id, client_id, conn.role, true).await;
    Ok(true)
}

/// Append a `ClientPresenceChanged` event and fan it out to the session's
/// attached clients (persist-before-publish). A failure is logged, never fatal —
/// presence is a convenience signal, not a correctness gate.
async fn publish_presence(
    state: &Arc<ServerState>,
    session_id: SessionId,
    client_id: ClientId,
    role: ClientRole,
    present: bool,
) {
    match ledger::append_next_event(
        &state.pool,
        session_id,
        &codypendent_protocol::Actor::Client { client_id },
        &codypendent_protocol::EventBody::ClientPresenceChanged {
            client_id,
            role,
            present,
        },
        chrono::Utc::now(),
    )
    .await
    {
        Ok(event) => state.subscriptions.publish(session_id, event),
        Err(error) => tracing::warn!(%error, "could not record client presence"),
    }
}

/// Forward persisted session events to one attached client, filtered by its
/// subscription set. Never blocks the ledger: a lagging receiver skips the
/// missed span (the client re-attaches to catch up) and a vanished client ends
/// the task.
///
/// `catchup_through` is the last sequence the attach reply already delivered
/// (its `through`); events at or below it are dropped here, because subscribing
/// before catch-up can queue an event on the receiver that catch-up also
/// included — forwarding it again would double-render it on the client.
async fn forward_events(
    writer: SharedWriter,
    mut receiver: broadcast::Receiver<SessionEvent>,
    subscriptions: Vec<Subscription>,
    client_id: ClientId,
    session_id: SessionId,
    catchup_through: u64,
) {
    loop {
        match receiver.recv().await {
            Ok(event) => {
                // Already delivered in the catch-up reply — drop the overlap.
                if event.sequence <= catchup_through {
                    continue;
                }
                if !subscription_matches(&subscriptions, &event) {
                    continue;
                }
                let mut envelope = Envelope::request(client_id, Payload::Event(event));
                envelope.session_id = Some(session_id);
                if send(&writer, &envelope).await.is_err() {
                    break; // client gone
                }
            }
            // Slow consumer: skip the dropped span rather than stall the writer.
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Forward a document's live CRDT syncs to one subscribed client, framing each
/// as a [`Payload::DocumentSync`]. Never blocks the publisher: a lagging receiver
/// skips the dropped span (its next merge reconverges — CRDT updates are
/// idempotent snapshots) and a vanished client ends the task. Document syncs are
/// not session-scoped, so the frame carries no `session_id`; the client routes by
/// the sync's own `document_id`.
async fn forward_document_syncs(
    writer: SharedWriter,
    mut receiver: broadcast::Receiver<codypendent_protocol::DocumentSync>,
    client_id: ClientId,
) {
    loop {
        match receiver.recv().await {
            Ok(sync) => {
                let envelope = Envelope::request(client_id, Payload::DocumentSync(sync));
                if send(&writer, &envelope).await.is_err() {
                    break; // client gone
                }
            }
            // Slow consumer: skip the dropped span; the next sync reconverges.
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Forward a workflow run's live blackboard posts to one subscribed client,
/// framing each as a [`Payload::BlackboardPosted`] (Phase 5 STEP 5.3). Never
/// blocks the publisher: a lagging receiver skips the dropped span (its next read
/// of the board reconverges — items merge idempotently by id) and a vanished
/// client ends the task. Board posts are not session-scoped, so the frame carries
/// no `session_id`; the client routes by the item's own `workflow_run_id`.
/// Mirrors [`forward_document_syncs`].
async fn forward_blackboard_posts(
    writer: SharedWriter,
    mut receiver: broadcast::Receiver<codypendent_protocol::BlackboardItemView>,
    client_id: ClientId,
) {
    loop {
        match receiver.recv().await {
            Ok(item) => {
                let envelope = Envelope::request(client_id, Payload::BlackboardPosted(item));
                if send(&writer, &envelope).await.is_err() {
                    break; // client gone
                }
            }
            // Slow consumer: skip the dropped span; the next read reconverges.
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Forward a workflow run's live node-lifecycle events to one subscribed client,
/// framing each as a [`Payload::WorkflowEvent`] (Phase 5 STEP 5.2 / T9). Never
/// blocks the publisher: a lagging receiver skips the dropped span (its next
/// snapshot read reconverges — each node transition is full-state, merged by
/// `node_id`) and a vanished client ends the task. Workflow events are not
/// session-scoped, so the frame carries no `session_id`; the client routes by the
/// event's own `workflow_run_id`. Mirrors [`forward_blackboard_posts`].
async fn forward_workflow_events(
    writer: SharedWriter,
    mut receiver: broadcast::Receiver<codypendent_protocol::WorkflowEvent>,
    client_id: ClientId,
) {
    loop {
        match receiver.recv().await {
            Ok(event) => {
                let envelope = Envelope::request(client_id, Payload::WorkflowEvent { event });
                if send(&writer, &envelope).await.is_err() {
                    break; // client gone
                }
            }
            // Slow consumer: skip the dropped span; the next snapshot reconverges.
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Whether an event should be forwarded given a client's subscriptions. Phase 1
/// mapping: `SessionSummary`/`AgentActivity` receive every event; a
/// `RunTrace{run_id}` receives only that run's events; an empty set receives
/// everything. Views without a Phase 1 event mapping match nothing on their own.
fn subscription_matches(subscriptions: &[Subscription], event: &SessionEvent) -> bool {
    if subscriptions.is_empty() {
        return true;
    }
    subscriptions.iter().any(|subscription| match subscription {
        Subscription::SessionSummary | Subscription::AgentActivity => true,
        Subscription::RunTrace { run_id } => event_run_id(event) == Some(*run_id),
        _ => false,
    })
}

/// The run an event belongs to, if any (run-scoped events carry `run_id`).
fn event_run_id(event: &SessionEvent) -> Option<codypendent_protocol::RunId> {
    use codypendent_protocol::EventBody::*;
    match &event.body {
        RunStarted { run_id, .. }
        | RunStateChanged { run_id, .. }
        | ModelStreamDelta { run_id, .. }
        | ToolProposed { run_id, .. }
        | ToolStarted { run_id, .. }
        | ToolCompleted { run_id, .. }
        | PatchProposed { run_id, .. }
        | SteeringQueued { run_id }
        | SteeringApplied { run_id }
        | BudgetWarning { run_id, .. }
        | RunCompleted { run_id, .. } => Some(*run_id),
        _ => None,
    }
}

/// Frame one envelope onto the shared write half.
/// The shared gate for a workflow-lifecycle command (pause / resume / retry): the
/// `Controller` role is required and the assembly's `WorkflowLifecycle` seam must
/// be wired. On either failure this frames the rejection onto `writer` and returns
/// `None`; otherwise it returns the seam the caller drives the command through.
/// `verb` names the action in the role-denied message.
async fn workflow_control_seam<'a>(
    state: &'a Arc<ServerState>,
    conn: &ConnState,
    writer: &SharedWriter,
    request: &Envelope,
    verb: &str,
) -> anyhow::Result<Option<&'a Arc<dyn WorkflowLifecycle>>> {
    if conn.role != ClientRole::Controller {
        let reply = Envelope::reply_to(
            request,
            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                "protocol.role-denied",
                format!("only a Controller may {verb} a workflow run"),
                false,
            )),
        );
        send(writer, &reply).await?;
        return Ok(None);
    }
    let Some(lifecycle) = state.lifecycle.as_ref() else {
        let reply = Envelope::reply_to(
            request,
            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                "workflow.transport-unavailable",
                "workflow transport is not enabled on this daemon".to_string(),
                false,
            )),
        );
        send(writer, &reply).await?;
        return Ok(None);
    };
    Ok(Some(lifecycle))
}

async fn send(writer: &SharedWriter, envelope: &Envelope) -> Result<(), FrameError> {
    let mut guard = writer.lock().await;
    write_envelope(&mut *guard, envelope).await
}

async fn status(state: &ServerState) -> anyhow::Result<DaemonStatus> {
    let uptime = Utc::now()
        .signed_duration_since(state.started_at)
        .num_seconds()
        .max(0) as u64;
    Ok(DaemonStatus {
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        protocol_version: PROTOCOL_V1,
        instance_id: state.instance.instance_id,
        pid: std::process::id(),
        started_at: state.started_at,
        uptime_seconds: uptime,
        boot_count: state.instance.boot_count,
        database_path: state
            .paths
            .data_dir
            .join("codypendent.db")
            .display()
            .to_string(),
        socket_path: state.paths.socket_path.display().to_string(),
        session_count: ledger::session_count(&state.pool).await?,
        // The running daemon's per-build id and its count of non-terminal
        // runs (daemon-auto-restart): a client uses these to decide whether
        // it is safe to restart this daemon without losing in-flight work.
        build_id: BUILD_ID.to_string(),
        active_run_count: u64::try_from(ledger::active_run_count(&state.pool).await?)?,
    })
}

/// Load the per-user resume-signing secret, creating it (32 random bytes, mode
/// 0600) on first boot. Unix-only in Phase 1.
fn load_or_create_secret(data_dir: &Path) -> anyhow::Result<Vec<u8>> {
    let path = data_dir.join("daemon.secret");
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes.len() >= 32 {
            return Ok(bytes[..32].to_vec());
        }
        // A truncated secret is unusable; regenerate below.
    }
    let secret = random_secret();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Create the file with mode 0600 atomically, so the secret is never briefly
    // world-readable in the TOCTOU window a create-then-chmod would leave open.
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(&secret)?;
    }
    #[cfg(not(unix))]
    std::fs::write(&path, &secret)?;
    Ok(secret)
}

/// 32 random bytes from `/dev/urandom`, or, if that is unavailable, derived from
/// two v4 UUIDs (16 bytes each).
fn random_secret() -> Vec<u8> {
    use std::io::Read;
    let mut buf = [0u8; 32];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        if file.read_exact(&mut buf).is_ok() {
            return buf.to_vec();
        }
    }
    let mut secret = Vec::with_capacity(32);
    secret.extend_from_slice(uuid::Uuid::now_v7().as_bytes());
    secret.extend_from_slice(uuid::Uuid::now_v7().as_bytes());
    secret
}

/// Opaque, daemon-signed resume tokens (STEP 1.11).
///
/// A token is `hex(payload_json) + "." + hex(signature)`, where the signature
/// is HMAC-SHA256 over the payload. HMAC (not an ad-hoc keyed hash: the
/// original `sha256(secret‖payload‖secret)` sandwich has no security proof and
/// invites length-extension-shaped mistakes) with the `Mac` API's
/// constant-time verification (a `==` string compare leaks a timing oracle on
/// the signature prefix). The payload carries the `client_id`, the last
/// observed sequence, and a 24h validity window; verification rejects a
/// tampered signature or an expired token.
mod resume {
    use chrono::{DateTime, Utc};
    use codypendent_protocol::ClientId;
    use hmac::{Hmac, Mac};
    use serde::{Deserialize, Serialize};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;

    /// A resume token is valid for 24 hours from issue.
    const TOKEN_TTL_HOURS: i64 = 24;

    /// The signed claims inside a resume token.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub(super) struct ResumeClaims {
        pub(super) client_id: ClientId,
        pub(super) last_sequence: u64,
        pub(super) issued_at: DateTime<Utc>,
        pub(super) expires_at: DateTime<Utc>,
    }

    /// HMAC-SHA256 over `payload`, keyed by `secret`. HMAC accepts any key
    /// length, so construction cannot fail.
    fn mac(secret: &[u8], payload: &[u8]) -> HmacSha256 {
        let mut mac = HmacSha256::new_from_slice(secret).expect("hmac accepts any key length");
        mac.update(payload);
        mac
    }

    /// The hex-encoded signature for `payload` (mint-side; verification goes
    /// through [`Mac::verify_slice`], never a string compare).
    pub(super) fn sign(secret: &[u8], payload: &[u8]) -> String {
        hex::encode(mac(secret, payload).finalize().into_bytes())
    }

    /// Mint a token binding `client_id` + `last_sequence`, valid for 24h.
    pub(super) fn mint_resume_token(
        secret: &[u8],
        client_id: ClientId,
        last_sequence: u64,
    ) -> String {
        let issued_at = Utc::now();
        let claims = ResumeClaims {
            client_id,
            last_sequence,
            issued_at,
            expires_at: issued_at + chrono::Duration::hours(TOKEN_TTL_HOURS),
        };
        let payload = serde_json::to_vec(&claims).expect("resume claims serialize");
        let signature = sign(secret, &payload);
        format!("{}.{}", hex::encode(&payload), signature)
    }

    /// Verify a token, returning its claims iff the signature matches (in
    /// constant time) and it has not expired. A malformed, tampered, or
    /// expired token yields `None`.
    pub(super) fn verify_resume_token(secret: &[u8], token: &str) -> Option<ResumeClaims> {
        let (payload_hex, signature_hex) = token.split_once('.')?;
        let payload = hex::decode(payload_hex).ok()?;
        let signature = hex::decode(signature_hex).ok()?;
        mac(secret, &payload).verify_slice(&signature).ok()?;
        let claims: ResumeClaims = serde_json::from_slice(&payload).ok()?;
        if claims.expires_at <= Utc::now() {
            return None;
        }
        Some(claims)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        admits_run, claim_ui_plugin_command, persist_ui_plugin_command_result,
        remote_ui_artifact_range, resume,
    };
    use chrono::Utc;
    use codypendent_protocol::{
        AgentMode, ClientId, CommandBody, CommandId, Payload, RunId, SessionId,
    };

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

    #[test]
    fn admits_run_covers_exactly_the_run_creating_commands() {
        // The idle-guard refuses these once a shutdown is authorized — they are
        // the only commands that can raise `active_run_count`.
        assert!(admits_run(&CommandBody::StartRun {
            session_id: SessionId::new(),
            objective: "x".to_string(),
            mode: AgentMode::Build,
            repository: None,
            model: None,
        }));
        assert!(admits_run(&CommandBody::SubmitUserInput {
            session_id: SessionId::new(),
            text: "x".to_string(),
            mode: AgentMode::Build,
            model: None,
        }));
        // A run-state transition mutates an existing run — it never admits a
        // new one, so it stays allowed even mid-shutdown.
        assert!(!admits_run(&CommandBody::CancelRun {
            run_id: RunId::new(),
        }));
        assert!(!admits_run(&CommandBody::PauseRun {
            run_id: RunId::new(),
        }));
    }

    #[test]
    fn resume_token_round_trips() {
        let client_id = ClientId::new();
        let token = resume::mint_resume_token(SECRET, client_id, 42);
        let claims = resume::verify_resume_token(SECRET, &token).expect("valid token verifies");
        assert_eq!(claims.client_id, client_id);
        assert_eq!(claims.last_sequence, 42);
    }

    #[test]
    fn tampered_signature_is_rejected() {
        let token = resume::mint_resume_token(SECRET, ClientId::new(), 1);
        let mut chars: Vec<char> = token.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == 'a' { 'b' } else { 'a' };
        let tampered: String = chars.into_iter().collect();
        assert!(resume::verify_resume_token(SECRET, &tampered).is_none());
    }

    #[test]
    fn wrong_secret_is_rejected() {
        let token = resume::mint_resume_token(SECRET, ClientId::new(), 1);
        assert!(resume::verify_resume_token(b"a-different-secret-of-32-bytes!!", &token).is_none());
    }

    #[test]
    fn expired_token_is_rejected() {
        let claims = resume::ResumeClaims {
            client_id: ClientId::new(),
            last_sequence: 5,
            issued_at: Utc::now() - chrono::Duration::hours(48),
            expires_at: Utc::now() - chrono::Duration::hours(24),
        };
        let payload = serde_json::to_vec(&claims).unwrap();
        let token = format!(
            "{}.{}",
            hex::encode(&payload),
            resume::sign(SECRET, &payload)
        );
        assert!(resume::verify_resume_token(SECRET, &token).is_none());
    }

    #[test]
    fn artifact_projection_supports_bounded_range_and_page_addressing() {
        let range = std::collections::BTreeMap::from([
            ("offset".to_owned(), serde_json::json!(7)),
            ("length".to_owned(), serde_json::json!(11)),
        ]);
        let range = remote_ui_artifact_range(&range, 100).unwrap();
        assert_eq!((range.offset, range.length, range.total), (7, 11, 100));
        assert_eq!(range.page, None);

        let page = std::collections::BTreeMap::from([
            ("maxBytes".to_owned(), serde_json::json!(64)),
            ("page".to_owned(), serde_json::json!(2)),
            ("pageSize".to_owned(), serde_json::json!(16)),
        ]);
        let page = remote_ui_artifact_range(&page, 40).unwrap();
        assert_eq!((page.offset, page.length), (32, 8));
        assert_eq!((page.page, page.page_size), (Some(2), Some(16)));

        let mixed = std::collections::BTreeMap::from([
            ("page".to_owned(), serde_json::json!(1)),
            ("offset".to_owned(), serde_json::json!(1)),
        ]);
        assert!(remote_ui_artifact_range(&mixed, 100).is_err());
        let huge =
            std::collections::BTreeMap::from([("length".to_owned(), serde_json::json!(u64::MAX))]);
        assert_eq!(
            remote_ui_artifact_range(&huge, u64::MAX).unwrap().length,
            1024 * 1024
        );
    }

    #[tokio::test]
    async fn plugin_lifecycle_journal_reconciles_crash_and_replays_across_clients() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE ui_plugin_commands (\
             client_id TEXT NOT NULL, idempotency_key TEXT PRIMARY KEY NOT NULL, \
             body_hash TEXT NOT NULL, result_json TEXT, created_at TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let first_client = ClientId::new();
        let second_client = ClientId::new();
        let key = "ui-plugin:stable-operation";
        let hash = "body-sha256";
        assert!(claim_ui_plugin_command(&pool, first_client, key, hash)
            .await
            .unwrap()
            .is_none());
        // Simulate a crash before result persistence: an exact reconnect is
        // allowed to re-drive the store's idempotent transition immediately.
        assert!(claim_ui_plugin_command(&pool, second_client, key, hash)
            .await
            .unwrap()
            .is_none());

        let command_id = CommandId::new();
        let reply = Payload::UiPluginLifecycle {
            command_id,
            plugins: Vec::new(),
        };
        persist_ui_plugin_command_result(&pool, key, hash, &reply)
            .await
            .unwrap();
        assert!(matches!(
            claim_ui_plugin_command(&pool, second_client, key, hash)
                .await
                .unwrap(),
            Some(Payload::UiPluginLifecycle { command_id: replayed, .. }) if replayed == command_id
        ));
        let conflict = claim_ui_plugin_command(&pool, second_client, key, "different-body")
            .await
            .unwrap_err();
        assert_eq!(conflict.code, "plugin.idempotency-conflict");
    }
}
