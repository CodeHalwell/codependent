//! The real embedding model behind knowledge's [`SemanticEmbedder`] seam
//! (rubric 9).
//!
//! `codypendent-knowledge` stays model-free (ADR-009): it defines the trait and
//! the offline `HashingEmbedder` fallback, and THIS crate — the only one allowed
//! to talk to providers — implements the trait over an OpenAI-compatible
//! `POST {base_url}/embeddings` endpoint. The daemon constructs one from
//! `models.toml`'s `[embedding]` entry and injects it into context assembly and
//! the index-outbox drain, exactly as it injects [`LlmFactExtractor`] for memory
//! extraction. With no `[embedding]` entry there is no embedder, retrieval keeps
//! the hashing model, and nothing about today's behavior changes.
//!
//! The wire shape is the OpenAI embeddings API, which Ollama
//! (`nomic-embed-text` at `http://localhost:11434/v1`), OpenAI, and Nebius
//! (`Qwen/Qwen3-Embedding-8B`) all serve:
//!
//! ```json
//! → {"model": "nomic-embed-text", "input": ["first text", "second text"]}
//! ← {"data": [{"index": 0, "embedding": [..]}, {"index": 1, "embedding": [..]}]}
//! ```
//!
//! Three properties matter for correctness and are enforced here rather than
//! assumed of the provider: responses are re-ordered by their `index` field (the
//! spec permits any order); a batch is split into
//! [`MAX_BATCH_INPUTS`]-sized requests so a large registry cannot exceed a
//! provider's per-request limit; and every returned vector is checked to have
//! ONE width, matching the configured `dims` when set — a mismatched width means
//! the endpoint is not the model the persisted vectors were written under.
//!
//! Everything is bounded and fallible-by-value: a timeout, an HTTP error, a
//! malformed body, or a width mismatch returns [`EmbedError`], and knowledge's
//! `semantic_indexes` degrades that to the hashing embedder. Retrieval is an
//! aid, never a gate on running.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use codypendent_knowledge::{EmbedError, SemanticEmbedder};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::models::EmbeddingConfig;

/// Wall-clock bound on ONE `/embeddings` request (the batch is chunked, so this
/// bounds each chunk). Mirrors the fact extractor's 30s model-call budget.
const EMBED_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Largest number of inputs sent in one request. Providers cap batch size
/// (OpenAI at 2048 inputs; local servers far lower), so a registry of any size
/// is chunked rather than refused.
const MAX_BATCH_INPUTS: usize = 64;

/// The provider string this adapter serves. An `[embedding]` entry naming
/// anything else is rejected at construction rather than silently attempted.
const OPENAI_COMPATIBLE: &str = "openai-compatible";

/// An OpenAI-compatible embedding model, batched and content-hash cached.
///
/// Built with [`HttpEmbedder::from_config`] from the `[embedding]` entry. The
/// API key is resolved from its environment variable **at call time** — never
/// stored on this struct, never logged (Chapter 11, "Secrets") — so a key
/// exported after the daemon started still applies.
pub struct HttpEmbedder {
    client: reqwest::Client,
    /// The fully-resolved endpoint: `{base_url}/embeddings`.
    endpoint: String,
    /// The provider-side model name, sent verbatim and persisted alongside every
    /// vector so a model change invalidates stored rows.
    model: String,
    /// The NAME of the environment variable holding the API key (empty for a
    /// local endpoint with no auth). Only the name is retained.
    api_key_env: String,
    /// Expected vector width, when the operator declared one.
    dims: Option<usize>,
    /// hex SHA-256 of an input text → its embedding. Makes a re-embed of an
    /// unchanged registry description (or a repeated query) free within a
    /// process; the `registry_embeddings` table is the cross-process cache.
    cache: Mutex<HashMap<String, Vec<f32>>>,
}

impl std::fmt::Debug for HttpEmbedder {
    /// Redacting by construction: no key is held, and the cached vectors are
    /// noise in a log — only the endpoint and model identify this embedder.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpEmbedder")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("dims", &self.dims)
            .finish_non_exhaustive()
    }
}

impl HttpEmbedder {
    /// Build an embedder from the `[embedding]` entry.
    ///
    /// Fails (so the caller keeps the hashing embedder) when the provider is not
    /// `openai-compatible`, the base URL is blank, or the model name is blank —
    /// a misconfigured entry is a legible startup warning, never a silently
    /// broken retrieval path.
    pub fn from_config(config: &EmbeddingConfig) -> Result<Self, EmbedError> {
        if config.provider != OPENAI_COMPATIBLE {
            return Err(EmbedError(format!(
                "unsupported embedding provider `{}` (only `{OPENAI_COMPATIBLE}` is wired)",
                config.provider
            )));
        }
        let base = config.base_url.trim().trim_end_matches('/');
        if base.is_empty() {
            return Err(EmbedError("embedding base_url is blank".to_string()));
        }
        if config.model.trim().is_empty() {
            return Err(EmbedError("embedding model is blank".to_string()));
        }
        let client = reqwest::Client::builder()
            .timeout(EMBED_REQUEST_TIMEOUT)
            .build()
            .map_err(|error| {
                EmbedError(format!("could not build the embedding client: {error}"))
            })?;
        Ok(Self {
            client,
            endpoint: format!("{base}/embeddings"),
            model: config.model.trim().to_string(),
            api_key_env: config.api_key_env.trim().to_string(),
            dims: config.dims,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// The endpoint this embedder posts to (diagnostics + tests).
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Read the API key from its configured environment variable, at call time.
    /// An unset or blank variable means "no auth" (a local endpoint), matching
    /// [`ModelConfig::api_key_env`](crate::models::ModelConfig::api_key_env).
    fn api_key(&self) -> Option<String> {
        if self.api_key_env.is_empty() {
            return None;
        }
        std::env::var(&self.api_key_env)
            .ok()
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
    }

    /// POST one chunk of inputs and return their vectors **in input order**.
    async fn embed_chunk(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut request = self
            .client
            .post(&self.endpoint)
            .json(&json!({"model": self.model, "input": inputs}));
        if let Some(key) = self.api_key() {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .map_err(|error| EmbedError(format!("{} unreachable: {error}", self.endpoint)))?;
        let status = response.status();
        if !status.is_success() {
            // The body of an error response can echo request content; report the
            // status only.
            return Err(EmbedError(format!(
                "{} returned HTTP {status}",
                self.endpoint
            )));
        }
        let payload: serde_json::Value = response
            .json()
            .await
            .map_err(|error| EmbedError(format!("invalid embeddings response: {error}")))?;
        parse_embeddings(&payload, inputs.len(), self.dims)
    }
}

/// Decode an OpenAI-shaped embeddings response into `expected` vectors, ordered
/// by each entry's `index` (the spec does not promise request order), and verify
/// they share one width — matching `dims` when the operator declared one.
///
/// A missing/duplicate index, a short array, a non-numeric component, or a width
/// disagreement is an error rather than a silently degraded vector: retrieval
/// ranking a query against half-decoded vectors would be worse than falling back
/// to the deterministic hashing model.
fn parse_embeddings(
    payload: &serde_json::Value,
    expected: usize,
    dims: Option<usize>,
) -> Result<Vec<Vec<f32>>, EmbedError> {
    let data = payload
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| EmbedError("embeddings response has no `data` array".to_string()))?;
    if data.len() != expected {
        return Err(EmbedError(format!(
            "embeddings response carried {} vectors for {expected} inputs",
            data.len()
        )));
    }

    let mut ordered: Vec<Option<Vec<f32>>> = vec![None; expected];
    for (position, entry) in data.iter().enumerate() {
        // `index` is optional in practice (some local servers omit it); fall
        // back to the entry's position, which is the order every server that
        // omits it uses.
        let index = entry
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .map_or(position, |i| i as usize);
        let slot = ordered
            .get_mut(index)
            .ok_or_else(|| EmbedError(format!("embeddings response index {index} out of range")))?;
        if slot.is_some() {
            return Err(EmbedError(format!(
                "embeddings response repeated index {index}"
            )));
        }
        let vector = entry
            .get("embedding")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| EmbedError(format!("embeddings entry {index} has no `embedding`")))?
            .iter()
            .map(|value| {
                value
                    .as_f64()
                    .map(|v| v as f32)
                    .ok_or_else(|| EmbedError(format!("embeddings entry {index} is not numeric")))
            })
            .collect::<Result<Vec<f32>, EmbedError>>()?;
        if vector.is_empty() {
            return Err(EmbedError(format!("embeddings entry {index} is empty")));
        }
        *slot = Some(vector);
    }

    let vectors: Vec<Vec<f32>> = ordered
        .into_iter()
        .enumerate()
        .map(|(index, slot)| {
            slot.ok_or_else(|| EmbedError(format!("embeddings response skipped index {index}")))
        })
        .collect::<Result<_, EmbedError>>()?;

    // One width across the whole batch, and the declared one when configured —
    // a mismatch means this endpoint is not the model the persisted vectors were
    // written under (knowledge's freshness check catches the persisted half).
    let width = vectors[0].len();
    if let Some(expected_dims) = dims {
        if width != expected_dims {
            return Err(EmbedError(format!(
                "embedding model returned {width}-dimensional vectors, but `dims` declares {expected_dims}"
            )));
        }
    }
    if let Some(odd) = vectors.iter().position(|vector| vector.len() != width) {
        return Err(EmbedError(format!(
            "embeddings response mixed widths ({width} and {})",
            vectors[odd].len()
        )));
    }
    Ok(vectors)
}

/// The hex SHA-256 cache key for one input text.
fn cache_key(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

#[async_trait::async_trait]
impl SemanticEmbedder for HttpEmbedder {
    fn model(&self) -> &str {
        &self.model
    }

    /// Embed `texts` in input order, serving what the content-hash cache already
    /// holds and requesting only the rest — chunked into [`MAX_BATCH_INPUTS`]
    /// requests. A repeated text within one batch is requested once.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let keys: Vec<String> = texts.iter().map(|text| cache_key(text)).collect();

        // Split into cache hits and the distinct texts still to request.
        let mut pending: Vec<String> = Vec::new();
        let mut pending_keys: Vec<String> = Vec::new();
        {
            let cache = self.cache.lock().expect("embedding cache poisoned");
            for (text, key) in texts.iter().zip(&keys) {
                if !cache.contains_key(key) && !pending_keys.contains(key) {
                    pending.push(text.clone());
                    pending_keys.push(key.clone());
                }
            }
        }

        // Request the misses, chunked; fill the cache as each chunk lands so a
        // later failure still leaves the earlier work cached.
        for (chunk, chunk_keys) in pending
            .chunks(MAX_BATCH_INPUTS)
            .zip(pending_keys.chunks(MAX_BATCH_INPUTS))
        {
            let vectors = self.embed_chunk(chunk).await?;
            let mut cache = self.cache.lock().expect("embedding cache poisoned");
            for (key, vector) in chunk_keys.iter().zip(vectors) {
                cache.insert(key.clone(), vector);
            }
        }

        let cache = self.cache.lock().expect("embedding cache poisoned");
        keys.iter()
            .map(|key| {
                cache
                    .get(key)
                    .cloned()
                    .ok_or_else(|| EmbedError("embedding cache miss after request".to_string()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(base_url: &str, model: &str) -> EmbeddingConfig {
        EmbeddingConfig {
            provider: OPENAI_COMPATIBLE.to_string(),
            base_url: base_url.to_string(),
            model: model.to_string(),
            api_key_env: String::new(),
            dims: None,
        }
    }

    #[test]
    fn from_config_rejects_an_unwired_provider_or_blank_fields() {
        let mut unwired = config("http://localhost:11434/v1", "nomic-embed-text");
        unwired.provider = "cohere".to_string();
        assert!(HttpEmbedder::from_config(&unwired).is_err());

        assert!(HttpEmbedder::from_config(&config("", "nomic-embed-text")).is_err());
        assert!(HttpEmbedder::from_config(&config("http://x/v1", "  ")).is_err());
    }

    /// The endpoint is the base URL plus `/embeddings`, with a trailing slash on
    /// the base absorbed (so `http://host/v1/` and `http://host/v1` agree).
    #[test]
    fn endpoint_appends_embeddings_to_the_base_url() {
        let embedder =
            HttpEmbedder::from_config(&config("http://localhost:11434/v1/", "nomic-embed-text"))
                .expect("builds");
        assert_eq!(embedder.endpoint(), "http://localhost:11434/v1/embeddings");
        assert_eq!(embedder.model(), "nomic-embed-text");
    }

    // -- parse_embeddings ---------------------------------------------------

    #[test]
    fn parse_orders_by_index_not_response_order() {
        let payload = json!({"data": [
            {"index": 1, "embedding": [0.0, 1.0]},
            {"index": 0, "embedding": [1.0, 0.0]},
        ]});
        let vectors = parse_embeddings(&payload, 2, None).expect("parses");
        assert_eq!(vectors, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
    }

    #[test]
    fn parse_falls_back_to_position_when_index_is_absent() {
        let payload = json!({"data": [
            {"embedding": [1.0, 0.0]},
            {"embedding": [0.0, 1.0]},
        ]});
        let vectors = parse_embeddings(&payload, 2, None).expect("parses");
        assert_eq!(vectors, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
    }

    #[test]
    fn parse_rejects_a_declared_dims_mismatch() {
        let payload = json!({"data": [{"index": 0, "embedding": [1.0, 0.0, 0.0]}]});
        assert!(parse_embeddings(&payload, 1, Some(3)).is_ok());
        let error = parse_embeddings(&payload, 1, Some(768)).expect_err("dims mismatch");
        assert!(error.to_string().contains("768"), "{error}");
    }

    #[test]
    fn parse_rejects_malformed_bodies() {
        // No `data` array at all.
        assert!(parse_embeddings(&json!({"error": "nope"}), 1, None).is_err());
        // Fewer vectors than inputs.
        assert!(parse_embeddings(&json!({"data": []}), 1, None).is_err());
        // A repeated index leaves another slot unfilled.
        let repeated = json!({"data": [
            {"index": 0, "embedding": [1.0]},
            {"index": 0, "embedding": [1.0]},
        ]});
        assert!(parse_embeddings(&repeated, 2, None).is_err());
        // An out-of-range index.
        let out_of_range = json!({"data": [{"index": 7, "embedding": [1.0]}]});
        assert!(parse_embeddings(&out_of_range, 1, None).is_err());
        // A non-numeric component.
        let ragged = json!({"data": [{"index": 0, "embedding": ["x"]}]});
        assert!(parse_embeddings(&ragged, 1, None).is_err());
        // Mixed widths across the batch.
        let mixed = json!({"data": [
            {"index": 0, "embedding": [1.0, 0.0]},
            {"index": 1, "embedding": [1.0]},
        ]});
        assert!(parse_embeddings(&mixed, 2, None).is_err());
    }
}
