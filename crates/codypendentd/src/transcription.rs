//! The `codypendent-daemon` transcription seam, implemented (voice v1, rubric 8).
//!
//! The daemon declares [`Transcriber`] and enforces the classification gate; it
//! cannot own an HTTP speech-to-text client because `codypendent-runtime`
//! depends on it, not the other way round. This assembly crate depends on both,
//! so the implementation lives here — exactly like the document, workflow, and
//! promotion seams.
//!
//! Two configuration files decide behaviour, and neither is invented here:
//!
//! * `<data_dir>/models.toml`'s `[transcription]` table names the endpoint and
//!   model, and its `local` flag decides
//!   [`TranscriptionMode::Local`] vs [`TranscriptionMode::Remote`].
//! * `<data_dir>/routing.toml`'s `policy.max_off_device` is the off-device
//!   ceiling — the SAME ceiling that decides whether a hosted *chat* model may
//!   see classified data. Voice deliberately reuses it rather than inventing a
//!   second, divergent privacy knob.
//!
//! **This machine has no audio hardware and no provider credentials.** The
//! wiring here is exercised by the daemon's socket tests (through a fake
//! transcriber) and the runtime's wiremock tests (through a mock provider).
//! Nothing here has been run against a real speech provider.

use std::sync::Arc;

use codypendent_daemon::transcription::{
    Transcriber, Transcription, TranscriptionFuture, TranscriptionRequest,
};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::input::{OffDevicePolicy, TranscriptionMode};
use codypendent_protocol::{CodypendentError, ModelId};
use codypendent_runtime::models::{load_audio_models, AudioTranscriber};
use tracing::{info, warn};

use crate::routing::RoutingConfig;

/// Transcribes stored audio through an OpenAI-compatible
/// `/audio/transcriptions` endpoint (Groq, OpenAI, DeepInfra, Together, or a
/// local whisper server).
pub struct HostedTranscriber {
    client: AudioTranscriber,
    mode: TranscriptionMode,
    policy: OffDevicePolicy,
    model: ModelId,
}

impl HostedTranscriber {
    /// Build the seam from `<data_dir>/models.toml` + `<data_dir>/routing.toml`,
    /// or `None` when voice input is not configured.
    ///
    /// Returning `None` is the ordinary case (no `[transcription]` table), and
    /// it leaves the daemon rejecting audio submissions with
    /// `voice.transport-unavailable` — never silently degrading to some other
    /// engine. A models.toml that exists but does not parse is logged loudly and
    /// also yields `None`: a typo must not take the whole daemon down, and it
    /// must not enable voice either.
    #[must_use]
    pub fn from_paths(paths: &RuntimePaths) -> Option<Self> {
        let audio = match load_audio_models(&paths.data_dir.join("models.toml")) {
            Ok(audio) => audio,
            Err(error) => {
                warn!(%error, "could not read the [transcription] entry; voice input disabled");
                return None;
            }
        };
        let client = AudioTranscriber::new(&audio, load_auth(paths)).ok()?;
        let model = ModelId(client.config().model.clone());
        // A local engine is Local; anything else is assumed to leave the device.
        let mode = if client.is_local() {
            TranscriptionMode::Local
        } else {
            TranscriptionMode::Remote
        };
        // The off-device ceiling is the router's, not a voice-specific one: one
        // privacy posture governs both what a hosted chat model may see and what
        // a hosted transcriber may hear. With no `routing.toml`, this is the
        // built-in `balanced` policy's ceiling (`Confidential`) — which does
        // permit remote transcription of default-classified media. An operator
        // who wants voice to stay on-device sets a lower ceiling in
        // `routing.toml` (e.g. `Internal`) or marks the endpoint `local = true`.
        let policy = OffDevicePolicy {
            max_off_device: RoutingConfig::load(paths).policy.max_off_device,
        };
        info!(
            model = %model.0,
            local = client.is_local(),
            ceiling = ?policy.max_off_device,
            "voice input enabled (speech-to-text)"
        );
        Some(Self {
            client,
            mode,
            policy,
            model,
        })
    }

    /// Build the seam as a trait object ready for injection, or `None`.
    #[must_use]
    pub fn arc_from_paths(paths: &RuntimePaths) -> Option<Arc<dyn Transcriber>> {
        Self::from_paths(paths).map(|t| Arc::new(t) as Arc<dyn Transcriber>)
    }
}

/// Load `<data_dir>/auth.json` so a TUI-saved key resolves, exactly as the chat
/// model registry does. A corrupt store degrades to empty here (rather than
/// failing the daemon): the key then falls through to `api_key_env`, and a
/// genuinely missing key surfaces as a legible per-request error.
fn load_auth(paths: &RuntimePaths) -> codypendent_runtime::auth::AuthStore {
    codypendent_runtime::auth::AuthStore::load(&paths.data_dir).unwrap_or_default()
}

impl Transcriber for HostedTranscriber {
    fn mode(&self) -> TranscriptionMode {
        self.mode
    }

    fn off_device_policy(&self) -> OffDevicePolicy {
        self.policy
    }

    fn transcribe(&self, request: TranscriptionRequest) -> TranscriptionFuture<'_> {
        let model = self.model.clone();
        Box::pin(async move {
            // The stored media type drives the filename extension, which is how
            // most providers sniff the container.
            let extension = match request.audio.media_type.as_str() {
                "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
                "audio/mpeg" | "audio/mp3" => "mp3",
                "audio/ogg" | "audio/opus" => "ogg",
                "audio/webm" => "webm",
                "audio/flac" | "audio/x-flac" => "flac",
                _ => "wav",
            };
            let text = self
                .client
                .transcribe(
                    &request.bytes,
                    &format!("voice.{extension}"),
                    &request.audio.media_type,
                )
                .await
                .map_err(|error| {
                    CodypendentError::new("voice.transcription-failed", error.to_string(), true)
                })?;
            Ok(Transcription {
                text,
                model: Some(model),
                // The provider is not asked for a duration; the client's own
                // capture measurement is the honest number and the daemon
                // prefers it anyway.
                duration_ms: None,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_transcription_table_leaves_voice_input_unwired() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(tmp.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        std::fs::write(
            paths.data_dir.join("models.toml"),
            "[[model]]\nid = \"m\"\nprovider = \"openai-compatible\"\n\
             base_url = \"http://127.0.0.1:1\"\nmodel = \"x\"\n",
        )
        .expect("write models.toml");
        assert!(
            HostedTranscriber::arc_from_paths(&paths).is_none(),
            "voice stays unwired until an operator configures it"
        );
    }

    #[test]
    fn a_hosted_endpoint_is_remote_and_inherits_the_routers_ceiling() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(tmp.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        std::fs::write(
            paths.data_dir.join("models.toml"),
            "[transcription]\nbase_url = \"https://api.groq.com/openai/v1\"\n\
             model = \"whisper-large-v3-turbo\"\napi_key_env = \"GROQ_API_KEY\"\n",
        )
        .expect("write models.toml");
        // A routing.toml that lowers the ceiling to Internal must also lower it
        // for voice — one privacy posture, not two.
        std::fs::write(
            paths.data_dir.join("routing.toml"),
            "enabled = true\n\n[policy]\nname = \"tight\"\nversion = 1\n\
             quality_threshold = 0.7\nmax_off_device = { type = \"Internal\" }\n\n\
             [policy.lambdas]\ncost = 1.0\nlatency = 1.0\nprivacy = 1.0\nfailure = 1.0\n",
        )
        .expect("write routing.toml");

        let transcriber = HostedTranscriber::from_paths(&paths).expect("configured");
        assert_eq!(transcriber.mode(), TranscriptionMode::Remote);
        assert_eq!(
            transcriber.off_device_policy().max_off_device,
            codypendent_protocol::DataClassification::Internal,
        );
        // Under that ceiling, default-classified (Confidential) media is refused
        // — the daemon's gate does the refusing, but this proves the inputs it
        // reads are the operator's, not a voice-specific invention.
        assert!(codypendent_protocol::input::transcription_allowed(
            codypendent_protocol::input::DEFAULT_MEDIA_CLASSIFICATION,
            transcriber.mode(),
            &transcriber.off_device_policy(),
        )
        .is_err());
    }

    #[test]
    fn a_local_endpoint_is_local_under_any_ceiling() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(tmp.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        std::fs::write(
            paths.data_dir.join("models.toml"),
            "[transcription]\nbase_url = \"http://127.0.0.1:8080/v1\"\n\
             model = \"whisper-cpp\"\nlocal = true\n",
        )
        .expect("write models.toml");
        std::fs::write(
            paths.data_dir.join("routing.toml"),
            "enabled = true\n\n[policy]\nname = \"tight\"\nversion = 1\n\
             quality_threshold = 0.7\nmax_off_device = { type = \"Public\" }\n\n\
             [policy.lambdas]\ncost = 1.0\nlatency = 1.0\nprivacy = 1.0\nfailure = 1.0\n",
        )
        .expect("write routing.toml");

        let transcriber = HostedTranscriber::from_paths(&paths).expect("configured");
        assert_eq!(transcriber.mode(), TranscriptionMode::Local);
        assert!(
            codypendent_protocol::input::transcription_allowed(
                codypendent_protocol::input::DEFAULT_MEDIA_CLASSIFICATION,
                transcriber.mode(),
                &transcriber.off_device_policy(),
            )
            .is_ok(),
            "on-device transcription is permitted under even the tightest ceiling"
        );
    }

    #[test]
    fn a_malformed_models_toml_disables_voice_rather_than_killing_the_daemon() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::from_data_dir(tmp.path().to_path_buf());
        paths.ensure_directories().expect("directories");
        std::fs::write(
            paths.data_dir.join("models.toml"),
            "[transcription]\nbase_url =",
        )
        .expect("write models.toml");
        assert!(HostedTranscriber::arc_from_paths(&paths).is_none());
    }
}
