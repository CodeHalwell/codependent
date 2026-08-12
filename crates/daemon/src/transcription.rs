//! Voice transcription seam (dependency inversion) — voice v1, rubric 8.
//!
//! The daemon owns the socket, the ledger, and the content-addressed artifact
//! store; it must NOT own an HTTP speech-to-text client (that lives in
//! `codypendent-runtime`, which depends on this crate — the same cycle
//! [`RunExecutor`](crate::executor::RunExecutor) inverts). So this module
//! declares *what* the daemon needs — a [`Transcriber`] that turns stored audio
//! bytes into text — and the `codypendentd` assembly provides the concrete
//! implementation and injects it into the [`server`](crate::server).
//!
//! The classification gate is enforced **here**, in the daemon, not in the
//! implementation: [`transcribe_envelope`] runs the protocol's existing
//! [`transcription_allowed`] math over the *stored artifact's own*
//! classification (captured media defaults to
//! [`DEFAULT_MEDIA_CLASSIFICATION`](codypendent_protocol::input::DEFAULT_MEDIA_CLASSIFICATION),
//! i.e. `Confidential`) against the ceiling the injected transcriber declares.
//! A remote transcriber under a restrictive ceiling therefore refuses *before*
//! any audio leaves the process — an implementation cannot opt out of the gate,
//! because it is never asked.
//!
//! The original-is-never-replaced invariant is upheld structurally: a produced
//! transcript is *added* to the [`AudioArtifact`] alongside its preserved
//! `original` ref, never substituted for it.

use std::future::Future;
use std::pin::Pin;

use codypendent_protocol::input::{
    transcription_allowed, AudioArtifact, InputBlock, InputEnvelope, OffDevicePolicy, Transcript,
    TranscriptionMode,
};
use codypendent_protocol::{ArtifactRef, CodypendentError, ModelId};
use sqlx::SqlitePool;

use crate::artifacts::ArtifactStore;

/// One stored audio artifact handed to a [`Transcriber`].
///
/// The bytes are read out of the content-addressed store by the daemon, so an
/// implementation never touches the filesystem and cannot widen its own read
/// scope. `audio` is the ref whose `sensitivity` the gate already cleared —
/// carried along so an implementation can attribute/log the occurrence, never
/// so it can re-decide the policy question.
#[derive(Debug, Clone)]
pub struct TranscriptionRequest {
    /// The stored occurrence's reference (classification, media type, length).
    pub audio: ArtifactRef,
    /// The audio bytes, already read from the artifact store.
    pub bytes: Vec<u8>,
    /// The mode the daemon's classification gate admitted this request under.
    pub mode: TranscriptionMode,
}

/// What a [`Transcriber`] produced from one [`TranscriptionRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Transcription {
    /// The recognized text.
    pub text: String,
    /// The transcription model, when a hosted/known one produced it.
    pub model: Option<ModelId>,
    /// The audio's duration as the provider reported it, when it did. Used only
    /// to describe the transcription in a note; the daemon prefers the duration
    /// the client measured at capture time.
    pub duration_ms: Option<u64>,
}

/// The future a [`Transcriber`] returns. Mirrors the
/// [`DocumentMutator`](crate::documents::DocumentMutator) seam's shape so the
/// daemon needs no `async-trait` dependency.
pub type TranscriptionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Transcription, CodypendentError>> + Send + 'a>>;

/// The daemon's seam for turning stored audio into text.
///
/// Implemented by the assembly binary over an OpenAI-compatible
/// `/audio/transcriptions` endpoint (`codypendent_runtime::models`); injected
/// into the server alongside the [`RunExecutor`](crate::executor::RunExecutor).
/// With none injected, a `SubmitUserInput` carrying un-transcribed audio is
/// rejected `voice.transport-unavailable` — the same fail-closed posture the
/// document/workflow seams take.
pub trait Transcriber: Send + Sync {
    /// Where this transcriber runs: [`TranscriptionMode::Local`] for an
    /// on-device engine, [`TranscriptionMode::Remote`] for a hosted endpoint.
    /// The daemon feeds this straight into [`transcription_allowed`], so a
    /// `Remote` transcriber is refused for media above the ceiling.
    fn mode(&self) -> TranscriptionMode;

    /// The operator's off-device ceiling (`routing.toml`'s `max_off_device` in
    /// the assembly). Read by the daemon's gate, never by the implementation.
    fn off_device_policy(&self) -> OffDevicePolicy;

    /// Transcribe one stored audio artifact. Errors are surfaced verbatim to the
    /// submitting client as a `CommandRejected`.
    fn transcribe(&self, request: TranscriptionRequest) -> TranscriptionFuture<'_>;
}

/// What [`transcribe_envelope`] transcribed, for the note the server surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscribedAudio {
    /// The transcript text, in block order.
    pub text: String,
    /// The model that produced it, when a known one did.
    pub model: Option<ModelId>,
    /// The audio's duration in milliseconds, when known.
    pub duration_ms: Option<u64>,
}

impl TranscribedAudio {
    /// The human-facing note the daemon appends to the session ledger, e.g.
    /// `transcribed 4.0 s of audio (model whisper-large-v3-turbo)`. Duration and
    /// model are each omitted when unknown rather than fabricated.
    #[must_use]
    pub fn note(&self) -> String {
        let duration = match self.duration_ms {
            Some(ms) => format!("{:.1} s of audio", ms as f64 / 1000.0),
            None => "audio".to_string(),
        };
        match &self.model {
            Some(model) => format!("transcribed {duration} (model {})", model.0),
            None => format!("transcribed {duration}"),
        }
    }
}

/// Transcribe every un-transcribed [`InputBlock::Audio`] in `envelope`, in
/// place, and return what was produced (`None` when the envelope carries no
/// audio needing transcription — the plain-text path, untouched).
///
/// Order of operations is the whole point:
///
/// 1. **Gate first.** [`transcription_allowed`] is evaluated against the
///    artifact's own `sensitivity` before a single byte is read, so audio a
///    policy forbids off-device never even reaches the transcriber.
/// 2. **Read from the store, not the client.** The bytes come from the
///    content-addressed store by [`ArtifactId`](codypendent_protocol::ArtifactId),
///    so what is transcribed is exactly what `PutArtifact` durably stored.
/// 3. **Add, never replace.** The produced [`Transcript`] is attached to the
///    block whose `original` ref stays exactly as it was, linked back by
///    `source_audio`.
///
/// A block that already carries a transcript (a client that transcribed locally
/// and let the user review it before submitting) is left completely alone — no
/// re-transcription, no gate, no network.
pub async fn transcribe_envelope(
    store: &ArtifactStore,
    pool: &SqlitePool,
    transcriber: &dyn Transcriber,
    envelope: &mut InputEnvelope,
) -> Result<Option<TranscribedAudio>, CodypendentError> {
    let mode = transcriber.mode();
    let policy = transcriber.off_device_policy();
    let mut texts: Vec<String> = Vec::new();
    let mut model: Option<ModelId> = None;
    let mut duration_ms: Option<u64> = None;

    for block in &mut envelope.blocks {
        let InputBlock::Audio(audio) = block else {
            continue;
        };
        if audio.transcript.is_some() {
            continue;
        }
        // (1) The classification gate, on the artifact's OWN classification.
        transcription_allowed(audio.original.sensitivity, mode, &policy).map_err(|error| {
            CodypendentError::new(
                "voice.off-device-forbidden",
                format!(
                    "audio may not be transcribed off-device: {error}; \
                     configure a local transcriber or raise the policy ceiling"
                ),
                false,
            )
        })?;

        // (2) The bytes come from the store, addressed by the ref the client cited.
        let bytes = read_artifact(store, pool, audio).await?;
        let produced = transcriber
            .transcribe(TranscriptionRequest {
                audio: audio.original.clone(),
                bytes,
                mode,
            })
            .await?;

        // (3) The transcript is ADDED; `original` is untouched and linked back.
        texts.push(produced.text.clone());
        model = model.or(produced.model.clone());
        // The client's own measurement wins: it timed the actual capture.
        duration_ms = duration_ms.or(audio.duration_ms).or(produced.duration_ms);
        audio.transcript = Some(Transcript {
            text: produced.text,
            mode,
            model: produced.model,
            // The daemon transcribed it; no human reviewed this text before
            // submission (a client that offers review sets this itself).
            reviewed: false,
            source_audio: audio.original.id,
        });
    }

    if texts.is_empty() {
        return Ok(None);
    }
    Ok(Some(TranscribedAudio {
        text: texts.join("\n"),
        model,
        duration_ms,
    }))
}

/// Read one stored audio blob out of the artifact store.
async fn read_artifact(
    store: &ArtifactStore,
    pool: &SqlitePool,
    audio: &AudioArtifact,
) -> Result<Vec<u8>, CodypendentError> {
    use tokio::io::AsyncReadExt as _;

    let mut file = store.open(pool, audio.original.id).await.map_err(|error| {
        CodypendentError::new(
            "voice.artifact-missing",
            format!(
                "audio artifact {} is not in this daemon's store: {error}",
                audio.original.id
            ),
            false,
        )
    })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await.map_err(|error| {
        CodypendentError::new(
            "voice.artifact-unreadable",
            format!(
                "could not read audio artifact {}: {error}",
                audio.original.id
            ),
            true,
        )
    })?;
    Ok(bytes)
}

/// The text a resolved envelope contributes as the run's objective: every
/// transcript and text block, in order. Used when the submitted `text` is empty
/// (the push-to-talk shape — the client has no text to send until the daemon
/// has transcribed).
#[must_use]
pub fn envelope_text(envelope: &InputEnvelope) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for block in &envelope.blocks {
        match block {
            InputBlock::Text { text } if !text.trim().is_empty() => parts.push(text),
            InputBlock::Audio(audio) => {
                if let Some(transcript) = &audio.transcript {
                    if !transcript.text.trim().is_empty() {
                        parts.push(&transcript.text);
                    }
                }
            }
            _ => {}
        }
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    use codypendent_protocol::input::{InputSource, ScopeLevel, DEFAULT_MEDIA_CLASSIFICATION};
    use codypendent_protocol::{ArtifactId, DataClassification};

    /// A transcriber that returns canned text — no audio hardware and no
    /// network are available in this environment, so every test here drives
    /// fixture bytes through a fake.
    struct FakeTranscriber {
        mode: TranscriptionMode,
        policy: OffDevicePolicy,
        text: String,
    }

    impl Transcriber for FakeTranscriber {
        fn mode(&self) -> TranscriptionMode {
            self.mode
        }

        fn off_device_policy(&self) -> OffDevicePolicy {
            self.policy
        }

        fn transcribe(&self, request: TranscriptionRequest) -> TranscriptionFuture<'_> {
            let text = self.text.clone();
            let byte_length = request.bytes.len();
            Box::pin(async move {
                assert!(byte_length > 0, "the daemon must read the stored bytes");
                Ok(Transcription {
                    text,
                    model: Some(ModelId("whisper-large-v3-turbo".to_string())),
                    duration_ms: None,
                })
            })
        }
    }

    fn audio_ref(sensitivity: DataClassification) -> ArtifactRef {
        ArtifactRef {
            id: ArtifactId::new(),
            media_type: "audio/wav".to_string(),
            byte_length: 44,
            sha256: "0".repeat(64),
            sensitivity,
        }
    }

    fn envelope_with(original: ArtifactRef) -> InputEnvelope {
        InputEnvelope {
            source: InputSource::Voice,
            blocks: vec![InputBlock::Audio(AudioArtifact {
                original,
                transcript: None,
                duration_ms: Some(4_000),
                sample_rate_hz: Some(16_000),
            })],
            scope: ScopeLevel::Session,
            attachments: Vec::new(),
        }
    }

    /// A minimal 44-byte RIFF/WAVE header with no samples — enough for the
    /// artifact store, which is byte-agnostic. NOT a real recording: this
    /// container has no audio hardware, so nothing here is device-tested.
    fn fixture_wav() -> Vec<u8> {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&36u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&16_000u32.to_le_bytes());
        wav.extend_from_slice(&32_000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&0u32.to_le_bytes());
        wav
    }

    async fn store_fixture(
        tmp: &tempfile::TempDir,
    ) -> (ArtifactStore, SqlitePool, codypendent_protocol::ArtifactRef) {
        let pool = crate::db::open_database(&tmp.path().join("db.sqlite"))
            .await
            .expect("open db");
        let store = ArtifactStore::new(tmp.path().join("artifacts"));
        let stored = store
            .put(
                &pool,
                "audio/wav",
                DEFAULT_MEDIA_CLASSIFICATION,
                crate::artifacts::Provenance::user_upload(),
                &fixture_wav(),
            )
            .await
            .expect("store fixture wav");
        (store, pool, stored)
    }

    #[tokio::test]
    async fn a_local_transcriber_transcribes_confidential_audio_and_keeps_the_original() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (store, pool, stored) = store_fixture(&tmp).await;
        let mut envelope = envelope_with(stored.clone());
        let transcriber = FakeTranscriber {
            // Local transcription is ALWAYS permitted, even under the most
            // restrictive ceiling — that is the whole point of the local path.
            mode: TranscriptionMode::Local,
            policy: OffDevicePolicy::restrictive(),
            text: "fix the flaky test".to_string(),
        };

        let produced = transcribe_envelope(&store, &pool, &transcriber, &mut envelope)
            .await
            .expect("local transcription is permitted")
            .expect("audio was transcribed");
        assert_eq!(produced.text, "fix the flaky test");
        assert_eq!(envelope_text(&envelope), "fix the flaky test");

        // The original is preserved and the transcript links back to it.
        let InputBlock::Audio(audio) = &envelope.blocks[0] else {
            panic!("expected the audio block to survive transcription");
        };
        assert_eq!(audio.original, stored, "the original ref is untouched");
        let transcript = audio.transcript.as_ref().expect("transcript attached");
        assert_eq!(transcript.source_audio, stored.id);
        assert_eq!(transcript.mode, TranscriptionMode::Local);
        assert!(
            !transcript.reviewed,
            "the daemon never claims a human reviewed its own transcript"
        );
    }

    #[tokio::test]
    async fn a_restrictive_ceiling_refuses_remote_transcription_before_reading_bytes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (store, pool, stored) = store_fixture(&tmp).await;
        let mut envelope = envelope_with(stored);
        let transcriber = FakeTranscriber {
            mode: TranscriptionMode::Remote,
            // Media defaults to Confidential; this ceiling allows only Internal.
            policy: OffDevicePolicy::restrictive(),
            text: "should never be produced".to_string(),
        };

        let error = transcribe_envelope(&store, &pool, &transcriber, &mut envelope)
            .await
            .expect_err("Confidential audio may not leave the device");
        assert_eq!(error.code, "voice.off-device-forbidden");
        assert!(!error.retryable, "a policy refusal is not retryable");
        let InputBlock::Audio(audio) = &envelope.blocks[0] else {
            panic!("expected an audio block");
        };
        assert!(
            audio.transcript.is_none(),
            "a refused envelope carries no transcript"
        );
    }

    #[tokio::test]
    async fn a_permissive_ceiling_admits_remote_transcription() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (store, pool, stored) = store_fixture(&tmp).await;
        let mut envelope = envelope_with(stored);
        let transcriber = FakeTranscriber {
            mode: TranscriptionMode::Remote,
            policy: OffDevicePolicy::permissive(),
            text: "ship it".to_string(),
        };

        let produced = transcribe_envelope(&store, &pool, &transcriber, &mut envelope)
            .await
            .expect("Confidential audio is within a permissive ceiling")
            .expect("audio was transcribed");
        assert_eq!(produced.text, "ship it");
        assert_eq!(
            produced.note(),
            "transcribed 4.0 s of audio (model whisper-large-v3-turbo)"
        );
    }

    #[tokio::test]
    async fn an_already_transcribed_block_is_left_alone() {
        // A client that transcribed locally and let the user REVIEW the text
        // must not have its reviewed transcript silently re-produced.
        let tmp = tempfile::tempdir().expect("tempdir");
        let (store, pool, stored) = store_fixture(&tmp).await;
        let reviewed = Transcript {
            text: "the reviewed text".to_string(),
            mode: TranscriptionMode::Local,
            model: None,
            reviewed: true,
            source_audio: stored.id,
        };
        let mut envelope = InputEnvelope {
            source: InputSource::Voice,
            blocks: vec![InputBlock::Audio(AudioArtifact {
                original: stored,
                transcript: Some(reviewed.clone()),
                duration_ms: Some(1_000),
                sample_rate_hz: Some(16_000),
            })],
            scope: ScopeLevel::Session,
            attachments: Vec::new(),
        };
        let transcriber = FakeTranscriber {
            // Remote + a ceiling that forbids it: proof the gate is not even
            // consulted for a block that needs no transcription.
            mode: TranscriptionMode::Remote,
            policy: OffDevicePolicy::restrictive(),
            text: "must not replace the reviewed text".to_string(),
        };

        let produced = transcribe_envelope(&store, &pool, &transcriber, &mut envelope)
            .await
            .expect("nothing to transcribe is not an error");
        assert!(produced.is_none(), "no fresh transcription happened");
        let InputBlock::Audio(audio) = &envelope.blocks[0] else {
            panic!("expected an audio block");
        };
        assert_eq!(audio.transcript.as_ref(), Some(&reviewed));
        assert_eq!(envelope_text(&envelope), "the reviewed text");
    }

    #[tokio::test]
    async fn audio_the_store_never_saw_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (store, pool, _) = store_fixture(&tmp).await;
        let mut envelope = envelope_with(audio_ref(DEFAULT_MEDIA_CLASSIFICATION));
        let transcriber = FakeTranscriber {
            mode: TranscriptionMode::Local,
            policy: OffDevicePolicy::restrictive(),
            text: "unreachable".to_string(),
        };

        let error = transcribe_envelope(&store, &pool, &transcriber, &mut envelope)
            .await
            .expect_err("an unknown artifact id cannot be transcribed");
        assert_eq!(error.code, "voice.artifact-missing");
    }

    #[test]
    fn a_note_omits_what_it_does_not_know() {
        let unknown = TranscribedAudio {
            text: "hi".to_string(),
            model: None,
            duration_ms: None,
        };
        assert_eq!(unknown.note(), "transcribed audio");
    }

    #[test]
    fn envelope_text_joins_text_and_transcript_blocks_in_order() {
        let original = audio_ref(DEFAULT_MEDIA_CLASSIFICATION);
        let envelope = InputEnvelope {
            source: InputSource::Voice,
            blocks: vec![
                InputBlock::Text {
                    text: "context:".to_string(),
                },
                InputBlock::Audio(AudioArtifact {
                    original: original.clone(),
                    transcript: Some(Transcript {
                        text: "the spoken part".to_string(),
                        mode: TranscriptionMode::Local,
                        model: None,
                        reviewed: false,
                        source_audio: original.id,
                    }),
                    duration_ms: None,
                    sample_rate_hz: None,
                }),
            ],
            scope: ScopeLevel::Session,
            attachments: Vec::new(),
        };
        assert_eq!(envelope_text(&envelope), "context:\nthe spoken part");
    }
}
