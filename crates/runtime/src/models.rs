//! Model providers (STEP 1.9).
//!
//! Three pieces, deliberately kept separate so only one of them depends on a
//! concrete framework provider crate:
//!
//! 1. [`ModelConfig`] / [`load_models`] / [`ModelRegistry`] — parse
//!    `models.toml` and, at call time, build an
//!    `agent_framework_openai::OpenAIChatCompletionClient` for a given
//!    [`ModelId`]. Gated behind the `provider-openai` feature (on by
//!    default), per ADR-009: this crate depends on `agent-framework-rs`
//!    provider crates only behind provider features.
//! 2. [`ModelPolicy`] — the Phase 1 ordered candidate list per
//!    [`AgentMode`]. This is *not* the Phase 7 utility router; it is the
//!    minimal "try this, then that" list called for by STEP 1.9.
//! 3. [`resolve_model`] — walks a policy's candidates for a mode and returns
//!    the first model the provider actually reports as available. Merely
//!    accepting a TCP connection is not enough: an Ollama server can be up
//!    while the configured tag is absent. The caller uses the returned
//!    [`ResolvedModel::id`] both to attribute the run and obtain the client.
//!
//! API keys are read from the configured environment variable at the moment
//! a client is constructed ([`ModelRegistry::client_for`]) — never persisted,
//! never logged, never placed in model context (Chapter 11, "Secrets").

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use codypendent_protocol::{AgentMode, ModelId};
use serde::{Deserialize, Serialize};

use crate::auth::AuthStore;
use codypendent_providers::Catalog;

#[cfg(feature = "provider-openai")]
use agent_framework_openai::OpenAIChatCompletionClient;

#[cfg(feature = "provider-openai")]
use std::collections::BTreeMap;
#[cfg(feature = "provider-openai")]
use std::sync::Arc;

#[cfg(feature = "provider-openai")]
use codypendent_providers::{
    credential_for, AuthMethod, CredentialError, Protocol, ResolvedCredential,
};

/// This module's result alias.
pub type Result<T> = std::result::Result<T, ModelsError>;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// One `[[model]]` entry from `<config_dir>/codypendent/models.toml`.
///
/// ```toml
/// [[model]]
/// id = "hosted-default"
/// provider = "openai-compatible"
/// base_url = "https://api.openai.com/v1"
/// model = "gpt-5.1-codex"
/// api_key_env = "OPENAI_API_KEY"   # env var NAME; value never stored
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelConfig {
    /// The [`ModelId`] this profile is selected by (from a [`ModelPolicy`]
    /// candidate list, or directly).
    pub id: ModelId,
    /// The runtime adapter family: `"openai-compatible"` for REST chat
    /// models, or `"acp"` for an external agent from the official ACP
    /// registry. ACP agents own their model and tool loop; [`Self::model`] is
    /// their registry id. `"openai-compatible"` is broader than its name: the
    /// ACTUAL wire protocol (OpenAI chat-completions, Anthropic Messages, ...)
    /// is resolved from [`Self::provider_id`] against the provider catalog at
    /// call time (see `config_to_protocol_auth`), not from this field — an
    /// entry with `provider_id = Some("anthropic")` speaks the Anthropic wire
    /// even though `provider` still reads `"openai-compatible"`.
    pub provider: String,
    /// The OpenAI-compatible base URL. Empty for ACP profiles.
    #[serde(default)]
    pub base_url: String,
    /// Provider-side model name, or the pinned ACP `agent-id@version` coordinate.
    pub model: String,
    /// The NAME of the environment variable holding the API key, read at
    /// call time. Empty string means no key is needed (e.g. a local Ollama
    /// endpoint with no auth).
    #[serde(default)]
    pub api_key_env: String,
    /// The provider-catalog id this model was added from (e.g. `"nebius"`,
    /// `"azure-openai"`), when known. Additive and optional: it lets the
    /// registry resolve the provider's auth header/prefix, extra headers, and
    /// documented key env NAMES from the catalog at call time, so a
    /// non-bearer provider (Azure OpenAI's `api-key` header, GitHub Models'
    /// API-version header) authenticates correctly instead of being
    /// flattened to `Authorization: Bearer`. `None` (every pre-existing
    /// entry) keeps the legacy bearer behavior exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// The model's context window in tokens, if known. Sourced from
    /// `models.toml` (a user-set `context_tokens` key) — there is no
    /// auto-population from the provider catalog in v1. Used for two
    /// things: (1) the `num_ctx` request hint, and (2) the denominator of
    /// the context-usage percentage surfaced in the TUI footer. `None`
    /// means "unknown" — no percentage is fabricated and no `num_ctx` is
    /// sent; a config without this key parses unchanged (back-compatible).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
}

/// The on-disk shape of `models.toml`: a bare array of `[[model]]` tables.
#[derive(Debug, Deserialize)]
struct ModelsFile {
    #[serde(default, rename = "model")]
    model: Vec<ModelConfig>,
}

/// A `context_tokens` above this cannot come from any real model this catalog
/// knows about — the largest curated ceiling as of this build is OpenAI's
/// 1,050,000-token tier (`crates/providers/builtin_catalog.toml`); doubling
/// that and rounding up leaves headroom for near-future genuine growth
/// without needing a code change on every model release, while still
/// rejecting anything a hostile or misconfigured gateway would plausibly send
/// (a `u64::MAX`, or any other implausible `"context_length"` in a provider's
/// own `/models` response).
pub const MAX_PLAUSIBLE_CONTEXT_TOKENS: u64 = 2_000_000;

/// Clamp an untrusted `context_tokens` reading to [`MAX_PLAUSIBLE_CONTEXT_TOKENS`].
/// `None` (unknown) passes through unchanged — this only caps an implausible
/// number, it never fabricates one.
///
/// This is the load-bearing half of closing the trust-boundary gap the
/// 2026-08-13 review found (F4, `acp-models.md`): `crates/cli/src/tui.rs`'s
/// `merge_catalog_rows` lets a provider's own `/models` response win over the
/// curated catalog's `context_tokens` on the (reasonable, for every OTHER
/// field) theory that "the provider knows its own model best" — but that
/// value is then persisted into `models.toml` verbatim and, from there, is
/// load-bearing rather than display-only: `FrameworkModelDriver::from_registry`
/// (`crates/runtime/src/agent.rs`) reads it and forwards it as Ollama's
/// `{"options":{"num_ctx": n}}` request hint, and it is the denominator of the
/// TUI footer's context-usage percentage. Clamping here, at the one place
/// every `models.toml` entry is parsed regardless of which writer produced it
/// (the TUI's add-model flow, `codypendent models add`, or a hand-edit), means
/// every downstream consumer is safe even though none of them re-validates —
/// a defense-in-depth floor under the TUI-side and `agent.rs`-side fixes
/// proposed alongside this change (`.impl/proposals/agent-tui-from-agent-models.md`,
/// `.impl/proposals/agent-retrieval-from-agent-models.md`), not a replacement
/// for them: this is a blunt absolute ceiling, not the tighter
/// per-model catalog ceiling [`ModelRegistry::context_tokens_for`] applies
/// when a catalog is attached.
#[must_use]
pub fn clamp_context_tokens(context_tokens: Option<u64>) -> Option<u64> {
    context_tokens.map(|tokens| tokens.min(MAX_PLAUSIBLE_CONTEXT_TOKENS))
}

/// Parse `models.toml` at `path` into its [`ModelConfig`] entries.
///
/// Exposed standalone (in addition to [`ModelRegistry::load`]) so tests — and
/// callers that want to inspect or filter configs before building a registry
/// — can drive parsing directly against a temp file. Every entry's
/// `context_tokens` is passed through [`clamp_context_tokens`] here, so this
/// function — not only [`ModelRegistry`] — never hands a caller an implausible
/// value; a test that parses a fixture file directly (bypassing the registry)
/// is protected exactly like a live daemon load.
pub fn load_models(path: &Path) -> Result<Vec<ModelConfig>> {
    let text = std::fs::read_to_string(path).map_err(|source| ModelsError::ReadConfig {
        path: path.to_path_buf(),
        source,
    })?;
    let file: ModelsFile = toml::from_str(&text).map_err(|source| ModelsError::ParseConfig {
        path: path.to_path_buf(),
        source,
    })?;
    let mut models = file.model;
    for model in &mut models {
        model.context_tokens = clamp_context_tokens(model.context_tokens);
    }
    Ok(models)
}

// ---------------------------------------------------------------------------
// models.toml extras: the `[embedding]` entry + `[retrieval]` tuning
// ---------------------------------------------------------------------------

/// The `[embedding]` entry in `models.toml` (rubric 9 — real embeddings):
/// names the OpenAI-compatible `/embeddings` endpoint retrieval embeds with.
/// Absent, retrieval keeps the offline hashing embedder (today's behavior).
///
/// ```toml
/// [embedding]
/// provider = "openai-compatible"          # the default; may be omitted
/// base_url = "http://localhost:11434/v1"  # Ollama, OpenAI, Nebius, …
/// model = "nomic-embed-text"
/// api_key_env = ""                        # env var NAME; value never stored
/// dims = 768                              # optional: verified against responses
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// The runtime adapter; only `"openai-compatible"` is wired.
    #[serde(default = "default_embedding_provider")]
    pub provider: String,
    /// The OpenAI-compatible base URL (`POST {base_url}/embeddings`).
    pub base_url: String,
    /// Provider-side embedding model name (e.g. `nomic-embed-text`,
    /// `Qwen/Qwen3-Embedding-8B`).
    pub model: String,
    /// The NAME of the environment variable holding the API key, read at call
    /// time — never persisted (the `ModelConfig::api_key_env` contract). Empty
    /// means no key (a local endpoint).
    #[serde(default)]
    pub api_key_env: String,
    /// Expected vector dimensionality. `None` accepts whatever the model
    /// returns; `Some(n)` rejects a mismatched response (a wrong-model guard).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dims: Option<usize>,
}

fn default_embedding_provider() -> String {
    "openai-compatible".to_string()
}

/// Default `[retrieval] mcp_top_k`: when a run's MCP bridge offers more than
/// this many tools, only the `mcp_top_k` most relevant to the run are
/// advertised (at or below it, all are — today's behavior). `0` disables the
/// gate entirely (full injection).
pub const DEFAULT_MCP_TOP_K: usize = 8;

/// Default `[retrieval] builtin_top_k`: how many BUILT-IN tools the retrieval
/// funnel selects for a run on top of the always-advertised floor
/// (`codypendent_runtime::agent::ALWAYS_ADVERTISED_TOOLS`). `0` disables the gate
/// entirely (advertise every offered built-in — full injection, the behavior
/// before rubric 9 reached this family).
///
/// Eight matches [`DEFAULT_MCP_TOP_K`] and the `skills.search` card budget, and
/// with the seven-tool floor it lands a default Build run well under the full
/// catalog rather than at all of it.
pub const DEFAULT_BUILTIN_TOP_K: usize = 8;

/// The `[retrieval]` tuning table in `models.toml`.
///
/// ```toml
/// [retrieval]
/// mcp_top_k = 8       # 0 disables retrieval gating (advertise every MCP tool)
/// builtin_top_k = 8   # 0 disables it for the built-in tools
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalSettings {
    /// See [`DEFAULT_MCP_TOP_K`].
    #[serde(default = "default_mcp_top_k")]
    pub mcp_top_k: usize,
    /// See [`DEFAULT_BUILTIN_TOP_K`].
    #[serde(default = "default_builtin_top_k")]
    pub builtin_top_k: usize,
}

fn default_mcp_top_k() -> usize {
    DEFAULT_MCP_TOP_K
}

fn default_builtin_top_k() -> usize {
    DEFAULT_BUILTIN_TOP_K
}

impl Default for RetrievalSettings {
    fn default() -> Self {
        Self {
            mcp_top_k: DEFAULT_MCP_TOP_K,
            builtin_top_k: DEFAULT_BUILTIN_TOP_K,
        }
    }
}

/// The non-`[[model]]` tables of `models.toml`, parsed independently of
/// [`load_models`] so both readers ignore each other's keys and an existing
/// file is back-compatible in both directions.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ModelExtras {
    /// The `[embedding]` entry, when configured.
    #[serde(default)]
    pub embedding: Option<EmbeddingConfig>,
    /// The `[retrieval]` tuning; every field defaults when the table is absent.
    #[serde(default)]
    pub retrieval: RetrievalSettings,
}

/// Parse the `[embedding]` / `[retrieval]` extras from `models.toml`. An
/// ABSENT file yields the defaults (no embedding model, default tuning) —
/// unlike [`load_models`], because these tables are optional configuration and
/// a daemon with no `models.toml` at all must still assemble context. A
/// present-but-malformed file is an error, so a typo is legible rather than a
/// silent fallback.
pub fn load_model_extras(path: &Path) -> Result<ModelExtras> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ModelExtras::default())
        }
        Err(source) => {
            return Err(ModelsError::ReadConfig {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    toml::from_str(&text).map_err(|source| ModelsError::ParseConfig {
        path: path.to_path_buf(),
        source,
    })
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from model configuration, client construction, and candidate
/// resolution.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModelsError {
    /// `models.toml` could not be read.
    #[error("failed to read model config file at {path}: {source}")]
    ReadConfig {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// `models.toml` was read but is not valid TOML / does not match the
    /// expected shape.
    #[error("failed to parse model config file at {path}: {source}")]
    ParseConfig {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    /// A [`ModelId`] was requested that has no entry in the registry.
    #[error("model `{0}` is not registered")]
    UnknownModel(ModelId),

    /// A model's `provider` field names no installed runtime adapter.
    #[error(
        "model `{model}` uses unsupported provider `{provider}` (supported: \"openai-compatible\", \"acp\")"
    )]
    UnsupportedProvider { model: ModelId, provider: String },

    /// A model's provider maps to a wire protocol this build does not wire a
    /// client for. OpenAI-chat and (when the `provider-anthropic` feature is
    /// compiled in — on by default) Anthropic are wired; Gemini native and
    /// anything else are follow-ups. Also returned for a wired protocol when
    /// the enabling feature was compiled out (`--no-default-features`), so a
    /// passing [`ModelRegistry::check_model`] never promises more than
    /// [`ModelRegistry::client_for`] can deliver.
    #[error("model `{model}` uses protocol `{protocol}` which is not wired in this build")]
    ProtocolNotWired { model: ModelId, protocol: String },

    /// A model's `api_key_env` names an environment variable that is not
    /// set. Names the variable, per STEP 1.9's test requirement and the
    /// Chapter 11 rule that secrets are identified, never guessed at, in
    /// error output.
    #[error(
        "model `{model}` requires the environment variable `{var}` for its API key, but it is not set"
    )]
    MissingApiKeyEnv { model: ModelId, var: String },

    /// A `base_url` could not be reduced to a `host:port` authority for a
    /// connectivity check.
    #[error("could not parse base_url `{base_url}`: {reason}")]
    InvalidBaseUrl { base_url: String, reason: String },

    /// A connectivity check against `base_url` failed (connection refused,
    /// unreachable, timed out, ...).
    #[error("connection check to `{base_url}` failed: {reason}")]
    ConnectionFailed { base_url: String, reason: String },

    /// The endpoint answered, but the configured provider-side model was not
    /// present in its OpenAI-compatible `/models` catalog.
    #[error("model `{model}` (`{provider_model}`) is unavailable: {reason}")]
    ModelUnavailable {
        model: ModelId,
        provider_model: String,
        reason: String,
    },

    /// [`ModelPolicy::candidates`] returned an empty list for the mode.
    #[error("no candidate model is configured for mode {mode:?}")]
    NoCandidates { mode: AgentMode },

    /// Every candidate for the mode failed to resolve (unregistered or
    /// unreachable). Carries each attempted [`ModelId`] and its failure
    /// reason, in candidate order, for diagnostics.
    #[error("all candidate models for mode {mode:?} failed: {attempts:?}")]
    AllCandidatesFailed {
        mode: AgentMode,
        attempts: Vec<(ModelId, String)>,
    },
}

/// Whether a model-request failure is worth retrying (the transient/permanent
/// error taxonomy the loop's retry-with-backoff wrapper dispatches on — see
/// `FrameworkModelDriver::next_step` in `agent.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// Environmental and time-bounded — the identical request may genuinely
    /// succeed a moment later (connect refused/reset, timeouts, HTTP 408/429/
    /// 5xx, provider overload). Worth a bounded retry.
    Transient,
    /// The request itself (or its configuration) is refused — bad credentials,
    /// an unknown model, a 4xx contract violation, unparseable config. A retry
    /// can only repeat the same refusal, so it is surfaced immediately.
    Permanent,
}

impl ModelsError {
    /// Classify this error for retry purposes. Only the genuinely
    /// environmental variants are [`FailureClass::Transient`]:
    /// [`ConnectionFailed`](ModelsError::ConnectionFailed) (refused/reset/
    /// timed-out connection attempts) always is, and
    /// [`ModelUnavailable`](ModelsError::ModelUnavailable) is when its
    /// provider-supplied `reason` names a transient condition (an HTTP 5xx or
    /// 429 from `/models`). Everything else — config read/parse failures,
    /// unknown models, unsupported providers, missing credentials, exhausted
    /// candidate lists — is [`FailureClass::Permanent`]: retrying re-reads the
    /// same file and re-misses the same env var.
    #[must_use]
    pub fn failure_class(&self) -> FailureClass {
        match self {
            ModelsError::ConnectionFailed { .. } => FailureClass::Transient,
            ModelsError::ModelUnavailable { reason, .. } => classify_provider_message(reason),
            _ => FailureClass::Permanent,
        }
    }
}

/// Classify an OPAQUE provider/stream failure message as transient or
/// permanent. The live driver's stream errors cross the framework seam as
/// formatted strings (and `ModelUnavailable.reason` embeds a response body),
/// so this is a conservative textual taxonomy: only a message that plainly
/// names a transient condition — a connect/reset/timeout failure, provider
/// overload, or an HTTP 408/429/5xx status — is [`FailureClass::Transient`];
/// anything unrecognized is [`FailureClass::Permanent`], so a novel failure
/// surfaces immediately instead of being silently retried. HTTP status codes
/// are matched as standalone digit runs (never substrings), so a `500` inside
/// an id like `15005` cannot misclassify a message.
#[must_use]
pub fn classify_provider_message(message: &str) -> FailureClass {
    /// Phrases that name a plainly transient condition, matched
    /// case-insensitively.
    const TRANSIENT_PHRASES: [&str; 14] = [
        "connection refused",
        "connection reset",
        "connection closed",
        "broken pipe",
        "timed out",
        "timeout",
        "temporarily unavailable",
        "overloaded",
        "rate limit",
        "too many requests",
        "internal server error",
        "bad gateway",
        "service unavailable",
        "gateway timeout",
    ];
    /// Retryable HTTP status codes: request timeout, rate limited, and the
    /// server-side 5xx family a provider surfaces during a blip.
    const TRANSIENT_STATUS: [&str; 6] = ["408", "429", "500", "502", "503", "504"];
    let lower = message.to_ascii_lowercase();
    if TRANSIENT_PHRASES
        .iter()
        .any(|phrase| lower.contains(phrase))
    {
        return FailureClass::Transient;
    }
    let has_status = lower
        .split(|c: char| !c.is_ascii_digit())
        .any(|run| TRANSIENT_STATUS.contains(&run));
    if has_status {
        FailureClass::Transient
    } else {
        FailureClass::Permanent
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// The `auth.json` entry id holding a PROVIDER-wide key (as opposed to a
/// per-model entry keyed by the model's display id): `provider/<catalog-id>`.
/// Stored by the add-model flow alongside the per-model entry so one pasted
/// key serves every model later added from the same provider; read by
/// [`ModelRegistry::client_for`]/[`check_model`](ModelRegistry::check_model)
/// after the per-model entry and before the environment. The `provider/`
/// prefix is reserved (like the Tavily `integrations/tavily` id) and cannot
/// collide with add-model display ids, which are `<provider>/<model>` for
/// catalog providers and `acp/<agent>` for ACP agents.
#[must_use]
pub fn provider_auth_id(provider_id: &str) -> String {
    format!("provider/{provider_id}")
}

/// The process-wide built-in provider catalog, parsed once. The fallback for
/// registries built without [`ModelRegistry::with_catalog`] (e.g. the daemon
/// executor), so catalog-declared auth headers resolve there too.
fn builtin_catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(Catalog::builtin)
}

/// The set of configured model profiles, keyed by [`ModelId`], plus the
/// resolved [`AuthStore`] (`auth.json`) so [`ModelRegistry::client_for`] can
/// prefer a stored key over the model's `api_key_env`. The store's own redacting
/// `Debug` keeps the derived `Debug` here from leaking a key.
#[derive(Debug, Clone, Default)]
pub struct ModelRegistry {
    configs: HashMap<ModelId, ModelConfig>,
    auth: AuthStore,
    /// The provider catalog auth headers are resolved against (see
    /// [`ModelConfig::provider_id`]). `None` falls back to the built-ins, so
    /// a caller that cannot layer the user's `providers.toml` still resolves
    /// every built-in provider correctly.
    #[cfg_attr(not(feature = "provider-openai"), allow(dead_code))]
    catalog: Option<Catalog>,
}

impl ModelRegistry {
    /// Build a registry from already-parsed configs. Later entries with a
    /// duplicate `id` overwrite earlier ones. The auth store starts empty (no
    /// `auth.json` keys), so every model resolves exactly as before until one is
    /// attached with [`with_auth`](Self::with_auth).
    pub fn new(configs: impl IntoIterator<Item = ModelConfig>) -> Self {
        let configs = configs.into_iter().map(|c| (c.id.clone(), c)).collect();
        Self {
            configs,
            auth: AuthStore::default(),
            catalog: None,
        }
    }

    /// Attach the resolved [`AuthStore`] (`auth.json`) so `client_for` prefers a
    /// stored key over the model's `api_key_env`. Additive: the default empty
    /// store leaves every model resolving exactly as before.
    #[must_use]
    pub fn with_auth(mut self, auth: AuthStore) -> Self {
        self.auth = auth;
        self
    }

    /// Attach a resolved provider [`Catalog`] (built-ins layered with the
    /// user's `providers.toml`) for auth-header resolution. Additive: without
    /// it the embedded built-in catalog is used, so only a user-defined
    /// provider with a custom auth header needs this to be exact.
    #[must_use]
    pub fn with_catalog(mut self, catalog: Catalog) -> Self {
        self.catalog = Some(catalog);
        self
    }

    /// Parse `models.toml` at `path` and build a registry from it.
    pub fn load(path: &Path) -> Result<Self> {
        Ok(Self::new(load_models(path)?))
    }

    /// Look up a model's configuration by id.
    pub fn get(&self, id: &ModelId) -> Option<&ModelConfig> {
        self.configs.get(id)
    }

    /// Iterate over every registered model id.
    pub fn ids(&self) -> impl Iterator<Item = &ModelId> {
        self.configs.keys()
    }

    /// Whether this profile delegates the run to an ACP agent.
    #[must_use]
    pub fn is_acp(&self, id: &ModelId) -> bool {
        self.get(id).is_some_and(|config| config.provider == "acp")
    }

    /// The full ACP coordinate for an ACP profile — `id`, `id@version`, or
    /// `id@version#model` once the profile pins one of the agent's own models.
    /// Launch-oriented consumers strip the additive `#model` part with
    /// [`acp_coordinate_agent`]; [`acp_agent_model`](Self::acp_agent_model)
    /// reads the pin.
    #[must_use]
    pub fn acp_agent_id(&self, id: &ModelId) -> Option<&str> {
        self.get(id)
            .filter(|config| config.provider == "acp")
            .map(|config| config.model.as_str())
    }

    /// The agent-model id an ACP profile pins on its external agent
    /// (`id@version#model`), if any. `None` for a bare agent profile — the
    /// agent then keeps whatever default model it booted with.
    #[must_use]
    pub fn acp_agent_model(&self, id: &ModelId) -> Option<&str> {
        self.acp_agent_id(id).and_then(acp_coordinate_model)
    }
}

/// Split an ACP model coordinate `id[@version][#model]` at the first `#` and
/// return the launchable agent part. Additive: coordinates written before
/// model pinning existed carry no `#` and return themselves unchanged.
#[must_use]
pub fn acp_coordinate_agent(coordinate: &str) -> &str {
    coordinate
        .split_once('#')
        .map_or(coordinate, |(agent, _)| agent)
}

/// The agent-model id an ACP coordinate pins (`id[@version]#model`), if any.
/// A dangling `#` pins nothing.
#[must_use]
pub fn acp_coordinate_model(coordinate: &str) -> Option<&str> {
    coordinate
        .split_once('#')
        .map(|(_, model)| model)
        .filter(|model| !model.is_empty())
}

#[cfg(test)]
mod acp_coordinate_tests {
    use super::*;

    #[test]
    fn coordinates_split_into_agent_and_optional_model_pin() {
        assert_eq!(acp_coordinate_agent("demo-acp@1.2.3"), "demo-acp@1.2.3");
        assert_eq!(acp_coordinate_model("demo-acp@1.2.3"), None);
        assert_eq!(
            acp_coordinate_agent("demo-acp@1.2.3#agent-model-1"),
            "demo-acp@1.2.3"
        );
        assert_eq!(
            acp_coordinate_model("demo-acp@1.2.3#agent-model-1"),
            Some("agent-model-1")
        );
        // A dangling separator pins nothing (and still names the agent).
        assert_eq!(acp_coordinate_model("demo-acp@1.2.3#"), None);
        assert_eq!(acp_coordinate_agent("demo-acp@1.2.3#"), "demo-acp@1.2.3");
    }

    #[test]
    fn registry_reads_the_model_pin_from_acp_profiles_only() {
        let registry = ModelRegistry::new([
            ModelConfig {
                id: ModelId("acp/demo#agent-model-1".to_string()),
                provider: "acp".to_string(),
                base_url: String::new(),
                model: "demo-acp@1.2.3#agent-model-1".to_string(),
                api_key_env: String::new(),
                context_tokens: None,
                provider_id: None,
            },
            ModelConfig {
                id: ModelId("acp/demo".to_string()),
                provider: "acp".to_string(),
                base_url: String::new(),
                model: "demo-acp@1.2.3".to_string(),
                api_key_env: String::new(),
                context_tokens: None,
                provider_id: None,
            },
            ModelConfig {
                id: ModelId("hosted".to_string()),
                provider: "openai-compatible".to_string(),
                base_url: "https://example.test/v1".to_string(),
                model: "some#model".to_string(),
                api_key_env: String::new(),
                context_tokens: None,
                provider_id: None,
            },
        ]);
        assert_eq!(
            registry.acp_agent_model(&ModelId("acp/demo#agent-model-1".to_string())),
            Some("agent-model-1")
        );
        assert_eq!(
            registry.acp_agent_model(&ModelId("acp/demo".to_string())),
            None
        );
        // Non-ACP profiles never expose a pin, whatever their model string.
        assert_eq!(
            registry.acp_agent_model(&ModelId("hosted".to_string())),
            None
        );
    }
}

/// How a model's endpoint expects its key and extra headers on the wire,
/// resolved from the provider catalog via [`ModelConfig::provider_id`]. A
/// config with no (known) provider id resolves to the bearer default, so
/// every pre-existing `models.toml` entry behaves exactly as before.
#[cfg(feature = "provider-openai")]
#[derive(Debug, Clone)]
struct EndpointAuth {
    /// The header the key is injected under (default `Authorization`).
    header: String,
    /// The value prefix in front of the key (default `"Bearer "`).
    prefix: String,
    /// Provider-wide headers sent on every request — an API-version pin, for
    /// the providers whose catalog entry declares one.
    extra_headers: BTreeMap<String, String>,
    /// The provider's documented key env-var NAMES, consulted (first set
    /// wins) only when the model has no `auth.json` key and no explicit
    /// `api_key_env` of its own.
    provider_env: Vec<String>,
    /// The catalog provider declares API-key auth, even when its env-name
    /// list is empty. Kept separately from `provider_env` so readiness never
    /// mistakes a malformed key-auth provider for a deliberately keyless one.
    requires_api_key: bool,
}

#[cfg(feature = "provider-openai")]
impl EndpointAuth {
    /// Whether this is the exact shape the stock framework client sends
    /// (`Authorization: Bearer …`, nothing else) — the fast path.
    fn is_framework_default(&self) -> bool {
        self.header == "Authorization" && self.prefix == "Bearer " && self.extra_headers.is_empty()
    }
}

/// Resolve a model's [`EndpointAuth`] from the catalog. Defaults to bearer
/// with no extras when the entry names no provider, or an unknown one.
#[cfg(feature = "provider-openai")]
fn endpoint_auth_for(cfg: &ModelConfig, catalog: &Catalog) -> EndpointAuth {
    let provider = cfg.provider_id.as_deref().and_then(|id| catalog.get(id));
    let (header, prefix, provider_env, requires_api_key) = provider
        .and_then(|p| {
            p.auth.iter().find_map(|method| match method {
                AuthMethod::ApiKey {
                    env,
                    header,
                    prefix,
                } => Some((header.clone(), prefix.clone(), env.clone(), true)),
                _ => None,
            })
        })
        .unwrap_or_else(|| {
            (
                "Authorization".to_string(),
                "Bearer ".to_string(),
                Vec::new(),
                false,
            )
        });
    EndpointAuth {
        header,
        prefix,
        extra_headers: provider
            .map(|p| p.extra_headers.clone())
            .unwrap_or_default(),
        provider_env,
        requires_api_key,
    }
}

/// Map a persisted [`ModelConfig`] onto the provider abstraction. Chat profiles
/// become `(protocol, ApiKey|None)`; ACP profiles are marked so the assembly
/// executor can route them to the full-agent runtime instead of a chat client.
///
/// `provider == "openai-compatible"` on [`ModelConfig`] is a broad "REST chat
/// family" adapter marker, not a promise that the wire is literally OpenAI's:
/// the catalog is the authority on the actual wire [`Protocol`]. When the
/// entry names a known [`ModelConfig::provider_id`], that provider's declared
/// `protocol` wins (e.g. `anthropic` resolves to [`Protocol::Anthropic`], not
/// OpenAI chat-completions) — this is what lets `codypendent models add
/// anthropic claude-opus-5` build a client that actually speaks the Anthropic
/// Messages API instead of POSTing an OpenAI-shaped body to it. An absent or
/// unknown `provider_id` keeps today's [`Protocol::OpenAiChat`] default, so
/// every pre-existing `models.toml` entry (and every entry from a provider
/// this catalog doesn't curate) resolves exactly as before. The `ApiKey` arm
/// carries the catalog-resolved header/prefix (bearer when the entry names no
/// provider) so Azure- and Anthropic-shaped providers stop being flattened to
/// `Authorization: Bearer`.
#[cfg(feature = "provider-openai")]
fn config_to_protocol_auth(cfg: &ModelConfig, catalog: &Catalog) -> Result<(Protocol, AuthMethod)> {
    if cfg.provider == "acp" {
        return Ok((
            Protocol::Acp,
            AuthMethod::Acp {
                command: String::new(),
                args: Vec::new(),
                env: Default::default(),
            },
        ));
    }
    if cfg.provider != "openai-compatible" {
        return Err(ModelsError::UnsupportedProvider {
            model: cfg.id.clone(),
            provider: cfg.provider.clone(),
        });
    }
    let protocol = cfg
        .provider_id
        .as_deref()
        .and_then(|id| catalog.get(id))
        .map(|provider| provider.protocol)
        .unwrap_or(Protocol::OpenAiChat);
    let auth = if cfg.api_key_env.trim().is_empty() {
        AuthMethod::None
    } else {
        let endpoint = endpoint_auth_for(cfg, catalog);
        AuthMethod::ApiKey {
            env: vec![cfg.api_key_env.clone()],
            header: endpoint.header,
            prefix: endpoint.prefix,
        }
    };
    Ok((protocol, auth))
}

#[cfg(feature = "provider-openai")]
impl ModelRegistry {
    /// The catalog auth headers are resolved against: the attached one, or
    /// the process-wide built-ins.
    fn catalog(&self) -> &Catalog {
        self.catalog.as_ref().unwrap_or_else(|| builtin_catalog())
    }

    /// `id`'s `context_tokens`, clamped against the tighter of two ceilings:
    /// [`MAX_PLAUSIBLE_CONTEXT_TOKENS`] (always, via [`clamp_context_tokens`])
    /// and — when `id` names a catalog-known `provider_id` + provider-side
    /// `model` — that exact row's own curated `context_tokens`, when the
    /// catalog documents one. The second is strictly tighter: a curated
    /// Anthropic row tops out at 1,000,000, so a live `/models` response that
    /// claimed 1,900,000 for that same model would pass the blunt absolute
    /// ceiling but should not pass THIS one. Callers that forward
    /// `context_tokens` somewhere consequential (the Ollama `num_ctx` request
    /// hint, a context-usage percentage) should prefer this over reading
    /// [`ModelConfig::context_tokens`] directly — see F4 in
    /// `2026-08-13-verticals/acp-models.md` and [`clamp_context_tokens`]'s
    /// doc comment for the full trust-boundary account.
    #[must_use]
    pub fn context_tokens_for(&self, id: &ModelId) -> Option<u64> {
        let cfg = self.get(id)?;
        let configured = clamp_context_tokens(cfg.context_tokens)?;
        let catalog_ceiling = cfg
            .provider_id
            .as_deref()
            .and_then(|provider_id| self.catalog().model(provider_id, &cfg.model))
            .and_then(|row| row.context_tokens);
        Some(match catalog_ceiling {
            Some(ceiling) => configured.min(ceiling),
            None => configured,
        })
    }

    /// Resolve the key exactly as the live client does. Keeping this in one
    /// helper ensures the readiness probe and the first completion cannot
    /// disagree because they used different credential precedence:
    /// (1) the model's own `auth.json` entry; (2) the provider-wide
    /// `auth.json` entry ([`provider_auth_id`]); (3) the model's explicit
    /// `api_key_env` (missing → a naming error, unchanged); (4) the catalog
    /// provider's documented env NAMES, best-effort — an unset variable
    /// falls through to keyless exactly as before, never a new error.
    async fn api_key_for(&self, cfg: &ModelConfig) -> Result<String> {
        if let Some(key) = self
            .auth
            .get(cfg.id.0.as_str())
            .filter(|key| !key.trim().is_empty())
        {
            return Ok(key.to_string());
        }
        if let Some(key) = cfg
            .provider_id
            .as_deref()
            .and_then(|pid| self.auth.get(&provider_auth_id(pid)))
            .filter(|key| !key.trim().is_empty())
        {
            return Ok(key.to_string());
        }
        let (_, auth) = config_to_protocol_auth(cfg, self.catalog())?;
        match credential_for(&auth).resolve().await {
            Ok(ResolvedCredential::ApiKey { value, .. }) => Ok(value),
            Ok(ResolvedCredential::None) => {
                let endpoint = endpoint_auth_for(cfg, self.catalog());
                if !endpoint.provider_env.is_empty() {
                    let provider_auth = AuthMethod::ApiKey {
                        env: endpoint.provider_env,
                        header: endpoint.header,
                        prefix: endpoint.prefix,
                    };
                    if let Ok(ResolvedCredential::ApiKey { value, .. }) =
                        credential_for(&provider_auth).resolve().await
                    {
                        return Ok(value);
                    }
                }
                Ok(String::new())
            }
            Err(CredentialError::MissingEnv { var }) => Err(ModelsError::MissingApiKeyEnv {
                model: cfg.id.clone(),
                var,
            }),
            Err(other) => Err(ModelsError::ProtocolNotWired {
                model: cfg.id.clone(),
                protocol: other.to_string(),
            }),
        }
    }

    /// Whether the configured model can resolve every credential required to
    /// start a run, without exposing the credential itself.
    ///
    /// This deliberately delegates to [`Self::api_key_for`] so discovery and
    /// the live client share one precedence rule: model `auth.json`, provider
    /// `auth.json`, explicit model env, then catalog provider env. A keyless
    /// endpoint is ready when neither the model nor its catalog provider
    /// declares API-key authentication. ACP launchability is checked by its
    /// owning integration; it has no chat credential to resolve here.
    pub async fn credentials_resolvable(&self, id: &ModelId) -> Result<bool> {
        let cfg = self
            .get(id)
            .ok_or_else(|| ModelsError::UnknownModel(id.clone()))?;
        let (protocol, _) = config_to_protocol_auth(cfg, self.catalog())?;
        if matches!(protocol, Protocol::Acp) {
            return Ok(true);
        }

        let endpoint = endpoint_auth_for(cfg, self.catalog());
        let requires_api_key = !cfg.api_key_env.trim().is_empty() || endpoint.requires_api_key;
        if !requires_api_key {
            return Ok(true);
        }

        self.api_key_for(cfg)
            .await
            .map(|key| !key.trim().is_empty())
    }

    /// Verify that a configured model is genuinely usable enough to select:
    /// credentials resolve, the OpenAI-compatible `/models` endpoint answers,
    /// and its catalog contains the configured provider-side model name.
    pub async fn check_model(&self, id: &ModelId) -> Result<()> {
        let cfg = self
            .get(id)
            .ok_or_else(|| ModelsError::UnknownModel(id.clone()))?;
        if cfg.provider == "acp" {
            if cfg.model.trim().is_empty() {
                return Err(ModelsError::ModelUnavailable {
                    model: id.clone(),
                    provider_model: cfg.model.clone(),
                    reason: "ACP registry id is blank".to_string(),
                });
            }
            // The assembly executor performs the real cached-registry,
            // executable, and ACP-handshake readiness check because it owns the
            // RuntimePaths needed to resolve the installed agent.
            return Ok(());
        }
        let (protocol, _) = config_to_protocol_auth(cfg, self.catalog())?;
        // The reachability probe's path differs by wire protocol: OpenAI-chat
        // providers list models at `{base_url}/models` (`base_url` already
        // carries the `/v1` the provider documents, e.g.
        // `https://api.openai.com/v1`); Anthropic's Messages API base_url is
        // bare (`https://api.anthropic.com`) and lists models at the
        // spec-fixed `/v1/models` — both return the same `{"data":
        // [{"id": ...}, ...]}` shape, so the matching logic below is shared.
        // Anything else this build cannot wire a client for should not claim
        // to be checkable either — a passing `check` must mean the run that
        // follows can actually build a client.
        let models_suffix = match protocol {
            Protocol::OpenAiChat => "/models",
            #[cfg(feature = "provider-anthropic")]
            Protocol::Anthropic => "/v1/models",
            _ => {
                return Err(ModelsError::ProtocolNotWired {
                    model: id.clone(),
                    protocol: format!("{protocol:?}"),
                });
            }
        };
        if cfg.base_url.trim().is_empty() {
            return Err(ModelsError::InvalidBaseUrl {
                base_url: cfg.base_url.clone(),
                reason: "base_url is blank".to_string(),
            });
        }

        let key = self.api_key_for(cfg).await?;
        let endpoint = format!("{}{models_suffix}", cfg.base_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|error| ModelsError::ConnectionFailed {
                base_url: cfg.base_url.clone(),
                reason: error.to_string(),
            })?;
        // The exact header/prefix + extra headers the live client sends, so
        // an Azure-shaped provider (`api-key`) verifies with the same auth it
        // will run with — never a hardcoded bearer.
        let auth = endpoint_auth_for(cfg, self.catalog());
        let mut request = client.get(endpoint);
        for (name, value) in &auth.extra_headers {
            request = request.header(name, value);
        }
        if !key.is_empty() {
            let mut value =
                reqwest::header::HeaderValue::from_str(&format!("{}{key}", auth.prefix)).map_err(
                    |_| ModelsError::ModelUnavailable {
                        model: id.clone(),
                        provider_model: cfg.model.clone(),
                        reason: "the API key is not a valid header value".to_string(),
                    },
                )?;
            // Sensitive: reqwest redacts it from any error/debug output.
            value.set_sensitive(true);
            request = request.header(auth.header.as_str(), value);
        }
        let response = request
            .send()
            .await
            .map_err(|error| ModelsError::ConnectionFailed {
                base_url: cfg.base_url.clone(),
                reason: error.to_string(),
            })?;
        if !response.status().is_success() {
            return Err(ModelsError::ModelUnavailable {
                model: id.clone(),
                provider_model: cfg.model.clone(),
                reason: format!("provider returned HTTP {} from /models", response.status()),
            });
        }
        let payload: serde_json::Value =
            response
                .json()
                .await
                .map_err(|error| ModelsError::ModelUnavailable {
                    model: id.clone(),
                    provider_model: cfg.model.clone(),
                    reason: format!("provider returned an invalid /models response: {error}"),
                })?;
        let available = payload
            .get("data")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.get("id").and_then(serde_json::Value::as_str))
            .any(|candidate| candidate == cfg.model);
        if !available {
            return Err(ModelsError::ModelUnavailable {
                model: id.clone(),
                provider_model: cfg.model.clone(),
                reason: "provider did not list this model".to_string(),
            });
        }
        Ok(())
    }

    /// Build a framework chat client for `id`, dispatching on the model's wire
    /// [`Protocol`] and resolving credentials through the async
    /// `CredentialProvider` seam (`codypendent_providers::credential_for`).
    ///
    /// Resolves the API key right here, at call time, in precedence order:
    /// (1) `auth.json[model_id]`, when present and non-empty; (2) the
    /// provider-wide `auth.json` entry ([`provider_auth_id`]); (3) the
    /// model's `api_key_env` environment variable; (4) the catalog
    /// provider's documented env NAMES (best-effort); (5) no key at all
    /// (local endpoints with an empty `api_key_env`). Whichever wins is moved
    /// straight into the client and is never stored on the registry, logged,
    /// or otherwise retained by this function (Chapter 11, "Secrets"). A
    /// required-but-unset variable produces [`ModelsError::MissingApiKeyEnv`]
    /// naming the variable.
    ///
    /// [`Protocol::OpenAiChat`] and (when this build carries
    /// `provider-anthropic`, on by default) [`Protocol::Anthropic`] are
    /// wired. A legacy `models.toml` entry (`provider = "openai-compatible"`)
    /// maps onto one or the other via [`config_to_protocol_auth`], which
    /// consults the catalog: no `provider_id`, or one this build doesn't
    /// recognize, keeps the OpenAiChat default and builds the exact same
    /// `OpenAIChatCompletionClient::new(api_key, model).with_base_url(base_url)`
    /// as before — the one code path that serves both the hosted OpenAI
    /// endpoint and any OpenAI-compatible local/self-hosted endpoint (e.g.
    /// Ollama), per STEP 1.9 — now returned behind `Arc<dyn ChatClient>`. A
    /// [`ModelConfig::provider_id`] whose catalog auth is not the bearer
    /// default (or that declares extra headers) builds the wire-identical
    /// [`HeaderAuthChatClient`] instead, so Azure OpenAI / GitHub Models
    /// authenticate with the headers the provider actually expects.
    /// `provider_id = "anthropic"` (or any catalog provider declaring
    /// `protocol = "anthropic"`) builds an `agent_framework_anthropic::
    /// AnthropicClient` directly — it speaks the real Messages API wire
    /// (`x-api-key` + `anthropic-version`, its own request/response and SSE
    /// shapes) rather than being flattened through the OpenAI-chat path.
    /// Gemini native and any other undeclared protocol still return
    /// [`ModelsError::ProtocolNotWired`].
    pub async fn client_for(
        &self,
        id: &ModelId,
    ) -> Result<Arc<dyn agent_framework_core::client::ChatClient>> {
        let cfg = self
            .get(id)
            .ok_or_else(|| ModelsError::UnknownModel(id.clone()))?;
        let (protocol, _) = config_to_protocol_auth(cfg, self.catalog())?;
        match protocol {
            Protocol::OpenAiChat => {
                // Key resolution precedence (additive): (a) an `auth.json` key for
                // this model id wins → (b) the provider-wide `auth.json` entry →
                // (c) the model's `api_key_env` (today's path) → (d) none. A model
                // with no `auth.json` entry behaves exactly as before. The stored
                // key is moved straight into the client and is never logged or
                // retained by this function.
                let api_key = self.api_key_for(cfg).await?;
                let auth = endpoint_auth_for(cfg, self.catalog());
                if auth.is_framework_default() {
                    let client = OpenAIChatCompletionClient::new(api_key, cfg.model.clone())
                        .with_base_url(cfg.base_url.clone());
                    Ok(Arc::new(client))
                } else {
                    // A catalog-declared non-bearer header (Azure OpenAI's
                    // `api-key`) or provider extra headers (GitHub Models):
                    // the stock framework client hardcodes `bearer_auth`, so
                    // these run through the wire-identical header-aware client.
                    let client =
                        HeaderAuthChatClient::new(cfg, &auth, &api_key).ok_or_else(|| {
                            ModelsError::ModelUnavailable {
                                model: id.clone(),
                                provider_model: cfg.model.clone(),
                                reason: format!(
                                    "provider auth header `{}` or its value is not a valid \
                                     HTTP header",
                                    auth.header
                                ),
                            }
                        })?;
                    Ok(Arc::new(client))
                }
            }
            // Same key-resolution precedence as the OpenAiChat arm above;
            // `AnthropicClient` sends `x-api-key`/`anthropic-version` itself; it
            // is not routed through `HeaderAuthChatClient` because it is not an
            // OpenAI-chat-completions body over different headers — the request
            // and response shapes themselves differ (Messages API), so the
            // framework's purpose-built client owns the whole wire.
            #[cfg(feature = "provider-anthropic")]
            Protocol::Anthropic => {
                let api_key = self.api_key_for(cfg).await?;
                let client =
                    agent_framework_anthropic::AnthropicClient::new(api_key, cfg.model.clone())
                        .with_base_url(cfg.base_url.clone());
                Ok(Arc::new(client))
            }
            Protocol::Acp => Err(ModelsError::ProtocolNotWired {
                model: id.clone(),
                protocol: "acp (full-agent executor, not ChatClient)".to_string(),
            }),
            other => Err(ModelsError::ProtocolNotWired {
                model: id.clone(),
                protocol: format!("{other:?}"),
            }),
        }
    }
}

/// An OpenAI-compatible chat client for providers whose auth is NOT the
/// framework default `Authorization: Bearer …` (Azure OpenAI's `api-key`
/// header) or that require provider-wide extra headers (GitHub Models'
/// `X-GitHub-Api-Version`). `OpenAIChatCompletionClient` hardcodes
/// `bearer_auth` with no header hook, so this client mirrors it
/// request-for-request by reusing the framework's own public conversion and
/// SSE-parsing helpers — only header injection differs. The auth header value
/// is marked sensitive so reqwest never echoes it into an error.
#[cfg(feature = "provider-openai")]
struct HeaderAuthChatClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
    headers: reqwest::header::HeaderMap,
}

#[cfg(feature = "provider-openai")]
impl HeaderAuthChatClient {
    /// Build the client, assembling the fixed header set: the provider's
    /// extra headers plus (when a key is present) `header: prefix+key`,
    /// sensitive. `None` when a header name/value cannot be represented —
    /// the caller maps that to a legible config error naming no key.
    fn new(cfg: &ModelConfig, auth: &EndpointAuth, api_key: &str) -> Option<Self> {
        use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
        let mut headers = HeaderMap::new();
        for (name, value) in &auth.extra_headers {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).ok()?,
                HeaderValue::from_str(value).ok()?,
            );
        }
        if !api_key.is_empty() {
            let mut value = HeaderValue::from_str(&format!("{}{api_key}", auth.prefix)).ok()?;
            value.set_sensitive(true);
            headers.insert(HeaderName::from_bytes(auth.header.as_bytes()).ok()?, value);
        }
        Some(Self {
            http: reqwest::Client::new(),
            base_url: cfg.base_url.clone(),
            model: cfg.model.clone(),
            headers,
        })
    }

    /// The exact chat-completions body the stock client builds, via the
    /// framework's own public conversion helpers.
    fn build_body(
        &self,
        messages: &[agent_framework_core::types::Message],
        options: &agent_framework_core::types::ChatOptions,
        stream: bool,
    ) -> serde_json::Value {
        use agent_framework_openai::convert;
        let mut body = serde_json::Map::new();
        let model = options.model.clone().unwrap_or_else(|| self.model.clone());
        body.insert("model".into(), serde_json::json!(model));
        body.insert(
            "messages".into(),
            serde_json::json!(convert::messages_to_openai(messages)),
        );
        convert::apply_options(&mut body, options);
        let (tools, tool_choice) = convert::tools_to_openai(options);
        if let Some(tools) = tools {
            body.insert("tools".into(), tools);
        }
        if let Some(choice) = tool_choice {
            body.insert("tool_choice".into(), choice);
        }
        if stream {
            body.insert("stream".into(), serde_json::json!(true));
            body.insert(
                "stream_options".into(),
                serde_json::json!({ "include_usage": true }),
            );
        }
        serde_json::Value::Object(body)
    }

    async fn post(
        &self,
        body: &serde_json::Value,
    ) -> agent_framework_core::error::Result<reqwest::Response> {
        use agent_framework_core::error::Error;
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .post(&url)
            .headers(self.headers.clone())
            .json(body)
            .send()
            .await
            .map_err(|e| Error::service(format!("request failed: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(agent_framework_openai::classify_service_error(
                status.as_u16(),
                &text,
                format!("OpenAI-compatible API error {status}: {text}"),
                None,
            ));
        }
        Ok(resp)
    }
}

#[cfg(feature = "provider-openai")]
#[async_trait::async_trait]
impl agent_framework_core::client::ChatClient for HeaderAuthChatClient {
    async fn get_response(
        &self,
        messages: Vec<agent_framework_core::types::Message>,
        options: agent_framework_core::types::ChatOptions,
    ) -> agent_framework_core::error::Result<agent_framework_core::types::ChatResponse> {
        use agent_framework_core::error::Error;
        let body = self.build_body(&messages, &options, false);
        let resp = self.post(&body).await?;
        let value: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| Error::service(format!("invalid response json: {e}")))?;
        Ok(agent_framework_openai::convert::parse_response(&value))
    }

    async fn get_streaming_response(
        &self,
        messages: Vec<agent_framework_core::types::Message>,
        options: agent_framework_core::types::ChatOptions,
    ) -> agent_framework_core::error::Result<agent_framework_core::client::ChatStream> {
        use futures::StreamExt;
        let body = self.build_body(&messages, &options, true);
        let resp = self.post(&body).await?;
        Ok(agent_framework_openai::parse_sse_stream(resp).boxed())
    }

    fn model(&self) -> Option<&str> {
        Some(&self.model)
    }
}

// ---------------------------------------------------------------------------
// Phase 1 model policy
// ---------------------------------------------------------------------------

/// The Phase 1 model policy: an ordered candidate [`ModelId`] list per
/// [`AgentMode`], with an optional fallback list for modes with no explicit
/// entry.
///
/// This is intentionally minimal — a static ordered list, walked in order by
/// [`resolve_model`] until one connects. It is *not* the Phase 7 utility
/// router (cost/latency/quality-aware routing arrives there); see STEP 1.9.
#[derive(Debug, Clone, Default)]
pub struct ModelPolicy {
    per_mode: Vec<(AgentMode, Vec<ModelId>)>,
    default_candidates: Vec<ModelId>,
}

impl ModelPolicy {
    /// An empty policy (every mode resolves to no candidates until
    /// configured).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or replace) the ordered candidate list for `mode`.
    pub fn with_candidates(mut self, mode: AgentMode, candidates: impl Into<Vec<ModelId>>) -> Self {
        let candidates = candidates.into();
        match self.per_mode.iter_mut().find(|(m, _)| *m == mode) {
            Some(entry) => entry.1 = candidates,
            None => self.per_mode.push((mode, candidates)),
        }
        self
    }

    /// Set the fallback candidate list used by [`ModelPolicy::candidates`]
    /// for any mode without its own entry.
    pub fn with_default_candidates(mut self, candidates: impl Into<Vec<ModelId>>) -> Self {
        self.default_candidates = candidates.into();
        self
    }

    /// The ordered candidate list for `mode`: its own entry if configured,
    /// otherwise the default list (possibly empty).
    pub fn candidates(&self, mode: AgentMode) -> &[ModelId] {
        self.per_mode
            .iter()
            .find(|(m, _)| *m == mode)
            .map(|(_, c)| c.as_slice())
            .unwrap_or(&self.default_candidates)
    }
}

// ---------------------------------------------------------------------------
// Connectivity probing + resolution
// ---------------------------------------------------------------------------

/// A pluggable "is this endpoint reachable" check, used by [`resolve_model`]
/// to walk a policy's candidates.
///
/// Kept as a small abstraction — rather than hard-coding a real network call
/// inline in the resolution loop — for two reasons: it keeps candidate
/// *selection* free of any dependency on `provider-openai` (a raw TCP check
/// needs to know nothing about the OpenAI wire format), and it makes the
/// fallback-ordering logic in [`resolve_model_with_probe`] deterministically
/// testable without needing a real (and possibly costly) model call. The
/// default implementation, [`TcpConnectProbe`], performs a genuine TCP
/// connect attempt (not a canned/fake result), so the connect-refused test
/// exercises real OS-level connection failure rather than a mocked one.
#[async_trait::async_trait]
pub trait ConnectivityProbe: Send + Sync {
    /// Attempt to reach `base_url`. `Ok(())` means reachable.
    async fn check(&self, base_url: &str) -> Result<()>;
}

/// The default [`ConnectivityProbe`]: a raw TCP connect to the `base_url`'s
/// `host:port`, with a timeout.
///
/// A TCP-level check (rather than a full HTTP request, and deliberately far
/// short of a real chat completion) is intentional: selecting *which* model
/// serves a run should not itself burn API quota or require an already-valid
/// API key, and it needs to work identically for every provider wire format
/// this crate might ever support. Parsing the authority out of `base_url` is
/// done by hand (`str::split`) rather than via a URL-parsing crate because
/// none is available in this crate's dependency set; it handles the
/// `scheme://host[:port]/path` shape `models.toml` uses and is not a general
/// URL parser (e.g. it does not handle bracketed IPv6 literals).
#[derive(Debug, Clone)]
pub struct TcpConnectProbe {
    pub timeout: Duration,
}

impl Default for TcpConnectProbe {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(2),
        }
    }
}

#[async_trait::async_trait]
impl ConnectivityProbe for TcpConnectProbe {
    async fn check(&self, base_url: &str) -> Result<()> {
        let authority = authority_from_base_url(base_url)?;
        match tokio::time::timeout(self.timeout, tokio::net::TcpStream::connect(&authority)).await {
            Ok(Ok(_stream)) => Ok(()),
            Ok(Err(source)) => Err(ModelsError::ConnectionFailed {
                base_url: base_url.to_string(),
                reason: source.to_string(),
            }),
            Err(_elapsed) => Err(ModelsError::ConnectionFailed {
                base_url: base_url.to_string(),
                reason: "connection attempt timed out".to_string(),
            }),
        }
    }
}

/// Reduce a `scheme://host[:port]/path...` base URL to a `host:port`
/// authority suitable for `TcpStream::connect`. Defaults to port 80 for
/// `http://` and 443 for `https://` when no port is given.
fn authority_from_base_url(base_url: &str) -> Result<String> {
    let rest = base_url.split_once("://").map_or(base_url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return Err(ModelsError::InvalidBaseUrl {
            base_url: base_url.to_string(),
            reason: "no host found in base_url".to_string(),
        });
    }
    let has_explicit_port = authority
        .rsplit_once(':')
        .is_some_and(|(_, port)| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()));
    if has_explicit_port {
        Ok(authority.to_string())
    } else {
        let default_port = if base_url.starts_with("https://") {
            443
        } else {
            80
        };
        Ok(format!("{authority}:{default_port}"))
    }
}

/// The outcome of [`resolve_model`]: which candidate was selected, so the
/// caller (the agent loop) can attribute the run to this model id, per
/// STEP 1.9 / STEP 1.10 rule 3 ("every model request records: model id, ...").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModel {
    pub id: ModelId,
}

/// Walk `policy`'s candidates for `mode` in order, returning the first model
/// whose credentials resolve and whose provider `/models` catalog contains the
/// configured model name. This prevents an open Ollama port with a stale model
/// tag from being selected and failing the run immediately afterward.
///
/// See [`resolve_model_with_probe`] for the fallback semantics and for
/// injecting a different probe.
pub async fn resolve_model(
    registry: &ModelRegistry,
    policy: &ModelPolicy,
    mode: AgentMode,
) -> Result<ResolvedModel> {
    #[cfg(not(feature = "provider-openai"))]
    {
        return resolve_model_with_probe(registry, policy, mode, &TcpConnectProbe::default()).await;
    }

    #[cfg(feature = "provider-openai")]
    {
        let candidates = policy.candidates(mode);
        if candidates.is_empty() {
            return Err(ModelsError::NoCandidates { mode });
        }
        let mut attempts = Vec::with_capacity(candidates.len());
        for id in candidates {
            if registry.get(id).is_none() {
                attempts.push((id.clone(), "model not registered".to_string()));
                continue;
            }
            match registry.check_model(id).await {
                Ok(()) => return Ok(ResolvedModel { id: id.clone() }),
                Err(error) => attempts.push((id.clone(), error.to_string())),
            }
        }
        Err(ModelsError::AllCandidatesFailed { mode, attempts })
    }
}

/// Walk `policy`'s candidates for `mode` in order. For each candidate: if it
/// has no registry entry, or `probe.check` on its `base_url` fails, move to
/// the next candidate; the first one that connects is returned. If every
/// candidate fails, returns [`ModelsError::AllCandidatesFailed`] carrying
/// every attempt's id and reason, in order.
pub async fn resolve_model_with_probe(
    registry: &ModelRegistry,
    policy: &ModelPolicy,
    mode: AgentMode,
    probe: &dyn ConnectivityProbe,
) -> Result<ResolvedModel> {
    let candidates = policy.candidates(mode);
    if candidates.is_empty() {
        return Err(ModelsError::NoCandidates { mode });
    }
    let mut attempts = Vec::with_capacity(candidates.len());
    for id in candidates {
        let Some(cfg) = registry.get(id) else {
            attempts.push((id.clone(), "model not registered".to_string()));
            continue;
        };
        match probe.check(&cfg.base_url).await {
            Ok(()) => return Ok(ResolvedModel { id: id.clone() }),
            Err(e) => attempts.push((id.clone(), e.to_string())),
        }
    }
    Err(ModelsError::AllCandidatesFailed { mode, attempts })
}

// ===========================================================================
// Audio: speech-to-text and text-to-speech (voice v1, rubric 8)
// ===========================================================================
//
// Both ride the SAME OpenAI-compatible plumbing the chat profiles use — the
// `/audio/transcriptions` and `/audio/speech` endpoints are served by Groq
// (`whisper-large-v3`, `whisper-large-v3-turbo`), OpenAI (`gpt-4o-transcribe`,
// `gpt-4o-mini-transcribe`, `tts-1`, `gpt-4o-mini-tts`), DeepInfra and Together,
// so voice needs no new provider protocol. Configuration is two OPTIONAL tables
// in the existing `models.toml`, parsed independently of `[[model]]` so a file
// without them is unchanged:
//
// ```toml
// [transcription]
// base_url = "https://api.groq.com/openai/v1"
// model = "whisper-large-v3-turbo"
// api_key_env = "GROQ_API_KEY"
// # local = true   # set for an on-device engine, so classified audio is
// #                # allowed to be transcribed under any policy ceiling
//
// [speech]
// base_url = "https://api.openai.com/v1"
// model = "gpt-4o-mini-tts"
// voice = "alloy"
// api_key_env = "OPENAI_API_KEY"
// ```
//
// NOTE ON VERIFICATION: the machine this was written on has NO audio hardware
// and no provider credentials. Every test below drives a wiremock server and
// fixture bytes. Nothing here is evidence that a real provider's response shape
// or a real audio device behaves as expected.

/// One `[transcription]` / `[speech]` table in `models.toml`.
///
/// Deliberately its own type rather than a reuse of [`ModelConfig`]: an audio
/// profile has no [`ModelId`] (there is exactly one of each, selected by the
/// table name, not by a policy candidate list) and TTS carries a `voice` that
/// means nothing to a chat profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioModelConfig {
    /// The OpenAI-compatible base URL, e.g. `https://api.groq.com/openai/v1`.
    /// The endpoint path (`/audio/transcriptions`, `/audio/speech`) is appended.
    pub base_url: String,
    /// The provider-side model name, e.g. `whisper-large-v3-turbo`.
    pub model: String,
    /// The NAME of the environment variable holding the API key, read at call
    /// time and never stored. Empty means no key (a local endpoint).
    #[serde(default)]
    pub api_key_env: String,
    /// TTS only: the provider's voice name, e.g. `alloy`. Ignored for STT.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// TTS only: the requested container, e.g. `mp3`/`wav`/`opus`. Sent verbatim
    /// as `response_format`; omitted lets the provider choose its default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Whether this endpoint runs ON THIS DEVICE (a local whisper.cpp server,
    /// an Ollama-hosted ASR). This is the single knob that decides whether the
    /// daemon's classification gate treats a transcription as
    /// [`TranscriptionMode::Local`](codypendent_protocol::input::TranscriptionMode)
    /// — under which even `Confidential` audio may be transcribed — or as
    /// `Remote`, which the operator's off-device ceiling governs. It defaults to
    /// `false`: an unmarked endpoint is assumed to leave the device, so the
    /// safe classification is the one you get by saying nothing.
    #[serde(default)]
    pub local: bool,
    /// Request timeout in seconds. Audio requests are slower than chat ones (a
    /// minute of speech is a megabyte-scale upload), hence the generous default.
    #[serde(default = "default_audio_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_audio_timeout_secs() -> u64 {
    120
}

/// The optional audio tables in `models.toml`. Parsed with its OWN struct
/// (rather than by extending the `[[model]]` file shape) so an existing
/// `models.toml` — and every existing reader of it — is entirely unaffected.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AudioModels {
    /// The `[transcription]` table: the speech-to-text endpoint.
    #[serde(default)]
    pub transcription: Option<AudioModelConfig>,
    /// The `[speech]` table: the text-to-speech endpoint.
    #[serde(default)]
    pub speech: Option<AudioModelConfig>,
}

/// Parse the `[transcription]` / `[speech]` tables from `models.toml`.
///
/// A missing FILE yields an empty [`AudioModels`] (voice is simply not
/// configured — the overwhelmingly common case), but a file that exists and
/// does not parse is an error, so a typo in a voice table surfaces instead of
/// silently disabling voice.
pub fn load_audio_models(path: &Path) -> Result<AudioModels> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AudioModels::default())
        }
        Err(source) => {
            return Err(ModelsError::ReadConfig {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    toml::from_str(&text).map_err(|source| ModelsError::ParseConfig {
        path: path.to_path_buf(),
        source,
    })
}

/// Errors from the audio clients and the playback command.
///
/// Its own enum rather than new [`ModelsError`] variants: the audio path has
/// failure modes (no player configured, a spawn failure) that have nothing to
/// do with chat model resolution, and keeping them separate means a caller can
/// exhaustively match one without the other.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AudioError {
    /// `models.toml` has no `[transcription]` / `[speech]` table.
    #[error("no [{table}] entry in models.toml; voice {feature} is not configured")]
    NotConfigured {
        /// The missing table name.
        table: &'static str,
        /// What it would have enabled, for the message.
        feature: &'static str,
    },
    /// The configured `api_key_env` names an unset environment variable.
    #[error("environment variable {var} is not set (needed by the [{table}] entry)")]
    MissingApiKeyEnv {
        /// The table whose `api_key_env` could not be resolved.
        table: &'static str,
        /// The environment variable NAME (never a value).
        var: String,
    },
    /// The request could not be sent (DNS, TLS, connect, timeout).
    #[error("audio request to {url} failed: {source}")]
    Transport {
        /// The endpoint that was called.
        url: String,
        /// The underlying transport error.
        source: reqwest::Error,
    },
    /// The provider answered with a non-success status.
    #[error("audio endpoint {url} returned {status}: {body}")]
    Status {
        /// The endpoint that was called.
        url: String,
        /// The HTTP status.
        status: u16,
        /// The (truncated) response body, for diagnosis.
        body: String,
    },
    /// The provider's response did not have the documented shape.
    #[error("audio endpoint {url} returned an unreadable response: {detail}")]
    MalformedResponse {
        /// The endpoint that was called.
        url: String,
        /// What was wrong.
        detail: String,
    },
    /// Playback was requested with no `play_command` configured.
    #[error("no voice play_command is configured; set voice.play_command in config.toml (e.g. [\"mpv\", \"--no-terminal\", \"-\"])")]
    NoPlayer,
    /// The playback command could not be started or fed.
    #[error("could not run the playback command {command:?}: {source}")]
    Playback {
        /// The configured command, for the message.
        command: Vec<String>,
        /// The spawn/write failure.
        source: std::io::Error,
    },
}

/// Join a base URL and an endpoint path without doubling or dropping the slash.
fn audio_url(base_url: &str, path: &str) -> String {
    format!("{}/{}", base_url.trim_end_matches('/'), path)
}

/// Resolve an audio profile's API key with the SAME precedence a chat profile
/// uses: an `auth.json` entry first (keyed by the table name, so a TUI-saved key
/// applies), then the configured environment variable via the shared
/// [`credential_for`](codypendent_providers::credential_for) seam. An empty
/// `api_key_env` means "no key needed" and resolves to an empty string.
async fn audio_api_key(
    config: &AudioModelConfig,
    auth: &AuthStore,
    table: &'static str,
) -> std::result::Result<String, AudioError> {
    if let Some(key) = auth.get(table).filter(|key| !key.is_empty()) {
        return Ok(key.to_string());
    }
    if config.api_key_env.trim().is_empty() {
        return Ok(String::new());
    }
    let method = codypendent_providers::AuthMethod::ApiKey {
        env: vec![config.api_key_env.clone()],
        header: "Authorization".to_string(),
        prefix: "Bearer ".to_string(),
    };
    match codypendent_providers::credential_for(&method)
        .resolve()
        .await
    {
        Ok(codypendent_providers::ResolvedCredential::ApiKey { value, .. }) => Ok(value),
        Ok(codypendent_providers::ResolvedCredential::None) => Ok(String::new()),
        Err(codypendent_providers::CredentialError::MissingEnv { var }) => {
            Err(AudioError::MissingApiKeyEnv { table, var })
        }
        Err(other) => Err(AudioError::MissingApiKeyEnv {
            table,
            var: other.to_string(),
        }),
    }
}

/// Read a response body as text, truncated so a provider's HTML error page
/// cannot flood a log line or an error message.
async fn error_body(response: reqwest::Response) -> String {
    let mut body = response.text().await.unwrap_or_default();
    body.truncate(512);
    body
}

/// A speech-to-text client over an OpenAI-compatible
/// `{base_url}/audio/transcriptions` endpoint.
///
/// The request is `multipart/form-data` with a `file` part (the audio bytes)
/// and a `model` part, exactly as Groq/OpenAI/DeepInfra/Together document. The
/// multipart body is assembled by hand rather than through `reqwest`'s
/// `multipart` feature, so voice adds no new feature flag to the workspace's
/// shared HTTP dependency; the boundary is derived from the payload's own
/// SHA-256, which cannot collide with content it is a digest of.
#[derive(Debug, Clone)]
pub struct AudioTranscriber {
    config: AudioModelConfig,
    auth: AuthStore,
    http: reqwest::Client,
}

impl AudioTranscriber {
    /// Build a transcriber from the `[transcription]` table, or
    /// [`AudioError::NotConfigured`] when there is none.
    pub fn new(models: &AudioModels, auth: AuthStore) -> std::result::Result<Self, AudioError> {
        let config = models
            .transcription
            .clone()
            .ok_or(AudioError::NotConfigured {
                table: "transcription",
                feature: "input (speech-to-text)",
            })?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .unwrap_or_default();
        Ok(Self { config, auth, http })
    }

    /// The configured profile (endpoint, model name, locality).
    #[must_use]
    pub fn config(&self) -> &AudioModelConfig {
        &self.config
    }

    /// Whether this endpoint runs on-device — what the daemon's classification
    /// gate reads to pick `Local` vs `Remote`.
    #[must_use]
    pub fn is_local(&self) -> bool {
        self.config.local
    }

    /// Transcribe `bytes` (an audio container named `filename`, whose extension
    /// is how most providers detect the format) and return the recognized text.
    pub async fn transcribe(
        &self,
        bytes: &[u8],
        filename: &str,
        media_type: &str,
    ) -> std::result::Result<String, AudioError> {
        let url = audio_url(&self.config.base_url, "audio/transcriptions");
        let key = audio_api_key(&self.config, &self.auth, "transcription").await?;
        let boundary = multipart_boundary(bytes);
        let body = multipart_body(&boundary, bytes, filename, media_type, &self.config.model);

        let mut request = self
            .http
            .post(&url)
            .header(
                reqwest::header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body);
        if !key.is_empty() {
            request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {key}"));
        }
        let response = request
            .send()
            .await
            .map_err(|source| AudioError::Transport {
                url: url.clone(),
                source,
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(AudioError::Status {
                url,
                status: status.as_u16(),
                body: error_body(response).await,
            });
        }
        let payload: serde_json::Value =
            response
                .json()
                .await
                .map_err(|source| AudioError::MalformedResponse {
                    url: url.clone(),
                    detail: source.to_string(),
                })?;
        payload
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or(AudioError::MalformedResponse {
                url,
                detail: "no string `text` field in the response".to_string(),
            })
    }
}

/// A boundary derived from the payload's SHA-256. A multipart boundary must not
/// occur inside any part; a digest of the very bytes it delimits cannot, short
/// of a preimage the payload would have to contain of itself.
fn multipart_boundary(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("codypendentaudio{}", hex::encode(&hasher.finalize()[..12]))
}

/// Assemble the `multipart/form-data` body: a `file` part then a `model` part.
fn multipart_body(
    boundary: &str,
    bytes: &[u8],
    filename: &str,
    media_type: &str,
    model: &str,
) -> Vec<u8> {
    let mut body = Vec::with_capacity(bytes.len() + 512);
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {media_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
    body.extend_from_slice(model.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

/// Audio a [`AudioSynthesizer`] produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynthesizedSpeech {
    /// The encoded audio bytes, in whatever container the provider returned.
    pub bytes: Vec<u8>,
    /// The `Content-Type` the provider reported, e.g. `audio/mpeg`.
    pub media_type: String,
}

/// A text-to-speech client over an OpenAI-compatible `{base_url}/audio/speech`
/// endpoint (Groq, OpenAI, DeepInfra, Together). The reply is raw audio bytes,
/// not JSON.
#[derive(Debug, Clone)]
pub struct AudioSynthesizer {
    config: AudioModelConfig,
    auth: AuthStore,
    http: reqwest::Client,
}

impl AudioSynthesizer {
    /// Build a synthesizer from the `[speech]` table, or
    /// [`AudioError::NotConfigured`] when there is none.
    pub fn new(models: &AudioModels, auth: AuthStore) -> std::result::Result<Self, AudioError> {
        let config = models.speech.clone().ok_or(AudioError::NotConfigured {
            table: "speech",
            feature: "output (text-to-speech)",
        })?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .unwrap_or_default();
        Ok(Self { config, auth, http })
    }

    /// The configured profile.
    #[must_use]
    pub fn config(&self) -> &AudioModelConfig {
        &self.config
    }

    /// Synthesize `text` into audio bytes.
    pub async fn synthesize(
        &self,
        text: &str,
    ) -> std::result::Result<SynthesizedSpeech, AudioError> {
        let url = audio_url(&self.config.base_url, "audio/speech");
        let key = audio_api_key(&self.config, &self.auth, "speech").await?;
        let mut payload = serde_json::json!({
            "model": self.config.model,
            "input": text,
        });
        if let Some(voice) = &self.config.voice {
            payload["voice"] = serde_json::Value::String(voice.clone());
        }
        if let Some(format) = &self.config.format {
            payload["response_format"] = serde_json::Value::String(format.clone());
        }

        let mut request = self.http.post(&url).json(&payload);
        if !key.is_empty() {
            request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {key}"));
        }
        let response = request
            .send()
            .await
            .map_err(|source| AudioError::Transport {
                url: url.clone(),
                source,
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(AudioError::Status {
                url,
                status: status.as_u16(),
                body: error_body(response).await,
            });
        }
        let media_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("audio/mpeg")
            .to_string();
        let bytes = response
            .bytes()
            .await
            .map_err(|source| AudioError::Transport {
                url: url.clone(),
                source,
            })?
            .to_vec();
        if bytes.is_empty() {
            return Err(AudioError::MalformedResponse {
                url,
                detail: "the speech endpoint returned no audio bytes".to_string(),
            });
        }
        Ok(SynthesizedSpeech { bytes, media_type })
    }
}

/// Plays synthesized audio by piping it to a USER-CONFIGURED command's stdin.
///
/// There is no bundled audio backend on purpose: shipping one would mean a
/// platform-specific native dependency for a strictly optional feature, and
/// every desktop already has a player that reads stdin (`mpv --no-terminal -`,
/// `ffplay -nodisp -autoexit -`, `paplay`, `afplay` via a temp file). The
/// operator names theirs in `config.toml`.
///
/// The child is spawned **detached**: its handle is dropped immediately, so a
/// long clip never blocks the caller's task, and its stdout/stderr are silenced
/// so a chatty player cannot corrupt a TUI's terminal. Nothing here has been
/// exercised against a real audio device — this container has none; the tests
/// substitute a `cat > file` "player" and assert the bytes arrive on its stdin.
#[derive(Debug, Clone, Default)]
pub struct AudioPlayer {
    command: Vec<String>,
}

impl AudioPlayer {
    /// A player driven by `command` (program plus arguments). An empty command
    /// means "unconfigured": [`play`](Self::play) then fails
    /// [`AudioError::NoPlayer`] instead of guessing a binary.
    #[must_use]
    pub fn new(command: Vec<String>) -> Self {
        Self { command }
    }

    /// Whether a playback command is configured.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        !self.command.is_empty()
    }

    /// Pipe `bytes` to the configured command's stdin and return without
    /// waiting for it to finish.
    ///
    /// The write itself is awaited (so a spawn/pipe failure is reported rather
    /// than silently swallowed) but the process is NOT waited on. A player that
    /// outlives the caller is intentional: playback continues while the UI
    /// carries on. Note this leaves the child unreaped by this process; callers
    /// that spawn many should run this from a task whose runtime reaps children
    /// (tokio's `Command` does so via its signal handling).
    pub async fn play(&self, bytes: &[u8]) -> std::result::Result<(), AudioError> {
        use tokio::io::AsyncWriteExt as _;

        let Some((program, args)) = self.command.split_first() else {
            return Err(AudioError::NoPlayer);
        };
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(false)
            .spawn()
            .map_err(|source| AudioError::Playback {
                command: self.command.clone(),
                source,
            })?;
        let mut stdin = child.stdin.take().ok_or_else(|| AudioError::Playback {
            command: self.command.clone(),
            source: std::io::Error::other("the playback command has no stdin"),
        })?;
        stdin
            .write_all(bytes)
            .await
            .map_err(|source| AudioError::Playback {
                command: self.command.clone(),
                source,
            })?;
        // Closing stdin is what tells the player the clip is complete.
        stdin.shutdown().await.ok();
        drop(stdin);
        // Detach: the clip plays on while this task returns immediately.
        drop(child);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn model_id(s: &str) -> ModelId {
        ModelId(s.to_string())
    }

    // -- config parse --------------------------------------------------

    #[test]
    fn parses_two_model_entries_from_toml() {
        let mut file = tempfile::NamedTempFile::new().expect("create temp file");
        write!(
            file,
            r#"
[[model]]
id = "hosted-default"
provider = "openai-compatible"
base_url = "https://api.openai.com/v1"
model = "gpt-5.1-codex"
api_key_env = "OPENAI_API_KEY"

[[model]]
id = "local-default"
provider = "openai-compatible"
base_url = "http://localhost:11434/v1"
model = "qwen2.5-coder:14b"
api_key_env = ""
"#
        )
        .expect("write temp file");

        let configs = load_models(file.path()).expect("parse models.toml");
        assert_eq!(configs.len(), 2);

        assert_eq!(configs[0].id, model_id("hosted-default"));
        assert_eq!(configs[0].provider, "openai-compatible");
        assert_eq!(configs[0].base_url, "https://api.openai.com/v1");
        assert_eq!(configs[0].model, "gpt-5.1-codex");
        assert_eq!(configs[0].api_key_env, "OPENAI_API_KEY");

        assert_eq!(configs[1].id, model_id("local-default"));
        assert_eq!(configs[1].base_url, "http://localhost:11434/v1");
        assert_eq!(configs[1].model, "qwen2.5-coder:14b");
        assert_eq!(configs[1].api_key_env, "");

        // Neither entry sets `context_tokens` — both must default to `None`
        // (back-compatible: an existing models.toml with no such key parses
        // unchanged), never a fabricated window.
        assert_eq!(configs[0].context_tokens, None);
        assert_eq!(configs[1].context_tokens, None);

        // ModelRegistry::load goes through the same path and should agree.
        let registry = ModelRegistry::load(file.path()).expect("load registry");
        assert!(registry.get(&model_id("hosted-default")).is_some());
        assert!(registry.get(&model_id("local-default")).is_some());
        assert_eq!(registry.ids().count(), 2);
    }

    #[tokio::test]
    async fn acp_profile_is_selectable_but_not_exposed_as_a_chat_client() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("models.toml");
        std::fs::write(
            &path,
            r#"[[model]]
id = "acp/codex-acp"
provider = "acp"
model = "codex-acp"
"#,
        )
        .expect("write minimal ACP config");
        let parsed = load_models(&path).expect("ACP does not need base_url/api_key_env");
        assert_eq!(parsed[0].base_url, "");
        assert_eq!(parsed[0].api_key_env, "");

        let config = ModelConfig {
            id: model_id("acp/codex-acp"),
            provider: "acp".to_string(),
            base_url: String::new(),
            model: "codex-acp".to_string(),
            api_key_env: String::new(),
            context_tokens: None,
            provider_id: None,
        };
        let registry = ModelRegistry::new(vec![config]);
        assert!(registry.is_acp(&model_id("acp/codex-acp")));
        assert_eq!(
            registry.acp_agent_id(&model_id("acp/codex-acp")),
            Some("codex-acp")
        );
        registry
            .check_model(&model_id("acp/codex-acp"))
            .await
            .expect("assembly executor owns ACP readiness");
        assert!(matches!(
            registry.client_for(&model_id("acp/codex-acp")).await,
            Err(ModelsError::ProtocolNotWired { .. })
        ));
    }

    #[test]
    fn context_tokens_parses_when_present_and_defaults_to_none_when_absent() {
        // Context-window protection (BT1): `context_tokens` is additive and
        // optional. A user who sets it under a `[[model]]` entry gets it back
        // as `Some(n)`; a config that omits it (the existing/back-compat
        // shape) must still parse, defaulting to `None` — never a fabricated
        // window.
        let mut file = tempfile::NamedTempFile::new().expect("create temp file");
        write!(
            file,
            r#"
[[model]]
id = "local-default"
provider = "openai-compatible"
base_url = "http://localhost:11434/v1"
model = "qwen2.5-coder:14b"
api_key_env = ""
context_tokens = 32768

[[model]]
id = "hosted-default"
provider = "openai-compatible"
base_url = "https://api.openai.com/v1"
model = "gpt-5.1-codex"
api_key_env = "OPENAI_API_KEY"
"#
        )
        .expect("write temp file");

        let configs = load_models(file.path()).expect("parse models.toml");
        assert_eq!(configs.len(), 2);
        assert_eq!(
            configs[0].context_tokens,
            Some(32_768),
            "an explicit context_tokens must parse verbatim"
        );
        assert_eq!(
            configs[1].context_tokens, None,
            "an entry with no context_tokens key must default to None, not a fabricated value"
        );
    }

    /// F4 (`2026-08-13-verticals/acp-models.md`): `crates/cli/src/tui.rs`'s
    /// `merge_catalog_rows` lets a provider's own `/models` response win over
    /// the curated catalog's `context_tokens` with no upper-bound check
    /// (`context_tokens.filter(|tokens| *tokens > 0)` is the only validation),
    /// and that value is persisted into `models.toml` verbatim, then forwarded
    /// as Ollama's `num_ctx` request hint and used as the TUI footer's
    /// context-usage denominator — "a misconfigured or hostile OpenAI-compatible
    /// gateway that reports `"context_length": 18446744073709551615` gets that
    /// number sent back as `num_ctx`". (The literal `u64::MAX` cannot itself
    /// ride through a `models.toml` round trip — TOML integers are signed
    /// 64-bit, so a provider's `serde_json` `u64::MAX` fails to serialize as a
    /// TOML integer at write time, before this clamp is even reached; this
    /// test uses a value that DOES survive the round trip — still absurd for
    /// any real model, still well under `i64::MAX` — to pin the clamp for
    /// every value that actually reaches parsing.) `load_models` is the one
    /// place every `models.toml` entry is parsed regardless of which writer
    /// produced it; this pins that an implausible reading never survives it.
    #[test]
    fn load_models_clamps_an_implausible_context_tokens_reading() {
        let mut file = tempfile::NamedTempFile::new().expect("create temp file");
        write!(
            file,
            r#"
[[model]]
id = "hostile-gateway"
provider = "openai-compatible"
base_url = "https://gateway.example/v1"
model = "some-model"
api_key_env = ""
context_tokens = 999999999999999
"#
        )
        .expect("write temp file");

        let configs = load_models(file.path()).expect("parse models.toml");
        assert_eq!(
            configs[0].context_tokens,
            Some(MAX_PLAUSIBLE_CONTEXT_TOKENS),
            "an implausible reading must be clamped, not passed through to num_ctx verbatim"
        );
    }

    #[test]
    fn clamp_context_tokens_passes_none_and_plausible_values_through_unchanged() {
        assert_eq!(clamp_context_tokens(None), None);
        assert_eq!(clamp_context_tokens(Some(200_000)), Some(200_000));
        assert_eq!(
            clamp_context_tokens(Some(MAX_PLAUSIBLE_CONTEXT_TOKENS)),
            Some(MAX_PLAUSIBLE_CONTEXT_TOKENS),
            "the ceiling itself is not clamped down further"
        );
    }

    /// [`ModelRegistry::context_tokens_for`] applies the TIGHTER of the two
    /// ceilings: an implausible-but-under-the-absolute-max reading (below
    /// [`MAX_PLAUSIBLE_CONTEXT_TOKENS`], so [`clamp_context_tokens`] alone
    /// would not catch it) must still be capped to the specific catalog row's
    /// own documented `context_tokens` when the config names a known
    /// `provider_id` + provider-side `model`.
    #[cfg(feature = "provider-openai")]
    #[test]
    fn context_tokens_for_clamps_to_the_specific_catalog_rows_ceiling() {
        let provider_toml = r#"
[[provider]]
id = "anthropic"
name = "Anthropic (Claude)"
protocol = "anthropic"
base_url = "https://api.anthropic.com"
[[provider.auth]]
kind = "api_key"
env = ["ANTHROPIC_API_KEY_UNSET_TEST_3"]
header = "x-api-key"
prefix = ""

[[model]]
id = "claude-opus-5"
provider_id = "anthropic"
context_tokens = 1000000
"#;
        let file: codypendent_providers::ProvidersFile =
            toml::from_str(provider_toml).expect("provider toml");
        let catalog = Catalog::from_parts(file.providers, file.models);

        let id = model_id("anthropic/claude-opus-5");
        let registry = ModelRegistry::new([ModelConfig {
            id: id.clone(),
            provider: "openai-compatible".to_string(),
            base_url: "https://api.anthropic.com".to_string(),
            model: "claude-opus-5".to_string(),
            api_key_env: String::new(),
            // A live /models response that overstated the real 1,000,000
            // ceiling but stayed under the absolute MAX_PLAUSIBLE_CONTEXT_TOKENS
            // sanity clamp — the case only the catalog-aware clamp catches.
            context_tokens: Some(1_900_000),
            provider_id: Some("anthropic".to_string()),
        }])
        .with_catalog(catalog);

        assert_eq!(
            registry.context_tokens_for(&id),
            Some(1_000_000),
            "must clamp to the catalog row's own documented ceiling, tighter than the absolute max"
        );
    }

    // -- missing-env-var --------------------------------------------------

    #[cfg(feature = "provider-openai")]
    #[tokio::test]
    async fn client_for_names_missing_env_var() {
        // A deliberately unique variable name: never set anywhere in this
        // process, so no set_var/remove_var is needed and there is no race
        // with other tests touching global env state.
        let var_name = "CODYPENDENT_TEST_MODELS_RS_UNSET_KEY_9f3c7ab1";
        assert!(
            std::env::var(var_name).is_err(),
            "test precondition: {var_name} must not be set"
        );

        let id = model_id("hosted-default");
        let registry = ModelRegistry::new([ModelConfig {
            id: id.clone(),
            provider: "openai-compatible".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            model: "gpt-5.1-codex".to_string(),
            api_key_env: var_name.to_string(),
            context_tokens: None,
            provider_id: None,
        }]);

        // `Arc<dyn ChatClient>` (the `Ok` type) has no `Debug` impl, so
        // `expect_err` (which would need to print it on the `Ok` branch)
        // isn't usable here; `.err().expect(..)` never needs to format `Ok`.
        let err = registry
            .client_for(&id)
            .await
            .err()
            .expect("missing env var must error");
        match &err {
            ModelsError::MissingApiKeyEnv { model, var } => {
                assert_eq!(model, &id);
                assert_eq!(var, var_name);
            }
            other => panic!("expected MissingApiKeyEnv, got {other:?}"),
        }
        assert!(
            err.to_string().contains(var_name),
            "error message must name the variable: {err}"
        );
    }

    #[cfg(feature = "provider-openai")]
    #[tokio::test]
    async fn client_for_allows_empty_api_key_env_for_local_endpoints() {
        let id = model_id("local-default");
        let registry = ModelRegistry::new([ModelConfig {
            id: id.clone(),
            provider: "openai-compatible".to_string(),
            base_url: "http://localhost:11434/v1".to_string(),
            model: "qwen2.5-coder:14b".to_string(),
            api_key_env: String::new(),
            context_tokens: None,
            provider_id: None,
        }]);

        // Builds an OpenAI-compatible client with no key (Ok is enough — the
        // concrete `model()` accessor is no longer reachable on the returned
        // `Arc<dyn ChatClient>`).
        assert!(
            registry.client_for(&id).await.is_ok(),
            "empty api_key_env is not an error"
        );
    }

    #[cfg(feature = "provider-openai")]
    #[tokio::test]
    async fn auth_json_key_resolves_even_when_the_env_var_is_unset() {
        use crate::auth::AuthStore;
        // A unique, never-set var: env alone cannot satisfy this model.
        let var = "CODYPENDENT_TEST_MODELS_AUTHJSON_UNSET_5b2e";
        assert!(std::env::var(var).is_err(), "precondition: {var} unset");

        let id = model_id("groq/llama");
        let cfg = ModelConfig {
            id: id.clone(),
            provider: "openai-compatible".to_string(),
            base_url: "https://api.groq.com/openai/v1".to_string(),
            model: "llama-3.1-8b".to_string(),
            api_key_env: var.to_string(),
            context_tokens: None,
            provider_id: None,
        };

        // Without an auth.json entry the env var is required and missing → error.
        let registry = ModelRegistry::new([cfg.clone()]);
        assert!(
            matches!(
                registry.client_for(&id).await,
                Err(ModelsError::MissingApiKeyEnv { .. })
            ),
            "with no auth.json entry the env path is unchanged (missing → error)"
        );

        // With an auth.json entry the key resolves from it — env never consulted.
        let mut auth = AuthStore::default();
        auth.set("groq/llama", "sk-from-authjson");
        let registry = ModelRegistry::new([cfg]).with_auth(auth);
        assert!(
            registry.client_for(&id).await.is_ok(),
            "an auth.json key must satisfy a model whose api_key_env is unset"
        );
    }

    #[cfg(feature = "provider-openai")]
    #[tokio::test]
    async fn local_model_needs_no_key_with_an_empty_auth_store() {
        use crate::auth::AuthStore;
        let id = model_id("ollama/qwen");
        let registry = ModelRegistry::new([ModelConfig {
            id: id.clone(),
            provider: "openai-compatible".to_string(),
            base_url: "http://localhost:11434/v1".to_string(),
            model: "qwen2.5-coder:14b".to_string(),
            api_key_env: String::new(),
            context_tokens: None,
            provider_id: None,
        }])
        .with_auth(AuthStore::default());
        assert!(
            registry.client_for(&id).await.is_ok(),
            "a local model (empty api_key_env, empty auth.json) needs no key"
        );
    }

    /// M1 (defense-in-depth): a hand-edited `auth.json` entry whose value is
    /// the EMPTY string must be treated as ABSENT, never as a present ""
    /// key — otherwise it would silently shadow a perfectly valid
    /// `api_key_env` into "no key". Proven two ways: (1) with the env var
    /// actually unset, an empty auth.json entry must still report
    /// `MissingApiKeyEnv` — exactly as if there were no auth.json entry at
    /// all (this is the discriminating half: without the `.filter(|k|
    /// !k.is_empty())` guard, the empty "" entry would be taken as a real
    /// key and this would wrongly resolve `Ok`); (2) with the SAME empty
    /// entry but the env var now set, resolution still succeeds — the
    /// empty entry never blocks a valid env var either.
    #[cfg(feature = "provider-openai")]
    #[tokio::test]
    async fn empty_auth_json_entry_is_ignored_not_treated_as_a_present_key() {
        use crate::auth::AuthStore;
        // A deliberately unique var name (mirrors
        // `codypendent_providers::credential::api_key_resolves_the_first_set_env_var`,
        // which uses the same set/remove-around-the-assertion pattern).
        let var = "CODYPENDENT_TEST_MODELS_RS_EMPTY_AUTH_FILTER_2f9d";
        assert!(std::env::var(var).is_err(), "precondition: {var} unset");

        let id = model_id("groq/empty-auth-entry");
        let cfg = ModelConfig {
            id: id.clone(),
            provider: "openai-compatible".to_string(),
            base_url: "https://api.groq.com/openai/v1".to_string(),
            model: "llama-3.1-8b".to_string(),
            api_key_env: var.to_string(),
            context_tokens: None,
            provider_id: None,
        };

        let mut auth = AuthStore::default();
        auth.set(id.0.as_str(), ""); // present, but empty
        let registry = ModelRegistry::new([cfg]).with_auth(auth);

        // (1) Env still unset: the empty entry must NOT count as a key, so this
        // must fail exactly like "no auth.json entry at all" — never silently
        // succeed with an empty key.
        assert!(
            matches!(
                registry.client_for(&id).await,
                Err(ModelsError::MissingApiKeyEnv { .. })
            ),
            "an empty auth.json entry must be ignored, falling through to the (missing) env var"
        );

        // (2) Now set the env var: the same empty entry must not shadow it.
        std::env::set_var(var, "sk-from-env-2f9d");
        assert!(
            registry.client_for(&id).await.is_ok(),
            "a set env var must still resolve when the auth.json entry for this id is empty"
        );
        std::env::remove_var(var);
    }

    #[cfg(feature = "provider-openai")]
    #[tokio::test]
    async fn client_for_rejects_unsupported_provider() {
        let id = model_id("weird");
        let registry = ModelRegistry::new([ModelConfig {
            id: id.clone(),
            provider: "anthropic-native".to_string(),
            base_url: "https://api.anthropic.com".to_string(),
            model: "claude-sonnet-5".to_string(),
            api_key_env: String::new(),
            context_tokens: None,
            provider_id: None,
        }]);

        let err = registry
            .client_for(&id)
            .await
            .err()
            .expect("unsupported provider must error");
        assert!(matches!(err, ModelsError::UnsupportedProvider { .. }));
    }

    #[test]
    fn client_for_unknown_model_is_reported() {
        // Exercise the unknown-model path without requiring provider-openai:
        // the underlying registry lookup used by `client_for` is provider
        // agnostic, so this checks it directly via `get`.
        let registry = ModelRegistry::new(Vec::new());
        assert!(registry.get(&model_id("nope")).is_none());
    }

    // -- ModelPolicy --------------------------------------------------

    #[test]
    fn policy_candidates_fall_back_to_default_list() {
        let hosted = model_id("hosted-default");
        let local = model_id("local-default");
        let policy = ModelPolicy::new()
            .with_candidates(AgentMode::Build, vec![hosted.clone(), local.clone()])
            .with_default_candidates(vec![local.clone()]);

        assert_eq!(
            policy.candidates(AgentMode::Build),
            &[hosted, local.clone()]
        );
        // Ask/Explore/etc. have no explicit entry, so they fall back.
        assert_eq!(policy.candidates(AgentMode::Ask), &[local]);
    }

    #[test]
    fn policy_with_no_entries_and_no_default_is_empty() {
        let policy = ModelPolicy::new();
        assert!(policy.candidates(AgentMode::Build).is_empty());
    }

    // -- fallback on connect-refused --------------------------------------

    #[tokio::test]
    async fn resolve_model_falls_back_past_a_closed_port() {
        // Port 1 is a privileged, essentially-never-listening TCP port; a
        // connect attempt against it on localhost gets an immediate OS-level
        // refusal rather than a slow timeout, making this deterministic and
        // fast without mocking anything.
        let closed = model_id("closed-port-candidate");
        // A real listener that accepts the TCP handshake (though nothing
        // speaks HTTP on it) stands in for "reachable". TCP connect()
        // succeeds once the handshake completes and the kernel queues the
        // connection, even with no explicit `accept()` call, so simply
        // keeping the listener bound and alive is enough.
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let reachable_addr = listener.local_addr().expect("local_addr");
        let reachable = model_id("reachable-candidate");

        let registry = ModelRegistry::new([
            ModelConfig {
                id: closed.clone(),
                provider: "openai-compatible".to_string(),
                base_url: "http://127.0.0.1:1/v1".to_string(),
                model: "unused".to_string(),
                api_key_env: String::new(),
                context_tokens: None,
                provider_id: None,
            },
            ModelConfig {
                id: reachable.clone(),
                provider: "openai-compatible".to_string(),
                base_url: format!("http://{reachable_addr}/v1"),
                model: "unused".to_string(),
                api_key_env: String::new(),
                context_tokens: None,
                provider_id: None,
            },
        ]);
        let policy = ModelPolicy::new()
            .with_candidates(AgentMode::Build, vec![closed.clone(), reachable.clone()]);

        let resolved = resolve_model_with_probe(
            &registry,
            &policy,
            AgentMode::Build,
            &TcpConnectProbe::default(),
        )
        .await
        .expect("second candidate should be reachable");
        assert_eq!(resolved.id, reachable);

        drop(listener);
    }

    #[tokio::test]
    async fn resolve_model_reports_structured_error_when_every_candidate_fails() {
        let closed = model_id("closed-port-only");
        let registry = ModelRegistry::new([ModelConfig {
            id: closed.clone(),
            provider: "openai-compatible".to_string(),
            base_url: "http://127.0.0.1:1/v1".to_string(),
            model: "unused".to_string(),
            api_key_env: String::new(),
            context_tokens: None,
            provider_id: None,
        }]);
        let policy = ModelPolicy::new().with_candidates(AgentMode::Build, vec![closed.clone()]);

        let err = resolve_model(&registry, &policy, AgentMode::Build)
            .await
            .expect_err("no reachable candidate");
        match err {
            ModelsError::AllCandidatesFailed { mode, attempts } => {
                assert_eq!(mode, AgentMode::Build);
                assert_eq!(attempts.len(), 1);
                assert_eq!(attempts[0].0, closed);
            }
            other => panic!("expected AllCandidatesFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_model_errors_when_no_candidates_configured() {
        let registry = ModelRegistry::new(Vec::new());
        let policy = ModelPolicy::new();
        let err = resolve_model(&registry, &policy, AgentMode::Explore)
            .await
            .expect_err("empty candidate list must error");
        assert!(matches!(err, ModelsError::NoCandidates { .. }));
    }

    #[tokio::test]
    async fn resolve_model_skips_unregistered_candidate_ids() {
        let reachable = model_id("registered-and-reachable");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let reachable_addr = listener.local_addr().expect("local_addr");

        let registry = ModelRegistry::new([ModelConfig {
            id: reachable.clone(),
            provider: "openai-compatible".to_string(),
            base_url: format!("http://{reachable_addr}/v1"),
            model: "unused".to_string(),
            api_key_env: String::new(),
            context_tokens: None,
            provider_id: None,
        }]);
        let ghost = model_id("not-in-registry");
        let policy =
            ModelPolicy::new().with_candidates(AgentMode::Plan, vec![ghost, reachable.clone()]);

        let resolved = resolve_model_with_probe(
            &registry,
            &policy,
            AgentMode::Plan,
            &TcpConnectProbe::default(),
        )
        .await
        .expect("second, registered candidate should resolve");
        assert_eq!(resolved.id, reachable);

        drop(listener);
    }

    #[cfg(feature = "provider-openai")]
    async fn models_server(models: &[&str]) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let data = models
            .iter()
            .map(|id| serde_json::json!({ "id": id }))
            .collect::<Vec<_>>();
        let body = serde_json::json!({ "data": data }).to_string();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        (format!("http://{address}/v1"), task)
    }

    #[cfg(feature = "provider-openai")]
    #[tokio::test]
    async fn resolve_model_falls_back_past_a_reachable_but_missing_model() {
        let stale = model_id("stale");
        let healthy = model_id("healthy");
        let (stale_url, stale_server) = models_server(&["something-else"]).await;
        let (healthy_url, healthy_server) = models_server(&["installed-model"]).await;
        let registry = ModelRegistry::new([
            ModelConfig {
                id: stale.clone(),
                provider: "openai-compatible".to_string(),
                base_url: stale_url,
                model: "missing-model".to_string(),
                api_key_env: String::new(),
                context_tokens: None,
                provider_id: None,
            },
            ModelConfig {
                id: healthy.clone(),
                provider: "openai-compatible".to_string(),
                base_url: healthy_url,
                model: "installed-model".to_string(),
                api_key_env: String::new(),
                context_tokens: None,
                provider_id: None,
            },
        ]);
        let policy =
            ModelPolicy::new().with_candidates(AgentMode::Build, vec![stale, healthy.clone()]);

        let resolved = resolve_model(&registry, &policy, AgentMode::Build)
            .await
            .expect("the installed fallback should be selected");
        assert_eq!(resolved.id, healthy);
        stale_server.await.unwrap();
        healthy_server.await.unwrap();
    }

    // -- models.toml extras: [embedding] + [retrieval] ---------------------

    #[test]
    fn extras_parse_embedding_and_retrieval_alongside_model_entries() {
        let mut file = tempfile::NamedTempFile::new().expect("create temp file");
        write!(
            file,
            r#"
[[model]]
id = "local-default"
provider = "openai-compatible"
base_url = "http://localhost:11434/v1"
model = "qwen2.5-coder:14b"
api_key_env = ""

[embedding]
base_url = "http://localhost:11434/v1"
model = "nomic-embed-text"
dims = 768

[retrieval]
mcp_top_k = 5
"#
        )
        .expect("write temp file");

        // Both readers coexist on one file: `load_models` ignores the new
        // tables; `load_model_extras` ignores the [[model]] entries.
        let models = load_models(file.path()).expect("model entries still parse");
        assert_eq!(models.len(), 1);
        let extras = load_model_extras(file.path()).expect("extras parse");
        let embedding = extras.embedding.expect("embedding entry present");
        assert_eq!(embedding.provider, "openai-compatible", "provider defaults");
        assert_eq!(embedding.base_url, "http://localhost:11434/v1");
        assert_eq!(embedding.model, "nomic-embed-text");
        assert_eq!(embedding.api_key_env, "", "api_key_env defaults empty");
        assert_eq!(embedding.dims, Some(768));
        assert_eq!(extras.retrieval.mcp_top_k, 5);
    }

    #[test]
    fn extras_default_when_tables_or_file_are_absent() {
        // A models.toml with only [[model]] entries (the existing shape) yields
        // the defaults: no embedding model, default MCP top-k.
        let mut file = tempfile::NamedTempFile::new().expect("create temp file");
        write!(
            file,
            r#"
[[model]]
id = "hosted-default"
provider = "openai-compatible"
base_url = "https://api.openai.com/v1"
model = "gpt-5.1-codex"
api_key_env = "OPENAI_API_KEY"
"#
        )
        .expect("write temp file");
        let extras = load_model_extras(file.path()).expect("extras parse");
        assert_eq!(extras.embedding, None);
        assert_eq!(extras.retrieval.mcp_top_k, DEFAULT_MCP_TOP_K);

        // No file at all: defaults too (context assembly must not need one).
        let dir = tempfile::tempdir().expect("tempdir");
        let extras =
            load_model_extras(&dir.path().join("models.toml")).expect("absent file defaults");
        assert_eq!(extras, ModelExtras::default());
    }

    #[test]
    fn malformed_extras_are_a_legible_error_not_a_silent_default() {
        let mut file = tempfile::NamedTempFile::new().expect("create temp file");
        write!(file, "[embedding]\nmodel = 42\n").expect("write temp file");
        assert!(
            matches!(
                load_model_extras(file.path()),
                Err(ModelsError::ParseConfig { .. })
            ),
            "a typo'd [embedding] table must error, never silently disable embeddings"
        );
    }

    // -- failure taxonomy (transient vs permanent) -------------------------

    #[test]
    fn classify_provider_message_marks_connect_timeout_and_5xx_transient() {
        for message in [
            "connection refused (os error 111)",
            "Connection reset by peer",
            "operation timed out",
            "request timeout",
            "provider returned HTTP 429 Too Many Requests from /models",
            "HTTP status server error (503 Service Unavailable)",
            "upstream 502",
            "the model is overloaded, please retry",
        ] {
            assert_eq!(
                classify_provider_message(message),
                FailureClass::Transient,
                "expected transient: {message}"
            );
        }
    }

    #[test]
    fn classify_provider_message_marks_contract_failures_permanent() {
        for message in [
            "provider returned HTTP 401 Unauthorized from /models",
            "invalid request: unknown model `nope`",
            "provider returned HTTP 404 from /models",
            "content policy violation",
            // A transient-looking digit run embedded in a longer number must
            // NOT match — status codes are standalone digit runs only.
            "request id 15005 was rejected: bad schema",
        ] {
            assert_eq!(
                classify_provider_message(message),
                FailureClass::Permanent,
                "expected permanent: {message}"
            );
        }
    }

    #[test]
    fn models_error_failure_class_follows_the_variant_taxonomy() {
        let transient = ModelsError::ConnectionFailed {
            base_url: "http://localhost:11434/v1".to_string(),
            reason: "connection refused".to_string(),
        };
        assert_eq!(transient.failure_class(), FailureClass::Transient);

        // ModelUnavailable defers to its provider-supplied reason: a 5xx from
        // /models is a blip worth retrying, a missing model tag is not.
        let blip = ModelsError::ModelUnavailable {
            model: model_id("m"),
            provider_model: "m".to_string(),
            reason: "provider returned HTTP 503 from /models".to_string(),
        };
        assert_eq!(blip.failure_class(), FailureClass::Transient);
        let missing = ModelsError::ModelUnavailable {
            model: model_id("m"),
            provider_model: "m".to_string(),
            reason: "provider did not list this model".to_string(),
        };
        assert_eq!(missing.failure_class(), FailureClass::Permanent);

        // Config/credential failures can never be fixed by waiting.
        let permanent = ModelsError::MissingApiKeyEnv {
            model: model_id("m"),
            var: "SOME_KEY".to_string(),
        };
        assert_eq!(permanent.failure_class(), FailureClass::Permanent);
        assert_eq!(
            ModelsError::UnknownModel(model_id("nope")).failure_class(),
            FailureClass::Permanent
        );
    }

    // -- catalog-resolved auth headers (provider_id) ------------------------

    /// A one-request HTTP server that answers `body` and hands back the raw
    /// request head it received, so a test can assert on the exact auth
    /// headers a probe/client sent.
    #[cfg(feature = "provider-openai")]
    async fn capture_server(body: &str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let body = body.to_string();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 8192];
            let n = stream.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&request[..n]).into_owned()
        });
        (format!("http://{address}/v1"), task)
    }

    /// The HIGH-severity auth-flatten regression: an azure-shaped provider
    /// (catalog auth `header = "api-key"`, `prefix = ""` — the built-in
    /// `azure-openai` entry) must round-trip its key under `api-key`, not
    /// under a hardcoded `Authorization: Bearer`. `check_model` is asserted
    /// on the wire; `client_for` must agree because both resolve the same
    /// [`EndpointAuth`].
    #[cfg(feature = "provider-openai")]
    #[tokio::test]
    async fn check_model_sends_the_catalog_declared_api_key_header_for_azure() {
        use crate::auth::AuthStore;
        let id = model_id("azure-openai/gpt-5.1");
        let (url, server) = capture_server(r#"{"data":[{"id":"gpt-5.1"}]}"#).await;
        let mut auth = AuthStore::default();
        auth.set(id.0.as_str(), "azure-secret-key");
        let registry = ModelRegistry::new([ModelConfig {
            id: id.clone(),
            provider: "openai-compatible".to_string(),
            base_url: url,
            model: "gpt-5.1".to_string(),
            api_key_env: String::new(),
            context_tokens: None,
            provider_id: Some("azure-openai".to_string()),
        }])
        .with_auth(auth);

        registry
            .check_model(&id)
            .await
            .expect("the endpoint lists the model");
        let request = server.await.unwrap();
        let head = request.to_lowercase();
        assert!(
            head.contains("api-key: azure-secret-key"),
            "the key must ride the catalog-declared `api-key` header:\n{request}"
        );
        assert!(
            !head.contains("authorization:"),
            "no bearer header may be sent for an api-key provider:\n{request}"
        );
    }

    /// The legacy shape (no `provider_id`) keeps the exact bearer wire
    /// behavior — the back-compat half of the azure round-trip test.
    #[cfg(feature = "provider-openai")]
    #[tokio::test]
    async fn check_model_defaults_to_bearer_when_no_provider_id_is_set() {
        use crate::auth::AuthStore;
        let id = model_id("groq/llama");
        let (url, server) = capture_server(r#"{"data":[{"id":"llama-3.1-8b"}]}"#).await;
        let mut auth = AuthStore::default();
        auth.set(id.0.as_str(), "sk-legacy");
        let registry = ModelRegistry::new([ModelConfig {
            id: id.clone(),
            provider: "openai-compatible".to_string(),
            base_url: url,
            model: "llama-3.1-8b".to_string(),
            api_key_env: String::new(),
            context_tokens: None,
            provider_id: None,
        }])
        .with_auth(auth);

        registry.check_model(&id).await.expect("listed");
        let request = server.await.unwrap();
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer sk-legacy"),
            "a legacy entry keeps `Authorization: Bearer`:\n{request}"
        );
    }

    /// `client_for` completes a chat call through the header-aware client for
    /// an azure-shaped provider: the mock asserts `api-key` (no bearer) and
    /// the provider's extra headers on the actual `/chat/completions` POST.
    #[cfg(feature = "provider-openai")]
    #[tokio::test]
    async fn client_for_sends_catalog_headers_on_chat_completions() {
        use crate::auth::AuthStore;
        use agent_framework_core::client::ChatClient;
        use agent_framework_core::types::{ChatOptions, Message};

        // A user-shadowed azure-shaped provider carrying an extra header, so
        // both the auth header and extra_headers are exercised in one call.
        let provider_toml = r#"
[[provider]]
id = "azure-shaped"
name = "Azure-shaped"
protocol = "openai-chat"
base_url = "https://unused.example/v1"
extra_headers = { "x-extra-version" = "2026-01-01" }
[[provider.auth]]
kind = "api_key"
env = ["AZURE_SHAPED_KEY_UNSET_TEST"]
header = "api-key"
prefix = ""
"#;
        let file: codypendent_providers::ProvidersFile =
            toml::from_str(provider_toml).expect("provider toml");
        let catalog = Catalog::from_providers(file.providers);

        let body = r#"{"id":"resp-1","model":"gpt-5.1","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#;
        let (url, server) = capture_server(body).await;

        let id = model_id("azure-shaped/gpt-5.1");
        let mut auth = AuthStore::default();
        auth.set(id.0.as_str(), "azure-secret-key");
        let registry = ModelRegistry::new([ModelConfig {
            id: id.clone(),
            provider: "openai-compatible".to_string(),
            base_url: url,
            model: "gpt-5.1".to_string(),
            api_key_env: String::new(),
            context_tokens: None,
            provider_id: Some("azure-shaped".to_string()),
        }])
        .with_auth(auth)
        .with_catalog(catalog);

        let client = registry.client_for(&id).await.expect("client builds");
        let response = client
            .get_response(vec![Message::user("ping")], ChatOptions::default())
            .await
            .expect("the mock answers");
        assert_eq!(response.text(), "hi");

        let request = server.await.unwrap();
        let head = request.to_lowercase();
        assert!(
            head.starts_with("post /v1/chat/completions"),
            "the chat route is the base_url sibling:\n{request}"
        );
        assert!(
            head.contains("api-key: azure-secret-key"),
            "the key rides the catalog-declared header:\n{request}"
        );
        assert!(
            head.contains("x-extra-version: 2026-01-01"),
            "provider extra headers are sent:\n{request}"
        );
        assert!(
            !head.contains("authorization:"),
            "no bearer header for an api-key provider:\n{request}"
        );
    }

    /// F3 (`2026-08-13-verticals/acp-models.md`): `codypendent models add
    /// anthropic claude-opus-5` writes `provider = "openai-compatible"` (every
    /// `models add` entry does — see `crates/cli/src/commands.rs::models_add`)
    /// with `provider_id = Some("anthropic")`. Before this fix,
    /// `config_to_protocol_auth` ignored `provider_id` entirely and hard-coded
    /// `Protocol::OpenAiChat`, so this exact config would have POSTed an
    /// OpenAI-shaped chat-completions body to `/chat/completions` with a
    /// bearer header — wrong on every axis for Anthropic's real Messages API.
    /// This test builds a client from that exact on-disk shape and asserts on
    /// the real wire: the real Messages route, `x-api-key` (never bearer), and
    /// the catalog's `anthropic-version` extra header.
    #[cfg(all(feature = "provider-openai", feature = "provider-anthropic"))]
    #[tokio::test]
    async fn client_for_speaks_the_anthropic_wire_for_a_models_add_style_config() {
        use crate::auth::AuthStore;
        use agent_framework_core::client::ChatClient;
        use agent_framework_core::types::{ChatOptions, Message};

        let provider_toml = r#"
[[provider]]
id = "anthropic"
name = "Anthropic (Claude)"
protocol = "anthropic"
base_url = "https://unused.example"
extra_headers = { "anthropic-version" = "2023-06-01" }
[[provider.auth]]
kind = "api_key"
env = ["ANTHROPIC_API_KEY_UNSET_TEST"]
header = "x-api-key"
prefix = ""
"#;
        let file: codypendent_providers::ProvidersFile =
            toml::from_str(provider_toml).expect("provider toml");
        let catalog = Catalog::from_providers(file.providers);

        let body = r#"{"id":"msg_1","type":"message","role":"assistant","model":"claude-opus-5",
            "content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn",
            "usage":{"input_tokens":1,"output_tokens":1}}"#;
        let (url, server) = capture_server(body).await;
        // `capture_server` returns a `/v1`-suffixed base_url for the OpenAI-chat
        // tests above; Anthropic's own base_url is bare (`https://api.anthropic.com`,
        // no `/v1` — the client appends `/v1/messages` itself), so strip it here.
        let base_url = url.trim_end_matches("/v1").to_string();

        // Exactly the shape `models_add` (`crates/cli/src/commands.rs`) writes:
        // `provider = "openai-compatible"`, `provider_id = Some("anthropic")`.
        let id = model_id("anthropic/claude-opus-5");
        let mut auth = AuthStore::default();
        auth.set(id.0.as_str(), "sk-ant-secret");
        let registry = ModelRegistry::new([ModelConfig {
            id: id.clone(),
            provider: "openai-compatible".to_string(),
            base_url,
            model: "claude-opus-5".to_string(),
            api_key_env: String::new(),
            context_tokens: None,
            provider_id: Some("anthropic".to_string()),
        }])
        .with_auth(auth)
        .with_catalog(catalog);

        let client = registry.client_for(&id).await.expect("client builds");
        let response = client
            .get_response(vec![Message::user("ping")], ChatOptions::default())
            .await
            .expect("the mock answers");
        assert_eq!(response.text(), "hi");

        let request = server.await.unwrap();
        let head = request.to_lowercase();
        assert!(
            head.starts_with("post /v1/messages"),
            "the Anthropic Messages route, not /chat/completions:\n{request}"
        );
        assert!(
            head.contains("x-api-key: sk-ant-secret"),
            "the key rides x-api-key, never Authorization:\n{request}"
        );
        assert!(
            head.contains("anthropic-version: 2023-06-01"),
            "the catalog's anthropic-version header is sent:\n{request}"
        );
        assert!(
            !head.contains("authorization:"),
            "no bearer header may be sent to an Anthropic endpoint:\n{request}"
        );
    }

    /// Same F3 fix, the reachability-probe half: `check_model` must ask
    /// Anthropic's real `GET /v1/models` (spec-fixed path; verified against
    /// `platform.claude.com/docs/en/api/models/list`), not the OpenAI-chat
    /// `{base_url}/models` this build used to send unconditionally — which
    /// resolved to `https://api.anthropic.com/models`, a route that does not
    /// exist, so `codypendent models check` on a freshly `models add`ed
    /// Anthropic entry always failed even with a valid key.
    #[cfg(all(feature = "provider-openai", feature = "provider-anthropic"))]
    #[tokio::test]
    async fn check_model_asks_v1_models_for_the_anthropic_protocol() {
        use crate::auth::AuthStore;

        let provider_toml = r#"
[[provider]]
id = "anthropic"
name = "Anthropic (Claude)"
protocol = "anthropic"
base_url = "https://unused.example"
extra_headers = { "anthropic-version" = "2023-06-01" }
[[provider.auth]]
kind = "api_key"
env = ["ANTHROPIC_API_KEY_UNSET_TEST_2"]
header = "x-api-key"
prefix = ""
"#;
        let file: codypendent_providers::ProvidersFile =
            toml::from_str(provider_toml).expect("provider toml");
        let catalog = Catalog::from_providers(file.providers);

        let (url, server) = capture_server(r#"{"data":[{"id":"claude-opus-5"}]}"#).await;
        let base_url = url.trim_end_matches("/v1").to_string();

        let id = model_id("anthropic/claude-opus-5");
        let mut auth = AuthStore::default();
        auth.set(id.0.as_str(), "sk-ant-secret");
        let registry = ModelRegistry::new([ModelConfig {
            id: id.clone(),
            provider: "openai-compatible".to_string(),
            base_url,
            model: "claude-opus-5".to_string(),
            api_key_env: String::new(),
            context_tokens: None,
            provider_id: Some("anthropic".to_string()),
        }])
        .with_auth(auth)
        .with_catalog(catalog);

        registry
            .check_model(&id)
            .await
            .expect("the endpoint lists the model at /v1/models");
        let request = server.await.unwrap();
        assert!(
            request.to_lowercase().starts_with("get /v1/models"),
            "must probe the spec-fixed Anthropic Models API path:\n{request}"
        );
    }

    /// A stored provider-wide key (`provider/<id>`) satisfies a model that has
    /// no per-model `auth.json` entry and no env var — the dedupe-key-entry
    /// seam: one pasted key serves every model added from that provider.
    #[cfg(feature = "provider-openai")]
    #[tokio::test]
    async fn provider_wide_auth_json_key_resolves_for_models_of_that_provider() {
        use crate::auth::AuthStore;
        let id = model_id("nebius/deepseek");
        let mut auth = AuthStore::default();
        auth.set(provider_auth_id("nebius"), "nb-provider-key");
        let registry = ModelRegistry::new([ModelConfig {
            id: id.clone(),
            provider: "openai-compatible".to_string(),
            base_url: "https://api.tokenfactory.nebius.com/v1".to_string(),
            model: "deepseek-ai/DeepSeek-V4-Flash".to_string(),
            api_key_env: String::new(),
            context_tokens: None,
            provider_id: Some("nebius".to_string()),
        }])
        .with_auth(auth);
        assert!(
            registry.client_for(&id).await.is_ok(),
            "a provider-wide stored key must satisfy the model"
        );
    }

    /// The discovery readiness check and the live client both delegate to
    /// `api_key_for`; pin the complete precedence here so neither side can
    /// silently drift as more credential sources are added.
    #[cfg(feature = "provider-openai")]
    #[tokio::test]
    async fn model_credentials_follow_model_provider_explicit_and_catalog_env_precedence() {
        use crate::auth::AuthStore;

        const MODEL_ENV: &str = "CODYPENDENT_TEST_MODEL_PRECEDENCE_KEY_17d9";
        const PROVIDER_ENV: &str = "CODYPENDENT_TEST_PROVIDER_PRECEDENCE_KEY_17d9";
        std::env::set_var(MODEL_ENV, "from-model-env");
        std::env::set_var(PROVIDER_ENV, "from-provider-env");

        let provider_toml = format!(
            r#"
[[provider]]
id = "precedence-test"
name = "Precedence test"
protocol = "openai-chat"
base_url = "https://example.invalid/v1"
[[provider.auth]]
kind = "api_key"
env = ["{PROVIDER_ENV}"]
header = "Authorization"
prefix = "Bearer "
"#
        );
        let file: codypendent_providers::ProvidersFile =
            toml::from_str(&provider_toml).expect("provider toml");
        let catalog = Catalog::from_providers(file.providers);
        let id = model_id("precedence/model");
        let cfg = ModelConfig {
            id: id.clone(),
            provider: "openai-compatible".to_string(),
            base_url: "https://example.invalid/v1".to_string(),
            model: "model".to_string(),
            api_key_env: MODEL_ENV.to_string(),
            provider_id: Some("precedence-test".to_string()),
            context_tokens: None,
        };

        let mut auth = AuthStore::default();
        auth.set(id.0.as_str(), "from-model-auth");
        auth.set(provider_auth_id("precedence-test"), "from-provider-auth");
        let registry = ModelRegistry::new([cfg.clone()])
            .with_auth(auth)
            .with_catalog(catalog.clone());
        assert_eq!(
            registry.api_key_for(&cfg).await.expect("model auth"),
            "from-model-auth"
        );

        let mut auth = AuthStore::default();
        auth.set(provider_auth_id("precedence-test"), "from-provider-auth");
        let registry = ModelRegistry::new([cfg.clone()])
            .with_auth(auth)
            .with_catalog(catalog.clone());
        assert_eq!(
            registry.api_key_for(&cfg).await.expect("provider auth"),
            "from-provider-auth"
        );

        let registry = ModelRegistry::new([cfg.clone()]).with_catalog(catalog.clone());
        assert_eq!(
            registry.api_key_for(&cfg).await.expect("model env"),
            "from-model-env"
        );

        let provider_env_cfg = ModelConfig {
            api_key_env: String::new(),
            ..cfg.clone()
        };
        let registry = ModelRegistry::new([provider_env_cfg.clone()]).with_catalog(catalog.clone());
        assert_eq!(
            registry
                .api_key_for(&provider_env_cfg)
                .await
                .expect("provider env"),
            "from-provider-env"
        );
        assert!(registry
            .credentials_resolvable(&id)
            .await
            .expect("readiness uses the same resolver"));

        // An explicit model env is authoritative: if it is missing, do not
        // silently switch to the provider env and run with a different key.
        let missing_explicit = ModelConfig {
            api_key_env: "CODYPENDENT_TEST_EXPLICIT_MISSING_KEY_17d9".to_string(),
            ..cfg
        };
        let registry = ModelRegistry::new([missing_explicit.clone()]).with_catalog(catalog);
        assert!(matches!(
            registry.api_key_for(&missing_explicit).await,
            Err(ModelsError::MissingApiKeyEnv { .. })
        ));

        std::env::remove_var(MODEL_ENV);
        std::env::remove_var(PROVIDER_ENV);
    }

    // -- authority_from_base_url -------------------------------------------

    #[test]
    fn authority_parsing_handles_explicit_and_default_ports() {
        assert_eq!(
            authority_from_base_url("http://127.0.0.1:1/v1").unwrap(),
            "127.0.0.1:1"
        );
        assert_eq!(
            authority_from_base_url("http://localhost:11434/v1").unwrap(),
            "localhost:11434"
        );
        assert_eq!(
            authority_from_base_url("https://api.openai.com/v1").unwrap(),
            "api.openai.com:443"
        );
        assert_eq!(
            authority_from_base_url("http://example.com").unwrap(),
            "example.com:80"
        );
        assert!(authority_from_base_url("not-a-url").is_ok());
    }
}
