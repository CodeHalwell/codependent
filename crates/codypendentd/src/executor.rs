//! The concrete [`RunExecutor`]: wraps the runtime agent loop.
//!
//! This lives in the assembly binary because it needs BOTH the daemon (the pool,
//! ledger, artifact store, subscription hub, approval broker, and the
//! [`recovery::fail_run`] helper) and the runtime ([`FrameworkAgentRuntime`],
//! [`FrameworkModelDriver`], the model registry/policy). The daemon crate cannot
//! name the runtime, so this seam is the one place both worlds meet.
//!
//! It also owns the shared [`SubscriptionHub`] + [`ApprovalBroker`] the server
//! binds to (via [`RunExecutor::collaborators`]): a run's events are published to
//! this hub — the same one the server forwards to attached clients — and
//! approvals are driven on this broker — the same one the server's command
//! processor resolves against. Without that sharing a headless client would
//! never observe the run it started.
//!
//! ## The SQLite boundary
//!
//! The runtime reaches the ledger + artifact store through a pool-erased
//! [`RunJournal`] and [`ArtifactSink`] (it cannot name `SqlitePool`; see the
//! agent-module docs). This crate *can* name the pool, so [`RuntimeExecutor`]
//! builds those from plain closures rather than the macros the runtime's own
//! integration tests use.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use codypendent_council::FileCouncilService;
use codypendent_daemon::approvals::ApprovalBroker;
use codypendent_daemon::artifacts::{ArtifactStore, Provenance};
use codypendent_daemon::blackboard::{BlackboardHub, BlackboardReader, BlackboardWriter};
use codypendent_daemon::executor::{PriorTurn, RunExecutor, RunLaunch};
use codypendent_daemon::poison::lock_recovering;
use codypendent_daemon::policy::{PolicyEngine, GITHUB_API_ENDPOINT, TAVILY_API_ENDPOINT};
use codypendent_daemon::questions::{QuestionBroker, QuestionReply};
use codypendent_daemon::subscriptions::SubscriptionHub;
use codypendent_daemon::workflow_stream::{WorkflowHub, WorkflowReader};
use codypendent_daemon::worktrees::{ReleaseOutcome, WorktreeError, WorktreeManager};
use codypendent_daemon::{ledger, projections, recovery};
use codypendent_integrations::acp::PermissionOption;
use codypendent_integrations::acp_client::{
    forwardable_mcp_servers, AcpClient, AcpEventSink, AcpSessionOptions, AcpStopReason,
};
use codypendent_integrations::acp_registry::{agent_model_from_coordinate, AcpRegistryStore};
use codypendent_integrations::github::{GitHubApi, RepoId};
use codypendent_integrations::mcp::{McpBridge, McpRegistry};
use codypendent_integrations::search::SearchApi;
#[cfg(test)]
use codypendent_integrations::search::TavilyClient;
use codypendent_knowledge::context::assemble_context_with;
use codypendent_knowledge::{
    chronicle_candidates, extract_candidates, ContextAssembler, Curation, ExtractionInput,
    FactExtractor, GitRevision, MemoryStore, NoopExtractor, Revision, Scope, SemanticEmbedder,
};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{
    Actor, AgentId, AgentMode, ApprovalDecision, ArtifactRef, ChangeSetId, DataClassification,
    EventBody, ModelId, ProposedAction, RepositoryId, Risk, RiskLevel, RunDisposition, RunId,
    RunState, SessionId,
};
use codypendent_runtime::agent::{
    cancellation, mode_overlay, ApprovalRequest, CancellationHandle, CancellationToken,
    FrameworkAgentRuntime, FrameworkModelDriver, QuestionChannel, RunContext, RunJournal, TurnItem,
};
use codypendent_runtime::models::{
    load_models, resolve_model, ModelPolicy, ModelRegistry, RetrievalSettings,
};
use codypendent_runtime::tools::{ArtifactSink, ClosureSink};
use sqlx::SqlitePool;
use tracing::{error, info, warn};

use crate::blackboard::{AssemblyBoardWriter, AssemblyTaskBoardChannel, WorkflowBlackboardReader};
use crate::promotion::PromotionStoreGateway;
use crate::retrieval::PoolRegistrySearch;
use crate::routing::{
    derive_run_classification, estimate_input_tokens, RoutingConfig, RoutingCoordinator,
};
use crate::scan;
use crate::session_history::{context_turn, continuation_prior, CONTEXT_PSEUDO_TOOL};
use crate::workflow_exec::{build_workflow_host, AgentLoopNodeExecutor, WorkflowRunCancellations};
use crate::workflows::{AssemblyWorkflowQuery, WorkflowConductorHost, WorkflowRunReader};

/// How many of a session's most recent runs a continuation replays VERBATIM
/// (turn-by-turn); every earlier run is compacted to a single summary turn
/// (continuous-session plan). Bounds the token cost each follow-up re-pays —
/// see [`crate::session_history::session_transcript`].
const VERBATIM_RUNS: usize = 3;
const MAX_ACP_PATCH_BYTES: usize = 64 * 1024 * 1024;

/// The one-line marker a continuation's trace opens with, in place of the full
/// `=== CONTEXT` repo-map manifest a first run emits: on a follow-up the shared
/// context already rides in the seeded transcript, so re-mapping the repository
/// every message would only re-pay tokens for nothing (continuous-session plan,
/// Task 4).
const CONTINUATION_CONTEXT_NOTE: &str = "context carried from the conversation";

/// Bound on how much of a prior tool call's stored artifact is replayed into
/// a continuation's seed transcript, per `TurnItem::ToolResult` (continuation-
/// content plan, Task 3). The stored `read_file` observation is already
/// capped at ~200 lines by its producer (Task 1), so this exists to bound the
/// CONTINUATION SEED specifically — a further cap independent of whatever the
/// producer already applied, so a single huge artifact cannot dominate a
/// follow-up's opening prompt.
const CONTINUATION_TOOL_EXCERPT_BYTES: usize = 2048;

/// Aggregate bound across every hydrated `ToolResult` in one continuation's
/// seed (Task 3): protects a run that read many files from re-paying a large
/// multiple of [`CONTINUATION_TOOL_EXCERPT_BYTES`] on every follow-up. Once
/// the running total of hydrated bytes reaches this, remaining artifacts are
/// left at their `tool_result_summary` fallback (logged, not silently
/// dropped — see [`RuntimeExecutor::hydrate_tool_artifacts`]).
const CONTINUATION_HYDRATION_AGGREGATE_BYTES: usize = 16 * 1024;

/// Appended to a hydrated excerpt when the stored artifact held more bytes
/// than [`CONTINUATION_TOOL_EXCERPT_BYTES`] — truthful, since the excerpt
/// above it is exactly the stored bytes' head, never a fabricated summary of
/// the rest.
const CONTINUATION_TRUNCATION_MARKER: &str = "\n… (truncated; re-read for full content)";

/// Every run-control map the executor owns, under ONE mutex.
///
/// # Lock order
///
/// There is exactly one run-control lock, so there is no order to get wrong.
/// That is the point: the live-handle map and the two pending sets used to be
/// three independent `Mutex`es, and correctness then depended on every call
/// site nesting them identically. A single inverted acquisition (say, taking a
/// pending set and then reaching for the live map while another thread did the
/// reverse) deadlocks the executor and blocks every subsequent run-control
/// command, because `cancel_run`/`pause_run`/`resume_run` all funnel through
/// the same maps.
///
/// **Do not split these fields back into separate mutexes.** If a future change
/// genuinely needs finer granularity, it must document a total lock order and
/// every site must obey it; `run_control_survives_concurrent_start_stop_traffic`
/// is the regression guard (it fails by timeout, loudly, rather than hanging CI
/// forever).
///
/// `run_control` is a leaf lock: nothing else is acquired while it is held. The
/// unrelated `steerings` / `scanned` / `watchers` mutexes are never nested with
/// it.
#[derive(Default)]
pub(crate) struct RunControlRegistry {
    /// Live per-run cancellation handles, keyed by `RunId`.
    live: HashMap<RunId, CancellationHandle>,
    /// Cancellation commands accepted before `spawn_run` reaches the executor.
    /// Entries are consumed when the corresponding run is registered.
    pending_cancellations: HashSet<RunId>,
    /// Pause commands accepted before the worker installs its control handle.
    pending_pauses: HashSet<RunId>,
}

impl RunControlRegistry {
    /// Register a freshly created run's handle, applying any control command
    /// that arrived before the run reached the executor. Atomic by construction:
    /// consuming the pending entry and installing the handle happen under the
    /// one lock the caller already holds, so a `CancelRun` racing the spawn is
    /// either consumed here or finds the handle in [`Self::live`] — never lost
    /// between the two.
    fn register(&mut self, run_id: RunId, handle: CancellationHandle) {
        if self.pending_cancellations.remove(&run_id) {
            handle.cancel();
        } else if self.pending_pauses.remove(&run_id) {
            handle.pause();
        }
        self.live.insert(run_id, handle);
    }

    /// Drop every trace of a run that has reached a terminal state, so the
    /// registry does not grow without bound and a late `cancel_run` for it is a
    /// clean no-op.
    fn forget(&mut self, run_id: RunId) {
        self.live.remove(&run_id);
        self.pending_cancellations.remove(&run_id);
        self.pending_pauses.remove(&run_id);
    }
}

/// Executes accepted runs by driving the runtime agent loop. Cheap to clone —
/// every field is an `Arc`-backed handle or a plain (clonable) path bundle.
#[derive(Clone)]
pub struct RuntimeExecutor {
    pool: SqlitePool,
    paths: RuntimePaths,
    /// The daemon's startup repository root (its working directory at launch).
    /// Carried so the workflow node executor can fall back to it for a run that
    /// recorded no repository (an older client), resolved once here rather than
    /// from a wandering `current_dir()` at node-execution time (Phase 5 T5,
    /// P5-D1). A single-agent run never needs it — its `RunLaunch` always carries
    /// a repository (the server fills the daemon's cwd when a client sends none).
    startup_repository_root: PathBuf,
    subscriptions: SubscriptionHub,
    approvals: ApprovalBroker,
    questions: QuestionBroker,
    /// The revision each repository's code graph was last folded at, this
    /// process's lifetime. A per-user daemon can serve several checkouts over one
    /// socket, so each run derives its OWN repository identity from its
    /// repository root and the first run for a repository warms it here (issue #6
    /// item 1).
    ///
    /// Keyed by revision, not a bare "seen" flag (2026-08-11 review): a
    /// once-per-boot gate left a long-lived daemon serving a graph from whatever
    /// the checkout looked like at its first run — a branch switch, pull, or
    /// commit silently kept the stale map for days. A run whose `HEAD` no longer
    /// matches the folded revision re-scans; a run at the same revision reuses
    /// the graph exactly as before. `Arc<Mutex<…>>` so every clone shares one map.
    scanned: Arc<Mutex<HashMap<RepositoryId, GitRevision>>>,
    /// The live code-graph watcher for each repository this daemon has folded
    /// (outcome 14). Armed once, right after a repository's first successful
    /// scan, and kept for the daemon's life: a watcher is one `notify` thread
    /// plus one debouncing task, so a per-repository entry is cheap, while
    /// re-arming per run would leak a thread per run. `Arc<Mutex<…>>` so every
    /// clone of this executor shares ONE registry — otherwise the clone the
    /// server holds and the clone `spawn_run` holds would each arm their own.
    watchers: Arc<Mutex<HashMap<RepositoryId, scan::RepositoryWatcher>>>,
    /// Run control — live cancellation handles plus the two pre-registration
    /// pending sets — behind ONE mutex. `spawn_run` registers a run's handle
    /// before its loop starts and removes it once the loop is terminal;
    /// [`cancel_run`](RunExecutor::cancel_run) fires the matching handle so an
    /// accepted `CancelRun` actually stops the runtime instead of only marking
    /// the projection `Cancelled`. `Arc<Mutex<…>>` so every clone of this
    /// (cheap-to-clone) executor shares one registry — the clone the server holds
    /// must see the handle the worker task registered.
    ///
    /// **These three maps share one lock on purpose.** They used to be three
    /// separate mutexes, and every run-control path had to nest them in exactly
    /// the same order or two clients issuing start/resume in the registration
    /// window could deadlock the executor and wedge every later run-control
    /// command. One mutex removes that class of bug outright and makes the
    /// check-then-register step (consume a pending cancel/pause, then insert the
    /// handle) genuinely atomic. See [`RunControlRegistry`].
    run_control: Arc<Mutex<RunControlRegistry>>,
    /// Live per-run steering channels (Adoption 06).
    steerings: Arc<Mutex<HashMap<RunId, tokio::sync::mpsc::UnboundedSender<String>>>>,
    /// The GitHub client the `github.*` tools call, if a personal-mode token was
    /// discovered at startup (Phase 3 STEP 3.2). `None` leaves those tools
    /// unavailable and the run behaves exactly as before.
    github: Option<Arc<dyn GitHubApi>>,
    /// The MCP bridge the `mcp.<server>.<tool>` tools dispatch through (PR B —
    /// MCP client), built from the operator-declared `<config_dir>/mcp.toml` at
    /// startup. `None` (no file, or no servers declared) leaves those tools
    /// unoffered and the run behaves exactly as before. Stored pre-coerced to
    /// the trait object, like `github`, so the runtime's `with_mcp` needs no
    /// cast at the call sites.
    mcp: Option<Arc<dyn McpBridge>>,
    /// The web-search client the `web.search` tool calls (PR C1), built from
    /// the `TAVILY_API_KEY` discovered at startup. `None` leaves the tool
    /// unoffered and the run behaves exactly as before. Stored pre-coerced to
    /// the trait object, like `github`/`mcp`, so the runtime's `with_search`
    /// needs no cast at the call sites.
    search: Option<Arc<dyn SearchApi>>,
    /// The speech-to-text seam a `SubmitUserInput` carrying voice audio routes
    /// through (voice v1, rubric 8), built from `models.toml`'s `[transcription]`
    /// table. `None` (the default — no table configured) leaves the daemon
    /// rejecting audio submissions `voice.transport-unavailable`; plain-text
    /// input is unaffected either way. Carried here, like `github`/`mcp`/`search`,
    /// because the server pulls every assembly-provided seam off the executor.
    transcriber: Option<Arc<dyn codypendent_daemon::transcription::Transcriber>>,
    /// The workflow-execution host: creates, drives, recovers, and controls durable
    /// workflow runs (Phase 5 STEP 5.2). One shared host backs both the
    /// [`WorkflowStarter`](codypendent_daemon::workflows::WorkflowStarter) and
    /// [`WorkflowLifecycle`](codypendent_daemon::workflows::WorkflowLifecycle) seams
    /// the server pulls out, so their per-run drive locks are the same registry —
    /// a `PauseWorkflow` and the `StartWorkflow` drive it pauses serialize together.
    workflow_host: WorkflowConductorHost<AgentLoopNodeExecutor>,
    /// The repository root a `PublishDocument` command writes/commits against
    /// (Phase 4 STEP 4.4). Unlike a run's repository — carried per-command on
    /// `StartRun` (issue #6 item 1) — a document has no per-command repository
    /// field (documents live outside the session ledger), so publication uses
    /// this daemon's own startup working directory, set via
    /// [`Self::with_repository_root`]. Defaults to the process's current
    /// directory so a caller that never sets it still gets a sensible root.
    repository_root: PathBuf,
    /// The promotion-pipeline gateway (Phase 7 STEP 7.5): backs
    /// `ProposePromotion`/`AdvancePromotion`/`ApprovePromotion`/`RollbackPromotion`.
    /// Stateless beyond the pool, so it is built once here and cloned out by
    /// [`RunExecutor::promotion_gateway`].
    promotion: PromotionStoreGateway,
    /// The per-run blackboard fan-out (Phase 5 STEP 5.3). Owned here so BOTH the
    /// workflow node executor (which publishes an agent's posted artifacts through
    /// it) and the server (which subscribes a client's `Subscription::Blackboard`
    /// forwarder to it, via [`RunExecutor::blackboard_hub`]) share one hub — the
    /// publisher is the agent loop inside the executor, so it cannot be a
    /// server-created fresh hub the way the document hub is.
    blackboards: BlackboardHub,
    /// The per-run node-lifecycle fan-out (Phase 5 STEP 5.2 / T9). Owned here for the
    /// same reason as `blackboards`: the publisher is the workflow host + observer
    /// inside the executor, so the server subscribes a client's
    /// `Subscription::Workflow` forwarder to THIS hub (via [`RunExecutor::workflow_hub`]).
    workflows: WorkflowHub,
    /// The in-flight node agent-run cancellation registry (T9). Owned here — and
    /// carried across a `with_github` rebuild like `drive_locks` — so a
    /// `CancelWorkflow` fires the token the node executor registered, even if the host
    /// was reconfigured after the drive started.
    workflow_cancellations: WorkflowRunCancellations,
    /// The Phase-7 routing seam (STEP 7.2/7.3), **default OFF**. When the
    /// `<data_dir>/routing.toml` registry item enables it, [`Self::execute`] asks
    /// the router which model to run a task on (recording the decision in the
    /// trace) instead of the Phase-1 [`resolve_model`]; when it is absent/disabled
    /// the run resolves a model exactly as before. Bound to the shared
    /// subscription hub so a recorded routing note reaches attached clients live.
    routing: RoutingCoordinator,
    /// The caching context assembler (2026-08-11 review item 3): serves
    /// [`emit_context`](Self::emit_context) the registry's derived retrieval
    /// indexes from a registry-stamped cache instead of rebuilding dense+BM25
    /// from scratch on every first run. `Arc` so every clone of this executor
    /// shares ONE cache; invalidation is the stamp probe inside the assembler
    /// (any registry write — a skill install, a builtin refresh — moves it).
    context: Arc<ContextAssembler>,
    /// The configured embedding model (rubric 9), or `None` when `models.toml`
    /// declares no `[embedding]` entry — in which case retrieval keeps the
    /// offline hashing embedder and every path here behaves exactly as before.
    /// Shared with the index-maintenance job, so one content-hash cache serves
    /// both context assembly and the outbox drain.
    embedder: Option<Arc<dyn SemanticEmbedder>>,
    /// The `[retrieval]` tuning (today: the MCP top-k threshold), handed to each
    /// run's [`FrameworkAgentRuntime`].
    retrieval: RetrievalSettings,
    /// Unified Exec manager for PTY interactive processes (adoption 09).
    unified_exec: Arc<codypendent_daemon::unified_exec::UnifiedExecManager>,
    /// Live LSP diagnostics feedback engine (adoption 10).
    lsp: Option<Arc<codypendent_knowledge::LspManager>>,
}

impl RuntimeExecutor {
    /// Build an executor over the daemon's pool + paths, minting the shared
    /// fan-out + approval broker the server binds to via [`Self::collaborators`].
    /// `startup_repository` identifies the daemon's fallback checkout.
    /// `startup_repository_root` is that directory's path — the fallback
    /// repository a workflow run that recorded none is driven against (Phase 5
    /// T5). The scanned map starts empty because startup no longer blocks on a
    /// code-graph walk; the first session or run warms a valid Git checkout in
    /// the background.
    pub fn new(
        pool: SqlitePool,
        paths: RuntimePaths,
        _startup_repository: RepositoryId,
        startup_repository_root: PathBuf,
    ) -> Self {
        let subscriptions = SubscriptionHub::new();
        // Bind the broker to the SAME hub the server fans out to, so an
        // `ApprovalRequested` raised by the agent loop reaches attached clients
        // live (not only on re-attach catch-up).
        let approvals = ApprovalBroker::new().with_subscriptions(subscriptions.clone());
        let questions = QuestionBroker::new().with_subscriptions(subscriptions.clone());
        let scanned = HashMap::new();
        // The per-run blackboard fan-out, shared with every workflow agent node so
        // an agent's posts reach the server's subscribers (Phase 5 STEP 5.3).
        let blackboards = BlackboardHub::new();
        // The per-run node-lifecycle fan-out + the in-flight node cancellation
        // registry, shared with the workflow host so its drives publish transitions
        // here and its cancel seam fires the tokens the node executor registers (T9).
        let workflows = WorkflowHub::new();
        let workflow_cancellations = WorkflowRunCancellations::default();
        // The Phase-7 routing seam, loaded from `<data_dir>/routing.toml` (absent
        // ⇒ OFF). Bound to the shared fan-out so a recorded routing decision
        // reaches attached clients live, exactly like the run's context note.
        // Built BEFORE the workflow host so a workflow agent node's model
        // selection goes through this SAME seam (closing the gap where a node
        // used to resolve a model classification-blind, discarding its
        // `model_policy`) — see `workflow_exec::ConfiguredModelDriverFactory`.
        let routing = RoutingCoordinator::new(pool.clone(), RoutingConfig::load(&paths))
            .with_subscriptions(subscriptions.clone());
        let unified_exec = Arc::new(codypendent_daemon::unified_exec::UnifiedExecManager::new());
        let lsp_enabled =
            codypendent_runtime::models::load_model_extras(&paths.data_dir.join("models.toml"))
                .map(|extras| extras.lsp.enabled)
                .unwrap_or(true);
        let lsp = if lsp_enabled {
            Some(Arc::new(codypendent_knowledge::LspManager::new()))
        } else {
            None
        };
        // The first workflow host this process builds: no existing drive-lock
        // registry to share, so `build_workflow_host` mints a fresh one.
        let workflow_host = build_workflow_host(
            pool.clone(),
            paths.clone(),
            subscriptions.clone(),
            approvals.clone(),
            None,
            None,
            None,
            None,
            startup_repository_root.clone(),
            blackboards.clone(),
            workflows.clone(),
            workflow_cancellations.clone(),
            routing.clone(),
            unified_exec.clone(),
            lsp.clone(),
        );
        let promotion = PromotionStoreGateway::new(pool.clone());
        Self {
            pool,
            paths,
            startup_repository_root,
            subscriptions,
            approvals,
            questions,
            scanned: Arc::new(Mutex::new(scanned)),
            watchers: Arc::new(Mutex::new(HashMap::new())),
            run_control: Arc::new(Mutex::new(RunControlRegistry::default())),
            steerings: Arc::new(Mutex::new(HashMap::new())),
            github: None,
            mcp: None,
            search: None,
            transcriber: None,
            workflow_host,
            repository_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            promotion,
            blackboards,
            workflows,
            workflow_cancellations,
            routing,
            context: Arc::new(ContextAssembler::new()),
            embedder: None,
            retrieval: RetrievalSettings::default(),
            unified_exec,
            lsp,
        }
    }

    /// Inject the retrieval configuration (rubric 9): the embedding model the
    /// context assembler and `skills.search` embed with, and the `[retrieval]`
    /// tuning each run's agent runtime is built with.
    ///
    /// Unlike [`Self::with_github`]/[`Self::with_mcp`] this does NOT rebuild the
    /// workflow host: nothing it sets reaches a workflow agent node today (a
    /// node emits no context manifest), so rebuilding would only risk dropping
    /// another builder's injection for no gain. A node's runtime therefore keeps
    /// the default MCP threshold and is not offered `skills.search`.
    #[must_use]
    pub fn with_retrieval(
        mut self,
        embedder: Option<Arc<dyn SemanticEmbedder>>,
        retrieval: RetrievalSettings,
    ) -> Self {
        self.embedder = embedder;
        self.retrieval = retrieval;
        self
    }

    /// Set the repository root a `PublishDocument` command operates against
    /// (Phase 4 STEP 4.4). The assembly's `main` calls this with the same
    /// working directory it derives the startup repository identity from, so
    /// publication and the code-graph scan agree on one root.
    #[must_use]
    pub fn with_repository_root(mut self, root: PathBuf) -> Self {
        self.repository_root = root;
        self
    }

    /// Attach the speech-to-text seam (voice v1, rubric 8), so a
    /// `SubmitUserInput` carrying an audio `InputEnvelope` can be transcribed.
    /// Unlike `with_github`/`with_search` this rebuilds nothing: transcription
    /// happens in the SERVER's command path (before a run exists), not inside
    /// the agent loop or a workflow node, so the workflow host is unaffected.
    #[must_use]
    pub fn with_transcriber(
        mut self,
        transcriber: Arc<dyn codypendent_daemon::transcription::Transcriber>,
    ) -> Self {
        self.transcriber = Some(transcriber);
        self
    }

    /// Startup recovery for durable workflow runs (Phase 5 STEP 5.2): spawn a drive
    /// for every incomplete run so a crash-interrupted workflow resumes. Called from
    /// `main` alongside [`relaunch_queued_runs`](Self::relaunch_queued_runs); the
    /// drives run in the background, so this returns as soon as they are spawned.
    pub async fn recover_workflows(&self) -> anyhow::Result<usize> {
        Ok(self.workflow_host.recover().await?)
    }

    /// Re-arm approval-gated document publications whose durable continuation
    /// survived a daemon restart.
    pub async fn recover_document_publications(&self) -> anyhow::Result<usize> {
        let mut publisher = crate::publish::KnowledgePublisher::new(
            self.pool.clone(),
            self.approvals.clone(),
            self.repository_root.clone(),
            artifact_store(&self.paths),
        );
        if let Some(github) = &self.github {
            publisher = publisher.with_github(github.clone());
        }
        publisher.recover_pending().await
    }

    /// Inject the GitHub client (Phase 3 STEP 3.2). When set, the agent loop
    /// gains the `github.*` tools and the policy admits the GitHub API endpoint
    /// so a mutation reaches the approval gate (every write still needs approval).
    pub fn with_github(mut self, github: Arc<dyn GitHubApi>) -> Self {
        self.github = Some(github.clone());
        // Rebuild the workflow host so agent nodes drive with the same GitHub
        // client, but SHARE the existing drive-lock registry rather than minting
        // a fresh one (P5-D6c): today this is called once at startup before any
        // run exists, but a fresh registry would only be safe under that
        // construction-order assumption — carrying the same registry forward
        // means a drive already serializing under the OLD host would still
        // serialize against the NEW host for the same run id even if that
        // assumption ever stopped holding.
        let drive_locks = self.workflow_host.drive_locks();
        self.workflow_host = build_workflow_host(
            self.pool.clone(),
            self.paths.clone(),
            self.subscriptions.clone(),
            self.approvals.clone(),
            Some(github),
            // Carry the MCP bridge and the search client forward across the
            // rebuild, like the drive-lock registry below — whichever of
            // `with_github`/`with_mcp`/`with_search` runs last must not drop
            // the others' injection.
            self.mcp.clone(),
            self.search.clone(),
            Some(drive_locks),
            self.startup_repository_root.clone(),
            self.blackboards.clone(),
            // Carry the SAME node-lifecycle hub + cancellation registry forward (like
            // `drive_locks`), so a drive that started under the OLD host still
            // publishes to — and is cancellable through — the shared instances (T9).
            self.workflows.clone(),
            self.workflow_cancellations.clone(),
            self.routing.clone(),
            self.unified_exec.clone(),
            self.lsp.clone(),
        );
        self
    }

    /// Inject the MCP bridge (PR B — MCP client). When set, the agent loop gains
    /// the `mcp.<server>.<tool>` tools the registry's warm servers offer (every
    /// call is still dispositioned by the policy's `[mcp]` section). The
    /// workflow host is rebuilt exactly as [`Self::with_github`] rebuilds it —
    /// SHARING the existing drive-lock registry, hubs, and cancellation
    /// registry, and carrying the GitHub client and search client forward — so
    /// a workflow agent node's runtime is configured identically to a
    /// single-agent run's no matter which builder ran last.
    pub fn with_mcp(mut self, mcp: Arc<McpRegistry>) -> Self {
        self.mcp = Some(mcp);
        let drive_locks = self.workflow_host.drive_locks();
        self.workflow_host = build_workflow_host(
            self.pool.clone(),
            self.paths.clone(),
            self.subscriptions.clone(),
            self.approvals.clone(),
            self.github.clone(),
            self.mcp.clone(),
            self.search.clone(),
            Some(drive_locks),
            self.startup_repository_root.clone(),
            self.blackboards.clone(),
            self.workflows.clone(),
            self.workflow_cancellations.clone(),
            self.routing.clone(),
            self.unified_exec.clone(),
            self.lsp.clone(),
        );
        self
    }

    /// Inject the web-search client (PR C1). When set, the agent loop gains the
    /// `web.search` tool and the policy admits the Tavily API endpoint on the
    /// network allow-list. The workflow host is rebuilt exactly as
    /// [`Self::with_github`]/[`Self::with_mcp`] rebuild it — SHARING the
    /// existing drive-lock registry, hubs, and cancellation registry, and
    /// carrying the GitHub client and MCP bridge forward — so a workflow agent
    /// node's runtime is configured identically to a single-agent run's no
    /// matter which builder ran last.
    pub fn with_search(mut self, search: Arc<dyn SearchApi>) -> Self {
        self.search = Some(search);
        let drive_locks = self.workflow_host.drive_locks();
        self.workflow_host = build_workflow_host(
            self.pool.clone(),
            self.paths.clone(),
            self.subscriptions.clone(),
            self.approvals.clone(),
            self.github.clone(),
            self.mcp.clone(),
            self.search.clone(),
            Some(drive_locks),
            self.startup_repository_root.clone(),
            self.blackboards.clone(),
            self.workflows.clone(),
            self.workflow_cancellations.clone(),
            self.routing.clone(),
            self.unified_exec.clone(),
            self.lsp.clone(),
        );
        self
    }

    /// Warm `repository`'s code graph when this daemon has no fold of its
    /// CURRENT revision, so [`emit_context`](Self::emit_context) opens with the
    /// right repository map.
    ///
    /// The gate is the checkout's `HEAD`, not a once-per-boot flag: a daemon
    /// lives for days across branch switches and pulls, and a bare flag pinned
    /// its repository map to whatever the tree looked like at the first run
    /// (2026-08-11 review). A run at an already-folded revision still costs
    /// nothing but the `rev-parse` the run's identity derivation already pays.
    /// A checkout with no resolvable `HEAD` reports the same `"workdir"`
    /// placeholder every time, so it scans exactly once, as before.
    ///
    /// The `std` mutex over the revision map is never held across an await; the
    /// mutual exclusion that matters is [`scan::lock_repository`], an async lock
    /// held across the whole scan.
    ///
    /// **Two callers fire this for one `codypendent run`** — the server's
    /// `CreateSession` hook (via [`Self::ensure_repository_scanned`]) and
    /// `spawn_run`. Before the async lock, both read the revision map, both saw
    /// "not folded", and both ran a full `codegraph::rebuild_repository` against the
    /// same database: reproducibly `database is locked`, after which the losing
    /// scan never recorded its revision (so the repository re-scanned on every
    /// later run), and a run could read the repository map between the winner's
    /// clear and its rebuild — a torn graph in the model's opening note
    /// (2026-08-13 review, F6). The guard is re-checked *under* the lock, so the
    /// second caller finds the fold already recorded and does no work.
    async fn ensure_scanned(&self, repository: RepositoryId, root: &Path) {
        let revision = scan::head_revision(root);
        let folded_current = {
            let seen = lock_recovering(&self.scanned);
            seen.get(&repository) == Some(&revision)
        };
        if folded_current {
            return;
        }
        let guard = scan::lock_repository(repository).await;
        // Re-check: another caller may have folded this exact revision while
        // this one waited for the lock. This is the check that makes the pair of
        // triggers idempotent — the cheap pre-check above only avoids the wait.
        let already_folded = {
            let seen = lock_recovering(&self.scanned);
            seen.get(&repository) == Some(&revision)
        };
        if already_folded {
            return;
        }
        match scan::scan_repository(&self.pool, repository, root).await {
            // The scan now reports what it saw (`ScanSummary`); it already logs
            // its own headline, including a warning when it folded nothing.
            Ok(_summary) => {
                lock_recovering(&self.scanned).insert(repository, revision);
                // Outcome 14: arm the live watcher the moment a repository has a
                // valid graph, so an edit made DURING the session — the agent's
                // own `edit_file` included — is folded incrementally and is
                // visible to the next tool call, with no commit and no restart.
                self.ensure_watching(repository, root);
                // Released before the docs sweep below: that sweep reads the
                // graph but never writes it, and holding the graph's writer lock
                // across it would stall the watcher for the sweep's duration.
                drop(guard);
                // The graph just changed, which is exactly when documentation
                // can have gone stale — so run the `/update-docs` sweep against
                // it (STEP 4.6, previously tested but never wired). It only
                // FILES SUGGESTIONS, so nothing it finds edits a document; a
                // failure is logged and the run continues, since documentation
                // maintenance must never gate agent work.
                match crate::docs_job::run_docs_check(&self.pool, root).await {
                    Ok(report) if report.stale_findings > 0 => info!(
                        %repository,
                        documents = report.documents_checked,
                        links = report.links_resolved,
                        stale = report.stale_findings,
                        suggestions = report.suggestions_filed,
                        "documentation staleness sweep filed suggestions"
                    ),
                    Ok(_) => {}
                    Err(error) => {
                        warn!(%repository, %error, "documentation staleness sweep failed")
                    }
                }
            }
            Err(error) => {
                warn!(%repository, %error, "code-graph scan failed; a later run will retry");
            }
        }
    }

    /// Arm this repository's live code-graph watcher, once (outcome 14).
    ///
    /// Idempotent by construction: the registry entry IS the "already watching"
    /// flag, so the second run against a repository re-uses the first run's
    /// watcher. A watcher that cannot be armed (an unsupported platform, an
    /// inotify limit) is logged and skipped — the daemon keeps working exactly
    /// as it did before, with a graph that only moves on `HEAD`.
    fn ensure_watching(&self, repository: RepositoryId, root: &Path) {
        let mut watchers = lock_recovering(&self.watchers);
        if watchers.contains_key(&repository) {
            return;
        }
        match scan::arm_watcher(self.pool.clone(), repository, root) {
            Ok(watcher) => {
                watchers.insert(repository, watcher);
            }
            Err(error) => warn!(
                %repository,
                %error,
                "could not arm the code-graph watcher; the graph will refresh only on a revision change"
            ),
        }
    }

    /// The content-addressed store rooted at `<data_dir>/artifacts`.
    fn artifacts(&self) -> ArtifactStore {
        artifact_store(&self.paths)
    }

    /// Re-launch every run still `Queued` at startup. A crash between committing
    /// the `StartRun` transaction and the fire-and-forget `spawn_run` leaves a run
    /// `Queued` with no worker; startup recovery only sweeps *live* states and
    /// skips `Queued`, so without this the run is stuck forever. Re-launching is
    /// safe — the agent loop does not re-emit `RunStarted` for an existing run.
    /// Returns how many were re-launched.
    pub async fn relaunch_queued_runs(&self) -> anyhow::Result<usize> {
        let rows: Vec<(String, String, String, String)> =
            sqlx::query_as("SELECT id, session_id, objective, mode FROM runs WHERE state = ?")
                .bind(projections::run_state_to_db(
                    codypendent_protocol::RunState::Queued,
                ))
                .fetch_all(&self.pool)
                .await?;

        let fallback = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let mut relaunched = 0usize;
        for (id, session, objective, mode) in rows {
            let (Ok(run_id), Ok(session_id)) = (
                id.parse::<codypendent_protocol::RunId>(),
                session.parse::<SessionId>(),
            ) else {
                warn!(run = %id, "skipping a queued run with an unparseable id");
                continue;
            };
            // Recover the run's own repository AND pinned model from its
            // originating StartRun command: relaunching against the daemon's cwd
            // would attribute a multi-checkout run's context and memories to the
            // wrong repository (issue #6 item 1), and dropping the pin would let a
            // crash-relaunched run resolve/route a different model than the
            // operator chose (STEP MP2). Fall back to the cwd exactly as the live
            // path does for an older client that sent no repository.
            let (repository, model) = queued_run_overrides(&self.pool, &id).await;
            let repository = repository.unwrap_or_else(|| fallback.clone());
            self.spawn_run(RunLaunch {
                session_id,
                run_id,
                objective,
                mode: projections::agent_mode_from_db(&mode),
                repository,
                model,
                // A crash-relaunched run recovers its repository and pinned
                // model (above) but not prior history: recovery re-runs the
                // SAME queued run, not a continuation (Task 2, continuous-
                // session plan — no construction site populates `prior` yet).
                prior: Vec::new(),
            });
            relaunched += 1;
        }
        Ok(relaunched)
    }

    /// Load a model registry + a Phase-1 policy from `<data_dir>/models.toml`,
    /// or an error string when none is configured. In a bare environment (no
    /// endpoint, no config) this is the expected path — the run is then failed
    /// cleanly by the caller. Delegates to the [`load_model_registry`] free
    /// function so the workflow agent-node executor shares the exact loading.
    fn load_registry(&self) -> Result<(ModelRegistry, ModelPolicy), String> {
        load_model_registry(&self.paths)
    }

    /// Build this run's [`PolicyEngine`] (PF4 — the policy-files design spec,
    /// "Executor wiring"). Loads the two file layers over the built-in
    /// defaults, trust-routed by origin (Decision 1):
    ///
    /// - the REPO-LOCAL `<repository>/.codypendent/policy.toml` is untrusted
    ///   (the repo may be one the agent is merely reviewing) and can only
    ///   narrow;
    /// - the GLOBAL `self.paths.global_policy_path()` (the operator's own
    ///   config directory) is trusted and may widen the shell/network
    ///   allow-lists and `fs_read`, or relax git/network approval.
    ///
    /// `PolicyEngine::load` applies the global layer first (widen-or-narrow)
    /// and the repo layer last (narrow-only), so the repo can always claw
    /// back what the global layer granted but never exceed it. When a GitHub
    /// client is configured, [`GITHUB_API_ENDPOINT`] is admitted on the
    /// network allow-list AFTER the load, so it composes with whatever the
    /// file layers granted rather than being lost to them — admitting the
    /// endpoint alone grants nothing; every GitHub write still requires
    /// approval. [`TAVILY_API_ENDPOINT`] is admitted the same way when a
    /// web-search client is configured (PR C1).
    ///
    /// A malformed or unknown-key file in EITHER layer is mapped to a legible
    /// error string here, and the caller (`execute`) propagates it with `?` so
    /// the run does not start. This must NEVER fall back to
    /// `PolicyEngine::with_defaults` on a load error — that would silently
    /// widen the effective policy back to the (weaker) built-ins for a layer
    /// an operator meant to narrow, or silently drop the widening they wrote
    /// a global `pytest` line to get, reproducing the exact honesty gap this
    /// wiring closes.
    fn load_run_policy(&self, repository: &Path) -> Result<PolicyEngine, String> {
        let repo_policy = repository.join(".codypendent").join("policy.toml");
        let global_policy = self.paths.global_policy_path();
        let mut policy = PolicyEngine::load(Some(&repo_policy), Some(&global_policy))
            .map_err(|e| format!("policy configuration error: {e}"))?;
        if self.github.is_some() {
            policy = policy.admitting_network([GITHUB_API_ENDPOINT.to_string()]);
        }
        if self.search.is_some() {
            policy = policy.admitting_network([TAVILY_API_ENDPOINT.to_string()]);
        }
        Ok(policy)
    }

    /// The pool-erased [`RunJournal`]. Delegates to the shared [`run_journal`].
    fn journal(&self) -> RunJournal {
        run_journal(&self.pool, &self.approvals)
    }

    /// The pool-erased [`ArtifactSink`] over the store + pool. Delegates to the
    /// shared [`artifact_sink`].
    fn sink(&self, store: ArtifactStore) -> Box<dyn ArtifactSink> {
        artifact_sink(&self.pool, store)
    }

    /// The run body: resolve a model, then drive the agent loop to a terminal
    /// disposition. `Ok(())` means the loop reached a terminal state itself;
    /// `Err(reason)` means the run could not run (e.g. no model configured) and
    /// the caller must fail it cleanly.
    async fn execute(
        &self,
        launch: &RunLaunch,
        reconstructed_prior: Vec<TurnItem>,
        token: CancellationToken,
    ) -> Result<(), String> {
        let (registry, policy) = self.load_registry()?;
        // The routed model AND the measured price the router chose it with
        // (outcome 20): the price is the only MEASURED rate in the product, it
        // reaches this frame and nowhere else, and it used to be dropped here —
        // which is why `runs.cost_micros` was NULL for every agent run.
        let (model_id, price_per_1k_usd, routed_selection) = match &launch.model {
            // A model PINNED by the operator via the `/model` picker (STEP MP2):
            // run on exactly it — but a pin must NEVER bypass the classification
            // hard filter. When routing is ENABLED, validate the pin against the
            // run's classification / off-device ceiling and FAIL CLOSED (refuse,
            // like a routing refusal) if it is ineligible (e.g. a hosted model
            // pinned for classified data); when routing is OFF the pin selects the
            // model under the existing classification-blind Phase-1 posture — no
            // worse than today. A pin overrides the router's *quality* judgment,
            // never its *security* constraint.
            Some(pinned) => {
                // Task 8 / STEP 7.2: a pin bypasses routing UTILITY, not the
                // hard filter. When routing is ENABLED, validate the pin against the
                // hard filter and fail the run CLOSED (the same error message shape
                // like a routing refusal) if it is ineligible (e.g. a hosted model
                // pinned for classified data); when routing is OFF the pin selects the
                // model unchecked, preserving the Phase-1 baseline.
                let run_classification = derive_run_classification(None, &launch.objective);
                self.routing
                    .validate_pin(
                        launch.mode,
                        "agent",
                        &launch.objective,
                        estimate_input_tokens(&launch.objective),
                        run_classification,
                        pinned,
                    )
                    .await
                    .map_err(|e| format!("routing refused the pinned model: {e}"))?;
                registry.check_model(pinned).await.map_err(|error| {
                    format!("pinned model `{pinned}` is not available: {error}")
                })?;
                // A pin bypasses routing, so no measured price exists for it.
                // Unmeasured price ⇒ unmeasured cost, never a fabricated zero.
                (pinned.clone(), None, None)
            }
            // No pin: the Phase-7 routing seam (STEP 7.2/7.3), DEFAULT OFF. When
            // routing is enabled the router picks the model from the measured
            // profile store and the decision is recorded in the run trace; when it
            // is disabled (the default, and the state in every existing test — none
            // writes a `routing.toml`) this returns `None` and the model is
            // resolved exactly as before. A refusal (classified data with no
            // eligible model) fails the run CLOSED here rather than leaking
            // off-device through the classification-blind Phase-1 resolver.
            None => {
                let run_classification = derive_run_classification(None, &launch.objective);
                let routed = self
                    .routing
                    .select(
                        launch.mode,
                        "agent",
                        &launch.objective,
                        estimate_input_tokens(&launch.objective),
                        run_classification,
                    )
                    .await
                    .map_err(|e| format!("routing refused to place this run: {e}"))?;
                match routed {
                    Some(selection) => {
                        registry
                            .check_model(selection.model())
                            .await
                            .map_err(|error| {
                                format!(
                                    "routed model `{}` is not available: {error}",
                                    selection.model()
                                )
                            })?;
                        if let Err(error) = self
                            .routing
                            .record_decision(launch.session_id, launch.run_id, &selection.decision)
                            .await
                        {
                            warn!(run_id = %launch.run_id, %error, "could not record the routing decision in the trace");
                        }
                        (
                            selection.model().clone(),
                            selection.price_per_1k_usd,
                            Some(selection),
                        )
                    }
                    None => (
                        self.resolve_run_model(&registry, &policy, launch.mode)
                            .await?,
                        None,
                        None,
                    ),
                }
            }
        };
        info!(run_id = %launch.run_id, model = %model_id, "resolved model; executing run");

        if let Some(agent_id) = registry.acp_agent_id(&model_id) {
            return self
                .execute_acp(launch, &model_id, agent_id, reconstructed_prior, token)
                .await;
        }

        let driver = FrameworkModelDriver::from_registry(&registry, model_id)
            .await
            .map_err(|e| format!("could not build model client: {e}"))?
            // The routed model's measured rate, applied where the tokens are
            // measured. `None` (a pin, routing off, or a profile with no
            // measured price) keeps the run's cost UNMEASURED rather than
            // reporting a zero nobody measured.
            .with_price_per_1k_usd(price_per_1k_usd);

        let policy = self.load_run_policy(&launch.repository)?;

        let mut runtime = FrameworkAgentRuntime::new(
            registry,
            policy,
            self.approvals.clone(),
            self.subscriptions.clone(),
            self.journal(),
            self.sink(self.artifacts()),
        );
        if let Some(github) = &self.github {
            runtime = runtime.with_github(github.clone());
        }
        if let Some(mcp) = &self.mcp {
            runtime = runtime.with_mcp(mcp.clone());
        }
        if let Some(search) = &self.search {
            runtime = runtime.with_search(search.clone());
        }
        // Rubric 9: the MCP top-k threshold this run gates its tool
        // advertisement with, and the registry seam that backs `skills.search`.
        // Both are unconditional — an unset `[retrieval]` table yields the
        // default threshold, and the search tool reads the same pool + funnel
        // `emit_context` just used.
        runtime = runtime
            .with_mcp_top_k(self.retrieval.mcp_top_k)
            .with_builtin_top_k(self.retrieval.builtin_top_k)
            .with_registry_search(Arc::new(PoolRegistrySearch::new(
                self.pool.clone(),
                self.embedder.clone(),
            )))
            // Outcome 5: the `graph.*` tools. A pure read of the derived graph
            // this daemon's own scan wrote, so it is wired unconditionally like
            // the registry search above; a repository with no folded graph
            // simply answers "no results".
            .with_code_graph(Arc::new(crate::scan::PoolCodeGraph::new(self.pool.clone())))
            // The agent lever on the same graph (`graph.assert_edge`): a run can
            // record a relation the parser cannot see. Wired beside the read
            // seam and over the same pool, so an assertion lands under the id
            // the scan folded and the next `graph.callers_of` can see it. The
            // store refuses to let it displace a resolved fact; nothing here
            // needs to.
            .with_code_graph_assertions(Arc::new(
                crate::graph_assertions::PoolCodeGraphAssertions::new(self.pool.clone()),
            ))
            // Outcome 11: the writeback that fills `performance.task_class_success`.
            // Unconditional like the reads above — the store no-ops for a model
            // with no benched profile, so an unbenched deployment is unaffected.
            .with_routing_outcomes(Arc::new(crate::routing_outcomes::PoolRoutingOutcomes::new(
                self.pool.clone(),
            )));
        // The `docs.*` tools (rubric #4): always wired — this daemon always has
        // the knowledge fabric. What an agent may actually do to a document is
        // bounded by the document's collaboration mode inside the channel, not
        // by withholding the tools.
        runtime = runtime.with_docs(Arc::new(crate::docs_channel::AssemblyDocsChannel::new(
            self.pool.clone(),
            self.startup_repository_root.clone(),
        )));
        // Rubrics 5 / 10: a PLAIN chat run gets the repository-scoped workflow
        // read and backlog tools — the whole point is that "break this feature
        // into backlog cards" and "how did the last /fix-ci go?" work in ordinary
        // conversation, not only inside a workflow node. Both are wired here
        // unconditionally; the offering gate is the run's repository identity,
        // set below.
        runtime = runtime
            .with_workflow_query(Arc::new(AssemblyWorkflowQuery::new(self.pool.clone())))
            .with_workflow_control(Arc::new(self.workflow_host.clone()))
            .with_task_board(Arc::new(AssemblyTaskBoardChannel::new(
                self.pool.clone(),
                self.blackboards.clone(),
            )))
            .with_councils(Arc::new(FileCouncilService::new(self.paths.clone())))
            .with_questions(Arc::new(PoolQuestionChannel {
                pool: self.pool.clone(),
                broker: self.questions.clone(),
            }))
            .with_plan_bridge(Arc::new(PoolPlanBridge::new(
                self.pool.clone(),
                self.subscriptions.clone(),
            )));

        let hook_engine: Option<Arc<dyn codypendent_daemon::hook_engine::HookDispatch>> =
            match codypendent_sandbox::enforcing_executor() {
                Ok(exec) => Some(Arc::new(codypendent_daemon::hook_engine::HookEngine::new(
                    self.pool.clone(),
                    Arc::from(exec),
                ))),
                Err(err) => {
                    tracing::debug!(
                        ?err,
                        "enforcing sandbox executor unavailable; hooks disabled"
                    );
                    None
                }
            };
        runtime = runtime
            .with_hooks(hook_engine)
            .with_unified_exec(self.unified_exec.clone());
        if let Some(lsp) = &self.lsp {
            runtime = runtime.with_lsp(lsp.clone());
        }

        // Bind the run's worktree (STEP 1.8, the Phase-1 follow-up): a writing
        // mode (`Build`) gets a DEDICATED, isolated worktree carved from the
        // repository through the [`WorktreeManager`], so its writes never touch
        // the shared checkout; a read-only mode (Explore/Ask/Plan/Review — writes
        // denied by policy) keeps running in the repository root, exactly as
        // before.
        let manager = WorktreeManager::new();
        let binding = bind_run_worktree(
            &self.pool,
            &self.artifacts(),
            &manager,
            launch.run_id,
            run_writes_to_worktree(launch.mode),
            &launch.repository,
        )
        .await?;

        // The agent operates ENTIRELY within its bound tree: the policy read/search
        // root (`$REPOSITORY`) and the write root (`$WORKTREE`) are BOTH that tree,
        // so a write and its read-back hit the same directory (read-your-writes).
        // For an isolated run that tree is the worktree (a full checkout at HEAD,
        // outside the repository, so `$REPOSITORY` = the repo would NOT cover it);
        // for a read-only run it is the repository root. Repository IDENTITY (the
        // code graph, curated memories, and the GitHub target) stays the run's
        // repository `R`, resolved separately — in `spawn_run`'s scan and in the
        // GitHub resolution below — never conflated with this policy read root.
        let is_writing_run = binding.lease.is_some();
        let operating_tree = binding.worktree.clone();
        let guard = WorktreeReleaseGuard::arm(
            self.pool.clone(),
            self.artifacts(),
            manager,
            self.unified_exec.clone(),
            binding,
        );

        if is_writing_run {
            if let Err(e) = codypendent_daemon::checkpoints::record_checkpoint(
                &self.pool,
                &self.subscriptions,
                launch.session_id,
                &launch.repository,
                &operating_tree,
                launch.run_id,
                1,
            )
            .await
            {
                warn!(run_id = %launch.run_id, error = %e, "could not record launch checkpoint");
            }
        }

        let home = std::env::var("HOME").ok().map(PathBuf::from);
        let instructions = codypendent_runtime::instructions::discover_instructions(
            &operating_tree,
            home.as_deref(),
        );

        let mut ctx = RunContext::new(
            launch.session_id,
            launch.run_id,
            launch.objective.clone(),
            launch.mode,
            operating_tree.clone(),
            operating_tree.clone(),
        )
        .with_instructions(instructions.clone());
        let driver = driver.with_instructions(instructions);
        let (steer_tx, steer_rx) = tokio::sync::mpsc::unbounded_channel();
        lock_recovering(&self.steerings).insert(launch.run_id, steer_tx);
        ctx = ctx.with_steering(steer_rx);

        if is_writing_run {
            ctx = ctx.with_checkpointer(Arc::new(PoolTurnCheckpointer {
                pool: self.pool.clone(),
                subscriptions: self.subscriptions.clone(),
                session_id: launch.session_id,
                repository: launch.repository.clone(),
                worktree: operating_tree.clone(),
                run_id: launch.run_id,
            }));
        }
        // Seed the run's transcript with the prior conversation
        // (continuous-session plan). The live source is the ledger-reconstructed
        // `reconstructed_prior` (Task 3) — this crate sees `TurnItem` directly.
        // The launch's own `PriorTurn` carrier (Task 2) is honored ahead of it
        // for completeness, but every construction site leaves it empty today, so
        // the seed is exactly the reconstructed prior (empty for a first run,
        // making this behavior-neutral there).
        let mut prior = convert_launch_prior(&launch.prior);
        prior.extend(reconstructed_prior);
        ctx = ctx.with_prior(prior);
        // The board/history subject is the run's repository IDENTITY (`R`) — never
        // the operating tree above, which for an isolated run is a throwaway
        // worktree. Cards must accumulate on one board per checkout, not scatter
        // across per-run worktrees (rubrics 5 / 10).
        ctx = ctx.with_repository_identity(launch.repository.to_string_lossy().into_owned());
        // Resolve the run's GitHub `owner/repo` from the checkout's origin remote,
        // so the `github.*` tools know their target. Uses the repository IDENTITY
        // (`R`), not the worktree read root. Only meaningful when a client is
        // configured; a checkout with no GitHub origin leaves the tools inert.
        if self.github.is_some() {
            if let Some(repo) = resolve_github_repo(&launch.repository).await {
                ctx = ctx.with_github_repo(repo);
            }
        }

        // Seed the run with the session's latest IDE context (Phase 3 STEP 3.4),
        // so the read path can flag a file whose disk bytes diverge from an unsaved
        // editor buffer. Absent (no attached IDE) leaves the read path unchanged.
        match projections::load_ide_context(&self.pool, launch.session_id).await {
            Ok(Some(ide)) if !ide.dirty_buffers.is_empty() => {
                ctx = ctx.with_ide_context(ide.dirty_buffers);
            }
            Ok(_) => {}
            Err(error) => warn!(%error, "could not load IDE context for the run"),
        }

        // Drive the loop, then release the worktree — the guard releases it even if
        // the loop unwinds (the manager preserves any unmerged work as a patch
        // before teardown; a read-only run bound no worktree, so release is a no-op).
        let outcome = runtime.execute_run(&driver, ctx, token).await;
        // Measured usage is persisted by the runtime journal before
        // `RunCompleted`. That terminal event is the CloseSession barrier, so
        // no projection/event write may be deferred until after execute_run.
        // A routed run that errored: note the escalation tier the policy WOULD
        // advance to — and say plainly that nothing was switched.
        //
        // This deliberately does NOT call `RoutingCoordinator::escalate` +
        // `record_transition`. That pair writes an old→new model switch stamped
        // `artifacts_preserved: true`, but nothing here re-executes the run (the
        // live mid-run re-drive is still unwired — re-driving would emit a second
        // terminal `RunCompleted`), so it would fabricate a switch that never
        // happened in every failed routed run's trace. It also read the profile
        // store and could fire a capability PROBE on the failure path;
        // `escalation_candidate` is pure policy arithmetic.
        if let Err(ref e) = outcome {
            if let Some(ref selection) = routed_selection {
                let from = selection.model().clone();
                if let Some(to) = self.routing.escalation_candidate(&from).cloned() {
                    if let Err(err) = self
                        .routing
                        .record_escalation_candidate(
                            launch.session_id,
                            launch.run_id,
                            &from,
                            &to,
                            &e.to_string(),
                        )
                        .await
                    {
                        warn!(run_id = %launch.run_id, %err, "could not record the escalation candidate in trace");
                    }
                }
            }
        }
        let result = outcome.map(|_| ()).map_err(|e| format!("run failed: {e}"));
        guard.release().await;
        result
    }

    /// Resolve the first runnable policy candidate. ACP profiles need a
    /// filesystem-aware launch check that the generic model registry cannot do;
    /// keeping that check in the candidate walk preserves native fallback
    /// semantics when (for example) `uvx` is missing or a binary is not installed.
    async fn resolve_run_model(
        &self,
        registry: &ModelRegistry,
        policy: &ModelPolicy,
        mode: AgentMode,
    ) -> Result<ModelId, String> {
        let candidates = policy.candidates(mode);
        if candidates.is_empty() {
            return Err(format!(
                "no model configured: no candidates configured for {mode:?}"
            ));
        }
        let acp = AcpRegistryStore::new(&self.paths.data_dir);
        // Profiles may arrive through models.toml or a restored session before
        // this process has ever opened the provider picker. Seed/refresh the
        // official catalogue here as well, so ACP discovery is automatic on
        // the daemon path rather than accidentally depending on prior UI use.
        if candidates.iter().any(|id| {
            registry
                .acp_agent_id(id)
                .is_some_and(|coordinate| !coordinate.contains('@'))
        }) {
            // A pinned connected profile resolves from its immutable local
            // snapshot even while offline. Best-effort discovery here only
            // seeds unpinned/backward-compatible profiles and never blocks a
            // previously connected agent when the registry is unreachable.
            let _ = acp.load_or_refresh().await;
        }
        let mut attempts = Vec::with_capacity(candidates.len());
        for id in candidates {
            if registry.get(id).is_none() {
                attempts.push(format!("{id}: model not registered"));
                continue;
            }
            if let Some(agent_id) = registry.acp_agent_id(id) {
                match acp.launch_spec(agent_id) {
                    Ok(_) => return Ok(id.clone()),
                    Err(error) => attempts.push(format!("{id}: {error}")),
                }
                continue;
            }
            match registry.check_model(id).await {
                Ok(()) => return Ok(id.clone()),
                Err(error) => attempts.push(format!("{id}: {error}")),
            }
        }
        Err(format!(
            "no model configured: every candidate failed for {mode:?}: {}",
            attempts.join("; ")
        ))
    }

    /// Execute a configured ACP profile as a first-class agent runtime. The
    /// external process owns its model/tool loop; Codypendent still owns the
    /// worktree, durable event ledger, approvals, cancellation, change review,
    /// chronicle, and terminal state.
    async fn execute_acp(
        &self,
        launch: &RunLaunch,
        model_id: &ModelId,
        registry_agent_id: &str,
        reconstructed_prior: Vec<TurnItem>,
        token: CancellationToken,
    ) -> Result<(), String> {
        let store = AcpRegistryStore::new(&self.paths.data_dir);
        let launch_spec = store
            .launch_spec(registry_agent_id)
            .map_err(|error| format!("ACP agent `{registry_agent_id}` is unavailable: {error}"))?;

        let manager = WorktreeManager::new();
        let binding = bind_run_worktree(
            &self.pool,
            &self.artifacts(),
            &manager,
            launch.run_id,
            run_writes_to_worktree(launch.mode),
            &launch.repository,
        )
        .await?;
        let is_writing_run = binding.lease.is_some();
        let operating_tree = binding.worktree.clone();
        let guard = WorktreeReleaseGuard::arm(
            self.pool.clone(),
            self.artifacts(),
            manager,
            self.unified_exec.clone(),
            binding,
        );

        if is_writing_run {
            if let Err(e) = codypendent_daemon::checkpoints::record_checkpoint(
                &self.pool,
                &self.subscriptions,
                launch.session_id,
                &launch.repository,
                &operating_tree,
                launch.run_id,
                1,
            )
            .await
            {
                warn!(run_id = %launch.run_id, error = %e, "could not record launch checkpoint");
            }
        }

        if token.wait_until_running().await.is_none() {
            self.finish_acp_run(
                launch,
                model_id,
                &launch_spec,
                AcpRunCompletion {
                    state: RunState::Cancelled,
                    disposition: RunDisposition::Cancelled {
                        reason: Some("run cancelled before ACP execution".to_string()),
                    },
                    summary: None,
                    changed_files: Vec::new(),
                },
            )
            .await?;
            guard.release().await;
            return Ok(());
        }
        self.transition_acp(launch.session_id, launch.run_id, RunState::Preparing)
            .await?;

        let command = launch_spec.command.to_string_lossy().into_owned();
        // The external agent inherits the same operator-declared MCP servers a
        // native run is offered, so delegating a run does not silently shrink
        // the tool surface. Launch specs only — never the operator's `env`
        // pairs (see `forwardable_mcp_servers`).
        let mut client = AcpClient::spawn_with(
            &command,
            &launch_spec.args,
            &launch_spec.env,
            operating_tree.to_string_lossy().as_ref(),
            AcpSessionOptions {
                mcp_servers: forwardable_mcp_servers(&self.paths.global_mcp_path()),
            },
        )
        .await
        .map_err(|error| format!("could not start ACP agent `{registry_agent_id}`: {error}"))?;
        // A profile pinned to one of the agent's own models (`…#model`) selects
        // it for this session BEFORE the turn starts. A failure here is fatal:
        // silently running the agent's default model would attribute the run to
        // a model that never executed it.
        if let Some(model) = agent_model_from_coordinate(registry_agent_id) {
            client.set_model(model).await.map_err(|error| {
                format!("ACP agent `{registry_agent_id}` could not select model `{model}`: {error}")
            })?;
        }
        self.transition_acp(launch.session_id, launch.run_id, RunState::Running)
            .await?;

        let mut prior = convert_launch_prior(&launch.prior);
        prior.extend(reconstructed_prior);
        let objective = render_acp_prompt(&prior, &launch.objective);
        let mut sink = AcpRunSink {
            pool: self.pool.clone(),
            subscriptions: self.subscriptions.clone(),
            approvals: self.approvals.clone(),
            session_id: launch.session_id,
            run_id: launch.run_id,
            actor: Actor::Agent {
                agent_id: AgentId::new(),
                run_id: launch.run_id,
                model: model_id.clone(),
            },
            registry_agent_id: registry_agent_id.to_string(),
            cancellation: token.clone(),
            assistant_text: String::new(),
            tools: Vec::new(),
            failure: None,
        };

        // How long a cancelled agent gets to wind its turn down gracefully
        // before the process group is torn down. Long enough for an agent to
        // finish an in-flight write and answer `cancelled`; short enough that a
        // wedged agent cannot hold a cancelled run open.
        const CANCEL_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

        let stop = {
            // Taken BEFORE the prompt borrows the client mutably.
            let cancel = client.cancel_handle();
            let prompt = client.prompt(&objective, launch.run_id, &mut sink);
            tokio::pin!(prompt);
            tokio::select! {
                result = &mut prompt => {
                    result.map_err(|error| format!("ACP prompt failed: {error}"))?
                }
                _ = token.cancelled() => {
                    // Graceful first: `session/cancel` lets the agent stop its
                    // own tool loop and report the wire-correct `cancelled`
                    // stop reason. Only when it will not wind down inside the
                    // grace period does teardown fall back to killing the
                    // process group (the `drop` below).
                    cancel.cancel().await;
                    match tokio::time::timeout(CANCEL_GRACE, &mut prompt).await {
                        Ok(Ok(stop)) => stop,
                        Ok(Err(error)) => {
                            warn!(run_id = %launch.run_id, %error, "cancelled ACP turn ended with an error");
                            AcpStopReason::Cancelled
                        }
                        Err(_) => {
                            warn!(
                                run_id = %launch.run_id,
                                "ACP agent did not acknowledge session/cancel; tearing down its process group"
                            );
                            AcpStopReason::Cancelled
                        }
                    }
                }
            }
        };
        drop(client); // aborts the driver/process group if a cancellation won.
        if let Some(failure) = sink.failure.take() {
            guard.release().await;
            return Err(failure);
        }

        // A cancelled external agent may already have produced useful edits.
        // Snapshot them before worktree teardown just like a completed turn.
        let changed_files = self
            .emit_acp_changeset(launch, &operating_tree, &sink.actor)
            .await?;
        let empty_end_turn =
            matches!(stop, AcpStopReason::EndTurn) && sink.assistant_text.trim().is_empty();
        let (state, disposition) = match stop {
            AcpStopReason::EndTurn if empty_end_turn => (
                RunState::Failed,
                RunDisposition::Failed {
                    reason: format!(
                        "ACP agent `{registry_agent_id}` ended the turn without returning an assistant message; retry after updating or re-authenticating the agent"
                    ),
                },
            ),
            AcpStopReason::EndTurn => (
                RunState::Completed,
                RunDisposition::Completed {
                    summary: last_nonempty_line(&sink.assistant_text),
                },
            ),
            AcpStopReason::Cancelled => (
                RunState::Cancelled,
                RunDisposition::Cancelled {
                    reason: Some("run cancelled".to_string()),
                },
            ),
            AcpStopReason::Refusal => (
                RunState::Failed,
                RunDisposition::Failed {
                    reason: "ACP agent refused the prompt".to_string(),
                },
            ),
        };
        self.finish_acp_run(
            launch,
            model_id,
            &launch_spec,
            AcpRunCompletion {
                state,
                disposition,
                summary: last_nonempty_line(&sink.assistant_text),
                changed_files,
            },
        )
        .await?;
        guard.release().await;
        Ok(())
    }

    async fn transition_acp(
        &self,
        session_id: SessionId,
        run_id: RunId,
        state: RunState,
    ) -> Result<(), String> {
        let event = ledger::append_run_state_changed(
            &self.pool,
            session_id,
            &Actor::System,
            run_id,
            state,
            Utc::now(),
        )
        .await
        .map_err(|error| error.to_string())?;
        self.subscriptions.publish(session_id, event);
        Ok(())
    }

    async fn finish_acp_run(
        &self,
        launch: &RunLaunch,
        model_id: &ModelId,
        agent: &codypendent_integrations::acp_registry::AcpLaunchSpec,
        completion: AcpRunCompletion,
    ) -> Result<(), String> {
        let chronicle = serde_json::json!({
            "objective": launch.objective,
            "runtime": {
                "protocol": "acp",
                "agent": agent.registry_id,
                "agentName": agent.name,
                "agentVersion": agent.version,
                "profile": model_id,
            },
            "summary": completion.summary,
            "investigations": [],
            "actions": [],
            "changes": completion.changed_files,
            "costs": {"model_requests": null, "tokens": null, "cost_micros": null},
            "unresolved": []
        });
        let chronicle_ref = self
            .artifacts()
            .put(
                &self.pool,
                "application/json",
                DataClassification::Internal,
                Provenance::system("run-chronicle"),
                &serde_json::to_vec_pretty(&chronicle).map_err(|error| error.to_string())?,
            )
            .await
            .map_err(|error| error.to_string())?;
        self.transition_acp(launch.session_id, launch.run_id, completion.state)
            .await?;
        let event = ledger::append_next_event(
            &self.pool,
            launch.session_id,
            &Actor::System,
            &EventBody::RunCompleted {
                run_id: launch.run_id,
                disposition: completion.disposition,
                chronicle: chronicle_ref,
            },
            Utc::now(),
        )
        .await
        .map_err(|error| error.to_string())?;
        self.subscriptions.publish(launch.session_id, event);
        Ok(())
    }

    async fn emit_acp_changeset(
        &self,
        launch: &RunLaunch,
        worktree: &Path,
        actor: &Actor,
    ) -> Result<Vec<String>, String> {
        if !run_writes_to_worktree(launch.mode) {
            return Ok(Vec::new());
        }
        // ACP agents write through their own tool loop, outside Codypendent's
        // typed file sink. Stage the isolated worktree (never the user's shared
        // checkout) so Git's binary diff includes new, deleted, and empty files
        // as well as tracked modifications.
        let staged = tokio::process::Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(["add", "--all", "--"])
            .output()
            .await
            .map_err(|error| format!("could not stage ACP worktree snapshot: {error}"))?;
        if !staged.status.success() {
            return Err(format!(
                "could not stage ACP worktree snapshot: {}",
                String::from_utf8_lossy(&staged.stderr).trim()
            ));
        }
        let diff = bounded_acp_git_diff(worktree).await?;
        if diff.is_empty() {
            return Ok(Vec::new());
        }
        let names = tokio::process::Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(["diff", "--name-only", "HEAD"])
            .output()
            .await
            .map_err(|error| error.to_string())?;
        let files = String::from_utf8_lossy(&names.stdout)
            .lines()
            .filter(|line| !line.is_empty())
            .take(10_000)
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let (additions, deletions) = diff_counts(worktree).await;
        let artifact = self
            .artifacts()
            .put(
                &self.pool,
                "text/x-diff",
                DataClassification::Internal,
                Provenance::tool_output("acp.agent", launch.run_id),
                &diff,
            )
            .await
            .map_err(|error| error.to_string())?;
        let preview_bytes = diff.len().min(64 * 1024);
        let preview = String::from_utf8_lossy(&diff[..preview_bytes]).into_owned();
        let event = ledger::append_next_event(
            &self.pool,
            launch.session_id,
            actor,
            &EventBody::PatchProposed {
                run_id: launch.run_id,
                changeset_id: ChangeSetId::new(),
                artifact,
                files: files.clone(),
                additions,
                deletions,
                preview,
                preview_truncated: diff.len() > preview_bytes,
            },
            Utc::now(),
        )
        .await
        .map_err(|error| error.to_string())?;
        self.subscriptions.publish(launch.session_id, event);
        Ok(files)
    }

    /// Append a run-scoped `NoteAppended` event to `session_id`'s ledger and
    /// publish it to the shared fan-out — append-then-publish, mirroring
    /// [`recovery::fail_run`] so an attached client observes the note live. Used
    /// to surface the context manifest and the curated memories in a run's trace.
    /// The note carries its `run_id` so a client routes it to the right run's
    /// transcript even when runs interleave (issue #6 item 3).
    async fn emit_note(
        &self,
        session_id: SessionId,
        run_id: RunId,
        text: String,
    ) -> anyhow::Result<()> {
        // Atomic sequence claim — the note may race a concurrent client command
        // on the same session.
        let event = ledger::append_next_event(
            &self.pool,
            session_id,
            &Actor::System,
            &EventBody::NoteAppended {
                text,
                run_id: Some(run_id),
            },
            Utc::now(),
        )
        .await?;
        // Persist-before-publish: only after the append does the note fan out.
        self.subscriptions.publish(session_id, event);
        Ok(())
    }

    /// Assemble the knowledge-fabric context (repository map + tool/skill cards +
    /// cited memories) for `objective` and note its render into the trace, so
    /// every run opens with the three manifests. Returns the rendered manifest —
    /// the SAME text the note carries — so the caller can also seed it into the
    /// model-visible transcript (2026-08-11 review item 1: previously the
    /// manifest reached only this trace note and the model started blind).
    ///
    /// Called **before** the agent loop, never concurrently with it — the note is
    /// appended and published from the worker before `execute` spawns, so it can
    /// never race the loop for a sequence. A fabric failure is warned and swallowed
    /// (context is an aid, never a gate on running); a note-append failure still
    /// returns the manifest — the model seed must not depend on the trace write.
    async fn emit_context(
        &self,
        session_id: SessionId,
        run_id: RunId,
        repository: RepositoryId,
        objective: &str,
    ) -> Option<String> {
        // System (built-ins) + the operator's local user scope (data-dir skills
        // installed via `codypendent skill add` / the startup scan register
        // there) + this repository (harvested run memories are stored at
        // repository visibility, and repo-local skills anchor here), so a memory
        // a prior run curated — and every locally installed skill — resurfaces.
        let scopes = [
            Scope::System,
            codypendent_knowledge::local_user_scope(),
            Scope::Repository(repository),
        ];
        // Rubric 9: with an `[embedding]` entry configured, dense retrieval runs
        // over the PERSISTED vectors that model produced, with the query embedded
        // in the same space — that path sources the registry per call, so it does
        // not use the stamped cache. With no model configured (the default) the
        // caching assembler serves the offline hashing indexes, which is both the
        // cheaper path and byte-for-byte the previous behaviour.
        let assembled = match self.embedder.as_deref() {
            Some(semantic) => {
                assemble_context_with(&self.pool, repository, objective, &scopes, Some(semantic))
                    .await
            }
            None => {
                self.context
                    .assemble(&self.pool, repository, objective, &scopes)
                    .await
            }
        };
        match assembled {
            Ok(manifest) => {
                let rendered = manifest.render();
                if let Err(error) = self.emit_note(session_id, run_id, rendered.clone()).await {
                    warn!(%session_id, %run_id, %error, "could not emit run context note");
                }
                Some(rendered)
            }
            Err(error) => {
                warn!(%session_id, %error, "could not assemble run context");
                None
            }
        }
    }

    /// Reconstruct a continuation run's prior transcript from the session ledger
    /// (continuous-session plan, Task 3): load the session's events and project
    /// the runs OTHER than this one into a seed `Vec<TurnItem>` (verbatim for the
    /// last [`VERBATIM_RUNS`], compacted older). The FIRST run of a session has no
    /// prior runs, so this is empty and the run starts cold exactly as before. A
    /// load failure degrades to an empty prior (start cold) rather than failing
    /// the run — the prior is an aid, never a gate on running.
    async fn reconstruct_prior(&self, session_id: SessionId, run_id: RunId) -> Vec<TurnItem> {
        match ledger::load_events(&self.pool, session_id).await {
            Ok(events) => {
                let turns = continuation_prior(events, run_id, VERBATIM_RUNS);
                // Task 3: hydrate every prior `ToolResult` that carries an
                // artifact ref (Task 2) with a bounded excerpt of its actual
                // stored content, so the seed transcript SHOWS the prior file
                // content instead of the `tool_result_summary` fallback.
                self.hydrate_tool_artifacts(turns).await
            }
            Err(error) => {
                warn!(%session_id, %run_id, %error, "could not load events to reconstruct the continuation prior; starting cold");
                Vec::new()
            }
        }
    }

    /// Hydrate a continuation's seed transcript (continuous-session plan,
    /// Task 3): for each `TurnItem::ToolResult` carrying an `artifact` (Task
    /// 2), replace its `tool_result_summary` fallback `output` with a bounded
    /// excerpt of the artifact's real stored bytes. The stored bytes already
    /// open with the path/line header a `read_file` observation was recorded
    /// with (Task 1 / #37), so that header survives at the front of the
    /// excerpt.
    ///
    /// **Best-effort, never fails the run:** any artifact that fails to open,
    /// read, or decode leaves that turn's `output` at its existing
    /// `"succeeded"` fallback and moves on — a missing/broken artifact must
    /// never fail continuation reconstruction (the module's degrade-to-cold
    /// ethos; mirrors `load_chronicle`'s callers, which warn and skip rather
    /// than propagate).
    ///
    /// **Bounded:** each artifact is capped at
    /// [`CONTINUATION_TOOL_EXCERPT_BYTES`], and an aggregate budget
    /// ([`CONTINUATION_HYDRATION_AGGREGATE_BYTES`]) across the whole seed
    /// protects a run that read many files — once spent, remaining artifacts
    /// are skipped (logged, not silently dropped).
    async fn hydrate_tool_artifacts(&self, mut turns: Vec<TurnItem>) -> Vec<TurnItem> {
        let mut hydrated_bytes = 0usize;
        for turn in &mut turns {
            let TurnItem::ToolResult {
                tool,
                output,
                artifact: Some(artifact),
            } = turn
            else {
                continue;
            };
            if hydrated_bytes >= CONTINUATION_HYDRATION_AGGREGATE_BYTES {
                info!(
                    %tool,
                    artifact = %artifact.id,
                    "continuation hydration aggregate budget spent; leaving the succeeded fallback"
                );
                continue;
            }
            match self
                .read_artifact_excerpt(artifact, CONTINUATION_TOOL_EXCERPT_BYTES)
                .await
            {
                Ok(excerpt) => {
                    hydrated_bytes = hydrated_bytes.saturating_add(excerpt.len());
                    *output = excerpt;
                }
                Err(error) => {
                    warn!(
                        %tool,
                        artifact = %artifact.id,
                        %error,
                        "could not hydrate a continuation's prior tool artifact; keeping the succeeded fallback"
                    );
                }
            }
        }
        turns
    }

    /// Read at most `cap` bytes from `artifact`'s stored blob — a BOUNDED
    /// read (`take`), never loading a huge blob fully — lossily decode to a
    /// `String`, and append [`CONTINUATION_TRUNCATION_MARKER`] when the
    /// artifact held more bytes than `cap`. Reuses the exact open/read
    /// pattern [`Self::load_chronicle`] uses for a chronicle artifact.
    async fn read_artifact_excerpt(
        &self,
        artifact: &ArtifactRef,
        cap: usize,
    ) -> anyhow::Result<String> {
        use tokio::io::AsyncReadExt;
        let file = self.artifacts().open(&self.pool, artifact.id).await?;
        // Read one byte past the cap so the post-read length tells us whether
        // more bytes remained; that extra byte is always truncated away below
        // and never surfaced.
        let mut limited = file.take(cap as u64 + 1);
        let mut buf = Vec::with_capacity(cap.min(8192) + 1);
        limited.read_to_end(&mut buf).await?;
        let truncated = buf.len() > cap;
        if truncated {
            buf.truncate(cap);
        }
        let mut excerpt = String::from_utf8_lossy(&buf).into_owned();
        if truncated {
            excerpt.push_str(CONTINUATION_TRUNCATION_MARKER);
        }
        Ok(excerpt)
    }

    /// Open a run's trace. The FIRST run of a session (empty reconstructed
    /// `prior`) gets the full knowledge-fabric context manifest via
    /// [`emit_context`](Self::emit_context) — returned so the caller seeds it
    /// into the model transcript too; a CONTINUATION (non-empty prior) gets a
    /// one-line [`CONTINUATION_CONTEXT_NOTE`] marker instead and returns `None`
    /// — its shared context already rides in the seeded transcript (the stored
    /// manifest note projected by `continuation_prior`), so re-assembling the
    /// full `=== CONTEXT` repo-map every follow-up would only re-pay tokens for
    /// nothing (continuous-session plan, Task 4).
    async fn emit_run_opening(
        &self,
        session_id: SessionId,
        run_id: RunId,
        repository: RepositoryId,
        objective: &str,
        prior: &[TurnItem],
    ) -> Option<String> {
        if prior.is_empty() {
            return self
                .emit_context(session_id, run_id, repository, objective)
                .await;
        }
        if let Err(error) = self
            .emit_note(session_id, run_id, CONTINUATION_CONTEXT_NOTE.to_string())
            .await
        {
            warn!(%session_id, %run_id, %error, "could not emit the continuation context marker");
        }
        None
    }

    /// Build a run's model-visible seed transcript AND open its trace — the one
    /// place both first runs and continuations decide what the model sees ahead
    /// of the objective (2026-08-11 review item 1):
    ///
    /// - a FIRST run reconstructs an empty prior, emits the full manifest note,
    ///   and seeds that SAME manifest as a bounded [`context_turn`] — so the
    ///   repo map, disclosed skill cards, and curated memories reach the model,
    ///   not just the human trace;
    /// - a CONTINUATION reconstructs the prior runs (whose head
    ///   `continuation_prior` already seeds from the stored manifest note) and
    ///   emits only the carried-context marker.
    ///
    /// The seed feeds BOTH execution paths: the native loop via
    /// `RunContext::with_prior` (the runtime appends the objective after it) and
    /// the ACP path via `render_acp_prompt`.
    async fn build_run_seed(
        &self,
        session_id: SessionId,
        run_id: RunId,
        repository: RepositoryId,
        objective: &str,
    ) -> Vec<TurnItem> {
        let mut prior = self.reconstruct_prior(session_id, run_id).await;
        if let Some(manifest) = self
            .emit_run_opening(session_id, run_id, repository, objective, &prior)
            .await
        {
            // First run: the manifest turn leads, the runtime pushes the
            // objective after the seed — evidence first, then direction.
            prior.insert(0, context_turn(&manifest));
        }
        prior
    }

    /// Read + JSON-parse the bytes behind a chronicle [`ArtifactRef`]
    /// (best-effort; the caller warns and skips on any error).
    async fn load_chronicle(&self, chronicle: &ArtifactRef) -> anyhow::Result<serde_json::Value> {
        use tokio::io::AsyncReadExt;
        let mut file = self.artifacts().open(&self.pool, chronicle.id).await?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).await?;
        Ok(serde_json::from_slice(&buf)?)
    }

    /// After a run reaches a terminal state, harvest curated memories from its own
    /// event trace and note each durable one, so "a run produces a curated memory
    /// whose provenance opens to its source" holds for every run.
    ///
    /// Public entry: resolves the [`FactExtractor`] for this run's mode (M3a
    /// always resolves [`NoopExtractor`]; M3b's `build_fact_extractor` adds the
    /// D2 selection order) and delegates to the testable core, [`Self::harvest_with`].
    async fn harvest_memories(
        &self,
        session_id: SessionId,
        run_id: RunId,
        repository: RepositoryId,
        mode: AgentMode,
    ) {
        let extractor = self.build_fact_extractor(mode).await;
        self.harvest_with(session_id, run_id, repository, extractor.as_ref())
            .await;
    }

    /// Fold a successful run into the governed learning ledger. This is
    /// intentionally separate from `harvest_memories`: the legacy harvest is a
    /// broad compatibility path, while learning capture accepts only explicit
    /// user preferences/corrections and locally successful allow-listed checks.
    /// Failure is observational and never changes a terminal run disposition.
    async fn harvest_learnings(
        &self,
        session_id: SessionId,
        run_id: RunId,
        repository: RepositoryId,
    ) {
        let events = match ledger::load_events(&self.pool, session_id).await {
            Ok(events) => events,
            Err(error) => {
                warn!(%session_id, %run_id, %error, "could not load events for learning capture");
                return;
            }
        };
        let report = match crate::learning_capture::capture_completed_run(
            &self.pool, &events, session_id, run_id, repository,
        )
        .await
        {
            Ok(report) => report,
            Err(error) => {
                warn!(%session_id, %run_id, %error, "curated learning capture failed");
                return;
            }
        };
        if report.is_empty() {
            return;
        }

        let body = EventBody::LearningsCaptured {
            run_id,
            proposed_count: report.proposed_ids.len() as u32,
            proposed_ids: report.proposed_ids,
            activated_count: report.activated_ids.len() as u32,
            activated_ids: report.activated_ids,
        };
        match ledger::append_next_event(&self.pool, session_id, &Actor::System, &body, Utc::now())
            .await
        {
            Ok(event) => self.subscriptions.publish(session_id, event),
            Err(error) => {
                // Records are already durable. A missing projection can be
                // reconstructed from the learning store and must not roll them
                // back or fail the completed run.
                warn!(%session_id, %run_id, %error, "could not project captured learnings");
            }
        }
    }

    /// M3b: the extractor this run's harvest is injected with, per the D2
    /// selection order: (1) a configured `memory_extraction_model` (from
    /// `routing.toml`), when set AND resolvable in the model registry;
    /// (2) else the run's own resolved model (the same `resolve_model` call
    /// `execute` uses); (3) else [`NoopExtractor`] — no model configured at
    /// all. Every step is fail-safe: a missing registry, an unresolvable
    /// model, or a client-construction error all fall back to
    /// `NoopExtractor` rather than failing the harvest (which itself never
    /// fails a run).
    ///
    /// NOT gated on `#[cfg(feature = "provider-openai")]`: `codypendentd`
    /// pulls `codypendent-runtime` with default features (provider-openai
    /// on) and already calls its `client_for`/`from_registry` unconditionally
    /// elsewhere (`execute`, above; `load_model_registry_resolves_a_key_from_auth_json`'s
    /// comment), and defines no `provider-openai` feature of its own — so
    /// gating here would make the real path dead code in every build this
    /// crate ships.
    async fn build_fact_extractor(&self, mode: AgentMode) -> Box<dyn FactExtractor> {
        let (registry, policy) = match self.load_registry() {
            Ok(rp) => rp,
            Err(_) => return Box::new(NoopExtractor), // no model configured ⇒ Noop
        };
        // D2 selection: (1) configured extraction model, (2) run's resolved
        // model, (3) Noop.
        let configured = RoutingConfig::load(&self.paths).memory_extraction_model;
        let model_id = match configured.filter(|id| registry.get(id).is_some()) {
            Some(id) => id,
            None => match resolve_model(&registry, &policy, mode).await {
                Ok(resolved) => resolved.id,
                Err(_) => return Box::new(NoopExtractor),
            },
        };
        // D2 config visibility: warn ONCE per process that extraction makes a
        // per-run model call, so an operator points `memory_extraction_model`
        // at a cheap model.
        static NOTE: std::sync::Once = std::sync::Once::new();
        NOTE.call_once(|| tracing::info!(
            "memory extraction makes a best-effort per-run model call; set `memory_extraction_model` in routing.toml to a cheap/local model to keep cost off the coding model"
        ));
        match codypendent_runtime::LlmFactExtractor::from_registry(&registry, model_id).await {
            Ok(extractor) => Box::new(extractor),
            Err(error) => {
                warn!(%error, "could not build memory extraction client; extraction disabled for this run");
                Box::new(NoopExtractor)
            }
        }
    }

    /// The testable harvest core: loads the ledger, appends the pure
    /// heuristic/agent-tool candidates, calls the injected `extractor`
    /// best-effort, re-anchors every candidate to REPOSITORY scope, and
    /// curates each through [`MemoryStore::curate`].
    ///
    /// Runs **after** `execute` returns (the loop is no longer appending), so the
    /// note appends never race the agent loop. The curator redacts secrets before
    /// anything is stored, so a `remembered:` note can never carry secret text.
    /// Every failure is warned and swallowed — a harvesting error must not turn a
    /// finished run into a failed one. `extractor.extract` itself can never fail
    /// (it returns `Vec`, never `Result`), so its contribution is best-effort by
    /// construction: an unreachable/slow/misconfigured model degrades to
    /// "contributes nothing," never to a failed run.
    async fn harvest_with(
        &self,
        session_id: SessionId,
        run_id: RunId,
        repository: RepositoryId,
        extractor: &dyn FactExtractor,
    ) {
        let events = match ledger::load_events(&self.pool, session_id).await {
            Ok(events) => events,
            Err(error) => {
                warn!(%session_id, %error, "could not load events for memory harvest");
                return;
            }
        };
        // Extract under the SESSION scope so the event-range extractors (repeated
        // `shell.run` procedures, explicit `memory.propose:` notes) can resolve
        // their evidence session id — a System scope yields none, harvesting only
        // chronicle memories. Then re-anchor each candidate to REPOSITORY
        // visibility so the curated memory resurfaces in later runs' context
        // (which `emit_context` queries at System + this repository); a
        // session-scoped memory would never be seen again.
        let repository_scope = Scope::Repository(repository);
        let session_scope = Scope::Session(session_id);
        let mut candidates = extract_candidates(&events, session_scope.clone());

        // Heuristic chronicle facts (M1). Locate the RunCompleted event, load its
        // chronicle artifact, parse it, and append discrete candidates. Every
        // step is best-effort: a miss/parse failure is warned and skipped, never
        // fatal to an otherwise-finished run.
        if let Some((chronicle_ref, seq, at)) = events.iter().rev().find_map(|e| match &e.body {
            EventBody::RunCompleted { chronicle, .. } => {
                Some((chronicle.clone(), e.sequence, e.occurred_at))
            }
            _ => None,
        }) {
            match self.load_chronicle(&chronicle_ref).await {
                Ok(chronicle) => {
                    let valid_from = Revision::sequence(seq);
                    candidates.extend(chronicle_candidates(
                        &chronicle,
                        &session_scope,
                        &chronicle_ref,
                        run_id,
                        at,
                        valid_from.clone(),
                        chronicle_ref.sensitivity,
                    ));

                    // M3a: the LLM-extractor seam (no model call yet — the
                    // injected extractor is best-effort NoopExtractor by
                    // default; M3b swaps in the real model-backed one).
                    let objective = chronicle["objective"].as_str().unwrap_or("");
                    let transcript_excerpt = run_transcript_excerpt(&events, run_id);
                    let input = ExtractionInput {
                        objective,
                        chronicle: &chronicle,
                        transcript_excerpt: &transcript_excerpt,
                        scope: &session_scope,
                        chronicle_ref: &chronicle_ref,
                        run_id,
                        observed_at: at,
                        valid_from,
                        sensitivity: chronicle_ref.sensitivity,
                    };
                    candidates.extend(extractor.extract(input).await);
                }
                Err(error) => {
                    warn!(%session_id, %run_id, %error, "could not load run chronicle for memory harvest");
                }
            }
        }

        for candidate in &mut candidates {
            candidate.scope = Some(repository_scope.clone());
        }
        let store = MemoryStore::new();
        for candidate in candidates {
            match store.curate(&self.pool, candidate).await {
                Ok(Curation::Accepted(record)) => {
                    if let Err(error) = self
                        .emit_note(
                            session_id,
                            run_id,
                            format!("remembered: {}", record.statement),
                        )
                        .await
                    {
                        warn!(%session_id, %run_id, %error, "could not emit curated-memory note");
                    }
                }
                // A detected contradiction resolves by supersession — never a
                // silent overwrite — but the note used to say exactly the same
                // generic "remembered:" an ordinary accepted fact gets, so the
                // user was never told a contradiction was found at all
                // (2026-08-13 review F5: "explicit contradiction resolution").
                // Say so directly, distinct from a plain new memory.
                Ok(Curation::Superseded { record, .. }) => {
                    if let Err(error) = self
                        .emit_note(
                            session_id,
                            run_id,
                            format!(
                                "remembered (replacing an earlier, contradicting note): {}",
                                record.statement
                            ),
                        )
                        .await
                    {
                        warn!(%session_id, %run_id, %error, "could not emit curated-memory note");
                    }
                }
                // Redacted / Duplicate / Rejected: nothing durable, nothing to note.
                Ok(_) => {}
                Err(error) => warn!(%session_id, %error, "memory curation failed"),
            }
        }
    }
}

/// A cheap join of this run's note/tool-observation texts from the session
/// ledger, un-capped (M3a): the extractor implementation is responsible for
/// bounding its own input, per [`ExtractionInput::transcript_excerpt`]'s
/// contract.
fn run_transcript_excerpt(events: &[codypendent_protocol::SessionEvent], run_id: RunId) -> String {
    use codypendent_protocol::ToolOutcome;

    events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::NoteAppended {
                text,
                run_id: note_run,
            } if *note_run == Some(run_id) => Some(text.clone()),
            EventBody::ToolCompleted {
                run_id: tool_run,
                tool,
                outcome,
                ..
            } if *tool_run == run_id => Some(match outcome {
                ToolOutcome::Succeeded => format!("{tool}: succeeded"),
                ToolOutcome::Failed { message } => format!("{tool}: failed - {message}"),
                _ => format!("{tool}: unknown outcome"),
            }),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Durable bridge from an ACP prompt to the session ledger and approval broker.
struct AcpRunCompletion {
    state: RunState,
    disposition: RunDisposition,
    summary: Option<String>,
    changed_files: Vec<String>,
}

struct AcpRunSink {
    pool: SqlitePool,
    subscriptions: SubscriptionHub,
    approvals: ApprovalBroker,
    session_id: SessionId,
    run_id: RunId,
    actor: Actor,
    registry_agent_id: String,
    cancellation: CancellationToken,
    assistant_text: String,
    tools: Vec<String>,
    failure: Option<String>,
}

impl AcpRunSink {
    async fn persist(&mut self, body: EventBody) -> anyhow::Result<()> {
        let event =
            ledger::append_next_event(&self.pool, self.session_id, &self.actor, &body, Utc::now())
                .await?;
        self.subscriptions.publish(self.session_id, event);
        Ok(())
    }

    async fn transition(&mut self, state: RunState) -> anyhow::Result<()> {
        let event = ledger::append_run_state_changed(
            &self.pool,
            self.session_id,
            &Actor::System,
            self.run_id,
            state,
            Utc::now(),
        )
        .await?;
        self.subscriptions.publish(self.session_id, event);
        Ok(())
    }

    fn fail(&mut self, error: impl std::fmt::Display) {
        if self.failure.is_none() {
            self.failure = Some(format!("could not persist ACP run event: {error}"));
        }
    }
}

#[async_trait]
impl AcpEventSink for AcpRunSink {
    async fn on_event(&mut self, event: EventBody) {
        match &event {
            EventBody::ModelStreamDelta { text, .. } => {
                if self.assistant_text.len() < 2 * 1024 * 1024 {
                    let remaining = 2 * 1024 * 1024 - self.assistant_text.len();
                    self.assistant_text.push_str(&bounded_text(text, remaining));
                }
            }
            EventBody::ToolStarted { tool, .. } if self.tools.len() < 10_000 => {
                self.tools.push(tool.clone());
            }
            _ => {}
        }
        if let Err(error) = self.persist(event).await {
            self.fail(error);
        }
    }

    async fn on_permission(
        &mut self,
        tool_call: serde_json::Value,
        options: Vec<PermissionOption>,
    ) -> Option<String> {
        if self.failure.is_some() || self.cancellation.is_cancelled() {
            return reject_option(&options);
        }
        let title = tool_call
            .get("title")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                tool_call
                    .get("toolCallId")
                    .and_then(serde_json::Value::as_str)
            })
            .unwrap_or("ACP tool call")
            .to_string();
        let canonical = canonical_json(tool_call);
        let action = ProposedAction::AcpToolCall {
            agent: self.registry_agent_id.clone(),
            title: bounded_text(&title, 512),
            details: bounded_text(&canonical.to_string(), 32 * 1024),
        };
        let risk = Risk {
            level: RiskLevel::Medium,
            reasons: vec!["external ACP agent requested permission to execute a tool".to_string()],
        };
        let approval_id = match self
            .approvals
            .request_with_reuse(
                &self.pool,
                self.session_id,
                self.run_id,
                None,
                action.clone(),
                risk,
                Vec::new(),
                None,
                true,
            )
            .await
        {
            Ok(id) => id,
            Err(error) => {
                self.fail(error);
                return reject_option(&options);
            }
        };
        if let Err(error) = self.transition(RunState::WaitingForApproval).await {
            self.fail(error);
            self.approvals.forget_waiter(approval_id);
            return reject_option(&options);
        }
        if let Err(error) = self
            .persist(EventBody::ToolProposed {
                run_id: self.run_id,
                approval_id,
                action,
            })
            .await
        {
            self.fail(error);
            self.approvals.forget_waiter(approval_id);
            return reject_option(&options);
        }
        let decision = tokio::select! {
            result = self.approvals.await_decision(approval_id) => match result {
                Ok(decision) => decision,
                Err(error) => {
                    self.fail(error);
                    return reject_option(&options);
                }
            },
            _ = self.cancellation.cancelled() => {
                self.approvals.forget_waiter(approval_id);
                return reject_option(&options);
            }
        };
        if let Err(error) = self.transition(RunState::Running).await {
            self.fail(error);
            return reject_option(&options);
        }
        match decision {
            ApprovalDecision::Approve => allow_option(&options),
            ApprovalDecision::Reject | ApprovalDecision::Unknown => reject_option(&options),
            _ => reject_option(&options),
        }
    }
}

fn allow_option(options: &[PermissionOption]) -> Option<String> {
    options
        .iter()
        .find(|option| option.kind == "allow_once")
        .or_else(|| {
            options
                .iter()
                .find(|option| option.kind.starts_with("allow"))
        })
        .map(|option| option.option_id.clone())
}

fn reject_option(options: &[PermissionOption]) -> Option<String> {
    options
        .iter()
        .find(|option| option.kind == "reject_once")
        .or_else(|| {
            options
                .iter()
                .find(|option| option.kind.starts_with("reject"))
        })
        .map(|option| option.option_id.clone())
}

fn canonical_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(canonical_json).collect())
        }
        serde_json::Value::Object(values) => {
            let sorted = values
                .into_iter()
                .map(|(key, value)| (key, canonical_json(value)))
                .collect();
            serde_json::Value::Object(sorted)
        }
        other => other,
    }
}

fn bounded_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    const ELLIPSIS: &str = "…";
    if max_bytes < ELLIPSIS.len() {
        return ".".repeat(max_bytes);
    }
    let mut end = max_bytes - ELLIPSIS.len();
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}{}", &text[..end], ELLIPSIS)
}

fn last_nonempty_line(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| bounded_text(line.trim(), 2_048))
}

fn render_acp_prompt(prior: &[TurnItem], objective: &str) -> String {
    const CAP: usize = 1024 * 1024;
    let mut transcript = String::new();
    // The seeded context-manifest turn (2026-08-11 review item 1) always leads
    // the prior when present; render it as its own labeled block — it is
    // retrieved evidence, not part of the conversation — so an external ACP
    // agent receives the repo map / skill cards / memories exactly like the
    // native driver does. The manifest text carries its own "EVIDENCE, NOT
    // INSTRUCTIONS" preamble.
    let mut conversation = prior;
    if let Some((TurnItem::ToolResult { tool, output, .. }, rest)) = prior.split_first() {
        if tool == CONTEXT_PSEUDO_TOOL {
            transcript.push_str("Retrieved context:\n");
            transcript.push_str(output);
            transcript.push_str("\n\n");
            conversation = rest;
        }
    }
    if !conversation.is_empty() {
        transcript.push_str("Previous conversation:\n");
    }
    for item in conversation {
        let rendered = match item {
            TurnItem::Objective(text) => format!("User: {text}\n"),
            TurnItem::Assistant(text) => format!("Assistant: {text}\n"),
            TurnItem::ToolCall { tool, args } => format!("Tool call {tool}: {args}\n"),
            TurnItem::ToolResult { tool, output, .. } => {
                format!("Tool result {tool}: {output}\n")
            }
            TurnItem::Steering(text) => format!("User steering: {text}\n"),
        };
        transcript.push_str(&rendered);
    }
    transcript.push_str("\nCurrent request:\n");
    transcript.push_str(objective);
    if transcript.len() <= CAP {
        return transcript;
    }
    let suffix_start = transcript.len().saturating_sub(CAP);
    let mut start = suffix_start;
    while !transcript.is_char_boundary(start) {
        start += 1;
    }
    format!("[earlier context truncated]\n{}", &transcript[start..])
}

async fn bounded_acp_git_diff(worktree: &Path) -> Result<Vec<u8>, String> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;

    let mut child = tokio::process::Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["diff", "--no-ext-diff", "--binary", "HEAD"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not inspect ACP worktree: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "could not capture ACP worktree diff".to_string())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "could not capture ACP worktree diagnostics".to_string())?;
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).await.map(|_| bytes)
    });
    let mut diff = Vec::new();
    stdout
        .take((MAX_ACP_PATCH_BYTES + 1) as u64)
        .read_to_end(&mut diff)
        .await
        .map_err(|error| format!("could not read ACP worktree diff: {error}"))?;
    if diff.len() > MAX_ACP_PATCH_BYTES {
        let _ = child.kill().await;
        let _ = child.wait().await;
        let _ = stderr_task.await;
        return Err(format!(
            "ACP worktree diff exceeds the {} MiB review limit",
            MAX_ACP_PATCH_BYTES / (1024 * 1024)
        ));
    }
    let status = child
        .wait()
        .await
        .map_err(|error| format!("could not wait for ACP worktree diff: {error}"))?;
    let stderr = stderr_task
        .await
        .map_err(|error| format!("ACP worktree diagnostic task failed: {error}"))?
        .map_err(|error| format!("could not read ACP worktree diagnostics: {error}"))?;
    if !status.success() {
        return Err(format!(
            "could not inspect ACP worktree: {}",
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    Ok(diff)
}

async fn diff_counts(worktree: &Path) -> (u64, u64) {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["diff", "--numstat", "HEAD"])
        .output()
        .await;
    let Ok(output) = output else {
        return (0, 0);
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .fold((0u64, 0u64), |(added, removed), line| {
            let mut fields = line.split('\t');
            let next_added = fields
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            let next_removed = fields
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            (
                added.saturating_add(next_added),
                removed.saturating_add(next_removed),
            )
        })
}

impl RunExecutor for RuntimeExecutor {
    fn spawn_run(&self, launch: RunLaunch) {
        let executor = self.clone();
        let run_id = launch.run_id;
        let (handle, token) = cancellation();
        // ONE lock covers the live handles and both pending sets (see
        // `RunControlRegistry`), so this check-then-register step cannot
        // interleave with a concurrent cancel/pause/resume and cannot deadlock
        // against one either.
        lock_recovering(&executor.run_control).register(run_id, handle);
        tokio::spawn(async move {
            // Carry the identity out before `launch` is moved into the worker.
            let session_id = launch.session_id;
            let run_id = launch.run_id;
            let objective = launch.objective.clone();
            // `AgentMode` is `Copy` — carried out alongside the identity above so
            // `harvest_memories` can resolve the M3a/M3b fact extractor for this
            // run's mode (D2) after `launch` is moved into the worker below.
            let mode = launch.mode;
            // This run's OWN repository identity, derived from its repository root
            // (issue #6 item 1) — NOT the daemon's startup directory — so a shared
            // daemon attributes its context map and curated memories correctly.
            let repository = scan::repository_id_for(&launch.repository);

            // Register a cancellation handle BEFORE the loop starts, so a
            // `CancelRun` accepted at any point after this run was launched can
            // stop it. The token drives `execute`; the handle stays in the shared
            // registry for `cancel_run` to fire.
            // Warm this repository's code graph the first time the daemon serves a
            // run for it, so the context below opens with the right repository map.
            executor
                .ensure_scanned(repository, &launch.repository)
                .await;

            // Build the run's seed transcript ONCE (continuous-session plan,
            // Task 3 + 2026-08-11 review item 1): it reconstructs the
            // continuation prior, opens the trace (full manifest note for a
            // first run, carried-context marker for a follow-up), and seeds the
            // manifest into the MODEL-VISIBLE transcript — the wire the review
            // found missing. Done here, BEFORE the agent loop, so neither the
            // opening note nor the seed races the loop's sequences.
            let prior = executor
                .build_run_seed(session_id, run_id, repository, &objective)
                .await;

            // Run the work in a CHILD task so even a panic in the agent loop
            // becomes a clean terminal failure (a `JoinError`) rather than a
            // wedged, forever-`Queued`/`Running` run. The reconstructed `prior`
            // seeds the run's transcript (moved into the worker).
            let worker = executor.clone();
            let joined =
                tokio::spawn(async move { worker.execute(&launch, prior, token).await }).await;

            let failure = match joined {
                Ok(Ok(())) => None,              // the loop reached a terminal state itself
                Ok(Err(reason)) => Some(reason), // could not run (e.g. no model)
                Err(join) => Some(format!("run task aborted: {join}")), // panic / cancel
            };

            if let Some(reason) = failure {
                warn!(%run_id, reason = %reason, "run did not execute; failing it cleanly");
                // Retried: this is the last line of defense against a run being
                // left non-terminal (a headless `codypendent run` then hangs
                // forever), and a transient SQLITE_BUSY from a concurrently
                // streaming run must not defeat it.
                let mut attempt = 0u32;
                loop {
                    attempt += 1;
                    match recovery::fail_run(
                        &executor.pool,
                        &executor.artifacts(),
                        &executor.subscriptions,
                        run_id,
                        session_id,
                        &objective,
                        &reason,
                    )
                    .await
                    {
                        Ok(()) => break,
                        Err(e) if attempt < 4 => {
                            warn!(%run_id, error = %e, attempt, "failing the run did not stick; retrying");
                            tokio::time::sleep(std::time::Duration::from_millis(
                                100 * u64::from(attempt),
                            ))
                            .await;
                        }
                        Err(e) => {
                            error!(%run_id, error = %e, "could not fail run cleanly");
                            break;
                        }
                    }
                }
            }

            // The run has reached a terminal state; drop its cancellation handle
            // so the registry does not grow without bound (and a late `cancel_run`
            // for this run becomes a clean no-op).
            lock_recovering(&executor.run_control).forget(run_id);
            lock_recovering(&executor.steerings).remove(&run_id);

            // The run has now reached a terminal state (either the loop finished
            // it, or `fail_run` above did). Harvest any curated memories from its
            // event trace and note each durable one — emitted AFTER the loop, so
            // these appends never race it either.
            executor
                .harvest_memories(session_id, run_id, repository, mode)
                .await;
            executor
                .harvest_learnings(session_id, run_id, repository)
                .await;
        });
    }

    fn steer_run(&self, run_id: RunId, text: String) -> bool {
        if let Some(tx) = lock_recovering(&self.steerings).get(&run_id) {
            tx.send(text).is_ok()
        } else {
            false
        }
    }

    fn cancel_run(&self, run_id: RunId) {
        // Fire the run's cancellation token if it is still executing in this
        // process; a finished or unknown run simply is not in the registry, so
        // this is a clean no-op.
        let mut control = lock_recovering(&self.run_control);
        if let Some(handle) = control.live.get(&run_id) {
            handle.cancel();
            return;
        }
        control.pending_cancellations.insert(run_id);
    }

    fn pause_run(&self, run_id: RunId) {
        let mut control = lock_recovering(&self.run_control);
        if let Some(handle) = control.live.get(&run_id) {
            handle.pause();
            return;
        }
        control.pending_pauses.insert(run_id);
    }

    fn resume_run(&self, run_id: RunId) {
        // Clearing the pending pause and resuming the live handle are one
        // atomic step under the single run-control lock, so a resume racing the
        // registering `spawn_run` can no longer land between them.
        let mut control = lock_recovering(&self.run_control);
        control.pending_pauses.remove(&run_id);
        if let Some(handle) = control.live.get(&run_id) {
            handle.resume();
        }
    }

    fn collaborators(&self) -> Option<(SubscriptionHub, ApprovalBroker, QuestionBroker)> {
        Some((
            self.subscriptions.clone(),
            self.approvals.clone(),
            self.questions.clone(),
        ))
    }

    fn document_mutator(&self) -> Option<Arc<dyn codypendent_daemon::documents::DocumentMutator>> {
        // Apply `MutateDocument` over the knowledge document engine (mode-gated by
        // scope, single-writer via edit leases). Shares the daemon's pool.
        Some(Arc::new(crate::documents::KnowledgeDocumentMutator::new(
            self.pool.clone(),
        )))
    }

    fn document_leaser(&self) -> Option<Arc<dyn codypendent_daemon::documents::DocumentLeaser>> {
        // Acquire/release the block-range edit leases that gate `MutateDocument`,
        // over the same knowledge lease store the mutator's `require` enforces.
        Some(Arc::new(crate::documents::KnowledgeDocumentMutator::new(
            self.pool.clone(),
        )))
    }

    fn document_creator(&self) -> Option<Arc<dyn codypendent_daemon::documents::DocumentCreator>> {
        // Create a document from `CreateDocument`, importing any seed Markdown
        // into typed blocks. Falls back to this daemon's startup checkout when a
        // request names no repository, exactly as the publisher does.
        Some(Arc::new(crate::docs_job::KnowledgeDocumentCreator::new(
            self.pool.clone(),
            self.startup_repository_root.clone(),
        )))
    }

    fn document_maintainer(
        &self,
    ) -> Option<Arc<dyn codypendent_daemon::documents::DocumentMaintainer>> {
        // The `/update-docs` staleness sweep. Shares the daemon's fan-out so a
        // sweep asked to report into a session reaches attached clients live.
        Some(Arc::new(crate::docs_job::KnowledgeDocumentMaintainer::new(
            self.pool.clone(),
            self.startup_repository_root.clone(),
            self.subscriptions.clone(),
        )))
    }

    fn document_publisher(
        &self,
    ) -> Option<Arc<dyn codypendent_daemon::documents::DocumentPublisher>> {
        // Compute a `PublishDocument` plan, park its approval, and (once
        // approved) execute it against this daemon's repository root (Phase 4
        // STEP 4.4). Shares the same approval broker the server resolves
        // `ResolveApproval` against, and the same GitHub client (if any) the
        // `github.*` tools use, so the PR target's idempotent create/update
        // behaves identically to an agent-proposed `GitHubMutation`.
        let mut publisher = crate::publish::KnowledgePublisher::new(
            self.pool.clone(),
            self.approvals.clone(),
            self.repository_root.clone(),
            artifact_store(&self.paths),
        );
        if let Some(github) = &self.github {
            publisher = publisher.with_github(github.clone());
        }
        Some(Arc::new(publisher))
    }

    fn workflow_starter(&self) -> Option<Arc<dyn codypendent_daemon::workflows::WorkflowStarter>> {
        // Create a durable run from a `StartWorkflow` manifest and drive it to a
        // terminal state in the background (Phase 5 STEP 5.2). Shares the one host,
        // so its per-run drive locks match the lifecycle seam's.
        Some(Arc::new(self.workflow_host.clone()))
    }

    fn workflow_lifecycle(
        &self,
    ) -> Option<Arc<dyn codypendent_daemon::workflows::WorkflowLifecycle>> {
        // Pause/resume/retry an existing durable run over the same host (Phase 5
        // STEP 5.2).
        Some(Arc::new(self.workflow_host.clone()))
    }

    fn promotion_gateway(
        &self,
    ) -> Option<Arc<dyn codypendent_daemon::promotion::PromotionGateway>> {
        // Propose/advance/approve/roll back a promotion candidate (Phase 7 STEP
        // 7.5) over `codypendent-eval`'s durable store.
        Some(Arc::new(self.promotion.clone()))
    }

    fn memory_gateway(&self) -> Option<Arc<dyn codypendent_daemon::memory::MemoryGateway>> {
        // Inspect/correct/forget a curated memory, and open the evidence behind
        // one (outcome 17). Needs the artifact store as well as the pool: a
        // correction's own receipt is written there, and evidence may BE an
        // artifact.
        Some(Arc::new(crate::memory_ops::MemoryStoreGateway::new(
            self.pool.clone(),
            self.artifacts(),
        )))
    }

    fn code_graph_gateway(
        &self,
    ) -> Option<Arc<dyn codypendent_daemon::codegraph::CodeGraphGateway>> {
        // `codypendent graph {build,status,show}`. Shares this executor's
        // `scanned` and `watchers` registries by `Arc`, not a copy: an on-demand
        // build must count as THE fold for the checkout's current revision (so
        // the next run reuses it instead of re-scanning) and must arm the same
        // single live watcher a session-opened fold arms.
        Some(Arc::new(crate::codegraph_ops::CodeGraphOps::new(
            self.pool.clone(),
            Arc::clone(&self.scanned),
            Arc::clone(&self.watchers),
        )))
    }

    fn blackboard_reader(&self) -> Option<Arc<dyn BlackboardReader>> {
        // Read a durable run's board for a `ReadBlackboard` command over the
        // workflow `BlackboardStore` on the shared pool (Phase 5 STEP 5.3).
        Some(Arc::new(WorkflowBlackboardReader::new(self.pool.clone())))
    }

    fn blackboard_writer(&self) -> Option<Arc<dyn BlackboardWriter>> {
        // Apply a Controller's `PostBlackboardItem` / `UpdateBlackboardItem` over
        // the SAME store and the SAME fan-out hub an agent's board write uses
        // (Phase B kanban), so a human's move and an agent's `task.move` produce
        // identical rows and identical live deliveries.
        Some(Arc::new(AssemblyBoardWriter::new(
            self.pool.clone(),
            self.blackboards.clone(),
        )))
    }

    fn blackboard_hub(&self) -> Option<BlackboardHub> {
        // The server reuses THIS hub (rather than a fresh one) so an agent's posts,
        // published deep inside the workflow executor, reach the server's
        // `Subscription::Blackboard` forwarders (Phase 5 STEP 5.3).
        Some(self.blackboards.clone())
    }

    fn workflow_reader(&self) -> Option<Arc<dyn WorkflowReader>> {
        // Read a durable run's observability snapshot for a `ReadWorkflowRun` command
        // over the workflow store on the shared pool (Phase 5 STEP 5.2 / T9).
        Some(Arc::new(WorkflowRunReader::new(self.pool.clone())))
    }

    fn workflow_hub(&self) -> Option<WorkflowHub> {
        // The server reuses THIS hub (rather than a fresh one) so node transitions,
        // published by the workflow host's observer deep inside the executor, reach the
        // server's `Subscription::Workflow` forwarders (Phase 5 STEP 5.2 / T9).
        Some(self.workflows.clone())
    }

    fn ensure_repository_scanned(&self, root: PathBuf) {
        // Fire-and-forget, exactly like `spawn_run`: the server must never await
        // this. Reuses `Self::ensure_scanned` — the SAME guarded warm-up
        // `spawn_run` calls before a run's context opens — so a repository
        // opened here and later run against is scanned at most once either way,
        // and a repository already warmed by a run is not re-scanned on open.
        let executor = self.clone();
        tokio::spawn(async move {
            let repository = scan::repository_id_for(&root);
            executor.ensure_scanned(repository, &root).await;
        });
    }

    fn transcriber(&self) -> Option<Arc<dyn codypendent_daemon::transcription::Transcriber>> {
        // Voice v1 (rubric 8): `None` unless `models.toml` declares a
        // `[transcription]` endpoint, which leaves audio submissions rejected
        // `voice.transport-unavailable` rather than silently unhandled.
        self.transcriber.clone()
    }
}

/// Load a model registry + a Phase-1 policy from `<data_dir>/models.toml`, or an
/// error string when none is configured. Shared by [`RuntimeExecutor::execute`]
/// and the workflow agent-node executor so both resolve models identically.
pub(crate) fn load_model_registry(
    paths: &RuntimePaths,
) -> Result<(ModelRegistry, ModelPolicy), String> {
    let path = paths.data_dir.join("models.toml");
    if !path.exists() {
        return Err("no model configured (no models.toml)".to_string());
    }
    let configs = load_models(&path).map_err(|e| format!("invalid models.toml: {e}"))?;
    if configs.is_empty() {
        return Err("no model configured (models.toml is empty)".to_string());
    }
    let ids: Vec<_> = configs.iter().map(|c| c.id.clone()).collect();
    // Additive: also load `<data_dir>/auth.json` so a TUI-added model's stored key
    // resolves at client build (precedence: auth.json → api_key_env → none). An
    // absent file yields an empty store (`AuthStore::load`'s `Ok(default)` path),
    // leaving every model resolving as before; a present-but-corrupt file is a
    // real failure and is propagated here exactly like an invalid `models.toml`
    // above, rather than silently masked as "no keys saved".
    let auth = codypendent_runtime::auth::AuthStore::load(&paths.data_dir)
        .map_err(|e| format!("invalid auth.json: {e}"))?;
    let registry = ModelRegistry::new(configs).with_auth(auth);
    // Phase-1 policy: every mode tries every configured model, in file order,
    // until one connects. (The Phase-7 utility router replaces this.)
    let policy = ModelPolicy::new().with_default_candidates(ids);
    Ok((registry, policy))
}

/// The pool-erased [`RunJournal`]: a persist closure (ledger append, with the run
/// projection updated in step for a `RunStateChanged`) and an approval-request
/// closure driving the *shared* broker so the runtime's `await_decision` observes
/// a client's resolution. Shared by [`RuntimeExecutor`] and the workflow agent-node
/// executor so both persist run events the same way.
pub(crate) fn run_journal(pool: &SqlitePool, approvals: &ApprovalBroker) -> RunJournal {
    let persist_pool = pool.clone();
    let approve_pool = pool.clone();
    let state_pool = pool.clone();
    let terminal_pool = pool.clone();
    let approve_broker = approvals.clone();
    RunJournal::new(
        move |session: SessionId, actor: Actor, body: EventBody| {
            let pool = persist_pool.clone();
            async move {
                match body {
                    EventBody::RunStateChanged { run_id, state } => {
                        ledger::append_run_state_changed(
                            &pool,
                            session,
                            &actor,
                            run_id,
                            state,
                            Utc::now(),
                        )
                        .await
                    }
                    body @ EventBody::RunUsage { .. } => {
                        ledger::append_run_usage(&pool, session, &actor, &body, Utc::now()).await
                    }
                    body => {
                        ledger::append_next_event(&pool, session, &actor, &body, Utc::now()).await
                    }
                }
            }
        },
        move |req: ApprovalRequest| {
            let pool = approve_pool.clone();
            let broker = approve_broker.clone();
            async move {
                let id = broker
                    .request_with_reuse(
                        &pool,
                        req.session_id,
                        req.run_id,
                        req.repository.as_deref(),
                        req.action,
                        req.risk,
                        req.capabilities,
                        None,
                        req.allow_run_reuse,
                    )
                    .await?;
                Ok(id)
            }
        },
    )
    .with_terminal_persist(move |session, actor, state, body| {
        let pool = terminal_pool.clone();
        async move {
            ledger::append_run_terminal(&pool, session, &actor, state, &body, Utc::now()).await
        }
    })
    .with_state_reader(move |run_id| {
        let pool = state_pool.clone();
        async move { projections::load_run_state(&pool, run_id).await }
    })
}

/// The content-addressed [`ArtifactStore`] rooted at `<data_dir>/artifacts`.
pub(crate) fn artifact_store(paths: &RuntimePaths) -> ArtifactStore {
    ArtifactStore::new(paths.data_dir.join("artifacts"))
}

/// The pool-erased [`ArtifactSink`] over the store + pool. Shared by
/// [`RuntimeExecutor`] and the workflow agent-node executor.
pub(crate) fn artifact_sink(pool: &SqlitePool, store: ArtifactStore) -> Box<dyn ArtifactSink> {
    let pool = pool.clone();
    Box::new(ClosureSink(
        move |media: String, prov: Provenance, bytes: Vec<u8>| {
            let store = store.clone();
            let pool = pool.clone();
            async move {
                store
                    .put(&pool, &media, DataClassification::Internal, prov, &bytes)
                    .await
            }
        },
    ))
}

pub(crate) struct PoolQuestionChannel {
    pub(crate) pool: SqlitePool,
    pub(crate) broker: QuestionBroker,
}

#[async_trait]
impl QuestionChannel for PoolQuestionChannel {
    async fn ask(
        &self,
        session_id: SessionId,
        run_id: RunId,
        questions: Vec<codypendent_protocol::QuestionPrompt>,
    ) -> anyhow::Result<QuestionReply> {
        let q_id = self
            .broker
            .ask(&self.pool, session_id, run_id, questions)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let reply = self
            .broker
            .await_reply(q_id)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(reply)
    }
}

pub(crate) struct PoolPlanBridge {
    pub(crate) pool: SqlitePool,
    pub(crate) subscriptions: SubscriptionHub,
    /// The `(run, target mode)` transitions this bridge has already enqueued —
    /// the [`PlanBridge`] idempotency contract ("a duplicate call for the same
    /// transition enqueues once"). A bridge is built per run drive, so this is a
    /// tiny per-run set, not a process-wide cache.
    ///
    /// [`PlanBridge`]: codypendent_runtime::agent::PlanBridge
    enqueued: std::sync::Mutex<Vec<(RunId, AgentMode)>>,
}

impl PoolPlanBridge {
    pub(crate) fn new(pool: SqlitePool, subscriptions: SubscriptionHub) -> Self {
        Self {
            pool,
            subscriptions,
            enqueued: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Claim `(run_id, target)` for this bridge, returning `false` when the
    /// transition was already enqueued. The claim is taken BEFORE the work so two
    /// concurrent duplicates cannot both proceed, and released again by
    /// [`Self::release`] if the work fails (a failed enqueue must stay retryable).
    fn claim(&self, run_id: RunId, target: AgentMode) -> bool {
        let mut seen = match self.enqueued.lock() {
            Ok(seen) => seen,
            Err(poisoned) => poisoned.into_inner(),
        };
        if seen.contains(&(run_id, target)) {
            return false;
        }
        seen.push((run_id, target));
        true
    }

    fn release(&self, run_id: RunId, target: AgentMode) {
        let mut seen = match self.enqueued.lock() {
            Ok(seen) => seen,
            Err(poisoned) => poisoned.into_inner(),
        };
        seen.retain(|entry| *entry != (run_id, target));
    }

    /// The enqueue + `PendingPromptsChanged` half of [`Self::switch_mode`], split
    /// out so the caller can un-claim the transition on failure.
    async fn enqueue_transition(
        &self,
        session_id: SessionId,
        target: AgentMode,
        text: String,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let views = codypendent_daemon::prompt_queue::enqueue(
            &mut tx,
            session_id,
            &text,
            target,
            codypendent_protocol::PromptDelivery::Queue,
        )
        .await?;
        tx.commit().await?;

        // Sequence allocation and insert MUST be one statement: `events` is keyed
        // `(session_id, sequence)` and the live run appends concurrently through
        // `append_next_event`. A read-then-insert races it, and a losing insert
        // would leave the prompt committed above with no `PendingPromptsChanged`
        // published — a continuation nobody is told about.
        let event = codypendent_daemon::ledger::append_next_event(
            &self.pool,
            session_id,
            &Actor::System,
            &EventBody::PendingPromptsChanged { prompts: views },
            chrono::Utc::now(),
        )
        .await?;
        self.subscriptions.publish(session_id, event);
        Ok(())
    }
}

#[async_trait]
impl codypendent_runtime::agent::PlanBridge for PoolPlanBridge {
    async fn switch_mode(
        &self,
        session_id: SessionId,
        run_id: RunId,
        target: AgentMode,
        text: String,
    ) -> anyhow::Result<()> {
        // Idempotent on `(run_id, target)`: a model that calls `plan_exit` twice,
        // or a retried tool call, must leave ONE continuation turn on the queue.
        if !self.claim(run_id, target) {
            return Ok(());
        }
        match self.enqueue_transition(session_id, target, text).await {
            Ok(()) => Ok(()),
            Err(error) => {
                self.release(run_id, target);
                Err(error)
            }
        }
    }
}

pub(crate) struct PoolTurnCheckpointer {
    pub(crate) pool: SqlitePool,
    pub(crate) subscriptions: SubscriptionHub,
    pub(crate) session_id: SessionId,
    pub(crate) repository: PathBuf,
    pub(crate) worktree: PathBuf,
    pub(crate) run_id: RunId,
}

#[async_trait]
impl codypendent_runtime::agent::TurnCheckpointer for PoolTurnCheckpointer {
    async fn checkpoint_turn(&self, ordinal: u32) {
        if let Err(e) = codypendent_daemon::checkpoints::record_checkpoint(
            &self.pool,
            &self.subscriptions,
            self.session_id,
            &self.repository,
            &self.worktree,
            self.run_id,
            ordinal,
        )
        .await
        {
            warn!(run_id = %self.run_id, ordinal, error = %e, "could not record turn checkpoint");
        }
    }
}

/// A run's bound worktree: the path its agent loop operates in, plus the lease
/// to release once the run is terminal. `lease` is `None` for a read-only run
/// that keeps the repository root (nothing was allocated, so nothing to release).
pub(crate) struct WorktreeBinding {
    /// The worktree root the run's `$WORKTREE` scope resolves to.
    pub worktree: PathBuf,
    /// The workspace lease to release on teardown, if a worktree was allocated.
    pub lease: Option<uuid::Uuid>,
}

/// Whether a run in `mode` may write to its worktree — the single source of the
/// "does this run need an isolated worktree" decision. Keyed on the policy
/// [`mode_overlay`], so it tracks the mode→write-capability mapping the runtime
/// enforces (only `Build` writes the worktree today; Explore/Ask/Plan/Review are
/// read-only). A read-only run keeps the repository root; a writer is isolated so
/// two concurrent writers never share a tree (Phase 5 exit criterion 1).
pub(crate) fn run_writes_to_worktree(mode: AgentMode) -> bool {
    mode_overlay(mode).write_allowed
}

/// Convert a launch's dependency-safe prior-transcript carrier
/// (`codypendent_daemon::executor::PriorTurn`) into the runtime's own
/// [`TurnItem`], 1:1 per variant (Task 2, continuous-session plan). This
/// assembly crate is the only place that can name both types —
/// `codypendent-daemon` must never depend on `codypendent-runtime` (see
/// `PriorTurn`'s doc comment) — so the mapping lives here as a plain
/// function rather than a `From` impl on either side (the orphan rule blocks
/// implementing a foreign trait between two foreign types from a third
/// crate).
fn convert_launch_prior(prior: &[PriorTurn]) -> Vec<TurnItem> {
    prior
        .iter()
        .map(|item| match item {
            PriorTurn::Objective(text) => TurnItem::Objective(text.clone()),
            PriorTurn::Assistant(text) => TurnItem::Assistant(text.clone()),
            PriorTurn::ToolResult { tool, output } => TurnItem::ToolResult {
                tool: tool.clone(),
                output: output.clone(),
                // `PriorTurn` (the daemon-crate-local, never-persisted
                // carrier) has no artifact field to map from — every
                // construction site of it leaves this empty today (see
                // `with_prior` above), so there is nothing to thread through
                // here (continuation-content plan, Task 2).
                artifact: None,
            },
            PriorTurn::Steering(text) => TurnItem::Steering(text.clone()),
        })
        .collect()
}

/// Bind a worktree for a run. When `isolate` is set, allocate a dedicated,
/// isolated worktree through the [`WorktreeManager`] (recording the lease on the
/// run's projection for provenance) and return its path; otherwise the run keeps
/// the repository root read-only and no lease is taken. An allocation failure is
/// returned as a human reason the caller fails the run with — never a silent
/// fall-through to a shared writable tree.
///
/// `artifacts` must be the CANONICAL store ([`artifact_store`], rooted at
/// `<data_dir>/artifacts`): the failed-stash-reapply path force-releases the
/// freshly allocated worktree, and the manager exports the discarded work as a
/// safety patch into this store while recording an `ArtifactRef` row in the DB.
/// A different root would make that row dangling and the only copy of the user's
/// work unreachable through every reader.
pub(crate) async fn bind_run_worktree(
    pool: &SqlitePool,
    artifacts: &ArtifactStore,
    manager: &WorktreeManager,
    run_id: RunId,
    isolate: bool,
    repository: &Path,
) -> Result<WorktreeBinding, String> {
    if !isolate {
        return Ok(WorktreeBinding {
            worktree: repository.to_path_buf(),
            lease: None,
        });
    }

    // Query session fork metadata to see if this run belongs to a forked session
    let fork_info: Option<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT s.fork_base_commit, s.fork_checkpoint_sha, s.fork_checkpoint_kind \
         FROM runs r JOIN sessions s ON r.session_id = s.id WHERE r.id = ?",
    )
    .bind(run_id.to_string())
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("could not query session fork info: {e}"))?;

    let (base_commit, checkpoint_sha, checkpoint_kind) = match fork_info {
        Some((b, s, k)) => (b, s, k),
        None => (None, None, None),
    };

    let alloc_res = manager
        .allocate_at(pool, repository, run_id, base_commit.as_deref())
        .await;

    match alloc_res {
        Ok(lease) => {
            // If the fork origin was a stash checkpoint, reapply the stash onto the fresh worktree
            if checkpoint_kind.as_deref() == Some("stash") {
                if let Some(sha) = checkpoint_sha.as_deref() {
                    if let Err(error) =
                        codypendent_daemon::worktrees::apply_stash(&lease.worktree_path, sha).await
                    {
                        let _ = manager.release(pool, artifacts, lease.id, true).await;
                        return Err(format!("could not reapply fork checkpoint stash: {error}"));
                    }
                }
            }

            // Record run→lease provenance on the reserved projection column, so a
            // run's real worktree is recoverable from its `runs` row alone.
            if let Err(error) = projections::set_run_workspace_lease(pool, run_id, lease.id).await {
                warn!(%run_id, %error, "could not record the run's workspace lease");
            }
            Ok(WorktreeBinding {
                worktree: lease.worktree_path,
                lease: Some(lease.id),
            })
        }
        Err(error) => {
            let message = match &error {
                WorktreeError::NotAGitRepository { .. } => error.to_string(),
                _ => format!("could not allocate an isolated worktree: {error}"),
            };
            Err(message)
        }
    }
}

/// Release a run's bound worktree, protecting any unmerged work (the manager
/// exports a patch and retains the directory when the branch holds commits or the
/// tree is dirty — `force: false`). A no-op when the run bound no worktree. A
/// release failure is logged, never fatal: the run has already reached its
/// terminal state, and a stale lease is swept by startup reconciliation.
pub(crate) async fn release_run_worktree(
    pool: &SqlitePool,
    artifacts: &ArtifactStore,
    manager: &WorktreeManager,
    binding: &WorktreeBinding,
) {
    if let Some(lease_id) = binding.lease {
        match manager.release(pool, artifacts, lease_id, false).await {
            Ok(outcome) => announce_preserved_worktree(pool, &outcome).await,
            Err(error) => warn!(%lease_id, %error, "could not release the run's worktree"),
        }
    }
}

/// Tell the user when a released worktree was **retained** because it held work.
///
/// The protective release path exports a safety patch and keeps the directory
/// and its `codypendent/run-*` branch precisely so a failed worker's work is not
/// lost — and, until this existed, said so to nobody: the `ReleaseOutcome` that
/// records it was computed in the daemon and dropped one frame below the only
/// observer. A fan-out whose workers failed left orphan worktrees and branches in
/// the user's repository with no explanation and no pointer to the patch. The
/// run's own session ledger is where the user is already looking, so the note
/// lands there, naming the path, the branch and the artifact.
///
/// Best effort throughout: a run that has already reached a terminal state must
/// never fail because its epilogue could not be written.
async fn announce_preserved_worktree(pool: &SqlitePool, outcome: &ReleaseOutcome) {
    if !outcome.preserved {
        return;
    }
    let session_id = match projections::run_session(pool, outcome.owner_run_id).await {
        Ok(Some(session_id)) => session_id,
        Ok(None) => return,
        Err(error) => {
            warn!(%error, "could not attribute a preserved worktree to its session");
            return;
        }
    };
    let reason = match (outcome.unmerged_commits, outcome.dirty) {
        (0, _) => "uncommitted changes".to_string(),
        (1, false) => "1 unmerged commit".to_string(),
        (commits, false) => format!("{commits} unmerged commits"),
        (1, true) => "1 unmerged commit and uncommitted changes".to_string(),
        (commits, true) => format!("{commits} unmerged commits and uncommitted changes"),
    };
    let patch = outcome.patch.as_ref().map_or_else(
        || " No patch could be exported, so the worktree is the only copy.".to_string(),
        |patch| format!(" Its diff is saved as artifact {}.", patch.id),
    );
    let text = format!(
        "Kept the worktree {} and its branch `{}`: it held {reason}, so nothing was deleted.{patch} \
         Recover or discard it with `git -C {} worktree remove {}` and `git branch -D {}`.",
        outcome.worktree_path.display(),
        outcome.branch,
        outcome.worktree_path.display(),
        outcome.worktree_path.display(),
        outcome.branch,
    );
    if let Err(error) = ledger::append_next_event(
        pool,
        session_id,
        &Actor::System,
        &EventBody::NoteAppended {
            text,
            run_id: Some(outcome.owner_run_id),
        },
        chrono::Utc::now(),
    )
    .await
    {
        warn!(%error, "could not note a preserved worktree on the session");
    }
}

/// Release a run's worktree whose contents are ALREADY captured durably
/// elsewhere — a workflow agent node whose diff became a content-addressed
/// `proposed_patch` artifact before this call.
///
/// `force` is safe here for exactly that reason, and only that reason. The
/// protective (`force: false`) path retains the directory whenever the tree is
/// dirty or the branch holds unmerged commits, which is *always* true of an
/// implementer node — it edits files by design — so a fan-out of eight workers
/// left eight retained trees and eight `codypendent/run-*` refs per run, each a
/// second copy of bytes that were already durable and already reachable
/// (F15.5).
///
/// The manager still exports a patch spanning `base_commit -> working tree`
/// before removing anything, and still refuses to remove when that export comes
/// back empty, so this cannot destroy work the node's own capture missed
/// (staged-but-uncommitted edits, or commits, which `git diff` alone does not
/// carry).
pub(crate) async fn release_captured_run_worktree(
    pool: &SqlitePool,
    artifacts: &ArtifactStore,
    manager: &WorktreeManager,
    binding: &WorktreeBinding,
) {
    if let Some(lease_id) = binding.lease {
        match manager.release(pool, artifacts, lease_id, true).await {
            // `force` still preserves the tree when the safety export came back
            // empty (the manager refuses to remove what it could not capture) —
            // that is exactly the case a user must hear about, so this path
            // reports too rather than assuming `force` always removes.
            Ok(outcome) => announce_preserved_worktree(pool, &outcome).await,
            Err(error) => warn!(%lease_id, %error, "could not release the run's captured worktree"),
        }
    }
}

/// Releases a run's bound worktree **even if the drive panics**. A plain
/// post-await `release_run_worktree` is skipped when the agent loop unwinds,
/// leaking the lease + worktree for the process lifetime — startup reconciliation
/// cannot reclaim a directory that still exists. This guard closes that gap: the
/// normal path calls [`release`](Self::release) (awaited, so a caller/test observes
/// the released state synchronously); an unwind drops the guard while still armed,
/// which schedules the async release on the current runtime — `Drop` cannot itself
/// `await`, so a detached, best-effort task does the teardown while the runtime is
/// alive. `force = false` semantics are unchanged (unmerged work is still
/// preserved as a patch).
pub(crate) struct WorktreeReleaseGuard {
    pool: SqlitePool,
    artifacts: ArtifactStore,
    manager: WorktreeManager,
    unified_exec: Arc<codypendent_daemon::unified_exec::UnifiedExecManager>,
    /// `Some` while armed; taken by a normal `release` or by `Drop` on unwind.
    binding: Option<WorktreeBinding>,
}

impl WorktreeReleaseGuard {
    /// Arm a guard over `binding`. Until [`release`](Self::release) runs, an
    /// unwind schedules the release.
    pub(crate) fn arm(
        pool: SqlitePool,
        artifacts: ArtifactStore,
        manager: WorktreeManager,
        unified_exec: Arc<codypendent_daemon::unified_exec::UnifiedExecManager>,
        binding: WorktreeBinding,
    ) -> Self {
        Self {
            pool,
            artifacts,
            manager,
            unified_exec,
            binding: Some(binding),
        }
    }

    /// Normal teardown: release the worktree, awaiting completion, then disarm (so
    /// `Drop` is a no-op). Consumes the guard.
    pub(crate) async fn release(mut self) {
        if let Some(binding) = self.binding.take() {
            self.unified_exec.terminate_under(&binding.worktree).await;
            release_run_worktree(&self.pool, &self.artifacts, &self.manager, &binding).await;
        }
    }

    /// Teardown for a worktree whose contents are already durable elsewhere —
    /// see [`release_captured_run_worktree`]. The caller must have proof of that
    /// capture in hand (a `Some` patch artifact), never merely an expectation of
    /// one. Consumes the guard, so an unwind before this point still takes the
    /// protective path.
    pub(crate) async fn release_captured(mut self) {
        if let Some(binding) = self.binding.take() {
            self.unified_exec.terminate_under(&binding.worktree).await;
            release_captured_run_worktree(&self.pool, &self.artifacts, &self.manager, &binding)
                .await;
        }
    }
}

impl Drop for WorktreeReleaseGuard {
    fn drop(&mut self) {
        // Fires only on the unwind path (a normal `release` already took the
        // binding). `Drop` cannot await, so schedule the async release on the
        // current runtime as a detached, best-effort task — enough not to leak on a
        // panic while the runtime is still alive. A run-with-no-worktree binding
        // needs no task at all.
        if let Some(binding) = self.binding.take() {
            if binding.lease.is_some() {
                let pool = self.pool.clone();
                let artifacts = self.artifacts.clone();
                let manager = self.manager.clone();
                let unified_exec = self.unified_exec.clone();
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        unified_exec.terminate_under(&binding.worktree).await;
                        release_run_worktree(&pool, &artifacts, &manager, &binding).await;
                    });
                } else {
                    tracing::warn!(
                        "WorktreeReleaseGuard dropped outside of active Tokio runtime; lease cleanup deferred to startup reconciliation"
                    );
                }
            }
        }
    }
}

/// The per-run overrides recorded on the `StartRun` command that created a
/// queued run — its `repository` and pinned `model`, if any. The commands table
/// stores the applied outcome (`result_json`, with `created_run`) beside the
/// body, so the originating command is found by the run id it created.
/// Recovering both in one read lets a crash-relaunched run keep its repository
/// identity (issue #6 item 1) AND its operator-pinned model (STEP MP2), instead
/// of silently resolving/routing a different model on restart.
async fn queued_run_overrides(
    pool: &sqlx::SqlitePool,
    run_id: &str,
) -> (Option<std::path::PathBuf>, Option<ModelId>) {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT body FROM commands \
         WHERE status = 'applied' AND json_extract(result_json, '$.created_run') = ?",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let Some((body_json,)) = row else {
        return (None, None);
    };
    match serde_json::from_str::<codypendent_protocol::CommandBody>(&body_json) {
        Ok(codypendent_protocol::CommandBody::StartRun {
            repository, model, ..
        }) => (repository.map(std::path::PathBuf::from), model),
        // A continuation launched by a `SubmitUserInput` records its OWN
        // mid-conversation model pin on the command body — recover it so a
        // crash-relaunched re-pinned run resolves that model, not an unpinned
        // default. It carries no repository (that stays the session's).
        Ok(codypendent_protocol::CommandBody::SubmitUserInput { model, .. }) => (None, model),
        _ => (None, None),
    }
}

/// Resolve a checkout's GitHub `owner/repo` from its `origin` remote, or `None`
/// if the checkout has no GitHub origin (the `github.*` tools then stay inert).
pub(crate) async fn resolve_github_repo(repository: &Path) -> Option<RepoId> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["remote", "get-url", "origin"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout);
    parse_github_slug(url.trim())
}

/// Parse an `owner/repo` [`RepoId`] from a GitHub remote URL, accepting both the
/// HTTPS (`https://github.com/owner/repo.git`) and scp-like SSH
/// (`git@github.com:owner/repo.git`) forms. The host is matched **exactly**
/// against `github.com` (never by substring), so `mygithub.com` or
/// `github.com.evil.example` is rejected, and any embedded userinfo (a token in
/// the URL) is discarded, not propagated.
fn parse_github_slug(url: &str) -> Option<RepoId> {
    // Drop the scheme (`https://`, `ssh://`) and any `user[:pass]@` userinfo.
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let rest = rest.rsplit_once('@').map_or(rest, |(_, rest)| rest);
    // The host runs up to the first delimiter: `/` in the URL form, `:` in the
    // scp-like form. Everything after it is the path.
    let boundary = rest.find(['/', ':'])?;
    let host = &rest[..boundary];
    if host != "github.com" {
        return None;
    }
    let mut path = rest[boundary + 1..].trim_start_matches('/');
    // A URL-form remote may carry an explicit port (`github.com:443/owner/repo`);
    // the `:` boundary would otherwise hand the port digits to the owner slot.
    if rest.as_bytes()[boundary] == b':' {
        if let Some((maybe_port, remainder)) = path.split_once('/') {
            if !maybe_port.is_empty() && maybe_port.bytes().all(|b| b.is_ascii_digit()) {
                path = remainder;
            }
        }
    }
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/').filter(|segment| !segment.is_empty());
    let owner = parts.next()?;
    let repo = parts.next()?;
    Some(RepoId::new(owner.to_string(), repo.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether the session's ledger carries the full `=== CONTEXT` manifest
    /// (the first-run repo-map note), used to tell a first run's opening from a
    /// continuation's carried-context marker.
    async fn context_manifest_present(pool: &SqlitePool, session: SessionId) -> bool {
        ledger::load_events(pool, session)
            .await
            .expect("load events")
            .iter()
            .any(|event| {
                matches!(
                    &event.body,
                    EventBody::NoteAppended { text, .. } if text.starts_with("=== CONTEXT")
                )
            })
    }

    /// PR #68 review, "lock-order inversion": run control lived in THREE
    /// mutexes. `spawn_run` took the live-handle map and then, still holding it,
    /// the pending sets; a sibling path taking a pending set first and the live
    /// map second would deadlock the executor outright and wedge every later
    /// run-control command, because cancel/pause/resume all funnel through the
    /// same maps. They are now one mutex, so there is no order to invert.
    ///
    /// This drives every run-control entry point concurrently — the registering
    /// side of `spawn_run` plus `cancel_run`/`pause_run`/`resume_run` — from
    /// several threads at once, under a bounded `tokio::time::timeout`.
    /// Reintroduce nested run-control locks in opposite orders and this fails
    /// loudly by timeout instead of hanging CI forever.
    /// Poison the run-control registry the only way it can be poisoned — a
    /// panic while holding it — and prove cancellation still lands, both for a
    /// live run and for one that has not registered yet. With
    /// `.expect("run control registry lock")` back in place, every one of these
    /// calls panics and NO run can be cancelled for the daemon's lifetime.
    #[tokio::test]
    async fn run_control_still_cancels_after_the_registry_is_poisoned() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");
        let repository = scan::repository_id_for(dir.path());
        let executor = RuntimeExecutor::new(pool, paths, repository, dir.path().to_path_buf());

        let live = RunId::new();
        let (handle, token) = cancellation();
        lock_recovering(&executor.run_control).register(live, handle);

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = executor.run_control.lock().expect("fresh mutex");
            panic!("a holder panicked");
        }));
        assert!(executor.run_control.is_poisoned());

        // A run already executing still stops.
        executor.cancel_run(live);
        assert!(token.is_cancelled(), "the live run was told to stop");

        // A cancel that beats its run to the executor is still remembered, so
        // `register` consumes it instead of starting an uncancellable run.
        let early = RunId::new();
        executor.cancel_run(early);
        let (early_handle, early_token) = cancellation();
        lock_recovering(&executor.run_control).register(early, early_handle);
        assert!(
            early_token.is_cancelled(),
            "the pending cancel was consumed"
        );

        // Steering a run nobody registered is still a clean `false`, not a panic.
        assert!(!executor.steer_run(RunId::new(), "hello".to_string()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn run_control_survives_concurrent_start_stop_traffic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");
        let repository = scan::repository_id_for(dir.path());
        let executor = RuntimeExecutor::new(pool, paths, repository, dir.path().to_path_buf());

        // The same run ids on every worker, so start/resume/cancel/pause really
        // do collide on the same registry entries rather than sharding apart.
        let run_ids: Vec<RunId> = (0..64).map(|_| RunId::new()).collect();

        let exercise = async {
            let mut workers = Vec::new();
            for worker in 0..4 {
                let executor = executor.clone();
                let run_ids = run_ids.clone();
                workers.push(tokio::task::spawn_blocking(move || {
                    for _ in 0..50 {
                        for &run_id in &run_ids {
                            match worker {
                                // The registering half of `spawn_run`, without
                                // launching an actual agent loop.
                                0 => {
                                    let (handle, _token) = cancellation();
                                    executor
                                        .run_control
                                        .lock()
                                        .expect("run control registry lock")
                                        .register(run_id, handle);
                                }
                                1 => executor.resume_run(run_id),
                                2 => executor.cancel_run(run_id),
                                3 => executor.pause_run(run_id),
                                _ => unreachable!(),
                            }
                        }
                    }
                }));
            }
            for worker in workers {
                worker.await.expect("run-control worker");
            }
        };

        tokio::time::timeout(std::time::Duration::from_secs(30), exercise)
            .await
            .expect("run control deadlocked: the registry maps must stay under ONE lock");

        // The terminal-state cleanup still empties every map for every run.
        {
            let mut control = executor
                .run_control
                .lock()
                .expect("run control registry lock");
            for &run_id in &run_ids {
                control.forget(run_id);
            }
            assert!(control.live.is_empty(), "live handles leaked");
            assert!(
                control.pending_cancellations.is_empty(),
                "pending cancellations leaked"
            );
            assert!(control.pending_pauses.is_empty(), "pending pauses leaked");
        }
    }

    /// A control command that arrives before the run reaches the executor is
    /// consumed by the registration, not lost: the pending set is drained and
    /// the handle installed under one lock, so nothing can slip between them.
    #[test]
    fn a_pending_cancel_is_consumed_when_the_run_registers() {
        let mut control = RunControlRegistry::default();
        let run_id = RunId::new();

        control.pending_cancellations.insert(run_id);
        let (handle, token) = cancellation();
        control.register(run_id, handle);

        assert!(token.is_cancelled(), "the pending cancel must have fired");
        assert!(
            control.pending_cancellations.is_empty(),
            "the pending entry is consumed, never left to fire against a later run"
        );
        assert!(control.live.contains_key(&run_id));

        control.forget(run_id);
        assert!(control.live.is_empty());
    }

    /// 2026-08-11 review, "graph staleness": `ensure_scanned` gated on a bare
    /// per-process "seen" flag, so a daemon that outlived a branch switch or a
    /// pull kept serving the repository map from its FIRST run forever. The gate
    /// is now the checkout's revision: a moved `HEAD` re-folds the graph, and a
    /// run at an already-folded revision still does not re-scan.
    #[tokio::test]
    async fn a_moved_head_re_scans_the_code_graph_and_an_unchanged_one_does_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");

        let parent = tempfile::tempdir().expect("repo parent");
        let repo = init_git_repo(parent.path());
        std::fs::write(repo.join("alpha.rs"), "pub struct Alpha;\n").expect("write alpha");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "alpha"]);

        let repository = scan::repository_id_for(&repo);
        let executor = RuntimeExecutor::new(pool.clone(), paths, repository, repo.clone());

        executor.ensure_scanned(repository, &repo).await;
        let first_revision = scan::head_revision(&repo);
        assert_eq!(
            executor
                .scanned
                .lock()
                .expect("scanned map lock")
                .get(&repository),
            Some(&first_revision),
            "the fold is recorded against the revision it was taken at"
        );
        let names = |nodes: Vec<codypendent_knowledge::CodeNode>| -> Vec<String> {
            nodes
                .into_iter()
                .map(|node| node.key.qualified_name)
                .collect()
        };
        let after_first = names(
            codypendent_knowledge::codegraph::nodes(&pool, repository)
                .await
                .expect("nodes"),
        );
        assert!(
            after_first.iter().any(|name| name.contains("Alpha")),
            "the first fold carries the committed symbol: {after_first:?}"
        );

        // A second run at the SAME revision must not re-scan: clearing the graph
        // behind the executor's back is only repaired if it re-folds. A rebuild
        // over an empty file list IS the clear — there is no bare public wipe,
        // because on its own it destroys every agent-asserted edge.
        codypendent_knowledge::codegraph::rebuild_repository(
            &pool,
            repository,
            &first_revision,
            std::iter::empty::<(&str, &str)>(),
            // A COMPLETE scan that saw no files: the retire pass is what empties
            // the graph here. Truncated coverage would keep every row instead.
            codypendent_knowledge::codegraph::ScanCoverage::Complete,
        )
        .await
        .expect("clear graph");
        executor.ensure_scanned(repository, &repo).await;
        assert!(
            codypendent_knowledge::codegraph::nodes(&pool, repository)
                .await
                .expect("nodes")
                .is_empty(),
            "an unchanged HEAD must reuse the fold, not re-scan"
        );

        // A commit moves HEAD, so the next run re-folds and picks the new symbol up.
        std::fs::write(repo.join("beta.rs"), "pub struct Beta;\n").expect("write beta");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "beta"]);
        let second_revision = scan::head_revision(&repo);
        assert_ne!(first_revision, second_revision, "HEAD moved");

        executor.ensure_scanned(repository, &repo).await;
        let after_second = names(
            codypendent_knowledge::codegraph::nodes(&pool, repository)
                .await
                .expect("nodes"),
        );
        assert!(
            after_second.iter().any(|name| name.contains("Beta")),
            "a moved HEAD must re-fold the graph: {after_second:?}"
        );
        assert_eq!(
            executor
                .scanned
                .lock()
                .expect("scanned map lock")
                .get(&repository),
            Some(&second_revision),
            "the recorded revision advances with the fold"
        );
    }

    #[tokio::test]
    async fn first_run_emits_the_full_context_a_continuation_does_not() {
        // Continuous-session plan (Task 4): the FIRST run of a session (empty
        // reconstructed prior) opens its trace with the full `=== CONTEXT`
        // repo-map manifest; a CONTINUATION (non-empty prior) opens with the
        // one-line carried-context marker instead, never re-mapping the
        // repository on a follow-up.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");
        let repository = scan::repository_id_for(dir.path());
        let executor =
            RuntimeExecutor::new(pool.clone(), paths, repository, dir.path().to_path_buf());

        // First run: empty prior → the full context manifest lands on the ledger
        // AND is returned for the model seed (2026-08-11 review item 1).
        let first_session = SessionId::new();
        ledger::create_session(&pool, first_session, "first")
            .await
            .expect("create session");
        let manifest = executor
            .emit_run_opening(first_session, RunId::new(), repository, "objective", &[])
            .await;
        assert!(
            context_manifest_present(&pool, first_session).await,
            "a first run must emit the full === CONTEXT manifest"
        );
        assert!(
            manifest.is_some_and(|text| text.starts_with("=== CONTEXT")),
            "the first run's opening must hand back the manifest for the model seed"
        );

        // Continuation: non-empty prior → NO manifest, the marker instead — and
        // nothing returned (the seed already carries the stored context turn).
        let cont_session = SessionId::new();
        ledger::create_session(&pool, cont_session, "cont")
            .await
            .expect("create session");
        let prior = vec![
            TurnItem::Objective("earlier".to_string()),
            TurnItem::Assistant("reply".to_string()),
        ];
        let continuation = executor
            .emit_run_opening(cont_session, RunId::new(), repository, "follow up", &prior)
            .await;
        assert!(
            !context_manifest_present(&pool, cont_session).await,
            "a continuation must NOT emit the === CONTEXT manifest"
        );
        assert!(
            continuation.is_none(),
            "a continuation opening returns no manifest to seed"
        );
    }

    /// M1: `harvest_memories` loads + parses the `RunCompleted` chronicle
    /// artifact and folds `chronicle_candidates` in alongside the existing
    /// event-derived candidates, so a code-ref finding, a decision, an applied
    /// changeset, and a failed action all land as curated `MemoryRecord`s.
    #[tokio::test]
    async fn harvest_memories_appends_chronicle_candidates() {
        use codypendent_daemon::artifacts::Provenance;
        use codypendent_protocol::{ArtifactId, RunDisposition};

        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");
        let repository = scan::repository_id_for(dir.path());
        let executor =
            RuntimeExecutor::new(pool.clone(), paths, repository, dir.path().to_path_buf());

        let session = SessionId::new();
        let run_id = RunId::new();
        ledger::create_session(&pool, session, "harvest")
            .await
            .expect("create session");

        let chronicle_bytes = serde_json::to_vec(&serde_json::json!({
            "objective": "fix the guard",
            "investigations": ["crates/x/src/a.rs:42 the guard is inverted"],
            "changes": [{"changeset_id": "cs-1", "artifact": ArtifactId::new().to_string(), "byte_length": 128}],
            "actions": [{"tool": "shell.run", "outcome": "failed", "artifact": null}],
            "decisions": [],
        }))
        .expect("serialize chronicle");
        let chronicle_ref = executor
            .artifacts()
            .put(
                &pool,
                "application/json",
                DataClassification::Internal,
                Provenance::system("test-chronicle"),
                &chronicle_bytes,
            )
            .await
            .expect("store chronicle artifact");

        let event = codypendent_protocol::SessionEvent {
            sequence: 1,
            occurred_at: Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor: Actor::System,
            body: EventBody::RunCompleted {
                run_id,
                disposition: RunDisposition::Completed {
                    summary: Some("done".to_string()),
                },
                chronicle: chronicle_ref,
            },
        };
        ledger::append_event(&pool, session, &event)
            .await
            .expect("append RunCompleted");

        executor
            .harvest_memories(session, run_id, repository, AgentMode::Build)
            .await;

        let statements: Vec<(String, String)> =
            sqlx::query_as("SELECT class, statement FROM memories ORDER BY class")
                .fetch_all(&pool)
                .await
                .expect("query memories");

        assert!(
            statements
                .iter()
                .any(|(class, statement)| class == "code"
                    && statement.contains("crates/x/src/a.rs:42")),
            "expected a Code memory from the code-ref finding, got {statements:?}"
        );
        assert!(
            !statements
                .iter()
                .any(|(class, statement)| class == "episodic" && statement.contains("cs-1")),
            "routine changeset breadcrumbs belong in the chronicle, not durable memory: {statements:?}"
        );
        assert!(
            statements
                .iter()
                .any(|(class, statement)| class == "failure"
                    && statement.contains("shell.run")
                    && statement.contains("failed")),
            "expected a Failure memory from the failed action, got {statements:?}"
        );
    }

    /// M3a: a `FactExtractor` that always returns an empty `Vec`, mirroring
    /// what `NoopExtractor` does but defined locally so the test asserts
    /// against the TRAIT boundary (`&dyn FactExtractor`), not the concrete
    /// `codypendent_knowledge` type.
    struct MockExtractor(Vec<codypendent_knowledge::CandidateMemory>);

    #[async_trait::async_trait]
    impl FactExtractor for MockExtractor {
        async fn extract(
            &self,
            _input: ExtractionInput<'_>,
        ) -> Vec<codypendent_knowledge::CandidateMemory> {
            self.0.clone()
        }
    }

    /// M3a fallback: `harvest_with` injected with an extractor that
    /// contributes nothing still curates the M1 heuristic candidates — an
    /// empty M3 contribution must never suppress the other mechanisms.
    #[tokio::test]
    async fn harvest_with_empty_extractor_still_curates_heuristic_candidates() {
        use codypendent_daemon::artifacts::Provenance;
        use codypendent_protocol::RunDisposition;

        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");
        let repository = scan::repository_id_for(dir.path());
        let executor =
            RuntimeExecutor::new(pool.clone(), paths, repository, dir.path().to_path_buf());

        let session = SessionId::new();
        let run_id = RunId::new();
        ledger::create_session(&pool, session, "harvest-fallback")
            .await
            .expect("create session");

        let chronicle_bytes = serde_json::to_vec(&serde_json::json!({
            "objective": "fix the guard",
            "investigations": ["crates/x/src/a.rs:42 the guard is inverted"],
            "changes": [],
            "actions": [],
            "decisions": [],
        }))
        .expect("serialize chronicle");
        let chronicle_ref = executor
            .artifacts()
            .put(
                &pool,
                "application/json",
                DataClassification::Internal,
                Provenance::system("test-chronicle-fallback"),
                &chronicle_bytes,
            )
            .await
            .expect("store chronicle artifact");

        let event = codypendent_protocol::SessionEvent {
            sequence: 1,
            occurred_at: Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor: Actor::System,
            body: EventBody::RunCompleted {
                run_id,
                disposition: RunDisposition::Completed {
                    summary: Some("done".to_string()),
                },
                chronicle: chronicle_ref,
            },
        };
        ledger::append_event(&pool, session, &event)
            .await
            .expect("append RunCompleted");

        executor
            .harvest_with(session, run_id, repository, &MockExtractor(vec![]))
            .await;

        let statements: Vec<(String, String)> =
            sqlx::query_as("SELECT class, statement FROM memories ORDER BY class")
                .fetch_all(&pool)
                .await
                .expect("query memories");

        assert!(
            statements
                .iter()
                .any(|(class, statement)| class == "code"
                    && statement.contains("crates/x/src/a.rs:42")),
            "an empty M3a extractor must not suppress the M1 heuristic candidate, got {statements:?}"
        );
    }

    /// M3b: a mock extractor that returns two distinct facts contributes TWO
    /// additional curated `MemoryRecord`s, alongside the M1 heuristic
    /// candidate from the same chronicle — the fan-in accepts every producer's
    /// output, not just the heuristic one.
    #[tokio::test]
    async fn harvest_with_extractor_returning_two_facts_curates_both() {
        use codypendent_daemon::artifacts::Provenance;
        use codypendent_knowledge::{CandidateMemory, EvidenceRef, MemoryClass};
        use codypendent_protocol::{ArtifactId, RunDisposition};

        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");
        let repository = scan::repository_id_for(dir.path());
        let executor =
            RuntimeExecutor::new(pool.clone(), paths, repository, dir.path().to_path_buf());

        let session = SessionId::new();
        let run_id = RunId::new();
        ledger::create_session(&pool, session, "harvest-two-facts")
            .await
            .expect("create session");

        let chronicle_bytes = serde_json::to_vec(&serde_json::json!({
            "objective": "fix the guard",
            "investigations": [],
            "changes": [],
            "actions": [],
            "decisions": [],
        }))
        .expect("serialize chronicle");
        let chronicle_ref = executor
            .artifacts()
            .put(
                &pool,
                "application/json",
                DataClassification::Internal,
                Provenance::system("test-chronicle-two-facts"),
                &chronicle_bytes,
            )
            .await
            .expect("store chronicle artifact");

        let event = codypendent_protocol::SessionEvent {
            sequence: 1,
            occurred_at: Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor: Actor::System,
            body: EventBody::RunCompleted {
                run_id,
                disposition: RunDisposition::Completed {
                    summary: Some("done".to_string()),
                },
                chronicle: chronicle_ref.clone(),
            },
        };
        ledger::append_event(&pool, session, &event)
            .await
            .expect("append RunCompleted");

        // A fabricated "chronicle artifact" evidence ref: any valid `ArtifactRef`
        // works as provenance — `curate` only checks that provenance is non-empty.
        let evidence_artifact = codypendent_protocol::ArtifactRef {
            id: ArtifactId::new(),
            media_type: "application/json".to_string(),
            byte_length: 0,
            sha256: "0".repeat(64),
            sensitivity: DataClassification::Internal,
        };
        let build_fact = |statement: &str, class: MemoryClass| CandidateMemory {
            class,
            scope: None,
            statement: statement.to_string(),
            structured_value: None,
            provenance: vec![EvidenceRef::Artifact {
                artifact: evidence_artifact.clone(),
                source_path: None,
            }],
            confidence: 0.7,
            observed_at: Utc::now(),
            valid_from: codypendent_knowledge::Revision::sequence(1),
            sensitivity: DataClassification::Internal,
            retention: None,
        };
        let mock = MockExtractor(vec![
            build_fact("prefer sqlx over diesel", MemoryClass::Semantic),
            build_fact(
                "retrying without backoff floods the API",
                MemoryClass::Failure,
            ),
        ]);

        executor
            .harvest_with(session, run_id, repository, &mock)
            .await;

        let statements: Vec<(String, String)> =
            sqlx::query_as("SELECT class, statement FROM memories ORDER BY class")
                .fetch_all(&pool)
                .await
                .expect("query memories");

        assert!(
            statements
                .iter()
                .any(|(_, s)| s == "prefer sqlx over diesel"),
            "expected the first extractor fact to be curated, got {statements:?}"
        );
        assert!(
            statements
                .iter()
                .any(|(_, s)| s == "retrying without backoff floods the API"),
            "expected the second extractor fact to be curated, got {statements:?}"
        );
        // The empty chronicle contributes no M1 heuristic candidates. M0's
        // completed-run breadcrumb remains in the chronicle but the memory
        // quality gate rejects it, leaving exactly the two reusable facts.
        assert_eq!(
            statements.len(),
            2,
            "expected only the two reusable extractor facts, got {statements:?}"
        );
    }

    /// M6: cross-mechanism integration. A single run's harvest fans in all
    /// three fact producers at once — a heuristic `Code` finding pulled from
    /// the chronicle (M1), a `memory.propose:` note as `memory.remember`
    /// would emit it (M2), and a stubbed `FactExtractor` standing in for the
    /// LLM path (M3) — and proves they all land in the SAME `memories` table
    /// as distinct, one-line-statement records, retrievable via
    /// `assemble_context` exactly like a later run would see them. A second
    /// extractor fact that duplicates the M1 Code finding verbatim proves the
    /// `curate` dedup gate (`> DEDUP_SIMILARITY` trigram cosine, gate c)
    /// fires ACROSS mechanisms, not merely within one producer's own output —
    /// it collapses into the earlier record instead of adding a second.
    #[tokio::test]
    async fn harvest_composes_heuristic_note_and_extractor_facts_with_dedup() {
        use codypendent_daemon::artifacts::Provenance;
        use codypendent_knowledge::{CandidateMemory, EvidenceRef, MemoryClass};
        use codypendent_protocol::{ArtifactId, RunDisposition};

        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");
        let repository = scan::repository_id_for(dir.path());
        let executor =
            RuntimeExecutor::new(pool.clone(), paths, repository, dir.path().to_path_buf());

        let session = SessionId::new();
        let run_id = RunId::new();
        ledger::create_session(&pool, session, "harvest-compose")
            .await
            .expect("create session");

        // (M1) A chronicle whose one `investigations` line is a code-ref
        // finding, so `chronicle_candidates` yields exactly one Code fact.
        let code_ref_fragment = "crates/x/src/compose.rs:9";
        let chronicle_bytes = serde_json::to_vec(&serde_json::json!({
            "objective": "fix the compose guard",
            "investigations": [format!("{code_ref_fragment} the guard is inverted")],
            "changes": [],
            "actions": [],
            "decisions": [],
        }))
        .expect("serialize chronicle");
        let chronicle_ref = executor
            .artifacts()
            .put(
                &pool,
                "application/json",
                DataClassification::Internal,
                Provenance::system("test-chronicle-compose"),
                &chronicle_bytes,
            )
            .await
            .expect("store chronicle artifact");

        // (M2) The note `memory.remember`'s `execute_memory_remember` would
        // emit for a plain (no structured value) proposal.
        let note_event = codypendent_protocol::SessionEvent {
            sequence: 1,
            occurred_at: Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor: Actor::System,
            body: EventBody::NoteAppended {
                text: "memory.propose: prefer sqlx over diesel".to_string(),
                run_id: Some(run_id),
            },
        };
        ledger::append_event(&pool, session, &note_event)
            .await
            .expect("append NoteAppended");

        let completed_event = codypendent_protocol::SessionEvent {
            sequence: 2,
            occurred_at: Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor: Actor::System,
            body: EventBody::RunCompleted {
                run_id,
                disposition: RunDisposition::Completed {
                    summary: Some("done".to_string()),
                },
                chronicle: chronicle_ref.clone(),
            },
        };
        ledger::append_event(&pool, session, &completed_event)
            .await
            .expect("append RunCompleted");

        // (M3) The stubbed extractor: one genuinely new fact, plus one that
        // duplicates the M1 Code finding verbatim (same statement, same
        // class) — the cross-mechanism dedup case.
        let evidence_artifact = codypendent_protocol::ArtifactRef {
            id: ArtifactId::new(),
            media_type: "application/json".to_string(),
            byte_length: 0,
            sha256: "0".repeat(64),
            sensitivity: DataClassification::Internal,
        };
        let build_fact = |statement: &str, class: MemoryClass| CandidateMemory {
            class,
            scope: None,
            statement: statement.to_string(),
            structured_value: None,
            provenance: vec![EvidenceRef::Artifact {
                artifact: evidence_artifact.clone(),
                source_path: None,
            }],
            confidence: 0.7,
            observed_at: Utc::now(),
            valid_from: codypendent_knowledge::Revision::sequence(2),
            sensitivity: DataClassification::Internal,
            retention: None,
        };
        let llm_distinct_statement = "authentication tokens expire after 24 hours";
        let llm_duplicate_statement = format!("{code_ref_fragment} the guard is inverted");
        let mock = MockExtractor(vec![
            build_fact(llm_distinct_statement, MemoryClass::Semantic),
            build_fact(&llm_duplicate_statement, MemoryClass::Code),
        ]);

        executor
            .harvest_with(session, run_id, repository, &mock)
            .await;

        let statements: Vec<(String, String)> =
            sqlx::query_as("SELECT class, statement FROM memories ORDER BY class, statement")
                .fetch_all(&pool)
                .await
                .expect("query memories");

        // Exactly one Code record survives — the LLM's duplicate collapsed
        // into the M1 heuristic finding rather than adding a second.
        let code_records: Vec<_> = statements
            .iter()
            .filter(|(class, _)| class == "code")
            .collect();
        assert_eq!(
            code_records.len(),
            1,
            "the LLM's duplicate Code fact must dedup against the M1 heuristic finding, got {statements:?}"
        );
        assert!(
            code_records[0].1.contains(code_ref_fragment),
            "expected the surviving Code record to be the heuristic finding, got {statements:?}"
        );

        assert!(
            statements
                .iter()
                .any(|(class, s)| class == "semantic" && s == "prefer sqlx over diesel"),
            "expected the M2 memory.propose note to be curated, got {statements:?}"
        );
        assert!(
            statements
                .iter()
                .any(|(class, s)| class == "semantic" && s == llm_distinct_statement),
            "expected the M3 extractor's distinct fact to be curated, got {statements:?}"
        );

        // Retrieval end-to-end: the DISTINCT curated facts resurface as
        // separate one-line statements in the same repository-scoped context
        // manifest a later run would open with (`context.rs` `=== MEMORIES
        // ===`, capped `MAX_CONTEXT_MEMORIES`).
        let manifest = codypendent_knowledge::assemble_context(
            &pool,
            repository,
            "compose check",
            &[Scope::Repository(repository)],
        )
        .await
        .expect("assemble context");
        let manifest_statements: Vec<&str> = manifest
            .memories
            .iter()
            .map(|m| m.statement.as_str())
            .collect();
        assert!(
            manifest_statements
                .iter()
                .any(|s| s.contains(code_ref_fragment)),
            "expected the Code finding to resurface via assemble_context, got {manifest_statements:?}"
        );
        assert!(
            manifest_statements.contains(&"prefer sqlx over diesel"),
            "expected the M2 note to resurface via assemble_context, got {manifest_statements:?}"
        );
        assert!(
            manifest_statements.contains(&llm_distinct_statement),
            "expected the M3 extractor fact to resurface via assemble_context, got {manifest_statements:?}"
        );

        // Code(1) + Semantic-note(1) + Semantic-llm(1) = 3 durable records.
        // The duplicate LLM fact contributes zero and the routine M0
        // completed-run breadcrumb stays in the chronicle rather than memory.
        assert_eq!(
            statements.len(),
            3,
            "expected exactly 3 reusable records, got {statements:?}"
        );
    }

    // NOT `#[cfg(feature = "provider-openai")]`: `codypendentd` pulls
    // `codypendent-runtime` with default features (provider-openai on), uses
    // `client_for`/`from_registry` unconditionally (executor.rs:454), and defines
    // no `provider-openai` feature of its own — so gating here would make the test
    // dead code.
    #[tokio::test]
    async fn load_model_registry_resolves_a_key_from_auth_json() {
        use codypendent_runtime::auth::AuthStore;
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        paths.ensure_directories().expect("directories");

        // A hosted model whose api_key_env is deliberately unset: env alone fails.
        std::fs::write(
            paths.data_dir.join("models.toml"),
            r#"
[[model]]
id = "groq/llama"
provider = "openai-compatible"
base_url = "https://api.groq.com/openai/v1"
model = "llama-3.1-8b"
api_key_env = "CODYPENDENT_TEST_EXECUTOR_AUTHJSON_UNSET_9c1d"
"#,
        )
        .expect("write models.toml");

        // auth.json carries the key, so the model must build.
        let mut auth = AuthStore::default();
        auth.set("groq/llama", "sk-authjson");
        auth.save(&paths.data_dir).expect("save auth.json");

        let (registry, _policy) = load_model_registry(&paths).expect("load registry");
        assert!(
            registry
                .client_for(&ModelId("groq/llama".to_string()))
                .await
                .is_ok(),
            "load_model_registry must attach auth.json so the key resolves"
        );
    }

    // --- PF4: the executor seam wires `PolicyEngine::load` (policy-files spec) ---

    /// The security-critical failure mode this wiring closes: a malformed
    /// GLOBAL policy file must fail `load_run_policy` with a legible error —
    /// never silently fall back to `PolicyEngine::with_defaults` (which would
    /// be a silent widen back to weaker built-ins for a file the operator
    /// meant to narrow, or a silent drop of the widening they wrote it for).
    /// `execute` propagates this with `?`, so the run does not start.
    #[tokio::test]
    async fn load_run_policy_fails_on_malformed_global_policy_no_silent_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().join("data"));
        paths.ensure_directories().expect("directories");
        std::fs::create_dir_all(&paths.config_dir).expect("config dir");
        std::fs::write(paths.global_policy_path(), "[shell]\nbogus_key = true\n")
            .expect("write malformed global policy");

        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");
        let repo_root = dir.path().join("repo");
        std::fs::create_dir_all(&repo_root).expect("repo root");
        let repository = scan::repository_id_for(&repo_root);
        let executor = RuntimeExecutor::new(pool, paths, repository, repo_root.clone());

        let result = executor.load_run_policy(&repo_root);
        let err = result
            .expect_err("a malformed global policy must fail the run, never silently default");
        assert!(
            err.contains("policy configuration error"),
            "the error must be legible and name the cause, got: {err}"
        );
    }

    /// The contrast: with NEITHER policy file present, `load_run_policy`
    /// succeeds and behaves exactly like `PolicyEngine::with_defaults` — a
    /// user who has written no policy files sees no change (missing is fine,
    /// not an error).
    #[tokio::test]
    async fn load_run_policy_with_no_files_matches_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().join("data"));
        paths.ensure_directories().expect("directories");

        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");
        let repo_root = dir.path().join("repo");
        std::fs::create_dir_all(&repo_root).expect("repo root");
        let repository = scan::repository_id_for(&repo_root);
        let executor = RuntimeExecutor::new(pool, paths, repository, repo_root.clone());

        let policy = executor
            .load_run_policy(&repo_root)
            .expect("no files present: load_run_policy must succeed");
        assert_eq!(
            policy.policy_version(),
            codypendent_daemon::policy::PolicyEngine::with_defaults().policy_version(),
            "with no policy files the loaded engine must match with_defaults exactly"
        );
    }

    /// PR C1: with a web-search client configured, `load_run_policy` admits
    /// the Tavily endpoint on the network allow-list — after the file layers
    /// load, exactly like the GitHub admission — so a `web.search` read
    /// evaluates `Allow`; with no client configured the same proposal stays
    /// denied.
    #[tokio::test]
    async fn load_run_policy_admits_tavily_endpoint_only_when_search_is_configured() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().join("data"));
        paths.ensure_directories().expect("directories");

        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");
        let repo_root = dir.path().join("repo");
        std::fs::create_dir_all(&repo_root).expect("repo root");
        let repository = scan::repository_id_for(&repo_root);
        let executor = RuntimeExecutor::new(pool, paths, repository, repo_root.clone());

        let tavily_request = codypendent_protocol::ProposedAction::NetworkRequest {
            destination: TAVILY_API_ENDPOINT.to_string(),
        };
        let eval_ctx = codypendent_daemon::policy::EvalContext::new(&repo_root, &repo_root)
            .with_mode(mode_overlay(AgentMode::Build));

        let without_search = executor
            .load_run_policy(&repo_root)
            .expect("load without search");
        assert_eq!(
            without_search.evaluate(&tavily_request, &eval_ctx).decision,
            codypendent_daemon::policy::Decision::Deny,
            "no search client → the Tavily endpoint is not admitted"
        );

        let client = TavilyClient::new(
            "http://127.0.0.1:9",
            codypendent_integrations::search::TavilyKey::new("tvly-test"),
        )
        .expect("build client");
        let with_search = executor
            .with_search(Arc::new(client))
            .load_run_policy(&repo_root)
            .expect("load with search");
        assert_eq!(
            with_search.evaluate(&tavily_request, &eval_ctx).decision,
            codypendent_daemon::policy::Decision::Allow,
            "a configured search client admits the Tavily endpoint for reads"
        );
    }

    /// End-to-end through the executor seam: a TRUSTED global policy widens
    /// the shell allow-list to `pytest`, but the SAME addition through the
    /// UNTRUSTED repo-local `.codypendent/policy.toml` does not take effect —
    /// proving the executor's trust routing (global → widen, repo → narrow
    /// only), not just the lower-level `PolicyEngine::load`.
    #[tokio::test]
    async fn load_run_policy_global_widens_pytest_but_repo_cannot() {
        use codypendent_daemon::policy::{Decision, EvalContext};
        use codypendent_protocol::ProposedAction;

        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().join("data"));
        paths.ensure_directories().expect("directories");
        std::fs::create_dir_all(&paths.config_dir).expect("config dir");
        std::fs::write(
            paths.global_policy_path(),
            "[shell]\nallowed_programs = [\"pytest\"]\n",
        )
        .expect("write global policy");

        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");
        let repo_root = dir.path().join("repo");
        std::fs::create_dir_all(&repo_root).expect("repo root");
        let repository = scan::repository_id_for(&repo_root);
        let executor = RuntimeExecutor::new(pool, paths, repository, repo_root.clone());

        let widened = executor
            .load_run_policy(&repo_root)
            .expect("well-formed global policy loads");
        let ctx = EvalContext::new(&repo_root, &repo_root);
        let decision = widened.evaluate(
            &ProposedAction::ExecuteCommand {
                program: "pytest".to_string(),
                args: Vec::new(),
                environment: Vec::new(),
                cwd: None,
            },
            &ctx,
        );
        assert_eq!(
            decision.decision,
            Decision::RequireApproval,
            "the executor must route the global file through the trusted (widening) path"
        );

        // A SEPARATE executor with NO global policy at all — a repo-local file
        // trying the same `pytest` addition on its own must not grant it: the
        // repo layer alone (`apply_untrusted_overlay`) has no widening branch.
        let bare_dir = tempfile::tempdir().expect("tempdir");
        let bare_paths = RuntimePaths::from_data_dir(bare_dir.path().join("data"));
        bare_paths.ensure_directories().expect("directories");
        let bare_pool =
            codypendent_daemon::db::open_database(&bare_paths.data_dir.join("codypendent.db"))
                .await
                .expect("open db");
        let repo_root_2 = bare_dir.path().join("repo");
        std::fs::create_dir_all(repo_root_2.join(".codypendent")).expect(".codypendent dir");
        std::fs::write(
            repo_root_2.join(".codypendent").join("policy.toml"),
            "[shell]\nallowed_programs = [\"cargo\", \"pytest\"]\n",
        )
        .expect("write repo policy");
        let repository_2 = scan::repository_id_for(&repo_root_2);
        let bare_executor =
            RuntimeExecutor::new(bare_pool, bare_paths, repository_2, repo_root_2.clone());

        let repo_only = bare_executor
            .load_run_policy(&repo_root_2)
            .expect("well-formed repo policy loads");
        let repo_ctx = EvalContext::new(&repo_root_2, &repo_root_2);
        let repo_decision = repo_only.evaluate(
            &ProposedAction::ExecuteCommand {
                program: "pytest".to_string(),
                args: Vec::new(),
                environment: Vec::new(),
                cwd: None,
            },
            &repo_ctx,
        );
        assert_eq!(
            repo_decision.decision,
            Decision::Deny,
            "the repo-local layer alone must never be able to widen the allow-list to pytest"
        );

        // Back on the FIRST executor (whose global policy widens pytest): a
        // repo-local file that narrows to `cargo` only must claw the widen
        // back — the repo layer is applied LAST and narrow-only, so it can
        // reduce authority the global layer granted but never exceed it.
        let clawback_repo = dir.path().join("clawback-repo");
        std::fs::create_dir_all(clawback_repo.join(".codypendent")).expect(".codypendent dir");
        std::fs::write(
            clawback_repo.join(".codypendent").join("policy.toml"),
            "[shell]\nallowed_programs = [\"cargo\"]\n",
        )
        .expect("write narrowing repo policy");
        let clawed_back = executor
            .load_run_policy(&clawback_repo)
            .expect("well-formed repo policy loads");
        let clawback_ctx = EvalContext::new(&clawback_repo, &clawback_repo);
        let clawback_decision = clawed_back.evaluate(
            &ProposedAction::ExecuteCommand {
                program: "pytest".to_string(),
                args: Vec::new(),
                environment: Vec::new(),
                cwd: None,
            },
            &clawback_ctx,
        );
        assert_eq!(
            clawback_decision.decision,
            Decision::Deny,
            "the repo-local layer, applied last and narrow-only, must claw back the global widen"
        );
    }

    #[test]
    fn convert_launch_prior_maps_every_variant_and_preserves_order() {
        // Task 2 (continuous-session plan): `RunLaunch.prior` carries
        // `PriorTurn`s (a `codypendent-daemon`-local mirror of
        // `codypendent_runtime::agent::TurnItem` — the daemon crate cannot
        // depend on the runtime crate, see `RunExecutor`'s module doc), which
        // this assembly crate converts 1:1 into `TurnItem` when it builds the
        // `RunContext`.
        let prior = vec![
            PriorTurn::Objective("obj".to_string()),
            PriorTurn::Assistant("reply".to_string()),
            PriorTurn::ToolResult {
                tool: "shell.run".to_string(),
                output: "ok".to_string(),
            },
            PriorTurn::Steering(String::new()),
        ];

        let turns = convert_launch_prior(&prior);

        assert_eq!(
            turns,
            vec![
                TurnItem::Objective("obj".to_string()),
                TurnItem::Assistant("reply".to_string()),
                TurnItem::ToolResult {
                    tool: "shell.run".to_string(),
                    output: "ok".to_string(),
                    artifact: None,
                },
                TurnItem::Steering(String::new()),
            ]
        );
    }

    #[test]
    fn convert_launch_prior_of_empty_is_empty() {
        assert!(convert_launch_prior(&[]).is_empty());
    }

    #[test]
    fn parses_https_and_ssh_remotes() {
        for url in [
            "https://github.com/octocat/hello-world.git",
            "https://github.com/octocat/hello-world",
            "git@github.com:octocat/hello-world.git",
            "ssh://git@github.com/octocat/hello-world.git",
        ] {
            let repo = parse_github_slug(url).expect("parse");
            assert_eq!(repo.owner, "octocat");
            assert_eq!(repo.repo, "hello-world");
        }
    }

    #[test]
    fn discards_url_embedded_credentials() {
        // A token in the URL must be dropped, and the host still matched exactly.
        let repo = parse_github_slug("https://user:ghp_secret@github.com/octocat/hello-world.git")
            .expect("parse");
        assert_eq!(repo.owner, "octocat");
        assert_eq!(repo.repo, "hello-world");
    }

    #[test]
    fn rejects_non_github_and_lookalike_hosts() {
        assert!(parse_github_slug("https://gitlab.com/octocat/hello-world.git").is_none());
        // Look-alike hosts that merely contain the substring must be rejected.
        assert!(parse_github_slug("https://mygithub.com/octocat/hello-world.git").is_none());
        assert!(parse_github_slug("https://github.com.evil.example/octocat/hello.git").is_none());
        assert!(parse_github_slug("").is_none());
    }

    // -- Per-run worktree binding (Phase 5 T5) ------------------------------

    /// Run `git` synchronously in a test, asserting success.
    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Initialise a git repo `parent/repo` with one commit and return its path.
    fn init_git_repo(parent: &Path) -> PathBuf {
        let repo = parent.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "test@codypendent.dev"]);
        git(&repo, &["config", "user.name", "Codypendent Test"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "initial"]);
        repo
    }

    #[tokio::test]
    async fn acp_diff_snapshot_includes_new_and_empty_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_git_repo(dir.path());
        std::fs::write(repo.join("created.txt"), "from ACP\n").unwrap();
        std::fs::write(repo.join("empty.txt"), "").unwrap();
        git(&repo, &["add", "--all", "--"]);
        let diff = bounded_acp_git_diff(&repo).await.expect("bounded diff");
        let text = String::from_utf8_lossy(&diff);
        assert!(text.contains("created.txt"), "new content file: {text}");
        assert!(text.contains("empty.txt"), "new empty file: {text}");
        assert!(text.contains("from ACP"), "new file contents: {text}");
    }

    /// A migrated pool plus an artifact store, both under `dir`.
    async fn test_pool(dir: &Path) -> (SqlitePool, ArtifactStore) {
        let pool = codypendent_daemon::db::open_database(&dir.join("test.db"))
            .await
            .expect("open database");
        (pool, ArtifactStore::new(dir.join("artifacts")))
    }

    /// Insert a session + run so a lease's `owner_run_id` FK resolves.
    async fn seed_run(pool: &SqlitePool) -> RunId {
        let session_id = SessionId::new();
        let run_id = RunId::new();
        let now = Utc::now().to_rfc3339();
        sqlx::query("INSERT INTO sessions (id, title, created_at, updated_at) VALUES (?, ?, ?, ?)")
            .bind(session_id.to_string())
            .bind("worktree-bind")
            .bind(&now)
            .bind(&now)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, budget_json) \
             VALUES (?, ?, 'diagnose', 'Running', 'Build', 'hosted-default', '{}')",
        )
        .bind(run_id.to_string())
        .bind(session_id.to_string())
        .execute(pool)
        .await
        .unwrap();
        run_id
    }

    #[test]
    fn run_writes_to_worktree_matches_the_mode_write_capability() {
        // Only Build writes the worktree (and so needs isolation); the read-only
        // modes keep the shared repository root.
        assert!(run_writes_to_worktree(AgentMode::Build));
        assert!(!run_writes_to_worktree(AgentMode::Explore));
        assert!(!run_writes_to_worktree(AgentMode::Ask));
        assert!(!run_writes_to_worktree(AgentMode::Plan));
        assert!(!run_writes_to_worktree(AgentMode::Review));
    }

    #[tokio::test]
    async fn build_run_allocates_and_releases_an_isolated_worktree() {
        // A single-agent Build run (writes allowed) binds a DEDICATED worktree
        // outside the repository, records the lease on its projection, and
        // releases it cleanly (clean tree ⇒ directory removed, lease released).
        let tmp = tempfile::tempdir().unwrap();
        let (pool, artifacts) = test_pool(tmp.path()).await;
        let repo = init_git_repo(tmp.path());
        let run_id = seed_run(&pool).await;
        let manager = WorktreeManager::new();

        let binding = bind_run_worktree(
            &pool,
            &artifacts,
            &manager,
            run_id,
            run_writes_to_worktree(AgentMode::Build),
            &repo,
        )
        .await
        .expect("Build binds a worktree");
        assert!(binding.lease.is_some(), "a writing run takes a lease");
        assert!(
            binding.worktree.exists(),
            "the worktree directory is created"
        );
        assert!(
            !binding
                .worktree
                .starts_with(std::fs::canonicalize(&repo).unwrap()),
            "the worktree lives OUTSIDE the repository"
        );
        // The lease is recorded on the run's projection (run→worktree provenance).
        let lease_id: Option<String> =
            sqlx::query_scalar("SELECT workspace_lease_id FROM runs WHERE id = ?")
                .bind(run_id.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(lease_id, Some(binding.lease.unwrap().to_string()));

        let worktree = binding.worktree.clone();
        release_run_worktree(&pool, &artifacts, &manager, &binding).await;
        assert!(!worktree.exists(), "a clean worktree is removed on release");
        let state: String = sqlx::query_scalar("SELECT state FROM workspace_leases WHERE id = ?")
            .bind(binding.lease.unwrap().to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "released");
    }

    /// A worker whose worktree is RETAINED (it held work) must tell the user so.
    ///
    /// The protective release path exports a patch and keeps the directory and
    /// its branch on purpose — and reported it to nobody: `ReleaseOutcome` had no
    /// reader outside the daemon's own tests, so a fan-out whose workers failed
    /// left orphan worktrees and `codypendent/run-*` branches in the repository
    /// with nothing in the product mentioning them.
    #[tokio::test]
    async fn a_retained_worktree_is_reported_on_the_run_s_session() {
        let tmp = tempfile::tempdir().unwrap();
        let (pool, artifacts) = test_pool(tmp.path()).await;
        let repo = init_git_repo(tmp.path());
        let run_id = seed_run(&pool).await;
        let manager = WorktreeManager::new();

        let binding = bind_run_worktree(
            &pool,
            &artifacts,
            &manager,
            run_id,
            run_writes_to_worktree(AgentMode::Build),
            &repo,
        )
        .await
        .expect("Build binds a worktree");

        // Dirty the worktree, so the protective path preserves it.
        std::fs::write(binding.worktree.join("worker.txt"), b"half-finished work").unwrap();

        release_run_worktree(&pool, &artifacts, &manager, &binding).await;
        assert!(
            binding.worktree.exists(),
            "unmerged work must still be preserved"
        );

        let session_id = projections::run_session(&pool, run_id)
            .await
            .unwrap()
            .expect("the run has a session");
        let notes: Vec<String> = sqlx::query_scalar("SELECT body FROM events WHERE session_id = ?")
            .bind(session_id.to_string())
            .fetch_all(&pool)
            .await
            .unwrap();
        let note = notes
            .iter()
            .find(|body| body.contains("Kept the worktree"))
            .unwrap_or_else(|| {
                panic!("a preserved worktree must be reported on the session, got {notes:?}")
            });
        assert!(
            note.contains(&binding.worktree.display().to_string()),
            "the note must name the retained path: {note}"
        );
        assert!(
            note.contains("worktree remove") && note.contains("branch -D"),
            "the note must tell the user how to recover or discard it: {note}"
        );
    }

    #[tokio::test]
    async fn build_run_against_non_git_directory_fails_with_actionable_message() {
        // The usability bug this guards: launching a Build run from a directory
        // that is not a Git repository used to die with the raw
        // `git rev-parse HEAD` stderr. `bind_run_worktree` must instead fail the
        // run with a message that names the path and tells the user what to do —
        // and must NOT leak `rev-parse` or git's raw "fatal: not a git
        // repository" text.
        let tmp = tempfile::tempdir().unwrap();
        let (pool, artifacts) = test_pool(tmp.path()).await;
        let not_a_repo = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(&not_a_repo).unwrap();
        let run_id = seed_run(&pool).await;
        let manager = WorktreeManager::new();

        // `.err()` rather than `expect_err` — `WorktreeBinding` (the `Ok` side)
        // does not derive `Debug`, which `expect_err` requires.
        let error = bind_run_worktree(
            &pool,
            &artifacts,
            &manager,
            run_id,
            run_writes_to_worktree(AgentMode::Build),
            &not_a_repo,
        )
        .await
        .err()
        .expect("a non-git directory must fail the run rather than allocate");

        let canonical = std::fs::canonicalize(&not_a_repo).unwrap();
        assert!(
            error.contains(&canonical.display().to_string()),
            "error must name the path, got: {error}"
        );
        assert!(
            error.contains("git init"),
            "error must guide the user to `git init`, got: {error}"
        );
        assert!(
            !error.contains("rev-parse"),
            "error must not leak the raw git command, got: {error}"
        );
        assert!(
            !error.contains("fatal: not a git repository"),
            "error must not leak raw git stderr, got: {error}"
        );
    }

    #[tokio::test]
    async fn explore_run_keeps_the_repository_root_and_binds_no_worktree() {
        // A read-only Explore run (writes denied by policy) keeps running in the
        // repository root: no worktree is allocated, no lease is taken, and
        // releasing the (empty) binding is a no-op.
        let tmp = tempfile::tempdir().unwrap();
        let (pool, artifacts) = test_pool(tmp.path()).await;
        let repo = init_git_repo(tmp.path());
        let run_id = seed_run(&pool).await;
        let manager = WorktreeManager::new();

        let binding = bind_run_worktree(
            &pool,
            &artifacts,
            &manager,
            run_id,
            run_writes_to_worktree(AgentMode::Explore),
            &repo,
        )
        .await
        .expect("Explore keeps the repo root");
        assert!(binding.lease.is_none(), "a read-only run takes no lease");
        assert_eq!(binding.worktree, repo, "it runs in the repository root");
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM workspace_leases")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "no lease row is written for a read-only run");

        // Releasing an empty binding is a clean no-op (and leaves the repo intact).
        release_run_worktree(&pool, &artifacts, &manager, &binding).await;
        assert!(repo.exists());
    }

    #[tokio::test]
    async fn a_failed_fork_stash_reapply_stores_its_safety_patch_where_readers_look() {
        // When reapplying a fork's stash checkpoint fails, the freshly allocated
        // worktree is FORCE-removed and the manager exports the discarded work as
        // a safety patch, recording an `ArtifactRef` row in the DB. That blob must
        // land in the CANONICAL store (`<data_dir>/artifacts`) every reader opens
        // — writing it under the user's checkout left the recorded row dangling
        // (the only copy of their work unreachable) and littered the repository
        // with an untracked `.codypendent/artifacts/`.
        use codypendent_protocol::ArtifactId;
        use std::str::FromStr;

        let tmp = tempfile::tempdir().unwrap();
        let (pool, artifacts) = test_pool(tmp.path()).await;
        let repo = init_git_repo(tmp.path());

        // A stash whose reapply CONFLICTS with HEAD: `apply_stash` fails and
        // leaves real (conflicted) content in the tree for the patch to capture.
        std::fs::write(repo.join("README.md"), "stashed work\n").unwrap();
        git(&repo, &["stash", "push", "-q", "-m", "fork checkpoint"]);
        let stash_sha = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(&repo)
                .args(["rev-parse", "stash@{0}"])
                .output()
                .expect("spawn git")
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        std::fs::write(repo.join("README.md"), "conflicting head\n").unwrap();
        git(&repo, &["commit", "-aqm", "conflicting change"]);

        let run_id = seed_run(&pool).await;
        sqlx::query(
            "UPDATE sessions SET fork_checkpoint_kind = 'stash', fork_checkpoint_sha = ? \
             WHERE id = (SELECT session_id FROM runs WHERE id = ?)",
        )
        .bind(&stash_sha)
        .bind(run_id.to_string())
        .execute(&pool)
        .await
        .unwrap();

        let manager = WorktreeManager::new();
        let error = bind_run_worktree(&pool, &artifacts, &manager, run_id, true, &repo)
            .await
            .err()
            .expect("a conflicting stash reapply must fail the bind");
        assert!(
            error.contains("could not reapply fork checkpoint stash"),
            "unexpected failure: {error}"
        );

        // The safety patch is recorded AND retrievable through the same store the
        // readers use — `verify` reads the blob back from that store's root.
        let patch_id: String =
            sqlx::query_scalar("SELECT id FROM artifacts WHERE media_type = 'text/x-diff'")
                .fetch_one(&pool)
                .await
                .expect("the discarded work is exported as a safety patch");
        let patch_id = ArtifactId::from_str(&patch_id).unwrap();
        assert!(
            artifacts
                .verify(&pool, patch_id)
                .await
                .expect("the recorded artifact resolves in the canonical store"),
            "the safety patch must be readable through the canonical store"
        );

        assert!(
            !repo.join(".codypendent/artifacts").exists(),
            "no artifact store may be created inside the user's checkout"
        );
    }

    /// A session row so the plan bridge's prompt/ledger writes resolve.
    async fn seed_session(pool: &SqlitePool) -> SessionId {
        let session_id = SessionId::new();
        ledger::create_session(pool, session_id, "plan-bridge")
            .await
            .unwrap();
        session_id
    }

    #[tokio::test]
    async fn a_duplicate_plan_exit_enqueues_one_continuation() {
        // The `PlanBridge` contract is "idempotent on `run_id`: a duplicate call
        // for the same transition enqueues once". The bridge ignored `run_id`, so
        // a model that called `plan_exit` twice — or a retried tool call — left
        // TWO continuation turns on the queue. The texts differ here on purpose:
        // `prompt_queue::enqueue`'s exact-text dedupe must not be what saves us.
        use codypendent_runtime::agent::PlanBridge;

        let tmp = tempfile::tempdir().unwrap();
        let (pool, _artifacts) = test_pool(tmp.path()).await;
        let session_id = seed_session(&pool).await;
        let run_id = RunId::new();
        let bridge = PoolPlanBridge::new(pool.clone(), SubscriptionHub::new());

        bridge
            .switch_mode(session_id, run_id, AgentMode::Build, "plan done".into())
            .await
            .unwrap();
        bridge
            .switch_mode(
                session_id,
                run_id,
                AgentMode::Build,
                "plan done (retry)".into(),
            )
            .await
            .unwrap();

        let queued: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pending_prompts WHERE session_id = ?")
                .bind(session_id.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(queued, 1, "a duplicate transition enqueues once");

        let events = ledger::load_events(&pool, session_id).await.unwrap();
        let announced = events
            .iter()
            .filter(|e| matches!(e.body, EventBody::PendingPromptsChanged { .. }))
            .count();
        assert_eq!(announced, 1, "and is announced once");

        // A DIFFERENT transition on the same run is a distinct transition and
        // still enqueues.
        bridge
            .switch_mode(session_id, run_id, AgentMode::Review, "now review".into())
            .await
            .unwrap();
        let queued: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pending_prompts WHERE session_id = ?")
                .bind(session_id.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            queued, 2,
            "a different target mode is a different transition"
        );
    }

    /// SQLite serializes writers, so a concurrent write can come back `SQLITE_BUSY`
    /// ("database is locked"). That is transient contention every caller retries —
    /// NOT the defect under test, which surfaces as a `UNIQUE`/primary-key
    /// violation on `events (session_id, sequence)`.
    fn is_transient_lock(error: &anyhow::Error) -> bool {
        let text = format!("{error:#}");
        text.contains("database is locked")
    }

    #[tokio::test]
    async fn a_mode_switch_races_the_live_run_s_appends_without_losing_its_event() {
        // `events` is keyed `(session_id, sequence)` and the live run appends
        // through the atomic `append_next_event`. The bridge used to read
        // `next_sequence` and then insert the row it computed — a losing race
        // made that insert violate the primary key, so `plan_exit` reported
        // failure to the model AFTER the prompt had already been committed in
        // the earlier transaction: a duplicate continuation sat in the queue and
        // no `PendingPromptsChanged` was ever published.
        use codypendent_runtime::agent::PlanBridge;

        let tmp = tempfile::tempdir().unwrap();
        let (pool, _artifacts) = test_pool(tmp.path()).await;
        let session_id = seed_session(&pool).await;
        let bridge = PoolPlanBridge::new(pool.clone(), SubscriptionHub::new());

        // A concurrent appender standing in for the live run's own event stream,
        // interleaving with the bridge at every await point.
        const APPENDS: usize = 400;
        const SWITCHES: usize = 60;
        let appender = {
            let pool = pool.clone();
            tokio::spawn(async move {
                for _ in 0..APPENDS {
                    loop {
                        match ledger::append_next_event(
                            &pool,
                            session_id,
                            &Actor::System,
                            &EventBody::NoteAppended {
                                text: "live run progress".into(),
                                run_id: None,
                            },
                            Utc::now(),
                        )
                        .await
                        {
                            Ok(_) => break,
                            Err(error) if is_transient_lock(&error) => {
                                tokio::task::yield_now().await;
                            }
                            Err(error) => panic!("a concurrent append collided: {error:#}"),
                        }
                    }
                    tokio::task::yield_now().await;
                }
            })
        };

        // Distinct runs, so the idempotency claim never short-circuits a call:
        // every one of these must survive the contention.
        for i in 0..SWITCHES {
            loop {
                match bridge
                    .switch_mode(
                        session_id,
                        RunId::new(),
                        AgentMode::Build,
                        format!("plan done {i}"),
                    )
                    .await
                {
                    Ok(()) => break,
                    Err(error) if is_transient_lock(&error) => tokio::task::yield_now().await,
                    Err(error) => {
                        panic!("the mode switch lost a sequence race: {error:#}")
                    }
                }
            }
        }
        appender.await.unwrap();

        let events = ledger::load_events(&pool, session_id).await.unwrap();
        let sequences: Vec<u64> = events.iter().map(|e| e.sequence).collect();
        let expected: Vec<u64> = (1..=sequences.len() as u64).collect();
        assert_eq!(
            sequences, expected,
            "every append lands on a unique, gapless sequence"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.body, EventBody::PendingPromptsChanged { .. }))
                .count(),
            SWITCHES,
            "each queued continuation is announced exactly once"
        );
    }

    #[tokio::test]
    async fn release_guard_releases_the_worktree_on_the_normal_path() {
        let tmp = tempfile::tempdir().unwrap();
        let (pool, artifacts) = test_pool(tmp.path()).await;
        let repo = init_git_repo(tmp.path());
        let run_id = seed_run(&pool).await;
        let manager = WorktreeManager::new();
        let binding = bind_run_worktree(&pool, &artifacts, &manager, run_id, true, &repo)
            .await
            .unwrap();
        let lease_id = binding.lease.unwrap();
        let worktree = binding.worktree.clone();

        let unified_exec = Arc::new(codypendent_daemon::unified_exec::UnifiedExecManager::new());
        WorktreeReleaseGuard::arm(pool.clone(), artifacts, manager, unified_exec, binding)
            .release()
            .await;

        assert!(
            !worktree.exists(),
            "the normal release removes a clean worktree"
        );
        let state: String = sqlx::query_scalar("SELECT state FROM workspace_leases WHERE id = ?")
            .bind(lease_id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "released");
    }

    #[tokio::test]
    async fn release_guard_releases_the_worktree_on_unwind() {
        // A guard dropped while still armed (the panic path — `release` never ran)
        // schedules the async release, so the lease still lands `released` and the
        // clean worktree is removed: a panicking drive leaks nothing.
        let tmp = tempfile::tempdir().unwrap();
        let (pool, artifacts) = test_pool(tmp.path()).await;
        let repo = init_git_repo(tmp.path());
        let run_id = seed_run(&pool).await;
        let manager = WorktreeManager::new();
        let binding = bind_run_worktree(&pool, &artifacts, &manager, run_id, true, &repo)
            .await
            .unwrap();
        let lease_id = binding.lease.unwrap();
        let worktree = binding.worktree.clone();

        // Drop the guard WITHOUT calling `release` — models an unwind through it.
        let unified_exec = Arc::new(codypendent_daemon::unified_exec::UnifiedExecManager::new());
        drop(WorktreeReleaseGuard::arm(
            pool.clone(),
            artifacts,
            manager,
            unified_exec,
            binding,
        ));

        // The detached release runs on the current runtime; wait for it to land.
        let mut released = false;
        for _ in 0..200 {
            let state: Option<String> =
                sqlx::query_scalar("SELECT state FROM workspace_leases WHERE id = ?")
                    .bind(lease_id.to_string())
                    .fetch_optional(&pool)
                    .await
                    .unwrap();
            if state.as_deref() == Some("released") {
                released = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(released, "the unwind path releases the lease");
        assert!(
            !worktree.exists(),
            "the unwind path removes the clean worktree"
        );
    }

    /// Continuation-content plan, Task 3: `reconstruct_prior` must HYDRATE a
    /// prior run's `TurnItem::ToolResult` from its stored artifact — the whole
    /// payoff of T1 (persist) + T2 (carry the ref) is that a continuation's
    /// seed transcript shows the real file content instead of the
    /// `tool_result_summary` fallback string.
    #[tokio::test]
    async fn reconstruct_prior_hydrates_a_tool_result_from_its_stored_artifact() {
        use codypendent_daemon::artifacts::Provenance;
        use codypendent_protocol::ToolOutcome;

        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");
        let repository = scan::repository_id_for(dir.path());
        let executor =
            RuntimeExecutor::new(pool.clone(), paths, repository, dir.path().to_path_buf());

        let session = SessionId::new();
        let prior_run = RunId::new();
        let current_run = RunId::new();
        ledger::create_session(&pool, session, "hydrate-prior")
            .await
            .expect("create session");

        let stored_bytes = b"crates/x/src/a.rs:1-5\nfn a() {\n    todo!()\n}\n".to_vec();
        let artifact_ref = executor
            .artifacts()
            .put(
                &pool,
                "text/plain",
                DataClassification::Internal,
                Provenance::tool_output("workspace.read_file", prior_run),
                &stored_bytes,
            )
            .await
            .expect("store read_file artifact");

        let events = [
            EventBody::RunStarted {
                run_id: prior_run,
                objective: "read the file".to_string(),
                mode: AgentMode::Build,
            },
            EventBody::ToolCompleted {
                run_id: prior_run,
                tool: "workspace.read_file".to_string(),
                outcome: ToolOutcome::Succeeded,
                artifact: Some(artifact_ref.clone()),
            },
        ];
        for (index, body) in events.into_iter().enumerate() {
            let event = codypendent_protocol::SessionEvent {
                sequence: (index + 1) as u64,
                occurred_at: Utc::now(),
                causation_id: None,
                correlation_id: None,
                actor: Actor::System,
                body,
            };
            ledger::append_event(&pool, session, &event)
                .await
                .expect("append event");
        }

        let prior = executor.reconstruct_prior(session, current_run).await;

        let hydrated = prior
            .iter()
            .find(
                |t| matches!(t, TurnItem::ToolResult { tool, .. } if tool == "workspace.read_file"),
            )
            .expect("hydrated workspace.read_file ToolResult");
        match hydrated {
            TurnItem::ToolResult { output, .. } => {
                assert!(
                    output.contains("crates/x/src/a.rs:1-5"),
                    "hydrated output must contain the stored path/line header, got: {output}"
                );
                assert!(
                    output.contains("todo!()"),
                    "hydrated output must contain the stored file excerpt, got: {output}"
                );
                assert_ne!(
                    output, "succeeded",
                    "the succeeded fallback must be replaced by the real content"
                );
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    /// A stored artifact LARGER than `CONTINUATION_TOOL_EXCERPT_BYTES` is cut
    /// to the cap and annotated with a truthful truncation marker — never
    /// silently handed to the model as if it were the whole file.
    #[tokio::test]
    async fn reconstruct_prior_truncates_an_oversized_artifact_with_a_marker() {
        use codypendent_daemon::artifacts::Provenance;
        use codypendent_protocol::ToolOutcome;

        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");
        let repository = scan::repository_id_for(dir.path());
        let executor =
            RuntimeExecutor::new(pool.clone(), paths, repository, dir.path().to_path_buf());

        let session = SessionId::new();
        let prior_run = RunId::new();
        let current_run = RunId::new();
        ledger::create_session(&pool, session, "hydrate-truncate")
            .await
            .expect("create session");

        // Well over the 2 KiB per-turn cap.
        let stored_bytes = "x".repeat(CONTINUATION_TOOL_EXCERPT_BYTES * 4).into_bytes();
        let artifact_ref = executor
            .artifacts()
            .put(
                &pool,
                "text/plain",
                DataClassification::Internal,
                Provenance::tool_output("workspace.read_file", prior_run),
                &stored_bytes,
            )
            .await
            .expect("store oversized artifact");

        let events = [
            EventBody::RunStarted {
                run_id: prior_run,
                objective: "read a huge file".to_string(),
                mode: AgentMode::Build,
            },
            EventBody::ToolCompleted {
                run_id: prior_run,
                tool: "workspace.read_file".to_string(),
                outcome: ToolOutcome::Succeeded,
                artifact: Some(artifact_ref.clone()),
            },
        ];
        for (index, body) in events.into_iter().enumerate() {
            let event = codypendent_protocol::SessionEvent {
                sequence: (index + 1) as u64,
                occurred_at: Utc::now(),
                causation_id: None,
                correlation_id: None,
                actor: Actor::System,
                body,
            };
            ledger::append_event(&pool, session, &event)
                .await
                .expect("append event");
        }

        let prior = executor.reconstruct_prior(session, current_run).await;
        let hydrated = prior
            .iter()
            .find(
                |t| matches!(t, TurnItem::ToolResult { tool, .. } if tool == "workspace.read_file"),
            )
            .expect("hydrated workspace.read_file ToolResult");
        match hydrated {
            TurnItem::ToolResult { output, .. } => {
                assert!(
                    output.len() <= CONTINUATION_TOOL_EXCERPT_BYTES + 64,
                    "output must be bounded near the per-turn cap, got {} bytes",
                    output.len()
                );
                assert!(
                    output.contains("truncated"),
                    "an oversized artifact must be annotated with a truncation marker, got: {output}"
                );
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    /// Best-effort / never-fail: a `ToolResult` whose artifact ref points at
    /// nothing readable (missing row) must fall back to the `"succeeded"`
    /// summary rather than panicking or failing continuation reconstruction —
    /// the module's degrade-to-cold ethos (mirrors `load_chronicle`'s callers).
    #[tokio::test]
    async fn reconstruct_prior_falls_back_to_succeeded_when_the_artifact_is_missing() {
        use codypendent_protocol::{ArtifactId, ToolOutcome};

        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");
        let repository = scan::repository_id_for(dir.path());
        let executor =
            RuntimeExecutor::new(pool.clone(), paths, repository, dir.path().to_path_buf());

        let session = SessionId::new();
        let prior_run = RunId::new();
        let current_run = RunId::new();
        ledger::create_session(&pool, session, "hydrate-missing")
            .await
            .expect("create session");

        // A well-formed `ArtifactRef` that names an id no row was ever written
        // for — `open` must fail, and that failure must degrade gracefully.
        let dangling_artifact = ArtifactRef {
            id: ArtifactId::new(),
            media_type: "text/plain".to_string(),
            byte_length: 42,
            sha256: "0".repeat(64),
            sensitivity: DataClassification::Internal,
        };

        let events = [
            EventBody::RunStarted {
                run_id: prior_run,
                objective: "read a file".to_string(),
                mode: AgentMode::Build,
            },
            EventBody::ToolCompleted {
                run_id: prior_run,
                tool: "workspace.read_file".to_string(),
                outcome: ToolOutcome::Succeeded,
                artifact: Some(dangling_artifact),
            },
        ];
        for (index, body) in events.into_iter().enumerate() {
            let event = codypendent_protocol::SessionEvent {
                sequence: (index + 1) as u64,
                occurred_at: Utc::now(),
                causation_id: None,
                correlation_id: None,
                actor: Actor::System,
                body,
            };
            ledger::append_event(&pool, session, &event)
                .await
                .expect("append event");
        }

        // Must not panic; the run continues with the untouched fallback.
        let prior = executor.reconstruct_prior(session, current_run).await;
        let untouched = prior
            .iter()
            .find(
                |t| matches!(t, TurnItem::ToolResult { tool, .. } if tool == "workspace.read_file"),
            )
            .expect("ToolResult for the run with the dangling artifact");
        match untouched {
            TurnItem::ToolResult { output, .. } => {
                assert!(
                    output.starts_with("succeeded"),
                    "a missing artifact must leave the succeeded fallback in place, got: {output}"
                );
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    /// Continuation-content plan, Task 4 (capstone integration test): prove
    /// the T1 (persist) → T2 (carry the ref through the projection) → T3
    /// (hydrate) chain COMPOSES across a real run boundary — the gap a
    /// per-layer unit test cannot see. A PRIOR run is driven through the REAL
    /// `FrameworkAgentRuntime::execute_run` loop (Explore mode: reading needs
    /// no approval), so its two `workspace.read_file` calls persist genuine
    /// artifacts through the actual T1 producer code — never hand-built
    /// bytes — landing real `ToolCompleted` events with `artifact: Some(..)`
    /// on the ledger, in the SAME `ArtifactStore` `reconstruct_prior` reads
    /// from below. A NEW run in the same session then calls the real
    /// `RuntimeExecutor::reconstruct_prior` — the exact seam a live
    /// continuation run calls at start — and the assertion is the whole
    /// payoff of the plan: the seed transcript's `ToolResult`s for
    /// `workspace.read_file` must carry the real file content (the #37
    /// path/line header plus the excerpt), never the bare `"succeeded"`
    /// `tool_result_summary` fallback — so the model sees what it already
    /// read instead of re-reading it.
    #[tokio::test]
    async fn continuation_seed_carries_prior_read_file_content_across_a_real_run() {
        use codypendent_protocol::RunDisposition;
        use codypendent_runtime::agent::{ModelStep, ScriptedDriver};

        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");

        // The repository the prior run reads — a real checkout-shaped
        // directory, separate from the daemon's own data dir.
        let repo = tempfile::tempdir().expect("repo tempdir");
        std::fs::write(
            repo.path().join("alpha.rs"),
            "fn alpha() -> u32 {\n    1\n}\n",
        )
        .expect("write alpha.rs");
        std::fs::write(
            repo.path().join("beta.rs"),
            "fn beta() -> u32 {\n    2\n}\n",
        )
        .expect("write beta.rs");

        let repository = scan::repository_id_for(repo.path());
        let executor =
            RuntimeExecutor::new(pool.clone(), paths, repository, repo.path().to_path_buf());

        let session = SessionId::new();
        let prior_run = RunId::new();
        let current_run = RunId::new();
        ledger::create_session(&pool, session, "cross-run-hydration")
            .await
            .expect("create session");

        // Seed the prior run's row + `RunStarted`, exactly as the `StartRun`
        // command does before the loop runs — `execute_run` executes an
        // ALREADY-STARTED run (mirrors `seed_started_run!` in
        // `crates/runtime/tests/agent_it.rs`).
        projections::insert_run(
            &pool,
            prior_run,
            session,
            "read two files",
            AgentMode::Explore,
            "hosted",
            "{}",
        )
        .await
        .expect("insert prior run row");
        let prior_started = codypendent_protocol::SessionEvent {
            sequence: ledger::next_sequence(&pool, session)
                .await
                .expect("sequence"),
            occurred_at: Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor: Actor::System,
            body: EventBody::RunStarted {
                run_id: prior_run,
                objective: "read two files".to_string(),
                mode: AgentMode::Explore,
            },
        };
        ledger::append_event(&pool, session, &prior_started)
            .await
            .expect("append RunStarted");

        // Drive the REAL agent loop for the prior run over the SAME pool +
        // artifact store `reconstruct_prior` reads from below — the T1
        // producer runs for real here, not a hand-built `ToolCompleted`.
        let broker = ApprovalBroker::new();
        let hub = SubscriptionHub::new();
        let journal = run_journal(&pool, &broker);
        let sink = artifact_sink(&pool, executor.artifacts());
        let runtime = FrameworkAgentRuntime::new(
            ModelRegistry::new(Vec::new()),
            PolicyEngine::with_defaults(),
            broker,
            hub,
            journal,
            sink,
        );
        let driver = ScriptedDriver::new(vec![
            ModelStep::CallTool {
                tool: "workspace.read_file".to_string(),
                args: serde_json::json!({"path": "alpha.rs"}),
            },
            ModelStep::CallTool {
                tool: "workspace.read_file".to_string(),
                args: serde_json::json!({"path": "beta.rs"}),
            },
            ModelStep::Finish {
                summary: "read both files".to_string(),
            },
        ]);
        let ctx = RunContext::new(
            session,
            prior_run,
            "read two files",
            AgentMode::Explore,
            repo.path(),
            repo.path(),
        );
        let outcome = runtime
            .execute_run(&driver, ctx, CancellationToken::never())
            .await
            .expect("prior run executes");
        assert!(
            matches!(outcome.disposition, RunDisposition::Completed { .. }),
            "the prior run must complete for its events to land on the ledger"
        );

        // Sanity: the prior run's real `ToolCompleted` events actually carry
        // artifacts (the T1 producer ran for real) before asserting on the
        // continuation seed built from them.
        let prior_events = ledger::load_events(&pool, session)
            .await
            .expect("load prior events");
        let artifact_count = prior_events
            .iter()
            .filter(|e| {
                matches!(
                    &e.body,
                    EventBody::ToolCompleted {
                        artifact: Some(_),
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            artifact_count, 2,
            "both read_file calls must have persisted a real artifact"
        );

        // A NEW run in the same session — its own `RunStarted` is already on
        // the ledger before it executes (the runtime seeds the current
        // objective itself), mirroring `continuation_prior`'s doc contract.
        let follow_up_started = codypendent_protocol::SessionEvent {
            sequence: ledger::next_sequence(&pool, session)
                .await
                .expect("sequence"),
            occurred_at: Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor: Actor::System,
            body: EventBody::RunStarted {
                run_id: current_run,
                objective: "now check the config".to_string(),
                mode: AgentMode::Explore,
            },
        };
        ledger::append_event(&pool, session, &follow_up_started)
            .await
            .expect("append current run's RunStarted");

        // THE ASSERTION: the real `reconstruct_prior` — the exact seam a live
        // continuation run calls — must show the actual prior file content,
        // never the `tool_result_summary` "succeeded" fallback.
        let prior = executor.reconstruct_prior(session, current_run).await;

        let tool_results: Vec<&TurnItem> = prior
            .iter()
            .filter(
                |t| matches!(t, TurnItem::ToolResult { tool, .. } if tool == "workspace.read_file"),
            )
            .collect();
        assert_eq!(
            tool_results.len(),
            2,
            "both prior read_file calls must appear in the continuation seed, got: {tool_results:?}"
        );

        let outputs: Vec<&str> = tool_results
            .iter()
            .map(|t| match t {
                TurnItem::ToolResult { output, .. } => output.as_str(),
                _ => unreachable!("filtered to ToolResult above"),
            })
            .collect();

        for output in &outputs {
            assert_ne!(
                *output, "succeeded",
                "the continuation must SEE the prior read's content, not the bare succeeded fallback"
            );
        }
        assert!(
            outputs
                .iter()
                .any(|o| o.contains("alpha.rs") && o.contains("fn alpha")),
            "the alpha.rs read must hydrate with its path header and content, got: {outputs:?}"
        );
        assert!(
            outputs
                .iter()
                .any(|o| o.contains("beta.rs") && o.contains("fn beta")),
            "the beta.rs read must hydrate with its path header and content, got: {outputs:?}"
        );
    }

    /// A [`codypendent_runtime::agent::ModelDriver`] that records every
    /// transcript it is handed and immediately finishes — the probe for the
    /// 2026-08-11 review's headline fix: what does the MODEL actually receive?
    struct RecordingDriver {
        transcripts: Arc<Mutex<Vec<Vec<TurnItem>>>>,
    }

    #[async_trait]
    impl codypendent_runtime::agent::ModelDriver for RecordingDriver {
        fn model_id(&self) -> ModelId {
            ModelId("recording".to_string())
        }

        async fn next_step(
            &self,
            transcript: &[TurnItem],
            _tools: &[codypendent_runtime::agent::ToolDefinition],
            _sink: &mut dyn codypendent_runtime::agent::DeltaSink,
        ) -> anyhow::Result<codypendent_runtime::agent::StepOutcome> {
            use codypendent_runtime::agent::{ModelStep, StepOutcome};
            self.transcripts
                .lock()
                .expect("recording lock")
                .push(transcript.to_vec());
            Ok(StepOutcome::new(
                ModelStep::Finish {
                    summary: "done".to_string(),
                },
                None,
            ))
        }
    }

    /// Write a minimal ACTIVE repository-scoped skill package at `dir`, with
    /// intents matching a CI-failure objective.
    fn write_ci_skill_package(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).expect("package dir");
        std::fs::write(
            dir.join("skill.toml"),
            "schema_version = 1\n\
             id = \"test.fix-flaky-ci\"\n\
             name = \"Fix Flaky CI\"\n\
             version = \"0.1.0\"\n\
             scope = \"repository\"\n\
             status = \"active\"\n\
             description = \"Diagnose and repair flaky CI test failures.\"\n\
             intents = [\"ci failure\", \"flaky test\"]\n\
             \n\
             [entrypoints]\n\
             instructions = \"SKILL.md\"\n\
             \n\
             [trust]\n\
             publisher = \"local-user\"\n\
             signature_required = false\n",
        )
        .expect("write skill.toml");
        std::fs::write(dir.join("SKILL.md"), "# Fix flaky CI\n").expect("write SKILL.md");
    }

    /// 2026-08-11 review item 1 (the CRITICAL "context never reaches the model"
    /// finding), first-run side: the seed built by the REAL
    /// [`RuntimeExecutor::build_run_seed`] — the exact code `spawn_run` runs —
    /// must open with the context-manifest turn, and driving the REAL
    /// [`FrameworkAgentRuntime::execute_run`] with that seed must hand the
    /// DRIVER a transcript containing the repository map and the disclosed
    /// skill card, ahead of the objective. Before the fix the manifest was a
    /// `NoteAppended` trace event only and this transcript carried nothing but
    /// the objective.
    #[tokio::test]
    async fn first_run_seed_reaches_the_model_driver_with_repo_map_and_skill_card() {
        use codypendent_knowledge::{codegraph, register_builtins, GitRevision, Registry};

        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");

        let repo = tempfile::tempdir().expect("repo tempdir");
        let repository = scan::repository_id_for(repo.path());
        let executor =
            RuntimeExecutor::new(pool.clone(), paths, repository, repo.path().to_path_buf());

        // Seed all three manifest surfaces: builtins (tool cards), a real code
        // graph (repository map), and an ACTIVE repository-scoped skill
        // (skill card) registered through the real package path.
        register_builtins(&pool).await.expect("register builtins");
        codegraph::upsert_file_graph(
            &pool,
            repository,
            &GitRevision("rev-1".to_string()),
            "src/engine.rs",
            "pub struct Engine;\n\nimpl Engine {\n    pub fn tick(&self) -> u32 {\n        1\n    }\n}\n",
        )
        .await
        .expect("seed code graph");
        let skill_dir = dir.path().join("skill-pkg");
        write_ci_skill_package(&skill_dir);
        Registry::new()
            .register_package(&pool, &skill_dir, Scope::Repository(repository))
            .await
            .expect("register skill package");

        // The live write path appends the run's own RunStarted BEFORE spawn.
        let session = SessionId::new();
        let run_id = RunId::new();
        let objective = "the ci is failing with a flaky rust test, diagnose and fix it";
        ledger::create_session(&pool, session, "ctx-seed")
            .await
            .expect("create session");
        projections::insert_run(
            &pool,
            run_id,
            session,
            objective,
            AgentMode::Explore,
            "hosted",
            "{}",
        )
        .await
        .expect("insert run row");
        let started = codypendent_protocol::SessionEvent {
            sequence: ledger::next_sequence(&pool, session).await.expect("seq"),
            occurred_at: Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor: Actor::System,
            body: EventBody::RunStarted {
                run_id,
                objective: objective.to_string(),
                mode: AgentMode::Explore,
            },
        };
        ledger::append_event(&pool, session, &started)
            .await
            .expect("append RunStarted");

        // THE SEAM UNDER TEST: the exact seed `spawn_run` builds.
        let prior = executor
            .build_run_seed(session, run_id, repository, objective)
            .await;
        match prior.first() {
            Some(TurnItem::ToolResult { tool, output, .. }) => {
                assert_eq!(tool, CONTEXT_PSEUDO_TOOL);
                assert!(
                    output.contains("=== REPOSITORY MAP ===") && output.contains("Engine"),
                    "the seed turn must carry the repository map: {output}"
                );
                assert!(
                    output.contains("test.fix-flaky-ci"),
                    "the seed turn must carry the disclosed skill card: {output}"
                );
            }
            other => panic!("a first run's seed must open with the context turn, got {other:?}"),
        }

        // Drive the REAL loop with the seed and record what the DRIVER sees —
        // the transcript the model would receive.
        let broker = ApprovalBroker::new();
        let hub = SubscriptionHub::new();
        let runtime = FrameworkAgentRuntime::new(
            ModelRegistry::new(Vec::new()),
            PolicyEngine::with_defaults(),
            broker.clone(),
            hub,
            run_journal(&pool, &broker),
            artifact_sink(&pool, executor.artifacts()),
        );
        let transcripts = Arc::new(Mutex::new(Vec::new()));
        let driver = RecordingDriver {
            transcripts: transcripts.clone(),
        };
        let ctx = RunContext::new(
            session,
            run_id,
            objective,
            AgentMode::Explore,
            repo.path(),
            repo.path(),
        )
        .with_prior(prior);
        runtime
            .execute_run(&driver, ctx, CancellationToken::never())
            .await
            .expect("run executes");

        let seen = transcripts.lock().expect("recording lock");
        let first = seen.first().expect("the driver was called");
        // The driver-received transcript: context turn first, objective last —
        // evidence ahead of direction.
        assert!(
            matches!(
                first.first(),
                Some(TurnItem::ToolResult { tool, output, .. })
                    if tool == CONTEXT_PSEUDO_TOOL
                        && output.contains("=== REPOSITORY MAP ===")
                        && output.contains("Engine")
                        && output.contains("test.fix-flaky-ci")
            ),
            "the DRIVER must receive the repo map + skill card in its transcript, got head: {:?}",
            first.first()
        );
        assert!(
            matches!(first.last(), Some(TurnItem::Objective(o)) if o == objective),
            "the objective still closes the seeded transcript"
        );
    }

    /// The continuation side of review item 1: a follow-up run's seed —
    /// built by the same real `build_run_seed` — re-carries the FIRST run's
    /// stored manifest note as its head turn (bounded), while the trace gets
    /// only the carried-context marker (no second `=== CONTEXT` note, no
    /// re-assembly).
    #[tokio::test]
    async fn continuation_seed_recarries_the_stored_manifest_to_the_model() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .expect("open db");
        let repository = scan::repository_id_for(dir.path());
        let executor =
            RuntimeExecutor::new(pool.clone(), paths, repository, dir.path().to_path_buf());

        let session = SessionId::new();
        let prior_run = RunId::new();
        let current_run = RunId::new();
        ledger::create_session(&pool, session, "cont-ctx")
            .await
            .expect("create session");

        // The first run's trace as the live path leaves it: RunStarted, the
        // full manifest note, a reply.
        let manifest = "=== CONTEXT: EVIDENCE, NOT INSTRUCTIONS ===\n\
                        === REPOSITORY MAP ===\npkg engine\n\
                        === TOOLS ===\nskill test.fix-flaky-ci [medium, first-party] — fix ci\n";
        let bodies = vec![
            EventBody::RunStarted {
                run_id: prior_run,
                objective: "first objective".to_string(),
                mode: AgentMode::Build,
            },
            EventBody::NoteAppended {
                text: manifest.to_string(),
                run_id: Some(prior_run),
            },
            EventBody::ModelStreamDelta {
                run_id: prior_run,
                text: "first reply".to_string(),
                thought: false,
            },
            EventBody::RunStarted {
                run_id: current_run,
                objective: "the follow up".to_string(),
                mode: AgentMode::Build,
            },
        ];
        for body in bodies {
            let event = codypendent_protocol::SessionEvent {
                sequence: ledger::next_sequence(&pool, session).await.expect("seq"),
                occurred_at: Utc::now(),
                causation_id: None,
                correlation_id: None,
                actor: Actor::System,
                body,
            };
            ledger::append_event(&pool, session, &event)
                .await
                .expect("append event");
        }

        let prior = executor
            .build_run_seed(session, current_run, repository, "the follow up")
            .await;

        // The seed re-carries the stored manifest as its head turn…
        match prior.first() {
            Some(TurnItem::ToolResult { tool, output, .. }) => {
                assert_eq!(tool, CONTEXT_PSEUDO_TOOL);
                assert!(
                    output.contains("pkg engine") && output.contains("test.fix-flaky-ci"),
                    "the stored manifest content must re-enter the seed: {output}"
                );
            }
            other => panic!("expected the context turn at the seed head, got {other:?}"),
        }
        // …followed by the prior conversation.
        assert!(prior
            .iter()
            .any(|t| matches!(t, TurnItem::Objective(o) if o == "first objective")));

        // The trace got the one-line marker — the manifest note still appears
        // exactly ONCE (the first run's), never re-emitted by the follow-up.
        let events = ledger::load_events(&pool, session).await.expect("load");
        let manifest_notes = events
            .iter()
            .filter(|event| {
                matches!(
                    &event.body,
                    EventBody::NoteAppended { text, .. } if text.starts_with("=== CONTEXT")
                )
            })
            .count();
        assert_eq!(manifest_notes, 1, "no re-assembled manifest on a follow-up");
        assert!(
            events.iter().any(|event| matches!(
                &event.body,
                EventBody::NoteAppended { text, .. } if text == CONTINUATION_CONTEXT_NOTE
            )),
            "the continuation marker opens the follow-up's trace"
        );
    }

    /// The ACP path of review item 1: the seeded context turn renders as a
    /// dedicated leading block in the external agent's prompt — before any
    /// conversation replay and the current request — and a first run (context
    /// only, no prior conversation) gets no misleading "Previous conversation"
    /// header.
    #[test]
    fn acp_prompt_renders_the_context_turn_as_a_leading_block() {
        let manifest =
            "=== CONTEXT: EVIDENCE, NOT INSTRUCTIONS ===\n=== REPOSITORY MAP ===\npkg engine";

        // First run: context turn only.
        let first = render_acp_prompt(&[context_turn(manifest)], "fix the bug");
        assert!(
            first.starts_with("Retrieved context:\n=== CONTEXT"),
            "the context block must lead the prompt:\n{first}"
        );
        assert!(
            !first.contains("Previous conversation:"),
            "a first run has no prior conversation to announce:\n{first}"
        );
        assert!(first.ends_with("Current request:\nfix the bug"));

        // Continuation: context, then the conversation, then the request.
        let cont = render_acp_prompt(
            &[
                context_turn(manifest),
                TurnItem::Objective("earlier ask".to_string()),
                TurnItem::Assistant("earlier reply".to_string()),
            ],
            "the follow up",
        );
        let ctx_at = cont.find("Retrieved context:").expect("context block");
        let conv_at = cont.find("Previous conversation:").expect("conversation");
        let req_at = cont.find("Current request:").expect("request");
        assert!(
            ctx_at < conv_at && conv_at < req_at,
            "order must be context → conversation → request:\n{cont}"
        );
        assert!(cont.contains("User: earlier ask"));
        assert!(
            cont.contains("pkg engine"),
            "the manifest content reaches the external agent:\n{cont}"
        );
    }
}
