//! DR4 (pure decision) + DR5 (effectful driver) for detecting — and safely
//! reconciling — a running daemon that is a different **build** than the
//! connecting client (the daemon-auto-restart-on-version-mismatch feature).
//! See `docs/superpowers/specs/2026-07-28-daemon-auto-restart-design.md`.
//!
//! This is the SAFETY-CRITICAL crux of the feature: [`decide_restart`] must
//! never authorize a restart unless the daemon is CONFIRMED idle, and the
//! effectful driver ([`reconcile_interactive`]) re-confirms idleness under an
//! advisory lock immediately before stopping anything. Against a v1.3+ daemon
//! it then hands the FINAL idle decision to the daemon (`ShutdownIfIdle`),
//! which re-checks atomically against concurrent run admission — fully closing
//! the TOCTOU window. A legacy daemon falls back to the plain restart with a
//! narrow residual window (see [`reconcile_interactive`]).

use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{DaemonStatus, ResumeToken, ServerHello};

use crate::commands;
use crate::connection::Connection;

// ---------------------------------------------------------------------------
// DR4 — the pure restart decision. No I/O: fully unit-testable, and the one
// place the safety invariant ("never restart unless confirmed idle") lives.
// ---------------------------------------------------------------------------

/// What a connecting client should do about a (potentially mismatched)
/// daemon build, given the daemon's reported `active_run_count`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartDecision {
    /// Same build — the overwhelming common case. Continue unchanged.
    Proceed,
    /// A different build AND confirmed idle (`active_run_count == 0`) — safe
    /// to stop the old daemon and spawn a fresh one.
    Restart,
    /// A different build, but at least one run is active — NEVER restart;
    /// warn and keep working against the existing (old) daemon.
    WarnActive,
    /// A different build, but idleness could not be confirmed (the status
    /// query failed) — NEVER restart on uncertainty; warn and continue.
    WarnUnknown,
}

/// The pure decision at the heart of the daemon-auto-restart feature.
///
/// `daemon_build_id` empty is a pre-feature daemon (one that predates
/// `ServerHello`/`DaemonStatus` carrying `build_id`) and is therefore, by
/// definition, older than any client that has this field — treated as an
/// ordinary mismatch, subject to the exact same idle gate as any other
/// differing build, never a special case.
///
/// The safety invariant this function encodes: a restart is authorized ONLY
/// when the build ids differ AND `active == Some(0)`. Any doubt (`None`) or
/// any active run (`Some(n)` with `n > 0`) always resolves to a
/// warn-and-continue, never a restart.
pub fn decide_restart(
    client_build_id: &str,
    daemon_build_id: &str,
    active: Option<u64>,
) -> RestartDecision {
    if daemon_build_id == client_build_id {
        return RestartDecision::Proceed;
    }
    match active {
        Some(0) => RestartDecision::Restart,
        Some(_) => RestartDecision::WarnActive,
        None => RestartDecision::WarnUnknown,
    }
}

// ---------------------------------------------------------------------------
// The restart lock — an advisory single-flight guard over the whole
// stop/spawn/reconnect critical section, mirroring
// `crates/daemon/src/server.rs::acquire_socket`'s pidfile idiom (atomic
// `create_new` + PID, with stale-holder recovery) rather than pulling in a
// new file-locking crate.
// ---------------------------------------------------------------------------

const RESTART_LOCK_FILE_NAME: &str = "daemon-restart.lock";

fn restart_lock_path(paths: &RuntimePaths) -> PathBuf {
    paths.data_dir.join(RESTART_LOCK_FILE_NAME)
}

/// Releases the restart lock on drop — covers every return path out of
/// [`reconcile_interactive`], including an early `?` or a hard error.
struct RestartLockGuard {
    path: PathBuf,
}

impl Drop for RestartLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

enum LockAttempt {
    Acquired(RestartLockGuard),
    /// Another live client holds the lock (or the stale-liveness probe still
    /// says the holder is alive).
    HeldByOther,
}

fn claim_lock_file(path: &Path) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(std::process::id().to_string().as_bytes())
}

fn read_lock_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// `kill -0 <pid>`: true when a process with this pid exists. No new
/// dependency (no `libc`/`nix`) — this shells out to the same `kill` binary
/// the pidfile-staleness idiom would use, exactly as `std::process::Command`
/// is already used to spawn the daemon itself (`commands::daemon_command`).
/// A failure to even RUN `kill` (missing binary, sandboxing) is treated as
/// "can't tell, assume alive": a stale lock is reclaimed only on a POSITIVE
/// confirmation the holder is gone, never on doubt.
fn process_is_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(true)
}

/// Try to claim the restart lock: atomic `create_new` + this process's PID
/// (mirrors `acquire_socket`). On contention, probe the holder's liveness and
/// reclaim a stale lock (the prior holder crashed mid-restart) exactly once;
/// a live holder is reported as [`LockAttempt::HeldByOther`] for the caller
/// to poll.
fn try_acquire_restart_lock(paths: &RuntimePaths) -> anyhow::Result<LockAttempt> {
    let path = restart_lock_path(paths);
    match claim_lock_file(&path) {
        Ok(()) => Ok(LockAttempt::Acquired(RestartLockGuard { path })),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            let stale = match read_lock_pid(&path) {
                Some(pid) => !process_is_alive(pid),
                // A corrupt/unreadable lock file can never be a live holder.
                None => true,
            };
            if !stale {
                return Ok(LockAttempt::HeldByOther);
            }
            let _ = std::fs::remove_file(&path);
            match claim_lock_file(&path) {
                Ok(()) => Ok(LockAttempt::Acquired(RestartLockGuard { path })),
                // Lost the race to reclaim it — treat exactly like ordinary
                // contention (poll and converge).
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    Ok(LockAttempt::HeldByOther)
                }
                Err(error) => Err(error).context("reclaiming a stale daemon-restart lock"),
            }
        }
        Err(error) => Err(error).context("creating the daemon-restart lock"),
    }
}

/// How long the losing side of a restart-lock race polls for the winner to
/// finish before giving up (matches the 5s budget `ensure_daemon`/`stop`
/// already use elsewhere in this crate).
const LOCK_POLL_ATTEMPTS: u32 = 50;
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(100);

// ---------------------------------------------------------------------------
// DR5 — the effectful driver.
// ---------------------------------------------------------------------------

/// What [`reconcile_interactive`] actually did — for the caller's own status
/// line and for tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// Matched build, or a mismatch another client already resolved: continue
    /// on the (possibly reconnected) connection unchanged.
    Proceed,
    /// This call stopped the old daemon, spawned a fresh one, and
    /// reconnected — the connection now points at the new build.
    Restarted,
    /// A run is active (initially or discovered under the lock): did NOT
    /// restart; the caller continues on the existing daemon.
    WarnActive,
    /// Idleness could not be confirmed: did NOT restart; the caller continues
    /// on the existing daemon.
    WarnUnknown,
}

/// The I/O [`reconcile_interactive`] needs, abstracted so a unit test can
/// inject fakes with no real socket, daemon, or process — mirroring DR3's
/// injectable `restart_daemon_with` closures. [`LiveRestartOps`] is the real
/// production wiring.
#[async_trait]
pub trait RestartOps {
    /// Query the daemon's current status (its `build_id` and
    /// `active_run_count`).
    async fn status(&mut self) -> anyhow::Result<DaemonStatus>;
    /// Stop the running daemon and spawn a fresh one (DR3's
    /// `commands::restart_daemon`). Used for legacy (pre-v1.3) daemons that
    /// cannot make the idle decision themselves.
    async fn restart_daemon(&mut self) -> anyhow::Result<()>;
    /// Idle-guarded restart (DR7's `commands::restart_daemon_if_idle`): the
    /// daemon makes the final atomic idle decision and may refuse. Used when the
    /// daemon's negotiated minor is ≥ [`IDLE_SHUTDOWN_MIN_MINOR`].
    async fn restart_daemon_if_idle(&mut self) -> anyhow::Result<commands::IdleRestartOutcome>;
    /// Reconnect and re-handshake, returning the fresh `ServerHello`. On
    /// success the implementation's own connection is swapped to the new one
    /// (see [`LiveRestartOps::into_connection`]).
    async fn reconnect(&mut self) -> anyhow::Result<ServerHello>;
}

/// The interactive (TUI/attach) reconcile driver (DR5): given a detected
/// mismatch (`client_build_id != daemon_build_id`, taken from the just-
/// completed handshake's `ServerHello`), decide and — when safe — perform a
/// daemon restart.
///
/// Zero-round-trip on the common path: a matching build returns
/// [`ReconcileOutcome::Proceed`] immediately, with no call to `ops` at all.
/// Only a detected mismatch pays for a `status` query.
///
/// The safety invariant (never silently kill a live run) is enforced in
/// layers: once by the initial `decide_restart` call, again by a SECOND
/// `status` query issued only after the restart lock is held, and — for a
/// daemon that speaks protocol v1.3+ — a THIRD time atomically at the daemon
/// itself (`ShutdownIfIdle`: the daemon re-checks `active_run_count` under an
/// exclusive run-admission guard and refuses rather than dying with work in
/// flight).
///
/// For a v1.3+ daemon this fully CLOSES the TOCTOU window: a run that another
/// client starts in the sub-window between the client's recheck and the stop
/// is caught by the daemon's own atomic check, and the restart is refused
/// (falls to [`ReconcileOutcome::WarnActive`]) instead of killing it. For a
/// legacy (pre-v1.3) daemon — which cannot decode `ShutdownIfIdle` — the client
/// falls back to the plain restart gated only by the under-lock recheck, so a
/// narrow residual window remains; it is closed for good once the newly
/// installed (v1.3+) build is the one running. Either path is a strict
/// improvement over the manual `daemon stop`/`restart`, which idle-check
/// nothing.
pub async fn reconcile_interactive(
    paths: &RuntimePaths,
    client_build_id: &str,
    daemon_build_id: &str,
    ops: &mut impl RestartOps,
    warn: &mut dyn FnMut(&str),
) -> anyhow::Result<ReconcileOutcome> {
    if daemon_build_id == client_build_id {
        return Ok(ReconcileOutcome::Proceed);
    }

    // Mismatch: pay for exactly one status query to learn `active_run_count`.
    let active = ops
        .status()
        .await
        .ok()
        .map(|status| status.active_run_count);
    match decide_restart(client_build_id, daemon_build_id, active) {
        RestartDecision::Proceed => unreachable!("build ids differ on this branch"),
        RestartDecision::WarnActive => {
            warn(
                "a newer codypendent is installed; the daemon will restart automatically \
                 once the current run(s) finish, or run `codypendent daemon restart`",
            );
            Ok(ReconcileOutcome::WarnActive)
        }
        RestartDecision::WarnUnknown => {
            warn(
                "couldn't confirm the daemon is idle; not auto-restarting — run \
                 `codypendent daemon restart` if needed",
            );
            Ok(ReconcileOutcome::WarnUnknown)
        }
        RestartDecision::Restart => {
            warn("a newer codypendent is installed; restarting the daemon\u{2026}");
            restart_under_lock(paths, client_build_id, ops, warn).await
        }
    }
}

/// The lock-guarded restart critical section, split out of
/// [`reconcile_interactive`] so its two branches (acquired vs. contended) read
/// linearly.
async fn restart_under_lock(
    paths: &RuntimePaths,
    client_build_id: &str,
    ops: &mut impl RestartOps,
    warn: &mut dyn FnMut(&str),
) -> anyhow::Result<ReconcileOutcome> {
    // A lock-creation failure (disk full, permissions, a missing data dir) is
    // treated as "can't safely coordinate a restart" — degrade to a warn and
    // continue on the healthy existing daemon (spec §8), NEVER abort the whole
    // launch over a transient filesystem hiccup and never restart uncoordinated.
    let attempt = match try_acquire_restart_lock(paths) {
        Ok(attempt) => attempt,
        Err(error) => {
            warn(&format!(
                "couldn't acquire the daemon-restart lock ({error:#}); not auto-restarting — \
                 run `codypendent daemon restart` if needed"
            ));
            return Ok(ReconcileOutcome::WarnUnknown);
        }
    };
    match attempt {
        LockAttempt::HeldByOther => {
            // Another client is very likely mid-restart already; block-poll
            // briefly for it to finish, then converge (idempotent no-op).
            poll_for_matching_build(client_build_id, ops, warn).await
        }
        LockAttempt::Acquired(guard) => {
            // Re-check UNDER the lock: a run may have started, or another client
            // may have already restarted the daemon, since the first (pre-lock)
            // check. Keep the whole status — its `protocol_version` decides
            // whether the daemon can make the FINAL idle decision itself.
            let recheck_status = ops.status().await;
            let recheck = match &recheck_status {
                Ok(status) => decide_restart(
                    client_build_id,
                    &status.build_id,
                    Some(status.active_run_count),
                ),
                Err(_) => RestartDecision::WarnUnknown,
            };
            match recheck {
                RestartDecision::WarnActive => {
                    drop(guard);
                    warn(
                        "a run started while acquiring the restart lock; deferring the \
                         restart until the daemon is idle",
                    );
                    Ok(ReconcileOutcome::WarnActive)
                }
                RestartDecision::WarnUnknown => {
                    drop(guard);
                    warn(
                        "couldn't confirm the daemon is idle under the restart lock; not \
                         auto-restarting — run `codypendent daemon restart` if needed",
                    );
                    Ok(ReconcileOutcome::WarnUnknown)
                }
                RestartDecision::Proceed => {
                    // Another client already restarted it while we were
                    // acquiring the lock — idempotent no-op, just reconnect.
                    drop(guard);
                    reconnect_and_assert(client_build_id, ops).await?;
                    Ok(ReconcileOutcome::Restarted)
                }
                RestartDecision::Restart => {
                    let daemon_minor = recheck_status
                        .as_ref()
                        .map(|status| status.protocol_version.minor)
                        .unwrap_or(0);
                    if daemon_minor >= IDLE_SHUTDOWN_MIN_MINOR {
                        // The daemon understands `ShutdownIfIdle`: let IT make the
                        // final idle decision, atomically against a concurrent run
                        // admission. This CLOSES the TOCTOU window entirely — a run
                        // that starts between here and the stop is caught daemon-side
                        // and the restart is refused rather than killing it.
                        match ops
                            .restart_daemon_if_idle()
                            .await
                            .context("restarting the daemon (idle-guarded) to load the new build")?
                        {
                            commands::IdleRestartOutcome::RefusedActive(active) => {
                                drop(guard);
                                warn(&format!(
                                    "a run became active as the daemon was restarting ({active} \
                                     active); deferring until it is idle — run \
                                     `codypendent daemon restart` if needed"
                                ));
                                Ok(ReconcileOutcome::WarnActive)
                            }
                            commands::IdleRestartOutcome::Restarted => {
                                drop(guard);
                                reconnect_and_assert(client_build_id, ops).await?;
                                Ok(ReconcileOutcome::Restarted)
                            }
                        }
                    } else {
                        // A legacy (pre-v1.3) daemon can't decode `ShutdownIfIdle`.
                        // Fall back to the plain restart, gated only by the under-
                        // lock idle re-check above (the documented residual window
                        // for legacy daemons — closed once this new build is what
                        // is running).
                        ops.restart_daemon()
                            .await
                            .context("restarting the daemon to load the new build")?;
                        drop(guard);
                        reconnect_and_assert(client_build_id, ops).await?;
                        Ok(ReconcileOutcome::Restarted)
                    }
                }
            }
        }
    }
}

/// The daemon protocol minor that first understands `Payload::ShutdownIfIdle`
/// (protocol v1.3). A daemon reporting at least this minor makes the final,
/// atomic idle decision itself; an older one falls back to the plain restart.
const IDLE_SHUTDOWN_MIN_MINOR: u16 = 3;

/// Block-poll (bounded, never hangs) for the lock holder's restart to land —
/// observed as `DaemonStatus.build_id` matching `client_build_id` — then
/// reconnect. If the holder never finishes within the budget, degrade to a
/// warn rather than wait forever: the invariant is "never restart on
/// uncertainty", and by now the caller can no longer be certain what the
/// other client is doing.
async fn poll_for_matching_build(
    client_build_id: &str,
    ops: &mut impl RestartOps,
    warn: &mut dyn FnMut(&str),
) -> anyhow::Result<ReconcileOutcome> {
    for _ in 0..LOCK_POLL_ATTEMPTS {
        if let Ok(status) = ops.status().await {
            if status.build_id == client_build_id {
                reconnect_and_assert(client_build_id, ops).await?;
                return Ok(ReconcileOutcome::Restarted);
            }
        }
        tokio::time::sleep(LOCK_POLL_INTERVAL).await;
    }
    warn(
        "another client appears to be restarting the daemon, but it did not finish in time; \
         continuing without restarting — run `codypendent daemon restart` if needed",
    );
    Ok(ReconcileOutcome::WarnUnknown)
}

/// Reconnect + re-handshake and assert the fresh hello's `build_id` matches
/// `client_build_id` — a legible hard error (never a silent half-restart) if
/// it still mismatches (a stale on-disk binary, or a different `codypendent`
/// on `PATH`).
async fn reconnect_and_assert(
    client_build_id: &str,
    ops: &mut impl RestartOps,
) -> anyhow::Result<()> {
    let hello = ops
        .reconnect()
        .await
        .context("reconnecting to the daemon after a build-mismatch restart")?;
    if hello.build_id != client_build_id {
        anyhow::bail!(
            "daemon restart did not load the new build (on-disk binary may be stale, or a \
             different codypendent is on PATH); expected {client_build_id}, got {}",
            hello.build_id
        );
    }
    Ok(())
}

/// Production wiring of [`RestartOps`] over a real [`Connection`] and
/// `RuntimePaths`. Holds the live connection so [`RestartOps::reconnect`] can
/// replace it in place; the caller retrieves the final connection with
/// [`LiveRestartOps::into_connection`] once [`reconcile_interactive`] returns.
pub struct LiveRestartOps<'a> {
    paths: &'a RuntimePaths,
    conn: Connection,
    client_name: &'static str,
    resume: Option<ResumeToken>,
}

impl<'a> LiveRestartOps<'a> {
    pub fn new(
        paths: &'a RuntimePaths,
        conn: Connection,
        client_name: &'static str,
        resume: Option<ResumeToken>,
    ) -> Self {
        Self {
            paths,
            conn,
            client_name,
            resume,
        }
    }

    /// The final connection — the original one on
    /// [`ReconcileOutcome::Proceed`]/`WarnActive`/`WarnUnknown`, or the fresh
    /// post-restart one on [`ReconcileOutcome::Restarted`].
    pub fn into_connection(self) -> Connection {
        self.conn
    }
}

#[async_trait]
impl RestartOps for LiveRestartOps<'_> {
    async fn status(&mut self) -> anyhow::Result<DaemonStatus> {
        crate::client::daemon_status(&self.paths.socket_path).await
    }

    async fn restart_daemon(&mut self) -> anyhow::Result<()> {
        commands::restart_daemon(self.paths).await?;
        Ok(())
    }

    async fn restart_daemon_if_idle(&mut self) -> anyhow::Result<commands::IdleRestartOutcome> {
        commands::restart_daemon_if_idle(self.paths).await
    }

    async fn reconnect(&mut self) -> anyhow::Result<ServerHello> {
        let mut conn = Connection::connect(&self.paths.socket_path)
            .await
            .context("reconnecting to the daemon after a build-mismatch restart")?;
        let hello = conn
            .handshake(
                self.client_name,
                codypendent_protocol::BUILD_ID,
                self.resume.clone(),
            )
            .await?;
        self.conn = conn;
        Ok(hello)
    }
}

// ---------------------------------------------------------------------------
// The headless (`run --jsonl`) path: WARN-ONLY, by design (T9 scope). A
// scripted/non-interactive invocation must never have the daemon bounced out
// from under it, so this never touches the restart lock or `restart_daemon`
// at all — there is no code path here capable of restarting anything.
// ---------------------------------------------------------------------------

/// A build mismatch message for the headless path, or `None` when the builds
/// match. Pure (no I/O, no restart capability whatsoever) — the headless
/// caller's entire "reconcile" step is printing this to stderr and proceeding
/// unchanged.
pub fn headless_mismatch_warning(client_build_id: &str, daemon_build_id: &str) -> Option<String> {
    if daemon_build_id == client_build_id {
        return None;
    }
    Some(format!(
        "a newer codypendent build is installed (daemon is running {daemon_build_id:?}, this \
         client is {client_build_id:?}); continuing against the existing daemon \u{2014} \
         auto-restart is disabled for --jsonl (headless) runs. Run `codypendent daemon restart` \
         when convenient."
    ))
}

#[cfg(test)]
mod decide_restart_tests {
    use super::*;

    #[test]
    fn matching_build_ids_proceed_regardless_of_active_count() {
        assert_eq!(
            decide_restart("1.0+abc", "1.0+abc", Some(0)),
            RestartDecision::Proceed
        );
        assert_eq!(
            decide_restart("1.0+abc", "1.0+abc", Some(3)),
            RestartDecision::Proceed
        );
        assert_eq!(
            decide_restart("1.0+abc", "1.0+abc", None),
            RestartDecision::Proceed
        );
    }

    #[test]
    fn mismatch_and_idle_restarts() {
        assert_eq!(
            decide_restart("1.0+new", "1.0+old", Some(0)),
            RestartDecision::Restart
        );
    }

    #[test]
    fn mismatch_and_active_warns_without_restarting() {
        assert_eq!(
            decide_restart("1.0+new", "1.0+old", Some(1)),
            RestartDecision::WarnActive
        );
        assert_eq!(
            decide_restart("1.0+new", "1.0+old", Some(42)),
            RestartDecision::WarnActive
        );
    }

    #[test]
    fn mismatch_and_unknown_active_count_warns_without_restarting() {
        assert_eq!(
            decide_restart("1.0+new", "1.0+old", None),
            RestartDecision::WarnUnknown
        );
    }

    #[test]
    fn empty_daemon_build_id_is_a_mismatch_a_pre_feature_daemon() {
        // A daemon predating this feature never sends `build_id`, which
        // deserializes to "" (ServerHello/DaemonStatus's `#[serde(default)]`).
        // It must be treated as an ordinary mismatch, subject to the same
        // idle gate — never a special case.
        assert_eq!(
            decide_restart("1.0+new", "", Some(0)),
            RestartDecision::Restart
        );
        assert_eq!(
            decide_restart("1.0+new", "", Some(1)),
            RestartDecision::WarnActive
        );
        assert_eq!(
            decide_restart("1.0+new", "", None),
            RestartDecision::WarnUnknown
        );
    }
}

#[cfg(test)]
mod headless_warning_tests {
    use super::*;

    #[test]
    fn matching_build_ids_warn_nothing() {
        assert_eq!(headless_mismatch_warning("1.0+abc", "1.0+abc"), None);
    }

    #[test]
    fn mismatch_warns_and_never_restarts() {
        // There is no restart-capable call anywhere on this path: the whole
        // "reconcile" step for --jsonl is this pure function plus an
        // `eprintln!` at the call site (see `commands::run_over_connection`).
        let message =
            headless_mismatch_warning("1.0+new", "1.0+old").expect("a mismatch must warn");
        assert!(message.contains("1.0+new"));
        assert!(message.contains("1.0+old"));
        assert!(message.contains("daemon restart"));
    }
}

#[cfg(test)]
mod reconcile_interactive_tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    use codypendent_protocol::{DaemonInstanceId, ProtocolVersion};

    fn temp_paths(tag: &str) -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempfile::Builder::new()
            .prefix(&format!("cp-restart-{tag}-"))
            .tempdir()
            .expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        (dir, paths)
    }

    fn sample_hello(build_id: &str) -> ServerHello {
        ServerHello {
            selected_protocol: ProtocolVersion { major: 1, minor: 2 },
            daemon_version: "0.1.0".to_string(),
            daemon_instance: DaemonInstanceId::new(),
            heartbeat_interval_ms: 15_000,
            resume_token: None,
            build_id: build_id.to_string(),
        }
    }

    fn sample_status(build_id: &str, active_run_count: u64) -> DaemonStatus {
        DaemonStatus {
            daemon_version: "0.1.0".to_string(),
            protocol_version: ProtocolVersion { major: 1, minor: 2 },
            instance_id: DaemonInstanceId::new(),
            pid: 4242,
            started_at: chrono::Utc::now(),
            uptime_seconds: 1,
            boot_count: 1,
            database_path: "/dev/null".to_string(),
            socket_path: "/dev/null".to_string(),
            session_count: 0,
            build_id: build_id.to_string(),
            active_run_count,
        }
    }

    /// A [`DaemonStatus`] reporting protocol v1.3 — a daemon that understands
    /// the idle-guarded `ShutdownIfIdle`, so the client delegates the final
    /// idle decision to it.
    fn sample_status_v3(build_id: &str, active_run_count: u64) -> DaemonStatus {
        DaemonStatus {
            protocol_version: ProtocolVersion { major: 1, minor: 3 },
            ..sample_status(build_id, active_run_count)
        }
    }

    /// An injected fake [`RestartOps`]: no real socket, daemon, or process.
    /// Scripted `status_sequence` is consumed in order (one entry per `status`
    /// call); `restart_daemon`/`restart_daemon_if_idle`/`reconnect` are tracked
    /// so a test can assert exactly which stop path ran.
    struct FakeOps {
        status_sequence: Mutex<Vec<DaemonStatus>>,
        restart_calls: AtomicU32,
        idle_restart_calls: AtomicU32,
        reconnect_calls: AtomicU32,
        /// What `reconnect` reports as the fresh build id (simulates the new
        /// daemon, post-restart, actually running the client's build).
        reconnected_build_id: String,
        /// What `restart_daemon_if_idle` returns (default: `Restarted`).
        idle_restart_result: commands::IdleRestartOutcome,
    }

    impl FakeOps {
        fn new(status_sequence: Vec<DaemonStatus>, reconnected_build_id: &str) -> Self {
            Self {
                status_sequence: Mutex::new(status_sequence),
                restart_calls: AtomicU32::new(0),
                idle_restart_calls: AtomicU32::new(0),
                reconnect_calls: AtomicU32::new(0),
                reconnected_build_id: reconnected_build_id.to_string(),
                idle_restart_result: commands::IdleRestartOutcome::Restarted,
            }
        }

        /// Script what the idle-guarded restart returns (e.g. a daemon-side
        /// refusal when a run raced in).
        fn with_idle_result(mut self, result: commands::IdleRestartOutcome) -> Self {
            self.idle_restart_result = result;
            self
        }

        fn restart_call_count(&self) -> u32 {
            self.restart_calls.load(Ordering::SeqCst)
        }

        fn idle_restart_call_count(&self) -> u32 {
            self.idle_restart_calls.load(Ordering::SeqCst)
        }

        fn reconnect_call_count(&self) -> u32 {
            self.reconnect_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl RestartOps for FakeOps {
        async fn status(&mut self) -> anyhow::Result<DaemonStatus> {
            let mut sequence = self.status_sequence.lock().unwrap();
            if sequence.is_empty() {
                anyhow::bail!("FakeOps::status called more times than scripted");
            }
            Ok(sequence.remove(0))
        }

        async fn restart_daemon(&mut self) -> anyhow::Result<()> {
            self.restart_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn restart_daemon_if_idle(&mut self) -> anyhow::Result<commands::IdleRestartOutcome> {
            self.idle_restart_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.idle_restart_result)
        }

        async fn reconnect(&mut self) -> anyhow::Result<ServerHello> {
            self.reconnect_calls.fetch_add(1, Ordering::SeqCst);
            Ok(sample_hello(&self.reconnected_build_id))
        }
    }

    fn no_warnings() -> Vec<String> {
        Vec::new()
    }

    #[tokio::test]
    async fn matching_build_ids_proceed_with_zero_round_trips() {
        // A poisoned/empty FakeOps proves `status` is never called: the
        // matching path is a single string compare, nothing more.
        let mut ops = FakeOps::new(Vec::new(), "1.0+new");
        let mut messages = no_warnings();
        let mut warn = |m: &str| messages.push(m.to_string());
        let (_dir, paths) = temp_paths("match");

        let outcome = reconcile_interactive(&paths, "1.0+new", "1.0+new", &mut ops, &mut warn)
            .await
            .expect("matching build ids never fail");

        assert_eq!(outcome, ReconcileOutcome::Proceed);
        assert_eq!(ops.restart_call_count(), 0);
        assert_eq!(ops.reconnect_call_count(), 0);
        assert!(
            messages.is_empty(),
            "matching path prints nothing: {messages:?}"
        );
    }

    #[tokio::test]
    async fn mismatch_and_idle_restarts_reconnects_and_confirms_the_new_id() {
        let mut ops = FakeOps::new(
            vec![
                sample_status("1.0+old", 0), // pre-lock check: idle
                sample_status("1.0+old", 0), // re-check UNDER the lock: still idle
            ],
            "1.0+new",
        );
        let mut messages = no_warnings();
        let mut warn = |m: &str| messages.push(m.to_string());
        let (_dir, paths) = temp_paths("idle-restart");

        let outcome = reconcile_interactive(&paths, "1.0+new", "1.0+old", &mut ops, &mut warn)
            .await
            .expect("an idle mismatch restarts cleanly");

        assert_eq!(outcome, ReconcileOutcome::Restarted);
        assert_eq!(
            ops.restart_call_count(),
            1,
            "a legacy (v1.2) daemon takes the plain restart path exactly once"
        );
        assert_eq!(
            ops.idle_restart_call_count(),
            0,
            "a legacy daemon must NOT be sent ShutdownIfIdle (it can't decode it)"
        );
        assert_eq!(
            ops.reconnect_call_count(),
            1,
            "must reconnect after restarting"
        );
        assert!(
            messages.iter().any(|m| m.contains("restarting the daemon")),
            "got: {messages:?}"
        );
    }

    #[tokio::test]
    async fn a_v13_daemon_uses_the_idle_guarded_restart_not_the_plain_path() {
        // A daemon advertising protocol v1.3 can make the final idle decision
        // itself, so the client delegates via `restart_daemon_if_idle` and never
        // issues the plain (unconditional) restart.
        let mut ops = FakeOps::new(
            vec![
                sample_status_v3("1.0+old", 0), // pre-lock: idle
                sample_status_v3("1.0+old", 0), // under the lock: still idle, minor 3
            ],
            "1.0+new",
        );
        let mut messages = no_warnings();
        let mut warn = |m: &str| messages.push(m.to_string());
        let (_dir, paths) = temp_paths("v13-idle-restart");

        let outcome = reconcile_interactive(&paths, "1.0+new", "1.0+old", &mut ops, &mut warn)
            .await
            .expect("a v1.3 idle mismatch restarts cleanly");

        assert_eq!(outcome, ReconcileOutcome::Restarted);
        assert_eq!(
            ops.idle_restart_call_count(),
            1,
            "a v1.3 daemon takes the idle-guarded path exactly once"
        );
        assert_eq!(
            ops.restart_call_count(),
            0,
            "the plain (unconditional) restart must NOT run for a v1.3 daemon"
        );
        assert_eq!(
            ops.reconnect_call_count(),
            1,
            "must reconnect after restarting"
        );
    }

    #[tokio::test]
    async fn a_v13_daemon_refusing_idle_shutdown_warns_active_and_does_not_reconnect() {
        // Idle at BOTH client checks, but the daemon refuses the idle-guarded
        // shutdown because a run raced in at the daemon-side atomic re-check —
        // exactly the residual window DR7 closes. The client must warn and keep
        // running, never reconnect to a daemon that did not actually restart.
        let mut ops = FakeOps::new(
            vec![
                sample_status_v3("1.0+old", 0),
                sample_status_v3("1.0+old", 0),
            ],
            "1.0+new",
        )
        .with_idle_result(commands::IdleRestartOutcome::RefusedActive(2));
        let mut messages = no_warnings();
        let mut warn = |m: &str| messages.push(m.to_string());
        let (_dir, paths) = temp_paths("v13-refused");

        let outcome = reconcile_interactive(&paths, "1.0+new", "1.0+old", &mut ops, &mut warn)
            .await
            .expect("a daemon-side refusal degrades, never errors");

        assert_eq!(outcome, ReconcileOutcome::WarnActive);
        assert_eq!(
            ops.idle_restart_call_count(),
            1,
            "the idle-guarded restart was attempted"
        );
        assert_eq!(
            ops.reconnect_call_count(),
            0,
            "a refused restart must NOT reconnect — the old daemon is still up"
        );
        assert!(
            messages.iter().any(|m| m.contains("became active")),
            "got: {messages:?}"
        );
    }

    #[tokio::test]
    async fn mismatch_with_an_active_run_warns_and_never_restarts() {
        let mut ops = FakeOps::new(vec![sample_status("1.0+old", 2)], "1.0+new");
        let mut messages = no_warnings();
        let mut warn = |m: &str| messages.push(m.to_string());
        let (_dir, paths) = temp_paths("active-warn");

        let outcome = reconcile_interactive(&paths, "1.0+new", "1.0+old", &mut ops, &mut warn)
            .await
            .expect("an active mismatch degrades, never errors");

        assert_eq!(outcome, ReconcileOutcome::WarnActive);
        assert_eq!(
            ops.restart_call_count(),
            0,
            "NEVER restart while a run is active — the safety invariant"
        );
        assert_eq!(ops.reconnect_call_count(), 0);
        assert!(
            messages.iter().any(|m| m.contains("newer codypendent")),
            "got: {messages:?}"
        );
    }

    #[tokio::test]
    async fn a_failed_status_query_warns_unknown_and_never_restarts() {
        // Empty sequence: the very first `status` call fails (scripted as
        // "called more times than scripted" inside FakeOps), simulating a
        // daemon that stopped answering right at detection time.
        let mut ops = FakeOps::new(Vec::new(), "1.0+new");
        let mut messages = no_warnings();
        let mut warn = |m: &str| messages.push(m.to_string());
        let (_dir, paths) = temp_paths("unknown-warn");

        let outcome = reconcile_interactive(&paths, "1.0+new", "1.0+old", &mut ops, &mut warn)
            .await
            .expect("an unconfirmable mismatch degrades, never errors");

        assert_eq!(outcome, ReconcileOutcome::WarnUnknown);
        assert_eq!(
            ops.restart_call_count(),
            0,
            "NEVER restart on uncertainty — the safety invariant"
        );
        assert!(
            messages.iter().any(|m| m.contains("couldn't confirm")),
            "got: {messages:?}"
        );
    }

    #[tokio::test]
    async fn toctou_a_run_starting_under_the_lock_cancels_the_restart() {
        // Idle at the FIRST (pre-lock) check, but a run has started by the
        // time the SECOND check runs under the lock — the restart must be
        // cancelled, not just delayed: `restart_daemon` must never be called.
        let mut ops = FakeOps::new(
            vec![
                sample_status("1.0+old", 0), // pre-lock: idle -> decides Restart
                sample_status("1.0+old", 1), // under the lock: now active!
            ],
            "1.0+new",
        );
        let mut messages = no_warnings();
        let mut warn = |m: &str| messages.push(m.to_string());
        let (_dir, paths) = temp_paths("toctou");

        let outcome = reconcile_interactive(&paths, "1.0+new", "1.0+old", &mut ops, &mut warn)
            .await
            .expect("a TOCTOU-active recheck degrades, never errors");

        assert_eq!(outcome, ReconcileOutcome::WarnActive);
        assert_eq!(
            ops.restart_call_count(),
            0,
            "a run starting under the lock must cancel the restart — never kill it"
        );
        assert_eq!(ops.reconnect_call_count(), 0);
        assert!(
            messages
                .iter()
                .any(|m| m.contains("while acquiring the restart lock")),
            "got: {messages:?}"
        );
    }

    #[tokio::test]
    async fn a_lock_creation_failure_degrades_to_warn_unknown_never_restarts() {
        // data_dir is a regular FILE, so creating <data_dir>/daemon-restart.lock
        // fails with a non-AlreadyExists error (ENOTDIR). The driver must
        // degrade to a warn and continue — never abort the launch (a `?`), and
        // never restart when it cannot coordinate.
        let dir = tempfile::Builder::new()
            .prefix("cp-restart-lockfail-")
            .tempdir()
            .expect("tempdir");
        let not_a_dir = dir.path().join("this-is-a-file");
        std::fs::write(&not_a_dir, b"x").expect("create a file where a dir is expected");
        let paths = RuntimePaths::from_data_dir(not_a_dir);

        let mut ops = FakeOps::new(vec![sample_status("1.0+old", 0)], "1.0+new");
        let mut messages = no_warnings();
        let mut warn = |m: &str| messages.push(m.to_string());

        let outcome = reconcile_interactive(&paths, "1.0+new", "1.0+old", &mut ops, &mut warn)
            .await
            .expect("a lock-creation failure degrades, never errors");

        assert_eq!(outcome, ReconcileOutcome::WarnUnknown);
        assert_eq!(
            ops.restart_call_count(),
            0,
            "a lock we cannot create must never lead to a restart"
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("couldn't acquire the daemon-restart lock")),
            "got: {messages:?}"
        );
    }

    #[tokio::test]
    async fn a_reconnect_that_still_mismatches_is_a_hard_error() {
        // The restart ran, but the fresh daemon STILL reports the old build
        // (a stale on-disk binary, or a different codypendent on PATH) — this
        // must be a legible hard error, never a silent half-restart.
        let mut ops = FakeOps::new(
            vec![sample_status("1.0+old", 0), sample_status("1.0+old", 0)],
            "1.0+old", // reconnect reports the SAME (still-mismatched) id
        );
        let mut messages = no_warnings();
        let mut warn = |m: &str| messages.push(m.to_string());
        let (_dir, paths) = temp_paths("still-mismatched");

        let error = reconcile_interactive(&paths, "1.0+new", "1.0+old", &mut ops, &mut warn)
            .await
            .expect_err("a still-mismatched reconnect must be a hard error");

        let message = format!("{error:#}");
        assert!(message.contains("did not load the new build"), "{message}");
        assert!(message.contains("1.0+new"));
        assert!(message.contains("1.0+old"));
    }
}

#[cfg(test)]
mod restart_lock_tests {
    //! Direct coverage of the single-flight restart lock (Decision 3). The
    //! `reconcile_interactive` tests above each use a fresh tempdir, so they
    //! never actually contend for the lock or exercise stale-holder recovery —
    //! this module drives `try_acquire_restart_lock` head-on.
    use super::*;

    fn temp_paths(tag: &str) -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempfile::Builder::new()
            .prefix(&format!("cp-lock-{tag}-"))
            .tempdir()
            .expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        (dir, paths)
    }

    fn expect_acquired(attempt: LockAttempt) -> RestartLockGuard {
        match attempt {
            LockAttempt::Acquired(guard) => guard,
            LockAttempt::HeldByOther => panic!("expected to acquire the lock, but it was held"),
        }
    }

    #[test]
    fn a_second_acquire_while_held_is_reported_as_held_by_other() {
        let (_dir, paths) = temp_paths("single-flight");
        // First acquisition succeeds and is kept alive for the whole test.
        let _guard = expect_acquired(try_acquire_restart_lock(&paths).expect("first acquire"));
        // A concurrent client (this process still holds it, so it IS alive)
        // must be told the lock is held — never handed a second acquisition.
        match try_acquire_restart_lock(&paths).expect("second acquire attempt") {
            LockAttempt::HeldByOther => {}
            LockAttempt::Acquired(_) => panic!("single-flight violated: acquired a held lock"),
        }
    }

    #[test]
    fn dropping_the_guard_releases_the_lock() {
        let (_dir, paths) = temp_paths("release-on-drop");
        let lock_path = restart_lock_path(&paths);
        {
            let _guard = expect_acquired(try_acquire_restart_lock(&paths).expect("acquire"));
            assert!(lock_path.exists(), "lock file exists while held");
        }
        assert!(
            !lock_path.exists(),
            "the Drop guard must remove the lock file"
        );
        // And a fresh acquisition succeeds now that it is released.
        let _reacquired = expect_acquired(try_acquire_restart_lock(&paths).expect("re-acquire"));
    }

    #[test]
    fn a_stale_lock_from_a_dead_holder_is_reclaimed() {
        let (_dir, paths) = temp_paths("stale-dead-pid");
        let lock_path = restart_lock_path(&paths);
        // A pid far above any real process (macOS pids top out ~99998): the
        // liveness probe (`kill -0`) reports it gone, so the lock is stale.
        std::fs::write(&lock_path, "2147483647").expect("plant stale lock");
        let _guard = expect_acquired(
            try_acquire_restart_lock(&paths).expect("a dead holder's lock must be reclaimed"),
        );
        // The reclaimed lock now records THIS process's pid.
        assert_eq!(read_lock_pid(&lock_path), Some(std::process::id()));
    }

    #[test]
    fn a_corrupt_lock_file_is_treated_as_stale_and_reclaimed() {
        let (_dir, paths) = temp_paths("corrupt");
        let lock_path = restart_lock_path(&paths);
        // An unparseable lock file can never be proof of a live holder.
        std::fs::write(&lock_path, "not-a-pid").expect("plant corrupt lock");
        let _guard = expect_acquired(
            try_acquire_restart_lock(&paths).expect("a corrupt lock must be reclaimed"),
        );
    }

    #[test]
    fn a_live_holders_pid_is_never_reclaimed() {
        let (_dir, paths) = temp_paths("live-holder");
        let lock_path = restart_lock_path(&paths);
        // This test process is unquestionably alive: a lock recording its pid
        // must be respected, never stolen — the safety side of stale recovery.
        std::fs::write(&lock_path, std::process::id().to_string()).expect("plant live lock");
        match try_acquire_restart_lock(&paths).expect("attempt against a live holder") {
            LockAttempt::HeldByOther => {}
            LockAttempt::Acquired(_) => {
                panic!("reclaimed a live holder's lock — must never happen")
            }
        }
    }
}
