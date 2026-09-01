//! Starting `codypendentd` from the desktop shell.
//!
//! The shell used to connect to the socket and nothing else. On a machine
//! where no daemon is running — every first launch — the operator got a
//! banner quoting `No such file or directory (os error 2)` and a reconnect
//! loop that would never succeed, because nothing anywhere told them a daemon
//! had to be started, let alone started it. The CLI has done this for itself
//! since Phase 1 (`crates/cli/src/commands.rs::ensure_daemon`): spawn the
//! daemon detached, log it to `<data_dir>/logs/daemon.log`, wait for the
//! socket. This module is that flow for the desktop, with one difference: the
//! desktop binary is not `codypendent`, so the daemon program has to be found
//! rather than assumed to be `current_exe`.
//!
//! Nothing here is privileged beyond what the operator could type in a
//! terminal — `codypendent daemon start` — and the resolution never guesses:
//! when no program can be found the outcome names exactly what was looked for
//! and the command to run by hand.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{read_envelope, write_envelope, ClientId, Envelope, Payload};
use serde::Serialize;
use tokio::net::UnixStream;

/// How long to wait for the socket to answer after spawning, in total.
const START_BUDGET: Duration = Duration::from_secs(8);
/// How often to poll the socket while waiting.
const START_POLL: Duration = Duration::from_millis(100);

/// The daemon program the shell would launch, and how.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonInvocation {
    /// The executable, as resolved.
    pub program: String,
    /// Arguments after the program: `["__daemon"]` for the single binary,
    /// nothing for a standalone `codypendentd`.
    pub args: Vec<String>,
}

/// Where a daemon program was found: the operator's override, the `PATH`, or
/// one of the directories the installer uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InvocationSource {
    Override,
    Path,
    KnownDirectory,
    BesideThisApp,
}

/// What `daemon_launch_status` answers with: whether anything is listening,
/// what the shell would launch, and what to type if it cannot.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchStatus {
    pub socket_path: String,
    /// Whether a daemon answered a `Ping` on that socket just now.
    pub listening: bool,
    /// The program the shell would run, when one was found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invocation: Option<DaemonInvocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<InvocationSource>,
    /// The command to run in a terminal instead. Always present, so the UI can
    /// always name it.
    pub manual_command: String,
    /// Where the spawned daemon's output goes.
    pub log_path: String,
    /// Every location that was checked and had nothing, when none had it.
    pub searched: Vec<String>,
}

/// What `daemon_start` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum StartOutcome {
    /// A daemon was already answering; nothing was spawned.
    AlreadyRunning,
    /// A daemon was spawned and answered within the budget.
    Started { pid: u32, program: String },
}

/// The environment variable an operator sets to name the daemon program
/// explicitly — a development build, a non-standard install directory.
pub const PROGRAM_OVERRIDE_ENV: &str = "CODYPENDENT_BINARY";

/// The command shown to the operator when the shell cannot start the daemon
/// itself. `codypendent daemon start` is the documented human-facing verb
/// (`docs/cli-and-tui-user-guide.md` §2).
pub const MANUAL_COMMAND: &str = "codypendent daemon start";

/// True when a daemon answers `Ping` with `Pong` on this socket. The CLI's
/// `client::ping`, verbatim: a `DaemonStatus` request would be richer, but
/// "is anything there" is the only question the launcher asks.
pub async fn ping(socket: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket).await else {
        return false;
    };
    let request = Envelope::request(ClientId::new(), Payload::Ping);
    if write_envelope(&mut stream, &request).await.is_err() {
        return false;
    }
    matches!(
        read_envelope(&mut stream).await,
        Ok(Some(Envelope {
            payload: Payload::Pong,
            ..
        }))
    )
}

/// The directories an installer puts `codypendent` in, tried when `PATH` does
/// not have it. A bundled `.app` launched from Finder inherits a minimal
/// `PATH` that has none of them, which is exactly when this list matters.
fn known_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        directories.push(home.join(".cargo").join("bin"));
        directories.push(home.join(".local").join("bin"));
    }
    directories.push(PathBuf::from("/usr/local/bin"));
    directories.push(PathBuf::from("/opt/homebrew/bin"));
    directories.push(PathBuf::from("/opt/codypendent/bin"));
    directories
}

/// The program names to look for, in order: the single binary first (it runs
/// a daemon that always matches its own protocol), then the standalone daemon.
const PROGRAM_NAMES: [&str; 2] = ["codypendent", "codypendentd"];

fn invocation_for(program: &Path) -> DaemonInvocation {
    let is_single_binary = program
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem == "codypendent");
    DaemonInvocation {
        program: program.display().to_string(),
        args: if is_single_binary {
            vec!["__daemon".to_owned()]
        } else {
            Vec::new()
        },
    }
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Everything the resolver consults, so the pure resolution below can be
/// exercised with a fake environment.
struct Environment {
    override_program: Option<PathBuf>,
    path_entries: Vec<PathBuf>,
    known_directories: Vec<PathBuf>,
    beside_app: Option<PathBuf>,
}

impl Environment {
    fn from_process() -> Self {
        Self {
            override_program: std::env::var_os(PROGRAM_OVERRIDE_ENV)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            path_entries: std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).collect())
                .unwrap_or_default(),
            known_directories: known_directories(),
            beside_app: std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(Path::to_path_buf)),
        }
    }
}

/// Resolve the daemon program, returning what was found (and where), or every
/// location that was checked.
fn resolve_in(
    environment: &Environment,
    executable: &dyn Fn(&Path) -> bool,
) -> Result<(DaemonInvocation, InvocationSource), Vec<String>> {
    let mut searched = Vec::new();
    if let Some(program) = &environment.override_program {
        if executable(program) {
            return Ok((invocation_for(program), InvocationSource::Override));
        }
        searched.push(format!("{PROGRAM_OVERRIDE_ENV}={}", program.display()));
    }
    let candidates = |directories: &[PathBuf]| {
        let mut found = Vec::new();
        for directory in directories {
            for name in PROGRAM_NAMES {
                found.push(directory.join(name));
            }
        }
        found
    };
    for (directories, source) in [
        (&environment.path_entries, InvocationSource::Path),
        (
            &environment.known_directories,
            InvocationSource::KnownDirectory,
        ),
    ] {
        for candidate in candidates(directories) {
            if executable(&candidate) {
                return Ok((invocation_for(&candidate), source));
            }
            searched.push(candidate.display().to_string());
        }
    }
    if let Some(beside) = &environment.beside_app {
        for name in PROGRAM_NAMES {
            let candidate = beside.join(name);
            if executable(&candidate) {
                return Ok((invocation_for(&candidate), InvocationSource::BesideThisApp));
            }
            searched.push(candidate.display().to_string());
        }
    }
    Err(searched)
}

fn resolve() -> Result<(DaemonInvocation, InvocationSource), Vec<String>> {
    resolve_in(&Environment::from_process(), &is_executable)
}

fn log_path(paths: &RuntimePaths) -> PathBuf {
    paths.log_dir.join("daemon.log")
}

/// Whether a daemon is listening, and what the shell would do about it if not.
pub async fn launch_status(paths: &RuntimePaths) -> LaunchStatus {
    let listening = ping(&paths.socket_path).await;
    let (invocation, source, searched) = match resolve() {
        Ok((invocation, source)) => (Some(invocation), Some(source), Vec::new()),
        Err(searched) => (None, None, searched),
    };
    LaunchStatus {
        socket_path: paths.socket_path.display().to_string(),
        listening,
        invocation,
        source,
        manual_command: MANUAL_COMMAND.to_owned(),
        log_path: log_path(paths).display().to_string(),
        searched,
    }
}

/// Start a daemon unless one is already answering, then wait for the socket.
///
/// The spawn is detached — its own process group, stdin closed, output to the
/// daemon log — so it outlives this window, exactly as the CLI's does. An
/// error names what was tried and the command to run by hand.
pub async fn start_daemon(paths: &RuntimePaths) -> anyhow::Result<StartOutcome> {
    if ping(&paths.socket_path).await {
        return Ok(StartOutcome::AlreadyRunning);
    }
    let (invocation, _) = resolve().map_err(|searched| {
        anyhow::anyhow!(
            "no codypendent program was found to start the daemon with. Looked for \
             `codypendent` and `codypendentd` on PATH and in {}. Install Codypendent \
             (see install.sh), set {PROGRAM_OVERRIDE_ENV} to the binary, or run \
             `{MANUAL_COMMAND}` in a terminal.",
            searched
                .iter()
                .filter(|entry| entry.ends_with("/codypendent"))
                .map(|entry| entry.trim_end_matches("/codypendent").to_owned())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;

    paths
        .ensure_directories()
        .context("creating the codypendent data directories")?;
    let log = log_path(paths);
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("opening {}", log.display()))?;
    let log_for_stderr = log_file
        .try_clone()
        .with_context(|| format!("opening {}", log.display()))?;

    let mut command = std::process::Command::new(&invocation.program);
    command
        .args(&invocation.args)
        .stdin(std::process::Stdio::null())
        .stdout(log_file)
        .stderr(log_for_stderr);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Its own process group: the daemon must not die with this window.
        command.process_group(0);
    }
    let child = command
        .spawn()
        .with_context(|| format!("starting {}", invocation.program))?;
    let pid = child.id();

    let deadline = tokio::time::Instant::now() + START_BUDGET;
    while tokio::time::Instant::now() < deadline {
        if ping(&paths.socket_path).await {
            return Ok(StartOutcome::Started {
                pid,
                program: invocation.program,
            });
        }
        tokio::time::sleep(START_POLL).await;
    }
    bail!(
        "{} (pid {pid}) was started but nothing answered on {} within {} seconds; check {}",
        invocation.program,
        paths.socket_path.display(),
        START_BUDGET.as_secs(),
        log.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn environment(
        override_program: Option<&str>,
        path_entries: &[&str],
        known: &[&str],
        beside: Option<&str>,
    ) -> Environment {
        Environment {
            override_program: override_program.map(PathBuf::from),
            path_entries: path_entries.iter().map(PathBuf::from).collect(),
            known_directories: known.iter().map(PathBuf::from).collect(),
            beside_app: beside.map(PathBuf::from),
        }
    }

    fn present(paths: &[&str]) -> impl Fn(&Path) -> bool {
        let set: HashSet<PathBuf> = paths.iter().map(PathBuf::from).collect();
        move |path: &Path| set.contains(path)
    }

    #[test]
    fn the_single_binary_on_path_is_launched_as_its_own_daemon() {
        let env = environment(None, &["/usr/bin", "/usr/local/bin"], &[], None);
        let (invocation, source) =
            resolve_in(&env, &present(&["/usr/local/bin/codypendent"])).expect("found");
        assert_eq!(source, InvocationSource::Path);
        assert_eq!(invocation.program, "/usr/local/bin/codypendent");
        assert_eq!(invocation.args, vec!["__daemon".to_owned()]);
    }

    #[test]
    fn a_standalone_daemon_binary_takes_no_arguments() {
        let env = environment(None, &["/usr/local/bin"], &[], None);
        let (invocation, _) =
            resolve_in(&env, &present(&["/usr/local/bin/codypendentd"])).expect("found");
        assert_eq!(invocation.program, "/usr/local/bin/codypendentd");
        assert!(invocation.args.is_empty());
    }

    #[test]
    fn the_override_wins_and_a_bad_override_is_reported_not_trusted() {
        let env = environment(Some("/opt/dev/codypendent"), &["/usr/local/bin"], &[], None);
        let (invocation, source) = resolve_in(
            &env,
            &present(&["/opt/dev/codypendent", "/usr/local/bin/codypendent"]),
        )
        .expect("found");
        assert_eq!(source, InvocationSource::Override);
        assert_eq!(invocation.program, "/opt/dev/codypendent");

        let searched = resolve_in(&env, &present(&[])).expect_err("nothing there");
        assert_eq!(searched[0], "CODYPENDENT_BINARY=/opt/dev/codypendent");
        assert!(searched.contains(&"/usr/local/bin/codypendent".to_owned()));
    }

    #[test]
    fn known_directories_and_the_app_bundle_are_fallbacks_after_path() {
        let env = environment(
            None,
            &["/usr/bin"],
            &["/home/dan/.cargo/bin"],
            Some("/Applications/Codypendent.app/Contents/MacOS"),
        );
        let (invocation, source) =
            resolve_in(&env, &present(&["/home/dan/.cargo/bin/codypendent"])).expect("found");
        assert_eq!(source, InvocationSource::KnownDirectory);
        assert!(invocation.program.ends_with("/.cargo/bin/codypendent"));

        let (invocation, source) = resolve_in(
            &env,
            &present(&["/Applications/Codypendent.app/Contents/MacOS/codypendentd"]),
        )
        .expect("found");
        assert_eq!(source, InvocationSource::BesideThisApp);
        assert!(invocation.args.is_empty());
    }

    #[tokio::test]
    async fn an_answering_daemon_is_reported_as_already_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().join("data"));
        paths.ensure_directories().expect("dirs");
        let listener = tokio::net::UnixListener::bind(&paths.socket_path).expect("bind");
        // Answers every Ping until aborted: the status read below pings again.
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let request = read_envelope(&mut stream)
                    .await
                    .expect("read")
                    .expect("envelope");
                assert!(matches!(request.payload, Payload::Ping));
                let reply = Envelope::reply_to(&request, Payload::Pong);
                write_envelope(&mut stream, &reply).await.expect("write");
            }
        });
        assert!(ping(&paths.socket_path).await);
        let status = launch_status(&paths).await;
        assert!(status.listening);
        assert_eq!(status.manual_command, MANUAL_COMMAND);
        assert!(matches!(
            start_daemon(&paths).await.expect("already running"),
            StartOutcome::AlreadyRunning
        ));
        server.abort();
    }

    #[tokio::test]
    async fn a_silent_socket_is_not_listening() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().join("data"));
        assert!(!ping(&paths.socket_path).await);
        let status = launch_status(&paths).await;
        assert!(!status.listening);
        assert!(status.log_path.ends_with("daemon.log"));
    }
}
