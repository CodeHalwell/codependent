# Adoption 09 — Unified Exec: Daemon-Owned PTY Sessions with Yield-Based Reads

**Effort:** L · **Depends on:** nothing (07 — arity permissions — composes well but is not required) · **Reference:** `reference-repos/codex/codex-rs/core/src/unified_exec/` + `codex-rs/utils/pty/`
**Ported from:** codex · **Status:** ⬜ not started

## 1. Summary

`shell.run` is one-shot: spawn, wait (bounded), capture, spill, return. That shape
cannot express a dev server that must stay up while the agent probes it, a REPL or
debugger session, an interactive installer, or a long build the model wants to poll.
This spec adds **unified exec**: a pair of tools that let the model

1. **open** an interactive process attached to a PTY — the call returns after a
   bounded *yield window*, carrying whatever output arrived plus a numeric
   `process_id` when the process is still running;
2. **write stdin / poll** an open process by `process_id`, again returning after a
   bounded yield window with the new output.

Every read is bounded on two axes: `yield_time_ms` (clamped to **250 ms – 30 s**)
and `max_output_tokens` (default **10 000**, hard cap **1 MiB** of retained bytes).
When the window expires with the process alive, the model gets the new output plus
a "still running" marker and the `process_id` to come back with. Retention uses a
**head+tail buffer** (first half + last half survive, middle is dropped with an
explicit `... N bytes omitted ...` marker). At most **64** concurrent processes
exist; beyond that the least-recently-used unprotected entry is pruned.

The decisive improvement over codex: codex's process table lives inside the CLI
process and dies with it. Codypendent's agent loop already runs inside the
persistent daemon (`codypendentd`), so the process table lives in
**`codypendent-daemon`** and interactive processes **survive client detach and
reattach** — close the TUI, reopen it tomorrow, and the dev server the agent
started is still there, still pollable by `process_id`.

Approval and policy are unchanged in kind: opening a process goes through the
**identical** `ProposedAction::ExecuteCommand` → policy → approval middleware as
`shell.run` (same allow-list, same `cwd` scope check, same environment deny-list).

## 2. Reference implementation

All paths relative to `reference-repos/codex/codex-rs/`.

### 2.1 Module layout (`core/src/unified_exec/`)

| File | Responsibility |
|---|---|
| `mod.rs` | Constants, request/response types, `ProcessStore` (map `i32 → ProcessEntry` + reserved-id set), `clamp_yield_time`, `resolve_max_tokens`, omission-marker format |
| `process.rs` | `UnifiedExecProcess`: PTY lifecycle + output plumbing. `OutputHandles { output_buffer, output_notify, output_closed, output_closed_notify, cancellation_token }`; a broadcast channel fans raw chunks out; a `watch<ProcessState>` carries exit/failure; an **interaction lock** (`Arc<Mutex<()>>`) serializes reads/writes per process; 150 ms early-exit grace at spawn classifies "short-lived command" vs "session" |
| `process_state.rs` | `ProcessState { has_exited, exit_code, failure_message, sandbox_denied }` with `exited()`/`failed()` transitions |
| `process_manager.rs` | `UnifiedExecProcessManager`: id allocation (random 1000..100 000; deterministic max+1 in tests), `exec_command` (open + first yield), `write_stdin` (write + yield), `collect_output_until_deadline` (the yield-read core), `refresh_process_state` (exited entries are removed from the store on observation), LRU pruning with the 8 most-recently-used protected, `terminate_all_processes`, `list_processes` |
| `head_tail_buffer.rs` | `HeadTailBuffer::new(max_bytes)` — head budget = max/2 filled first, tail budget = rest kept as a rolling `VecDeque`, `omitted_bytes` counted; `drain()`, `push_buffer()`, `to_bytes_with_omission_marker()` |
| `async_watcher.rs` | Background streaming of output deltas to session events (UTF-8-boundary chunks, ≤ 8 KiB per delta) and an exit watcher that emits the terminal tool event from the retained transcript |
| `errors.rs` | `UnifiedExecError`: `CreateProcess`, `ProcessFailed`, `UnknownProcessId`, `WriteToStdin`, `StdinClosed`, `MissingCommandLine`, `SandboxDenied` |

### 2.2 The normative constants (`mod.rs` lines 66–75)

```rust
pub(crate) const MIN_YIELD_TIME_MS: u64 = 250;
pub(crate) const WINDOWS_INITIAL_EXEC_YIELD_TIME_FLOOR_MS: u64 = 10_000;
pub(crate) const MIN_EMPTY_YIELD_TIME_MS: u64 = 5_000;      // empty write_stdin = poll
pub(crate) const MAX_YIELD_TIME_MS: u64 = 30_000;
pub(crate) const DEFAULT_MAX_BACKGROUND_TERMINAL_TIMEOUT_MS: u64 = 300_000;
pub(crate) const DEFAULT_MAX_OUTPUT_TOKENS: usize = 10_000;
pub(crate) const UNIFIED_EXEC_OUTPUT_MAX_BYTES: usize = 1024 * 1024;   // 1 MiB
pub(crate) const UNIFIED_EXEC_OUTPUT_MAX_TOKENS: usize = UNIFIED_EXEC_OUTPUT_MAX_BYTES / 4;
pub(crate) const MAX_UNIFIED_EXEC_PROCESSES: usize = 64;
```

Yield clamping (`clamp_yield_time`): Windows raises the floor to 10 s, then
`clamp(250, 30_000)`. `write_stdin` distinguishes: a non-empty write clamps to
`max(250) .. min(30_000)`; an **empty** write (a pure poll) clamps to
`5_000 .. max_write_stdin_yield_time_ms` (default 300 000) so background polling
can wait longer than interactive stdin.

### 2.3 The yield-read core (`process_manager.rs::collect_output_until_deadline`)

The loop that makes reads bounded and complete:

1. Drain the shared `HeadTailBuffer` under its lock; if non-empty, fold into the
   collected buffer (`push_buffer` preserves omission counts) and continue.
2. If empty: if the exit token has fired **and** the output task has closed, stop.
   Otherwise `select!` on {new-output notify, exit token, remaining-deadline
   sleep}. After exit fires, wait at most a further **50 ms**
   (`POST_EXIT_CLOSE_WAIT_CAP`) for the output task to publish its final chunks —
   producers publish all chunks *before* setting `output_closed` with `Release`
   ordering, so the final drain is safe.
3. On deadline expiry, return whatever was collected.

Spawn-side classification (`process.rs::from_spawned`): after wiring the output
task, poll the exit receiver; if the process exits within a **150 ms grace**
(`EARLY_EXIT_GRACE_PERIOD`), the call is treated as a short-lived command (no
`process_id` in the response); otherwise a background task records the eventual
exit into the `watch<ProcessState>` and cancels the token.

### 2.4 Environment (`process_manager.rs` lines 83–94)

Interactive processes get a deliberately dumb terminal so output stays parseable:

```rust
const UNIFIED_EXEC_ENV: [(&str, &str); 10] = [
    ("NO_COLOR", "1"), ("TERM", "dumb"), ("LANG", "C.UTF-8"), ("LC_CTYPE", "C.UTF-8"),
    ("LC_ALL", "C.UTF-8"), ("COLORTERM", ""), ("PAGER", "cat"), ("GIT_PAGER", "cat"),
    ("GH_PAGER", "cat"), ("CODEX_CI", "1"),
];
```

### 2.5 Pruning (`process_manager.rs::prune_processes_if_needed`)

At 64 entries: protect the 8 most-recently-used; among the rest, prefer the
least-recently-used **exited** entry, else the least-recently-used live one; never
prune an entry whose interaction lock is currently held (a `write_stdin` or
terminal-event publication is in flight against it).

### 2.6 PTY layer (`utils/pty/`)

Built on **`portable-pty = "0.9.0"`** (verified in `codex-rs/Cargo.toml:381`;
`utils/pty/Cargo.toml:12` consumes it via workspace). `spawn_pty_process` returns a
`SpawnedProcess { session: ProcessHandle, stdout_rx, stderr_rx, exit_rx }` where the
handle exposes `writer_sender()` (mpsc of stdin bytes), `signal()`, `terminate()`,
`has_exited()`, `exit_code()`. Under a PTY, stdout and stderr arrive merged on the
master; `combine_output_receivers` merges the broadcast receivers.

## 3. Current state in codypendent (verified)

### 3.1 One-shot shell (`crates/runtime/src/tools/shell.rs`)

- `CommandRequest { program: PathBuf, args: Vec<String>, cwd: PathBuf, environment: Vec<EnvironmentBinding>, timeout: Duration }`
  — structured, never an unparsed shell string.
- Gates, in order, before any spawn: `command_scope.allows_program(program)`
  (RULE 2a), `path_scope.classify(cwd)` (RULE 2b), `is_denied_env(name)` over every
  binding (RULE 2d — `PATH`, `LD_*`/`DYLD_*`, `GIT_CONFIG*`, `*_WRAPPER`,
  interpreter startup vars, …).
- Executes with `env_clear()` + explicit bindings only, `process_group(0)`,
  `kill_on_drop`, drains both pipes concurrently with a 16 MiB capture cap
  (`MAX_CAPTURE_BYTES`, `tools/mod.rs:365`), clamps the timeout
  (`effective_timeout`, absolute ceiling 1 h), kills the whole group via
  `/bin/kill -KILL -- -<pgid>` (no `libc`/`nix` — `unsafe` is denied workspace-wide),
  spills full output to the artifact store (`ArtifactSink`), and returns a
  `ShellOutcome` whose model-facing view is `salient::SalientView`
  (head 40 + tail 40 + capped error lines + artifact ref). A timeout is a
  *successful non-zero outcome*, not a `ToolError`.

### 3.2 Tool middleware (`crates/runtime/src/agent.rs`)

`run_tool` (line 3175): `prepare` maps `(tool, args)` → typed input +
`ProposedAction` (Shell at line 3403 builds `Shell::proposed_action(&request)`);
`self.policy.evaluate(action, ctx)` yields `Deny` (emit `ToolDenied` +
`ToolCompleted`, never execute), `RequireApproval` (park in `WaitingForApproval`
via `journal.request(ApprovalRequest{ allow_run_reuse, .. })` and
`approvals.await_decision`, raced against the cancellation token), or allow.
`execute_prepared` (line 3926) then runs the tool under
`write_scope` + `self.policy.command_scope()`. `repository.test` reuses exactly
this path by wrapping its detected command in the same `CommandRequest` shape.

### 3.3 Process/crate topology

- `codypendent-runtime` **depends on** `codypendent-daemon`
  (`crates/runtime/Cargo.toml:13`); the daemon can never depend on the runtime.
  The assembly binary `crates/codypendentd` implements the daemon's `RunExecutor`
  seam (`crates/daemon/src/executor.rs`) over the runtime loop and injects it via
  `server::run_with_executor`.
- The agent loop **already runs inside `codypendentd`**, so runs survive client
  detach today. What does *not* survive a run is its worktree:
  `crates/codypendentd/src/executor.rs` arms a `WorktreeReleaseGuard` around each
  isolated run and `guard.release()` deletes the worktree at run end (lines
  952–1311). Any long-lived process whose `cwd` is inside that worktree would
  outlive its own directory — §4.5 handles this.
- `FrameworkAgentRuntime::new(models, policy, approvals, subscriptions, journal, sink)`
  plus `with_github` / `with_mcp` / `with_search` builder-style collaborators
  (agent.rs line 1632) — the manager handle follows the same pattern.

### 3.4 Sandbox reality (important correction to the plan)

The adoption plan assumed one-shot shell "executes under Seatbelt". **It does
not.** `crates/sandbox` (Seatbelt SBPL generation + `sandbox-exec`, bubblewrap
argv generation, fail-closed `RefusingSandbox`) is the **plugin** security
boundary (Phase 6); `shell.run` runs a plain `tokio::process::Command` gated by
policy scopes, the env deny-list, and the emptied environment. "Same enforcement
as one-shot shell" for this spec therefore means: **the same
`CommandScope`/`PathScope`/env-deny/env-clear gates and the same approval
middleware** — not OS-level confinement. Confining PTY sessions under
`codypendent_sandbox::SandboxExecutor` is a possible follow-up (§10), not part of
this spec.

### 3.5 What does not exist yet

No PTY dependency anywhere in the workspace; no `tokio-util` (the codex code uses
`CancellationToken`); no interactive-process tool, table, or protocol surface; no
`CloseSession` command (nothing appends `EventBody::SessionClosed` today — it is
only read in `crates/daemon/src/commands.rs:2473` and `replay.rs:28`).

## 4. Design

### 4.1 Model vocabulary — two tools

**`shell.exec`** — open an interactive process (or run a short command and get its
output in one shot when it exits inside the first yield window).

```json
{
  "name": "shell.exec",
  "description": "Run a command attached to an interactive terminal. Returns after yield_time_ms with the output so far. If the process is still running the result carries a numeric process_id; interact with it via shell.write_stdin. Use for dev servers, REPLs, debuggers, watchers, and anything interactive; use shell.run for ordinary one-shot commands.",
  "parameters": {
    "type": "object",
    "properties": {
      "program":  { "type": "string" },
      "args":     { "type": "array", "items": { "type": "string" } },
      "cwd":      { "type": "string" },
      "environment": { "type": "object", "additionalProperties": { "type": "string" } },
      "yield_time_ms":     { "type": "integer", "description": "How long to collect output before returning. Clamped to 250..30000." },
      "max_output_tokens": { "type": "integer", "description": "Cap on returned output tokens. Default 10000." }
    },
    "required": ["program"]
  }
}
```

**`shell.write_stdin`** — write to (or poll, with empty `input`) an open process.

```json
{
  "name": "shell.write_stdin",
  "description": "Write input to a process opened by shell.exec and return the output that follows. An empty input polls without writing. Send \"\\u0003\" to interrupt. Include trailing \"\\n\" to submit a line.",
  "parameters": {
    "type": "object",
    "properties": {
      "process_id":        { "type": "integer" },
      "input":             { "type": "string" },
      "yield_time_ms":     { "type": "integer" },
      "max_output_tokens": { "type": "integer" }
    },
    "required": ["process_id"]
  }
}
```

Result body (rendered as the tool observation, mirroring the salient-view style):

```text
$ npm run dev            (process 41283, still running)
[wall time 3.0s; 1832 bytes output, 0 omitted]
<output …>
… 4096 bytes omitted …
<tail …>
process 41283 is still running — call shell.write_stdin {"process_id":41283} to
poll it or send input; it survives this run and this client.
```

or, for an exited process: `exit 0 (wall time 0.4s)` and no `process_id` line.

RULES:

1. `yield_time_ms` clamps to `250..=30_000` on `shell.exec` and non-empty
   `shell.write_stdin`; an **empty** `write_stdin` clamps to
   `5_000..=300_000` (poll semantics). Constants copied from §2.2 with the same
   names; keep `WINDOWS_INITIAL_EXEC_YIELD_TIME_FLOOR_MS = 10_000` behind
   `cfg!(windows)` even though Windows is not a supported target yet — it
   documents the reason the clamp exists.
2. `max_output_tokens` defaults to `10_000`; the retained transcript and every
   per-read collection buffer are hard-capped at `1 MiB`
   (`UNIFIED_EXEC_OUTPUT_MAX_BYTES`); tokens are estimated at 4 bytes/token
   (consistent with `CHARS_PER_TOKEN` in `agent.rs`).
3. A process that exits within the **150 ms** early-exit grace, or before its
   first yield window closes, returns its exit code and **no** `process_id` —
   nothing is stored.
4. `process_id` is a random `i32` in `1_000..100_000`, unique against a
   reserved-id set; deterministic (`max(existing, 999) + 1`) under
   `cfg(test)`/an explicit test switch, exactly as codex does.
5. At most **64** stored processes; storing the 65th prunes per §2.5 (protect the
   8 most-recently-used; prefer exited; never prune while the interaction lock is
   held).
6. Reads and writes against one process serialize on its interaction lock;
   different processes are fully concurrent.
7. On a non-TTY stream error, `input == "\u{3}"` maps to an interrupt signal
   (mirror `INTERRUPT` handling); any other write to a closed stdin is the
   `StdinClosed` error with the corrective message from §2.7 of the reference.

### 4.2 Ownership — the table lives in `codypendent-daemon`

New module **`crates/daemon/src/unified_exec/`** holding the manager, process,
and buffer types. This is legal (`runtime → daemon` already exists) and puts the
table in the process that never restarts between client attaches:

```rust
// crates/daemon/src/unified_exec/manager.rs
pub struct UnifiedExecManager {
    store: tokio::sync::Mutex<ProcessStore>,
    max_poll_yield_time_ms: u64,          // default 300_000
}

struct ProcessStore {
    processes: HashMap<i32, ProcessEntry>,
    reserved_ids: HashSet<i32>,
}

struct ProcessEntry {
    process: Arc<UnifiedExecProcess>,
    session_id: SessionId,     // owner: only this session's runs may touch it
    run_id: RunId,             // origin, for audit
    command: String,           // display form, for listings/audit
    cwd: PathBuf,              // lifecycle: worktree-scoped termination (§4.5)
    transcript: Arc<tokio::sync::Mutex<HeadTailBuffer>>,   // full-session retention
    last_used: tokio::time::Instant,
}
```

Public surface (all `async` where they lock the store):

```rust
impl UnifiedExecManager {
    pub fn new() -> Self;
    pub async fn allocate_process_id(&self) -> i32;
    pub async fn release_process_id(&self, process_id: i32);
    /// Spawn `spec` on a PTY and run the first yield window. The caller has
    /// ALREADY passed policy + approval; this function never re-evaluates policy.
    pub async fn exec(&self, spec: OpenProcessSpec, read: ReadBudget)
        -> Result<ExecOutput, UnifiedExecError>;
    pub async fn write_stdin(&self, session: SessionId, process_id: i32,
        input: &str, read: ReadBudget) -> Result<ExecOutput, UnifiedExecError>;
    pub async fn terminate_process(&self, session: SessionId, process_id: i32) -> bool;
    /// Kill every process whose cwd is under `root` (worktree release hook).
    pub async fn terminate_under(&self, root: &Path);
    /// Kill every process owned by `session` (future session-close hook).
    pub async fn terminate_session(&self, session: SessionId);
    pub async fn terminate_all(&self);
    pub async fn list(&self, session: SessionId) -> Vec<ProcessInfo>;
}

pub struct OpenProcessSpec {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub program: PathBuf,          // already resolved + allow-listed by the tool
    pub args: Vec<String>,
    pub cwd: PathBuf,              // already scope-checked by the tool
    pub environment: Vec<(String, String)>,  // already deny-list-filtered
}

pub struct ReadBudget { pub yield_time_ms: u64, pub max_output_tokens: usize }

pub struct ExecOutput {
    pub process_id: Option<i32>,   // None ⇒ exited; Some ⇒ still running
    pub exit_code: Option<i32>,
    pub wall_time: Duration,
    pub output: String,            // UTF-8-lossy, omission marker inserted
    pub original_token_count: usize,
    pub omitted_bytes: usize,
}
```

**Session ownership check**: `write_stdin`/`terminate_process`/`list` take the
calling run's `SessionId` and return `UnknownProcessId` for a `process_id` owned
by a different session. A run can never drive another session's terminal.

### 4.3 Process internals — port of `process.rs`, local transport only

`UnifiedExecProcess` keeps codex's shape minus the exec-server transport arm:

```rust
// crates/daemon/src/unified_exec/process.rs
pub(crate) struct UnifiedExecProcess {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    writer_tx: tokio::sync::mpsc::Sender<Vec<u8>>,     // stdin bytes
    output: OutputHandles,
    state_tx: tokio::sync::watch::Sender<ProcessState>,
    state_rx: tokio::sync::watch::Receiver<ProcessState>,
    interaction_lock: Arc<tokio::sync::Mutex<()>>,
    reader_task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Clone)]
pub(crate) struct OutputHandles {
    pub output_buffer: Arc<tokio::sync::Mutex<HeadTailBuffer>>,
    pub output_notify: Arc<tokio::sync::Notify>,
    pub output_closed: Arc<std::sync::atomic::AtomicBool>,
    pub output_closed_notify: Arc<tokio::sync::Notify>,
    pub exit_token: tokio_util::sync::CancellationToken,
}
```

Spawn path: `portable_pty::native_pty_system().openpty(PtySize{rows:24, cols:80, ..})`,
`pair.slave.spawn_command(CommandBuilder)` with the merged environment
(§4.4), then:

- a **blocking reader thread** (`std::thread` or `spawn_blocking`) reads the
  master (`pair.master.try_clone_reader()`), pushes chunks into
  `output_buffer` under the async lock (via a small `blocking_lock` shim or an
  mpsc bridge into an async pump task), notifies `output_notify`, and on EOF sets
  `output_closed` (with `Release`) and notifies `output_closed_notify` — the
  publish-before-close ordering §2.3 depends on;
- a **writer task** owns `pair.master.take_writer()`, draining `writer_tx`;
- an **exit watcher** blocks on `child.wait()` (`spawn_blocking`), then
  `state_tx.send_replace(state.exited(code))` and cancels `exit_token`.

`collect_output_until_deadline` is a direct port of §2.3 (drain → fold →
select on notify/exit/deadline → 50 ms post-exit close wait), minus codex's
elicitation-pause extension (codypendent parks approvals *before* execution, so
no read is ever in flight while an approval is pending for the same call).

`HeadTailBuffer` is ported verbatim from `head_tail_buffer.rs` (194 lines, pure,
with its test file) — head budget filled first, rolling tail, `omitted_bytes`,
`drain()`, `push_buffer()`, `to_bytes_with_omission_marker()`.

`Drop for UnifiedExecProcess` calls `killer.kill()` best-effort and additionally
issues the `/bin/kill -KILL -- -<pid>` group kill used by `shell.rs`
(`fixed_kill_binary()` — hoist that helper somewhere shareable or duplicate the
five lines; `unsafe`/`libc` remain unavailable), because a PTY child may have
spawned descendants that survive the leader.

### 4.4 Tool front-end (`crates/runtime/src/tools/unified_exec.rs`)

The tool performs **exactly the same pre-spawn gates as `shell.run`**, then hands
off to the manager:

```rust
pub struct ShellExec;
impl ShellExec {
    pub const NAME: &'static str = "shell.exec";
    pub fn proposed_action(request: &CommandRequest) -> ProposedAction {
        Shell::proposed_action(request)   // the SAME ExecuteCommand shape
    }
    pub async fn execute(
        request: &CommandRequest,
        read: ReadBudget,
        path_scope: &PathScope,
        command_scope: &CommandScope,
        manager: &UnifiedExecManager,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<ExecOutput, ToolError>;
}

pub struct ShellWriteStdin;
impl ShellWriteStdin {
    pub const NAME: &'static str = "shell.write_stdin";
    // No ProposedAction: writes ride the approval granted at open (§4.6).
    pub async fn execute(process_id: i32, input: &str, read: ReadBudget,
        manager: &UnifiedExecManager, session_id: SessionId)
        -> Result<ExecOutput, ToolError>;
}
```

`ShellExec::execute` RULES (each mirrors the `shell.rs` rule it copies):

1. Program must satisfy `command_scope.allows_program` — refuse with
   `ToolError::ProgramNotAllowed` before any spawn.
2. `cwd` must classify `Allowed` under `path_scope` (same tri-state handling as
   `shell.rs::execute`).
3. Every environment binding runs through the **same** `is_denied_env` (make
   `is_denied_env` `pub(crate)` in `shell.rs` rather than copying the list —
   one list, two callers).
4. Resolve the program on the daemon PATH with `resolve_program` (reuse from
   `shell.rs`, made `pub(crate)`), because the child runs with a cleared env.
5. Child environment = the ten `UNIFIED_EXEC_ENV` pairs (§2.4, with
   `CODEX_CI` renamed `CODYPENDENT_CI`) overlaid by the request's explicit
   bindings; nothing inherited.
6. Output is returned raw (with the omission marker), truncated to
   `max_output_tokens` by a middle-cut with an explicit marker — **not** run
   through `SalientView` (yield reads are already small and shaped by the model's
   own budget), and **not** spilled to the artifact store per read. The
   full-session `transcript` `HeadTailBuffer` is spilled once, at process exit,
   through the existing `ArtifactSink` with
   `Provenance::tool_output("shell.exec", run_id)` so the audit trail matches
   `shell.run`'s.

### 4.5 Lifecycle rules

| Trigger | Effect on stored processes |
|---|---|
| Client detaches / TUI closes | **None.** The table lives in `codypendentd`. |
| Run completes / fails / is cancelled | **None**, *except* worktree-bound processes (next row). A later run in the same session may use the same `process_id`s. |
| Isolated run's worktree released | `WorktreeReleaseGuard` (codypendentd `executor.rs`) calls `manager.terminate_under(&worktree_path)` **before** deletion — a process must not outlive its own `cwd`. Processes rooted in the repository itself survive. |
| Session closed | `manager.terminate_session(id)` — wired defensively even though nothing appends `SessionClosed` today (verified §3.5); the hook goes next to wherever session close lands. |
| 65th process stored | LRU prune per §2.5. |
| Daemon shutdown | `manager.terminate_all()` from the server's shutdown path. |
| Daemon crash/restart | Processes die with the daemon (they are its children). A model holding a stale `process_id` gets `UnknownProcessId` with the message "no such process — it may have ended or the daemon restarted; re-open it with shell.exec". |

### 4.6 Approval semantics

- `shell.exec` is gated by the **identical** middleware as `shell.run`: `prepare`
  builds `Shell::proposed_action(&request)` (an `ExecuteCommand` carrying
  program, args, full environment, cwd), `policy.evaluate` runs under the mode
  overlay (so a read-only/plan run is never offered the tool — extend the
  `command_allowed` filter in `offered_tool_names`), a `RequireApproval` parks
  the run in `WaitingForApproval`, and `allow_run_reuse` approval caching works
  unchanged.
- `shell.write_stdin` is **not** separately approved: the operator approved
  *running this program interactively*; stdin is that program's normal operation.
  It is fully audited — the model's `ToolCall { tool: "shell.write_stdin", args }`
  turn and the `ToolStarted`/`ToolCompleted` events already record every byte of
  `input` in the ledger (the args carry it verbatim). The session-ownership check
  (§4.2) bounds the blast radius to processes the same session already approved.
- Deliberate consequence to document in the tool description: approving
  `shell.exec {"program": "python3"}` approves an interpreter — the same
  judgment call `shell.run {"program": "python3", "args": ["-c", …]}` already
  presents. The allow-list, not the approval prompt, is the control for "no
  interpreters ever".

## 5. Changes, file by file

### 5.1 `Cargo.toml` (workspace root)

```toml
[workspace.dependencies]
portable-pty = "0.9.0"
tokio-util = { version = "0.7", default-features = false }
```

### 5.2 `crates/daemon/Cargo.toml`

```toml
portable-pty = { workspace = true }
tokio-util = { workspace = true }
rand = { workspace = true }          # only if not already present
```

### 5.3 `crates/daemon/src/unified_exec/` (new)

- `mod.rs` — constants (§2.2 names verbatim), `OpenProcessSpec`, `ReadBudget`,
  `ExecOutput`, `ProcessInfo`, `clamp_yield_time`, `resolve_max_tokens`,
  `format_output_omission_marker`, `UnifiedExecError` (§2.1 variants minus
  `SandboxDenied`/`ForeignPath`, plus `NotOwner` folded into `UnknownProcessId`
  so cross-session probing is indistinguishable from a dead id).
- `head_tail_buffer.rs` (+ `head_tail_buffer_tests.rs`) — verbatim port.
- `process.rs` — §4.3.
- `manager.rs` — §4.2, including `collect_output_until_deadline`,
  `refresh_process_state` (remove-on-observed-exit), pruning, and the lifecycle
  terminators.
- `crates/daemon/src/lib.rs` — `pub mod unified_exec;`.

### 5.4 `crates/runtime/src/tools/unified_exec.rs` (new) + `tools/mod.rs`

§4.4; `mod.rs` gains `mod unified_exec;` and re-exports
`ShellExec, ShellWriteStdin`. In `shell.rs`, change `fn is_denied_env`,
`fn resolve_program` to `pub(crate)`.

### 5.5 `crates/runtime/src/agent.rs`

- `FrameworkAgentRuntime` gains
  `unified_exec: Option<Arc<codypendent_daemon::unified_exec::UnifiedExecManager>>`
  (default `None` in `new`) and `pub fn with_unified_exec(mut self, m: Arc<…>) -> Self`.
- `offered_tool_names`: offer `ShellExec::NAME` and `ShellWriteStdin::NAME` only
  when `self.unified_exec.is_some()`, filtered by the same `command_allowed`
  overlay branch that removes `shell.run`.
- Tool declarations (`decl` table near line 6407): the two JSON schemas of §4.1.
- `prepare`: `ShellExec::NAME` parses a `CommandRequest` + `ReadBudget` and
  returns `Prepared { action: Shell::proposed_action(..), tool: PreparedTool::ShellExec{..} }`;
  `ShellWriteStdin::NAME` parses `{process_id, input, yield_time_ms, max_output_tokens}`
  and returns a **policy-exempt** prepared tool — model it the way read tools are
  modeled (an action that evaluates to Allow), or add a
  `ProposedAction::InteractWithProcess { process_id }` if the policy engine
  requires every tool to carry an action; either way the evaluation result must
  be Allow-without-prompt while remaining visible in the ledger.
- `execute_prepared`: two new arms calling the tool fronts with
  `self.unified_exec` (refuse with a legible "unified exec is not wired" tool
  error if `None` — mirrors the `github`/`artifact` gating style), rendering
  `ExecOutput` into the observation format of §4.1.

### 5.6 `crates/codypendentd/src/executor.rs`

- `RuntimeExecutor` constructs one
  `Arc<UnifiedExecManager>` in `new` (alongside the hub/broker) and passes
  `.with_unified_exec(manager.clone())` where each run's
  `FrameworkAgentRuntime` is assembled (~line 853).
- `WorktreeReleaseGuard::release` (or the call sites at lines 1059/1164/1260/1311)
  calls `manager.terminate_under(&worktree_path).await` before the worktree is
  deleted.
- The daemon shutdown path calls `manager.terminate_all().await`.

### 5.7 Docs

`docs/docs/adoptions/00-index.md` row for 09 is already present; add the module
to the daemon chapter map if one exists for Phase 1 tooling.

## 6. Protocol & persistence

**No wire-protocol change.** The tools ride the existing tool-call surface:
`ToolStarted` / `ToolCompleted` events carry the observation; the model's
`TurnItem::ToolCall` records `shell.write_stdin` inputs verbatim in the ledger,
which is the audit record for stdin interaction. `PROTOCOL_V1` stays at 1.4.

**No new persistence.** Processes are in-memory daemon state; they are
deliberately *not* resurrected after a daemon restart (§4.5 last row). The one
durable artifact is the end-of-process transcript spill (§4.4 rule 6), stored
through the existing content-addressed `ArtifactStore` with tool provenance.

## 7. Acceptance criteria

1. `shell.exec {"program":"cat"}` returns within its yield window with a
   `process_id`; `shell.write_stdin {"process_id":N,"input":"hello\n"}` returns
   output containing `hello`; a second TUI attach to the same session can issue
   another `write_stdin` against the same id (client-restart survival).
2. `shell.exec` on a program that exits immediately (`/bin/echo hi`) returns
   `exit 0`, output `hi`, and **no** `process_id`; the store remains empty.
3. `yield_time_ms: 1` clamps to 250 ms; `yield_time_ms: 600000` clamps to 30 s;
   an empty-input `write_stdin` with `yield_time_ms: 1` waits at least 5 s.
4. Output beyond 1 MiB retains head+tail with a `... N bytes omitted ...` marker
   whose `N` equals the true dropped byte count; `original_token_count` and
   `omitted_bytes` are populated.
5. A 65th open prunes the LRU unprotected entry; the 8 most-recently-used ids
   all remain valid.
6. A program not on the allow-list, a `cwd` outside the path scope, and an
   `LD_PRELOAD` binding are each refused **without a process being spawned**,
   with the same `ToolError` codes `shell.run` uses.
7. Opening under an approval-required policy parks the run in
   `WaitingForApproval`; rejection produces the standard rejected observation;
   an approved open followed by `write_stdin` calls prompts **no further**
   approvals; every `write_stdin` appears in the ledger with its full `input`.
8. A `write_stdin` with a `process_id` owned by another session returns
   `UnknownProcessId`.
9. Cancelling a run mid-`shell.exec` leaves the process either fully terminated
   (if the open had not completed) or alive-and-owned (if it had) — never a
   zombie unreachable by id; releasing an isolated run's worktree terminates
   every process rooted in it before deletion.
10. Standard gate: `cargo fmt --all -- --check`,
    `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
    `cargo test --workspace` all green; no `unsafe`, no `libc`/`nix`.

## 8. Tests

Port the shape (not the transport-specific halves) of codex's four test files:

- `head_tail_buffer` unit tests: verbatim port (budget split, omission counting,
  single-chunk-larger-than-tail, `drain`/`push_buffer` round-trip).
- `manager` tests (deterministic ids on):
  - open `cat` → id assigned → write → echoed output → terminate → id unknown;
  - open `sh -c 'sleep 5'` → still-running marker; poll returns empty output and
    still-running again; interrupt via `"\u{3}"` after `tty` echo settles;
  - early-exit classification (`/bin/echo`), including the 150 ms grace;
  - clamp table (unit, pure): every row of acceptance criterion 3;
  - prune-at-64 with protected-recent-8 (use tiny `sleep 60` children or a fake
    entry constructor);
  - session-ownership refusal.
- `agent.rs` middleware tests (extend the existing `ScriptedDriver` suites):
  scripted `CallTool{"shell.exec",…}` under deny-policy → `ToolDenied`, no spawn
  (assert via a nonexistent-program sentinel that would error differently);
  under approval-policy → parked, approve → executes; `shell.write_stdin`
  recorded as a `ToolCall` turn in the transcript.
- codypendentd integration test: run start → open long-lived process in the
  worktree → run completes → worktree released → process gone (probe by id).

Unix-only (`#[cfg(unix)]`) for anything spawning a PTY, mirroring codex's
`#[cfg(unix)]` on `process_tests.rs`/`mod_tests.rs`.

## 9. Gotchas

- **PTY merges stdout/stderr** and echoes stdin back into the output stream; the
  observation will contain the model's own typed input. Codex accepts this; do
  the same and say so in the tool description ("your input is echoed").
- **`\r\n` line endings** from the PTY line discipline; do not "fix" them —
  models handle them, and rewriting bytes breaks byte-count honesty in the
  omission marker.
- **Blocking PTY reader**: `portable-pty` readers are blocking; never read on
  the tokio runtime threads. The reader thread must exit on EOF *and* on kill —
  reading a closed master returns `Ok(0)`/`Err`, both of which must close the
  output (set-flag-then-notify, `Release` before notify, or
  `collect_output_until_deadline`'s final drain races).
- **Do not hold the store lock across `.await` on process operations** — codex
  clones the `Arc` out of the store, drops the guard, then locks the interaction
  lock (see `write_stdin`, lines 758–766). Copy that discipline; clippy's
  `await_holding_lock` won't catch `tokio::sync::Mutex`.
- **Pruning vs in-flight interaction**: only prune entries whose interaction
  lock `try_lock` succeeds (§2.5), or a live `write_stdin` observes its process
  vanish mid-call.
- **`kill_on_drop` only kills the direct child**; PTY children that spawned
  process groups need the `/bin/kill -- -pgid` fallback exactly as `shell.rs`
  learned (module doc, lines 369–376).
- **The 150 ms early-exit grace vs slow interpreter startup**: `python3` takes
  longer than 150 ms to start but must NOT be classified short-lived — the grace
  classifies only processes that *exited*, not processes that are quiet. Port
  the logic exactly; do not "optimize" the grace into a stillness check.
- **Timeout is not an error** for one-shot shell; for unified exec, *yield
  expiry* is not an error either — never surface "still running" as a
  `ToolError`, or the model will retry-open instead of polling.
- **Store removal on observed exit** (`refresh_process_state`) means a poll of a
  just-exited process returns its exit code once, and the id is unknown after.
  Tests must not poll twice expecting the exit code twice.
- **Env deny-list still matters under a PTY** — a REPL approved once can be
  steered by `PYTHONSTARTUP` exactly like a one-shot; rule 3 of §4.4 is
  load-bearing, not ceremony.

## 10. Out of scope

- TUI surfacing of running processes (codex's `unified_exec_footer.rs`), a
  `list` tool, and a kill palette command — follow-up once the tool proves out.
- Streaming per-chunk output deltas to clients mid-yield (codex's
  `async_watcher.rs` delta events). V1 returns output only in tool results; the
  ledger still gets everything at completion.
- Windows/ConPTY support (workspace targets Linux/macOS; the constant remains).
- OS-level confinement of PTY sessions under `codypendent_sandbox` (Seatbelt /
  bubblewrap) — see §3.4; would compose here later as a spawn-wrapper.
- Codex's exec-server remote transport, network-proxy deferred approvals, and
  plugin metrics sidecars — no codypendent counterpart exists.
- Resurrecting processes across daemon restarts.
