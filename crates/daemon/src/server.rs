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
    board_scope_id, read_envelope, write_envelope, Actor, Catchup, ClientId, ClientRole, Command,
    CommandBody, CommandId, DaemonStatus, DataClassification, Envelope, EventBody, FrameError,
    InputBlock, Payload, ProtocolError, ServerHello, SessionEvent, SessionId, Subscription,
    BUILD_ID, PROTOCOL_V1,
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
use crate::blackboard::{
    BlackboardHub, BlackboardReader, BlackboardWriter, BoardTarget, PostBlackboardRequest,
    ReadBlackboardRequest, UpdateBlackboardRequest,
};
use crate::commands::{ApplyContext, CommandProcessor};
use crate::documents::{
    DocsCheckRequest, DocumentCreateRequest, DocumentCreator, DocumentHub,
    DocumentLeaseReleaseRequest, DocumentLeaseRequest, DocumentLeaser, DocumentMaintainer,
    DocumentMutationRequest, DocumentMutator, DocumentPublisher, PublishDocumentRequest,
};
use crate::executor::{RunExecutor, RunLaunch};
use crate::instance::InstanceRecord;
use crate::ledger;
use crate::principal::PeerPrincipal;
use crate::projections;
use crate::promotion::{
    AdvancePromotionRequest, ApprovePromotionRequest, PromotionGateway, ProposePromotionRequest,
    RollbackPromotionRequest, SubmitEvalEvidenceRequest,
};
use crate::remote_ui::{
    broker_error, RemoteUiBroker, UiBrokerFrame, UiBrokerTarget, UiMediatedAction,
    UiMediatedSubscription, UiProducerHandle,
};
use crate::remote_ui_plugins::{system_remote_ui_runtime, RemoteUiPluginStore};
use crate::remote_ui_workers::{RemoteUiWorkerService, UiWorkerRequest};
use crate::subscriptions::SubscriptionHub;
use crate::transcription::Transcriber;
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
const MAX_INTEGRATION_ISSUES: usize = 128;
const MAX_INTEGRATION_ISSUE_CHARS: usize = 512;

/// Live, process-scoped health for optional integrations assembled above the
/// daemon crate. Reports are sanitized and de-duplicated before they can cross
/// the status protocol; raw provider/server output and secrets stay in logs.
#[derive(Clone, Default)]
pub struct IntegrationHealth {
    issues: Arc<std::sync::RwLock<Vec<String>>>,
}

impl IntegrationHealth {
    /// Record one actionable integration issue. Terminal controls and bidi
    /// overrides are discarded so a local config value cannot inject status UI.
    pub fn report(&self, issue: impl AsRef<str>) {
        let issue = sanitize_health_issue(issue.as_ref());
        if issue.is_empty() {
            return;
        }
        let mut issues = self
            .issues
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if issues.len() < MAX_INTEGRATION_ISSUES && !issues.contains(&issue) {
            issues.push(issue);
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<String> {
        self.issues
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

fn sanitize_health_issue(issue: &str) -> String {
    issue
        .chars()
        .filter(|character| {
            !character.is_control()
                && !matches!(
                    *character as u32,
                    0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069
                )
        })
        .take(MAX_INTEGRATION_ISSUE_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}
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
    /// The uid this daemon process runs as, read off the socket inode it has
    /// just bound (a file's owner is the creating process's effective uid, so
    /// this is exact and needs no libc). It is the owner of last resort: rows
    /// written before migration 0031, and daemon-internal sessions created
    /// outside the command write path, carry no `owner_uid`, and the single
    /// local user the daemon serves is the only principal that can have made
    /// them. See `session_owner_uid`.
    pub daemon_uid: u32,
    /// Optional integration failures projected through `DaemonStatus`.
    pub integration_health: IntegrationHealth,
    pub shutdown: watch::Sender<bool>,
    /// The crash-consistent command write path (persist-before-publish); shares
    /// its [`SubscriptionHub`] with `subscriptions` below.
    pub commands: CommandProcessor,
    /// Per-session event fan-out the server subscribes attached clients to.
    pub subscriptions: SubscriptionHub,
    /// Content-addressed artifact store (`<data_dir>/artifacts`); held here so
    /// the session server owns it for later steps (tool output, chronicles).
    pub artifacts: ArtifactStore,
    /// Serializes the lookup/transaction for connection-level artifact upload
    /// idempotency. The database journal is the durable authority; this lock
    /// prevents two live sockets from racing the same unique key before either
    /// transaction commits.
    pub artifact_uploads: Arc<Mutex<()>>,
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
    /// Creates a collaborative document from an accepted `CreateDocument` (rubric
    /// #4 doc-writer). `None` in a lib-only / test embedding (the command is then
    /// rejected `document.transport-unavailable`); injected together with
    /// `mutator`/`leaser` by the assembly.
    pub creator: Option<Arc<dyn DocumentCreator>>,
    /// Runs the documentation staleness sweep for an accepted `CheckDocuments`
    /// (`/update-docs`, Phase 4 STEP 4.6). `None` in a lib-only / test embedding
    /// (the command is then rejected `document.transport-unavailable`).
    pub maintainer: Option<Arc<dyn DocumentMaintainer>>,
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
    /// Stores a `Controller` client's `PostBlackboardItem` / `UpdateBlackboardItem`
    /// (Phase B kanban). `None` in a lib-only / test embedding (both commands are
    /// then rejected `workflow.transport-unavailable`); the assembly injects a
    /// `codypendent-workflow`-backed implementation over the pool.
    pub blackboard_writer: Option<Arc<dyn BlackboardWriter>>,
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
    /// Backs the client-facing memory inspect/correct/forget commands (outcome
    /// 17). `None` in a lib-only / test embedding (every memory command is then
    /// rejected `memory.transport-unavailable`); the assembly injects a
    /// `codypendent-knowledge`-backed implementation over the pool.
    pub memory: Option<Arc<dyn crate::memory::MemoryGateway>>,
    /// Turns a `SubmitUserInput`'s stored audio into text (voice v1, rubric 8).
    /// `None` in a lib-only / test embedding (an un-transcribed audio envelope is
    /// then rejected `voice.transport-unavailable`); the assembly injects an
    /// implementation over an OpenAI-compatible `/audio/transcriptions` endpoint
    /// (dependency inversion — see [`crate::transcription`]). The classification
    /// gate itself lives in the daemon, never in the implementation.
    pub transcriber: Option<Arc<dyn Transcriber>>,
    /// Serializes the lookup → transcription → command-commit sequence for
    /// voice submissions. The durable command lookup prevents normal retries
    /// from re-transcribing; this lock also closes the concurrent-duplicate
    /// window in which two sockets could both observe a missing key before
    /// either committed it.
    pub voice_resolution: Arc<Mutex<()>>,
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
    run_with_executor_on_and_health(
        listener,
        pool,
        paths,
        instance,
        executor,
        IntegrationHealth::default(),
    )
    .await
}

/// Like [`run_with_executor_on`], with the assembly's live optional-integration
/// health projection attached to daemon status responses.
pub async fn run_with_executor_on_and_health(
    listener: UnixListener,
    pool: SqlitePool,
    paths: RuntimePaths,
    instance: InstanceRecord,
    executor: Option<Arc<dyn RunExecutor>>,
    integration_health: IntegrationHealth,
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
    let creator = executor.as_ref().and_then(|e| e.document_creator());
    let maintainer = executor.as_ref().and_then(|e| e.document_maintainer());
    let starter = executor.as_ref().and_then(|e| e.workflow_starter());
    let lifecycle = executor.as_ref().and_then(|e| e.workflow_lifecycle());
    let promotion = executor.as_ref().and_then(|e| e.promotion_gateway());
    let memory = executor.as_ref().and_then(|e| e.memory_gateway());
    let documents = DocumentHub::new();
    // The blackboard read seam, bundled with the executor by the assembly. Unlike
    // the document hub, the per-run blackboard fan-out is REUSED from the executor
    // (not created fresh): the publisher is the agent loop deep inside the executor,
    // so both sides must share one hub — exactly as `collaborators` shares the
    // `SubscriptionHub`. Absent an executor, a fresh empty hub (never published to).
    let blackboard_reader = executor.as_ref().and_then(|e| e.blackboard_reader());
    let blackboard_writer = executor.as_ref().and_then(|e| e.blackboard_writer());
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
    // The speech-to-text seam (voice v1, rubric 8), bundled with the executor by
    // the assembly exactly like the document/workflow seams. Absent, an audio
    // `InputEnvelope` is refused `voice.transport-unavailable`; plain-text
    // `SubmitUserInput` is untouched.
    let transcriber = executor.as_ref().and_then(|e| e.transcriber());

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

    // The daemon's own uid, taken from the socket inode it just bound. Read
    // before the first connection is accepted so no request can observe a
    // half-initialized owner-of-last-resort.
    let daemon_uid = daemon_uid_from_socket(&paths)?;
    // Adopt every pre-0031 session. These rows predate the ownership column and
    // can only have been created by the local user this daemon serves, so
    // stamping them once at boot makes the column self-describing instead of
    // leaving the gate to infer the same thing on every request.
    let adopted = sqlx::query("UPDATE sessions SET owner_uid = ? WHERE owner_uid IS NULL")
        .bind(i64::from(daemon_uid))
        .execute(&pool)
        .await?
        .rows_affected();
    if adopted > 0 {
        info!(
            sessions = adopted,
            uid = daemon_uid,
            "adopted pre-0031 sessions for the local user"
        );
    }
    // The same one-shot adoption for pre-0033 workflow runs. Without it every
    // such row has neither a bound session nor an owner uid, and
    // `principal_may_read_workflow` fails them closed — which would make a
    // pre-upgrade user's existing workflow runs abruptly unreadable.
    let adopted_workflows =
        sqlx::query("UPDATE workflow_runs SET owner_uid = ? WHERE owner_uid IS NULL")
            .bind(i64::from(daemon_uid))
            .execute(&pool)
            .await?
            .rows_affected();
    if adopted_workflows > 0 {
        info!(
            workflow_runs = adopted_workflows,
            uid = daemon_uid,
            "adopted pre-0033 workflow runs for the local user"
        );
    }

    let state = Arc::new(ServerState {
        pool,
        paths: paths.clone(),
        instance,
        started_at: Utc::now(),
        daemon_uid,
        integration_health,
        shutdown: shutdown_tx,
        commands,
        subscriptions,
        artifacts,
        artifact_uploads: Arc::new(Mutex::new(())),
        secret,
        executor,
        documents,
        mutator,
        leaser,
        creator,
        maintainer,
        starter,
        lifecycle,
        publisher,
        promotion,
        memory,
        blackboards,
        blackboard_reader,
        blackboard_writer,
        workflows,
        workflow_reader,
        transcriber,
        voice_resolution: Arc::new(Mutex::new(())),
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

/// The uid this daemon process runs as, read from the socket inode it has just
/// bound. A file's owner is the effective uid of the process that created it,
/// so this is exact — and it needs no `libc`/`unsafe` (the workspace denies
/// `unsafe_code`) and no new dependency.
fn daemon_uid_from_socket(paths: &RuntimePaths) -> anyhow::Result<u32> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = std::fs::metadata(&paths.socket_path).map_err(|error| {
        anyhow::anyhow!(
            "cannot determine the daemon's own uid from {}: {error}",
            paths.socket_path.display()
        )
    })?;
    Ok(metadata.uid())
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
    /// **Who this connection is**, derived by the server from the socket's peer
    /// credentials at accept time (outcome 19). Never read from a frame, never
    /// mutated after construction: it is the only thing on this struct a client
    /// cannot choose. Every ownership decision and every `Actor::Human` the
    /// daemon records comes from here.
    principal: PeerPrincipal,
    /// A **correlation** token — from `ClientHello` (its envelope, or a valid
    /// resume token). `None` until the connection handshakes. It ties reconnects,
    /// presence and event attribution together and confers no authority: a client
    /// choosing someone else's `client_id` gains nothing by it.
    client_id: Option<ClientId>,
    /// The role applied to commands on this connection. A handshaken local
    /// client defaults to [`ClientRole::Controller`]: it is already the owning
    /// principal (peer uid), so it may create sessions and control its own runs
    /// without a prior attach. An explicit `AttachSession` may narrow (or
    /// re-assert) the role — e.g. an observer-only view — but the role only ever
    /// *subtracts*: it cannot reach a session the principal does not own, so
    /// asserting `Approver` grants nothing the principal did not already have.
    role: ClientRole,
    /// Whether a `ClientHello` has been seen (session interaction requires it).
    handshaken: bool,
    /// Sessions this connection is attached to, with the role it attached under.
    /// On disconnect a `ClientPresenceChanged { present: false }` is published for
    /// each, so other clients see it leave (Phase 3 STEP 3.7).
    attached: Vec<(SessionId, ClientRole)>,
}

impl ConnState {
    fn new(principal: PeerPrincipal) -> Self {
        Self {
            principal,
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
    // Establish the principal from the TRANSPORT, before a single byte of the
    // client's is read. `SO_PEERCRED` is filled in by the kernel at connect(2)
    // from the connecting process's credentials, so this is the one fact about
    // the caller it cannot choose. Fail closed: for a connected AF_UNIX socket
    // this cannot legitimately fail, so a failure means something we do not
    // understand, and an unidentified connection gets served nothing.
    let principal = match PeerPrincipal::from_stream(&stream) {
        Ok(principal) => principal,
        Err(error) => {
            warn!(%error, "refusing a connection with no derivable peer credentials");
            return Ok(());
        }
    };
    let (mut read_half, write_half) = stream.into_split();
    let writer: SharedWriter = Arc::new(Mutex::new(write_half));
    let mut conn = ConnState::new(principal);
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

// ---------------------------------------------------------------------------
// Voice v1 (rubric 8): client-uploaded artifacts + the transcription seam.
// ---------------------------------------------------------------------------

/// Cap on one `PutArtifact` upload's DECODED bytes.
///
/// The transport already bounds a frame at
/// [`MAX_FRAME_BYTES`](codypendent_protocol::MAX_FRAME_BYTES) (16 MiB), which is
/// ample for the target case — roughly a minute of 16 kHz mono PCM WAV is ~2 MB.
/// This lower decoded cap leaves headroom for base64's 4/3 expansion plus the
/// enclosing JSON, so an upload that would not survive framing is refused with a
/// legible error instead of tearing the connection down. Mirrors the
/// `InstallUiPlugin` precedent, which bounds its own base64 payload the same way.
const MAX_PUT_ARTIFACT_BYTES: usize = 10 * 1024 * 1024;

/// Store one client-uploaded blob in the content-addressed artifact store and
/// reply with the minted [`ArtifactRef`](codypendent_protocol::ArtifactRef).
///
/// Controller-gated: an upload is operator-supplied *input*, so an Observer (or
/// a connection that never asserted a role) must not be able to write bytes into
/// the daemon's store. The classification travels on the command and is recorded
/// verbatim on the occurrence row — the store never guesses it from the bytes,
/// and a later occurrence of the same bytes never inherits an earlier row's
/// (lower) classification.
#[allow(clippy::too_many_arguments)]
async fn handle_put_artifact(
    state: &Arc<ServerState>,
    role: ClientRole,
    client_id: ClientId,
    command_id: CommandId,
    idempotency_key: &str,
    media_type: &str,
    bytes_base64: &str,
    sensitivity: DataClassification,
) -> Payload {
    if role != ClientRole::Controller {
        return Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
            "artifact.role-denied",
            "storing an artifact requires the Controller role",
            false,
        ));
    }
    // Refuse on the ENCODED length first: decoding a hostile payload to learn it
    // is too big would allocate the very memory the cap exists to bound.
    if bytes_base64.len() > MAX_PUT_ARTIFACT_BYTES / 3 * 4 + 4 {
        return Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
            "artifact.too-large",
            format!("an uploaded artifact may not exceed {MAX_PUT_ARTIFACT_BYTES} bytes"),
            false,
        ));
    }
    let bytes = match base64::engine::general_purpose::STANDARD.decode(bytes_base64) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                "artifact.malformed-base64",
                format!("uploaded bytes are not valid base64: {error}"),
                false,
            ));
        }
    };
    if bytes.len() > MAX_PUT_ARTIFACT_BYTES {
        return Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
            "artifact.too-large",
            format!("an uploaded artifact may not exceed {MAX_PUT_ARTIFACT_BYTES} bytes"),
            false,
        ));
    }
    let request_hash = {
        use sha2::{Digest as _, Sha256};
        let encoded = match serde_json::to_vec(&(media_type, bytes_base64, sensitivity)) {
            Ok(encoded) => encoded,
            Err(error) => {
                return Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                    "artifact.request-invalid",
                    format!("could not canonicalize the artifact upload: {error}"),
                    false,
                ));
            }
        };
        hex::encode(Sha256::digest(encoded))
    };
    let _upload_guard = state.artifact_uploads.lock().await;
    match state
        .artifacts
        .put_user_upload_idempotent(
            &state.pool,
            client_id,
            command_id,
            idempotency_key,
            &request_hash,
            media_type,
            sensitivity,
            &bytes,
        )
        .await
    {
        Ok(upload) => Payload::ArtifactStored {
            command_id: upload.command_id,
            artifact: upload.artifact,
        },
        Err(crate::artifacts::ArtifactUploadError::Conflict) => {
            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                "artifact.idempotency-conflict",
                "idempotency key was already used for a different artifact upload",
                false,
            ))
        }
        Err(error) => Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
            "artifact.store-failed",
            format!("could not store the uploaded artifact: {error}"),
            true,
        )),
    }
}

/// What [`resolve_voice_input`] decided about one command.
#[derive(Default)]
struct ResolvedVoiceInput {
    /// The command to apply INSTEAD of the client's, when transcription (or an
    /// envelope-derived objective) changed it. `None` — the overwhelmingly
    /// common case — means "apply exactly what the client sent".
    rewritten: Option<Command>,
    /// What was transcribed, for the note the server appends after the apply.
    transcribed: Option<crate::transcription::TranscribedAudio>,
}

fn has_untranscribed_audio(command: &Command) -> bool {
    matches!(
        &command.body,
        CommandBody::SubmitUserInput {
            envelope: Some(envelope),
            ..
        } if envelope.blocks.iter().any(
            |block| matches!(block, InputBlock::Audio(audio) if audio.transcript.is_none())
        )
    )
}

/// Resolve a `SubmitUserInput`'s [`InputEnvelope`](codypendent_protocol::InputEnvelope)
/// into the text the run will actually execute (voice v1, rubric 8).
///
/// Runs BEFORE the write path so the ledger records the transcript as the run's
/// objective: a run recovered after a crash re-executes text, never silent
/// audio. Un-transcribed [`InputBlock::Audio`] blocks are sent through the
/// daemon's [`transcription`](crate::transcription) seam, whose classification
/// gate refuses off-device transcription of media above the operator's ceiling;
/// that refusal surfaces to the client as an ordinary `CommandRejected`.
///
/// Everything else — every non-`SubmitUserInput` command, and a plain-text
/// submission with no envelope — returns [`ResolvedVoiceInput::default`] and is
/// applied byte-for-byte as the client sent it.
///
async fn resolve_voice_input(
    state: &Arc<ServerState>,
    command: &Command,
) -> Result<ResolvedVoiceInput, codypendent_protocol::CodypendentError> {
    let CommandBody::SubmitUserInput {
        session_id,
        text,
        mode,
        model,
        envelope: Some(envelope),
    } = &command.body
    else {
        return Ok(ResolvedVoiceInput::default());
    };

    let untranscribed = envelope
        .blocks
        .iter()
        .any(|block| matches!(block, InputBlock::Audio(audio) if audio.transcript.is_none()));
    let mut envelope = envelope.clone();
    let transcribed = if untranscribed {
        let Some(transcriber) = state.transcriber.as_ref() else {
            return Err(codypendent_protocol::CodypendentError::new(
                "voice.transport-unavailable",
                "this daemon has no transcriber configured; \
                 add a [transcription] entry to models.toml and restart it",
                true,
            ));
        };
        crate::transcription::transcribe_envelope(
            &state.artifacts,
            &state.pool,
            transcriber.as_ref(),
            &mut envelope,
        )
        .await?
    } else {
        None
    };

    // The objective: whatever the client typed, else everything the (now
    // resolved) envelope says. A client that sends BOTH keeps its own text as
    // the objective — the transcript still rides along on the envelope, linked
    // to its original audio.
    let objective = if text.trim().is_empty() {
        crate::transcription::envelope_text(&envelope)
    } else {
        text.clone()
    };
    if objective.trim().is_empty() {
        return Err(codypendent_protocol::CodypendentError::new(
            "voice.empty-transcript",
            "the submitted input produced no text to run",
            false,
        ));
    }
    if transcribed.is_none() && objective == *text {
        // Nothing changed — leave the client's command exactly as it was.
        return Ok(ResolvedVoiceInput::default());
    }
    Ok(ResolvedVoiceInput {
        rewritten: Some(Command {
            body: CommandBody::SubmitUserInput {
                session_id: *session_id,
                text: objective,
                mode: *mode,
                model: model.clone(),
                envelope: Some(envelope),
            },
            ..command.clone()
        }),
        transcribed,
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
            // A valid resume token restores the prior correlation id; an invalid
            // or expired one is ignored (proceed as a fresh client, do not drop).
            // This is deliberately NOT an authentication step: the connection's
            // identity was already fixed by the kernel at accept time
            // (`conn.principal`). A `client_id` — resumed or self-asserted —
            // correlates frames, presence and idempotency keys and authorizes
            // nothing, so a stolen resume token grants no access.
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
                    if reject_unowned_session(state, conn, writer, &request, *session_id).await? {
                        return Ok(false);
                    }
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
                // Creating a collaborative document goes to the knowledge fabric,
                // not the session ledger, so it is intercepted here exactly like
                // `MutateDocument` below. Creating is a write, so — as with a
                // non-resolving mutation — an Observer may not do it.
                CommandBody::CreateDocument {
                    title,
                    scope,
                    repository,
                    initial_markdown,
                } => {
                    if conn.role == ClientRole::Observer {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "protocol.role-denied",
                                "an Observer may not create documents".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
                    let Some(creator) = state.creator.as_ref() else {
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
                    let create = DocumentCreateRequest {
                        title: title.clone(),
                        scope: scope.clone(),
                        repository: repository.clone(),
                        initial_markdown: initial_markdown.clone(),
                        client_id: conn.client_id_or(request.client_id),
                    };
                    let reply = match creator.create(create).await {
                        Ok(document_id) => Envelope::reply_to(
                            &request,
                            Payload::DocumentCreated {
                                command_id: command.command_id,
                                document_id,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                // The `/update-docs` staleness sweep: it FILES SUGGESTIONS (never
                // direct edits), so it is a write and an Observer may not run it.
                // Intercepted here like the other document commands.
                CommandBody::CheckDocuments {
                    repository,
                    session_id,
                } => {
                    if conn.role == ClientRole::Observer {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "protocol.role-denied",
                                "an Observer may not run the documentation check".to_string(),
                                false,
                            )),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
                    let Some(maintainer) = state.maintainer.as_ref() else {
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
                    let check = DocsCheckRequest {
                        repository: repository.clone(),
                        session_id: *session_id,
                        client_id: conn.client_id_or(request.client_id),
                    };
                    let reply = match maintainer.check(check).await {
                        Ok(report) => Envelope::reply_to(
                            &request,
                            Payload::DocsCheckCompleted {
                                command_id: command.command_id,
                                documents_checked: report.documents_checked,
                                links_resolved: report.links_resolved,
                                stale_findings: report.stale_findings,
                                suggestions_filed: report.suggestions_filed,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
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
                    if reject_unowned_document(state, conn, writer, &request, *document_id).await? {
                        return Ok(false);
                    }
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
                    // Ownership AFTER role + transport, so those refusals keep
                    // their existing meaning; this is the gate a Controller peer
                    // that knows a document id has to pass.
                    if reject_unowned_document(state, conn, writer, &request, lease.document_id)
                        .await?
                    {
                        return Ok(false);
                    }
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
                        // Prefer an explicitly framed attached session, otherwise
                        // use the connection's sole/latest attachment. A one-shot
                        // CLI that only bootstraps its role has no attachment and
                        // intentionally falls back to the publisher's synthetic
                        // session; an attached TUI gets the approval on its own
                        // durable event stream.
                        session_id: request
                            .session_id
                            .filter(|session_id| {
                                conn.attached.iter().any(|(id, _)| id == session_id)
                            })
                            .or_else(|| conn.attached.last().map(|(id, _)| *id)),
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
                        owner_uid: conn.principal.uid(),
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
                        // Ownership AFTER role/transport: a role refusal describes the
                        // connection, not the resource, so it leaks nothing and must keep
                        // its existing contract. This gate is what stops a Controller peer.
                        if reject_unowned_workflow(state, conn, writer, &request, workflow_run_id)
                            .await?
                        {
                            return Ok(false);
                        }
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
                        // Ownership AFTER role/transport: a role refusal describes the
                        // connection, not the resource, so it leaks nothing and must keep
                        // its existing contract. This gate is what stops a Controller peer.
                        if reject_unowned_workflow(state, conn, writer, &request, workflow_run_id)
                            .await?
                        {
                            return Ok(false);
                        }
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
                        if reject_unowned_workflow(state, conn, writer, &request, workflow_run_id)
                            .await?
                        {
                            return Ok(false);
                        }
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
                        // Ownership AFTER role/transport: a role refusal describes the
                        // connection, not the resource, so it leaks nothing and must keep
                        // its existing contract. This gate is what stops a Controller peer.
                        if reject_unowned_workflow(state, conn, writer, &request, workflow_run_id)
                            .await?
                        {
                            return Ok(false);
                        }
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
                    if conn.principal.uid() != state.daemon_uid {
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
                    }
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
                    if conn.principal.uid() != state.daemon_uid {
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
                    }
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
                    if conn.principal.uid() != state.daemon_uid {
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
                    }
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
                        // command — from the connection's peer credentials, so
                        // the audit trail names the OS user that actually
                        // approved rather than a UUID the caller chose.
                        approver: codypendent_protocol::Actor::Human {
                            user_id: conn.principal.user_id(),
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
                    if conn.principal.uid() != state.daemon_uid {
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
                    }
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
                            user_id: conn.principal.user_id(),
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
                // Outcome 17: the curated-memory store lives outside the session
                // ledger, so — like the promotion commands — these are
                // intercepted here and applied through the assembly's
                // `MemoryGateway` seam. Reads are open to any handshaken client;
                // the two destructive verbs are `Controller`-only. The scope
                // gate lives INSIDE the seam, where the memory is fetched, and
                // refuses "not visible" identically to "not there".
                CommandBody::InspectMemory { id, repository } => {
                    let Some(gateway) =
                        memory_seam(state, conn, writer, &request, false, "inspect").await?
                    else {
                        return Ok(false);
                    };
                    let reply = match gateway
                        .inspect(crate::memory::InspectMemoryRequest {
                            id: *id,
                            repository: repository.clone(),
                        })
                        .await
                    {
                        Ok(memory) => Envelope::reply_to(
                            &request,
                            Payload::Memory {
                                command_id: command.command_id,
                                memory,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                CommandBody::CorrectMemory {
                    id,
                    repository,
                    statement,
                    structured_value,
                    confidence,
                } => {
                    let Some(gateway) =
                        memory_seam(state, conn, writer, &request, true, "correct").await?
                    else {
                        return Ok(false);
                    };
                    let reply = match gateway
                        .correct(crate::memory::CorrectMemoryRequest {
                            id: *id,
                            repository: repository.clone(),
                            statement: statement.clone(),
                            structured_value: structured_value.clone(),
                            confidence: *confidence,
                        })
                        .await
                    {
                        Ok(memory) => Envelope::reply_to(
                            &request,
                            Payload::Memory {
                                command_id: command.command_id,
                                memory,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                CommandBody::ForgetMemory { id, repository } => {
                    let Some(gateway) =
                        memory_seam(state, conn, writer, &request, true, "forget").await?
                    else {
                        return Ok(false);
                    };
                    let reply = match gateway
                        .forget(crate::memory::ForgetMemoryRequest {
                            id: Some(*id),
                            repository: repository.clone(),
                            tier: codypendent_protocol::MemoryScopeTier::Unknown,
                        })
                        .await
                    {
                        Ok(forgotten) => Envelope::reply_to(
                            &request,
                            Payload::MemoryForgotten {
                                command_id: command.command_id,
                                forgotten,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                CommandBody::ForgetMemoryScope { repository, tier } => {
                    let Some(gateway) =
                        memory_seam(state, conn, writer, &request, true, "forget").await?
                    else {
                        return Ok(false);
                    };
                    let reply = match gateway
                        .forget(crate::memory::ForgetMemoryRequest {
                            id: None,
                            repository: repository.clone(),
                            tier: *tier,
                        })
                        .await
                    {
                        Ok(forgotten) => Envelope::reply_to(
                            &request,
                            Payload::MemoryForgotten {
                                command_id: command.command_id,
                                forgotten,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                CommandBody::OpenMemoryEvidence {
                    id,
                    repository,
                    evidence_index,
                } => {
                    let Some(gateway) =
                        memory_seam(state, conn, writer, &request, false, "inspect").await?
                    else {
                        return Ok(false);
                    };
                    let reply = match gateway
                        .open_evidence(crate::memory::OpenMemoryEvidenceRequest {
                            id: *id,
                            repository: repository.clone(),
                            evidence_index: *evidence_index,
                        })
                        .await
                    {
                        Ok(evidence) => Envelope::reply_to(
                            &request,
                            Payload::MemoryEvidence {
                                command_id: command.command_id,
                                evidence,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                // The regression evidence a promotion gate consumes must arrive
                // over THIS socket, from an authenticated Controller, so the
                // daemon writes `eval_suite_reports` itself. Before this command
                // existed the CLI opened the daemon's own SQLite file and
                // INSERTed the row directly — which made migration 0017's "the
                // regression verdict is derived from a persisted SuiteReport" a
                // statement about the caller's own claim, not about anything the
                // daemon observed.
                CommandBody::SubmitEvalEvidence {
                    candidate_id,
                    suite,
                    routing_policy,
                    report_json,
                } => {
                    // The promotion store is daemon-wide, exactly like the
                    // memory store, so it belongs to the uid the daemon runs as
                    // and there is no per-row owner to compare against. `role`
                    // is REQUESTED by the client, so it authenticates nothing:
                    // without this a peer able to reach the socket could submit
                    // a syntactically valid, all-passing SuiteReport for any
                    // known candidate and the role-only promotion controls would
                    // then consume that fabricated evidence. The refusal reuses
                    // the transport-unavailable error so a foreign principal
                    // cannot distinguish "not allowed" from "not enabled here".
                    if conn.principal.uid() != state.daemon_uid {
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
                    }
                    if conn.role != ClientRole::Controller {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                                "protocol.role-denied",
                                "only a Controller may submit promotion eval evidence".to_string(),
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
                    let submit = SubmitEvalEvidenceRequest {
                        candidate_id: candidate_id.clone(),
                        suite: suite.clone(),
                        routing_policy: routing_policy.clone(),
                        report_json: report_json.clone(),
                        client_id: conn.client_id_or(request.client_id),
                    };
                    let reply = match gateway.submit_eval_evidence(submit).await {
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
                // commands this is a READ — an Observer may issue it (only a
                // Controller writes, through `PostBlackboardItem` below) — so it
                // carries no role gate, only the transport check (Phase 5 STEP 5.3).
                // `board_repository` re-points the same read at a repository task
                // board (Phase B kanban); the assembly resolves it to the synthetic
                // board run, and an unwritten board reads empty.
                CommandBody::ReadBlackboard {
                    workflow_run_id,
                    kind,
                    include_superseded,
                    board_repository,
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
                    // A repository board read re-points at a synthetic board run
                    // the assembly resolves, so gate on what the client named:
                    // the board repository when present, the workflow run id
                    // otherwise. Both go through the same ownership rule.
                    let gated_id = board_repository
                        .as_deref()
                        .map_or_else(|| workflow_run_id.clone(), board_scope_id);
                    if !principal_may_read_workflow(state, conn.principal, &gated_id).await? {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(workflow_run_not_found(workflow_run_id)),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
                    let read = ReadBlackboardRequest {
                        workflow_run_id: workflow_run_id.clone(),
                        board_repository: board_repository.clone(),
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
                // The two client-facing board WRITES (Phase B kanban), intercepted
                // alongside the read because the board lives outside the session
                // ledger. Unlike the read they are `Controller`-only: a board write
                // is the trusted local operator's, an Observer stays read-only, and
                // an agent writes through its own `blackboard.*` / `task.*` tools
                // (which carry server-built agent attribution) rather than here.
                CommandBody::PostBlackboardItem { scope, item } => {
                    // Every scope, not only WorkflowRun: a repository board is
                    // now owner-checked too (see principal_may_read_workflow),
                    // and scoping this to workflow runs was what left the board
                    // writable by any peer that could name the checkout.
                    if let Some(gated_id) = board_scope_gate_id(scope) {
                        if reject_unowned_workflow(state, conn, writer, &request, &gated_id).await?
                        {
                            return Ok(false);
                        }
                    }
                    let Some(target) = board_target(scope) else {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(unknown_board_scope()),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    };
                    let Some(writer_seam) =
                        board_writer_or_reject(state, conn, &request, writer).await?
                    else {
                        return Ok(false);
                    };
                    let post = PostBlackboardRequest {
                        target,
                        item: item.clone(),
                        client_id: conn.client_id_or(request.client_id),
                    };
                    let reply = match writer_seam.post(post).await {
                        Ok(item) => Envelope::reply_to(
                            &request,
                            Payload::BlackboardItemApplied {
                                command_id: command.command_id,
                                item,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                CommandBody::UpdateBlackboardItem {
                    scope,
                    item_id,
                    status,
                    assignee,
                    ordinal,
                    payload,
                } => {
                    // Every scope, not only WorkflowRun: a repository board is
                    // now owner-checked too (see principal_may_read_workflow),
                    // and scoping this to workflow runs was what left the board
                    // writable by any peer that could name the checkout.
                    if let Some(gated_id) = board_scope_gate_id(scope) {
                        if reject_unowned_workflow(state, conn, writer, &request, &gated_id).await?
                        {
                            return Ok(false);
                        }
                    }
                    let Some(target) = board_target(scope) else {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(unknown_board_scope()),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    };
                    let Some(writer_seam) =
                        board_writer_or_reject(state, conn, &request, writer).await?
                    else {
                        return Ok(false);
                    };
                    let update = UpdateBlackboardRequest {
                        target,
                        item_id: item_id.clone(),
                        status: status.clone(),
                        assignee: assignee.clone(),
                        ordinal: *ordinal,
                        payload: payload.clone(),
                        client_id: conn.client_id_or(request.client_id),
                    };
                    let reply = match writer_seam.update(update).await {
                        Ok(item) => Envelope::reply_to(
                            &request,
                            Payload::BlackboardItemApplied {
                                command_id: command.command_id,
                                item,
                            },
                        ),
                        Err(error) => Envelope::reply_to(&request, Payload::CommandRejected(error)),
                    };
                    send(writer, &reply).await?;
                }
                // One page of a session's durable history. Intercepted here (not run
                // through the command write path) for the same reason as every read:
                // it appends nothing to the ledger. Any attached client — an
                // Observer included — may page; the daemon serves it from the same
                // windowed `(session_id, sequence)` read the attach catch-up uses, so
                // a client >500 events behind can rebuild its transcript instead of
                // being handed a `Catchup::Snapshot` with no history at all.
                CommandBody::ReadSessionEvents {
                    session_id,
                    after_sequence,
                    limit,
                } => {
                    // The gate the review walked straight through: a fresh,
                    // never-attached client read another session's entire
                    // history — prompts, model output, the context manifest.
                    // Ownership is re-derived here, at the FETCH, not inherited
                    // from an attach that may never have happened.
                    if !principal_may_use_session(state, conn.principal, *session_id).await? {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(session_not_found(*session_id)),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
                    let reply = match read_session_events_page(
                        &state.pool,
                        *session_id,
                        *after_sequence,
                        *limit,
                    )
                    .await
                    {
                        Ok((events, through, has_more)) => Envelope::reply_to(
                            &request,
                            Payload::SessionEventsPage {
                                command_id: command.command_id,
                                session_id: *session_id,
                                events,
                                through,
                                has_more,
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
                    if !principal_may_read_workflow(state, conn.principal, workflow_run_id).await? {
                        let reply = Envelope::reply_to(
                            &request,
                            Payload::CommandRejected(workflow_run_not_found(workflow_run_id)),
                        );
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
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
                // Uploading client-captured bytes (voice v1, rubric 8) is a
                // connection-level concern, intercepted here like `AttachSession`:
                // artifacts live in the content-addressed store OUTSIDE the
                // session ledger, so there is no session to append to and no
                // sequence to allocate. The reply carries the minted
                // `ArtifactRef` because the client needs it back — it is what the
                // next `SubmitUserInput`'s audio block references.
                CommandBody::PutArtifact {
                    media_type,
                    bytes_base64,
                    sensitivity,
                } => {
                    let reply = handle_put_artifact(
                        state,
                        conn.role,
                        conn.client_id_or(request.client_id),
                        command.command_id,
                        &command.idempotency_key,
                        media_type,
                        bytes_base64,
                        *sensitivity,
                    )
                    .await;
                    send(writer, &Envelope::reply_to(&request, reply)).await?;
                }
                // Every other command flows through the crash-consistent write
                // path under the role recorded at attach (role enforcement is
                // inherited from the pipeline).
                _ => {
                    // Ownership, before anything else this command might do —
                    // and before the role check inside the pipeline, so a
                    // principal probing another user's ids learns nothing from
                    // the difference between `role-denied` and `not-found`.
                    if let Err(denial) =
                        authorize_command(state, conn.principal, &command.body).await?
                    {
                        let reply = Envelope::reply_to(&request, Payload::CommandRejected(denial));
                        send(writer, &reply).await?;
                        return Ok(false);
                    }
                    let ctx = ApplyContext {
                        client_id: conn.client_id_or(request.client_id),
                        role: conn.role,
                        principal: conn.principal,
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
                    // Voice v1 (rubric 8): a `SubmitUserInput` carrying an audio
                    // `InputEnvelope` is RESOLVED before it is applied. The
                    // transcript has to be what the ledger records as the run's
                    // objective — otherwise a recovered/replayed run would re-run
                    // silent audio — so transcription happens on this side of the
                    // write path and its refusals are ordinary `CommandRejected`s.
                    // Every non-voice command falls straight through untouched.
                    // A voice retry must cross the durable idempotency boundary
                    // BEFORE transcription. Hold the narrow lock through apply
                    // so concurrent copies cannot both observe a missing key and
                    // disclose/process the same audio twice.
                    let _voice_guard = if has_untranscribed_audio(command) {
                        Some(state.voice_resolution.lock().await)
                    } else {
                        None
                    };
                    if _voice_guard.is_some() {
                        match state
                            .commands
                            .replay_existing(&state.pool, &command.idempotency_key)
                            .await
                        {
                            Ok(Some(outcome)) => {
                                let mut reply = Envelope::reply_to(
                                    &request,
                                    Payload::CommandAccepted {
                                        command_id: outcome.command_id,
                                        sequence: outcome.last_sequence,
                                        created_run: outcome.created_run,
                                    },
                                );
                                if let Some(created) = outcome.created_session {
                                    reply.session_id = Some(created);
                                }
                                send(writer, &reply).await?;
                                return Ok(false);
                            }
                            Ok(None) => {}
                            Err(error) => {
                                send(
                                    writer,
                                    &Envelope::reply_to(&request, Payload::CommandRejected(error)),
                                )
                                .await?;
                                return Ok(false);
                            }
                        }
                    }
                    let resolved = match resolve_voice_input(state, command).await {
                        Ok(resolved) => resolved,
                        Err(error) => {
                            send(
                                writer,
                                &Envelope::reply_to(&request, Payload::CommandRejected(error)),
                            )
                            .await?;
                            return Ok(false);
                        }
                    };
                    let transcribed = resolved.transcribed;
                    // The rewritten body (transcript as objective, envelope with
                    // its transcript attached) is what gets persisted and applied;
                    // absent a voice envelope this is the client's exact command.
                    let command = resolved.rewritten.as_ref().unwrap_or(command);
                    let reply_envelope = match state
                        .commands
                        .apply(&state.pool, ctx, command.clone())
                        .await
                    {
                        Ok(outcome) => {
                            // Voice v1: surface the transcription on the session
                            // ledger, so a reader of the transcript sees that this
                            // turn's text came from audio and which model produced
                            // it. Appended BEFORE the executor dispatch below so it
                            // lands immediately after the run's `RunStarted`, and
                            // gated on `newly_applied` so a duplicate delivery does
                            // not double-note. Best-effort: a ledger hiccup must
                            // never fail a run that was already accepted.
                            if let (true, Some(note)) =
                                (outcome.newly_applied, transcribed.as_ref())
                            {
                                if let CommandBody::SubmitUserInput { session_id, .. } =
                                    &command.body
                                {
                                    match ledger::append_next_event(
                                        &state.pool,
                                        *session_id,
                                        &Actor::System,
                                        &EventBody::NoteAppended {
                                            text: note.note(),
                                            run_id: outcome.created_run,
                                        },
                                        Utc::now(),
                                    )
                                    .await
                                    {
                                        Ok(event) => {
                                            state.subscriptions.publish(*session_id, event)
                                        }
                                        Err(error) => {
                                            warn!(%error, "could not record the transcription note")
                                        }
                                    }
                                }
                            }
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
                                    // `envelope` is deliberately ignored here: by
                                    // this point `resolve_voice_input` has already
                                    // folded any transcript into `text`, which is
                                    // what the run's objective must be.
                                    CommandBody::SubmitUserInput {
                                        session_id,
                                        text,
                                        mode,
                                        model,
                                        ..
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
        "blackboard" => {
            let resource = request
                .resource_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("blackboard subscription requires resourceId"))?;
            authorize_workflow_resource(&state.pool, session_id, resource).await?;
            // Subscribe before the baseline read: a post racing the read is
            // either already in it or arrives as the next revision.
            let mut live = state.blackboards.subscribe(resource.to_owned());
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
        "blackboard" => {
            let resource = request
                .resource_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("blackboard subscription requires resourceId"))?;
            authorize_workflow_resource(&state.pool, session_id, resource).await?;
            let Some(reader) = &state.blackboard_reader else {
                anyhow::bail!("blackboard projection transport is unavailable");
            };
            // Read-only: a Remote UI producer observes the board, it never
            // posts to it (only the workflow executor writes).
            let items = reader
                .read(ReadBlackboardRequest {
                    workflow_run_id: resource.to_owned(),
                    // A Remote UI producer addresses a workflow run's board by
                    // its run id; the repository task board is a different
                    // projection kind, so never resolved here.
                    board_repository: None,
                    kind: request
                        .parameters
                        .get("kind")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    include_superseded: request
                        .parameters
                        .get("includeSuperseded")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    client_id: ClientId::new(),
                })
                .await
                .map_err(|error| anyhow::anyhow!("{}: {}", error.code, error.message))?;
            Ok((
                false,
                serde_json::to_value(codypendent_protocol::UiBlackboardProjection {
                    workflow_run_id: resource.to_owned(),
                    items,
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

// --- the connection principal's authority over stored resources ---------------
//
// One rule, one place (outcome 19, F-19-1 / F-19-5 / F-19-8). Every by-id read,
// every subscription, and every command that names a pre-existing resource
// resolves that resource to an owner **in the daemon's own storage** and
// compares it to the connection's kernel-derived principal. Nothing here ever
// reads an identity out of a request.
//
// Every refusal below reuses the resource's ordinary *not-found* error, byte for
// byte. "You may not" and "it does not exist" must be indistinguishable or the
// gate becomes an enumeration oracle — the mistake recorded as F-19-7 on the
// artifact path. `authorize_workflow_resource` below already had this shape and
// is the pattern the rest now follow.

/// The principal that owns `session_id`, or `None` when this daemon has never
/// seen it. A `NULL` `owner_uid` (a pre-0031 row, or a daemon-internal session
/// created outside the command write path) resolves to the daemon's own uid:
/// the single local user it serves is the only principal that could have
/// created one.
async fn session_owner_uid(
    state: &ServerState,
    session_id: SessionId,
) -> anyhow::Result<Option<u32>> {
    Ok(ledger::session_owner_uid(&state.pool, session_id)
        .await?
        .map(|owner| owner.unwrap_or(state.daemon_uid)))
}

/// Whether `principal` may see `session_id` at all. Existence and ownership are
/// decided by the same query and collapse to the same answer, so a caller
/// learns nothing about sessions it does not own.
async fn principal_may_use_session(
    state: &ServerState,
    principal: PeerPrincipal,
    session_id: SessionId,
) -> anyhow::Result<bool> {
    Ok(session_owner_uid(state, session_id)
        .await?
        .is_some_and(|owner| principal.owns(owner)))
}

/// Refuse a connection-level command that names a resource this principal does
/// not own, with the resource's own not-found error so a refusal is
/// indistinguishable from a miss.
///
/// The connection-level branches are intercepted BEFORE the command ledger and
/// therefore never reach [`authorize_command`], which is where by-id ownership
/// is enforced for everything else. The first pass of the multi-uid work gated
/// the read and subscribe paths and left every write and lifecycle path on this
/// side of that fence — so a peer that knew an id could mutate what it could not
/// read. These helpers exist so the check is one call at each site rather than a
/// pattern each site is trusted to remember.
async fn reject_unowned_workflow(
    state: &ServerState,
    conn: &ConnState,
    writer: &SharedWriter,
    request: &Envelope,
    workflow_run_id: &str,
) -> anyhow::Result<bool> {
    if principal_may_read_workflow(state, conn.principal, workflow_run_id).await? {
        return Ok(false);
    }
    let reply = Envelope::reply_to(
        request,
        Payload::CommandRejected(workflow_run_not_found(workflow_run_id)),
    );
    send(writer, &reply).await?;
    Ok(true)
}

async fn reject_unowned_document(
    state: &ServerState,
    conn: &ConnState,
    writer: &SharedWriter,
    request: &Envelope,
    document_id: codypendent_protocol::DocumentId,
) -> anyhow::Result<bool> {
    if principal_may_read_document(state, conn.principal, document_id).await? {
        return Ok(false);
    }
    let reply = Envelope::reply_to(
        request,
        Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
            "document.not-found",
            format!("no document {document_id}"),
            false,
        )),
    );
    send(writer, &reply).await?;
    Ok(true)
}

async fn reject_unowned_session(
    state: &ServerState,
    conn: &ConnState,
    writer: &SharedWriter,
    request: &Envelope,
    session_id: SessionId,
) -> anyhow::Result<bool> {
    if principal_may_use_session(state, conn.principal, session_id).await? {
        return Ok(false);
    }
    let reply = Envelope::reply_to(
        request,
        Payload::CommandRejected(session_not_found(session_id)),
    );
    send(writer, &reply).await?;
    Ok(true)
}

/// Whether `principal` may read a workflow run's observability snapshot or its
/// blackboard, live or by id.
///
/// A **repository task board** (`board:<canonical repo>`) is deliberately
/// allowed: it is a synthetic run with no owning session, it is the shared
/// kanban for a checkout on this machine, and every principal that can reach it
/// is by definition the local user.
///
/// An **unbound workflow run** — one whose `workflow_runs.run_id` is NULL — is
/// governed by its own `owner_uid` (migration 0033). That case is the norm, not
/// an edge: `WorkflowStore::create_run_idempotent` inserts `run_id = NULL`
/// unconditionally, so *every* client-created workflow run is unbound. An
/// earlier version of this gate allowed unbound runs outright on the reasoning
/// that they had no owner to protect — true of the schema as it stood, and
/// wrong the moment you notice which runs are unbound: it made every workflow
/// run in the product readable by any principal that could guess its id.
///
/// A run that IS bound to a session run is governed by that session's owner.
/// Binding can only ever narrow who may read a run, never widen it.
///
/// A row with neither (created before 0033) is adopted for the daemon's own uid
/// once at boot, exactly as pre-0031 sessions are, so this path sees it as
/// owned rather than having to infer the same thing on every request.
async fn principal_may_read_workflow(
    state: &ServerState,
    principal: PeerPrincipal,
    workflow_run_id: &str,
) -> anyhow::Result<bool> {
    if is_repository_board_id(workflow_run_id) {
        // A repository task board is a synthetic run with no owning session and
        // no owner_uid — its id is `board:<canonical repo>`, which any peer that
        // knows the checkout path can construct. I previously returned true here
        // and wrote "deliberately shared" in the comment, which was an assumption
        // about who can reach the socket rather than anything derived: a peer
        // could read and, via the repository-scoped board writes, corrupt another
        // user's kanban.
        //
        // It is daemon-wide state with no per-row owner, exactly like the memory
        // and promotion stores, so it takes the same answer they do: it belongs
        // to the uid the daemon runs as.
        return Ok(principal.uid() == state.daemon_uid);
    }
    match workflow_run_owner(&state.pool, workflow_run_id).await? {
        WorkflowOwner::Session(session_id) => {
            principal_may_use_session(state, principal, session_id).await
        }
        WorkflowOwner::Uid(owner_uid) => Ok(principal.uid() == owner_uid),
        WorkflowOwner::Missing => Ok(false),
    }
}

/// Who owns a workflow run, distinguishing "no such run" from "a real run that
/// was never bound to a session" — a distinction the earlier inner-join lookup
/// collapsed into `None`, which is what made every unbound run unreadable.
enum WorkflowOwner {
    /// The run is bound to a session run; that session's owner governs.
    Session(SessionId),
    /// The run has no session bound, but carries the uid of the principal that
    /// created it (migration 0033). This is the common case.
    Uid(u32),
    /// No such workflow run — or one with neither owner, which cannot survive
    /// boot adoption and so is treated as absent rather than as public.
    Missing,
}

async fn workflow_run_owner(
    pool: &SqlitePool,
    workflow_run_id: &str,
) -> anyhow::Result<WorkflowOwner> {
    // LEFT JOIN, so an unbound run still yields a row (with a NULL session).
    let row: Option<(Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT r.session_id, w.owner_uid FROM workflow_runs w \
         LEFT JOIN runs r ON r.id = w.run_id WHERE w.id = ?",
    )
    .bind(workflow_run_id)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        None => WorkflowOwner::Missing,
        // A bound session governs; it is the narrower of the two.
        Some((Some(id), _)) if SessionId::from_str(&id).is_ok() => {
            WorkflowOwner::Session(SessionId::from_str(&id).expect("checked above"))
        }
        Some((_, Some(uid))) => match u32::try_from(uid) {
            Ok(uid) => WorkflowOwner::Uid(uid),
            Err(_) => WorkflowOwner::Missing,
        },
        // Neither: a pre-0033 row that boot adoption did not reach. Fail closed.
        Some(_) => WorkflowOwner::Missing,
    })
}

/// The id a board scope is authorized against, or `None` for a scope this
/// daemon does not understand (rejected separately by `board_target`).
fn board_scope_gate_id(scope: &codypendent_protocol::BlackboardScope) -> Option<String> {
    match scope {
        codypendent_protocol::BlackboardScope::WorkflowRun { workflow_run_id } => {
            Some(workflow_run_id.clone())
        }
        codypendent_protocol::BlackboardScope::RepositoryBoard { repository } => {
            Some(codypendent_protocol::board_scope_id(repository))
        }
        _ => None,
    }
}

/// Whether `workflow_run_id` names a repository task board rather than a real
/// durable workflow run. Boards are minted by
/// [`codypendent_protocol::board_scope_id`] as `board:<canonical repo>`.
fn is_repository_board_id(workflow_run_id: &str) -> bool {
    workflow_run_id.starts_with("board:")
}

/// The one gate every write-path command passes before it reaches the
/// crash-consistent pipeline: if the command names a resource that already
/// exists, that resource must resolve to a session this principal owns.
///
/// `Ok(())` means "carry on" — either the command names nothing pre-existing
/// (`CreateSession`), or the principal owns what it named. The `Err` is always
/// the resource's own not-found rejection, so a refusal is indistinguishable
/// from a miss.
///
/// **`ResolveApproval` is the reason this function exists.** The review parked a
/// `shell.run ls -la`, resolved it from an unrelated never-attached socket
/// client, and the daemon executed it: the human-in-the-loop gate in front of
/// arbitrary command execution held against nothing. An approval now has to
/// resolve `approval → run → session → owner_uid` and match the connection's
/// peer credentials.
async fn authorize_command(
    state: &ServerState,
    principal: PeerPrincipal,
    body: &CommandBody,
) -> anyhow::Result<Result<(), codypendent_protocol::CodypendentError>> {
    let target = match body {
        CommandBody::StartRun { session_id, .. }
        | CommandBody::SubmitUserInput { session_id, .. } => {
            CommandTarget::Session(*session_id, session_not_found(*session_id))
        }
        CommandBody::CancelRun { run_id }
        | CommandBody::PauseRun { run_id }
        | CommandBody::ResumeRun { run_id }
        | CommandBody::QueueSteering { run_id, .. } => CommandTarget::Run(
            *run_id,
            codypendent_protocol::CodypendentError::new(
                "protocol.run-not-found",
                format!("no run {run_id}"),
                false,
            ),
        ),
        CommandBody::ResolveApproval { approval_id, .. } => CommandTarget::Approval(
            *approval_id,
            codypendent_protocol::CodypendentError::new(
                "approval.not-found",
                format!("no approval {approval_id}"),
                false,
            ),
        ),
        // Nothing pre-existing is named: `CreateSession` mints its own id (and
        // records this principal as its owner), and the remaining bodies in the
        // generic write path carry no session/run/approval reference.
        _ => return Ok(Ok(())),
    };

    let (session_id, denial) = match target {
        CommandTarget::Session(session_id, denial) => (Some(session_id), denial),
        CommandTarget::Run(run_id, denial) => {
            (projections::run_session(&state.pool, run_id).await?, denial)
        }
        CommandTarget::Approval(approval_id, denial) => {
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT r.session_id FROM approvals a JOIN runs r ON a.run_id = r.id WHERE a.id = ?",
            )
            .bind(approval_id.to_string())
            .fetch_optional(&state.pool)
            .await?;
            (row.and_then(|(id,)| SessionId::from_str(&id).ok()), denial)
        }
    };

    // Deny-first: an id that resolves to no session (missing, or an orphaned row)
    // fails exactly as one owned by another principal does.
    let permitted = match session_id {
        Some(session_id) => principal_may_use_session(state, principal, session_id).await?,
        None => false,
    };
    if permitted {
        Ok(Ok(()))
    } else {
        Ok(Err(denial))
    }
}

/// What a write-path command names, paired with the not-found rejection that
/// both "missing" and "not yours" collapse to.
enum CommandTarget {
    Session(SessionId, codypendent_protocol::CodypendentError),
    Run(
        codypendent_protocol::RunId,
        codypendent_protocol::CodypendentError,
    ),
    Approval(
        codypendent_protocol::ApprovalId,
        codypendent_protocol::CodypendentError,
    ),
}

/// Whether `principal` may follow a collaborative document's live CRDT sync.
///
/// Documents carry a knowledge `Scope`, not an owner: in practice they are
/// repository-, system- or organization-scoped (`docs_job::parse_scope`), so
/// there is no per-session owner to re-derive. The honest gate for a
/// single-local-user daemon is therefore the daemon's own uid — a *session*
/// scoped document additionally has to belong to a session this principal owns.
/// An unknown document id is refused like an unowned one.
async fn principal_may_read_document(
    state: &ServerState,
    principal: PeerPrincipal,
    document_id: codypendent_protocol::DocumentId,
) -> anyhow::Result<bool> {
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT scope_tier, scope_key FROM documents WHERE id = ?")
            .bind(document_id.to_string())
            .fetch_optional(&state.pool)
            .await?;
    let Some((scope_tier, scope_key)) = row else {
        return Ok(false);
    };
    if scope_tier == "session" {
        let Some(session_id) = scope_key
            .as_deref()
            .and_then(|key| SessionId::from_str(key).ok())
        else {
            return Ok(false);
        };
        return principal_may_use_session(state, principal, session_id).await;
    }
    Ok(principal.owns(state.daemon_uid))
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
        Ok(body) => match ensure_remote_ui_command_session(state, session_id, &body).await {
            Err(error) => Err(error),
            Ok(owner_uid) => match action.requester {
                None => Err(codypendent_protocol::CodypendentError::new(
                    "ui.action.user-context-required",
                    "component commands require an attached user renderer",
                    false,
                )),
                // Workflow lifecycle control lives outside the session ledger
                // and is served by the `WorkflowLifecycle` seam, exactly as the
                // connection-level `PauseWorkflow`/`CancelWorkflow` path does.
                Some((_, role)) if is_remote_ui_workflow_control(&body) => {
                    apply_remote_ui_workflow_control(state, role, &body).await
                }
                Some((client_id, role)) => {
                    // A plugin acts inside a session, never on its own account,
                    // so its principal is that session's recorded owner — read
                    // out of the daemon's own storage by the authorization step
                    // above, never carried in from the plugin.
                    let principal = PeerPrincipal::from_uid(owner_uid);
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
                        .apply(
                            &state.pool,
                            ApplyContext {
                                client_id,
                                role,
                                principal,
                            },
                            command,
                        )
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

/// Authorize one mediated Remote-UI command against the broker session, and
/// return that session's **owning principal's uid** — the identity the command
/// is then applied under. Returning the owner rather than `()` is what keeps the
/// plugin path from having to name a principal of its own: there is exactly one
/// lookup, in the daemon's storage, and its answer is both the gate and the
/// identity.
async fn ensure_remote_ui_command_session(
    state: &ServerState,
    session_id: SessionId,
    body: &CommandBody,
) -> Result<u32, codypendent_protocol::CodypendentError> {
    let pool = &state.pool;
    let owner_uid = match session_owner_uid(state, session_id).await {
        Ok(Some(owner_uid)) => owner_uid,
        Ok(None) => return Err(session_not_found(session_id)),
        Err(error) => {
            return Err(codypendent_protocol::CodypendentError::new(
                "ui.action.lookup-failed",
                error.to_string(),
                true,
            ))
        }
    };
    let run_id = match body {
        CommandBody::PauseRun { run_id }
        | CommandBody::ResumeRun { run_id }
        | CommandBody::CancelRun { run_id } => *run_id,
        // A workflow run is owned through the agent run that started it; the
        // same join the `workflow` projection authorizes with.
        CommandBody::PauseWorkflow { workflow_run_id }
        | CommandBody::ResumeWorkflow { workflow_run_id }
        | CommandBody::RetryWorkflowNode {
            workflow_run_id, ..
        }
        | CommandBody::CancelWorkflow { workflow_run_id } => {
            return authorize_workflow_resource(pool, session_id, workflow_run_id)
                .await
                .map(|()| owner_uid)
                .map_err(|_| {
                    codypendent_protocol::CodypendentError::new(
                        "ui.action.cross-session",
                        "the requested workflow run does not belong to the Remote UI broker session",
                        false,
                    )
                });
        }
        _ => return Ok(owner_uid),
    };
    match projections::run_session(pool, run_id).await {
        Ok(Some(owner)) if owner == session_id => Ok(owner_uid),
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

/// One allowlisted Remote UI action: the canonical action id a component may
/// invoke and the daemon command it lowers to.
///
/// This table *is* the mediation boundary — an action id absent from it can
/// never reach a command, whatever a component declares. Adding a mediated
/// action is one row; every row still passes through the same ownership check
/// ([`ensure_remote_ui_command_session`]) and the same role gate the equivalent
/// socket command uses.
struct RemoteUiAction {
    /// Canonical action id. The `core.`-prefixed spelling is also accepted, so
    /// `run.pause` and `core.run.pause` name the same command.
    action_id: &'static str,
    lower: fn(
        &codypendent_protocol::UiActionInvocation,
    ) -> Result<CommandBody, codypendent_protocol::CodypendentError>,
}

const REMOTE_UI_ACTIONS: &[RemoteUiAction] = &[
    RemoteUiAction {
        action_id: "run.pause",
        lower: |invocation| {
            Ok(CommandBody::PauseRun {
                run_id: remote_ui_run_id(invocation)?,
            })
        },
    },
    RemoteUiAction {
        action_id: "run.resume",
        lower: |invocation| {
            Ok(CommandBody::ResumeRun {
                run_id: remote_ui_run_id(invocation)?,
            })
        },
    },
    RemoteUiAction {
        action_id: "run.cancel",
        lower: |invocation| {
            Ok(CommandBody::CancelRun {
                run_id: remote_ui_run_id(invocation)?,
            })
        },
    },
    RemoteUiAction {
        action_id: "workflow.pause",
        lower: |invocation| {
            Ok(CommandBody::PauseWorkflow {
                workflow_run_id: remote_ui_workflow_run_id(invocation)?,
            })
        },
    },
    RemoteUiAction {
        action_id: "workflow.resume",
        lower: |invocation| {
            Ok(CommandBody::ResumeWorkflow {
                workflow_run_id: remote_ui_workflow_run_id(invocation)?,
            })
        },
    },
    RemoteUiAction {
        action_id: "workflow.retry_node",
        lower: |invocation| {
            Ok(CommandBody::RetryWorkflowNode {
                workflow_run_id: remote_ui_workflow_run_id(invocation)?,
                node_id: remote_ui_payload_string(invocation, "nodeId")?,
            })
        },
    },
    RemoteUiAction {
        action_id: "workflow.cancel",
        lower: |invocation| {
            Ok(CommandBody::CancelWorkflow {
                workflow_run_id: remote_ui_workflow_run_id(invocation)?,
            })
        },
    },
];

fn remote_ui_payload_string(
    invocation: &codypendent_protocol::UiActionInvocation,
    field: &str,
) -> Result<String, codypendent_protocol::CodypendentError> {
    invocation
        .payload
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            codypendent_protocol::CodypendentError::new(
                "ui.action.invalid-payload",
                format!(
                    "Remote UI action {:?} requires a {field}",
                    invocation.action_id
                ),
                false,
            )
        })
}

fn remote_ui_run_id(
    invocation: &codypendent_protocol::UiActionInvocation,
) -> Result<codypendent_protocol::RunId, codypendent_protocol::CodypendentError> {
    remote_ui_payload_string(invocation, "runId")?
        .parse::<codypendent_protocol::RunId>()
        .map_err(|_| {
            codypendent_protocol::CodypendentError::new(
                "ui.action.invalid-payload",
                "runId is not a valid run identifier",
                false,
            )
        })
}

fn remote_ui_workflow_run_id(
    invocation: &codypendent_protocol::UiActionInvocation,
) -> Result<String, codypendent_protocol::CodypendentError> {
    remote_ui_payload_string(invocation, "workflowRunId")
}

fn remote_ui_command(
    invocation: &codypendent_protocol::UiActionInvocation,
) -> Result<CommandBody, codypendent_protocol::CodypendentError> {
    let action_id = invocation.action_id.as_str();
    let canonical = action_id.strip_prefix("core.").unwrap_or(action_id);
    REMOTE_UI_ACTIONS
        .iter()
        .find(|action| action.action_id == canonical)
        .ok_or_else(|| {
            codypendent_protocol::CodypendentError::new(
                "ui.action.not-authorized",
                format!("Remote UI action {action_id:?} is not a mediated daemon command"),
                false,
            )
        })
        .and_then(|action| (action.lower)(invocation))
}

fn is_remote_ui_workflow_control(body: &CommandBody) -> bool {
    matches!(
        body,
        CommandBody::PauseWorkflow { .. }
            | CommandBody::ResumeWorkflow { .. }
            | CommandBody::RetryWorkflowNode { .. }
            | CommandBody::CancelWorkflow { .. }
    )
}

/// Drive an allowlisted `workflow.*` action through the same
/// [`WorkflowLifecycle`] seam and the same `Controller`-only gate the
/// connection-level workflow commands use. A workflow run lives outside the
/// session ledger, so there is no `CommandService` write path to reuse; the
/// reply mirrors the connection path's fast accept/reject.
async fn apply_remote_ui_workflow_control(
    state: &Arc<ServerState>,
    role: ClientRole,
    body: &CommandBody,
) -> Result<crate::commands::CommandOutcome, codypendent_protocol::CodypendentError> {
    if role != ClientRole::Controller {
        return Err(codypendent_protocol::CodypendentError::new(
            "protocol.role-denied",
            format!("role {role:?} may not control a workflow run"),
            false,
        ));
    }
    let Some(lifecycle) = state.lifecycle.as_ref() else {
        return Err(codypendent_protocol::CodypendentError::new(
            "workflow.transport-unavailable",
            "workflow transport is not enabled on this daemon",
            false,
        ));
    };
    let client_id = ClientId::new();
    match body {
        CommandBody::PauseWorkflow { workflow_run_id } => {
            lifecycle
                .pause(PauseWorkflowRequest {
                    workflow_run_id: workflow_run_id.clone(),
                    client_id,
                })
                .await
        }
        CommandBody::ResumeWorkflow { workflow_run_id } => {
            lifecycle
                .resume(ResumeWorkflowRequest {
                    workflow_run_id: workflow_run_id.clone(),
                    client_id,
                })
                .await
        }
        CommandBody::RetryWorkflowNode {
            workflow_run_id,
            node_id,
        } => {
            lifecycle
                .retry_node(RetryWorkflowNodeRequest {
                    workflow_run_id: workflow_run_id.clone(),
                    node_id: node_id.clone(),
                    client_id,
                })
                .await
        }
        CommandBody::CancelWorkflow { workflow_run_id } => {
            lifecycle
                .cancel(CancelWorkflowRequest {
                    workflow_run_id: workflow_run_id.clone(),
                    client_id,
                })
                .await
        }
        _ => Err(codypendent_protocol::CodypendentError::new(
            "ui.action.not-authorized",
            "not a workflow lifecycle command",
            false,
        )),
    }
    .map(|()| crate::commands::CommandOutcome {
        command_id: codypendent_protocol::CommandId::new(),
        created_session: None,
        created_run: None,
        last_sequence: None,
        newly_applied: true,
    })
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
        state.remote_ui.clone(),
        client_id,
        session_id,
        subscription.receiver,
    ));
    forwarders.insert(session_id, handle);
    Ok(())
}

async fn forward_remote_ui(
    writer: SharedWriter,
    broker: RemoteUiBroker,
    client_id: ClientId,
    session_id: SessionId,
    mut receiver: broadcast::Receiver<UiBrokerFrame>,
) {
    loop {
        let messages = match receiver.recv().await {
            Ok(frame) => match frame.target {
                UiBrokerTarget::AllRenderers => vec![frame.message],
                UiBrokerTarget::Renderer(target) if target == client_id => vec![frame.message],
                UiBrokerTarget::Renderer(_) | UiBrokerTarget::Producer(_) => continue,
            },
            // Recover with a complete, renderer-filtered baseline sent directly
            // over the socket. A generic recoverable error has no document id,
            // so clients cannot turn it into a per-document resync request.
            Err(broadcast::error::RecvError::Lagged(skipped)) => match broker
                .renderer_baseline(session_id, client_id)
            {
                Ok(baseline) => baseline,
                Err(error) => vec![broker_error(format!(
                    "Remote UI fan-out dropped {skipped} messages and baseline recovery failed: {error}"
                ))],
            },
            Err(broadcast::error::RecvError::Closed) => break,
        };
        for message in messages {
            if send(&writer, &remote_ui_envelope(client_id, session_id, message))
                .await
                .is_err()
            {
                return;
            }
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

    // Reject an attach to a session this daemon has never seen — or to one this
    // principal does not own. An empty catch-up here used to make a typo'd id
    // indistinguishable from a valid empty session — the client then bound a
    // blank UI to a dead id whose every `StartRun` rejected `session-not-found`.
    // Clients that probe a remembered id (the TUI's resume flow) treat a
    // non-`Catchup` reply as "gone" and fall through to creating a fresh
    // session. Another principal's session answers identically to a missing one,
    // so an attach cannot be used to enumerate what exists.
    if !principal_may_use_session(state, conn.principal, session_id).await? {
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
    // Each of these subscriptions names a resource by an id the CLIENT chose,
    // independent of the session being attached — so owning this session buys no
    // access to them. Every one is re-derived against the connection's principal
    // from what the server stored, exactly like a by-id read. A subscription the
    // principal may not have is dropped silently rather than refused: naming it
    // must not reveal whether it exists (F-19-7), and the attach itself is
    // legitimate.
    let mut new_doc_forwarders: Vec<JoinHandle<()>> = Vec::new();
    for subscription in &subscriptions {
        match subscription {
            Subscription::Document { document_id } => {
                if !principal_may_read_document(state, conn.principal, *document_id).await? {
                    warn!(
                        %document_id,
                        uid = conn.principal.uid(),
                        "dropping a document subscription this principal does not own"
                    );
                    continue;
                }
                let receiver = state.documents.subscribe(*document_id);
                new_doc_forwarders.push(tokio::spawn(forward_document_syncs(
                    Arc::clone(writer),
                    receiver,
                    client_id,
                )));
            }
            Subscription::Blackboard { workflow_run_id } => {
                if !principal_may_read_workflow(state, conn.principal, workflow_run_id).await? {
                    warn!(
                        workflow_run_id,
                        uid = conn.principal.uid(),
                        "dropping a blackboard subscription this principal does not own"
                    );
                    continue;
                }
                let receiver = state.blackboards.subscribe(workflow_run_id.clone());
                new_doc_forwarders.push(tokio::spawn(forward_blackboard_posts(
                    Arc::clone(writer),
                    receiver,
                    client_id,
                )));
            }
            Subscription::Workflow { workflow_run_id } => {
                if !principal_may_read_workflow(state, conn.principal, workflow_run_id).await? {
                    warn!(
                        workflow_run_id,
                        uid = conn.principal.uid(),
                        "dropping a workflow subscription this principal does not own"
                    );
                    continue;
                }
                let receiver = state.workflows.subscribe(workflow_run_id.clone());
                new_doc_forwarders.push(tokio::spawn(forward_workflow_events(
                    Arc::clone(writer),
                    receiver,
                    client_id,
                )));
            }
            _ => {}
        }
    }
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
        // A policy denial belongs to the run that proposed the action. Omitting
        // it made `RunTrace` — "the detailed trace of one run" — the ONE
        // subscription that never showed a denial, so a client watching a
        // single run saw the safe actions and none of the refused ones.
        | ToolDenied { run_id, .. }
        | ToolStarted { run_id, .. }
        | ToolCompleted { run_id, .. }
        | PatchProposed { run_id, .. }
        | SteeringQueued { run_id }
        | SteeringApplied { run_id }
        | BudgetWarning { run_id, .. }
        | RunCompleted { run_id, .. }
        | RunUsage { run_id, .. }
        | LearningsCaptured { run_id, .. } => Some(*run_id),
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

/// The shared gate for a memory command (outcome 17): the assembly's
/// [`MemoryGateway`](crate::memory::MemoryGateway) must be wired, and a
/// *mutating* verb additionally requires the `Controller` role — an Observer may
/// read what the fabric remembers, never rewrite or erase it. On either failure
/// this frames the rejection onto `writer` and returns `None`.
///
/// The scope check is deliberately NOT here: it belongs where the memory is
/// fetched, inside the seam, so "you may not see this" and "this does not
/// exist" collapse to one answer instead of two.
async fn memory_seam<'a>(
    state: &'a Arc<ServerState>,
    conn: &ConnState,
    writer: &SharedWriter,
    request: &Envelope,
    mutating: bool,
    verb: &str,
) -> anyhow::Result<Option<&'a Arc<dyn crate::memory::MemoryGateway>>> {
    // The memory store is daemon-wide: its scopes are System, the local user,
    // and a repository — there is no per-row owner to resolve, so unlike a
    // session or a workflow run there is nothing here to compare a principal
    // AGAINST. The store therefore belongs to the uid the daemon runs as, and
    // only that principal may touch it.
    //
    // The role check below is NOT a substitute: `ClientRole` is requested by
    // the client, so without this a peer that can reach the socket could ask
    // for `Controller` and issue `ForgetMemoryScope { tier: User | System }`,
    // erasing every memory in a shared scope without knowing a single id. The
    // socket directory's 0700 mode normally keeps other users out; this is the
    // check that does not depend on that being true.
    if conn.principal.uid() != state.daemon_uid {
        let reply = Envelope::reply_to(
            request,
            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                "memory.transport-unavailable",
                "the memory store is not enabled on this daemon".to_string(),
                false,
            )),
        );
        send(writer, &reply).await?;
        return Ok(None);
    }
    if mutating && conn.role != ClientRole::Controller {
        let reply = Envelope::reply_to(
            request,
            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                "protocol.role-denied",
                format!("only a Controller may {verb} a memory"),
                false,
            )),
        );
        send(writer, &reply).await?;
        return Ok(None);
    }
    let Some(memory) = state.memory.as_ref() else {
        let reply = Envelope::reply_to(
            request,
            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                "memory.transport-unavailable",
                "the memory store is not enabled on this daemon".to_string(),
                false,
            )),
        );
        send(writer, &reply).await?;
        return Ok(None);
    };
    Ok(Some(memory))
}

/// Lower a wire [`BlackboardScope`] to the daemon's [`BoardTarget`], or `None`
/// for the `Unknown` variant a newer client's scope parses into — rejected
/// structurally at the edge rather than guessed at (Phase B kanban).
fn board_target(scope: &codypendent_protocol::BlackboardScope) -> Option<BoardTarget> {
    match scope {
        codypendent_protocol::BlackboardScope::WorkflowRun { workflow_run_id } => {
            Some(BoardTarget::WorkflowRun(workflow_run_id.clone()))
        }
        codypendent_protocol::BlackboardScope::RepositoryBoard { repository } => {
            Some(BoardTarget::Repository(repository.clone()))
        }
        _ => None,
    }
}

/// The rejection for a board scope this daemon does not understand.
/// The refusal every session-ownership gate returns.
///
/// Deliberately identical — code, message, retryability — to the rejection a
/// genuinely missing session gets (`commands::validate`, `handle_attach`), so a
/// caller cannot tell "not yours" from "not there" and cannot enumerate the
/// daemon's sessions by probing ids.
fn session_not_found(session_id: SessionId) -> codypendent_protocol::CodypendentError {
    codypendent_protocol::CodypendentError::new(
        "protocol.session-not-found",
        format!("no session {session_id}"),
        false,
    )
}

/// The workflow-run equivalent of [`session_not_found`], matching byte for byte
/// what the assembly's `WorkflowReader` returns for a run that does not exist
/// (`codypendentd::workflows`, `workflow.run-not-found`).
fn workflow_run_not_found(workflow_run_id: &str) -> codypendent_protocol::CodypendentError {
    codypendent_protocol::CodypendentError::new(
        "workflow.run-not-found",
        format!("no workflow run {workflow_run_id}"),
        false,
    )
}

fn unknown_board_scope() -> codypendent_protocol::CodypendentError {
    codypendent_protocol::CodypendentError::new(
        "blackboard.unknown-scope",
        "this daemon does not understand the requested board scope".to_string(),
        false,
    )
}

/// The `Controller` role gate + transport check both board writes share: replies
/// the rejection itself and yields `None` when either fails, mirroring
/// [`workflow_control_seam`].
async fn board_writer_or_reject<'a>(
    state: &'a Arc<ServerState>,
    conn: &ConnState,
    request: &Envelope,
    writer: &SharedWriter,
) -> anyhow::Result<Option<&'a Arc<dyn BlackboardWriter>>> {
    if conn.role != ClientRole::Controller {
        let reply = Envelope::reply_to(
            request,
            Payload::CommandRejected(codypendent_protocol::CodypendentError::new(
                "protocol.role-denied",
                "only a Controller may write the blackboard".to_string(),
                false,
            )),
        );
        send(writer, &reply).await?;
        return Ok(None);
    }
    let Some(seam) = state.blackboard_writer.as_ref() else {
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
    Ok(Some(seam))
}

/// The largest page `ReadSessionEvents` serves, and the page size a request that
/// names none gets. Bounded so one command can never be asked to materialize a
/// 100k-event session into a single frame (the 16 MiB frame limit is a wall, not
/// a policy) — a pager simply walks forward instead.
const MAX_SESSION_EVENTS_PAGE: u32 = 500;

/// Serve one ascending page of a session's durable history: events with
/// `after_sequence < sequence <= after_sequence + limit`, the page's highest
/// sequence, and whether anything existed beyond it at read time. An unknown
/// session is rejected rather than answered with an empty page, so a typo'd id
/// never reads as "empty session" (the same discipline the attach catch-up uses).
async fn read_session_events_page(
    pool: &SqlitePool,
    session_id: SessionId,
    after_sequence: u64,
    limit: u32,
) -> Result<(Vec<SessionEvent>, u64, bool), codypendent_protocol::CodypendentError> {
    let store_error = |error: anyhow::Error| {
        codypendent_protocol::CodypendentError::new(
            "protocol.store-error",
            format!("could not read the session history: {error}"),
            true,
        )
    };
    match ledger::session_exists(pool, session_id).await {
        Ok(true) => {}
        Ok(false) => {
            return Err(codypendent_protocol::CodypendentError::new(
                "protocol.session-not-found",
                format!("no session {session_id}"),
                false,
            ));
        }
        Err(error) => return Err(store_error(error)),
    }
    // 0 (or absent) asks for the server default; anything larger is clamped to
    // the ceiling rather than refused, so a client never has to know the limit.
    let limit = if limit == 0 {
        MAX_SESSION_EVENTS_PAGE
    } else {
        limit.min(MAX_SESSION_EVENTS_PAGE)
    };
    let through = after_sequence.saturating_add(u64::from(limit));
    let events = ledger::load_events_between(pool, session_id, after_sequence, through)
        .await
        .map_err(store_error)?;
    // `next_sequence` is the sequence the NEXT append will take, so the highest
    // one that exists is one below it — comparing against the next would report
    // `has_more` on a fully drained ledger and spin a pager forever.
    let highest = ledger::next_sequence(pool, session_id)
        .await
        .map_err(store_error)?
        .saturating_sub(1);
    // An empty page keeps the caller's cursor where it was, so paging is a fixed
    // point once drained rather than silently skipping the requested window.
    let reached = events.last().map_or(after_sequence, |event| event.sequence);
    Ok((events, reached, highest > reached))
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
        integration_issues: state.integration_health.snapshot(),
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
        admits_run, claim_ui_plugin_command, is_remote_ui_workflow_control,
        persist_ui_plugin_command_result, remote_ui_artifact_range, remote_ui_command, resume,
        IntegrationHealth, REMOTE_UI_ACTIONS,
    };

    /// `WorkflowStore::create_run_idempotent` inserts `run_id = NULL`
    /// unconditionally, so EVERY client-created workflow run is unbound. An
    /// earlier version of this gate read "unbound" as "no owner to protect" and
    /// allowed those outright — which made every workflow run in the product
    /// readable by any principal that could guess its id. Migration 0033 gives
    /// the run its own owner; these pin the three cases apart.
    ///
    /// Driven at `workflow_run_owner` rather than over the socket on purpose:
    /// the first version of this test sent `ReadWorkflowRun` to a test daemon
    /// that has no workflow transport, so it was refused with
    /// `workflow.transport-unavailable` before the gate was ever consulted and
    /// passed against the bug it was written to catch.
    #[tokio::test]
    async fn an_unbound_workflow_run_is_owned_by_its_creator_not_by_everyone() {
        use super::{workflow_run_owner, WorkflowOwner};

        let dir = tempfile::tempdir().expect("tempdir");
        let pool = crate::db::open_database(&dir.path().join("test.db"))
            .await
            .expect("migrated pool");
        let now = Utc::now().to_rfc3339();

        let insert = |id: &'static str, owner: Option<i64>| {
            let pool = pool.clone();
            let now = now.clone();
            async move {
                sqlx::query(
                    "INSERT INTO workflow_runs \
                     (id, workflow_id, workflow_version, graph_signature, run_id, inputs_json, \
                      state, created_at, updated_at, owner_uid) \
                     VALUES (?, 'demo', 1, 'sig', NULL, '{}', 'pending', ?, ?, ?)",
                )
                .bind(id)
                .bind(&now)
                .bind(&now)
                .bind(owner)
                .execute(&pool)
                .await
                .expect("seed workflow run");
            }
        };
        insert("wf-owned", Some(4242)).await;
        insert("wf-legacy", None).await;

        assert!(
            matches!(
                workflow_run_owner(&pool, "wf-owned").await.expect("query"),
                WorkflowOwner::Uid(4242)
            ),
            "an unbound run must be governed by the uid that created it"
        );
        assert!(
            matches!(
                workflow_run_owner(&pool, "wf-legacy").await.expect("query"),
                WorkflowOwner::Missing
            ),
            "a pre-0033 row boot adoption never reached must fail closed, not open"
        );
        assert!(
            matches!(
                workflow_run_owner(&pool, "wf-absent").await.expect("query"),
                WorkflowOwner::Missing
            ),
            "an absent run is Missing — the same answer a foreign one produces"
        );
    }

    #[test]
    fn integration_health_is_sanitized_and_deduplicated() {
        let health = IntegrationHealth::default();
        health.report("MCP failed\u{1b}[31m\u{202e}");
        health.report("MCP failed[31m");
        assert_eq!(health.snapshot(), vec!["MCP failed[31m"]);
    }

    #[test]
    fn integration_health_is_bounded() {
        let health = IntegrationHealth::default();
        health.report("x".repeat(super::MAX_INTEGRATION_ISSUE_CHARS + 20));
        assert_eq!(
            health.snapshot()[0].chars().count(),
            super::MAX_INTEGRATION_ISSUE_CHARS
        );
        for index in 0..(super::MAX_INTEGRATION_ISSUES + 20) {
            health.report(format!("issue {index}"));
        }
        assert_eq!(health.snapshot().len(), super::MAX_INTEGRATION_ISSUES);
    }
    use chrono::Utc;
    use codypendent_protocol::{
        AgentMode, ClientId, CommandBody, CommandId, Payload, RunId, SessionEvent, SessionId,
        Subscription,
    };

    fn invocation(
        action_id: &str,
        payload: serde_json::Value,
    ) -> codypendent_protocol::UiActionInvocation {
        codypendent_protocol::UiActionInvocation {
            invocation_id: "invocation-1".into(),
            document_id: codypendent_protocol::UiDocumentId::from("document-1"),
            revision: codypendent_protocol::UiRevision(1),
            source_node_id: codypendent_protocol::UiNodeId::from("node-1"),
            action_id: codypendent_protocol::UiActionId::from(action_id),
            payload,
            form_data: Default::default(),
            interaction_token: None,
            interaction_event_type: None,
        }
    }

    fn run_event(body: codypendent_protocol::EventBody) -> SessionEvent {
        SessionEvent {
            sequence: 1,
            occurred_at: Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor: codypendent_protocol::Actor::System,
            body,
        }
    }

    /// `Subscription::RunTrace` is "the detailed trace of one run" — a policy
    /// denial and a measured-usage report are both part of that trace, and both
    /// were dropped because [`super::event_run_id`] did not match them.
    #[test]
    fn run_trace_carries_denials_and_usage_for_its_own_run() {
        use codypendent_protocol::EventBody;
        let run_id = RunId::new();
        let other = RunId::new();
        let trace = vec![Subscription::RunTrace { run_id }];

        let denied = |run| {
            run_event(EventBody::ToolDenied {
                run_id: run,
                action: codypendent_protocol::ProposedAction::ExecuteCommand {
                    program: "rm".into(),
                    args: vec!["-rf".into(), "/".into()],
                    environment: Vec::new(),
                    cwd: None,
                },
                reasons: vec!["program is not allow-listed".into()],
            })
        };
        let usage = |run| {
            run_event(EventBody::RunUsage {
                run_id: run,
                prompt_tokens: Some(1002),
                completion_tokens: Some(60),
                cost_micros: None,
            })
        };

        assert!(super::subscription_matches(&trace, &denied(run_id)));
        assert!(super::subscription_matches(&trace, &usage(run_id)));
        // …and the filter is still a filter: another run's denial stays out.
        assert!(!super::subscription_matches(&trace, &denied(other)));
        assert!(!super::subscription_matches(&trace, &usage(other)));
    }

    #[test]
    fn the_mediated_action_table_is_the_whole_allowlist() {
        // Adding a mediated action must be one table row and nothing else.
        let ids = REMOTE_UI_ACTIONS
            .iter()
            .map(|action| action.action_id)
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec![
                "run.pause",
                "run.resume",
                "run.cancel",
                "workflow.pause",
                "workflow.resume",
                "workflow.retry_node",
                "workflow.cancel",
            ]
        );

        let run_id = RunId::new();
        assert!(matches!(
            remote_ui_command(&invocation("run.pause", serde_json::json!({ "runId": run_id.to_string() }))),
            Ok(CommandBody::PauseRun { run_id: lowered }) if lowered == run_id
        ));
        // The `core.` spelling names the same command.
        assert!(matches!(
            remote_ui_command(&invocation(
                "core.run.cancel",
                serde_json::json!({ "runId": run_id.to_string() })
            )),
            Ok(CommandBody::CancelRun { .. })
        ));
        assert!(matches!(
            remote_ui_command(&invocation("workflow.retry_node", serde_json::json!({ "workflowRunId": "wf-1", "nodeId": "build" }))),
            Ok(CommandBody::RetryWorkflowNode { workflow_run_id, node_id }) if workflow_run_id == "wf-1" && node_id == "build"
        ));
        assert!(matches!(
            remote_ui_command(&invocation("workflow.cancel", serde_json::json!({ "workflowRunId": "wf-1" }))),
            Ok(CommandBody::CancelWorkflow { workflow_run_id }) if workflow_run_id == "wf-1"
        ));

        // Everything outside the table is refused, including near-misses and
        // commands the daemon has but never mediates.
        for action_id in [
            "workflow.start",
            "session.close",
            "run.pause.extra",
            "core.core.run.pause",
            "blackboard.post",
            "",
        ] {
            let error = remote_ui_command(&invocation(
                action_id,
                serde_json::json!({ "runId": run_id.to_string() }),
            ))
            .expect_err("unlisted action is not mediated");
            assert_eq!(error.code, "ui.action.not-authorized", "{action_id}");
        }

        // A listed action with an unusable payload is a payload error, never a
        // command with a fabricated resource.
        let error = remote_ui_command(&invocation(
            "workflow.retry_node",
            serde_json::json!({ "workflowRunId": "wf-1" }),
        ))
        .expect_err("retry needs a node id");
        assert_eq!(error.code, "ui.action.invalid-payload");
        let error = remote_ui_command(&invocation("run.pause", serde_json::json!({ "runId": "" })))
            .expect_err("an empty run id is not a run id");
        assert_eq!(error.code, "ui.action.invalid-payload");
    }

    #[test]
    fn workflow_control_is_routed_off_the_session_ledger() {
        // Workflow lifecycle lives in its own durable store, so these bodies go
        // to the `WorkflowLifecycle` seam rather than `CommandService::apply`.
        for body in [
            CommandBody::PauseWorkflow {
                workflow_run_id: "wf-1".into(),
            },
            CommandBody::ResumeWorkflow {
                workflow_run_id: "wf-1".into(),
            },
            CommandBody::RetryWorkflowNode {
                workflow_run_id: "wf-1".into(),
                node_id: "build".into(),
            },
            CommandBody::CancelWorkflow {
                workflow_run_id: "wf-1".into(),
            },
        ] {
            assert!(is_remote_ui_workflow_control(&body), "{body:?}");
        }
        assert!(!is_remote_ui_workflow_control(&CommandBody::PauseRun {
            run_id: RunId::new()
        }));
    }

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
            envelope: None,
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
