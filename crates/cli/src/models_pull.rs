//! `codypendent models pull <hf-repo>[:<quant>]` — resolve an Unsloth GGUF
//! repo + quant against the Hugging Face Hub, drive `ollama pull
//! hf.co/<org>/<repo>:<quant>` with streamed progress, then register the
//! result in `models.toml` against the `ollama` provider using the EXACT
//! reference Ollama itself uses for it (`hf.co/<org>/<repo>:<quant>` — what
//! `ollama list` shows, and what the OpenAI-compatible `model` field must
//! match at call time).
//!
//! Honesty note (this build container has no GPU and no `ollama` binary):
//! every subprocess interaction here is a `Command` spawn that degrades to an
//! actionable [`PullError::BinaryNotFound`] when `ollama` is missing from
//! `PATH`. The unit tests below drive [`pull_via_ollama`] against a fake
//! `ollama` shell script (success, a non-zero exit, and the missing-binary
//! case) — nothing here claims device-tested behavior.

use std::process::Stdio;

use anyhow::Context;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

use codypendent_integrations::unsloth::{pick_default_quant, HfCatalogApi};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::ModelId;
use codypendent_runtime::models::{load_models, ModelConfig};

/// The org `codypendent models pull <repo>` assumes when `repo` carries no
/// explicit `org/` prefix.
pub const DEFAULT_ORG: &str = codypendent_integrations::unsloth::DEFAULT_UNSLOTH_ORG;

/// The catalog provider id every pulled model registers against.
const OLLAMA_PROVIDER_ID: &str = "ollama";

/// A parsed `codypendent models pull` target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullSpec {
    /// `org/repo`, e.g. `unsloth/Qwen3-32B-GGUF`.
    pub repo_id: String,
    /// The quant tag, when the caller named one (`repo:QUANT`). `None` means
    /// "resolve one automatically" — see [`pick_default_quant`].
    pub quant: Option<String>,
}

/// Parse `<hf-repo>[:<quant>]`, defaulting a bare repo name (no `/`) to
/// `default_org/<repo>`. The quant, when present, is everything after the
/// LAST `:` (a GGUF quant tag never contains a colon); a trailing bare colon
/// with nothing after it is treated the same as omitting the quant entirely.
#[must_use]
pub fn parse_pull_spec(spec: &str, default_org: &str) -> PullSpec {
    let trimmed = spec.trim();
    let (name, quant) = match trimmed.rsplit_once(':') {
        Some((name, quant)) if !quant.trim().is_empty() => (name, Some(quant.trim().to_string())),
        Some((name, _blank)) => (name, None),
        None => (trimmed, None),
    };
    let repo_id = if name.contains('/') {
        name.to_string()
    } else {
        format!("{default_org}/{name}")
    };
    PullSpec { repo_id, quant }
}

/// The exact model reference Ollama uses for an `hf.co` pull — what `ollama
/// list` shows, and what the OpenAI-compatible `model` field must match.
/// Registered as both the `models.toml` id and the provider-side model name.
#[must_use]
pub fn ollama_hf_reference(repo_id: &str, quant: &str) -> String {
    format!("hf.co/{repo_id}:{quant}")
}

/// Errors from driving `ollama pull` and registering its result.
#[derive(Debug, thiserror::Error)]
pub enum PullError {
    /// `ollama` is not on `PATH`. Actionable, never a bare "not found".
    #[error("`ollama` was not found on PATH — install it from https://ollama.com, then retry")]
    BinaryNotFound,
    /// The binary exists but could not be spawned (permissions, exec format, …).
    #[error("failed to launch `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    /// The pull process exited non-zero. `tail` is the last few captured
    /// output lines (diagnostics only — never fabricated).
    #[error("`ollama pull {reference}` exited with {status}{tail}")]
    ExitStatus {
        reference: String,
        status: String,
        tail: String,
    },
}

/// Drive `<ollama_bin> pull hf.co/<repo_id>:<quant>`, forwarding every parsed
/// progress line to `progress` as it arrives, and returning once the process
/// exits. Splits on both `\n` and `\r` — Ollama redraws its single-line
/// progress bar with a bare `\r`, so splitting on `\n` alone would coalesce
/// an entire download into one unusably long "line". A non-zero exit is
/// [`PullError::ExitStatus`], carrying the tail of captured output.
pub async fn pull_via_ollama(
    ollama_bin: &str,
    repo_id: &str,
    quant: &str,
    progress: UnboundedSender<String>,
) -> Result<(), PullError> {
    let reference = ollama_hf_reference(repo_id, quant);
    let mut command = Command::new(ollama_bin);
    command
        .arg("pull")
        .arg(&reference)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(PullError::BinaryNotFound);
        }
        Err(source) => {
            return Err(PullError::Spawn {
                program: ollama_bin.to_string(),
                source,
            });
        }
    };

    // Both streams are drained concurrently: `ollama pull` may write
    // meaningful text to either, and an undrained pipe can block the child
    // once its OS buffer fills.
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let out_task = tokio::spawn(stream_progress_lines(stdout, progress.clone()));
    let err_task = tokio::spawn(stream_progress_lines(stderr, progress.clone()));
    let (out_tail, err_tail) = tokio::join!(out_task, err_task);

    let status = child.wait().await.map_err(|source| PullError::Spawn {
        program: ollama_bin.to_string(),
        source,
    })?;
    if status.success() {
        return Ok(());
    }

    let mut tail: Vec<String> = out_tail.unwrap_or_default();
    tail.extend(err_tail.unwrap_or_default());
    let tail_text = tail_snippet(&tail);
    Err(PullError::ExitStatus {
        reference,
        status: status.to_string(),
        tail: if tail_text.is_empty() {
            String::new()
        } else {
            format!(" — {tail_text}")
        },
    })
}

/// The last few captured lines, joined for a one-line diagnostic suffix.
fn tail_snippet(lines: &[String]) -> String {
    const MAX_TAIL_LINES: usize = 5;
    let start = lines.len().saturating_sub(MAX_TAIL_LINES);
    lines[start..].join(" | ")
}

/// Read `reader` in chunks, splitting on `\n` or `\r`, sending each non-empty
/// trimmed segment to `progress` as it completes and returning the (bounded)
/// tail of lines seen — for [`pull_via_ollama`]'s failure diagnostic.
async fn stream_progress_lines(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    progress: UnboundedSender<String>,
) -> Vec<String> {
    const MAX_TAIL: usize = 20;
    let mut buffer: Vec<u8> = Vec::new();
    let mut tail: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &byte in &chunk[..read] {
            if byte == b'\n' || byte == b'\r' {
                emit_line(&mut buffer, &progress, &mut tail, MAX_TAIL);
            } else {
                buffer.push(byte);
            }
        }
    }
    emit_line(&mut buffer, &progress, &mut tail, MAX_TAIL);
    tail.into_iter().collect()
}

fn emit_line(
    buffer: &mut Vec<u8>,
    progress: &UnboundedSender<String>,
    tail: &mut std::collections::VecDeque<String>,
    max_tail: usize,
) {
    if buffer.is_empty() {
        return;
    }
    let line = String::from_utf8_lossy(buffer).trim().to_string();
    buffer.clear();
    if line.is_empty() {
        return;
    }
    // An unbounded channel send only fails if every receiver was dropped
    // (e.g. the TUI overlay was dismissed mid-pull) — the pull keeps running
    // either way, so a dropped line here is not itself an error.
    let _ = progress.send(line.clone());
    if tail.len() >= max_tail {
        tail.pop_front();
    }
    tail.push_back(line);
}

/// Register a pulled model in `<data_dir>/models.toml` against the `ollama`
/// provider, using [`ollama_hf_reference`] as both the display id and the
/// provider-side model name; `context_tokens` is carried through when the
/// Hub reported one. Update-in-place: re-pulling the same `repo:quant`
/// replaces its prior entry rather than duplicating it.
///
/// Deliberately an independent, self-contained implementation rather than a
/// shared call into `crates/cli/src/tui.rs`'s `write_add_model` (the same
/// atomic load-modify-write shape): that function is a sibling vertical's
/// add-model flow, out of scope to modify here.
pub fn register_pulled_model(
    paths: &RuntimePaths,
    repo_id: &str,
    quant: &str,
    context_tokens: Option<u64>,
) -> anyhow::Result<String> {
    let reference = ollama_hf_reference(repo_id, quant);
    let base_url = ollama_base_url(paths)?;

    let data_dir = &paths.data_dir;
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("creating the data dir {}", data_dir.display()))?;
    let models_path = data_dir.join("models.toml");
    let mut configs = if models_path.exists() {
        load_models(&models_path).with_context(|| format!("reading {}", models_path.display()))?
    } else {
        Vec::new()
    };
    configs.retain(|c| c.id.0 != reference);
    configs.push(ModelConfig {
        id: ModelId(reference.clone()),
        provider: "openai-compatible".to_string(),
        base_url,
        model: reference.clone(),
        api_key_env: String::new(),
        context_tokens,
        // A pulled GGUF is served by the local `ollama` provider, whose auth is
        // the catalog's `none` — no header resolution is needed.
        provider_id: Some("ollama".to_string()),
    });

    #[derive(serde::Serialize)]
    struct ModelsToml {
        #[serde(rename = "model")]
        model: Vec<ModelConfig>,
    }
    let rendered = toml::to_string_pretty(&ModelsToml { model: configs })
        .context("serializing models.toml")?;
    let tmp = data_dir.join("models.toml.tmp");
    std::fs::write(&tmp, rendered.as_bytes())
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &models_path)
        .with_context(|| format!("replacing {}", models_path.display()))?;
    Ok(reference)
}

/// The `ollama` catalog provider's `base_url` (built-ins layered with any
/// user `providers.toml` override) — never hardcoded here, so a user who
/// repoints `ollama` in `providers.toml` (e.g. a remote host) is honored.
fn ollama_base_url(paths: &RuntimePaths) -> anyhow::Result<String> {
    let catalog = codypendent_providers::Catalog::load_with_user_overrides(
        &paths.data_dir.join("providers.toml"),
    )
    .unwrap_or_else(|_| codypendent_providers::Catalog::builtin());
    let provider = catalog.get(OLLAMA_PROVIDER_ID).ok_or_else(|| {
        anyhow::anyhow!("the `{OLLAMA_PROVIDER_ID}` provider is not in the catalog")
    })?;
    provider
        .base_url
        .clone()
        .ok_or_else(|| anyhow::anyhow!("the `{OLLAMA_PROVIDER_ID}` provider has no base_url"))
}

/// The literal program name production callers resolve on `PATH`.
pub const OLLAMA_BIN: &str = "ollama";

/// `codypendent models pull <hf-repo>[:<quant>]`: resolve the repo + quant
/// against the Hub, drive the pull with progress printed to stdout, register
/// the result, and suggest `models bench`. `hf` is injected so tests supply a
/// fixture-backed stub instead of hitting the network; `ollama_bin` is
/// injected (production callers pass [`OLLAMA_BIN`]) so tests point it at a
/// fake script by absolute path instead of mutating the process `PATH`.
pub async fn run(
    paths: &RuntimePaths,
    spec: &str,
    hf: &dyn HfCatalogApi,
    ollama_bin: &str,
) -> anyhow::Result<()> {
    let target = parse_pull_spec(spec, DEFAULT_ORG);
    let quant = resolve_quant(hf, &target).await?;
    println!(
        "codypendent models pull: resolved {} : {quant}",
        target.repo_id
    );

    // Best-effort: a repo with no parsed `gguf` metadata (or a metadata call
    // that fails) registers with `context_tokens: None` rather than aborting
    // the pull over a display-only field.
    let context_tokens = hf
        .repo_metadata(&target.repo_id)
        .await
        .ok()
        .and_then(|m| m.context_length);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let repo_id = target.repo_id.clone();
    let quant_for_pull = quant.clone();
    let ollama_bin = ollama_bin.to_string();
    let pull_task =
        tokio::spawn(
            async move { pull_via_ollama(&ollama_bin, &repo_id, &quant_for_pull, tx).await },
        );
    while let Some(line) = rx.recv().await {
        println!("  {line}");
    }
    pull_task.await.context("ollama pull task panicked")??;

    let registered_id = register_pulled_model(paths, &target.repo_id, &quant, context_tokens)?;
    println!(
        "codypendent models pull: registered `{registered_id}` against the ollama provider in {}",
        paths.data_dir.join("models.toml").display()
    );
    println!("next: `codypendent models bench {registered_id}` to measure it for routing");
    Ok(())
}

/// Resolve the quant to pull: the caller's explicit `:quant`, or an
/// auto-picked default (see [`pick_default_quant`]) — erroring with the full
/// list of available quants when the choice is ambiguous, rather than
/// guessing.
async fn resolve_quant(hf: &dyn HfCatalogApi, target: &PullSpec) -> anyhow::Result<String> {
    if let Some(quant) = &target.quant {
        return Ok(quant.clone());
    }
    let variants = hf
        .list_quant_variants(&target.repo_id)
        .await
        .with_context(|| format!("listing quant variants for {}", target.repo_id))?;
    if let Some(picked) = pick_default_quant(&variants) {
        return Ok(picked.quant.clone());
    }
    let available: Vec<&str> = variants.iter().map(|v| v.quant.as_str()).collect();
    if available.is_empty() {
        anyhow::bail!(
            "{} has no GGUF quant files — nothing to pull",
            target.repo_id
        );
    }
    anyhow::bail!(
        "{} offers multiple quants and none is the default Q4_K_M — pass one explicitly, e.g. \
         `codypendent models pull {}:{}`. Available: {}",
        target.repo_id,
        target.repo_id,
        available[0],
        available.join(", ")
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use codypendent_integrations::unsloth::{HfError, HfRepoMetadata, HfRepoSummary, QuantVariant};
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ------------------------------------------------------------------
    // parse_pull_spec
    // ------------------------------------------------------------------

    #[test]
    fn a_bare_repo_name_gets_the_default_org_prefixed() {
        let target = parse_pull_spec("Qwen3-32B-GGUF", "unsloth");
        assert_eq!(target.repo_id, "unsloth/Qwen3-32B-GGUF");
        assert_eq!(target.quant, None);
    }

    #[test]
    fn an_explicit_org_repo_is_kept_verbatim() {
        let target = parse_pull_spec("someone-else/Some-Model-GGUF", "unsloth");
        assert_eq!(target.repo_id, "someone-else/Some-Model-GGUF");
    }

    #[test]
    fn a_trailing_quant_suffix_is_split_off() {
        let target = parse_pull_spec("Qwen3-32B-GGUF:UD-Q4_K_XL", "unsloth");
        assert_eq!(target.repo_id, "unsloth/Qwen3-32B-GGUF");
        assert_eq!(target.quant.as_deref(), Some("UD-Q4_K_XL"));
    }

    #[test]
    fn an_explicit_org_and_quant_both_resolve() {
        let target = parse_pull_spec("unsloth/Qwen3-32B-GGUF:Q4_K_M", "unsloth");
        assert_eq!(target.repo_id, "unsloth/Qwen3-32B-GGUF");
        assert_eq!(target.quant.as_deref(), Some("Q4_K_M"));
    }

    #[test]
    fn a_bare_trailing_colon_is_treated_as_no_quant() {
        let target = parse_pull_spec("Qwen3-32B-GGUF:", "unsloth");
        assert_eq!(target.repo_id, "unsloth/Qwen3-32B-GGUF");
        assert_eq!(target.quant, None);
    }

    #[test]
    fn ollama_hf_reference_matches_the_documented_shape() {
        assert_eq!(
            ollama_hf_reference("unsloth/Qwen3-32B-GGUF", "UD-Q4_K_XL"),
            "hf.co/unsloth/Qwen3-32B-GGUF:UD-Q4_K_XL"
        );
    }

    // ------------------------------------------------------------------
    // pull_via_ollama (fake binary — no real ollama in this container)
    // ------------------------------------------------------------------

    fn fake_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write fake script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake script");
        }
        path
    }

    #[tokio::test]
    async fn pull_via_ollama_streams_progress_and_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = fake_script(
            dir.path(),
            "ollama",
            "#!/bin/sh\necho 'pulling manifest'\necho 'verifying sha256 digest'\necho 'success'\nexit 0\n",
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let result = pull_via_ollama(
            script.to_str().expect("utf8 path"),
            "unsloth/Qwen3-32B-GGUF",
            "Q4_K_M",
            tx,
        )
        .await;
        assert!(result.is_ok(), "expected success, got {result:?}");

        let mut lines = Vec::new();
        while let Ok(line) = rx.try_recv() {
            lines.push(line);
        }
        assert!(lines.iter().any(|l| l.contains("pulling manifest")));
        assert!(lines.iter().any(|l| l.contains("verifying sha256 digest")));
        assert!(lines.iter().any(|l| l.contains("success")));
    }

    #[tokio::test]
    async fn pull_via_ollama_splits_carriage_return_progress_into_separate_lines() {
        // Ollama redraws its single-line progress bar with bare `\r`, not `\n`.
        let dir = tempfile::tempdir().expect("tempdir");
        let script = fake_script(
            dir.path(),
            "ollama",
            "#!/bin/sh\nprintf 'pulling 10%%\\rpulling 55%%\\rpulling 100%%\\n'\nexit 0\n",
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        pull_via_ollama(
            script.to_str().expect("utf8 path"),
            "unsloth/Qwen3-32B-GGUF",
            "Q4_K_M",
            tx,
        )
        .await
        .expect("success");
        let mut lines = Vec::new();
        while let Ok(line) = rx.try_recv() {
            lines.push(line);
        }
        assert_eq!(
            lines,
            vec!["pulling 10%", "pulling 55%", "pulling 100%"],
            "each \\r-delimited segment must become its own progress line"
        );
    }

    #[tokio::test]
    async fn pull_via_ollama_reports_a_nonzero_exit_with_a_diagnostic_tail() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = fake_script(
            dir.path(),
            "ollama",
            "#!/bin/sh\necho 'pulling manifest'\n>&2 echo 'Error: model not found'\nexit 1\n",
        );
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let error = pull_via_ollama(
            script.to_str().expect("utf8 path"),
            "unsloth/does-not-exist",
            "Q4_K_M",
            tx,
        )
        .await
        .expect_err("a nonzero exit must be an error");
        match error {
            PullError::ExitStatus {
                reference, tail, ..
            } => {
                assert_eq!(reference, "hf.co/unsloth/does-not-exist:Q4_K_M");
                assert!(
                    tail.contains("model not found"),
                    "tail must carry the real diagnostic text, got: {tail}"
                );
            }
            other => panic!("expected ExitStatus, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pull_via_ollama_reports_an_actionable_error_when_the_binary_is_missing() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let error = pull_via_ollama(
            "codypendent-test-definitely-not-a-real-binary",
            "unsloth/Qwen3-32B-GGUF",
            "Q4_K_M",
            tx,
        )
        .await
        .expect_err("a missing binary must be a clear error");
        assert!(matches!(error, PullError::BinaryNotFound));
        assert!(error.to_string().contains("ollama.com"));
        assert!(!error.to_string().to_lowercase().contains("gpu"));
    }

    // ------------------------------------------------------------------
    // register_pulled_model
    // ------------------------------------------------------------------

    fn test_paths(root: &std::path::Path) -> RuntimePaths {
        RuntimePaths {
            data_dir: root.join("data"),
            config_dir: root.join("config"),
            run_dir: root.join("run"),
            socket_path: root.join("run/codypendent.sock"),
            pid_path: root.join("run/codypendent.pid"),
            log_dir: root.join("log"),
        }
    }

    #[test]
    fn register_pulled_model_writes_a_models_toml_entry_against_ollama() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = test_paths(dir.path());
        let id =
            register_pulled_model(&paths, "unsloth/Qwen3-32B-GGUF", "UD-Q4_K_XL", Some(40_960))
                .expect("register succeeds");
        assert_eq!(id, "hf.co/unsloth/Qwen3-32B-GGUF:UD-Q4_K_XL");

        let configs = load_models(&paths.data_dir.join("models.toml")).expect("parse models.toml");
        assert_eq!(configs.len(), 1);
        let entry = &configs[0];
        assert_eq!(entry.id.0, "hf.co/unsloth/Qwen3-32B-GGUF:UD-Q4_K_XL");
        assert_eq!(entry.model, "hf.co/unsloth/Qwen3-32B-GGUF:UD-Q4_K_XL");
        assert_eq!(entry.provider, "openai-compatible");
        assert_eq!(entry.base_url, "http://localhost:11434/v1");
        assert_eq!(entry.api_key_env, "");
        assert_eq!(entry.context_tokens, Some(40_960));
    }

    #[test]
    fn register_pulled_model_omits_context_tokens_when_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = test_paths(dir.path());
        register_pulled_model(&paths, "unsloth/Qwen3-32B-GGUF", "Q8_0", None)
            .expect("register succeeds");
        let configs = load_models(&paths.data_dir.join("models.toml")).expect("parse models.toml");
        assert_eq!(configs[0].context_tokens, None);
    }

    #[test]
    fn register_pulled_model_updates_in_place_on_a_repeat_pull() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = test_paths(dir.path());
        register_pulled_model(&paths, "unsloth/Qwen3-32B-GGUF", "Q4_K_M", Some(1)).unwrap();
        register_pulled_model(&paths, "unsloth/Qwen3-32B-GGUF", "Q4_K_M", Some(2)).unwrap();
        let configs = load_models(&paths.data_dir.join("models.toml")).expect("parse models.toml");
        assert_eq!(configs.len(), 1, "a repeat pull replaces, never duplicates");
        assert_eq!(configs[0].context_tokens, Some(2));
    }

    #[test]
    fn register_pulled_model_preserves_other_existing_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = test_paths(dir.path());
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        std::fs::write(
            paths.data_dir.join("models.toml"),
            r#"
[[model]]
id = "hosted-default"
provider = "openai-compatible"
base_url = "https://api.openai.com/v1"
model = "gpt-5.1-codex"
api_key_env = "OPENAI_API_KEY"
"#,
        )
        .unwrap();
        register_pulled_model(&paths, "unsloth/Qwen3-32B-GGUF", "Q4_K_M", None).unwrap();
        let configs = load_models(&paths.data_dir.join("models.toml")).expect("parse models.toml");
        assert_eq!(configs.len(), 2);
        assert!(configs.iter().any(|c| c.id.0 == "hosted-default"));
        assert!(configs
            .iter()
            .any(|c| c.id.0 == "hf.co/unsloth/Qwen3-32B-GGUF:Q4_K_M"));
    }

    // ------------------------------------------------------------------
    // run() end-to-end over a stub HfCatalogApi + fake ollama binary
    // ------------------------------------------------------------------

    /// A fixture-backed [`HfCatalogApi`] stub — no network, ever.
    struct StubHf {
        quants: Vec<QuantVariant>,
        context_length: Option<u64>,
        quant_calls: AtomicUsize,
    }

    #[async_trait]
    impl HfCatalogApi for StubHf {
        async fn list_gguf_repos(
            &self,
            _author: &str,
            _limit: u32,
        ) -> Result<Vec<HfRepoSummary>, HfError> {
            Ok(Vec::new())
        }

        async fn list_quant_variants(&self, _repo_id: &str) -> Result<Vec<QuantVariant>, HfError> {
            self.quant_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.quants.clone())
        }

        async fn repo_metadata(&self, _repo_id: &str) -> Result<HfRepoMetadata, HfError> {
            Ok(HfRepoMetadata {
                context_length: self.context_length,
            })
        }
    }

    fn quant(label: &str, size: u64) -> QuantVariant {
        QuantVariant {
            quant: label.to_string(),
            files: vec![codypendent_integrations::unsloth::GgufFile {
                path: format!("Repo-{label}.gguf"),
                size_bytes: size,
            }],
            total_size_bytes: size,
        }
    }

    #[tokio::test]
    async fn run_resolves_registers_and_suggests_bench_end_to_end() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = fake_script(
            dir.path(),
            "ollama",
            "#!/bin/sh\necho 'pulling manifest'\necho success\nexit 0\n",
        );
        let paths = test_paths(dir.path());
        let hf = StubHf {
            quants: vec![quant("Q4_K_M", 123), quant("UD-Q4_K_XL", 456)],
            context_length: Some(40_960),
            quant_calls: AtomicUsize::new(0),
        };

        run(
            &paths,
            "unsloth/Qwen3-32B-GGUF",
            &hf,
            script.to_str().expect("utf8 path"),
        )
        .await
        .expect("run succeeds");

        let configs = load_models(&paths.data_dir.join("models.toml")).expect("parse models.toml");
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].id.0, "hf.co/unsloth/Qwen3-32B-GGUF:Q4_K_M");
        assert_eq!(configs[0].context_tokens, Some(40_960));
        // No `:quant` was given, so the default-quant resolution path (and
        // therefore `list_quant_variants`) had to run exactly once.
        assert_eq!(hf.quant_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn run_with_an_explicit_quant_never_calls_list_quant_variants() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = fake_script(dir.path(), "ollama", "#!/bin/sh\necho ok\nexit 0\n");
        let paths = test_paths(dir.path());
        let hf = StubHf {
            quants: vec![quant("Q4_K_M", 123)],
            context_length: None,
            quant_calls: AtomicUsize::new(0),
        };
        run(
            &paths,
            "unsloth/Qwen3-32B-GGUF:UD-Q4_K_XL",
            &hf,
            script.to_str().expect("utf8 path"),
        )
        .await
        .expect("run succeeds");
        let configs = load_models(&paths.data_dir.join("models.toml")).expect("parse models.toml");
        assert_eq!(configs[0].id.0, "hf.co/unsloth/Qwen3-32B-GGUF:UD-Q4_K_XL");
        assert_eq!(
            hf.quant_calls.load(Ordering::SeqCst),
            0,
            "an explicit quant must skip discovery entirely"
        );
    }

    #[tokio::test]
    async fn run_fails_closed_on_an_ambiguous_quant_without_pulling_anything() {
        // A binary name guaranteed not to exist: if `run` tried to pull
        // despite the ambiguity, it would fail with BinaryNotFound instead of
        // this error, and the assertion on the message would catch the mix-up.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = test_paths(dir.path());
        let hf = StubHf {
            quants: vec![quant("UD-IQ1_S", 1), quant("Q8_0", 2)],
            context_length: None,
            quant_calls: AtomicUsize::new(0),
        };
        let error = run(
            &paths,
            "unsloth/Qwen3-32B-GGUF",
            &hf,
            "codypendent-test-definitely-not-a-real-binary",
        )
        .await
        .expect_err("an ambiguous quant must not silently guess");
        let message = error.to_string();
        assert!(message.contains("UD-IQ1_S"));
        assert!(message.contains("Q8_0"));
        assert!(!paths.data_dir.join("models.toml").exists());
    }
}
