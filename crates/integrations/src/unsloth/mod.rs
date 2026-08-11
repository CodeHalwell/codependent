//! Hugging Face Hub discovery for the Unsloth GGUF catalog — local-model
//! browsing for `codypendent models pull` and the TUI's "Local models: browse
//! Unsloth catalog" overlay.
//!
//! - [`HfCatalogApi`] — the transport-agnostic trait the CLI/TUI harness
//!   depends on (mirrors [`crate::search::SearchApi`] / [`crate::github`]'s
//!   trait-plus-`reqwest`-impl shape), so tests substitute a fixture-backed
//!   stub and never touch the network.
//! - [`client`] — the `reqwest`-backed [`client::HfHubClient`], built on the
//!   same bounded pattern as the other integrations clients (30s timeout,
//!   redirects ≤ 5, a byte ceiling on every response body). The Hub's
//!   `api/models` surface is public/keyless, so — unlike
//!   [`crate::search::key`] — there is no key broker here.
//! - [`quant`] — pure grouping of a repo's file tree into named quant
//!   variants (`Q4_K_M`, `UD-Q4_K_XL`, `BF16`, …), including Unsloth's
//!   dynamic-quant naming and multi-part split files. No network, unit-tested
//!   against real recorded shapes.
//!
//! Every returned string (repo ids, quant labels, timestamps) is Hub-supplied
//! display data — bounded by the client's response ceiling, never executed or
//! interpolated into a shell command by this crate (the CLI layer that drives
//! `ollama pull` passes the resolved org/repo/quant as separate `Command`
//! arguments, never through a shell).

pub mod client;
pub mod quant;

pub use client::HfHubClient;
pub use quant::{GgufFile, QuantVariant};

use async_trait::async_trait;

/// The org this integration browses by default (`codypendent models pull
/// <repo>` with no `org/` prefix assumes this).
pub const DEFAULT_UNSLOTH_ORG: &str = "unsloth";

/// The real Hugging Face Hub's base URL. Tests point [`client::HfHubClient`]
/// at a `wiremock` server instead.
pub const HF_HUB_BASE_URL: &str = "https://huggingface.co";

/// One GGUF-tagged repo under an author, as listed by the Hub's `api/models`
/// endpoint — enough to render a browsable row (name, downloads, likes, last
/// updated) without fetching each repo's full file tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HfRepoSummary {
    /// The full repo id, e.g. `unsloth/Qwen3-32B-GGUF`.
    pub id: String,
    pub downloads: u64,
    pub likes: u64,
    /// The repo's last-modified timestamp (ISO 8601 UTC, as the Hub reports
    /// it), when present. `None` on a response shape that omitted it — the
    /// caller renders that as "unknown" rather than fabricating a date.
    pub updated_at: Option<String>,
}

/// Best-effort repo metadata used to derive a registration hint when a model
/// is pulled (context window). Every field is optional: not every GGUF repo
/// carries Hub-parsed `gguf` metadata (e.g. adapters, or a repo the Hub has
/// not run its GGUF parser over yet) — an absent value is omitted at
/// registration rather than guessed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HfRepoMetadata {
    pub context_length: Option<u64>,
}

/// Errors from the Hugging Face Hub discovery client: HTTP transport, non-2xx
/// API responses, response decode failures, and timeouts. The Hub API this
/// module calls is public and keyless, so — unlike
/// [`crate::search::SearchError`] — there is no `MissingKey` variant and
/// nothing here ever carries a secret.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HfError {
    /// The underlying HTTP transport failed (connect, TLS, body read).
    #[error("hugging face hub http transport error: {0}")]
    Http(reqwest::Error),

    /// The Hub API returned a non-2xx status. `message` is a bounded snippet
    /// of the response body (diagnostics only).
    #[error("hugging face hub api error (status {status}) for {url}: {message}")]
    Api {
        status: u16,
        url: String,
        message: String,
    },

    /// The response body was not the expected JSON shape.
    #[error("hugging face hub response decode error: {0}")]
    Decode(#[from] serde_json::Error),

    /// The request exceeded its wall-clock ceiling.
    #[error("hugging face hub request timed out after {seconds}s")]
    Timeout { seconds: u64 },
}

// A `reqwest` error maps to `Timeout` when it is one, and to `Http` otherwise
// — mirrors `crate::search::SearchError`'s identical conversion.
impl From<reqwest::Error> for HfError {
    fn from(error: reqwest::Error) -> Self {
        if error.is_timeout() {
            HfError::Timeout {
                seconds: client::REQUEST_TIMEOUT.as_secs(),
            }
        } else {
            HfError::Http(error)
        }
    }
}

/// The typed Hugging Face Hub discovery surface the CLI/TUI harness depends
/// on, so a test can substitute a fixture-backed stub.
#[async_trait]
pub trait HfCatalogApi: Send + Sync {
    /// List `author`'s GGUF-tagged repos, sorted by downloads descending,
    /// capped at `limit` rows.
    async fn list_gguf_repos(
        &self,
        author: &str,
        limit: u32,
    ) -> Result<Vec<HfRepoSummary>, HfError>;

    /// The quant variants (grouped GGUF files, with sizes) in `repo_id`'s
    /// `main` branch file tree.
    async fn list_quant_variants(&self, repo_id: &str) -> Result<Vec<QuantVariant>, HfError>;

    /// Best-effort metadata for `repo_id`, used to derive `context_tokens` at
    /// registration time.
    async fn repo_metadata(&self, repo_id: &str) -> Result<HfRepoMetadata, HfError>;
}
