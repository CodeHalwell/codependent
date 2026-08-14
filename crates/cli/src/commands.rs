//! Daemon lifecycle commands (Phase 0), the headless JSONL client (STEP 1.13:
//! `run` and `attach`), the Phase-2 `index rebuild` maintenance command, and
//! `docs publish` (Phase 4 STEP 4.4).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use base64::Engine as _;
use codypendent_daemon::policy::{ApprovalAction, MergedPolicy, PolicyEngine};
use codypendent_integrations::mcp::{load_mcp_config, McpConfig};
use codypendent_knowledge::{
    db as knowledge_db, install_package, is_retrievable_status, local_user_scope, plan_publication,
    publications, register_builtins, retrieve, user_skills_root, DocumentStore, HashingEmbedder,
    Publication, PublishTarget as KnowledgePublishTarget, Registry, RetrievalConfig,
    RetrievalIndexes, RetrievalQuery, RiskClass, Scope,
};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{
    AgentMode, ApprovalDecision, ApprovalScope, ClientRole, CommandBody, CommandId, DaemonStatus,
    DocumentId, ModelId, Payload, PromotionAction, SessionId, Subscription, WorkflowEvent,
    WorkflowNodeView, WorkflowRunPhase, WorkflowRunSnapshot, WorkspaceId,
};

use crate::client;
use crate::connection::Connection;
use crate::stream::{self, RunExit};

/// Outcome of making sure a daemon is listening: either one already was, or
/// this call spawned and waited for one. Shared by the human-facing
/// `codypendent daemon start` and the silent variant `run --jsonl` uses (its
/// stdout must carry nothing but JSONL envelopes).
#[derive(Debug)]
pub(crate) enum EnsureOutcome {
    AlreadyRunning,
    Started { pid: u32 },
}

/// Spawn the daemon (`codypendent __daemon`, this binary itself) detached if
/// nothing answers Ping yet, then wait for the socket to come up (5 second
/// budget). No I/O beyond the daemon's own log file — callers decide how (or
/// whether) to report the outcome.
pub(crate) async fn ensure_daemon(paths: &RuntimePaths) -> anyhow::Result<EnsureOutcome> {
    if client::ping(&paths.socket_path).await {
        return Ok(EnsureOutcome::AlreadyRunning);
    }
    paths.ensure_directories()?;

    let invocation = resolve_daemon_binary();
    let log_path = paths.log_dir.join("daemon.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log_for_stderr = log.try_clone()?;

    let mut command = daemon_command(&invocation);
    command
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log_for_stderr);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // New process group: the daemon must not die with this CLI's terminal.
        command.process_group(0);
    }
    let child = command
        .spawn()
        .with_context(|| format!("failed to spawn {}", invocation.program.display()))?;

    for _ in 0..50 {
        if client::ping(&paths.socket_path).await {
            return Ok(EnsureOutcome::Started { pid: child.id() });
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!(
        "daemon did not become ready within 5 seconds; check {}",
        log_path.display()
    )
}

/// `codypendent daemon start`: spawn the daemon (`codypendent __daemon`, this
/// binary) detached, then wait for the socket to answer Ping (5 second budget).
pub async fn start(paths: &RuntimePaths) -> anyhow::Result<()> {
    match ensure_daemon(paths).await? {
        EnsureOutcome::AlreadyRunning => println!("daemon already running"),
        EnsureOutcome::Started { pid } => println!("daemon started (pid {pid})"),
    }
    Ok(())
}

/// Core of `daemon stop`, with no stdout of its own: if a daemon is
/// listening, ask it to shut down gracefully and wait (5 second budget) for
/// the socket to stop answering. Returns whether one was running (`false` is
/// a no-op, not an error) so callers can report accordingly. Split out so
/// [`restart_daemon`] can reuse the exact same stop path instead of
/// reimplementing it.
async fn stop_running_daemon(paths: &RuntimePaths) -> anyhow::Result<bool> {
    if !client::ping(&paths.socket_path).await {
        return Ok(false);
    }
    client::shutdown(&paths.socket_path).await?;
    for _ in 0..50 {
        if !client::ping(&paths.socket_path).await {
            return Ok(true);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!("daemon acknowledged shutdown but is still answering after 5 seconds")
}

/// `codypendent daemon stop`: request graceful shutdown, then wait for the
/// socket to stop answering (5 second budget).
pub async fn stop(paths: &RuntimePaths) -> anyhow::Result<()> {
    if stop_running_daemon(paths).await? {
        println!("daemon stopped");
    } else {
        println!("daemon is not running");
    }
    Ok(())
}

/// The composition behind [`restart_daemon`]: stop, then start, in that
/// order. Split out (taking owned `RuntimePaths` plus injectable stop/start
/// steps) so a unit test can substitute fakes and assert the ordering and
/// the none-running short-circuit without touching a real socket or
/// spawning a real process; production code always calls it with
/// [`stop_running_daemon`] and [`ensure_daemon`] (see [`restart_daemon`]).
async fn restart_daemon_with<StopFut, StartFut>(
    paths: RuntimePaths,
    stop: impl FnOnce(RuntimePaths) -> StopFut,
    start: impl FnOnce(RuntimePaths) -> StartFut,
) -> anyhow::Result<EnsureOutcome>
where
    StopFut: std::future::Future<Output = anyhow::Result<bool>>,
    StartFut: std::future::Future<Output = anyhow::Result<EnsureOutcome>>,
{
    stop(paths.clone())
        .await
        .context("stopping the running daemon before restart")?;
    start(paths)
        .await
        .context("starting a fresh daemon after restart")
}

/// The reusable "stop the running daemon if one is running, then start a
/// fresh one" primitive. Backs the manual `codypendent daemon restart`
/// subcommand below, and is the building block the auto-restart driver
/// (`reconcile_daemon_build`) reuses on its idle path rather than
/// reimplementing stop/spawn.
///
/// Idempotent when nothing is running: [`stop_running_daemon`] is then a
/// no-op and this simply starts one, exactly like `daemon start`. Errors from
/// either step are legible (the stop-still-answering message or
/// `ensure_daemon`'s not-ready message) and never hang.
pub(crate) async fn restart_daemon(paths: &RuntimePaths) -> anyhow::Result<EnsureOutcome> {
    restart_daemon_with(
        paths.clone(),
        |paths| async move { stop_running_daemon(&paths).await },
        |paths| async move { ensure_daemon(&paths).await },
    )
    .await
}

/// What [`restart_daemon_if_idle`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleRestartOutcome {
    /// Stopped the old (daemon-confirmed-idle) daemon and started a fresh one.
    Restarted,
    /// The daemon refused the idle-guarded shutdown because a run was active;
    /// nothing was stopped or started. Carries the count the daemon reported.
    RefusedActive(u64),
}

/// Like [`restart_daemon`], but asks the daemon to stop ONLY if it is idle
/// (`client::shutdown_if_idle`, protocol v1.3): the daemon makes the final
/// idle decision atomically against concurrent run admission, so this fully
/// closes the auto-restart TOCTOU window. If the daemon refuses (a run is
/// active) nothing is stopped or spawned and [`IdleRestartOutcome::RefusedActive`]
/// is returned; the caller continues on the existing daemon. Only call this
/// against a daemon whose negotiated minor is ≥ 3 — the auto-restart driver
/// gates on exactly that and otherwise falls back to [`restart_daemon`].
pub(crate) async fn restart_daemon_if_idle(
    paths: &RuntimePaths,
) -> anyhow::Result<IdleRestartOutcome> {
    // Nothing running → treat as already stopped and just start a fresh one
    // (idempotent, exactly like `restart_daemon`'s none-running short-circuit).
    if !client::ping(&paths.socket_path).await {
        ensure_daemon(paths)
            .await
            .context("starting a fresh daemon after restart")?;
        return Ok(IdleRestartOutcome::Restarted);
    }
    match client::shutdown_if_idle(&paths.socket_path)
        .await
        .context("requesting an idle-guarded daemon shutdown before restart")?
    {
        client::ShutdownIfIdleOutcome::RefusedActive(active) => {
            Ok(IdleRestartOutcome::RefusedActive(active))
        }
        client::ShutdownIfIdleOutcome::Stopped => {
            // Wait for the socket to stop answering, then spawn the new build —
            // the same 5s budget and sequence as `stop_running_daemon`.
            for _ in 0..50 {
                if !client::ping(&paths.socket_path).await {
                    ensure_daemon(paths)
                        .await
                        .context("starting a fresh daemon after restart")?;
                    return Ok(IdleRestartOutcome::Restarted);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            anyhow::bail!(
                "daemon acknowledged idle-shutdown but is still answering after 5 seconds"
            )
        }
    }
}

/// `codypendent daemon restart`: atomically ask the daemon to stop only when
/// no session or workflow run is active, then start a fresh one. A manual
/// restart has the same no-kill safety invariant as auto-restart.
pub async fn restart(paths: &RuntimePaths) -> anyhow::Result<()> {
    match restart_daemon_if_idle(paths).await? {
        IdleRestartOutcome::Restarted => {
            println!("daemon restarted");
            Ok(())
        }
        IdleRestartOutcome::RefusedActive(active) => anyhow::bail!(
            "daemon restart refused: {active} run(s) are active; wait for them to finish or cancel them explicitly"
        ),
    }
}

/// `codypendent daemon status [--json]`.
///
/// Prints the status (human text or JSON) and RETURNS whether the daemon is
/// running (`true`) or not (`false`). The library never calls
/// `std::process::exit`; the `status` subcommand's exit-1-when-not-running
/// decision lives in `main.rs`.
pub async fn status(paths: &RuntimePaths, json: bool) -> anyhow::Result<bool> {
    match client::daemon_status(&paths.socket_path).await {
        Ok(status) => {
            if json {
                let value = serde_json::json!({ "running": true, "status": status });
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                print!("{}", render_status_text(&status));
            }
            Ok(true)
        }
        Err(_) => {
            if json {
                println!("{}", serde_json::json!({ "running": false }));
            } else {
                println!("daemon is not running");
            }
            Ok(false)
        }
    }
}

/// Render `daemon status`'s human-readable text (the `--json` path
/// serializes `DaemonStatus` directly instead, so it already carries
/// `build_id`/`active_run_count` without any change here). Split into a pure
/// function so a test can assert the build id and active-run-count lines
/// without a running daemon.
fn render_status_text(status: &DaemonStatus) -> String {
    let mut out = String::new();
    out.push_str("Codypendent daemon\n");
    out.push_str("  running      yes\n");
    out.push_str(&format!("  version      {}\n", status.daemon_version));
    out.push_str(&format!("  build        {}\n", status.build_id));
    out.push_str(&format!("  protocol     {}\n", status.protocol_version));
    out.push_str(&format!("  pid          {}\n", status.pid));
    out.push_str(&format!("  instance     {}\n", status.instance_id));
    out.push_str(&format!("  boot count   {}\n", status.boot_count));
    out.push_str(&format!(
        "  started at   {}\n",
        status.started_at.to_rfc3339()
    ));
    out.push_str(&format!("  uptime       {}s\n", status.uptime_seconds));
    out.push_str(&format!("  database     {}\n", status.database_path));
    out.push_str(&format!("  socket       {}\n", status.socket_path));
    out.push_str(&format!("  sessions     {}\n", status.session_count));
    out.push_str(&format!("  active runs  {}\n", status.active_run_count));
    if status.integration_issues.is_empty() {
        out.push_str("  integrations  healthy\n");
    } else {
        out.push_str(&format!(
            "  integrations  {} issue(s)\n",
            status.integration_issues.len()
        ));
        for issue in &status.integration_issues {
            out.push_str(&format!("    - {issue}\n"));
        }
    }
    out
}

/// How to launch the daemon: the program to run plus the leading args. The
/// primary form runs THIS binary (`codypendent`) with the hidden `__daemon`
/// subcommand, so an updated `codypendent` always spawns a matching daemon —
/// there is no separate `codypendentd` on disk to go stale (the version-skew
/// bug this design eliminates).
struct DaemonInvocation {
    program: PathBuf,
    args: Vec<String>,
}

/// Resolve how to launch the daemon. Primary: run this executable itself via
/// `codypendent __daemon`. Fallback (only when `current_exe` is unavailable — a
/// rare failure): a `codypendentd` on PATH, launched with no extra args, keeping
/// the pre-single-binary path working.
fn resolve_daemon_binary() -> DaemonInvocation {
    resolve_daemon_invocation(std::env::current_exe())
}

/// The pure core of [`resolve_daemon_binary`], taking the `current_exe` result
/// so both the self-spawn and the fallback are unit-testable without depending
/// on the test binary's own path.
fn resolve_daemon_invocation(current_exe: std::io::Result<PathBuf>) -> DaemonInvocation {
    match current_exe {
        // Run the daemon from THIS binary: `codypendent __daemon`.
        Ok(program) => DaemonInvocation {
            program,
            args: vec!["__daemon".to_string()],
        },
        // `current_exe` unavailable: fall back to a `codypendentd` on PATH.
        Err(_) => DaemonInvocation {
            program: PathBuf::from("codypendentd"),
            args: Vec::new(),
        },
    }
}

/// Build the (unspawned) command that launches the daemon per `invocation`.
/// Split from [`ensure_daemon`] so a test can assert the resolved program +
/// argv (`current_exe __daemon`) without spawning a real daemon.
fn daemon_command(invocation: &DaemonInvocation) -> std::process::Command {
    let mut command = std::process::Command::new(&invocation.program);
    command.args(&invocation.args);
    command
}

// --- STEP 1.13: headless JSONL client ---------------------------------------

/// `codypendent run --objective "..." [--mode build] [--repo PATH] --jsonl`.
///
/// Ensures a daemon is running, creates a session, starts one run in it, and
/// streams every session event to stdout as JSONL until the run reaches a
/// terminal state. Returns the STEP 1.13 exit code (`0` completed, `2`
/// failed, `130` cancelled); `main` is the only place that calls
/// `std::process::exit`.
///
/// `repo` is validated (must exist) and its canonical path is carried on
/// `StartRun`, so the daemon attributes the run's repository map and curated
/// memories to *this* checkout rather than to its own working directory — the
/// per-user daemon can serve several checkouts over one socket (issue #6
/// item 1). `CreateSession` still carries only an opaque `WorkspaceId`; binding
/// a dedicated worktree to a run is a later step (STEP 1.8).
pub async fn run(
    paths: &RuntimePaths,
    objective: String,
    mode: AgentMode,
    repo: PathBuf,
    model: Option<String>,
    jsonl: bool,
) -> anyhow::Result<i32> {
    if !jsonl {
        anyhow::bail!(
            "codypendent run currently requires --jsonl; interactive TUI attach \
             lands in a later build step"
        );
    }
    let repo = repo.canonicalize().with_context(|| {
        format!(
            "--repo {}: not a valid, accessible directory",
            repo.display()
        )
    })?;
    if !repo.is_dir() {
        anyhow::bail!("--repo {}: not a directory", repo.display());
    }
    // Caught here, before the daemon is even contacted, so a typo'd `--model`
    // fails fast with the same list a user would check by hand — not a
    // StartRun round trip followed by an opaque daemon-side resolution error.
    if let Some(id) = &model {
        let configured =
            codypendent_runtime::models::load_models(&paths.data_dir.join("models.toml"))
                .unwrap_or_default();
        if !configured.iter().any(|c| c.id.0 == *id) {
            anyhow::bail!(
                "--model `{id}` is not configured; see `codypendent models list` for the \
                 configured ids"
            );
        }
    }

    // The daemon-start banner ("daemon already running" / "daemon started
    // (pid N)") is Phase 0 human output; --jsonl's contract is that stdout
    // carries nothing but JSONL envelopes, so this step is silent on success
    // and only ever writes to stderr on failure (via the `?` below).
    ensure_daemon(paths).await?;

    let mut conn = Connection::connect(&paths.socket_path).await?;
    let mut stdout = std::io::stdout();
    let repository = repo.to_string_lossy().into_owned();
    let model = model.map(ModelId);
    let exit =
        run_over_connection(&mut conn, objective, mode, &repository, model, &mut stdout).await?;
    Ok(exit.exit_code())
}

/// The connected core of [`run`]: handshake, create + attach + start, then
/// stream to `out` until terminal. Split out so tests can drive it against a
/// hand-rolled mock server over a `Connection` that already points at a test
/// socket, asserting the returned [`RunExit`] directly instead of a process
/// exit code.
pub async fn run_over_connection<W: Write>(
    conn: &mut Connection,
    objective: String,
    mode: AgentMode,
    repository: &str,
    model: Option<ModelId>,
    out: &mut W,
) -> anyhow::Result<RunExit> {
    let hello = conn
        .handshake("codypendent", env!("CARGO_PKG_VERSION"), None)
        .await?;
    // `run --jsonl` is headless/scripted (T9 scope): a daemon-build mismatch
    // is WARN-ONLY here, never auto-restarted — bouncing the daemon out from
    // under a non-interactive invocation (possibly one step in a scripted
    // batch, with a strict JSONL stdout contract) would be actively wrong.
    // stderr only; stdout carries nothing but JSONL envelopes.
    if let Some(message) =
        crate::restart::headless_mismatch_warning(codypendent_protocol::BUILD_ID, &hello.build_id)
    {
        eprintln!("codypendent: {message}");
    }

    // CreateSession: the daemon's `CommandAccepted` *payload* is intentionally
    // minimal (only `command_id` + `sequence`). The freshly created session's id
    // travels on the reply envelope's own `session_id` field
    // (`Envelope.session_id`, Chapter 03) — connection-level metadata the server
    // sets on a `CreateSession` reply from `CommandOutcome::created_session`
    // (`crates/daemon/src/server.rs`). This client reads it from there; if a
    // daemon ever omits it we fail loudly and specifically below rather than
    // hang waiting for an id that will never arrive.
    let workspace = WorkspaceId::new();
    let create_reply = conn
        .send_command(CommandBody::CreateSession {
            workspace,
            title: objective.clone(),
            // Attribute the session to the `--repo` the client is operating on
            // (mirrors the `StartRun.repository` below), so the daemon can
            // build its code graph on open, not only on the first run.
            repository: Some(repository.to_owned()),
        })
        .await?;
    let session_id = match &create_reply.payload {
        Payload::CommandAccepted { .. } => create_reply.session_id.ok_or_else(|| {
            anyhow::anyhow!(
                "daemon accepted CreateSession but its reply carried no session_id \
                 (neither in the payload nor Envelope.session_id); codypendent run \
                 cannot learn the newly created session's id"
            )
        })?,
        Payload::CommandRejected(error) => {
            anyhow::bail!("CreateSession rejected: {} ({})", error.message, error.code)
        }
        other => anyhow::bail!("unexpected reply to CreateSession: {other:?}"),
    };

    let attach_reply = conn
        .send_command(CommandBody::AttachSession {
            session_id,
            last_seen_sequence: None,
            subscriptions: vec![Subscription::SessionSummary, Subscription::AgentActivity],
            requested_role: ClientRole::Controller,
            repository: Some(repository.to_owned()),
        })
        .await?;
    let catchup = expect_catchup(attach_reply)?;
    stream::replay_catchup(out, conn.client_id(), session_id, catchup)?;

    let start_reply = conn
        .send_command(CommandBody::StartRun {
            session_id,
            objective,
            mode,
            // Attribute the run to the `--repo` the client is operating on, so a
            // shared daemon does not store its memories under its own directory
            // (issue #6 item 1).
            repository: Some(repository.to_owned()),
            // `--model` pins the run exactly like the TUI's `/model` picker
            // (STEP MP2); `None` keeps the prior behavior — routing (if
            // enabled) or the resolver's first reachable candidate.
            model,
        })
        .await?;
    if let Payload::CommandRejected(error) = &start_reply.payload {
        anyhow::bail!("StartRun rejected: {} ({})", error.message, error.code);
    }
    // Bind to exactly the run OUR StartRun created (the daemon reports it on
    // the accept). Falling back to first-observed `RunStarted` is only for an
    // older daemon that doesn't send it — under which a concurrent client's
    // run starting first could otherwise capture the exit code.
    let created_run = match &start_reply.payload {
        Payload::CommandAccepted { created_run, .. } => *created_run,
        _ => None,
    };

    stream::stream_until_terminal(conn, out, created_run).await
}

/// `codypendent attach <SESSION_ID> [--from-sequence N] --events jsonl`.
///
/// Attaches as an `Observer` and streams the catch-up plus every subsequent
/// session event as JSONL until the connection ends or the user interrupts
/// with Ctrl-C — never stopping (let alone affecting) the run itself.
pub async fn attach(
    paths: &RuntimePaths,
    session_id: SessionId,
    from_sequence: Option<u64>,
) -> anyhow::Result<()> {
    let mut conn = Connection::connect(&paths.socket_path).await?;
    let mut stdout = std::io::stdout();
    tokio::select! {
        result = attach_over_connection(&mut conn, session_id, from_sequence, &mut stdout) => result,
        _ = tokio::signal::ctrl_c() => Ok(()),
    }
}

/// The connected core of [`attach`], split out for the same testability
/// reason as [`run_over_connection`].
pub async fn attach_over_connection<W: Write>(
    conn: &mut Connection,
    session_id: SessionId,
    from_sequence: Option<u64>,
    out: &mut W,
) -> anyhow::Result<()> {
    conn.handshake("codypendent", env!("CARGO_PKG_VERSION"), None)
        .await?;

    let attach_reply = conn
        .send_command(CommandBody::AttachSession {
            session_id,
            last_seen_sequence: from_sequence,
            subscriptions: vec![Subscription::SessionSummary, Subscription::AgentActivity],
            requested_role: ClientRole::Observer,
            // No repo context here: this attaches to a session another client
            // already created (and scanned, if it carried one) — see
            // `crates/cli/src/tui.rs` for the construction sites that do carry
            // the canonical repo root.
            repository: None,
        })
        .await?;
    let catchup = expect_catchup(attach_reply)?;
    stream::replay_catchup(out, conn.client_id(), session_id, catchup)?;

    stream::stream_forever(conn, out).await
}

/// Common `AttachSession` reply handling shared by `run` and `attach`.
pub(crate) fn expect_catchup(
    reply: codypendent_protocol::Envelope,
) -> anyhow::Result<codypendent_protocol::Catchup> {
    match reply.payload {
        Payload::Catchup { catchup } => Ok(catchup),
        Payload::CommandRejected(error) => {
            anyhow::bail!("AttachSession rejected: {} ({})", error.message, error.code)
        }
        other => anyhow::bail!("unexpected reply to AttachSession: {other:?}"),
    }
}

/// `codypendent index rebuild`: delete the derived indexes and rebuild them from
/// the authoritative rows (STEP 2.1 rule 2 / the Phase-2 "stale indexes rebuild
/// from authority" exit criterion).
///
/// The derived indexes are a *pure function* of the authoritative
/// registry/memory/code rows, so they can be discarded at any time and replaying
/// authority restores identical results. In Phase 2 the retrieval indexes
/// (Tantivy BM25 + the vector index) are held in memory and rebuilt from the
/// registry on demand — persisting them under `<data_dir>/index/` is a later
/// step. This command is self-contained (it does not require the daemon): it
/// opens the database directly, ensures the built-in tools are registered,
/// removes `<data_dir>/index/` if present (forward-compatible with persisted
/// indexes, a no-op today), rebuilds the retrieval indexes from the registry,
/// and runs a canary query to prove the fresh index serves retrieval.
pub async fn index_rebuild(paths: &RuntimePaths) -> anyhow::Result<()> {
    paths.ensure_directories()?;
    let database_path = paths.data_dir.join("codypendent.db");
    let pool = knowledge_db::open(&database_path)
        .await
        .with_context(|| format!("opening {}", database_path.display()))?;

    // Idempotent baseline: a rebuild on a never-started daemon still has the
    // built-in tools to index.
    register_builtins(&pool).await?;

    // Derived indexes are deletable at any time.
    let index_dir = paths.data_dir.join("index");
    if index_dir.exists() {
        std::fs::remove_dir_all(&index_dir)
            .with_context(|| format!("removing derived index dir {}", index_dir.display()))?;
    }

    // Replay authority into fresh indexes.
    let items = Registry::new().list(&pool).await?;
    let indexes = RetrievalIndexes::build(&items, HashingEmbedder::new())?;

    // Canary: the freshly rebuilt index still serves retrieval (System-scoped
    // built-ins are visible; a Medium ceiling admits every first-party tool).
    let query = RetrievalQuery::new("run the tests", vec![Scope::System], RiskClass::Medium);
    let result = retrieve(&items, &indexes, &query, &RetrievalConfig::default())?;

    println!(
        "search index rebuild complete: {} registry item(s) re-indexed from authority; \
         canary \"run the tests\" -> {} tool card(s), {} skill card(s)",
        items.len(),
        result.tools.len(),
        result.skills.len(),
    );
    // Said out loud, on the success line, because the old wording actively
    // misled: "index rebuild complete: 29 registry item(s) re-indexed" reads as
    // "the index, code graph included, is built" — and that reading is exactly
    // why an empty code graph went unexplained. Naming the command that DOES
    // build it turns a dead end into a next step.
    println!(
        "This rebuilt the SEARCH indexes (full-text + vectors) only. It does not \
         build the code graph.\nTo (re)build the code graph for a repository, run \
         `codypendent graph build`; `codypendent graph status` shows what it holds."
    );
    Ok(())
}

/// `codypendent skill add <dir>`: validate the skill package at `dir`, install a
/// copy under `<data_dir>/skills/<id>/`, and register it into the governed
/// registry so retrieval can disclose it to a run.
///
/// Self-contained like `index rebuild` — it opens the database directly rather
/// than requiring a running daemon, and the daemon's own startup scan re-walks
/// the same root on its next boot, so an install taken while the daemon is down
/// is picked up rather than lost.
///
/// A package declaring `scope = "repository"` anchors to the checkout the
/// OPERATOR is standing in, exactly as [`skill_new`] does — not to wherever the
/// package directory happens to sit. `dir` is a source to copy from and carries
/// no repository identity of its own: the 2026-08-13 review followed the
/// skill-writer's own printed promotion instruction (`skill add
/// <data_dir>/skills/<id>`), which anchored the promoted package to
/// `<data_dir>`'s path, so `draft` landed under the checkout and `active`
/// landed under an identity no run ever queries — an installed skill that
/// retrieval never disclosed, reported as nothing at all. Adding a package kept
/// outside the checkout (the natural place to keep one) failed the same way.
///
/// See [`crate::repo_anchor`] for the invariant this is one instance of.
///
/// A non-`active` package installs and registers, but is reported loudly: the
/// retrieval funnel hard-filters everything but Active, so a draft skill is
/// installed-but-never-disclosed, and that is precisely the failure mode the
/// only shipped package spent its life in.
pub async fn skill_add(paths: &RuntimePaths, dir: &std::path::Path) -> anyhow::Result<()> {
    paths.ensure_directories()?;
    let database_path = paths.data_dir.join("codypendent.db");
    let pool = knowledge_db::open(&database_path)
        .await
        .with_context(|| format!("opening {}", database_path.display()))?;

    let skills_root = user_skills_root(&paths.data_dir);
    let anchor = crate::repo_anchor::anchor_repository_id(&std::env::current_dir()?);
    let (item, installed) = install_package(&pool, dir, &skills_root, anchor)
        .await
        .with_context(|| format!("installing the skill package at {}", dir.display()))?;

    println!(
        "installed skill {} {} ({}) -> {}",
        item.name,
        item.version.0,
        item.scope.tier(),
        installed.display()
    );
    if !is_retrievable_status(item.status) {
        println!(
            "warning: status is {:?}, so retrieval will never disclose this skill \
             — set `status = \"active\"` in skill.toml and re-add it",
            item.status
        );
    }
    Ok(())
}

/// `codypendent skill new <ID> --name … --description … --procedure <FILE>`
/// (outcome 4): author a skill package from the command line and register it
/// through the SAME validate-and-install pipeline [`skill_add`] runs — a thin
/// dispatch over [`crate::skill_writer`], which owns the manifest rendering
/// and its round-trip tests.
///
/// It always lands as `draft`: [`SkillDraft`](crate::skill_writer::SkillDraft)
/// has no constructor that starts active, and retrieval hard-filters
/// everything but Active. So a newly authored skill is installed and
/// inspectable but never disclosed to a run until a human promotes it — the
/// review gate outcome 4 asks for, enforced by construction rather than by
/// remembering to check.
pub async fn skill_new(
    paths: &RuntimePaths,
    id: &str,
    name: &str,
    description: &str,
    scope: &str,
    procedure: &std::path::Path,
    directory: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    paths.ensure_directories()?;
    let body = std::fs::read_to_string(procedure)
        .with_context(|| format!("reading the procedure body {}", procedure.display()))?;

    // Derived exactly as `skill_add` derives it — through the one accessor, so
    // authoring and promoting a skill can never disagree about which checkout
    // it belongs to. A `repository`-scoped skill registered under any other
    // identity is invisible to every run.
    let anchor = crate::repo_anchor::anchor_repository_id(&std::env::current_dir()?);
    let scope = match scope {
        "user" => local_user_scope(),
        "repository" => Scope::Repository(anchor),
        other => anyhow::bail!("unknown scope {other:?}: expected \"user\" or \"repository\""),
    };
    let draft = crate::skill_writer::SkillDraft::new(id, name, scope, description, body);

    // Staging is throwaway — `author_and_install` copies the validated package
    // under `<data_dir>/skills/`, and that copy is what the registry points at.
    // Never a path built from `id`: only `install_package` vets it for
    // traversal, and that happens after the authoring write.
    let staging;
    let source_dir = match directory {
        Some(dir) => dir,
        None => {
            staging = tempfile::tempdir()
                .with_context(|| "creating a staging directory for the drafted package")?;
            staging.path()
        }
    };

    let database_path = paths.data_dir.join("codypendent.db");
    let pool = knowledge_db::open(&database_path)
        .await
        .with_context(|| format!("opening {}", database_path.display()))?;
    let skills_root = user_skills_root(&paths.data_dir);
    let (item, installed) =
        crate::skill_writer::author_and_install(&pool, source_dir, &skills_root, anchor, &draft)
            .await
            .with_context(|| format!("authoring the skill package {id}"))?;

    println!(
        "authored skill {} {} ({}) -> {}",
        item.name,
        item.version.0,
        item.scope.tier(),
        installed.display()
    );
    if !is_retrievable_status(item.status) {
        // The version bump is not optional advice: a same-version status flip
        // re-registers as `Modified`, not `Active`, because the registry
        // detects the changed hash (see `skill_writer`'s module doc).
        println!(
            "status is {:?}: registered, but retrieval will not disclose it until it is \
             promoted — in {}/skill.toml set `status = \"active\"`, bump `version`, then \
             re-run `codypendent skill add {}`",
            item.status,
            installed.display(),
            installed.display()
        );
    }
    Ok(())
}

/// `codypendent open <session> --in <ide>` (STEP 3.7). Print how the IDE should
/// attach to the session, then best-effort launch the editor with the session in
/// its environment. The IDE joins as a *contributor* to the SAME session — the
/// run is never restarted; the daemon publishes a `ClientPresenceChanged` so the
/// TUI shows the editor arriving. A missing editor binary is not an error: the
/// printed instructions still let a user attach manually.
pub async fn open(
    paths: &RuntimePaths,
    session_id: SessionId,
    ide_binary: &str,
    ide_name: &str,
    repo: PathBuf,
) -> anyhow::Result<()> {
    println!("{}", handoff_message(session_id, paths, ide_name));

    // Best-effort launch. The extension reads `CODYPENDENT_SESSION` to attach to
    // this exact session (rather than opening a fresh one).
    let launched = std::process::Command::new(ide_binary)
        .arg(&repo)
        .env("CODYPENDENT_SESSION", session_id.to_string())
        .env("CODYPENDENT_SOCKET", &paths.socket_path)
        .spawn();
    match launched {
        Ok(_) => println!("Launched {ide_name}."),
        Err(_) => println!(
            "Could not launch `{ide_binary}` (is it on PATH?). \
             Open {ide_name} yourself and attach to the session above."
        ),
    }
    Ok(())
}

/// `codypendent docs new <TITLE> [--from FILE] [--scope S]` — the CLI half of
/// document creation (rubric #4). Ensures a daemon, sends `CreateDocument` with
/// the current checkout as the repository (so a repository-scoped document
/// lands with the code it documents), and prints the id the daemon minted.
///
/// `--from` reads the Markdown here rather than passing a path, so the daemon
/// never opens a client-named file: the seed content crosses the socket as
/// data, which also works when the daemon runs elsewhere.
pub async fn docs_new(
    paths: &RuntimePaths,
    title: &str,
    from: Option<&std::path::Path>,
    scope: Option<String>,
) -> anyhow::Result<()> {
    let initial_markdown = match from {
        Some(path) => Some(
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?,
        ),
        None => None,
    };
    let repository = std::env::current_dir()
        .ok()
        .map(|dir| dir.to_string_lossy().into_owned());

    ensure_daemon(paths).await?;
    let mut conn = Connection::connect(&paths.socket_path)
        .await
        .with_context(|| "connecting to the daemon (is it running?)")?;
    conn.handshake("codypendent", env!("CARGO_PKG_VERSION"), None)
        .await?;
    bind_control_role(&mut conn).await?;

    let reply = conn
        .send_command(CommandBody::CreateDocument {
            title: title.to_string(),
            scope,
            repository,
            initial_markdown,
        })
        .await?;
    match reply.payload {
        Payload::DocumentCreated { document_id, .. } => {
            println!("Created \"{title}\" ({document_id}).");
            println!("Open it in the TUI's Docs Studio (D) to edit, review, or publish.");
            Ok(())
        }
        Payload::CommandRejected(error) => {
            anyhow::bail!(
                "daemon rejected the document: {} ({})",
                error.message,
                error.code
            )
        }
        other => anyhow::bail!("unexpected reply to CreateDocument: {other:?}"),
    }
}

/// `codypendent docs list` — the documents this checkout can see (repository +
/// system scope), newest first. Reads the daemon's database directly, the same
/// read-only projection seam `docs publish` and the TUI's Docs Studio use, so
/// listing never needs a running daemon.
pub async fn docs_list(paths: &RuntimePaths) -> anyhow::Result<()> {
    paths.ensure_directories()?;
    let database_path = paths.data_dir.join("codypendent.db");
    let pool = knowledge_db::open(&database_path)
        .await
        .with_context(|| format!("opening {}", database_path.display()))?;

    // The checkout, never the current directory: the daemon stores rows under
    // the Git toplevel, so hashing the directory as-opened listed nothing
    // whenever this was run from a subdirectory. `crate::repo_anchor` is the
    // one accessor for that resolution.
    let repository = crate::repo_anchor::anchor_repository_id(&std::env::current_dir()?);
    let scopes = [Scope::Repository(repository), Scope::System];
    let summaries = DocumentStore::new().list(&pool, &scopes).await?;
    if summaries.is_empty() {
        println!("No documents yet. Create one with `codypendent docs new \"<title>\"`.");
        return Ok(());
    }
    println!("{:<38} {:<10} {:>4}  TITLE", "ID", "STATUS", "REV");
    for summary in &summaries {
        println!(
            "{:<38} {:<10} {:>4}  {}",
            summary.id.to_string(),
            summary.status.as_str(),
            summary.revision,
            summary.title
        );
    }
    Ok(())
}

/// `codypendent docs check` — the on-demand `/update-docs` sweep. Ensures a
/// daemon (the sweep runs there, against the code graph it maintains) and
/// prints the finding counts it reports back.
pub async fn docs_check(paths: &RuntimePaths) -> anyhow::Result<()> {
    let repository = std::env::current_dir()
        .ok()
        .map(|dir| dir.to_string_lossy().into_owned());

    ensure_daemon(paths).await?;
    let mut conn = Connection::connect(&paths.socket_path)
        .await
        .with_context(|| "connecting to the daemon (is it running?)")?;
    conn.handshake("codypendent", env!("CARGO_PKG_VERSION"), None)
        .await?;
    bind_control_role(&mut conn).await?;

    let reply = conn
        .send_command(CommandBody::CheckDocuments {
            repository,
            session_id: None,
        })
        .await?;
    match reply.payload {
        Payload::DocsCheckCompleted {
            documents_checked,
            links_resolved,
            stale_findings,
            suggestions_filed,
            ..
        } => {
            println!("Checked {documents_checked} document(s); {links_resolved} symbol link(s) resolved.");
            if stale_findings == 0 {
                println!("No stale documentation found.");
            } else {
                println!(
                    "{stale_findings} stale finding(s); {suggestions_filed} suggestion(s) filed \
                     for review."
                );
                println!("Review them in the TUI's Docs Studio (D).");
            }
            Ok(())
        }
        Payload::CommandRejected(error) => {
            anyhow::bail!(
                "daemon rejected the check: {} ({})",
                error.message,
                error.code
            )
        }
        other => anyhow::bail!("unexpected reply to CheckDocuments: {other:?}"),
    }
}

/// `codypendent docs publish --target <T>` (Phase 4 STEP 4.4). Which Git
/// target to publish to, decoupled from `clap`'s `ValueEnum` derive (mirrors
/// `codypendent_knowledge::PublishTarget`'s three variants with CLI-friendly
/// names: `repo-file` / `docs-branch` / `doc-pr`).
pub enum PublishTargetKind {
    RepoFile,
    DocsBranch,
    DocPr,
}

/// `codypendent docs publish <DOCUMENT> --target <T>` (Phase 4 STEP 4.4,
/// closing the deferred "executing a `PublishPlan`" roadmap item).
///
/// Opens the daemon's database directly (the CLI projection seam — the same
/// read-only pattern the TUI's Docs view and `index rebuild` use) to load the
/// document and compute the exact deterministic plan the daemon itself will
/// compute, so the target/changed-files/Git-action preview printed here is
/// never a guess (STEP 4.4.2). After confirming (or `--yes`), ensures a
/// daemon, sends `PublishDocument` — which durably parks an approval and
/// replies with the parked plan the daemon computed independently — then
/// immediately resolves that approval with the confirmed decision over the
/// SAME connection (this CLI invocation is the human approver). A rejection
/// performs no write. On approval the daemon executes in the background, so
/// this polls the publication history briefly for the resulting commit before
/// reporting the outcome.
pub async fn docs_publish(
    paths: &RuntimePaths,
    document_id: DocumentId,
    target: PublishTargetKind,
    path: Option<String>,
    branch: Option<String>,
    title: Option<String>,
    yes: bool,
) -> anyhow::Result<()> {
    paths.ensure_directories()?;
    let database_path = paths.data_dir.join("codypendent.db");
    let pool = knowledge_db::open(&database_path)
        .await
        .with_context(|| format!("opening {}", database_path.display()))?;

    let doc = DocumentStore::new()
        .snapshot_document(&pool, document_id)
        .await
        .with_context(|| format!("loading document {document_id}"))?
        .ok_or_else(|| anyhow::anyhow!("no document {document_id}"))?;

    let resolved_target = resolve_publish_target(target, &doc.title, path, branch, title);
    let plan = plan_publication(&doc, resolved_target.clone());

    println!("Publish plan for \"{}\" ({document_id}):", doc.title);
    println!("  target: {}", describe_publish_target(&resolved_target));
    println!("  changed files:");
    for file in &plan.changed_files {
        println!("    {file}");
    }
    println!("  git action: {}", plan.git_action);

    let approved = if yes {
        true
    } else {
        print!("Publish? [y/N] ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    };

    let decision = if approved {
        ApprovalDecision::Approve
    } else {
        ApprovalDecision::Reject
    };

    ensure_daemon(paths).await?;
    let mut conn = Connection::connect(&paths.socket_path)
        .await
        .with_context(|| "connecting to the daemon (is it running?)")?;
    let approval_id = docs_publish_over_connection(
        &mut conn,
        document_id,
        to_wire_target(&resolved_target),
        decision,
    )
    .await?;
    println!("Parked approval {approval_id}.");

    if !approved {
        println!("Publish rejected; nothing was written.");
        return Ok(());
    }

    let existing = publications(&pool, document_id).await?.len();
    match wait_for_publish_outcome(&pool, document_id, approval_id, existing).await {
        PublishOutcome::Published(publication) => println!(
            "Published \"{}\" ({document_id}) -> commit {}",
            doc.title,
            publication.git_commit.as_deref().unwrap_or("(none)")
        ),
        // A terminal job state is a verdict, not a delay: telling the user to
        // re-run would loop them forever on a publish that already resolved.
        // Non-zero exit so a script can tell these from a success.
        PublishOutcome::Failed => anyhow::bail!(
            "Publish failed; nothing was written. The daemon recorded approval {approval_id} \
             as failed — see {} for the reason.",
            paths.log_dir.join("daemon.log").display()
        ),
        PublishOutcome::Cancelled => anyhow::bail!(
            "Publish was cancelled before it ran; nothing was written. Approval {approval_id} \
             was rejected or expired before the daemon executed it."
        ),
        PublishOutcome::StillRunning => println!(
            "Publish approved; the daemon is still executing it in the background. \
             Check the daemon log, or re-run `codypendent docs publish` shortly to see \
             the recorded commit."
        ),
    }
    Ok(())
}

/// The connected core of [`docs_publish`]: handshake, bind the `Controller`
/// role, send `PublishDocument`, then immediately resolve the parked approval
/// with `decision` over the SAME connection (this CLI invocation is the human
/// approver — the confirmation already happened before this is called).
/// Returns the parked [`ApprovalId`](codypendent_protocol::ApprovalId). Split
/// out so a test can drive it against a mock daemon, mirroring
/// [`workflow_run_over_connection`].
pub async fn docs_publish_over_connection(
    conn: &mut Connection,
    document_id: DocumentId,
    target: codypendent_protocol::document::PublishTarget,
    decision: ApprovalDecision,
) -> anyhow::Result<codypendent_protocol::ApprovalId> {
    conn.handshake("codypendent", env!("CARGO_PKG_VERSION"), None)
        .await?;
    bind_control_role(conn).await?;

    let reply = conn
        .send_command(CommandBody::PublishDocument {
            document_id,
            target,
        })
        .await?;
    let approval_id = match reply.payload {
        Payload::DocumentPublishRequested { approval_id, .. } => approval_id,
        Payload::CommandRejected(error) => {
            anyhow::bail!("publish rejected: {} ({})", error.message, error.code)
        }
        other => anyhow::bail!("unexpected reply to PublishDocument: {other:?}"),
    };

    let reply = conn
        .send_command(CommandBody::ResolveApproval {
            approval_id,
            decision,
            scope: ApprovalScope::Once,
        })
        .await?;
    match reply.payload {
        Payload::CommandAccepted { .. } => Ok(approval_id),
        Payload::CommandRejected(error) => {
            anyhow::bail!(
                "could not resolve the publish approval: {} ({})",
                error.message,
                error.code
            )
        }
        other => anyhow::bail!("unexpected reply to ResolveApproval: {other:?}"),
    }
}

/// Resolve `kind` (plus any explicit `--path`/`--branch`/`--title`) into the
/// knowledge engine's domain `PublishTarget`, filling in sensible defaults: a
/// slug of the document's title under `docs/` for `path`, `docs/publish` for
/// `branch`, and `Publish: <title>` for a PR's `title`.
fn resolve_publish_target(
    kind: PublishTargetKind,
    document_title: &str,
    path: Option<String>,
    branch: Option<String>,
    title: Option<String>,
) -> KnowledgePublishTarget {
    let path = path.unwrap_or_else(|| format!("docs/{}.md", slugify(document_title)));
    match kind {
        PublishTargetKind::RepoFile => KnowledgePublishTarget::RepositoryFile { path },
        PublishTargetKind::DocsBranch => {
            let branch = branch.unwrap_or_else(|| "docs/publish".to_string());
            KnowledgePublishTarget::DocsBranchCommit { branch, path }
        }
        PublishTargetKind::DocPr => {
            let branch = branch.unwrap_or_else(|| "docs/publish".to_string());
            let title = title.unwrap_or_else(|| format!("Publish: {document_title}"));
            KnowledgePublishTarget::DocumentationPr {
                branch,
                path,
                title,
            }
        }
    }
}

/// A short human description of a target, matching the daemon seam's own
/// `describe_target` (kept independent — the CLI's is a client-side preview
/// computed from the SAME plan function, not a value the daemon returns until
/// after `PublishDocument` is sent).
fn describe_publish_target(target: &KnowledgePublishTarget) -> String {
    match target {
        KnowledgePublishTarget::RepositoryFile { path } => format!("repository file {path}"),
        KnowledgePublishTarget::DocsBranchCommit { branch, path } => {
            format!("docs-branch commit {path} on {branch}")
        }
        KnowledgePublishTarget::DocumentationPr {
            branch,
            path,
            title,
        } => format!("documentation PR \"{title}\" ({path} on {branch})"),
    }
}

/// Convert the knowledge engine's domain `PublishTarget` into its wire mirror
/// for the `PublishDocument` command.
fn to_wire_target(
    target: &KnowledgePublishTarget,
) -> codypendent_protocol::document::PublishTarget {
    use codypendent_protocol::document::PublishTarget as Wire;
    match target {
        KnowledgePublishTarget::RepositoryFile { path } => {
            Wire::RepositoryFile { path: path.clone() }
        }
        KnowledgePublishTarget::DocsBranchCommit { branch, path } => Wire::DocsBranchCommit {
            branch: branch.clone(),
            path: path.clone(),
        },
        KnowledgePublishTarget::DocumentationPr {
            branch,
            path,
            title,
        } => Wire::DocumentationPr {
            branch: branch.clone(),
            path: path.clone(),
            title: title.clone(),
        },
    }
}

/// A filesystem/branch-safe slug: lowercased alphanumerics, runs of anything
/// else collapsed to a single `-`, with no leading/trailing dash. Never empty
/// (falls back to `document`).
fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "document".to_string()
    } else {
        slug
    }
}

/// What became of an approved publish, as read back from the daemon's own
/// database by [`wait_for_publish_outcome`].
enum PublishOutcome {
    /// The daemon recorded a publication row.
    Published(Box<Publication>),
    /// `document_publish_jobs.state = 'failed'` — the execution ran and lost.
    Failed,
    /// `document_publish_jobs.state = 'cancelled'` — the approval was rejected
    /// or expired before execution, so nothing ever ran.
    Cancelled,
    /// The bound elapsed with the job still pending or executing.
    StillRunning,
}

/// The recorded state of the publish job this invocation parked, or `None`
/// while the row is not yet readable. Keyed by `approval_id` — the table's
/// primary key, and the exact job this CLI invocation caused, so a concurrent
/// publish of the same document cannot be mistaken for ours.
async fn publish_job_state(
    pool: &sqlx::SqlitePool,
    approval_id: codypendent_protocol::ApprovalId,
) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT state FROM document_publish_jobs WHERE approval_id = ?")
        .bind(approval_id.to_string())
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
}

/// Poll for the outcome of an approved publish: a fresh publication row beyond
/// `existing_count`, or a terminal state for this approval's job. The daemon's
/// execution is fire-and-forget once the approval resolves, so a failure is
/// recorded ONLY in `document_publish_jobs` — watching publications alone left
/// a failed publish reporting "still executing, re-run shortly" forever
/// (2026-08-13 review F8). Gives up after a generous bound rather than hang.
async fn wait_for_publish_outcome(
    pool: &sqlx::SqlitePool,
    document_id: DocumentId,
    approval_id: codypendent_protocol::ApprovalId,
    existing_count: usize,
) -> PublishOutcome {
    for _ in 0..100 {
        // Publications first: the daemon records the row *before* it marks the
        // job `completed`, so a success is never reported as anything else.
        let Ok(published) = publications(pool, document_id).await else {
            // A read error is not a verdict — never claim a failure the
            // database did not state.
            return PublishOutcome::StillRunning;
        };
        if published.len() > existing_count {
            if let Some(publication) = published.into_iter().next() {
                return PublishOutcome::Published(Box::new(publication));
            }
        }
        match publish_job_state(pool, approval_id).await.as_deref() {
            Some("failed") => return PublishOutcome::Failed,
            Some("cancelled") => return PublishOutcome::Cancelled,
            // `pending`/`executing`/`completed` (whose publication row is
            // written first, so it was already caught above) — keep waiting.
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    PublishOutcome::StillRunning
}

/// `codypendent workflow validate <FILE> [--agents <DIR>]` (Phase 5 STEP 5.1):
/// parse and compile a declarative `workflow.yaml`, reporting either a one-line
/// summary of the validated graph or the precise error (naming the offending
/// step). Self-contained — it never touches the daemon; a manifest and its agent
/// profiles are just text on disk.
///
/// Without `--agents` this is **structural** validation: schema version,
/// unique/non-empty ids, exactly one action per step, resolvable + acyclic
/// dependencies, budget sanity, and the multi-agent `orchestration_reason` rule.
/// With `--agents <DIR>` it additionally **resolves agent roles**: every agent
/// step's short role must be fulfilled by a profile in that directory, so an
/// author catches a role with no profile before a run reaches it. (Whether a
/// named *tool* or *skill* exists still needs the live registry — a daemon-side
/// cross-check via `compile_with_registry`.)
pub fn workflow_validate(
    file: &std::path::Path,
    agents: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let yaml = std::fs::read_to_string(file)
        .with_context(|| format!("reading workflow manifest {}", file.display()))?;
    // A structural error is the user's to fix — surface it verbatim, tagged with
    // the file, and exit non-zero (via `?` in `main`).
    let compiled = codypendent_workflow::compile_yaml(&yaml)
        .map_err(|error| anyhow::anyhow!("{}: {error}", file.display()))?;
    println!("{}", workflow_summary(&compiled));

    if let Some(agents_dir) = agents {
        let profiles = codypendent_workflow::AgentProfileSet::load_dir(agents_dir)
            .with_context(|| format!("loading agent profiles from {}", agents_dir.display()))?;
        let unresolved = profiles.unresolved_roles(&compiled);
        if unresolved.is_empty() {
            println!(
                "\u{2713} agent roles: all resolved against {} ({} profile(s))",
                agents_dir.display(),
                profiles.len(),
            );
        } else {
            // Report every unresolved role so an author fixes them in one pass.
            let detail = unresolved
                .iter()
                .map(|r| format!("step `{}` \u{2192} role `{}`", r.step, r.role))
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!(
                "{}: {} agent role(s) unresolved against {}: {detail}",
                file.display(),
                unresolved.len(),
                agents_dir.display(),
            );
        }
    }
    Ok(())
}

/// `codypendent workflow show <FILE> [--json]` (Phase 5 STEP 5.2): compile a
/// manifest and print its full graph — every node's action, dependencies,
/// workspace, approval, retry, and declared outputs — as a human tree or, with
/// `--json`, the serialized [`CompiledWorkflow`] projection a graph-view client
/// consumes. Structural compilation only, like [`workflow_validate`]; a compile
/// error is surfaced verbatim and exits non-zero.
pub fn workflow_show(file: &std::path::Path, json: bool) -> anyhow::Result<()> {
    let yaml = std::fs::read_to_string(file)
        .with_context(|| format!("reading workflow manifest {}", file.display()))?;
    let compiled = codypendent_workflow::compile_yaml(&yaml)
        .map_err(|error| anyhow::anyhow!("{}: {error}", file.display()))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&compiled)?);
    } else {
        print!("{}", workflow_tree(&compiled));
    }
    Ok(())
}

/// `codypendent workflow run <FILE> [--inputs <JSON>] [--repo <PATH>]` (Phase 5
/// STEP 5.2): start a durable workflow run. Ensures a daemon, sends `StartWorkflow`,
/// and prints the new run id the daemon drives to a terminal state in the
/// background. The manifest content (never a path) is what crosses the wire,
/// `--inputs` is parsed as a JSON value the manifest's typed inputs bind to, and
/// `repo`'s canonical path is carried so the daemon carves each writing node its
/// own isolated worktree from *this* checkout (Phase 5 T5) — mirroring how the
/// `run` command carries `StartRun`'s repository.
pub async fn workflow_run(
    paths: &RuntimePaths,
    file: &std::path::Path,
    inputs: Option<String>,
    repo: PathBuf,
) -> anyhow::Result<()> {
    let manifest = std::fs::read_to_string(file)
        .with_context(|| format!("reading workflow manifest {}", file.display()))?;
    let inputs = match inputs {
        Some(text) => {
            serde_json::from_str(&text).with_context(|| "parsing --inputs as a JSON value")?
        }
        None => serde_json::Value::Null,
    };
    let repo = repo.canonicalize().with_context(|| {
        format!(
            "--repo {}: not a valid, accessible directory",
            repo.display()
        )
    })?;
    if !repo.is_dir() {
        anyhow::bail!("--repo {}: not a directory", repo.display());
    }
    let repository = repo.to_string_lossy().into_owned();
    ensure_daemon(paths).await?;
    let mut conn = Connection::connect(&paths.socket_path).await?;
    let run_id =
        workflow_run_over_connection(&mut conn, manifest, inputs, Some(repository)).await?;
    println!("workflow run started: {run_id}");
    Ok(())
}

/// The connected core of [`workflow_run`]: handshake, bind the `Controller` role,
/// send `StartWorkflow`, and return the new run id. Split out so a test can drive it
/// against a mock server (like [`run_over_connection`]). `repository` is the
/// canonical repo root the run's agent nodes operate on (Phase 5 T5); `None` lets
/// the daemon fall back to its startup repository.
pub async fn workflow_run_over_connection(
    conn: &mut Connection,
    manifest: String,
    inputs: serde_json::Value,
    repository: Option<String>,
) -> anyhow::Result<String> {
    conn.handshake("codypendent", env!("CARGO_PKG_VERSION"), None)
        .await?;
    bind_control_role(conn).await?;
    let reply = conn
        .send_command(CommandBody::StartWorkflow {
            manifest,
            workflow_id: None,
            inputs,
            repository,
        })
        .await?;
    match reply.payload {
        Payload::WorkflowRunStarted {
            workflow_run_id, ..
        } => Ok(workflow_run_id),
        Payload::CommandRejected(error) => {
            anyhow::bail!("StartWorkflow rejected: {} ({})", error.message, error.code)
        }
        other => anyhow::bail!("unexpected reply to StartWorkflow: {other:?}"),
    }
}

/// `codypendent fix-ci --pr <N> [--repo PATH]` (Phase 5 STEP 5.1.4). Runs the
/// declarative `repair-github-check` workflow — the supervised investigator →
/// implementer → independent-reviewer flow that replaced the Phase-3 hard-coded
/// `/fix-ci` objective. The daemon resolves the workflow from its own sources
/// (the embedded built-in, shadowable by `<repo>/.codypendent/workflows`), so a
/// fresh checkout needs no manifest file. Every GitHub write still parks for
/// durable approval; without a GitHub token the run fails with the same
/// `github is not configured` error the prompt flow gave, at its first check read.
pub async fn fix_ci(paths: &RuntimePaths, pr: u64, repo: PathBuf) -> anyhow::Result<()> {
    let repo = repo.canonicalize().with_context(|| {
        format!(
            "--repo {}: not a valid, accessible directory",
            repo.display()
        )
    })?;
    if !repo.is_dir() {
        anyhow::bail!("--repo {}: not a directory", repo.display());
    }
    let repository = repo.to_string_lossy().into_owned();
    ensure_daemon(paths).await?;
    let mut conn = Connection::connect(&paths.socket_path).await?;
    let run_id = fix_ci_over_connection(&mut conn, pr, Some(repository)).await?;
    println!("workflow run started: {run_id}");
    Ok(())
}

/// The connected core of [`fix_ci`]: handshake, bind `Controller`, and start the
/// named `repair-github-check` workflow with the PR number as its input (Phase 5
/// STEP 5.1.4). Split out so a test can drive it against a mock server, mirroring
/// [`workflow_run_over_connection`]. Sends no inline manifest — the daemon resolves
/// the workflow by id, enforcing the source registry's version + shadowing rules.
pub async fn fix_ci_over_connection(
    conn: &mut Connection,
    pr: u64,
    repository: Option<String>,
) -> anyhow::Result<String> {
    conn.handshake("codypendent", env!("CARGO_PKG_VERSION"), None)
        .await?;
    bind_control_role(conn).await?;
    let reply = conn
        .send_command(CommandBody::StartWorkflow {
            manifest: String::new(),
            workflow_id: Some(codypendent_workflow::REPAIR_GITHUB_CHECK_ID.to_string()),
            inputs: serde_json::json!({ "pull_request": pr }),
            repository,
        })
        .await?;
    match reply.payload {
        Payload::WorkflowRunStarted {
            workflow_run_id, ..
        } => Ok(workflow_run_id),
        Payload::CommandRejected(error) => {
            anyhow::bail!("/fix-ci rejected: {} ({})", error.message, error.code)
        }
        other => anyhow::bail!("unexpected reply to /fix-ci StartWorkflow: {other:?}"),
    }
}

/// `codypendent workflow pause <RUN_ID>` (Phase 5 STEP 5.2).
pub async fn workflow_pause(paths: &RuntimePaths, workflow_run_id: String) -> anyhow::Result<()> {
    lifecycle_command(
        paths,
        CommandBody::PauseWorkflow { workflow_run_id },
        "pause",
    )
    .await
}

/// `codypendent workflow resume <RUN_ID>` (Phase 5 STEP 5.2).
pub async fn workflow_resume(paths: &RuntimePaths, workflow_run_id: String) -> anyhow::Result<()> {
    lifecycle_command(
        paths,
        CommandBody::ResumeWorkflow { workflow_run_id },
        "resume",
    )
    .await
}

/// `codypendent workflow retry <RUN_ID> --node <NODE>` (Phase 5 STEP 5.2).
pub async fn workflow_retry(
    paths: &RuntimePaths,
    workflow_run_id: String,
    node: String,
) -> anyhow::Result<()> {
    lifecycle_command(
        paths,
        CommandBody::RetryWorkflowNode {
            workflow_run_id,
            node_id: node,
        },
        "retry",
    )
    .await
}

/// `codypendent workflow cancel <RUN_ID>` (Phase 5 T9). A cooperative drain — the
/// driver stops launching new nodes, any in-flight node's agent run is interrupted,
/// pending nodes are skipped, and the run lands cancelled (terminal — no resume).
pub async fn workflow_cancel(paths: &RuntimePaths, workflow_run_id: String) -> anyhow::Result<()> {
    lifecycle_command(
        paths,
        CommandBody::CancelWorkflow { workflow_run_id },
        "cancel",
    )
    .await
}

/// `codypendent workflow watch <RUN_ID>` (Phase 5 T9): print the run's current
/// observability snapshot, then stream each node transition + run-phase change until
/// the run reaches a terminal phase. Attaches a `Subscription::Workflow` forwarder to
/// a throwaway session (the connection-level anchor a per-run subscription needs),
/// reads the snapshot as the baseline over the SAME connection, then folds the live
/// stream — the catch-up/idempotency contract (subscribe-then-snapshot, merge by
/// node id). Does not start a daemon: watching only makes sense against a live run.
pub async fn workflow_watch(paths: &RuntimePaths, workflow_run_id: String) -> anyhow::Result<()> {
    let mut conn = Connection::connect(&paths.socket_path)
        .await
        .with_context(|| "connecting to the daemon (is it running?)")?;
    let mut stdout = std::io::stdout();
    workflow_watch_over_connection(&mut conn, workflow_run_id, &mut stdout).await
}

/// The connected core of [`workflow_watch`], split out for testability like
/// [`workflow_run_over_connection`]: handshake, create + attach a throwaway session
/// carrying the `Subscription::Workflow` forwarder, read the snapshot baseline, then
/// stream live events until a terminal run phase.
pub async fn workflow_watch_over_connection<W: Write>(
    conn: &mut Connection,
    workflow_run_id: String,
    out: &mut W,
) -> anyhow::Result<()> {
    conn.handshake("codypendent", env!("CARGO_PKG_VERSION"), None)
        .await?;

    // A per-run subscription forwarder spawns only on a valid session attach (the
    // connection-level anchor); create a throwaway session as the Observer to carry
    // it. Subscribe BEFORE reading the snapshot so no transition is missed between
    // the read and the subscribe (the catch-up contract).
    let create_reply = conn
        .send_command(CommandBody::CreateSession {
            workspace: WorkspaceId::new(),
            title: format!("watch {workflow_run_id}"),
            // Throwaway session (only a connection-level anchor for the
            // per-run subscription forwarder): no repo context to carry.
            repository: None,
        })
        .await?;
    let session_id = match &create_reply.payload {
        Payload::CommandAccepted { .. } => create_reply.session_id.ok_or_else(|| {
            anyhow::anyhow!("daemon accepted CreateSession but sent no session id")
        })?,
        Payload::CommandRejected(error) => {
            anyhow::bail!("CreateSession rejected: {} ({})", error.message, error.code)
        }
        other => anyhow::bail!("unexpected reply to CreateSession: {other:?}"),
    };
    conn.send_command(CommandBody::AttachSession {
        session_id,
        last_seen_sequence: None,
        subscriptions: vec![Subscription::Workflow {
            workflow_run_id: workflow_run_id.clone(),
        }],
        requested_role: ClientRole::Observer,
        repository: None,
    })
    .await?;

    // The catch-up baseline: the run's current phase + every node's view.
    let snapshot_reply = conn
        .send_command(CommandBody::ReadWorkflowRun {
            workflow_run_id: workflow_run_id.clone(),
        })
        .await?;
    match snapshot_reply.payload {
        Payload::WorkflowRunSnapshot { snapshot, .. } => {
            render_workflow_snapshot(out, &snapshot)?;
            if snapshot_phase_is_terminal(snapshot.phase) {
                // Already finished — nothing to stream.
                return Ok(());
            }
        }
        Payload::CommandRejected(error) => {
            anyhow::bail!(
                "ReadWorkflowRun rejected: {} ({})",
                error.message,
                error.code
            )
        }
        other => anyhow::bail!("unexpected reply to ReadWorkflowRun: {other:?}"),
    }

    // Fold the live stream until a terminal run phase (or the daemon closes).
    while let Some(envelope) = conn.next_envelope().await? {
        if let Payload::WorkflowEvent { event } = envelope.payload {
            // Route only this run's events (the frame is not session-scoped).
            if workflow_event_run_id(&event) != Some(workflow_run_id.as_str()) {
                continue;
            }
            render_workflow_event(out, &event)?;
            if let WorkflowEvent::RunPhaseChanged { phase, .. } = &event {
                if snapshot_phase_is_terminal(*phase) {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

/// Whether a run phase is terminal (the watch stream ends there).
fn snapshot_phase_is_terminal(phase: WorkflowRunPhase) -> bool {
    matches!(
        phase,
        WorkflowRunPhase::Completed | WorkflowRunPhase::Failed | WorkflowRunPhase::Cancelled
    )
}

/// The run id a live workflow event belongs to.
fn workflow_event_run_id(event: &WorkflowEvent) -> Option<&str> {
    match event {
        WorkflowEvent::NodeTransitioned(view) => Some(view.workflow_run_id.as_str()),
        WorkflowEvent::RunPhaseChanged {
            workflow_run_id, ..
        } => Some(workflow_run_id.as_str()),
        _ => None,
    }
}

/// Print a run snapshot as a human header + one line per node.
fn render_workflow_snapshot<W: Write>(
    out: &mut W,
    snapshot: &WorkflowRunSnapshot,
) -> anyhow::Result<()> {
    writeln!(
        out,
        "workflow run {} — {}",
        snapshot.workflow_run_id,
        run_phase_label(snapshot.phase)
    )?;
    for node in &snapshot.nodes {
        writeln!(out, "  {}", node_view_line(node))?;
    }
    Ok(())
}

/// Print one live workflow event as a human line.
fn render_workflow_event<W: Write>(out: &mut W, event: &WorkflowEvent) -> anyhow::Result<()> {
    match event {
        WorkflowEvent::NodeTransitioned(view) => writeln!(out, "  {}", node_view_line(view))?,
        WorkflowEvent::RunPhaseChanged { phase, .. } => {
            writeln!(out, "run {}", run_phase_label(*phase))?
        }
        _ => {}
    }
    Ok(())
}

/// A one-line human rendering of a node's view (state, attempt, cost, error).
fn node_view_line(view: &WorkflowNodeView) -> String {
    let mut line = format!("{}: {}", view.node_id, node_state_label(view));
    if view.attempt > 1 {
        line.push_str(&format!(" (attempt {})", view.attempt));
    }
    if let Some(cost) = &view.cost {
        if let Some(rendered) = render_cost(cost) {
            line.push_str(&format!(" · {rendered}"));
        }
    }
    if let Some(error) = &view.error {
        line.push_str(&format!(" — {error}"));
    }
    line
}

/// The wire node state as a lowercase label.
fn node_state_label(view: &WorkflowNodeView) -> &'static str {
    use codypendent_protocol::WorkflowNodeState::*;
    match view.state {
        Pending => "pending",
        Running => "running",
        WaitingApproval => "waiting_approval",
        Blocked => "blocked",
        Completed => "completed",
        Failed => "failed",
        Skipped => "skipped",
        _ => "unknown",
    }
}

/// The wire run phase as a lowercase label.
fn run_phase_label(phase: WorkflowRunPhase) -> &'static str {
    match phase {
        WorkflowRunPhase::Pending => "pending",
        WorkflowRunPhase::Running => "running",
        WorkflowRunPhase::Paused => "paused",
        WorkflowRunPhase::Completed => "completed",
        WorkflowRunPhase::Failed => "failed",
        WorkflowRunPhase::Cancelled => "cancelled",
        _ => "unknown",
    }
}

/// Render a node's measured cost JSON (`wall_time_secs`, `tool_calls`,
/// `tokens`, `cost_micros`) as a human string, or `None` when empty. Only
/// measured dimensions — never a fabricated token/USD figure (T8).
fn render_cost(cost: &serde_json::Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(secs) = cost.get("wall_time_secs").and_then(|v| v.as_u64()) {
        parts.push(format!("{secs}s"));
    }
    if let Some(calls) = cost.get("tool_calls").and_then(|v| v.as_u64()) {
        let unit = if calls == 1 {
            "tool call"
        } else {
            "tool calls"
        };
        parts.push(format!("{calls} {unit}"));
    }
    // Measured-only, exactly like the producer (`NodeCost::to_json` omits an
    // unmeasured dimension): an absent key prints nothing rather than a zero
    // that would read as "this node was free".
    if let Some(tokens) = cost.get("tokens").and_then(|v| v.as_u64()) {
        parts.push(format!("{tokens} tokens"));
    }
    if let Some(micros) = cost.get("cost_micros").and_then(|v| v.as_u64()) {
        parts.push(format!("${:.4}", micros as f64 / 1_000_000.0));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

/// Send one workflow lifecycle command to a *running* daemon (it does not start
/// one — pausing/resuming/retrying only makes sense against live durable runs) and
/// report whether it was accepted. `verb` names the action in the output/errors.
async fn lifecycle_command(
    paths: &RuntimePaths,
    body: CommandBody,
    verb: &str,
) -> anyhow::Result<()> {
    let mut conn = Connection::connect(&paths.socket_path)
        .await
        .with_context(|| "connecting to the daemon (is it running?)")?;
    conn.handshake("codypendent", env!("CARGO_PKG_VERSION"), None)
        .await?;
    bind_control_role(&mut conn).await?;
    let reply = conn.send_command(body).await?;
    match reply.payload {
        Payload::CommandAccepted { .. } => {
            println!("workflow {verb} accepted");
            Ok(())
        }
        Payload::CommandRejected(error) => {
            anyhow::bail!(
                "workflow {verb} rejected: {} ({})",
                error.message,
                error.code
            )
        }
        other => anyhow::bail!("unexpected reply to workflow {verb}: {other:?}"),
    }
}

/// Bind this connection to the `Controller` role, which starting and controlling a
/// workflow requires. Roles bind at the connection level via an `AttachSession`
/// (Chapter 03); a workflow lives outside any session, so we attach to a throwaway
/// session id purely for the role — the daemon binds the role before it checks the
/// session, so the expected `session-not-found` rejection is irrelevant and ignored.
async fn bind_control_role(conn: &mut Connection) -> anyhow::Result<()> {
    conn.send_command(CommandBody::AttachSession {
        session_id: SessionId::new(),
        last_seen_sequence: None,
        subscriptions: vec![],
        requested_role: ClientRole::Controller,
        // Role-bootstrap-only attach to a throwaway session id: no repo context.
        repository: None,
    })
    .await?;
    Ok(())
}

// --- STEP 7.1: eval harness runner -------------------------------------------

/// `codypendent eval run --suite <NAME> [--policy P] [--candidate-id ID] --report out.json`
/// (Phase 7 STEP 7.1; `--policy` closed by the "routing⇄eval composition"
/// follow-up). Loads every case in the suite, runs each headlessly against
/// its pinned fixture revision, scores it, and writes the aggregate
/// [`codypendent_eval::SuiteReport`] to `--report`.
///
/// When `--policy P` is given, [`crate::eval::route_cases`] resolves EVERY
/// case's model through `codypendent-routing` over the persisted
/// `model_profiles`, fail-closed: an unrecognized policy name, an empty
/// profile store, or a case the router refuses to route all stop this
/// BEFORE any case runs, with a non-zero exit and a clear message — never a
/// silent fallback to the default model for a policy that was explicitly
/// requested. The resolved model is additively recorded per case in the
/// report (see [`crate::eval::report_json_with_routing`]) **and** pinned into
/// that same case's own `StartRun.model` (see [`crate::eval::run_suite`]) —
/// both are fed the SAME routing result below, so the model the report says
/// ran is always the model that actually ran (see `crate::eval`'s module doc
/// for the classification-safety argument that makes this pin safe). When
/// `--policy` is absent, behavior is byte-for-byte unchanged — every case
/// still sends `model: None` and the daemon resolves/routes as usual (the
/// `eval-smoke` CI path).
///
/// `--candidate-id` turns the resulting report into durable promotion evidence:
/// the candidate must exist before the run, the core suite is mandatory, and a
/// router candidate must run under its named policy. Reports without this flag
/// remain ordinary output files and cannot advance a later candidate.
pub async fn eval_run(
    paths: &RuntimePaths,
    suite: &str,
    policy: Option<String>,
    candidate_id: Option<&str>,
    report: &std::path::Path,
) -> anyhow::Result<()> {
    let suite_dir = crate::eval::resolve_suite_dir(suite)?;
    let cases = crate::eval::load_suite(&suite_dir)?;

    // Promotion evidence must name its candidate BEFORE any cases execute. The
    // bound artifact snapshot is copied onto the report row and re-checked by
    // the daemon at advancement, so neither a stale global report nor a report
    // for another artifact can be substituted later.
    let promotion_target = if let Some(candidate_id) = candidate_id {
        if suite != "core" {
            anyhow::bail!("promotion regression evidence must run the `core` suite, got `{suite}`");
        }
        let pool =
            codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db")).await?;
        let artifact: Option<(String, String, i64)> = sqlx::query_as(
            "SELECT artifact_kind, artifact_name, artifact_version \
             FROM promotion_candidates WHERE id = ?",
        )
        .bind(candidate_id)
        .fetch_optional(&pool)
        .await
        .context("loading promotion candidate for eval evidence")?;
        let Some((kind, name, version)) = artifact else {
            anyhow::bail!("no such promotion candidate: {candidate_id}");
        };
        if kind == "router" && policy.as_deref() != Some(name.as_str()) {
            anyhow::bail!(
                "router candidate `{name}` requires `eval run --policy {name}` so the report \
                 exercises the candidate policy"
            );
        }
        Some((pool, candidate_id.to_string(), kind, name, version))
    } else {
        None
    };
    println!(
        "eval: loaded {} case(s) from {}{}",
        cases.len(),
        suite_dir.display(),
        policy
            .as_deref()
            .map(|p| format!(
                " (policy: {p} — each case routed via Phase-7 routing, fail-closed; \
                 selection recorded per case in the report)"
            ))
            .unwrap_or_default()
    );

    // Phase-7 routing⇄eval composition: resolve every case's model BEFORE any
    // case runs, so a misconfigured `--policy` (unknown name, no measured
    // profiles, or a case the router refuses) fails this command cleanly
    // rather than letting some cases run unrouted. `None` when `--policy` is
    // absent — the unchanged path every existing test/CI job exercises.
    let routed = match policy.as_deref() {
        Some(name) => Some(crate::eval::route_cases(paths, &cases, name).await?),
        None => None,
    };

    let fixture_root = crate::eval::fixture_root(&suite_dir, "tiny-crate")?;
    // The SAME `routed` result pins each case's `StartRun.model` below AND
    // feeds the report's `routed_model` field — one source of truth, so the
    // report can never misattribute which model ran (see `crate::eval`'s
    // module doc).
    let suite_report =
        crate::eval::run_suite(paths, &cases, &fixture_root, routed.as_deref()).await?;

    // Only an explicitly bound run becomes durable promotion evidence. An
    // ordinary eval still writes its requested report file, but it cannot be
    // consumed by a later, unrelated promotion candidate.
    if let Some((pool, candidate_id, kind, name, version)) = promotion_target {
        sqlx::query(
            "INSERT INTO eval_suite_reports \
             (id, candidate_id, artifact_kind, artifact_name, artifact_version, suite, \
              routing_policy, report_json, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        )
        .bind(codypendent_protocol::MessageId::new().to_string())
        .bind(candidate_id)
        .bind(kind)
        .bind(name)
        .bind(version)
        .bind(suite)
        .bind(policy.as_deref().unwrap_or("daemon-default"))
        .bind(serde_json::to_string(&suite_report)?)
        .execute(&pool)
        .await
        .context("persisting candidate-bound eval suite evidence for promotion")?;
    }

    let json = crate::eval::report_json_with_routing(&suite_report, routed.as_deref())?;
    std::fs::write(report, json)
        .with_context(|| format!("writing suite report to {}", report.display()))?;

    let passed = suite_report.results.iter().filter(|r| r.passed()).count();
    println!(
        "eval: {passed}/{} case(s) passed ({:.0}%); report written to {}",
        suite_report.results.len(),
        suite_report.success_rate() * 100.0,
        report.display()
    );
    if !suite_report.all_passed() {
        anyhow::bail!(
            "eval suite did not pass: failed case(s): {}",
            suite_report.failed_case_ids().join(", ")
        );
    }
    Ok(())
}

// --- STEP 7.5: promotion pipeline commands ----------------------------------

/// `codypendent promote propose --kind K --name NAME --version N
/// [--requires-permission-review]` (Phase 7 STEP 7.5). Ensures a daemon (like
/// `workflow run`, this is the "creation" verb) and prints the new candidate id.
pub async fn promote_propose(
    paths: &RuntimePaths,
    kind: String,
    name: String,
    version: u32,
    requires_permission_review: bool,
) -> anyhow::Result<()> {
    ensure_daemon(paths).await?;
    let mut conn = Connection::connect(&paths.socket_path).await?;
    let candidate_id =
        promote_propose_over_connection(&mut conn, kind, name, version, requires_permission_review)
            .await?;
    println!("promotion candidate proposed: {candidate_id}");
    Ok(())
}

/// The connected core of [`promote_propose`]: handshake, bind the `Controller`
/// role, send `ProposePromotion`, and return the new candidate id. Split out
/// for the same testability reason as [`workflow_run_over_connection`].
pub async fn promote_propose_over_connection(
    conn: &mut Connection,
    kind: String,
    name: String,
    version: u32,
    requires_permission_review: bool,
) -> anyhow::Result<String> {
    conn.handshake("codypendent", env!("CARGO_PKG_VERSION"), None)
        .await?;
    bind_control_role(conn).await?;
    let reply = conn
        .send_command(CommandBody::ProposePromotion {
            kind,
            name,
            version,
            requires_permission_review,
        })
        .await?;
    match reply.payload {
        Payload::PromotionProposed { candidate_id, .. } => Ok(candidate_id),
        Payload::CommandRejected(error) => {
            anyhow::bail!("propose rejected: {} ({})", error.message, error.code)
        }
        other => anyhow::bail!("unexpected reply to ProposePromotion: {other:?}"),
    }
}

/// `codypendent promote advance <CANDIDATE_ID> --step <STEP>` (Phase 7 STEP
/// 7.5). Evidence-bearing actions carry metrics in `PromotionAction`.
pub async fn promote_advance(
    paths: &RuntimePaths,
    candidate_id: String,
    action: PromotionAction,
) -> anyhow::Result<()> {
    promotion_command(
        paths,
        CommandBody::AdvancePromotion {
            candidate_id,
            action,
        },
        "advance",
    )
    .await
}

/// `codypendent promote approve <CANDIDATE_ID>` — the human-approval gate
/// (Phase 7 STEP 7.5, ADR-010). This command does not start a daemon: approving
/// only makes sense against an already-running one with a real candidate.
pub async fn promote_approve(paths: &RuntimePaths, candidate_id: String) -> anyhow::Result<()> {
    promotion_command(
        paths,
        CommandBody::ApprovePromotion { candidate_id },
        "approve",
    )
    .await
}

/// `codypendent promote rollback <CANDIDATE_ID>` (Phase 7 STEP 7.5).
pub async fn promote_rollback(paths: &RuntimePaths, candidate_id: String) -> anyhow::Result<()> {
    promotion_command(
        paths,
        CommandBody::RollbackPromotion { candidate_id },
        "rollback",
    )
    .await
}

/// Send one promotion command to a *running* daemon (mirrors
/// [`lifecycle_command`]: advancing/approving/rolling back only makes sense
/// against a daemon that already exists) and report whether it was accepted.
async fn promotion_command(
    paths: &RuntimePaths,
    body: CommandBody,
    verb: &str,
) -> anyhow::Result<()> {
    let mut conn = Connection::connect(&paths.socket_path)
        .await
        .with_context(|| "connecting to the daemon (is it running?)")?;
    promotion_command_over_connection(&mut conn, body, verb).await
}

/// The connected core of [`promotion_command`]: handshake, bind the
/// `Controller` role, send `body`, and report whether it was accepted. Split
/// out (and `pub`, like [`workflow_run_over_connection`]) so a test can drive
/// it against a hand-rolled mock daemon.
pub async fn promotion_command_over_connection(
    conn: &mut Connection,
    body: CommandBody,
    verb: &str,
) -> anyhow::Result<()> {
    conn.handshake("codypendent", env!("CARGO_PKG_VERSION"), None)
        .await?;
    bind_control_role(conn).await?;
    let reply = conn.send_command(body).await?;
    match reply.payload {
        Payload::CommandAccepted { .. } => {
            println!("promotion {verb} accepted");
            Ok(())
        }
        Payload::CommandRejected(error) => {
            anyhow::bail!(
                "promotion {verb} rejected: {} ({})",
                error.message,
                error.code
            )
        }
        other => anyhow::bail!("unexpected reply to promotion {verb}: {other:?}"),
    }
}

/// A human, indented rendering of a compiled workflow graph. Pure, so it is tested
/// directly. Nodes are listed in topological order; each shows its action and the
/// execution-affecting settings that are set.
fn workflow_tree(compiled: &codypendent_workflow::CompiledWorkflow) -> String {
    use codypendent_workflow::NodeAction;
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} v{} ({} step(s), {} agent step(s))",
        compiled.id,
        compiled.version,
        compiled.nodes.len(),
        compiled.agent_node_count()
    );
    for node in &compiled.nodes {
        let action = match &node.action {
            NodeAction::Agent { role, skill, .. } => match skill {
                Some(skill) => format!("agent {role} · skill {skill}"),
                None => format!("agent {role}"),
            },
            NodeAction::Tool { name } => format!("tool {name}"),
        };
        let _ = writeln!(out, "  - {} [{action}]", node.id);
        if !node.depends_on.is_empty() {
            let _ = writeln!(out, "      depends_on: {}", node.depends_on.join(", "));
        }
        if let Some(approval) = &node.approval {
            let _ = writeln!(out, "      approval: {approval:?}");
        }
        if !node.outputs.is_empty() {
            let _ = writeln!(out, "      outputs: {}", node.outputs.join(", "));
        }
    }
    out
}

/// A one-line human summary of a validated workflow graph. Pure, so it is tested
/// directly.
fn workflow_summary(compiled: &codypendent_workflow::CompiledWorkflow) -> String {
    let order: Vec<&str> = compiled.nodes.iter().map(|node| node.id.as_str()).collect();
    format!(
        "\u{2713} {} v{} valid — {} step(s), {} agent step(s); order: {}",
        compiled.id,
        compiled.version,
        compiled.nodes.len(),
        compiled.agent_node_count(),
        order.join(" \u{2192} "),
    )
}

/// `codypendent plugin inspect <FILE>` (Phase 6 STEP 6.1): parse a `plugin.toml`
/// and render its identity, requested capabilities, resource caps, and trust
/// posture — the "evaluate permissions (render the capability list to the user)"
/// step, before a plugin is ever enabled. Manifest parsing only; nothing runs.
pub fn plugin_inspect(file: &std::path::Path) -> anyhow::Result<()> {
    let toml = std::fs::read_to_string(file)
        .with_context(|| format!("reading plugin manifest {}", file.display()))?;
    let manifest = codypendent_sandbox::parse_manifest(&toml)
        .map_err(|error| anyhow::anyhow!("{}: {error}", file.display()))?;
    print!("{}", plugin_report(&manifest));
    Ok(())
}

/// `codypendent plugin diff <INSTALLED> <UPDATE>` (Phase 6 STEP 6.1): parse both
/// manifests, print the permission diff (capabilities **and** resource caps —
/// P6-A), and report whether the update expands permissions and so requires
/// re-approval (exit criterion 2). Exits non-zero when the update expands
/// permissions, so a caller (or CI) can gate on it.
pub fn plugin_diff(installed: &std::path::Path, update: &std::path::Path) -> anyhow::Result<()> {
    let installed_manifest = read_manifest(installed)?;
    let update_manifest = read_manifest(update)?;
    if installed_manifest.id != update_manifest.id {
        anyhow::bail!(
            "these are different plugins ({} vs {}); a diff compares versions of one plugin",
            installed_manifest.id,
            update_manifest.id
        );
    }
    // diff_manifests() folds resource-cap changes in alongside the capability
    // diff (P6-A) — a bare CapabilitySet::diff_to() here would miss a raised
    // memory/cpu/wall/output cap entirely, since it has no resource data to
    // compare, letting this CI re-approval gate print "safe" and exit 0 on
    // exactly the update it exists to catch.
    let diff = codypendent_sandbox::diff_manifests(&installed_manifest, &update_manifest);
    print!("{}", plugin_diff_report(&installed_manifest.id, &diff));
    if diff.expands_permissions() {
        // A widening update is not applied without re-approval — signal that with a
        // non-zero exit so automation blocks on it.
        anyhow::bail!("update expands permissions — re-approval required before it can be applied");
    }
    Ok(())
}

fn read_manifest(file: &std::path::Path) -> anyhow::Result<codypendent_sandbox::PluginManifest> {
    let toml = std::fs::read_to_string(file)
        .with_context(|| format!("reading plugin manifest {}", file.display()))?;
    codypendent_sandbox::parse_manifest(&toml)
        .map_err(|error| anyhow::anyhow!("{}: {error}", file.display()))
}

/// A human rendering of a plugin manifest's identity, capabilities, resources, and
/// trust posture. Pure, so it is tested directly.
fn plugin_report(manifest: &codypendent_sandbox::PluginManifest) -> String {
    use codypendent_sandbox::CapabilitySet;
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} v{} ({}) — publisher {}",
        manifest.id,
        manifest.version,
        manifest.kind.as_str(),
        manifest.publisher,
    );
    let trust = if manifest.security.is_signed() {
        "signed"
    } else {
        "unsigned"
    };
    let checksum = if manifest.security.checksum.is_empty() {
        "no checksum"
    } else {
        manifest.security.checksum.as_str()
    };
    let profile = if manifest.security.sandbox_profile.is_empty() {
        "(none)"
    } else {
        manifest.security.sandbox_profile.as_str()
    };
    let _ = writeln!(
        out,
        "  trust: {trust} ({checksum}), sandbox profile {profile}"
    );

    let caps = CapabilitySet::from_spec(&manifest.capabilities);
    if caps.is_empty() {
        let _ = writeln!(
            out,
            "  capabilities: none — this plugin requests no capabilities"
        );
    } else {
        let _ = writeln!(out, "  capabilities:");
        for cap in caps.iter() {
            let _ = writeln!(out, "    {cap}");
        }
    }
    if let Some(ui) = &manifest.ui {
        if ui.requested_capabilities.is_empty() {
            let _ = writeln!(out, "  UI host capabilities: none");
        } else {
            let _ = writeln!(out, "  UI host capabilities (approval required):");
            for capability in &ui.requested_capabilities {
                let _ = writeln!(out, "    {}", capability.as_str());
            }
        }
        let _ = writeln!(out, "  UI contributions:");
        for contribution in &ui.contributions {
            let _ = writeln!(
                out,
                "    {} -> {} ({})",
                contribution.id,
                contribution.point.as_str(),
                contribution.renderer
            );
        }
    }

    let r = &manifest.resources;
    let _ = writeln!(
        out,
        "  resources: {} MB mem, {} CPU s, {} wall s, {} MB output",
        r.memory_mb, r.cpu_seconds, r.wall_seconds, r.maximum_output_mb,
    );
    if !manifest.scopes.is_empty() {
        let _ = writeln!(out, "  scopes: {}", manifest.scopes.join(", "));
    }
    out
}

/// A human rendering of a permission diff between two versions of a plugin, with
/// the re-approval verdict. Pure, so it is tested directly.
fn plugin_diff_report(id: &str, diff: &codypendent_sandbox::PermissionDiff) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    if diff.is_identical() {
        let _ = writeln!(out, "{id}: no permission changes — safe to update.");
        return out;
    }
    let _ = writeln!(out, "{id}: permission changes:");
    let _ = writeln!(out, "{}", diff.render());
    if diff.expands_permissions() {
        let _ = writeln!(
            out,
            "\u{2192} update EXPANDS permissions — re-approval required (exit criterion 2)."
        );
    } else {
        let _ = writeln!(
            out,
            "\u{2192} update only narrows permissions — safe to update."
        );
    }
    out
}

/// Resolve the trusted-publisher key store path
/// (`<config_dir>/codypendent/trusted_publishers.toml`, the `models.toml`
/// precedent). `CODYPENDENT_CONFIG_DIR` overrides the config root (test isolation),
/// mirroring how `CODYPENDENT_DATA_DIR` overrides the data root.
fn trusted_publishers_path() -> anyhow::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CODYPENDENT_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir).join("trusted_publishers.toml"));
    }
    let dirs = directories::ProjectDirs::from("", "", "codypendent")
        .context("cannot determine a config directory for the current user")?;
    Ok(dirs.config_dir().join("trusted_publishers.toml"))
}

/// `codypendent plugin trust add <ID> <PUBLIC_KEY_B64>` (Phase 6 STEP 6.2): record
/// a trusted publisher's ed25519 public key so signed plugins from that publisher
/// verify against a real key. The key is validated before it is stored.
pub fn plugin_trust_add(id: &str, public_key_b64: &str) -> anyhow::Result<()> {
    let path = trusted_publishers_path()?;
    let mut store = codypendent_sandbox::TrustedPublishers::load(&path)
        .with_context(|| format!("loading trusted-publisher store {}", path.display()))?;
    store
        .add(id, public_key_b64)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    store
        .save(&path)
        .with_context(|| format!("writing trusted-publisher store {}", path.display()))?;
    println!("Trusted publisher `{id}` added ({}).", path.display());
    Ok(())
}

/// `codypendent plugin trust list` (Phase 6 STEP 6.2): print the trusted
/// publishers and their public keys.
pub fn plugin_trust_list() -> anyhow::Result<()> {
    let path = trusted_publishers_path()?;
    let store = codypendent_sandbox::TrustedPublishers::load(&path)
        .with_context(|| format!("loading trusted-publisher store {}", path.display()))?;
    if store.is_empty() {
        println!(
            "No trusted publishers ({}). Signed plugins from unknown publishers are refused.",
            path.display()
        );
        return Ok(());
    }
    println!("Trusted publishers ({}):", path.display());
    for (id, key) in store.list() {
        println!("  {id}  {key}");
    }
    Ok(())
}

/// `codypendent plugin trust remove <ID>` (Phase 6 STEP 6.2): stop trusting a
/// publisher. Exits non-zero if the publisher was not present.
pub async fn plugin_trust_remove(paths: &RuntimePaths, id: &str) -> anyhow::Result<()> {
    let revoked = ui_plugin_command(
        paths,
        CommandBody::RemoveTrustedUiPublisher {
            publisher_id: id.to_owned(),
        },
    )
    .await?;
    println!(
        "Trusted publisher `{id}` removed; {} signed Remote UI plugin(s) revoked and stopped.",
        revoked.len()
    );
    print_ui_plugin_result(revoked);
    Ok(())
}

/// `codypendent plugin verify <MANIFEST> <ARTIFACT>` (Phase 6 STEP 6.2): verify a
/// plugin artifact against its manifest using the **trusted-publisher key store** —
/// the real-keys install gate. Checksum + signature are checked, then the grant is
/// evaluated (`install_disabled`). A signed plugin from an unknown publisher, a bad
/// signature, or an unsigned plugin (unless `--allow-unsigned`) is **refused** with
/// a non-zero exit, so this is the fail-closed pre-install verification a stateful
/// `plugin install` builds on (persisting the installed record is daemon-wired
/// follow-up work).
pub fn plugin_verify(
    manifest_file: &std::path::Path,
    artifact_file: &std::path::Path,
    allow_unsigned: bool,
) -> anyhow::Result<()> {
    let manifest = read_manifest(manifest_file)?;
    let artifact = std::fs::read(artifact_file)
        .with_context(|| format!("reading plugin artifact {}", artifact_file.display()))?;

    let path = trusted_publishers_path()?;
    let store = codypendent_sandbox::TrustedPublishers::load(&path)
        .with_context(|| format!("loading trusted-publisher store {}", path.display()))?;

    let unsigned = if allow_unsigned {
        codypendent_sandbox::UnsignedPolicy::Allow
    } else {
        codypendent_sandbox::UnsignedPolicy::Deny
    };
    // The store is the resolver: an unknown publisher yields no key, so a signed
    // plugin fails closed (default-deny unsigned already covers the unsigned case).
    let publisher_key = store.key_for(&manifest.publisher);

    // Full grant at install: the profile is derived from what the manifest requests.
    let granted = codypendent_sandbox::CapabilitySet::from_spec(&manifest.capabilities);
    let granted_ui = manifest
        .ui
        .as_ref()
        .map(|ui| ui.requested_capabilities.iter().copied().collect())
        .unwrap_or_default();
    let installed = codypendent_sandbox::InstalledPlugin::install_disabled(
        manifest.clone(),
        &artifact,
        publisher_key.map(|k| k.as_slice()),
        unsigned,
        granted,
        granted_ui,
    )
    .map_err(|error| {
        anyhow::anyhow!("{} @ {}: refused — {error}", manifest.id, manifest.version)
    })?;

    let trust = if installed.is_signed() {
        format!("signed by trusted publisher `{}`", manifest.publisher)
    } else {
        "unsigned (allowed by --allow-unsigned)".to_string()
    };
    println!(
        "\u{2713} {} v{} verified — {trust}; installed disabled (inert until enabled).",
        manifest.id, manifest.version,
    );
    Ok(())
}

pub async fn plugin_install(
    paths: &RuntimePaths,
    manifest: &std::path::Path,
    artifact: &std::path::Path,
    allow_unsigned: bool,
) -> anyhow::Result<()> {
    let (manifest_toml, artifact_base64) = read_ui_plugin_candidate(manifest, artifact)?;
    print_ui_plugin_result(
        ui_plugin_command(
            paths,
            CommandBody::InstallUiPlugin {
                manifest_toml,
                artifact_base64,
                allow_unsigned,
            },
        )
        .await?,
    );
    Ok(())
}

pub async fn plugin_smoke_test(paths: &RuntimePaths, plugin_id: String) -> anyhow::Result<()> {
    print_ui_plugin_result(
        ui_plugin_command(paths, CommandBody::SmokeTestUiPlugin { plugin_id }).await?,
    );
    Ok(())
}

pub async fn plugin_enable(
    paths: &RuntimePaths,
    plugin_id: String,
    scope: String,
    session_id: Option<SessionId>,
) -> anyhow::Result<()> {
    print_ui_plugin_result(
        ui_plugin_command(
            paths,
            CommandBody::EnableUiPlugin {
                plugin_id,
                scope,
                session_id,
            },
        )
        .await?,
    );
    Ok(())
}

pub async fn plugin_list(paths: &RuntimePaths) -> anyhow::Result<()> {
    let plugins = ui_plugin_command(paths, CommandBody::ListUiPlugins).await?;
    if plugins.is_empty() {
        println!("No Remote UI plugins installed.");
    } else {
        print_ui_plugin_result(plugins);
    }
    Ok(())
}

pub async fn plugin_update(
    paths: &RuntimePaths,
    plugin_id: String,
    manifest: &std::path::Path,
    artifact: &std::path::Path,
    allow_unsigned: bool,
) -> anyhow::Result<()> {
    let (manifest_toml, artifact_base64) = read_ui_plugin_candidate(manifest, artifact)?;
    print_ui_plugin_result(
        ui_plugin_command(
            paths,
            CommandBody::UpdateUiPlugin {
                plugin_id,
                manifest_toml,
                artifact_base64,
                allow_unsigned,
            },
        )
        .await?,
    );
    Ok(())
}

pub async fn plugin_approve_update(
    paths: &RuntimePaths,
    plugin_id: String,
    approval_receipt: String,
) -> anyhow::Result<()> {
    print_ui_plugin_result(
        ui_plugin_command(
            paths,
            CommandBody::ApproveUiPluginUpdate {
                plugin_id,
                approval_receipt,
            },
        )
        .await?,
    );
    Ok(())
}

pub async fn plugin_reject_update(
    paths: &RuntimePaths,
    plugin_id: String,
    approval_receipt: String,
) -> anyhow::Result<()> {
    print_ui_plugin_result(
        ui_plugin_command(
            paths,
            CommandBody::RejectUiPluginUpdate {
                plugin_id,
                approval_receipt,
            },
        )
        .await?,
    );
    Ok(())
}

pub async fn plugin_revoke(paths: &RuntimePaths, plugin_id: String) -> anyhow::Result<()> {
    print_ui_plugin_result(
        ui_plugin_command(paths, CommandBody::RevokeUiPlugin { plugin_id }).await?,
    );
    Ok(())
}

fn read_ui_plugin_candidate(
    manifest: &std::path::Path,
    artifact: &std::path::Path,
) -> anyhow::Result<(String, String)> {
    let manifest_toml = std::fs::read_to_string(manifest)
        .with_context(|| format!("reading plugin manifest {}", manifest.display()))?;
    // Parse locally for an immediate human-legible schema error; the daemon
    // independently parses and verifies the same bytes at the trust boundary.
    codypendent_sandbox::parse_manifest(&manifest_toml)?;
    let artifact = std::fs::read(artifact)
        .with_context(|| format!("reading plugin archive {}", artifact.display()))?;
    if artifact.len() > 10 * 1024 * 1024 {
        anyhow::bail!("plugin archive exceeds the 10 MiB management-frame bound");
    }
    Ok((
        manifest_toml,
        base64::engine::general_purpose::STANDARD.encode(artifact),
    ))
}

async fn ui_plugin_command(
    paths: &RuntimePaths,
    body: CommandBody,
) -> anyhow::Result<Vec<codypendent_protocol::UiPluginLifecycleStatus>> {
    ensure_daemon(paths).await?;
    let command_id = CommandId::new();
    let operation_id = format!("ui-plugin:{command_id}");
    let mut last_error = None;
    let reply = loop {
        let result = async {
            let mut connection = Connection::connect(&paths.socket_path).await?;
            connection
                .handshake("codypendent", env!("CARGO_PKG_VERSION"), None)
                .await?;
            bind_control_role(&mut connection).await?;
            connection
                .send_command_with_idempotency(body.clone(), command_id, operation_id.clone())
                .await
        }
        .await;
        match result {
            Ok(reply) => break reply,
            Err(error) if last_error.is_none() => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => {
                return Err(error).context(format!(
                    "plugin lifecycle retry also failed after: {}",
                    last_error.expect("first attempt error recorded")
                ))
            }
        }
    };
    match reply.payload {
        Payload::UiPluginLifecycle { plugins, .. } => Ok(plugins),
        Payload::CommandRejected(error) => anyhow::bail!(
            "plugin lifecycle command rejected: {} ({})",
            error.message,
            error.code
        ),
        other => anyhow::bail!("unexpected plugin lifecycle reply: {other:?}"),
    }
}

fn print_ui_plugin_result(plugins: Vec<codypendent_protocol::UiPluginLifecycleStatus>) {
    for plugin in plugins {
        println!("{} v{} — {}", plugin.id, plugin.version, plugin.state);
        if let Some(scope) = plugin.enabled_scope {
            println!("  scope: {scope}");
        }
        if let Some(diff) = plugin.update_permission_diff {
            println!("  permission update:\n{}", indent_lines(&diff, "    "));
        }
        if let Some(receipt) = plugin.update_approval_receipt {
            println!("  approval receipt: {receipt}");
        }
    }
}

fn indent_lines(value: &str, prefix: &str) -> String {
    value
        .lines()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The handoff instructions printed by [`open`]. Pure (no I/O) so it is testable.
fn handoff_message(session_id: SessionId, paths: &RuntimePaths, ide_name: &str) -> String {
    format!(
        "Handing session {session_id} off to {ide_name}.\n\
         The editor attaches as a contributor to this session — the run keeps \
         going, it does not restart.\n\
         Session: {session_id}\n\
         Socket:  {}",
        paths.socket_path.display()
    )
}

/// `codypendent mcp list` (PR B — MCP client): print each operator-declared
/// MCP server from `<config_dir>/mcp.toml` — its launch line, env KEY NAMES
/// ONLY (values may be secrets — never printed), `inherit_environment`, and
/// the effective disposition from the builtin+global merged policy. A
/// **config-level** view only: no server is ever spawned here. A missing file
/// is a normal unconfigured state (`Ok`); a malformed file is a broken config
/// and earns a non-zero exit, its legible load error (path + reason) as the
/// failure message.
pub async fn mcp_list(paths: &RuntimePaths) -> anyhow::Result<()> {
    let mcp_path = paths.global_mcp_path();
    if !mcp_path.exists() {
        println!("no MCP servers configured (create {})", mcp_path.display());
        return Ok(());
    }
    let config = load_mcp_config(&mcp_path).map_err(anyhow::Error::new)?;
    if config.servers.is_empty() {
        println!("no MCP servers declared in {}", mcp_path.display());
        return Ok(());
    }
    let engine = PolicyEngine::load(None, Some(&paths.global_policy_path()))
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    print!("{}", render_mcp_list(&config, engine.merged()));
    Ok(())
}

/// The pure renderer behind [`mcp_list`]: one block per declared server. The
/// disposition is the per-server `[mcp.servers]` override when present, else
/// `[mcp] default` — the same resolution the policy engine's
/// `eval_mcp_tool_call` applies at run time.
fn render_mcp_list(config: &McpConfig, merged: &MergedPolicy) -> String {
    let mut out = String::new();
    for server in &config.servers {
        let disposition = merged
            .mcp_servers
            .get(&server.name)
            .copied()
            .unwrap_or(merged.mcp_default);
        out.push_str(&server.name);
        out.push('\n');
        let mut launch = server.command.clone();
        for arg in &server.args {
            launch.push(' ');
            launch.push_str(arg);
        }
        out.push_str(&format!("  command: {launch}\n"));
        if server.env.is_empty() {
            out.push_str("  env: (none)\n");
        } else {
            // KEY NAMES ONLY — env values may be secrets.
            let keys: Vec<&str> = server.env.iter().map(|(key, _)| key.as_str()).collect();
            out.push_str(&format!("  env keys: {}\n", keys.join(", ")));
        }
        out.push_str(&format!(
            "  inherit_environment: {}\n",
            server.inherit_environment
        ));
        out.push_str(&format!(
            "  disposition: {}\n",
            disposition_label(disposition)
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// `codypendent graph {build,status,show}`
// ---------------------------------------------------------------------------

/// Which repository the graph commands act on: the **checkout**, resolved
/// through [`crate::repo_anchor`], never the directory the operator happens to
/// be standing in.
///
/// Every graph command resolves it here and nowhere else. The alternative —
/// each command sending its own `current_dir()` — is the shape the 2026-08-13
/// review found behind "No documents yet." for a document that existed: a
/// subdirectory hashed to a different identity, the `WHERE` matched nothing,
/// and the empty list was a perfectly legitimate-looking answer to a
/// perfectly legitimate question. A code graph queried under the wrong
/// identity fails exactly the same way, and reporting "0 nodes" for it would
/// be indistinguishable from the bug this command family exists to fix.
fn graph_repository(repo: Option<PathBuf>) -> anyhow::Result<String> {
    let dir = match repo {
        Some(repo) => repo,
        None => std::env::current_dir()?,
    };
    Ok(crate::repo_anchor::anchor_repository_path(&dir)
        .display()
        .to_string())
}

/// Open a Controller-role connection for a graph command. A build is a write
/// (it clears and rewrites the repository's graph), so it needs the role; the
/// reads bind it too, which costs one frame and keeps the three commands'
/// connection setup identical.
async fn graph_connection(paths: &RuntimePaths) -> anyhow::Result<Connection> {
    ensure_daemon(paths).await?;
    let mut conn = Connection::connect(&paths.socket_path)
        .await
        .with_context(|| "connecting to the daemon (is it running?)")?;
    conn.handshake("codypendent", env!("CARGO_PKG_VERSION"), None)
        .await?;
    bind_control_role(&mut conn).await?;
    Ok(conn)
}

/// Turn a daemon refusal into a message that says what to do about it. A bare
/// `graph.not-a-repository` code is exactly as unhelpful as the silence being
/// fixed.
fn graph_rejection(verb: &str, error: &codypendent_protocol::CodypendentError) -> anyhow::Error {
    let hint = match error.code.as_str() {
        "graph.not-a-repository" => {
            "\nThe code graph is per-checkout. Run this inside a Git repository, \
             or pass --repo <PATH>."
        }
        "graph.transport-unavailable" => {
            "\nThis daemon was built without the code-graph transport. \
             Run `codypendent daemon restart` after updating."
        }
        _ => "",
    };
    anyhow::anyhow!(
        "graph {verb} rejected: {} ({}){hint}",
        error.message,
        error.code
    )
}

/// `codypendent graph build` — fold the repository's code graph now, and say
/// what the fold saw.
///
/// The **report is the feature**. Before this command the graph was built only
/// as a side effect of opening a session or starting a run, and an empty graph
/// explained nothing: on a mixed repository, Python and TSX files contributed
/// zero nodes and no surface said so. `index rebuild` — whose name reads as
/// "build the index, graph included" — explicitly does not touch the graph, and
/// its cheerful "29 registry item(s) re-indexed" made the silence worse.
///
/// Goes through the daemon rather than folding in-process, because the graph
/// has exactly one writer gate (`scan::lock_repository`) and it is a
/// per-process lock. A CLI that cleared and rebuilt the graph beside a running
/// daemon would reproduce the torn-graph race verbatim (2026-08-13 review, F6).
pub async fn graph_build(
    paths: &RuntimePaths,
    repo: Option<PathBuf>,
    json: bool,
) -> anyhow::Result<()> {
    let repository = graph_repository(repo)?;
    let mut conn = graph_connection(paths).await?;
    if !json {
        println!("Folding the code graph for {repository} …");
    }
    let reply = conn
        .send_command(CommandBody::BuildCodeGraph {
            repository: repository.clone(),
        })
        .await?;
    match reply.payload {
        Payload::CodeGraphBuilt { report, .. } => {
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", render_scan_report(&report));
            }
            Ok(())
        }
        Payload::CommandRejected(error) => Err(graph_rejection("build", &error)),
        other => anyhow::bail!("unexpected reply to BuildCodeGraph: {other:?}"),
    }
}

/// `codypendent graph status` — what the stored graph holds, with no re-scan.
pub async fn graph_status(
    paths: &RuntimePaths,
    repo: Option<PathBuf>,
    json: bool,
) -> anyhow::Result<()> {
    let repository = graph_repository(repo)?;
    let mut conn = graph_connection(paths).await?;
    let reply = conn
        .send_command(CommandBody::ReadCodeGraphStatus { repository })
        .await?;
    match reply.payload {
        Payload::CodeGraphStatus { status, .. } => {
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print!("{}", render_status(&status));
            }
            Ok(())
        }
        Payload::CommandRejected(error) => Err(graph_rejection("status", &error)),
        other => anyhow::bail!("unexpected reply to ReadCodeGraphStatus: {other:?}"),
    }
}

/// `codypendent graph show` — list the graph's nodes and edges, filtered, so it
/// is inspectable from a terminal rather than only through the TUI overlay.
#[allow(clippy::too_many_arguments)]
pub async fn graph_show(
    paths: &RuntimePaths,
    repo: Option<PathBuf>,
    query: codypendent_protocol::CodeGraphQuery,
    json: bool,
) -> anyhow::Result<()> {
    let repository = graph_repository(repo)?;
    let mut conn = graph_connection(paths).await?;
    let reply = conn
        .send_command(CommandBody::ReadCodeGraph { repository, query })
        .await?;
    match reply.payload {
        Payload::CodeGraphPage { page, .. } => {
            if json {
                println!("{}", serde_json::to_string_pretty(&page)?);
            } else {
                print!("{}", render_page(&page));
            }
            Ok(())
        }
        Payload::CommandRejected(error) => Err(graph_rejection("show", &error)),
        other => anyhow::bail!("unexpected reply to ReadCodeGraph: {other:?}"),
    }
}

/// The grammar roster as one line: `rust (.rs) · python (.py .pyi) · …`. Built
/// from what the daemon sent, never from a list held here — a client-side copy
/// of the roster is stale the day a grammar is added.
fn render_grammars(grammars: &[codypendent_protocol::CodeGraphGrammar]) -> String {
    grammars
        .iter()
        .map(|grammar| {
            let extensions: Vec<String> = grammar
                .extensions
                .iter()
                .map(|extension| format!(".{extension}"))
                .collect();
            format!("{} ({})", grammar.language, extensions.join(" "))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Render a build report.
///
/// Pure and returning a `String` rather than printing, so the wording that
/// explains an empty graph is testable — that wording *is* the feature, and a
/// feature that only exists inside a `println!` cannot be asserted on.
fn render_scan_report(report: &codypendent_protocol::CodeGraphScanReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "\ncode graph built — {}\n",
        report.repository_root
    ));
    out.push_str(&format!(
        "  revision      {} ({} ms)\n",
        report.revision, report.elapsed_ms
    ));
    out.push_str(&format!(
        "  walked        {} file(s); {} matched a grammar\n",
        report.files_walked, report.files_supported
    ));
    out.push_str(&format!(
        "  folded        {} file(s) -> {} node(s), {} edge(s)\n",
        report.files_folded, report.nodes, report.edges
    ));
    for language in &report.by_language {
        out.push_str(&format!(
            "                {:<12} {:>5} file(s) {:>7} node(s) {:>7} edge(s)\n",
            language.language, language.files, language.nodes, language.edges
        ));
    }
    if report.files_ignored > 0 {
        out.push_str(&format!(
            "  ignored       {} file(s) a grammar covers but .gitignore excludes\n",
            report.files_ignored
        ));
    }

    // A file whose extension a grammar covers but which produced nothing is a
    // different failure from one no grammar covers, and hiding it inside the
    // unsupported count would misattribute a parse bug to a missing language.
    //
    // Ignored files are subtracted first because they are a SUBSET of the
    // supported ones — the scan only reaches its ignore filter for candidates a
    // grammar already claimed. Leaving them in reported one phantom parse
    // failure per `.gitignore`d source file, which is precisely the kind of
    // confidently-wrong number this command exists to stop producing.
    let unparsed = report
        .files_supported
        .saturating_sub(report.files_folded)
        .saturating_sub(report.files_ignored);
    if unparsed > 0 {
        out.push_str(&format!(
            "  ! {unparsed} file(s) matched a grammar but produced nothing — unreadable, or\n\
             \x20   the parser rejected them. See the daemon log for the per-file reason.\n"
        ));
    }

    if report.nodes == 0 {
        out.push_str("\n  The graph is EMPTY.\n");
        if report.files_walked == 0 {
            out.push_str(
                "  This checkout has no files the walk could see. There is nothing to fold.\n",
            );
        } else {
            out.push_str(&format!(
                "  {} file(s) were walked and not one of them produced a symbol.\n",
                report.files_walked
            ));
        }
    }

    if report.files_unsupported > 0 {
        out.push_str(&format!(
            "\n  {} file(s) are in a language no grammar covers",
            report.files_unsupported
        ));
        if report.not_folded.is_empty() {
            out.push_str(".\n");
        } else {
            out.push_str(":\n");
            for skipped in &report.not_folded {
                out.push_str(&format!(
                    "                .{:<12} {:>6} file(s)\n",
                    skipped.extension, skipped.files
                ));
            }
        }
        if !report.grammars.is_empty() {
            out.push_str(&format!(
                "  This build folds: {}\n",
                render_grammars(&report.grammars)
            ));
        }
        out.push_str(
            "  Those files contribute nothing to the graph. That is a limit of the\n\
             \x20 extractor, not a fault in your repository.\n",
        );
    }

    if report.cap_hit {
        out.push_str(&format!(
            "\n  ! The {}-file scan cap was reached. This graph is a TRUNCATION of the\n\
             \x20   repository, not the repository.\n",
            report.file_cap
        ));
    }
    out.push('\n');
    out
}

/// Render a status view.
fn render_status(status: &codypendent_protocol::CodeGraphStatusView) -> String {
    let mut out = String::new();
    out.push_str(&format!("\ncode graph — {}\n", status.repository_root));
    out.push_str(&format!(
        "  HEAD          {} ({})\n",
        status.head_revision,
        if status.working_tree_dirty {
            "working tree has uncommitted changes"
        } else {
            "working tree clean"
        }
    ));
    out.push_str(&format!(
        "  stored        {} node(s), {} edge(s) across {} file(s)\n",
        status.nodes, status.edges, status.files
    ));
    for language in &status.by_language {
        out.push_str(&format!(
            "                {:<12} {:>5} file(s) {:>7} node(s) {:>7} edge(s)\n",
            language.language, language.files, language.nodes, language.edges
        ));
    }
    if !status.by_kind.is_empty() {
        out.push_str(&format!(
            "  kinds         {}\n",
            render_tallies(&status.by_kind)
        ));
    }
    if !status.revisions.is_empty() {
        out.push_str(&format!(
            "  built at      {}\n",
            render_tallies(&status.revisions)
        ));
    }
    match (&status.stale, &status.stale_reason) {
        (true, Some(reason)) => out.push_str(&format!("  status        STALE — {reason}\n")),
        (true, None) => out.push_str("  status        STALE\n"),
        (false, _) => out.push_str("  status        current\n"),
    }
    if status.nodes == 0 {
        // Deliberately not "nothing has been built yet": a build may well have
        // run and found nothing to fold, and telling that user to run it again
        // as though they had not is the same species of confidently-wrong
        // message as the bare zero. Point at the command that EXPLAINS the
        // emptiness, whichever of the two cases they are in.
        out.push_str(
            "\n  This repository's graph holds nothing. Run `codypendent graph build`:\n\
             \x20 it folds the graph and reports which files it walked and which\n\
             \x20 extensions produced nothing, so an empty result explains itself.\n\
             \x20 (`codypendent index rebuild` does NOT build the code graph.)\n",
        );
    } else if status.stale {
        out.push_str("\n  Run `codypendent graph build` to refold it.\n");
    }
    out.push('\n');
    out
}

/// `label n · label n · …`, bounded so one long tail cannot fill the terminal.
fn render_tallies(tallies: &[codypendent_protocol::CodeGraphTally]) -> String {
    const SHOWN: usize = 8;
    let mut parts: Vec<String> = tallies
        .iter()
        .take(SHOWN)
        .map(|tally| format!("{} {}", tally.label, tally.count))
        .collect();
    if tallies.len() > SHOWN {
        parts.push(format!("…and {} more", tallies.len() - SHOWN));
    }
    parts.join(" · ")
}

/// Render one page of nodes/edges.
fn render_page(page: &codypendent_protocol::CodeGraphPage) -> String {
    let mut out = String::new();
    if page.nodes.is_empty() && page.edges.is_empty() {
        out.push_str(
            "\nNo nodes matched.\n\
             Run `codypendent graph status` to see whether the graph holds anything at all,\n\
             and `codypendent graph build` to fold it.\n\n",
        );
        return out;
    }
    if !page.nodes.is_empty() {
        out.push_str(&format!(
            "\nnodes  (showing {} of {})\n",
            page.nodes.len(),
            page.total_nodes
        ));
        for node in &page.nodes {
            out.push_str(&format!(
                "  {:<10} {:<12} {:<28} {}\n",
                node.language,
                node.kind,
                node.source_path.as_deref().unwrap_or("-"),
                node.qualified_name
            ));
            out.push_str(&format!(
                "             id {}  @{}\n",
                node.id, node.revision
            ));
        }
    }
    if !page.edges.is_empty() {
        out.push_str(&format!(
            "\nedges  (showing {} of {})\n",
            page.edges.len(),
            page.total_edges
        ));
        for edge in &page.edges {
            out.push_str(&format!(
                "  {:<12} {} -> {}  ({:.2}, {})\n",
                edge.relation, edge.from_name, edge.to_name, edge.confidence, edge.evidence_kind
            ));
            if let Some(assertion) = &edge.asserted_by {
                out.push_str(&render_edge_assertion(assertion));
            }
        }
    }
    out.push('\n');
    out
}

/// The indent every edge continuation line hangs under: two leading spaces plus
/// the 12-column relation field and its separator, so provenance lines up under
/// the endpoint names rather than under the relation.
const EDGE_CONTINUATION_INDENT: &str = "               ";

/// A conservative terminal width — the one every wrapped line here fits inside.
const EDGE_LINE_WIDTH: usize = 80;

/// The columns a rationale may occupy, derived from the width it must fit and
/// the indent it hangs under (plus the two columns that set it apart from the
/// run/session lines) rather than restated, so the two cannot drift.
const RATIONALE_WIDTH: usize = EDGE_LINE_WIDTH - EDGE_CONTINUATION_INDENT.len() - 2;

/// Render an agent-asserted edge's provenance under its row: the run that
/// claimed it, and the reason it gave.
///
/// `evidence_kind` already told the reader THAT a model wrote this edge. That is
/// the claim; this is the audit trail, and the audit trail is the whole reason a
/// model is permitted to write to the graph. So the rationale travels in full
/// rather than being reduced to "asserted by run <uuid>" — the same choice
/// `crate::tui::evidence_source` makes for a memory's provenance.
///
/// A rationale is free text up to 400 characters, which is five terminal rows,
/// so it is wrapped at [`RATIONALE_WIDTH`] under a fixed indent instead of being
/// pushed onto the edge row: an edge table whose rows are sometimes 400 columns
/// wide is not readable, and truncating would throw away the only part a
/// reviewer actually needs.
fn render_edge_assertion(assertion: &codypendent_protocol::CodeGraphEdgeAssertion) -> String {
    // Run and session on separate lines: two full UUIDs plus the indent is 113
    // columns, which wraps in every terminal and makes both unselectable.
    let mut out = format!(
        "{EDGE_CONTINUATION_INDENT}asserted by run {}\n\
         {EDGE_CONTINUATION_INDENT}in session {}\n",
        assertion.run_id, assertion.session_id
    );
    for line in wrap_words(assertion.rationale.trim(), RATIONALE_WIDTH) {
        out.push_str(&format!("{EDGE_CONTINUATION_INDENT}  {line}\n"));
    }
    out
}

/// Greedy word wrap to `width` columns. A single word longer than `width` is
/// left whole on its own line rather than split — breaking mid-identifier makes
/// a symbol name unsearchable, which is worse than one long line.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= width => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    lines
}

/// The `[mcp]` disposition as the operator wrote it in `policy.toml`
/// (kebab-case, matching the serde spelling).
fn disposition_label(action: ApprovalAction) -> &'static str {
    match action {
        ApprovalAction::Allow => "allow",
        ApprovalAction::Approval => "approval",
        ApprovalAction::AlwaysApproval => "always-approval",
        ApprovalAction::Deny => "deny",
    }
}

#[cfg(test)]
mod graph_tests {
    use super::*;

    use codypendent_protocol::{
        CodeGraphGrammar, CodeGraphLanguageCount, CodeGraphNodeView, CodeGraphPage,
        CodeGraphScanReport, CodeGraphSkippedExtension, CodeGraphStatusView, CodeGraphTally,
    };

    fn empty_report() -> CodeGraphScanReport {
        CodeGraphScanReport {
            repository_root: "/home/user/api".to_string(),
            revision: "9f1c2ab".to_string(),
            files_walked: 1204,
            files_supported: 0,
            files_folded: 0,
            files_unsupported: 1204,
            files_ignored: 38,
            nodes: 0,
            edges: 0,
            by_language: Vec::new(),
            not_folded: vec![CodeGraphSkippedExtension {
                extension: "go".to_string(),
                files: 1204,
            }],
            grammars: vec![CodeGraphGrammar {
                language: "python".to_string(),
                extensions: vec!["py".to_string(), "pyi".to_string()],
            }],
            file_cap: 2000,
            cap_hit: false,
            elapsed_ms: 41,
        }
    }

    /// **The user's report, as a test.** "the DAG isn't being built" was a graph
    /// that came out empty and said nothing about why. An empty build must name
    /// the count of files it walked, the extensions that produced nothing, and
    /// the grammars that would have worked — a bare zero is the bug.
    #[test]
    fn an_empty_build_explains_itself_instead_of_printing_a_zero() {
        let rendered = render_scan_report(&empty_report());

        assert!(rendered.contains("EMPTY"), "{rendered}");
        assert!(rendered.contains("1204"), "{rendered}");
        assert!(
            rendered.contains(".go"),
            "the extension responsible must be named: {rendered}"
        );
        assert!(
            rendered.contains("python (.py .pyi)"),
            "the roster that WOULD have folded must be shown: {rendered}"
        );
        assert!(
            rendered.contains("38 file(s)"),
            "ignored files are accounted for, not silently dropped: {rendered}"
        );
    }

    /// A file whose extension a grammar covers but which folded to nothing is a
    /// different failure from one no grammar covers — a parser problem, not a
    /// missing language. Rolling it into the unsupported count would send the
    /// reader looking for a grammar that already exists.
    #[test]
    fn a_supported_file_that_folded_nothing_is_reported_separately() {
        let mut report = empty_report();
        report.files_supported = 5;
        report.files_folded = 2;
        report.files_ignored = 0;
        report.nodes = 7;
        let rendered = render_scan_report(&report);
        assert!(
            rendered.contains("3 file(s) matched a grammar but produced nothing"),
            "{rendered}"
        );
    }

    /// A `.gitignore`d source file is a supported candidate the scan chose not
    /// to fold — NOT a parse failure. Counting it as one reported a phantom
    /// broken file for every ignored `.js` in `node_modules`, observed on a real
    /// checkout before this subtraction was fixed.
    #[test]
    fn an_ignored_source_file_is_not_reported_as_a_parse_failure() {
        let mut report = empty_report();
        report.files_walked = 9;
        report.files_supported = 7;
        report.files_folded = 6;
        report.files_ignored = 1;
        report.files_unsupported = 2;
        report.nodes = 32;
        let rendered = render_scan_report(&report);
        assert!(
            !rendered.contains("matched a grammar but produced nothing"),
            "6 folded + 1 ignored accounts for all 7 candidates: {rendered}"
        );
        assert!(
            rendered.contains("1 file(s) a grammar covers but .gitignore excludes"),
            "{rendered}"
        );
    }

    /// The cap is the difference between "this is your repository" and "this is
    /// part of your repository", so it is stated, loudly, not inferred.
    #[test]
    fn hitting_the_scan_cap_says_the_graph_is_a_truncation() {
        let mut report = empty_report();
        report.cap_hit = true;
        report.files_folded = 2000;
        report.nodes = 40_000;
        let rendered = render_scan_report(&report);
        assert!(rendered.contains("TRUNCATION"), "{rendered}");
        assert!(rendered.contains("2000-file scan cap"), "{rendered}");
    }

    fn status(nodes: u64, stale: Option<&str>) -> CodeGraphStatusView {
        CodeGraphStatusView {
            repository_root: "/home/user/api".to_string(),
            nodes,
            edges: nodes / 2,
            files: 3,
            by_language: vec![CodeGraphLanguageCount {
                language: "rust".to_string(),
                files: 3,
                nodes,
                edges: nodes / 2,
            }],
            by_kind: vec![CodeGraphTally {
                label: "function".to_string(),
                count: nodes,
            }],
            revisions: vec![CodeGraphTally {
                label: "9f1c2ab".to_string(),
                count: nodes,
            }],
            head_revision: "9f1c2ab".to_string(),
            working_tree_dirty: false,
            stale: stale.is_some(),
            stale_reason: stale.map(ToOwned::to_owned),
        }
    }

    /// An empty stored graph must point at the command that fills it — and say
    /// out loud that `index rebuild` is not that command. That confusion is
    /// half the reported bug: `index rebuild` prints a cheerful success line
    /// and never touches the graph.
    #[test]
    fn an_empty_status_points_at_graph_build_and_disowns_index_rebuild() {
        let rendered = render_status(&status(0, Some("the graph is empty")));
        assert!(rendered.contains("codypendent graph build"), "{rendered}");
        assert!(
            rendered.contains("index rebuild` does NOT build the code graph"),
            "{rendered}"
        );
    }

    /// A stale graph names its reason. "STALE" alone is as unhelpful as "0".
    #[test]
    fn a_stale_status_prints_the_reason_and_the_remedy() {
        let rendered = render_status(&status(12, Some("folded at old, but HEAD is now 9f1c2ab")));
        assert!(rendered.contains("STALE — folded at old"), "{rendered}");
        assert!(rendered.contains("graph build` to refold"), "{rendered}");
        assert!(!rendered.contains("does NOT build"), "{rendered}");
    }

    #[test]
    fn a_current_status_says_current() {
        let rendered = render_status(&status(12, None));
        assert!(rendered.contains("status        current"), "{rendered}");
        assert!(rendered.contains("working tree clean"), "{rendered}");
    }

    /// An empty page must not read as "your filter matched nothing, move on":
    /// the far more likely cause is a graph that was never built.
    #[test]
    fn an_empty_page_sends_the_reader_to_status_and_build() {
        let rendered = render_page(&CodeGraphPage {
            nodes: Vec::new(),
            edges: Vec::new(),
            total_nodes: 0,
            total_edges: 0,
            limit: 50,
        });
        assert!(rendered.contains("graph status"), "{rendered}");
        assert!(rendered.contains("graph build"), "{rendered}");
    }

    /// A page says how much it is NOT showing, so a limit never reads as the
    /// whole graph.
    #[test]
    fn a_page_states_the_total_it_was_drawn_from() {
        let rendered = render_page(&CodeGraphPage {
            nodes: vec![CodeGraphNodeView {
                id: "n1".to_string(),
                language: "rust".to_string(),
                package: None,
                source_path: Some("src/lib.rs".to_string()),
                qualified_name: "one".to_string(),
                kind: "function".to_string(),
                revision: "9f1c2ab".to_string(),
            }],
            edges: Vec::new(),
            total_nodes: 812,
            total_edges: 0,
            limit: 50,
        });
        assert!(rendered.contains("showing 1 of 812"), "{rendered}");
    }

    fn edge_view(
        evidence_kind: &str,
        asserted_by: Option<codypendent_protocol::CodeGraphEdgeAssertion>,
    ) -> codypendent_protocol::CodeGraphEdgeView {
        codypendent_protocol::CodeGraphEdgeView {
            from_id: "n1".to_string(),
            from_name: "routes::handle_charge".to_string(),
            to_id: "n2".to_string(),
            to_name: "services::ChargeService::run".to_string(),
            relation: "calls".to_string(),
            confidence: 0.6,
            evidence_kind: evidence_kind.to_string(),
            revision: "9f1c2ab".to_string(),
            asserted_by,
        }
    }

    fn page_of(edges: Vec<codypendent_protocol::CodeGraphEdgeView>) -> CodeGraphPage {
        let total_edges = edges.len() as u64;
        CodeGraphPage {
            nodes: Vec::new(),
            edges,
            total_nodes: 0,
            total_edges,
            limit: 50,
        }
    }

    /// The audit trail an agent-written edge exists to leave: `graph show
    /// --edges` must name the run that asserted it and print the reason it
    /// gave. Before this, the row said `agent_asserted` and stopped — the claim
    /// without the grounds.
    #[test]
    fn an_agent_asserted_edge_prints_its_run_and_rationale() {
        let session_id = SessionId::new();
        let run_id = codypendent_protocol::RunId::new();
        let rendered = render_page(&page_of(vec![edge_view(
            "agent_asserted",
            Some(codypendent_protocol::CodeGraphEdgeAssertion {
                session_id,
                run_id,
                rationale: "src/routes.rs dispatches POST /charge to this handler by name"
                    .to_string(),
            }),
        )]));
        assert!(rendered.contains("agent_asserted"), "{rendered}");
        assert!(
            rendered.contains(&format!("asserted by run {run_id}")),
            "{rendered}"
        );
        assert!(rendered.contains(&session_id.to_string()), "{rendered}");
        assert!(
            rendered.contains("dispatches POST /charge to this handler by name"),
            "{rendered}"
        );
    }

    /// A parsed edge asserts nothing, so its row must be exactly what it was —
    /// no blank provenance line, no "asserted by" with nothing after it.
    #[test]
    fn a_parsed_edge_gains_no_provenance_line() {
        let rendered = render_page(&page_of(vec![edge_view("syntax_inferred", None)]));
        assert!(rendered.contains("syntax_inferred"), "{rendered}");
        assert!(!rendered.contains("asserted by"), "{rendered}");
    }

    /// A rationale is free text up to 400 characters. It must wrap under the
    /// edge row rather than smear one row across 400 columns — the row layout is
    /// what makes an edge table readable at all.
    #[test]
    fn a_long_rationale_wraps_instead_of_running_off_the_row() {
        let rationale = "the route table maps every path to a handler by name at startup, so \
                         nothing in the source text links this handler to the service it \
                         ultimately calls; the link is only visible at runtime, which is exactly \
                         why a human had to tell the graph about it"
            .to_string();
        assert!(rationale.len() > 200, "the fixture must actually be long");
        let rendered = render_page(&page_of(vec![edge_view(
            "agent_asserted",
            Some(codypendent_protocol::CodeGraphEdgeAssertion {
                session_id: SessionId::new(),
                run_id: codypendent_protocol::RunId::new(),
                rationale: rationale.clone(),
            }),
        )]));

        // Only the provenance lines are this function's to bound — an edge row
        // itself is as wide as the two symbol names it must print in full.
        let provenance: Vec<&str> = rendered
            .lines()
            .filter(|line| line.starts_with(EDGE_CONTINUATION_INDENT))
            .collect();
        assert!(
            provenance.len() > 3,
            "a 200+ character rationale must occupy several lines: {provenance:?}"
        );
        for line in &provenance {
            assert!(
                line.chars().count() <= EDGE_LINE_WIDTH,
                "every provenance line stays inside {EDGE_LINE_WIDTH} columns: {line:?}"
            );
        }
        // Wrapped, not truncated: every word survives.
        let flattened: String = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
        let wanted: String = rationale.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flattened.contains(&wanted), "{rendered}");
    }

    #[test]
    fn wrap_words_never_splits_a_single_long_word() {
        let long = "a_very_long_identifier_that_exceeds_the_width_on_its_own";
        assert_eq!(wrap_words(long, 10), vec![long.to_string()]);
        assert_eq!(
            wrap_words("one two three", 7),
            vec!["one two".to_string(), "three".to_string()]
        );
        assert!(wrap_words("   ", 10).is_empty());
    }
}

#[cfg(test)]
mod open_tests {
    use super::*;

    #[test]
    fn handoff_message_names_the_session_and_socket() {
        let paths = RuntimePaths::from_data_dir(std::path::PathBuf::from("/tmp/cp-test"));
        let session = SessionId::new();
        let message = handoff_message(session, &paths, "VS Code");
        assert!(message.contains(&session.to_string()));
        assert!(message.contains("VS Code"));
        assert!(message.contains("does not restart"));
        assert!(message.contains(&paths.socket_path.display().to_string()));
    }
}

#[cfg(test)]
mod mcp_tests {
    use super::*;
    use codypendent_integrations::mcp::McpServerConfig;

    fn server(name: &str, command: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_owned(),
            command: command.to_owned(),
            args: Vec::new(),
            env: Vec::new(),
            inherit_environment: true,
        }
    }

    #[test]
    fn render_shows_dispositions_and_never_env_values() {
        let mut github = server("github", "npx");
        github.args = vec![
            "-y".to_owned(),
            "@modelcontextprotocol/server-github".to_owned(),
        ];
        github.env = vec![("GITHUB_TOKEN".to_owned(), "secret-value".to_owned())];
        let mut hermetic = server("hermetic", "/usr/local/bin/mcp-fs");
        hermetic.inherit_environment = false;
        let config = McpConfig {
            servers: vec![github, hermetic],
        };
        let mut merged = MergedPolicy::builtin_defaults();
        merged
            .mcp_servers
            .insert("github".to_owned(), ApprovalAction::Allow);

        let out = render_mcp_list(&config, &merged);
        assert!(out.starts_with("github\n"), "server block missing:\n{out}");
        assert!(
            out.contains("command: npx -y @modelcontextprotocol/server-github\n"),
            "launch line missing:\n{out}"
        );
        assert!(
            out.contains("env keys: GITHUB_TOKEN\n"),
            "env key NAMES should render:\n{out}"
        );
        assert!(
            !out.contains("secret-value"),
            "env VALUES must never render:\n{out}"
        );
        assert!(
            out.contains("disposition: allow\n"),
            "the per-server override should win:\n{out}"
        );
        // No override for `hermetic`: the builtin default (`approval`) shows,
        // along with its hermetic launch and empty env.
        assert!(
            out.contains("hermetic\n  command: /usr/local/bin/mcp-fs\n"),
            "second server block missing:\n{out}"
        );
        assert!(out.contains("env: (none)\n"), "empty env line:\n{out}");
        assert!(
            out.contains("inherit_environment: false\n"),
            "inherit_environment missing:\n{out}"
        );
        assert!(
            out.contains("disposition: approval\n"),
            "the default disposition should show:\n{out}"
        );
    }
}

#[cfg(test)]
mod workflow_tests {
    use super::*;

    const VALID: &str = "\
schema_version: 1
id: pipeline
version: 2
budget:
  maximum_cost_usd: 5.0
steps:
  - id: build
    tool: repository.test
  - id: check
    depends_on: [build]
    tool: repository.test
";

    #[test]
    fn summary_reports_id_version_counts_and_order() {
        let compiled = codypendent_workflow::compile_yaml(VALID).unwrap();
        let summary = workflow_summary(&compiled);
        assert!(summary.contains("pipeline v2 valid"));
        assert!(summary.contains("2 step(s)"));
        assert!(summary.contains("0 agent step(s)"));
        // Topological order is shown, dependency first.
        assert!(summary.contains("build \u{2192} check"), "got: {summary}");
    }

    #[test]
    fn validate_accepts_a_good_manifest_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("wf.yaml");
        std::fs::write(&path, VALID).unwrap();
        workflow_validate(&path, None).expect("a valid manifest validates");
    }

    #[test]
    fn validate_reports_a_compile_error_tagged_with_the_file() {
        // A step depending on a missing step fails to compile; the error names the
        // file and the offending dependency.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("broken.yaml");
        std::fs::write(
            &path,
            "schema_version: 1\nid: wf\nversion: 1\nsteps:\n  - id: a\n    depends_on: [ghost]\n    tool: repository.test\n",
        )
        .unwrap();
        let err = workflow_validate(&path, None).unwrap_err().to_string();
        assert!(err.contains("broken.yaml"), "error names the file: {err}");
        assert!(err.contains("ghost"), "error names the bad dep: {err}");
    }

    #[test]
    fn validate_reports_a_missing_file() {
        let err = workflow_validate(std::path::Path::new("/no/such/manifest.yaml"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("reading workflow manifest"));
    }

    #[test]
    fn validate_with_agents_resolves_or_reports_roles() {
        // `AGENT_MANIFEST` has one agent step (`inspect`, role `investigator`).
        let tmp = tempfile::tempdir().unwrap();
        let manifest = tmp.path().join("wf.yaml");
        std::fs::write(&manifest, AGENT_MANIFEST).unwrap();
        let agents = tmp.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();

        // No profile fulfils `investigator` yet → the cross-check fails, naming
        // the manifest, the step, and the unresolved role.
        let err = workflow_validate(&manifest, Some(&agents))
            .unwrap_err()
            .to_string();
        assert!(err.contains("wf.yaml"), "names the manifest: {err}");
        assert!(err.contains("investigator"), "names the role: {err}");
        assert!(err.contains("inspect"), "names the step: {err}");

        // Add a profile fulfilling the role (via the id suffix) → it resolves.
        std::fs::write(
            agents.join("scout.toml"),
            "schema_version = 1\nid = \"agents.investigator\"\nname = \"Scout\"\n",
        )
        .unwrap();
        workflow_validate(&manifest, Some(&agents)).expect("every agent role now resolves");
    }

    const AGENT_MANIFEST: &str = "\
schema_version: 1
id: review-flow
version: 1
budget:
  maximum_cost_usd: 5.0
  maximum_agents: 1
steps:
  - id: inspect
    agent:
      role: investigator
    skill: github.inspect-failed-check
    outputs: [finding]
  - id: publish
    depends_on: [inspect]
    tool: github.update-pull-request
    approval: always
";

    #[test]
    fn tree_shows_each_node_action_edge_and_settings() {
        let compiled = codypendent_workflow::compile_yaml(AGENT_MANIFEST).unwrap();
        let tree = workflow_tree(&compiled);
        assert!(tree.contains("review-flow v1"));
        assert!(tree.contains("inspect [agent investigator · skill github.inspect-failed-check]"));
        assert!(tree.contains("publish [tool github.update-pull-request]"));
        assert!(tree.contains("depends_on: inspect"));
        assert!(tree.contains("approval: Always"));
        assert!(tree.contains("outputs: finding"));
    }

    #[test]
    fn show_json_emits_a_parseable_graph_projection() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("wf.yaml");
        std::fs::write(&path, AGENT_MANIFEST).unwrap();
        // The command runs; and the same compiled graph serializes to the JSON
        // shape a graph-view client parses (tagged actions, edges).
        workflow_show(&path, true).expect("show --json succeeds");
        let compiled = codypendent_workflow::compile_yaml(AGENT_MANIFEST).unwrap();
        let value = serde_json::to_value(&compiled).unwrap();
        assert_eq!(value["id"], "review-flow");
        assert_eq!(value["nodes"][0]["action"]["kind"], "agent");
        assert_eq!(
            value["nodes"][1]["action"]["name"],
            "github.update-pull-request"
        );
    }

    /// Outcome 15: a worker's spend must be *visible*. `NodeCost` measures
    /// tokens and `cost_micros` and stores them in `workflow_nodes.cost_json`;
    /// `workflow watch` used to read only wall-time and tool calls, dropping
    /// both at the last inch.
    #[test]
    fn render_cost_shows_measured_tokens_and_money() {
        let rendered = render_cost(&serde_json::json!({
            "wall_time_secs": 3, "tool_calls": 1, "tokens": 1200, "cost_micros": 2500
        }))
        .expect("a measured node renders");
        assert!(rendered.contains("1200 tokens"), "{rendered}");
        assert!(rendered.contains("$0.0025"), "{rendered}");
    }

    #[test]
    fn render_cost_never_fabricates_an_unmeasured_dimension() {
        // Both keys absent from `cost_json` — the default install, where
        // routing (the only price source) is off. Neither may print a zero.
        let bare = render_cost(&serde_json::json!({"wall_time_secs": 3, "tool_calls": 1}))
            .expect("wall time still renders");
        assert!(!bare.contains("tokens") && !bare.contains('$'), "{bare}");
        assert_eq!(render_cost(&serde_json::json!({})), None);
    }
}

// ---------------------------------------------------------------------------
// `codypendent models list | add | check` — headless parity with the TUI
// ---------------------------------------------------------------------------

/// `codypendent models list`: the configured models, one per line, with the
/// same facts the TUI's `/model` picker shows — provider, endpoint, context
/// window, and whether a key is stored. Never prints key material: only
/// whether one exists, and for an env-backed entry, the variable NAME.
///
/// An absent `models.toml` is not an error: it prints the "none configured"
/// line and the `models add` hint, since that is the true state of a fresh
/// install rather than a failure.
pub fn models_list(paths: &RuntimePaths) -> anyhow::Result<()> {
    use codypendent_runtime::auth::AuthStore;
    use codypendent_runtime::models::load_models;

    let models_path = paths.data_dir.join("models.toml");
    if !models_path.exists() {
        println!("no models configured ({})", models_path.display());
        println!("add one with: codypendent models add <provider> <model-id>");
        return Ok(());
    }
    let configs =
        load_models(&models_path).with_context(|| format!("reading {}", models_path.display()))?;
    if configs.is_empty() {
        println!("no models configured ({})", models_path.display());
        return Ok(());
    }
    let auth = AuthStore::load(&paths.data_dir).unwrap_or_default();
    for config in &configs {
        let key = if auth.get(&config.id.0).is_some_and(|k| !k.is_empty()) {
            "key: stored".to_string()
        } else if !config.api_key_env.trim().is_empty() {
            format!("key: env {}", config.api_key_env)
        } else if config
            .provider_id
            .as_deref()
            .and_then(|p| auth.get(&codypendent_runtime::models::provider_auth_id(p)))
            .is_some_and(|k| !k.is_empty())
        {
            "key: stored (provider-wide)".to_string()
        } else {
            "key: none".to_string()
        };
        let context = config
            .context_tokens
            .map_or_else(|| "context: —".to_string(), |t| format!("context: {t}"));
        let endpoint = if config.base_url.is_empty() {
            config.model.clone()
        } else {
            config.base_url.clone()
        };
        println!(
            "{}\n    {} · {} · {} · {}",
            config.id.0,
            config.provider_id.as_deref().unwrap_or(&config.provider),
            endpoint,
            context,
            key
        );
    }
    Ok(())
}

/// `codypendent models add <provider> <model-id> [--key-env NAME] [--id ID]`:
/// the headless twin of the TUI's add-model flow. Resolves the provider from
/// the same catalog (built-ins layered with the user's `providers.toml`),
/// records `provider_id` so the runtime sends that provider's real auth header,
/// carries the catalog row's context window when it has one, and writes
/// `models.toml` atomically.
///
/// No key is ever read from an argument (a key on a command line lands in the
/// shell history and the process table): `--key-env` names the environment
/// variable to read at call time, and without it the provider's own documented
/// variable — or a key already stored by the TUI — is used.
pub fn models_add(
    paths: &RuntimePaths,
    provider_id: &str,
    model: &str,
    key_env: Option<&str>,
    id: Option<&str>,
) -> anyhow::Result<()> {
    use codypendent_providers::Catalog;
    use codypendent_runtime::models::ModelConfig;

    let data_dir = &paths.data_dir;
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("creating the data dir {}", data_dir.display()))?;
    let catalog = Catalog::load_with_user_overrides(&data_dir.join("providers.toml"))
        .unwrap_or_else(|_| Catalog::builtin());
    let provider = catalog
        .get(provider_id)
        .ok_or_else(|| anyhow::anyhow!("provider `{provider_id}` is not in the catalog"))?;
    // Refuse through the SAME predicate `models list-providers` annotates with,
    // and say what to do instead. "has no base URL and cannot be added" was
    // true and useless: it named a fact about the catalog, not an action.
    if let Some(reason) = crate::tui::provider_unusable_reason(provider) {
        anyhow::bail!("provider `{provider_id}` cannot be added — {reason}");
    }
    let base_url = provider
        .base_url
        .as_deref()
        .map(|url| url.trim().trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty())
        .ok_or_else(|| {
            // Unreachable via the gate above; kept so a future catalog shape
            // cannot turn a blank URL into a silently malformed entry.
            anyhow::anyhow!("provider `{provider_id}` has no base URL and cannot be added")
        })?;
    let display_id = id
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{provider_id}/{model}"));
    if display_id.trim().is_empty() {
        anyhow::bail!("model id must not be blank");
    }

    let models_path = data_dir.join("models.toml");
    let config = ModelConfig {
        id: codypendent_protocol::ModelId(display_id.clone()),
        provider: "openai-compatible".to_string(),
        base_url,
        model: model.to_string(),
        api_key_env: key_env.unwrap_or_default().to_string(),
        provider_id: Some(provider_id.to_string()),
        context_tokens: catalog
            .model(provider_id, model)
            .and_then(|row| row.context_tokens),
    };
    let replaced = crate::models_file::update_model_entries(&models_path, |configs| {
        let replaced = configs.iter().any(|c| c.id.0 == display_id);
        configs.retain(|c| c.id.0 != display_id);
        configs.push(config);
        Ok(replaced)
    })?;
    println!(
        "{} model {display_id} ({})",
        if replaced { "updated" } else { "added" },
        models_path.display()
    );
    if key_env.is_none() && !provider.local {
        println!("verify it with: codypendent models check {display_id}");
    }
    Ok(())
}

/// `codypendent models check <id>`: the headless twin of the `/keys` verify
/// action. Runs the real [`ModelRegistry::check_model`], so the credential
/// precedence and the catalog-declared auth headers are exactly the ones a run
/// would use — an "ok" here means a run authenticates. Exits non-zero when the
/// provider does not list the configured model.
pub async fn models_check(paths: &RuntimePaths, id: &str) -> anyhow::Result<()> {
    use codypendent_runtime::auth::AuthStore;
    use codypendent_runtime::models::{load_models, ModelRegistry};

    let models_path = paths.data_dir.join("models.toml");
    let configs =
        load_models(&models_path).with_context(|| format!("reading {}", models_path.display()))?;
    if !configs.iter().any(|c| c.id.0 == id) {
        anyhow::bail!(
            "model `{id}` is not configured in {}",
            models_path.display()
        );
    }
    let auth = AuthStore::load(&paths.data_dir).unwrap_or_default();
    let catalog = codypendent_providers::Catalog::load_with_user_overrides(
        &paths.data_dir.join("providers.toml"),
    )
    .unwrap_or_else(|_| codypendent_providers::Catalog::builtin());
    ModelRegistry::new(configs)
        .with_auth(auth)
        .with_catalog(catalog)
        .check_model(&codypendent_protocol::ModelId(id.to_owned()))
        .await
        .with_context(|| format!("checking model `{id}`"))?;
    println!("{id}: ok — the provider lists this model and the credentials resolve");
    Ok(())
}

// ---------------------------------------------------------------------------
// `codypendent models bench <id>` (Phase 7 STEP 7.2.2)
// ---------------------------------------------------------------------------

/// `codypendent models bench <id>`: measure the local model `id` (configured in
/// `<data_dir>/models.toml`) and persist its measured profile + first-use
/// capability probe to the daemon's `model_profiles` store (migration 0014), so
/// the router reads MEASURED numbers.
///
/// This is an **offline measurement/maintenance command**: unlike the run/eval
/// commands (which drive the daemon over the socket), it opens the migrated
/// database directly — the same way the daemon does — because it writes the
/// `model_profiles` table the live daemon only *reads* (and only when routing is
/// enabled, default OFF). SQLite WAL + the shared `busy_timeout` make the
/// concurrent open safe.
pub async fn models_bench(
    paths: &RuntimePaths,
    id: &str,
    price_per_1m_usd: Option<f64>,
) -> anyhow::Result<()> {
    use codypendent_runtime::agent::FrameworkModelDriver;
    use codypendent_runtime::bench::{BenchOptions, DriverBenchTarget};
    use codypendent_runtime::models::{load_models, ModelRegistry};

    // Resolve the model's endpoint from models.toml (the profile + probe key).
    let models_path = paths.data_dir.join("models.toml");
    let configs =
        load_models(&models_path).with_context(|| format!("reading {}", models_path.display()))?;
    let config = configs
        .iter()
        .find(|c| c.id.0 == id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "model `{id}` is not configured in {}",
                models_path.display()
            )
        })?
        .clone();
    let endpoint = config.base_url.clone();

    // Build a real client for the model and a bench target over it. The model-
    // driver seam does not surface streaming/usage or the endpoint's advertised
    // capabilities, so the target is described with conservative declared
    // capabilities (a real capability-discovery probe against the endpoint is a
    // documented future seam); the timing + scripted-probe numbers are measured.
    let registry = ModelRegistry::new(configs);
    let driver = FrameworkModelDriver::from_registry(&registry, config.id.clone())
        .await
        .with_context(|| format!("building a model client for `{id}`"))?;
    let target = DriverBenchTarget::new(&driver, default_bench_description());

    // The persisted profile's location is derived from the endpoint (fail-closed
    // to hosted). `models bench` is a LOCAL-model harness; warn loudly when it is
    // pointed at a non-local endpoint rather than silently mislabelling it.
    let hosted = matches!(
        codypendent_runtime::bench::endpoint_location(&endpoint),
        codypendent_routing::ModelLocation::Hosted
    );
    if hosted {
        eprintln!(
            "models bench: WARNING — `{endpoint}` is not a local endpoint; the profile will be \
             stored as HOSTED (so the routing hard filter still applies) and its token price is \
             not measured. `models bench` is intended for local models."
        );
    }

    // A HOSTED model needs a real per-token price to ever clear the router's
    // "unmeasured hosted model" hard filter (outcome 11, F11.4) — this harness
    // measures timing, never a real endpoint's price. A LOCAL model needs
    // neither — its price is genuinely $0, not the harness's sentinel.
    let known_price_per_1k_usd = if !hosted {
        None
    } else {
        let catalog = codypendent_providers::Catalog::load_with_user_overrides(
            &paths.data_dir.join("providers.toml"),
        )
        .unwrap_or_else(|_| codypendent_providers::Catalog::builtin());
        let price = resolve_hosted_price(&config, &catalog, price_per_1m_usd);
        if price.is_none() {
            eprintln!(
                "models bench: WARNING — no catalog price found for `{id}` (provider_id={:?}, \
                 model={:?}); the profile will be stored UNPRICED and stay ineligible for \
                 routing until one is set — re-run with `--price-per-1m-usd <blended $/1M>`.",
                config.provider_id, config.model
            );
        }
        price
    };

    eprintln!("models bench: measuring `{id}` at {endpoint} (this drives the model)...");
    let profile = {
        let pool = codypendent_daemon::db::open_database(&paths.data_dir.join("codypendent.db"))
            .await
            .context("opening the database to persist the model profile")?;
        bench_to_store(
            &pool,
            &endpoint,
            &target,
            BenchOptions::default(),
            known_price_per_1k_usd,
        )
        .await?
    };

    let bench = profile
        .bench
        .as_ref()
        .expect("a benched profile carries a LocalBench");
    let price_line = if hosted {
        match known_price_per_1k_usd {
            Some(p) => format!("\n  price: ${:.4}/1K tokens (blended, routable)", p),
            None => "\n  price: none (unpriced — ineligible for routing)".to_string(),
        }
    } else {
        String::new()
    };
    println!(
        "measured `{id}` (persisted to model_profiles @ {endpoint}):\n  \
         tokens/sec: {:.1}\n  time-to-first-token: {:.0} ms\n  warm-up: {:.0} ms\n  \
         memory: {} MB\n  context limit: {}\n  structured-output reliability: {:.2}\n  \
         tool-call accuracy: {:.2}\n  coding-eval score: {:.2}{price_line}",
        bench.tokens_per_second,
        bench.time_to_first_token_ms,
        bench.warmup_ms,
        bench.memory_mb,
        bench.context_limit,
        bench.structured_output_reliability,
        bench.tool_call_accuracy,
        bench.coding_eval_score,
    );
    Ok(())
}

/// The per-1K-token price a benched HOSTED model should be persisted with, so
/// it can clear the router's "unmeasured hosted model" hard filter (outcome
/// 11, F11.4 — `2026-08-13-verticals/sandbox-eval-routing.md`): (1) an
/// explicit `--price-per-1m-usd` override always wins; (2) otherwise the
/// built-in provider catalog's own price for this exact `provider_id` +
/// provider-side `model`, when known (both `cost_per_1m_input_usd` and
/// `cost_per_1m_output_usd` present — a partial catalog row is treated as
/// unpriced rather than blending a real number against a missing one); (3)
/// `None` — an operator who wants this model routable must supply a price
/// through one of the first two, never a fabricated one.
fn resolve_hosted_price(
    config: &codypendent_runtime::models::ModelConfig,
    catalog: &codypendent_providers::Catalog,
    override_per_1m_usd: Option<f64>,
) -> Option<f64> {
    if let Some(price) = override_per_1m_usd {
        return Some(price / 1000.0);
    }
    let provider_id = config.provider_id.as_deref()?;
    let row = catalog.model(provider_id, &config.model)?;
    let (input, output) = (row.cost_per_1m_input_usd?, row.cost_per_1m_output_usd?);
    Some(codypendent_runtime::bench::blended_price_per_1k_usd(
        input, output,
    ))
}

/// Run the bench against `target` and persist the measured profile to the store,
/// returning the persisted profile. The persistence core, split from
/// [`models_bench`] so a test drives it with a scripted target and a temp DB
/// (no model, no network). `known_price_per_1k_usd` rides straight into
/// [`BenchOutcome::into_profile`] — see that method's doc comment for why a
/// hosted model needs one to be routable at all.
async fn bench_to_store(
    pool: &sqlx::SqlitePool,
    endpoint: &str,
    target: &dyn codypendent_runtime::bench::BenchTarget,
    options: codypendent_runtime::bench::BenchOptions,
    known_price_per_1k_usd: Option<f64>,
) -> anyhow::Result<codypendent_routing::ModelProfile> {
    let outcome = codypendent_runtime::bench::run_bench(target, options)
        .await
        .map_err(|reason| anyhow::anyhow!("bench failed: {reason}"))?;
    // Derive the location from the endpoint — never assume local. A non-local
    // endpoint stored as `Local` would short-circuit the routing hard filter
    // (`endpoint_location` fails closed to `Hosted`).
    let location = codypendent_runtime::bench::endpoint_location(endpoint);
    let profile = outcome.into_profile(location, known_price_per_1k_usd);
    codypendent_daemon::model_profiles::ModelProfileStore::new()
        .upsert(pool, endpoint, &profile)
        .await
        .context("persisting the measured model profile")?;
    Ok(profile)
}

/// `codypendent models list-providers`: the built-in + user-extended catalog's
/// providers, one per line — id, wire protocol, and how many models it
/// curates (F8: the `models add --help` text has always pointed here; this is
/// what makes that true). Local providers (Ollama, LM Studio, vLLM) are
/// marked so a user scanning the list can tell which ones need no API key.
///
/// Rows `models add` cannot serve carry the reason, from the same
/// [`crate::tui::provider_unusable_reason`] that refuses them — the review
/// found this listing printing 6 such providers unmarked, so a user picked one,
/// hit a bare "has no base URL and cannot be added", and had nothing telling
/// them which of the 42 rows were real.
pub fn models_list_providers(paths: &RuntimePaths) -> anyhow::Result<()> {
    let catalog = codypendent_providers::Catalog::load_with_user_overrides(
        &paths.data_dir.join("providers.toml"),
    )
    .unwrap_or_else(|_| codypendent_providers::Catalog::builtin());
    for provider in catalog.providers() {
        let curated = catalog
            .models()
            .filter(|m| m.provider_id == provider.id)
            .count();
        let protocol = match provider.protocol {
            codypendent_providers::Protocol::OpenAiChat => "openai-chat",
            codypendent_providers::Protocol::Anthropic => "anthropic",
            codypendent_providers::Protocol::GeminiNative => "gemini-native",
            codypendent_providers::Protocol::Acp => "acp",
            _ => "unknown",
        };
        println!(
            "{:20} {:14} {} model(s) curated{}",
            provider.id,
            protocol,
            curated,
            if provider.local { "  (local)" } else { "" }
        );
        if let Some(reason) = crate::tui::provider_unusable_reason(provider) {
            println!("{:20} └─ not addable: {reason}", "");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `codypendent routing status | enable | disable` (outcome 11)
// ---------------------------------------------------------------------------
//
// `crates/codypendentd/src/routing.rs`'s `RoutingConfig::load` reads
// `<data_dir>/routing.toml`, but nothing in the shipped CLI ever wrote one —
// the 2026-08-13 review (`2026-08-13-verticals/sandbox-eval-routing.md`,
// 11.8) found the routing-decision explanation surface fully built and
// reachable from real code, gated behind exactly this file, with "no CLI
// command that writes routing.toml" as the one missing piece keeping it from
// ever firing on a default install. These three commands are that piece.
//
// This is a SEPARATE writer from `RoutingConfig::load`'s reader by
// necessity (`codypendentd`'s `routing` module is private to that crate, and
// `crates/cli` cannot reach its `RoutingConfigFile` type — see this
// function's own doc comments for why that boundary is intentional rather
// than worked around), so it deliberately does the minimum a shared-file
// writer must: read the file as a generic [`toml::Value`] (never a struct
// that only models the keys this command knows about), touch only the
// specific keys each subcommand is documented to set, and write everything
// else in the table back untouched — an operator's hand-edited `[policy]`
// table (there is no CLI surface for authoring one; RULE: don't build a
// second, narrower one that silently drops it) survives `routing enable`/
// `disable` exactly as they left it.

/// `codypendent routing status`: whether the routing seam is enabled, and
/// what `<data_dir>/routing.toml` currently declares. Prints the raw file
/// state, not a re-validated one — `codypendentd`'s own fail-closed loader
/// (which rejects a malformed policy) is the daemon-side authority; this is
/// "here is what is on disk," useful precisely when those two might disagree.
pub fn routing_status(paths: &RuntimePaths) -> anyhow::Result<()> {
    let path = paths.data_dir.join("routing.toml");
    let Some(doc) = read_routing_toml(&path)? else {
        println!(
            "routing: disabled (no {} — the Phase-1 resolver picks a model)",
            path.display()
        );
        return Ok(());
    };
    let enabled = doc
        .get("enabled")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);
    println!(
        "routing: {} ({})",
        if enabled { "ENABLED" } else { "disabled" },
        path.display()
    );
    match doc
        .get("data_classification")
        .and_then(|v| v.get("type"))
        .and_then(toml::Value::as_str)
    {
        Some(kind) => println!("  data_classification ceiling: {kind}"),
        None => println!(
            "  data_classification ceiling: (undeclared — fails closed to Unknown, local-only)"
        ),
    }
    println!(
        "  policy: {}",
        if doc.get("policy").is_some() {
            "custom (see routing.toml [policy])"
        } else {
            "default (router/balanced/1)"
        }
    );
    if enabled {
        println!(
            "  note: routing also requires at least one benched profile \
             (`codypendent models bench <id>`) — with none, every run still \
             fails closed rather than silently falling back."
        );
    }
    Ok(())
}

/// `codypendent routing enable [--data-classification <level>]`: sets
/// `enabled = true`, creating `routing.toml` if absent. `--data-classification`
/// (`public`|`internal`|`confidential`|`secret`, case-insensitive) sets the
/// operator-declared ceiling; without it the ceiling stays whatever the file
/// already had, or the fail-closed `Unknown` default (local-only routing) on a
/// fresh file — enabling routing is never, by itself, an act of permitting
/// off-device data.
pub async fn routing_enable(
    paths: &RuntimePaths,
    data_classification: Option<&str>,
) -> anyhow::Result<()> {
    let path = paths.data_dir.join("routing.toml");
    let mut doc = read_routing_toml(&path)?.unwrap_or_else(empty_table);
    let has_classification = {
        let table = doc.as_table_mut().ok_or_else(|| {
            anyhow::anyhow!("{}: not a TOML table at the top level", path.display())
        })?;
        table.insert("enabled".to_string(), toml::Value::Boolean(true));
        if let Some(level) = data_classification {
            let variant = classification_variant_name(level).ok_or_else(|| {
                anyhow::anyhow!(
                    "--data-classification `{level}`: expected one of public, internal, \
                     confidential, secret"
                )
            })?;
            let mut classification = toml::map::Map::new();
            classification.insert("type".to_string(), toml::Value::String(variant.to_string()));
            table.insert(
                "data_classification".to_string(),
                toml::Value::Table(classification),
            );
        }
        table.contains_key("data_classification")
    };
    write_routing_toml(&path, &doc)?;
    report_routing_change(paths, "enabled", &path).await;
    if !has_classification {
        println!(
            "  data_classification ceiling is undeclared — fails closed to Unknown \
             (local-only). Pass --data-classification to permit hosted models."
        );
    }
    println!("  next: `codypendent models bench <id>` at least one model, then run normally.");
    Ok(())
}

/// Whether a daemon is answering right now.
///
/// `routing.toml` is read ONCE, when `RuntimeExecutor` builds its
/// `RoutingCoordinator`, and held in an `Arc` for the daemon's lifetime — there
/// is no reload and no IPC notification. So a running daemon keeps routing
/// exactly as it was until it restarts, and a bare "routing: disabled" would
/// tell a user their data had stopped going off-device while it was still
/// going off-device. That is the one thing this command must not do.
async fn daemon_is_live(paths: &RuntimePaths) -> bool {
    client::ping(&paths.socket_path).await
}

/// Print the truth about when a routing change takes effect.
async fn report_routing_change(paths: &RuntimePaths, what: &str, path: &std::path::Path) {
    if daemon_is_live(paths).await {
        println!("routing: {what} in {}", path.display());
        println!(
            "  the running daemon still has the PREVIOUS routing policy loaded — it reads \
             routing.toml once at startup."
        );
        println!("  run `codypendent daemon restart` to apply it.");
    } else {
        println!("routing: {what} ({})", path.display());
    }
}

/// `codypendent routing disable`: sets `enabled = false`, preserving every
/// other declared key (a `disable`/`enable` round trip must not discard a
/// hand-set policy or classification ceiling). A no-op, not an error, when no
/// `routing.toml` exists yet — routing is already off.
pub async fn routing_disable(paths: &RuntimePaths) -> anyhow::Result<()> {
    let path = paths.data_dir.join("routing.toml");
    let Some(mut doc) = read_routing_toml(&path)? else {
        println!("routing: already disabled (no {})", path.display());
        return Ok(());
    };
    let table = doc
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("{}: not a TOML table at the top level", path.display()))?;
    table.insert("enabled".to_string(), toml::Value::Boolean(false));
    write_routing_toml(&path, &doc)?;
    report_routing_change(paths, "disabled", &path).await;
    Ok(())
}

/// Map a case-insensitive CLI spelling to `DataClassification`'s exact
/// `#[serde(tag = "type")]` variant name (`crates/protocol/src/artifact.rs`) —
/// the derived (un-renamed) Serde encoding is the Rust variant name verbatim
/// (`"Internal"`, not `"internal"`), so this is not optional sugar; the wrong
/// case is a silent parse failure the NEXT time `routing.toml` is read.
/// `Unknown` is deliberately not offered — it is the fail-closed default an
/// operator reaches by declaring nothing, not something to opt into.
fn classification_variant_name(level: &str) -> Option<&'static str> {
    match level.to_ascii_lowercase().as_str() {
        "public" => Some("Public"),
        "internal" => Some("Internal"),
        "confidential" => Some("Confidential"),
        "secret" => Some("Secret"),
        _ => None,
    }
}

fn empty_table() -> toml::Value {
    toml::Value::Table(toml::map::Map::new())
}

/// Read `path` as a generic TOML document. `Ok(None)` for an absent file
/// (routing is simply off); a present-but-malformed file is a hard error here
/// — surfacing the exact parse problem beats silently treating it as absent
/// and clobbering whatever the operator meant to keep on a later `enable`/
/// `disable`.
fn read_routing_toml(path: &Path) -> anyhow::Result<Option<toml::Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let value: toml::Value =
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
            Ok(Some(value))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_routing_toml(path: &Path, doc: &toml::Value) -> anyhow::Result<()> {
    let text =
        toml::to_string_pretty(doc).with_context(|| format!("serializing {}", path.display()))?;
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// The conservative declared capabilities a benched local model is described
/// with until a real endpoint capability-discovery probe exists (the model-
/// driver seam surfaces none): streaming (OpenAI-compatible endpoints stream),
/// single tool calls, best-effort JSON mode, and an unadvertised (unbounded)
/// context window. Documented as declared, not measured.
fn default_bench_description() -> codypendent_runtime::bench::TargetDescription {
    use codypendent_routing::{ModelCapabilities, StructuredOutputSupport, ToolCallSupport};
    codypendent_runtime::bench::TargetDescription {
        capabilities: ModelCapabilities {
            streaming: true,
            tools: ToolCallSupport::Single,
            parallel_tools: false,
            structured_output: StructuredOutputSupport::JsonMode,
            vision: false,
            audio_input: false,
            embeddings: false,
            prompt_caching: false,
            reasoning_controls: false,
            context_tokens: None,
            output_tokens: None,
        },
        context_limit: 0,
        memory_mb: 0,
    }
}

/// `codypendent completion <shell>`: write a shell-completion script to stdout,
/// generated from the app's own clap [`Command`](clap::Command) so completions
/// never drift from the real CLI. The caller passes the derived command (only
/// `main.rs` has the `Cli` type), keeping this reusable and testable.
pub fn completion(shell: clap_complete::Shell, cmd: &mut clap::Command) {
    completion_to(shell, cmd, &mut std::io::stdout());
}

/// The testable core of [`completion`]: generate into any writer instead of
/// stdout, so a test can assert the script is non-empty and names the binary.
pub fn completion_to(
    shell: clap_complete::Shell,
    cmd: &mut clap::Command,
    out: &mut impl std::io::Write,
) {
    let name = cmd.get_name().to_string();
    clap_complete::generate(shell, cmd, name, out);
}

#[cfg(test)]
mod completion_tests {
    use super::*;

    /// Every supported shell generates a non-empty script that names the binary,
    /// against a stand-in command mirroring the real one's name + a subcommand.
    #[test]
    fn generates_non_empty_scripts_naming_the_binary() {
        for shell in [
            clap_complete::Shell::Bash,
            clap_complete::Shell::Zsh,
            clap_complete::Shell::Fish,
        ] {
            let mut cmd = clap::Command::new("codypendent")
                .subcommand(clap::Command::new("daemon"))
                .subcommand(clap::Command::new("doctor"));
            let mut out = Vec::new();
            completion_to(shell, &mut cmd, &mut out);
            let script = String::from_utf8(out).expect("completion output is valid UTF-8");
            assert!(!script.is_empty(), "{shell} completion must not be empty");
            assert!(
                script.contains("codypendent"),
                "{shell} completion must name the binary"
            );
        }
    }
}

#[cfg(test)]
mod models_bench_tests {
    use super::*;
    use async_trait::async_trait;
    use codypendent_protocol::ModelId;
    use codypendent_routing::{ModelCapabilities, StructuredOutputSupport, ToolCallSupport};
    use codypendent_runtime::bench::{
        BenchOptions, BenchTarget, GenerationSample, TargetDescription,
    };
    use std::time::Duration;

    /// A local scripted bench target (the CLI need not depend on the runtime's
    /// test-only mock): returns fixed numbers so `bench_to_store`'s persistence
    /// wiring runs with no model or network.
    struct ScriptedTarget;

    #[async_trait]
    impl BenchTarget for ScriptedTarget {
        fn model_id(&self) -> ModelId {
            ModelId("qwen-local".into())
        }
        async fn describe(&self) -> Result<TargetDescription, String> {
            Ok(TargetDescription {
                capabilities: ModelCapabilities {
                    streaming: true,
                    tools: ToolCallSupport::Parallel,
                    parallel_tools: true,
                    structured_output: StructuredOutputSupport::JsonMode,
                    vision: false,
                    audio_input: false,
                    embeddings: false,
                    prompt_caching: false,
                    reasoning_controls: false,
                    context_tokens: Some(128_000),
                    output_tokens: Some(8_192),
                },
                context_limit: 128_000,
                memory_mb: 9_200,
            })
        }
        async fn timed_generation(&self, warm: bool) -> Result<GenerationSample, String> {
            Ok(GenerationSample {
                tokens: 100,
                time_to_first_token: Duration::from_millis(if warm { 180 } else { 400 }),
                total: Duration::from_millis(if warm { 2_000 } else { 2_500 }),
            })
        }
        async fn structured_output_probe(&self, n: u32) -> Result<u32, String> {
            Ok(n.min(8))
        }
        async fn tool_call_probe(&self, n: u32) -> Result<u32, String> {
            Ok(n.min(7))
        }
        async fn coding_eval(&self, n: u32) -> Result<u32, String> {
            Ok(n.min(6))
        }
    }

    #[tokio::test]
    async fn bench_to_store_measures_and_persists_the_profile() {
        let dir = tempfile::tempdir().unwrap();
        let pool = codypendent_daemon::db::open_database(&dir.path().join("codypendent.db"))
            .await
            .unwrap();
        let endpoint = "http://localhost:11434/v1";

        let profile = bench_to_store(
            &pool,
            endpoint,
            &ScriptedTarget,
            BenchOptions::default(),
            None,
        )
        .await
        .unwrap();
        assert!(profile.is_local());
        assert_eq!(profile.id, ModelId("qwen-local".into()));

        // It is durably persisted and reads back identically.
        let stored = codypendent_daemon::model_profiles::ModelProfileStore::new()
            .get(&pool, &ModelId("qwen-local".into()), endpoint)
            .await
            .unwrap()
            .expect("the benched profile is persisted");
        assert_eq!(stored, profile);
        // The measured bench survived (50 tok/s from 100 tokens over 2.0s warm).
        assert!((stored.bench.unwrap().tokens_per_second - 50.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn benching_a_non_local_endpoint_stores_hosted_not_local() {
        // Finding-2 pin: a cloud endpoint must NOT be persisted as local (which
        // would short-circuit the routing hard filter). `bench_to_store` derives
        // the location from the endpoint (fail-closed to hosted).
        let dir = tempfile::tempdir().unwrap();
        let pool = codypendent_daemon::db::open_database(&dir.path().join("codypendent.db"))
            .await
            .unwrap();
        let profile = bench_to_store(
            &pool,
            "https://api.openai.com/v1",
            &ScriptedTarget,
            BenchOptions::default(),
            None,
        )
        .await
        .unwrap();
        assert!(
            !profile.is_local(),
            "a non-local base_url is stored as hosted, so the classification filter still applies"
        );
        assert_eq!(
            profile.performance.cost_per_1k_tokens_usd, 0.0,
            "with no catalog/override price, a hosted profile stays unpriced \
             (never a fabricated price) — and therefore ineligible for routing"
        );
    }

    fn model_config(
        model: &str,
        provider_id: Option<&str>,
    ) -> codypendent_runtime::models::ModelConfig {
        codypendent_runtime::models::ModelConfig {
            id: ModelId(format!("test/{model}")),
            provider: "openai-compatible".to_string(),
            base_url: "https://api.example.test/v1".to_string(),
            model: model.to_string(),
            api_key_env: String::new(),
            provider_id: provider_id.map(str::to_owned),
            context_tokens: None,
        }
    }

    #[test]
    fn resolve_hosted_price_prefers_the_explicit_override() {
        let catalog = codypendent_providers::Catalog::builtin();
        let config = model_config("claude-opus-5", Some("anthropic"));
        // $10/1M override -> $0.01/1K, regardless of what the catalog says.
        assert_eq!(
            resolve_hosted_price(&config, &catalog, Some(10.0)),
            Some(0.01)
        );
    }

    #[test]
    fn resolve_hosted_price_falls_back_to_the_catalogs_blended_price() {
        let catalog = codypendent_providers::Catalog::builtin();
        // The curated anthropic/claude-opus-5 row: $5/1M in, $25/1M out ->
        // blended $15/1M -> $0.015/1K.
        let config = model_config("claude-opus-5", Some("anthropic"));
        assert_eq!(resolve_hosted_price(&config, &catalog, None), Some(0.015));
    }

    #[test]
    fn resolve_hosted_price_is_none_when_neither_source_has_one() {
        let catalog = codypendent_providers::Catalog::builtin();
        // No provider_id at all.
        assert_eq!(
            resolve_hosted_price(&model_config("mystery", None), &catalog, None),
            None
        );
        // A provider_id the catalog does not curate this model under.
        assert_eq!(
            resolve_hosted_price(
                &model_config("not-a-real-model-xyz", Some("anthropic")),
                &catalog,
                None
            ),
            None
        );
    }
}

#[cfg(test)]
mod routing_command_tests {
    use super::*;
    use codypendent_protocol::discovery::RuntimePaths;

    fn temp_paths() -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        (dir, paths)
    }

    #[test]
    fn classification_variant_name_is_case_insensitive_and_matches_the_wire_spelling() {
        // Must match `DataClassification`'s un-renamed Serde encoding exactly
        // (`crates/protocol/src/artifact.rs`) — the Rust variant name, PascalCase.
        assert_eq!(classification_variant_name("internal"), Some("Internal"));
        assert_eq!(classification_variant_name("INTERNAL"), Some("Internal"));
        assert_eq!(
            classification_variant_name("Confidential"),
            Some("Confidential")
        );
        assert_eq!(classification_variant_name("public"), Some("Public"));
        assert_eq!(classification_variant_name("secret"), Some("Secret"));
        // `Unknown` is the fail-closed default, deliberately not an accepted spelling.
        assert_eq!(classification_variant_name("unknown"), None);
        assert_eq!(classification_variant_name("nonsense"), None);
    }

    #[tokio::test]
    async fn routing_enable_creates_the_file_and_status_reflects_it() {
        let (_dir, paths) = temp_paths();
        let path = paths.data_dir.join("routing.toml");
        assert!(!path.exists());

        routing_enable(&paths, Some("internal")).await.unwrap();
        assert!(path.exists());

        let doc: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            doc.get("enabled").and_then(toml::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            doc.get("data_classification")
                .and_then(|v| v.get("type"))
                .and_then(toml::Value::as_str),
            Some("Internal")
        );

        // `status` does not error and does not require re-parsing here — this
        // just pins that it runs cleanly against what `enable` just wrote.
        routing_status(&paths).unwrap();
    }

    #[tokio::test]
    async fn routing_enable_rejects_an_unknown_classification_and_writes_nothing() {
        let (_dir, paths) = temp_paths();
        let path = paths.data_dir.join("routing.toml");
        let err = routing_enable(&paths, Some("nonsense")).await.unwrap_err();
        assert!(err.to_string().contains("nonsense"));
        assert!(
            !path.exists(),
            "a rejected classification must not leave a half-written routing.toml"
        );
    }

    #[tokio::test]
    async fn routing_disable_preserves_everything_else_including_an_unmodeled_policy_table() {
        let (_dir, paths) = temp_paths();
        let path = paths.data_dir.join("routing.toml");
        // A hand-authored file with a full [policy] table this command's
        // struct-free toml::Value approach has never heard of — the "don't
        // build a second, narrower writer" requirement this module's own doc
        // comment states.
        std::fs::write(
            &path,
            r#"
enabled = true

[data_classification]
type = "Confidential"

[policy]
name = "coding"
version = 3
quality_threshold = 0.7
max_off_device = { type = "Confidential" }

[policy.lambdas]
cost = 1.0
latency = 0.05
privacy = 0.5
failure = 0.5
"#,
        )
        .unwrap();

        routing_disable(&paths).await.unwrap();

        let doc: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            doc.get("enabled").and_then(toml::Value::as_bool),
            Some(false),
            "enabled must flip to false"
        );
        assert_eq!(
            doc.get("data_classification")
                .and_then(|v| v.get("type"))
                .and_then(toml::Value::as_str),
            Some("Confidential"),
            "the classification ceiling survives a disable"
        );
        assert_eq!(
            doc.get("policy")
                .and_then(|p| p.get("name"))
                .and_then(toml::Value::as_str),
            Some("coding"),
            "the hand-authored [policy] table survives untouched"
        );
        assert_eq!(
            doc.get("policy")
                .and_then(|p| p.get("lambdas"))
                .and_then(|l| l.get("cost"))
                .and_then(toml::Value::as_float),
            Some(1.0),
            "nested [policy.lambdas] survives too"
        );
    }

    #[tokio::test]
    async fn routing_disable_without_a_file_is_a_clean_no_op() {
        let (_dir, paths) = temp_paths();
        // Must not error and must not create a file just to say "already off".
        routing_disable(&paths).await.unwrap();
        assert!(!paths.data_dir.join("routing.toml").exists());
    }

    #[tokio::test]
    async fn routing_status_without_a_file_reports_disabled_and_does_not_error() {
        let (_dir, paths) = temp_paths();
        routing_status(&paths).unwrap();
    }

    #[tokio::test]
    async fn enable_then_disable_round_trips_without_dropping_the_classification() {
        let (_dir, paths) = temp_paths();
        routing_enable(&paths, Some("secret")).await.unwrap();
        routing_disable(&paths).await.unwrap();
        let doc: toml::Value =
            toml::from_str(&std::fs::read_to_string(paths.data_dir.join("routing.toml")).unwrap())
                .unwrap();
        assert_eq!(
            doc.get("enabled").and_then(toml::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            doc.get("data_classification")
                .and_then(|v| v.get("type"))
                .and_then(toml::Value::as_str),
            Some("Secret"),
            "disable must not erase a classification enable had set"
        );
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::*;

    const GITHUB_MANIFEST: &str = r#"
schema_version = 1
id = "github"
name = "GitHub Integration"
version = "0.1.0"
kind = "native-process"
publisher = "codypendent-project"
scopes = ["user", "organization", "repository"]
[runtime]
command = "codypendent-plugin-github"
protocol = "mcp-stdio"
[capabilities]
network = ["api.github.com:443", "uploads.github.com:443"]
secrets = ["github-token"]
subprocess = false
[resources]
memory_mb = 256
cpu_seconds = 60
wall_seconds = 120
maximum_output_mb = 20
[security]
checksum = "sha256:set-during-packaging"
signature = "set-during-packaging"
sandbox_profile = "network-client"
"#;

    #[test]
    fn report_renders_identity_capabilities_and_trust() {
        let manifest = codypendent_sandbox::parse_manifest(GITHUB_MANIFEST).unwrap();
        let report = plugin_report(&manifest);
        assert!(report.contains("github v0.1.0 (native-process)"));
        assert!(report.contains("trust: unsigned"));
        assert!(report.contains("sandbox profile network-client"));
        // The capability list is rendered verbatim, one per line.
        assert!(report.contains("network: api.github.com:443"));
        assert!(report.contains("network: uploads.github.com:443"));
        assert!(report.contains("secret: github-token"));
        assert!(report.contains("256 MB mem"));
        assert!(report.contains("scopes: user, organization, repository"));
    }

    #[test]
    fn report_notes_a_capability_free_plugin() {
        let manifest = codypendent_sandbox::parse_manifest(
            "schema_version = 1\nid = \"theme\"\nname = \"T\"\nversion = \"1.0.0\"\nkind = \"wasm-component\"\npublisher = \"me\"\n[runtime]\ncommand = \"t.wasm\"\n",
        )
        .unwrap();
        let report = plugin_report(&manifest);
        assert!(report.contains("requests no capabilities"));
    }

    #[test]
    fn inspect_reads_a_manifest_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugin.toml");
        std::fs::write(&path, GITHUB_MANIFEST).unwrap();
        plugin_inspect(&path).expect("inspect succeeds on a valid manifest");
    }

    #[test]
    fn inspect_surfaces_a_parse_error_with_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "schema_version = 99\nid = \"x\"\n").unwrap();
        let err = plugin_inspect(&path).unwrap_err().to_string();
        assert!(err.contains("bad.toml"), "error names the file: {err}");
    }

    fn diff_report_for(installed_net: &[&str], update_net: &[&str]) -> String {
        let spec = |net: &[&str]| codypendent_sandbox::CapabilitiesSpec {
            filesystem_read: vec![],
            filesystem_write: vec![],
            network: net.iter().map(|s| s.to_string()).collect(),
            secrets: vec![],
            subprocess: false,
        };
        let old = codypendent_sandbox::CapabilitySet::from_spec(&spec(installed_net));
        let new = codypendent_sandbox::CapabilitySet::from_spec(&spec(update_net));
        plugin_diff_report("github", &old.diff_to(&new))
    }

    #[test]
    fn diff_report_flags_an_expanding_update() {
        let report = diff_report_for(
            &["api.github.com:443"],
            &["api.github.com:443", "uploads.github.com:443"],
        );
        assert!(report.contains("+ network: uploads.github.com:443"));
        assert!(report.contains("EXPANDS permissions"));
    }

    #[test]
    fn diff_report_marks_an_identical_update_safe() {
        let report = diff_report_for(&["api.github.com:443"], &["api.github.com:443"]);
        assert!(report.contains("no permission changes"));
    }

    #[test]
    fn diff_report_marks_a_narrowing_update_safe() {
        let report = diff_report_for(&["a:1", "b:2"], &["a:1"]);
        assert!(report.contains("only narrows"));
        assert!(!report.contains("EXPANDS"));
    }

    #[test]
    fn diff_command_exits_nonzero_when_permissions_expand() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("installed.toml");
        let update = dir.path().join("update.toml");
        std::fs::write(&installed, GITHUB_MANIFEST).unwrap();
        // The update adds a filesystem_read capability.
        let expanded = GITHUB_MANIFEST.replace(
            "network = [\"api.github.com:443\", \"uploads.github.com:443\"]",
            "network = [\"api.github.com:443\", \"uploads.github.com:443\"]\nfilesystem_read = [\"/etc\"]",
        );
        std::fs::write(&update, expanded).unwrap();
        let err = plugin_diff(&installed, &update).unwrap_err().to_string();
        assert!(err.contains("re-approval required"), "got: {err}");
    }

    #[test]
    fn diff_command_exits_nonzero_when_a_resource_cap_is_raised() {
        // P6-A fix pass: identical capabilities, only the memory cap raised.
        // plugin_diff() now routes through codypendent_sandbox::diff_manifests(),
        // which folds resource-cap changes into the diff. Before that, this
        // computed via a bare CapabilitySet diff that never saw resources,
        // so it printed "no permission changes — safe to update" and exited
        // 0 on exactly the update this CI gate exists to catch.
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("installed.toml");
        let update = dir.path().join("update.toml");
        std::fs::write(&installed, GITHUB_MANIFEST).unwrap();
        let raised = GITHUB_MANIFEST.replace("memory_mb = 256", "memory_mb = 4096");
        std::fs::write(&update, raised).unwrap();
        let err = plugin_diff(&installed, &update).unwrap_err().to_string();
        assert!(err.contains("re-approval required"), "got: {err}");
    }

    #[test]
    fn diff_rejects_two_different_plugins() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.toml");
        let b = dir.path().join("b.toml");
        std::fs::write(&a, GITHUB_MANIFEST).unwrap();
        std::fs::write(
            &b,
            GITHUB_MANIFEST.replace("id = \"github\"", "id = \"gitlab\""),
        )
        .unwrap();
        let err = plugin_diff(&a, &b).unwrap_err().to_string();
        assert!(err.contains("different plugins"));
    }
}

#[cfg(test)]
mod daemon_spawn_tests {
    use super::*;

    #[test]
    fn resolves_self_with_hidden_daemon_subcommand() {
        let inv = resolve_daemon_invocation(Ok(PathBuf::from("/opt/bin/codypendent")));
        assert_eq!(inv.program, PathBuf::from("/opt/bin/codypendent"));
        assert_eq!(inv.args, vec!["__daemon".to_string()]);
    }

    #[test]
    fn falls_back_to_path_codypendentd_when_current_exe_unavailable() {
        let err = std::io::Error::new(std::io::ErrorKind::NotFound, "no current_exe");
        let inv = resolve_daemon_invocation(Err(err));
        assert_eq!(inv.program, PathBuf::from("codypendentd"));
        assert!(inv.args.is_empty());
    }

    #[test]
    fn resolve_daemon_binary_uses_current_exe() {
        // In the test process `current_exe` is available, so the primary
        // self-spawn form is chosen: program == this binary, args == __daemon.
        let inv = resolve_daemon_binary();
        assert_eq!(inv.program, std::env::current_exe().unwrap());
        assert_eq!(inv.args, vec!["__daemon".to_string()]);
    }

    #[test]
    fn daemon_command_argv_is_program_then_daemon() {
        let inv = DaemonInvocation {
            program: PathBuf::from("/opt/bin/codypendent"),
            args: vec!["__daemon".to_string()],
        };
        let command = daemon_command(&inv);
        assert_eq!(
            command.get_program(),
            std::ffi::OsStr::new("/opt/bin/codypendent")
        );
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, vec![std::ffi::OsStr::new("__daemon")]);
    }

    #[test]
    fn daemon_command_argv_for_the_fallback_has_no_args() {
        // The `codypendentd` fallback (used when `current_exe` is unavailable)
        // is the standalone daemon binary, which does NOT parse `__daemon` — so
        // `daemon_command` must build it with an empty argv.
        let inv = DaemonInvocation {
            program: PathBuf::from("codypendentd"),
            args: vec![],
        };
        let command = daemon_command(&inv);
        assert_eq!(command.get_program(), std::ffi::OsStr::new("codypendentd"));
        assert_eq!(command.get_args().count(), 0);
    }
}

#[cfg(test)]
mod daemon_status_render_tests {
    use super::*;
    use chrono::Utc;
    use codypendent_protocol::{DaemonInstanceId, PROTOCOL_V1};

    fn sample_status() -> DaemonStatus {
        DaemonStatus {
            daemon_version: "0.1.0".to_string(),
            protocol_version: PROTOCOL_V1,
            instance_id: DaemonInstanceId::new(),
            pid: 4242,
            started_at: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            uptime_seconds: 3600,
            boot_count: 1,
            database_path: "/home/user/.local/share/codypendent/codypendent.db".to_string(),
            socket_path: "/home/user/.local/share/codypendent/run/daemon.sock".to_string(),
            session_count: 2,
            build_id: "0.1.0+a1b2c3d4e5f6".to_string(),
            active_run_count: 3,
            integration_issues: Vec::new(),
        }
    }

    #[test]
    fn render_status_text_shows_the_build_id_and_active_run_count() {
        let text = render_status_text(&sample_status());
        assert!(
            text.contains("build        0.1.0+a1b2c3d4e5f6"),
            "got: {text}"
        );
        assert!(text.contains("active runs  3"), "got: {text}");
        // The existing fields are still there — this is additive, not a rewrite.
        assert!(text.contains("version      0.1.0"));
        assert!(text.contains("sessions     2"));
        assert!(text.contains("integrations  healthy"));
    }

    #[test]
    fn render_status_text_lists_integration_issues() {
        let mut status = sample_status();
        status.integration_issues = vec!["MCP server `local` failed to start".to_string()];
        let text = render_status_text(&status);
        assert!(text.contains("integrations  1 issue(s)"));
        assert!(text.contains("- MCP server `local` failed to start"));
    }
}

#[cfg(test)]
mod daemon_restart_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn test_paths() -> RuntimePaths {
        RuntimePaths::from_data_dir(std::env::temp_dir().join("cp-restart-composition-test"))
    }

    #[tokio::test]
    async fn stops_before_starting_when_a_daemon_is_running() {
        let calls = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let stop_calls = calls.clone();
        let start_calls = calls.clone();

        let outcome = restart_daemon_with(
            test_paths(),
            move |_paths| {
                stop_calls.lock().unwrap().push("stop");
                async { Ok(true) }
            },
            move |_paths| {
                start_calls.lock().unwrap().push("start");
                async { Ok(EnsureOutcome::Started { pid: 4242 }) }
            },
        )
        .await
        .expect("both steps report success");

        assert_eq!(*calls.lock().unwrap(), vec!["stop", "start"]);
        assert!(matches!(outcome, EnsureOutcome::Started { pid: 4242 }));
    }

    #[tokio::test]
    async fn is_idempotent_and_just_starts_when_nothing_was_running() {
        // `stop` reporting "nothing was running" (`Ok(false)`) is not an
        // error — `restart_daemon` still proceeds to `start`, matching
        // `daemon start`'s own behaviour.
        let outcome = restart_daemon_with(
            test_paths(),
            |_paths| async { Ok(false) },
            |_paths| async { Ok(EnsureOutcome::Started { pid: 99 }) },
        )
        .await
        .expect("start-only path succeeds");
        assert!(matches!(outcome, EnsureOutcome::Started { pid: 99 }));
    }

    #[tokio::test]
    async fn surfaces_a_legible_error_and_never_starts_when_stop_fails() {
        let err = restart_daemon_with(
            test_paths(),
            |_paths| async {
                anyhow::bail!("daemon acknowledged shutdown but is still answering after 5 seconds")
            },
            |_paths| async {
                panic!("start must not run when stop fails — restart must not hang or half-restart")
            },
        )
        .await
        .unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains("stopping the running daemon before restart"),
            "got: {message}"
        );
        assert!(
            message.contains("still answering after 5 seconds"),
            "got: {message}"
        );
    }

    #[tokio::test]
    async fn surfaces_a_legible_error_when_start_fails() {
        let err = restart_daemon_with(
            test_paths(),
            |_paths| async { Ok(true) },
            |_paths| async {
                anyhow::bail!("daemon did not become ready within 5 seconds; check /tmp/daemon.log")
            },
        )
        .await
        .unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains("starting a fresh daemon after restart"),
            "got: {message}"
        );
        assert!(message.contains("did not become ready"), "got: {message}");
    }
}

#[cfg(test)]
mod docs_publish_outcome_tests {
    use super::*;
    use codypendent_protocol::ApprovalId;

    /// A migrated database holding the foreign-key chain a publish job needs
    /// (`sessions` -> `runs`, plus `documents`), and the ids to address it by.
    async fn seeded_pool(
        dir: &std::path::Path,
    ) -> (sqlx::SqlitePool, DocumentId, codypendent_protocol::RunId) {
        let pool = knowledge_db::open(&dir.join("codypendent.db"))
            .await
            .expect("migrated database");
        let session_id = SessionId::new();
        let run_id = codypendent_protocol::RunId::new();
        let document_id = DocumentId::new();
        let now = "2026-08-13T00:00:00Z";

        sqlx::query(
            "INSERT INTO sessions (id, workspace_id, title, state, created_at, updated_at, \
             revision) VALUES (?, NULL, 'test', 'open', ?, ?, 0)",
        )
        .bind(session_id.to_string())
        .bind(now)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO runs (id, session_id, objective, state, mode, model_policy, \
             budget_json) VALUES (?, ?, 'publish', 'running', 'Build', 'default', '{}')",
        )
        .bind(run_id.to_string())
        .bind(session_id.to_string())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO documents (id, title, scope_json, scope_tier, scope_key, status, \
             metadata_json, crdt_snapshot, links_json, citations_json, revision, created_at, \
             updated_at) VALUES (?, 'Doc', ?, 'system', NULL, 'draft', '{}', ?, '[]', '[]', 1, \
             ?, ?)",
        )
        .bind(document_id.to_string())
        .bind(serde_json::to_string(&Scope::System).unwrap())
        .bind(Vec::<u8>::new())
        .bind(now)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();

        (pool, document_id, run_id)
    }

    async fn park_job(
        pool: &sqlx::SqlitePool,
        approval_id: ApprovalId,
        run_id: codypendent_protocol::RunId,
        document_id: DocumentId,
        state: &str,
    ) {
        sqlx::query(
            "INSERT INTO document_publish_jobs (approval_id, run_id, document_id, plan_json, \
             state, created_at, updated_at) VALUES (?, ?, ?, '{}', ?, ?, ?)",
        )
        .bind(approval_id.to_string())
        .bind(run_id.to_string())
        .bind(document_id.to_string())
        .bind(state)
        .bind("2026-08-13T00:00:00Z")
        .bind("2026-08-13T00:00:00Z")
        .execute(pool)
        .await
        .unwrap();
    }

    /// 2026-08-13 review F8: a publish whose job already says `failed` must
    /// report a failure. Before this, only `document_publications` was polled,
    /// so the CLI printed "still executing … re-run shortly" forever.
    #[tokio::test]
    async fn a_failed_publish_job_is_reported_as_failed_not_still_running() {
        let dir = tempfile::tempdir().unwrap();
        let (pool, document_id, run_id) = seeded_pool(dir.path()).await;
        let approval_id = ApprovalId::new();
        park_job(&pool, approval_id, run_id, document_id, "failed").await;

        let outcome = wait_for_publish_outcome(&pool, document_id, approval_id, 0).await;
        assert!(
            matches!(outcome, PublishOutcome::Failed),
            "a job recorded as failed must not be reported as still running"
        );
    }

    /// A rejected/expired approval never executes: distinct from a failure,
    /// and equally not something re-running the command would resolve.
    #[tokio::test]
    async fn a_cancelled_publish_job_is_reported_as_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let (pool, document_id, run_id) = seeded_pool(dir.path()).await;
        let approval_id = ApprovalId::new();
        park_job(&pool, approval_id, run_id, document_id, "cancelled").await;

        assert!(matches!(
            wait_for_publish_outcome(&pool, document_id, approval_id, 0).await,
            PublishOutcome::Cancelled
        ));
    }

    /// A concurrent publish of the SAME document must not be read as ours —
    /// the probe is keyed by the approval this invocation parked. (Testing the
    /// probe rather than the loop: an "ours is still pending" assertion would
    /// have to sit through the whole poll bound to prove a negative.)
    #[tokio::test]
    async fn the_job_probe_is_keyed_by_approval_not_document() {
        let dir = tempfile::tempdir().unwrap();
        let (pool, document_id, run_id) = seeded_pool(dir.path()).await;
        let theirs = ApprovalId::new();
        let ours = ApprovalId::new();
        park_job(&pool, theirs, run_id, document_id, "failed").await;
        park_job(&pool, ours, run_id, document_id, "pending").await;

        assert_eq!(
            publish_job_state(&pool, ours).await.as_deref(),
            Some("pending")
        );
        assert_eq!(
            publish_job_state(&pool, theirs).await.as_deref(),
            Some("failed")
        );
        // A job row that has not appeared yet is not a verdict either.
        assert_eq!(publish_job_state(&pool, ApprovalId::new()).await, None);
    }
}
