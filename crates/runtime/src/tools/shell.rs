//! `shell.run` — structured, sandboxed command execution.
//!
//! Enforces the Chapter 11 / STEP 1.7 rules: a structured request (never an
//! unparsed shell string), an allow-listed program, a `cwd` inside the granted
//! path scope, an environment that starts *empty* plus only explicit bindings
//! (no inherited secrets), a timeout that kills the process group, an output cap
//! that spills to the artifact store, and an observation-compacted salient view
//! as the only thing the model reads.

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use codypendent_daemon::artifacts::Provenance;
use codypendent_daemon::policy::{CommandScope, PathScope, ScopeVerdict};
use codypendent_protocol::{ArtifactRef, ProposedAction, RunId};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

use super::{
    display_command, effective_timeout, salient, ArtifactSink, CapabilityKind, ToolError,
    MAX_CAPTURE_BYTES,
};

/// One name/value pair permitted into the child environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentBinding {
    /// The variable name.
    pub name: String,
    /// The variable value.
    pub value: String,
}

impl EnvironmentBinding {
    /// Convenience constructor.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// A structured command request (Chapter 11). There is deliberately no field for
/// an unparsed shell string.
#[derive(Debug, Clone)]
pub struct CommandRequest {
    /// The program to run (bare name resolved on the daemon PATH, or a path).
    pub program: PathBuf,
    /// Arguments, passed verbatim — never re-parsed by a shell.
    pub args: Vec<String>,
    /// Working directory; must be inside the granted path scope.
    pub cwd: PathBuf,
    /// The *complete* child environment — the process inherits nothing else.
    pub environment: Vec<EnvironmentBinding>,
    /// Wall-clock timeout; clamped down to the command scope's ceiling.
    pub timeout: Duration,
}

/// The result of running a command. `exit_code` is `None` when the process was
/// killed (timeout or signal); `salient` is the observation-compacted view and
/// the `*_ref` fields point at the full spilled output.
#[derive(Debug, Clone)]
pub struct ShellOutcome {
    /// Process exit code, or `None` if killed.
    pub exit_code: Option<i32>,
    /// Whether the command was killed for exceeding its timeout.
    pub timed_out: bool,
    /// Wall-clock duration.
    pub duration: Duration,
    /// Full standard output, if it was spilled to the store.
    pub stdout_ref: Option<ArtifactRef>,
    /// Full standard error, if it was spilled to the store.
    pub stderr_ref: Option<ArtifactRef>,
    /// The model-facing compacted view.
    pub salient: salient::SalientView,
}

impl ShellOutcome {
    /// Whether the command completed with a zero exit code.
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// The `shell.run` tool.
pub struct Shell;

impl Shell {
    /// The stable tool name.
    pub const NAME: &'static str = "shell.run";

    /// Capability classes this tool draws on.
    pub fn required_capabilities() -> &'static [CapabilityKind] {
        &[CapabilityKind::CommandExecute]
    }

    /// The [`ProposedAction`] the middleware evaluates before granting. The full
    /// child environment and `cwd` are carried on the action so the approver and
    /// the audit ledger see exactly what the command will run with — not just its
    /// program and args.
    pub fn proposed_action(request: &CommandRequest) -> ProposedAction {
        ProposedAction::ExecuteCommand {
            program: request.program.to_string_lossy().into_owned(),
            args: request.args.clone(),
            environment: request
                .environment
                .iter()
                .map(|b| (b.name.clone(), b.value.clone()))
                .collect(),
            cwd: Some(request.cwd.to_string_lossy().into_owned()),
        }
    }

    /// Run `request` under the granted scopes, spilling full output to `sink`.
    ///
    /// Refuses — with no process spawned — a program not in `command_scope` or a
    /// `cwd` outside `path_scope`. A timeout is a *successful* non-zero outcome,
    /// not an error.
    pub async fn execute(
        request: &CommandRequest,
        path_scope: &PathScope,
        command_scope: &CommandScope,
        sink: &dyn ArtifactSink,
        run_id: RunId,
    ) -> Result<ShellOutcome, ToolError> {
        // RULE 2a: the program must be allow-listed. Check before anything is
        // spawned so a rejected program leaves no trace.
        let program_str = request.program.to_string_lossy().into_owned();
        if !command_scope.allows_program(&program_str) {
            return Err(ToolError::ProgramNotAllowed(program_str));
        }

        // RULE 2b: the working directory must be inside the granted scope.
        match path_scope.classify(&request.cwd) {
            ScopeVerdict::Allowed => {}
            ScopeVerdict::Denied => return Err(ToolError::PathDenied(request.cwd.clone())),
            ScopeVerdict::OutsideRoots => {
                return Err(ToolError::CwdOutOfScope(request.cwd.clone()))
            }
        }

        // RULE 2d: refuse execution-hijacking environment bindings. The whole
        // environment is also surfaced on the approval card (see `proposed_action`)
        // so the approver sees every binding, but these specific names are denied
        // outright — a re-used or auto-granted approval must not be able to smuggle
        // an `LD_PRELOAD`/`RUSTC_WRAPPER`/shadowed-`PATH` past the gate.
        if let Some(binding) = request.environment.iter().find(|b| is_denied_env(&b.name)) {
            return Err(ToolError::EnvironmentNotAllowed(binding.name.clone()));
        }

        // Resolve the program to an absolute path using the daemon's PATH *now*,
        // so the child can run with an emptied environment and still be found.
        let resolved = resolve_program(&request.program, &request.cwd)
            .await
            .ok_or_else(|| ToolError::ProgramNotFound(program_str.clone()))?;

        let mut command = Command::new(&resolved);
        command
            .args(&request.args)
            .current_dir(&request.cwd)
            // RULE 2c: empty environment plus only the explicit bindings.
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for binding in &request.environment {
            command.env(&binding.name, &binding.value);
        }
        // RULE 3: isolate the child (and its descendants) in its own process
        // group so a timeout can terminate the whole group.
        #[cfg(unix)]
        command.process_group(0);

        let started = Instant::now();
        let mut child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ToolError::ProgramNotFound(program_str.clone())
            } else {
                ToolError::Io(e)
            }
        })?;
        let pid = child.id();
        let mut group_guard = ProcessGroupGuard::new(pid);

        // Drain both pipes concurrently so a chatty child never blocks, capping
        // what is held in memory.
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let out_task = tokio::spawn(async move { drain(stdout, MAX_CAPTURE_BYTES).await });
        let err_task = tokio::spawn(async move { drain(stderr, MAX_CAPTURE_BYTES).await });

        // RULE 3: enforce the (clamped) timeout, killing the group on expiry.
        // `wait_and_sweep` also ends the group on the *normal* exit path — a
        // parent that exited cleanly may still leave descendants holding stdout
        // or stderr open — and it does so before the leader is reaped, which is
        // the only point at which the pgid is provably still ours.
        let timeout = effective_timeout(request.timeout, command_scope.maximum_seconds);
        let (exit_code, timed_out) =
            match tokio::time::timeout(timeout, wait_and_sweep(&mut child, pid)).await {
                Ok(Ok(status)) => (status.code(), false),
                Ok(Err(e)) => return Err(ToolError::Io(e)),
                Err(_elapsed) => {
                    kill_group(pid, &mut child).await;
                    (None, true)
                }
            };
        // No `await` between the reap above and this disarm: the guard can never
        // fire on a pid the process no longer owns.
        group_guard.disarm();
        let duration = started.elapsed();

        // After a group kill the pipes normally reach EOF at once — but a
        // grandchild that escaped the kill (e.g. `kill` binary missing, or a
        // re-parented process) can hold the write end open indefinitely. Bound
        // the drain so `execute` always returns; on that (double-failure) edge
        // the partial capture is abandoned — the stream comes back empty and
        // flagged overflowed, never a silent hang.
        let (stdout_bytes, stdout_overflow) = join_drain_bounded(out_task).await?;
        let (stderr_bytes, stderr_overflow) = join_drain_bounded(err_task).await?;

        // Spill full output and build the salient view referencing it.
        let stdout_ref = spill(sink, "text/plain", run_id, &stdout_bytes).await?;
        let stderr_ref = spill(sink, "text/plain", run_id, &stderr_bytes).await?;

        let view = salient::SalientView {
            command: display_command(&request.program, &request.args),
            exit_code,
            timed_out,
            duration_ms: duration.as_millis(),
            stdout: salient::compute_stream(&stdout_bytes, stdout_overflow, stdout_ref.clone()),
            stderr: salient::compute_stream(&stderr_bytes, stderr_overflow, stderr_ref.clone()),
        };

        Ok(ShellOutcome {
            exit_code,
            timed_out,
            duration,
            stdout_ref,
            stderr_ref,
            salient: view,
        })
    }
}

/// Whether a model-supplied environment variable name can hijack what the
/// command actually executes: shared-library interposers (`LD_*`/`DYLD_*`),
/// compiler/tool wrappers (`*_WRAPPER`), git's external-program hooks and
/// injected config (`GIT_CONFIG*`), interpreter startup/option/load-path
/// injection (`NODE_OPTIONS`, `PYTHON*`, `PERL5*`, `RUBYOPT`/`RUBYLIB`),
/// delegated-helper variables that name a program the tool then EXECS
/// (`*PAGER`, `*EDITOR`, `LESSOPEN`, `MAKE*`, `CARGO`), shell startup files and
/// `CDPATH`, and `PATH` (which redirects the child's own subprocess lookups even
/// though the top-level program is resolved on the daemon's PATH).
///
/// STRUCTURALLY THIS IS THE WRONG SHAPE and the gaps it keeps growing are the
/// evidence: a deny-list is open by default, so every variable nobody has
/// thought of yet is permitted, and each allow-listed tool brings its own set
/// (git alone honours `GIT_PAGER`/`PAGER`/`GIT_EDITOR`/`GIT_SEQUENCE_EDITOR`,
/// `less` honours `LESSOPEN`, `make` honours `MAKEFILES`, python honours
/// `PYTHONBREAKPOINT`, cargo honours `CARGO`). The fail-closed shape is an
/// allow-list keyed to the allow-listed program — every binding refused unless
/// `CommandScope` names it. That is a `CommandScope`/`EnvironmentBinding`
/// contract change reaching the approval card and every caller, so it is not
/// done here; the families below are widened to prefix/suffix rules rather than
/// single names so that at least the next sibling in each family is closed.
pub(crate) fn is_denied_env(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "PATH"
            | "GIT_SSH_COMMAND"
            | "GIT_SSH"
            | "GIT_EXTERNAL_DIFF"
            | "GIT_PROXY_COMMAND"
            | "GIT_DIR"
            | "GIT_WORK_TREE"
            | "GIT_EXEC_PATH"
            | "GIT_OBJECT_DIRECTORY"
            | "GIT_ALTERNATE_OBJECT_DIRECTORIES"
            | "BASH_ENV"
            | "ENV"
            | "SHELLOPTS"
            | "CDPATH"
            | "PROMPT_COMMAND"
            | "IFS"
            | "ZDOTDIR"
            | "GLIBC_TUNABLES"
            | "NODE_OPTIONS"
            | "JAVA_TOOL_OPTIONS"
            | "JDK_JAVA_OPTIONS"
            | "_JAVA_OPTIONS"
            | "PYTHONPATH"
            | "PYTHONSTARTUP"
            | "PYTHONHOME"
            | "PYTHONBREAKPOINT"
            | "PERL5OPT"
            | "PERL5LIB"
            | "RUBYOPT"
            | "RUBYLIB"
            | "LUA_PATH"
            | "LUA_CPATH"
            | "RUSTC"
            | "RUSTDOC"
            // `cargo` re-execs `$CARGO` for build scripts, `cargo <subcommand>`
            // and `cargo test` harnesses, so it is `RUSTC` for the driver.
            | "CARGO"
            | "CARGO_HOME"
            | "RUSTUP_HOME"
            | "HOME"
            // `less`/`man` run `$LESSOPEN`'s value as an input preprocessor
            // command on every file they open.
            | "LESSOPEN"
            | "LESSCLOSE"
            // `make` reads these makefiles before the named target, so their
            // recipes run first.
            | "MAKEFILES"
            | "MAKEFLAGS"
    ) || upper.starts_with("LD_")
        || upper.starts_with("DYLD_")
        || upper.starts_with("GIT_CONFIG")
        || upper.ends_with("_WRAPPER")
        // `PAGER`, `GIT_PAGER`, `MANPAGER`, `SYSTEMD_PAGER`, … — git, man, less
        // and friends spawn the value through `sh -c`, so any of them is a
        // shell command that runs on a benign-looking `git log`.
        || upper.ends_with("PAGER")
        // `EDITOR`, `VISUAL`, `GIT_EDITOR`, `GIT_SEQUENCE_EDITOR`, … — likewise
        // exec'd by git (rebase/commit/tag), `crontab`, `visudo`.
        || upper.ends_with("EDITOR")
        || upper == "VISUAL"
        // `CARGO_TARGET_<TRIPLE>_RUNNER` / `_LINKER` name a program cargo execs
        // for `cargo run`/`cargo test`; the triple is in the middle of the name.
        || (upper.starts_with("CARGO_TARGET_")
            && (upper.ends_with("_RUNNER") || upper.ends_with("_LINKER")))
        || upper.starts_with("CARGO_BUILD_")
}

/// Spill a non-empty stream to the artifact store, returning its reference.
async fn spill(
    sink: &dyn ArtifactSink,
    media_type: &str,
    run_id: RunId,
    bytes: &[u8],
) -> Result<Option<ArtifactRef>, ToolError> {
    if bytes.is_empty() {
        return Ok(None);
    }
    let provenance = Provenance::tool_output(Shell::NAME, run_id);
    let reference = sink
        .store(media_type, provenance, bytes)
        .await
        .map_err(ToolError::Other)?;
    Ok(Some(reference))
}

/// Read a taken pipe fully, holding at most `max` bytes in memory and draining
/// (but not retaining) the rest. Returns the captured bytes and whether the cap
/// was exceeded.
async fn drain(
    reader: Option<impl AsyncReadExt + Unpin>,
    max: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let Some(mut reader) = reader else {
        return Ok((Vec::new(), false));
    };
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 8192];
    let mut overflowed = false;
    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        if buf.len() < max {
            let take = (max - buf.len()).min(n);
            buf.extend_from_slice(&chunk[..take]);
            if take < n {
                // This chunk pushed us past the cap: keep the prefix, then drain
                // the rest of the pipe to the void so the child never blocks.
                overflowed = true;
                tokio::io::copy(&mut reader, &mut tokio::io::sink()).await?;
                break;
            }
        } else {
            // Already at the cap: drain everything still coming in one shot.
            overflowed = true;
            tokio::io::copy(&mut reader, &mut tokio::io::sink()).await?;
            break;
        }
    }
    Ok((buf, overflowed))
}

/// Grace period a drain task gets after the child was killed before it is
/// abandoned (an escaped grandchild can hold the pipe open forever).
const DRAIN_GRACE: Duration = Duration::from_secs(5);

/// Like [`join_drain`], but bounded: used after a timeout kill, where the pipe
/// may never reach EOF. On expiry the reader task is aborted and the stream is
/// reported as empty-and-overflowed rather than wedging the whole tool call.
async fn join_drain_bounded(
    mut task: tokio::task::JoinHandle<std::io::Result<(Vec<u8>, bool)>>,
) -> Result<(Vec<u8>, bool), ToolError> {
    match tokio::time::timeout(DRAIN_GRACE, &mut task).await {
        Ok(Ok(result)) => result.map_err(ToolError::Io),
        Ok(Err(join)) => Err(ToolError::Other(anyhow::anyhow!(
            "output reader task failed: {join}"
        ))),
        Err(_elapsed) => {
            task.abort();
            Ok((Vec::new(), true))
        }
    }
}

/// Interval bounds for the non-reaping exit poll below.
const EXIT_POLL_MIN: Duration = Duration::from_millis(1);
const EXIT_POLL_MAX: Duration = Duration::from_millis(25);

/// Wait for the child to exit, sweeping its process group *before* it is reaped,
/// and return its status.
///
/// `Child::wait` reaps, and a reaped pid can be recycled by the kernel — so the
/// old "wait, then kill the group" order could SIGKILL a group belonging to some
/// unrelated process that had since been handed the same pid. Termination is
/// therefore detected with `waitid(WNOWAIT)`, which leaves the leader a zombie
/// (pid, and so pgid, still reserved), the group is swept, and only then is the
/// child reaped — `wait` returns immediately at that point.
///
/// The cost of that ordering is a poll rather than tokio's SIGCHLD-driven wake:
/// the interval starts at 1 ms and backs off to 25 ms, so a short command is
/// still noticed within a millisecond or two of exiting. If the probe cannot
/// observe the child at all, it degrades to a plain `wait` (no group sweep,
/// because at that point no pgid can be proven to be ours).
async fn wait_and_sweep(child: &mut Child, pid: Option<u32>) -> std::io::Result<ExitStatus> {
    #[cfg(unix)]
    if let Some(pid) = pid {
        let mut interval = EXIT_POLL_MIN;
        loop {
            match codypendent_sandbox::executor::child_terminated_unreaped(pid) {
                Some(true) => {
                    sweep_process_group(Some(pid));
                    return child.wait().await;
                }
                Some(false) => {}
                None => return child.wait().await,
            }
            tokio::time::sleep(interval).await;
            interval = (interval * 2).min(EXIT_POLL_MAX);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
    child.wait().await
}

/// Terminate the child's process group on timeout.
///
/// The child leads its own group (`process_group(0)`), and is still live here,
/// so the pgid is unambiguously ours: sweep the group first, then SIGKILL and
/// reap the direct child (which alone covers a single-process command such as
/// `sleep`).
async fn kill_group(pid: Option<u32>, child: &mut Child) {
    sweep_process_group(pid);
    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// SIGKILL the whole process group led by `pid`.
///
/// A `kill(-pgid, SIGKILL)` syscall, not a `/bin/kill` subprocess: that binary
/// is missing on minimal images, where the sweep silently did nothing and
/// grandchildren leaked. **Only call this while `pid` is unreaped** — see
/// [`codypendent_sandbox::executor::kill_process_group`].
fn sweep_process_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        codypendent_sandbox::executor::kill_process_group(pid);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// Synchronous drop backstop. If cancellation drops `Shell::execute` while the
/// child is live, kill the process group rather than relying on `kill_on_drop`,
/// which targets only the direct child.
struct ProcessGroupGuard {
    pid: Option<u32>,
    armed: bool,
}

impl ProcessGroupGuard {
    fn new(pid: Option<u32>) -> Self {
        Self { pid, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        // Armed means the child was never reaped by `execute` (it is dropped
        // after this guard, so it is still unreaped here too): the pgid is ours
        // and the sweep is safe. A syscall, so this Drop does not fork/exec.
        if self.armed {
            sweep_process_group(self.pid);
        }
    }
}

/// Resolve `program` to an absolute path, searching the daemon PATH for a bare
/// name. Returns `None` when nothing matches. Async so the executable-bit checks
/// never block the runtime thread.
pub(crate) async fn resolve_program(program: &Path, cwd: &Path) -> Option<PathBuf> {
    if program.is_absolute() {
        return is_executable(program).await.then(|| program.to_path_buf());
    }
    if program.components().count() > 1 {
        let joined = cwd.join(program);
        return is_executable(&joined).await.then_some(joined);
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(program);
        if is_executable(&candidate).await {
            return Some(candidate);
        }
    }
    None
}

/// Whether `path` is a regular file with an execute bit set. Checking the bit
/// (not merely existence) skips non-executable shims that shadow a real binary
/// earlier on PATH. Uses async `tokio::fs` so it does not block the runtime.
#[cfg(unix)]
async fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::metadata(path)
        .await
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
async fn is_executable(path: &Path) -> bool {
    tokio::fs::metadata(path)
        .await
        .map(|m| m.is_file())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::is_denied_env;

    /// The execution-hijacking environment names the shell tool must refuse,
    /// grouped by family. A gap here means an allow-listed interpreter can be
    /// steered by a model-supplied variable past a benign-looking approval.
    #[test]
    fn denies_execution_hijacking_env_names() {
        for name in [
            "PATH",
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "DYLD_INSERT_LIBRARIES",
            "RUSTC_WRAPPER",
            "CC_WRAPPER",
            "GIT_SSH_COMMAND",
            "GIT_EXTERNAL_DIFF",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_GLOBAL",
            "BASH_ENV",
            "ENV",
            "SHELLOPTS",
            "CDPATH",
            "NODE_OPTIONS",
            "PYTHONPATH",
            "PYTHONSTARTUP",
            "PYTHONHOME",
            "PERL5OPT",
            "PERL5LIB",
            "RUBYOPT",
            // FP-1: the Ruby load-path analogue of PERL5LIB/PYTHONPATH; a gap
            // here let `RUBYLIB` prepend an attacker directory to an
            // allow-listed `ruby`/`rake` invocation.
            "RUBYLIB",
            "PROMPT_COMMAND",
            "IFS",
            "ZDOTDIR",
            "GLIBC_TUNABLES",
            "LUA_PATH",
            "LUA_CPATH",
        ] {
            assert!(is_denied_env(name), "{name} must be denied");
        }
    }

    /// The deny check is case-insensitive (env names are matched uppercased) so
    /// a lower/mixed-case spelling cannot slip a denied variable through.
    #[test]
    fn deny_check_is_case_insensitive() {
        assert!(is_denied_env("rubylib"));
        assert!(is_denied_env("RubyLib"));
        assert!(is_denied_env("ld_preload"));
    }

    /// A benign build/config variable is allowed — the list is a targeted
    /// deny-list, not a blanket refusal.
    #[test]
    fn allows_ordinary_variables() {
        for name in [
            "CARGO_TERM_COLOR",
            "RUST_LOG",
            "MY_APP_CONFIG",
            "PYTHONUNBUFFERED",
        ] {
            assert!(!is_denied_env(name), "{name} should be allowed");
        }
    }

    /// Variables that do not *look* like execution control but name a program
    /// the allow-listed tool then EXECS. Each of these turned a benign-looking
    /// approved command into arbitrary code execution: `git log` spawns
    /// `$GIT_PAGER`/`$PAGER` through `sh -c`, `git rebase`/`git commit` exec
    /// `$GIT_EDITOR`/`$EDITOR`, `less`/`man` run `$LESSOPEN` as an input
    /// preprocessor, `make` reads `$MAKEFILES` before the named target,
    /// `python` imports and calls `$PYTHONBREAKPOINT`, and `cargo` re-execs
    /// `$CARGO` for build scripts and test harnesses.
    #[test]
    fn denies_delegated_helper_program_env_names() {
        for name in [
            "GIT_PAGER",
            "PAGER",
            "MANPAGER",
            "EDITOR",
            "VISUAL",
            "GIT_EDITOR",
            "GIT_SEQUENCE_EDITOR",
            "LESSOPEN",
            "LESSCLOSE",
            "MAKEFILES",
            "MAKEFLAGS",
            "PYTHONBREAKPOINT",
            "CARGO",
            "CARGO_BUILD_RUSTC",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER",
        ] {
            assert!(is_denied_env(name), "{name} must be denied");
        }
    }
}
