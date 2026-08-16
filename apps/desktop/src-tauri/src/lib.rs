//! Codypendent desktop shell.
//!
//! The Rust side owns the `codypendentd` connection (adoption 14 §4.1); the
//! webview is a projection of daemon state and holds no privileged path of its
//! own. Split in two so the part with logic is testable without a window:
//!
//! - [`daemon`] — the socket connection, built on the shared protocol crate
//!   and the same `Connection` the CLI and TUI use. No `tauri` types.
//! - [`bridge`] — the Tauri commands and event channel that expose it.

pub mod bridge;
pub mod daemon;

pub use daemon::{ConnectionInfo, DaemonClient, DaemonFrame, FrameSink, RunHandle, SessionRow};
