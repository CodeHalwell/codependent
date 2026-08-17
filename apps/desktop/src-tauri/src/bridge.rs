//! The webview-facing half of the shell: Tauri commands in, a Tauri channel
//! of daemon frames out.
//!
//! Nothing here decides anything. Every command is a thin wrapper over a real
//! protocol command, and every reply is what the daemon said. If the daemon is
//! not reachable, these commands return an error string and the UI renders a
//! disconnected state — there is no path through this module that produces
//! transcript content the daemon did not emit.

use std::sync::Arc;

use codypendent_protocol::{
    AgentMode, AnalyticsExportRequest, AnalyticsExportResult, AnalyticsPage, AnalyticsQuery,
    ApprovalDecision, ApprovalId, ArtifactRef, InboxEntry, InboxListQuery, InboxMutation,
    InboxPage, RunId, SessionId,
};
use tauri::ipc::{Channel, Response};
use tauri::State;
use tokio::sync::Mutex;

use crate::daemon::{
    socket_path, ConnectionInfo, DaemonClient, DaemonFrame, FrameSink, SessionRow,
};

/// A Tauri channel used as the frame sink. This is the only place a daemon
/// frame becomes a webview message.
struct ChannelSink(Channel<DaemonFrame>);

impl FrameSink for ChannelSink {
    fn emit(&self, frame: DaemonFrame) {
        // A send failure means the webview went away (window closed, reload).
        // There is nothing useful to do about it here; the reader task ends
        // with the connection.
        let _ = self.0.send(frame);
    }
}

/// The connection the shell currently holds, if any.
#[derive(Default)]
pub struct Bridge {
    connection: Mutex<Option<Connected>>,
}

struct Connected {
    client: Arc<DaemonClient>,
    sink: Arc<ChannelSink>,
}

/// Where the shell will look for a daemon, so the UI can name the socket in a
/// disconnected state instead of saying "unavailable" with no detail.
#[tauri::command]
fn daemon_socket() -> Result<String, String> {
    socket_path()
        .map(|path| path.display().to_string())
        .map_err(|error| format!("{error:#}"))
}

/// Connect and handshake. Succeeds only when a daemon actually answered.
#[tauri::command]
async fn daemon_connect(
    bridge: State<'_, Bridge>,
    channel: Channel<DaemonFrame>,
) -> Result<ConnectionInfo, String> {
    let socket = socket_path().map_err(|error| format!("{error:#}"))?;
    let repository = std::env::current_dir()
        .ok()
        .map(|dir| dir.display().to_string());
    let sink = Arc::new(ChannelSink(channel));

    let (client, info) = DaemonClient::connect(&socket, repository, Arc::clone(&sink))
        .await
        .map_err(|error| format!("{error:#}"))?;

    *bridge.connection.lock().await = Some(Connected { client, sink });
    Ok(info)
}

/// Drop the connection. The reader task ends when the socket closes.
#[tauri::command]
async fn daemon_disconnect(bridge: State<'_, Bridge>) -> Result<(), String> {
    bridge.connection.lock().await.take();
    Ok(())
}

#[tauri::command]
async fn list_sessions(bridge: State<'_, Bridge>) -> Result<Vec<SessionRow>, String> {
    let client = client_of(&bridge).await?;
    client
        .list_sessions()
        .await
        .map_err(|error| format!("{error:#}"))
}

/// Submit an objective as a real run. The reply carries the ids the daemon
/// minted; the transcript fills from the events that follow.
#[tauri::command]
async fn start_objective(
    bridge: State<'_, Bridge>,
    objective: String,
) -> Result<crate::daemon::RunHandle, String> {
    let (client, sink) = connected(&bridge).await?;
    client
        .start_objective(objective, AgentMode::Build, &sink)
        .await
        .map_err(|error| format!("{error:#}"))
}

/// Attach to a session that already exists; its catch-up replays into the
/// transcript.
#[tauri::command]
async fn attach_session(bridge: State<'_, Bridge>, session_id: SessionId) -> Result<(), String> {
    let (client, sink) = connected(&bridge).await?;
    client
        .attach(session_id, &sink)
        .await
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
async fn cancel_run(bridge: State<'_, Bridge>, run_id: RunId) -> Result<(), String> {
    let client = client_of(&bridge).await?;
    client
        .cancel_run(run_id)
        .await
        .map_err(|error| format!("{error:#}"))
}

/// Resolve the exact approval shown by the desktop card. The webview supplies
/// only approve/reject; the daemon client fixes the authority scope to `Once`.
#[tauri::command]
async fn resolve_approval(
    bridge: State<'_, Bridge>,
    approval_id: ApprovalId,
    approved: bool,
) -> Result<(), String> {
    let client = client_of(&bridge).await?;
    let decision = if approved {
        ApprovalDecision::Approve
    } else {
        ApprovalDecision::Reject
    };
    client
        .resolve_approval(approval_id, decision)
        .await
        .map_err(|error| format!("{error:#}"))
}

/// One page of the durable inbox, straight from `ListInbox`. An error here is
/// the honest answer when the daemon is absent or refused: the Inbox view
/// renders "unavailable" on it, which is not the same thing as an empty page.
#[tauri::command]
async fn list_inbox(
    bridge: State<'_, Bridge>,
    query: Option<InboxListQuery>,
) -> Result<InboxPage, String> {
    let client = client_of(&bridge).await?;
    client
        .list_inbox(query.unwrap_or_default())
        .await
        .map_err(|error| format!("{error:#}"))
}

/// Apply one idempotent inbox mutation and return the daemon's projection of
/// the entry afterwards.
#[tauri::command]
async fn mutate_inbox(
    bridge: State<'_, Bridge>,
    mutation: InboxMutation,
) -> Result<InboxEntry, String> {
    let client = client_of(&bridge).await?;
    client
        .mutate_inbox(mutation)
        .await
        .map_err(|error| format!("{error:#}"))
}

/// Measured analytics, straight from `QueryAnalytics`. Absent measurements stay
/// absent across this boundary — the page is forwarded as the daemon serialized
/// it, so a metric the daemon never measured arrives as null, not as zero.
#[tauri::command]
async fn query_analytics(
    bridge: State<'_, Bridge>,
    query: Option<AnalyticsQuery>,
) -> Result<AnalyticsPage, String> {
    let client = client_of(&bridge).await?;
    client
        .query_analytics(query.unwrap_or_default())
        .await
        .map_err(|error| format!("{error:#}"))
}

/// Export a bounded analytics query. The reply names the artifact the daemon
/// wrote; `read_artifact` fetches its bytes.
#[tauri::command]
async fn export_analytics(
    bridge: State<'_, Bridge>,
    request: AnalyticsExportRequest,
) -> Result<AnalyticsExportResult, String> {
    let client = client_of(&bridge).await?;
    client
        .export_analytics(request)
        .await
        .map_err(|error| format!("{error:#}"))
}

/// An artifact's bytes, retrieved and verified against the reference the
/// webview observed.
///
/// Returned as a raw IPC response rather than a string: an artifact is bytes
/// (a patch, a CSV, an audio blob), and decoding it as text in the shell would
/// both corrupt non-UTF-8 content and hide that fact. The webview receives an
/// `ArrayBuffer` and decodes it itself when it knows the artifact is text.
#[tauri::command]
async fn read_artifact(
    bridge: State<'_, Bridge>,
    artifact: ArtifactRef,
) -> Result<Response, String> {
    let client = client_of(&bridge).await?;
    client
        .read_artifact(&artifact)
        .await
        .map(Response::new)
        .map_err(|error| format!("{error:#}"))
}

async fn client_of(bridge: &State<'_, Bridge>) -> Result<Arc<DaemonClient>, String> {
    Ok(connected(bridge).await?.0)
}

async fn connected(
    bridge: &State<'_, Bridge>,
) -> Result<(Arc<DaemonClient>, Arc<ChannelSink>), String> {
    let guard = bridge.connection.lock().await;
    match guard.as_ref() {
        Some(connection) => Ok((Arc::clone(&connection.client), Arc::clone(&connection.sink))),
        None => Err("not connected to codypendentd".to_string()),
    }
}

/// Register the bridge state and command handlers on a Tauri builder.
pub fn register<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .manage(Bridge::default())
        .invoke_handler(tauri::generate_handler![
            daemon_socket,
            daemon_connect,
            daemon_disconnect,
            list_sessions,
            start_objective,
            attach_session,
            cancel_run,
            resolve_approval,
            list_inbox,
            mutate_inbox,
            query_analytics,
            export_analytics,
            read_artifact
        ])
}
