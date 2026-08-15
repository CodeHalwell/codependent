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
    /// env var — a manual override always wins. Accepts a
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
    /// the current repository's session.
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
    /// Start a headless run and stream its events — the scriptable twin of
    /// the interactive TUI.
    Run {
        /// What the agent should do.
        #[arg(long)]
        objective: String,
        /// The mode preset the run starts in: how much the agent may change
        /// without asking.
        #[arg(long, value_enum, default_value = "build")]
        mode: ModeArg,
        /// Repository the run operates in. Defaults to the current directory.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Pin the run to this configured model id (as it appears in
        /// `codypendent models list`), e.g. `openai/gpt-5.4` or `acp/cursor`.
        /// Without it, model selection falls to routing (if enabled) or the
        /// resolver's first reachable candidate in `models.toml` FILE ORDER —
        /// with several models configured, which one that is is not obvious
        /// from the command line alone. A pin overrides quality/routing
        /// choice, never the security ceiling: a pinned hosted model for
        /// classified data is still refused (fail-closed), same as the TUI's
        /// `/model` picker.
        #[arg(long)]
        model: Option<String>,
        /// Stream every session event to stdout as JSONL until the run
        /// terminates. Currently required: run `codypendent` with no
        /// subcommand for the interactive view.
        #[arg(long)]
        jsonl: bool,
    },
    /// Attach to an existing session and stream its events as JSONL.
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
    /// Maintain the derived search indexes over the knowledge fabric —
    /// full-text (BM25) and vectors. This is SEARCH, not the code graph:
    /// use `codypendent graph build` for the code graph.
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },
    /// Build and inspect this repository's code graph — the symbol/reference
    /// map the agent reasons over. `graph build` folds it on demand and reports
    /// why it came out the size it did; `graph status` describes what is
    /// stored; `graph show` lists it.
    Graph {
        #[command(subcommand)]
        command: GraphCommand,
    },
    /// Install skill packages into the governed registry, so retrieval can
    /// disclose them to a run.
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },
    /// Validate, start, and supervise declarative workflow manifests.
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    /// Investigate and repair a failed GitHub check on a pull request (`/fix-ci`).
    /// Runs the declarative `repair-github-check` workflow — the supervised
    /// investigator → implementer → independent-reviewer flow — through the
    /// daemon. Every GitHub write parks for your approval first.
    FixCi {
        /// The pull-request number whose failing check to repair.
        #[arg(long)]
        pr: u64,
        /// Repository the repair runs against (its agent nodes each get an
        /// isolated worktree). Defaults to the current directory.
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Write, list, and publish collaborative documents; `docs publish`
    /// commits one to Git.
    Docs {
        #[command(subcommand)]
        command: DocsCommand,
    },
    /// Score the agent against a suite of fixture cases and report the result.
    Eval {
        #[command(subcommand)]
        command: EvalCommand,
    },
    /// Add, list, verify, and benchmark the models this install can run.
    Models {
        #[command(subcommand)]
        command: ModelsCommand,
    },
    // ADR-010: promotion is operator-gated by design. Nothing the agent
    // learns can promote itself, however well it scores.
    /// Move something the agent learned — a prompt, a skill, a policy — through
    /// the evaluation gate into general use. Promotion always needs a human.
    Promote {
        #[command(subcommand)]
        command: PromoteCommand,
    },
    /// Inspect what a plugin declares and which permissions it asks for,
    /// before you install it.
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    /// Inspect the MCP servers you have declared, and the tools they offer.
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Manage, inspect, and approve/reject lifecycle hooks.
    Hook {
        #[command(subcommand)]
        command: HookCommand,
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
    /// Choose each task's model from measured benchmarks instead of the
    /// default order in `models.toml`. Reads and writes
    /// `<data_dir>/routing.toml`; off unless you turn it on.
    Routing {
        #[command(subcommand)]
        command: RoutingCommand,
    },
    /// Hand a session off to an IDE: print how to attach, and launch the
    /// editor if it is on `PATH`. The IDE joins the same session as a
    /// contributor — the run keeps going, it never restarts.
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
    /// Scaffold and check a local Unsloth QLoRA fine-tuning project.
    /// Subprocess orchestration only — no Python in this workspace; the
    /// scaffolded project is a standalone Python project you run yourself.
    Finetune {
        #[command(subcommand)]
        command: FinetuneCommand,
    },
    /// Manage approvals and learned approval rules.
    Approvals {
        #[command(subcommand)]
        command: ApprovalsCommand,
    },
}

#[derive(Subcommand)]
enum ApprovalsCommand {
    /// Manage persisted approval rules.
    Rules {
        #[command(subcommand)]
        command: Option<ApprovalRulesCommand>,
    },
}

#[derive(Subcommand)]
enum ApprovalRulesCommand {
    /// List active and revoked approval rules.
    List,
    /// Revoke a persisted approval rule by id.
    Revoke {
        /// The approval rule ID to revoke.
        id: String,
    },
}

#[derive(Subcommand)]
enum FinetuneCommand {
    /// Scaffold an Unsloth QLoRA project: pinned requirements, a `train.py`
    /// for the chosen base model, a JSONL chat-transcript dataset stub, and a
    /// README covering GPU requirements, training, GGUF export, and the
    /// `ollama create` step. Refuses if the target directory already exists.
    Init {
        /// The Hugging Face base model to fine-tune. Defaults to a small,
        /// popular Unsloth QLoRA-ready repo.
        #[arg(long)]
        model: Option<String>,
        /// Directory to scaffold into (must not already exist). Defaults to
        /// `./finetune`.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Verify Python and CUDA are present for running the scaffolded
    /// `train.py`. Read-only; exits non-zero only when Python itself is
    /// missing (no GPU warns instead of failing — the scaffold is still
    /// useful without one, e.g. editing the dataset).
    Check,
    /// Work with the fine-tuning dataset.
    Dataset {
        #[command(subcommand)]
        command: FinetuneDatasetCommand,
    },
}

#[derive(Subcommand)]
enum FinetuneDatasetCommand {
    /// Export the repo's own session/eval history into `dataset/train.jsonl`
    /// as fine-tuning data, when a clean seam exists to do so. Today: prints
    /// exactly why that seam doesn't exist yet rather than silently no-op'ing.
    Export,
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
    /// Delete the derived SEARCH indexes (full-text BM25 + vectors) and rebuild
    /// them from the authoritative rows. This does NOT build the code graph —
    /// for that, run `codypendent graph build`.
    Rebuild,
}

#[derive(Subcommand)]
enum GraphCommand {
    /// Fold this repository's code graph now, and report what the fold saw:
    /// files walked, files folded, nodes and edges written, a per-language
    /// breakdown, and — the point of the command — every file extension that
    /// produced nothing, so an empty graph explains itself instead of being a
    /// silent zero.
    #[command(visible_alias = "rebuild")]
    Build {
        /// Repository to fold. Defaults to the current directory; either way it
        /// anchors on the enclosing checkout, never on a subdirectory.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Emit the report as JSON instead of the human table.
        #[arg(long)]
        json: bool,
    },
    /// Show what the stored code graph holds for this repository — counts,
    /// per-language and per-kind breakdowns, the revision it was folded at, and
    /// whether it is stale relative to the working tree. Reads only; never
    /// re-scans.
    Status {
        /// Repository to describe. Defaults to the current directory.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Emit the status as JSON instead of the human table.
        #[arg(long)]
        json: bool,
    },
    /// List the graph's nodes (and optionally their edges), filtered, so the
    /// graph is inspectable from the terminal rather than only through the TUI
    /// overlay.
    Show {
        /// Repository to read. Defaults to the current directory.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Only nodes whose repo-relative source path starts with this prefix.
        #[arg(long)]
        path: Option<String>,
        /// Only nodes in this language, as stored (`rust`, `python`, …).
        #[arg(long)]
        language: Option<String>,
        /// Only nodes of this kind, as stored (`function`, `type`, `file`, …).
        #[arg(long)]
        kind: Option<String>,
        /// Only nodes whose qualified name contains this text.
        #[arg(long)]
        name: Option<String>,
        /// Exactly one node, by the id `graph show` prints. Scoped to this
        /// repository like every other filter: an id belonging to another
        /// checkout is refused identically to one that does not exist.
        #[arg(long = "node")]
        node_id: Option<String>,
        /// Also list the edges incident to the selected nodes.
        #[arg(long)]
        edges: bool,
        /// Maximum rows of each kind (server-clamped).
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Emit the page as JSON instead of the human table.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum SkillCommand {
    /// Validate a skill package directory, copy it under `<data_dir>/skills/`,
    /// and register it. Idempotent: re-adding the same package keeps its
    /// registry identity, and the daemon re-verifies it on its next boot.
    Add {
        /// The package directory — the one holding `skill.toml`.
        directory: PathBuf,
    },
    /// Author a new skill package and register it as a `draft` — installed and
    /// inspectable, but never disclosed to a run until a human promotes it.
    /// Validated and installed through the same pipeline `add` runs; nothing
    /// here can register an `active` skill.
    New {
        /// The skill's manifest id (also its directory name under
        /// `<data_dir>/skills/`).
        id: String,
        /// Human-readable name.
        #[arg(long)]
        name: String,
        /// One sentence on what the skill is for — what retrieval matches on.
        #[arg(long)]
        description: String,
        /// `user` (this machine's operator) or `repository` (anchored to the
        /// checkout this command runs in).
        #[arg(long, default_value = "user")]
        scope: String,
        /// A Markdown file holding the `SKILL.md` procedure body.
        #[arg(long)]
        procedure: PathBuf,
        /// Where to author the package before installing. Defaults to a
        /// temporary directory; pass one to keep the authored source to edit.
        #[arg(long)]
        directory: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    /// List the MCP servers declared in `<config_dir>/mcp.toml`: launch line,
    /// env key names (never values), and the effective policy disposition.
    /// Config-level only — no server is spawned.
    List,
}

#[derive(Subcommand)]
enum HookCommand {
    /// List all discovered hooks.
    List,
    /// Print a hook's hook.toml and approval status.
    Show {
        /// The hook id to show.
        id: String,
    },
    /// Approve a hook by content hash.
    Approve {
        /// The hook id to approve.
        id: String,
    },
    /// Reject a hook.
    Reject {
        /// The hook id to reject.
        id: String,
    },
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
        /// Acknowledge that Antigravity's community ACP bridge is third-party
        /// software whose OAuth use may violate Google's Terms.
        #[arg(long)]
        accept_community_risk: bool,
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
        /// Acknowledge Antigravity community-bridge account/Terms risk.
        #[arg(long)]
        accept_community_risk: bool,
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
        /// Acknowledge Antigravity community-bridge account/Terms risk.
        #[arg(long)]
        accept_community_risk: bool,
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
        /// Evidence mode: members explore the repository with read-only tools
        /// and cite `file:line`, instead of reasoning with no tools at all.
        /// The chair then weighs cited evidence over unsupported assertion.
        /// Stored on the definition; `council run` may also enable it per-run.
        #[arg(long)]
        evidence: bool,
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
        /// Render the most recently saved run report instead of the definition.
        #[arg(long)]
        last: bool,
    },
    /// Retrieve a durable council outcome by result id, or the latest outcome
    /// for a council name. Council results are not workflow/blackboard items.
    Result {
        /// A terminal `result ID`, or a configured council name.
        selector: String,
        #[arg(long)]
        json: bool,
    },
    /// Remove a council definition. Its prior durable sessions remain.
    Remove { name: String },
    /// Run member deliberation in parallel, then ask the chair to synthesize.
    /// Every run — completed, quorum-failed, or chair-failed — persists a
    /// JSON+Markdown report under the data dir; `council show <name> --last`
    /// replays it.
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
        /// Run in evidence mode for this run even if the council was not
        /// created with `--evidence` (ORed with the stored flag; never turns
        /// evidence mode off for a council that already has it).
        #[arg(long)]
        evidence: bool,
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
    /// Start a durable workflow run from a manifest. Ensures a daemon, sends
    /// the manifest, and prints the new run id the daemon drives to a terminal
    /// state in the background.
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
    /// Pause a running workflow run so its driver stops launching new nodes;
    /// resume it later with `workflow resume`.
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
    /// Cancel a workflow run: a cooperative drain — the driver stops
    /// launching new nodes, any in-flight node's agent run is interrupted, remaining
    /// pending nodes are skipped, and the run lands cancelled (terminal — no resume).
    Cancel {
        /// The durable workflow-run id.
        workflow_run_id: String,
    },
    /// Watch a workflow run's live node lifecycle: prints the run's
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
    /// Create a collaborative document (rubric #4 doc-writer). Sends
    /// `CreateDocument`; with `--from` the Markdown file is imported into typed
    /// blocks (headings / paragraphs / code / lists / tables / callouts /
    /// embeds), otherwise the document starts empty. Prints the new id.
    New {
        /// The document title.
        title: String,
        /// A Markdown file to seed the document's blocks from.
        #[arg(long = "from", value_name = "FILE")]
        from: Option<PathBuf>,
        /// The scope to create it in: `repository` (default), `system`, or
        /// `organization:<uuid>`.
        #[arg(long)]
        scope: Option<String>,
    },
    /// List the documents this checkout can see (repository + system scope),
    /// newest first, with their status and revision.
    List,
    /// Run the documentation staleness check (`/update-docs`): resolve every
    /// document's `{{ symbol:… }}` links against the code graph, diff them for
    /// signature changes and disappearances, and file each finding as a
    /// reviewable Maintain-mode suggestion. Prints the finding counts.
    Check,
    /// Publish a document's current revision to a Git target. Prints the
    /// computed plan (target / changed files / resulting Git action) and
    /// prompts for confirmation, then sends `PublishDocument`;
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

/// `codypendent docs publish --target <TARGET>`: where a published document
/// lands. Mirrors `codypendent_knowledge::PublishTarget`'s three variants with
/// CLI-friendly names.
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
    /// write a `SuiteReport`. Ensures a daemon; each case starts its own run
    /// against its pinned fixture repository.
    Run {
        /// The suite directory under `evals/tasks/` (e.g. `core` for
        /// `evals/tasks/core/`), or a path to it directly.
        #[arg(long, default_value = "core")]
        suite: String,
        /// The routing policy to select each case's model under. Resolved
        /// via `codypendent-routing`
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
    /// List the models configured in `models.toml`, with their provider,
    /// endpoint, context window, and key status. The headless twin of the
    /// TUI's `/model` picker.
    List,
    /// Add a model from a catalog provider to `models.toml`, exactly as the
    /// TUI's add-model flow does (same catalog lookup, same auth header
    /// resolution, same atomic write). With no `--key-env`, a key is read
    /// from the provider's documented environment variable at call time.
    Add {
        /// The catalog provider id, as `codypendent models list-providers`
        /// spells it: `openai`, `anthropic`, `nebius`, `openrouter`, …. That
        /// listing marks the providers this command cannot serve; every
        /// example here is one it can.
        provider: String,
        /// The provider-side model id, as it must be sent on the wire.
        model: String,
        /// Store this environment variable NAME on the entry rather than
        /// relying on the provider's documented default. The VALUE is never
        /// written to disk — only the name.
        #[arg(long, value_name = "NAME")]
        key_env: Option<String>,
        /// Override the `models.toml` id (defaults to `<provider>/<model>`).
        #[arg(long, value_name = "ID")]
        id: Option<String>,
    },
    /// Verify one configured model end to end: resolve its credentials the way
    /// a run does, call the provider's `/models`, and confirm the configured
    /// model is listed. Exits non-zero when it is not.
    Check {
        /// The `models.toml` model id to check.
        id: String,
    },
    /// Benchmark a local model configured in `models.toml` and persist its
    /// measured profile: tokens/sec, time-to-first-token,
    /// warm-up, memory, context limit, structured-output reliability, tool-call
    /// accuracy, and a small coding-eval score. The router reads these MEASURED
    /// numbers (never vibes). Also caches the first-use capability probe.
    Bench {
        /// The `models.toml` model id to benchmark (its `base_url` is the
        /// endpoint the profile + probe are keyed under).
        id: String,
        /// Set (or override) this model's blended per-1M-token price for
        /// routing's cost/utility scoring, e.g. `3.5` for an average of $3.5
        /// per million tokens. Without it, a HOSTED model's price is looked
        /// up from the built-in provider catalog when the entry names a
        /// known `provider_id` (the same catalog `models add` reads); when
        /// neither is available the model is benched with an unpriced
        /// profile and stays ineligible for routing (a hosted model can
        /// never be silently treated as free).
        /// A LOCAL model needs no price to route (routing costs it at $0
        /// genuinely, not as the harness's "unmeasured" sentinel), so this
        /// flag is normally only needed for a hosted endpoint the catalog
        /// does not curate.
        #[arg(long, value_name = "USD_PER_1M")]
        price_per_1m_usd: Option<f64>,
    },
    /// List the built-in + user-extended provider catalog: id, display name,
    /// wire protocol, and whether it curates any prefilled models — the
    /// input `models add <provider> <model-id>` expects.
    ListProviders,
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
enum RoutingCommand {
    /// Show whether measured routing is on, and what `routing.toml`
    /// currently declares.
    Status,
    /// Turn on measured routing: each task's model is chosen from benched
    /// profiles instead of the first reachable candidate in `models.toml`
    /// file order.
    Enable {
        /// The most sensitive data this scope is asserted to handle
        /// (`public`|`internal`|`confidential`|`secret`). Governs which
        /// models may be selected for off-device (hosted) routing — omit to
        /// keep the fail-closed default (`Unknown`, local-only).
        #[arg(long)]
        data_classification: Option<String>,
    },
    /// Turn off measured routing; `models.toml` file order decides again.
    Disable,
}

#[derive(Subcommand)]
enum PromoteCommand {
    /// Draft a candidate for the promotion pipeline. Prints the new
    /// candidate id.
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
    // ADR-010 is enforced here, not merely documented: the `Controller` role
    // this local-first socket maps to a human operator is required, so an
    // agent- or system-initiated approval is refused structurally.
    /// **Approve and promote a candidate.** Requires a human operator; an
    /// agent cannot approve its own promotion.
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
    /// plugin. Manifest parsing only; it does not run anything.
    Inspect {
        /// Path to the plugin manifest to inspect.
        file: PathBuf,
    },
    /// Compare an installed `plugin.toml` against an update and print the
    /// permission diff, reporting whether the update expands permissions and so
    /// requires re-approval.
    Diff {
        /// The currently-installed manifest.
        installed: PathBuf,
        /// The candidate update manifest.
        update: PathBuf,
    },
    /// Verify a plugin artifact against its manifest using the trusted-publisher
    /// key store — the install gate for real keys. A signed plugin
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
    /// Manage the trusted-publisher key store: the ed25519 public keys
    /// `plugin verify` checks signatures against.
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

/// Print a failed command's error the way a person reads it, and exit 1.
///
/// Returning `anyhow::Result` from `main` hands the error to Rust's
/// `Termination` impl, which prints the **Debug** form — and with
/// `RUST_BACKTRACE` set anywhere in the environment, that is the captured
/// backtrace. The 2026-08-13 review caught this as 36 frames dumped over the
/// terminal when the TUI lost its daemon, but the TUI was only where it was
/// noticed: EVERY command exits through here, so `models add azure-openai
/// gpt-5.4` printed its (correct, actionable) one-line refusal followed by a
/// stack trace naming `anyhow/error.rs` and `_start`.
///
/// `{error:#}` is anyhow's alternate Display: the whole `.context(...)` chain
/// on one line, no frames. The `Error:` prefix and the exit status are the
/// same ones `Termination` produced, so scripts see no change — only the
/// backtrace goes away.
#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
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
            model,
            jsonl,
        } => {
            let repo = match repo {
                Some(repo) => repo,
                None => std::env::current_dir()?,
            };
            let exit_code =
                commands::run(&paths, objective, mode.into(), repo, model, jsonl).await?;
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
        TopCommand::Graph { command } => match command {
            GraphCommand::Build { repo, json } => commands::graph_build(&paths, repo, json).await,
            GraphCommand::Status { repo, json } => commands::graph_status(&paths, repo, json).await,
            GraphCommand::Show {
                repo,
                path,
                language,
                kind,
                name,
                node_id,
                edges,
                limit,
                json,
            } => {
                let query = codypendent_protocol::CodeGraphQuery {
                    path,
                    language,
                    kind,
                    name,
                    node_id,
                    include_edges: edges,
                    include_nodes: true,
                    limit,
                };
                commands::graph_show(&paths, repo, query, json).await
            }
        },
        TopCommand::Skill { command } => match command {
            SkillCommand::Add { directory } => commands::skill_add(&paths, &directory).await,
            SkillCommand::New {
                id,
                name,
                description,
                scope,
                procedure,
                directory,
            } => {
                commands::skill_new(
                    &paths,
                    &id,
                    &name,
                    &description,
                    &scope,
                    &procedure,
                    directory.as_deref(),
                )
                .await
            }
        },
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
            DocsCommand::New { title, from, scope } => {
                commands::docs_new(&paths, &title, from.as_deref(), scope).await
            }
            DocsCommand::List => commands::docs_list(&paths).await,
            DocsCommand::Check => commands::docs_check(&paths).await,
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
            ModelsCommand::List => commands::models_list(&paths),
            ModelsCommand::Add {
                provider,
                model,
                key_env,
                id,
            } => commands::models_add(&paths, &provider, &model, key_env.as_deref(), id.as_deref()),
            ModelsCommand::Check { id } => commands::models_check(&paths, &id).await,
            ModelsCommand::Bench {
                id,
                price_per_1m_usd,
            } => commands::models_bench(&paths, &id, price_per_1m_usd).await,
            ModelsCommand::ListProviders => commands::models_list_providers(&paths),
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
        TopCommand::Hook { command } => match command {
            HookCommand::List => commands::hook_list(&paths).await,
            HookCommand::Show { id } => commands::hook_show(&paths, &id).await,
            HookCommand::Approve { id } => commands::hook_approve(&paths, &id).await,
            HookCommand::Reject { id } => commands::hook_reject(&paths, &id).await,
        },
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
                accept_community_risk,
            }) => {
                codypendent_cli::acp_clients::install(
                    &paths,
                    &agent,
                    refresh,
                    allow_unverified,
                    accept_community_risk,
                )
                .await
            }
            Some(AcpCommand::Connect {
                agent,
                profile,
                refresh,
                allow_unverified,
                accept_community_risk,
                repo: connect_repo,
            }) => {
                let repository = connect_repo.or(repo).unwrap_or(std::env::current_dir()?);
                codypendent_cli::acp_clients::connect(
                    &paths,
                    &agent,
                    profile.as_deref(),
                    refresh,
                    allow_unverified,
                    accept_community_risk,
                    &repository,
                )
                .await
            }
            Some(AcpCommand::Probe {
                agent,
                prompt,
                refresh,
                allow_unverified,
                accept_community_risk,
                repo: probe_repo,
            }) => {
                let repository = probe_repo.or(repo).unwrap_or(std::env::current_dir()?);
                codypendent_cli::acp_clients::probe(
                    &paths,
                    &agent,
                    &prompt,
                    refresh,
                    allow_unverified,
                    accept_community_risk,
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
                evidence,
            } => codypendent_cli::council::create(
                &paths,
                name,
                member,
                chair,
                rounds,
                description,
                evidence,
            ),
            CouncilCommand::List { json } => codypendent_cli::council::list(&paths, json),
            CouncilCommand::Show { name, json, last } => {
                codypendent_cli::council::show(&paths, &name, json, last)
            }
            CouncilCommand::Result { selector, json } => {
                codypendent_cli::council::show_result(&paths, &selector, json)
            }
            CouncilCommand::Remove { name } => codypendent_cli::council::remove(&paths, &name),
            CouncilCommand::Run {
                name,
                objective,
                repo,
                json,
                evidence,
            } => {
                let repository = repo.unwrap_or(std::env::current_dir()?);
                codypendent_cli::council::run(&paths, &name, objective, repository, json, evidence)
                    .await
            }
        },
        TopCommand::Routing { command } => match command {
            RoutingCommand::Status => commands::routing_status(&paths),
            RoutingCommand::Enable {
                data_classification,
            } => commands::routing_enable(&paths, data_classification.as_deref()).await,
            RoutingCommand::Disable => commands::routing_disable(&paths).await,
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
        TopCommand::Finetune { command } => match command {
            FinetuneCommand::Init { model, out } => {
                let options = codypendent_cli::finetune::InitOptions {
                    base_model: model.unwrap_or_else(|| {
                        codypendent_cli::finetune::DEFAULT_BASE_MODEL.to_string()
                    }),
                    out_dir: out.unwrap_or_else(|| {
                        PathBuf::from(codypendent_cli::finetune::DEFAULT_OUT_DIR)
                    }),
                };
                let report = codypendent_cli::finetune::init(&options)?;
                println!(
                    "codypendent finetune init: scaffolded {} file(s) in {}",
                    report.files_written.len(),
                    report.out_dir.display()
                );
                println!(
                    "next: cd {} && pip install -r requirements.txt",
                    report.out_dir.display()
                );
                println!("      codypendent finetune check   # verify python/CUDA first");
                Ok(())
            }
            FinetuneCommand::Check => {
                // Same exit-decision-lives-in-main convention as `doctor`
                // above: the library never calls `std::process::exit`. Only a
                // missing Python fails the process; a missing GPU warns.
                let report = codypendent_cli::finetune::check("python3", "nvidia-smi");
                print!("{}", report.render_text());
                if report.worst() == codypendent_cli::doctor::Status::Fail {
                    std::process::exit(1);
                }
                Ok(())
            }
            FinetuneCommand::Dataset { command } => match command {
                FinetuneDatasetCommand::Export => {
                    println!("{}", codypendent_cli::finetune::dataset_export());
                    Ok(())
                }
            },
        },
        TopCommand::Approvals { command } => match command {
            ApprovalsCommand::Rules { command } => {
                match command.unwrap_or(ApprovalRulesCommand::List) {
                    ApprovalRulesCommand::List => commands::approvals_rules_list(&paths).await,
                    ApprovalRulesCommand::Revoke { id } => {
                        commands::approvals_rules_revoke(&paths, &id).await
                    }
                }
            }
        },
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

        let antigravity = Cli::try_parse_from([
            "codypendent",
            "acp",
            "connect",
            "antigravity",
            "--accept-community-risk",
        ])
        .expect("Antigravity risk acknowledgement must parse");
        assert!(matches!(
            antigravity.command,
            Some(TopCommand::Acp {
                command: Some(AcpCommand::Connect {
                    agent,
                    accept_community_risk: true,
                    ..
                }),
                ..
            }) if agent == "antigravity"
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

        let result = Cli::try_parse_from([
            "codypendent",
            "council",
            "result",
            "019ff6cd-c6a6-7572-a91c-3c3caadd05a0",
            "--json",
        ])
        .expect("council result must parse");
        assert!(matches!(
            result.command,
            Some(TopCommand::Council {
                command: CouncilCommand::Result { json: true, .. }
            })
        ));
    }
}
