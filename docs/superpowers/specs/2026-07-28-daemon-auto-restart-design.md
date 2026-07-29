# Daemon auto-restart on version mismatch — design

## Problem

Codypendent ships as a **single binary**: the client and the daemon are the same
executable, and the client spawns the daemon by re-running its own `current_exe`
with the hidden `__daemon` subcommand (`crates/cli/src/commands.rs`,
`resolve_daemon_invocation` → `DaemonInvocation { program, args: ["__daemon"] }`
— the #30 single-binary path). The single-binary design deliberately removed the
*on-disk* skew between a `codypendent` client and a separate `codypendentd`.

But it did **not** remove the *in-memory* skew. The daemon is a long-running
process that holds the loaded binary's code in memory. After a user reinstalls
(overwrites the binary on disk), the still-running daemon keeps executing the
**old** code. Only the client picks up the new code, because it re-execs from
disk each invocation. To adopt daemon-side changes the user must currently run
`codypendent daemon stop && codypendent daemon start` by hand — a step nobody
remembers, so users silently run stale daemon logic after every upgrade.

## Goal

The **client** should detect, at connect time, that the running daemon is a
**different build** than the client binary, and transparently restart the daemon
so the new daemon code is loaded — **without** the user doing the stop/start
dance, and **without ever killing an in-flight run**.

## Approach (summary of the decisions)

1. Embed a **per-build identifier** in the single binary via a `build.rs` in the
   `protocol` crate (`codypendent_protocol::BUILD_ID`) — a compile-time constant
   both the client half and the daemon half link, because they are one binary in
   one build.
2. The daemon reports its running `BUILD_ID` to a connecting client by an
   **additive field on the existing `ServerHello`** (`#[serde(default)]`) — no
   new protocol message, zero extra round-trips on the common (matching) path.
3. The client compares `ServerHello.build_id` against its own compile-time
   `BUILD_ID` **right after the handshake**, before entering the TUI.
4. On a mismatch, the client gates on **idle vs active**: if the daemon has **no
   active runs**, it gracefully stops it (reuse `daemon stop`), spawns a fresh
   one (reuse the #30 spawn path), reconnects, and proceeds — with a one-line
   status message. If **any run is active**, it does **not** restart: it warns
   and continues on the old daemon (Decision B, below). It never kills a run.
5. Concurrent clients are serialised by an advisory restart lock; the losers
   re-check and no-op.
6. Any restart failure is surfaced as a legible error; the client never enters
   the TUI against a dead or half-restarted daemon.

---

## Verified: the real seams (as of `66a64a4`)

- **No per-build identifier exists today.** There is **no `build.rs`** anywhere
  in the workspace. The only version compiled in is `env!("CARGO_PKG_VERSION")`
  = `"0.1.0"`, used for both `ClientHello.client_version` and the daemon's
  `ServerHello.daemon_version` / `DaemonStatus.daemon_version`
  (`crates/daemon/src/server.rs:616,2004`). That is a *release* version — it does
  not change build-to-build, so it cannot detect a reinstall of the same
  release. `DaemonInstanceId` and `boot_count` exist but change every **boot** of
  the *same* binary, so they cannot distinguish a code change either. **A new
  per-build id must be added.**
- **The handshake already has the right shape.** `crates/protocol/src/handshake.rs`
  defines `ClientHello`/`ServerHello`; `ServerHello` already carries
  `daemon_version: String` and precedent for additive `#[serde(default)]` fields
  (`resume_token`, with a legacy-parse test). This is exactly where the daemon's
  build id belongs — extend it additively rather than adding a new message.
- **The connect flow is centralised.** `crates/cli/src/connection.rs`
  (`Connection::connect` → `Connection::handshake` returning `ServerHello`), and
  the TUI entrypoint `crates/cli/src/tui.rs::run()` calls
  `commands::ensure_daemon` → `Connection::connect` → `conn.handshake(...)` and
  binds the returned `hello` — the detection point drops in immediately after.
- **Spawn + stop are ready to reuse.** `commands::ensure_daemon`
  (`crates/cli/src/commands.rs:39`) is ping-gated (won't double-spawn) and spawns
  `current_exe __daemon` detached. `commands::stop` (`:92`) sends a graceful
  `Payload::Shutdown` via `client::shutdown` and waits (5 s budget) for the
  socket to stop answering `Ping`. Graceful `Shutdown` (`server.rs:596`) flips a
  watch channel and the process exits — it does **not** drain in-flight runs, so
  stopping while a run is live **kills** it. There is **no `daemon restart`**
  subcommand today (only `Start`/`Stop`/`Status` in `main.rs`).
- **Active-run state is queryable but not yet surfaced.** `RunState`
  (`crates/protocol/src/run.rs`) has terminal states `Completed` / `Failed` /
  `Cancelled` and non-terminal `Queued` / `Preparing` / `Running` /
  `WaitingForApproval` / `WaitingForUserInput` / `Paused` / `Recovering`. Runs are
  rows in a `runs` table queried by `state` (`executor.rs:333`:
  `SELECT ... FROM runs WHERE state = ?`). `DaemonStatus` reports `session_count`
  (via `ledger::session_count`, `ledger.rs:207`) but **not** an active-run count —
  one must be added.

---

## Architecture

### 1. Build identity — `crates/protocol/build.rs` + `BUILD_ID`

Add a `build.rs` to the **protocol** crate (a leaf both `cli` and `daemon` depend
on, compiled exactly once per `cargo build`), emitting a constant:

```rust
// crates/protocol/src/lib.rs (generated value injected by build.rs)
/// A per-build identifier, identical across the whole single binary because the
/// client half and the daemon half are one crate graph in one build.
pub const BUILD_ID: &str = env!("CODYPENDENT_BUILD_ID");
```

`build.rs` computes `CODYPENDENT_BUILD_ID` deterministically, in this precedence:

1. **Explicit override** — if the environment variable `CODYPENDENT_BUILD_ID` is
   already set (packagers / reproducible-build pipelines), use it verbatim. This
   makes the id fully controllable and deterministic where determinism matters.
2. **Git-derived** — else run `git rev-parse --short=12 HEAD` in `CARGO_MANIFEST_DIR`.
   If it succeeds, run `git status --porcelain --untracked-files=no`; a non-empty
   result appends `-dirty`. Format: `"{CARGO_PKG_VERSION}+{shorthash}[-dirty]"`,
   e.g. `0.1.0+a1b2c3d4e5f6` or `0.1.0+a1b2c3d4e5f6-dirty`.
3. **Fallback** — if git is unavailable or this is not a git tree (source tarball,
   `cargo install` from a registry), use `"{CARGO_PKG_VERSION}"` alone.

Key properties (satisfying the constraints):

- **No clean tree required.** A dirty tree still builds; it just yields a
  `-dirty` suffix. Two different dirty trees on the same HEAD may collide on id,
  which is acceptable — dirty builds are developer builds, and the worst case is
  a *missed* restart, never a wrong one.
- **No network.** `git rev-parse`/`git status` are local.
- **Reproducible.** The id is a pure function of (HEAD, tracked-tree-dirtiness),
  with **no timestamp** and no build-host data embedded, so a reproducible
  pipeline pins it via the override env var and gets a stable binary.
- **Rebuild triggering.** `build.rs` emits `cargo:rerun-if-changed=` for
  `../../.git/HEAD` and the resolved ref file (when they exist), and
  `cargo:rerun-if-env-changed=CODYPENDENT_BUILD_ID`, so the constant is refreshed
  when HEAD moves. (A missed rerun in a dev loop only risks a stale-but-harmless
  id; `cargo clean` or a release build always regenerates.)

**Why the protocol crate, not `cli`/`daemon` separately:** the daemon fills
`ServerHello.build_id` from code in `crates/daemon`, while the client reads its
own id from code in `crates/cli`. Both must resolve to the **same** constant for
a same-binary build, so the constant must live in a crate **both** link —
`protocol` is that crate, and it is already the home of the wire types.

### 2. Reporting the daemon's build id — additive protocol change

Extend `ServerHello` (`crates/protocol/src/handshake.rs`) with one additive
field, defaulted for wire compatibility exactly like `resume_token`:

```rust
pub struct ServerHello {
    pub selected_protocol: ProtocolVersion,
    pub daemon_version: String,          // unchanged: human-facing semver
    pub daemon_instance: DaemonInstanceId,
    pub heartbeat_interval_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_token: Option<ResumeToken>,
    /// The running daemon's per-build id (`BUILD_ID`). Defaulted for wire
    /// compatibility with daemons predating this field.
    #[serde(default)]
    pub build_id: String,
}
```

Extend `DaemonStatus` (`crates/protocol/src/envelope.rs`) with the build id **and**
an active-run count, both additive:

```rust
pub struct DaemonStatus {
    // ...existing fields...
    #[serde(default)]
    pub build_id: String,
    /// Count of runs in a non-terminal state (see idle definition).
    #[serde(default)]
    pub active_run_count: u64,
}
```

This is **additive**: `major` stays `1`, `minor` bumps to `2`
(`crates/protocol/src/version.rs`). `#[serde(default)]` means an older daemon that
does not emit `build_id` deserializes it to `""`, and an older *client* ignores
the new field. **Golden vectors are regenerated** (`crates/protocol/tests/golden_vectors.rs`,
which currently pins `daemon_version: "0.1.0"`), and legacy-parse tests are added
(mirroring the existing `server_hello_round_trips` legacy case) proving a hello
with no `build_id` still parses.

**Why `build_id` on `ServerHello`:** the TUI already performs a handshake and
binds `hello`. Putting the id there means the **matching** case (the overwhelming
common case) costs **zero** extra round-trips — the check is a string compare on
data already in hand. The extra `DaemonStatusRequest` (for `active_run_count`) is
issued **only** on the rare detected mismatch.

### 3. Daemon side — fill the fields

- `crates/daemon/src/server.rs`: set `build_id: codypendent_protocol::BUILD_ID.to_string()`
  in both the `ServerHello` construction (`:616`) and the `DaemonStatus`
  construction (`status()`, `:2004`).
- Add `ledger::active_run_count(pool) -> anyhow::Result<i64>` mirroring
  `session_count`, backed by
  `SELECT COUNT(*) FROM runs WHERE state NOT IN ('Completed','Failed','Cancelled')`,
  and wire it into `status()` as `active_run_count`. Terminal set is the three
  terminal `RunState`s; every other state counts as active.

### 4. Detection point — `crates/cli/src/tui.rs::run()`

Immediately after `let hello = conn.handshake("codypendent-tui", BUILD_ID, resume).await?`
and **before** `resolve_or_create_session`, call a reconcile step. (Also pass the
client id through `client_version`/a dedicated arg so the handshake advertises
`BUILD_ID`; the existing `env!("CARGO_PKG_VERSION")` argument is replaced by
`BUILD_ID` for the TUI client so both halves speak the same id vocabulary.)

The check is a pure comparison: `daemon_id := hello.build_id`; mismatch when
`daemon_id != BUILD_ID`. An empty `daemon_id` (a pre-feature daemon) is treated
as a mismatch — such a daemon is by definition an older build than any client
that has the field — subject to the same idle gate below.

**Scope.** Auto-restart is wired into the **interactive TUI / attach** path only.
The headless `codypendent run --jsonl` path (`commands::run`) does **not**
auto-restart: its stdout is a strict JSONL stream and it may be one invocation in
a scripted batch, so a silent daemon bounce mid-script is wrong. `run --jsonl`
instead emits a one-line **stderr** warning on mismatch and proceeds. This keeps
the "transparent when safe" behaviour where it belongs (a human at the TUI) and
avoids surprising automation.

### 5. Restart flow (idle path)

Given a detected mismatch, `reconcile_daemon_build(paths, &hello)` runs:

1. **Confirm idle.** Issue `client::daemon_status(socket)`. If it fails → cannot
   confirm idleness → **warn-and-continue** (fail-safe; never restart on
   uncertainty). If `active_run_count > 0` → **active path** (§6).
2. **Announce.** Print to **stderr**: `codypendent: a newer build is installed;
   restarting the daemon to load it…`.
3. **Acquire the restart lock** (§7). If it cannot be created → log and
   **warn-and-continue** (never block the user from working).
4. **Re-check under the lock** (closes the TOCTOU window): re-issue
   `daemon_status`.
   - If `build_id == BUILD_ID` now → another client already restarted it →
     release lock, **reconnect + re-handshake**, proceed. (Idempotent no-op.)
   - If `active_run_count > 0` now (a run started since step 1) → release lock,
     take the **active path** (§6). Never stop a daemon that became busy.
5. **Stop.** `client::shutdown(socket)` (the graceful `daemon stop` path) and wait
   (5 s budget) for `Ping` to stop answering. If it keeps answering → hard error
   (§8).
6. **Spawn.** `commands::ensure_daemon(paths)` — ping-gated spawn of
   `current_exe __daemon`, waits (5 s budget) for readiness. If it fails →
   propagate its legible error (§8).
7. **Release the lock.**
8. **Reconnect + re-handshake** and assert the fresh `hello.build_id == BUILD_ID`.
   If it **still** mismatches → hard error (§8): the on-disk binary itself is
   stale or a different `codypendent` is on `PATH`. Do **not** enter the TUI.
9. Continue `run()` with the fresh `Connection`.

The total added latency on the **matching** path is a single string compare; the
restart path pays only when a genuine reinstall happened.

### 6. THE KEY DECISION — active runs (Decision B: idle-only auto-restart)

**Chosen: (B) auto-restart only when idle; when runs are active, warn and
continue on the old daemon.** Rationale: silently killing an in-flight agent run
to pick up a code change is unacceptable data loss — the run's in-memory state,
partial work, and pending approvals would vanish. A prompt (option C) breaks the
"transparent, no manual step" goal and is disruptive mid-work. Restarting anyway
(option A) is the one behaviour we explicitly forbid.

On the active path, `reconcile_daemon_build`:

- Does **not** stop or spawn anything.
- Emits a single **stderr** line and a non-blocking **TUI status/banner line**:
  `A newer build is installed. The daemon will keep serving the current run(s)
  on the old build; it will switch automatically the next time you launch while
  idle, or run `codypendent daemon restart` when your runs finish.`
- Returns "proceed on the existing connection" — the user keeps working against
  the running (old) daemon with zero interruption.

**How "idle" is determined:** `DaemonStatus.active_run_count == 0`, where
`active_run_count` counts `runs` rows whose `state` is **not** one of the three
terminal states (`Completed`, `Failed`, `Cancelled`). Every non-terminal state —
including `WaitingForApproval`, `WaitingForUserInput`, and `Paused` — counts as
active, because the run is alive and its state lives in the daemon's memory; a
restart would lose it. Idle is **re-confirmed under the restart lock** (§5 step 4)
immediately before the stop, so a run that starts during the brief detection
window still cancels the restart.

Because the id is checked on **every** TUI launch, the deferred restart happens
naturally: the next time the user opens the TUI and the daemon is idle, the
mismatch is detected again and the (now safe) restart proceeds — no persisted
"pending upgrade" flag is needed.

### 7. Concurrency guard — advisory restart lock

Two clients can both detect the mismatch and both try to stop+spawn, racing into
the stop→spawn gap. Guard the whole stop/spawn/reconnect critical section with an
advisory lock file at `<data_dir>/daemon-restart.lock` (`RuntimePaths` already
exposes `data_dir`).

Mechanism (no new dependency): atomically claim the lock with
`OpenOptions::new().write(true).create_new(true)` and write the claimer's PID into
it; release by removing the file. Contention handling:

- **Loser blocks** by polling `daemon_status` (short bounded loop, e.g. up to the
  same 5 s budget). When the holder finishes, the loser observes
  `build_id == BUILD_ID` and **no-ops** (proceeds on a reconnect) — the restart is
  idempotent.
- **Stale lock** (holder crashed mid-restart): if `create_new` fails, read the PID
  in the file and probe liveness (`kill(pid, 0)` / `signal(0)`); if the holder is
  dead, remove the stale lock and retry the claim once. This needs no new crate
  and matches the repo's existing pidfile idiom (`server.rs::acquire_socket`).

This makes the restart **idempotent and single-flighted**: exactly one client
performs the stop+spawn; all others converge on the freshly spawned daemon.

*(Alternative considered: an OS `flock`/`fd-lock` advisory lock, which auto-releases
on holder crash and removes the staleness probe. Rejected as default only to
avoid a new dependency; noted as a minor open decision.)*

### 8. Failure handling

Every failure resolves to either a **legible hard error** (client exits, never
enters the TUI against a broken daemon) or a **safe degrade** (warn + continue on
the old daemon) — never a hang and never a silent half-restart:

| Failure | Handling |
| --- | --- |
| `daemon_status` query fails at detection | Degrade: warn + continue (can't confirm idle) |
| Runs active (or became active under lock) | Degrade: warn + continue (Decision B) |
| Lock cannot be created / claimed | Degrade: warn + continue (log the reason) |
| `shutdown` errors, or daemon still answers `Ping` after 5 s | **Hard error** (reuse `stop`'s "still answering after 5 seconds" message) |
| Spawn fails / socket not ready in 5 s | **Hard error** (propagate `ensure_daemon`'s "did not become ready … check <log>") |
| Reconnect handshake still mismatches | **Hard error**: `daemon restart did not load the new build (on-disk binary may be stale, or a different codypendent is on PATH); expected <BUILD_ID>, got <hello.build_id>` |

---

## Component decomposition (plan-task seeds)

- **T1 — protocol `build.rs` + `BUILD_ID`** (`crates/protocol`). Add `build.rs`
  implementing the 3-tier id precedence, `rerun-if-changed`/`rerun-if-env-changed`,
  and the `BUILD_ID` constant. Unit test: constant is non-empty and starts with
  `CARGO_PKG_VERSION`.
- **T2 — additive wire fields** (`crates/protocol`). Add `ServerHello.build_id`,
  `DaemonStatus.build_id`, `DaemonStatus.active_run_count`, all `#[serde(default)]`.
  Bump `PROTOCOL_V1` minor → `1.2`. Add round-trip + legacy-parse tests;
  regenerate golden vectors.
- **T3 — daemon fills the fields** (`crates/daemon`, `crates/codypendentd`). Set
  `build_id` in `ServerHello` and `DaemonStatus`; add
  `ledger::active_run_count` and wire it into `status()`.
- **T4 — `daemon status` display** (`crates/cli/src/commands.rs`). Show build id
  and active-run count in the human + JSON status output.
- **T5 — `daemon restart` subcommand** (`crates/cli`, `main.rs`). `stop` then
  `start`; the documented manual fallback for the active path.
- **T6 — pure restart decision** (`crates/cli`). A side-effect-free
  `decide_restart(client_id, daemon_id, active_run_count) -> Decision` where
  `Decision ∈ { Proceed, Restart, WarnActive, WarnUnknown }`. Fully unit-testable
  with no I/O (this is the crux of the "testable" constraint).
- **T7 — restart driver** (`crates/cli`). The effectful `reconcile_daemon_build`
  wrapping T6 with `daemon_status` / `shutdown` / `ensure_daemon` / reconnect, the
  stderr line, and the TUI banner.
- **T8 — restart lock** (`crates/cli`). The `<data_dir>/daemon-restart.lock`
  claim/release with PID-liveness staleness handling.
- **T9 — wire into `tui.rs::run()`** and the `run --jsonl` warn-only path;
  advertise `BUILD_ID` on the handshake.
- **T10 — integration test** (`crates/cli/tests`). Spawn a daemon reporting build
  id A, run a client with id B: (a) idle → asserts one restart and a matching
  reconnect; (b) with a live run → asserts **no** restart and a warning; (c) two
  concurrent clients → asserts a single restart.

---

## Data flow

```
TUI run():
  ensure_daemon(paths)                         # ping-gated; spawns current_exe __daemon if none
  conn = Connection::connect(socket)
  hello = conn.handshake("codypendent-tui", BUILD_ID, resume)
  ── reconcile_daemon_build(paths, &hello) ──
     decide_restart(BUILD_ID, hello.build_id, ?):
        match  -> Proceed
        mismatch:
           status = daemon_status(socket)?      # only on mismatch
           active>0 -> WarnActive  (continue on old daemon)
           idle     -> Restart:
                lock(<data_dir>/daemon-restart.lock)
                re-check status  -> already-matched? Proceed : active? WarnActive
                shutdown(socket); wait !ping (5s)
                ensure_daemon(paths); wait ping (5s)
                unlock
                reconnect + re-handshake; assert build_id == BUILD_ID
        query failed -> WarnUnknown (continue on old daemon)
  resolve_or_create_session(...)               # proceed as today
```

## Error handling / edge cases

- **Pre-feature daemon** (`build_id == ""`): treated as a mismatch and handled by
  the same idle gate — the first launch after installing this feature restarts an
  idle daemon to the new build; a busy one is deferred with a warning.
- **`current_exe` unavailable**: `resolve_daemon_invocation` already falls back to
  a `codypendentd` on `PATH`; the reconnect-mismatch hard error (§8) catches the
  case where that fallback is itself stale.
- **Same release, different build** (`0.1.0+hashA` vs `0.1.0+hashB`): detected,
  because the git hash differs — the whole point over bare `CARGO_PKG_VERSION`.
- **Reproducible / tarball build** (id degrades to bare `CARGO_PKG_VERSION`):
  auto-restart triggers only across semver bumps, which is the correct behaviour
  for release artifacts.

## Testing

- **Pure:** `decide_restart` truth table (match/mismatch × idle/active/unknown) —
  no I/O. `BUILD_ID` non-empty. Terminal-vs-active classification of every
  `RunState`.
- **Protocol:** `ServerHello`/`DaemonStatus` round-trip with and without the new
  fields; legacy hello (no `build_id`) parses to `""`; golden vectors regenerated
  and committed.
- **Integration:** the T10 scenarios (idle restart, active warn-no-restart,
  concurrent single-flight) against a real spawned daemon over a temp socket.

## Constraints

- **Never silently kill an in-flight run.** Active-run detection gates the
  restart (Decision B); idle is re-confirmed under the lock immediately before the
  stop. This is the crux of the design.
- **Transparent when safe.** On an idle mismatch the restart is automatic with a
  single stderr status line — no manual step. On an active mismatch, a
  non-blocking warning; the user keeps working.
- **Reuse existing paths.** The restart reuses `daemon stop`
  (`client::shutdown`) and the #30 spawn (`ensure_daemon` → `current_exe __daemon`);
  it introduces no second spawn/stop mechanism.
- **Build id is reproducible and offline.** No clean tree required (dirty ⇒
  suffix), no network, no embedded timestamp; overridable via
  `CODYPENDENT_BUILD_ID` for deterministic pipelines.
- **Additive protocol only.** Extend `ServerHello`/`DaemonStatus` with
  `#[serde(default)]` fields; bump minor to `1.2`; regenerate golden vectors. No
  new message, no breaking change.
- **Negligible startup latency.** The matching path is one string compare on data
  already in the handshake; the extra status query runs only on a real mismatch.
- **Testable.** The restart decision is a pure function; the effectful driver is
  covered by integration tests with a real daemon.

## Non-goals / follow-ups

- Restarting the daemon while a run is **active** (deferred by design; the manual
  `daemon restart` covers the impatient user).
- A persisted "pending upgrade" flag — unnecessary, since the id is re-checked on
  every launch.
- Auto-restart on the headless `run --jsonl` path (warn-only by design).
- Live/rolling daemon upgrade that migrates in-flight runs to a new process
  (out of scope; would require run-state handoff).

## Open decisions (confirm before planning)

1. **Decision B (idle-only auto-restart, warn-when-active) is the recommended
   active-run policy.** Confirm B over C (prompt the user). B is transparent and
   never disruptive; C reintroduces a manual step mid-work. Everything above is
   designed for B.
2. **`build_id` on `ServerHello` as the detection channel** (vs. a dedicated
   `DaemonStatusRequest` for the whole check). Recommended: on `ServerHello`, so
   the common matching path adds zero round-trips and the extra status query is
   paid only on a real mismatch. Confirm this placement.

*(Minor, non-blocking: the restart lock uses a `create_new` + PID-liveness lock
file to avoid a new dependency; an `flock`/`fd-lock` advisory lock is a drop-in
alternative if a dependency is acceptable.)*
