//! The curated built-in provider catalog + a loader that layers a user
//! `providers.toml` over it (a user entry with the same `id` shadows a built-in).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::model::{Model, Provider, ProvidersFile};

/// The curated catalog, embedded at build time.
const BUILTIN_CATALOG_TOML: &str = include_str!("../builtin_catalog.toml");

/// A failure loading a user provider catalog.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("failed to read provider catalog at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse provider catalog at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

/// Parse the embedded built-in catalog into its providers. Panics only on a
/// malformed embedded catalog — a build-time invariant pinned by a unit test.
#[must_use]
pub fn builtin_providers() -> Vec<Provider> {
    let file: ProvidersFile =
        toml::from_str(BUILTIN_CATALOG_TOML).expect("the embedded built-in catalog is valid TOML");
    file.providers
}

/// Models shipped alongside the built-in providers.
#[must_use]
pub fn builtin_models() -> Vec<Model> {
    let file: ProvidersFile =
        toml::from_str(BUILTIN_CATALOG_TOML).expect("the embedded built-in catalog is valid TOML");
    file.models
}

/// The resolved provider catalog, keyed by id (BTreeMap → stable ordering for the
/// picker).
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    providers: BTreeMap<String, Provider>,
    models: BTreeMap<(String, String), Model>,
}

impl Catalog {
    /// The built-in catalog only.
    #[must_use]
    pub fn builtin() -> Self {
        Self::from_parts(builtin_providers(), builtin_models())
    }

    /// Build from an explicit provider list (later ids overwrite earlier).
    pub fn from_providers(providers: impl IntoIterator<Item = Provider>) -> Self {
        Self::from_parts(providers, std::iter::empty())
    }

    /// Build from explicit providers and model metadata. Later duplicate keys
    /// overwrite earlier ones deterministically.
    pub fn from_parts(
        providers: impl IntoIterator<Item = Provider>,
        models: impl IntoIterator<Item = Model>,
    ) -> Self {
        let providers = providers.into_iter().map(|p| (p.id.clone(), p)).collect();
        let models = models
            .into_iter()
            .map(|m| ((m.provider_id.clone(), m.id.clone()), m))
            .collect();
        Self { providers, models }
    }

    /// Built-ins, then the user's `providers.toml` layered on top (same id shadows;
    /// new ids extend). A missing user file is fine — the built-ins stand alone.
    pub fn load_with_user_overrides(path: &Path) -> Result<Self, CatalogError> {
        let mut providers: BTreeMap<String, Provider> = builtin_providers()
            .into_iter()
            .map(|p| (p.id.clone(), p))
            .collect();
        let mut models: BTreeMap<(String, String), Model> = builtin_models()
            .into_iter()
            .map(|m| ((m.provider_id.clone(), m.id.clone()), m))
            .collect();
        if path.exists() {
            let text = std::fs::read_to_string(path).map_err(|source| CatalogError::Read {
                path: path.to_path_buf(),
                source,
            })?;
            let file: ProvidersFile =
                toml::from_str(&text).map_err(|source| CatalogError::Parse {
                    path: path.to_path_buf(),
                    source,
                })?;
            for p in file.providers {
                providers.insert(p.id.clone(), p);
            }
            for model in file.models {
                models.insert((model.provider_id.clone(), model.id.clone()), model);
            }
        }
        Ok(Self { providers, models })
    }

    /// Look up a provider by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Provider> {
        self.providers.get(id)
    }

    /// Iterate every provider, in id order.
    pub fn providers(&self) -> impl Iterator<Item = &Provider> {
        self.providers.values()
    }

    /// Look up model metadata by provider and provider-side model id.
    #[must_use]
    pub fn model(&self, provider_id: &str, id: &str) -> Option<&Model> {
        self.models.get(&(provider_id.to_string(), id.to_string()))
    }

    /// Iterate all catalog model rows in stable provider/id order.
    pub fn models(&self) -> impl Iterator<Item = &Model> {
        self.models.values()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Protocol;
    use std::io::Write;

    #[test]
    fn builtin_catalog_parses_and_has_known_providers() {
        let providers = builtin_providers();
        assert!(
            providers.len() >= 39,
            "expected ~39 providers, got {}",
            providers.len()
        );
        let cat = Catalog::builtin();
        // A provider a user can select must be one they can reach. These three
        // were live-probed dead on 2026-08-13 (ai21 and github-models answer
        // HTTP 410 "retired"; api.lambda.ai does not resolve) and removed, each
        // having curated zero models — so picking one produced a network/auth
        // failure where the honest answer was "this API was retired". Re-adding
        // any of them needs a fresh probe, not a hopeful commit.
        for retired in ["ai21", "github-models", "lambda"] {
            assert!(
                cat.get(retired).is_none(),
                "`{retired}` is dead upstream and must not be offered"
            );
        }
        assert_eq!(
            cat.get("openai").map(|p| p.protocol),
            Some(Protocol::OpenAiChat)
        );
        assert_eq!(
            cat.get("anthropic").map(|p| p.protocol),
            Some(Protocol::Anthropic)
        );
        assert_eq!(
            cat.get("claude-code").map(|p| p.protocol),
            Some(Protocol::Acp)
        );
        assert!(cat.get("ollama").is_some_and(|p| p.local));
        assert!(cat.get("groq").is_some());
    }

    #[test]
    fn a_user_providers_toml_shadows_a_builtin_by_id() {
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        // Override the built-in `groq` base_url and add a brand-new provider.
        write!(
            file,
            r#"
[[provider]]
id = "groq"
name = "Groq (my proxy)"
protocol = "openai-chat"
base_url = "http://localhost:9000/v1"
[[provider.auth]]
kind = "none"

[[provider]]
id = "my-gateway"
name = "My Gateway"
protocol = "openai-chat"
base_url = "https://gw.example/v1"
[[provider.auth]]
kind = "api_key"
env = ["MY_GATEWAY_KEY"]
"#
        )
        .expect("write");

        let cat = Catalog::load_with_user_overrides(file.path()).expect("load");
        // Built-in groq is shadowed by the user entry.
        let groq = cat.get("groq").expect("groq present");
        assert_eq!(groq.base_url.as_deref(), Some("http://localhost:9000/v1"));
        // The user's new provider is added.
        assert!(cat.get("my-gateway").is_some());
        // Untouched built-ins remain.
        assert!(cat.get("openai").is_some());
    }

    #[test]
    fn an_unknown_protocol_is_a_clear_parse_error_not_a_panic() {
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        write!(
            file,
            r#"
[[provider]]
id = "bogus"
name = "Bogus"
protocol = "not-a-real-protocol"
base_url = "https://example.invalid/v1"
[[provider.auth]]
kind = "api_key"
env = ["BOGUS_API_KEY"]
"#
        )
        .expect("write");

        let err = Catalog::load_with_user_overrides(file.path())
            .expect_err("an unknown protocol must fail to parse, not panic or silently drop");
        assert!(matches!(err, CatalogError::Parse { .. }));
        // The error names the offending file, and the underlying serde message
        // surfaces (rather than being swallowed) so a user can fix their config.
        let message = err.to_string();
        assert!(message.contains("failed to parse provider catalog"));
    }

    #[test]
    fn an_unknown_auth_kind_is_a_clear_parse_error_not_a_panic() {
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        write!(
            file,
            r#"
[[provider]]
id = "bogus-auth"
name = "Bogus Auth"
protocol = "openai-chat"
base_url = "https://example.invalid/v1"
[[provider.auth]]
kind = "not_a_real_kind"
"#
        )
        .expect("write");

        let err = Catalog::load_with_user_overrides(file.path())
            .expect_err("an unknown auth kind must fail to parse, not panic or silently drop");
        assert!(matches!(err, CatalogError::Parse { .. }));
    }
}
