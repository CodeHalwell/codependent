# Single self-contained binary (fold the daemon into `codypendent`) — design

**Date:** 2026-07-24 · **Status:** approved (pre-implementation) · **Branch:** `claude/single-binary` (off main)

## Problem

`codypendent` (CLI/TUI) and `codypendentd` (daemon) are two separate executables. The CLI
finds and spawns `codypendentd` as a sibling binary (`resolve_daemon_binary` → the daemon
next to `current_exe`, else PATH). This split caused a real, painful bug: a **stale daemon**.
The daemon is long-lived; when the user updated `codypendent` but the old `codypendentd`
process kept running (or an old daemon binary sat on disk), merged daemon-side work stayed
dormant until a manual rebuild+restart — client/daemon **version skew**.

## Goal

One self-contained `codypendent` binary that runs the daemon **from itself**. Installing or
updating `codypendent` automatically updates the daemon (no separate binary, no skew). The
existing `codypendentd` binary can remain (for direct/advanced use) but is no longer required
by the install or the CLI.

## Approach

Expose the daemon's run-loop as a **library** in the `codypendent-codypendentd` crate, add a
hidden **`codypendent __daemon`** subcommand that calls it, and point `resolve_daemon_binary`
at `current_exe __daemon`. The install ships **only `codypendent`**.

Feasibility (verified): `codypendentd` is a bin-only crate (`[[bin]] codypendentd`, `main.rs`
+ modules) and does **not** depend on `codypendent-cli`, so `cli → codypendentd` is acyclic.

## Architecture

### 1. Daemon run-loop as a library (`crates/codypendentd`)
- Add `crates/codypendentd/src/lib.rs` exposing `pub async fn run_daemon(paths: RuntimePaths)
  -> anyhow::Result<()>` (name/signature per the real `main.rs`), containing everything
  `main.rs` does today after arg/paths setup: open db, `recover_on_startup`, build the
  `RuntimeExecutor`/workflow host, `recover_workflows`, start `server::serve`, install the
  SIGTERM/SIGINT handler, run to shutdown. Keep the existing modules (`executor`, `routing`,
  `workflow_exec`, `session_history`, …) as they are — the lib just re-exposes them + the
  entry point.
- `crates/codypendentd/src/main.rs` (the `codypendentd` bin) shrinks to: resolve paths from
  env/args (as today), `codypendent_codypendentd::run_daemon(paths).await`. The standalone
  daemon binary keeps working, byte-for-byte behavior.
- Add a `[lib]` target to `crates/codypendentd/Cargo.toml` (name `codypendent_codypendentd`)
  alongside the existing `[[bin]]`.

### 2. CLI depends on the daemon lib + a hidden subcommand (`crates/cli`)
- `crates/cli/Cargo.toml`: add `codypendent-codypendentd = { workspace = true }` (acyclic).
- `crates/cli/src/main.rs`: add a **hidden** subcommand `__daemon` (clap `#[command(hide =
  true)]`) that, when invoked, resolves paths and calls
  `codypendent_codypendentd::run_daemon(paths).await` — i.e. `codypendent __daemon` *is* the
  daemon. Dispatch it before the normal CLI so it never prints CLI help/among user commands.
- This grows the `codypendent` binary (it now links the full daemon assembly: executor,
  routing, workflow, runtime, integrations). Accepted cost of single-binary.

### 3. The CLI spawns the daemon from itself (`crates/cli/src/commands.rs`)
- `resolve_daemon_binary` → `std::env::current_exe()` (the `codypendent` binary itself).
- `ensure_daemon` spawns `Command::new(current_exe).arg("__daemon")` (+ the existing new
  process-group / detached / stdout→daemon.log setup), instead of a separate `codypendentd`.
- Fallback: if `current_exe` is somehow unavailable, fall back to a sibling/PATH `codypendentd`
  (keeps the old path working). A running-daemon check (socket present + handshake) short-
  circuits before spawning, unchanged.

### 4. Release + installer ship one binary
- `.github/workflows/release.yml`: keep building `codypendent` (and optionally `codypendentd`
  for advanced users), but the primary artifact is `codypendent`. The tarball must contain
  `codypendent`; `codypendentd` becomes optional.
- `install.sh` (PR #24): install just `codypendent` (drop the hard requirement on
  `codypendentd`); the CLI is now self-sufficient. Keep installing `codypendentd` too **if
  present** (harmless), but do not fail if it is absent.

## Data flow

`codypendent` (any command needing the daemon) → `ensure_daemon` → socket check; if absent,
spawn `current_exe __daemon` (detached, new process group, stdout→daemon.log) → the spawned
process runs `codypendent_codypendentd::run_daemon(paths)` → listens on the socket → the CLI
handshakes. Updating `codypendent` ⇒ the next spawn runs the new daemon code from the same
binary — **no skew**. (A daemon already running from an older binary still needs a restart to
pick up new code — unchanged and unavoidable for a long-lived process — but there is no longer
a *separate stale binary on disk* to diverge, and `codypendent daemon restart`/stop manages it.)

## Error handling / edge cases

- **`current_exe` unavailable** (rare): fall back to sibling/PATH `codypendentd`; if neither,
  the existing "failed to spawn / daemon did not become ready" error.
- **Old separate `codypendentd` still running** on the socket: the socket-present check
  short-circuits (as today); `codypendent daemon restart` stops it and the new spawn is the
  self-binary daemon. (Document that a one-time restart adopts the single-binary daemon.)
- **`codypendent __daemon` invoked by a user directly**: it just runs the daemon (same as
  `codypendentd`); hidden from help but functional — acceptable.
- **Binary size / build time** grow (the CLI links the daemon). Verified acceptable; note it.

## Testing

- codypendentd: `run_daemon` is callable as a lib (a smoke test that it starts + binds a
  socket in a temp data dir + shuts down on signal — mirror any existing daemon IT harness);
  the `codypendentd` bin still starts (unchanged behavior).
- cli: `resolve_daemon_binary` returns `current_exe`; `ensure_daemon` spawns `current_exe
  __daemon` (assert the spawned argv), with the fallback path covered; the `__daemon`
  subcommand dispatches to `run_daemon` and is hidden from `--help`.
- end-to-end (gated/manual): a freshly-built single `codypendent` binary, with **no**
  `codypendentd` on disk/PATH, still auto-starts the daemon and runs a session.
- All existing tests green; no protocol change ⇒ no golden-vector change.

## Constraints

- No protocol/wire change; no behavior change to what the daemon does — only *where it runs
  from*. The `codypendentd` standalone binary keeps working identically.
- Acyclic deps (`cli → codypendentd` only; codypendentd never depends on cli).
- `cargo deny` still clean (no new external dep — just an intra-workspace dep edge + a lib
  target). Clippy Linux gate.
- Foreign files never touched. Installer/release edits are additive/compatible.

## Non-goals / follow-ups

- Removing the `codypendentd` binary entirely — kept for now (advanced/direct use); a later
  cleanup could drop it once nothing depends on it.
- A managed service (launchd/systemd) install — separate follow-up.
- Auto-restarting a running old-binary daemon on CLI update (the CLI could detect a
  version-mismatch handshake and offer to restart) — a nice follow-up, not v1.

## Open questions

- **Binary-size acceptability**: linking the full daemon into `codypendent` roughly doubles
  its size + build time. Confirm acceptable (it is, for the skew-elimination benefit); the
  plan measures before/after.
- **Version-mismatch handshake**: optionally stamp the daemon's build version and have the CLI
  warn/offer-restart on skew with a *running* daemon — pinned as a follow-up, not v1.
