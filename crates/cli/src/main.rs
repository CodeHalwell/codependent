//! `codypendent` — CLI entry point.
//!
//! Phase 0 surface:
//!
//! ```text
//! codypendent daemon start
//! codypendent daemon status [--json]
//! codypendent daemon stop
//! ```
//!
//! STEP 1.13 adds the headless JSONL client:
//!
//! ```text
//! codypendent run --objective "..." [--mode build] [--repo PATH] --jsonl
//! codypendent attach <SESSION_ID> [--from-sequence N] --events jsonl
//! ```
//!
//! STEP 1.12 makes the bare invocation open the interactive TUI:
//!
//! ```text
//! codypendent            # opens the TUI for the current repository's session
//! ```
//!
//! Phase 5 STEP 5.1 adds workflow-manifest validation:
//!
//! ```text
//! codypendent workflow validate path/to/workflow.yaml
//! ```
//!
//! Phase 6 STEP 6.1 adds plugin inspection and permission-diffing:
//!
//! ```text
//! codypendent plugin inspect path/to/plugin.toml
//! codypendent plugin diff installed.toml update.toml
//! ```

use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use codypendent_cli::{commands, theme_select, tui};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{AgentMode, DocumentId, SessionId};

#[derive(Parser)]
#[command(
    name = "codypendent",
    version,
    about = "Codypendent — the local-first agentic developer environment"
)]
struct Cli {
    /// Force a theme for the interactive TUI, overriding automatic terminal
    /// detection (`NO_COLOR`/`COLORTERM`/`TERM`) and any `CODYPENDENT_THEME`
    /// env var — a manual override always wins (STEP 6.6). Accepts a
    /// built-in variant (`dark`, `light`, `high-contrast`, `color-blind-safe`,
    /// `ansi256`, `ansi16`, `monochrome`) or the id of a theme pack loaded
    /// from `<data-dir>/themes/<id>.toml`. Only meaningful for the bare
    /// `codypendent` invocation, which is the only one that renders a
    /// themed UI.
    #[arg(long)]
    theme: Option<String>,
    /// Use the cooked, line-oriented accessible client for the bare invocation.
    /// No raw mode, alternate screen, mouse capture, colour, or Unicode chrome
    /// is emitted. `--plain` is an alias for scripts and limited terminals.
    #[arg(long, visible_alias = "plain")]
    accessible: bool,
    /// With no subcommand, `codypendent` opens the interactive TUI attached to
    /// the current repository's session (STEP 1.12).
    #[command(subcommand)]
    command: Option<TopCommand>,
}

#[derive(Subcommand)]
enum TopCommand {
    /// Run the daemon in-process. `codypendent __daemon` *is* the daemon — the
    /// hidden self-spawn target `ensure_daemon` launches as `current_exe
    /// __daemon`, so an updated `codypendent` always runs a matching daemon.
    /// Hidden from `--help`; behaves exactly like the standalone `codypendentd`.
    #[command(name = "__daemon", hide = true)]
    InternalDaemon,
    /// Manage the codypendentd daemon.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Start a headless run and stream its events (STEP 1.13).
    Run {
        /// What the agent should do.
        #[arg(long)]
        objective: String,
        /// The mode preset the run starts in (Chapter 20).
        #[arg(long, value_enum, default_value = "build")]
        mode: ModeArg,
        /// Repository the run operates in. Defaults to the current directory.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Stream every session event to stdout as JSONL until the run
        /// terminates. Currently required — interactive attach lands with
        /// the TUI (STEP 1.12).
        #[arg(long)]
        jsonl: bool,
    },
    /// Attach to an existing session and stream its events (STEP 1.13).
    Attach {
        /// The session to attach to.
        session_id: SessionId,
        /// The last sequence already seen: replay resumes at the *next* event
        /// (an exclusive cursor). Omit to replay the full retained history —
        /// or a snapshot — from the beginning of what the daemon still holds.
        #[arg(long = "from-sequence")]
        from_sequence: Option<u64>,
        /// Output format for the event stream. `jsonl` is the only format
        /// today; the flag exists so future formats are additive.
        #[arg(long, value_enum, default_value = "jsonl")]
        events: EventsFormat,
    },
    /// Maintain the knowledge fabric's derived RETRIEVAL indexes — full-text
    /// (BM25) + vectors (Phase 2). This is search, NOT the code graph: the
    /// code-graph nodes/edges are built per-repository when you open a session
    /// or start a run, not by this command.
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },
    /// Work with declarative workflow manifests (Phase 5).
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    /// Investigate and repair a failed GitHub check on a pull request (`/fix-ci`).
    /// Runs the declarative `repair-github-check` workflow — the supervised
    /// investigator → implementer → independent-reviewer flow — through the
    /// daemon; every GitHub write parks for approval (Phase 5 STEP 5.1.4).
    FixCi {
        /// The pull-request number whose failing check to repair.
        #[arg(long)]
        pr: u64,
        /// Repository the repair runs against (its agent nodes each get an
        /// isolated worktree). Defaults to the current directory.
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Publish a collaborative document to Git (Phase 4 STEP 4.4).
    Docs {
        #[command(subcommand)]
        command: DocsCommand,
    },
    /// Run the evaluation harness against a fixture suite (Phase 7 STEP 7.1).
    Eval {
        #[command(subcommand)]
        command: EvalCommand,
    },
    /// Measure and manage model profiles for the router (Phase 7 STEP 7.2).
    Models {
        #[command(subcommand)]
        command: ModelsCommand,
    },
    /// Drive a learnable artifact through the evaluation-gated promotion
    /// pipeline (Phase 7 STEP 7.5) — nothing promotes itself (ADR-010).
    Promote {
        #[command(subcommand)]
        command: PromoteCommand,
    },
    /// Inspect plugin manifests and their permissions (Phase 6).
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    /// Inspect the operator-declared MCP servers (PR B — MCP client).
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Connect external ACP agents, or expose Codypendent itself as an ACP agent.
    Acp {
        #[command(subcommand)]
        command: Option<AcpCommand>,
        /// Backward-compatible repository for `codypendent acp` (serve mode).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Create and run durable multi-provider agent councils.
    Council {
        #[command(subcommand)]
        command: CouncilCommand,
    },
    /// Hand a session off to an IDE (STEP 3.7): print how to attach, and launch
    /// the editor if it is on `PATH`. The IDE attaches as a contributor to the
    /// same session — the run keeps going, it never restarts.
    Open {
        /// The session to open in the IDE.
        session_id: SessionId,
        /// Which IDE to open the session in.
        #[arg(long = "in", value_enum, default_value = "vscode")]
        ide: IdeArg,
        /// Repository path to open. Defaults to the current directory.
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Print a shell-completion script for your shell. Install e.g. with:
    /// `codypendent completion zsh > ~/.zfunc/_codypendent` (zsh),
    /// `codypendent completion bash > /etc/bash_completion.d/codypendent`, or
    /// `codypendent completion fish > ~/.config/fish/completions/codypendent.fish`.
    Completion {
        /// The shell to generate completions for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Diagnose the local setup: binary + build id, daemon health, runtime
    /// paths, model config, and provider reachability. Read-only. Exits
    /// non-zero if any check FAILS.
    Doctor {
        /// Emit a structured JSON report instead of the human checklist.
        #[arg(long)]
        json: bool,
        /// Probe hosted providers too (a bare TCP connect to host:port), not
        /// just local model servers.
        #[arg(long)]
        deep: bool,
    },
    /// Self-update: install the latest GitHub release over the running binary
    /// (via `gh`, exactly as `install.sh`), then pick up the new build through
    /// the idle-guarded daemon auto-restart — never kills an active run.
    Update {
        /// Only report whether an update is available; download nothing.
        #[arg(long)]
        check: bool,
        /// Install a specific release tag instead of the latest.
        tag: Option<String>,
    },
}

/// The IDEs `codypendent open --in <IDE>` knows how to launch.
#[derive(Clone, Copy, ValueEnum)]
enum IdeArg {
    Vscode,
    Cursor,
    Zed,
}

impl IdeArg {
    /// The launcher binary and human name for this IDE.
    fn binary_and_name(self) -> (&'static str, &'static str) {
        match self {
            IdeArg::Vscode => ("code", "VS Code"),
            IdeArg::Cursor => ("cursor", "Cursor"),
            IdeArg::Zed => ("zed", "Zed"),
        }
    }
}

#[derive(Subcommand)]
enum IndexCommand {
    /// Delete the derived RETRIEVAL indexes (BM25 + vectors) and rebuild them
    /// from the authoritative rows. Does NOT rebuild the code graph — that is
    /// built per-repository on session open / first run.
    Rebuild,
}

#[derive(Subcommand)]
enum McpCommand {
    /// List the MCP servers declared in `<config_dir>/mcp.toml`: launch line,
    /// env key names (never values), and the effective policy disposition.
    /// Config-level only — no server is spawned.
    List,
}

#[derive(Subcommand)]
enum AcpCommand {
    /// Refresh the curated official ACP agent registry.
    Refresh,
    /// List every agent in the official registry.
    List {
        #[arg(long)]
        refresh: bool,
        #[arg(long)]
        json: bool,
    },
    /// Download/install a registry agent without adding a model profile.
    Install {
        /// Registry id or alias (for example claude-code, codex, kimi-code, amp, vibe-chat).
        agent: String,
        #[arg(long)]
        refresh: bool,
        /// Permit a curated binary URL whose registry entry has no SHA-256.
        #[arg(long)]
        allow_unverified: bool,
    },
    /// Install, handshake, and add an ACP agent to the model picker.
    Connect {
        /// Registry id or alias (for example claude-code, codex, kimi-code, amp, vibe-chat).
        agent: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        refresh: bool,
        #[arg(long)]
        allow_unverified: bool,
        /// Repository used for the handshake smoke test.
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Send one real, tool-denied prompt without saving a model profile.
    Probe {
        /// Registry id or alias (for example claude-code, codex, kimi-code, amp, vibe-chat).
        agent: String,
        /// Harmless prompt used to verify a live vendor response.
        #[arg(
            long,
            default_value = "Reply with exactly: ACP LIVE OK. Do not inspect files or use tools."
        )]
        prompt: String,
        #[arg(long)]
        refresh: bool,
        #[arg(long)]
        allow_unverified: bool,
        /// Repository passed as the ACP session working directory.
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Remove an ACP model profile (downloaded agent bytes remain cached).
    Disconnect { profile: String },
    /// Show readiness of configured ACP profiles.
    Status,
    /// Expose Codypendent as an ACP agent over stdio for Zed and other clients.
    Serve {
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum CouncilCommand {
    /// Persist a named council assembled from configured model profiles.
    Create {
        /// Stable council name (`A-Za-z0-9._-`).
        name: String,
        /// Member model and role, repeated: `--member MODEL=ROLE`.
        #[arg(long, required = true)]
        member: Vec<String>,
        /// Configured model profile that synthesizes the council result.
        #[arg(long)]
        chair: String,
        /// Deliberation rounds (1-3). Later rounds critique the prior dossier.
        #[arg(long, default_value_t = 1)]
        rounds: u8,
        /// Human-readable purpose for this council.
        #[arg(long)]
        description: Option<String>,
    },
    /// List configured councils.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show one council's exact members, roles, chair, and rounds.
    Show {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Remove a council definition. Its prior durable sessions remain.
    Remove { name: String },
    /// Run member deliberation in parallel, then ask the chair to synthesize.
    Run {
        name: String,
        /// Question or decision the council should deliberate.
        #[arg(long)]
        objective: String,
        /// Repository context. Defaults to the current directory.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Emit the complete attributed result as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum WorkflowCommand {
    /// Parse and compile a `workflow.yaml`, reporting the validated graph or the
    /// precise error. Structural validation only; it does not run the workflow.
    /// With `--agents`, it additionally cross-checks that every agent step's role
    /// resolves to a profile in that directory.
    Validate {
        /// Path to the workflow manifest to validate.
        file: PathBuf,
        /// Optional directory of `agent.toml` profiles to resolve step roles
        /// against (e.g. `.codypendent/agents`). When given, an agent step naming
        /// a role no profile fulfils is reported as an error.
        #[arg(long)]
        agents: Option<PathBuf>,
    },
    /// Compile a `workflow.yaml` and print its full graph (nodes, actions, edges,
    /// approvals, outputs) as a human tree, or the JSON projection with `--json`.
    Show {
        /// Path to the workflow manifest to show.
        file: PathBuf,
        /// Emit the compiled graph as JSON instead of a human tree.
        #[arg(long)]
        json: bool,
    },
    /// Start a durable workflow run from a manifest (Phase 5 STEP 5.2). Ensures a
    /// daemon, sends the manifest, and prints the new run id the daemon drives to a
    /// terminal state in the background.
    Run {
        /// Path to the workflow manifest to run.
        file: PathBuf,
        /// The typed inputs the manifest declares, as a JSON value (e.g.
        /// '{"pull_request": 7}'). Defaults to null.
        #[arg(long)]
        inputs: Option<String>,
        /// Repository the workflow's agent nodes operate on (each writing node is
        /// carved its own isolated worktree from it). Defaults to the current
        /// directory.
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Pause a running workflow run so its driver stops launching new nodes; resume
    /// it later with `workflow resume` (Phase 5 STEP 5.2).
    Pause {
        /// The durable workflow-run id (as printed by `workflow run`).
        workflow_run_id: String,
    },
    /// Resume a paused workflow run, driving it onward from where it stopped.
    Resume {
        /// The durable workflow-run id.
        workflow_run_id: String,
    },
    /// Re-drive a workflow run from a chosen node (its transitive dependents reset
    /// with it) — e.g. after fixing what made the node fail.
    Retry {
        /// The durable workflow-run id.
        workflow_run_id: String,
        /// The node id to re-drive from.
        #[arg(long)]
        node: String,
    },
    /// Cancel a workflow run (Phase 5 T9): a cooperative drain — the driver stops
    /// launching new nodes, any in-flight node's agent run is interrupted, remaining
    /// pending nodes are skipped, and the run lands cancelled (terminal — no resume).
    Cancel {
        /// The durable workflow-run id.
        workflow_run_id: String,
    },
    /// Watch a workflow run's live node lifecycle (Phase 5 T9): prints the run's
    /// current snapshot (each node's state, cost, and any failure/block reason), then
    /// streams each node transition and run-phase change until the run reaches a
    /// terminal state (or the stream is interrupted).
    Watch {
        /// The durable workflow-run id.
        workflow_run_id: String,
    },
}

#[derive(Subcommand)]
enum DocsCommand {
    /// Publish a document's current revision to a Git target (Phase 4 STEP
    /// 4.4). Prints the computed plan (target / changed files / resulting Git
    /// action) and prompts for confirmation, then sends `PublishDocument`;
    /// ensures a daemon, parks a durable approval, and resolves it with the
    /// confirmed decision. Nothing is written until the approval resolves —
    /// on approval, the daemon executes the plan in the background and this
    /// prints the resulting commit once recorded.
    Publish {
        /// The document to publish.
        document: DocumentId,
        /// Where to publish it.
        #[arg(long, value_enum)]
        target: PublishTargetArg,
        /// The repo-relative path to write. Defaults to a slug of the
        /// document's title under `docs/`.
        #[arg(long)]
        path: Option<String>,
        /// The docs branch (`docs-branch`/`doc-pr` targets only). Defaults to
        /// `docs/publish`.
        #[arg(long)]
        branch: Option<String>,
        /// The pull-request title (`doc-pr` target only). Defaults to
        /// `Publish: <document title>`.
        #[arg(long)]
        title: Option<String>,
        /// Skip the confirmation prompt and approve immediately.
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

/// `codypendent docs publish --target <TARGET>`: which Git target STEP 4.4.2
/// describes. Mirrors `codypendent_knowledge::PublishTarget`'s three variants
/// with CLI-friendly names.
#[derive(Clone, Copy, ValueEnum)]
enum PublishTargetArg {
    /// Write the rendered Markdown to a repository file in the working tree.
    RepoFile,
    /// Commit the rendered Markdown to a dedicated docs branch.
    DocsBranch,
    /// Open a documentation pull request via the GitHub write path.
    DocPr,
}

impl From<PublishTargetArg> for commands::PublishTargetKind {
    fn from(arg: PublishTargetArg) -> Self {
        match arg {
            PublishTargetArg::RepoFile => commands::PublishTargetKind::RepoFile,
            PublishTargetArg::DocsBranch => commands::PublishTargetKind::DocsBranch,
            PublishTargetArg::DocPr => commands::PublishTargetKind::DocPr,
        }
    }
}

#[derive(Subcommand)]
enum EvalCommand {
    /// Execute an `evals/tasks/` suite headlessly over the JSONL client and
    /// write a `SuiteReport` (Phase 7 STEP 7.1). Ensures a daemon; each case
    /// starts its own run against its pinned fixture repository.
    Run {
        /// The suite directory under `evals/tasks/` (e.g. `core` for
        /// `evals/tasks/core/`), or a path to it directly.
        #[arg(long, default_value = "core")]
        suite: String,
        /// The routing policy to select each case's model under (Phase 7's
        /// "routing⇄eval composition"). Resolved via `codypendent-routing`
        /// over the persisted model profiles, fail-closed: an unknown name or
        /// a case with no eligible model stops `eval run` before any case
        /// executes. The selection is recorded per case in the report
        /// (`routed_model`); it does not yet pin the daemon's own `StartRun`
        /// execution to that model (see `codypendent_cli::eval`'s module
        /// doc). Absent: every case runs under the daemon's own default model
        /// resolution, unchanged.
        #[arg(long)]
        policy: Option<String>,
        /// Bind this run's durable regression evidence to an existing promotion
        /// candidate. Without this option the report is still written, but it
        /// cannot advance any candidate's regression gate.
        #[arg(long)]
        candidate_id: Option<String>,
        /// Where to write the `SuiteReport` JSON.
        #[arg(long)]
        report: PathBuf,
    },
}

#[derive(Subcommand)]
enum ModelsCommand {
    /// Benchmark a local model configured in `models.toml` and persist its
    /// measured profile (Phase 7 STEP 7.2.2): tokens/sec, time-to-first-token,
    /// warm-up, memory, context limit, structured-output reliability, tool-call
    /// accuracy, and a small coding-eval score. The router reads these MEASURED
    /// numbers (never vibes). Also caches the first-use capability probe.
    Bench {
        /// The `models.toml` model id to benchmark (its `base_url` is the
        /// endpoint the profile + probe are keyed under).
        id: String,
    },
    /// Pull a GGUF model from the Unsloth catalog on Hugging Face and register
    /// it against the `ollama` provider. Resolves `<hf-repo>[:<quant>]`
    /// (defaulting a bare repo name to the `unsloth/` org and, with no
    /// `:quant`, an auto-picked default), drives `ollama pull
    /// hf.co/<org>/<repo>:<quant>` with streamed progress, then writes
    /// `models.toml` using the exact reference `ollama list` shows. Requires
    /// `ollama` on `PATH` (see https://ollama.com); this command never
    /// downloads or runs model weights itself.
    Pull {
        /// `<hf-repo>[:<quant>]`, e.g. `Qwen3-32B-GGUF`,
        /// `Qwen3-32B-GGUF:UD-Q4_K_XL`, or `some-org/Some-Model-GGUF:Q8_0`.
        spec: String,
    },
}

#[derive(Subcommand)]
enum PromoteCommand {
    /// Draft a candidate for the promotion pipeline (Phase 7 STEP 7.5). Prints
    /// the new candidate id.
    Propose {
        /// The artifact kind: `retrieval`, `skill`, `prompt`, `router`,
        /// `workflow`, or `model-profile`.
        #[arg(long)]
        kind: String,
        /// The artifact's name (e.g. `tool-selection`).
        #[arg(long)]
        name: String,
        /// The version being proposed.
        #[arg(long)]
        version: u32,
        /// Mark this candidate as synthesized (e.g. by a skill-clustering
        /// pipeline): it must pass permission review before it may enter
        /// evaluation.
        #[arg(long)]
        requires_permission_review: bool,
    },
    /// Advance a candidate through permission review, stored regression
    /// evidence, shadow traffic, and measured canary evidence. Regression
    /// verdicts are computed from the latest durable eval report; canary
    /// verdicts are derived by the daemon from the supplied raw metrics.
    Advance {
        /// The candidate id (as printed by `promote propose`).
        candidate_id: String,
        /// Which transition to attempt.
        #[arg(long, value_enum)]
        step: PromoteStepArg,
        /// Number of requests represented by an `observe-canary` sample.
        #[arg(long)]
        sample_count: Option<u64>,
        /// Current canary error rate, in basis points (0..=10000).
        #[arg(long)]
        error_rate_bps: Option<u16>,
        /// Baseline error rate, in basis points (0..=10000).
        #[arg(long)]
        baseline_error_rate_bps: Option<u16>,
        /// Current canary p95 latency in milliseconds.
        #[arg(long)]
        p95_latency_ms: Option<u64>,
        /// Baseline p95 latency in milliseconds.
        #[arg(long)]
        baseline_p95_latency_ms: Option<u64>,
    },
    /// **Approve and promote a candidate** (ADR-010: requires the `Controller`
    /// role, which this local-first socket maps to a human operator — an
    /// agent/system-initiated approval is refused structurally).
    Approve { candidate_id: String },
    /// Manually roll back a promoted candidate to its predecessor version.
    Rollback { candidate_id: String },
}

/// `codypendent promote advance --step <STEP>`.
#[derive(Clone, Copy, ValueEnum)]
enum PromoteStepArg {
    ReviewPermissions,
    Regression,
    Shadow,
    Canary,
    ObserveCanary,
    FinishCanary,
}

impl PromoteStepArg {
    fn into_wire(
        self,
        sample_count: Option<u64>,
        error_rate_bps: Option<u16>,
        baseline_error_rate_bps: Option<u16>,
        p95_latency_ms: Option<u64>,
        baseline_p95_latency_ms: Option<u64>,
    ) -> anyhow::Result<codypendent_protocol::PromotionAction> {
        use codypendent_protocol::{CanaryMetrics, PromotionAction};
        Ok(match self {
            PromoteStepArg::ReviewPermissions => PromotionAction::ReviewPermissions,
            PromoteStepArg::Regression => PromotionAction::RunRegression,
            PromoteStepArg::Shadow => PromotionAction::StartShadow,
            PromoteStepArg::Canary => PromotionAction::StartCanary,
            PromoteStepArg::ObserveCanary => PromotionAction::ObserveCanary {
                metrics: CanaryMetrics {
                    sample_count: sample_count.ok_or_else(|| {
                        anyhow::anyhow!("--sample-count is required for observe-canary")
                    })?,
                    error_rate_bps: error_rate_bps.ok_or_else(|| {
                        anyhow::anyhow!("--error-rate-bps is required for observe-canary")
                    })?,
                    baseline_error_rate_bps: baseline_error_rate_bps.ok_or_else(|| {
                        anyhow::anyhow!("--baseline-error-rate-bps is required for observe-canary")
                    })?,
                    p95_latency_ms: p95_latency_ms.ok_or_else(|| {
                        anyhow::anyhow!("--p95-latency-ms is required for observe-canary")
                    })?,
                    baseline_p95_latency_ms: baseline_p95_latency_ms.ok_or_else(|| {
                        anyhow::anyhow!("--baseline-p95-latency-ms is required for observe-canary")
                    })?,
                },
            },
            PromoteStepArg::FinishCanary => PromotionAction::FinishCanary,
        })
    }
}

#[derive(Subcommand)]
enum PluginCommand {
    /// Verify and install a Remote UI package disabled in the daemon-owned
    /// content-addressed plugin store.
    Install {
        manifest: PathBuf,
        artifact: PathBuf,
        #[arg(long)]
        allow_unsigned: bool,
    },
    /// Start the installed worker in the real sandbox, negotiate, then stop.
    SmokeTest { id: String },
    /// Enable a smoke-tested plugin. Non-user scopes require --session.
    Enable {
        id: String,
        #[arg(long, default_value = "user")]
        scope: String,
        #[arg(long)]
        session: Option<SessionId>,
    },
    /// List durable plugin lifecycle and pending approval state.
    List,
    /// Verify and apply/stage a package update.
    Update {
        id: String,
        manifest: PathBuf,
        artifact: PathBuf,
        #[arg(long)]
        allow_unsigned: bool,
    },
    /// Approve the exact sealed update candidate shown by `plugin update`.
    ApproveUpdate { id: String, receipt: String },
    /// Reject a sealed update candidate and restore the previous state.
    RejectUpdate { id: String, receipt: String },
    /// Revoke a plugin and stop all of its workers.
    Revoke { id: String },
    /// Parse a `plugin.toml` and render its identity, the capability list it
    /// requests, its resource caps, and its trust posture (signed? sandbox
    /// profile) — the "evaluate permissions" step a user sees before enabling a
    /// plugin (Phase 6 STEP 6.1). Manifest parsing only; it does not run anything.
    Inspect {
        /// Path to the plugin manifest to inspect.
        file: PathBuf,
    },
    /// Compare an installed `plugin.toml` against an update and print the
    /// permission diff, reporting whether the update expands permissions and so
    /// requires re-approval (Phase 6 STEP 6.1, exit criterion 2).
    Diff {
        /// The currently-installed manifest.
        installed: PathBuf,
        /// The candidate update manifest.
        update: PathBuf,
    },
    /// Verify a plugin artifact against its manifest using the trusted-publisher
    /// key store — the real-keys install gate (Phase 6 STEP 6.2). A signed plugin
    /// from an unknown publisher, a bad signature, or an unsigned plugin (unless
    /// `--allow-unsigned`) is refused with a non-zero exit (fails closed).
    Verify {
        /// The plugin manifest (`plugin.toml`).
        manifest: PathBuf,
        /// The plugin artifact whose checksum/signature is verified.
        artifact: PathBuf,
        /// Permit an unsigned (checksum-only) plugin. Default posture denies it.
        #[arg(long)]
        allow_unsigned: bool,
    },
    /// Manage the trusted-publisher key store (Phase 6 STEP 6.2): the ed25519
    /// public keys `plugin verify` checks signatures against.
    Trust {
        #[command(subcommand)]
        command: TrustCommand,
    },
}

#[derive(Subcommand)]
enum TrustCommand {
    /// Trust a publisher: record its base64 ed25519 public key.
    Add {
        /// The publisher id (matched against a manifest's `publisher`).
        id: String,
        /// The publisher's ed25519 public key, base64-encoded (32 raw bytes).
        public_key: String,
    },
    /// List the trusted publishers and their public keys.
    List,
    /// Stop trusting a publisher, revoke/stop its signed UI plugins, and require
    /// a newly verified reinstall before the same ids can run under a new key.
    Remove {
        /// The publisher id to remove.
        id: String,
    },
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Start the daemon if it is not already running.
    Start,
    /// Ask a running daemon to shut down gracefully.
    Stop,
    /// Show daemon status. Exit code 0 when running, 1 when not.
    Status {
        /// Print machine-readable JSON instead of human text.
        #[arg(long)]
        json: bool,
    },
    /// Restart only if the daemon confirms no session or workflow run is active.
    /// If no daemon is running this simply starts one.
    Restart,
}

/// CLI-facing mirror of [`AgentMode`] so `clap` can derive `--mode`'s parser
/// and `--help` text without teaching the wire protocol crate about `clap`.
#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    Ask,
    Explore,
    Plan,
    Build,
    Review,
}

impl From<ModeArg> for AgentMode {
    fn from(mode: ModeArg) -> Self {
        match mode {
            ModeArg::Ask => AgentMode::Ask,
            ModeArg::Explore => AgentMode::Explore,
            ModeArg::Plan => AgentMode::Plan,
            ModeArg::Build => AgentMode::Build,
            ModeArg::Review => AgentMode::Review,
        }
    }
}

/// `codypendent attach --events <FORMAT>`. Only `jsonl` exists today; a
/// dedicated enum keeps room for future formats without a breaking CLI change.
#[derive(Clone, Copy, ValueEnum)]
enum EventsFormat {
    Jsonl,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // `codypendent __daemon` *is* the daemon (the hidden self-spawn target of
    // `ensure_daemon`). Dispatch it before any TUI/theme setup so it behaves
    // exactly like the standalone `codypendentd` binary: init the daemon's
    // tracing, resolve paths, run the loop to shutdown.
    if matches!(cli.command, Some(TopCommand::InternalDaemon)) {
        codypendent_codypendentd::init_tracing();
        let paths = RuntimePaths::resolve()?;
        paths.ensure_directories()?;
        return codypendent_codypendentd::run_daemon(paths).await;
    }

    let paths = RuntimePaths::resolve()?;
    // `--theme` wins over `CODYPENDENT_THEME`; an empty value from either
    // source falls through to the other (see `theme_select::resolve_theme_override`
    // for why each source must be filtered before combining them).
    let theme_override =
        theme_select::resolve_theme_override(cli.theme, std::env::var("CODYPENDENT_THEME").ok());
    let Some(command) = cli.command else {
        // Bare `codypendent`: open the TUI for the current directory's repo.
        return tui::run(
            &paths,
            std::env::current_dir()?,
            theme_override,
            cli.accessible,
        )
        .await;
    };
    match command {
        // Dispatched before the match (see the early return in `main`); a
        // parsed `InternalDaemon` never reaches here.
        TopCommand::InternalDaemon => unreachable!("__daemon is dispatched before the match"),
        TopCommand::Daemon { command } => match command {
            DaemonCommand::Start => commands::start(&paths).await,
            DaemonCommand::Stop => commands::stop(&paths).await,
            DaemonCommand::Status { json } => {
                // `status` returns the running-state; the exit-1-when-not-running
                // decision lives here (the only place `std::process::exit` runs).
                let running = commands::status(&paths, json).await?;
                if running {
                    Ok(())
                } else {
                    std::process::exit(1);
                }
            }
            DaemonCommand::Restart => commands::restart(&paths).await,
        },
        TopCommand::Run {
            objective,
            mode,
            repo,
            jsonl,
        } => {
            let repo = match repo {
                Some(repo) => repo,
                None => std::env::current_dir()?,
            };
            let exit_code = commands::run(&paths, objective, mode.into(), repo, jsonl).await?;
            std::process::exit(exit_code);
        }
        TopCommand::Attach {
            session_id,
            from_sequence,
            events: EventsFormat::Jsonl,
        } => commands::attach(&paths, session_id, from_sequence).await,
        TopCommand::Index {
            command: IndexCommand::Rebuild,
        } => commands::index_rebuild(&paths).await,
        TopCommand::Workflow { command } => match command {
            WorkflowCommand::Validate { file, agents } => {
                commands::workflow_validate(&file, agents.as_deref())
            }
            WorkflowCommand::Show { file, json } => commands::workflow_show(&file, json),
            WorkflowCommand::Run { file, inputs, repo } => {
                let repo = match repo {
                    Some(repo) => repo,
                    None => std::env::current_dir()?,
                };
                commands::workflow_run(&paths, &file, inputs, repo).await
            }
            WorkflowCommand::Pause { workflow_run_id } => {
                commands::workflow_pause(&paths, workflow_run_id).await
            }
            WorkflowCommand::Resume { workflow_run_id } => {
                commands::workflow_resume(&paths, workflow_run_id).await
            }
            WorkflowCommand::Retry {
                workflow_run_id,
                node,
            } => commands::workflow_retry(&paths, workflow_run_id, node).await,
            WorkflowCommand::Cancel { workflow_run_id } => {
                commands::workflow_cancel(&paths, workflow_run_id).await
            }
            WorkflowCommand::Watch { workflow_run_id } => {
                commands::workflow_watch(&paths, workflow_run_id).await
            }
        },
        TopCommand::FixCi { pr, repo } => {
            let repo = match repo {
                Some(repo) => repo,
                None => std::env::current_dir()?,
            };
            commands::fix_ci(&paths, pr, repo).await
        }
        TopCommand::Docs { command } => match command {
            DocsCommand::Publish {
                document,
                target,
                path,
                branch,
                title,
                yes,
            } => {
                commands::docs_publish(&paths, document, target.into(), path, branch, title, yes)
                    .await
            }
        },
        TopCommand::Eval { command } => match command {
            EvalCommand::Run {
                suite,
                policy,
                candidate_id,
                report,
            } => commands::eval_run(&paths, &suite, policy, candidate_id.as_deref(), &report).await,
        },
        TopCommand::Models { command } => match command {
            ModelsCommand::Bench { id } => commands::models_bench(&paths, &id).await,
            ModelsCommand::Pull { spec } => {
                let hf = codypendent_integrations::unsloth::HfHubClient::hub()?;
                codypendent_cli::models_pull::run(
                    &paths,
                    &spec,
                    &hf,
                    codypendent_cli::models_pull::OLLAMA_BIN,
                )
                .await
            }
        },
        TopCommand::Promote { command } => match command {
            PromoteCommand::Propose {
                kind,
                name,
                version,
                requires_permission_review,
            } => {
                commands::promote_propose(&paths, kind, name, version, requires_permission_review)
                    .await
            }
            PromoteCommand::Advance {
                candidate_id,
                step,
                sample_count,
                error_rate_bps,
                baseline_error_rate_bps,
                p95_latency_ms,
                baseline_p95_latency_ms,
            } => {
                let action = step.into_wire(
                    sample_count,
                    error_rate_bps,
                    baseline_error_rate_bps,
                    p95_latency_ms,
                    baseline_p95_latency_ms,
                )?;
                commands::promote_advance(&paths, candidate_id, action).await
            }
            PromoteCommand::Approve { candidate_id } => {
                commands::promote_approve(&paths, candidate_id).await
            }
            PromoteCommand::Rollback { candidate_id } => {
                commands::promote_rollback(&paths, candidate_id).await
            }
        },
        TopCommand::Plugin { command } => match command {
            PluginCommand::Install {
                manifest,
                artifact,
                allow_unsigned,
            } => commands::plugin_install(&paths, &manifest, &artifact, allow_unsigned).await,
            PluginCommand::SmokeTest { id } => commands::plugin_smoke_test(&paths, id).await,
            PluginCommand::Enable { id, scope, session } => {
                commands::plugin_enable(&paths, id, scope, session).await
            }
            PluginCommand::List => commands::plugin_list(&paths).await,
            PluginCommand::Update {
                id,
                manifest,
                artifact,
                allow_unsigned,
            } => commands::plugin_update(&paths, id, &manifest, &artifact, allow_unsigned).await,
            PluginCommand::ApproveUpdate { id, receipt } => {
                commands::plugin_approve_update(&paths, id, receipt).await
            }
            PluginCommand::RejectUpdate { id, receipt } => {
                commands::plugin_reject_update(&paths, id, receipt).await
            }
            PluginCommand::Revoke { id } => commands::plugin_revoke(&paths, id).await,
            PluginCommand::Inspect { file } => commands::plugin_inspect(&file),
            PluginCommand::Diff { installed, update } => commands::plugin_diff(&installed, &update),
            PluginCommand::Verify {
                manifest,
                artifact,
                allow_unsigned,
            } => commands::plugin_verify(&manifest, &artifact, allow_unsigned),
            PluginCommand::Trust { command } => match command {
                TrustCommand::Add { id, public_key } => {
                    commands::plugin_trust_add(&id, &public_key)
                }
                TrustCommand::List => commands::plugin_trust_list(),
                TrustCommand::Remove { id } => commands::plugin_trust_remove(&paths, &id).await,
            },
        },
        TopCommand::Mcp {
            command: McpCommand::List,
        } => commands::mcp_list(&paths).await,
        TopCommand::Acp { command, repo } => match command {
            None => {
                let repo = repo.unwrap_or(std::env::current_dir()?);
                codypendent_cli::acp::serve(&paths, repo).await
            }
            Some(AcpCommand::Serve { repo: serve_repo }) => {
                let repo = serve_repo.or(repo).unwrap_or(std::env::current_dir()?);
                codypendent_cli::acp::serve(&paths, repo).await
            }
            Some(AcpCommand::Refresh) => codypendent_cli::acp_clients::refresh(&paths).await,
            Some(AcpCommand::List { refresh, json }) => {
                codypendent_cli::acp_clients::list(&paths, refresh, json).await
            }
            Some(AcpCommand::Install {
                agent,
                refresh,
                allow_unverified,
            }) => {
                codypendent_cli::acp_clients::install(&paths, &agent, refresh, allow_unverified)
                    .await
            }
            Some(AcpCommand::Connect {
                agent,
                profile,
                refresh,
                allow_unverified,
                repo: connect_repo,
            }) => {
                let repository = connect_repo.or(repo).unwrap_or(std::env::current_dir()?);
                codypendent_cli::acp_clients::connect(
                    &paths,
                    &agent,
                    profile.as_deref(),
                    refresh,
                    allow_unverified,
                    &repository,
                )
                .await
            }
            Some(AcpCommand::Probe {
                agent,
                prompt,
                refresh,
                allow_unverified,
                repo: probe_repo,
            }) => {
                let repository = probe_repo.or(repo).unwrap_or(std::env::current_dir()?);
                codypendent_cli::acp_clients::probe(
                    &paths,
                    &agent,
                    &prompt,
                    refresh,
                    allow_unverified,
                    &repository,
                )
                .await
            }
            Some(AcpCommand::Disconnect { profile }) => {
                codypendent_cli::acp_clients::disconnect(&paths, &profile)
            }
            Some(AcpCommand::Status) => codypendent_cli::acp_clients::status(&paths).await,
        },
        TopCommand::Council { command } => match command {
            CouncilCommand::Create {
                name,
                member,
                chair,
                rounds,
                description,
            } => codypendent_cli::council::create(&paths, name, member, chair, rounds, description),
            CouncilCommand::List { json } => codypendent_cli::council::list(&paths, json),
            CouncilCommand::Show { name, json } => {
                codypendent_cli::council::show(&paths, &name, json)
            }
            CouncilCommand::Remove { name } => codypendent_cli::council::remove(&paths, &name),
            CouncilCommand::Run {
                name,
                objective,
                repo,
                json,
            } => {
                let repository = repo.unwrap_or(std::env::current_dir()?);
                codypendent_cli::council::run(&paths, &name, objective, repository, json).await
            }
        },
        TopCommand::Open {
            session_id,
            ide,
            repo,
        } => {
            let repo = match repo {
                Some(repo) => repo,
                None => std::env::current_dir()?,
            };
            let (binary, name) = ide.binary_and_name();
            commands::open(&paths, session_id, binary, name, repo).await
        }
        TopCommand::Completion { shell } => {
            // Generate from the app's own derived command so completions never
            // drift from the real CLI. No daemon, no I/O beyond stdout.
            commands::completion(shell, &mut Cli::command());
            Ok(())
        }
        TopCommand::Doctor { json, deep } => {
            // `doctor` returns whether all checks passed; the exit-1-on-failure
            // decision lives here (the library never calls `std::process::exit`).
            let healthy = codypendent_cli::doctor::run(&paths, json, deep).await?;
            if healthy {
                Ok(())
            } else {
                std::process::exit(1);
            }
        }
        TopCommand::Update { check, tag } => {
            // `--check` exits 2 when an update is available (scriptable); the
            // exit decision lives here (the library never calls `process::exit`).
            let available = codypendent_cli::update::run(&paths, check, tag).await?;
            if check && available {
                std::process::exit(2);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn daemon_subcommand_parses_to_internal_daemon() {
        // `codypendent __daemon` is the hidden self-spawn target; it must parse.
        let cli = Cli::try_parse_from(["codypendent", "__daemon"]).expect("__daemon must parse");
        assert!(matches!(cli.command, Some(TopCommand::InternalDaemon)));
    }

    #[test]
    fn internal_daemon_is_hidden_from_help() {
        let mut cmd = Cli::command();
        let help = cmd.render_long_help().to_string();
        assert!(
            !help.contains("__daemon"),
            "the __daemon subcommand must be hidden from --help, got:\n{help}"
        );
    }

    #[test]
    fn accessible_and_plain_flags_select_the_cooked_client() {
        let accessible =
            Cli::try_parse_from(["codypendent", "--accessible"]).expect("accessible must parse");
        assert!(accessible.accessible);
        assert!(accessible.command.is_none());

        let plain = Cli::try_parse_from(["codypendent", "--plain"]).expect("plain must parse");
        assert!(plain.accessible);
        assert!(plain.command.is_none());

        let mut command = Cli::command();
        let help = command.render_long_help().to_string();
        assert!(help.contains("--accessible"));
        assert!(help.contains("--plain"));
    }

    #[test]
    fn acp_management_and_legacy_serve_forms_parse() {
        let connect = Cli::try_parse_from([
            "codypendent",
            "acp",
            "connect",
            "vibe-chat",
            "--profile",
            "agents/vibe",
        ])
        .expect("ACP connect must parse");
        assert!(matches!(
            connect.command,
            Some(TopCommand::Acp {
                command: Some(AcpCommand::Connect { agent, profile, .. }),
                ..
            }) if agent == "vibe-chat" && profile.as_deref() == Some("agents/vibe")
        ));

        let probe = Cli::try_parse_from(["codypendent", "acp", "probe", "kimi-code"])
            .expect("ACP live probe must parse");
        assert!(matches!(
            probe.command,
            Some(TopCommand::Acp {
                command: Some(AcpCommand::Probe { agent, .. }),
                ..
            }) if agent == "kimi-code"
        ));

        let legacy = Cli::try_parse_from(["codypendent", "acp", "--repo", "."])
            .expect("legacy ACP serve must parse");
        assert!(matches!(
            legacy.command,
            Some(TopCommand::Acp { command: None, .. })
        ));
    }

    #[test]
    fn council_create_and_run_forms_parse() {
        let create = Cli::try_parse_from([
            "codypendent",
            "council",
            "create",
            "design-board",
            "--member",
            "acp/claude=architect",
            "--member",
            "acp/codex=critic",
            "--chair",
            "acp/amp",
            "--rounds",
            "2",
        ])
        .expect("council create must parse");
        assert!(matches!(
            create.command,
            Some(TopCommand::Council {
                command: CouncilCommand::Create { rounds: 2, .. }
            })
        ));

        let run = Cli::try_parse_from([
            "codypendent",
            "council",
            "run",
            "design-board",
            "--objective",
            "Choose an architecture",
            "--json",
        ])
        .expect("council run must parse");
        assert!(matches!(
            run.command,
            Some(TopCommand::Council {
                command: CouncilCommand::Run { json: true, .. }
            })
        ));
    }
}
