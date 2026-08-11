//! The `reqwest`-backed [`HfHubClient`] (task: Unsloth local-model discovery).
//!
//! Three endpoints, all `GET`, all under the Hugging Face Hub's public
//! `api/models` surface:
//!
//! - `api/models?author=…&filter=gguf&sort=downloads&direction=-1&limit=…&full=true`
//!   — repo listing (`full=true` is the only way the Hub includes
//!   `lastModified` in a *list* response; this client does not request the
//!   per-repo `siblings`/`cardData` blocks `full=true` also carries, and does
//!   not parse them — only `id`/`downloads`/`likes`/`lastModified`).
//! - `api/models/{repo}/tree/main?recursive=true` — the file tree, grouped
//!   into quant variants by [`super::quant::group_quant_variants`].
//! - `api/models/{repo}` — repo detail, read here only for its optional
//!   `gguf.context_length`.
//!
//! Built on the same bounded pattern as [`crate::search::client`]: a 30s
//! wall-clock timeout so a stalled peer fails the call instead of hanging it,
//! redirects capped at 5, and a byte ceiling on every response body (an
//! unbounded read is both an OOM surface and, for the error-snippet path, an
//! oversized diagnostic).

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;

use super::quant::{group_quant_variants, TreeEntry};
use super::{HfCatalogApi, HfError, HfRepoMetadata, HfRepoSummary, QuantVariant};

/// Per-request wall-clock ceiling so a stalled peer cannot hang a call forever.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Ceiling on a JSON response body. A repo-listing or file-tree reply is at
/// most a few hundred KB in practice; a response this large is wrong, and an
/// unbounded read is an OOM surface.
const MAX_JSON_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Ceiling on an error-body diagnostic snippet (server-controlled text).
const MAX_ERROR_SNIPPET_BYTES: usize = 64 * 1024;
/// Ceiling on how many repos a single listing call may request. The catalog
/// browser only ever needs a screenful; this keeps a misused huge `limit`
/// from becoming an unbounded fetch.
const MAX_LIST_LIMIT: u32 = 100;

/// The Hugging Face Hub discovery client.
pub struct HfHubClient {
    http: Client,
    base_url: String,
}

impl HfHubClient {
    /// Build a client against `base_url` (e.g. [`super::HF_HUB_BASE_URL`] or a
    /// `wiremock` server's URL in tests). A trailing slash is stripped so the
    /// path join is exact.
    pub fn new(base_url: impl Into<String>) -> Result<Self, HfError> {
        let http = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
        })
    }

    /// A client against the real Hugging Face Hub.
    pub fn hub() -> Result<Self, HfError> {
        Self::new(super::HF_HUB_BASE_URL)
    }
}

/// The subset of a Hub listing row this client reads (`full=true`); every
/// other field the real API returns (`tags`, `pipeline_tag`, `siblings`,
/// `cardData`, …) is ignored — serde drops unknown fields by default.
#[derive(Deserialize)]
struct ListedModel {
    id: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    likes: u64,
    #[serde(default, rename = "lastModified")]
    last_modified: Option<String>,
}

/// One `tree/main` entry: only `type`/`path`/`size` matter for quant grouping
/// (`oid`, `lfs`, `xetHash` are ignored).
#[derive(Deserialize)]
struct TreeItem {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    #[serde(default)]
    size: u64,
}

#[derive(Deserialize, Default)]
struct GgufMetadata {
    #[serde(default)]
    context_length: Option<u64>,
}

#[derive(Deserialize, Default)]
struct RepoDetail {
    #[serde(default)]
    gguf: Option<GgufMetadata>,
}

#[async_trait]
impl HfCatalogApi for HfHubClient {
    async fn list_gguf_repos(
        &self,
        author: &str,
        limit: u32,
    ) -> Result<Vec<HfRepoSummary>, HfError> {
        let limit = limit.clamp(1, MAX_LIST_LIMIT).to_string();
        let url = format!("{}/api/models", self.base_url);
        let response = self
            .http
            .get(&url)
            .query(&[
                ("author", author),
                ("filter", "gguf"),
                ("sort", "downloads"),
                ("direction", "-1"),
                ("limit", limit.as_str()),
                ("full", "true"),
            ])
            .send()
            .await?;
        let response = ok_or_api_error(response).await?;
        let bytes = read_bounded(response, MAX_JSON_BODY_BYTES).await?;
        let parsed: Vec<ListedModel> = serde_json::from_slice(&bytes)?;
        Ok(parsed
            .into_iter()
            .map(|m| HfRepoSummary {
                id: m.id,
                downloads: m.downloads,
                likes: m.likes,
                updated_at: m.last_modified,
            })
            .collect())
    }

    async fn list_quant_variants(&self, repo_id: &str) -> Result<Vec<QuantVariant>, HfError> {
        let url = format!("{}/api/models/{repo_id}/tree/main", self.base_url);
        let response = self
            .http
            .get(&url)
            .query(&[("recursive", "true")])
            .send()
            .await?;
        let response = ok_or_api_error(response).await?;
        let bytes = read_bounded(response, MAX_JSON_BODY_BYTES).await?;
        let parsed: Vec<TreeItem> = serde_json::from_slice(&bytes)?;
        let entries: Vec<TreeEntry> = parsed
            .into_iter()
            .map(|item| TreeEntry {
                is_dir: item.kind == "directory",
                path: item.path,
                size: item.size,
            })
            .collect();
        Ok(group_quant_variants(&entries))
    }

    async fn repo_metadata(&self, repo_id: &str) -> Result<HfRepoMetadata, HfError> {
        let url = format!("{}/api/models/{repo_id}", self.base_url);
        let response = self.http.get(&url).send().await?;
        let response = ok_or_api_error(response).await?;
        let bytes = read_bounded(response, MAX_JSON_BODY_BYTES).await?;
        let parsed: RepoDetail = serde_json::from_slice(&bytes)?;
        Ok(HfRepoMetadata {
            context_length: parsed.gguf.and_then(|g| g.context_length),
        })
    }
}

/// Map a non-2xx response to a typed [`HfError::Api`] (naming the resolved
/// URL and a bounded body snippet); pass a successful response through
/// unchanged.
async fn ok_or_api_error(response: reqwest::Response) -> Result<reqwest::Response, HfError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let url = response.url().to_string();
    let message = read_error_snippet(response).await;
    Err(HfError::Api {
        status,
        url,
        message,
    })
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

/// Read a response body up to `cap` bytes, erroring (never truncating
/// silently) past it.
async fn read_bounded(mut response: reqwest::Response, cap: usize) -> Result<Vec<u8>, HfError> {
    let url = response.url().to_string();
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > cap {
            return Err(HfError::Api {
                status: 0,
                url,
                message: format!("response body exceeds the {cap}-byte ceiling"),
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
