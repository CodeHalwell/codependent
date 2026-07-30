//! The `reqwest`-backed [`SearchApi`] implementation (PR C1).
//!
//! [`TavilyClient`] POSTs `{base_url}/search` with
//! `{query, max_results, include_answer: true}` and parses the `answer` plus
//! the `results[].{title,url,content}` entries, tolerating missing optional
//! fields. Built on the same bounded pattern as the GitHub client
//! ([`crate::github::client`]): a 30s wall-clock timeout so a stalled peer
//! fails the call instead of hanging it, redirects bounded to 5, a ceiling on
//! the response body, and the bearer key set as a *sensitive* header value so
//! no debug rendering can print it. The key is never logged.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{HeaderValue, AUTHORIZATION};
use reqwest::{Client, Method};
use serde::Deserialize;

use super::key::TavilyKey;
use super::{SearchApi, SearchError, SearchOutcome, SearchResult};

/// Per-request wall-clock ceiling so a stalled peer cannot hang a call forever.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Ceiling on the JSON response body (search replies are small; a huge one is
/// wrong — and an unbounded read is an OOM surface).
const MAX_JSON_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Ceiling on an error-body diagnostic snippet. Error bodies are
/// server-controlled text that flows into diagnostics (and, via tool
/// observations, into the model transcript) — an unbounded `text()` here would
/// be both an OOM surface and an oversized injection channel.
const MAX_ERROR_SNIPPET_BYTES: usize = 64 * 1024;

/// The Tavily web-search client.
pub struct TavilyClient {
    http: Client,
    base_url: String,
    key: TavilyKey,
}

/// The subset of Tavily's `/search` response this client reads. Every field
/// is defaulted so a sparse reply (no answer, a result missing its content)
/// still decodes.
#[derive(Deserialize)]
struct TavilyResponse {
    #[serde(default)]
    answer: Option<String>,
    #[serde(default)]
    results: Vec<TavilyResult>,
}

#[derive(Deserialize)]
struct TavilyResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
}

impl TavilyClient {
    /// Build a client against `base_url` (e.g. `https://api.tavily.com` or a
    /// mock server URL). A trailing slash is stripped so the path join is
    /// exact.
    pub fn new(base_url: impl Into<String>, key: TavilyKey) -> Result<Self, SearchError> {
        let http = Client::builder()
            // A stalled peer must fail the call, not hang it indefinitely.
            .timeout(REQUEST_TIMEOUT)
            // Bounded redirects; reqwest strips `Authorization` on cross-origin
            // redirects, so a redirect cannot carry the key to another origin.
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()?;
        let base_url = base_url.into().trim_end_matches('/').to_string();
        Ok(Self {
            http,
            base_url,
            key,
        })
    }

    /// Start a request with the bearer key set. The header value is marked
    /// sensitive so any future debug rendering of the request (reqwest
    /// redacts sensitive headers) cannot print the key.
    fn request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        let builder = self.http.request(method, url);
        let raw = format!("Bearer {}", self.key.expose());
        match HeaderValue::from_str(&raw) {
            Ok(mut value) => {
                value.set_sensitive(true);
                builder.header(AUTHORIZATION, value)
            }
            // An invalid header value (impossible for a real key) keeps the
            // old behavior: the builder records the error and the send fails.
            Err(_) => builder.header(AUTHORIZATION, raw),
        }
    }
}

#[async_trait]
impl SearchApi for TavilyClient {
    async fn search(&self, query: &str, max_results: u32) -> Result<SearchOutcome, SearchError> {
        let url = format!("{}/search", self.base_url);
        let payload = serde_json::json!({
            "query": query,
            "max_results": max_results,
            "include_answer": true,
        });
        // A search POST is a read: it commits nothing server-side, so no
        // idempotency machinery is needed (unlike the GitHub writes).
        let response = self
            .request(Method::POST, &url)
            .json(&payload)
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let message = read_error_snippet(response).await;
            return Err(SearchError::Api { status, message });
        }
        let bytes = read_bounded(response, MAX_JSON_BODY_BYTES).await?;
        let parsed: TavilyResponse = serde_json::from_slice(&bytes)?;
        Ok(SearchOutcome {
            answer: parsed.answer,
            results: parsed
                .results
                .into_iter()
                .map(|r| SearchResult {
                    title: r.title,
                    url: r.url,
                    content: r.content,
                })
                .collect(),
        })
    }
}

/// Read at most [`MAX_ERROR_SNIPPET_BYTES`] of a non-2xx body for diagnostics,
/// truncating (with a marker) rather than erroring — the status code is the
/// signal; the body is best-effort context.
async fn read_error_snippet(mut response: reqwest::Response) -> String {
    let mut bytes: Vec<u8> = Vec::new();
    let mut truncated = false;
    while let Ok(Some(chunk)) = response.chunk().await {
        let room = MAX_ERROR_SNIPPET_BYTES - bytes.len();
        if chunk.len() > room {
            bytes.extend_from_slice(&chunk[..room]);
            truncated = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    let mut message = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        message.push_str("… [truncated]");
    }
    message
}

/// Read a response body up to `cap` bytes, erroring (never truncating silently)
/// past it — a response that big is wrong, and an unbounded read is an OOM.
async fn read_bounded(mut response: reqwest::Response, cap: usize) -> Result<Vec<u8>, SearchError> {
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > cap {
            return Err(SearchError::Api {
                status: 0,
                message: format!("response body exceeds the {cap}-byte ceiling"),
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
