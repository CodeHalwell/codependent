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
use std::time::Duration;

use codypendent_protocol::{AgentMode, ModelId};
use serde::{Deserialize, Serialize};

use crate::auth::AuthStore;

#[cfg(feature = "provider-openai")]
use agent_framework_openai::OpenAIChatCompletionClient;

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
    /// The runtime adapter: `"openai-compatible"` for chat-completions models,
    /// or `"acp"` for an external agent from the official ACP registry. ACP
    /// agents own their model and tool loop; [`Self::model`] is their registry id.
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

/// Parse `models.toml` at `path` into its [`ModelConfig`] entries.
///
/// Exposed standalone (in addition to [`ModelRegistry::load`]) so tests — and
/// callers that want to inspect or filter configs before building a registry
/// — can drive parsing directly against a temp file.
pub fn load_models(path: &Path) -> Result<Vec<ModelConfig>> {
    let text = std::fs::read_to_string(path).map_err(|source| ModelsError::ReadConfig {
        path: path.to_path_buf(),
        source,
    })?;
    let file: ModelsFile = toml::from_str(&text).map_err(|source| ModelsError::ParseConfig {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(file.model)
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

    /// A model's provider maps to a wire protocol this build does not yet wire
    /// (Anthropic/Gemini native are follow-ups; only OpenAI-compatible is wired).
    #[error("model `{model}` uses protocol `{protocol}` which is not yet wired (only OpenAI-compatible is)")]
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

/// The set of configured model profiles, keyed by [`ModelId`], plus the
/// resolved [`AuthStore`] (`auth.json`) so [`ModelRegistry::client_for`] can
/// prefer a stored key over the model's `api_key_env`. The store's own redacting
/// `Debug` keeps the derived `Debug` here from leaking a key.
#[derive(Debug, Clone, Default)]
pub struct ModelRegistry {
    configs: HashMap<ModelId, ModelConfig>,
    auth: AuthStore,
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

    /// The official registry id for an ACP profile.
    #[must_use]
    pub fn acp_agent_id(&self, id: &ModelId) -> Option<&str> {
        self.get(id)
            .filter(|config| config.provider == "acp")
            .map(|config| config.model.as_str())
    }
}

/// Map a persisted [`ModelConfig`] onto the provider abstraction. Chat profiles
/// become `(OpenAiChat, ApiKey|None)`; ACP profiles are marked so the assembly
/// executor can route them to the full-agent runtime instead of a chat client.
/// This remains the backward-compatible bridge for existing `models.toml` files.
#[cfg(feature = "provider-openai")]
fn config_to_protocol_auth(cfg: &ModelConfig) -> Result<(Protocol, AuthMethod)> {
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
    let auth = if cfg.api_key_env.trim().is_empty() {
        AuthMethod::None
    } else {
        AuthMethod::ApiKey {
            env: vec![cfg.api_key_env.clone()],
            header: "Authorization".to_string(),
            prefix: "Bearer ".to_string(),
        }
    };
    Ok((Protocol::OpenAiChat, auth))
}

#[cfg(feature = "provider-openai")]
impl ModelRegistry {
    /// Resolve the key exactly as the live client does. Keeping this in one
    /// helper ensures the readiness probe and the first completion cannot
    /// disagree because they used different credential precedence.
    async fn api_key_for(&self, cfg: &ModelConfig) -> Result<String> {
        if let Some(key) = self
            .auth
            .get(cfg.id.0.as_str())
            .filter(|key| !key.is_empty())
        {
            return Ok(key.to_string());
        }
        let (_, auth) = config_to_protocol_auth(cfg)?;
        match credential_for(&auth).resolve().await {
            Ok(ResolvedCredential::ApiKey { value, .. }) => Ok(value),
            Ok(ResolvedCredential::None) => Ok(String::new()),
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
        let (protocol, _) = config_to_protocol_auth(cfg)?;
        if !matches!(protocol, Protocol::OpenAiChat) {
            return Err(ModelsError::ProtocolNotWired {
                model: id.clone(),
                protocol: format!("{protocol:?}"),
            });
        }
        if cfg.base_url.trim().is_empty() {
            return Err(ModelsError::InvalidBaseUrl {
                base_url: cfg.base_url.clone(),
                reason: "base_url is blank".to_string(),
            });
        }

        let key = self.api_key_for(cfg).await?;
        let endpoint = format!("{}/models", cfg.base_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|error| ModelsError::ConnectionFailed {
                base_url: cfg.base_url.clone(),
                reason: error.to_string(),
            })?;
        let mut request = client.get(endpoint);
        if !key.is_empty() {
            request = request.bearer_auth(key);
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
    /// model's `api_key_env` environment variable; (3) no key at all (local
    /// endpoints with an empty `api_key_env`). Whichever wins is moved
    /// straight into the client and is never stored on the registry, logged,
    /// or otherwise retained by this function (Chapter 11, "Secrets"). A
    /// required-but-unset variable produces [`ModelsError::MissingApiKeyEnv`]
    /// naming the variable.
    ///
    /// Today only [`Protocol::OpenAiChat`] is wired: a legacy `models.toml`
    /// entry (`provider = "openai-compatible"`) maps onto it via
    /// [`config_to_protocol_auth`] and builds the exact same
    /// `OpenAIChatCompletionClient::new(api_key, model).with_base_url(base_url)`
    /// as before — the one code path that serves both the hosted OpenAI
    /// endpoint and any OpenAI-compatible local/self-hosted endpoint (e.g.
    /// Ollama), per STEP 1.9 — now returned behind `Arc<dyn ChatClient>`. Any
    /// other protocol (Anthropic/Gemini native are follow-ups) returns
    /// [`ModelsError::ProtocolNotWired`].
    pub async fn client_for(
        &self,
        id: &ModelId,
    ) -> Result<Arc<dyn agent_framework_core::client::ChatClient>> {
        let cfg = self
            .get(id)
            .ok_or_else(|| ModelsError::UnknownModel(id.clone()))?;
        let (protocol, _) = config_to_protocol_auth(cfg)?;
        match protocol {
            Protocol::OpenAiChat => {
                // Key resolution precedence (additive): (a) an `auth.json` key for
                // this model id wins → (b) the model's `api_key_env` (today's
                // path) → (c) none. A model with no `auth.json` entry behaves
                // exactly as before. The stored key is moved straight into the
                // client and is never logged or retained by this function.
                let api_key = self.api_key_for(cfg).await?;
                let client = OpenAIChatCompletionClient::new(api_key, cfg.model.clone())
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
            },
            ModelConfig {
                id: reachable.clone(),
                provider: "openai-compatible".to_string(),
                base_url: format!("http://{reachable_addr}/v1"),
                model: "unused".to_string(),
                api_key_env: String::new(),
                context_tokens: None,
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
            },
            ModelConfig {
                id: healthy.clone(),
                provider: "openai-compatible".to_string(),
                base_url: healthy_url,
                model: "installed-model".to_string(),
                api_key_env: String::new(),
                context_tokens: None,
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
