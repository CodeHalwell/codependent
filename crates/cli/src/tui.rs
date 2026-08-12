//! The interactive TUI harness (STEP 1.12).
//!
//! Running `codypendent` with no subcommand opens the Ratatui client attached to
//! the current repository's session (creating it if needed, auto-starting the
//! daemon if needed). The rendering, input mapping, and reducer all live in the
//! pure `codypendent-tui` crate, which performs no I/O; this module is the
//! *harness* that the crate's own docs describe — it owns the protocol
//! connection, the terminal, and the event loop, and it is the only place the
//! two worlds meet.
//!
//! # The loop
//!
//! ```text
//!   crossterm event ─┐                        ┌─▶ reduce(&mut AppState, action)
//!   daemon event ────┼─▶ tokio::select! ─▶ Action        │
//!   200ms tick ──────┘                                   ├─▶ drain outbox → Commands
//!                                                         └─▶ render(frame, &state)
//! ```
//!
//! Two background tasks decouple the socket from the loop so a keystroke never
//! cancels a half-read frame (RULE: no partial-frame loss): a **reader** task
//! owns the read half and forwards each live [`SessionEvent`] to the loop (and
//! answers heartbeat `Ping`s via the writer), and a **writer** task owns the
//! write half and serializes every outgoing envelope — commands from the loop
//! and pongs from the reader. A third OS thread bridges blocking `crossterm`
//! input into the async loop.

use std::collections::{BTreeSet, HashMap};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use codypendent_knowledge::{
    db as knowledge_db, BlockContent, CapabilityRequest, CollaborationMode, DocumentAuthor,
    DocumentBlock, DocumentReplica, DocumentStore, EvidenceRef, KnowledgeDocument, MemoryClass,
    MemoryRecord, MemoryStore, Registry, RegistryItem, RegistryItemKind, RegistryStatus, RiskClass,
    Scope, Suggestion, SuggestionStatus, SuggestionStore, TrustTier,
};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{
    read_envelope, write_envelope, Catchup, ClientId, ClientRole, Command, CommandBody, CommandId,
    DocumentEditLease, DocumentId, DocumentSync, Envelope, ModelId, Payload, RepositoryId,
    SessionEvent, SessionId, Subscription, WorkspaceId,
};
use codypendent_runtime::models::provider_auth_id;
use codypendent_tui::{
    accessible_snapshot, accessible_terminal_capabilities_message, map_accessible_input, map_event,
    reduce, render, render_splash, sanitize_accessible_text, terminal_capabilities_message, Action,
    AddModelRow, AppState, BlackboardItemCard, ColorDepth, DocBlockView, DocCard,
    DocSuggestionView, GraphEdgeCard, Intent, KeyStatus, KeyTarget, MemoryCard, ModelCard,
    ModelListOrigin, ModelLocationLabel, ModelReadiness, ProjectionKind, ProviderCard, SkillCard,
    TerminalGuard, Theme, WorkflowNodeCard, WorkflowNodeUpdate, EDGE_PAGE_SIZE,
};
use crossterm::event::Event as CrosstermEvent;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{mpsc, watch};
use tokio::{io::AsyncBufReadExt, task::JoinHandle};

use crate::commands;
use crate::connection::Connection;

/// How often the loop wakes with a [`Action::Tick`] for spinner / elapsed-timer
/// animation when nothing else is happening (5 fps — cheap, and the loop redraws
/// immediately on any real event anyway).
const TICK: Duration = Duration::from_millis(200);

/// The splash frame cadence while the TUI boots (D2) — ~12 fps so the stage
/// spinner animates smoothly during a multi-second daemon spawn.
const SPLASH_TICK: Duration = Duration::from_millis(80);

/// Full linear snapshots intentionally run slower than graphical frames.
/// Streaming token events would otherwise replay the whole document fast
/// enough to overwhelm a screen reader or redirected output.
const ACCESSIBLE_REFRESH: Duration = Duration::from_secs(1);

/// The most live events [`GapTracker`] will buffer while a gap repair is in
/// flight before giving up on the incremental replay and re-attaching for a
/// fresh catch-up (FP-2a). A slow client behind a fast producer could otherwise
/// grow this buffer without bound; the ledger is the source of truth, so
/// dropping the buffer and re-attaching from `last_seen` re-fetches the whole
/// span losslessly — we fail toward a fresh catch-up, never toward unbounded
/// memory.
const MAX_GAP_BUFFER: usize = 2048;

/// How long [`GapTracker`] waits for a gap repair's catch-up reply before
/// re-attaching afresh (FP-2b). Without a deadline a dropped catch-up reply
/// (the daemon's fan-out is lossy under lag) would wedge the client in
/// `repairing` forever, silently holding back every later event — worst case an
/// `ApprovalRequested`. On expiry we re-attach from `last_seen`, which re-drives
/// the catch-up.
const REPAIR_TIMEOUT: Duration = Duration::from_secs(10);

/// The client-facing subscription set for the TUI: it wants the whole session,
/// not one run's trace.
fn default_subscriptions() -> Vec<Subscription> {
    vec![Subscription::SessionSummary, Subscription::AgentActivity]
}

// ---------------------------------------------------------------------------
// Crash logger (crash investigation follow-up).
//
// A prior TUI crash left the terminal in raw mode — proof `TerminalGuard`'s
// `Drop` never ran, so it was either an abort/OS-kill, or a panic whose
// message was lost the instant the alternate screen was torn down alongside
// it. We could not tell which. `install_crash_hook` closes that gap for the
// NEXT occurrence: it installs a `std::panic::set_hook` that appends the
// panic message, its source location, and a force-captured backtrace to
// `<data_dir>/logs/tui-crash.log` — a plain file, independent of the
// terminal — before chaining to the previous (default) hook.
//
// Diagnostic contract for whoever reads the log after a future crash:
//   - A non-empty file (a new entry since the crash) ⇒ the hook ran ⇒ it was
//     a PANIC, and its message/location/backtrace are right there.
//   - The file stays empty ⇒ the hook never ran ⇒ the process was
//     aborted/OS-killed (e.g. out-of-memory) — that points at memory, not a
//     panic, and a panic-message hunt would be a waste of time.
// ---------------------------------------------------------------------------

/// The crash log's filename under `<data_dir>/logs/` (alongside `daemon.log`).
const CRASH_LOG_FILE_NAME: &str = "tui-crash.log";

/// Format one crash-log entry from an already-extracted message, an optional
/// source location, and a backtrace string. Pure (no I/O) and so trivially
/// unit-testable: the panic hook itself (`install_crash_hook`) cannot easily
/// be driven from a test without actually panicking the test process, but all
/// of its formatting decisions live here, in a plain function a test CAN call
/// directly with synthetic inputs.
fn format_crash_entry(message: &str, location: Option<&str>, backtrace: &str) -> String {
    let location = location.unwrap_or("<unknown location>");
    format!(
        "---- codypendent-tui crash {} ----\n\
         message: {message}\n\
         location: {location}\n\
         backtrace:\n{backtrace}\n",
        humantime_now(),
    )
}

/// A dependency-free `now` stamp for the crash entry header. Not meant to be
/// parsed — only to let a human tell two entries in the same file apart — so
/// this deliberately avoids pulling in a time-formatting crate (no new
/// dependency) in favor of the `SystemTime` Unix-epoch offset `std` already
/// gives us.
fn humantime_now() -> String {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(since_epoch) => format!("at unix-epoch-seconds {}", since_epoch.as_secs()),
        Err(_) => "at an unknown time".to_string(),
    }
}

/// Append `entry` to the crash log at `path`, creating its parent directory
/// (e.g. `<data_dir>/logs/`) and the file itself if either is missing, in
/// append mode so an earlier crash's entry is never overwritten. Returns the
/// I/O error to the caller rather than swallowing it here — the panic-hook
/// closure (`install_crash_hook`) is the one place that must never propagate
/// a failure (a panic while handling a panic aborts the process outright), so
/// swallowing happens there, not in this otherwise-ordinary, testable writer.
fn write_crash_entry(path: &Path, entry: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(entry.as_bytes())
}

/// The panic hook's own body, split out from the `Box::new(move |info| ...)`
/// closure in `install_crash_hook` so it reads linearly: extract the message
/// (a panic payload is either a `&str` literal or a formatted `String`) and
/// the source location from `info`, format the entry, and write it —
/// best-effort. Any failure (a missing data dir the caller couldn't create, a
/// full disk, permissions) is swallowed: this runs INSIDE the panic hook, and
/// a panic here would abort the process instead of letting the original
/// panic's unwind (and the previous hook's default reporting) continue.
fn append_crash_log(path: &Path, info: &std::panic::PanicHookInfo<'_>, backtrace: &str) {
    let message = if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    };
    let location = info.location().map(|location| location.to_string());
    let entry = format_crash_entry(&message, location.as_deref(), backtrace);
    let _ = write_crash_entry(path, &entry);
}

/// Install the crash-logging panic hook. Called once, as the very first thing
/// [`run`] does — before ANY other client setup, and so well before
/// [`TerminalGuard::enter`] — so it is active for a panic anywhere in the
/// client's lifetime and, critically, fires before the guard's `Drop` blanks
/// the alternate screen back to the caller's cooked terminal (the teardown
/// that made the original crash's panic message unrecoverable).
///
/// The previous hook (Rust's default, unless something upstream already
/// replaced it) is captured via [`std::panic::take_hook`] and chained after
/// the crash-log write, so normal panic reporting (and any other behavior the
/// previous hook carried) is entirely unchanged — this only ever ADDS the
/// file write in front of it.
fn install_crash_hook(paths: &RuntimePaths) {
    let crash_log_path = paths.log_dir.join(CRASH_LOG_FILE_NAME);
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        append_crash_log(&crash_log_path, info, &backtrace.to_string());
        previous_hook(info);
    }));
}

/// `codypendent` with no subcommand: open the interactive TUI for `repo`.
///
/// Auto-starts the daemon, resolves (or creates) `repo`'s session, attaches with
/// catch-up, and runs the event loop until the user detaches (`q`) or the daemon
/// closes the stream. Detaching never affects the run — the daemon keeps it
/// going; a later `codypendent` reopens the same session and catches up.
///
/// `theme_override` is `--theme <NAME>` / `CODYPENDENT_THEME` (resolved by the
/// caller, flag winning over env — see `main.rs`): a built-in variant name or
/// a theme-pack id under `<data-dir>/themes/<id>.toml` (STEP 6.6, see
/// `theme_select`). Resolved before any daemon/socket work so a bad name fails
/// fast on a normal cooked terminal instead of after entering raw mode.
pub async fn run(
    paths: &RuntimePaths,
    repo: PathBuf,
    theme_override: Option<String>,
    accessible: bool,
) -> anyhow::Result<()> {
    // Crash logger, installed FIRST — before repo validation, the terminal
    // guard, and the daemon connection — so a panic anywhere in this
    // function's lifetime is diagnosable, and so it fires before the guard's
    // `Drop` tears down the alternate screen (see the module docs above
    // `install_crash_hook` for the full motivation).
    install_crash_hook(paths);

    let repo = repo
        .canonicalize()
        .with_context(|| format!("{}: not a valid, accessible directory", repo.display()))?;
    if !repo.is_dir() {
        bail!("{}: not a directory", repo.display());
    }

    if accessible {
        return run_accessible(paths, repo).await;
    }

    // STEP 6.6 wiring: terminal color-depth detection (NO_COLOR/COLORTERM/TERM)
    // with a manual override that always wins, replacing the old hardcoded
    // `Theme::dark()`.
    let theme = crate::theme_select::resolve_theme(paths, theme_override.as_deref())?;

    // D2: enter raw mode + the alternate screen EARLY — before any daemon
    // work — so the (possibly multi-second) boot draws a splash instead of
    // leaving the user staring at a blank cooked terminal. RAII restores the
    // terminal on any exit path, including a boot error (whose text then
    // prints to the restored cooked screen) or a panic mid-loop, and the
    // event loop later takes over the SAME terminal — no teardown/re-enter
    // between splash and TUI.
    let mut guard = TerminalGuard::enter().context(
        "the interactive TUI needs a terminal (a TTY); for headless use run \
         `codypendent run --jsonl` instead",
    )?;

    // D2: boot steps publish their stage over this channel; the splash loop
    // below draws the latest stage every SPLASH_TICK until boot completes.
    let (stage_tx, stage_rx) = watch::channel(SplashStage::StartingDaemon);
    // Boot diagnostics (reconcile warnings, loader failure notes) collect
    // BESIDE the stage channel: a `watch` retains only the LATEST value, so a
    // warning published as a stage would be overwritten by the next stage and
    // lost. The splash draws them under the stage line; after boot each one
    // becomes a TUI notice.
    let boot_warnings: BootWarnings = BootWarnings::default();

    let mut state = AppState::new();
    let mut store = SessionStore::load(paths);

    // Drive boot and the splash concurrently: poll the pinned boot future to
    // completion while redrawing the splash on each tick. Once boot is ready,
    // a separate welcome state remains visible until the user deliberately
    // presses Enter. The block scopes the pinned future (it borrows
    // `state`/`store`) so its drop runs before the event loop takes them back.
    let booted = {
        let boot = boot_phase(
            paths,
            &repo,
            &mut state,
            &mut store,
            &stage_tx,
            &boot_warnings,
        );
        tokio::pin!(boot);
        let mut splash_ticker =
            tokio::time::interval_at(tokio::time::Instant::now() + SPLASH_TICK, SPLASH_TICK);
        let mut splash_ticks: u64 = 0;
        loop {
            tokio::select! {
                outcome = &mut boot => break outcome?,
                _ = splash_ticker.tick() => {
                    splash_ticks += 1;
                    let stage = stage_rx.borrow().text();
                    let warnings = boot_warnings
                        .lock()
                        .expect("boot warnings mutex poisoned")
                        .clone();
                    guard
                        .terminal_mut()
                        .draw(|frame| render_splash(frame, splash_ticks, &stage, &warnings, false, &theme))?;
                }
            }
        }
    };

    let (input_tx, mut input_rx) = mpsc::channel::<ClientInput>(256);
    let input_running = Arc::new(AtomicBool::new(true));
    spawn_input_thread(input_tx, Arc::clone(&input_running));

    let workspace_name = repo
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace");
    let ready_stage = format!("{workspace_name} is ready");
    let warnings = boot_warnings
        .lock()
        .expect("boot warnings mutex poisoned")
        .clone();
    if !wait_for_splash_entry(&mut guard, &theme, &ready_stage, &warnings, &mut input_rx).await? {
        input_running.store(false, Ordering::Relaxed);
        return Ok(());
    }

    // Boot diagnostics become the TUI's own notices — ALWAYS when any were
    // collected, whether or not the splash drew a single frame: a reconcile
    // warning or a loader failure note is meaningful after boot regardless
    // (the pre-splash behavior was a persistent stderr print).
    drain_boot_warnings(&mut state, &boot_warnings);
    let Booted {
        session_id,
        workspace_id,
        attach_watermark,
        docs_pool,
        mut live,
    } = booted;

    let (mut width, _) = crossterm::terminal::size().unwrap_or((80, 24));

    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let repository = repo.to_string_lossy().into_owned();
    let result = {
        let mut presentation = InteractivePresentation {
            guard: &mut guard,
            theme: &theme,
        };
        event_loop(
            &mut presentation,
            &mut state,
            &mut width,
            &mut live,
            &mut input_rx,
            &mut ticker,
            session_id,
            workspace_id,
            &repository,
            attach_watermark,
            docs_pool.clone(),
            &mut store,
            paths,
        )
        .await
    };

    // Teardown: stop the input thread, restore the terminal *before* any trailing
    // error text reaches the (now cooked) screen, then wind down the socket tasks.
    input_running.store(false, Ordering::Relaxed);
    drop(guard);
    live.shutdown();
    if let Some(pool) = docs_pool {
        pool.close().await;
    }
    result
}

/// Run the full client over ordinary cooked stdin/stdout. Unlike the Ratatui
/// path this never constructs [`TerminalGuard`], so it cannot emit alternate
/// screen, raw-mode, mouse-capture, or bracketed-paste control sequences.
async fn run_accessible(paths: &RuntimePaths, repo: PathBuf) -> anyhow::Result<()> {
    let mut stdout = io::stdout();
    writeln!(stdout, "Codypendent accessible mode")?;
    writeln!(stdout, "Starting daemon and restoring the session.")?;
    stdout.flush()?;

    let mut state = AppState::new();
    let mut store = SessionStore::load(paths);
    let (stage_tx, mut stage_rx) = watch::channel(SplashStage::StartingDaemon);
    let boot_warnings: BootWarnings = BootWarnings::default();
    let booted = {
        let boot = boot_phase(
            paths,
            &repo,
            &mut state,
            &mut store,
            &stage_tx,
            &boot_warnings,
        );
        tokio::pin!(boot);
        loop {
            tokio::select! {
                outcome = &mut boot => break outcome?,
                changed = stage_rx.changed() => {
                    if changed.is_ok() {
                        writeln!(stdout, "Boot: {}", ascii_stage(&stage_rx.borrow().text()))?;
                        stdout.flush()?;
                    }
                }
            }
        }
    };
    drain_boot_warnings(&mut state, &boot_warnings);
    let Booted {
        session_id,
        workspace_id,
        attach_watermark,
        docs_pool,
        mut live,
    } = booted;

    let (input_tx, mut input_rx) = mpsc::channel::<ClientInput>(256);
    let input_task = spawn_accessible_input(input_tx);
    let (mut width, _) = accessible_viewport();
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let repository = repo.to_string_lossy().into_owned();
    let mut presentation = AccessiblePresentation::new(stdout);
    let result = event_loop(
        &mut presentation,
        &mut state,
        &mut width,
        &mut live,
        &mut input_rx,
        &mut ticker,
        session_id,
        workspace_id,
        &repository,
        attach_watermark,
        docs_pool.clone(),
        &mut store,
        paths,
    )
    .await;

    input_task.abort();
    live.shutdown();
    if let Some(pool) = docs_pool {
        pool.close().await;
    }
    result
}

fn ascii_stage(stage: &str) -> String {
    sanitize_accessible_text(stage)
        .replace('…', "...")
        .replace('·', "-")
}

fn accessible_viewport() -> (u16, u16) {
    let parse = |name: &str, fallback: u16| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(fallback)
    };
    (parse("COLUMNS", 80), parse("LINES", 24))
}

/// Input from either presentation. Keeping cooked lines distinct from
/// crossterm events means accessible mode never has to synthesize key events
/// (or initialize crossterm at all).
enum ClientInput {
    Terminal(CrosstermEvent),
    AccessibleLine(String),
}

/// The terminal-specific edge of the otherwise shared client loop.
trait Presentation {
    fn viewport(&self) -> (u16, u16);
    fn capabilities_message(&self) -> codypendent_protocol::UiWireMessage;
    fn draw(&mut self, state: &AppState, force_prompt: bool) -> io::Result<()>;
    fn wants_periodic_draw(&self) -> bool {
        false
    }
}

struct InteractivePresentation<'a> {
    guard: &'a mut TerminalGuard,
    theme: &'a Theme,
}

impl Presentation for InteractivePresentation<'_> {
    fn viewport(&self) -> (u16, u16) {
        crossterm::terminal::size().unwrap_or((80, 24))
    }

    fn capabilities_message(&self) -> codypendent_protocol::UiWireMessage {
        let (width, height) = self.viewport();
        let depth = match ColorDepth::detect() {
            ColorDepth::TrueColor => 24,
            ColorDepth::Ansi256 => 8,
            ColorDepth::Ansi16 => 4,
            ColorDepth::Monochrome => 1,
        };
        terminal_capabilities_message(width, height, depth)
    }

    fn draw(&mut self, state: &AppState, _force_prompt: bool) -> io::Result<()> {
        self.guard
            .terminal_mut()
            .draw(|frame| render(frame, state, self.theme))
            .map(|_| ())
    }
}

/// A stable, append-only cooked presentation. A state transition prints one
/// complete linear snapshot, so redirected output and screen readers receive
/// the same ordering and never need cursor-addressing escape sequences.
struct AccessiblePresentation<W: Write> {
    output: W,
    last_snapshot: Option<String>,
    last_emitted_at: Option<Instant>,
}

impl<W: Write> AccessiblePresentation<W> {
    fn new(output: W) -> Self {
        Self {
            output,
            last_snapshot: None,
            last_emitted_at: None,
        }
    }
}

impl<W: Write> Presentation for AccessiblePresentation<W> {
    fn viewport(&self) -> (u16, u16) {
        accessible_viewport()
    }

    fn capabilities_message(&self) -> codypendent_protocol::UiWireMessage {
        let (width, height) = self.viewport();
        accessible_terminal_capabilities_message(width, height)
    }

    fn draw(&mut self, state: &AppState, force_prompt: bool) -> io::Result<()> {
        let snapshot = accessible_snapshot(state);
        if self.last_snapshot.as_deref() == Some(snapshot.as_str()) {
            if force_prompt {
                writeln!(self.output)?;
                write!(self.output, "command> ")?;
                self.output.flush()?;
            }
            return Ok(());
        }
        if !force_prompt
            && self
                .last_emitted_at
                .is_some_and(|emitted| emitted.elapsed() < ACCESSIBLE_REFRESH)
        {
            return Ok(());
        }
        writeln!(self.output, "\n--- accessible update ---")?;
        writeln!(self.output, "{snapshot}")?;
        write!(self.output, "command> ")?;
        self.output.flush()?;
        self.last_snapshot = Some(snapshot);
        self.last_emitted_at = Some(Instant::now());
        Ok(())
    }

    fn wants_periodic_draw(&self) -> bool {
        true
    }
}

fn spawn_accessible_input(tx: mpsc::Sender<ClientInput>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(ClientInput::AccessibleLine(line)).await.is_err() {
                break;
            }
        }
    })
}

/// The boot stages the D2 splash narrates, published by [`boot_phase`] over a
/// `watch` channel; the splash loop in [`run`] draws the latest stage each
/// tick. `Reconciling` carries the build-mismatch warning text for the spinner
/// line — what used to be a cooked-mode `eprintln!` — so nothing prints to
/// stderr from inside the alternate screen. The warning text ALSO lands in
/// [`BootWarnings`]: a `watch` retains only the latest value, so the stage
/// alone would lose a warning the moment the next stage publishes.
#[derive(Clone)]
enum SplashStage {
    StartingDaemon,
    Connecting,
    Reconciling(String),
    RestoringSession,
    LoadingWorkspace,
}

impl SplashStage {
    fn text(&self) -> String {
        match self {
            Self::StartingDaemon => "starting daemon…".to_owned(),
            Self::Connecting => "connecting…".to_owned(),
            Self::Reconciling(message) => message.clone(),
            Self::RestoringSession => "restoring session…".to_owned(),
            Self::LoadingWorkspace => "loading workspace…".to_owned(),
        }
    }
}

/// Boot diagnostics (D2) collected BESIDE the [`SplashStage`] watch channel:
/// a `watch` retains only the LATEST value, so a reconcile warning sent as
/// `SplashStage::Reconciling` is overwritten by the very next stage
/// (`RestoringSession`/`LoadingWorkspace`) almost immediately — and a boot too
/// fast to draw a single frame never shows it at all. This shared vec keeps
/// every diagnostic (reconcile warnings from `warn_stage`, the projection
/// loaders' best-effort failure notes) so the splash can draw them under the
/// stage line and [`run`] can surface each as a post-boot `Action::Notice` —
/// what used to be a cooked-mode `eprintln!`, which the early alternate
/// screen would swallow.
type BootWarnings = Arc<Mutex<Vec<String>>>;

/// Push one boot diagnostic onto the shared [`BootWarnings`] vec.
fn push_boot_warning(warnings: &BootWarnings, message: String) {
    warnings
        .lock()
        .expect("boot warnings mutex poisoned")
        .push(message);
}

/// Drain the collected boot diagnostics into post-boot TUI notices (D2):
/// ALWAYS when any were collected, whether or not the splash drew a frame —
/// a reconcile warning or a loader failure note is meaningful after boot
/// regardless (the pre-splash behavior was a persistent stderr print). One
/// persistent issue per diagnostic.
fn drain_boot_warnings(state: &mut AppState, warnings: &BootWarnings) {
    let collected = std::mem::take(&mut *warnings.lock().expect("boot warnings mutex poisoned"));
    for warning in collected {
        reduce(state, Action::Issue(warning));
    }
}

/// Everything [`boot_phase`] produces that the event loop and teardown still
/// need, bundled so [`run`]'s splash loop stays small.
struct Booted {
    session_id: SessionId,
    workspace_id: WorkspaceId,
    attach_watermark: u64,
    docs_pool: Option<sqlx::SqlitePool>,
    live: LiveIo,
}

/// The socket tasks and channels for the session currently shown by the TUI.
/// Keeping them together makes an in-place conversation switch an atomic
/// transport handoff: the old connection (and all of its old-session
/// forwarders) is dropped as one unit after the replacement is attached.
struct LiveIo {
    client_id: ClientId,
    out_tx: mpsc::Sender<Envelope>,
    event_rx: mpsc::Receiver<ReaderSignal>,
    query_tx: mpsc::Sender<ReaderSignal>,
    reader: tokio::task::JoinHandle<()>,
    writer: tokio::task::JoinHandle<()>,
}

impl LiveIo {
    fn start(conn: Connection) -> (Self, std::collections::VecDeque<Envelope>) {
        let (read_half, write_half, pending, client_id) = conn.into_split();
        let (out_tx, out_rx) = mpsc::channel::<Envelope>(256);
        let (event_tx, event_rx) = mpsc::channel::<ReaderSignal>(256);
        let query_tx = event_tx.clone();
        let reader = tokio::spawn(read_loop(read_half, event_tx, out_tx.clone(), client_id));
        let writer = tokio::spawn(write_loop(write_half, out_rx));
        (
            Self {
                client_id,
                out_tx,
                event_rx,
                query_tx,
                reader,
                writer,
            },
            pending,
        )
    }

    fn shutdown(self) {
        drop(self.out_tx);
        self.reader.abort();
        self.writer.abort();
    }
}

/// Every boot step before the event loop (D2): daemon ensure, handshake,
/// build-mismatch reconcile, session resolve, catch-up fold, projection
/// seeding, and the socket reader/writer tasks. Runs as a pinned future
/// inside [`run`]'s splash loop and publishes each stage over `stage_tx`;
/// `state` and `store` are seeded in place so nothing needs moving back out.
/// By the time this runs the terminal is already in the alternate screen, so
/// diagnostics go to the stage channel (spinner line) and the shared
/// `boot_warnings` vec (splash warning lines + post-boot notices) — never to
/// stderr.
async fn boot_phase(
    paths: &RuntimePaths,
    repo: &Path,
    state: &mut AppState,
    store: &mut SessionStore,
    stage_tx: &watch::Sender<SplashStage>,
    boot_warnings: &BootWarnings,
) -> anyhow::Result<Booted> {
    commands::ensure_daemon(paths).await?;
    let _ = stage_tx.send(SplashStage::Connecting);
    let mut conn = Connection::connect(&paths.socket_path).await?;
    let resume = store
        .resume_token
        .clone()
        .map(codypendent_protocol::ResumeToken);
    // Advertise the client's own per-build id (DR1/T4/T9) so both halves of
    // the handshake speak the same id vocabulary — the daemon-auto-restart
    // feature's detection point is the `hello.build_id` this returns.
    let hello = conn
        .handshake("codypendent-tui", codypendent_protocol::BUILD_ID, resume)
        .await?;
    // Store the daemon-issued token so the NEXT launch resumes this client
    // identity (best-effort; an absent token just means a fresh identity).
    if let Some(token) = hello.resume_token {
        store.resume_token = Some(token.0);
        store.save(paths);
    }

    // DR5: detect + (safely) reconcile a daemon build mismatch, right after
    // the handshake and BEFORE resolving/creating a session — the detection
    // point the design doc calls for. On a matching build (the overwhelming
    // common case) this is a single string compare and zero extra round
    // trips; `conn` is left untouched. On a genuine mismatch it may stop the
    // OLD daemon, spawn a fresh one, and reconnect — but ONLY when confirmed
    // idle (never while a run is active, never on uncertainty); otherwise it
    // warns and continues on the existing daemon. A restart that fails, or a
    // reconnect that still mismatches, is a legible hard error — this
    // function never enters the TUI against a broken or half-restarted
    // daemon.
    let resume_for_reconnect = store
        .resume_token
        .clone()
        .map(codypendent_protocol::ResumeToken);
    let mut restart_ops =
        crate::restart::LiveRestartOps::new(paths, conn, "codypendent-tui", resume_for_reconnect);
    // D2: these warnings used to be cooked-mode `eprintln!`s; the terminal is
    // in the alternate screen by now, so they become splash stage text (the
    // spinner line) AND persist in `boot_warnings` — the `watch` stage channel
    // alone would drop each one the moment the next stage publishes.
    let mut warn_stage = |message: &str| {
        push_boot_warning(boot_warnings, message.to_owned());
        let _ = stage_tx.send(SplashStage::Reconciling(message.to_owned()));
    };
    let reconcile_outcome = crate::restart::reconcile_interactive(
        paths,
        codypendent_protocol::BUILD_ID,
        &hello.build_id,
        &mut restart_ops,
        &mut warn_stage,
    )
    .await?;
    // D3: the header shows the running daemon's build id — the handshaken
    // one, unless the reconcile just restarted the daemon onto THIS build
    // (`reconnect_and_assert` guarantees the new daemon matches the client).
    state.daemon_build_id = Some(match reconcile_outcome {
        crate::restart::ReconcileOutcome::Restarted => codypendent_protocol::BUILD_ID.to_owned(),
        _ => hello.build_id.clone(),
    });
    let mut conn = restart_ops.into_connection();

    let _ = stage_tx.send(SplashStage::RestoringSession);
    let (session_id, workspace_id, catchup) =
        resolve_or_create_session(&mut conn, store, paths, repo).await?;

    // Seed the state from catch-up, then from any live event that outraced the
    // attach reply and was buffered during setup — both before the loop reads a
    // single new frame, so no event is dropped or reordered.
    let mut attach_watermark = fold_catchup(state, catchup);
    let (live, pending) = LiveIo::start(conn);
    for envelope in pending {
        if let Payload::Event(event) = envelope.payload {
            attach_watermark = attach_watermark.max(event.sequence);
            reduce(state, Action::DaemonEvent(Box::new(event)));
        }
    }

    // STEP 2.6 + Phase 4 client wiring: seed the Skill Studio, memory browser,
    // Docs Studio, and code-graph edge-inspector projections. This reads the
    // knowledge fabric's authoritative rows directly from SQLite (WAL allows
    // concurrent reads alongside the daemon) and maps them into the TUI's plain
    // projection structs — the one place the two worlds meet, done here (not in
    // the pure TUI crate, which never depends on `codypendent-knowledge`). A read
    // failure collects a diagnostic (surfaced on the splash and as a post-boot
    // notice) and continues with empty lists; it never fails the TUI.
    let _ = stage_tx.send(SplashStage::LoadingWorkspace);
    let mut loader_warnings = Vec::new();
    let projections = load_knowledge(paths, workspace_id, repo, &mut loader_warnings).await;
    state.skills = projections.skills;
    state.memories = projections.memories;
    state.docs = projections.docs;
    state.edges = projections.edges;
    state.edge_total = projections.edge_total;
    state.blackboard = projections.blackboard;
    // MP1: seed the model-picker projection (models.toml + any measured
    // profile from `model_profiles`), exactly like the projections above.
    state.models = load_model_cards(paths, &mut loader_warnings).await;
    // Task 8: seed the provider-catalog picker projection (the built-in
    // catalog + any user `providers.toml`), exactly like `load_model_cards`.
    state.providers = load_provider_cards(paths, &mut loader_warnings).await;
    // D1 (/keys): seed the API-key status projection — auth.json entries +
    // models.toml `api_key_env` declarations (the tui crate does no I/O, so
    // the harness reads the files and folds the projection as an Action, the
    // same Action re-fired after every key write and daemon restart).
    {
        let (models, tavily) = load_key_statuses(paths, &mut loader_warnings);
        reduce(state, Action::ApiKeyStatusesLoaded { models, tavily });
    }
    // Phase 5 STEP 5.2 + T8: seed the workflow-graph view by compiling the
    // repository's declared workflow manifests, then overlay each workflow's
    // LATEST durable run — its per-node live state, measured cost, and
    // failure/block reason — from the knowledge db (WAL allows a concurrent read
    // alongside the daemon). A malformed manifest collects a diagnostic and is
    // skipped; a db-open failure degrades to the compiled (pre-run) view —
    // neither fails the TUI.
    {
        let overlay_pool = knowledge_db::open(&paths.data_dir.join("codypendent.db"))
            .await
            .ok();
        let user_workflows = paths.data_dir.join("workflows");
        state.workflow = load_workflows(
            repo,
            Some(&user_workflows),
            overlay_pool.as_ref(),
            &mut loader_warnings,
        )
        .await;
    }
    // Publish the loaders' collected diagnostics alongside the reconcile
    // warnings so the splash can draw them and `run` notices them post-boot.
    boot_warnings
        .lock()
        .expect("boot warnings mutex poisoned")
        .append(&mut loader_warnings);

    // A persistent read pool for live document editing (Phase 4 STEP 4.3): the
    // event loop seeds a document's client replica from it and re-reads the
    // review rail's suggestions when a sync arrives. WAL mode lets this read
    // concurrently with the daemon. `None` on failure — document editing then
    // degrades to converging from live syncs alone (no seed, empty review rail).
    let docs_pool = knowledge_db::open(&paths.data_dir.join("codypendent.db"))
        .await
        .ok();

    Ok(Booted {
        session_id,
        workspace_id,
        attach_watermark,
        docs_pool,
        live,
    })
}

/// What [`GapTracker::on_event`] asks the harness to do with one live event.
#[derive(Debug)]
enum GapAction {
    /// Nothing to fold — the event was a duplicate, a sentinel already handled,
    /// or it was buffered pending an in-flight repair.
    Ignore,
    /// Fold this event into state now; the watermark has already advanced.
    /// Boxed to keep the action small (a `SessionEvent` is a large payload).
    Apply(Box<SessionEvent>),
    /// Re-attach with `last_seen_sequence` to replay a missed span (a detected
    /// gap, a buffer overflow, or — via [`GapTracker::on_tick`] — a repair
    /// timeout). Any events to fold afterwards are held inside the tracker.
    Reattach { last_seen_sequence: u64 },
}

/// The result of feeding a gap-repair catch-up reply to
/// [`GapTracker::on_catchup`].
struct CatchupDrain {
    /// Buffered events to fold now, in ascending sequence order, deduped
    /// against the watermark the catch-up advanced.
    apply: Vec<SessionEvent>,
    /// A follow-up re-attach `last_seen_sequence`, set when the buffered tail
    /// still revealed a missing span (more loss occurred while repairing).
    reattach: Option<u64>,
}

/// The reconnect / gap-repair state machine for the TUI's live event fold.
///
/// This is the code that keeps a lagged client from losing an event the daemon
/// dropped from its live fan-out (worst case, an `ApprovalRequested`). It is
/// deliberately **pure** — it owns no socket and no [`AppState`], only the
/// sequence bookkeeping — so the harness can drive it with I/O while the tests
/// drive it directly. The harness feeds it every live event, every gap-repair
/// catch-up reply, and every timer tick, and performs the [`GapAction`] /
/// [`CatchupDrain`] it returns.
///
/// # Sequence numbering
///
/// The daemon assigns ledger sequences 1-based (`COALESCE(MAX(sequence),0)+1`),
/// and even ephemeral events (presence) are appended before fan-out, so every
/// live event on the wire carries a sequence `>= 1`. Sequence `0` is therefore
/// a **sentinel** ("no ledger position"), never a real event: it cannot be a
/// duplicate and cannot open or fill a gap, so it is folded straight through in
/// any state (FP-2c — previously a sentinel arriving mid-repair was buffered and
/// then silently discarded).
struct GapTracker {
    /// The highest real ledger sequence folded so far (the catch-up + live
    /// dedup watermark). `0` means "no baseline yet" — the first live event
    /// seeds it without gap detection.
    last_seen: u64,
    /// Events held back while a repair is in flight. They must NOT fold (or
    /// advance `last_seen`) before the replay lands: advancing the watermark to
    /// the gap-revealing event first made every replayed event read as a
    /// duplicate and silently discarded the whole repair (the original C6 bug).
    gap_buffer: Vec<SessionEvent>,
    /// Whether a gap repair is in flight (awaiting a catch-up reply).
    repairing: bool,
    /// When the in-flight repair should be abandoned for a fresh re-attach
    /// (FP-2b). `None` when not repairing.
    repair_deadline: Option<Instant>,
}

impl GapTracker {
    /// Start tracking from the attach-time watermark (the catch-up's `through`).
    fn new(attach_watermark: u64) -> Self {
        Self {
            last_seen: attach_watermark,
            gap_buffer: Vec::new(),
            repairing: false,
            repair_deadline: None,
        }
    }

    /// The current dedup/catch-up watermark. A proactive re-attach (e.g. adding a
    /// Document subscription on the first edit, Phase 4 STEP 4.3) carries this so
    /// the daemon replays only what this client has not already folded.
    fn last_seen(&self) -> u64 {
        self.last_seen
    }

    /// Feed one live event. `now` anchors the repair deadline when a repair
    /// starts. Returns what the harness should do with it.
    fn on_event(&mut self, event: SessionEvent, now: Instant) -> GapAction {
        // Sentinel (see the type docs): no position to dedup or order by, so
        // fold it straight through, never buffered/discarded, even mid-repair.
        if event.sequence == 0 {
            return GapAction::Apply(Box::new(event));
        }
        // A duplicate of something already folded (catch-up + live overlap).
        if event.sequence <= self.last_seen {
            return GapAction::Ignore;
        }
        if self.repairing {
            // FP-2a: bound the buffer. On overflow, drop the incremental replay
            // and re-attach from `last_seen` — the ledger re-delivers the whole
            // span, so this loses nothing and can never grow without bound.
            if self.gap_buffer.len() >= MAX_GAP_BUFFER {
                self.gap_buffer.clear();
                self.repair_deadline = Some(now + REPAIR_TIMEOUT);
                return GapAction::Reattach {
                    last_seen_sequence: self.last_seen,
                };
            }
            // Hold ordering: nothing folds past the missing span until the
            // replay has landed.
            self.gap_buffer.push(event);
            return GapAction::Ignore;
        }
        if self.last_seen != 0 && event.sequence > self.last_seen + 1 {
            // Gap: re-attach to replay the missed span. Buffer this event
            // instead of folding it now — it is out of order until the span
            // before it has been replayed. Crucially, `last_seen` is NOT
            // advanced to this event, so the re-attach replays from the true
            // watermark (reverting that is the C6 regression the tests pin).
            self.repairing = true;
            self.repair_deadline = Some(now + REPAIR_TIMEOUT);
            let last_seen_sequence = self.last_seen;
            self.gap_buffer.push(event);
            return GapAction::Reattach { last_seen_sequence };
        }
        // In order (or the first event past a 0 baseline): fold it and advance.
        self.last_seen = self.last_seen.max(event.sequence);
        GapAction::Apply(Box::new(event))
    }

    /// Feed a gap-repair catch-up reply's `through` watermark (the harness has
    /// already folded the catch-up's own events into state). Advances the
    /// watermark, ends the repair, and drains the buffered events in order,
    /// asking for another repair if the buffered tail still reveals a gap.
    fn on_catchup(&mut self, through: u64, now: Instant) -> CatchupDrain {
        self.last_seen = self.last_seen.max(through);
        self.repairing = false;
        self.repair_deadline = None;

        let mut buffered = std::mem::take(&mut self.gap_buffer);
        buffered.sort_by_key(|event| event.sequence);

        let mut apply = Vec::new();
        for (index, event) in buffered.iter().enumerate() {
            if event.sequence <= self.last_seen {
                continue; // already folded (via the catch-up or an earlier event)
            }
            if event.sequence > self.last_seen + 1 {
                // Still a hole: repair again, keeping this event and the tail.
                self.repairing = true;
                self.repair_deadline = Some(now + REPAIR_TIMEOUT);
                self.gap_buffer = buffered[index..].to_vec();
                return CatchupDrain {
                    apply,
                    reattach: Some(self.last_seen),
                };
            }
            self.last_seen = event.sequence;
            apply.push(event.clone());
        }
        CatchupDrain {
            apply,
            reattach: None,
        }
    }

    /// Feed a timer tick. Returns a re-attach `last_seen_sequence` when an
    /// in-flight repair has outlived [`REPAIR_TIMEOUT`] (FP-2b): drop the stale
    /// buffer and re-drive the catch-up from the watermark rather than wedging
    /// the client in `repairing` forever.
    fn on_tick(&mut self, now: Instant) -> Option<u64> {
        if self.repairing {
            if let Some(deadline) = self.repair_deadline {
                if now >= deadline {
                    self.gap_buffer.clear();
                    self.repair_deadline = Some(now + REPAIR_TIMEOUT);
                    return Some(self.last_seen);
                }
            }
        }
        None
    }
}

/// Send an `AttachSession` re-attach carrying `last_seen_sequence`, so the
/// daemon replaces this connection's forwarder and replies with a `Catchup`
/// windowed to the missed span. Best-effort: a closed writer just means the
/// connection is going down and the loop will exit on its own.
async fn send_reattach(
    out_tx: &mpsc::Sender<Envelope>,
    client_id: ClientId,
    session_id: SessionId,
    last_seen_sequence: u64,
    subscriptions: &[Subscription],
) {
    let attach = command_envelope(
        client_id,
        CommandBody::AttachSession {
            session_id,
            last_seen_sequence: Some(last_seen_sequence),
            // The caller's live (possibly grown) subscription set, so a gap-repair
            // re-attach preserves Document subscriptions added while editing
            // (Phase 4 STEP 4.3) rather than resetting to the session defaults.
            subscriptions: subscriptions.to_vec(),
            requested_role: ClientRole::Controller,
            // A gap-repair re-attach to an already-open session: the original
            // create/attach already carried the repo root (see
            // `resolve_or_create_session`), so there is nothing new to warm here.
            repository: None,
        },
    );
    let _ = out_tx.send(attach).await;
}

/// The render/reduce/dispatch loop. Broken out from [`run`] so the setup and
/// teardown read linearly and the borrow of every loop input is explicit.
#[allow(clippy::too_many_arguments)]
async fn event_loop<P: Presentation>(
    presentation: &mut P,
    state: &mut AppState,
    width: &mut u16,
    live: &mut LiveIo,
    input_rx: &mut mpsc::Receiver<ClientInput>,
    ticker: &mut tokio::time::Interval,
    mut session_id: SessionId,
    workspace_id: WorkspaceId,
    repository: &str,
    attach_watermark: u64,
    docs_pool: Option<sqlx::SqlitePool>,
    // Persist the replacement session after an in-place "New Conversation"
    // transport swap so the next launch resumes the newly selected thread.
    store: &mut SessionStore,
    paths: &RuntimePaths,
) -> anyhow::Result<()> {
    let capabilities = presentation.capabilities_message();
    if live
        .out_tx
        .send(remote_ui_envelope(live.client_id, session_id, capabilities))
        .await
        .is_err()
    {
        return Ok(());
    }
    presentation.draw(state, false)?;

    // Tracks live-fan-out sequence continuity and drives gap repair. Live
    // fan-out is lossy for a slow client (the daemon skips `Lagged` spans), so a
    // jump past `last_seen + 1` means events were dropped from the live view and
    // a re-attach with `last_seen_sequence` replays exactly the gap. The
    // decision logic is extracted into [`GapTracker`] — a pure unit owning no
    // I/O and no `AppState` — so the reconnect/repair path (the code protecting
    // an `ApprovalRequested` from being lost under lag) is deterministically
    // testable; this loop performs the I/O the tracker asks for.
    let mut tracker = GapTracker::new(attach_watermark);

    // The client's live subscription set: seeded with the session views and grown
    // with a `Document` subscription the first time an edit targets one, so a
    // gap-repair re-attach preserves the documents this client is editing (Phase 4
    // STEP 4.3).
    let mut subscriptions = default_subscriptions();
    // Per-open-document client replicas that consume the sync stream. Presence in
    // the map also marks the document as already subscribed, so an edit
    // subscribes + seeds it exactly once.
    let mut replicas: HashMap<DocumentId, DocumentReplica> = HashMap::new();
    // Correlate empty blackboard baselines back to the run they were requested
    // for (the reply carries only command_id when `items` is empty).
    let mut blackboard_reads: HashMap<CommandId, String> = HashMap::new();

    loop {
        // A CRDT sync needs an async merge (+ a suggestion re-read) that cannot run
        // inside the `select!` arm, so the arm stashes it here and the loop body
        // folds it just after.
        let mut pending_sync: Option<Box<DocumentSync>> = None;
        let mut started_workflow: Option<String> = None;
        let mut connected_acp: Option<(String, String, Result<String, String>)> = None;
        let selected = tokio::select! {
            signal = live.event_rx.recv() => PendingActions::One(match signal {
                Some(ReaderSignal::Event(event)) => {
                    match tracker.on_event(*event, Instant::now()) {
                        GapAction::Ignore => Action::NoOp,
                        GapAction::Apply(event) => Action::DaemonEvent(event),
                        GapAction::Reattach { last_seen_sequence } => {
                            // Re-attach with the *grown* subscription set (Phase 4
                            // STEP 4.3) so a gap-repair preserves the Document
                            // subscriptions this client added while editing.
                            send_reattach(
                                &live.out_tx,
                                live.client_id,
                                session_id,
                                last_seen_sequence,
                                &subscriptions,
                            )
                            .await;
                            Action::NoOp
                        }
                    }
                }
                Some(ReaderSignal::Rejected { code, message }) => {
                    Action::Notice(format!("command rejected: {message} ({code})"))
                }
                Some(ReaderSignal::RemoteUi(message)) => Action::RemoteUiMessage(message),
                Some(ReaderSignal::UiPlugins(plugins)) => Action::UiPluginsLoaded(plugins),
                // Phase 4 STEP 4.3 live document editing. A sync is merged after the
                // select (it needs an async replica merge + suggestion re-read); the
                // lease replies fold directly.
                Some(ReaderSignal::DocumentSync(sync)) => {
                    pending_sync = Some(sync);
                    Action::NoOp
                }
                Some(ReaderSignal::DocumentLeaseGranted {
                    document_id,
                    lease_id,
                }) => Action::DocumentLeaseGranted {
                    document_id,
                    lease_id,
                },
                Some(ReaderSignal::DocumentLeaseBlocked) => Action::DocumentLeaseBlocked,
                Some(ReaderSignal::DocumentPublishPrepared {
                    target,
                    changed_files,
                    git_action,
                }) => Action::Notice(format!(
                    "publish plan ready: {target} · {changed_files} file(s) · {git_action}"
                )),
                Some(ReaderSignal::ProviderModels { provider_id, result }) => match result {
                    Ok((models, origin)) => Action::ProviderModelsLoaded { provider_id, models, origin },
                    Err(reason) => Action::ProviderModelsFailed { provider_id, reason },
                },
                Some(ReaderSignal::ModelKeyVerified { model_id, result }) => match result {
                    Ok(()) => Action::ModelKeyVerified { model_id, ok: true, reason: String::new() },
                    Err(reason) => Action::ModelKeyVerified { model_id, ok: false, reason },
                },
                Some(ReaderSignal::AcpConnected { display_id, provider_id, result }) => {
                    connected_acp = Some((display_id, provider_id, result));
                    Action::NoOp
                }
                Some(ReaderSignal::WorkflowRunStarted { workflow_run_id }) => {
                    started_workflow = Some(workflow_run_id);
                    Action::Notice("workflow started — attaching live view".to_owned())
                }
                Some(ReaderSignal::WorkflowSnapshot(snapshot)) => {
                    workflow_snapshot_action(*snapshot)
                }
                Some(ReaderSignal::WorkflowEvent(event)) => workflow_event_action(event),
                Some(ReaderSignal::BlackboardItems { command_id, items }) => {
                    let workflow_run_id = blackboard_reads
                        .remove(&command_id)
                        .or_else(|| items.first().map(|item| item.workflow_run_id.clone()));
                    match workflow_run_id {
                        Some(workflow_run_id) => Action::BlackboardLoaded {
                            items: wire_blackboard_cards(state, &items),
                            workflow_run_id,
                        },
                        None => Action::NoOp,
                    }
                }
                Some(ReaderSignal::BlackboardPosted(item)) => {
                    let label = workflow_label_for_run(state, &item.workflow_run_id);
                    Action::BlackboardItemUpdated(wire_blackboard_item_card(label, &item))
                }
                Some(ReaderSignal::Catchup(catchup)) => {
                    // Fold the gap replay (the daemon already windowed it to
                    // `(last_seen, through]`, and a too-large gap arrives as a
                    // snapshot), then drain the events buffered while the repair
                    // was in flight — watermark-deduped, in order. If the buffer
                    // itself still reveals a missing span (more loss while
                    // repairing), the tracker asks to repair again and keeps the
                    // tail.
                    let through = fold_catchup(state, *catchup);
                    let drain = tracker.on_catchup(through, Instant::now());
                    for event in drain.apply {
                        reduce(state, Action::DaemonEvent(Box::new(event)));
                    }
                    if let Some(last_seen_sequence) = drain.reattach {
                        // Same grown subscription set on a repair-during-repair
                        // re-attach (Phase 4 STEP 4.3).
                        send_reattach(
                            &live.out_tx,
                            live.client_id,
                            session_id,
                            last_seen_sequence,
                            &subscriptions,
                        )
                        .await;
                    }
                    Action::NoOp
                }
                // The daemon closed the stream (shutdown / dropped client). The
                // run is unaffected; we simply leave the TUI.
                Some(ReaderSignal::Closed) | None => return Ok(()),
            }),
            input = input_rx.recv() => match input {
                // Track width for mouse-column → pane mapping; the draw below
                // re-reads the real size, so a resize just needs a redraw.
                Some(ClientInput::Terminal(CrosstermEvent::Resize(w, h))) => {
                    *width = w;
                    PendingActions::One(Action::RemoteUiViewport { width: w, height: h })
                }
                Some(ClientInput::Terminal(event)) => PendingActions::One(map_event(
                    &event,
                    state.input_mode(),
                    *width,
                    &state.hit_map.borrow(),
                )),
                Some(ClientInput::AccessibleLine(line)) => {
                    PendingActions::Many(map_accessible_input(&line, state.input_mode()))
                }
                None => return Ok(()), // input bridge ended
            },
            _ = ticker.tick() => PendingActions::One({
                // FP-2b: a repair whose catch-up reply never arrived (the
                // daemon's fan-out drops spans under lag) must not wedge the
                // client in `repairing` forever — once the deadline passes,
                // re-attach afresh to re-drive the catch-up.
                if let Some(last_seen_sequence) = tracker.on_tick(Instant::now()) {
                    send_reattach(
                        &live.out_tx,
                        live.client_id,
                        session_id,
                        last_seen_sequence,
                        &subscriptions,
                    )
                    .await;
                }
                Action::Tick
            })
        };
        let force_prompt = matches!(&selected, PendingActions::Many(_));
        let actions = match selected {
            PendingActions::One(action) => vec![action],
            PendingActions::Many(actions) => actions,
        };
        let tick_action = actions.len() == 1 && matches!(&actions[0], Action::Tick);
        let notice_before = state.notice.clone();

        // Fold a merged document sync (its async merge could not run in the arm).
        if let Some(sync) = pending_sync.take() {
            if let Some(synced) =
                merge_document_sync(&mut replicas, docs_pool.as_ref(), *sync).await
            {
                reduce(state, synced);
            }
        }

        for action in actions {
            reduce(state, action);
        }
        if let Some((display_id, provider_id, result)) = connected_acp.take() {
            match result {
                Ok(coordinate) => {
                    match write_add_model(paths, &display_id, &provider_id, &coordinate, None, None)
                    {
                        Ok(()) => {
                            let mut warnings = Vec::new();
                            state.models = load_model_cards(paths, &mut warnings).await;
                            for warning in warnings {
                                reduce(state, Action::Issue(warning));
                            }
                            reload_key_statuses(state, paths);
                            reduce(state, Action::Notice(format!("connected {display_id}")));
                        }
                        Err(error) => reduce(
                            state,
                            Action::Notice(format!("could not save ACP profile: {error}")),
                        ),
                    }
                }
                Err(error) => reduce(
                    state,
                    Action::Notice(format!("could not connect ACP agent: {error}")),
                ),
            }
        }
        // The steady shell has no frame-based animation. Wakeups still drive
        // repair and reducer time, but only notice expiry and the periodic
        // projection refresh need a new frame; input and daemon events redraw
        // immediately through the non-tick path.
        let redraw = !tick_action
            || state.notice != notice_before
            || state.tick.is_multiple_of(25)
            || presentation.wants_periodic_draw();

        // `StartWorkflow` replies with the new run id after the durable rows are
        // committed. Reload once so the manifest cards bind to that exact run,
        // then queue the same watch path used when an existing run is focused.
        if let Some(workflow_run_id) = started_workflow.take() {
            let mut warnings = Vec::new();
            let user_workflows = paths.data_dir.join("workflows");
            state.workflow = load_workflows(
                Path::new(repository),
                Some(&user_workflows),
                docs_pool.as_ref(),
                &mut warnings,
            )
            .await;
            for warning in warnings {
                reduce(state, Action::Issue(warning));
            }
            state.outbox.push(Intent::WatchWorkflow { workflow_run_id });
        }

        for intent in state.drain_outbox() {
            if let Intent::RemoteUiMessage(message) = intent {
                if live
                    .out_tx
                    .send(remote_ui_envelope(live.client_id, session_id, *message))
                    .await
                    .is_err()
                {
                    return Ok(());
                }
                continue;
            }
            // `AddModel` is the one client-only intent: apply it locally (models.toml
            // + auth.json) and skip the daemon-command mapping entirely.
            if let Intent::AddModel {
                display_id,
                provider_id,
                model,
                api_key,
                context_tokens,
            } = &intent
            {
                let acp =
                    codypendent_integrations::acp_registry::AcpRegistryStore::new(&paths.data_dir)
                        .load_cached()
                        .ok()
                        .is_some_and(|registry| registry.get(provider_id).is_some());
                if acp {
                    let paths = paths.clone();
                    let repository = PathBuf::from(repository);
                    let display_id = display_id.clone();
                    let provider_id = provider_id.clone();
                    let tx = live.query_tx.clone();
                    tokio::spawn(async move {
                        let result = connect_acp_agent(&paths, &provider_id, &repository)
                            .await
                            .map_err(|error| error.to_string());
                        let _ = tx
                            .send(ReaderSignal::AcpConnected {
                                display_id,
                                provider_id,
                                result,
                            })
                            .await;
                    });
                    continue;
                }
                apply_add_model(
                    state,
                    paths,
                    display_id,
                    provider_id,
                    model,
                    api_key.as_ref().map(|k| k.0.as_str()),
                    *context_tokens,
                )
                .await;
                continue;
            }
            // `QueryProviderModels` is the other client-only intent (model
            // discovery). Three steps, none of them on the UI thread beyond a
            // cache read: seed the pick-list from `<data_dir>/model_lists/`
            // when a previous fetch left one there (instant), then spawn the
            // `<base_url>/models` GET and feed its merged result back as
            // `ReaderSignal::ProviderModels`. Never a daemon command. The
            // spawned task owns the key for the request and drops it — it is
            // never sent back.
            if let Intent::QueryProviderModels {
                provider_id,
                api_key,
                refresh,
            } = &intent
            {
                use codypendent_providers::{AuthMethod, Catalog};
                let catalog =
                    Catalog::load_with_user_overrides(&paths.data_dir.join("providers.toml"))
                        .unwrap_or_else(|_| Catalog::builtin());
                let curated = catalog_rows_for(&catalog, provider_id);
                let (base_url, header, prefix, listable) = match catalog.get(provider_id) {
                    Some(provider) => {
                        let base = provider.base_url.clone().unwrap_or_default();
                        let (header, prefix) = match provider.auth.first() {
                            Some(AuthMethod::ApiKey { header, prefix, .. }) => {
                                (header.clone(), prefix.clone())
                            }
                            _ => ("Authorization".to_string(), "Bearer ".to_string()),
                        };
                        (base, header, prefix, provider_can_list_models(provider))
                    }
                    None => (
                        String::new(),
                        "Authorization".to_string(),
                        "Bearer ".to_string(),
                        false,
                    ),
                };
                // Instant seed: the cached listing, catalog metadata merged in.
                if !*refresh {
                    if let Some((cached, age)) = read_model_list_cache(&paths.data_dir, provider_id)
                    {
                        reduce(
                            state,
                            Action::ProviderModelsLoaded {
                                provider_id: provider_id.clone(),
                                models: merge_catalog_rows(cached, &curated),
                                origin: ModelListOrigin::Cached(age),
                            },
                        );
                    }
                }
                // A provider with no listing endpoint (Perplexity) never
                // reaches the network at all: its curated rows ARE the answer.
                if !listable {
                    reduce(
                        state,
                        if curated.is_empty() {
                            Action::ProviderModelsFailed {
                                provider_id: provider_id.clone(),
                                reason: "this provider has no model-list endpoint".to_owned(),
                            }
                        } else {
                            Action::ProviderModelsLoaded {
                                provider_id: provider_id.clone(),
                                models: curated,
                                origin: ModelListOrigin::Catalog("no listing endpoint".to_owned()),
                            }
                        },
                    );
                    continue;
                }
                let provider_id = provider_id.clone();
                // No key in hand: fall back to the provider-wide key a previous
                // add already stored, so the same key is never asked for twice.
                let key = api_key.as_ref().map(|k| k.0.clone()).or_else(|| {
                    stored_provider_key(&paths.data_dir, &provider_id).filter(|k| !k.is_empty())
                });
                let tx = live.query_tx.clone();
                let data_dir = paths.data_dir.clone();
                tokio::spawn(async move {
                    let result =
                        match query_provider_models(&base_url, &header, &prefix, key.as_deref())
                            .await
                        {
                            Ok(live_rows) => {
                                write_model_list_cache(&data_dir, &provider_id, &live_rows);
                                Ok((
                                    merge_catalog_rows(live_rows, &curated),
                                    ModelListOrigin::Live,
                                ))
                            }
                            // A failed fetch still has the curated rows to
                            // offer — the picker never becomes a dead end.
                            Err(reason) if !curated.is_empty() => {
                                Ok((curated, ModelListOrigin::Catalog(reason)))
                            }
                            Err(reason) => Err(reason),
                        };
                    let _ = tx
                        .send(ReaderSignal::ProviderModels {
                            provider_id,
                            result,
                        })
                        .await;
                });
                continue;
            }
            // `VerifyApiKey` (`/keys`, `Ctrl-T`) is client-only for the same
            // reason `SetApiKey` is: the key stays on this machine. One
            // `/models` call through the real registry — the same credential
            // precedence and headers a run would use — answered back as
            // `Action::ModelKeyVerified`.
            if let Intent::VerifyApiKey { model_id } = &intent {
                let model_id = model_id.clone();
                let data_dir = paths.data_dir.clone();
                let tx = live.query_tx.clone();
                tokio::spawn(async move {
                    let result = verify_model_key(&data_dir, &model_id).await;
                    let _ = tx
                        .send(ReaderSignal::ModelKeyVerified { model_id, result })
                        .await;
                });
                continue;
            }
            // `SetApiKey` / `RemoveApiKey` (D1, `/keys`) are client-only intents
            // (the keep-secrets-off-the-wire invariant, exactly like `AddModel`):
            // apply them to `auth.json` locally (load-before-write, atomic,
            // 0600), re-fire the status projection, and skip the daemon-command
            // mapping entirely.
            if let Intent::SetApiKey { target, key } = &intent {
                apply_set_api_key(state, paths, target, &key.0);
                continue;
            }
            if let Intent::RemoveApiKey { target } = &intent {
                apply_remove_api_key(state, paths, target);
                continue;
            }
            // Council creation is local, private configuration just like the
            // model/key flows above. Keep it off the daemon wire and reuse the
            // CLI council module's exact validation + atomic 0600 persistence.
            if let Intent::CreateCouncil {
                name,
                description,
                members,
                chair,
                rounds,
            } = &intent
            {
                let definition = crate::council::CouncilDefinition {
                    name: name.clone(),
                    description: description.clone(),
                    chair: chair.clone(),
                    rounds: *rounds,
                    members: members
                        .iter()
                        .map(|(model, role)| crate::council::CouncilMember {
                            model: model.clone(),
                            role: role.clone(),
                        })
                        .collect(),
                };
                match crate::council::persist_definition(paths, definition) {
                    Ok(created) => reduce(
                        state,
                        Action::CouncilCreated {
                            name: created.name,
                            members: created.members.len(),
                            rounds: created.rounds,
                        },
                    ),
                    Err(error) => reduce(
                        state,
                        Action::CouncilCreateFailed {
                            name: name.clone(),
                            error: error.to_string(),
                        },
                    ),
                }
                continue;
            }
            // Create and attach to a genuinely fresh session without tearing
            // down the TUI. A brand-new socket is fully handshaken and attached
            // before it replaces the old one. Dropping the old socket removes
            // all of its old-session forwarders, so late events from the prior
            // conversation can never bleed into this fresh state.
            if matches!(intent, Intent::NewConversation) {
                let workspace_id = store
                    .sessions
                    .get(repository)
                    .map_or_else(WorkspaceId::new, |stored| stored.workspace_id);
                let next_subscriptions = default_subscriptions();
                let resume = store
                    .resume_token
                    .clone()
                    .map(codypendent_protocol::ResumeToken);
                match create_fresh_session_live(
                    paths,
                    repository,
                    workspace_id,
                    &next_subscriptions,
                    resume,
                )
                .await
                {
                    Ok(fresh) => {
                        state.begin_new_session();
                        let mut watermark = fold_catchup(state, fresh.catchup);
                        for envelope in fresh.pending {
                            if let Payload::Event(event) = envelope.payload {
                                watermark = watermark.max(event.sequence);
                                reduce(state, Action::DaemonEvent(Box::new(event)));
                            }
                        }

                        let old_live = std::mem::replace(live, fresh.live);
                        old_live.shutdown();
                        session_id = fresh.session_id;
                        let capabilities = presentation.capabilities_message();
                        let _ = live
                            .out_tx
                            .send(remote_ui_envelope(live.client_id, session_id, capabilities))
                            .await;
                        tracker = GapTracker::new(watermark);
                        subscriptions = next_subscriptions;
                        replicas.clear();
                        blackboard_reads.clear();

                        if let Some(token) = fresh.resume_token {
                            store.resume_token = Some(token);
                        }
                        store.sessions.insert(
                            repository.to_owned(),
                            StoredSession {
                                session_id,
                                workspace_id,
                            },
                        );
                        store.save(paths);
                        reduce(state, Action::Notice("fresh conversation ready".to_owned()));
                    }
                    Err(error) => reduce(
                        state,
                        Action::Issue(format!("could not create a fresh conversation: {error}")),
                    ),
                }
                continue;
            }
            if let Intent::RefreshProjection { kind } = &intent {
                let Some(pool) = docs_pool.as_ref() else {
                    reduce(
                        state,
                        Action::Issue(
                            "advanced views unavailable: knowledge database is closed".into(),
                        ),
                    );
                    continue;
                };
                let repository_id =
                    codypendent_knowledge::stable_repository_id(Path::new(repository));
                let scopes = [
                    Scope::System,
                    Scope::Workspace(workspace_id),
                    Scope::Repository(repository_id),
                ];
                let mut warnings = Vec::new();
                match *kind {
                    ProjectionKind::Skills => {
                        let selected = state.focused_skill().map(|card| card.name.clone());
                        match Registry::new().list(pool).await {
                            Ok(items) => {
                                state.skills = items.iter().map(skill_card).collect();
                                state.selected_skill = selected
                                    .as_deref()
                                    .and_then(|name| {
                                        state.skills.iter().position(|card| card.name == name)
                                    })
                                    .unwrap_or(0);
                            }
                            Err(error) => {
                                warnings.push(format!("could not refresh skills: {error}"));
                            }
                        }
                    }
                    ProjectionKind::Memory => {
                        let selected = state.focused_memory().map(|card| card.statement.clone());
                        match MemoryStore::new().query(pool, &scopes, None).await {
                            Ok(records) => {
                                state.memories = records.iter().map(memory_card).collect();
                                state.selected_memory = selected
                                    .as_deref()
                                    .and_then(|statement| {
                                        state
                                            .memories
                                            .iter()
                                            .position(|card| card.statement == statement)
                                    })
                                    .unwrap_or(0);
                            }
                            Err(error) => {
                                warnings.push(format!("could not refresh memories: {error}"));
                            }
                        }
                    }
                    ProjectionKind::Docs => {
                        let selected = state.focused_doc().map(|card| card.document_id);
                        state.docs = load_docs(pool, &scopes, &mut warnings).await;
                        state.selected_doc = selected
                            .and_then(|document_id| {
                                state
                                    .docs
                                    .iter()
                                    .position(|card| card.document_id == document_id)
                            })
                            .unwrap_or(0);
                        if let Some(document_id) = state.focused_doc().map(|card| card.document_id)
                        {
                            state.outbox.push(Intent::WatchDocument { document_id });
                        }
                    }
                    ProjectionKind::Workflow => {
                        let selected = state
                            .focused_node()
                            .map(|card| (card.workflow_id.clone(), card.id.clone()));
                        let user_workflows = paths.data_dir.join("workflows");
                        state.workflow = load_workflows(
                            Path::new(repository),
                            Some(&user_workflows),
                            Some(pool),
                            &mut warnings,
                        )
                        .await;
                        state.selected_node = selected
                            .as_ref()
                            .and_then(|(workflow_id, node_id)| {
                                state.workflow.iter().position(|card| {
                                    &card.workflow_id == workflow_id && &card.id == node_id
                                })
                            })
                            .unwrap_or(0);
                        if let Some(workflow_run_id) = state
                            .focused_node()
                            .and_then(|card| card.workflow_run_id.clone())
                        {
                            state.outbox.push(Intent::WatchWorkflow { workflow_run_id });
                        }
                    }
                }
                for warning in warnings {
                    reduce(state, Action::Issue(warning));
                }
                continue;
            }
            if let Intent::SearchEdges { query, page } = &intent {
                if let Some(pool) = docs_pool.as_ref() {
                    let repository_id =
                        codypendent_knowledge::stable_repository_id(Path::new(repository));
                    let mut warnings = Vec::new();
                    let (edges, total, page) =
                        load_edge_page(pool, repository_id, query, *page, &mut warnings).await;
                    reduce(
                        state,
                        Action::EdgesLoaded {
                            edges,
                            total,
                            query: query.clone(),
                            page,
                        },
                    );
                    for warning in warnings {
                        reduce(state, Action::Issue(warning));
                    }
                } else {
                    reduce(
                        state,
                        Action::Issue(
                            "code graph unavailable: knowledge database is closed".into(),
                        ),
                    );
                }
                continue;
            }
            // Opening/focusing a workflow run grows this connection's live
            // workflow + blackboard subscriptions and reads authoritative
            // baselines. Repeated watches skip the re-attach but deliberately
            // re-read both baselines, so reopening a panel is immediately fresh.
            if let Intent::WatchWorkflow { workflow_run_id } = &intent {
                let has_workflow = subscriptions.iter().any(|subscription| {
                    matches!(
                        subscription,
                        Subscription::Workflow { workflow_run_id: id } if id == workflow_run_id
                    )
                });
                let has_blackboard = subscriptions.iter().any(|subscription| {
                    matches!(
                        subscription,
                        Subscription::Blackboard { workflow_run_id: id } if id == workflow_run_id
                    )
                });
                if !has_workflow {
                    subscriptions.push(Subscription::Workflow {
                        workflow_run_id: workflow_run_id.clone(),
                    });
                }
                if !has_blackboard {
                    subscriptions.push(Subscription::Blackboard {
                        workflow_run_id: workflow_run_id.clone(),
                    });
                }
                if !has_workflow || !has_blackboard {
                    let attach = command_envelope(
                        live.client_id,
                        CommandBody::AttachSession {
                            session_id,
                            last_seen_sequence: Some(tracker.last_seen()),
                            subscriptions: subscriptions.clone(),
                            requested_role: ClientRole::Controller,
                            repository: None,
                        },
                    );
                    if live.out_tx.send(attach).await.is_err() {
                        return Ok(());
                    }
                }

                let snapshot = command_envelope(
                    live.client_id,
                    CommandBody::ReadWorkflowRun {
                        workflow_run_id: workflow_run_id.clone(),
                    },
                );
                if live.out_tx.send(snapshot).await.is_err() {
                    return Ok(());
                }
                let board = command_envelope(
                    live.client_id,
                    CommandBody::ReadBlackboard {
                        workflow_run_id: workflow_run_id.clone(),
                        kind: None,
                        include_superseded: true,
                    },
                );
                if let Payload::Command(command) = &board.payload {
                    blackboard_reads.insert(command.command_id, workflow_run_id.clone());
                }
                if live.out_tx.send(board).await.is_err() {
                    return Ok(());
                }
                continue;
            }
            // The first edit on a document subscribes this client to its live sync
            // stream (a re-attach carrying the grown subscription set) and seeds its
            // replica, so the edit's own resulting sync — and every other writer's —
            // comes back. Done before the edit command is sent.
            if let Some(document_id) = doc_intent_target(&intent) {
                if let std::collections::hash_map::Entry::Vacant(slot) = replicas.entry(document_id)
                {
                    slot.insert(seed_replica(docs_pool.as_ref(), document_id).await);
                    subscriptions.push(Subscription::Document { document_id });
                    let attach = command_envelope(
                        live.client_id,
                        CommandBody::AttachSession {
                            session_id,
                            last_seen_sequence: Some(tracker.last_seen()),
                            subscriptions: subscriptions.clone(),
                            requested_role: ClientRole::Controller,
                            // Re-attach to grow this connection's own
                            // subscription set (a new Document edit) on an
                            // already-open session: no new repo context to warm.
                            repository: None,
                        },
                    );
                    if live.out_tx.send(attach).await.is_err() {
                        return Ok(());
                    }
                }
            }
            if matches!(intent, Intent::WatchDocument { .. }) {
                continue;
            }
            let envelope = command_envelope(
                live.client_id,
                intent_to_command(intent, session_id, repository),
            );
            if live.out_tx.send(envelope).await.is_err() {
                return Ok(()); // writer gone → connection is down; leave cleanly
            }
        }

        if state.should_detach || state.session_closed {
            return Ok(());
        }

        if redraw {
            presentation.draw(state, force_prompt)?;
        }
    }
}

enum PendingActions {
    One(Action),
    Many(Vec<Action>),
}

/// What the reader task forwards to the loop.
enum ReaderSignal {
    /// A validated Remote UI frame for the reducer-owned host session.
    RemoteUi(Box<codypendent_protocol::UiWireMessage>),
    /// Host-owned lifecycle projection for `/plugins`.
    UiPlugins(Vec<codypendent_protocol::UiPluginLifecycleStatus>),
    /// A live session event to fold into state (boxed: it is a large payload and
    /// every other message here is tiny).
    Event(Box<SessionEvent>),
    /// The daemon rejected a command this TUI sent (code + message). Surfaced
    /// as a transient status notice — silence here meant a rejected StartRun
    /// showed the user nothing at all.
    Rejected {
        code: String,
        message: String,
    },
    /// A catch-up reply (from the loop's own gap-triggered re-attach).
    Catchup(Box<Catchup>),
    /// A collaborative document's live CRDT sync (Phase 4 STEP 4.3). Boxed — it
    /// carries opaque CRDT bytes and every other signal here is tiny. The loop
    /// merges it into the document's client replica.
    DocumentSync(Box<DocumentSync>),
    /// The daemon granted an edit lease this client requested.
    DocumentLeaseGranted {
        document_id: DocumentId,
        lease_id: String,
    },
    /// The daemon refused an edit lease: the block range is held by another writer
    /// (`document.range-leased`) — surfaced as the presence-lite "blocked" signal.
    DocumentLeaseBlocked,
    /// Human-readable acknowledgement that the publish plan is now parked on
    /// the ordinary approval rail; no document write has happened yet.
    DocumentPublishPrepared {
        target: String,
        changed_files: usize,
        git_action: String,
    },
    /// A provider's fetched model list (model-discovery): the result of the
    /// spawned `<base_url>/models` GET merged with the curated catalog rows,
    /// keyed by `provider_id`, plus where the rows came from. Mapped by the
    /// loop's `select!` to `Action::ProviderModelsLoaded` (Ok) /
    /// `ProviderModelsFailed` (Err — the fetch failed AND the catalog had
    /// nothing for this provider). Carries NO key.
    ProviderModels {
        provider_id: String,
        result: Result<(Vec<AddModelRow>, ModelListOrigin), String>,
    },
    /// The result of a one-shot `/keys` key verification (`Ctrl-T`): `Ok` when
    /// the provider listed the configured model with the stored key, `Err`
    /// with a key-free reason otherwise.
    ModelKeyVerified {
        model_id: String,
        result: Result<(), String>,
    },
    /// Completion of an off-UI-thread ACP install + handshake + typed profile
    /// write. The loop refreshes model/key projections after success.
    AcpConnected {
        display_id: String,
        provider_id: String,
        /// Immutable `registry-id@version` persisted after a successful handshake.
        result: Result<String, String>,
    },
    /// A newly-created durable workflow run. The loop reloads the compiled/live
    /// projection, then immediately subscribes and reads its baselines.
    WorkflowRunStarted {
        workflow_run_id: String,
    },
    WorkflowSnapshot(Box<codypendent_protocol::WorkflowRunSnapshot>),
    WorkflowEvent(codypendent_protocol::WorkflowEvent),
    BlackboardItems {
        command_id: CommandId,
        items: Vec<codypendent_protocol::BlackboardItemView>,
    },
    BlackboardPosted(codypendent_protocol::BlackboardItemView),
    /// The daemon closed the connection.
    Closed,
}

fn workflow_phase_label(phase: codypendent_protocol::WorkflowRunPhase) -> &'static str {
    use codypendent_protocol::WorkflowRunPhase;
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

fn workflow_node_state_label(state: codypendent_protocol::WorkflowNodeState) -> &'static str {
    use codypendent_protocol::WorkflowNodeState;
    match state {
        WorkflowNodeState::Pending => "pending",
        WorkflowNodeState::Running => "running",
        WorkflowNodeState::WaitingApproval => "waiting_approval",
        WorkflowNodeState::Blocked => "blocked",
        WorkflowNodeState::Completed => "completed",
        WorkflowNodeState::Failed => "failed",
        WorkflowNodeState::Skipped => "skipped",
        _ => "unknown",
    }
}

fn workflow_node_update(node: codypendent_protocol::WorkflowNodeView) -> WorkflowNodeUpdate {
    WorkflowNodeUpdate {
        node_id: node.node_id,
        state: workflow_node_state_label(node.state).to_owned(),
        cost: node
            .cost
            .as_ref()
            .map_or_else(|| "\u{2014}".to_owned(), render_node_cost),
        error: node.error.unwrap_or_else(|| "\u{2014}".to_owned()),
    }
}

fn workflow_snapshot_action(snapshot: codypendent_protocol::WorkflowRunSnapshot) -> Action {
    Action::WorkflowSnapshotLoaded {
        workflow_run_id: snapshot.workflow_run_id,
        phase: workflow_phase_label(snapshot.phase).to_owned(),
        nodes: snapshot
            .nodes
            .into_iter()
            .map(workflow_node_update)
            .collect(),
    }
}

fn workflow_event_action(event: codypendent_protocol::WorkflowEvent) -> Action {
    use codypendent_protocol::WorkflowEvent;
    match event {
        WorkflowEvent::NodeTransitioned(node) => {
            let workflow_run_id = node.workflow_run_id.clone();
            let node = workflow_node_update(node);
            Action::WorkflowNodeUpdated {
                workflow_run_id,
                node_id: node.node_id,
                state: node.state,
                cost: node.cost,
                error: node.error,
            }
        }
        WorkflowEvent::RunPhaseChanged {
            workflow_run_id,
            phase,
        } => Action::WorkflowPhaseUpdated {
            workflow_run_id,
            phase: workflow_phase_label(phase).to_owned(),
        },
        _ => Action::NoOp,
    }
}

fn workflow_label_for_run<'a>(state: &'a AppState, workflow_run_id: &str) -> Option<&'a str> {
    state
        .workflow
        .iter()
        .find(|card| card.workflow_run_id.as_deref() == Some(workflow_run_id))
        .map(|card| card.workflow.as_str())
}

fn wire_blackboard_cards(
    state: &AppState,
    items: &[codypendent_protocol::BlackboardItemView],
) -> Vec<BlackboardItemCard> {
    items
        .iter()
        .map(|item| {
            wire_blackboard_item_card(workflow_label_for_run(state, &item.workflow_run_id), item)
        })
        .collect()
}

/// Own the read half: forward each live [`SessionEvent`], answer heartbeat
/// `Ping`s through the writer, and signal `Closed` on EOF or error. Runs to
/// completion of each `read_envelope` (never cancelled by the loop's `select!`),
/// so a frame is never torn in half.
async fn read_loop(
    mut read_half: OwnedReadHalf,
    event_tx: mpsc::Sender<ReaderSignal>,
    out_tx: mpsc::Sender<Envelope>,
    client_id: ClientId,
) {
    loop {
        match read_envelope(&mut read_half).await {
            Ok(Some(envelope)) => match envelope.payload {
                Payload::Ping => {
                    let pong = Envelope::request(client_id, Payload::Pong);
                    if out_tx.send(pong).await.is_err() {
                        break;
                    }
                }
                Payload::Event(event) => {
                    if event_tx
                        .send(ReaderSignal::Event(Box::new(event)))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Payload::RemoteUi { message } => {
                    if event_tx
                        .send(ReaderSignal::RemoteUi(message))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Payload::UiPluginLifecycle { plugins, .. } => {
                    if event_tx
                        .send(ReaderSignal::UiPlugins(plugins))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Payload::CommandRejected(error) => {
                    // A refused edit lease drives the presence-lite "blocked"
                    // indicator; every other rejection is a transient notice.
                    let signal = if error.code == "document.range-leased" {
                        ReaderSignal::DocumentLeaseBlocked
                    } else {
                        ReaderSignal::Rejected {
                            code: error.code,
                            message: error.message,
                        }
                    };
                    if event_tx.send(signal).await.is_err() {
                        break;
                    }
                }
                Payload::DocumentLeaseGranted { grant, .. } => {
                    if event_tx
                        .send(ReaderSignal::DocumentLeaseGranted {
                            document_id: grant.document_id,
                            lease_id: grant.lease_id,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Payload::DocumentPublishRequested {
                    target,
                    changed_files,
                    git_action,
                    ..
                } => {
                    if event_tx
                        .send(ReaderSignal::DocumentPublishPrepared {
                            target,
                            changed_files: changed_files.len(),
                            git_action,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Payload::DocumentSync(sync) => {
                    if event_tx
                        .send(ReaderSignal::DocumentSync(Box::new(sync)))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Payload::WorkflowRunStarted {
                    workflow_run_id, ..
                } => {
                    if event_tx
                        .send(ReaderSignal::WorkflowRunStarted { workflow_run_id })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Payload::WorkflowRunSnapshot { snapshot, .. } => {
                    if event_tx
                        .send(ReaderSignal::WorkflowSnapshot(Box::new(snapshot)))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Payload::WorkflowEvent { event } => {
                    if event_tx
                        .send(ReaderSignal::WorkflowEvent(event))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Payload::BlackboardItems { command_id, items } => {
                    if event_tx
                        .send(ReaderSignal::BlackboardItems { command_id, items })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Payload::BlackboardPosted(item) => {
                    if event_tx
                        .send(ReaderSignal::BlackboardPosted(item))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Payload::Catchup { catchup } => {
                    if event_tx
                        .send(ReaderSignal::Catchup(Box::new(catchup)))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                // Everything else (CommandAccepted, stray replies) is not an
                // input to the reducer's live loop — the TUI's state is driven
                // by durable events (Chapter 03). Drop it.
                _ => {}
            },
            Ok(None) | Err(_) => {
                let _ = event_tx.send(ReaderSignal::Closed).await;
                break;
            }
        }
    }
}

/// Own the write half: serialize every outgoing envelope (loop commands + reader
/// pongs) so the two producers never interleave a frame on the socket.
async fn write_loop(mut write_half: OwnedWriteHalf, mut out_rx: mpsc::Receiver<Envelope>) {
    while let Some(envelope) = out_rx.recv().await {
        if write_envelope(&mut write_half, &envelope).await.is_err() {
            break;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SplashGateDecision {
    Continue,
    Quit,
    Redraw,
    Ignore,
}

/// Interpret only the small, host-owned input vocabulary of the welcome gate.
/// Enter is the sole transition into the workspace; Escape/Ctrl-C remain a
/// humane way out, and resize/focus events repaint without leaking a key into
/// the main composer after startup.
fn splash_gate_decision(event: &CrosstermEvent) -> SplashGateDecision {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

    match event {
        CrosstermEvent::Key(key) if key.kind == KeyEventKind::Release => SplashGateDecision::Ignore,
        CrosstermEvent::Key(key) if key.code == KeyCode::Enter => SplashGateDecision::Continue,
        CrosstermEvent::Key(key)
            if key.code == KeyCode::Esc
                || (key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL)) =>
        {
            SplashGateDecision::Quit
        }
        CrosstermEvent::Resize(_, _) | CrosstermEvent::FocusGained => SplashGateDecision::Redraw,
        _ => SplashGateDecision::Ignore,
    }
}

/// Hold the completed startup screen until the user explicitly enters the
/// workspace. The existing input thread owns crossterm reads, so this gate and
/// the main event loop share one lossless stream with no terminal teardown.
async fn wait_for_splash_entry(
    guard: &mut TerminalGuard,
    theme: &Theme,
    ready_stage: &str,
    warnings: &[String],
    input_rx: &mut mpsc::Receiver<ClientInput>,
) -> anyhow::Result<bool> {
    let draw = |guard: &mut TerminalGuard| -> io::Result<()> {
        guard.terminal_mut().draw(|frame| {
            render_splash(frame, 0, ready_stage, warnings, true, theme);
        })?;
        Ok(())
    };
    draw(guard)?;

    loop {
        match input_rx.recv().await {
            Some(ClientInput::Terminal(event)) => match splash_gate_decision(&event) {
                SplashGateDecision::Continue => return Ok(true),
                SplashGateDecision::Quit => return Ok(false),
                SplashGateDecision::Redraw => draw(guard)?,
                SplashGateDecision::Ignore => {}
            },
            Some(ClientInput::AccessibleLine(_)) => {}
            None => bail!("terminal input closed before the workspace was opened"),
        }
    }
}

/// Bridge blocking `crossterm` input into the async loop on a dedicated OS
/// thread. Polls with a short timeout so it observes `running` going false
/// promptly; sends each event over `tx` until the loop drops the receiver.
fn spawn_input_thread(tx: mpsc::Sender<ClientInput>, running: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while running.load(Ordering::Relaxed) {
            match crossterm::event::poll(Duration::from_millis(100)) {
                Ok(true) => match crossterm::event::read() {
                    Ok(event) => {
                        if tx.blocking_send(ClientInput::Terminal(event)).is_err() {
                            break; // loop ended
                        }
                    }
                    Err(_) => break,
                },
                Ok(false) => continue, // timed out; re-check `running`
                Err(_) => break,
            }
        }
    });
}

/// Map a reducer [`Intent`] to the wire [`CommandBody`], binding the session id
/// the TUI is attached to. A pure 1:1 translation — the whole point of the
/// outbox is that `reduce` stays I/O-free and this is the only place intents
/// become protocol.
fn intent_to_command(intent: Intent, session_id: SessionId, repository: &str) -> CommandBody {
    match intent {
        Intent::RemoteUiMessage(_) => unreachable!(
            "RemoteUiMessage is framed directly by the harness, never mapped to a CommandBody"
        ),
        Intent::ListUiPlugins => CommandBody::ListUiPlugins,
        Intent::SmokeTestUiPlugin { plugin_id } => {
            CommandBody::SmokeTestUiPlugin { plugin_id }
        }
        Intent::EnableUiPlugin { plugin_id, scope } => CommandBody::EnableUiPlugin {
            plugin_id,
            session_id: (scope != "user").then_some(session_id),
            scope,
        },
        Intent::ApproveUiPluginUpdate { plugin_id, receipt } => {
            CommandBody::ApproveUiPluginUpdate {
                plugin_id,
                approval_receipt: receipt,
            }
        }
        Intent::RejectUiPluginUpdate { plugin_id, receipt } => {
            CommandBody::RejectUiPluginUpdate {
                plugin_id,
                approval_receipt: receipt,
            }
        }
        Intent::RevokeUiPlugin { plugin_id } => CommandBody::RevokeUiPlugin { plugin_id },
        Intent::StartRun {
            objective,
            mode,
            model,
        } => CommandBody::StartRun {
            session_id,
            objective,
            mode,
            // Attribute the run to the repository this TUI is attached to, so a
            // shared daemon does not store its memories under its own directory
            // (issue #6 item 1).
            repository: Some(repository.to_owned()),
            // Carry the operator's pinned model (STEP MP2) onto the wire; `None`
            // lets the daemon resolve/route as before.
            model,
        },
        // Task 5 (continuous-session plan): a follow-up once the selected run
        // is terminal. Carries the operator's current `model` pin so a
        // mid-conversation switch is instant (the re-pick applies to this very
        // follow-up); `None` lets the daemon INHERIT the session's model (I-2)
        // from its provenance, unchanged. Repository (I-1) is still not on this
        // wire shape: the daemon RECOVERS it from the session's originating
        // `StartRun` (see `commands::session_run_provenance`), so a follow-up
        // runs against the session's real checkout, not the daemon's frozen
        // `current_dir()`.
        Intent::SubmitUserInput { text, mode, model } => CommandBody::SubmitUserInput {
            session_id,
            text,
            mode,
            model,
        },
        Intent::ResolveApproval {
            approval_id,
            decision,
            scope,
        } => CommandBody::ResolveApproval {
            approval_id,
            decision,
            scope,
        },
        Intent::PauseRun { run_id } => CommandBody::PauseRun { run_id },
        Intent::ResumeRun { run_id } => CommandBody::ResumeRun { run_id },
        Intent::CancelRun { run_id } => CommandBody::CancelRun { run_id },
        Intent::QueueSteering { run_id, text } => CommandBody::QueueSteering { run_id, text },
        // Phase 4 STEP 4.3: document editing. Subscription to the document's sync
        // stream is arranged separately (in the drain loop) before the first of
        // these is sent, so the client sees its own edit's authoritative result.
        Intent::AcquireDocumentLease {
            document_id,
            block_id,
        } => CommandBody::AcquireDocumentLease {
            lease: DocumentEditLease {
                document_id,
                block_id,
            },
            ttl_seconds: None,
        },
        Intent::ReleaseDocumentLease { lease_id } => CommandBody::ReleaseDocumentLease { lease_id },
        Intent::MutateDocument {
            document_id,
            mutation,
        } => CommandBody::MutateDocument {
            document_id,
            mutation,
        },
        Intent::PublishDocument {
            document_id,
            target,
        } => CommandBody::PublishDocument {
            document_id,
            target,
        },
        Intent::WatchDocument { .. } => unreachable!(
            "WatchDocument is applied locally by the harness, never sent to the daemon"
        ),
        Intent::SearchEdges { .. } => unreachable!(
            "SearchEdges is applied locally by the harness, never sent to the daemon"
        ),
        Intent::RefreshProjection { .. } => unreachable!(
            "RefreshProjection is applied locally by the harness, never sent to the daemon"
        ),
        Intent::StartWorkflow {
            workflow_id,
            inputs,
        } => CommandBody::StartWorkflow {
            manifest: String::new(),
            workflow_id: Some(workflow_id),
            inputs,
            repository: Some(repository.to_owned()),
        },
        Intent::WatchWorkflow { .. } => unreachable!(
            "WatchWorkflow is applied locally by the harness, never sent to the daemon"
        ),
        Intent::PauseWorkflow { workflow_run_id } => {
            CommandBody::PauseWorkflow { workflow_run_id }
        }
        Intent::ResumeWorkflow { workflow_run_id } => {
            CommandBody::ResumeWorkflow { workflow_run_id }
        }
        Intent::RetryWorkflowNode {
            workflow_run_id,
            node_id,
        } => CommandBody::RetryWorkflowNode {
            workflow_run_id,
            node_id,
        },
        Intent::CancelWorkflow { workflow_run_id } => {
            CommandBody::CancelWorkflow { workflow_run_id }
        }
        // `AddModel` and `QueryProviderModels` are CLIENT-ONLY intents applied
        // locally by the harness (see the drain loop's interceptions); neither
        // becomes a daemon command, so these mappings are never reached.
        Intent::AddModel { .. } => unreachable!(
            "AddModel is applied locally by the harness (write_add_model), never sent to the daemon"
        ),
        Intent::QueryProviderModels { .. } => unreachable!(
            "QueryProviderModels is applied locally by the harness (background GET), never sent to the daemon"
        ),
        // D1: the `/keys` intents are CLIENT-ONLY for the same reason (the key
        // never crosses the wire; adapters resolve auth.json at use time) —
        // intercepted in the drain loop, never mapped.
        Intent::SetApiKey { .. } | Intent::RemoveApiKey { .. } => unreachable!(
            "SetApiKey/RemoveApiKey are applied locally by the harness (write_api_key), never sent to the daemon"
        ),
        Intent::VerifyApiKey { .. } => unreachable!(
            "VerifyApiKey is probed locally by the harness (verify_model_key), never sent to the daemon"
        ),
        Intent::CreateCouncil { .. } => unreachable!(
            "CreateCouncil is validated and persisted locally by the harness, never sent to the daemon"
        ),
        Intent::NewConversation => unreachable!(
            "NewConversation is applied locally by the harness, never sent to the daemon"
        ),
    }
}

/// Apply an `Intent::AddModel` to the local config: append (or update in place) a
/// `[[model]]` entry in `<data_dir>/models.toml`, and, when a non-blank key was
/// entered, store it in `<data_dir>/auth.json` (mode `0600`). This is the
/// harness's job because the `tui` crate performs no I/O and never touches the
/// key.
///
/// The written entry is always `provider = "openai-compatible"` (the only wire
/// adapter `ModelConfig`/`client_for` supports today); `base_url` is read from the
/// catalog provider (`<data_dir>/providers.toml` layered over the built-ins). A
/// duplicate `display_id` UPDATES its entry rather than duplicating it. Both files
/// are written atomically (temp + rename) so a concurrent daemon read never sees a
/// torn file.
///
/// Two guards, deliberate and load-bearing (not just brief follow-through):
/// - A blank/whitespace-only `display_id` is rejected outright — nothing is
///   written (neither file). Model ids double as the `auth.json` key, so a blank
///   one is never a legitimate profile, just an empty prompt submitted as-is.
/// - A blank/whitespace-only `api_key` is treated exactly like `None` — the
///   `auth.json` write is skipped entirely. Storing `AuthStore::set(id, "")` would
///   silently shadow a valid `api_key_env` into "no key" at resolution time
///   (`ModelRegistry::client_for` prefers a *present* `auth.json` entry over the
///   env var unconditionally) — a real regression a prior review flagged (SDD
///   ledger M1) and this function must not reintroduce.
///
/// All-or-nothing when a key is entered (SDD ledger M3): `AuthStore::load` is
/// fallible (a hand-corrupted `auth.json` surfaces as `Err`), so it is called
/// BEFORE `models.toml` is written — a corrupt pre-existing `auth.json` aborts
/// the whole add before anything is written, rather than leaving a keyless
/// `models.toml` entry behind while the key silently fails to save. A keyless
/// add never loads `auth.json` at all, exactly as before.
///
/// The entry also records `provider_id`, so the runtime resolves this
/// provider's auth header/prefix and extra headers from the catalog instead of
/// flattening every provider to `Authorization: Bearer` (Azure OpenAI's
/// `api-key` header would otherwise 401 on the first run), and
/// `context_tokens` when the picked row knew it — never a guess. The key is
/// stored twice: under the model's display id (today's behavior) and under the
/// provider-wide `provider/<id>` entry, so the next model added from the same
/// provider does not re-prompt for the same key.
fn write_add_model(
    paths: &RuntimePaths,
    display_id: &str,
    provider_id: &str,
    model: &str,
    api_key: Option<&str>,
    context_tokens: Option<u64>,
) -> anyhow::Result<()> {
    use codypendent_providers::Catalog;
    use codypendent_runtime::auth::AuthStore;
    use codypendent_runtime::models::{load_models, ModelConfig};

    if display_id.trim().is_empty() {
        bail!("model id must not be blank");
    }

    let data_dir = &paths.data_dir;
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("creating the data dir {}", data_dir.display()))?;

    // A blank/whitespace-only key is treated exactly like `None` (see the doc
    // comment's second guard) — filtered up front so the load-order guard right
    // below and the final write agree on whether a key is really present.
    let key = api_key.filter(|k| !k.trim().is_empty());

    // M3 (all-or-nothing): when a key is present, load auth.json NOW, before
    // models.toml is written. `AuthStore::load` is fallible — a hand-corrupted
    // `auth.json` surfaces as `Err` right here, aborting the whole add before
    // anything is written, rather than leaving a keyless `models.toml` entry
    // behind while the key silently fails to save. A keyless add loads no
    // auth.json at all, exactly as before.
    let mut auth = key
        .is_some()
        .then(|| {
            AuthStore::load(data_dir)
                .with_context(|| format!("reading {}", data_dir.join("auth.json").display()))
        })
        .transpose()?;

    // Resolve the catalog base_url for the chosen provider (built-ins layered with
    // any user providers.toml; a load failure falls back to the built-ins).
    let acp_store = codypendent_integrations::acp_registry::AcpRegistryStore::new(data_dir);
    let acp_agent = acp_store
        .load_cached()
        .ok()
        .and_then(|registry| registry.get(provider_id).cloned());
    let catalog = Catalog::load_with_user_overrides(&data_dir.join("providers.toml"))
        .unwrap_or_else(|_| Catalog::builtin());
    let (runtime_provider, base_url, provider_model, catalog_provider) = if let Some(agent) =
        acp_agent
    {
        let coordinate = if codypendent_integrations::acp_registry::agent_id_from_coordinate(model)
            == agent.id
        {
            model.to_string()
        } else {
            agent.id.clone()
        };
        acp_store
            .launch_spec(&coordinate)
            .with_context(|| format!("ACP agent `{}` is not launchable", agent.id))?;
        ("acp".to_string(), String::new(), coordinate, false)
    } else {
        let provider = catalog
            .get(provider_id)
            .ok_or_else(|| anyhow!("provider `{provider_id}` is not in the catalog"))?;
        if !provider_runtime_supported(provider) {
            bail!(
                "provider `{provider_id}` uses {} and is not executable by this build",
                protocol_label(provider.protocol)
            );
        }
        (
            "openai-compatible".to_string(),
            // Normalized on persist: a catalog `base_url` written with a
            // trailing slash (`…/v1/`) would otherwise reach the chat client
            // as `…/v1//chat/completions`.
            normalize_base_url(
                provider
                    .base_url
                    .as_deref()
                    .expect("runtime-supported providers have a non-blank base URL"),
            ),
            model.to_string(),
            true,
        )
    };
    // A catalog row for this exact model fills in the context window when the
    // caller did not already know it (the picker passes what it displayed).
    let context_tokens = context_tokens.or_else(|| {
        catalog
            .model(provider_id, model)
            .and_then(|row| row.context_tokens)
    });

    // Read the existing models.toml (absent ⇒ start empty) through the real
    // loader, drop any entry sharing the new display id (update-in-place), then
    // append the new one — every other existing entry survives untouched.
    let models_path = data_dir.join("models.toml");
    let mut configs = if models_path.exists() {
        load_models(&models_path).with_context(|| format!("reading {}", models_path.display()))?
    } else {
        Vec::new()
    };
    configs.retain(|c| c.id.0 != display_id);
    configs.push(ModelConfig {
        id: ModelId(display_id.to_string()),
        provider: runtime_provider,
        base_url,
        model: provider_model,
        api_key_env: String::new(),
        // Only a catalog provider's auth is resolvable from the catalog; an
        // ACP agent's provider id names a registry agent, not a provider.
        provider_id: catalog_provider.then(|| provider_id.to_string()),
        context_tokens,
    });

    // Serialize back to `[[model]]` tables and write atomically.
    #[derive(serde::Serialize)]
    struct ModelsToml {
        #[serde(rename = "model")]
        model: Vec<ModelConfig>,
    }
    let rendered = toml::to_string_pretty(&ModelsToml { model: configs })
        .context("serializing models.toml")?;
    let models_tmp = data_dir.join("models.toml.tmp");
    std::fs::write(&models_tmp, rendered.as_bytes())
        .with_context(|| format!("writing {}", models_tmp.display()))?;
    std::fs::rename(&models_tmp, &models_path)
        .with_context(|| format!("replacing {}", models_path.display()))?;

    // Store the key (hosted providers only) in auth.json at 0600 — loaded
    // above, BEFORE models.toml was written, so a corrupt pre-existing
    // auth.json already aborted the whole operation before this point (M3).
    if let Some(key) = key {
        let auth = auth
            .as_mut()
            .expect("loaded above because `key` is Some (M3 ordering)");
        auth.set(display_id, key);
        // Also store it provider-wide, so adding a second model from the same
        // provider needs no second paste of the same key. The runtime reads
        // this entry after the per-model one (`provider_auth_id`).
        if catalog_provider {
            auth.set(provider_auth_id(provider_id), key);
        }
        auth.save(data_dir)
            .with_context(|| format!("writing {}", data_dir.join("auth.json").display()))?;
    }
    Ok(())
}

/// A provider `base_url` as it should be persisted: trailing slashes trimmed.
/// The catalog stores a few with one (`…/v1/`), and the chat client joins
/// `{base}/chat/completions` — the raw value would produce a double slash on
/// every request. A blank/whitespace-only URL is returned as an empty string
/// (the caller's own validation reports it).
fn normalize_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

/// The provider-wide key a previous add stored in `auth.json`
/// (`provider/<catalog-id>`), if any. Read so the add flow never asks for a key
/// it already holds; a missing or corrupt store is simply "no key".
fn stored_provider_key(data_dir: &Path, provider_id: &str) -> Option<String> {
    codypendent_runtime::auth::AuthStore::load(data_dir)
        .ok()?
        .get(&provider_auth_id(provider_id))
        .map(str::to_owned)
}

/// One-shot verification of a configured model's credentials (`/keys`,
/// `Ctrl-T`): run the real [`ModelRegistry::check_model`], which resolves the
/// key through the same precedence a run uses and sends the same
/// catalog-declared headers, so an "ok" here means the run would authenticate.
/// The returned reason is the registry's own error text, which never contains
/// key material.
async fn verify_model_key(data_dir: &Path, model_id: &str) -> Result<(), String> {
    use codypendent_runtime::models::{load_models, ModelRegistry};

    let models_path = data_dir.join("models.toml");
    let configs = load_models(&models_path).map_err(|error| error.to_string())?;
    let auth = codypendent_runtime::auth::AuthStore::load(data_dir).unwrap_or_default();
    let catalog =
        codypendent_providers::Catalog::load_with_user_overrides(&data_dir.join("providers.toml"))
            .unwrap_or_else(|_| codypendent_providers::Catalog::builtin());
    ModelRegistry::new(configs)
        .with_auth(auth)
        .with_catalog(catalog)
        .check_model(&ModelId(model_id.to_owned()))
        .await
        .map_err(|error| error.to_string())
}

/// Install (when necessary) and handshake one official ACP agent away from the
/// render loop. The caller writes `models.toml` only after this succeeds, so a
/// bad archive, missing runner, or incompatible agent never leaves a broken
/// selectable profile behind.
async fn connect_acp_agent(
    paths: &RuntimePaths,
    provider_id: &str,
    repository: &Path,
) -> anyhow::Result<String> {
    let store = codypendent_integrations::acp_registry::AcpRegistryStore::new(&paths.data_dir);
    let spec = store.install(provider_id, false).await?;
    let command = spec.command.to_string_lossy().into_owned();
    let client = codypendent_integrations::acp_client::AcpClient::spawn(
        &command,
        &spec.args,
        &spec.env,
        repository.to_string_lossy().as_ref(),
    )
    .await
    .with_context(|| format!("ACP handshake with `{provider_id}` failed"))?;
    drop(client);
    Ok(codypendent_integrations::acp_registry::agent_coordinate(
        &spec.registry_id,
        &spec.version,
    ))
}

/// Apply the client-only `Intent::AddModel` (the event-loop drain arm,
/// extracted so the behavior is directly testable): write `models.toml` +
/// `auth.json` locally, then re-seed the model picker AND re-fire the `/keys`
/// status projection — a model added WITH a key must show `Stored` in `/keys`
/// without a TUI restart. Any loader diagnostic surfaces as a notice.
async fn apply_add_model(
    state: &mut AppState,
    paths: &RuntimePaths,
    display_id: &str,
    provider_id: &str,
    model: &str,
    api_key: Option<&str>,
    context_tokens: Option<u64>,
) {
    match write_add_model(
        paths,
        display_id,
        provider_id,
        model,
        api_key,
        context_tokens,
    ) {
        Ok(()) => {
            // Re-seed the model picker so the new model shows immediately.
            let mut warnings = Vec::new();
            state.models = load_model_cards(paths, &mut warnings).await;
            for warning in warnings {
                reduce(state, Action::Issue(warning));
            }
            reload_key_statuses(state, paths);
            reduce(state, Action::Notice(format!("added model {display_id}")));
        }
        Err(error) => {
            reduce(
                state,
                Action::Notice(format!("could not add model: {error}")),
            );
        }
    }
}

/// The `auth.json` entry id a [`KeyTarget`] addresses (D1): a model target's
/// id doubles as the entry key (the add-model flow's convention); the Tavily
/// target maps onto the reserved, collision-proof `integrations/tavily` id the
/// daemon's `TavilyKey::discover` reads.
fn key_target_auth_id(target: &KeyTarget) -> String {
    match target {
        KeyTarget::Model(id) => id.clone(),
        KeyTarget::Tavily => codypendent_integrations::search::TAVILY_AUTH_ID.to_owned(),
    }
}

/// Apply an `Intent::SetApiKey` (`Some(key)`) or `Intent::RemoveApiKey`
/// (`None`) to `<data_dir>/auth.json` (D1). This is the harness's job because
/// the `tui` crate performs no I/O and never touches the key.
///
/// The same load-before-write guard as [`write_add_model`] (M3) applies to
/// BOTH operations: `AuthStore::load` is fallible, so a hand-corrupted
/// pre-existing `auth.json` aborts here with a legible error rather than
/// being silently replaced (a blind `set` on a fresh store would destroy the
/// other entries). The save is atomic at mode `0600` (`AuthStore::save`). A
/// blank/whitespace-only key is rejected outright — storing `set(id, "")`
/// would silently shadow a valid `api_key_env` (the M1 guard). A remove of an
/// absent entry skips the save (nothing changed, and no empty `auth.json` is
/// created for a store that never existed).
fn write_api_key(
    paths: &RuntimePaths,
    target: &KeyTarget,
    key: Option<&str>,
) -> anyhow::Result<()> {
    use codypendent_runtime::auth::AuthStore;

    let data_dir = &paths.data_dir;
    let id = key_target_auth_id(target);
    let mut auth = AuthStore::load(data_dir)
        .with_context(|| format!("reading {}", data_dir.join("auth.json").display()))?;
    match key {
        Some(key) => {
            let key = key.trim();
            if key.is_empty() {
                bail!("key must not be blank");
            }
            auth.set(id, key);
            auth.save(data_dir)
                .with_context(|| format!("writing {}", data_dir.join("auth.json").display()))?;
        }
        None => {
            if auth.remove(&id) {
                auth.save(data_dir)
                    .with_context(|| format!("writing {}", data_dir.join("auth.json").display()))?;
            }
        }
    }
    Ok(())
}

/// Apply the client-only `Intent::SetApiKey` (the event-loop drain arm,
/// extracted so the behavior is directly testable): write the key to
/// `auth.json` and re-fire the status projection. Model and Tavily credentials
/// are both resolved lazily by the daemon, so neither path needs a restart.
fn apply_set_api_key(state: &mut AppState, paths: &RuntimePaths, target: &KeyTarget, key: &str) {
    match write_api_key(paths, target, Some(key)) {
        Ok(()) => {
            reload_key_statuses(state, paths);
            match target {
                KeyTarget::Model(id) => {
                    // The daemon re-reads auth.json per run, so the
                    // key applies to the NEXT run — no restart.
                    reduce(
                        state,
                        Action::Notice(format!("key saved for {id} — applies to the next run")),
                    );
                }
                KeyTarget::Tavily => {
                    reduce(
                        state,
                        Action::Notice("Tavily key saved — web search is ready".to_owned()),
                    );
                }
            }
        }
        Err(error) => {
            reduce(
                state,
                Action::Notice(format!("could not save key: {error}")),
            );
        }
    }
}

/// Apply the client-only `Intent::RemoveApiKey` (the event-loop drain arm,
/// extracted so the behavior is directly testable): remove the entry from
/// `auth.json` and re-fire the status projection. Tavily resolves per call, so
/// removal also applies immediately.
fn apply_remove_api_key(state: &mut AppState, paths: &RuntimePaths, target: &KeyTarget) {
    match write_api_key(paths, target, None) {
        Ok(()) => {
            reload_key_statuses(state, paths);
            match target {
                KeyTarget::Model(id) => {
                    reduce(
                        state,
                        Action::Notice(format!("key for {id} removed — applies to the next run")),
                    );
                }
                KeyTarget::Tavily => {
                    reduce(
                        state,
                        Action::Notice("Tavily key removed — web search is disabled".to_owned()),
                    );
                }
            }
        }
        Err(error) => {
            reduce(
                state,
                Action::Notice(format!("could not remove key: {error}")),
            );
        }
    }
}

/// Read the `/keys` status projection (D1): one `(model_id, status)` per
/// `models.toml` model, plus the Tavily row's status — `Stored` when an
/// `auth.json` entry exists, else `Env(NAME)` when the model declares an
/// `api_key_env` (the NAME only, never the value), else `Missing`.
///
/// The Tavily row mirrors the daemon's `TavilyKey::discover` precedence: the
/// reserved `auth.json` entry first, then the `TAVILY_API_KEY` env var. The
/// env check reads THIS client's environment as an approximation of the
/// daemon's (the same approximation the model rows already make with
/// `api_key_env`) and checks PRESENCE only — the value is never read into the
/// projection.
///
/// Best-effort: a corrupt `auth.json` or unreadable `models.toml` degrades to
/// "no stored keys"/"no models" with a diagnostic in `warnings` (the terminal
/// is already in the alternate screen when this first runs, so nothing prints
/// to stderr). The WRITE path (`write_api_key`) still surfaces the same
/// corruption as a hard error — statuses are a view, never the authority.
fn load_key_statuses(
    paths: &RuntimePaths,
    warnings: &mut Vec<String>,
) -> (Vec<(String, KeyStatus)>, KeyStatus) {
    use codypendent_runtime::auth::AuthStore;
    use codypendent_runtime::models::load_models;

    let data_dir = &paths.data_dir;
    let auth = AuthStore::load(data_dir).unwrap_or_else(|error| {
        warnings.push(format!(
            "could not read {}: {error}; /keys statuses may be incomplete",
            data_dir.join("auth.json").display()
        ));
        AuthStore::default()
    });
    let configs = load_models(&data_dir.join("models.toml")).unwrap_or_default();
    let models = configs
        .iter()
        .map(|cfg| {
            let status = if auth.get(&cfg.id.0).is_some() {
                KeyStatus::Stored
            } else if cfg.api_key_env.trim().is_empty() {
                KeyStatus::Missing
            } else {
                KeyStatus::Env(cfg.api_key_env.clone())
            };
            (cfg.id.0.clone(), status)
        })
        .collect();
    let tavily = if auth
        .get(codypendent_integrations::search::TAVILY_AUTH_ID)
        .is_some()
    {
        KeyStatus::Stored
    } else if std::env::var(codypendent_integrations::search::key::TAVILY_API_KEY_ENV)
        .is_ok_and(|value| !value.trim().is_empty())
    {
        KeyStatus::Env(codypendent_integrations::search::key::TAVILY_API_KEY_ENV.to_owned())
    } else {
        KeyStatus::Missing
    };
    (models, tavily)
}

/// Re-read the key statuses and fold them into the TUI state (D1) — after the
/// initial seed, after every key write, and after a daemon restart. A
/// best-effort read diagnostic (e.g. a corrupt `auth.json`) surfaces as a
/// notice, exactly like the boot-time seed's diagnostics.
fn reload_key_statuses(state: &mut AppState, paths: &RuntimePaths) {
    let mut warnings = Vec::new();
    let (models, tavily) = load_key_statuses(paths, &mut warnings);
    reduce(state, Action::ApiKeyStatusesLoaded { models, tavily });
    for warning in warnings {
        reduce(state, Action::Issue(warning));
    }
}

/// The provider's OpenAI-compatible model-list URL: `<base_url>/models`. The
/// catalog `base_url` already carries its version segment (`…/v1`, `…/v4`, …),
/// so the list route is its sibling `/models` — never `/v1/models` (which would
/// double the version). A trailing slash is trimmed so the join is exact.
fn models_url(base_url: &str) -> String {
    format!("{}/models", base_url.trim_end_matches('/'))
}

/// One entry of a provider's `/models` response, keeping the OPTIONAL metadata
/// several OpenAI-compatible providers ship alongside the id. Every field but
/// `id` is best-effort: a provider that answers with bare ids parses exactly as
/// before, and a provider that answers with a shape this build does not know
/// simply contributes nothing extra.
///
/// The known spellings, all observed in the wild on the same endpoint the add
/// flow already calls: `context_length` (OpenRouter, Nebius `?verbose=true`),
/// `max_model_len` (vLLM-derived: DeepInfra, Novita, SambaNova),
/// `max_context_length`/`context_window` (Venice and friends), and a nested
/// `pricing.{prompt,completion}` object priced per TOKEN (OpenRouter's
/// convention) which is scaled to per-1M for display.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct DiscoveredModel {
    #[serde(default)]
    id: String,
    #[serde(default, alias = "display_name", alias = "name")]
    name: Option<String>,
    #[serde(
        default,
        alias = "context_length",
        alias = "max_model_len",
        alias = "max_context_length",
        alias = "context_window"
    )]
    context_tokens: Option<u64>,
    #[serde(default)]
    pricing: Option<DiscoveredPricing>,
}

/// A `/models` entry's optional pricing object. The values arrive as strings on
/// OpenRouter (`"0.0000004"`) and as numbers elsewhere, so both are accepted
/// and anything unparseable is simply dropped.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct DiscoveredPricing {
    #[serde(default, alias = "input")]
    prompt: Option<serde_json::Value>,
    #[serde(default, alias = "output")]
    completion: Option<serde_json::Value>,
}

/// A per-token price as USD per 1M tokens, accepting the string and number
/// spellings providers use. `None` for anything that is not a finite,
/// non-negative number — a fabricated price is worse than a blank column.
fn price_per_1m(value: Option<&serde_json::Value>) -> Option<f64> {
    let raw = match value? {
        serde_json::Value::String(text) => text.trim().parse::<f64>().ok()?,
        other => other.as_f64()?,
    };
    (raw.is_finite() && raw >= 0.0).then_some(raw * 1_000_000.0)
}

/// Parse an OpenAI/Ollama `/models` response body (`{ "object": "list", "data":
/// [ { "id": "…" }, … ] }`) into rows: trim each id, skip blank/missing, dedup
/// preserving order, and keep any optional metadata the provider volunteered.
/// An empty result is an `Err` so the caller can fall back to the catalog
/// uniformly. A pure function over the body string — the network GET is in
/// `query_provider_models` — so it is directly unit-testable. The error strings
/// are generic and never carry a key.
fn parse_models_response(body: &str) -> Result<Vec<AddModelRow>, String> {
    #[derive(serde::Deserialize)]
    struct ModelsResponse {
        #[serde(default)]
        data: Vec<DiscoveredModel>,
    }
    let parsed: ModelsResponse = serde_json::from_str(body)
        .map_err(|_| "the provider returned an unexpected response".to_string())?;
    let mut rows: Vec<AddModelRow> = Vec::new();
    for entry in parsed.data {
        let id = entry.id.trim().to_string();
        if id.is_empty() || rows.iter().any(|row| row.id == id) {
            continue;
        }
        let pricing = entry.pricing.unwrap_or_default();
        rows.push(AddModelRow {
            id,
            name: entry
                .name
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty()),
            context_tokens: entry.context_tokens.filter(|tokens| *tokens > 0),
            cost_per_1m_input_usd: price_per_1m(pricing.prompt.as_ref()),
            cost_per_1m_output_usd: price_per_1m(pricing.completion.as_ref()),
            live: true,
        });
    }
    if rows.is_empty() {
        return Err("provider returned no models".to_string());
    }
    Ok(rows)
}

/// Merge a provider's live listing with the curated catalog rows for the same
/// provider: catalog metadata fills any gap a live row left (never overwriting
/// what the provider itself said), and catalog models the listing did not name
/// are appended as unconfirmed (`live: false`) rows — the offline/no-listing
/// path. Live rows keep their listing order; catalog-only rows follow in
/// catalog order, so the provider's own ranking survives.
fn merge_catalog_rows(live: Vec<AddModelRow>, catalog: &[AddModelRow]) -> Vec<AddModelRow> {
    let mut rows = live;
    for row in &mut rows {
        let Some(known) = catalog.iter().find(|entry| entry.id == row.id) else {
            continue;
        };
        if row.name.is_none() {
            row.name.clone_from(&known.name);
        }
        if row.context_tokens.is_none() {
            row.context_tokens = known.context_tokens;
        }
        if row.cost_per_1m_input_usd.is_none() {
            row.cost_per_1m_input_usd = known.cost_per_1m_input_usd;
        }
        if row.cost_per_1m_output_usd.is_none() {
            row.cost_per_1m_output_usd = known.cost_per_1m_output_usd;
        }
    }
    for known in catalog {
        if !rows.iter().any(|row| row.id == known.id) {
            rows.push(known.clone());
        }
    }
    rows
}

/// The catalog's curated `[[model]]` rows for one provider, as pick-list rows.
/// `live: false` — these are offerable, but nothing has confirmed this account
/// can reach them.
fn catalog_rows_for(
    catalog: &codypendent_providers::Catalog,
    provider_id: &str,
) -> Vec<AddModelRow> {
    catalog
        .models()
        .filter(|model| model.provider_id == provider_id)
        .map(|model| AddModelRow {
            id: model.id.clone(),
            name: model.name.clone(),
            context_tokens: model.context_tokens,
            cost_per_1m_input_usd: model.cost_per_1m_input_usd,
            cost_per_1m_output_usd: model.cost_per_1m_output_usd,
            live: false,
        })
        .collect()
}

/// The on-disk shape of one cached provider listing
/// (`<data_dir>/model_lists/<provider>.json`). Plain data, no key material: it
/// holds exactly what the pick-list shows. `fetched_at_unix` is seconds since
/// the epoch — the same dependency-free stamp `humantime_now` uses, rather
/// than pulling a time-formatting crate into this crate's build.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedModelList {
    provider_id: String,
    fetched_at_unix: u64,
    models: Vec<CachedModelRow>,
}

/// One cached row. Mirrors [`AddModelRow`] minus `live` — everything in the
/// cache came from a live listing at `fetched_at`, and is re-marked as such
/// when it is read back.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedModelRow {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cost_per_1m_input_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cost_per_1m_output_usd: Option<f64>,
}

/// Where one provider's cached listing lives.
fn model_list_cache_path(data_dir: &Path, provider_id: &str) -> PathBuf {
    // Provider ids are catalog identifiers (`azure-openai`, `nebius`), but the
    // value reaches here from a user-editable `providers.toml`, so any path
    // separator is neutralized before it becomes a file name.
    let safe: String = provider_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    data_dir.join("model_lists").join(format!("{safe}.json"))
}

/// Read a provider's cached listing, if one is on disk and parseable. Returns
/// the rows (re-marked `live`, since they were a live listing when written)
/// and a human age label for the header. A missing/corrupt cache is simply
/// `None` — the cache is an accelerator, never a source of truth.
fn read_model_list_cache(data_dir: &Path, provider_id: &str) -> Option<(Vec<AddModelRow>, String)> {
    let text = std::fs::read_to_string(model_list_cache_path(data_dir, provider_id)).ok()?;
    let cached: CachedModelList = serde_json::from_str(&text).ok()?;
    if cached.models.is_empty() {
        return None;
    }
    let rows = cached
        .models
        .into_iter()
        .map(|row| AddModelRow {
            id: row.id,
            name: row.name,
            context_tokens: row.context_tokens,
            cost_per_1m_input_usd: row.cost_per_1m_input_usd,
            cost_per_1m_output_usd: row.cost_per_1m_output_usd,
            live: true,
        })
        .collect();
    Some((rows, cache_age_label(cached.fetched_at_unix, unix_now())))
}

/// Seconds since the Unix epoch, or `0` when the clock is before it (the same
/// dependency-free stamp the crash log uses).
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// A human age for a cached listing ("4m ago"). A stamp from the future (a
/// clock that moved backwards) reads as "just now" rather than as a negative
/// age — the label is a hint, and `Ctrl-R` is always available.
fn cache_age_label(fetched_at_unix: u64, now_unix: u64) -> String {
    let minutes = now_unix.saturating_sub(fetched_at_unix) / 60;
    match minutes {
        m if m < 1 => "just now".to_owned(),
        m if m < 60 => format!("{m}m ago"),
        m if m < 60 * 24 => format!("{}h ago", m / 60),
        m => format!("{}d ago", m / (60 * 24)),
    }
}

/// Persist a provider's live listing for the next add (instant seed). Failures
/// are ignored: a cache that cannot be written must never break the add flow.
fn write_model_list_cache(data_dir: &Path, provider_id: &str, rows: &[AddModelRow]) {
    let path = model_list_cache_path(data_dir, provider_id);
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let cached = CachedModelList {
        provider_id: provider_id.to_owned(),
        fetched_at_unix: unix_now(),
        models: rows
            .iter()
            .filter(|row| row.live)
            .map(|row| CachedModelRow {
                id: row.id.clone(),
                name: row.name.clone(),
                context_tokens: row.context_tokens,
                cost_per_1m_input_usd: row.cost_per_1m_input_usd,
                cost_per_1m_output_usd: row.cost_per_1m_output_usd,
            })
            .collect(),
    };
    if cached.models.is_empty() {
        return;
    }
    let Ok(rendered) = serde_json::to_string_pretty(&cached) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, rendered.as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// GET `<base_url>/models` for the add-model flow (model-discovery), applying the
/// provider's auth header only when a non-blank `api_key` is present (a keyless
/// OpenAI-compatible endpoint sends none). Bounded at 10s so a hung endpoint
/// can't wedge the query task. Non-2xx → `Err` with the STATUS ONLY (never the
/// key); the body is parsed defensively. Every returned `reason` is key-free and
/// URL-free (send errors map to fixed strings; the auth value is marked
/// sensitive so reqwest cannot echo it in any error). This is the only I/O in
/// the model-discovery feature; it runs on a spawned task off the UI thread.
async fn query_provider_models(
    base_url: &str,
    header: &str,
    prefix: &str,
    api_key: Option<&str>,
) -> Result<Vec<AddModelRow>, String> {
    let url = models_url(base_url);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| "could not build the HTTP client".to_string())?;
    let mut request = client.get(&url);
    if let Some(key) = api_key.filter(|k| !k.trim().is_empty()) {
        // Mark the auth value sensitive so reqwest redacts it from any error /
        // debug (mirrors the GitHub client). The key never appears in a reason.
        match reqwest::header::HeaderValue::from_str(&format!("{prefix}{key}")) {
            Ok(mut value) => {
                value.set_sensitive(true);
                request = request.header(header, value);
            }
            Err(_) => return Err("the API key is not a valid header value".to_string()),
        }
    }
    let response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            "request timed out".to_string()
        } else if error.is_connect() {
            "could not connect to the provider".to_string()
        } else {
            "the model-list request failed".to_string()
        }
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    let body = response
        .text()
        .await
        .map_err(|_| "could not read the response body".to_string())?;
    parse_models_response(&body)
}

/// The document a doc-editing intent operates on, when it is one the harness must
/// be subscribed to before sending (an edit needs to observe its own resulting
/// sync). A release needs no subscription.
fn doc_intent_target(intent: &Intent) -> Option<DocumentId> {
    match intent {
        Intent::AcquireDocumentLease { document_id, .. }
        | Intent::MutateDocument { document_id, .. }
        | Intent::PublishDocument { document_id, .. }
        | Intent::WatchDocument { document_id } => Some(*document_id),
        _ => None,
    }
}

/// Seed a document's client replica from its current persisted CRDT snapshot — the
/// document read path (Phase 4 STEP 4.3). Falls back to an empty replica when the
/// pool is absent or the read fails: an empty replica still converges on the first
/// full-snapshot sync, so editing degrades gracefully rather than breaking.
async fn seed_replica(pool: Option<&sqlx::SqlitePool>, document_id: DocumentId) -> DocumentReplica {
    if let Some(pool) = pool {
        match DocumentStore::new().load(pool, document_id).await {
            Ok(Some(document)) => match document.crdt.snapshot() {
                Ok(snapshot) => {
                    match DocumentReplica::from_snapshot(&snapshot, document.revision) {
                        Ok(replica) => return replica,
                        Err(error) => {
                            eprintln!("codypendent: could not seed a doc replica: {error}")
                        }
                    }
                }
                Err(error) => eprintln!("codypendent: could not read a doc snapshot: {error}"),
            },
            Ok(None) => {}
            Err(error) => eprintln!("codypendent: could not load a document to seed: {error}"),
        }
    }
    DocumentReplica::empty()
}

/// Merge one incoming [`DocumentSync`] into the document's replica and project the
/// result into a reducer action: the block-structured editor view (from the merged
/// replica) plus the review rail's pending suggestions (re-read from the store,
/// since a suggestion rides the DB, not the CRDT bytes). `None` when the merge or
/// projection fails.
async fn merge_document_sync(
    replicas: &mut HashMap<DocumentId, DocumentReplica>,
    pool: Option<&sqlx::SqlitePool>,
    sync: DocumentSync,
) -> Option<Action> {
    let document_id = sync.document_id;
    let replica = replicas
        .entry(document_id)
        .or_insert_with(DocumentReplica::empty);
    if let Err(error) = replica.merge(&sync) {
        eprintln!("codypendent: could not merge a document sync: {error}");
        return None;
    }
    let blocks: Vec<_> = match replica.blocks() {
        Ok(blocks) => blocks.iter().map(block_view).collect(),
        Err(error) => {
            eprintln!("codypendent: could not project a merged document: {error}");
            return None;
        }
    };
    let revision = format!("r{}", replica.revision());
    // Suggestions live in the DB (not the sync bytes); re-read them so a
    // just-proposed or just-resolved suggestion shows in the review rail.
    let suggestions = match pool {
        Some(pool) => SuggestionStore::new()
            .pending(pool, document_id)
            .await
            .map(|list| list.iter().map(suggestion_view).collect())
            .unwrap_or_default(),
        None => Vec::new(),
    };
    Some(Action::DocumentSynced {
        document_id,
        revision,
        blocks,
        suggestions,
    })
}

/// Wrap a command in a fresh, self-idempotent request envelope (the command id's
/// own string is the idempotency key, so a client-side retry reuses it — same
/// contract as [`Connection::send_command`](crate::connection::Connection)).
fn command_envelope(client_id: ClientId, body: CommandBody) -> Envelope {
    let command_id = CommandId::new();
    Envelope::request(
        client_id,
        Payload::Command(Command {
            command_id,
            idempotency_key: command_id.to_string(),
            expected_revision: None,
            body,
        }),
    )
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

/// A replacement transport prepared for an in-place fresh conversation. The
/// connection is already attached, so swapping it into the event loop cannot
/// expose an unattached or half-created UI state.
struct FreshLiveSession {
    session_id: SessionId,
    catchup: Catchup,
    pending: std::collections::VecDeque<Envelope>,
    resume_token: Option<String>,
    live: LiveIo,
}

/// Create a new durable session and attach the same new socket before returning.
/// The event loop keeps its old transport alive until this whole operation
/// succeeds; on success it atomically swaps transports and drops every
/// old-session forwarder with the old socket.
async fn create_fresh_session_live(
    paths: &RuntimePaths,
    repository: &str,
    workspace: WorkspaceId,
    subscriptions: &[Subscription],
    resume: Option<codypendent_protocol::ResumeToken>,
) -> anyhow::Result<FreshLiveSession> {
    let mut conn = Connection::connect(&paths.socket_path).await?;
    let hello = conn
        .handshake(
            "codypendent-tui-session-switch",
            codypendent_protocol::BUILD_ID,
            resume,
        )
        .await?;
    let title = Path::new(repository)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Codypendent".to_owned());
    let reply = conn
        .send_command(CommandBody::CreateSession {
            workspace,
            title,
            repository: Some(repository.to_owned()),
        })
        .await?;
    let session_id = match reply.payload {
        Payload::CommandAccepted { .. } => reply
            .session_id
            .ok_or_else(|| anyhow!("daemon accepted CreateSession without a session id")),
        Payload::CommandRejected(error) => {
            bail!("CreateSession rejected: {} ({})", error.message, error.code)
        }
        other => bail!("unexpected CreateSession reply: {other:?}"),
    }?;

    let attach = conn
        .send_command(CommandBody::AttachSession {
            session_id,
            last_seen_sequence: None,
            subscriptions: subscriptions.to_vec(),
            requested_role: ClientRole::Controller,
            repository: Some(repository.to_owned()),
        })
        .await?;
    let catchup = match attach.payload {
        Payload::Catchup { catchup } => catchup,
        Payload::CommandRejected(error) => {
            bail!(
                "fresh-session attach rejected: {} ({})",
                error.message,
                error.code
            )
        }
        Payload::Error(error) => {
            bail!(
                "fresh-session attach failed: {} ({})",
                error.message,
                error.code
            )
        }
        other => bail!("unexpected fresh-session attach reply: {other:?}"),
    };
    let (live, pending) = LiveIo::start(conn);
    Ok(FreshLiveSession {
        session_id,
        catchup,
        pending,
        resume_token: hello.resume_token.map(|token| token.0),
        live,
    })
}

/// Fold an attach-time [`Catchup`] into fresh state. `Catchup::Events` replays
/// each missed event through the reducer; `Catchup::Snapshot` (the client was
/// too far behind — Chapter 03's cap) and a future `Unknown` variant carry no
/// individual events, so the transcript simply begins from the next live event
/// (Phase 1 keeps runs short enough that this is rare).
fn fold_catchup(state: &mut AppState, catchup: Catchup) -> u64 {
    match catchup {
        Catchup::Events {
            events, through, ..
        } => {
            for event in events {
                reduce(state, Action::DaemonEvent(Box::new(event)));
            }
            through
        }
        // Too far behind for an event replay — fold the projection so a reopened
        // long-running session shows its title + active runs instead of blank.
        Catchup::Snapshot {
            through,
            projection,
        } => {
            reduce(
                state,
                Action::CatchupSnapshot {
                    title: projection.title,
                    closed: projection.closed,
                    runs: projection.active_runs,
                    pending_approvals: projection.pending_approvals,
                },
            );
            through
        }
        _ => 0,
    }
}

/// Resolve the session for `repo`, reusing the one this repo last used when it
/// still exists, otherwise creating a fresh one. Returns the session id and its
/// attach-time catch-up. This is what makes "close the TUI, reopen, the run
/// continued" work: the mapping persists across launches.
async fn resolve_or_create_session(
    conn: &mut Connection,
    store: &mut SessionStore,
    paths: &RuntimePaths,
    repo: &Path,
) -> anyhow::Result<(SessionId, WorkspaceId, Catchup)> {
    let key = repo.to_string_lossy().into_owned();

    // Try to resume the repo's remembered session.
    if let Some(stored) = store.sessions.get(&key).copied() {
        let reply = conn
            .send_command(CommandBody::AttachSession {
                session_id: stored.session_id,
                last_seen_sequence: None,
                subscriptions: default_subscriptions(),
                requested_role: ClientRole::Controller,
                // The canonical repo root, so the daemon can build its code
                // graph on open — not only on the first run.
                repository: Some(key.clone()),
            })
            .await?;
        // Only resume when the catch-up proves the session still exists. The
        // daemon can't tell an *absent* session from an empty one — it reports max
        // sequence 0 for both and replies with an empty `Catchup`, never a
        // rejection — but a real session always replays at least its
        // `SessionCreated` event (sequence 1). Accepting a zero-event catch-up
        // would open a blank TUI bound to a dead id whose every `StartRun` is then
        // rejected `session-not-found`; instead fall through and create a fresh
        // session, keeping the workspace. (issue #6 item 6)
        if let Payload::Catchup { catchup } = reply.payload {
            // A CLOSED session resumes technically (through > 0) but the event
            // loop exits the moment it folds `SessionClosed` — and with the
            // store never overwritten, every later launch would re-open the
            // closed session and instantly exit: a permanent lockout. Treat
            // closed like missing and fall through to create a fresh session.
            if catchup_proves_session_exists(&catchup) && !catchup_shows_closed(&catchup) {
                return Ok((stored.session_id, stored.workspace_id, catchup));
            }
        }
        // Rejected, or a zero-event catch-up to a session the daemon no longer has
        // (fresh data dir, GC'd, or closed): fall through and create a new one.
    }

    // Create a new session (reusing this repo's workspace id if we have one, so a
    // recreated session still belongs to the same logical workspace).
    let workspace = store
        .sessions
        .get(&key)
        .map(|s| s.workspace_id)
        .unwrap_or_default();
    let title = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| key.clone());

    let created = conn
        .send_command(CommandBody::CreateSession {
            workspace,
            title,
            // The canonical repo root, so the daemon can build its code graph
            // on open — not only on the first run.
            repository: Some(key.clone()),
        })
        .await?;
    let session_id = match &created.payload {
        Payload::CommandAccepted { .. } => created.session_id.ok_or_else(|| {
            anyhow!("daemon accepted CreateSession but its reply carried no session_id")
        })?,
        Payload::CommandRejected(error) => {
            bail!("CreateSession rejected: {} ({})", error.message, error.code)
        }
        other => bail!("unexpected reply to CreateSession: {other:?}"),
    };

    let attach = conn
        .send_command(CommandBody::AttachSession {
            session_id,
            last_seen_sequence: None,
            subscriptions: default_subscriptions(),
            requested_role: ClientRole::Controller,
            repository: Some(key.clone()),
        })
        .await?;
    let catchup = commands::expect_catchup(attach)?;

    store.sessions.insert(
        key,
        StoredSession {
            session_id,
            workspace_id: workspace,
        },
    );
    store.save(paths); // best-effort: a persistence miss only costs the next
                       // launch a fresh session, never correctness.
    Ok((session_id, workspace, catchup))
}

/// Whether an attach-time [`Catchup`] proves its session still exists in the
/// daemon. A live session always replays at least its `SessionCreated` event, so
/// its watermark is `>= 1`; the daemon reports `0` for an absent session (it
/// cannot distinguish "gone" from "empty"). An unrecognized future variant is
/// accepted rather than needlessly discarding a resumable session — the concrete
/// failure (issue #6 item 6) is specifically the provably-empty catch-up.
/// Whether an attach-time [`Catchup`] shows the session is already CLOSED — a
/// `SessionClosed` in the replayed events, or the snapshot's `closed` flag. A
/// closed session must not be resumed from the store: the event loop exits the
/// moment it folds the close, and the remembered mapping would re-open it on
/// every later launch (a permanent lockout).
fn catchup_shows_closed(catchup: &Catchup) -> bool {
    match catchup {
        Catchup::Events { events, .. } => events
            .iter()
            .any(|event| matches!(event.body, codypendent_protocol::EventBody::SessionClosed)),
        Catchup::Snapshot { projection, .. } => projection.closed,
        _ => false,
    }
}

fn catchup_proves_session_exists(catchup: &Catchup) -> bool {
    match catchup {
        Catchup::Events { through, .. } | Catchup::Snapshot { through, .. } => *through > 0,
        _ => true,
    }
}

/// The knowledge-fabric projections the TUI reads (STEP 2.6 + Phase 4 client
/// wiring), all loaded in the one place the two worlds meet.
struct KnowledgeProjections {
    skills: Vec<SkillCard>,
    memories: Vec<MemoryCard>,
    docs: Vec<DocCard>,
    edges: Vec<GraphEdgeCard>,
    edge_total: usize,
    blackboard: Vec<BlackboardItemCard>,
}

/// Read the knowledge fabric's registry, memories, documents, and code-graph
/// edges directly from SQLite and map them into the TUI's plain projection
/// structs (STEP 2.6 + Phase 4 client wiring). This is the CLI's job precisely
/// because the TUI crate performs no I/O and never depends on
/// `codypendent-knowledge`; the mapping from the knowledge domain types to the
/// projection structs happens here and nowhere else.
///
/// The database is opened via the same helper the `index rebuild` path uses; WAL
/// mode lets this read concurrently with the running daemon. Every failure path
/// (open, list, query) is swallowed into an empty list with a stderr note, so a
/// missing or busy database only means empty browsers — never a TUI that refuses
/// to start.
async fn load_knowledge(
    paths: &RuntimePaths,
    workspace_id: WorkspaceId,
    repo: &Path,
    warnings: &mut Vec<String>,
) -> KnowledgeProjections {
    let empty = || KnowledgeProjections {
        skills: Vec::new(),
        memories: Vec::new(),
        docs: Vec::new(),
        edges: Vec::new(),
        edge_total: 0,
        blackboard: Vec::new(),
    };

    let database_path = paths.data_dir.join("codypendent.db");
    let pool = match knowledge_db::open(&database_path).await {
        Ok(pool) => pool,
        Err(error) => {
            warnings.push(format!(
                "knowledge views unavailable (opening {}: {error})",
                database_path.display()
            ));
            return empty();
        }
    };

    let skills = match Registry::new().list(&pool).await {
        Ok(items) => items.iter().map(skill_card).collect(),
        Err(error) => {
            warnings.push(format!("could not list registry items: {error}"));
            Vec::new()
        }
    };

    // Visible scopes: the System tier, this session's workspace, and THIS
    // repository — where a run's harvested memories and documents live, derived
    // from the same canonical path the daemon uses. The stores enforce
    // cross-scope isolation in SQL; an empty result is fine.
    let repository = codypendent_knowledge::stable_repository_id(repo);
    let scopes = vec![
        Scope::System,
        Scope::Workspace(workspace_id),
        Scope::Repository(repository),
    ];
    let memories = match MemoryStore::new().query(&pool, &scopes, None).await {
        Ok(records) => records.iter().map(memory_card).collect(),
        Err(error) => {
            warnings.push(format!("could not query memories: {error}"));
            Vec::new()
        }
    };

    let docs = load_docs(&pool, &scopes, warnings).await;
    let (edges, edge_total, _) = load_edge_page(&pool, repository, "", 0, warnings).await;
    // Phase 5 STEP 5.3: the blackboard artifacts on the active workflow runs. The
    // workflow tables share this database (the migrations are workspace-wide), so
    // the same pool serves them; empty until a run posts artifacts.
    let blackboard = load_blackboard(&pool, warnings).await;

    pool.close().await;
    KnowledgeProjections {
        skills,
        memories,
        docs,
        edges,
        edge_total,
        blackboard,
    }
}

/// Seed the model-picker projection (MP1): every model configured in
/// `<data_dir>/models.toml` (the authoritative selectable list — STEP 1.9),
/// enriched with its measured profile from the `model_profiles` table
/// (migration 0014) when one exists, matched by `(id, base_url)` — a profile
/// row is keyed by `(model_id, endpoint)`, and `base_url` is a model's
/// endpoint. This is the CLI's job precisely because the TUI crate performs no
/// I/O and never depends on `codypendent-routing`; the mapping happens here
/// and nowhere else, exactly as [`load_knowledge`] maps the other browsers'
/// projections.
///
/// Never fails the TUI: a missing/unparsable `models.toml` degrades to an
/// empty picker (with a diagnostic in `warnings`); an unopenable database or a
/// profile-list failure degrades every model to its **id-only fallback**
/// (every badge absent) since profiles are best-effort enrichment, not the
/// selectable list itself.
async fn load_model_cards(paths: &RuntimePaths, warnings: &mut Vec<String>) -> Vec<ModelCard> {
    use codypendent_daemon::model_profiles::ModelProfileStore;
    use codypendent_runtime::models::load_models;

    let models_path = paths.data_dir.join("models.toml");
    let configs = match load_models(&models_path) {
        Ok(configs) => configs,
        Err(error) => {
            warnings.push(format!(
                "model picker unavailable (reading {}: {error})",
                models_path.display()
            ));
            return Vec::new();
        }
    };
    if configs.is_empty() {
        return Vec::new();
    }

    let database_path = paths.data_dir.join("codypendent.db");
    let pool = match knowledge_db::open(&database_path).await {
        Ok(pool) => Some(pool),
        Err(error) => {
            warnings.push(format!(
                "model profiles unavailable (opening {}: {error}); \
                 models still list, id-only",
                database_path.display()
            ));
            None
        }
    };

    let mut profiles: HashMap<(ModelId, String), codypendent_routing::ModelProfile> =
        HashMap::new();
    if let Some(pool) = &pool {
        match ModelProfileStore::new().list(pool).await {
            Ok(stored) => {
                for entry in stored {
                    profiles.insert(
                        (entry.profile.id.clone(), entry.endpoint.clone()),
                        entry.profile,
                    );
                }
            }
            Err(error) => {
                warnings.push(format!("could not list model profiles: {error}"));
            }
        }
    }
    if let Some(pool) = pool {
        pool.close().await;
    }

    let auth = codypendent_runtime::auth::AuthStore::load(&paths.data_dir).unwrap_or_default();
    let registry = codypendent_runtime::models::ModelRegistry::new(configs.clone()).with_auth(auth);
    let acp_store = codypendent_integrations::acp_registry::AcpRegistryStore::new(&paths.data_dir);
    let mut cards = Vec::with_capacity(configs.len());
    for config in configs {
        let local = local_model_endpoint(&config.base_url);
        let readiness = if config.provider == "acp" {
            match acp_store.launch_spec(&config.model) {
                Ok(_) => ModelReadiness::Ready,
                Err(error) => ModelReadiness::Unavailable(error.to_string()),
            }
        } else if config.base_url.trim().is_empty() {
            ModelReadiness::Unavailable("base URL is missing".to_owned())
        } else if local {
            match registry.check_model(&config.id).await {
                Ok(()) => ModelReadiness::Ready,
                Err(error) => ModelReadiness::Unavailable(error.to_string()),
            }
        } else {
            ModelReadiness::Unverified
        };
        cards.push(model_card(config, &profiles, readiness, local));
    }
    cards
}

fn local_model_endpoint(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    ["localhost", "127.0.0.1", "0.0.0.0", "[::1]", "::1"]
        .iter()
        .any(|host| lower.contains(host))
}

/// Map one configured [`ModelConfig`](codypendent_runtime::models::ModelConfig)
/// into a [`ModelCard`], enriching it with its measured profile (matched by
/// `(id, base_url)`) when `profiles` has one — an id-only fallback (every
/// badge `None`) otherwise, so an unprofiled model still selects, just without
/// badges.
fn model_card(
    config: codypendent_runtime::models::ModelConfig,
    profiles: &HashMap<(ModelId, String), codypendent_routing::ModelProfile>,
    readiness: ModelReadiness,
    local: bool,
) -> ModelCard {
    let profile = profiles.get(&(config.id.clone(), config.base_url.clone()));
    let configured_context = config.context_tokens;
    ModelCard {
        id: config.id,
        provider: config.provider,
        readiness,
        location: Some(profile.map_or_else(
            || {
                if local {
                    ModelLocationLabel::Local
                } else {
                    ModelLocationLabel::Hosted
                }
            },
            |profile| {
                if profile.is_local() {
                    ModelLocationLabel::Local
                } else {
                    ModelLocationLabel::Hosted
                }
            },
        )),
        cost_per_1k_usd: profile.map(|profile| profile.performance.cost_per_1k_tokens_usd),
        // A measured profile is the freshest source, but an explicit
        // `models.toml` value is authoritative when no profile exists. The old
        // projection discarded that configured value and showed `—` even while
        // the runtime was correctly using it.
        context_tokens: profile
            .and_then(|profile| profile.capabilities.context_tokens)
            .or(configured_context),
    }
}

/// Seed the provider-catalog projection for the `/provider` picker (Task 8):
/// the built-in ~40-provider catalog layered with the user's
/// `<data_dir>/providers.toml`, exactly as [`load_model_cards`] maps
/// `models.toml` into [`ModelCard`]s. This is the CLI's job precisely because
/// the TUI crate performs no I/O and never depends on `codypendent-providers`.
///
/// Never fails the TUI: a missing user `providers.toml` is fine (the loader
/// treats it as absent and returns the built-ins); a *malformed* one degrades
/// to the built-ins alone, with a diagnostic in `warnings`.
async fn load_provider_cards(
    paths: &RuntimePaths,
    warnings: &mut Vec<String>,
) -> Vec<ProviderCard> {
    use codypendent_integrations::acp_registry::AcpRegistryStore;
    use codypendent_providers::{AuthMethod, Catalog};

    let providers_path = paths.data_dir.join("providers.toml");
    let catalog = match Catalog::load_with_user_overrides(&providers_path) {
        Ok(catalog) => catalog,
        Err(error) => {
            warnings.push(format!("provider catalog fell back to built-ins ({error})"));
            Catalog::builtin()
        }
    };
    // How many curated models the catalog ships per provider, counted once
    // rather than re-scanning for every card.
    let mut catalog_model_counts: HashMap<&str, usize> = HashMap::new();
    for model in catalog.models() {
        *catalog_model_counts
            .entry(model.provider_id.as_str())
            .or_default() += 1;
    }
    let auth = codypendent_runtime::auth::AuthStore::load(&paths.data_dir).unwrap_or_default();
    let mut cards: Vec<_> = catalog
        .providers()
        // ACP entries come from the official live registry below. Keeping the
        // five historical built-ins as well would show stale duplicate adapters.
        .filter(|p| !matches!(p.protocol, codypendent_providers::Protocol::Acp))
        .map(|p| ProviderCard {
            id: p.id.clone(),
            name: p.name.clone(),
            protocol: protocol_label(p.protocol).to_owned(),
            auth: match p.auth.first() {
                None | Some(AuthMethod::None) => "none".to_string(),
                Some(AuthMethod::ApiKey { env, .. }) => {
                    format!("api-key: {}", env.first().map(String::as_str).unwrap_or(""))
                }
                Some(AuthMethod::Acp { command, .. }) => format!("acp: {command}"),
                Some(AuthMethod::CloudIam { variant, .. }) => format!("cloud-iam: {variant}"),
                Some(AuthMethod::OAuth { .. }) => "oauth".to_string(),
                // `AuthMethod` is `#[non_exhaustive]`: a future variant this
                // build does not understand still renders (protocol RULE 1),
                // rather than failing to compile or panicking.
                Some(_) => "unknown".to_string(),
            },
            local: p.local,
            // Adding a model from this provider needs a key iff its first auth
            // method is an API key (local/none/acp/cloud-iam/oauth skip the key
            // step). Extracted to `provider_requires_key` so this derivation has
            // an isolated unit test against the real `AuthMethod` enum, rather
            // than only being exercised indirectly through this I/O function.
            requires_key: provider_requires_key(p),
            // Whether the add-model flow can offer a live `/models` pick-list for
            // this provider (OpenAiChat + base_url + ApiKey/None), vs. the
            // free-text fallback. Extracted to `provider_can_list_models`, unit
            // tested against the real enums.
            can_list_models: provider_can_list_models(p),
            available: provider_runtime_supported(p),
            // The curated `[[model]]` rows this provider ships: what the add
            // flow can offer with no network at all.
            catalog_models: catalog_model_counts
                .get(p.id.as_str())
                .copied()
                .unwrap_or_default(),
            // A provider-wide key already in `auth.json` means the add flow
            // can skip straight to the pick-list.
            has_key: auth
                .get(&provider_auth_id(&p.id))
                .is_some_and(|key| !key.is_empty()),
        })
        .collect();
    let acp_store = AcpRegistryStore::new(&paths.data_dir);
    let acp_registry = match acp_store.load_or_refresh().await {
        Ok(registry) => Some(registry),
        Err(error) => {
            warnings.push(format!("official ACP registry unavailable ({error})"));
            None
        }
    };
    if let Some(registry) = acp_registry {
        cards.extend(registry.agents.iter().map(|agent| {
            let distribution = if agent.distribution.npx.is_some() {
                "npx"
            } else if agent.distribution.uvx.is_some() {
                "uvx"
            } else {
                "binary"
            };
            let binary_installable = agent
                .distribution
                .binary
                .get(codypendent_integrations::acp_registry::current_platform())
                .is_some_and(|binary| binary.sha256.is_some());
            ProviderCard {
                id: agent.id.clone(),
                name: agent.name.clone(),
                protocol: "acp".to_string(),
                auth: format!("acp: {distribution} · {}", agent.version),
                local: false,
                requires_key: false,
                can_list_models: false,
                // Verified platform binaries can be installed in the
                // background when selected. Package entries are selectable
                // only when their runner is actually present.
                available: acp_store.launch_spec(&agent.id).is_ok() || binary_installable,
                // An ACP agent owns its own model; there is nothing for the
                // catalog to curate and no provider key to hold.
                catalog_models: 0,
                has_key: false,
            }
        }));
    }
    if let Some(spec) = codypendent_integrations::acp_registry::local_kimi_code_spec() {
        if !cards.iter().any(|card| card.id == spec.registry_id) {
            cards.push(ProviderCard {
                id: spec.registry_id,
                name: spec.name,
                protocol: "acp".to_string(),
                auth: format!("acp: local · {}", spec.version),
                local: true,
                requires_key: false,
                can_list_models: false,
                available: true,
                catalog_models: 0,
                has_key: false,
            });
        }
    }
    // Put usable providers first, with local endpoints before hosted ones. The
    // complete catalog remains searchable below them as an honest preview.
    cards.sort_by(|a, b| {
        b.available
            .cmp(&a.available)
            .then_with(|| b.local.cmp(&a.local))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    cards
}

/// Whether adding a model from `p` needs an API key: its first configured auth
/// method is `AuthMethod::ApiKey` (a local/no-auth/ACP/cloud-iam/OAuth provider,
/// or one with no auth methods at all, skips the key step). A tiny pure
/// expression — no I/O — extracted out of [`load_provider_cards`] so this
/// bool derivation is directly unit-testable against the real
/// `codypendent_providers::AuthMethod` enum, independent of that function's
/// file I/O.
fn provider_requires_key(p: &codypendent_providers::Provider) -> bool {
    use codypendent_providers::AuthMethod;
    matches!(p.auth.first(), Some(AuthMethod::ApiKey { .. }))
}

/// Whether adding a model from `p` can use a live `/models` list: the protocol
/// is OpenAI-compatible (`OpenAiChat`), a non-blank `base_url` is set, and the
/// first auth method is `ApiKey` or `None` (or there is none at all). Native
/// (Anthropic/Gemini), ACP, cloud-IAM, and OAuth providers — and any without a
/// `base_url` — cannot list here and take the free-text path. A tiny pure
/// expression (no I/O), extracted out of `load_provider_cards` so it is directly
/// unit-testable against the real `codypendent_providers` enums.
fn provider_can_list_models(p: &codypendent_providers::Provider) -> bool {
    use codypendent_providers::{AuthMethod, Protocol};
    matches!(p.protocol, Protocol::OpenAiChat)
        && p.base_url.as_deref().is_some_and(|u| !u.trim().is_empty())
        && matches!(
            p.auth.first(),
            Some(AuthMethod::ApiKey { .. } | AuthMethod::None) | None
        )
}

/// The provider shapes today's runtime can execute. Keeping this separate from
/// catalog visibility prevents native/ACP/cloud-auth cards from producing an
/// apparently valid `openai-compatible` model entry that can only fail later.
fn provider_runtime_supported(p: &codypendent_providers::Provider) -> bool {
    provider_can_list_models(p)
}

/// The provider picker's wire-protocol label — the same kebab-case spelling
/// `codypendent_providers::model::Protocol`'s `Serialize` impl emits (e.g.
/// `"openai-chat"`), spelled out explicitly here rather than derived from
/// `{:?}` because `Debug` prints the Rust identifier (`"OpenAiChat"`), not the
/// wire spelling. `Protocol` is `#[non_exhaustive]`, so a future variant this
/// build does not understand still renders as `"unknown"` rather than
/// failing to compile.
fn protocol_label(protocol: codypendent_providers::Protocol) -> &'static str {
    use codypendent_providers::Protocol;
    match protocol {
        Protocol::OpenAiChat => "openai-chat",
        Protocol::Anthropic => "anthropic",
        Protocol::GeminiNative => "gemini-native",
        Protocol::Acp => "acp",
        _ => "unknown",
    }
}

/// Project each visible-scope document (snapshot + pending suggestions) into a
/// [`DocCard`]. A per-document read failure collects a diagnostic and skips
/// that document; the browser degrades to what it could load rather than
/// failing.
async fn load_docs(
    pool: &sqlx::SqlitePool,
    scopes: &[Scope],
    warnings: &mut Vec<String>,
) -> Vec<DocCard> {
    let doc_store = DocumentStore::new();
    let suggestion_store = SuggestionStore::new();
    let summaries = match doc_store.list(pool, scopes).await {
        Ok(summaries) => summaries,
        Err(error) => {
            warnings.push(format!("could not list documents: {error}"));
            return Vec::new();
        }
    };

    let mut docs = Vec::with_capacity(summaries.len());
    for summary in summaries {
        let document = match doc_store.snapshot_document(pool, summary.id).await {
            Ok(Some(document)) => document,
            Ok(None) => continue,
            Err(error) => {
                warnings.push(format!("could not load document {}: {error}", summary.id));
                continue;
            }
        };
        let suggestions = suggestion_store
            .pending(pool, summary.id)
            .await
            .unwrap_or_else(|error| {
                warnings.push(format!(
                    "could not load suggestions for {}: {error}",
                    summary.id
                ));
                Vec::new()
            });
        docs.push(doc_card(&document, &suggestions));
    }
    docs
}

/// Read one filtered graph page with endpoint names joined in SQLite. This never
/// materializes the repository's full node/edge sets: a 50k-edge checkout costs
/// one `COUNT` plus at most [`EDGE_PAGE_SIZE`] joined rows.
async fn load_edge_page(
    pool: &sqlx::SqlitePool,
    repository: RepositoryId,
    query: &str,
    requested_page: usize,
    warnings: &mut Vec<String>,
) -> (Vec<GraphEdgeCard>, usize, usize) {
    let repository = repository.to_string();
    let query = query.trim();
    let pattern = format!(
        "%{}%",
        query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let base = " FROM code_edges e \
                JOIN code_nodes f ON f.id = e.from_node \
                JOIN code_nodes t ON t.id = e.to_node \
                WHERE f.repository = ? AND t.repository = ?";
    let filter = " AND (f.qualified_name LIKE ? ESCAPE '\\' \
                         OR t.qualified_name LIKE ? ESCAPE '\\' \
                         OR e.relation LIKE ? ESCAPE '\\' \
                         OR e.evidence_kind LIKE ? ESCAPE '\\')";

    let total_result = if query.is_empty() {
        sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*){base}"))
            .bind(&repository)
            .bind(&repository)
            .fetch_one(pool)
            .await
    } else {
        sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*){base}{filter}"))
            .bind(&repository)
            .bind(&repository)
            .bind(&pattern)
            .bind(&pattern)
            .bind(&pattern)
            .bind(&pattern)
            .fetch_one(pool)
            .await
    };
    let total = match total_result {
        Ok(total) => usize::try_from(total.max(0)).unwrap_or(usize::MAX),
        Err(error) => {
            warnings.push(format!("could not count code-graph edges: {error}"));
            return (Vec::new(), 0, 0);
        }
    };
    let max_page = total.saturating_sub(1) / EDGE_PAGE_SIZE;
    let page = requested_page.min(max_page);
    let select = format!(
        "SELECT f.qualified_name AS from_name, t.qualified_name AS to_name, \
                e.relation, e.confidence, e.evidence_kind, e.evidence_artifact, e.revision \
         {base}{} \
         ORDER BY f.qualified_name COLLATE NOCASE, t.qualified_name COLLATE NOCASE, \
                  e.relation, e.id LIMIT ? OFFSET ?",
        if query.is_empty() { "" } else { filter }
    );
    let mut rows_query = sqlx::query(&select).bind(&repository).bind(&repository);
    if !query.is_empty() {
        rows_query = rows_query
            .bind(&pattern)
            .bind(&pattern)
            .bind(&pattern)
            .bind(&pattern);
    }
    rows_query = rows_query
        .bind(i64::try_from(EDGE_PAGE_SIZE).unwrap_or(i64::MAX))
        .bind(i64::try_from(page * EDGE_PAGE_SIZE).unwrap_or(i64::MAX));
    let rows = match rows_query.fetch_all(pool).await {
        Ok(rows) => rows,
        Err(error) => {
            warnings.push(format!("could not load code-graph page: {error}"));
            return (Vec::new(), total, page);
        }
    };

    let cards = rows
        .into_iter()
        .map(|row| {
            let evidence_json: Option<String> = row.get("evidence_artifact");
            let evidence = evidence_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<EvidenceRef>(json).ok())
                .as_ref()
                .map_or_else(|| "(none)".to_owned(), evidence_source);
            GraphEdgeCard {
                from: row.get("from_name"),
                to: row.get("to_name"),
                relation: row.get::<String, _>("relation").replace('_', "-"),
                confidence: row.get::<f64, _>("confidence") as f32,
                evidence_kind: row.get("evidence_kind"),
                evidence,
                revision: row.get("revision"),
            }
        })
        .collect();
    (cards, total, page)
}

/// Map a governed [`RegistryItem`] into the TUI's [`SkillCard`] projection,
/// rendering each requested capability **verbatim** (STEP 2.6 "skill permissions
/// are visible").
fn skill_card(item: &RegistryItem) -> SkillCard {
    SkillCard {
        name: item.name.clone(),
        kind: registry_kind_label(item.kind).to_owned(),
        scope: scope_label(&item.scope),
        trust: trust_label(item.trust.tier).to_owned(),
        status: status_label(item.status).to_owned(),
        risk: risk_label(item.risk).to_owned(),
        description: item.description.clone(),
        permissions: item.permissions.iter().map(capability_verbatim).collect(),
    }
}

/// Map a [`MemoryRecord`] into the TUI's [`MemoryCard`] projection. `source` is a
/// human rendering of the record's evidence refs (joined when there are several),
/// which the memory browser's "open source" affordance surfaces in full.
fn memory_card(record: &MemoryRecord) -> MemoryCard {
    let source = if record.provenance.is_empty() {
        "(no evidence)".to_owned()
    } else {
        record
            .provenance
            .iter()
            .map(evidence_source)
            .collect::<Vec<_>>()
            .join("; ")
    };
    MemoryCard {
        statement: record.statement.clone(),
        class: memory_class_label(record.class).to_owned(),
        scope: scope_label(&record.scope),
        revision: record.valid_from.0.clone(),
        observed: record.observed_at.date_naive().to_string(),
        confidence: record.confidence,
        source,
    }
}

/// Render one requested capability exactly as declared, e.g.
/// `"filesystem_read: $REPOSITORY"` or `"command: cargo"` — the verbatim form the
/// Skill Studio shows.
fn capability_verbatim(capability: &CapabilityRequest) -> String {
    match capability {
        CapabilityRequest::FilesystemRead(value) => format!("filesystem_read: {value}"),
        CapabilityRequest::FilesystemWrite(value) => format!("filesystem_write: {value}"),
        CapabilityRequest::Command(value) => format!("command: {value}"),
        CapabilityRequest::Network(value) => format!("network: {value}"),
        CapabilityRequest::Secret(value) => format!("secret: {value}"),
    }
}

/// A human rendering of a memory's evidence ref (what "open source" reveals).
fn evidence_source(evidence: &EvidenceRef) -> String {
    match evidence {
        EvidenceRef::EventRange {
            session_id,
            from_sequence,
            to_sequence,
        } => format!("events {from_sequence}..{to_sequence} of session {session_id}"),
        EvidenceRef::Artifact {
            artifact,
            source_path,
        } => match source_path {
            Some(path) => format!("artifact {} ({path})", artifact.id),
            None => format!("artifact {}", artifact.id),
        },
    }
}

/// A compact human label for a memory/registry [`Scope`]: the tier, plus a short
/// prefix of its key for the id-bearing tiers (the full UUID is noise in a card).
fn scope_label(scope: &Scope) -> String {
    match scope.key() {
        Some(key) => format!(
            "{} {}",
            scope.tier(),
            key.chars().take(8).collect::<String>()
        ),
        None => scope.tier().to_owned(),
    }
}

fn registry_kind_label(kind: RegistryItemKind) -> &'static str {
    match kind {
        RegistryItemKind::Tool => "tool",
        RegistryItemKind::Skill => "skill",
        RegistryItemKind::Plugin => "plugin",
        RegistryItemKind::Hook => "hook",
        RegistryItemKind::Command => "command",
    }
}

fn trust_label(tier: TrustTier) -> &'static str {
    match tier {
        TrustTier::Untrusted => "untrusted",
        TrustTier::Community => "community",
        TrustTier::Verified => "verified",
        TrustTier::FirstParty => "first-party",
    }
}

fn status_label(status: RegistryStatus) -> &'static str {
    match status {
        RegistryStatus::Draft => "draft",
        RegistryStatus::Active => "active",
        RegistryStatus::Modified => "modified",
        RegistryStatus::Deprecated => "deprecated",
    }
}

fn risk_label(risk: RiskClass) -> &'static str {
    match risk {
        RiskClass::Safe => "safe",
        RiskClass::Low => "low",
        RiskClass::Medium => "medium",
        RiskClass::High => "high",
    }
}

fn memory_class_label(class: MemoryClass) -> &'static str {
    match class {
        MemoryClass::Working => "working",
        MemoryClass::Episodic => "episodic",
        MemoryClass::Semantic => "semantic",
        MemoryClass::Procedural => "procedural",
        MemoryClass::Preference => "preference",
        MemoryClass::Failure => "failure",
        MemoryClass::Artifact => "artifact",
        MemoryClass::Code => "code",
    }
}

/// Map a [`KnowledgeDocument`] (plus its pending suggestions) into the TUI's
/// [`DocCard`] projection. `mode` is the collaboration mode the document's scope
/// defaults to — org-scope docs read `suggest`, the suggest-by-default the
/// engine enforces (STEP 4.3).
fn doc_card(document: &KnowledgeDocument, suggestions: &[Suggestion]) -> DocCard {
    DocCard {
        document_id: document.id,
        title: document.title.clone(),
        scope: scope_label(&document.scope),
        status: document.status.as_str().to_owned(),
        mode: collab_mode_label(CollaborationMode::default_for_scope(&document.scope)).to_owned(),
        revision: format!("r{}", document.revision),
        blocks: document.blocks.iter().map(block_view).collect(),
        suggestions: suggestions.iter().map(suggestion_view).collect(),
    }
}

/// Render one [`DocumentBlock`] into the editor rail's [`DocBlockView`]: a kind
/// label and a single-line human rendering of its content (never the raw
/// serialized block). Structured/embed blocks get a compact stand-in.
fn block_view(block: &DocumentBlock) -> DocBlockView {
    let (kind, text) = match &block.content {
        BlockContent::Heading { level, text } => (format!("heading h{level}"), text.clone()),
        BlockContent::Paragraph { text } => ("paragraph".to_owned(), text.clone()),
        BlockContent::Code { language, text } => (
            match language {
                Some(language) => format!("code {language}"),
                None => "code".to_owned(),
            },
            text.clone(),
        ),
        BlockContent::Diagram { format, .. } => {
            (format!("diagram {format}"), "(diagram)".to_owned())
        }
        BlockContent::Table { rows } => ("table".to_owned(), format!("({} rows)", rows.len())),
        BlockContent::Callout { kind, text } => (format!("callout {kind}"), text.clone()),
        BlockContent::Checklist { items } => {
            ("checklist".to_owned(), format!("({} items)", items.len()))
        }
        BlockContent::Query { query } => ("query".to_owned(), query.clone()),
        BlockContent::EmbeddedFile { path } => ("embed-file".to_owned(), path.clone()),
        BlockContent::EmbeddedSymbol { symbol } => ("embed-symbol".to_owned(), symbol.clone()),
        BlockContent::EmbeddedWorkflow { workflow } => {
            ("embed-workflow".to_owned(), workflow.clone())
        }
        BlockContent::EmbeddedSkill { skill } => ("embed-skill".to_owned(), skill.clone()),
    };
    // Collapse to one line — the editor rail renders a block per row.
    DocBlockView {
        id: block.id.clone(),
        kind,
        text: text.replace('\n', " "),
    }
}

/// Map a [`Suggestion`] into the review rail's [`DocSuggestionView`].
fn suggestion_view(suggestion: &Suggestion) -> DocSuggestionView {
    DocSuggestionView {
        id: suggestion.id.clone(),
        status: suggestion_status_label(suggestion.status).to_owned(),
        author: document_author_label(&suggestion.author),
        range: format!("{}..{}", suggestion.range_start, suggestion.range_end),
        replacement: suggestion.replacement.clone(),
        rationale: suggestion.rationale.clone(),
    }
}

/// The cap on workflow nodes surfaced in the view. A pathological manifest set
/// could declare a very large graph; the read-only view shows the first
/// [`MAX_WORKFLOW_NODES`] (in discovery + topological order) and logs when it
/// truncates — never a silent cut.
const MAX_WORKFLOW_NODES: usize = 500;

/// Compile the repository's declared workflow manifests
/// (`.codypendent/workflows/*.{yaml,yml}`) and project each compiled node into a
/// [`WorkflowNodeCard`] for the workflow-graph view (Phase 5 STEP 5.2). This is
/// the CLI's job precisely because the TUI crate performs no I/O and never
/// depends on `codypendent-workflow`; the mapping from the compiled graph to the
/// projection happens here and nowhere else.
///
/// Manifests are compiled in sorted filename order for a deterministic view; an
/// unreadable or non-compiling manifest logs to stderr and is skipped, so one
/// broken file drops only its own workflow — never the others, and never the
/// TUI. Nodes keep their compiled topological order.
///
/// When `pool` is present, each workflow's LATEST durable run overlays its
/// per-node live state, MEASURED cost, and failure/block reason (Phase 5 T8 /
/// P5-D4) onto the compiled defaults; `None` (or no run yet) shows the pre-run
/// values (`pending` / `—`). Read failures degrade to the compiled view, never
/// fail the TUI.
async fn load_workflows(
    repo: &Path,
    user_dir: Option<&Path>,
    pool: Option<&sqlx::SqlitePool>,
    warnings: &mut Vec<String>,
) -> Vec<WorkflowNodeCard> {
    use codypendent_workflow::{parse_definition, WorkflowSourceRegistry, REPAIR_GITHUB_CHECK_ID};

    let repository_dir = repo.join(".codypendent").join("workflows");
    let registry = WorkflowSourceRegistry::load(user_dir, Some(&repository_dir));
    let mut ids = BTreeSet::from([REPAIR_GITHUB_CHECK_ID.to_owned()]);

    // Discover the user/repository ids while preserving useful diagnostics for
    // malformed siblings. Resolution stays with WorkflowSourceRegistry so its
    // precedence and same-version collision rules remain authoritative.
    for dir in user_dir
        .into_iter()
        .chain(std::iter::once(repository_dir.as_path()))
    {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                warnings.push(format!(
                    "could not read workflows in {}: {error}",
                    dir.display()
                ));
                continue;
            }
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                matches!(
                    path.extension().and_then(|ext| ext.to_str()),
                    Some("yaml" | "yml")
                )
            })
            .collect();
        files.sort();
        for path in files {
            match std::fs::read_to_string(&path) {
                Ok(yaml) => match parse_definition(&yaml) {
                    Ok(definition) => {
                        ids.insert(definition.id);
                    }
                    Err(error) => warnings.push(format!(
                        "skipping workflow {} (does not parse: {error})",
                        path.display()
                    )),
                },
                Err(error) => {
                    warnings.push(format!("skipping workflow {} ({error})", path.display()))
                }
            }
        }
    }

    let mut cards = Vec::new();
    for workflow_id in ids {
        let yaml = match registry.resolve(&workflow_id) {
            Ok(yaml) => yaml,
            Err(error) => {
                warnings.push(format!("skipping workflow {workflow_id} ({error})"));
                continue;
            }
        };
        match codypendent_workflow::compile_yaml(yaml) {
            Ok(compiled) => {
                let label = format!("{} v{}", compiled.id, compiled.version);
                let inputs = if compiled.inputs.is_empty() {
                    "\u{2014}".to_owned()
                } else {
                    compiled
                        .inputs
                        .iter()
                        .map(|(name, input)| {
                            format!(
                                "{}:{}{}",
                                name,
                                input.input_type,
                                if input.required { "*" } else { "" }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                // Overlay the latest durable run's per-node state/cost/error, when
                // one exists and a pool is available.
                let latest = match pool {
                    Some(pool) => latest_workflow_run(pool, &compiled.id).await,
                    None => None,
                };
                cards.extend(compiled.nodes.iter().map(|node| {
                    workflow_node_card(&compiled.id, &label, &inputs, latest.as_ref(), node)
                }));
            }
            Err(error) => {
                warnings.push(format!(
                    "skipping workflow {workflow_id} (does not compile: {error})"
                ));
            }
        }
    }

    if cards.len() > MAX_WORKFLOW_NODES {
        warnings.push(format!(
            "workflow view showing the first {MAX_WORKFLOW_NODES} of {} nodes",
            cards.len()
        ));
        cards.truncate(MAX_WORKFLOW_NODES);
    }
    cards
}

/// The most recent durable run's node records for `workflow_id`, keyed by node id
/// — the overlay [`load_workflows`] applies so the graph view shows a run's live
/// state, MEASURED cost, and failure/block reason. An empty map when no run exists
/// or a read fails: the view then shows the compiled defaults, never a stale one.
struct LatestWorkflowRun {
    id: String,
    phase: String,
    nodes: HashMap<String, codypendent_workflow::WorkflowNodeRecord>,
}

async fn latest_workflow_run(
    pool: &sqlx::SqlitePool,
    workflow_id: &str,
) -> Option<LatestWorkflowRun> {
    let run_id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM workflow_runs WHERE workflow_id = ? \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(workflow_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let run_id = run_id?;
    match codypendent_workflow::WorkflowStore::new()
        .snapshot(pool, &run_id)
        .await
    {
        Ok(Some(snapshot)) => Some(LatestWorkflowRun {
            id: snapshot.run.id,
            phase: snapshot.run.state.as_str().to_owned(),
            nodes: snapshot
                .nodes
                .into_iter()
                .map(|node| (node.node_id.clone(), node))
                .collect(),
        }),
        _ => None,
    }
}

/// Render a node's MEASURED cost JSON into a human string for the graph view
/// (Phase 5 T8). Only the dimensions actually measured are shown — wall time and
/// tool calls — so the column never displays a fabricated token/USD figure. An
/// empty or unrecognized cost shape renders `"—"` (nothing was measured).
fn render_node_cost(cost: &serde_json::Value) -> String {
    let mut parts = Vec::new();
    if let Some(secs) = cost
        .get("wall_time_secs")
        .and_then(serde_json::Value::as_u64)
    {
        parts.push(format!("{secs}s"));
    }
    if let Some(calls) = cost.get("tool_calls").and_then(serde_json::Value::as_u64) {
        parts.push(format!(
            "{calls} tool call{}",
            if calls == 1 { "" } else { "s" }
        ));
    }
    if parts.is_empty() {
        "\u{2014}".to_owned()
    } else {
        parts.join(" \u{b7} ")
    }
}

/// Map one [`CompiledNode`](codypendent_workflow::CompiledNode) into the view's
/// [`WorkflowNodeCard`], pre-rendering every field to a human string. `workflow`
/// is the owning workflow's `id vN` label the view groups by.
///
/// `record` is the node's durable run record when a run of this workflow exists:
/// its live state, MEASURED cost (T8), and failure/block reason (P5-D4) overlay
/// the compiled node's pre-run defaults (`pending` / `—` / `—`). This is the seam
/// that turns the forever-`—` cost column and the reasonless `failed`/`blocked`
/// node into real values.
fn workflow_node_card(
    workflow_id: &str,
    workflow: &str,
    inputs: &str,
    latest: Option<&LatestWorkflowRun>,
    node: &codypendent_workflow::CompiledNode,
) -> WorkflowNodeCard {
    use codypendent_workflow::{ApprovalPolicy, NodeAction, WorkspaceMode};

    let dash = || "\u{2014}".to_owned(); // "—"

    // Live state / measured cost / failure reason from a durable run record, else
    // the compiled node's pre-run defaults.
    let record = latest.and_then(|run| run.nodes.get(&node.id));
    let state = record.map_or_else(
        || "pending".to_owned(),
        |record| record.state.as_str().to_owned(),
    );
    let cost = record
        .and_then(|record| record.cost.as_ref())
        .map_or_else(dash, render_node_cost);
    let error = record
        .and_then(|record| record.error.clone())
        .unwrap_or_else(dash);
    let (kind, action, agent, model_policy) = match &node.action {
        NodeAction::Agent {
            role,
            model_policy,
            skill,
        } => {
            let action = match skill {
                Some(skill) => format!("agent {role} \u{b7} skill {skill}"),
                None => format!("agent {role}"),
            };
            (
                "agent".to_owned(),
                action,
                role.clone(),
                model_policy.clone().unwrap_or_else(dash),
            )
        }
        NodeAction::Tool { name } => ("tool".to_owned(), format!("tool {name}"), dash(), dash()),
    };

    let workspace = match node.workspace_mode {
        WorkspaceMode::SharedWorktree => "shared worktree",
        WorkspaceMode::IsolatedWorktree => "isolated worktree",
    }
    .to_owned();

    let approval = match node.approval {
        Some(ApprovalPolicy::BeforeWrite) => "before write".to_owned(),
        Some(ApprovalPolicy::Always) => "always".to_owned(),
        None => "none".to_owned(),
    };

    let retry = {
        let attempts = node.retry.attempts;
        let unit = if attempts == 1 { "attempt" } else { "attempts" };
        if node.retry.backoff_seconds == 0 {
            format!("{attempts} {unit}")
        } else {
            format!(
                "{attempts} {unit} \u{b7} {}s backoff",
                node.retry.backoff_seconds
            )
        }
    };

    let join = |items: &[String]| {
        if items.is_empty() {
            dash()
        } else {
            items.join(", ")
        }
    };

    WorkflowNodeCard {
        workflow_id: workflow_id.to_owned(),
        workflow: workflow.to_owned(),
        workflow_run_id: latest.map(|run| run.id.clone()),
        run_phase: latest.map_or_else(|| "not started".to_owned(), |run| run.phase.clone()),
        inputs: inputs.to_owned(),
        id: node.id.clone(),
        action,
        kind,
        state,
        agent,
        model_policy,
        workspace,
        approval,
        retry,
        depends_on: join(&node.depends_on),
        outputs: join(&node.outputs),
        cost,
        error,
    }
}

/// The cap on blackboard artifacts surfaced in the view — a long-running board
/// can accumulate many; the read-only view shows the first [`MAX_BLACKBOARD_ITEMS`]
/// (newest first, across the active runs) and logs when it truncates.
const MAX_BLACKBOARD_ITEMS: usize = 500;

/// Project the blackboard artifacts on the active workflow runs into
/// [`BlackboardItemCard`]s (Phase 5 STEP 5.3). The workflow tables share the
/// knowledge database (the migrations are workspace-wide), so the same pool
/// serves them. Runs are the daemon's non-terminal set (the boards worth
/// watching); each run's full board — live and superseded — is queried so the
/// view can dim corrected artifacts. Empty until the executor posts artifacts; a
/// query failure collects a diagnostic and skips that run rather than failing
/// the view.
async fn load_blackboard(
    pool: &sqlx::SqlitePool,
    warnings: &mut Vec<String>,
) -> Vec<BlackboardItemCard> {
    use codypendent_workflow::{BlackboardStore, WorkflowStore};

    let runs = match WorkflowStore::new().list_incomplete_runs(pool).await {
        Ok(runs) => runs,
        Err(error) => {
            warnings.push(format!("could not list workflow runs: {error}"));
            return Vec::new();
        }
    };

    let board = BlackboardStore::new();
    let mut cards = Vec::new();
    for run in runs {
        let run_label = format!("{} \u{b7} run {}", run.workflow_id, short_run_id(&run.id));
        match board.query(pool, &run.id, None, true).await {
            Ok(items) => cards.extend(
                items
                    .iter()
                    .map(|item| blackboard_item_card(&run.id, &run_label, item)),
            ),
            Err(error) => {
                warnings.push(format!(
                    "could not query the blackboard for run {}: {error}",
                    run.id
                ));
            }
        }
        if cards.len() >= MAX_BLACKBOARD_ITEMS {
            warnings.push(format!(
                "blackboard view showing the first {MAX_BLACKBOARD_ITEMS} artifacts"
            ));
            cards.truncate(MAX_BLACKBOARD_ITEMS);
            break;
        }
    }
    cards
}

/// Map a [`BlackboardItem`](codypendent_workflow::BlackboardItem) into the view's
/// [`BlackboardItemCard`], rendering its opaque JSON payload/author/evidence to
/// human strings. `run` is the owning run's label the view groups by.
fn blackboard_item_card(
    workflow_run_id: &str,
    run: &str,
    item: &codypendent_workflow::BlackboardItem,
) -> BlackboardItemCard {
    BlackboardItemCard {
        id: item.id.clone(),
        workflow_run_id: workflow_run_id.to_owned(),
        run: run.to_owned(),
        kind: item.kind.as_str().to_owned(),
        summary: summarize_json(&item.payload),
        author: summarize_author(&item.author),
        confidence: item
            .confidence
            .map_or_else(|| "\u{2014}".to_owned(), |c| format!("{c:.2}")),
        evidence: if item.evidence.is_empty() {
            "\u{2014}".to_owned()
        } else {
            format!("{} ref(s)", item.evidence.len())
        },
        revision: format!("r{}", item.revision),
        superseded: item.superseded_by.is_some(),
    }
}

/// Map the protocol's opaque blackboard view into the same card shape used by
/// the SQLite boot projection. `workflow_label` is resolved from the live
/// workflow cards when possible; the compact run id is always retained.
fn wire_blackboard_item_card(
    workflow_label: Option<&str>,
    item: &codypendent_protocol::BlackboardItemView,
) -> BlackboardItemCard {
    let run = format!(
        "{} · run {}",
        workflow_label.unwrap_or("workflow"),
        short_run_id(&item.workflow_run_id)
    );
    BlackboardItemCard {
        id: item.id.clone(),
        workflow_run_id: item.workflow_run_id.clone(),
        run,
        kind: item.kind.clone(),
        summary: summarize_json(&item.payload),
        author: summarize_author(&item.author),
        confidence: item
            .confidence
            .map_or_else(|| "\u{2014}".to_owned(), |c| format!("{c:.2}")),
        evidence: if item.evidence.is_empty() {
            "\u{2014}".to_owned()
        } else {
            format!("{} ref(s)", item.evidence.len())
        },
        revision: format!("r{}", item.revision),
        superseded: item.superseded_by.is_some(),
    }
}

/// The first 8 characters of a run id, for a compact run label.
fn short_run_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// A one-line human summary of an opaque artifact payload: a string payload as-is;
/// an object's first human-text field (`summary`/`title`/`statement`/…) when one
/// is present; otherwise its compact JSON. Capped so a large payload cannot blow
/// out the card.
fn summarize_json(value: &serde_json::Value) -> String {
    use serde_json::Value;
    let raw = match value {
        Value::String(text) => text.clone(),
        Value::Object(map) => {
            let field = [
                "summary",
                "title",
                "statement",
                "text",
                "description",
                "message",
            ]
            .iter()
            .find_map(|key| map.get(*key).and_then(Value::as_str));
            match field {
                Some(text) => text.to_owned(),
                None => value.to_string(),
            }
        }
        other => other.to_string(),
    };
    truncate_chars(&raw, 200)
}

/// A compact rendering of an opaque author record: a string as-is; an object's
/// `role`/`agent` as `"agent <role>"`; otherwise its compact JSON.
fn summarize_author(value: &serde_json::Value) -> String {
    use serde_json::Value;
    let raw = match value {
        Value::String(text) => text.clone(),
        Value::Object(map) => match map
            .get("role")
            .or_else(|| map.get("agent"))
            .and_then(Value::as_str)
        {
            Some(role) => format!("agent {role}"),
            None => value.to_string(),
        },
        other => other.to_string(),
    };
    truncate_chars(&raw, 80)
}

/// Truncate to at most `max` characters, appending an ellipsis when cut (char-safe
/// so a multi-byte boundary is never split).
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let kept: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}\u{2026}")
    }
}

fn collab_mode_label(mode: CollaborationMode) -> &'static str {
    match mode {
        CollaborationMode::Ask => "ask",
        CollaborationMode::Suggest => "suggest",
        CollaborationMode::Edit => "edit",
        CollaborationMode::CoAuthor => "co-author",
        CollaborationMode::Review => "review",
        CollaborationMode::Maintain => "maintain",
    }
}

fn suggestion_status_label(status: SuggestionStatus) -> &'static str {
    match status {
        SuggestionStatus::Pending => "pending",
        SuggestionStatus::Accepted => "accepted",
        SuggestionStatus::Rejected => "rejected",
    }
}

/// A compact label for who authored a document mutation — an agent sentence
/// names its serving model (the traceability triple's public face).
fn document_author_label(author: &DocumentAuthor) -> String {
    match author {
        DocumentAuthor::Human { .. } => "human".to_owned(),
        DocumentAuthor::Agent { model, .. } => format!("agent ({model})"),
        DocumentAuthor::Integration { integration } => format!("integration ({integration})"),
    }
}

/// The persisted repo → session mapping, so reopening the TUI in a repository
/// resumes its session instead of starting over. Stored as JSON in the data dir;
/// a corrupt or absent file reads as empty (the store is a convenience, never a
/// source of truth — the daemon's ledger is).
#[derive(Debug, Default, Serialize, Deserialize)]
struct SessionStore {
    /// Canonical repository path → the session last opened there.
    sessions: HashMap<String, StoredSession>,
    /// The opaque daemon-issued resume token from the last handshake, presented
    /// on the next launch so this client keeps one identity across restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resume_token: Option<String>,
}

/// One remembered session: its id and the workspace it belongs to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct StoredSession {
    session_id: SessionId,
    workspace_id: WorkspaceId,
}

impl SessionStore {
    fn file(paths: &RuntimePaths) -> PathBuf {
        paths.data_dir.join("tui-sessions.json")
    }

    fn load(paths: &RuntimePaths) -> Self {
        std::fs::read(Self::file(paths))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, paths: &RuntimePaths) {
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = paths.ensure_directories();
            let _ = std::fs::write(Self::file(paths), bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_protocol::{
        AgentMode, ApprovalDecision, ApprovalId, ApprovalScope, ModelId, RunId,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    #[test]
    fn splash_gate_requires_a_fresh_enter_and_keeps_quit_and_resize_responsive() {
        let enter = CrosstermEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(splash_gate_decision(&enter), SplashGateDecision::Continue);

        let mut released_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        released_enter.kind = KeyEventKind::Release;
        assert_eq!(
            splash_gate_decision(&CrosstermEvent::Key(released_enter)),
            SplashGateDecision::Ignore,
            "a release event must not enter the workspace"
        );
        assert_eq!(
            splash_gate_decision(&CrosstermEvent::Key(KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::NONE,
            ))),
            SplashGateDecision::Ignore
        );
        assert_eq!(
            splash_gate_decision(&CrosstermEvent::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            ))),
            SplashGateDecision::Quit
        );
        assert_eq!(
            splash_gate_decision(&CrosstermEvent::Resize(120, 40)),
            SplashGateDecision::Redraw
        );
    }

    #[test]
    fn accessible_presentation_is_stable_and_emits_no_terminal_escapes() {
        let mut state = AppState::new();
        reduce(
            &mut state,
            Action::Notice("ready \u{1b}[31m safely".to_owned()),
        );
        let mut presentation = AccessiblePresentation::new(Vec::<u8>::new());
        presentation
            .draw(&state, false)
            .expect("first cooked snapshot");
        let first_len = presentation.output.len();
        presentation
            .draw(&state, false)
            .expect("unchanged state is not repeated");
        assert_eq!(presentation.output.len(), first_len);

        let output = String::from_utf8(presentation.output).expect("UTF-8 output");
        assert!(output.contains("--- accessible update ---"));
        assert!(output.contains("Notice: ready  safely"));
        assert!(!output.as_bytes().contains(&0x1b));
        assert!(
            !output.contains("[?1049h"),
            "must not enter alternate screen"
        );
        assert!(!output.contains("[?1000h"), "must not enable mouse capture");
    }

    #[test]
    fn accessible_script_maps_lines_without_synthetic_terminal_events() {
        let mut state = AppState::new();
        for line in ["type hello", "enter"] {
            for action in map_accessible_input(line, state.input_mode()) {
                reduce(&mut state, action);
            }
        }
        assert!(state.composer.is_empty());
        assert!(state.drain_outbox().iter().any(
            |intent| matches!(intent, Intent::StartRun { objective, .. } if objective == "hello")
        ));
    }

    #[tokio::test]
    async fn graph_loader_pages_and_filters_without_materializing_the_full_graph() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = knowledge_db::open(&dir.path().join("graph.db"))
            .await
            .expect("knowledge db");
        let repository = RepositoryId::new();
        let repository_text = repository.to_string();
        let created_at = "2026-08-05T00:00:00Z";
        for (id, name, symbol_key) in [
            ("node-from", "crate::parser::parse", "parser-parse"),
            ("node-to", "crate::lexer::next", "lexer-next"),
        ] {
            sqlx::query(
                "INSERT INTO code_nodes \
                 (id, repository, language, package, source_path, qualified_name, kind, \
                  signature_hash, symbol_key, revision, created_at) \
                 VALUES (?, ?, 'rust', NULL, 'src/lib.rs', ?, 'function', NULL, ?, 'rev-1', ?)",
            )
            .bind(id)
            .bind(&repository_text)
            .bind(name)
            .bind(symbol_key)
            .bind(created_at)
            .execute(&pool)
            .await
            .expect("insert node");
        }
        let mut tx = pool.begin().await.expect("edge transaction");
        for index in 0..130 {
            let relation = if index == 129 {
                "calls_special"
            } else {
                "calls"
            };
            sqlx::query(
                "INSERT INTO code_edges \
                 (id, from_node, to_node, relation, confidence, evidence_kind, \
                  evidence_artifact, revision, created_at) \
                 VALUES (?, 'node-from', 'node-to', ?, 0.45, 'syntax_inferred', NULL, 'rev-1', ?)",
            )
            .bind(format!("edge-{index:03}"))
            .bind(relation)
            .bind(created_at)
            .execute(&mut *tx)
            .await
            .expect("insert edge");
        }
        tx.commit().await.expect("commit edges");

        let mut warnings = Vec::new();
        let (first, total, page) = load_edge_page(&pool, repository, "", 0, &mut warnings).await;
        assert_eq!(total, 130);
        assert_eq!(page, 0);
        assert_eq!(first.len(), EDGE_PAGE_SIZE);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

        let (last, total, page) = load_edge_page(&pool, repository, "", 99, &mut warnings).await;
        assert_eq!(total, 130);
        assert_eq!(page, 1, "an oversized page clamps to the final page");
        assert_eq!(last.len(), 30);

        let (filtered, total, page) =
            load_edge_page(&pool, repository, "special", 0, &mut warnings).await;
        assert_eq!(total, 1);
        assert_eq!(page, 0);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].relation, "calls-special");
    }

    #[test]
    fn intents_map_to_the_matching_command_bodies() {
        let session_id = SessionId::new();
        let run_id = RunId::new();
        let repository = "/repo/one";

        assert_eq!(
            intent_to_command(
                Intent::StartRun {
                    objective: "diagnose".into(),
                    mode: AgentMode::Build,
                    // A pinned model (STEP MP2) must flow onto the command.
                    model: Some(ModelId("hosted-gpt".into())),
                },
                session_id,
                repository,
            ),
            CommandBody::StartRun {
                session_id,
                objective: "diagnose".into(),
                mode: AgentMode::Build,
                repository: Some(repository.to_owned()),
                model: Some(ModelId("hosted-gpt".into())),
            }
        );

        let approval_id = ApprovalId::new();
        assert_eq!(
            intent_to_command(
                Intent::ResolveApproval {
                    approval_id,
                    decision: ApprovalDecision::Approve,
                    scope: ApprovalScope::Once,
                },
                session_id,
                repository,
            ),
            CommandBody::ResolveApproval {
                approval_id,
                decision: ApprovalDecision::Approve,
                scope: ApprovalScope::Once,
            }
        );

        // Task 5 (continuous-session plan): a follow-up after a terminal run
        // maps to `SubmitUserInput`, not `StartRun` — the daemon seeds it from
        // the session's prior turns instead of starting cold. Mirrors
        // `StartRun`'s `session_id` binding above; the operator's current model
        // pin threads through so a mid-conversation switch reaches the wire
        // (repository is still not on this shape).
        assert_eq!(
            intent_to_command(
                Intent::SubmitUserInput {
                    text: "also add tests".into(),
                    mode: AgentMode::Build,
                    model: Some(codypendent_protocol::ModelId("pinned-model-x".into())),
                },
                session_id,
                repository,
            ),
            CommandBody::SubmitUserInput {
                session_id,
                text: "also add tests".into(),
                mode: AgentMode::Build,
                model: Some(codypendent_protocol::ModelId("pinned-model-x".into())),
            }
        );
        // And an unpinned follow-up carries no model on the wire (inherit).
        assert_eq!(
            intent_to_command(
                Intent::SubmitUserInput {
                    text: "keep going".into(),
                    mode: AgentMode::Build,
                    model: None,
                },
                session_id,
                repository,
            ),
            CommandBody::SubmitUserInput {
                session_id,
                text: "keep going".into(),
                mode: AgentMode::Build,
                model: None,
            }
        );

        assert_eq!(
            intent_to_command(Intent::PauseRun { run_id }, session_id, repository),
            CommandBody::PauseRun { run_id }
        );
        assert_eq!(
            intent_to_command(Intent::ResumeRun { run_id }, session_id, repository),
            CommandBody::ResumeRun { run_id }
        );
        assert_eq!(
            intent_to_command(Intent::CancelRun { run_id }, session_id, repository),
            CommandBody::CancelRun { run_id }
        );
        assert_eq!(
            intent_to_command(
                Intent::QueueSteering {
                    run_id,
                    text: "focus on the failing test".into(),
                },
                session_id,
                repository,
            ),
            CommandBody::QueueSteering {
                run_id,
                text: "focus on the failing test".into(),
            }
        );

        // Phase 4 STEP 4.3 document-editing intents lower to their commands.
        let document_id = codypendent_protocol::DocumentId::new();
        assert_eq!(
            intent_to_command(
                Intent::AcquireDocumentLease {
                    document_id,
                    block_id: Some("b1".into()),
                },
                session_id,
                repository,
            ),
            CommandBody::AcquireDocumentLease {
                lease: DocumentEditLease {
                    document_id,
                    block_id: Some("b1".into()),
                },
                ttl_seconds: None,
            }
        );
        assert_eq!(
            intent_to_command(
                Intent::ReleaseDocumentLease {
                    lease_id: "lease-1".into(),
                },
                session_id,
                repository,
            ),
            CommandBody::ReleaseDocumentLease {
                lease_id: "lease-1".into(),
            }
        );
        let mutation = codypendent_protocol::DocumentMutation::AcceptSuggestion {
            suggestion_id: "s1".into(),
        };
        assert_eq!(
            intent_to_command(
                Intent::MutateDocument {
                    document_id,
                    mutation: mutation.clone(),
                },
                session_id,
                repository,
            ),
            CommandBody::MutateDocument {
                document_id,
                mutation,
            }
        );

        // Only edit-bearing intents drive a subscription; a release does not.
        assert_eq!(
            doc_intent_target(&Intent::MutateDocument {
                document_id,
                mutation: codypendent_protocol::DocumentMutation::RejectSuggestion {
                    suggestion_id: "s1".into(),
                },
            }),
            Some(document_id)
        );
        assert_eq!(
            doc_intent_target(&Intent::ReleaseDocumentLease {
                lease_id: "lease-1".into(),
            }),
            None
        );
    }

    /// MP1: `model_card` maps a configured model to its measured profile when
    /// one exists at the SAME endpoint (a profile is keyed by
    /// `(model_id, endpoint)`). Without one, configured endpoint locality and
    /// context still render honestly while measured cost remains absent.
    #[test]
    fn model_card_matches_a_profile_by_id_and_endpoint_or_falls_back_id_only() {
        use codypendent_routing::{
            EditProtocol, ModelCapabilities, ModelExecutionProfile, ModelLocation,
            ModelPerformance, ModelProfile, SchemaRepairPolicy, StructuredOutputSupport,
            ToolCallSupport,
        };
        use std::collections::BTreeMap;

        let hosted = codypendent_runtime::models::ModelConfig {
            id: ModelId("hosted-default".into()),
            provider: "openai-compatible".to_owned(),
            base_url: "https://api.openai.com/v1".to_owned(),
            model: "gpt-5.1-codex".to_owned(),
            api_key_env: "OPENAI_API_KEY".to_owned(),
            context_tokens: None,
            provider_id: None,
        };
        // Same id, but the profile below is measured against a DIFFERENT
        // endpoint — must not match (proves the lookup keys on the pair, not
        // just the id).
        let same_id_other_endpoint = codypendent_runtime::models::ModelConfig {
            id: ModelId("hosted-default".into()),
            provider: "openai-compatible".to_owned(),
            base_url: "https://other.example.com/v1".to_owned(),
            model: "gpt-5.1-codex".to_owned(),
            api_key_env: String::new(),
            context_tokens: None,
            provider_id: None,
        };
        let unprofiled = codypendent_runtime::models::ModelConfig {
            id: ModelId("local-default".into()),
            provider: "openai-compatible".to_owned(),
            base_url: "http://localhost:11434/v1".to_owned(),
            model: "qwen2.5-coder:14b".to_owned(),
            api_key_env: String::new(),
            context_tokens: None,
            provider_id: None,
        };

        let profile = ModelProfile {
            id: ModelId("hosted-default".into()),
            location: ModelLocation::Hosted,
            capabilities: ModelCapabilities {
                streaming: true,
                tools: ToolCallSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::Strict,
                vision: false,
                audio_input: false,
                embeddings: false,
                prompt_caching: true,
                reasoning_controls: false,
                context_tokens: Some(200_000),
                output_tokens: Some(8_192),
            },
            performance: ModelPerformance {
                reliability: 0.9,
                cost_per_1k_tokens_usd: 0.03,
                latency_ms_p50: 500.0,
                task_class_success: BTreeMap::new(),
                failure_patterns: vec![],
            },
            execution: ModelExecutionProfile {
                preferred_tool_count: 8,
                edit_protocol: EditProtocol::StructuredPatch,
                context_layout: "system-context-history".to_owned(),
                reasoning_budget: None,
                schema_repair: SchemaRepairPolicy::Reprompt,
            },
            bench: None,
        };
        let mut profiles = HashMap::new();
        profiles.insert(
            (
                ModelId("hosted-default".into()),
                "https://api.openai.com/v1".to_owned(),
            ),
            profile,
        );

        let hosted_card = model_card(hosted, &profiles, ModelReadiness::Unverified, false);
        assert_eq!(hosted_card.id, ModelId("hosted-default".into()));
        assert_eq!(hosted_card.provider, "openai-compatible");
        assert_eq!(hosted_card.location, Some(ModelLocationLabel::Hosted));
        assert!(
            (hosted_card.cost_per_1k_usd.unwrap() - 0.03).abs() < 1e-9,
            "cost should round-trip: {:?}",
            hosted_card.cost_per_1k_usd
        );
        assert_eq!(hosted_card.context_tokens, Some(200_000));

        let other_endpoint_card = model_card(
            same_id_other_endpoint,
            &profiles,
            ModelReadiness::Unverified,
            false,
        );
        assert_eq!(
            other_endpoint_card.location,
            Some(ModelLocationLabel::Hosted),
            "a mismatched profile must not erase configured endpoint locality"
        );

        let unprofiled_card = model_card(unprofiled, &profiles, ModelReadiness::Ready, true);
        assert_eq!(unprofiled_card.id, ModelId("local-default".into()));
        assert_eq!(
            unprofiled_card.location,
            Some(ModelLocationLabel::Local),
            "a local configured endpoint remains visible without a profile"
        );
        assert!(unprofiled_card.cost_per_1k_usd.is_none());
        assert!(unprofiled_card.context_tokens.is_none());
    }

    #[test]
    fn command_envelope_is_self_idempotent() {
        let client_id = ClientId::new();
        let envelope = command_envelope(
            client_id,
            CommandBody::PauseRun {
                run_id: RunId::new(),
            },
        );
        match envelope.payload {
            Payload::Command(command) => {
                // A client-side retry reuses the command id, so the idempotency
                // key must be that same id — the duplicate-delivery contract.
                assert_eq!(command.idempotency_key, command.command_id.to_string());
                assert!(command.expected_revision.is_none());
            }
            other => panic!("expected a Command payload, got {other:?}"),
        }
    }

    #[test]
    fn session_store_round_trips_through_the_data_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = RuntimePaths::from_data_dir(tmp.path().to_path_buf());
        paths.ensure_directories().unwrap();

        let mut store = SessionStore::default();
        let stored = StoredSession {
            session_id: SessionId::new(),
            workspace_id: WorkspaceId::new(),
        };
        store.sessions.insert("/repo/one".into(), stored);
        store.save(&paths);

        let loaded = SessionStore::load(&paths);
        let got = loaded.sessions.get("/repo/one").expect("entry persisted");
        assert_eq!(got.session_id, stored.session_id);
        assert_eq!(got.workspace_id, stored.workspace_id);
    }

    #[test]
    fn a_missing_store_reads_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = RuntimePaths::from_data_dir(tmp.path().to_path_buf());
        assert!(SessionStore::load(&paths).sessions.is_empty());
    }

    /// Crash-logger tests (crash investigation follow-up). `format_crash_entry`
    /// is pure (no I/O), so a test can assert its shape with a synthetic
    /// message/location/backtrace without ever touching a real
    /// `PanicHookInfo` — deliberately, since fabricating one outside an actual
    /// panic is not practical, and this fn is what the panic hook's formatting
    /// reduces to (see `append_crash_log`).
    #[test]
    fn format_crash_entry_includes_message_and_location() {
        let entry = format_crash_entry(
            "index out of bounds: the len is 3 but the index is 5",
            Some("crates/tui/src/render.rs:42:9"),
            "0: codypendent_cli::tui::install_crash_hook\n1: <backtrace omitted>",
        );
        assert!(!entry.is_empty());
        assert!(entry.contains("index out of bounds: the len is 3 but the index is 5"));
        assert!(entry.contains("crates/tui/src/render.rs:42:9"));
        assert!(entry.contains("<backtrace omitted>"));
    }

    #[test]
    fn format_crash_entry_falls_back_when_location_is_absent() {
        let entry = format_crash_entry("boom", None, "<no backtrace captured>");
        assert!(!entry.is_empty());
        assert!(entry.contains("boom"));
    }

    #[test]
    fn write_crash_entry_creates_the_log_and_appends_a_nonempty_entry() {
        let tmp = tempfile::tempdir().unwrap();
        // The parent `logs/` dir does not exist yet — `write_crash_entry` must
        // create it (mirroring `<data_dir>/logs/tui-crash.log`), never fail
        // just because nothing has been logged there before.
        let path = tmp.path().join("logs").join("tui-crash.log");
        assert!(!path.exists());

        let entry = format_crash_entry("boom", Some("src/main.rs:1:1"), "<bt>");
        write_crash_entry(&path, &entry).expect("write_crash_entry must create the dir + file");

        let contents = std::fs::read_to_string(&path).expect("the crash log must now exist");
        assert!(!contents.is_empty());
        assert!(contents.contains("boom"));
        assert!(contents.contains("src/main.rs:1:1"));
    }

    #[test]
    fn write_crash_entry_appends_rather_than_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("logs").join("tui-crash.log");

        write_crash_entry(&path, "first entry\n").unwrap();
        write_crash_entry(&path, "second entry\n").unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("first entry") && contents.contains("second entry"),
            "a second crash must not erase the first crash's entry, got:\n{contents}"
        );
    }

    #[test]
    fn a_zero_event_catchup_is_treated_as_a_missing_session() {
        // The helper keys off the watermark, not the event vec. An absent session
        // watermarks at 0 and must not be resumed (issue #6 item 6); a live one
        // replays at least its SessionCreated event, so its watermark is >= 1.
        assert!(!catchup_proves_session_exists(&Catchup::Events {
            from: 1,
            through: 0,
            events: vec![],
        }));
        assert!(catchup_proves_session_exists(&Catchup::Events {
            from: 1,
            through: 3,
            events: vec![],
        }));
        // A forward-compat variant we can't inspect is accepted rather than
        // discarding a possibly-resumable session against a newer daemon.
        assert!(catchup_proves_session_exists(&Catchup::Unknown));
    }

    /// A manifest exercising both action kinds and every rendered node field: an
    /// agent step with a skill, an isolated worktree, and a before-write
    /// approval; a tool step with a multi-attempt retry and a dependency.
    const TEST_MANIFEST: &str = "\
schema_version: 1
id: test-workflow
version: 1
orchestration_reason: independent-review
budget:
  maximum_cost_usd: 5.0
  maximum_agents: 2
steps:
  - id: patch
    agent:
      role: implementer
      model_policy: coding
    skill: code.repair
    workspace:
      mode: isolated-worktree
    approval: before-write
    outputs: [proposed_patch]
  - id: verify
    depends_on: [patch]
    tool: repository.test
    retry:
      attempts: 2
      backoff_seconds: 5
    outputs: [test_result]
";

    #[test]
    fn workflow_node_card_renders_agent_and_tool_nodes() {
        let compiled = codypendent_workflow::compile_yaml(TEST_MANIFEST).expect("compiles");
        let label = format!("{} v{}", compiled.id, compiled.version);
        let cards: Vec<_> = compiled
            .nodes
            .iter()
            .map(|node| workflow_node_card(&compiled.id, &label, "—", None, node))
            .collect();

        let patch = cards.iter().find(|c| c.id == "patch").expect("patch node");
        assert_eq!(patch.workflow, "test-workflow v1");
        assert_eq!(patch.kind, "agent");
        assert_eq!(patch.agent, "implementer");
        assert_eq!(patch.model_policy, "coding");
        assert!(
            patch.action.contains("skill code.repair"),
            "{}",
            patch.action
        );
        assert_eq!(patch.workspace, "isolated worktree");
        assert_eq!(patch.approval, "before write");
        // A compiled-but-not-yet-run node (no record) is pending with no cost/error.
        assert_eq!(patch.state, "pending");
        assert_eq!(patch.cost, "\u{2014}");
        assert_eq!(patch.error, "\u{2014}");
        assert_eq!(patch.depends_on, "\u{2014}"); // no dependencies
        assert_eq!(patch.outputs, "proposed_patch");

        let verify = cards
            .iter()
            .find(|c| c.id == "verify")
            .expect("verify node");
        assert_eq!(verify.kind, "tool");
        assert_eq!(verify.agent, "\u{2014}");
        assert_eq!(verify.model_policy, "\u{2014}");
        assert_eq!(verify.action, "tool repository.test");
        assert_eq!(verify.workspace, "shared worktree");
        assert_eq!(verify.approval, "none");
        assert_eq!(verify.retry, "2 attempts \u{b7} 5s backoff");
        assert_eq!(verify.depends_on, "patch");
    }

    /// The T8 seam: a durable node record overlays the compiled defaults — the
    /// graph view renders the node's live state, MEASURED cost (only measured
    /// dimensions), and failure/block reason (P5-D4). This is what turns the
    /// forever-`—` cost column and the reasonless block into real values.
    #[test]
    fn workflow_node_card_renders_a_durable_records_cost_state_and_error() {
        use codypendent_workflow::{NodeState, WorkflowNodeRecord};
        let compiled = codypendent_workflow::compile_yaml(TEST_MANIFEST).expect("compiles");
        let label = format!("{} v{}", compiled.id, compiled.version);
        let node = compiled.node("verify").expect("verify node");

        // A blocked record carrying a measured cost + a budget-block reason.
        let record = WorkflowNodeRecord {
            node_id: "verify".to_owned(),
            state: NodeState::Blocked,
            agent_run_id: None,
            attempt: 1,
            topo_order: node.topo_order,
            cost: Some(serde_json::json!({ "wall_time_secs": 12, "tool_calls": 3 })),
            error: Some("workflow.budget-exceeded: node budget for `tool_calls`".to_owned()),
        };
        let latest = LatestWorkflowRun {
            id: "workflow-run-1".to_owned(),
            phase: "running".to_owned(),
            nodes: HashMap::from([("verify".to_owned(), record)]),
        };
        let card = workflow_node_card(&compiled.id, &label, "—", Some(&latest), node);
        assert_eq!(card.state, "blocked");
        assert_eq!(card.cost, "12s \u{b7} 3 tool calls");
        assert!(card.error.contains("budget"), "error: {}", card.error);

        // A cost with only a tool-call count renders just that (singular form),
        // never a fabricated wall-time or token/USD figure.
        assert_eq!(
            render_node_cost(&serde_json::json!({ "tool_calls": 1 })),
            "1 tool call"
        );
        // An unrecognized / empty cost shape renders "—" (nothing was measured).
        assert_eq!(render_node_cost(&serde_json::json!({})), "\u{2014}");
    }

    #[tokio::test]
    async fn load_workflows_compiles_manifests_and_skips_the_uncompilable() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        // A fresh install still exposes the embedded repair workflow.
        let built_in = load_workflows(repo, None, None, &mut Vec::new()).await;
        assert!(built_in
            .iter()
            .any(|card| card.workflow_id == "repair-github-check"));

        let dir = repo.join(".codypendent").join("workflows");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("repair.yaml"), TEST_MANIFEST).unwrap();
        // A manifest that parses but fails to compile (no steps) is skipped, not
        // fatal — the good workflow still loads.
        std::fs::write(
            dir.join("broken.yaml"),
            "schema_version: 1\nid: broken\nversion: 1\nsteps: []\n",
        )
        .unwrap();
        // A non-manifest file is ignored by extension.
        std::fs::write(dir.join("notes.txt"), "ignore me").unwrap();

        let mut warnings = Vec::new();
        let cards = load_workflows(repo, None, None, &mut warnings).await;
        assert_eq!(
            cards.len(),
            7,
            "five built-in nodes plus both nodes of the good manifest"
        );
        let test_cards: Vec<_> = cards
            .iter()
            .filter(|card| card.workflow_id == "test-workflow")
            .collect();
        assert!(test_cards.iter().all(|c| c.workflow == "test-workflow v1"));
        // Nodes keep their compiled topological order.
        assert_eq!(test_cards[0].id, "patch");
        assert_eq!(test_cards[1].id, "verify");
        // The skipped manifest left a diagnostic, not a stderr print.
        assert_eq!(warnings.len(), 1, "warnings: {warnings:?}");
        assert!(
            warnings[0].contains("broken") && warnings[0].contains("does not compile"),
            "the diagnostic names the skipped manifest: {warnings:?}"
        );
    }

    #[test]
    fn blackboard_item_card_renders_opaque_payload_and_provenance() {
        use codypendent_workflow::{BlackboardItem, BlackboardKind};
        use serde_json::json;

        let item = BlackboardItem {
            id: "0192-abc".to_owned(),
            kind: BlackboardKind::Finding,
            payload: json!({ "summary": "off-by-one in paginate()", "detail": "…" }),
            author: json!({ "role": "investigator", "run": "r1" }),
            confidence: Some(0.85),
            evidence: vec![json!({ "artifact": "a1" }), json!({ "artifact": "a2" })],
            revision: 1,
            superseded_by: None,
        };
        let card = blackboard_item_card(
            "workflow-run-1",
            "repair-github-check \u{b7} run 0192abcd",
            &item,
        );
        assert_eq!(card.kind, "finding");
        assert_eq!(card.summary, "off-by-one in paginate()");
        assert_eq!(card.author, "agent investigator");
        assert_eq!(card.confidence, "0.85");
        assert_eq!(card.evidence, "2 ref(s)");
        assert_eq!(card.revision, "r1");
        assert!(!card.superseded);
    }

    #[test]
    fn json_summaries_fall_back_gracefully() {
        use codypendent_workflow::{BlackboardItem, BlackboardKind};
        use serde_json::json;

        // A string payload is used verbatim; an object without a known text field
        // falls back to compact JSON rather than panicking.
        assert_eq!(summarize_json(&json!("plain text")), "plain text");
        assert!(summarize_json(&json!({ "x": 1 })).contains("\"x\""));
        assert!(summarize_author(&json!({ "who": "?" })).contains("who"));

        // A superseded hypothesis with no confidence or evidence renders em dashes.
        let item = BlackboardItem {
            id: "1".to_owned(),
            kind: BlackboardKind::Hypothesis,
            payload: json!("a guess"),
            author: json!("someone"),
            confidence: None,
            evidence: vec![],
            revision: 3,
            superseded_by: Some("2".to_owned()),
        };
        let card = blackboard_item_card("workflow-run-1", "run", &item);
        assert_eq!(card.summary, "a guess");
        assert_eq!(card.author, "someone");
        assert_eq!(card.confidence, "\u{2014}");
        assert_eq!(card.evidence, "\u{2014}");
        assert_eq!(card.revision, "r3");
        assert!(card.superseded);
    }

    // ----------------------------------------------------------------------
    // GapTracker — the reconnect / gap-repair state machine (C6 + FP-2).
    //
    // This is the code that keeps a lagged client from losing an event the
    // daemon dropped from its live fan-out — worst case an `ApprovalRequested`.
    // It had zero tests; these drive the pure decision unit directly.
    // ----------------------------------------------------------------------

    use codypendent_protocol::{Actor, EventBody, ProposedAction, Risk, RiskLevel};

    /// A benign live event at `sequence` (body content is irrelevant to the
    /// tracker, which orders purely by sequence).
    fn ev(sequence: u64) -> SessionEvent {
        SessionEvent {
            sequence,
            occurred_at: chrono::Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor: Actor::System,
            body: EventBody::NoteAppended {
                text: format!("event {sequence}"),
                run_id: None,
            },
        }
    }

    /// An `ApprovalRequested` event at `sequence` carrying `approval_id` — the
    /// event whose loss under lag the whole repair path exists to prevent.
    fn approval_ev(sequence: u64, approval_id: ApprovalId) -> SessionEvent {
        SessionEvent {
            sequence,
            occurred_at: chrono::Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor: Actor::System,
            body: EventBody::ApprovalRequested {
                approval_id,
                action: ProposedAction::ReadFiles {
                    paths: vec!["src/lib.rs".to_owned()],
                },
                risk: Risk {
                    level: RiskLevel::Low,
                    reasons: Vec::new(),
                },
            },
        }
    }

    fn seqs(events: &[SessionEvent]) -> Vec<u64> {
        events.iter().map(|e| e.sequence).collect()
    }

    /// An in-order event is applied and advances the watermark; a duplicate of
    /// an already-folded event is ignored (catch-up + live overlap).
    #[test]
    fn in_order_events_apply_and_duplicates_are_ignored() {
        let mut t = GapTracker::new(5);
        let now = Instant::now();

        match t.on_event(ev(6), now) {
            GapAction::Apply(e) => assert_eq!(e.sequence, 6),
            other => panic!("expected Apply(6), got {other:?}"),
        }
        assert_eq!(t.last_seen, 6);
        // A stale re-delivery of an already-folded sequence folds nothing.
        assert!(matches!(t.on_event(ev(4), now), GapAction::Ignore));
        assert!(matches!(t.on_event(ev(6), now), GapAction::Ignore));
        assert_eq!(t.last_seen, 6);
    }

    /// Behaviour 1: a detected gap re-attaches from `last_seen` — the watermark
    /// BEFORE the gap-revealing event, never the gap event's own sequence.
    #[test]
    fn detected_gap_reattaches_from_last_seen_not_the_gap_event() {
        let mut t = GapTracker::new(5);
        let now = Instant::now();

        // Expected next is 6; 8 arrives → a 6..=7 gap.
        match t.on_event(ev(8), now) {
            GapAction::Reattach { last_seen_sequence } => {
                assert_eq!(
                    last_seen_sequence, 5,
                    "must replay from the pre-gap watermark, not the gap event"
                );
            }
            other => panic!("expected Reattach, got {other:?}"),
        }
        // The watermark did NOT advance to the gap event (the C6 invariant), and
        // the gap event is held for after the replay.
        assert_eq!(t.last_seen, 5);
        assert_eq!(seqs(&t.gap_buffer), vec![8]);
    }

    /// Behaviour 2 + 3: events that arrive live during the repair are buffered,
    /// then applied after the catch-up in ascending order, deduped against the
    /// watermark — none dropped (the original C6 bug dropped them), none
    /// duplicated.
    #[test]
    fn buffered_events_apply_in_order_after_repair_without_loss_or_dup() {
        let mut t = GapTracker::new(5);
        let now = Instant::now();

        // Gap at 8 opens the repair.
        assert!(matches!(t.on_event(ev(8), now), GapAction::Reattach { .. }));
        // More live events during the repair, out of order and with a duplicate.
        assert!(matches!(t.on_event(ev(10), now), GapAction::Ignore));
        assert!(matches!(t.on_event(ev(9), now), GapAction::Ignore));
        assert!(matches!(t.on_event(ev(8), now), GapAction::Ignore)); // dup of buffered
                                                                      // A stale re-delivery below the watermark is ignored, not buffered.
        assert!(matches!(t.on_event(ev(3), now), GapAction::Ignore));

        // The daemon replayed 6,7 (the harness folds those); catch-up through=7.
        let drain = t.on_catchup(7, now);
        assert_eq!(
            seqs(&drain.apply),
            vec![8, 9, 10],
            "buffered events apply in order, deduped, none lost"
        );
        assert!(drain.reattach.is_none());
        assert_eq!(t.last_seen, 10);
    }

    /// Behaviour 4 (the safety property): an `ApprovalRequested` that fell in
    /// the gap is applied after the repair, never dropped.
    #[test]
    fn an_approval_in_the_gap_is_never_lost() {
        let approval_id = ApprovalId::new();
        let mut t = GapTracker::new(5);
        let now = Instant::now();

        // The gap-revealing event IS the approval (it raced ahead of its span).
        assert!(matches!(
            t.on_event(approval_ev(8, approval_id), now),
            GapAction::Reattach { .. }
        ));
        let drain = t.on_catchup(7, now);
        assert_eq!(drain.apply.len(), 1, "the approval must survive the repair");
        match &drain.apply[0].body {
            EventBody::ApprovalRequested {
                approval_id: got, ..
            } => assert_eq!(*got, approval_id),
            other => panic!("the approval was lost or corrupted: {other:?}"),
        }
    }

    /// The C6 fix summary "buffers mid-repair events and re-repairs": if the
    /// catch-up did not reach the buffered tail (more loss occurred while
    /// repairing), the tracker applies what it can and asks to repair again,
    /// keeping the tail — still no loss, still in order.
    #[test]
    fn a_hole_in_the_buffered_tail_triggers_another_repair() {
        let mut t = GapTracker::new(5);
        let now = Instant::now();

        assert!(matches!(t.on_event(ev(8), now), GapAction::Reattach { .. }));
        // Further loss: 12 arrives with 9..=11 still missing.
        assert!(matches!(t.on_event(ev(12), now), GapAction::Ignore));

        // First catch-up only reached 7. Buffer is [8, 12].
        let drain = t.on_catchup(7, now);
        assert_eq!(
            seqs(&drain.apply),
            vec![8],
            "8 folds, 12 is still out of order"
        );
        assert_eq!(
            drain.reattach,
            Some(8),
            "re-repair from 8, keeping the tail"
        );
        assert_eq!(seqs(&t.gap_buffer), vec![12]);

        // Second catch-up fills 9,10,11 → through=11; 12 now folds.
        let drain2 = t.on_catchup(11, now);
        assert_eq!(seqs(&drain2.apply), vec![12]);
        assert!(drain2.reattach.is_none());
        assert_eq!(t.last_seen, 12);
    }

    /// FP-2a: the buffer is bounded. Once it fills during a repair, the next
    /// event drops the incremental replay and re-attaches afresh from the
    /// watermark — failing toward a fresh catch-up, never toward unbounded
    /// memory. The ledger re-delivers the whole span, so nothing is lost.
    #[test]
    fn a_full_gap_buffer_reattaches_fresh_instead_of_growing() {
        let mut t = GapTracker::new(5);
        let now = Instant::now();

        assert!(matches!(t.on_event(ev(8), now), GapAction::Reattach { .. }));

        // Feed distinct later sequences until the buffer overflows. The range is
        // generous enough (cap + slack) that the overflow must occur within it.
        let mut overflowed = false;
        for seq in 9..=(9 + MAX_GAP_BUFFER as u64 + 5) {
            match t.on_event(ev(seq), now) {
                GapAction::Ignore => {}
                GapAction::Reattach { last_seen_sequence } => {
                    assert_eq!(last_seen_sequence, 5, "overflow replays from the watermark");
                    overflowed = true;
                    break;
                }
                other => panic!("unexpected action during buffering: {other:?}"),
            }
        }
        assert!(
            overflowed,
            "the buffer must overflow into a fresh re-attach, not grow past the cap"
        );
        assert!(t.gap_buffer.len() <= MAX_GAP_BUFFER);
        assert!(
            t.gap_buffer.is_empty(),
            "the stale buffer is dropped on overflow"
        );
        assert!(t.repairing, "still awaiting the fresh catch-up");
    }

    /// FP-2b: a repair whose catch-up reply never arrives is abandoned once the
    /// deadline passes — `on_tick` asks for a fresh re-attach from the
    /// watermark instead of wedging the client in `repairing` forever.
    #[test]
    fn a_stalled_repair_times_out_into_a_fresh_reattach() {
        let mut t = GapTracker::new(5);
        let t0 = Instant::now();

        assert!(matches!(t.on_event(ev(8), t0), GapAction::Reattach { .. }));
        // Before the deadline: nothing.
        assert!(t.on_tick(t0 + Duration::from_millis(1)).is_none());
        assert!(t
            .on_tick(t0 + REPAIR_TIMEOUT - Duration::from_millis(1))
            .is_none());
        // Past the deadline: re-attach from the watermark, dropping the stale
        // buffer.
        assert_eq!(
            t.on_tick(t0 + REPAIR_TIMEOUT + Duration::from_millis(1)),
            Some(5)
        );
        assert!(t.gap_buffer.is_empty());

        // A tracker that is not repairing never fires a timeout.
        let mut idle = GapTracker::new(5);
        assert!(idle
            .on_tick(Instant::now() + Duration::from_secs(3600))
            .is_none());
    }

    /// FP-2c: sequence 0 is a sentinel (the daemon numbers events 1-based), so
    /// it is folded straight through in any state — including mid-repair, where
    /// it was previously buffered and then silently discarded — and it never
    /// moves the watermark or disturbs the gap buffer.
    #[test]
    fn sentinel_sequence_zero_is_applied_in_any_state() {
        let now = Instant::now();

        // Idle: applied, watermark untouched.
        let mut t = GapTracker::new(5);
        match t.on_event(ev(0), now) {
            GapAction::Apply(e) => assert_eq!(e.sequence, 0),
            other => panic!("expected Apply(0), got {other:?}"),
        }
        assert_eq!(t.last_seen, 5);

        // Mid-repair: STILL applied immediately (not buffered/discarded), and
        // the real gap buffer is left intact.
        let mut t = GapTracker::new(5);
        assert!(matches!(t.on_event(ev(8), now), GapAction::Reattach { .. }));
        match t.on_event(ev(0), now) {
            GapAction::Apply(e) => assert_eq!(e.sequence, 0),
            other => panic!("a sentinel must fold through mid-repair, got {other:?}"),
        }
        assert_eq!(seqs(&t.gap_buffer), vec![8]);
        assert_eq!(t.last_seen, 5);
    }

    /// A zero attach-watermark (an empty catch-up baseline) accepts the first
    /// live event without gap detection — there is no baseline to gap against.
    #[test]
    fn a_zero_watermark_seeds_from_the_first_live_event() {
        let mut t = GapTracker::new(0);
        let now = Instant::now();
        match t.on_event(ev(42), now) {
            GapAction::Apply(e) => assert_eq!(e.sequence, 42),
            other => panic!("expected Apply(42), got {other:?}"),
        }
        assert_eq!(t.last_seen, 42);
        // And now a real gap past that seed is detected.
        assert!(matches!(
            t.on_event(ev(50), now),
            GapAction::Reattach { .. }
        ));
    }

    // -- write_add_model (add-a-usable-model-from-the-TUI, Task 3) ------------

    #[test]
    fn write_add_model_appends_an_entry_that_round_trips_through_load_models() {
        use codypendent_runtime::models::load_models;
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());

        // "groq" is a built-in catalog provider (hosted, api-key).
        write_add_model(
            &paths,
            "groq/llama",
            "groq",
            "llama-3.1-8b",
            Some("sk-secret"),
        )
        .expect("write_add_model");

        let configs = load_models(&paths.data_dir.join("models.toml")).expect("parse");
        let entry = configs
            .iter()
            .find(|c| c.id.0 == "groq/llama")
            .expect("the entry is present");
        assert_eq!(entry.provider, "openai-compatible");
        assert_eq!(entry.model, "llama-3.1-8b");
        assert!(
            entry.base_url.contains("groq"),
            "base_url comes from the catalog: {}",
            entry.base_url
        );
        assert_eq!(
            entry.api_key_env, "",
            "the key lives in auth.json, not api_key_env"
        );

        // The key landed in auth.json.
        let auth =
            codypendent_runtime::auth::AuthStore::load(&paths.data_dir).expect("auth.json loads");
        assert_eq!(auth.get("groq/llama"), Some("sk-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn write_add_model_stores_the_key_at_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        write_add_model(
            &paths,
            "groq/llama",
            "groq",
            "llama-3.1-8b",
            Some("sk-secret"),
        )
        .expect("write");
        let meta = std::fs::metadata(paths.data_dir.join("auth.json")).expect("metadata");
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn write_add_model_for_a_local_provider_writes_no_key() {
        use codypendent_runtime::models::load_models;
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());

        // "ollama" is a built-in LOCAL provider (auth none) — no key entered.
        write_add_model(&paths, "ollama/qwen", "ollama", "qwen2.5-coder:14b", None).expect("write");

        let configs = load_models(&paths.data_dir.join("models.toml")).expect("parse");
        assert!(configs.iter().any(|c| c.id.0 == "ollama/qwen"));
        assert!(
            !paths.data_dir.join("auth.json").exists(),
            "a local add writes no auth.json"
        );
    }

    /// Hard requirement: an empty (or whitespace-only) key must never reach
    /// `AuthStore::set` — storing `set(id, "")` would silently shadow a valid
    /// `api_key_env` into "no key" at resolution time (SDD ledger M1). A blank
    /// key is treated exactly like `None`: no `auth.json` at all.
    #[test]
    fn write_add_model_treats_a_blank_key_as_no_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());

        write_add_model(&paths, "groq/llama", "groq", "llama-3.1-8b", Some("   ")).expect("write");

        assert!(
            !paths.data_dir.join("auth.json").exists(),
            "a blank/whitespace-only key must never be written to auth.json"
        );
    }

    #[test]
    fn write_add_model_updates_a_duplicate_display_id_in_place() {
        use codypendent_runtime::models::load_models;
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());

        write_add_model(&paths, "groq/llama", "groq", "llama-3.1-8b", Some("k1")).expect("write 1");
        write_add_model(&paths, "groq/llama", "groq", "llama-3.3-70b", Some("k2"))
            .expect("write 2");

        let configs = load_models(&paths.data_dir.join("models.toml")).expect("parse");
        let matching: Vec<_> = configs.iter().filter(|c| c.id.0 == "groq/llama").collect();
        assert_eq!(
            matching.len(),
            1,
            "a duplicate display id updates in place, never dupes"
        );
        assert_eq!(
            matching[0].model, "llama-3.3-70b",
            "the entry took the new model"
        );
        assert_eq!(
            codypendent_runtime::auth::AuthStore::load(&paths.data_dir)
                .expect("auth.json loads")
                .get("groq/llama"),
            Some("k2"),
            "the key updated too"
        );
    }

    /// Hard requirement: appending a new model must not clobber an existing
    /// `[[model]]` entry already in `models.toml` — the write goes through the
    /// real loader (parse → dedupe-by-id → serialize), not a blind text append.
    #[test]
    fn write_add_model_preserves_an_existing_entry_when_adding_another() {
        use codypendent_runtime::models::load_models;
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());

        write_add_model(&paths, "groq/llama", "groq", "llama-3.1-8b", Some("k1"))
            .expect("write first model");
        write_add_model(&paths, "ollama/qwen", "ollama", "qwen2.5-coder:14b", None)
            .expect("write second model");

        let configs = load_models(&paths.data_dir.join("models.toml")).expect("parse");
        assert_eq!(configs.len(), 2, "both entries survive: {configs:?}");
        assert!(configs.iter().any(|c| c.id.0 == "groq/llama"));
        assert!(configs.iter().any(|c| c.id.0 == "ollama/qwen"));
        // The first model's own key is untouched by the second (keyless) add.
        assert_eq!(
            codypendent_runtime::auth::AuthStore::load(&paths.data_dir)
                .expect("auth.json loads")
                .get("groq/llama"),
            Some("k1")
        );
    }

    /// Hard requirement: a blank/whitespace-only display id is rejected with a
    /// user-visible error and writes nothing — neither `models.toml` nor
    /// `auth.json` is created.
    #[test]
    fn write_add_model_rejects_a_blank_display_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());

        let error = write_add_model(&paths, "   ", "groq", "llama-3.1-8b", Some("sk-secret"))
            .expect_err("a blank display id must be rejected");
        assert!(
            !error.to_string().is_empty(),
            "the error must carry a user-visible message"
        );
        assert!(
            !paths.data_dir.join("models.toml").exists(),
            "a rejected add writes no models.toml"
        );
        assert!(
            !paths.data_dir.join("auth.json").exists(),
            "a rejected add writes no auth.json"
        );
    }

    /// Hard requirement (M3, all-or-nothing): when a non-blank key is entered,
    /// a pre-existing but CORRUPT `auth.json` must abort the whole add before
    /// anything is written — never leaving a keyless `models.toml` entry
    /// behind. This is why `write_add_model` loads `auth.json` (fallible)
    /// BEFORE writing `models.toml` — see its doc comment.
    #[test]
    fn write_add_model_is_all_or_nothing_when_auth_json_is_corrupt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());

        std::fs::create_dir_all(&paths.data_dir).expect("create data dir");
        std::fs::write(paths.data_dir.join("auth.json"), b"{ not json")
            .expect("seed corrupt auth.json");

        let error = write_add_model(
            &paths,
            "groq/llama",
            "groq",
            "llama-3.1-8b",
            Some("sk-secret"),
        )
        .expect_err("a corrupt pre-existing auth.json must abort the whole add");
        assert!(
            !error.to_string().is_empty(),
            "the error must carry a user-visible message"
        );
        assert!(
            !paths.data_dir.join("models.toml").exists(),
            "a corrupt auth.json must abort BEFORE models.toml is written (all-or-nothing)"
        );
    }

    // -- write_api_key / load_key_statuses (D1, `/keys`) ----------------------

    #[test]
    fn write_api_key_sets_replaces_and_round_trips_at_mode_0600() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());

        // An unrelated entry must survive a set (load-before-write).
        write_api_key(
            &paths,
            &KeyTarget::Model("openai/gpt".to_owned()),
            Some("sk-other"),
        )
        .expect("set unrelated");
        write_api_key(
            &paths,
            &KeyTarget::Model("groq/llama".to_owned()),
            Some("sk-first"),
        )
        .expect("set");
        write_api_key(
            &paths,
            &KeyTarget::Model("groq/llama".to_owned()),
            Some("sk-second"),
        )
        .expect("replace");

        let auth =
            codypendent_runtime::auth::AuthStore::load(&paths.data_dir).expect("auth.json loads");
        assert_eq!(auth.get("groq/llama"), Some("sk-second"), "replace wins");
        assert_eq!(
            auth.get("openai/gpt"),
            Some("sk-other"),
            "the unrelated entry survived"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(paths.data_dir.join("auth.json")).expect("metadata");
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn write_api_key_remove_deletes_the_entry_and_an_absent_remove_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());

        // Removing an absent entry creates no auth.json at all.
        write_api_key(&paths, &KeyTarget::Model("groq/llama".to_owned()), None)
            .expect("absent remove is a no-op");
        assert!(
            !paths.data_dir.join("auth.json").exists(),
            "a no-op remove must not create an empty auth.json"
        );

        write_api_key(
            &paths,
            &KeyTarget::Model("groq/llama".to_owned()),
            Some("sk-secret"),
        )
        .expect("set");
        write_api_key(&paths, &KeyTarget::Model("groq/llama".to_owned()), None).expect("remove");
        let auth =
            codypendent_runtime::auth::AuthStore::load(&paths.data_dir).expect("auth.json loads");
        assert_eq!(auth.get("groq/llama"), None, "the entry is gone");
    }

    #[test]
    fn write_api_key_maps_the_tavily_target_to_the_reserved_id_the_daemon_reads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());

        write_api_key(&paths, &KeyTarget::Tavily, Some("tvly-saved")).expect("set the Tavily key");
        let auth =
            codypendent_runtime::auth::AuthStore::load(&paths.data_dir).expect("auth.json loads");
        assert_eq!(
            auth.get(codypendent_integrations::search::TAVILY_AUTH_ID),
            Some("tvly-saved"),
            "the Tavily target lands under the reserved id"
        );

        // And the daemon's own discovery (auth.json first, env second) reads
        // exactly this slot — the file wins regardless of the env, so this
        // assertion never touches the process environment.
        let key = codypendent_integrations::search::TavilyKey::discover(&paths.data_dir)
            .expect("the daemon's discovery reads the saved key");
        assert_eq!(key.expose(), "tvly-saved");

        write_api_key(&paths, &KeyTarget::Tavily, None).expect("remove");
        let auth =
            codypendent_runtime::auth::AuthStore::load(&paths.data_dir).expect("auth.json loads");
        assert_eq!(
            auth.get(codypendent_integrations::search::TAVILY_AUTH_ID),
            None,
            "the reserved entry removes too"
        );
    }

    #[test]
    fn write_api_key_rejects_a_blank_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        write_api_key(
            &paths,
            &KeyTarget::Model("groq/llama".to_owned()),
            Some("   "),
        )
        .expect_err("a blank key is rejected (the M1 shadow guard)");
        assert!(
            !paths.data_dir.join("auth.json").exists(),
            "a rejected write creates no auth.json"
        );
    }

    #[test]
    fn write_api_key_aborts_on_a_corrupt_auth_json_without_touching_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        std::fs::create_dir_all(&paths.data_dir).expect("create data dir");
        std::fs::write(paths.data_dir.join("auth.json"), b"{ not json")
            .expect("seed corrupt auth.json");

        write_api_key(
            &paths,
            &KeyTarget::Model("groq/llama".to_owned()),
            Some("sk-secret"),
        )
        .expect_err("a corrupt pre-existing auth.json must abort (load-before-write)");
        write_api_key(&paths, &KeyTarget::Model("groq/llama".to_owned()), None)
            .expect_err("a remove aborts too");
        assert_eq!(
            std::fs::read(paths.data_dir.join("auth.json")).expect("read"),
            b"{ not json",
            "the corrupt file is left untouched, never silently replaced"
        );
    }

    #[test]
    fn load_key_statuses_reflects_stored_env_and_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        std::fs::create_dir_all(&paths.data_dir).expect("create data dir");
        std::fs::write(
            paths.data_dir.join("models.toml"),
            r#"
[[model]]
id = "groq/llama"
provider = "openai-compatible"
base_url = "https://api.groq.com/openai/v1"
model = "llama-3.1-8b"

[[model]]
id = "openai/gpt"
provider = "openai-compatible"
base_url = "https://api.openai.com/v1"
model = "gpt-5.1-codex"
api_key_env = "OPENAI_API_KEY"

[[model]]
id = "ollama/qwen"
provider = "openai-compatible"
base_url = "http://localhost:11434/v1"
model = "qwen2.5-coder:14b"
"#,
        )
        .expect("write models.toml");

        // Nothing stored yet: env-declared shows the NAME, the rest Missing.
        let (models, _) = load_key_statuses(&paths, &mut Vec::new());
        assert_eq!(
            models,
            vec![
                ("groq/llama".to_owned(), KeyStatus::Missing),
                (
                    "openai/gpt".to_owned(),
                    KeyStatus::Env("OPENAI_API_KEY".to_owned())
                ),
                ("ollama/qwen".to_owned(), KeyStatus::Missing),
            ]
        );

        // A stored key beats the env declaration. (The Tavily row lives in
        // `load_key_statuses_tavily_row_mirrors_the_daemon_discovery_precedence`
        // — the ONE test allowed to touch `TAVILY_API_KEY`.)
        write_api_key(
            &paths,
            &KeyTarget::Model("openai/gpt".to_owned()),
            Some("sk-x"),
        )
        .expect("set model key");
        let (models, _) = load_key_statuses(&paths, &mut Vec::new());
        assert_eq!(
            models,
            vec![
                ("groq/llama".to_owned(), KeyStatus::Missing),
                ("openai/gpt".to_owned(), KeyStatus::Stored),
                ("ollama/qwen".to_owned(), KeyStatus::Missing),
            ]
        );
    }

    #[test]
    fn load_key_statuses_tavily_row_mirrors_the_daemon_discovery_precedence() {
        // Every `TAVILY_API_KEY`-touching case lives in this ONE test: the
        // process environment is global mutable state, so two tests racing
        // `set_var`/`remove_var` on the SAME variable would flake (the
        // `search/key.rs` convention). No other test may set the variable or
        // assert on the Tavily row.
        use codypendent_integrations::search::key::TAVILY_API_KEY_ENV;
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        std::fs::create_dir_all(&paths.data_dir).expect("create data dir");

        // 1. Neither source → Missing.
        std::env::remove_var(TAVILY_API_KEY_ENV);
        let (_, tavily) = load_key_statuses(&paths, &mut Vec::new());
        assert_eq!(tavily, KeyStatus::Missing);

        // 2. Env set (no auth.json entry) → Env(NAME) — the variable NAME
        //    only; the value is never read into the projection.
        std::env::set_var(TAVILY_API_KEY_ENV, "tvly-env-key");
        let (_, tavily) = load_key_statuses(&paths, &mut Vec::new());
        assert_eq!(tavily, KeyStatus::Env(TAVILY_API_KEY_ENV.to_owned()));

        // 3. A blank env value counts as absent (exactly like `discover`).
        std::env::set_var(TAVILY_API_KEY_ENV, "   ");
        let (_, tavily) = load_key_statuses(&paths, &mut Vec::new());
        assert_eq!(tavily, KeyStatus::Missing);

        // 4. A stored entry beats the env — the file wins, exactly like
        //    `TavilyKey::discover`.
        std::env::set_var(TAVILY_API_KEY_ENV, "tvly-env-key");
        write_api_key(&paths, &KeyTarget::Tavily, Some("tvly-stored")).expect("set tavily");
        let (_, tavily) = load_key_statuses(&paths, &mut Vec::new());
        assert_eq!(tavily, KeyStatus::Stored);

        std::env::remove_var(TAVILY_API_KEY_ENV);
    }

    #[test]
    fn tavily_key_set_and_remove_apply_without_a_daemon_restart() {
        use codypendent_tui::Overlay;
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        let mut state = AppState::new();

        // The daemon's reloading adapter resolves immediately before each call.
        apply_set_api_key(&mut state, &paths, &KeyTarget::Tavily, "tvly-saved");
        assert_eq!(state.overlay, Overlay::None);
        assert_eq!(state.tavily_key_status, KeyStatus::Stored);
        assert!(state.notice.as_ref().unwrap().0.contains("ready"));

        // Removal is equally immediate (an environment fallback may still be
        // reported separately by the status projection).
        apply_remove_api_key(&mut state, &paths, &KeyTarget::Tavily);
        assert_eq!(state.overlay, Overlay::None);
        assert_eq!(state.tavily_key_status, KeyStatus::Missing);
        let notice = state
            .notice
            .as_ref()
            .map(|(text, _)| text.as_str())
            .unwrap_or("");
        assert!(
            notice.contains("disabled"),
            "immediate removal notice: {notice}"
        );
    }

    #[tokio::test]
    async fn apply_add_model_refires_the_key_status_projection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        let mut state = AppState::new();

        // "groq" is a built-in catalog provider (hosted, api-key).
        apply_add_model(
            &mut state,
            &paths,
            "groq/llama",
            "groq",
            "llama-3.1-8b",
            Some("sk-secret"),
        )
        .await;

        // The picker re-seeded…
        assert!(
            state.models.iter().any(|card| card.id.0 == "groq/llama"),
            "the model picker re-seeded"
        );
        // …and the /keys projection re-fired with it — a model added WITH a
        // key shows `Stored` immediately, without a TUI restart.
        assert_eq!(
            state.key_status,
            vec![("groq/llama".to_owned(), KeyStatus::Stored)]
        );
        assert_eq!(
            state.notice.as_ref().map(|(text, _)| text.as_str()),
            Some("added model groq/llama")
        );
    }

    #[test]
    fn boot_warnings_survive_the_watch_channel_and_notice_post_boot() {
        // What `warn_stage` does: the stage channel keeps only the LATEST
        // value — the very next stage overwrites a `Reconciling` warning —
        // while the shared vec keeps every one.
        let (stage_tx, stage_rx) = watch::channel(SplashStage::StartingDaemon);
        let warnings: BootWarnings = BootWarnings::default();
        let warn_stage = |message: &str| {
            push_boot_warning(&warnings, message.to_owned());
            let _ = stage_tx.send(SplashStage::Reconciling(message.to_owned()));
        };
        warn_stage("daemon build mismatch; continuing on the running build");
        warn_stage("restart refused: 2 active run(s)");
        let _ = stage_tx.send(SplashStage::RestoringSession);

        // The channel retained only the overwrite…
        assert_eq!(stage_rx.borrow().text(), "restoring session…");
        // …but the vec kept both warnings, and draining persists both in the
        // diagnostics centre instead of losing all but the final transient.
        let mut state = AppState::new();
        drain_boot_warnings(&mut state, &warnings);
        assert!(
            warnings.lock().expect("poisoned").is_empty(),
            "drained exactly once"
        );
        assert_eq!(state.issues.len(), 2);
        assert!(state
            .issues
            .iter()
            .any(|issue| issue == "daemon build mismatch; continuing on the running build"));
        assert!(state
            .issues
            .iter()
            .any(|issue| issue == "restart refused: 2 active run(s)"));
    }

    // -- provider_requires_key (Task 8 add-model key step derivation) --------

    /// A minimal `Provider` for exercising `provider_requires_key` directly —
    /// every field but `auth` is irrelevant to that derivation.
    fn provider_with_auth(
        auth: Vec<codypendent_providers::AuthMethod>,
    ) -> codypendent_providers::Provider {
        codypendent_providers::Provider {
            id: "test-provider".to_string(),
            name: "Test Provider".to_string(),
            protocol: codypendent_providers::Protocol::OpenAiChat,
            base_url: None,
            auth,
            extra_headers: Default::default(),
            query_params: Default::default(),
            local: false,
        }
    }

    #[test]
    fn provider_requires_key_is_true_when_first_auth_is_api_key() {
        use codypendent_providers::AuthMethod;
        let p = provider_with_auth(vec![AuthMethod::ApiKey {
            env: vec!["GROQ_API_KEY".to_string()],
            header: "Authorization".to_string(),
            prefix: "Bearer ".to_string(),
        }]);
        assert!(provider_requires_key(&p));
    }

    #[test]
    fn provider_requires_key_is_false_when_first_auth_is_none() {
        use codypendent_providers::AuthMethod;
        let p = provider_with_auth(vec![AuthMethod::None]);
        assert!(!provider_requires_key(&p));
    }

    #[test]
    fn provider_requires_key_is_false_for_acp_cloud_iam_and_oauth() {
        use codypendent_providers::AuthMethod;

        let acp = provider_with_auth(vec![AuthMethod::Acp {
            command: "gemini".to_string(),
            args: vec!["--acp".to_string()],
            env: Default::default(),
        }]);
        assert!(!provider_requires_key(&acp));

        let cloud_iam = provider_with_auth(vec![AuthMethod::CloudIam {
            variant: "aws_sigv4".to_string(),
            env: Default::default(),
            scopes: vec![],
        }]);
        assert!(!provider_requires_key(&cloud_iam));

        let oauth = provider_with_auth(vec![AuthMethod::OAuth {
            authorize_url: "https://example.com/authorize".to_string(),
            token_url: "https://example.com/token".to_string(),
            client_id: "client".to_string(),
            scopes: vec![],
            pkce: true,
        }]);
        assert!(!provider_requires_key(&oauth));
    }

    #[test]
    fn provider_requires_key_is_false_for_an_empty_auth_list() {
        let p = provider_with_auth(vec![]);
        assert!(!provider_requires_key(&p));
    }

    // -- provider_can_list_models (model-discovery gate) ----------------------

    /// A `Provider` with an explicit protocol + base_url, for exercising
    /// `provider_can_list_models` (which reads all three of protocol, base_url,
    /// and the first auth method).
    fn provider_listable(
        protocol: codypendent_providers::Protocol,
        base_url: Option<&str>,
        auth: Vec<codypendent_providers::AuthMethod>,
    ) -> codypendent_providers::Provider {
        codypendent_providers::Provider {
            id: "test-provider".to_string(),
            name: "Test Provider".to_string(),
            protocol,
            base_url: base_url.map(str::to_string),
            auth,
            extra_headers: Default::default(),
            query_params: Default::default(),
            local: false,
        }
    }

    #[test]
    fn can_list_models_true_for_openai_chat_with_base_url_and_api_key() {
        use codypendent_providers::{AuthMethod, Protocol};
        let p = provider_listable(
            Protocol::OpenAiChat,
            Some("https://api.groq.com/openai/v1"),
            vec![AuthMethod::ApiKey {
                env: vec!["GROQ_API_KEY".to_string()],
                header: "Authorization".to_string(),
                prefix: "Bearer ".to_string(),
            }],
        );
        assert!(provider_can_list_models(&p));
    }

    #[test]
    fn can_list_models_true_for_openai_chat_with_base_url_and_no_auth() {
        use codypendent_providers::{AuthMethod, Protocol};
        let p = provider_listable(
            Protocol::OpenAiChat,
            Some("http://localhost:11434/v1"),
            vec![AuthMethod::None],
        );
        assert!(provider_can_list_models(&p));
    }

    #[test]
    fn can_list_models_true_for_openai_chat_with_base_url_and_empty_auth() {
        use codypendent_providers::Protocol;
        let p = provider_listable(
            Protocol::OpenAiChat,
            Some("http://localhost:1234/v1"),
            vec![],
        );
        assert!(provider_can_list_models(&p));
    }

    #[test]
    fn can_list_models_false_without_a_base_url() {
        use codypendent_providers::{AuthMethod, Protocol};
        let p = provider_listable(
            Protocol::OpenAiChat,
            None,
            vec![AuthMethod::ApiKey {
                env: vec!["OPENAI_API_KEY".to_string()],
                header: "Authorization".to_string(),
                prefix: "Bearer ".to_string(),
            }],
        );
        assert!(!provider_can_list_models(&p));
    }

    #[test]
    fn can_list_models_false_for_a_blank_base_url() {
        use codypendent_providers::{AuthMethod, Protocol};
        let p = provider_listable(Protocol::OpenAiChat, Some("   "), vec![AuthMethod::None]);
        assert!(!provider_can_list_models(&p));
    }

    #[test]
    fn can_list_models_false_for_non_openai_chat_protocols() {
        use codypendent_providers::{AuthMethod, Protocol};
        for protocol in [Protocol::Anthropic, Protocol::GeminiNative, Protocol::Acp] {
            let p = provider_listable(
                protocol,
                Some("https://api.anthropic.com"),
                vec![AuthMethod::ApiKey {
                    env: vec!["ANTHROPIC_API_KEY".to_string()],
                    header: "x-api-key".to_string(),
                    prefix: "".to_string(),
                }],
            );
            assert!(
                !provider_can_list_models(&p),
                "protocol {protocol:?} must not list"
            );
        }
    }

    #[test]
    fn can_list_models_false_for_cloud_iam_and_oauth() {
        use codypendent_providers::{AuthMethod, Protocol};
        let cloud_iam = provider_listable(
            Protocol::OpenAiChat,
            Some("https://bedrock.example/v1"),
            vec![AuthMethod::CloudIam {
                variant: "aws_sigv4".to_string(),
                env: Default::default(),
                scopes: vec![],
            }],
        );
        assert!(!provider_can_list_models(&cloud_iam));

        let oauth = provider_listable(
            Protocol::OpenAiChat,
            Some("https://oauth.example/v1"),
            vec![AuthMethod::OAuth {
                authorize_url: "https://example.com/authorize".to_string(),
                token_url: "https://example.com/token".to_string(),
                client_id: "client".to_string(),
                scopes: vec![],
                pkce: true,
            }],
        );
        assert!(!provider_can_list_models(&oauth));
    }

    // -- models_url + parse_models_response (model-discovery, pure) -----------

    #[test]
    fn models_url_appends_models_without_doubling_the_version() {
        // The base_url already carries its version segment; the list route is its
        // sibling `/models`, never `/v1/models`.
        assert_eq!(
            models_url("https://api.groq.com/openai/v1"),
            "https://api.groq.com/openai/v1/models"
        );
        assert_eq!(
            models_url("http://localhost:11434/v1"),
            "http://localhost:11434/v1/models"
        );
        // A non-`/v1` base (z.ai) must not be forced to `/v1`.
        assert_eq!(
            models_url("https://api.z.ai/api/paas/v4"),
            "https://api.z.ai/api/paas/v4/models"
        );
        // A trailing slash is trimmed so the join is exact.
        assert_eq!(
            models_url("http://localhost:1234/v1/"),
            "http://localhost:1234/v1/models"
        );
    }

    #[test]
    fn parse_models_response_extracts_ids_from_the_openai_shape() {
        let body = r#"{"object":"list","data":[{"id":"llama-3.1-8b"},{"id":"llama-3.3-70b"}]}"#;
        assert_eq!(
            parse_models_response(body).expect("parse"),
            vec!["llama-3.1-8b".to_string(), "llama-3.3-70b".to_string()]
        );
    }

    #[test]
    fn parse_models_response_skips_blank_and_dedups_preserving_order() {
        let body = r#"{"data":[{"id":"a"},{"id":"  "},{"id":""},{"id":"a"},{"id":"b"}]}"#;
        assert_eq!(
            parse_models_response(body).expect("parse"),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn parse_models_response_errors_on_an_empty_list() {
        let body = r#"{"object":"list","data":[]}"#;
        let err = parse_models_response(body).expect_err("empty list must be an error");
        assert!(err.contains("no models"), "reason: {err}");
    }

    #[test]
    fn parse_models_response_errors_when_the_data_key_is_absent() {
        // `data` is `#[serde(default)]`, so a body missing the key entirely is
        // still valid JSON (unlike a malformed body) — it must fall through to
        // the same "no models" error as an explicit empty list, not panic or
        // silently succeed with an empty `Vec`.
        let body = r#"{"object":"list"}"#;
        let err = parse_models_response(body).expect_err("missing data key must be an error");
        assert!(err.contains("no models"), "reason: {err}");
    }

    #[test]
    fn parse_models_response_errors_on_a_malformed_body() {
        assert!(parse_models_response("not json at all").is_err());
        assert!(parse_models_response("").is_err());
    }
}
