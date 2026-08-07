//! A minimal, hand-rolled JSON-RPC 2.0 client over newline-delimited JSON —
//! the framing discipline of `crate::acp_client` without the
//! `agent-client-protocol` crate (MCP stdio is ndjson JSON-RPC 2.0, and the
//! workspace takes no new dependency for it).
//!
//! The client is a thin handle over a driver task (the `acp_client` bridge
//! shape): [`call`](JsonRpcClient::call) / [`notify`](JsonRpcClient::notify)
//! ship commands over an mpsc channel; the driver owns the byte transport,
//! assigns monotonically increasing request ids, writes frames, and
//! demultiplexes responses by id onto oneshot waiters. When the transport
//! closes — EOF on the peer's stdout, a write failure, an oversized frame —
//! the driver fails every pending waiter so no caller hangs on a dead server.
//!
//! MCP semantics (`initialize`, `tools/list`, ...) live one layer up in
//! [`super::client`]; this module is pure JSON-RPC plumbing.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// Largest single ndjson frame the read loop accepts. Tool results should be
/// text summaries — bulk output spills through the runtime's artifact sink —
/// so 1 MiB is far beyond a sane control message while still absorbing a
/// verbose `tools/list` from a big server.
const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Depth of the command channel feeding the driver. The layer-up MCP client
/// issues one request at a time in practice; the queue only decouples a
/// caller's `send` from the driver's `recv`.
const COMMAND_QUEUE_DEPTH: usize = 16;
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// JSON-RPC `method not found` — the answer to any server→client *request*
/// (`roots/list`, `sampling/createMessage`, ...). v1 serves none of them, but
/// an unanswered request would hang a server that blocks on it.
const METHOD_NOT_FOUND: i64 = -32601;

/// A failure of the JSON-RPC transport or of one request on it.
#[derive(Debug, thiserror::Error)]
pub enum JsonRpcError {
    /// An I/O failure writing to or reading from the transport.
    #[error("json-rpc transport I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The transport closed (peer exited, EOF on its stdout) while requests
    /// were pending. Every pending waiter is failed with this so no caller
    /// hangs on a dead server.
    #[error("json-rpc transport closed before the response arrived")]
    TransportClosed,
    /// The peer answered a request with a JSON-RPC error object.
    #[error("json-rpc peer returned error {code}: {message}")]
    Server {
        /// The JSON-RPC error code.
        code: i64,
        /// The peer's error message.
        message: String,
    },
    /// No response arrived within the per-request timeout.
    #[error("json-rpc request `{method}` timed out after {timeout:?}")]
    Timeout {
        /// The method that timed out.
        method: String,
        /// The bound that fired.
        timeout: Duration,
    },
    /// A single frame exceeded the 1 MiB frame limit; the transport is
    /// considered broken and torn down.
    #[error("json-rpc frame exceeded the 1 MiB frame limit")]
    FrameTooLarge,
}

/// A command from a [`JsonRpcClient`] handle to its connection driver.
enum DriverCommand {
    /// A request expecting a response, demultiplexed by id onto `reply`.
    Request {
        method: String,
        params: Value,
        reply: oneshot::Sender<Result<Value, JsonRpcError>>,
    },
    /// A notification: written, never answered.
    Notification { method: String, params: Value },
}

/// A connected JSON-RPC peer: a thin handle over the driver task. Cloning
/// shares the same connection. Dropping the last handle closes the command
/// channel; the driver then exits and — for the [`spawn`](Self::spawn) path —
/// the dropped `Child` kills the process (`kill_on_drop`).
#[derive(Clone)]
pub struct JsonRpcClient {
    commands: mpsc::Sender<DriverCommand>,
    /// Held so the driver is not detached silently; it exits on its own once
    /// `commands` closes or the transport dies.
    _driver: Arc<JoinHandle<()>>,
}

impl JsonRpcClient {
    /// Start a driver over an existing byte transport (`reader` = the peer's
    /// output, `writer` = the peer's input). Generic over the stream halves so
    /// an in-memory `tokio::io::duplex` drives the whole client in tests;
    /// [`spawn`](Self::spawn) is the production entry point.
    pub fn connect<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::drive(reader, writer, None)
    }

    /// Spawn `command args` as a child and speak JSON-RPC over its stdio.
    ///
    /// `env` pairs are merged over the inherited environment; with
    /// `inherit_environment = false` the child starts from an EMPTY environment
    /// (a hermetic launch). stderr is INHERITED, so a server's own diagnostics
    /// land in the daemon log next to everything else — stderr is
    /// operator-facing diagnostics, not protocol, and capturing it would buy a
    /// second pump task's worth of complexity for marginal value. `cwd` sets
    /// the child's working directory; `None` (what the v1 config model passes)
    /// inherits the daemon's.
    pub fn spawn(
        command: &str,
        args: &[String],
        env: &[(String, String)],
        inherit_environment: bool,
        cwd: Option<&Path>,
    ) -> Result<Self, JsonRpcError> {
        let mut process = Command::new(command);
        process
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            process.process_group(0);
        }
        if !inherit_environment {
            process.env_clear();
        }
        for (key, value) in env {
            process.env(key, value);
        }
        if let Some(dir) = cwd {
            process.current_dir(dir);
        }
        let mut child = process.spawn()?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        Ok(Self::drive(stdout, stdin, Some(child)))
    }

    fn drive<R, W>(reader: R, writer: W, child: Option<Child>) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (commands, command_rx) = mpsc::channel(COMMAND_QUEUE_DEPTH);
        let driver = tokio::spawn(run_driver(reader, writer, command_rx, child));
        Self {
            commands,
            _driver: Arc::new(driver),
        }
    }

    /// Send a request and await its id-correlated response, bounded by
    /// `timeout`. A response that arrives after the timeout is dropped by the
    /// driver (its waiter is gone).
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, JsonRpcError> {
        let (reply, response) = oneshot::channel();
        let command = DriverCommand::Request {
            method: method.to_string(),
            params,
            reply,
        };
        let outcome = tokio::time::timeout(timeout, async move {
            self.commands
                .send(command)
                .await
                .map_err(|_| JsonRpcError::TransportClosed)?;
            response.await.map_err(|_| JsonRpcError::TransportClosed)?
        })
        .await;
        match outcome {
            Ok(result) => result,
            Err(_) => Err(JsonRpcError::Timeout {
                method: method.to_string(),
                timeout,
            }),
        }
    }

    /// Send a notification (no id, no response expected).
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), JsonRpcError> {
        self.commands
            .send(DriverCommand::Notification {
                method: method.to_string(),
                params,
            })
            .await
            .map_err(|_| JsonRpcError::TransportClosed)
    }
}

/// Drive one JSON-RPC connection: service outbound commands and inbound frames
/// until every [`JsonRpcClient`] handle drops or the transport dies, then fail
/// every pending waiter. `_child` is only HELD — dropping it on exit kills a
/// spawned server (`kill_on_drop`).
async fn run_driver<R, W>(
    reader: R,
    mut writer: W,
    mut commands: mpsc::Receiver<DriverCommand>,
    mut child: Option<Child>,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    // Keep frame assembly in its own task. A previous `select!` polled
    // `read_frame` directly beside the pruning timer; when the timer won it
    // cancelled the read after bytes had been consumed, and the next call
    // cleared the partial buffer. A slow newline-free oversized frame could
    // therefore evade the cap forever. The bounded pump is never cancelled
    // between chunks and cannot grow an unbounded delivery queue.
    let (frame_tx, mut frames) = mpsc::channel(8);
    let reader_task = tokio::spawn(pump_frames(reader, frame_tx));
    let mut pending: HashMap<u64, oneshot::Sender<Result<Value, JsonRpcError>>> = HashMap::new();
    let mut next_id: u64 = 1;
    let mut oversized = false;
    let mut prune = tokio::time::interval(Duration::from_secs(1));
    prune.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = prune.tick() => {
                pending.retain(|_, waiter| !waiter.is_closed());
            }
            command = commands.recv() => {
                let Some(command) = command else { break };
                match command {
                    DriverCommand::Request { method, params, reply } => {
                        let id = next_id;
                        next_id += 1;
                        let message =
                            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
                        if let Err(error) = write_message(&mut writer, &message).await {
                            let _ = reply.send(Err(JsonRpcError::Io(error)));
                            break;
                        }
                        pending.insert(id, reply);
                    }
                    DriverCommand::Notification { method, params } => {
                        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
                        if write_message(&mut writer, &message).await.is_err() {
                            break;
                        }
                    }
                }
            }
            inbound = frames.recv() => {
                match inbound {
                    Some(InboundFrame::Frame(frame)) => {
                        if !handle_frame(&frame, &mut pending, &mut writer).await {
                            break;
                        }
                    }
                    Some(InboundFrame::Closed) | None => break,
                    Some(InboundFrame::Failed(error)) => {
                        oversized = matches!(error, JsonRpcError::FrameTooLarge);
                        tracing::debug!(%error, "json-rpc transport closed on a read failure");
                        break;
                    }
                }
            }
        }
    }
    // Whatever the exit reason, no caller may hang on a dead transport.
    for (_, waiter) in pending.drain() {
        let error = if oversized {
            JsonRpcError::FrameTooLarge
        } else {
            JsonRpcError::TransportClosed
        };
        let _ = waiter.send(Err(error));
    }
    reader_task.abort();
    if let Some(mut child) = child.take() {
        kill_child_group(&mut child).await;
    }
}

enum InboundFrame {
    Frame(Vec<u8>),
    Closed,
    Failed(JsonRpcError),
}

async fn pump_frames<R>(reader: R, tx: mpsc::Sender<InboundFrame>)
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(reader);
    loop {
        let mut frame = Vec::new();
        let inbound = match read_frame(&mut reader, &mut frame).await {
            Ok(0) => InboundFrame::Closed,
            Ok(_) => InboundFrame::Frame(frame),
            Err(error) => InboundFrame::Failed(error),
        };
        let terminal = !matches!(inbound, InboundFrame::Frame(_));
        if tx.send(inbound).await.is_err() || terminal {
            return;
        }
    }
}

/// Handle one inbound frame. Returns false when answering a server→client
/// request failed on the wire (the transport is dead; the driver tears down).
async fn handle_frame<W: AsyncWrite + Unpin>(
    frame: &[u8],
    pending: &mut HashMap<u64, oneshot::Sender<Result<Value, JsonRpcError>>>,
    writer: &mut W,
) -> bool {
    let Ok(text) = std::str::from_utf8(frame) else {
        tracing::warn!("json-rpc frame is not valid UTF-8; ignored");
        return true;
    };
    let message: Value = match serde_json::from_str(text.trim_end()) {
        Ok(message) => message,
        Err(error) => {
            tracing::warn!(%error, "json-rpc frame is not valid JSON; ignored");
            return true;
        }
    };
    if let Some(method) = message.get("method").and_then(Value::as_str) {
        if let Some(id) = message.get("id") {
            // A server→client REQUEST (`roots/list`, `sampling/createMessage`,
            // ...): v1 serves none of them, but an unanswered request would
            // hang a server that blocks on it, so answer method-not-found.
            tracing::debug!(method, "json-rpc server request answered method-not-found");
            let reply = json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": METHOD_NOT_FOUND, "message": format!("method not found: {method}") },
            });
            return write_message(writer, &reply).await.is_ok();
        }
        // A server→client notification: logged, ignored in v1.
        tracing::debug!(method, "json-rpc server notification ignored");
        return true;
    }
    if let Some(id) = message.get("id").and_then(Value::as_u64) {
        match pending.remove(&id) {
            Some(waiter) => {
                let _ = waiter.send(response_outcome(&message));
            }
            None => tracing::debug!(
                id,
                "json-rpc response for an unknown id (already timed out?)"
            ),
        }
        return true;
    }
    tracing::warn!("json-rpc frame has neither method nor id; ignored");
    true
}

/// Project a response frame onto the waiter's outcome: `error` becomes
/// [`JsonRpcError::Server`]; a missing `result` becomes `null`.
fn response_outcome(message: &Value) -> Result<Value, JsonRpcError> {
    if let Some(error) = message.get("error") {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
        let detail = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(JsonRpcError::Server {
            code,
            message: detail.to_string(),
        });
    }
    Ok(message.get("result").cloned().unwrap_or(Value::Null))
}

/// Read one newline-delimited frame into `buffer`, bounding its growth at
/// [`MAX_FRAME_BYTES`]. Returns `Ok(0)` on a clean EOF; an EOF mid-frame is a
/// broken transport.
async fn read_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
) -> Result<usize, JsonRpcError> {
    buffer.clear();
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            // EOF: clean only when no partial frame is pending.
            return if buffer.is_empty() {
                Ok(0)
            } else {
                Err(JsonRpcError::TransportClosed)
            };
        }
        match chunk.iter().position(|byte| *byte == b'\n') {
            Some(pos) => {
                buffer.extend_from_slice(&chunk[..pos]);
                reader.consume(pos + 1);
                if buffer.len() > MAX_FRAME_BYTES {
                    return Err(JsonRpcError::FrameTooLarge);
                }
                return Ok(buffer.len());
            }
            None => {
                buffer.extend_from_slice(chunk);
                let len = chunk.len();
                reader.consume(len);
                if buffer.len() > MAX_FRAME_BYTES {
                    return Err(JsonRpcError::FrameTooLarge);
                }
            }
        }
    }
}

/// Write one frame: compact JSON + a newline, flushed.
async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Value,
) -> std::io::Result<()> {
    let mut bytes = serde_json::to_string(message)
        .map_err(std::io::Error::other)?
        .into_bytes();
    bytes.push(b'\n');
    tokio::time::timeout(WRITE_TIMEOUT, async {
        writer.write_all(&bytes).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "json-rpc write timed out"))?
}

async fn kill_child_group(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let kill = ["/bin/kill", "/usr/bin/kill"]
            .into_iter()
            .find(|path| std::fs::metadata(path).is_ok_and(|meta| meta.is_file()));
        if let Some(kill) = kill {
            let _ = Command::new(kill)
                .args(["-KILL", "--", &format!("-{pid}")])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

#[cfg(test)]
mod tests {
    //! End-to-end tests over scripted in-process peers speaking the real
    //! newline-delimited JSON-RPC 2.0 wire over `tokio::io::duplex` (the
    //! `acp_client` test-harness shape).

    use super::*;

    async fn write<W: AsyncWrite + Unpin>(writer: &mut W, message: &Value) {
        write_message(writer, message).await.expect("write");
    }

    async fn read<R: AsyncBufRead + Unpin>(reader: &mut R) -> Option<Value> {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await.ok()?;
        if read == 0 {
            return None;
        }
        serde_json::from_str(line.trim()).ok()
    }

    /// Wire a client to a scripted peer; returns the client and the server task.
    fn connected_pair() -> (JsonRpcClient, DuplexHalfServer) {
        let (client_reads, server_writes) = tokio::io::duplex(8192);
        let (server_reads, client_writes) = tokio::io::duplex(8192);
        (
            JsonRpcClient::connect(client_reads, client_writes),
            DuplexHalfServer {
                reader: BufReader::new(server_reads),
                writer: server_writes,
            },
        )
    }

    struct DuplexHalfServer {
        reader: BufReader<tokio::io::DuplexStream>,
        writer: tokio::io::DuplexStream,
    }

    #[tokio::test]
    async fn call_round_trips_a_result() {
        let (client, mut server) = connected_pair();
        let task = tokio::spawn(async move {
            let request = read(&mut server.reader).await.expect("request");
            assert_eq!(request["jsonrpc"], "2.0");
            assert_eq!(request["method"], "ping");
            assert_eq!(request["params"], json!({ "x": 1 }));
            write(
                &mut server.writer,
                &json!({ "jsonrpc": "2.0", "id": request["id"], "result": { "pong": true } }),
            )
            .await;
        });

        let result = client
            .call("ping", json!({ "x": 1 }), Duration::from_secs(5))
            .await
            .expect("result");
        assert_eq!(result, json!({ "pong": true }));
        task.await.expect("server task");
    }

    #[tokio::test]
    async fn an_error_response_surfaces_code_and_message() {
        let (client, mut server) = connected_pair();
        let task = tokio::spawn(async move {
            let request = read(&mut server.reader).await.expect("request");
            write(
                &mut server.writer,
                &json!({
                    "jsonrpc": "2.0", "id": request["id"],
                    "error": { "code": -32602, "message": "bad params" }
                }),
            )
            .await;
        });

        let error = client
            .call("ping", json!({}), Duration::from_secs(30))
            .await
            .expect_err("server error");
        assert!(matches!(error, JsonRpcError::Server { code: -32602, .. }));
        assert!(error.to_string().contains("bad params"));
        task.await.expect("server task");
    }

    #[tokio::test]
    async fn an_unsolicited_notification_is_tolerated() {
        let (client, mut server) = connected_pair();
        let task = tokio::spawn(async move {
            let request = read(&mut server.reader).await.expect("request");
            // A progress notification arrives BEFORE the response.
            write(
                &mut server.writer,
                &json!({
                    "jsonrpc": "2.0", "method": "notifications/progress",
                    "params": { "progressToken": "t", "progress": 0.5 }
                }),
            )
            .await;
            write(
                &mut server.writer,
                &json!({ "jsonrpc": "2.0", "id": request["id"], "result": null }),
            )
            .await;
        });

        let result = client
            .call("ping", json!({}), Duration::from_secs(5))
            .await
            .expect("notification did not break the call");
        assert_eq!(result, Value::Null);
        task.await.expect("server task");
    }

    #[tokio::test]
    async fn a_server_request_gets_method_not_found() {
        let (client, mut server) = connected_pair();
        let task = tokio::spawn(async move {
            let request = read(&mut server.reader).await.expect("request");
            // The server asks the client for something (e.g. `roots/list`) with
            // a STRING id — echoed back verbatim.
            write(
                &mut server.writer,
                &json!({ "jsonrpc": "2.0", "id": "srv-1", "method": "roots/list" }),
            )
            .await;
            let answer = read(&mut server.reader)
                .await
                .expect("method-not-found answer");
            assert_eq!(answer["id"], "srv-1");
            assert_eq!(answer["error"]["code"], METHOD_NOT_FOUND);
            // The server, unblocked, now answers the client's pending request.
            write(
                &mut server.writer,
                &json!({ "jsonrpc": "2.0", "id": request["id"], "result": { "done": true } }),
            )
            .await;
        });

        let result = client
            .call("ping", json!({}), Duration::from_secs(5))
            .await
            .expect("server request did not hang the call");
        assert_eq!(result, json!({ "done": true }));
        task.await.expect("server task");
    }

    #[tokio::test]
    async fn an_unanswered_request_times_out() {
        let (client, mut server) = connected_pair();
        let task = tokio::spawn(async move {
            let _request = read(&mut server.reader).await;
            // Never respond; keep the transport open.
            std::future::pending::<()>().await;
        });

        let error = client
            .call("slow", json!({}), Duration::from_millis(50))
            .await
            .expect_err("times out");
        assert!(
            matches!(error, JsonRpcError::Timeout { .. }),
            "got: {error}"
        );
        task.abort();
    }

    #[tokio::test]
    async fn a_peer_close_fails_the_pending_waiter() {
        let (client, mut server) = connected_pair();
        let task = tokio::spawn(async move {
            let _request = read(&mut server.reader).await;
            // Dropping both halves without answering = a dead child. The caller
            // must get a legible transport-closed error, not a hang. The
            // explicit `drop` forces the `async move` block to capture ALL of
            // `server` — edition-2021 disjoint capture would otherwise take
            // only `server.reader` and leave the write half alive in the test
            // frame, so the client would never see EOF.
            drop(server);
        });

        let error = client
            .call("ping", json!({}), Duration::from_secs(30))
            .await
            .expect_err("transport closed");
        assert!(
            matches!(error, JsonRpcError::TransportClosed),
            "got: {error}"
        );
        task.await.expect("server task");
    }

    #[tokio::test]
    async fn an_oversized_frame_tears_down_the_transport() {
        let (client_reads, mut server_writes) = tokio::io::duplex(MAX_FRAME_BYTES + 8192);
        let (server_reads, client_writes) = tokio::io::duplex(8192);
        let task = tokio::spawn(async move {
            let mut reader = BufReader::new(server_reads);
            let _request = read(&mut reader).await;
            let junk = vec![b'x'; MAX_FRAME_BYTES + 1];
            server_writes
                .write_all(&junk)
                .await
                .expect("fits the pipe buffer");
            std::future::pending::<()>().await;
        });
        let client = JsonRpcClient::connect(client_reads, client_writes);

        let error = client
            .call("ping", json!({}), Duration::from_secs(30))
            .await
            .expect_err("frame rejected");
        assert!(matches!(error, JsonRpcError::FrameTooLarge), "got: {error}");
        task.abort();
    }

    #[tokio::test]
    async fn notify_sends_a_message_without_an_id() {
        let (client, mut server) = connected_pair();
        let task = tokio::spawn(async move {
            let notification = read(&mut server.reader).await.expect("notification");
            assert_eq!(notification["method"], "notifications/initialized");
            assert!(notification.get("id").is_none());
        });

        client
            .notify("notifications/initialized", json!({}))
            .await
            .expect("sent");
        task.await.expect("server task");
    }
}
