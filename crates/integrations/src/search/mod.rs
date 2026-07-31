//! Web search (PR C1 — agent capabilities).
//!
//! A typed, minimal web-search surface backed by Tavily, structured exactly
//! like the [`crate::github`] module:
//!
//! - [`SearchApi`] — the transport-agnostic trait the tool layer calls. The
//!   daemon and runtime depend on the trait, never the concrete client, so
//!   tests can substitute a stub and a second backend can slot in later.
//! - [`key`] — the key broker: the Tavily API key is read from
//!   `TAVILY_API_KEY` **by name**, held in an opaque [`TavilyKey`] that never
//!   leaks its value into `Debug`, logs, or any serializable type.
//! - [`client`] — the `reqwest`-backed [`TavilyClient`], built on the same
//!   bounded pattern as the GitHub client (30s timeout, redirects ≤ 5, body
//!   ceilings, a sensitive bearer header).
//!
//! Everything the endpoint returns is maximally untrusted web content; the
//! runtime sanitizes it as evidence before it enters the model's observation
//! stream (that chokepoint lives in the runtime, not here).

pub mod client;
pub mod key;

pub use client::TavilyClient;
pub use key::{TavilyKey, TAVILY_AUTH_ID};

use async_trait::async_trait;

/// One titled source a search returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    /// The page title (empty when the backend supplied none).
    pub title: String,
    /// The source URL.
    pub url: String,
    /// The extracted content snippet.
    pub content: String,
}

/// What a search call returns: an optional synthesized answer plus the titled
/// sources behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOutcome {
    /// The backend's synthesized answer, when one was requested and provided.
    pub answer: Option<String>,
    /// The titled sources, most relevant first.
    pub results: Vec<SearchResult>,
}

/// Errors from the search client: HTTP transport, non-2xx API responses,
/// response decode failures, timeouts, and a missing key. No variant's
/// `Display` EVER includes the key — the bearer value rides only in the
/// request header (marked sensitive), never in a diagnostic.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SearchError {
    /// The underlying HTTP transport failed (connect, TLS, body read).
    /// `reqwest`'s message names the URL, never the `Authorization` header.
    #[error("search http transport error: {0}")]
    Http(reqwest::Error),

    /// The search API returned a non-2xx status. The `message` is a bounded
    /// snippet of the response body, which never contains the key.
    #[error("search api error (status {status}): {message}")]
    Api {
        /// The HTTP status code.
        status: u16,
        /// The response body snippet, used for diagnostics.
        message: String,
    },

    /// The response body was not the expected JSON shape.
    #[error("search response decode error: {0}")]
    Decode(#[from] serde_json::Error),

    /// The request exceeded its wall-clock ceiling — a stalled peer must fail
    /// the call, not hang it indefinitely.
    #[error("search request timed out after {seconds}s")]
    Timeout {
        /// The ceiling that expired.
        seconds: u64,
    },

    /// No usable API key could be found. The payload names the env var that
    /// was tried — never a key value.
    #[error("missing tavily api key: {0}")]
    MissingKey(String),
}

// A `reqwest` error maps to `Timeout` when it is one (the caller deserves the
// dedicated variant the design names), and to `Http` otherwise.
impl From<reqwest::Error> for SearchError {
    fn from(error: reqwest::Error) -> Self {
        if error.is_timeout() {
            SearchError::Timeout {
                seconds: client::REQUEST_TIMEOUT.as_secs(),
            }
        } else {
            SearchError::Http(error)
        }
    }
}

/// The typed web-search surface the tool layer depends on.
#[async_trait]
pub trait SearchApi: Send + Sync {
    /// Search the web for `query`, returning at most `max_results` sources.
    async fn search(&self, query: &str, max_results: u32) -> Result<SearchOutcome, SearchError>;
}
