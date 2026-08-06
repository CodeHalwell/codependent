//! The registry: per-server lazy spawn, cached tool lists, and the single
//! fail-closed transport reset policy — behind the [`McpBridge`] trait the
//! runtime consumes (the `GitHubApi` precedent).

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;

use super::client::{McpClient, McpError};
use super::config::{McpConfig, McpServerConfig};

/// One offered tool, projected for the runtime's advertisement and dispatch.
#[derive(Debug, Clone, PartialEq)]
pub struct McpToolInfo {
    /// The server the tool lives on (the `mcp.<server>.<tool>` name parts).
    pub server: String,
    /// The tool name within its server.
    pub name: String,
    /// Server-supplied description (`""` when absent).
    pub description: String,
    /// Server-supplied JSON Schema for the arguments, verbatim.
    pub input_schema: Value,
}

/// The MCP surface the runtime depends on — only this trait, via
/// `Arc<dyn McpBridge>`, so the runtime stays testable with a stub.
#[async_trait]
pub trait McpBridge: Send + Sync {
    /// Currently-known tools from warm servers (sync, from cache — never blocks).
    fn offered_tools(&self) -> Vec<McpToolInfo>;
    /// Invoke `tool` on `server` with JSON `args`; returns the concatenated
    /// text content.
    async fn call_tool(&self, server: &str, tool: &str, args: Value) -> Result<String, McpError>;
}

/// How a server gets connected. Production spawns a child process per
/// [`McpServerConfig`]; tests inject scripted in-memory peers. Takes the config
/// by value so the returned future is `'static`.
pub type McpConnector = Arc<
    dyn Fn(McpServerConfig) -> Pin<Box<dyn Future<Output = Result<McpClient, McpError>> + Send>>
        + Send
        + Sync,
>;

/// Where one server is in its lifecycle. `Failed` records the last error for
/// logs but does NOT latch — the next access retries the spawn.
enum ServerState {
    /// Never started (or reset after a dead transport).
    Cold,
    /// Connected, initialized, tools listed.
    Ready(McpClient),
    /// The last spawn/handshake/list failed with this message.
    Failed(String),
}

/// One server's config plus its lifecycle state, behind its own lock.
struct ServerEntry {
    config: McpServerConfig,
    state: ServerState,
}

/// The registry built from an [`McpConfig`]. Per-server state lives behind its
/// own `tokio::sync::Mutex` so warming one server never blocks another; the
/// *offered-tools* cache is a separate std mutex so
/// [`McpBridge::offered_tools`] stays sync and never blocks.
pub struct McpRegistry {
    servers: BTreeMap<String, Arc<Mutex<ServerEntry>>>,
    connector: McpConnector,
    /// server name → its tools; mirrors `ServerState::Ready` entries. Inserted
    /// on a successful start, removed on a failed one.
    tool_cache: StdMutex<BTreeMap<String, Vec<McpToolInfo>>>,
}

impl McpRegistry {
    /// A registry that spawns each server per its config.
    #[must_use]
    pub fn new(config: McpConfig) -> Self {
        let connector: McpConnector = Arc::new(
            |config: McpServerConfig| -> Pin<Box<dyn Future<Output = Result<McpClient, McpError>> + Send>> {
                Box::pin(async move { McpClient::spawn(&config).await })
            },
        );
        Self::with_connector(config, connector)
    }

    /// A registry with an injected connector — the test seam (scripted
    /// `tokio::io::duplex` peers) and the hook for any future non-stdio
    /// transport.
    #[must_use]
    pub fn with_connector(config: McpConfig, connector: McpConnector) -> Self {
        let servers = config
            .servers
            .into_iter()
            .map(|config| {
                (
                    config.name.clone(),
                    Arc::new(Mutex::new(ServerEntry {
                        config,
                        state: ServerState::Cold,
                    })),
                )
            })
            .collect();
        Self {
            servers,
            connector,
            tool_cache: StdMutex::new(BTreeMap::new()),
        }
    }

    /// Spawn + initialize + list every configured server, logging failures
    /// non-fatally (a server that won't start simply offers no tools). The
    /// daemon calls this as a fire-and-forget background task; the lazy path
    /// in [`McpBridge::call_tool`] shares the same per-server warm logic
    /// ([`ensure_ready`](Self::ensure_ready)).
    pub async fn warm_all(&self) {
        futures::future::join_all(self.servers.keys().map(|server| async move {
            if let Err(error) = self.ensure_ready(server).await {
                tracing::warn!(
                    server = %server,
                    %error,
                    "mcp server failed to warm; its tools stay unavailable"
                );
            }
        }))
        .await;
    }

    /// Ensure `server` is ready — spawning, handshaking, and listing tools on
    /// first use or after a failure — and return its cached tools.
    async fn ensure_ready(&self, server: &str) -> Result<Vec<McpToolInfo>, McpError> {
        let entry = self
            .servers
            .get(server)
            .ok_or_else(|| McpError::UnknownServer(server.to_string()))?;
        let mut guard = entry.lock().await;
        if matches!(guard.state, ServerState::Ready(_)) {
            return Ok(self.cached_tools(server));
        }
        if let ServerState::Failed(previous) = &guard.state {
            tracing::debug!(
                server = %server,
                previous = %previous,
                "mcp server failed before; retrying the spawn"
            );
        }
        match self.start(server, &guard.config).await {
            Ok((client, tools)) => {
                guard.state = ServerState::Ready(client);
                self.tool_cache
                    .lock()
                    .expect("mcp tool-cache mutex poisoned")
                    .insert(server.to_string(), tools.clone());
                Ok(tools)
            }
            Err(error) => {
                guard.state = ServerState::Failed(error.to_string());
                self.tool_cache
                    .lock()
                    .expect("mcp tool-cache mutex poisoned")
                    .remove(server);
                Err(error)
            }
        }
    }

    /// Connect, then list and project the tools. The per-server lock is held
    /// by the caller, so a slow start serializes only this server.
    async fn start(
        &self,
        server: &str,
        config: &McpServerConfig,
    ) -> Result<(McpClient, Vec<McpToolInfo>), McpError> {
        let client = (self.connector)(config.clone()).await?;
        let tools = client.list_tools().await?;
        let infos = tools
            .into_iter()
            .map(|tool| McpToolInfo {
                server: server.to_string(),
                name: tool.name,
                description: tool.description,
                input_schema: tool.input_schema,
            })
            .collect();
        Ok((client, infos))
    }

    fn cached_tools(&self, server: &str) -> Vec<McpToolInfo> {
        self.tool_cache
            .lock()
            .expect("mcp tool-cache mutex poisoned")
            .get(server)
            .cloned()
            .unwrap_or_default()
    }

    /// Reset a server to cold, dropping its client (which closes the command
    /// channel; the driver exits and kills the child). Cached tools stay
    /// advertised until the next start attempt refreshes or removes them — a
    /// brief staleness window that beats flickering the catalog.
    async fn mark_cold(&self, server: &str) {
        if let Some(entry) = self.servers.get(server) {
            entry.lock().await.state = ServerState::Cold;
        }
    }

    /// One `tools/call` attempt: ensure-ready, then invoke. The client handle
    /// is cloned out of the lock so a 60s tool call never holds it.
    async fn call_once(&self, server: &str, tool: &str, args: &Value) -> Result<String, McpError> {
        self.ensure_ready(server).await?;
        let entry = self
            .servers
            .get(server)
            .ok_or_else(|| McpError::UnknownServer(server.to_string()))?;
        let client = {
            let guard = entry.lock().await;
            match &guard.state {
                ServerState::Ready(client) => client.clone(),
                // Lost a race with a concurrent reset; treat it as a dead
                // transport so the caller's single-retry policy applies.
                _ => {
                    return Err(McpError::TransportClosed {
                        server: server.to_string(),
                        operation: "tool dispatch",
                    });
                }
            }
        };
        client.call_tool(tool, args.clone()).await
    }
}

#[async_trait]
impl McpBridge for McpRegistry {
    fn offered_tools(&self) -> Vec<McpToolInfo> {
        self.tool_cache
            .lock()
            .expect("mcp tool-cache mutex poisoned")
            .values()
            .flat_map(|tools| tools.iter().cloned())
            .collect()
    }

    async fn call_tool(&self, server: &str, tool: &str, args: Value) -> Result<String, McpError> {
        match self.call_once(server, tool, &args).await {
            Err(error) if invalidates_transport(&error) => {
                tracing::warn!(
                    server = %server,
                    tool = %tool,
                    %error,
                    "mcp transport became unreliable; resetting without retrying an ambiguous effect"
                );
                self.mark_cold(server).await;
                Err(error)
            }
            outcome => outcome,
        }
    }
}

/// EOF, write failure, or timeout invalidates the cached transport. The
/// current tool call is not replayed because the remote effect is ambiguous;
/// the next access starts a fresh process. JSON-RPC and tool errors prove the
/// server answered and therefore keep the transport warm.
fn invalidates_transport(error: &McpError) -> bool {
    matches!(
        error,
        McpError::TransportClosed { .. } | McpError::Io { .. } | McpError::Timeout { .. }
    )
}

#[cfg(test)]
mod tests {
    //! Registry lifecycle tests over scripted `tokio::io::duplex` MCP servers,
    //! injected through the [`McpConnector`] seam (no real child processes).

    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{
        AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader,
    };

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

    /// What the scripted server does on `tools/call`.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum CallAction {
        /// Answer with one text block.
        Respond,
        /// Close the transport without answering (a dead child).
        HangUp,
    }

    /// A scripted MCP server: handshake, one-tool `tools/list`, then
    /// `tools/call` per `call`.
    async fn scripted_mcp_server<R, W>(reader: R, mut writer: W, call: CallAction)
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut reader = BufReader::new(reader);
        while let Some(message) = read(&mut reader).await {
            let method = message["method"].as_str().unwrap_or_default();
            let id = message.get("id").cloned();
            match method {
                "initialize" => {
                    write(
                        &mut writer,
                        &json!({
                            "jsonrpc": "2.0", "id": id,
                            "result": {
                                "protocolVersion": "2025-06-18",
                                "capabilities": {},
                                "serverInfo": { "name": "fake", "version": "0" }
                            }
                        }),
                    )
                    .await;
                }
                "tools/list" => {
                    write(
                        &mut writer,
                        &json!({
                            "jsonrpc": "2.0", "id": id,
                            "result": { "tools": [ {
                                "name": "search",
                                "description": "search things",
                                "inputSchema": { "type": "object" }
                            } ] }
                        }),
                    )
                    .await;
                }
                "tools/call" => match call {
                    CallAction::Respond => {
                        write(
                            &mut writer,
                            &json!({
                                "jsonrpc": "2.0", "id": id,
                                "result": { "content": [ { "type": "text", "text": "ok from tool" } ] }
                            }),
                        )
                        .await;
                    }
                    // Returning drops both halves: the client sees EOF.
                    CallAction::HangUp => return,
                },
                // `notifications/initialized` and anything else need no answer.
                _ => {}
            }
        }
    }

    /// Per-registry scripted behavior across spawns.
    #[derive(Clone, Copy)]
    enum Behavior {
        /// Every spawn serves calls normally.
        Respond,
        /// Every spawn hangs up on `tools/call`.
        HangUpOnCall,
        /// The FIRST spawn hangs up on `tools/call`; later spawns respond.
        HangUpOnFirstSpawnOnly,
        /// The FIRST spawn fails before connecting; later spawns respond.
        FailFirstSpawn,
    }

    /// A one-server registry whose "spawn" runs a scripted server over duplex
    /// pipes. `spawns` counts connector invocations.
    fn scripted_registry(behavior: Behavior, spawns: Arc<AtomicUsize>) -> McpRegistry {
        let config = McpConfig {
            servers: vec![McpServerConfig {
                name: "fake".to_string(),
                command: "unused-in-tests".to_string(),
                args: Vec::new(),
                env: Vec::new(),
                inherit_environment: true,
            }],
        };
        let connector: McpConnector = Arc::new(
            move |server: McpServerConfig| -> Pin<Box<dyn Future<Output = Result<McpClient, McpError>> + Send>> {
                let spawns = Arc::clone(&spawns);
                Box::pin(async move {
                    let spawn_number = spawns.fetch_add(1, Ordering::SeqCst) + 1;
                    if matches!(behavior, Behavior::FailFirstSpawn) && spawn_number == 1 {
                        return Err(McpError::Handshake {
                            server: server.name.clone(),
                            reason: "scripted spawn failure".to_string(),
                        });
                    }
                    let call = match behavior {
                        Behavior::Respond | Behavior::FailFirstSpawn => CallAction::Respond,
                        Behavior::HangUpOnCall => CallAction::HangUp,
                        Behavior::HangUpOnFirstSpawnOnly if spawn_number == 1 => CallAction::HangUp,
                        Behavior::HangUpOnFirstSpawnOnly => CallAction::Respond,
                    };
                    let (client_reads, server_writes) = tokio::io::duplex(8192);
                    let (server_reads, client_writes) = tokio::io::duplex(8192);
                    tokio::spawn(scripted_mcp_server(server_reads, server_writes, call));
                    McpClient::connect(client_reads, client_writes, &server.name).await
                })
            },
        );
        McpRegistry::with_connector(config, connector)
    }

    #[tokio::test]
    async fn offered_tools_are_empty_until_a_server_is_warm() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let registry = scripted_registry(Behavior::Respond, Arc::clone(&spawns));
        assert!(
            registry.offered_tools().is_empty(),
            "a cold server offers nothing"
        );

        registry.warm_all().await;
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        assert_eq!(
            registry.offered_tools(),
            vec![McpToolInfo {
                server: "fake".to_string(),
                name: "search".to_string(),
                description: "search things".to_string(),
                input_schema: json!({ "type": "object" }),
            }]
        );
    }

    #[tokio::test]
    async fn call_tool_lazily_spawns_and_then_reuses_the_server() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let registry = scripted_registry(Behavior::Respond, Arc::clone(&spawns));

        let text = registry
            .call_tool("fake", "search", json!({ "q": "x" }))
            .await
            .expect("lazy spawn + call");
        assert_eq!(text, "ok from tool");
        assert_eq!(spawns.load(Ordering::SeqCst), 1);

        let text = registry
            .call_tool("fake", "search", json!({ "q": "y" }))
            .await
            .expect("cached server");
        assert_eq!(text, "ok from tool");
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            1,
            "the warm server is reused"
        );
    }

    #[tokio::test]
    async fn a_dead_transport_resets_and_the_next_call_respawns() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let registry = scripted_registry(Behavior::HangUpOnFirstSpawnOnly, Arc::clone(&spawns));

        let error = registry
            .call_tool("fake", "search", json!({}))
            .await
            .expect_err("an ambiguous call is never replayed automatically");
        assert!(matches!(error, McpError::TransportClosed { .. }));
        assert_eq!(spawns.load(Ordering::SeqCst), 1);

        let text = registry
            .call_tool("fake", "search", json!({}))
            .await
            .expect("the next independent call uses a fresh transport");
        assert_eq!(text, "ok from tool");
        assert_eq!(spawns.load(Ordering::SeqCst), 2, "spawned exactly twice");
    }

    #[tokio::test]
    async fn repeated_dead_transports_are_reset_without_replaying_calls() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let registry = scripted_registry(Behavior::HangUpOnCall, Arc::clone(&spawns));

        let error = registry
            .call_tool("fake", "search", json!({}))
            .await
            .expect_err("both transports died");
        assert!(
            matches!(error, McpError::TransportClosed { .. }),
            "got: {error}"
        );
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            1,
            "the ambiguous call is not retried"
        );
        assert!(error.to_string().contains("fake"), "names the server");

        let second = registry
            .call_tool("fake", "search", json!({}))
            .await
            .expect_err("a fresh process that also dies is surfaced again");
        assert!(matches!(second, McpError::TransportClosed { .. }));
        assert_eq!(spawns.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_failed_spawn_is_retried_on_the_next_access() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let registry = scripted_registry(Behavior::FailFirstSpawn, Arc::clone(&spawns));

        let error = registry
            .call_tool("fake", "search", json!({}))
            .await
            .expect_err("first spawn fails");
        assert!(matches!(error, McpError::Handshake { .. }), "got: {error}");
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        assert!(
            registry.offered_tools().is_empty(),
            "a failed server offers nothing"
        );

        let text = registry
            .call_tool("fake", "search", json!({}))
            .await
            .expect("the failure did not latch; retry succeeds");
        assert_eq!(text, "ok from tool");
        assert_eq!(spawns.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn an_unknown_server_is_an_error_not_a_spawn() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let registry = scripted_registry(Behavior::Respond, Arc::clone(&spawns));

        let error = registry
            .call_tool("nope", "search", json!({}))
            .await
            .expect_err("unknown server");
        assert!(
            matches!(error, McpError::UnknownServer(ref name) if name == "nope"),
            "got: {error}"
        );
        assert_eq!(spawns.load(Ordering::SeqCst), 0, "nothing was spawned");
    }
}
