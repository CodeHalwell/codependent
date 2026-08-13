//! `codypendent-cli` library surface.
//!
//! `src/main.rs` is a thin binary wrapper over this library so that
//! integration tests (`tests/jsonl_it.rs`) can drive the connection/streaming
//! logic directly against a hand-rolled mock server, without spawning the
//! `codypendent` binary itself.

pub mod acp;
pub mod acp_clients;
pub mod client;
pub mod commands;
pub mod connection;
pub mod council;
pub mod doctor;
pub mod eval;
pub mod finetune;
pub mod models_file;
pub mod models_pull;
pub mod restart;
pub mod stream;
pub mod theme_select;
pub mod tui;
pub mod update;
/// The TUI's voice host (voice v1, rubric 8): push-to-talk capture and spoken
/// replies, kept out of `codypendent-tui` so that crate stays free of
/// subprocesses, audio, and network.
pub mod voice;
