# Single Self-Contained Binary (fold the daemon into `codypendent`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make one self-contained `codypendent` binary that runs the daemon *from itself* (via a hidden `codypendent __daemon` subcommand), so updating `codypendent` always updates the daemon — eliminating the stale-daemon / client-daemon version-skew bug.

**Architecture:** Expose the daemon's run-loop from the `codypendent-codypendentd` crate as a library (`run_daemon` + `init_tracing`) alongside its existing `codypendentd` binary. The `codypendent` CLI gains a `codypendent-codypendentd` dependency (an intra-workspace edge; `codypendentd` never depends on `cli`, so it stays acyclic) and a hidden `__daemon` clap subcommand that calls `run_daemon`. `ensure_daemon` then spawns `std::env::current_exe() __daemon` instead of a sibling `codypendentd`. The standalone `codypendentd` binary keeps working, byte-for-byte.

**Tech Stack:** Rust (edition 2021, workspace rust-version 1.88); `tokio` (`#[tokio::main]`); `clap` v4 derive; `anyhow`; `tracing`/`tracing-subscriber`; the existing `codypendent-protocol` Unix-socket wire protocol (unchanged). No new external crate — only an intra-workspace dependency edge plus a `[lib]` target.

## Global Constraints

*Copied verbatim from the spec's "Constraints" section, plus the project-wide gates every task must satisfy:*

- **No protocol/wire change; no behavior change to what the daemon does — only *where it runs from*. The `codypendentd` standalone binary keeps working identically** (byte-for-byte runtime behavior: same tracing init, same startup order, same socket/db/recovery/serve path).
- **Acyclic deps (`cli → codypendentd` only; codypendentd never depends on cli).** Verified: `crates/codypendentd/Cargo.toml` has no `codypendent-cli` dependency, and every crate `codypendentd` depends on (daemon, runtime, protocol, routing, knowledge, integrations, workflow, eval) is *already* a dependency of `cli` today — so the only new edge is `cli → codypendentd`.
- **`cargo deny` still clean (no new external dep — just an intra-workspace dep edge + a lib target). Clippy Linux gate.**
- **Foreign files never touched. Installer/release edits are additive/compatible.**
- **`cargo deny` licence/advisory gate stays green** — because no new external crate enters the graph (`codypendent-codypendentd` is intra-workspace; its external deps are a subset of `cli`'s existing external deps), `deny.toml` needs no change.
- **Clippy runs on Linux CI:** `cargo clippy --workspace --all-targets --all-features -- -D warnings`. Gate any platform-only helper with the same `#[cfg]` as its sole caller (the `#[cfg(unix)] process_group(0)` block in `ensure_daemon` already follows this; no new platform-gated helpers are introduced). Watch the macOS/Linux dead-code trap: every new non-test function must have a non-test caller.
- **NEVER edit/stage** `README.md`, `docs/cli-and-tui-user-guide.md`, `docs/docs/*`, `ROADMAP.md`, or anything under `.superpowers/`. Stage only changed files by explicit path; never `git add -A`.
- **Commit trailer on every commit:** `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- **Full gate green per task:** `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo test --workspace --all-features`.

---

## Design decisions

**The exposed entry point — `run_daemon(paths)` + `init_tracing()`, not one `run_daemon_from_env()`.** The real `crates/codypendentd/src/main.rs` today does, in order: (1) init a `tracing_subscriber` to stderr; (2) `let paths = RuntimePaths::resolve()?; paths.ensure_directories()?;`; (3) everything else (open db, record boot, recovery, register built-ins, scan repo, build `RuntimeExecutor`, GitHub discovery, relaunch queued runs, recover workflows, maybe start the webhook listener, `server::run_with_executor_on`). To keep the standalone `codypendentd` byte-for-byte we split at the same seams the spec names:

- `pub fn init_tracing()` — exactly the current top-of-`main` subscriber. Both binaries call it first, so daemon logs still reach stderr (which the CLI redirects to `daemon.log`). Putting it in the lib avoids giving `crates/cli` a *direct* `tracing-subscriber` dependency (honoring "no new external dep" — the CLI reaches it only through the daemon lib it already links).
- `pub async fn run_daemon(paths: RuntimePaths) -> anyhow::Result<()>` — the body from `let database_path = …` through the final `server::run_with_executor_on(…).await`. It takes an already-resolved, already-`ensure_directories()`'d `RuntimePaths`, exactly as the spec specifies (`main.rs shrinks to: resolve paths from env/args (as today), run_daemon(paths).await`).

`RuntimePaths` here is `codypendent_protocol::discovery::RuntimePaths` (the real type `main.rs` uses today — **not** a new "RuntimePaths" type; the spec's placeholder `RuntimePaths` resolves to this).

**Spec vs. reality — the SIGTERM/SIGINT handler.** The spec says `run_daemon` should "install the SIGTERM/SIGINT handler, run to shutdown." In the real `main.rs` there is *no* explicit signal-handler code: the final call `server::run_with_executor_on(listener, pool, paths, boot, Some(executor)).await` blocks until shutdown and owns the signal handling internally (the same call the CLI's `daemon stop` drives via a `Payload::Shutdown` request). So `run_daemon` reaches shutdown purely by moving the existing lines; it adds no new signal code. This is a faithful move, not a re-implementation.

**Spawning the self-daemon — a `DaemonInvocation { program, args }`.** The real `resolve_daemon_binary()` returns a bare `PathBuf` and `ensure_daemon` does `Command::new(&daemon_binary)`. The new primary path must pass the `__daemon` argument, but the `current_exe`-unavailable fallback (`codypendentd` on PATH) must pass *no* argument. Bundling `program + args` in one small struct keeps that coupling correct and makes the resolved argv unit-testable (`std::process::Command::get_program()` / `get_args()`, stable since Rust 1.57). The fallback resolves to a PATH `codypendentd` (no sibling lookup — a sibling path can only be computed from `current_exe`, which is precisely what is unavailable in the fallback branch; this matches the spec's "if `current_exe` is somehow unavailable, fall back to a sibling/PATH `codypendentd`" as far as it is reachable).

**The hidden subcommand is dispatched before the TUI/theme setup.** `codypendent __daemon` is intercepted immediately after `Cli::parse()` (before `RuntimePaths::resolve()` for the normal path, theme resolution, and the bare-invocation TUI branch), so it behaves exactly like `codypendentd`: init tracing, resolve paths, run the loop. `#[command(name = "__daemon", hide = true)]` keeps it out of `--help` while still parseable.

**Binary size / build time (spec open question).** The CLI *already* depends on `daemon`, `runtime`, `protocol`, `routing`, `knowledge`, `integrations`, `workflow`, and `eval` (see `crates/cli/Cargo.toml`) — the same crates `codypendentd` pulls in. So linking `codypendent-codypendentd` adds essentially only that crate's own assembly glue (its `src/*.rs`), not a new external subtree. The "roughly doubles" worry in the spec is likely overstated; Task 2 measures the real before→after to confirm.

## File structure

- **New:** `crates/codypendentd/src/lib.rs` — the daemon crate's library root: declares the existing modules, and exposes `init_tracing()` + `run_daemon(paths)`. (Task 1)
- **New:** `crates/codypendentd/tests/run_daemon_lib_it.rs` — smoke test that the *library* entry point starts, binds its socket, answers Ping, and shuts down on `Payload::Shutdown`. (Task 1)
- **Modify:** `crates/codypendentd/src/main.rs` — shrinks to `init_tracing(); resolve paths; run_daemon(paths).await`. (Task 1)
- **Modify:** `crates/codypendentd/Cargo.toml` — add the `[lib]` target. (Task 1)
- **Modify:** root `Cargo.toml` — declare `codypendent-codypendentd` in `[workspace.dependencies]`. (Task 2)
- **Modify:** `crates/cli/Cargo.toml` — add `codypendent-codypendentd = { workspace = true }`. (Task 2)
- **Modify:** `crates/cli/src/main.rs` — add the hidden `__daemon` subcommand + early dispatch + clap-structure tests. (Task 2)
- **Modify:** `crates/cli/src/commands.rs` — `resolve_daemon_binary` → `DaemonInvocation` via `current_exe`; `ensure_daemon` spawns `current_exe __daemon`; unit tests. (Task 3)
- **Modify:** `.github/workflows/release.yml` — primary artifact `codypendent`; `codypendentd` optional in the tarball. (Task 4)
- **Modify:** `install.sh` — require only `codypendent`; install `codypendentd` too *if present*. (Task 4)

---

## Task 1: Expose the daemon run-loop as a library (`crates/codypendentd`)

**Files:**
- Create: `crates/codypendentd/src/lib.rs`
- Create: `crates/codypendentd/tests/run_daemon_lib_it.rs`
- Modify: `crates/codypendentd/src/main.rs` (replace the whole file)
- Modify: `crates/codypendentd/Cargo.toml` (add `[lib]`)

**Interfaces:**
- Produces:
  - `pub fn codypendent_codypendentd::init_tracing()` — installs the daemon's `tracing_subscriber` (stderr writer, `EnvFilter` from env, default `info`).
  - `pub async fn codypendent_codypendentd::run_daemon(paths: codypendent_protocol::discovery::RuntimePaths) -> anyhow::Result<()>` — the full daemon run-loop; expects `paths` already resolved and `ensure_directories()`'d; returns `Ok(())` on graceful shutdown.
- Consumes: nothing from earlier tasks.

- [ ] **Step 1: Add the `[lib]` target to `crates/codypendentd/Cargo.toml`**

Insert a `[lib]` section immediately after the existing `[[bin]]` block (keep the `[[bin]]`). The lib name matches the default crate name (`codypendent-codypendentd` → `codypendent_codypendentd`); declaring it explicitly documents intent.

```toml
[[bin]]
name = "codypendentd"
path = "src/main.rs"

[lib]
name = "codypendent_codypendentd"
path = "src/lib.rs"
```

- [ ] **Step 2: Create `crates/codypendentd/src/lib.rs` with the moved modules, `init_tracing`, `run_daemon`, and `maybe_start_webhook_listener`**

Move the module declarations, the imports, and the run-loop body out of `main.rs` and into the library. This is a faithful *move* of the current `main.rs` lines (tracing init → `init_tracing`; the body after `ensure_directories()` → `run_daemon`; the private `maybe_start_webhook_listener` → private lib fn). Write the file exactly:

```rust
//! `codypendent-codypendentd` — the persistent Codypendent daemon, exposed both
//! as the standalone `codypendentd` binary (`src/main.rs`) and as a library so
//! the single `codypendent` binary can run the SAME daemon in-process via the
//! hidden `codypendent __daemon` subcommand. Installing/updating `codypendent`
//! therefore always updates the daemon — no separate binary on disk to go stale
//! (the client/daemon version-skew bug this crate's library form eliminates).
//!
//! This is the composition root. It depends on BOTH `codypendent-daemon` (the
//! server + persistence) and `codypendent-runtime` (the agent loop) — which the
//! daemon crate itself cannot, because the runtime depends on the daemon (a
//! cycle). [`run_daemon`] performs the daemon startup exactly as the old
//! `main.rs` did (paths already resolved, db, boot, recovery), then constructs a
//! [`RuntimeExecutor`] that drives the runtime agent loop and injects it into
//! the server.

mod blackboard;
mod documents;
mod executor;
mod promotion;
mod publish;
mod routing;
mod scan;
mod session_history;
mod workflow_exec;
mod workflows;

use std::path::PathBuf;
use std::sync::Arc;

use codypendent_daemon::{db, instance, recovery, server};
use codypendent_protocol::discovery::RuntimePaths;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::executor::RuntimeExecutor;

/// Install the daemon's tracing subscriber: a formatted layer to stderr, its
/// filter taken from the environment (`RUST_LOG`/`EnvFilter`) and defaulting to
/// `info`. Both the standalone `codypendentd` binary and `codypendent __daemon`
/// call this FIRST — the daemon's stderr is what the CLI redirects to
/// `daemon.log`, so this preserves the daemon's logging behavior byte-for-byte.
pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
}

/// Run the Codypendent daemon to shutdown. `paths` must already be resolved and
/// have had `ensure_directories()` called (the caller — the `codypendentd`
/// binary or `codypendent __daemon` — does that, "as today"). Returns `Ok(())`
/// on graceful shutdown.
pub async fn run_daemon(paths: RuntimePaths) -> anyhow::Result<()> {
    let database_path = paths.data_dir.join("codypendent.db");

    // Claim single-instance exclusivity FIRST — before touching any shared
    // state. Recovery fails live runs, the relaunch spawns workers, and the
    // scan wipes/rebuilds the code graph; if a second daemon ran those against
    // a live daemon's database before discovering the socket was taken, it
    // would corrupt in-flight runs (contradictory terminal events, double
    // execution). Binding the socket is the mutex; losers exit here.
    let listener = server::acquire_socket(&paths).await?;

    let pool = db::open_database(&database_path).await?;
    let boot = instance::record_boot(&pool).await?;
    info!(
        instance = %boot.instance_id,
        boot_count = boot.boot_count,
        database = %database_path.display(),
        "codypendentd starting"
    );

    // Reconcile state a previous process may have left mid-flight — after the
    // exclusivity claim, before serving, so no client observes a half-recovered
    // run (STEP 1.14).
    let report = recovery::recover_on_startup(&pool, &paths).await?;
    info!(
        swept_tmp = report.swept_tmp,
        orphaned_leases = report.orphaned_leases.len(),
        reconciled_effects = report.reconciled_effects,
        failed_runs = report.failed_runs.len(),
        expired_approvals = report.expired_approvals.len(),
        "startup recovery complete"
    );

    // Register the built-in tools into the governed registry (STEP 2.2 — Phase-1
    // tools "now registered with metadata"). Idempotent: `register_builtins`
    // upserts by identity and reuses ids, so this is safe on every boot and is
    // what gives retrieval and the Skill Studio a populated registry from the
    // first start. A failure here is logged but never blocks the daemon.
    match codypendent_knowledge::register_builtins(&pool).await {
        Ok(()) => info!("built-in tools registered in the knowledge registry"),
        Err(error) => warn!(%error, "failed to register built-in tools"),
    }

    // Derive the process's repository identity from the working directory's
    // canonical path, so the SAME checkout maps to the SAME id across restarts —
    // a random id per boot would orphan the previous run's code graph and
    // repository-scoped memories and bloat the database. Then warm the code graph
    // so the repository map a run's context opens with is real. The same id is
    // handed to the executor, so runs, their context maps, and their curated
    // memories all share one stable repository. The scan is bounded and
    // failure-tolerant — a parse error on one file must never abort startup.
    let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let repository = scan::repository_id_for(&workdir);
    scan::scan_repository(&pool, repository, &workdir).await;

    // The executor owns the shared event fan-out + approval broker the server
    // binds to (`RunExecutor::collaborators`), and drives each accepted run
    // through the runtime agent loop. `workdir` is the daemon's startup root,
    // used both as the per-run worktree-binding fallback / node repository (T5,
    // the 4th `new` arg) and as the document-publish root (Phase 4 STEP 4.4 —
    // a document has no per-command repository field the way `StartRun` does,
    // so publication uses this same startup root, as the code-graph scan does).
    let mut executor =
        RuntimeExecutor::new(pool.clone(), paths.clone(), repository, workdir.clone())
            .with_repository_root(workdir);

    // Personal-mode GitHub (Phase 3 STEP 3.2): discover a token from `gh auth
    // token` or `GITHUB_TOKEN` and enable the `github.*` tools. Absent (the
    // common case in CI/headless), the tools stay disabled and the daemon runs
    // exactly as before. The token is a secret — only whether one was found is
    // ever logged, never its value.
    match codypendent_integrations::github::GitHubToken::discover().await {
        Ok(token) => {
            match codypendent_integrations::github::RestGitHubClient::new(
                "https://api.github.com",
                token,
            ) {
                Ok(client) => {
                    executor = executor.with_github(Arc::new(client));
                    info!("github personal-mode client enabled");
                }
                Err(error) => {
                    warn!(%error, "could not build the github client; github tools disabled")
                }
            }
        }
        Err(_) => info!("no github token found; github tools disabled"),
    }

    let executor = Arc::new(executor);

    // Re-launch any run left `Queued` by a crash between `StartRun`'s commit and
    // its fire-and-forget spawn — recovery's live-state sweep does not cover
    // `Queued`, so those runs would otherwise be stuck with no worker.
    match executor.relaunch_queued_runs().await {
        Ok(0) => {}
        Ok(n) => info!(
            relaunched = n,
            "re-launched queued runs orphaned by a prior crash"
        ),
        Err(error) => warn!(%error, "could not re-launch queued runs at startup"),
    }

    // Resume any durable workflow run left non-terminal by a crash (Phase 5 STEP
    // 5.2): recompile each from its stored manifest and drive it onward from where
    // it stopped. Paused runs are left for an explicit resume. Fire-and-forget, so
    // a slow workflow never stalls the socket server below; a run that cannot be
    // recompiled is a no-op logged by its drive task.
    match executor.recover_workflows().await {
        Ok(0) => {}
        Ok(n) => info!(recovered = n, "resumed incomplete workflow runs"),
        Err(error) => warn!(%error, "could not resume workflow runs at startup"),
    }

    // Optionally open the GitHub webhook listener (Phase 3 STEP 3.3). It is
    // disabled unless `<data_dir>/webhooks.toml` sets `enabled = true`, and even
    // then binds loopback by default. Deliveries are verified, deduplicated by
    // their `X-GitHub-Delivery` GUID, and normalized; they never trigger
    // workflows here (that requires explicit policy, wired in a later phase). The
    // listener runs concurrently with the blocking socket server below.
    maybe_start_webhook_listener(&paths, &pool).await;

    server::run_with_executor_on(listener, pool, paths, boot, Some(executor)).await
}

/// Start the webhook listener if `<data_dir>/webhooks.toml` enables it. Any
/// failure is logged and never blocks daemon startup — the webhook endpoint is
/// an optional, opt-in surface.
async fn maybe_start_webhook_listener(paths: &RuntimePaths, pool: &sqlx::SqlitePool) {
    use codypendent_integrations::webhook::{config, SqliteDeliveryStore, WebhookIngestor};

    let config_path = paths.data_dir.join("webhooks.toml");
    let webhooks = match config::load(&config_path) {
        Ok(Some(webhooks)) if webhooks.enabled => webhooks,
        Ok(_) => return, // absent or disabled — the default
        Err(error) => {
            warn!(%error, "failed to load webhooks configuration; listener not started");
            return;
        }
    };

    // The secret never reaches a log line: only its presence is reported.
    let secret = webhooks
        .secret
        .as_ref()
        .map(|value| value.as_bytes().to_vec());
    let store = Arc::new(SqliteDeliveryStore::new(pool.clone()));
    // Deliveries never trigger workflows in this phase (default-deny policy).
    let ingestor = Arc::new(WebhookIngestor::new(store, secret, false));

    match codypendent_integrations::webhook::server::bind(&webhooks.listen_addr).await {
        Ok(listener) => {
            info!(
                addr = %webhooks.listen_addr,
                signed = webhooks.secret.is_some(),
                "webhook listener enabled"
            );
            tokio::spawn(async move {
                if let Err(error) =
                    codypendent_integrations::webhook::server::serve(listener, ingestor).await
                {
                    warn!(%error, "webhook listener stopped");
                }
            });
        }
        Err(error) => warn!(
            %error,
            addr = %webhooks.listen_addr,
            "could not bind the webhook listener"
        ),
    }
}
```

- [ ] **Step 3: Replace `crates/codypendentd/src/main.rs` with the thin binary shell**

The bin now delegates to the library. It keeps the exact startup order the daemon has today — `init_tracing()` first, then resolve paths + `ensure_directories()`, then `run_daemon` — so the standalone `codypendentd` behaves byte-for-byte as before. Overwrite the whole file:

```rust
//! `codypendentd` — the persistent Codypendent daemon (standalone binary shell).
//!
//! The daemon's run-loop lives in this crate's library (`lib.rs`) as
//! [`run_daemon`], so the single `codypendent` binary can run the SAME daemon
//! via `codypendent __daemon`. This shell keeps the standalone `codypendentd`
//! binary working byte-for-byte: init tracing, resolve paths, delegate.
//!
//! [`run_daemon`]: codypendent_codypendentd::run_daemon

use codypendent_protocol::discovery::RuntimePaths;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    codypendent_codypendentd::init_tracing();

    let paths = RuntimePaths::resolve()?;
    paths.ensure_directories()?;

    codypendent_codypendentd::run_daemon(paths).await
}
```

- [ ] **Step 4: Confirm the crate compiles (lib + bin) and formats**

Run:
```bash
cargo build -p codypendent-codypendentd --bins --lib
cargo fmt -p codypendent-codypendentd -- --check
```
Expected: both succeed. The bin (`codypendentd`) and the new lib (`codypendent_codypendentd`) both build; the bin links the lib.

- [ ] **Step 5: Write the failing library smoke test**

Create `crates/codypendentd/tests/run_daemon_lib_it.rs`. It drives the **library** entry point (`run_daemon`) — not the binary — proving it starts, binds its socket in a temp data dir, answers `Ping`, and shuts down cleanly on `Payload::Shutdown`. It mirrors the control path `codypendent daemon start`/`stop` use (`crates/cli/src/client.rs`: connect, send a bare control `Payload`, read one reply — no handshake needed for `Ping`/`Shutdown`), and the temp-data-dir harness of `crates/codypendentd/tests/recovery_it.rs` (`RuntimePaths::from_data_dir`).

```rust
//! Smoke test that the daemon run-loop is callable as a *library*
//! (`codypendent_codypendentd::run_daemon`) — it starts, binds its socket in a
//! temp data dir, answers Ping, and returns cleanly on a Shutdown request. This
//! is exactly what `codypendent __daemon` relies on. It exercises the control
//! path (`Payload::Ping`/`Payload::Shutdown`), the same one `codypendent daemon
//! start`/`stop` use, rather than a full session.

use std::path::Path;
use std::time::Duration;

use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{read_envelope, write_envelope, ClientId, Envelope, Payload};
use tokio::net::UnixStream;

/// Connect, send one control payload, read one reply — mirrors the CLI's
/// `client::request`, which `Ping`/`Shutdown`/`DaemonStatusRequest` use with no
/// handshake. Returns `None` if the socket is not up yet.
async fn control(socket: &Path, payload: Payload) -> Option<Payload> {
    let mut stream = UnixStream::connect(socket).await.ok()?;
    write_envelope(&mut stream, &Envelope::request(ClientId::new(), payload))
        .await
        .ok()?;
    Some(read_envelope(&mut stream).await.ok()??.payload)
}

#[tokio::test]
async fn run_daemon_lib_starts_binds_and_shuts_down() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = RuntimePaths::from_data_dir(tmp.path().to_path_buf());
    paths.ensure_directories().unwrap();

    // Drive the LIBRARY entry point directly — this is what `codypendent
    // __daemon` calls. It blocks until shutdown, so run it on a task.
    let daemon = tokio::spawn({
        let paths = paths.clone();
        async move { codypendent_codypendentd::run_daemon(paths).await }
    });

    // It comes up and answers Ping with Pong (socket bound, server serving).
    let mut up = false;
    for _ in 0..200 {
        if matches!(
            control(&paths.socket_path, Payload::Ping).await,
            Some(Payload::Pong)
        ) {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        up,
        "run_daemon never bound its socket at {}",
        paths.socket_path.display()
    );

    // A Shutdown request drains it cleanly; the run_daemon future then resolves.
    assert!(matches!(
        control(&paths.socket_path, Payload::Shutdown).await,
        Some(Payload::ShutdownAck)
    ));
    let joined = tokio::time::timeout(Duration::from_secs(5), daemon)
        .await
        .expect("run_daemon did not return within 5s of Shutdown")
        .expect("run_daemon task panicked");
    joined.expect("run_daemon returned an error");
}
```

- [ ] **Step 6: Run the smoke test to verify it passes**

Run:
```bash
cargo test -p codypendent-codypendentd --test run_daemon_lib_it -- --nocapture
```
Expected: PASS (`run_daemon_lib_starts_binds_and_shuts_down ... ok`). If it fails to *compile* with "cannot find function `run_daemon`", the `[lib]` target or `lib.rs` from Steps 1-2 is missing/misnamed.

- [ ] **Step 7: Verify the standalone `codypendentd` binary is unchanged — run the full crate test suite**

The existing integration tests (`recovery_it.rs`, `blackboard_it.rs`, `docs_sync_it.rs`) spawn the real `codypendentd` binary via `env!("CARGO_BIN_EXE_codypendentd")` and drive it over the socket. They do not import the new lib, so they are the regression gate proving the bin still boots/serves/recovers identically.

Run:
```bash
cargo test -p codypendent-codypendentd
cargo clippy -p codypendent-codypendentd --all-targets -- -D warnings
```
Expected: all existing IT tests PASS unchanged, plus the new smoke test; clippy clean.

- [ ] **Step 8: Commit**

```bash
git add crates/codypendentd/Cargo.toml crates/codypendentd/src/lib.rs crates/codypendentd/src/main.rs crates/codypendentd/tests/run_daemon_lib_it.rs
git commit -m "codypendentd: expose run_daemon as a library alongside the bin

Add a [lib] target and lib.rs exposing init_tracing() + run_daemon(paths),
moving the run-loop out of main.rs verbatim. The standalone codypendentd
binary shrinks to init_tracing + resolve paths + run_daemon and keeps
byte-for-byte behavior; a new lib smoke test proves run_daemon starts,
binds its socket, and shuts down cleanly.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 2: CLI depends on the daemon lib + a hidden `__daemon` subcommand (`crates/cli`)

**Files:**
- Modify: root `Cargo.toml` (add `codypendent-codypendentd` to `[workspace.dependencies]`)
- Modify: `crates/cli/Cargo.toml` (add the dependency)
- Modify: `crates/cli/src/main.rs` (hidden `__daemon` subcommand + early dispatch + tests)

**Interfaces:**
- Consumes (from Task 1): `codypendent_codypendentd::init_tracing()` and `codypendent_codypendentd::run_daemon(paths: RuntimePaths) -> anyhow::Result<()>` where `RuntimePaths` is `codypendent_protocol::discovery::RuntimePaths`.
- Produces: a hidden `TopCommand::InternalDaemon` clap variant (command name `__daemon`) that, when parsed, runs the in-process daemon. Task 3 relies on this command name existing so `current_exe __daemon` works.

- [ ] **Step 1: Measure the BASELINE `codypendent` binary size + rebuild time (before adding the dep)**

Do this *before* editing any file in this task, so the "before" is the pre-single-binary CLI. Record the numbers (paste them into this task's commit message).

```bash
# Size of the release CLI binary as it is today.
cargo build --release --bin codypendent
ls -l target/release/codypendent | awk '{print "before size (bytes):", $5}'

# Incremental relink/codegen time for the CLI bin (isolates the CLI's own cost).
touch crates/cli/src/main.rs
{ time cargo build --release --bin codypendent ; } 2>&1 | grep -E '^(real|user|sys)'
```
Expected: a size in bytes and a `real` wall time. Keep them.

- [ ] **Step 2: Declare `codypendent-codypendentd` in the workspace dependency table**

In root `Cargo.toml`, under `[workspace.dependencies]` in the "Internal crates" block, add the entry (it is currently absent — `{ workspace = true }` in `crates/cli/Cargo.toml` cannot resolve without it). Place it right after the `codypendent-daemon` line to keep the internal crates grouped:

```toml
codypendent-daemon = { path = "crates/daemon" }
codypendent-codypendentd = { path = "crates/codypendentd" }
```

- [ ] **Step 3: Add the dependency to `crates/cli/Cargo.toml`**

Add to `[dependencies]` (place it next to the other daemon-side deps, e.g. just after the `codypendent-runtime = { workspace = true }` line). The comment records why the edge is acyclic and dep-free:

```toml
# The daemon run-loop as a library (`run_daemon`/`init_tracing`), so `codypendent
# __daemon` runs the SAME daemon in-process — no separate `codypendentd` on disk
# to go stale. Acyclic: `codypendentd` never depends on `cli`, and every crate it
# pulls in is already a `cli` dependency, so this adds no new external crate.
codypendent-codypendentd = { workspace = true }
```

- [ ] **Step 4: Confirm the dependency edge is acyclic and dep-clean**

Run:
```bash
cargo tree -p codypendent-cli -i codypendent-codypendentd   # cli -> codypendentd edge exists
cargo tree -p codypendent-codypendentd -i codypendent-cli 2>&1 | grep -q codypendent-cli \
  && echo "CYCLE! codypendentd depends on cli" || echo "acyclic: codypendentd does not depend on cli"
cargo build -p codypendent-cli
```
Expected: the first shows `codypendent-cli` depends on `codypendent-codypendentd`; the second prints "acyclic…"; the build succeeds. If cargo reports a cyclic dependency error, stop — the acyclicity assumption is violated.

- [ ] **Step 5: Write the failing clap-structure tests**

Add an inline test module at the end of `crates/cli/src/main.rs`. `Cli`/`TopCommand` are private to the bin crate, so the tests must live in `main.rs` itself (bin unit tests). They assert the `__daemon` subcommand (a) parses to the new hidden variant and (b) does not appear in `--help`.

```rust
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
}
```

- [ ] **Step 6: Run the tests to verify they fail**

Run:
```bash
cargo test -p codypendent-cli --bin codypendent tests:: 2>&1 | tail -20
```
Expected: FAIL to compile with `no variant ... named InternalDaemon` (the variant does not exist yet). That is the expected red state.

- [ ] **Step 7: Add the hidden `__daemon` variant to `TopCommand`**

In `crates/cli/src/main.rs`, add a new variant to `enum TopCommand`. Put it first so it reads as the special internal command. `#[command(name = "__daemon", hide = true)]` gives it the literal name `__daemon` and hides it from help:

```rust
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
    // ... (all existing variants unchanged) ...
}
```

- [ ] **Step 8: Dispatch `__daemon` before the normal CLI, and keep the match exhaustive**

In `main`, intercept `InternalDaemon` immediately after `Cli::parse()` — before `RuntimePaths::resolve()` for the normal path, theme resolution, and the bare-invocation TUI branch — so it runs like `codypendentd` (init tracing, resolve paths, run the loop). Edit the top of `main`:

```rust
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
    // ... existing theme_override + bare-invocation TUI branch, unchanged ...
```

Then add a match arm so `match command { … }` stays exhaustive (the early return already handled it):

```rust
    match command {
        // Dispatched before the match (see the early return in `main`); a
        // parsed `InternalDaemon` never reaches here.
        TopCommand::InternalDaemon => unreachable!("__daemon is dispatched before the match"),
        TopCommand::Daemon { command } => match command {
        // ... existing arms unchanged ...
```

- [ ] **Step 9: Run the clap-structure tests to verify they pass**

Run:
```bash
cargo test -p codypendent-cli --bin codypendent tests::
```
Expected: PASS — `daemon_subcommand_parses_to_internal_daemon` and `internal_daemon_is_hidden_from_help` both green. Also eyeball that `__daemon` is hidden:
```bash
cargo run -q --bin codypendent -- --help | grep -c __daemon   # expect: 0
```

- [ ] **Step 10: Measure the AFTER `codypendent` binary size + rebuild time**

Now that the CLI links the daemon lib, re-measure with the same commands as Step 1:

```bash
cargo build --release --bin codypendent
ls -l target/release/codypendent | awk '{print "after size (bytes):", $5}'
touch crates/cli/src/main.rs
{ time cargo build --release --bin codypendent ; } 2>&1 | grep -E '^(real|user|sys)'
```
Record the before→after size delta and rebuild-time delta in the commit message. (Expectation per the design note: a modest increase — the CLI already links daemon/runtime/workflow/eval/etc.; only `codypendentd`'s own assembly glue is new. There is also a one-time cost to compile `codypendent-codypendentd` on the first build.)

- [ ] **Step 11: Full gate + commit**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p codypendent-cli
git add Cargo.toml crates/cli/Cargo.toml crates/cli/src/main.rs
git commit -m "cli: link the daemon lib and add the hidden \`__daemon\` subcommand

codypendent-cli now depends on codypendent-codypendentd (an acyclic
intra-workspace edge; no new external crate). A hidden clap subcommand
\`__daemon\` — dispatched before normal CLI/TUI setup — runs the daemon
in-process via run_daemon, so \`codypendent __daemon\` IS the daemon.

Binary size before -> after: <BEFORE> -> <AFTER> bytes.
CLI relink time before -> after: <BEFORE>s -> <AFTER>s.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```
(Replace the `<BEFORE>`/`<AFTER>` placeholders with the Step 1/Step 10 measurements before committing.)

---

## Task 3: The CLI spawns the daemon from itself (`crates/cli/src/commands.rs`)

**Files:**
- Modify: `crates/cli/src/commands.rs` (`resolve_daemon_binary`, `ensure_daemon`, new `DaemonInvocation`/`daemon_command`, tests)

**Interfaces:**
- Consumes (from Task 2): the `codypendent __daemon` command name (the argv this task spawns).
- Produces: `resolve_daemon_binary() -> DaemonInvocation` (`{ program: PathBuf, args: Vec<String> }`) and `daemon_command(&DaemonInvocation) -> std::process::Command`. Internal to `crates/cli`; no later task depends on them.

- [ ] **Step 1: Write the failing unit tests for invocation resolution + argv**

Add an inline test module at the end of `crates/cli/src/commands.rs`. `resolve_daemon_binary`/`daemon_command`/`resolve_daemon_invocation`/`DaemonInvocation` are private to the crate, so the tests live in the module. They assert: the self-spawn form (program = `current_exe`, args = `["__daemon"]`), the fallback (program = `codypendentd`, no args), and the built `Command`'s program + argv.

```rust
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
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:
```bash
cargo test -p codypendent-cli daemon_spawn_tests:: 2>&1 | tail -20
```
Expected: FAIL to compile — `cannot find type DaemonInvocation` / `cannot find function resolve_daemon_invocation` / `daemon_command`. That is the expected red state.

- [ ] **Step 3: Replace `resolve_daemon_binary` with the `DaemonInvocation`-returning form**

In `crates/cli/src/commands.rs`, replace the existing `resolve_daemon_binary` function (the one that returns `PathBuf` and looks for a sibling `codypendentd`) with the struct, the pure resolver, and the command builder:

```rust
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
```

- [ ] **Step 4: Rewire `ensure_daemon` to spawn the resolved invocation**

In `ensure_daemon`, replace the `let daemon_binary = resolve_daemon_binary();` line and the `std::process::Command::new(&daemon_binary)` construction so it uses `daemon_command`, and update the spawn error context to name the resolved program. The socket-present short-circuit, `ensure_directories`, the `daemon.log` open, the `#[cfg(unix)] process_group(0)`, and the 5-second readiness poll all stay exactly as they are. The changed region becomes:

```rust
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
```

Also update the `ensure_daemon` doc comment's first line from "Spawn `codypendentd` detached…" to reflect the self-spawn, e.g.: `/// Spawn the daemon (`codypendent __daemon`, this binary itself) detached if`. Leave the `start` doc comment's wording as-is unless clippy/readers complain — it is non-load-bearing.

- [ ] **Step 5: Run the unit tests to verify they pass**

Run:
```bash
cargo test -p codypendent-cli daemon_spawn_tests::
```
Expected: PASS — all four tests green (self-spawn resolution, fallback, real `current_exe`, and the `current_exe __daemon` argv).

- [ ] **Step 6: Full gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p codypendent-cli
```
Expected: clean. Watch specifically for an unused-import/dead-code warning on the old sibling-`codypendentd` logic — the replacement removed it, so there should be none.

- [ ] **Step 7: MANUAL end-to-end verification (gated — do once, not in CI)**

Prove the single binary auto-starts its own daemon with **no** `codypendentd` on disk or PATH (the spec's e2e item). In a scratch dir:

```bash
# Build only the CLI; do NOT build codypendentd.
cargo build --release --bin codypendent
mkdir -p /tmp/sb-e2e/bin && cp target/release/codypendent /tmp/sb-e2e/bin/
# Fresh data dir + a PATH containing ONLY our codypendent (no codypendentd).
export CODYPENDENT_DATA_DIR=/tmp/sb-e2e/data
env -i HOME="$HOME" CODYPENDENT_DATA_DIR="$CODYPENDENT_DATA_DIR" PATH=/tmp/sb-e2e/bin \
  /tmp/sb-e2e/bin/codypendent daemon start
env -i HOME="$HOME" CODYPENDENT_DATA_DIR="$CODYPENDENT_DATA_DIR" PATH=/tmp/sb-e2e/bin \
  /tmp/sb-e2e/bin/codypendent daemon status
# Confirm the running daemon process is `codypendent __daemon`:
pgrep -af "codypendent __daemon" || echo "NOTE: check ps for the __daemon process"
env -i HOME="$HOME" CODYPENDENT_DATA_DIR="$CODYPENDENT_DATA_DIR" PATH=/tmp/sb-e2e/bin \
  /tmp/sb-e2e/bin/codypendent daemon stop
```
Expected: `daemon started (pid N)`, then `running yes`, a `codypendent __daemon` process, then `daemon stopped` — all with no `codypendentd` binary anywhere. Check `/tmp/sb-e2e/data`'s `daemon.log` has the daemon's tracing output (proves `init_tracing` ran on the `__daemon` path).

- [ ] **Step 8: Commit**

```bash
git add crates/cli/src/commands.rs
git commit -m "cli: spawn the daemon from the codypendent binary itself

resolve_daemon_binary now returns the current_exe + \`__daemon\` argv, and
ensure_daemon spawns \`codypendent __daemon\` (keeping the detached / new
process-group / stdout->daemon.log setup and the socket short-circuit). If
current_exe is unavailable it falls back to a PATH codypendentd. Updating
codypendent now always spawns a matching daemon — no version skew.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 4: Release + installer ship one binary

**Files:**
- Modify: `.github/workflows/release.yml` (tarball packaging)
- Modify: `install.sh` (require only `codypendent`; `codypendentd` optional)

**Interfaces:** none (build/release scripts; no Rust interface). Both files exist on this branch (verified), so this task is unconditional.

- [ ] **Step 1: Make `codypendentd` optional in the release tarball**

In `.github/workflows/release.yml`, in the "Package the tarball" step, keep the `codypendent` copy hard (it is the primary artifact) and make the `codypendentd` copy non-fatal. Leave the "Build release binaries" step building both (`--bin codypendent --bin codypendentd`) so advanced users still get the standalone daemon. Replace the two `cp` lines:

Current:
```bash
          cp "target/${{ matrix.target }}/release/codypendent" "$dist/"
          cp "target/${{ matrix.target }}/release/codypendentd" "$dist/"
```
New:
```bash
          # The primary artifact is the single self-contained `codypendent`
          # binary (it runs the daemon from itself via `codypendent __daemon`).
          # It MUST be present.
          cp "target/${{ matrix.target }}/release/codypendent" "$dist/"
          # `codypendentd` stays as an OPTIONAL standalone daemon for advanced
          # use; ship it if this build produced it, but never fail if absent.
          cp "target/${{ matrix.target }}/release/codypendentd" "$dist/" 2>/dev/null || true
```

- [ ] **Step 2: Verify the workflow YAML still parses**

Run (uses `actionlint` if available, else a YAML syntax check):
```bash
actionlint .github/workflows/release.yml 2>/dev/null \
  || python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/release.yml')); print('release.yml: YAML OK')"
```
Expected: no errors / "YAML OK".

- [ ] **Step 3: Make the installer require only `codypendent`**

In `install.sh`, update the header comment and the presence check + install so `codypendent` alone is sufficient and `codypendentd` is installed only when present.

(a) Update the top comment (line ~2) from:
```bash
# and installs `codypendent` + `codypendentd` onto your PATH.
```
to:
```bash
# and installs `codypendent` onto your PATH (self-sufficient — it runs the
# daemon from itself; the optional standalone `codypendentd` is installed too if
# the tarball carries it).
```

(b) Replace the binaries-present check (the line asserting both `-x codypendent` and `-x codypendentd`):
```bash
[ -x "$src/codypendent" ] && [ -x "$src/codypendentd" ] || { echo "error: binaries missing in $asset" >&2; exit 1; }
```
with a check for only the primary binary:
```bash
[ -x "$src/codypendent" ] || { echo "error: codypendent binary missing in $asset" >&2; exit 1; }
```

(c) Replace the install block (the `# 5. Install both binaries …` comment through the `install -m 0755 … codypendentd …` sudo branch) with one that always installs `codypendent` and adds `codypendentd` only if present:
```bash
# 5. Install `codypendent` (self-sufficient — it runs the daemon from itself
#    via `codypendent __daemon`). Also install the OPTIONAL standalone
#    `codypendentd` if the tarball carried it. Use sudo only if the target dir
#    is not writable.
bins=("$src/codypendent")
[ -x "$src/codypendentd" ] && bins+=("$src/codypendentd")
mkdir -p "$BINDIR" 2>/dev/null || true
if [ -w "$BINDIR" ]; then
  install -m 0755 "${bins[@]}" "$BINDIR"/
else
  echo "codypendent: $BINDIR is not writable — using sudo"
  sudo install -m 0755 "${bins[@]}" "$BINDIR"/
fi
```

(d) Replace the final "installed …" echo so it only names `codypendentd` when it was installed:
```bash
if [ -x "$src/codypendentd" ]; then
  echo "codypendent: installed $BINDIR/codypendent and $BINDIR/codypendentd"
else
  echo "codypendent: installed $BINDIR/codypendent"
fi
```

- [ ] **Step 4: Verify the installer parses and its guards behave**

Syntax-check, then dry-run the presence guard both ways (a `src` with only `codypendent`, and one with both) using the same shell constructs the script uses:

```bash
bash -n install.sh && echo "install.sh: syntax OK"

# codypendent-only tarball must pass the guard and install just codypendent.
work="$(mktemp -d)"; mkdir -p "$work/src"; : > "$work/src/codypendent"; chmod +x "$work/src/codypendent"
( src="$work/src"
  [ -x "$src/codypendent" ] || { echo "FAIL: primary check rejected codypendent-only"; exit 1; }
  bins=("$src/codypendent"); [ -x "$src/codypendentd" ] && bins+=("$src/codypendentd")
  [ "${#bins[@]}" -eq 1 ] && echo "OK: codypendent-only -> installs 1 binary" || echo "FAIL: expected 1 binary" )

# both-present tarball must install two.
: > "$work/src/codypendentd"; chmod +x "$work/src/codypendentd"
( src="$work/src"
  bins=("$src/codypendent"); [ -x "$src/codypendentd" ] && bins+=("$src/codypendentd")
  [ "${#bins[@]}" -eq 2 ] && echo "OK: both present -> installs 2 binaries" || echo "FAIL: expected 2 binaries" )
rm -rf "$work"
```
Expected: `syntax OK`, `OK: codypendent-only -> installs 1 binary`, `OK: both present -> installs 2 binaries`.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/release.yml install.sh
git commit -m "release+install: ship codypendent as the primary single binary

The release tarball must contain codypendent; codypendentd becomes an
optional standalone daemon (copied if built, never fatal if absent). The
installer requires only codypendent (self-sufficient via \`codypendent
__daemon\`) and installs codypendentd only when the tarball carries it.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Final verification (after all tasks)

- [ ] **Whole-workspace gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check           # licences/advisories still clean — no new external dep
```
Expected: all green. `cargo deny` is unchanged because the only new crate in the graph is the intra-workspace `codypendent-codypendentd`, whose external deps are already a subset of the CLI's.

- [ ] **Both binaries still build**

```bash
cargo build --release --bin codypendent --bin codypendentd
```
Expected: both build. `codypendent` runs the daemon from itself; `codypendentd` remains for direct/advanced use.

---

## Self-Review

**1. Spec coverage** — every spec section maps to a task:

- *Architecture §1 (daemon run-loop as a library; `run_daemon`; `main.rs` shrinks; `[lib]` target name `codypendent_codypendentd`)* → **Task 1** (Steps 1-3). Byte-for-byte standalone bin behavior preserved (init order kept; Step 7 regression-gates via the existing IT suite).
- *Architecture §2 (`cli` depends on the daemon lib; hidden `__daemon` clap subcommand dispatched before normal CLI; accepts the binary-size growth)* → **Task 2** (dep in Steps 2-3; hidden subcommand + early dispatch in Steps 7-8; size/build growth measured in Steps 1, 10).
- *Architecture §3 (`resolve_daemon_binary → current_exe`; `ensure_daemon` spawns `current_exe __daemon` with the detached/new-process-group/log setup + socket short-circuit; fallback to sibling/PATH `codypendentd`)* → **Task 3** (Steps 3-4; fallback covered in Step 1's `falls_back_…` test).
- *Architecture §4 (release primary artifact `codypendent`, `codypendentd` optional; installer installs just `codypendent`, keeps `codypendentd` if present)* → **Task 4**.
- *Testing bullets* → Task 1 Step 5-7 (lib callable smoke + bin unchanged), Task 3 Step 1 (`resolve_daemon_binary` returns current_exe; `ensure_daemon` argv; fallback), Task 2 Step 5 (`__daemon` dispatch + hidden from `--help`), Task 3 Step 7 (gated e2e: single binary, no `codypendentd`, auto-starts). "All existing tests green; no golden-vector change" → Final verification.
- *Constraints* → Global Constraints section (verbatim) + Task 2 Step 4 (acyclic) + Final `cargo deny`.
- *Open question (binary size/build time)* → Task 2 Steps 1, 10, 11 (measured before→after, recorded in the commit).

No spec requirement is left without a task.

**2. Placeholder scan** — no `TBD`/`TODO`/"implement later"/"add error handling"/"similar to Task N". The only intentional fill-ins are the `<BEFORE>`/`<AFTER>` measurement numbers in Task 2's commit message, which Steps 1 and 10 produce and Step 11 instructs to substitute — a runtime measurement, not undefined code. All code steps show complete code; the large `run_daemon` body is reproduced in full (a verbatim move, not a sketch).

**3. Type/signature consistency** — the entry point is identical everywhere it appears:
- Task 1 defines `pub async fn run_daemon(paths: RuntimePaths) -> anyhow::Result<()>` and `pub fn init_tracing()` (with `RuntimePaths = codypendent_protocol::discovery::RuntimePaths`).
- Task 1's `main.rs` calls `codypendent_codypendentd::init_tracing()` then `codypendent_codypendentd::run_daemon(paths).await`.
- Task 1's smoke test calls `codypendent_codypendentd::run_daemon(paths).await` (paths from `RuntimePaths::from_data_dir`).
- Task 2's `__daemon` dispatch calls `codypendent_codypendentd::init_tracing()` then `codypendent_codypendentd::run_daemon(paths).await` (paths from `RuntimePaths::resolve()?` + `ensure_directories()?`) — same signature, same arg type.
- Task 3's `DaemonInvocation { program: PathBuf, args: Vec<String> }`, `resolve_daemon_binary() -> DaemonInvocation`, `resolve_daemon_invocation(std::io::Result<PathBuf>) -> DaemonInvocation`, and `daemon_command(&DaemonInvocation) -> std::process::Command` are used consistently across the implementation (Steps 3-4) and the tests (Step 1). The `__daemon` command name Task 3 spawns matches the `#[command(name = "__daemon")]` Task 2 registers.

No mismatched names or signatures found.
