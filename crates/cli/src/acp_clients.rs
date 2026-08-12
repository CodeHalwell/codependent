//! Manage external ACP agents from the official registry.

use std::path::Path;

use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use codypendent_integrations::acp::PermissionOption;
use codypendent_integrations::acp_client::{AcpClient, AcpClientError, AcpEventSink};
use codypendent_integrations::acp_registry::{
    agent_coordinate, agent_coordinate_with_model, agent_id_from_coordinate,
    agent_model_from_coordinate, local_kimi_code_spec, AcpRegistry, AcpRegistryStore,
};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::{EventBody, ModelId, RunId};
use codypendent_runtime::models::{load_models, ModelConfig};

pub async fn refresh(paths: &RuntimePaths) -> anyhow::Result<()> {
    let registry = AcpRegistryStore::new(&paths.data_dir).refresh().await?;
    println!(
        "refreshed ACP registry {} ({} agents)",
        registry.version,
        registry.agents.len()
    );
    Ok(())
}

pub async fn list(paths: &RuntimePaths, refresh: bool, json: bool) -> anyhow::Result<()> {
    let store = AcpRegistryStore::new(&paths.data_dir);
    let registry = load_registry(&store, refresh).await?;
    let connected = connected_profiles(paths)?;
    if json {
        let mut rows = registry
            .agents
            .iter()
            .map(|agent| {
                let status = agent_status(&store, agent);
                serde_json::json!({
                    "id": agent.id,
                    "name": agent.name,
                    "version": agent.version,
                    "description": agent.description,
                    "repository": agent.repository,
                    "distribution": distribution_label(agent),
                    "ready": status.starts_with("ready"),
                    "status": status,
                    "connectedProfiles": connected.iter().filter_map(|(id, registry_id)| (agent_id_from_coordinate(registry_id) == agent.id).then_some(id)).collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        if let Some(spec) = local_kimi_code_spec() {
            rows.push(serde_json::json!({
                "id": spec.registry_id,
                "name": spec.name,
                "version": spec.version,
                "description": "Locally installed Kimi Code native ACP server",
                "repository": "https://github.com/MoonshotAI/kimi-code",
                "distribution": "local",
                "ready": true,
                "status": "ready (local ACP executable)",
                "connectedProfiles": connected.iter().filter_map(|(id, registry_id)| (agent_id_from_coordinate(registry_id) == spec.registry_id).then_some(id)).collect::<Vec<_>>()
            }));
        }
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    println!("{:<24} {:<28} {:<12} STATE", "ID", "AGENT", "VERSION");
    for agent in &registry.agents {
        let profiles = connected
            .iter()
            .filter_map(|(id, registry_id)| {
                (agent_id_from_coordinate(registry_id) == agent.id).then_some(id.as_str())
            })
            .collect::<Vec<_>>();
        let state = if profiles.is_empty() {
            agent_status(&store, agent)
        } else {
            format!(
                "connected as {} · {}",
                profiles.join(", "),
                agent_status(&store, agent)
            )
        };
        println!(
            "{:<24} {:<28} {:<12} {}",
            agent.id, agent.name, agent.version, state
        );
    }
    if let Some(spec) = local_kimi_code_spec() {
        let profiles = connected
            .iter()
            .filter_map(|(id, registry_id)| {
                (agent_id_from_coordinate(registry_id) == spec.registry_id).then_some(id.as_str())
            })
            .collect::<Vec<_>>();
        let state = if profiles.is_empty() {
            "ready (local ACP executable)".to_string()
        } else {
            format!(
                "connected as {} · ready (local ACP executable)",
                profiles.join(", ")
            )
        };
        println!(
            "{:<24} {:<28} {:<12} {}",
            spec.registry_id, spec.name, spec.version, state
        );
    }
    Ok(())
}

pub async fn install(
    paths: &RuntimePaths,
    agent: &str,
    refresh: bool,
    allow_unverified: bool,
) -> anyhow::Result<()> {
    let store = AcpRegistryStore::new(&paths.data_dir);
    load_registry(&store, refresh).await?;
    let spec = store.install(agent, allow_unverified).await?;
    println!(
        "installed {} {} ({})",
        spec.name,
        spec.version,
        spec.command.display()
    );
    Ok(())
}

pub async fn connect(
    paths: &RuntimePaths,
    agent: &str,
    profile: Option<&str>,
    refresh: bool,
    allow_unverified: bool,
    repository: &Path,
) -> anyhow::Result<()> {
    let store = AcpRegistryStore::new(&paths.data_dir);
    load_registry(&store, refresh).await?;
    let spec = store.install(agent, allow_unverified).await?;

    // Handshake before changing models.toml: a missing launcher, incompatible
    // adapter, or failed session/new leaves no broken selectable profile.
    let command = spec.command.to_string_lossy();
    let client = AcpClient::spawn(
        &command,
        &spec.args,
        &spec.env,
        repository.to_string_lossy().as_ref(),
    )
    .await
    .map_err(|error| handshake_failure(agent, &error))?;
    // The handshake told us which models this agent owns; one profile per model
    // makes them selectable everywhere a model id is (`/model`, councils,
    // `--model`), alongside the bare profile that leaves the agent on its own
    // default. An agent advertising none degrades to exactly the bare profile.
    let models = client.discovered_models();
    let modes = client.discovered_modes();
    drop(client);

    let base = profile
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("acp/{}", spec.registry_id));
    let coordinate = agent_coordinate(&spec.registry_id, &spec.version);
    let mut profiles = vec![(base.clone(), coordinate.clone())];
    for model in &models {
        profiles.push((
            model_profile_id(&base, &model.id),
            agent_coordinate_with_model(&spec.registry_id, &spec.version, &model.id),
        ));
    }
    replace_profile_family(paths, &base, &profiles)?;

    println!(
        "connected {} {} as model `{base}` (agent default)",
        spec.name, spec.version
    );
    if models.is_empty() {
        println!("the agent advertised no model selector; it keeps its own default model");
    } else {
        println!("discovered {} agent model(s):", models.len());
        for (model, (id, _)) in models.iter().zip(profiles.iter().skip(1)) {
            let marker = if model.current { "*" } else { " " };
            println!("  {marker} {id:<40} {} ({})", model.name, model.id);
        }
    }
    if !modes.is_empty() {
        let names = modes
            .iter()
            .map(|mode| mode.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!("session modes: {names}");
    }
    println!("Open /model in the TUI or run with one of these model pins.");
    Ok(())
}

/// The profile id for one of an agent's own models: the connected profile plus
/// the agent-model id, mirroring the `id@version#model` coordinate it stores.
fn model_profile_id(base: &str, model_id: &str) -> String {
    format!("{base}#{model_id}")
}

/// Turn a failed handshake into an actionable error. The client already names
/// the agent's advertised authentication methods inside the error; this adds
/// the one thing it cannot know — that authentication happens in the agent's
/// own CLI, not in Codypendent, which holds no credentials for it.
fn handshake_failure(agent: &str, error: &AcpClientError) -> anyhow::Error {
    let message = error.to_string();
    let hint = if message.contains("the agent advertises authentication:") {
        "\nAuthenticate with the agent's own CLI (Codypendent stores no credentials for it), then re-run this command."
    } else {
        ""
    };
    anyhow!("ACP handshake with `{agent}` failed: {message}{hint}")
}

/// Exercise a real ACP `session/prompt` without persisting a model profile.
/// Tool permission requests are always rejected, making this suitable for a
/// compatibility/authentication smoke test against live vendor clients.
pub async fn probe(
    paths: &RuntimePaths,
    agent: &str,
    prompt: &str,
    refresh: bool,
    allow_unverified: bool,
    repository: &Path,
) -> anyhow::Result<()> {
    if prompt.is_empty() || prompt.len() > 32 * 1024 || prompt.contains('\0') {
        bail!("ACP probe prompt must contain 1..=32768 bytes and no NUL");
    }
    let store = AcpRegistryStore::new(&paths.data_dir);
    load_registry(&store, refresh).await?;
    let spec = store.install(agent, allow_unverified).await?;
    let command = spec.command.to_string_lossy();
    let mut client = AcpClient::spawn(
        &command,
        &spec.args,
        &spec.env,
        repository.to_string_lossy().as_ref(),
    )
    .await
    .map_err(|error| handshake_failure(agent, &error))?;
    let discovered = client.discovered_models();
    let mut sink = ProbeSink::default();
    let stop = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        client.prompt(prompt, RunId::new(), &mut sink),
    )
    .await
    .with_context(|| format!("live ACP prompt to `{agent}` timed out after 120 seconds"))?
    .with_context(|| format!("live ACP prompt to `{agent}` failed"))?;
    drop(client);
    let safe_output = codypendent_tui::sanitize_accessible_text(&sink.output);
    println!(
        "{} {} live prompt: {:?}\n{}",
        spec.name,
        spec.version,
        stop,
        if safe_output.trim().is_empty() {
            "(agent returned no text)"
        } else {
            safe_output.trim()
        }
    );
    // A probe is also the cheapest way to see what a live agent advertises,
    // without persisting anything.
    if !discovered.is_empty() {
        println!(
            "advertised models: {}",
            discovered
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

#[derive(Default)]
struct ProbeSink {
    output: String,
}

#[async_trait]
impl AcpEventSink for ProbeSink {
    async fn on_event(&mut self, event: EventBody) {
        if let EventBody::ModelStreamDelta { text, .. } = event {
            const MAX_OUTPUT: usize = 1024 * 1024;
            let remaining = MAX_OUTPUT.saturating_sub(self.output.len());
            if remaining > 0 {
                let mut end = text.len().min(remaining);
                while !text.is_char_boundary(end) {
                    end = end.saturating_sub(1);
                }
                self.output.push_str(&text[..end]);
            }
        }
    }

    async fn on_permission(
        &mut self,
        _tool_call: serde_json::Value,
        options: Vec<PermissionOption>,
    ) -> Option<String> {
        options
            .iter()
            .find(|option| option.kind == "reject_once")
            .or_else(|| {
                options
                    .iter()
                    .find(|option| option.kind.starts_with("reject"))
            })
            .map(|option| option.option_id.clone())
    }
}

pub fn disconnect(paths: &RuntimePaths, profile: &str) -> anyhow::Result<()> {
    let models_path = paths.data_dir.join("models.toml");
    if !models_path.exists() {
        bail!("no configured models");
    }
    let mut configs = load_models(&models_path)?;
    let before = configs.len();
    let child_prefix = format!("{profile}#");
    configs.retain(|config| {
        !(config.provider == "acp"
            && (config.id.0 == profile || config.id.0.starts_with(&child_prefix)))
    });
    if configs.len() == before {
        bail!("ACP profile `{profile}` is not configured");
    }
    write_models(&models_path, &configs)?;
    println!("disconnected ACP profile `{profile}`");
    Ok(())
}

pub async fn status(paths: &RuntimePaths) -> anyhow::Result<()> {
    let store = AcpRegistryStore::new(&paths.data_dir);
    let profiles = connected_profiles(paths)?;
    if profiles.is_empty() {
        println!("no ACP agents connected; run `codypendent acp connect claude-acp`");
        return Ok(());
    }
    store.load_or_refresh().await?;
    for (profile, agent) in profiles {
        // A profile may pin one of the agent's own models (`…#model`); the
        // launch is the same either way, so say which model the run will ask
        // for rather than showing two indistinguishable "ready" lines.
        let pin = agent_model_from_coordinate(&agent)
            .map(|model| format!(" · model {model}"))
            .unwrap_or_else(|| " · agent default model".to_string());
        match store.launch_spec(&agent) {
            Ok(spec) => println!(
                "ready  {:<28} {} {} ({}){pin}",
                profile,
                spec.name,
                spec.version,
                spec.command.display()
            ),
            Err(error) => println!("error  {profile:<28} {agent}: {error}"),
        }
    }
    Ok(())
}

async fn load_registry(store: &AcpRegistryStore, refresh: bool) -> anyhow::Result<AcpRegistry> {
    if refresh {
        return Ok(store.refresh().await?);
    }
    Ok(store.load_or_refresh().await?)
}

fn connected_profiles(paths: &RuntimePaths) -> anyhow::Result<Vec<(String, String)>> {
    let path = paths.data_dir.join("models.toml");
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(load_models(&path)?
        .into_iter()
        .filter(|config| config.provider == "acp")
        .map(|config| (config.id.0, config.model))
        .collect())
}

/// Replace one ACP profile family (base id plus its discovered `#model`
/// children) in `models.toml` in ONE
/// load-modify-write, so connecting an agent that advertises many models does
/// not rewrite the file once per model — and never leaves a half-written set
/// behind if one id turns out to be invalid. Reconnecting is an exact
/// replacement: children the agent no longer advertises are removed.
fn replace_profile_family(
    paths: &RuntimePaths,
    base: &str,
    profiles: &[(String, String)],
) -> anyhow::Result<()> {
    if base.is_empty() || base.len() > 256 || base.contains(['\0', '\n', '\r']) {
        bail!("invalid ACP profile id");
    }
    let family_prefix = format!("{base}#");
    for (profile, _) in profiles {
        if profile.is_empty()
            || profile.len() > 256
            || profile.contains(['\0', '\n', '\r'])
            || (profile != base && !profile.starts_with(&family_prefix))
        {
            bail!("invalid ACP profile id");
        }
    }
    std::fs::create_dir_all(&paths.data_dir)?;
    let path = paths.data_dir.join("models.toml");
    let mut configs = if path.exists() {
        load_models(&path)?
    } else {
        Vec::new()
    };
    if let Some(conflict) = configs.iter().find(|config| {
        config.provider != "acp" && profiles.iter().any(|(profile, _)| config.id.0 == *profile)
    }) {
        bail!(
            "ACP profile `{}` conflicts with an existing {} model",
            conflict.id.0,
            conflict.provider
        );
    }
    configs.retain(|config| {
        !(config.provider == "acp"
            && (config.id.0 == base || config.id.0.starts_with(&family_prefix)))
    });
    configs.extend(profiles.iter().map(|(profile, agent)| ModelConfig {
        id: ModelId(profile.clone()),
        provider: "acp".to_string(),
        base_url: String::new(),
        model: agent.clone(),
        api_key_env: String::new(),
        context_tokens: None,
        // An ACP agent is launched, not addressed over HTTP, so it has no
        // catalog provider whose auth header would need resolving.
        provider_id: None,
    }));
    write_models(&path, &configs)
}

fn write_models(path: &Path, configs: &[ModelConfig]) -> anyhow::Result<()> {
    // `models.toml` is a shared configuration document: voice,
    // transcription, embeddings, and retrieval live beside `[[model]]`.
    // Replacing the whole file from a model-only struct silently erased those
    // tables whenever an ACP client connected or disconnected.
    let mut document = if path.exists() {
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        toml::from_str::<toml::Value>(&raw).with_context(|| format!("parse {}", path.display()))?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let table = document
        .as_table_mut()
        .ok_or_else(|| anyhow!("models.toml root must be a table"))?;
    table.insert("model".to_string(), toml::Value::try_from(configs)?);
    let bytes = toml::to_string_pretty(&document)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("models path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".models-{}.tmp", std::process::id()));
    std::fs::write(&temp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&temp, path)?;
    Ok(())
}

fn distribution_label(
    agent: &codypendent_integrations::acp_registry::AcpRegistryAgent,
) -> &'static str {
    if agent.distribution.npx.is_some() {
        "npx"
    } else if agent.distribution.uvx.is_some() {
        "uvx"
    } else {
        "binary"
    }
}

fn agent_status(
    store: &AcpRegistryStore,
    agent: &codypendent_integrations::acp_registry::AcpRegistryAgent,
) -> String {
    if store.launch_spec(&agent.id).is_ok() {
        return format!("ready ({})", distribution_label(agent));
    }
    if let Some(binary) = agent
        .distribution
        .binary
        .get(codypendent_integrations::acp_registry::current_platform())
    {
        return if binary.sha256.is_some() {
            "install required (verified binary)".to_string()
        } else {
            "install requires --allow-unverified".to_string()
        };
    }
    format!("{} runner/platform unavailable", distribution_label(agent))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The single-profile shorthand the tests below drive `replace_profile_family`
    /// with (`connect` always writes a whole set at once).
    fn upsert_profile(paths: &RuntimePaths, profile: &str, agent: &str) -> anyhow::Result<()> {
        replace_profile_family(paths, profile, &[(profile.to_string(), agent.to_string())])
    }

    #[test]
    fn profile_upsert_is_typed_and_preserves_other_models() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        upsert_profile(&paths, "acp/codex", "codex-acp").expect("add profile");
        upsert_profile(&paths, "acp/vibe", "mistral-vibe").expect("add second");
        upsert_profile(&paths, "acp/codex", "codex-acp").expect("replace profile");
        let configs = load_models(&paths.data_dir.join("models.toml")).expect("load");
        assert_eq!(configs.len(), 2);
        assert!(configs.iter().all(|config| config.provider == "acp"));
        assert_eq!(
            configs
                .iter()
                .find(|config| config.id.0 == "acp/vibe")
                .map(|config| config.model.as_str()),
            Some("mistral-vibe")
        );
    }

    #[test]
    fn discovered_models_become_one_profile_each_beside_the_bare_agent_profile() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        // What `connect` persists after a handshake advertising two models.
        let base = "acp/demo".to_string();
        let profiles = vec![
            (base.clone(), agent_coordinate("demo-acp", "1.2.3")),
            (
                model_profile_id(&base, "agent-model-1"),
                agent_coordinate_with_model("demo-acp", "1.2.3", "agent-model-1"),
            ),
            (
                model_profile_id(&base, "agent-model-2"),
                agent_coordinate_with_model("demo-acp", "1.2.3", "agent-model-2"),
            ),
        ];
        replace_profile_family(&paths, &base, &profiles).expect("persist profiles");

        let configs = load_models(&paths.data_dir.join("models.toml")).expect("load");
        assert_eq!(configs.len(), 3);
        assert!(configs.iter().all(|config| config.provider == "acp"));
        // The bare profile leaves the agent on its own default (no pin).
        assert_eq!(
            configs
                .iter()
                .find(|config| config.id.0 == "acp/demo")
                .map(|config| config.model.as_str()),
            Some("demo-acp@1.2.3")
        );
        // Every per-model profile resolves to the SAME launchable agent, and
        // differs only in the model it pins.
        for config in &configs {
            assert_eq!(agent_id_from_coordinate(&config.model), "demo-acp");
        }
        assert_eq!(
            configs
                .iter()
                .find(|config| config.id.0 == "acp/demo#agent-model-2")
                .and_then(|config| agent_model_from_coordinate(&config.model)),
            Some("agent-model-2")
        );
    }

    #[test]
    fn reconnecting_replaces_the_previous_profile_set_and_spares_other_models() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        upsert_profile(&paths, "acp/other", "other-acp@9.9.9").expect("unrelated profile");
        replace_profile_family(
            &paths,
            "acp/demo",
            &[
                ("acp/demo".to_string(), "demo-acp@1.0.0".to_string()),
                (
                    "acp/demo#agent-model-1".to_string(),
                    "demo-acp@1.0.0#agent-model-1".to_string(),
                ),
                (
                    "acp/demo#removed-model".to_string(),
                    "demo-acp@1.0.0#removed-model".to_string(),
                ),
            ],
        )
        .expect("first connect");
        // A second connect at a newer version replaces the entire family,
        // including removing a model the agent stopped advertising.
        replace_profile_family(
            &paths,
            "acp/demo",
            &[
                ("acp/demo".to_string(), "demo-acp@2.0.0".to_string()),
                (
                    "acp/demo#agent-model-1".to_string(),
                    "demo-acp@2.0.0#agent-model-1".to_string(),
                ),
            ],
        )
        .expect("second connect");

        let configs = load_models(&paths.data_dir.join("models.toml")).expect("load");
        assert_eq!(configs.len(), 3, "no duplicate ids: {configs:?}");
        assert_eq!(
            configs
                .iter()
                .find(|config| config.id.0 == "acp/demo#agent-model-1")
                .map(|config| config.model.as_str()),
            Some("demo-acp@2.0.0#agent-model-1")
        );
        assert!(
            configs.iter().any(|config| config.id.0 == "acp/other"),
            "an unrelated ACP profile must survive"
        );
        assert!(
            configs
                .iter()
                .all(|config| config.id.0 != "acp/demo#removed-model"),
            "a stale discovered-model child must be removed"
        );
    }

    #[test]
    fn an_invalid_profile_id_writes_nothing_at_all() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        let error = replace_profile_family(
            &paths,
            "acp/demo",
            &[
                ("acp/demo".to_string(), "demo-acp@1.0.0".to_string()),
                ("bad\nid".to_string(), "demo-acp@1.0.0#x".to_string()),
            ],
        )
        .expect_err("rejects the invalid id");
        assert!(error.to_string().contains("invalid ACP profile id"));
        assert!(
            !paths.data_dir.join("models.toml").exists(),
            "a rejected set must leave no half-written file behind"
        );
    }

    #[test]
    fn acp_updates_preserve_non_model_configuration_tables() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        std::fs::create_dir_all(&paths.data_dir).expect("data dir");
        let path = paths.data_dir.join("models.toml");
        std::fs::write(
            &path,
            r#"
[voice]
enabled = true

[transcription]
model = "whisper"

[embedding]
model = "nomic"

[retrieval]
mcp_top_k = 7

[[model]]
id = "local/existing"
provider = "ollama"
base_url = "http://localhost:11434/v1"
model = "qwen"
api_key_env = ""
"#,
        )
        .expect("seed config");

        upsert_profile(&paths, "acp/demo", "demo-acp@1.0.0").expect("connect");
        let after_connect: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).expect("read after connect"))
                .expect("parse after connect");
        assert_eq!(after_connect["voice"]["enabled"].as_bool(), Some(true));
        assert_eq!(
            after_connect["transcription"]["model"].as_str(),
            Some("whisper")
        );
        assert_eq!(after_connect["embedding"]["model"].as_str(), Some("nomic"));
        assert_eq!(
            after_connect["retrieval"]["mcp_top_k"].as_integer(),
            Some(7)
        );

        disconnect(&paths, "acp/demo").expect("disconnect");
        let after_disconnect: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).expect("read after disconnect"))
                .expect("parse after disconnect");
        for table in ["voice", "transcription", "embedding", "retrieval"] {
            assert_eq!(
                after_disconnect.get(table),
                after_connect.get(table),
                "{table} must survive ACP disconnect"
            );
        }
    }

    #[test]
    fn disconnecting_a_base_profile_removes_all_discovered_children() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        replace_profile_family(
            &paths,
            "acp/demo",
            &[
                ("acp/demo".to_string(), "demo@1".to_string()),
                ("acp/demo#one".to_string(), "demo@1#one".to_string()),
                ("acp/demo#two".to_string(), "demo@1#two".to_string()),
            ],
        )
        .expect("connect family");
        upsert_profile(&paths, "acp/other", "other@1").expect("other family");

        disconnect(&paths, "acp/demo").expect("disconnect family");
        let configs = load_models(&paths.data_dir.join("models.toml")).expect("load");
        assert!(configs
            .iter()
            .all(|config| { config.id.0 != "acp/demo" && !config.id.0.starts_with("acp/demo#") }));
        assert!(configs.iter().any(|config| config.id.0 == "acp/other"));
    }

    #[test]
    fn an_auth_gated_handshake_failure_stays_actionable() {
        let advertised = AcpClientError::Handshake(
            "session/new failed: auth required (the agent advertises authentication: Agent login — Sign in first)"
                .to_string(),
        );
        let message = handshake_failure("demo-acp", &advertised).to_string();
        assert!(message.contains("Agent login"), "{message}");
        assert!(message.contains("agent's own CLI"), "{message}");

        // A non-auth failure gets no invented remedy.
        let plain = AcpClientError::Handshake("initialize failed: broken pipe".to_string());
        let message = handshake_failure("demo-acp", &plain).to_string();
        assert!(message.contains("broken pipe"), "{message}");
        assert!(!message.contains("own CLI"), "{message}");
    }

    #[tokio::test]
    async fn live_probe_collects_text_and_denies_tool_permissions() {
        let mut sink = ProbeSink::default();
        sink.on_event(EventBody::ModelStreamDelta {
            run_id: RunId::new(),
            text: "ACP LIVE OK".to_string(),
        })
        .await;
        assert_eq!(sink.output, "ACP LIVE OK");
        let choice = sink
            .on_permission(
                serde_json::json!({"title":"write"}),
                vec![
                    PermissionOption {
                        option_id: "allow".to_string(),
                        name: "Allow".to_string(),
                        kind: "allow_once".to_string(),
                    },
                    PermissionOption {
                        option_id: "reject".to_string(),
                        name: "Reject".to_string(),
                        kind: "reject_once".to_string(),
                    },
                ],
            )
            .await;
        assert_eq!(choice.as_deref(), Some("reject"));
    }
}
