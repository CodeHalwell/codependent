//! Provider VCR cassettes for deterministic integration testing (Adoption 12 A3).
//!
//! Provides [`RecordingDriver`] to capture real model interactions into serializable
//! cassettes, and [`CassetteDriver`] to deterministically replay them without network I/O.
//! Includes secret sanitization before writing cassettes to disk.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use codypendent_protocol::ModelId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::agent::{DeltaSink, ModelDriver, StepOutcome, ToolDefinition, TurnItem};

/// Errors produced during VCR playback.
#[derive(Debug, Error)]
pub enum VcrError {
    #[error("VCR request mismatch for model '{model}':\nExpected fingerprint:\n  {expected}\nActual fingerprint:\n  {actual}\n\nTo re-record this cassette, run with CODYPENDENT_VCR=record")]
    Mismatch {
        model: String,
        expected: String,
        actual: String,
    },
    #[error("Cassette exhausted: expected interaction index {index}, but cassette only contains {total} interactions")]
    Exhausted { index: usize, total: usize },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// A recorded interaction containing the request fingerprint, summary, streamed chunks, and final outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Interaction {
    pub request_fingerprint: String,
    pub request_summary: String,
    pub streamed_chunks: Vec<String>,
    pub outcome: StepOutcome,
}

/// A serialized cassette containing recorded interactions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cassette {
    pub version: u32,
    pub model_id: ModelId,
    pub interactions: Vec<Interaction>,
}

impl Cassette {
    /// Creates a new empty cassette for the given model.
    #[must_use]
    pub fn new(model_id: ModelId) -> Self {
        Self {
            version: 1,
            model_id,
            interactions: Vec::new(),
        }
    }

    /// Loads a cassette from a JSON file.
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self, VcrError> {
        let content = fs::read_to_string(path)?;
        let cassette: Self = serde_json::from_str(&content)?;
        Ok(cassette)
    }

    /// Saves the cassette to a JSON file after applying secret sanitization.
    pub fn save_to_file(&mut self, path: impl AsRef<Path>) -> Result<(), VcrError> {
        self.sanitize();
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        fs::write(path, content)?;
        Ok(())
    }

    /// Sanitizes API keys and sensitive tokens in place before persistence.
    pub fn sanitize(&mut self) {
        for interaction in &mut self.interactions {
            interaction.request_summary = sanitize_string(&interaction.request_summary);
            for chunk in &mut interaction.streamed_chunks {
                *chunk = sanitize_string(chunk);
            }
            // The final `StepOutcome` can echo a model-returned secret (e.g. a key
            // reflected in assistant text or a tool argument), which would persist
            // to the cassette unredacted. Apply the SAME string redaction to every
            // string within its serialized form, walking the JSON so structure is
            // always preserved.
            if let Ok(mut value) = serde_json::to_value(&interaction.outcome) {
                sanitize_json_value(&mut value);
                if let Ok(cleaned) = serde_json::from_value(value) {
                    interaction.outcome = cleaned;
                }
            }
        }
    }
}

/// Recursively redact secrets in every string within a JSON value, leaving the
/// structure (objects, arrays, non-string scalars) untouched.
fn sanitize_json_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => *s = sanitize_string(s),
        serde_json::Value::Array(items) => items.iter_mut().for_each(sanitize_json_value),
        serde_json::Value::Object(map) => map.values_mut().for_each(sanitize_json_value),
        _ => {}
    }
}

fn sanitize_string(s: &str) -> String {
    // Basic regex-free pattern sanitizer for common API tokens
    let mut out = s.to_string();
    for prefix in ["sk-", "Bearer ", "anthropic-key-", "ghp_"] {
        while let Some(start) = out.find(prefix) {
            let rest = &out[start + prefix.len()..];
            let end_offset = rest
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
                .unwrap_or(rest.len());
            let secret_end = start + prefix.len() + end_offset;
            out.replace_range(start..secret_end, "[REDACTED]");
        }
    }
    out
}

/// Computes a stable SHA-256 fingerprint for a driver request.
#[must_use]
pub fn compute_fingerprint(
    model_id: &ModelId,
    transcript: &[TurnItem],
    tools: &[ToolDefinition],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(model_id.0.as_bytes());
    if let Ok(transcript_json) = serde_json::to_vec(transcript) {
        hasher.update(&transcript_json);
    }
    for tool in tools {
        hasher.update(tool.name.as_bytes());
        hasher.update(tool.description.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Computes a human-readable summary of the request for debug diffs.
#[must_use]
pub fn compute_summary(transcript: &[TurnItem], tools: &[ToolDefinition]) -> String {
    format!(
        "transcript_turns={}, tools=[{}]",
        transcript.len(),
        tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Sink adapter that captures chunks in addition to forwarding to the inner sink.
struct RecordingSink<'a> {
    inner: &'a mut dyn DeltaSink,
    chunks: Vec<String>,
}

impl<'a> RecordingSink<'a> {
    fn new(inner: &'a mut dyn DeltaSink) -> Self {
        Self {
            inner,
            chunks: Vec::new(),
        }
    }
}

impl<'a> DeltaSink for RecordingSink<'a> {
    fn on_text(&mut self, chunk: &str) {
        self.chunks.push(chunk.to_string());
        self.inner.on_text(chunk);
    }
}

/// A `ModelDriver` that wraps an inner driver and records all requests and responses into a cassette.
pub struct RecordingDriver<D: ModelDriver> {
    inner: D,
    cassette: Arc<Mutex<Cassette>>,
}

impl<D: ModelDriver> RecordingDriver<D> {
    /// Wraps an inner driver with recording capabilities.
    pub fn new(inner: D, cassette: Arc<Mutex<Cassette>>) -> Self {
        Self { inner, cassette }
    }
}

#[async_trait]
impl<D: ModelDriver> ModelDriver for RecordingDriver<D> {
    fn model_id(&self) -> ModelId {
        self.inner.model_id()
    }

    async fn next_step(
        &self,
        transcript: &[TurnItem],
        tools: &[ToolDefinition],
        sink: &mut dyn DeltaSink,
    ) -> anyhow::Result<StepOutcome> {
        let fingerprint = compute_fingerprint(&self.model_id(), transcript, tools);
        let summary = compute_summary(transcript, tools);

        let mut recording_sink = RecordingSink::new(sink);
        let outcome = self
            .inner
            .next_step(transcript, tools, &mut recording_sink)
            .await?;

        if let Ok(mut cassette) = self.cassette.lock() {
            cassette.interactions.push(Interaction {
                request_fingerprint: fingerprint,
                request_summary: summary,
                streamed_chunks: recording_sink.chunks,
                outcome: outcome.clone(),
            });
        }

        Ok(outcome)
    }

    fn context_window(&self) -> Option<u64> {
        self.inner.context_window()
    }

    fn endpoint(&self) -> Option<String> {
        self.inner.endpoint()
    }
}

/// A `ModelDriver` that replays recorded interactions from a `Cassette`.
pub struct CassetteDriver {
    cassette: Cassette,
    cursor: AtomicUsize,
}

impl CassetteDriver {
    /// Creates a new `CassetteDriver` loaded with the given cassette.
    #[must_use]
    pub fn new(cassette: Cassette) -> Self {
        Self {
            cassette,
            cursor: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl ModelDriver for CassetteDriver {
    fn model_id(&self) -> ModelId {
        self.cassette.model_id.clone()
    }

    async fn next_step(
        &self,
        transcript: &[TurnItem],
        tools: &[ToolDefinition],
        sink: &mut dyn DeltaSink,
    ) -> anyhow::Result<StepOutcome> {
        let index = self.cursor.fetch_add(1, Ordering::SeqCst);
        let Some(interaction) = self.cassette.interactions.get(index) else {
            return Err(VcrError::Exhausted {
                index,
                total: self.cassette.interactions.len(),
            }
            .into());
        };

        let actual_fingerprint = compute_fingerprint(&self.model_id(), transcript, tools);
        if actual_fingerprint != interaction.request_fingerprint {
            let actual_summary = compute_summary(transcript, tools);
            return Err(VcrError::Mismatch {
                model: self.model_id().0,
                expected: format!(
                    "{} ({})",
                    interaction.request_fingerprint, interaction.request_summary
                ),
                actual: format!("{actual_fingerprint} ({actual_summary})"),
            }
            .into());
        }

        for chunk in &interaction.streamed_chunks {
            sink.on_text(chunk);
        }

        Ok(interaction.outcome.clone())
    }

    fn context_window(&self) -> Option<u64> {
        Some(200_000)
    }

    fn endpoint(&self) -> Option<String> {
        Some("https://vcr.playback.local/v1".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{ModelStep, ScriptedDriver};

    struct TestSink(Vec<String>);
    impl DeltaSink for TestSink {
        fn on_text(&mut self, text: &str) {
            self.0.push(text.to_string());
        }
    }

    #[tokio::test]
    async fn record_and_playback_interaction() {
        let model_id = ModelId("gpt-4o-test".to_string());
        let cassette_arc = Arc::new(Mutex::new(Cassette::new(model_id.clone())));

        // 1. Record an interaction
        let scripted = ScriptedDriver::new(vec![ModelStep::Say("Hello, VCR!".into())])
            .with_model(model_id.clone());
        let recording_driver = RecordingDriver::new(scripted, Arc::clone(&cassette_arc));

        let mut sink = TestSink(Vec::new());
        let outcome = recording_driver
            .next_step(&[], &[], &mut sink)
            .await
            .expect("record next step");
        assert_eq!(outcome.step, ModelStep::Say("Hello, VCR!".into()));

        // 2. Extract cassette and replay
        let cassette = cassette_arc.lock().unwrap().clone();
        assert_eq!(cassette.interactions.len(), 1);

        let playback_driver = CassetteDriver::new(cassette);
        let mut play_sink = TestSink(Vec::new());
        let play_outcome = playback_driver
            .next_step(&[], &[], &mut play_sink)
            .await
            .expect("playback next step");
        assert_eq!(play_outcome.step, ModelStep::Say("Hello, VCR!".into()));
    }

    #[test]
    fn sanitizes_sensitive_api_keys() {
        let mut cassette = Cassette::new(ModelId("test-model".to_string()));
        cassette.interactions.push(Interaction {
            request_fingerprint: "abc".to_string(),
            request_summary: "request with sk-1234567890abcdef and Bearer secret_jwt_token"
                .to_string(),
            streamed_chunks: vec!["anthropic-key-secret123 data".to_string()],
            outcome: StepOutcome::unmeasured(ModelStep::Say("done".into())),
        });

        cassette.sanitize();
        assert!(!cassette.interactions[0]
            .request_summary
            .contains("sk-1234567890abcdef"));
        assert!(!cassette.interactions[0]
            .request_summary
            .contains("secret_jwt_token"));
        assert!(!cassette.interactions[0].streamed_chunks[0].contains("anthropic-key-secret123"));
        assert!(cassette.interactions[0]
            .request_summary
            .contains("[REDACTED]"));
    }

    #[test]
    fn sanitizes_secret_echoed_in_step_outcome() {
        // A model that echoes a key back in its assistant text lands the secret in
        // the recorded `StepOutcome`; sanitize must redact it there too, not only
        // in the request summary / streamed chunks.
        let mut cassette = Cassette::new(ModelId("test-model".to_string()));
        cassette.interactions.push(Interaction {
            request_fingerprint: "abc".to_string(),
            request_summary: "clean".to_string(),
            streamed_chunks: vec!["clean".to_string()],
            outcome: StepOutcome::unmeasured(ModelStep::Say(
                "your key is sk-leaked1234567890 keep it safe".into(),
            )),
        });

        cassette.sanitize();

        let serialized =
            serde_json::to_string(&cassette.interactions[0].outcome).expect("serialize outcome");
        assert!(
            !serialized.contains("sk-leaked1234567890"),
            "secret leaked into serialized StepOutcome: {serialized}"
        );
        assert!(serialized.contains("[REDACTED]"));
    }
}
