//! The LOCAL CONFIG half of the desktop: `models.toml`, `providers.toml` and
//! `auth.json` under the runtime data dir.
//!
//! None of this is protocol. There is no `ListModels`, `ListProviders` or
//! `SetApiKey` on the wire — the TUI reads and writes these files directly
//! (`crates/cli/src/tui.rs`), so the shell must too. A webview cannot open a
//! file any more than it can open a socket, which is why the whole surface
//! lives here rather than in TypeScript.
//!
//! Two rules shape every function below.
//!
//! **Nothing is reimplemented that already exists.** `ModelConfig`/`load_models`
//! and `AuthStore` come from `codypendent-runtime`, the provider catalog from
//! `codypendent-providers` — the same types the TUI, the CLI and the daemon
//! parse these files with. The two write paths ([`update_model_entries`],
//! [`remove_model`]) are ports of `crates/cli/src/models_file.rs` and
//! `crates/cli/src/tui.rs::write_remove_model`, kept structurally identical
//! (advisory lock, formatting preservation, atomic 0600 install) because
//! `models.toml` is a document a human owns and four independent writers have
//! already destroyed parts of it once each.
//!
//! **A secret is never a value that crosses back to the webview.** A key can
//! travel IN, as [`SecretKey`] (whose `Debug` prints `<redacted>`, mirroring
//! `crates/tui/src/action.rs`'s carrier of the same name); what travels OUT is
//! [`KeyStatus`] — stored, or the NAME of an environment variable, or missing.
//! There is no function here that returns key material, and
//! [`KeyTarget`] carries only a non-secret id.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::ModelId;
use codypendent_providers::{AuthMethod, Catalog, Protocol, Provider};
use codypendent_runtime::auth::AuthStore;
use codypendent_runtime::models::{load_models, provider_auth_id, ModelConfig};
use serde::{Deserialize, Serialize};

/// The `<data_dir>` every file in this module hangs off, resolved exactly the
/// way `socket_path()` resolves the socket.
fn data_dir() -> anyhow::Result<PathBuf> {
    Ok(RuntimePaths::resolve()
        .context("resolving the codypendent data directory")?
        .data_dir)
}

fn models_path(data_dir: &Path) -> PathBuf {
    data_dir.join("models.toml")
}

fn providers_path(data_dir: &Path) -> PathBuf {
    data_dir.join("providers.toml")
}

// ---------------------------------------------------------------------------
// Secret carriers
// ---------------------------------------------------------------------------

/// An API key on its way IN to `auth.json`. Deliberately not `Serialize`: this
/// type can be received from the webview and written to disk, and can never be
/// sent back. Its `Debug` redacts, so a stray `format!("{target:?}")` anywhere
/// downstream cannot leak the value (the discipline of
/// `crates/tui/src/action.rs::SecretKey`).
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct SecretKey(pub String);

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretKey(<redacted>)")
    }
}

/// Which `auth.json` entry an operation addresses. Carries an id, never key
/// material — the same split as `crates/tui/src/action.rs::KeyTarget`.
///
/// Two variants, not the TUI's four: `Tavily`, `Transcription` and `Speech`
/// resolve through `codypendent-integrations` constants and `models.toml`'s
/// `[transcription]`/`[speech]` tables, and hardcoding those load-bearing
/// strings here would silently save keys that read back as absent. They are
/// reported as unavailable rather than guessed at.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeyTarget {
    /// One configured model, keyed by its `models.toml` display id verbatim.
    Model { id: String },
    /// A provider-wide key, shared by every model added from that provider.
    Provider { id: String },
}

impl KeyTarget {
    /// The `auth.json` map key. A model id is stored verbatim; a provider key
    /// lives under the reserved `provider/<id>` prefix
    /// (`codypendent_runtime::models::provider_auth_id`).
    fn auth_id(&self) -> String {
        match self {
            KeyTarget::Model { id } => id.clone(),
            KeyTarget::Provider { id } => provider_auth_id(id),
        }
    }
}

/// Presence, never value. The projection `crates/tui/src/state.rs::KeyStatus`
/// makes, plus one variant it does not need: the TUI's `/keys` view degrades a
/// corrupt `auth.json` to "no stored keys", which is indistinguishable from a
/// user who has stored none. Here that case is [`KeyStatus::Unknown`], so a
/// failed read never renders as a confident "Missing".
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum KeyStatus {
    /// A key is stored in `auth.json` for this id.
    Stored,
    /// No stored key, but the entry names an environment variable. The NAME is
    /// not a secret; the value is never read for this projection.
    Env { name: String },
    /// Neither a stored key nor a configured environment variable name.
    Missing,
    /// `auth.json` exists but could not be read, so presence is unknown. This
    /// is an outcome, not an absence.
    Unknown { reason: String },
}

// ---------------------------------------------------------------------------
// Reading: models
// ---------------------------------------------------------------------------

/// One `[[model]]` entry as the desktop shows it.
///
/// There is no `readiness` field. The TUI computes one by probing local
/// endpoints and resolving credentials through `ModelRegistry`, which this
/// build does not compile (the shell configures models; the daemon runs them).
/// An unverified model is reported as unverified by omission rather than
/// rendered as ready.
#[derive(Debug, Clone, Serialize)]
pub struct ModelRow {
    pub id: String,
    /// The runtime adapter family (`"openai-compatible"`, `"acp"`).
    pub provider: String,
    pub base_url: String,
    /// The provider-side model name.
    pub model: String,
    /// The provider-catalog id this entry was added from, when known.
    pub provider_id: Option<String>,
    /// The declared context window. `None` is unknown — never rendered as 0.
    pub context_tokens: Option<u64>,
    /// Presence of a credential for this entry. Value never included.
    pub key: KeyStatus,
}

/// What [`list_models`] answers with.
#[derive(Debug, Clone, Serialize)]
pub struct ModelsView {
    pub models: Vec<ModelRow>,
    /// The file these rows came from, so an empty list can name what it read.
    pub models_path: String,
    /// Whether `models.toml` exists at all. `false` + an empty list means "no
    /// models configured yet"; an unreadable file is an `Err`, not this.
    pub configured: bool,
    /// Degradations that did not stop the read (e.g. a `providers.toml` that
    /// did not parse). Surfaced, never swallowed.
    pub warnings: Vec<String>,
    /// The model pinned for the next run, if one is pinned in this shell.
    pub pinned: Option<String>,
}

/// Every configured model, with credential PRESENCE.
///
/// A missing `models.toml` is "read, empty" — the normal state of a fresh
/// install. A `models.toml` that exists but does not parse is an error: the
/// caller renders unavailable, which is not the same thing as an empty list.
pub fn list_models(pinned: Option<&ModelId>) -> anyhow::Result<ModelsView> {
    let data_dir = data_dir()?;
    let path = models_path(&data_dir);
    let configured = path.exists();
    let configs = if configured {
        load_models(&path).with_context(|| format!("reading {}", path.display()))?
    } else {
        Vec::new()
    };

    let mut warnings = Vec::new();
    let auth = load_auth_for_view(&data_dir, &mut warnings);

    let models = configs
        .into_iter()
        .map(|config| {
            let key = model_key_status(&auth, &config);
            ModelRow {
                id: config.id.0,
                provider: config.provider,
                base_url: config.base_url,
                model: config.model,
                provider_id: config.provider_id,
                context_tokens: config.context_tokens,
                key,
            }
        })
        .collect();

    Ok(ModelsView {
        models,
        models_path: path.display().to_string(),
        configured,
        warnings,
        pinned: pinned.map(|id| id.0.clone()),
    })
}

/// The key-status projection for one model entry, exactly as
/// `crates/cli/src/tui.rs::load_key_statuses` derives it: a stored entry wins,
/// then the entry's own `api_key_env` NAME, then missing.
fn model_key_status(auth: &Result<AuthStore, String>, config: &ModelConfig) -> KeyStatus {
    match auth {
        Err(reason) => KeyStatus::Unknown {
            reason: reason.clone(),
        },
        Ok(auth) => {
            if auth.get(&config.id.0).is_some() {
                KeyStatus::Stored
            } else if config.api_key_env.trim().is_empty() {
                KeyStatus::Missing
            } else {
                KeyStatus::Env {
                    name: config.api_key_env.clone(),
                }
            }
        }
    }
}

/// `auth.json` for a read-only projection. A *missing* file is an empty store
/// (`AuthStore::load`'s own contract); a file that exists and does not parse is
/// kept as the error text so every row that depends on it renders
/// [`KeyStatus::Unknown`] instead of a confident "Missing".
fn load_auth_for_view(data_dir: &Path, warnings: &mut Vec<String>) -> Result<AuthStore, String> {
    match AuthStore::load(data_dir) {
        Ok(auth) => Ok(auth),
        Err(error) => {
            let reason = format!(
                "could not read {}: {error}",
                data_dir.join("auth.json").display()
            );
            warnings.push(reason.clone());
            Err(reason)
        }
    }
}

// ---------------------------------------------------------------------------
// Reading: providers
// ---------------------------------------------------------------------------

/// One catalog provider as the desktop shows it. Every derived boolean is the
/// TUI's own gate, ported below rather than re-derived from prose.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderRow {
    pub id: String,
    pub name: String,
    /// Wire protocol label: `openai-chat` | `anthropic` | `gemini-native` |
    /// `acp` | `unknown`.
    pub protocol: String,
    /// Auth label, e.g. `api-key: GROQ_API_KEY` | `none` | `oauth`.
    pub auth: String,
    pub local: bool,
    pub requires_key: bool,
    pub can_list_models: bool,
    /// Whether a model added from this provider is executable by the runtime.
    pub available: bool,
    /// Curated `[[model]]` rows the catalog ships for this provider.
    pub catalog_models: usize,
    /// Whether a provider-wide key resolves (stored, or a documented env var is
    /// set). A boolean — the key itself never crosses this boundary.
    pub has_key: bool,
    /// Why the add-model flow refuses this row, when it does. The TUI's own
    /// refusal text (`crates/tui/src/reduce.rs`).
    pub unusable_reason: Option<String>,
    /// Whether selecting this row must first pass a host-owned trust
    /// confirmation. See [`community_bridge_row`].
    pub community_consent_required: bool,
    /// The risk the confirmation must state, when one is required.
    pub community_consent_detail: Option<String>,
}

/// What [`list_providers`] answers with.
#[derive(Debug, Clone, Serialize)]
pub struct ProvidersView {
    pub providers: Vec<ProviderRow>,
    /// The user overlay file, so the view can name where overrides come from.
    pub providers_path: String,
    pub warnings: Vec<String>,
    /// Why ACP agents are not in this list. The TUI's provider catalog also
    /// carries live-registry and locally-discovered ACP agents, which come from
    /// `codypendent-integrations`; this build does not link it, so those rows
    /// are ABSENT and said to be absent rather than quietly missing.
    pub acp_unavailable: String,
}

const ACP_UNAVAILABLE: &str = "ACP agents are not listed here: the official ACP registry and \
local agent discovery live in codypendent-integrations, which this shell does not link. Use the \
CLI or TUI provider catalog to add an ACP agent.";

/// The catalog, layered with the user's `providers.toml`.
///
/// A `providers.toml` that does not parse falls back to the built-ins with a
/// warning — the TUI's behaviour (`load_provider_cards`), and the right one: the
/// built-in catalog is real curated data, not a placeholder.
pub fn list_providers() -> anyhow::Result<ProvidersView> {
    let data_dir = data_dir()?;
    let overlay = providers_path(&data_dir);
    let mut warnings = Vec::new();
    let catalog = match Catalog::load_with_user_overrides(&overlay) {
        Ok(catalog) => catalog,
        Err(error) => {
            warnings.push(format!("provider catalog fell back to built-ins ({error})"));
            Catalog::builtin()
        }
    };

    let mut catalog_model_counts: BTreeMap<String, usize> = BTreeMap::new();
    for model in catalog.models() {
        *catalog_model_counts
            .entry(model.provider_id.clone())
            .or_default() += 1;
    }

    let auth = AuthStore::load(&data_dir).unwrap_or_else(|error| {
        warnings.push(format!(
            "could not read {}: {error}; provider key presence may be incomplete",
            data_dir.join("auth.json").display()
        ));
        AuthStore::default()
    });

    let mut providers: Vec<ProviderRow> = catalog
        .providers()
        .map(|provider| {
            let available = provider_runtime_supported(provider);
            let protocol = protocol_label(provider.protocol).to_owned();
            ProviderRow {
                id: provider.id.clone(),
                name: provider.name.clone(),
                unusable_reason: (!available).then(|| {
                    format!(
                        "{} is catalog-only — its {protocol} runtime adapter is not installed",
                        provider.id
                    )
                }),
                protocol,
                auth: auth_label(provider),
                local: provider.local,
                requires_key: provider_requires_key(provider),
                can_list_models: provider_can_list_models(provider),
                available,
                catalog_models: catalog_model_counts
                    .get(&provider.id)
                    .copied()
                    .unwrap_or_default(),
                has_key: provider_has_resolvable_key(&provider.id, &auth, &provider_key_envs(provider)),
                community_consent_required: false,
                community_consent_detail: None,
            }
        })
        .collect();

    // The community-bridge gate, carried from `load_provider_cards`. The TUI
    // FORCE-OVERWRITES any catalog row with this id so that a catalog update —
    // remote registry or a hand-edited `providers.toml` — can never publish a
    // row that bypasses the trust confirmation. Same here.
    //
    // Unlike the TUI this does not SYNTHESISE the row when the catalog has
    // none: the TUI's copy is built from a real
    // `codypendent_integrations::acp_registry::community_acp_agent` descriptor,
    // and inventing one here from a hardcoded string would be exactly the
    // fabricated list this client must not show.
    if let Some(row) = providers
        .iter_mut()
        .find(|row| row.id == COMMUNITY_ACP_BRIDGE_ID)
    {
        *row = community_bridge_row();
    }

    // Usable first, local endpoints before hosted, then by name — the TUI's
    // ordering, so the same provider is in the same place in both clients.
    providers.sort_by(|a, b| {
        b.available
            .cmp(&a.available)
            .then_with(|| b.local.cmp(&a.local))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(ProvidersView {
        providers,
        providers_path: overlay.display().to_string(),
        warnings,
        acp_unavailable: ACP_UNAVAILABLE.to_string(),
    })
}

/// The one provider id whose selection is a trust decision rather than a
/// configuration choice.
const COMMUNITY_ACP_BRIDGE_ID: &str = "antigravity-acp";

/// The host-owned row for the community Antigravity bridge.
///
/// Google ships no native Antigravity ACP server; the bridge is a third-party
/// package with its own terms. The TUI gates it behind
/// `Overlay::ConfirmCommunityAcpInstall` and this shell keeps that gate — with
/// an honest terminal state, because installing an ACP agent needs
/// `codypendent-integrations`, which this build does not link. The row is
/// therefore shown, marked, and refused, rather than silently dropped (which
/// would hide that the catalog contains it) or silently accepted.
fn community_bridge_row() -> ProviderRow {
    ProviderRow {
        id: COMMUNITY_ACP_BRIDGE_ID.to_string(),
        name: "Google Antigravity (community bridge)".to_string(),
        protocol: "acp".to_string(),
        auth: "acp: verified install · third-party ToS risk".to_string(),
        local: true,
        requires_key: false,
        can_list_models: false,
        available: false,
        catalog_models: 0,
        has_key: false,
        unusable_reason: Some(
            "installing the community Antigravity bridge is a third-party trust decision, and \
             this shell cannot install ACP agents — add it from the CLI or TUI, where the \
             install is verified against a pinned URL and SHA-256."
                .to_string(),
        ),
        community_consent_required: true,
        community_consent_detail: Some(
            "Google does not ship a native Antigravity ACP server. This is a community bridge \
             with third-party ToS risk: it is not published, reviewed or supported by Google, \
             and using it may breach the terms of the account it drives."
                .to_string(),
        ),
    }
}

/// `crates/cli/src/tui.rs::protocol_label`.
fn protocol_label(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::OpenAiChat => "openai-chat",
        Protocol::Anthropic => "anthropic",
        Protocol::GeminiNative => "gemini-native",
        Protocol::Acp => "acp",
        // `Protocol` is `#[non_exhaustive]`: a future variant this build does
        // not understand still renders, rather than failing to compile.
        _ => "unknown",
    }
}

/// The auth label `load_provider_cards` shows. Names environment VARIABLES,
/// never values.
fn auth_label(provider: &Provider) -> String {
    match provider.auth.first() {
        None | Some(AuthMethod::None) => "none".to_string(),
        Some(AuthMethod::ApiKey { env, .. }) => {
            format!("api-key: {}", env.first().map(String::as_str).unwrap_or(""))
        }
        Some(AuthMethod::Acp { command, .. }) => format!("acp: {command}"),
        Some(AuthMethod::CloudIam { variant, .. }) => format!("cloud-iam: {variant}"),
        Some(AuthMethod::OAuth { .. }) => "oauth".to_string(),
        Some(_) => "unknown".to_string(),
    }
}

/// The documented env var NAMES for a provider's API-key auth method, if it has
/// one. Names only.
fn provider_key_envs(provider: &Provider) -> Vec<String> {
    provider
        .auth
        .iter()
        .find_map(|method| match method {
            AuthMethod::ApiKey { env, .. } => Some(env.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// `crates/cli/src/tui.rs::provider_requires_key`.
fn provider_requires_key(provider: &Provider) -> bool {
    matches!(provider.auth.first(), Some(AuthMethod::ApiKey { .. }))
}

/// `crates/cli/src/tui.rs::provider_endpoint_usable`.
fn provider_endpoint_usable(provider: &Provider) -> bool {
    provider
        .base_url
        .as_deref()
        .is_some_and(|url| !url.trim().is_empty())
        && matches!(
            provider.auth.first(),
            Some(AuthMethod::ApiKey { .. } | AuthMethod::None) | None
        )
}

/// `crates/cli/src/tui.rs::provider_can_list_models`.
fn provider_can_list_models(provider: &Provider) -> bool {
    matches!(provider.protocol, Protocol::OpenAiChat) && provider_endpoint_usable(provider)
}

/// `crates/cli/src/tui.rs::provider_runtime_supported` — the gate that decides
/// whether the add-model flow may write an entry for this provider at all.
fn provider_runtime_supported(provider: &Provider) -> bool {
    matches!(
        provider.protocol,
        Protocol::OpenAiChat | Protocol::Anthropic | Protocol::GeminiNative
    ) && provider_endpoint_usable(provider)
}

/// `crates/cli/src/tui.rs::resolve_provider_api_key`, reduced to the boolean
/// its caller actually needs. Precedence: `auth.json[provider/<id>]`, then the
/// first non-blank documented environment value. Blank and whitespace-only
/// values are absent, never a valid key. The value is dropped here and never
/// returned.
fn provider_has_resolvable_key(provider_id: &str, auth: &AuthStore, env_names: &[String]) -> bool {
    if auth
        .get(&provider_auth_id(provider_id))
        .is_some_and(|key| !key.trim().is_empty())
    {
        return true;
    }
    env_names.iter().any(|name| {
        std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
    })
}

// ---------------------------------------------------------------------------
// Reading: the catalog's curated models for one provider
// ---------------------------------------------------------------------------

/// One curated `[[model]]` row from the provider catalog: what the add-model
/// flow can offer with no network at all.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogModelRow {
    pub id: String,
    pub name: Option<String>,
    /// `None` is unknown. Never rendered as 0.
    pub context_tokens: Option<u64>,
    pub cost_per_1m_input_usd: Option<f64>,
    pub cost_per_1m_output_usd: Option<f64>,
}

/// What [`list_catalog_models`] answers with.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogModelsView {
    pub models: Vec<CatalogModelRow>,
    pub warnings: Vec<String>,
    /// Why a live `/models` list is not offered. The TUI additionally GETs
    /// `{base_url}/models` with the provider key
    /// (`crates/cli/src/tui.rs::query_provider_models`); this shell does not,
    /// so a provider whose curated rows are empty offers the free-text path
    /// rather than a list that looks exhaustive and is not.
    pub live_listing_unavailable: String,
}

const LIVE_LISTING_UNAVAILABLE: &str = "These are the catalog's curated rows, not the provider's \
live /models response. This shell does not query the provider; a model the catalog does not list \
can still be added by typing its provider-side name.";

/// The curated catalog rows for `provider_id`, in catalog order.
pub fn list_catalog_models(provider_id: &str) -> anyhow::Result<CatalogModelsView> {
    let data_dir = data_dir()?;
    let mut warnings = Vec::new();
    let catalog = match Catalog::load_with_user_overrides(&providers_path(&data_dir)) {
        Ok(catalog) => catalog,
        Err(error) => {
            warnings.push(format!("provider catalog fell back to built-ins ({error})"));
            Catalog::builtin()
        }
    };
    if catalog.get(provider_id).is_none() {
        bail!("provider `{provider_id}` is not in the catalog");
    }
    let models = catalog
        .models()
        .filter(|model| model.provider_id == provider_id)
        .map(|model| CatalogModelRow {
            id: model.id.clone(),
            name: model.name.clone(),
            context_tokens: model.context_tokens,
            cost_per_1m_input_usd: model.cost_per_1m_input_usd,
            cost_per_1m_output_usd: model.cost_per_1m_output_usd,
        })
        .collect();
    Ok(CatalogModelsView {
        models,
        warnings,
        live_listing_unavailable: LIVE_LISTING_UNAVAILABLE.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Reading: key statuses
// ---------------------------------------------------------------------------

/// One row of the API-key surface. Presence only.
#[derive(Debug, Clone, Serialize)]
pub struct KeyRow {
    pub target: KeyTarget,
    pub label: String,
    /// Non-secret context: which endpoint, or which provider.
    pub detail: String,
    pub status: KeyStatus,
}

/// What [`key_statuses`] answers with.
#[derive(Debug, Clone, Serialize)]
pub struct KeysView {
    pub keys: Vec<KeyRow>,
    pub auth_path: String,
    pub warnings: Vec<String>,
    /// Credential rows this build does not project, and why. The TUI's `/keys`
    /// also lists the Tavily search key and the `[transcription]`/`[speech]`
    /// voice keys; their `auth.json` ids are load-bearing constants owned by
    /// `codypendent-integrations` and `codypendent-runtime`'s audio clients, so
    /// they are declared unavailable rather than guessed.
    pub unavailable: String,
}

const KEYS_UNAVAILABLE: &str = "The Tavily search key and the voice (speech-to-text / \
text-to-speech) keys are not shown here: their auth.json ids are owned by crates this shell does \
not link, and a wrong id would save a key that reads back as absent. Manage them from the CLI or \
TUI /keys view.";

/// The host part of a `base_url`, for display only — the projection
/// `crates/cli/src/tui.rs::endpoint_host` makes. A string trim rather than a URL
/// parse: this feeds a label, and a `base_url` it cannot make sense of should
/// render as itself rather than vanish.
fn endpoint_host(base_url: &str) -> &str {
    let rest = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest)
        .trim_start_matches('/');
    let host = rest.split('/').next().unwrap_or(rest);
    if host.is_empty() {
        base_url
    } else {
        host
    }
}

/// Which credentials are set — one row per configured model, plus one per
/// provider that either backs a configured model or already holds a
/// provider-wide key.
///
/// Never returns a key. A corrupt `auth.json` yields [`KeyStatus::Unknown`]
/// rows rather than a page of confident "Missing".
pub fn key_statuses() -> anyhow::Result<KeysView> {
    let data_dir = data_dir()?;
    let path = models_path(&data_dir);
    let configs = if path.exists() {
        load_models(&path).with_context(|| format!("reading {}", path.display()))?
    } else {
        Vec::new()
    };

    let mut warnings = Vec::new();
    let auth = load_auth_for_view(&data_dir, &mut warnings);
    let catalog = match Catalog::load_with_user_overrides(&providers_path(&data_dir)) {
        Ok(catalog) => catalog,
        Err(error) => {
            warnings.push(format!("provider catalog fell back to built-ins ({error})"));
            Catalog::builtin()
        }
    };

    let mut keys: Vec<KeyRow> = configs
        .iter()
        .map(|config| KeyRow {
            target: KeyTarget::Model {
                id: config.id.0.clone(),
            },
            label: config.id.0.clone(),
            detail: format!("{} · {}", config.model, endpoint_host(&config.base_url)),
            status: model_key_status(&auth, config),
        })
        .collect();

    // Provider-wide rows: every provider a configured model was added from,
    // plus every provider that already holds a `provider/<id>` entry, so a key
    // stored for a provider whose models were all removed is still visible and
    // still removable. Never every catalog provider — 39 Missing rows would
    // bury the ones that mean something.
    let mut provider_ids: Vec<String> = configs
        .iter()
        .filter_map(|config| config.provider_id.clone())
        .collect();
    if let Ok(auth) = &auth {
        for provider in catalog.providers() {
            if auth.get(&provider_auth_id(&provider.id)).is_some() {
                provider_ids.push(provider.id.clone());
            }
        }
    }
    provider_ids.sort();
    provider_ids.dedup();

    for provider_id in provider_ids {
        let envs = catalog
            .get(&provider_id)
            .map(provider_key_envs)
            .unwrap_or_default();
        let name = catalog
            .get(&provider_id)
            .map_or_else(|| provider_id.clone(), |provider| provider.name.clone());
        let status = match &auth {
            Err(reason) => KeyStatus::Unknown {
                reason: reason.clone(),
            },
            Ok(auth) => {
                if auth.get(&provider_auth_id(&provider_id)).is_some() {
                    KeyStatus::Stored
                } else if let Some(name) = envs
                    .iter()
                    .find(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty()))
                {
                    KeyStatus::Env { name: name.clone() }
                } else {
                    KeyStatus::Missing
                }
            }
        };
        keys.push(KeyRow {
            target: KeyTarget::Provider {
                id: provider_id.clone(),
            },
            label: format!("{name} (provider-wide)"),
            detail: if envs.is_empty() {
                format!("provider/{provider_id}")
            } else {
                format!("provider/{provider_id} · {}", envs.join(", "))
            },
            status,
        });
    }

    Ok(KeysView {
        keys,
        auth_path: data_dir.join("auth.json").display().to_string(),
        warnings,
        unavailable: KEYS_UNAVAILABLE.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Writing: auth.json
// ---------------------------------------------------------------------------

/// Store one API key, or remove one. A port of
/// `crates/cli/src/tui.rs::write_api_key`, with its two guards intact:
///
/// * a blank or whitespace-only key is REFUSED, because `set(id, "")` would
///   silently shadow a valid `api_key_env` and look like it worked;
/// * `AuthStore::load` runs before any write, so a hand-corrupted `auth.json`
///   aborts with a legible error instead of being replaced by a fresh store
///   holding one entry.
///
/// Removing an absent entry skips the save entirely — nothing changed, and no
/// empty `auth.json` is created for a store that never existed.
pub fn write_api_key(target: &KeyTarget, key: Option<&SecretKey>) -> anyhow::Result<()> {
    let data_dir = data_dir()?;
    let id = target.auth_id();
    let mut auth = AuthStore::load(&data_dir)
        .with_context(|| format!("reading {}", data_dir.join("auth.json").display()))?;
    match key {
        Some(key) => {
            let key = key.0.trim();
            if key.is_empty() {
                bail!("key must not be blank");
            }
            auth.set(id, key);
            auth.save(&data_dir)
                .with_context(|| format!("writing {}", data_dir.join("auth.json").display()))?;
        }
        None => {
            if auth.remove(&id) {
                auth.save(&data_dir)
                    .with_context(|| format!("writing {}", data_dir.join("auth.json").display()))?;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Writing: models.toml (add)
// ---------------------------------------------------------------------------

/// A provider `base_url` as it should be persisted: trailing slashes trimmed.
/// `crates/cli/src/tui.rs::normalize_base_url` — the catalog stores a few with
/// one (`…/v1/`) and the chat client joins `{base}/chat/completions`, so the raw
/// value would produce a double slash on every request.
fn normalize_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

/// Add (or update in place) one `[[model]]` entry, and optionally store its key.
///
/// A port of `crates/cli/src/tui.rs::write_add_model` minus its ACP arm (this
/// build cannot launch ACP agents). Every refusal is carried:
///
/// * a blank `display_id` writes nothing to EITHER file;
/// * a blank `api_key` is treated exactly as `None` — the `auth.json` write is
///   skipped rather than storing an empty string that would shadow a valid
///   `api_key_env`;
/// * when a key IS present, `auth.json` is loaded BEFORE `models.toml` is
///   written, so a corrupt key store aborts the whole add rather than leaving a
///   keyless model entry behind;
/// * a provider absent from the catalog, or one whose runtime adapter is not
///   installed, is a hard error — never a written entry that can only fail at
///   run time;
/// * a duplicate `display_id` UPDATES in place; it never appends a second row.
pub fn add_model(
    display_id: &str,
    provider_id: &str,
    model: &str,
    api_key: Option<&SecretKey>,
    context_tokens: Option<u64>,
) -> anyhow::Result<()> {
    if display_id.trim().is_empty() {
        bail!("model id must not be blank");
    }
    // Not in the reference, which reaches this function only from a picker that
    // cannot produce a blank model. Reached from a webview, it can — and an
    // entry with an empty provider-side model name is unrunnable, so it is
    // refused here rather than written.
    if model.trim().is_empty() {
        bail!("provider-side model name must not be blank");
    }
    let display_id = display_id.trim();
    let model = model.trim();

    let data_dir = data_dir()?;
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating the data dir {}", data_dir.display()))?;

    // A blank/whitespace-only key is treated exactly like `None`, filtered up
    // front so the load-order guard below and the final write agree on whether
    // a key is really present.
    let key = api_key.filter(|key| !key.0.trim().is_empty());

    // All-or-nothing: when a key is present, load auth.json NOW, before
    // models.toml is written, so a corrupt store surfaces here instead of
    // leaving a keyless models.toml entry behind while the key silently fails
    // to save. A keyless add loads no auth.json at all.
    let mut auth = key
        .is_some()
        .then(|| {
            AuthStore::load(&data_dir)
                .with_context(|| format!("reading {}", data_dir.join("auth.json").display()))
        })
        .transpose()?;

    let catalog = Catalog::load_with_user_overrides(&providers_path(&data_dir))
        .unwrap_or_else(|_| Catalog::builtin());
    let provider = catalog
        .get(provider_id)
        .ok_or_else(|| anyhow!("provider `{provider_id}` is not in the catalog"))?;
    if provider_id == COMMUNITY_ACP_BRIDGE_ID {
        bail!(
            "`{COMMUNITY_ACP_BRIDGE_ID}` is a community ACP bridge; this shell cannot install or \
             run ACP agents"
        );
    }
    if !provider_runtime_supported(provider) {
        bail!(
            "provider `{provider_id}` uses {} and is not executable by this build",
            protocol_label(provider.protocol)
        );
    }
    let base_url = normalize_base_url(provider.base_url.as_deref().ok_or_else(|| {
        anyhow!("provider `{provider_id}` has no base URL and cannot be configured")
    })?);

    // A catalog row for this exact model fills in the context window when the
    // caller did not already know it.
    let context_tokens = context_tokens.or_else(|| {
        catalog
            .model(provider_id, model)
            .and_then(|row| row.context_tokens)
    });

    let config = ModelConfig {
        id: ModelId(display_id.to_string()),
        provider: "openai-compatible".to_string(),
        base_url,
        model: model.to_string(),
        // The key, if any, goes to auth.json — never into models.toml, which
        // holds env var NAMES only.
        api_key_env: String::new(),
        provider_id: Some(provider_id.to_string()),
        context_tokens,
    };
    update_model_entries(&models_path(&data_dir), |configs| {
        configs.retain(|existing| existing.id.0 != display_id);
        configs.push(config);
        Ok(())
    })?;

    if let Some(key) = key {
        let auth = auth
            .as_mut()
            .expect("loaded above because `key` is Some (load-before-write ordering)");
        auth.set(display_id, key.0.trim());
        // Also store it provider-wide, so adding a second model from the same
        // provider needs no second paste of the same key. The runtime reads
        // this entry after the per-model one.
        auth.set(provider_auth_id(provider_id), key.0.trim());
        auth.save(&data_dir)
            .with_context(|| format!("writing {}", data_dir.join("auth.json").display()))?;
    }
    Ok(())
}

/// Serialize one read-modify-write of the shared model list under an advisory
/// lock, preserving every other table.
///
/// A port of `crates/cli/src/models_file.rs::update_model_entries`, which
/// exists because four independent writers each rebuilt `models.toml` from a
/// model-only struct and thereby deleted the user's `[embedding]`,
/// `[retrieval]`, `[transcription]` and `[speech]` configuration. The invariant
/// it encodes: EDIT THE PARSED DOCUMENT IN PLACE; never serialize the file from
/// a struct that models only one section. An unknown table is carried through
/// untouched.
///
/// The lock is the same `<data_dir>/.models.toml.lock` the CLI and TUI take, so
/// a desktop edit and a `codypendent models add` running at the same moment
/// cannot erase each other.
fn update_model_entries<R>(
    path: &Path,
    edit: impl FnOnce(&mut Vec<ModelConfig>) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{}: has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let lock_path = parent.join(".models.toml.lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("opening {}", lock_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("securing {}", lock_path.display()))?;
    }
    fs4::FileExt::lock_exclusive(&lock)
        .with_context(|| format!("locking {}", lock_path.display()))?;

    let mut configs = if path.exists() {
        load_models(path).with_context(|| format!("reading {}", path.display()))?
    } else {
        Vec::new()
    };
    let result = edit(&mut configs)?;
    write_model_entries_locked(path, &configs)?;
    let _ = fs4::FileExt::unlock(&lock);
    Ok(result)
}

/// Replace only the `model` key of the parsed document and install it
/// atomically at mode 0600. `crates/cli/src/models_file.rs`.
fn write_model_entries_locked(path: &Path, configs: &[ModelConfig]) -> anyhow::Result<()> {
    let mut document = if path.exists() {
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str::<toml::Value>(&raw).with_context(|| format!("parsing {}", path.display()))?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let table = document
        .as_table_mut()
        .ok_or_else(|| anyhow!("{}: root must be a TOML table", path.display()))?;
    table.insert(
        "model".to_string(),
        toml::Value::try_from(configs).context("serializing models.toml")?,
    );
    let rendered = toml::to_string_pretty(&document).context("serializing models.toml")?;

    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{}: has no parent directory", path.display()))?;
    // Unique per WRITE, not per process: two writes inside one process must not
    // share a temp path, or one can rename the other's half-written render into
    // place.
    static WRITE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ticket = WRITE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = parent.join(format!(".models-{}-{ticket}.tmp", std::process::id()));
    let mut temp_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .with_context(|| format!("creating {}", temp.display()))?;
    temp_file
        .write_all(rendered.as_bytes())
        .with_context(|| format!("writing {}", temp.display()))?;
    temp_file
        .sync_all()
        .with_context(|| format!("syncing {}", temp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // An entry names the environment variable holding a key, never the key
        // itself, but the endpoint list is still the user's business alone.
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("securing {}", temp.display()))?;
    }
    std::fs::rename(&temp, path).with_context(|| format!("replacing {}", path.display()))?;
    #[cfg(unix)]
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("syncing {}", parent.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Writing: models.toml (remove)
// ---------------------------------------------------------------------------

/// Remove one `[[model]]` entry and its stored key.
///
/// A port of `crates/cli/src/tui.rs::write_remove_model`. Unlike the add path,
/// removal edits the document with `toml_edit`, which PRESERVES the file's
/// comments, key order and formatting — a user's `models.toml` is a document
/// they own, and silently reformatting it while deleting one row is a defect.
///
/// The two halves commit in an order that leaves neither behind: the key store
/// is written first, and either failure path undoes the other half, so a failed
/// `auth.save` leaves `models.toml` untouched and a failed rename restores the
/// original key store.
pub fn remove_model(model_id: &str) -> anyhow::Result<()> {
    let model_id = model_id.trim();
    if model_id.is_empty() {
        bail!("model id must not be blank");
    }
    let data_dir = data_dir()?;
    let models_path = models_path(&data_dir);

    // Validate the key store before touching models.toml. A corrupt auth.json
    // must never be silently overwritten or leave the operator unsure which
    // half of the requested cleanup happened.
    let mut auth = AuthStore::load(&data_dir)
        .with_context(|| format!("reading {}", data_dir.join("auth.json").display()))?;
    let original_auth = auth.clone();
    let removed_key = auth.remove(model_id);

    let parent = models_path
        .parent()
        .ok_or_else(|| anyhow!("{}: has no parent directory", models_path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let lock_path = parent.join(".models.toml.lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("opening {}", lock_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("securing {}", lock_path.display()))?;
    }
    fs4::FileExt::lock_exclusive(&lock)
        .with_context(|| format!("locking {}", lock_path.display()))?;

    let raw = std::fs::read_to_string(&models_path)
        .with_context(|| format!("reading {}", models_path.display()))?;
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .with_context(|| format!("parsing {}", models_path.display()))?;

    // Remove EVERY entry carrying this id, not just the first: a hand-edited
    // `models.toml` can list the same id twice, and `auth.remove` above is
    // unconditional — leaving one copy behind would leave a listed model with
    // no credential.
    let emptied = {
        let Some(item) = doc.get_mut("model") else {
            bail!("model `{model_id}` is not configured");
        };
        let Some(array) = item.as_array_of_tables_mut() else {
            bail!(
                "`model` entry in {} is not an array of tables",
                models_path.display()
            );
        };
        let matching: Vec<usize> = array
            .iter()
            .enumerate()
            .filter_map(|(index, table)| {
                (table.get("id").and_then(toml_edit::Item::as_str) == Some(model_id))
                    .then_some(index)
            })
            .collect();
        if matching.is_empty() {
            bail!("model `{model_id}` is not configured");
        }
        for index in matching.into_iter().rev() {
            array.remove(index);
        }
        array.is_empty()
    };
    if emptied {
        doc.remove("model");
    }

    let tmp_path = parent.join(format!(".models-{}.toml.tmp", std::process::id()));
    let write_tmp = || -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp_path)?;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            file.write_all(doc.to_string().as_bytes())?;
            file.sync_all()?;
        }
        #[cfg(not(unix))]
        std::fs::write(&tmp_path, doc.to_string())?;
        Ok(())
    };
    if let Err(error) = write_tmp() {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(error).with_context(|| format!("writing {}", tmp_path.display()));
    }

    // Commit the secret cleanup first, then make the already-written model temp
    // visible. Either half can still fail, so each failure path undoes the
    // other half: the operation is all-or-nothing in both directions.
    if removed_key {
        if let Err(error) = auth.save(&data_dir) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(error)
                .with_context(|| format!("writing {}", data_dir.join("auth.json").display()));
        }
    }
    if let Err(error) = std::fs::rename(&tmp_path, &models_path) {
        let _ = std::fs::remove_file(&tmp_path);
        if removed_key {
            original_auth.save(&data_dir).with_context(|| {
                format!(
                    "restoring {} after models.toml replacement failed",
                    data_dir.join("auth.json").display()
                )
            })?;
        }
        return Err(error).with_context(|| format!("replacing {}", models_path.display()));
    }
    let _ = fs4::FileExt::unlock(&lock);

    Ok(())
}

/// Whether `model_id` names an entry that is actually in `models.toml`.
///
/// The gate under pinning a model for the next run: a pin that names nothing
/// configured would make the daemon refuse the run later, with the refusal
/// attributed to the run rather than to the pin.
pub fn model_is_configured(model_id: &ModelId) -> anyhow::Result<bool> {
    let data_dir = data_dir()?;
    let path = models_path(&data_dir);
    if !path.exists() {
        return Ok(false);
    }
    let configs = load_models(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(configs.iter().any(|config| config.id == *model_id))
}

// ---------------------------------------------------------------------------
// Mode
// ---------------------------------------------------------------------------

/// One selectable agent mode.
///
/// `crates/tui/src/state.rs::MODE_CARDS` is `pub(crate)` and cannot be
/// imported, so the five cards are re-declared here with the labels and
/// summaries copied from it verbatim. `AgentMode` itself is the protocol enum —
/// only the presentation text is duplicated.
#[derive(Debug, Clone, Serialize)]
pub struct ModeCard {
    /// The protocol `AgentMode`, serialized as `{ "type": "Build" }`.
    pub mode: codypendent_protocol::AgentMode,
    pub label: &'static str,
    pub summary: &'static str,
}

/// The modes the picker offers, in the TUI's presentation order.
pub fn mode_cards() -> Vec<ModeCard> {
    use codypendent_protocol::AgentMode;
    vec![
        ModeCard {
            mode: AgentMode::Ask,
            label: "Ask",
            summary: "read-only Q&A — no writes, commands, or network",
        },
        ModeCard {
            mode: AgentMode::Explore,
            label: "Explore",
            summary: "read-only investigation — no writes, commands, or network",
        },
        ModeCard {
            mode: AgentMode::Plan,
            label: "Plan",
            summary: "investigate read-only, then finish with a numbered implementation plan",
        },
        ModeCard {
            mode: AgentMode::Build,
            label: "Build",
            summary: "full worktree access — writes, commands, and network (the default)",
        },
        ModeCard {
            mode: AgentMode::Review,
            label: "Review",
            summary: "read-only verification with commands — no writes or network",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one rule this module exists to keep: a key can be received and
    /// written, and can never be printed.
    #[test]
    fn a_secret_key_never_appears_in_its_own_debug() {
        let key = SecretKey("sk-not-in-the-output".to_string());
        let rendered = format!("{key:?}");
        assert!(!rendered.contains("sk-not-in-the-output"), "{rendered}");
        assert_eq!(rendered, "SecretKey(<redacted>)");
    }

    /// A key target names an id, never key material — so a debug log of a
    /// failed write cannot leak the key it was writing.
    #[test]
    fn a_key_target_carries_only_a_non_secret_id() {
        let target = KeyTarget::Provider {
            id: "groq".to_string(),
        };
        assert_eq!(target.auth_id(), "provider/groq");
        assert_eq!(
            KeyTarget::Model {
                id: "groq/llama".to_string()
            }
            .auth_id(),
            "groq/llama"
        );
    }

    /// A base URL reaches the chat client as `{base}/chat/completions`, so a
    /// catalog value written with a trailing slash must not persist with one.
    #[test]
    fn a_persisted_base_url_has_no_trailing_slash() {
        assert_eq!(
            normalize_base_url(" https://api.example.com/v1/ "),
            "https://api.example.com/v1"
        );
    }

    /// The gate that decides whether the add flow may write an entry at all.
    #[test]
    fn a_provider_with_no_base_url_is_not_runtime_supported() {
        let provider = Provider {
            id: "oauth-only".to_string(),
            name: "OAuth only".to_string(),
            protocol: Protocol::OpenAiChat,
            base_url: Some("   ".to_string()),
            auth: vec![AuthMethod::None],
            extra_headers: BTreeMap::new(),
            query_params: BTreeMap::new(),
            local: false,
        };
        assert!(!provider_runtime_supported(&provider));
        assert!(!provider_can_list_models(&provider));
    }

    /// The community bridge is a trust decision, and this build cannot make it
    /// succeed — so it must say so rather than look like an ordinary provider.
    #[test]
    fn the_community_bridge_row_is_gated_and_unavailable() {
        let row = community_bridge_row();
        assert!(row.community_consent_required);
        assert!(!row.available);
        assert!(row.community_consent_detail.is_some());
        assert!(row.unusable_reason.is_some());
    }

    /// A `models.toml` a human owns holds more than models. Adding one must
    /// not delete their embedding, retrieval or voice configuration — the bug
    /// `crates/cli/src/models_file.rs` exists to make unrepresentable.
    #[test]
    fn rewriting_the_model_array_preserves_every_other_table() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("models.toml");
        std::fs::write(
            &path,
            r#"
[[model]]
id = "existing/one"
provider = "openai-compatible"
base_url = "https://api.example.com/v1"
model = "existing-one"
api_key_env = "EXAMPLE_KEY"

[embedding]
provider = "ollama"
base_url = "http://localhost:11434/v1"
model = "nomic-embed-text"

[speech]
base_url = "http://localhost:9000/v1"
model = "tts-1"
voice = "alloy"
"#,
        )
        .expect("seed");

        update_model_entries(&path, |configs| {
            configs.push(ModelConfig {
                id: ModelId("added/two".to_string()),
                provider: "openai-compatible".to_string(),
                base_url: "https://api.example.com/v1".to_string(),
                model: "added-two".to_string(),
                api_key_env: String::new(),
                provider_id: Some("example".to_string()),
                context_tokens: None,
            });
            Ok(())
        })
        .expect("update");

        let raw = std::fs::read_to_string(&path).expect("read back");
        assert!(raw.contains("nomic-embed-text"), "{raw}");
        assert!(raw.contains("tts-1"), "{raw}");
        let configs = load_models(&path).expect("parse");
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[1].id.0, "added/two");
    }
}
