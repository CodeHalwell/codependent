//! `codypendentd` — the persistent Codypendent daemon (standalone binary shell).
//!
//! The daemon's run-loop lives in this crate's library (`lib.rs`) as
//! [`run_daemon`], so the single `codypendent` binary can run the SAME daemon
//! via `codypendent __daemon`. This shell keeps the standalone `codypendentd`
//! binary working byte-for-byte: parse arguments, init tracing, resolve paths,
//! delegate.
//!
//! [`run_daemon`]: codypendent_codypendentd::run_daemon

use codypendent_protocol::discovery::RuntimePaths;

/// What the command line asked for.
enum Invocation {
    /// No arguments: boot the daemon, exactly as before argument parsing existed.
    Run,
    /// Print `text` on stdout and exit 0 without touching anything.
    Print(String),
    /// Print `text` on stderr and exit 2 without touching anything.
    Refuse(String),
}

const USAGE: &str = "\
codypendentd — the persistent Codypendent daemon

Usage: codypendentd [OPTIONS]

Options:
  -h, --help     Print this help and exit
  -V, --version  Print the daemon version and exit

The daemon takes no other arguments; it is configured through the environment:
  CODYPENDENT_DATA_DIR   the data directory (database, socket, secrets)
  CODYPENDENT_SOCKET     an explicit socket path, overriding the data directory
";

/// Classify the command line **before** anything is opened.
///
/// This exists because the binary previously ignored its arguments entirely:
/// `codypendentd --version` did not print a version, it *started a daemon* —
/// creating the user's real database, WAL and daemon secret under the default
/// data directory and running startup recovery, which mutates existing state.
/// The two flags every Unix binary answers must answer without side effects, and
/// an argument this daemon does not understand must be refused rather than
/// silently dropped (a typo'd flag is not consent to boot with defaults).
fn classify(mut args: impl Iterator<Item = String>) -> Invocation {
    // The first argument decides: every argument this daemon understands is
    // terminal, and anything else is refused, so there is never a second one to
    // read.
    let Some(argument) = args.next() else {
        return Invocation::Run;
    };
    match argument.as_str() {
        "-h" | "--help" => Invocation::Print(USAGE.to_string()),
        "-V" | "--version" => {
            Invocation::Print(format!("codypendentd {}", env!("CARGO_PKG_VERSION")))
        }
        other => Invocation::Refuse(format!(
            "codypendentd: unrecognized argument `{other}`\n\n{USAGE}"
        )),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Argument handling comes first: `--version` must not open a database.
    match classify(std::env::args().skip(1)) {
        Invocation::Run => {}
        Invocation::Print(text) => {
            println!("{text}");
            return Ok(());
        }
        Invocation::Refuse(text) => {
            eprintln!("{text}");
            std::process::exit(2);
        }
    }

    codypendent_codypendentd::init_tracing();

    let paths = RuntimePaths::resolve()?;
    paths.ensure_directories()?;

    codypendent_codypendentd::run_daemon(paths).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify_args(args: &[&str]) -> Invocation {
        classify(args.iter().map(|arg| (*arg).to_string()))
    }

    #[test]
    fn no_arguments_still_boots_the_daemon() {
        assert!(matches!(classify_args(&[]), Invocation::Run));
    }

    #[test]
    fn version_and_help_answer_without_booting() {
        for flag in ["-V", "--version"] {
            match classify_args(&[flag]) {
                Invocation::Print(text) => {
                    assert!(text.starts_with("codypendentd "), "got {text}");
                    assert!(text.contains(env!("CARGO_PKG_VERSION")));
                }
                _ => panic!("{flag} must print a version"),
            }
        }
        for flag in ["-h", "--help"] {
            match classify_args(&[flag]) {
                Invocation::Print(text) => assert!(text.contains("Usage: codypendentd")),
                _ => panic!("{flag} must print usage"),
            }
        }
    }

    #[test]
    fn an_unrecognized_argument_is_refused_not_ignored() {
        match classify_args(&["--data-dir=/tmp/x"]) {
            Invocation::Refuse(text) => {
                assert!(text.contains("unrecognized argument `--data-dir=/tmp/x`"));
            }
            _ => panic!("an unknown flag must be refused, never silently ignored"),
        }
    }
}
