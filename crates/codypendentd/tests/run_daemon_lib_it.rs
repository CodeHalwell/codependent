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
