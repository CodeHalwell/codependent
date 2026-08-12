//! The audio (STT/TTS) clients against a mock OpenAI-compatible provider, and
//! the playback command against a fake player (voice v1, rubric 8).
//!
//! **This machine has no audio hardware, no microphone, and no provider
//! credentials.** Every test drives `wiremock` and fixture bytes; the "player"
//! is `sh -c 'cat > file'`. Passing tests here prove the request shapes, the
//! error paths, and that bytes reach a player's stdin — they are NOT evidence
//! that any real speech provider or audio device behaves as expected.

use codypendent_runtime::auth::AuthStore;
use codypendent_runtime::models::{
    load_audio_models, AudioError, AudioModels, AudioPlayer, AudioSynthesizer, AudioTranscriber,
};
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// A bare 44-byte RIFF/WAVE header. NOT a recording — nothing here was captured
/// from a device; the clients only care that bytes exist.
fn fixture_wav() -> Vec<u8> {
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&36u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&32_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&0u32.to_le_bytes());
    wav
}

fn models_toml(base_url: &str) -> String {
    format!(
        r#"
[[model]]
id = "hosted-default"
provider = "openai-compatible"
base_url = "{base_url}"
model = "gpt-5.1-codex"
api_key_env = ""

[transcription]
base_url = "{base_url}"
model = "whisper-large-v3-turbo"
api_key_env = "CODYPENDENT_TEST_STT_KEY"

[speech]
base_url = "{base_url}"
model = "gpt-4o-mini-tts"
voice = "alloy"
format = "mp3"
api_key_env = ""
"#
    )
}

fn write_models(dir: &tempfile::TempDir, body: &str) -> AudioModels {
    let path = dir.path().join("models.toml");
    std::fs::write(&path, body).expect("write models.toml");
    load_audio_models(&path).expect("parse audio tables")
}

/// An `auth.json` store with a key saved under the table name, mirroring how
/// the TUI's key flow saves one.
fn auth_with(table: &str, key: &str) -> AuthStore {
    let mut auth = AuthStore::default();
    auth.set(table, key);
    auth
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[test]
fn a_models_toml_without_audio_tables_parses_to_nothing_configured() {
    // The back-compatibility guarantee: an existing models.toml is unchanged
    // and simply has no voice.
    let dir = tempfile::tempdir().expect("tempdir");
    let models = write_models(
        &dir,
        r#"
[[model]]
id = "hosted-default"
provider = "openai-compatible"
base_url = "https://api.openai.com/v1"
model = "gpt-5.1-codex"
api_key_env = "OPENAI_API_KEY"
"#,
    );
    assert!(models.transcription.is_none());
    assert!(models.speech.is_none());

    match AudioTranscriber::new(&models, AuthStore::default()) {
        Err(AudioError::NotConfigured { table, .. }) => assert_eq!(table, "transcription"),
        other => panic!("expected NotConfigured, got {other:?}"),
    }
    match AudioSynthesizer::new(&models, AuthStore::default()) {
        Err(AudioError::NotConfigured { table, .. }) => assert_eq!(table, "speech"),
        other => panic!("expected NotConfigured, got {other:?}"),
    }
}

#[test]
fn an_absent_models_toml_is_not_an_error_but_a_malformed_one_is() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = load_audio_models(&dir.path().join("nope.toml")).expect("absent is fine");
    assert!(missing.transcription.is_none());

    let path = dir.path().join("models.toml");
    std::fs::write(&path, "[transcription]\nbase_url = ").expect("write");
    assert!(
        load_audio_models(&path).is_err(),
        "a typo in a voice table must surface, not silently disable voice"
    );
}

#[test]
fn an_endpoint_is_remote_unless_it_says_otherwise() {
    // The safe classification is the one you get by saying nothing: an unmarked
    // endpoint is assumed to leave the device, so the daemon's gate treats it
    // as Remote and the operator's ceiling governs it.
    let dir = tempfile::tempdir().expect("tempdir");
    let models = write_models(&dir, &models_toml("http://127.0.0.1:1"));
    let transcriber = AudioTranscriber::new(&models, AuthStore::default()).expect("configured");
    assert!(!transcriber.is_local());

    let local = write_models(
        &dir,
        r#"
[transcription]
base_url = "http://127.0.0.1:8080/v1"
model = "whisper-cpp"
local = true
"#,
    );
    let transcriber = AudioTranscriber::new(&local, AuthStore::default()).expect("configured");
    assert!(transcriber.is_local());
    assert_eq!(transcriber.config().timeout_secs, 120, "generous default");
}

// ---------------------------------------------------------------------------
// Speech-to-text
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transcription_posts_multipart_and_returns_the_text() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .and(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "text": "fix the flaky test"
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let models = write_models(&dir, &models_toml(&server.uri()));
    let transcriber =
        AudioTranscriber::new(&models, auth_with("transcription", "sk-test")).expect("configured");

    let text = transcriber
        .transcribe(&fixture_wav(), "voice.wav", "audio/wav")
        .await
        .expect("transcribe");
    assert_eq!(text, "fix the flaky test");

    // Inspect the request the provider actually received.
    let requests = server.received_requests().await.expect("recorded requests");
    let request: &Request = &requests[0];
    let content_type = request
        .headers
        .get("content-type")
        .expect("content-type")
        .to_str()
        .expect("ascii");
    assert!(
        content_type.starts_with("multipart/form-data; boundary="),
        "STT is a multipart upload, got {content_type}"
    );
    let boundary = content_type
        .split("boundary=")
        .nth(1)
        .expect("boundary")
        .to_string();
    let body = String::from_utf8_lossy(&request.body);
    assert!(
        body.contains(r#"name="file"; filename="voice.wav""#),
        "the audio rides in a `file` part named by the client"
    );
    assert!(
        body.contains("name=\"model\"") && body.contains("whisper-large-v3-turbo"),
        "the configured provider-side model is sent"
    );
    assert!(
        body.ends_with(&format!("--{boundary}--\r\n")),
        "the body is terminated by the closing boundary"
    );
    assert!(
        !body[..body.len() - boundary.len() - 6].contains(&format!("--{boundary}--")),
        "the closing boundary appears exactly once"
    );
    // The audio bytes survive verbatim inside the part.
    assert!(
        request.body.windows(4).any(|window| window == b"RIFF"),
        "the fixture's own bytes are in the upload"
    );
    // The saved auth.json key wins over the (unset) api_key_env.
    assert_eq!(
        request
            .headers
            .get("authorization")
            .expect("authorization")
            .to_str()
            .expect("ascii"),
        "Bearer sk-test"
    );
}

#[tokio::test]
async fn a_provider_error_status_is_surfaced_with_its_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("{\"error\":\"bad key\"}"))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let models = write_models(&dir, &models_toml(&server.uri()));
    let transcriber =
        AudioTranscriber::new(&models, auth_with("transcription", "sk-bad")).expect("configured");

    match transcriber
        .transcribe(&fixture_wav(), "voice.wav", "audio/wav")
        .await
    {
        Err(AudioError::Status { status, body, .. }) => {
            assert_eq!(status, 401);
            assert!(body.contains("bad key"), "the provider's reason survives");
        }
        other => panic!("expected a status error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_response_without_text_is_a_malformed_response_not_an_empty_transcript() {
    // Silently returning "" would submit an EMPTY run objective; the caller
    // must be able to tell "the provider misbehaved" from "you said nothing".
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"oops": 1})))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let models = write_models(&dir, &models_toml(&server.uri()));
    let transcriber =
        AudioTranscriber::new(&models, auth_with("transcription", "sk-test")).expect("configured");

    assert!(matches!(
        transcriber
            .transcribe(&fixture_wav(), "voice.wav", "audio/wav")
            .await,
        Err(AudioError::MalformedResponse { .. })
    ));
}

#[tokio::test]
async fn an_unset_api_key_env_is_reported_by_name_never_by_value() {
    let dir = tempfile::tempdir().expect("tempdir");
    let models = write_models(&dir, &models_toml("http://127.0.0.1:1"));
    // No auth.json entry, and `CODYPENDENT_TEST_STT_KEY` is not set.
    let transcriber = AudioTranscriber::new(&models, AuthStore::default()).expect("configured");
    match transcriber
        .transcribe(&fixture_wav(), "voice.wav", "audio/wav")
        .await
    {
        Err(AudioError::MissingApiKeyEnv { table, var }) => {
            assert_eq!(table, "transcription");
            assert_eq!(var, "CODYPENDENT_TEST_STT_KEY");
        }
        other => panic!("expected MissingApiKeyEnv, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Text-to-speech
// ---------------------------------------------------------------------------

#[tokio::test]
async fn speech_posts_the_model_voice_and_format_and_returns_audio_bytes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/speech"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(b"ID3-fake-mp3".to_vec())
                .insert_header("content-type", "audio/mpeg"),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let models = write_models(&dir, &models_toml(&server.uri()));
    let synthesizer = AudioSynthesizer::new(&models, AuthStore::default()).expect("configured");

    let spoken = synthesizer.synthesize("all tests pass").await.expect("tts");
    assert_eq!(spoken.bytes, b"ID3-fake-mp3");
    assert_eq!(spoken.media_type, "audio/mpeg");

    let requests = server.received_requests().await.expect("recorded requests");
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("json request body");
    assert_eq!(body["model"], "gpt-4o-mini-tts");
    assert_eq!(body["input"], "all tests pass");
    assert_eq!(body["voice"], "alloy");
    assert_eq!(body["response_format"], "mp3");
    // `api_key_env = ""` means no key is needed, so no header is invented.
    assert!(
        requests[0].headers.get("authorization").is_none(),
        "a keyless endpoint gets no Authorization header"
    );
}

#[tokio::test]
async fn an_empty_speech_response_is_an_error_not_a_silent_clip() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/speech"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(Vec::new()))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let models = write_models(&dir, &models_toml(&server.uri()));
    let synthesizer = AudioSynthesizer::new(&models, AuthStore::default()).expect("configured");

    assert!(matches!(
        synthesizer.synthesize("hello").await,
        Err(AudioError::MalformedResponse { .. })
    ));
}

// ---------------------------------------------------------------------------
// Playback
// ---------------------------------------------------------------------------

#[tokio::test]
async fn playback_pipes_the_clip_to_the_configured_commands_stdin() {
    // The fake "player" is `sh -c 'cat > out.bin'` — no audio device is
    // involved (this container has none). What is proven is that the exact
    // synthesized bytes reach the command's stdin and that the call returns
    // without waiting for the player to exit.
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("played.bin");
    let player = AudioPlayer::new(vec![
        "sh".to_string(),
        "-c".to_string(),
        format!("cat > {}", out.display()),
    ]);
    assert!(player.is_configured());

    let clip = b"ID3-fake-mp3-payload".to_vec();
    player.play(&clip).await.expect("playback starts");

    // The player is detached, so poll briefly for it to finish writing.
    for _ in 0..100 {
        if std::fs::read(&out).map(|got| got == clip).unwrap_or(false) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!(
        "the player never received the clip; got {:?}",
        std::fs::read(&out).ok()
    );
}

#[tokio::test]
async fn playback_without_a_configured_command_fails_with_actionable_guidance() {
    let player = AudioPlayer::default();
    assert!(!player.is_configured());
    let error = player
        .play(b"clip")
        .await
        .expect_err("nothing to play with");
    assert!(matches!(error, AudioError::NoPlayer));
    assert!(
        error.to_string().contains("play_command"),
        "the error names the setting to fix: {error}"
    );
}

#[tokio::test]
async fn a_missing_player_binary_is_reported_not_silently_dropped() {
    let player = AudioPlayer::new(vec!["codypendent-no-such-player".to_string()]);
    match player.play(b"clip").await {
        Err(AudioError::Playback { command, .. }) => {
            assert_eq!(command, vec!["codypendent-no-such-player".to_string()]);
        }
        other => panic!("expected a playback error, got {other:?}"),
    }
}
