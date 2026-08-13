//! The one writer of `models.toml`.
//!
//! `models.toml` is a **shared configuration document**, not a model list.
//! `codypendent-runtime` parses five tables out of it: `[[model]]`,
//! `[embedding]`, `[retrieval]`, `[transcription]` and `[speech]`. Four
//! separate code paths add or remove a model — `models add`, `models pull`, the
//! TUI's add-model flow, and ACP client connect/disconnect — and each one grew
//! its own serializer.
//!
//! Three of the four independently rediscovered the same bug and fixed it in
//! place, leaving a comment behind:
//!
//! * `models_pull.rs` — "would silently delete every one of them, disabling a
//!   user's voice and retrieval setup"
//! * `acp_clients.rs` — "silently erased those tables" (past tense: it shipped)
//! * `tui.rs` — "must survive adding a model from the TUI"
//!
//! The fourth, `models add`, did not, and destroyed a user's embedding,
//! retrieval, STT and TTS configuration on every invocation while printing
//! `added model …`.
//!
//! Patching the fourth site would have left four copies of one invariant and a
//! fifth writer free to reintroduce it. So the invariant lives here instead:
//! **edit the parsed document in place; never serialize the file from a struct
//! that models only one section.** Adding a table to `models.toml` requires no
//! change to this module — an unknown table is carried through untouched.

use std::path::Path;

use anyhow::{anyhow, Context};
use codypendent_runtime::models::ModelConfig;

/// Replace the `[[model]]` array in `path` with `configs`, preserving every
/// other table, and install it atomically at mode 0600.
///
/// A missing file is created. A file that is not a TOML table is an error
/// rather than something to overwrite — that is a user's file with content this
/// code does not understand.
pub fn write_model_entries(path: &Path, configs: &[ModelConfig]) -> anyhow::Result<()> {
    let mut document = if path.exists() {
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str::<toml::Value>(&raw)
            .with_context(|| format!("parsing {}", path.display()))?
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
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    // Pid-unique so two concurrent writers never share a temp file.
    let temp = parent.join(format!(".models-{}.tmp", std::process::id()));
    std::fs::write(&temp, rendered.as_bytes())
        .with_context(|| format!("writing {}", temp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // An entry names the environment variable holding a key, never the key
        // itself, but the endpoint list is still the user's business alone.
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("securing {}", temp.display()))?;
    }
    std::fs::rename(&temp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> ModelConfig {
        ModelConfig {
            id: codypendent_protocol::ModelId(id.to_string()),
            provider: "openai-compatible".to_string(),
            base_url: "https://api.example.com/v1".to_string(),
            model: id.to_string(),
            api_key_env: "EXAMPLE_KEY".to_string(),
            provider_id: Some("example".to_string()),
            context_tokens: Some(128_000),
        }
    }

    /// The bug this module exists to make unrepresentable: `models add` rebuilt
    /// the file from a model-only struct, so adding a model silently turned off
    /// the user's embeddings, retrieval tuning, speech-to-text and
    /// text-to-speech.
    #[test]
    fn adding_a_model_preserves_every_other_table() {
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
model = "nomic-embed-text"

[retrieval]
mcp_top_k = 12
builtin_top_k = 0

[transcription]
base_url = "http://localhost:9000/v1"
model = "whisper-1"

[speech]
base_url = "http://localhost:9000/v1"
model = "tts-1"
voice = "alloy"

[a-table-a-later-version-adds]
setting = true
"#,
        )
        .expect("seed the file");

        write_model_entries(&path, &[entry("existing/one"), entry("openai/gpt-4o")])
            .expect("write");

        let reparsed: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap())
            .expect("the result is valid TOML");
        let root = reparsed.as_table().expect("a table");
        for table in [
            "embedding",
            "retrieval",
            "transcription",
            "speech",
            "a-table-a-later-version-adds",
        ] {
            assert!(root.contains_key(table), "[{table}] must survive the write");
        }
        // The real `RetrievalSettings` keys, by name: both are `#[serde(default)]`,
        // so a second writer that round-tripped the struct would silently reset
        // `builtin_top_k = 0` (retrieval gating deliberately OFF) back to the
        // default and quietly change what tools the model is shown.
        assert_eq!(root["retrieval"]["mcp_top_k"].as_integer(), Some(12));
        assert_eq!(root["retrieval"]["builtin_top_k"].as_integer(), Some(0));
        assert_eq!(root["speech"]["voice"].as_str(), Some("alloy"));
        assert_eq!(
            root["model"].as_array().map(Vec::len),
            Some(2),
            "both models are present"
        );
    }

    #[test]
    fn a_missing_file_is_created_with_just_the_models() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("models.toml");
        write_model_entries(&path, &[entry("openai/gpt-4o")]).expect("write");
        let models = codypendent_runtime::models::load_models(&path).expect("load");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id.0, "openai/gpt-4o");
    }
}
