//! A hand-rolled MCP (Model Context Protocol) *client* for stdio servers —
//! modeled on `crate::acp_client` (transport-generic `connect`, child-process
//! `spawn`, an mpsc + driver-task bridge, ndjson framing) but with the tiny
//! JSON-RPC 2.0 layer written by hand so no new external dependency enters the
//! workspace.
//!
//! Trust posture: every configured server is an operator-declared trusted
//! LAUNCH — the daemon spawns exactly the `[[server]]` lines of `mcp.toml`,
//! and a server absent from that file is unreachable. But everything a server
//! RETURNS (tool descriptions, call results) is untrusted evidence: this layer
//! returns raw text verbatim and never executes, interpolates, or acts on it.
//! Sanitization (`[untrusted output from mcp:<server>]` framing, byte caps)
//! happens in the runtime, at the one chokepoint every tool result flows
//! through.
//!
//! Layout: `config` loads `mcp.toml`; `jsonrpc` is the transport plumbing;
//! `client` adds MCP semantics (`initialize`, `tools/list`, `tools/call`);
//! `registry` owns the per-server lifecycle and exposes [`McpBridge`] — the
//! only surface the runtime consumes, via `Arc<dyn McpBridge>` (the
//! `GitHubApi` precedent).

mod client;
mod config;
mod jsonrpc;
mod registry;

pub use client::{McpClient, McpError, McpToolDescription};
pub use config::{load_mcp_config, McpConfig, McpConfigError, McpServerConfig};
pub use registry::{McpBridge, McpConnector, McpRegistry, McpToolInfo};
