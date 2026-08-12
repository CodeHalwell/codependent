//! codypendent-runtime.
//!
//! Agent runs, the tool layer, the approvals bridge, model integration,
//! context, and compaction. This is the only crate that depends on the
//! `agent-framework-rs` provider crates, and it does so behind provider
//! features (ADR-009: selected crates, never the umbrella `full`).

pub mod agent;
pub mod auth;
pub mod bench;
pub mod blackboard;
// The real embedding model behind `codypendent_knowledge`'s `SemanticEmbedder`
// seam (rubric 9) — the same "knowledge stays model-free, the runtime owns the
// provider call" split as `extractor`, but over a plain `reqwest` call rather
// than a framework `ChatClient`, so it needs no provider feature.
pub mod embedding;
// The model-backed `FactExtractor` (M3b, smarter-memory): needs `futures` to
// drain the streaming `ChatClient` response, which is only pulled in behind
// `provider-openai` (see Cargo.toml).
#[cfg(feature = "provider-openai")]
pub mod extractor;
pub mod models;
pub mod tools;

pub use embedding::HttpEmbedder;

#[cfg(feature = "provider-openai")]
pub use extractor::LlmFactExtractor;
