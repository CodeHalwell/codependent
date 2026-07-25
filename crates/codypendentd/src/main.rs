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
