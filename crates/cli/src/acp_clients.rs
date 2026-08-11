//! Manage external ACP agents from the official registry.

use std::path::Path;

use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use codypendent_integrations::acp::PermissionOption;
use codypendent_integrations::acp_client::{AcpClient, AcpEventSink};
use codypendent_integrations::acp_registry::{
    agent_coordinate, agent_id_from_coordinate, local_kimi_code_spec, AcpRegistry, AcpRegistryStore,
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
    .with_context(|| format!("ACP handshake with `{agent}` failed"))?;
    drop(client);

    let profile = profile
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("acp/{}", spec.registry_id));
    upsert_profile(
        paths,
        &profile,
        &agent_coordinate(&spec.registry_id, &spec.version),
    )?;
    println!(
        "connected {} {} as model `{}`\nOpen /model in the TUI or run with this model pin.",
        spec.name, spec.version, profile
    );
    Ok(())
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
    .with_context(|| format!("ACP handshake with `{agent}` failed"))?;
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
    configs.retain(|config| !(config.id.0 == profile && config.provider == "acp"));
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
        match store.launch_spec(&agent) {
            Ok(spec) => println!(
                "ready  {:<28} {} {} ({})",
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

fn upsert_profile(paths: &RuntimePaths, profile: &str, agent: &str) -> anyhow::Result<()> {
    if profile.is_empty() || profile.len() > 256 || profile.contains(['\0', '\n', '\r']) {
        bail!("invalid ACP profile id");
    }
    std::fs::create_dir_all(&paths.data_dir)?;
    let path = paths.data_dir.join("models.toml");
    let mut configs = if path.exists() {
        load_models(&path)?
    } else {
        Vec::new()
    };
    configs.retain(|config| config.id.0 != profile);
    configs.push(ModelConfig {
        id: ModelId(profile.to_string()),
        provider: "acp".to_string(),
        base_url: String::new(),
        model: agent.to_string(),
        api_key_env: String::new(),
        context_tokens: None,
    });
    write_models(&path, &configs)
}

fn write_models(path: &Path, configs: &[ModelConfig]) -> anyhow::Result<()> {
    #[derive(serde::Serialize)]
    struct ModelsToml<'a> {
        #[serde(rename = "model")]
        model: &'a [ModelConfig],
    }
    let bytes = toml::to_string_pretty(&ModelsToml { model: configs })?;
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

    #[test]
    fn profile_upsert_is_typed_and_preserves_other_models() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(dir.path().to_path_buf());
        upsert_profile(&paths, "acp/codex", "codex-acp").expect("add profile");
        upsert_profile(&paths, "acp/claude", "claude-acp").expect("add second");
        upsert_profile(&paths, "acp/codex", "codex-acp").expect("replace profile");
        let configs = load_models(&paths.data_dir.join("models.toml")).expect("load");
        assert_eq!(configs.len(), 2);
        assert!(configs.iter().all(|config| config.provider == "acp"));
        assert_eq!(
            configs
                .iter()
                .find(|config| config.id.0 == "acp/claude")
                .map(|config| config.model.as_str()),
            Some("claude-acp")
        );
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
