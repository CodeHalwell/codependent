//! Minimal protocol client used by CLI commands.

use std::path::Path;

use codypendent_protocol::{
    read_envelope, write_envelope, ClientId, DaemonStatus, Envelope, Payload,
};
use tokio::net::UnixStream;

async fn request(socket: &Path, payload: Payload) -> anyhow::Result<Envelope> {
    let mut stream = UnixStream::connect(socket).await?;
    let request = Envelope::request(ClientId::new(), payload);
    write_envelope(&mut stream, &request).await?;
    match read_envelope(&mut stream).await? {
        Some(reply) => Ok(reply),
        None => anyhow::bail!("daemon closed the connection before replying"),
    }
}

/// True when a daemon answers Ping with Pong on this socket.
pub async fn ping(socket: &Path) -> bool {
    matches!(
        request(socket, Payload::Ping).await,
        Ok(Envelope {
            payload: Payload::Pong,
            ..
        })
    )
}

pub async fn daemon_status(socket: &Path) -> anyhow::Result<DaemonStatus> {
    match request(socket, Payload::DaemonStatusRequest).await?.payload {
        Payload::DaemonStatusResponse(status) => Ok(status),
        other => anyhow::bail!("unexpected reply to status request: {other:?}"),
    }
}

pub async fn shutdown(socket: &Path) -> anyhow::Result<()> {
    match request(socket, Payload::Shutdown).await?.payload {
        Payload::ShutdownAck => Ok(()),
        other => anyhow::bail!("unexpected reply to shutdown request: {other:?}"),
    }
}

/// What an idle-guarded shutdown ([`shutdown_if_idle`]) did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownIfIdleOutcome {
    /// The daemon was idle and acknowledged — it is shutting down.
    Stopped,
    /// The daemon refused because a run is active; it keeps running. Carries
    /// the count it observed.
    RefusedActive(u64),
}

/// Ask the daemon to shut down ONLY if it is idle (protocol v1.3). Unlike
/// [`shutdown`], the daemon may refuse — atomically, against concurrent run
/// admission — when a run is active, in which case it keeps running and the
/// caller must not restart it. Only send this to a daemon whose negotiated
/// minor is ≥ 3 (an older daemon cannot decode `ShutdownIfIdle`).
pub async fn shutdown_if_idle(socket: &Path) -> anyhow::Result<ShutdownIfIdleOutcome> {
    match request(socket, Payload::ShutdownIfIdle).await?.payload {
        Payload::ShutdownAck => Ok(ShutdownIfIdleOutcome::Stopped),
        Payload::ShutdownRefused { active_run_count } => {
            Ok(ShutdownIfIdleOutcome::RefusedActive(active_run_count))
        }
        other => anyhow::bail!("unexpected reply to idle-shutdown request: {other:?}"),
    }
}
