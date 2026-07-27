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

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use codypendent_knowledge::{
    db as knowledge_db, BlockContent, CapabilityRequest, CodeEdge, CodeRelation, CollaborationMode,
    DocumentAuthor, DocumentBlock, DocumentReplica, DocumentStore, EvidenceKind, EvidenceRef,
    KnowledgeDocument, MemoryClass, MemoryRecord, MemoryStore, Registry, RegistryItem,
    RegistryItemKind, RegistryStatus, RiskClass, Scope, Suggestion, SuggestionStatus,
    SuggestionStore, TrustTier,
};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{
    read_envelope, write_envelope, Catchup, ClientId, ClientRole, CodeNodeId, Command, CommandBody,
    CommandId, DocumentEditLease, DocumentId, DocumentSync, Envelope, ModelId, Payload,
    RepositoryId, SessionEvent, SessionId, Subscription, WorkspaceId,
};
use codypendent_tui::{
    map_event, reduce, render, Action, AppState, BlackboardItemCard, DocBlockView, DocCard,
    DocSuggestionView, GraphEdgeCard, Intent, MemoryCard, ModelCard, ModelLocationLabel,
    ProviderCard, SkillCard, TerminalGuard, Theme, WorkflowNodeCard,
};
use crossterm::event::Event as CrosstermEvent;
use serde::{Deserialize, Serialize};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;

use crate::commands;
use crate::connection::Connection;

/// How often the loop wakes with a [`Action::Tick`] for spinner / elapsed-timer
/// animation when nothing else is happening (5 fps — cheap, and the loop redraws
/// immediately on any real event anyway).
const TICK: Duration = Duration::from_millis(200);

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
) -> anyhow::Result<()> {
    // Crash logger, installed FIRST — before repo validation, the daemon
    // connection, and (much later) `TerminalGuard::enter` — so a panic
    // anywhere in this function's lifetime is diagnosable, and so it fires
    // before the guard's `Drop` tears down the alternate screen (see the
    // module docs above `install_crash_hook` for the full motivation).
    install_crash_hook(paths);

    let repo = repo
        .canonicalize()
        .with_context(|| format!("{}: not a valid, accessible directory", repo.display()))?;
    if !repo.is_dir() {
        bail!("{}: not a directory", repo.display());
    }

    // STEP 6.6 wiring: terminal color-depth detection (NO_COLOR/COLORTERM/TERM)
    // with a manual override that always wins, replacing the old hardcoded
    // `Theme::dark()`.
    let theme = crate::theme_select::resolve_theme(paths, theme_override.as_deref())?;

    commands::ensure_daemon(paths).await?;
    let mut conn = Connection::connect(&paths.socket_path).await?;
    let mut store = SessionStore::load(paths);
    let resume = store
        .resume_token
        .clone()
        .map(codypendent_protocol::ResumeToken);
    let hello = conn
        .handshake("codypendent-tui", env!("CARGO_PKG_VERSION"), resume)
        .await?;
    // Store the daemon-issued token so the NEXT launch resumes this client
    // identity (best-effort; an absent token just means a fresh identity).
    if let Some(token) = hello.resume_token {
        store.resume_token = Some(token.0);
        store.save(paths);
    }

    let (session_id, workspace_id, catchup) =
        resolve_or_create_session(&mut conn, &mut store, paths, &repo).await?;

    // Seed the state from catch-up, then from any live event that outraced the
    // attach reply and was buffered during setup — both before the loop reads a
    // single new frame, so no event is dropped or reordered.
    let mut state = AppState::new();
    let attach_watermark = fold_catchup(&mut state, catchup);
    let (read_half, write_half, pending, client_id) = conn.into_split();
    for envelope in pending {
        if let Payload::Event(event) = envelope.payload {
            reduce(&mut state, Action::DaemonEvent(Box::new(event)));
        }
    }

    // STEP 2.6 + Phase 4 client wiring: seed the Skill Studio, memory browser,
    // Docs Studio, and code-graph edge-inspector projections. This reads the
    // knowledge fabric's authoritative rows directly from SQLite (WAL allows
    // concurrent reads alongside the daemon) and maps them into the TUI's plain
    // projection structs — the one place the two worlds meet, done here (not in
    // the pure TUI crate, which never depends on `codypendent-knowledge`). A read
    // failure logs and continues with empty lists; it never fails the TUI. Done
    // before entering the terminal so any diagnostic reaches a cooked screen.
    let projections = load_knowledge(paths, workspace_id, &repo).await;
    state.skills = projections.skills;
    state.memories = projections.memories;
    state.docs = projections.docs;
    state.edges = projections.edges;
    state.blackboard = projections.blackboard;
    // MP1: seed the model-picker projection (models.toml + any measured
    // profile from `model_profiles`), exactly like the projections above.
    state.models = load_model_cards(paths).await;
    // Task 8: seed the provider-catalog picker projection (the built-in
    // catalog + any user `providers.toml`), exactly like `load_model_cards`.
    state.providers = load_provider_cards(paths);
    // Phase 5 STEP 5.2 + T8: seed the workflow-graph view by compiling the
    // repository's declared workflow manifests, then overlay each workflow's
    // LATEST durable run — its per-node live state, measured cost, and
    // failure/block reason — from the knowledge db (WAL allows a concurrent read
    // alongside the daemon). A malformed manifest logs and is skipped; a db-open
    // failure degrades to the compiled (pre-run) view — neither fails the TUI.
    {
        let overlay_pool = knowledge_db::open(&paths.data_dir.join("codypendent.db"))
            .await
            .ok();
        state.workflow = load_workflows(&repo, overlay_pool.as_ref()).await;
    }

    // A persistent read pool for live document editing (Phase 4 STEP 4.3): the
    // event loop seeds a document's client replica from it and re-reads the
    // review rail's suggestions when a sync arrives. WAL mode lets this read
    // concurrently with the daemon. `None` on failure — document editing then
    // degrades to converging from live syncs alone (no seed, empty review rail).
    let docs_pool = knowledge_db::open(&paths.data_dir.join("codypendent.db"))
        .await
        .ok();

    // Wire the two socket tasks. Start them before the terminal so no live
    // event or heartbeat is missed during setup.
    let (out_tx, out_rx) = mpsc::channel::<Envelope>(256);
    let (event_tx, mut event_rx) = mpsc::channel::<ReaderSignal>(256);
    // A second sender clone for the model-discovery query tasks, which feed
    // `ReaderSignal::ProviderModels` back into the same loop (the reader task
    // owns the first clone).
    let query_tx = event_tx.clone();
    let reader = tokio::spawn(read_loop(read_half, event_tx, out_tx.clone(), client_id));
    let writer = tokio::spawn(write_loop(write_half, out_rx));

    // Enter raw mode + the alternate screen. RAII restores the terminal on any
    // exit path, including a panic mid-loop. Done before spawning the input
    // bridge so it never reads a keystroke in cooked mode, and so a non-TTY
    // error returns here without a stray thread to wind down.
    let mut guard = TerminalGuard::enter().context(
        "the interactive TUI needs a terminal (a TTY); for headless use run \
         `codypendent run --jsonl` instead",
    )?;

    let (input_tx, mut input_rx) = mpsc::channel::<CrosstermEvent>(256);
    let input_running = Arc::new(AtomicBool::new(true));
    spawn_input_thread(input_tx, Arc::clone(&input_running));

    let (mut width, _) = crossterm::terminal::size().unwrap_or((80, 24));

    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let repository = repo.to_string_lossy().into_owned();
    let result = event_loop(
        &mut guard,
        &theme,
        &mut state,
        &mut width,
        &mut event_rx,
        query_tx,
        &mut input_rx,
        &mut ticker,
        &out_tx,
        client_id,
        session_id,
        &repository,
        attach_watermark,
        docs_pool.clone(),
        &mut store,
        paths,
    )
    .await;

    // Teardown: stop the input thread, restore the terminal *before* any trailing
    // error text reaches the (now cooked) screen, then wind down the socket tasks.
    input_running.store(false, Ordering::Relaxed);
    drop(guard);
    drop(out_tx);
    reader.abort();
    writer.abort();
    if let Some(pool) = docs_pool {
        pool.close().await;
    }
    result
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
        },
    );
    let _ = out_tx.send(attach).await;
}

/// The render/reduce/dispatch loop. Broken out from [`run`] so the setup and
/// teardown read linearly and the borrow of every loop input is explicit.
#[allow(clippy::too_many_arguments)]
async fn event_loop(
    guard: &mut TerminalGuard,
    theme: &Theme,
    state: &mut AppState,
    width: &mut u16,
    event_rx: &mut mpsc::Receiver<ReaderSignal>,
    query_tx: mpsc::Sender<ReaderSignal>,
    input_rx: &mut mpsc::Receiver<CrosstermEvent>,
    ticker: &mut tokio::time::Interval,
    out_tx: &mpsc::Sender<Envelope>,
    client_id: ClientId,
    session_id: SessionId,
    repository: &str,
    attach_watermark: u64,
    docs_pool: Option<sqlx::SqlitePool>,
    // Task 5 (continuous-session plan): only touched by the "New Conversation"
    // exit path below — everywhere else in the loop is unchanged.
    store: &mut SessionStore,
    paths: &RuntimePaths,
) -> anyhow::Result<()> {
    guard
        .terminal_mut()
        .draw(|frame| render(frame, state, theme))?;

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

    loop {
        // A CRDT sync needs an async merge (+ a suggestion re-read) that cannot run
        // inside the `select!` arm, so the arm stashes it here and the loop body
        // folds it just after.
        let mut pending_sync: Option<Box<DocumentSync>> = None;
        let action = tokio::select! {
            signal = event_rx.recv() => match signal {
                Some(ReaderSignal::Event(event)) => {
                    match tracker.on_event(*event, Instant::now()) {
                        GapAction::Ignore => Action::NoOp,
                        GapAction::Apply(event) => Action::DaemonEvent(event),
                        GapAction::Reattach { last_seen_sequence } => {
                            // Re-attach with the *grown* subscription set (Phase 4
                            // STEP 4.3) so a gap-repair preserves the Document
                            // subscriptions this client added while editing.
                            send_reattach(
                                out_tx,
                                client_id,
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
                Some(ReaderSignal::ProviderModels { provider_id, result }) => match result {
                    Ok(models) => Action::ProviderModelsLoaded { provider_id, models },
                    Err(reason) => Action::ProviderModelsFailed { provider_id, reason },
                },
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
                            out_tx,
                            client_id,
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
            },
            input = input_rx.recv() => match input {
                // Track width for mouse-column → pane mapping; the draw below
                // re-reads the real size, so a resize just needs a redraw.
                Some(CrosstermEvent::Resize(w, _)) => { *width = w; Action::NoOp }
                Some(event) => map_event(&event, state.input_mode(), *width, &state.hit_map.borrow()),
                None => return Ok(()), // input bridge ended
            },
            _ = ticker.tick() => {
                // FP-2b: a repair whose catch-up reply never arrived (the
                // daemon's fan-out drops spans under lag) must not wedge the
                // client in `repairing` forever — once the deadline passes,
                // re-attach afresh to re-drive the catch-up.
                if let Some(last_seen_sequence) = tracker.on_tick(Instant::now()) {
                    send_reattach(
                        out_tx,
                        client_id,
                        session_id,
                        last_seen_sequence,
                        &subscriptions,
                    )
                    .await;
                }
                Action::Tick
            }
        };

        // Fold a merged document sync (its async merge could not run in the arm).
        if let Some(sync) = pending_sync.take() {
            if let Some(synced) =
                merge_document_sync(&mut replicas, docs_pool.as_ref(), *sync).await
            {
                reduce(state, synced);
            }
        }

        reduce(state, action);

        for intent in state.drain_outbox() {
            // `AddModel` is the one client-only intent: apply it locally (models.toml
            // + auth.json) and skip the daemon-command mapping entirely.
            if let Intent::AddModel {
                display_id,
                provider_id,
                model,
                api_key,
            } = &intent
            {
                let key = api_key.as_ref().map(|k| k.0.as_str());
                match write_add_model(paths, display_id, provider_id, model, key) {
                    Ok(()) => {
                        // Re-seed the model picker so the new model shows immediately.
                        state.models = load_model_cards(paths).await;
                        reduce(state, Action::Notice(format!("added model {display_id}")));
                    }
                    Err(error) => {
                        reduce(
                            state,
                            Action::Notice(format!("could not add model: {error}")),
                        );
                    }
                }
                continue;
            }
            // `QueryProviderModels` is the other client-only intent (model
            // discovery): resolve the catalog provider's base_url + first
            // api-key header/prefix, then spawn the `<base_url>/models` GET off
            // the UI thread and feed the result back as
            // `ReaderSignal::ProviderModels`. Never a daemon command. The spawned
            // task owns the key for the request and drops it — it is never sent
            // back.
            if let Intent::QueryProviderModels {
                provider_id,
                api_key,
            } = &intent
            {
                use codypendent_providers::{AuthMethod, Catalog};
                let catalog =
                    Catalog::load_with_user_overrides(&paths.data_dir.join("providers.toml"))
                        .unwrap_or_else(|_| Catalog::builtin());
                let (base_url, header, prefix) = match catalog.get(provider_id) {
                    Some(provider) => {
                        let base = provider.base_url.clone().unwrap_or_default();
                        let (header, prefix) = match provider.auth.first() {
                            Some(AuthMethod::ApiKey { header, prefix, .. }) => {
                                (header.clone(), prefix.clone())
                            }
                            _ => ("Authorization".to_string(), "Bearer ".to_string()),
                        };
                        (base, header, prefix)
                    }
                    None => (
                        String::new(),
                        "Authorization".to_string(),
                        "Bearer ".to_string(),
                    ),
                };
                let provider_id = provider_id.clone();
                let key = api_key.as_ref().map(|k| k.0.clone());
                let tx = query_tx.clone();
                tokio::spawn(async move {
                    let result =
                        query_provider_models(&base_url, &header, &prefix, key.as_deref()).await;
                    let _ = tx
                        .send(ReaderSignal::ProviderModels {
                            provider_id,
                            result,
                        })
                        .await;
                });
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
                        client_id,
                        CommandBody::AttachSession {
                            session_id,
                            last_seen_sequence: Some(tracker.last_seen()),
                            subscriptions: subscriptions.clone(),
                            requested_role: ClientRole::Controller,
                        },
                    );
                    if out_tx.send(attach).await.is_err() {
                        return Ok(());
                    }
                }
            }
            let envelope =
                command_envelope(client_id, intent_to_command(intent, session_id, repository));
            if out_tx.send(envelope).await.is_err() {
                return Ok(()); // writer gone → connection is down; leave cleanly
            }
        }

        if state.should_detach || state.session_closed {
            // Task 5 (continuous-session plan): "New Conversation" cannot swap
            // this connection to a new session live (see
            // `AppState::start_new_conversation`'s doc comment) — instead it
            // forgets this repository's remembered session before the TUI
            // ends, so `resolve_or_create_session` cannot resume it on the
            // next launch and creates a genuinely fresh, unseeded one. A plain
            // detach (`start_new_conversation` false) leaves the mapping
            // alone, exactly as before this task.
            if state.start_new_conversation {
                store.sessions.remove(repository);
                store.save(paths);
            }
            return Ok(());
        }

        guard
            .terminal_mut()
            .draw(|frame| render(frame, state, theme))?;
    }
}

/// What the reader task forwards to the loop.
enum ReaderSignal {
    /// A live session event to fold into state (boxed: it is a large payload and
    /// every other message here is tiny).
    Event(Box<SessionEvent>),
    /// The daemon rejected a command this TUI sent (code + message). Surfaced
    /// as a transient status notice — silence here meant a rejected StartRun
    /// showed the user nothing at all.
    Rejected { code: String, message: String },
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
    /// A provider's fetched model list (model-discovery): the result of the
    /// spawned `<base_url>/models` GET, keyed by `provider_id`. Mapped by the
    /// loop's `select!` to `Action::ProviderModelsLoaded` (Ok) /
    /// `ProviderModelsFailed` (Err). Carries NO key.
    ProviderModels {
        provider_id: String,
        result: Result<Vec<String>, String>,
    },
    /// The daemon closed the connection.
    Closed,
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
                Payload::DocumentSync(sync) => {
                    if event_tx
                        .send(ReaderSignal::DocumentSync(Box::new(sync)))
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

/// Bridge blocking `crossterm` input into the async loop on a dedicated OS
/// thread. Polls with a short timeout so it observes `running` going false
/// promptly; sends each event over `tx` until the loop drops the receiver.
fn spawn_input_thread(tx: mpsc::Sender<CrosstermEvent>, running: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while running.load(Ordering::Relaxed) {
            match crossterm::event::poll(Duration::from_millis(100)) {
                Ok(true) => match crossterm::event::read() {
                    Ok(event) => {
                        if tx.blocking_send(event).is_err() {
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
        // `AddModel` and `QueryProviderModels` are CLIENT-ONLY intents applied
        // locally by the harness (see the drain loop's interceptions); neither
        // becomes a daemon command, so these mappings are never reached.
        Intent::AddModel { .. } => unreachable!(
            "AddModel is applied locally by the harness (write_add_model), never sent to the daemon"
        ),
        Intent::QueryProviderModels { .. } => unreachable!(
            "QueryProviderModels is applied locally by the harness (background GET), never sent to the daemon"
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
fn write_add_model(
    paths: &RuntimePaths,
    display_id: &str,
    provider_id: &str,
    model: &str,
    api_key: Option<&str>,
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
    let catalog = Catalog::load_with_user_overrides(&data_dir.join("providers.toml"))
        .unwrap_or_else(|_| Catalog::builtin());
    let base_url = catalog
        .get(provider_id)
        .and_then(|p| p.base_url.clone())
        .unwrap_or_default();

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
        provider: "openai-compatible".to_string(),
        base_url,
        model: model.to_string(),
        api_key_env: String::new(),
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
        auth.save(data_dir)
            .with_context(|| format!("writing {}", data_dir.join("auth.json").display()))?;
    }
    Ok(())
}

/// The provider's OpenAI-compatible model-list URL: `<base_url>/models`. The
/// catalog `base_url` already carries its version segment (`…/v1`, `…/v4`, …),
/// so the list route is its sibling `/models` — never `/v1/models` (which would
/// double the version). A trailing slash is trimmed so the join is exact.
fn models_url(base_url: &str) -> String {
    format!("{}/models", base_url.trim_end_matches('/'))
}

/// Parse an OpenAI/Ollama `/models` response body (`{ "object": "list", "data":
/// [ { "id": "…" }, … ] }`) into the model ids: trim each, skip blank/missing,
/// dedup preserving order. An empty result is an `Err` so the reducer's failure
/// arm routes to the free-text fallback uniformly. A pure function over the body
/// string — the network GET is in `query_provider_models` — so it is directly
/// unit-testable. The error strings are generic and never carry a key.
fn parse_models_response(body: &str) -> Result<Vec<String>, String> {
    #[derive(serde::Deserialize)]
    struct ModelsResponse {
        #[serde(default)]
        data: Vec<ModelEntry>,
    }
    #[derive(serde::Deserialize)]
    struct ModelEntry {
        #[serde(default)]
        id: String,
    }
    let parsed: ModelsResponse = serde_json::from_str(body)
        .map_err(|_| "the provider returned an unexpected response".to_string())?;
    let mut ids: Vec<String> = Vec::new();
    for entry in parsed.data {
        let id = entry.id.trim().to_string();
        if id.is_empty() || ids.contains(&id) {
            continue;
        }
        ids.push(id);
    }
    if ids.is_empty() {
        return Err("provider returned no models".to_string());
    }
    Ok(ids)
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
) -> Result<Vec<String>, String> {
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
        | Intent::MutateDocument { document_id, .. } => Some(*document_id),
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
        .send_command(CommandBody::CreateSession { workspace, title })
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
    blackboard: Vec<BlackboardItemCard>,
}

/// The cap on edges surfaced in the inspector. A large repository's graph can
/// carry thousands of edges; the read-only inspector shows the first
/// [`MAX_INSPECTOR_EDGES`] (oldest first) and logs when it truncates — never a
/// silent cut.
const MAX_INSPECTOR_EDGES: usize = 500;

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
) -> KnowledgeProjections {
    let empty = || KnowledgeProjections {
        skills: Vec::new(),
        memories: Vec::new(),
        docs: Vec::new(),
        edges: Vec::new(),
        blackboard: Vec::new(),
    };

    let database_path = paths.data_dir.join("codypendent.db");
    let pool = match knowledge_db::open(&database_path).await {
        Ok(pool) => pool,
        Err(error) => {
            eprintln!(
                "codypendent: knowledge views unavailable \
                 (opening {}: {error})",
                database_path.display()
            );
            return empty();
        }
    };

    let skills = match Registry::new().list(&pool).await {
        Ok(items) => items.iter().map(skill_card).collect(),
        Err(error) => {
            eprintln!("codypendent: could not list registry items: {error}");
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
            eprintln!("codypendent: could not query memories: {error}");
            Vec::new()
        }
    };

    let docs = load_docs(&pool, &scopes).await;
    let edges = load_edges(&pool, repository).await;
    // Phase 5 STEP 5.3: the blackboard artifacts on the active workflow runs. The
    // workflow tables share this database (the migrations are workspace-wide), so
    // the same pool serves them; empty until a run posts artifacts.
    let blackboard = load_blackboard(&pool).await;

    pool.close().await;
    KnowledgeProjections {
        skills,
        memories,
        docs,
        edges,
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
/// empty picker (with a stderr note); an unopenable database or a profile-list
/// failure degrades every model to its **id-only fallback** (every badge
/// absent) since profiles are best-effort enrichment, not the selectable list
/// itself.
async fn load_model_cards(paths: &RuntimePaths) -> Vec<ModelCard> {
    use codypendent_daemon::model_profiles::ModelProfileStore;
    use codypendent_runtime::models::load_models;

    let models_path = paths.data_dir.join("models.toml");
    let configs = match load_models(&models_path) {
        Ok(configs) => configs,
        Err(error) => {
            eprintln!(
                "codypendent: model picker unavailable (reading {}: {error})",
                models_path.display()
            );
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
            eprintln!(
                "codypendent: model profiles unavailable (opening {}: {error}); \
                 models still list, id-only",
                database_path.display()
            );
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
                eprintln!("codypendent: could not list model profiles: {error}");
            }
        }
    }
    if let Some(pool) = pool {
        pool.close().await;
    }

    configs
        .into_iter()
        .map(|config| model_card(config, &profiles))
        .collect()
}

/// Map one configured [`ModelConfig`](codypendent_runtime::models::ModelConfig)
/// into a [`ModelCard`], enriching it with its measured profile (matched by
/// `(id, base_url)`) when `profiles` has one — an id-only fallback (every
/// badge `None`) otherwise, so an unprofiled model still selects, just without
/// badges.
fn model_card(
    config: codypendent_runtime::models::ModelConfig,
    profiles: &HashMap<(ModelId, String), codypendent_routing::ModelProfile>,
) -> ModelCard {
    let profile = profiles.get(&(config.id.clone(), config.base_url.clone()));
    ModelCard {
        id: config.id,
        provider: config.provider,
        location: profile.map(|profile| {
            if profile.is_local() {
                ModelLocationLabel::Local
            } else {
                ModelLocationLabel::Hosted
            }
        }),
        cost_per_1k_usd: profile.map(|profile| profile.performance.cost_per_1k_tokens_usd),
        context_tokens: profile.and_then(|profile| profile.capabilities.context_tokens),
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
/// to the built-ins alone, with a stderr note.
fn load_provider_cards(paths: &RuntimePaths) -> Vec<ProviderCard> {
    use codypendent_providers::{AuthMethod, Catalog};

    let providers_path = paths.data_dir.join("providers.toml");
    let catalog = match Catalog::load_with_user_overrides(&providers_path) {
        Ok(catalog) => catalog,
        Err(error) => {
            eprintln!("codypendent: provider catalog fell back to built-ins ({error})");
            Catalog::builtin()
        }
    };
    catalog
        .providers()
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
        })
        .collect()
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
/// [`DocCard`]. A per-document read failure logs and skips that document; the
/// browser degrades to what it could load rather than failing.
async fn load_docs(pool: &sqlx::SqlitePool, scopes: &[Scope]) -> Vec<DocCard> {
    let doc_store = DocumentStore::new();
    let suggestion_store = SuggestionStore::new();
    let summaries = match doc_store.list(pool, scopes).await {
        Ok(summaries) => summaries,
        Err(error) => {
            eprintln!("codypendent: could not list documents: {error}");
            return Vec::new();
        }
    };

    let mut docs = Vec::with_capacity(summaries.len());
    for summary in summaries {
        let document = match doc_store.snapshot_document(pool, summary.id).await {
            Ok(Some(document)) => document,
            Ok(None) => continue,
            Err(error) => {
                eprintln!(
                    "codypendent: could not load document {}: {error}",
                    summary.id
                );
                continue;
            }
        };
        let suggestions = suggestion_store
            .pending(pool, summary.id)
            .await
            .unwrap_or_else(|error| {
                eprintln!(
                    "codypendent: could not load suggestions for {}: {error}",
                    summary.id
                );
                Vec::new()
            });
        docs.push(doc_card(&document, &suggestions));
    }
    docs
}

/// Project this repository's code-graph edges into [`GraphEdgeCard`]s, resolving
/// each endpoint node id to its qualified name. Bounded by
/// [`MAX_INSPECTOR_EDGES`] with a note when it truncates.
async fn load_edges(pool: &sqlx::SqlitePool, repository: RepositoryId) -> Vec<GraphEdgeCard> {
    use codypendent_knowledge::codegraph;

    let names: HashMap<CodeNodeId, String> = match codegraph::nodes(pool, repository).await {
        Ok(nodes) => nodes
            .into_iter()
            .map(|node| (node.id, node.key.qualified_name))
            .collect(),
        Err(error) => {
            eprintln!("codypendent: could not load code-graph nodes: {error}");
            HashMap::new()
        }
    };

    let mut edges = match codegraph::edges(pool, repository).await {
        Ok(edges) => edges,
        Err(error) => {
            eprintln!("codypendent: could not load code-graph edges: {error}");
            return Vec::new();
        }
    };
    if edges.len() > MAX_INSPECTOR_EDGES {
        eprintln!(
            "codypendent: code graph has {} edges; the inspector shows the first {MAX_INSPECTOR_EDGES}",
            edges.len()
        );
        edges.truncate(MAX_INSPECTOR_EDGES);
    }
    edges.iter().map(|edge| edge_card(edge, &names)).collect()
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

/// Map a [`CodeEdge`] into the inspector's [`GraphEdgeCard`], resolving each
/// endpoint node id to its qualified name via `names` (falling back to the id
/// when a node is not in the map). Carries the evidence + revision the Phase 4
/// exit criterion requires the inspector to expose.
fn edge_card(edge: &CodeEdge, names: &HashMap<CodeNodeId, String>) -> GraphEdgeCard {
    let name = |id: &CodeNodeId| {
        names
            .get(id)
            .cloned()
            .unwrap_or_else(|| format!("node {id}"))
    };
    GraphEdgeCard {
        from: name(&edge.from),
        to: name(&edge.to),
        relation: code_relation_label(edge.relation).to_owned(),
        confidence: edge.confidence,
        evidence_kind: evidence_kind_label(edge.evidence_kind).to_owned(),
        evidence: edge
            .evidence
            .as_ref()
            .map_or_else(|| "(none)".to_owned(), evidence_source),
        revision: edge.revision.0.clone(),
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
async fn load_workflows(repo: &Path, pool: Option<&sqlx::SqlitePool>) -> Vec<WorkflowNodeCard> {
    let dir = repo.join(".codypendent").join("workflows");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        // A repository with no workflows directory is the common case — an empty
        // view, not an error.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            eprintln!(
                "codypendent: workflow view unavailable (reading {}: {error})",
                dir.display()
            );
            return Vec::new();
        }
    };

    // Collect and sort the manifest paths so the view order is deterministic.
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

    let mut cards = Vec::new();
    for path in files {
        let yaml = match std::fs::read_to_string(&path) {
            Ok(yaml) => yaml,
            Err(error) => {
                eprintln!(
                    "codypendent: skipping workflow {} ({error})",
                    path.display()
                );
                continue;
            }
        };
        match codypendent_workflow::compile_yaml(&yaml) {
            Ok(compiled) => {
                let label = format!("{} v{}", compiled.id, compiled.version);
                // Overlay the latest durable run's per-node state/cost/error, when
                // one exists and a pool is available.
                let records = match pool {
                    Some(pool) => latest_run_node_records(pool, &compiled.id).await,
                    None => HashMap::new(),
                };
                cards.extend(
                    compiled
                        .nodes
                        .iter()
                        .map(|node| workflow_node_card(&label, node, records.get(&node.id))),
                );
            }
            Err(error) => {
                eprintln!(
                    "codypendent: skipping workflow {} (does not compile: {error})",
                    path.display()
                );
            }
        }
    }

    if cards.len() > MAX_WORKFLOW_NODES {
        eprintln!(
            "codypendent: workflow view showing the first {MAX_WORKFLOW_NODES} of {} nodes",
            cards.len()
        );
        cards.truncate(MAX_WORKFLOW_NODES);
    }
    cards
}

/// The most recent durable run's node records for `workflow_id`, keyed by node id
/// — the overlay [`load_workflows`] applies so the graph view shows a run's live
/// state, MEASURED cost, and failure/block reason. An empty map when no run exists
/// or a read fails: the view then shows the compiled defaults, never a stale one.
async fn latest_run_node_records(
    pool: &sqlx::SqlitePool,
    workflow_id: &str,
) -> HashMap<String, codypendent_workflow::WorkflowNodeRecord> {
    let run_id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM workflow_runs WHERE workflow_id = ? \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(workflow_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let Some(run_id) = run_id else {
        return HashMap::new();
    };
    match codypendent_workflow::WorkflowStore::new()
        .snapshot(pool, &run_id)
        .await
    {
        Ok(Some(snapshot)) => snapshot
            .nodes
            .into_iter()
            .map(|node| (node.node_id.clone(), node))
            .collect(),
        _ => HashMap::new(),
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
    workflow: &str,
    node: &codypendent_workflow::CompiledNode,
    record: Option<&codypendent_workflow::WorkflowNodeRecord>,
) -> WorkflowNodeCard {
    use codypendent_workflow::{ApprovalPolicy, NodeAction, WorkspaceMode};

    let dash = || "\u{2014}".to_owned(); // "—"

    // Live state / measured cost / failure reason from a durable run record, else
    // the compiled node's pre-run defaults.
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
        workflow: workflow.to_owned(),
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
/// query failure logs and skips that run rather than failing the view.
async fn load_blackboard(pool: &sqlx::SqlitePool) -> Vec<BlackboardItemCard> {
    use codypendent_workflow::{BlackboardStore, WorkflowStore};

    let runs = match WorkflowStore::new().list_incomplete_runs(pool).await {
        Ok(runs) => runs,
        Err(error) => {
            eprintln!("codypendent: could not list workflow runs: {error}");
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
                    .map(|item| blackboard_item_card(&run_label, item)),
            ),
            Err(error) => {
                eprintln!(
                    "codypendent: could not query the blackboard for run {}: {error}",
                    run.id
                );
            }
        }
        if cards.len() >= MAX_BLACKBOARD_ITEMS {
            eprintln!(
                "codypendent: blackboard view showing the first {MAX_BLACKBOARD_ITEMS} artifacts"
            );
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
    run: &str,
    item: &codypendent_workflow::BlackboardItem,
) -> BlackboardItemCard {
    BlackboardItemCard {
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

fn code_relation_label(relation: CodeRelation) -> &'static str {
    match relation {
        CodeRelation::Contains => "contains",
        CodeRelation::Defines => "defines",
        CodeRelation::Imports => "imports",
        CodeRelation::References => "references",
        CodeRelation::Calls => "calls",
        CodeRelation::Implements => "implements",
        CodeRelation::Extends => "extends",
        CodeRelation::Reads => "reads",
        CodeRelation::Writes => "writes",
        CodeRelation::Mutates => "mutates",
        CodeRelation::Returns => "returns",
        CodeRelation::Accepts => "accepts",
        CodeRelation::Tests => "tests",
        CodeRelation::Configures => "configures",
        CodeRelation::Serializes => "serializes",
        CodeRelation::DependsOn => "depends-on",
        CodeRelation::GeneratedFrom => "generated-from",
    }
}

fn evidence_kind_label(kind: EvidenceKind) -> &'static str {
    match kind {
        EvidenceKind::SyntaxInferred => "syntax_inferred",
        EvidenceKind::LspResolved => "lsp_resolved",
        EvidenceKind::CompilerResolved => "compiler_resolved",
        EvidenceKind::RuntimeObserved => "runtime_observed",
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
    /// `(model_id, endpoint)`), and falls back to an id-only card (every badge
    /// `None`) when it does not — `models.toml` is the authoritative
    /// selectable list, so an unprofiled model still appears.
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
        };
        let unprofiled = codypendent_runtime::models::ModelConfig {
            id: ModelId("local-default".into()),
            provider: "openai-compatible".to_owned(),
            base_url: "http://localhost:11434/v1".to_owned(),
            model: "qwen2.5-coder:14b".to_owned(),
            api_key_env: String::new(),
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

        let hosted_card = model_card(hosted, &profiles);
        assert_eq!(hosted_card.id, ModelId("hosted-default".into()));
        assert_eq!(hosted_card.provider, "openai-compatible");
        assert_eq!(hosted_card.location, Some(ModelLocationLabel::Hosted));
        assert!(
            (hosted_card.cost_per_1k_usd.unwrap() - 0.03).abs() < 1e-9,
            "cost should round-trip: {:?}",
            hosted_card.cost_per_1k_usd
        );
        assert_eq!(hosted_card.context_tokens, Some(200_000));

        let other_endpoint_card = model_card(same_id_other_endpoint, &profiles);
        assert_eq!(
            other_endpoint_card.location, None,
            "a profile at a different endpoint must not match"
        );

        let unprofiled_card = model_card(unprofiled, &profiles);
        assert_eq!(unprofiled_card.id, ModelId("local-default".into()));
        assert_eq!(
            unprofiled_card.location, None,
            "no profile row: an id-only fallback"
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

    /// Task 5 (continuous-session plan): "New Conversation" cannot swap the
    /// live connection to a new session, so it forgets this repository's
    /// remembered mapping instead (`event_loop`'s exit check, on
    /// `state.start_new_conversation`) — the exact `sessions.remove` +
    /// `save` this test exercises. With the entry gone,
    /// `resolve_or_create_session` finds nothing to resume on the next
    /// launch and creates a genuinely fresh, unseeded session (never fakes
    /// continuity by reusing the forgotten one).
    #[test]
    fn forgetting_a_repository_removes_its_remembered_session() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = RuntimePaths::from_data_dir(tmp.path().to_path_buf());
        paths.ensure_directories().unwrap();

        let mut store = SessionStore::default();
        store.sessions.insert(
            "/repo/one".into(),
            StoredSession {
                session_id: SessionId::new(),
                workspace_id: WorkspaceId::new(),
            },
        );
        store.save(&paths);
        assert!(SessionStore::load(&paths)
            .sessions
            .contains_key("/repo/one"));

        store.sessions.remove("/repo/one");
        store.save(&paths);

        assert!(
            !SessionStore::load(&paths)
                .sessions
                .contains_key("/repo/one"),
            "the forgotten repository must not resume its old session"
        );
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
id: repair-github-check
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
            .map(|node| workflow_node_card(&label, node, None))
            .collect();

        let patch = cards.iter().find(|c| c.id == "patch").expect("patch node");
        assert_eq!(patch.workflow, "repair-github-check v1");
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
        let card = workflow_node_card(&label, node, Some(&record));
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

        // No workflows directory → an empty view, not an error.
        assert!(load_workflows(repo, None).await.is_empty());

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

        let cards = load_workflows(repo, None).await;
        assert_eq!(
            cards.len(),
            2,
            "both nodes of the good manifest, none of the broken"
        );
        assert!(cards.iter().all(|c| c.workflow == "repair-github-check v1"));
        // Nodes keep their compiled topological order.
        assert_eq!(cards[0].id, "patch");
        assert_eq!(cards[1].id, "verify");
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
        let card = blackboard_item_card("repair-github-check \u{b7} run 0192abcd", &item);
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
        let card = blackboard_item_card("run", &item);
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
