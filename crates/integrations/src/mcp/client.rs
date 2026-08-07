//! MCP (Model Context Protocol) semantics on top of [`super::jsonrpc`]: the
//! `initialize` handshake, `tools/list` with cursor pagination, and `tools/call`
//! with content-block flattening.

use std::collections::HashSet;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};

use super::config::McpServerConfig;
use super::jsonrpc::{JsonRpcClient, JsonRpcError};

/// The newest MCP protocol revision this client speaks. The server's answer is
/// accepted as-is (and logged) — version negotiation is the server's choice.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Bound on `initialize` / `notifications/initialized`. A local stdio server
/// answers in milliseconds; 10s absorbs a cold server's first-run setup
/// without letting a hung one stall daemon startup or a run.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on one `tools/list` page. A metadata operation on an already-running
/// server; 10s matches the handshake bound.
const LIST_TOOLS_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on one `tools/call`. MCP tools do real work (API calls, builds,
/// searches), so this is generous — but a hung server must not park a run
/// forever; deeper budgeting belongs to the run/policy layer.
const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_TOOL_PAGES: usize = 100;
const MAX_TOOLS: usize = 10_000;

/// One tool as the server describes it on `tools/list`.
#[derive(Debug, Clone, PartialEq)]
pub struct McpToolDescription {
    /// The tool name (unique within its server).
    pub name: String,
    /// The human/model-facing description; `""` when the server omits it.
    pub description: String,
    /// The JSON Schema for the tool's arguments; `{}` when the server omits it.
    pub input_schema: Value,
}

/// A failure of an MCP server or of one operation on it. Every variant names
/// the server, so a daemon log line or tool error is actionable on its own.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// A server name was used that no `[[server]]` entry declares.
    #[error("mcp server `{0}` is not configured")]
    UnknownServer(String),
    /// The server process could not be spawned.
    #[error("mcp server `{server}` failed to start: {source}")]
    Spawn {
        /// The server that failed to launch.
        server: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The `initialize` / `notifications/initialized` handshake did not
    /// complete.
    #[error("mcp server `{server}` handshake failed: {reason}")]
    Handshake {
        /// The server being handshaked with.
        server: String,
        /// Why the handshake failed.
        reason: String,
    },
    /// The transport closed (the child exited, stdout EOF) mid-operation.
    #[error("mcp server `{server}` closed its transport during {operation}")]
    TransportClosed {
        /// The server whose transport died.
        server: String,
        /// What was in flight when it died.
        operation: &'static str,
    },
    /// The server answered a request with a JSON-RPC error object.
    #[error("mcp server `{server}` returned JSON-RPC error {code}: {message}")]
    Rpc {
        /// The server that answered.
        server: String,
        /// The JSON-RPC error code.
        code: i64,
        /// The server's error message.
        message: String,
    },
    /// The tool call itself failed — the server answered `isError: true`.
    #[error("mcp server `{server}` tool `{tool}` failed: {message}")]
    Tool {
        /// The server hosting the tool.
        server: String,
        /// The tool that failed.
        tool: String,
        /// The tool's own error text (its content blocks, flattened).
        message: String,
    },
    /// No response arrived within the operation's bound.
    #[error("mcp server `{server}` timed out after {timeout:?} during {operation}")]
    Timeout {
        /// The server that did not answer in time.
        server: String,
        /// What was in flight.
        operation: &'static str,
        /// The bound that fired.
        timeout: Duration,
    },
    /// A transport I/O failure.
    #[error("mcp server `{server}` I/O error: {source}")]
    Io {
        /// The server whose transport failed.
        server: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The server returned malformed or unbounded pagination metadata.
    #[error("mcp server `{server}` protocol violation during {operation}: {reason}")]
    Protocol {
        server: String,
        operation: &'static str,
        reason: String,
    },
}

/// Map a transport-layer error onto an [`McpError`] carrying the server name
/// and the operation that was in flight.
fn rpc_error(server: &str, operation: &'static str, error: JsonRpcError) -> McpError {
    match error {
        JsonRpcError::Io(source) => McpError::Io {
            server: server.to_string(),
            source,
        },
        // A broken frame tears the transport down, so it surfaces the same way.
        JsonRpcError::TransportClosed | JsonRpcError::FrameTooLarge => McpError::TransportClosed {
            server: server.to_string(),
            operation,
        },
        JsonRpcError::Server { code, message } => McpError::Rpc {
            server: server.to_string(),
            code,
            message,
        },
        JsonRpcError::Timeout { timeout, .. } => McpError::Timeout {
            server: server.to_string(),
            operation,
            timeout,
        },
    }
}

/// A handshaked connection to one MCP server. Cheap to clone (clones share the
/// connection); the registry holds one per ready server.
#[derive(Clone)]
pub struct McpClient {
    rpc: JsonRpcClient,
    /// The configured server name, attached to every error and log line.
    server: String,
}

impl McpClient {
    /// Connect over an existing byte transport (`reader` = the server's output,
    /// `writer` = the server's input) and complete the MCP handshake. Generic
    /// over the stream halves so a scripted `tokio::io::duplex` peer drives the
    /// whole client in tests; [`spawn`](Self::spawn) is the production entry
    /// point.
    pub async fn connect<R, W>(reader: R, writer: W, server_name: &str) -> Result<Self, McpError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let client = Self {
            rpc: JsonRpcClient::connect(reader, writer),
            server: server_name.to_string(),
        };
        client.handshake().await?;
        Ok(client)
    }

    /// Spawn the configured server as a child process and complete the
    /// handshake over its stdio.
    pub async fn spawn(config: &McpServerConfig) -> Result<Self, McpError> {
        let rpc = JsonRpcClient::spawn(
            &config.command,
            &config.args,
            &config.env,
            config.inherit_environment,
            None,
        )
        .map_err(|error| match error {
            JsonRpcError::Io(source) => McpError::Spawn {
                server: config.name.clone(),
                source,
            },
            other => McpError::Handshake {
                server: config.name.clone(),
                reason: other.to_string(),
            },
        })?;
        let client = Self {
            rpc,
            server: config.name.clone(),
        };
        client.handshake().await?;
        Ok(client)
    }

    /// The MCP handshake: offer the newest protocol revision we speak, accept
    /// whatever the server answers (logging it), then send `initialized`.
    async fn handshake(&self) -> Result<(), McpError> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "codypendent", "version": env!("CARGO_PKG_VERSION") },
        });
        let handshake_error = |error: JsonRpcError| McpError::Handshake {
            server: self.server.clone(),
            reason: error.to_string(),
        };
        let result = self
            .rpc
            .call("initialize", params, HANDSHAKE_TIMEOUT)
            .await
            .map_err(handshake_error)?;
        let negotiated = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        tracing::debug!(
            server = %self.server,
            protocol = negotiated,
            "mcp server initialized"
        );
        self.rpc
            .notify("notifications/initialized", json!({}))
            .await
            .map_err(handshake_error)?;
        Ok(())
    }

    /// List every tool the server offers, following `nextCursor` pagination
    /// until absent. Entries without a string `name` are skipped with a
    /// warning (a malformed entry must not poison the whole list).
    pub async fn list_tools(&self) -> Result<Vec<McpToolDescription>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen = HashSet::new();
        for page in 0..MAX_TOOL_PAGES {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let result = self
                .rpc
                .call("tools/list", params, LIST_TOOLS_TIMEOUT)
                .await
                .map_err(|error| rpc_error(&self.server, "tools/list", error))?;
            if let Some(entries) = result.get("tools").and_then(Value::as_array) {
                for entry in entries {
                    let Some(name) = entry.get("name").and_then(Value::as_str) else {
                        tracing::warn!(
                            server = %self.server,
                            "mcp tools/list entry without a string name; skipped"
                        );
                        continue;
                    };
                    tools.push(McpToolDescription {
                        name: name.to_string(),
                        description: entry
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        input_schema: entry
                            .get("inputSchema")
                            .filter(|schema| schema.is_object())
                            .cloned()
                            .unwrap_or_else(|| json!({})),
                    });
                    if tools.len() > MAX_TOOLS {
                        return Err(McpError::Protocol {
                            server: self.server.clone(),
                            operation: "tools/list",
                            reason: format!("tool count exceeded {MAX_TOOLS}"),
                        });
                    }
                }
            }
            match result.get("nextCursor").and_then(Value::as_str) {
                Some("") => {
                    return Err(McpError::Protocol {
                        server: self.server.clone(),
                        operation: "tools/list",
                        reason: "empty nextCursor".into(),
                    })
                }
                Some(next) if !seen.insert(next.to_string()) => {
                    return Err(McpError::Protocol {
                        server: self.server.clone(),
                        operation: "tools/list",
                        reason: format!("repeated nextCursor `{next}`"),
                    })
                }
                Some(next) => cursor = Some(next.to_string()),
                None => return Ok(tools),
            }
            if page + 1 == MAX_TOOL_PAGES {
                return Err(McpError::Protocol {
                    server: self.server.clone(),
                    operation: "tools/list",
                    reason: format!("pagination exceeded {MAX_TOOL_PAGES} pages"),
                });
            }
        }
        unreachable!("bounded pagination returns from every terminal branch")
    }

    /// Call `tool` with `arguments`, returning the result's content blocks
    /// flattened to one string. `isError: true` in the result maps to
    /// [`McpError::Tool`].
    pub async fn call_tool(&self, tool: &str, args: Value) -> Result<String, McpError> {
        let result = self
            .rpc
            .call(
                "tools/call",
                json!({ "name": tool, "arguments": args }),
                TOOL_CALL_TIMEOUT,
            )
            .await
            .map_err(|error| rpc_error(&self.server, "tools/call", error))?;
        let text = flatten_content(&result);
        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(McpError::Tool {
                server: self.server.clone(),
                tool: tool.to_string(),
                message: text,
            });
        }
        Ok(text)
    }
}

/// Flatten a `tools/call` result's `content[]` into one string: text blocks
/// are concatenated (newline-joined); every other block type (image, audio,
/// resource, ...) contributes a placeholder line so its presence is visible
/// without inventing content. `structuredContent` is ignored in v1 — the
/// runtime renders evidence from the text channel.
fn flatten_content(result: &Value) -> String {
    let Some(blocks) = result.get("content").and_then(Value::as_array) else {
        return String::new();
    };
    let mut lines = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    lines.push(text.to_string());
                }
            }
            other => lines.push(format!("[{} content omitted]", other.unwrap_or("unknown"))),
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    //! End-to-end tests over scripted in-process MCP *servers* speaking the
    //! real ndjson wire over `tokio::io::duplex`.

    use super::*;
    use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};

    async fn write<W: AsyncWrite + Unpin>(writer: &mut W, message: &Value) {
        let mut line = serde_json::to_string(message).expect("serialize");
        line.push('\n');
        writer.write_all(line.as_bytes()).await.expect("write");
        writer.flush().await.expect("flush");
    }

    async fn read<R: AsyncBufRead + Unpin>(reader: &mut R) -> Option<Value> {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await.ok()?;
        if read == 0 {
            return None;
        }
        serde_json::from_str(line.trim()).ok()
    }

    /// The scripted-server half of a duplex pair.
    struct ServerHalf {
        reader: BufReader<tokio::io::DuplexStream>,
        writer: tokio::io::DuplexStream,
    }

    fn connected_pair() -> (tokio::io::DuplexStream, tokio::io::DuplexStream, ServerHalf) {
        let (client_reads, server_writes) = tokio::io::duplex(8192);
        let (server_reads, client_writes) = tokio::io::duplex(8192);
        (
            client_reads,
            client_writes,
            ServerHalf {
                reader: BufReader::new(server_reads),
                writer: server_writes,
            },
        )
    }

    /// Answer one `initialize`, then assert the `notifications/initialized`.
    async fn serve_handshake(server: &mut ServerHalf) {
        let initialize = read(&mut server.reader).await.expect("initialize");
        assert_eq!(initialize["method"], "initialize");
        assert_eq!(initialize["params"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(initialize["params"]["clientInfo"]["name"], "codypendent");
        write(
            &mut server.writer,
            &json!({
                "jsonrpc": "2.0", "id": initialize["id"],
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "serverInfo": { "name": "fake", "version": "0" }
                }
            }),
        )
        .await;
        let initialized = read(&mut server.reader).await.expect("initialized");
        assert_eq!(initialized["method"], "notifications/initialized");
        assert!(initialized.get("id").is_none());
    }

    #[tokio::test]
    async fn connect_completes_the_handshake_and_accepts_the_servers_version() {
        let (client_reads, client_writes, mut server) = connected_pair();
        let task = tokio::spawn(async move {
            serve_handshake(&mut server).await;
        });
        // The scripted server answers an OLDER revision; the client accepts it.
        McpClient::connect(client_reads, client_writes, "fake")
            .await
            .expect("handshake completes");
        task.await.expect("server task");
    }

    #[tokio::test]
    async fn list_tools_follows_the_cursor_and_defaults_missing_fields() {
        let (client_reads, client_writes, mut server) = connected_pair();
        let task = tokio::spawn(async move {
            serve_handshake(&mut server).await;
            let page_one = read(&mut server.reader).await.expect("tools/list page 1");
            assert_eq!(page_one["method"], "tools/list");
            assert!(page_one["params"].get("cursor").is_none());
            write(
                &mut server.writer,
                &json!({
                    "jsonrpc": "2.0", "id": page_one["id"],
                    "result": {
                        "tools": [
                            { "name": "search", "description": "search things",
                              "inputSchema": { "type": "object" } }
                        ],
                        "nextCursor": "page-2"
                    }
                }),
            )
            .await;
            let page_two = read(&mut server.reader).await.expect("tools/list page 2");
            assert_eq!(page_two["params"]["cursor"], "page-2");
            write(
                &mut server.writer,
                &json!({
                    "jsonrpc": "2.0", "id": page_two["id"],
                    "result": {
                        "tools": [
                            // No description, no inputSchema: both default.
                            { "name": "bare" },
                            { "name": "fetch", "description": "", "inputSchema": {} }
                        ]
                    }
                }),
            )
            .await;
        });

        let client = McpClient::connect(client_reads, client_writes, "fake")
            .await
            .expect("handshake");
        let tools = client.list_tools().await.expect("tools listed");
        assert_eq!(
            tools,
            vec![
                McpToolDescription {
                    name: "search".to_string(),
                    description: "search things".to_string(),
                    input_schema: json!({ "type": "object" }),
                },
                McpToolDescription {
                    name: "bare".to_string(),
                    description: String::new(),
                    input_schema: json!({}),
                },
                McpToolDescription {
                    name: "fetch".to_string(),
                    description: String::new(),
                    input_schema: json!({}),
                },
            ]
        );
        task.await.expect("server task");
    }

    #[tokio::test]
    async fn call_tool_concatenates_text_and_marks_non_text_blocks() {
        let (client_reads, client_writes, mut server) = connected_pair();
        let task = tokio::spawn(async move {
            serve_handshake(&mut server).await;
            let call = read(&mut server.reader).await.expect("tools/call");
            assert_eq!(call["method"], "tools/call");
            assert_eq!(call["params"]["name"], "search");
            assert_eq!(call["params"]["arguments"], json!({ "q": "rust" }));
            write(
                &mut server.writer,
                &json!({
                    "jsonrpc": "2.0", "id": call["id"],
                    "result": {
                        "content": [
                            { "type": "text", "text": "line one" },
                            { "type": "image", "data": "…", "mimeType": "image/png" },
                            { "type": "text", "text": "line two" }
                        ]
                    }
                }),
            )
            .await;
        });

        let client = McpClient::connect(client_reads, client_writes, "fake")
            .await
            .expect("handshake");
        let text = client
            .call_tool("search", json!({ "q": "rust" }))
            .await
            .expect("call succeeds");
        assert_eq!(text, "line one\n[image content omitted]\nline two");
        task.await.expect("server task");
    }

    #[tokio::test]
    async fn call_tool_is_error_maps_to_a_tool_error() {
        let (client_reads, client_writes, mut server) = connected_pair();
        let task = tokio::spawn(async move {
            serve_handshake(&mut server).await;
            let call = read(&mut server.reader).await.expect("tools/call");
            write(
                &mut server.writer,
                &json!({
                    "jsonrpc": "2.0", "id": call["id"],
                    "result": {
                        "isError": true,
                        "content": [ { "type": "text", "text": "boom" } ]
                    }
                }),
            )
            .await;
        });

        let client = McpClient::connect(client_reads, client_writes, "fake")
            .await
            .expect("handshake");
        let error = client
            .call_tool("explode", json!({}))
            .await
            .expect_err("tool error");
        assert!(
            matches!(
                error,
                McpError::Tool {
                    ref tool,
                    ref message,
                    ..
                } if tool == "explode" && message == "boom"
            ),
            "got: {error}"
        );
        assert!(error.to_string().contains("fake"), "names the server");
        task.await.expect("server task");
    }

    #[tokio::test]
    async fn call_tool_surfaces_a_json_rpc_error_with_code_and_message() {
        let (client_reads, client_writes, mut server) = connected_pair();
        let task = tokio::spawn(async move {
            serve_handshake(&mut server).await;
            let call = read(&mut server.reader).await.expect("tools/call");
            write(
                &mut server.writer,
                &json!({
                    "jsonrpc": "2.0", "id": call["id"],
                    "error": { "code": -32000, "message": "server exploded" }
                }),
            )
            .await;
        });

        let client = McpClient::connect(client_reads, client_writes, "fake")
            .await
            .expect("handshake");
        let error = client
            .call_tool("search", json!({}))
            .await
            .expect_err("rpc error");
        assert!(
            matches!(error, McpError::Rpc { code: -32000, .. }),
            "got: {error}"
        );
        let message = error.to_string();
        assert!(message.contains("server exploded"), "got: {message}");
        assert!(message.contains("fake"), "names the server");
        task.await.expect("server task");
    }
}
