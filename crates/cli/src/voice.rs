//! The TUI's voice host (voice v1, rubric 8): push-to-talk capture on the way
//! in, spoken replies on the way out.
//!
//! Everything device-facing lives here rather than in `codypendent-tui`, for
//! the same reason the daemon does not own an HTTP client: the TUI crate is a
//! pure render/reduce unit that must stay free of subprocesses, audio, and
//! network. It only learns two booleans (`state.voice`), which drive the
//! status-line indicator and the palette toggle.
//!
//! ## Capture
//!
//! There is **no bundled recorder**. Codypendent shells out to a recorder the
//! user already has, because bundling an audio backend would mean a
//! platform-native dependency for a strictly optional feature. At startup the
//! host probes `$PATH` **once** (the result is cached for the process) for
//! `rec` (sox), `arecord` (ALSA), then `ffmpeg`, and an explicit
//! `record_command` in `[voice]` always wins. With none of them present, the
//! push-to-talk key produces an actionable error naming what to install —
//! never a silent no-op.
//!
//! Stopping sends **SIGINT**, not SIGKILL: `rec`/`arecord`/`ffmpeg` all
//! finalize the WAV header on an interrupt, and a killed recorder would leave a
//! header claiming zero samples.
//!
//! ## Speech
//!
//! A finalized assistant turn is detected from the event stream the client
//! already receives — `ModelStreamDelta` text accumulated per run, flushed when
//! that run reaches a terminal state — so nothing in the reducer or the chat
//! rendering has to change. Synthesis and playback run on their own task with a
//! **queue depth of one**: while a clip is being produced, a newer finished turn
//! REPLACES the queued one rather than piling up, so speech tracks the
//! conversation instead of falling behind it.
//!
//! ## Privacy (outbound)
//!
//! Capture is gated on the way IN: the daemon re-derives the stored artifact's
//! classification and runs [`transcription_allowed`] before a byte is read
//! (`codypendent_daemon::transcription::transcribe_envelope`). Until this fix,
//! a finished assistant turn had **no gate on the way OUT** — every reply,
//! source code included, was POSTed to the hosted `[speech]` endpoint
//! regardless of `routing.toml` (2026-08-13 review, F8). The speech worker now
//! runs the identical [`transcription_allowed`] check against the text about to
//! be synthesized: it defaults to [`DEFAULT_MEDIA_CLASSIFICATION`]
//! (`Confidential`, the same "assume worst case" label captured media gets,
//! via [`speech_classification`]), evaluated against the SAME `routing.toml`
//! `[policy].max_off_device` ceiling the daemon's transcriber reads
//! (`codypendentd::transcription::HostedTranscriber::from_paths`), under
//! [`TranscriptionMode::Local`]/[`TranscriptionMode::Remote`] read from the
//! `[speech]` table's own `local` flag exactly as `[transcription]`'s is read
//! for STT (see [`speech_mode`]) — one function, one default, one ceiling,
//! applied to both directions of travel. A refusal never reaches the network:
//! it is reported through `speech_error` exactly like a synthesis or playback
//! failure, so a gagged reply is legible, not silently swallowed. The ceiling
//! is read fresh before every utterance — unlike the recorder probe, which is
//! cached once — so an operator who tightens `routing.toml` mid-session is
//! obeyed on the very next reply, not only after a restart.
//!
//! ## Honesty
//!
//! **The machine this was developed on has no audio hardware, no microphone,
//! and no speech-provider credentials.** The recorder-selection logic, the
//! command templating, the finalized-turn detection, and the drop-stale queue
//! are unit-tested; the STT/TTS HTTP clients are tested against wiremock in
//! `codypendent-runtime`. *Nothing here has ever driven a real microphone or a
//! real speaker.* Treat first-run capture on a real machine as unverified.

use std::path::{Path, PathBuf};
use std::time::Instant;

use codypendent_protocol::discovery::RuntimePaths;
use codypendent_protocol::input::{
    transcription_allowed, AudioArtifact, InputBlock, InputEnvelope, InputSource, OffDevicePolicy,
    ScopeLevel, TranscriptionMode, DEFAULT_MEDIA_CLASSIFICATION,
};
use codypendent_protocol::{
    ArtifactRef, DataClassification, EventBody, RunId, RunState, SessionEvent,
};
use codypendent_runtime::auth::AuthStore;
use codypendent_runtime::models::{
    load_audio_models, AudioModelConfig, AudioPlayer, AudioSynthesizer,
};
use crossterm::event::{Event as CrosstermEvent, KeyCode, KeyEvent};
use serde::Deserialize;

/// Where the speech worker leaves its most recent failure, for the UI to
/// surface. A TUI cannot log: stderr would corrupt the display, so a speech
/// failure is reported through the status line instead of being swallowed.
type SpeechError = std::sync::Arc<std::sync::Mutex<Option<String>>>;

/// The media type captured audio is uploaded as. Every probed recorder is
/// configured to write a WAV container.
pub const CAPTURE_MEDIA_TYPE: &str = "audio/wav";

/// The `[voice]` table in `<data_dir>/models.toml` — the same file that carries
/// `[transcription]`/`[speech]`, so all of voice is configured in one place.
///
/// ```toml
/// [voice]
/// # Optional: omit to auto-detect rec/arecord/ffmpeg on $PATH.
/// record_command = ["rec", "-q", "-r", "16000", "-c", "1", "-b", "16", "{path}"]
/// # Required for spoken replies; fed the clip on stdin.
/// play_command = ["mpv", "--no-terminal", "-"]
/// push_to_talk_key = "F4"
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VoiceConfig {
    /// The capture command. `{path}` is replaced with a temp `.wav` path the
    /// recorder must write. Empty means "probe `$PATH`".
    #[serde(default)]
    pub record_command: Vec<String>,
    /// The playback command, fed the clip on **stdin**. Empty disables spoken
    /// replies with an actionable error rather than guessing a binary.
    #[serde(default)]
    pub play_command: Vec<String>,
    /// The push-to-talk key, as a function key name (`"F4"`) or a single
    /// character. Defaults to `F4`, which no other binding uses.
    #[serde(default)]
    pub push_to_talk_key: Option<String>,
}

/// The `[voice]` table's enclosing file shape. Its own struct so `models.toml`'s
/// other tables (and every existing reader of them) are untouched.
#[derive(Debug, Default, Deserialize)]
struct VoiceFile {
    #[serde(default)]
    voice: Option<VoiceConfig>,
}

/// Read `[voice]` from `<data_dir>/models.toml`. An absent or malformed file
/// yields defaults: voice configuration must never be able to stop the TUI from
/// starting.
#[must_use]
pub fn load_voice_config(models_toml: &Path) -> VoiceConfig {
    std::fs::read_to_string(models_toml)
        .ok()
        .and_then(|text| toml::from_str::<VoiceFile>(&text).ok())
        .and_then(|file| file.voice)
        .unwrap_or_default()
}

/// A capture command template, and the binary it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorder {
    /// The program plus arguments; one argument contains `{path}`.
    pub command: Vec<String>,
    /// How the recorder was chosen, for diagnostics.
    pub source: RecorderSource,
}

/// Where a [`Recorder`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecorderSource {
    /// An explicit `record_command` in `[voice]`.
    Configured,
    /// `rec`, from sox.
    Sox,
    /// `arecord`, from alsa-utils.
    Arecord,
    /// `ffmpeg`.
    Ffmpeg,
}

impl RecorderSource {
    /// The binary a probed recorder needs (`None` when explicitly configured).
    #[must_use]
    pub fn binary(self) -> Option<&'static str> {
        match self {
            RecorderSource::Configured => None,
            RecorderSource::Sox => Some("rec"),
            RecorderSource::Arecord => Some("arecord"),
            RecorderSource::Ffmpeg => Some("ffmpeg"),
        }
    }
}

/// The recorders probed for, in preference order. `rec` first (sox is the most
/// portable and needs no platform-specific input spec), then ALSA's `arecord`,
/// then `ffmpeg` — whose capture device *is* platform-specific, hence last.
fn candidate_recorders() -> Vec<Recorder> {
    let mono_16k =
        |command: Vec<&str>| -> Vec<String> { command.into_iter().map(str::to_string).collect() };
    vec![
        Recorder {
            command: mono_16k(vec![
                "rec", "-q", "-r", "16000", "-c", "1", "-b", "16", "{path}",
            ]),
            source: RecorderSource::Sox,
        },
        Recorder {
            command: mono_16k(vec![
                "arecord", "-q", "-f", "S16_LE", "-r", "16000", "-c", "1", "-t", "wav", "{path}",
            ]),
            source: RecorderSource::Arecord,
        },
        Recorder {
            command: mono_16k(vec![
                "ffmpeg",
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                // The capture device is platform-specific; these are the
                // conventional defaults and may need overriding in `[voice]`.
                #[cfg(target_os = "macos")]
                "-f",
                #[cfg(target_os = "macos")]
                "avfoundation",
                #[cfg(target_os = "macos")]
                "-i",
                #[cfg(target_os = "macos")]
                ":0",
                #[cfg(not(target_os = "macos"))]
                "-f",
                #[cfg(not(target_os = "macos"))]
                "alsa",
                #[cfg(not(target_os = "macos"))]
                "-i",
                #[cfg(not(target_os = "macos"))]
                "default",
                "-ar",
                "16000",
                "-ac",
                "1",
                "{path}",
            ]),
            source: RecorderSource::Ffmpeg,
        },
    ]
}

/// Whether `program` is an executable on `$PATH`. A plain `$PATH` scan rather
/// than spawning `which`/`--version`: it costs no subprocess, and not every
/// recorder answers the same version flag.
fn on_path(program: &str, path_var: Option<&str>) -> bool {
    let Some(paths) = path_var else {
        return false;
    };
    std::env::split_paths(paths).any(|dir| {
        let candidate = dir.join(program);
        std::fs::metadata(&candidate).is_ok_and(|meta| meta.is_file())
    })
}

/// Choose the capture command: an explicit `record_command` always wins;
/// otherwise the first probed binary present on `$PATH`. `None` means no
/// recorder exists — the caller turns that into an actionable error.
#[must_use]
pub fn select_recorder(config: &VoiceConfig, path_var: Option<&str>) -> Option<Recorder> {
    if !config.record_command.is_empty() {
        return Some(Recorder {
            command: config.record_command.clone(),
            source: RecorderSource::Configured,
        });
    }
    candidate_recorders().into_iter().find(|recorder| {
        recorder
            .source
            .binary()
            .is_some_and(|b| on_path(b, path_var))
    })
}

/// The message shown when no recorder can be found — it names every binary that
/// would work and the setting that overrides the probe, so the user can act on
/// it without reading the docs.
#[must_use]
pub fn no_recorder_message() -> String {
    "voice: no recorder found — install sox (`rec`), alsa-utils (`arecord`), or ffmpeg, \
     or set voice.record_command in models.toml"
        .to_string()
}

/// Parse the configured push-to-talk key. Defaults to `F4`, which no other
/// binding uses; an unparseable value falls back to the default rather than
/// leaving the feature silently unreachable.
#[must_use]
pub fn push_to_talk_key(configured: Option<&str>) -> KeyCode {
    let Some(raw) = configured.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return KeyCode::F(4);
    };
    if let Some(number) = raw
        .strip_prefix('F')
        .or_else(|| raw.strip_prefix('f'))
        .and_then(|n| n.parse::<u8>().ok())
        .filter(|n| (1..=12).contains(n))
    {
        return KeyCode::F(number);
    }
    let mut chars = raw.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => KeyCode::Char(c),
        _ => KeyCode::F(4),
    }
}

/// What one push-to-talk keypress produced.
#[derive(Debug)]
pub enum CaptureOutcome {
    /// Capture just started; show the recording indicator.
    Started,
    /// Capture stopped and produced audio ready to upload.
    Captured {
        /// The WAV bytes.
        bytes: Vec<u8>,
        /// How long the recorder ran, in milliseconds — the honest measure of
        /// what was captured (no sample-rate arithmetic is invented).
        duration_ms: u64,
    },
    /// Something went wrong; the string is user-facing and actionable.
    Failed(String),
}

/// A capture in flight: the child recorder plus where it is writing.
struct ActiveCapture {
    child: tokio::process::Child,
    path: PathBuf,
    /// Kept so the temp directory outlives the recording.
    _dir: tempfile::TempDir,
    started: Instant,
}

/// The TUI's voice host.
pub struct VoiceHost {
    config: VoiceConfig,
    /// The probe result, computed ONCE at construction and cached for the
    /// process: `$PATH` does not change under a running TUI, and re-probing on
    /// every keypress would put filesystem work on the input path.
    recorder: Option<Recorder>,
    key: KeyCode,
    capture: Option<ActiveCapture>,
    /// Assistant text accumulated per run, flushed when the run finishes.
    pending_turns: std::collections::HashMap<RunId, String>,
    /// The queue-depth-one slot feeding the speech worker. `None` when speech
    /// is not configured.
    speech: Option<tokio::sync::watch::Sender<Option<String>>>,
    /// The speech worker's most recent failure, drained by the UI.
    speech_error: SpeechError,
}

impl VoiceHost {
    /// Build the host: read `[voice]`, probe for a recorder once, and start the
    /// speech worker when both a `[speech]` endpoint and a `play_command` are
    /// configured. Never fails — an unconfigured or broken voice setup degrades
    /// to actionable errors at the point of use.
    #[must_use]
    pub fn new(paths: &RuntimePaths) -> Self {
        let models_toml = paths.data_dir.join("models.toml");
        let config = load_voice_config(&models_toml);
        let path_var = std::env::var("PATH").ok();
        let recorder = select_recorder(&config, path_var.as_deref());
        let key = push_to_talk_key(config.push_to_talk_key.as_deref());
        let speech_error: SpeechError = std::sync::Arc::new(std::sync::Mutex::new(None));
        let speech = start_speech_worker(
            &models_toml,
            &paths.data_dir,
            &config.play_command,
            speech_error.clone(),
        );
        Self {
            config,
            recorder,
            key,
            capture: None,
            pending_turns: std::collections::HashMap::new(),
            speech,
            speech_error,
        }
    }

    /// Take the speech worker's most recent failure, if any, so the UI can show
    /// it once. A TUI cannot write to stderr without corrupting the display, so
    /// a failed synthesis or a missing player surfaces here rather than in a log
    /// nobody is watching.
    #[must_use]
    pub fn take_speech_error(&self) -> Option<String> {
        self.speech_error
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
    }

    /// Whether `event` is the push-to-talk key. Only a bare (unmodified) press
    /// counts, so `Shift`/`Ctrl` variants stay free for other bindings.
    #[must_use]
    pub fn is_push_to_talk(&self, event: &CrosstermEvent) -> bool {
        matches!(
            event,
            CrosstermEvent::Key(KeyEvent { code, modifiers, .. })
                if *code == self.key && modifiers.is_empty()
        )
    }

    /// Whether a capture is running.
    #[must_use]
    pub fn is_recording(&self) -> bool {
        self.capture.is_some()
    }

    /// Whether spoken replies are available at all (a `[speech]` endpoint AND a
    /// `play_command`). Used to explain the palette toggle when it cannot work.
    #[must_use]
    pub fn can_speak(&self) -> bool {
        self.speech.is_some()
    }

    /// The message explaining why speech is unavailable.
    #[must_use]
    pub fn speech_unavailable_message(&self) -> String {
        if self.config.play_command.is_empty() {
            "voice: set voice.play_command in models.toml (e.g. [\"mpv\", \"--no-terminal\", \"-\"]) to hear replies".to_string()
        } else {
            "voice: add a [speech] entry to models.toml to hear replies".to_string()
        }
    }

    /// Toggle push-to-talk: start a capture, or stop the running one and return
    /// its audio.
    pub async fn toggle(&mut self) -> CaptureOutcome {
        match self.capture.take() {
            Some(capture) => self.stop(capture).await,
            None => self.start().await,
        }
    }

    async fn start(&mut self) -> CaptureOutcome {
        let Some(recorder) = self.recorder.clone() else {
            return CaptureOutcome::Failed(no_recorder_message());
        };
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => {
                return CaptureOutcome::Failed(format!("voice: no scratch directory: {error}"))
            }
        };
        let path = dir.path().join("capture.wav");
        let argv = render_record_command(&recorder.command, &path);
        let Some((program, args)) = argv.split_first() else {
            return CaptureOutcome::Failed("voice: record_command is empty".to_string());
        };
        match tokio::process::Command::new(program)
            .args(args)
            // The recorder must never write onto the TUI's terminal.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            // A live microphone must not outlive the UI that opened it. If the
            // TUI exits, detaches, or loses the daemon between the two
            // push-to-talk presses, `VoiceHost` is dropped mid-recording;
            // without this the recorder keeps capturing indefinitely with no
            // window left to stop it.
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => {
                self.capture = Some(ActiveCapture {
                    child,
                    path,
                    _dir: dir,
                    started: Instant::now(),
                });
                CaptureOutcome::Started
            }
            Err(error) => CaptureOutcome::Failed(format!(
                "voice: could not start `{program}`: {error} — {}",
                no_recorder_message()
            )),
        }
    }

    async fn stop(&mut self, mut capture: ActiveCapture) -> CaptureOutcome {
        let duration_ms = u64::try_from(capture.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        // SIGINT, not kill: every supported recorder finalizes its WAV header on
        // an interrupt, and a SIGKILLed one leaves a header claiming no samples.
        interrupt(&capture.child).await;
        let exited = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            Box::pin(capture.child.wait()),
        )
        .await;
        if exited.is_err() {
            // It ignored the interrupt; take the file anyway after a hard stop.
            let _ = capture.child.kill().await;
        }
        match std::fs::read(&capture.path) {
            Ok(bytes) if bytes.len() > WAV_HEADER_BYTES => {
                CaptureOutcome::Captured { bytes, duration_ms }
            }
            Ok(_) => CaptureOutcome::Failed(
                "voice: the recorder produced no audio — check your input device".to_string(),
            ),
            Err(error) => {
                CaptureOutcome::Failed(format!("voice: no recording was written: {error}"))
            }
        }
    }

    /// Observe one durable session event, so the host can detect a **finalized**
    /// assistant turn: `ModelStreamDelta` text is accumulated per run and
    /// flushed the moment that run reaches a terminal state. Nothing is spoken
    /// mid-stream — half a sentence read aloud is worse than silence.
    ///
    /// `speak` is the palette toggle's current value; when it is off the text is
    /// still tracked (so turning speech on mid-run does not replay backlog, and
    /// turning it off stops immediately) but never enqueued.
    pub fn observe_event(&mut self, event: &SessionEvent, speak: bool) {
        match &event.body {
            EventBody::ModelStreamDelta { run_id, text } => {
                self.pending_turns
                    .entry(*run_id)
                    .or_default()
                    .push_str(text);
            }
            EventBody::RunStateChanged { run_id, state } if state.is_terminal() => {
                if let Some(text) = self.pending_turns.remove(run_id) {
                    if speak {
                        self.speak(text);
                    }
                }
            }
            EventBody::RunCompleted { run_id, .. } => {
                if let Some(text) = self.pending_turns.remove(run_id) {
                    if speak {
                        self.speak(text);
                    }
                }
            }
            _ => {}
        }
    }

    /// Queue `text` to be spoken. Queue depth is ONE: a newer turn replaces a
    /// still-queued older one, so speech tracks the conversation instead of
    /// falling minutes behind it. Never blocks.
    fn speak(&self, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if let Some(sender) = &self.speech {
            // `watch` IS the drop-stale queue: the worker only ever observes the
            // most recent value, so an overtaken clip is discarded by design.
            let _ = sender.send(Some(text));
        }
    }
}

/// A RIFF/WAVE header is 44 bytes; anything at or below that carries no samples.
const WAV_HEADER_BYTES: usize = 44;

/// Substitute `{path}` in a record-command template.
#[must_use]
pub fn render_record_command(template: &[String], path: &Path) -> Vec<String> {
    let rendered = path.display().to_string();
    template
        .iter()
        .map(|arg| arg.replace("{path}", &rendered))
        .collect()
}

/// Send SIGINT to a child so it can finalize its output file.
///
/// Delivered by shelling out to `kill` rather than through a libc/rustix
/// binding: signal support is not in this workspace's `rustix` feature set, and
/// adding a feature to a dependency shared by every crate — for one signal on
/// an optional feature's stop path — is a worse trade than one short-lived
/// process. A failure here is not fatal; the caller falls back to a hard kill.
async fn interrupt(child: &tokio::process::Child) {
    let Some(pid) = child.id() else {
        return;
    };
    let _ = tokio::process::Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
}

/// Whether a `[speech]` endpoint runs on-device — the OUTBOUND mirror of
/// `AudioTranscriber::is_local` (`codypendent_runtime::models`), read from the
/// exact same per-table `local` flag `[transcription]` uses for STT. Feeds the
/// identical [`transcription_allowed`] gate STT runs, so a local speech server
/// is always permitted and a hosted one is governed by the off-device ceiling.
#[must_use]
fn speech_mode(config: &AudioModelConfig) -> TranscriptionMode {
    if config.local {
        TranscriptionMode::Local
    } else {
        TranscriptionMode::Remote
    }
}

/// The classification assistant text is treated as before speech synthesis
/// (F8 — the outbound mirror of [`capture_classification`]): an assistant turn
/// routinely contains repository source, diffs, and command output, and this
/// build tracks no finer-grained per-turn classification, so it defaults to the
/// same "assume worst case" label captured media gets rather than assuming
/// text is safe to leave the device.
#[must_use]
pub fn speech_classification() -> DataClassification {
    DEFAULT_MEDIA_CLASSIFICATION
}

/// The `[policy]` table's off-device ceiling, read straight from
/// `<data_dir>/routing.toml` — its own minimal struct (like [`VoiceFile`]) so
/// the rest of the file (name, version, lambdas, quality_threshold,
/// escalation_chain) is neither required nor touched.
#[derive(Debug, Default, Deserialize)]
struct RoutingCeilingFile {
    #[serde(default)]
    policy: Option<RoutingCeilingPolicy>,
}

#[derive(Debug, Deserialize)]
struct RoutingCeilingPolicy {
    max_off_device: DataClassification,
}

/// Read `<data_dir>/routing.toml`'s off-device ceiling — the SAME value the
/// daemon's STT gate reads
/// (`RoutingConfig::load(paths).policy.max_off_device` in
/// `codypendentd::transcription::HostedTranscriber::from_paths`), so voice's
/// two directions share one privacy posture rather than two independently
/// maintained ones. Read fresh on every call (not cached like the recorder
/// probe): an operator who tightens the ceiling mid-session must be obeyed on
/// the very next reply, not only after a restart.
///
/// An absent or malformed file degrades to `Confidential` — exactly the
/// `RoutingPolicy::balanced()` ceiling an absent/malformed `routing.toml`
/// degrades to for STT (`RoutingConfig::default`/`RoutingConfig::invalid` both
/// keep `policy: RoutingPolicy::balanced()`) — so a typo here can neither
/// gag every reply nor open the gate wider than STT already tolerates.
#[must_use]
fn load_off_device_ceiling(data_dir: &Path) -> DataClassification {
    std::fs::read_to_string(data_dir.join("routing.toml"))
        .ok()
        .and_then(|text| toml::from_str::<RoutingCeilingFile>(&text).ok())
        .and_then(|file| file.policy)
        .map_or(DataClassification::Confidential, |policy| {
            policy.max_off_device
        })
}

/// Start the synthesis+playback worker, or `None` when either half of spoken
/// replies is unconfigured.
///
/// The worker owns the whole slow path — an HTTP round trip plus spawning a
/// player — on its own task, so the UI thread never waits on a speech provider.
fn start_speech_worker(
    models_toml: &Path,
    data_dir: &Path,
    play_command: &[String],
    errors: SpeechError,
) -> Option<tokio::sync::watch::Sender<Option<String>>> {
    if play_command.is_empty() {
        return None;
    }
    let audio = load_audio_models(models_toml).ok()?;
    let auth = AuthStore::load(data_dir).unwrap_or_default();
    let synthesizer = AudioSynthesizer::new(&audio, auth).ok()?;
    let player = AudioPlayer::new(play_command.to_vec());
    // Captured before `synthesizer` moves into the task below.
    let mode = speech_mode(synthesizer.config());
    let data_dir = data_dir.to_path_buf();

    let (tx, mut rx) = tokio::sync::watch::channel(None::<String>);
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            // Take the LATEST value: anything queued while the previous clip was
            // being synthesized has already been overwritten (drop-stale).
            let Some(text) = rx.borrow_and_update().clone() else {
                continue;
            };
            // Outbound privacy gate (F8): the SAME `transcription_allowed`
            // check the daemon runs on audio-in before a byte is read, run
            // here before a byte is sent. Confidential text — the default, and
            // an assistant turn routinely carries source, diffs, and command
            // output — may not leave the device unless a local endpoint or a
            // raised ceiling says otherwise. This must stay the FIRST thing in
            // the loop body, before any use of `synthesizer`.
            let policy = OffDevicePolicy {
                max_off_device: load_off_device_ceiling(&data_dir),
            };
            if let Err(refused) = transcription_allowed(speech_classification(), mode, &policy) {
                if let Ok(mut slot) = errors.lock() {
                    *slot = Some(format!(
                        "voice: reply not spoken — {refused}; add a local [speech] endpoint \
                         (local = true) or raise routing.toml's policy.max_off_device"
                    ));
                }
                continue;
            }
            let failure = match synthesizer.synthesize(&text).await {
                Ok(spoken) => player
                    .play(&spoken.bytes)
                    .await
                    .err()
                    .map(|error| format!("voice: playback failed — {error}")),
                Err(error) => Some(format!("voice: speech failed — {error}")),
            };
            if let (Some(failure), Ok(mut slot)) = (failure, errors.lock()) {
                *slot = Some(failure);
            }
        }
    });
    Some(tx)
}

/// Build the `InputEnvelope` for a captured voice note referencing the artifact
/// the daemon just stored. The transcript is deliberately absent: the daemon
/// produces it behind its classification gate, and the client must not pretend
/// to know what was said.
#[must_use]
pub fn voice_envelope(artifact: ArtifactRef, duration_ms: u64) -> InputEnvelope {
    InputEnvelope {
        source: InputSource::Voice,
        blocks: vec![InputBlock::Audio(AudioArtifact {
            original: artifact,
            transcript: None,
            duration_ms: Some(duration_ms),
            sample_rate_hz: Some(16_000),
        })],
        scope: ScopeLevel::Session,
        attachments: Vec::new(),
    }
}

/// The classification captured audio is uploaded under. Media defaults to
/// `Confidential` so a recording never leaves the device by accident; the
/// daemon's gate reads exactly this value back off the stored artifact.
#[must_use]
pub fn capture_classification() -> codypendent_protocol::DataClassification {
    DEFAULT_MEDIA_CLASSIFICATION
}

/// Whether a run state means the turn is over.
trait TerminalRunState {
    fn is_terminal(&self) -> bool;
}

impl TerminalRunState for RunState {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            RunState::Completed | RunState::Failed | RunState::Cancelled
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use codypendent_protocol::Actor;
    use crossterm::event::KeyModifiers;

    fn config_with(record: Vec<&str>) -> VoiceConfig {
        VoiceConfig {
            record_command: record.into_iter().map(str::to_string).collect(),
            play_command: Vec::new(),
            push_to_talk_key: None,
        }
    }

    #[test]
    fn a_configured_record_command_beats_every_probe() {
        // A user who names their own recorder must not be second-guessed, even
        // when sox is sitting right there on $PATH.
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("rec");
        std::fs::write(&bin, "#!/bin/sh\n").expect("write fake rec");
        let path_var = dir.path().display().to_string();

        let chosen = select_recorder(&config_with(vec!["my-recorder", "{path}"]), Some(&path_var))
            .expect("configured");
        assert_eq!(chosen.source, RecorderSource::Configured);
        assert_eq!(chosen.command[0], "my-recorder");
    }

    #[test]
    fn probing_prefers_sox_then_arecord_then_ffmpeg() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path_var = dir.path().display().to_string();
        let empty = VoiceConfig::default();

        // Nothing installed → nothing selected, and the message says what to do.
        assert!(select_recorder(&empty, Some(&path_var)).is_none());
        assert!(no_recorder_message().contains("sox"));
        assert!(no_recorder_message().contains("arecord"));
        assert!(no_recorder_message().contains("ffmpeg"));
        assert!(no_recorder_message().contains("record_command"));

        // ffmpeg alone → ffmpeg.
        std::fs::write(dir.path().join("ffmpeg"), "#!/bin/sh\n").expect("write");
        assert_eq!(
            select_recorder(&empty, Some(&path_var))
                .expect("ffmpeg")
                .source,
            RecorderSource::Ffmpeg
        );
        // arecord outranks ffmpeg.
        std::fs::write(dir.path().join("arecord"), "#!/bin/sh\n").expect("write");
        assert_eq!(
            select_recorder(&empty, Some(&path_var))
                .expect("arecord")
                .source,
            RecorderSource::Arecord
        );
        // rec (sox) outranks both.
        std::fs::write(dir.path().join("rec"), "#!/bin/sh\n").expect("write");
        assert_eq!(
            select_recorder(&empty, Some(&path_var))
                .expect("rec")
                .source,
            RecorderSource::Sox
        );
    }

    #[test]
    fn an_absent_path_selects_nothing_rather_than_guessing() {
        assert!(select_recorder(&VoiceConfig::default(), None).is_none());
    }

    #[test]
    fn every_probed_recorder_writes_to_the_templated_path() {
        // A template that never mentions `{path}` would record into the void.
        for recorder in candidate_recorders() {
            assert!(
                recorder.command.iter().any(|arg| arg.contains("{path}")),
                "{:?} must write to the templated path",
                recorder.source
            );
            let rendered = render_record_command(&recorder.command, Path::new("/tmp/x.wav"));
            assert!(rendered.iter().any(|arg| arg == "/tmp/x.wav"));
            assert!(!rendered.iter().any(|arg| arg.contains("{path}")));
        }
    }

    #[test]
    fn the_push_to_talk_key_defaults_to_f4_and_parses_overrides() {
        assert_eq!(push_to_talk_key(None), KeyCode::F(4));
        assert_eq!(push_to_talk_key(Some("")), KeyCode::F(4));
        assert_eq!(push_to_talk_key(Some("F9")), KeyCode::F(9));
        assert_eq!(push_to_talk_key(Some("f9")), KeyCode::F(9));
        assert_eq!(push_to_talk_key(Some("v")), KeyCode::Char('v'));
        // Nonsense falls back rather than leaving voice unreachable.
        assert_eq!(push_to_talk_key(Some("F99")), KeyCode::F(4));
        assert_eq!(push_to_talk_key(Some("not a key")), KeyCode::F(4));
    }

    #[test]
    fn the_voice_table_is_optional_and_a_broken_file_never_stops_the_tui() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("models.toml");

        std::fs::write(
            &path,
            "[[model]]\nid = \"m\"\nprovider = \"x\"\nmodel = \"y\"\n",
        )
        .expect("write");
        assert!(load_voice_config(&path).play_command.is_empty());

        std::fs::write(
            &path,
            "[voice]\nplay_command = [\"mpv\", \"-\"]\npush_to_talk_key = \"F7\"\n",
        )
        .expect("write");
        let config = load_voice_config(&path);
        assert_eq!(
            config.play_command,
            vec!["mpv".to_string(), "-".to_string()]
        );
        assert_eq!(
            push_to_talk_key(config.push_to_talk_key.as_deref()),
            KeyCode::F(7)
        );

        std::fs::write(&path, "[voice]\nplay_command = ").expect("write");
        assert!(load_voice_config(&path).play_command.is_empty());
        assert!(load_voice_config(&dir.path().join("gone.toml"))
            .play_command
            .is_empty());
    }

    fn host_for_tests() -> VoiceHost {
        VoiceHost {
            config: VoiceConfig::default(),
            recorder: None,
            key: KeyCode::F(4),
            capture: None,
            pending_turns: std::collections::HashMap::new(),
            speech: None,
            speech_error: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn event(sequence: u64, body: EventBody) -> SessionEvent {
        SessionEvent {
            sequence,
            occurred_at: chrono::Utc::now(),
            causation_id: None,
            correlation_id: None,
            actor: Actor::System,
            body,
        }
    }

    #[test]
    fn only_a_finished_run_flushes_its_assistant_turn() {
        // Speaking mid-stream would read half a sentence aloud; the turn is
        // spoken only once the run that produced it is done.
        let mut host = host_for_tests();
        let run_id = RunId::new();
        host.observe_event(
            &event(
                1,
                EventBody::ModelStreamDelta {
                    run_id,
                    text: "all ".to_string(),
                },
            ),
            true,
        );
        host.observe_event(
            &event(
                2,
                EventBody::ModelStreamDelta {
                    run_id,
                    text: "tests pass".to_string(),
                },
            ),
            true,
        );
        assert_eq!(
            host.pending_turns.get(&run_id).map(String::as_str),
            Some("all tests pass"),
            "deltas accumulate until the run finishes"
        );

        host.observe_event(
            &event(
                3,
                EventBody::RunStateChanged {
                    run_id,
                    state: RunState::Completed,
                },
            ),
            true,
        );
        assert!(
            host.pending_turns.is_empty(),
            "a finished run's turn is flushed"
        );
    }

    #[test]
    fn a_running_transition_does_not_flush_the_turn() {
        let mut host = host_for_tests();
        let run_id = RunId::new();
        host.observe_event(
            &event(
                1,
                EventBody::ModelStreamDelta {
                    run_id,
                    text: "thinking".to_string(),
                },
            ),
            true,
        );
        host.observe_event(
            &event(
                2,
                EventBody::RunStateChanged {
                    run_id,
                    state: RunState::Running,
                },
            ),
            true,
        );
        assert!(host.pending_turns.contains_key(&run_id));
    }

    #[test]
    fn turns_from_interleaved_runs_never_bleed_together() {
        let mut host = host_for_tests();
        let (a, b) = (RunId::new(), RunId::new());
        for (run_id, text) in [(a, "alpha"), (b, "beta"), (a, "-one"), (b, "-two")] {
            host.observe_event(
                &event(
                    1,
                    EventBody::ModelStreamDelta {
                        run_id,
                        text: text.to_string(),
                    },
                ),
                true,
            );
        }
        assert_eq!(
            host.pending_turns.get(&a).map(String::as_str),
            Some("alpha-one")
        );
        assert_eq!(
            host.pending_turns.get(&b).map(String::as_str),
            Some("beta-two")
        );
    }

    #[test]
    fn speech_off_still_tracks_but_discards_the_finished_turn() {
        let mut host = host_for_tests();
        let run_id = RunId::new();
        host.observe_event(
            &event(
                1,
                EventBody::ModelStreamDelta {
                    run_id,
                    text: "quiet".to_string(),
                },
            ),
            false,
        );
        host.observe_event(
            &event(
                2,
                EventBody::RunStateChanged {
                    run_id,
                    state: RunState::Completed,
                },
            ),
            false,
        );
        assert!(
            host.pending_turns.is_empty(),
            "the turn is dropped, not left to be replayed when speech turns on"
        );
    }

    #[tokio::test]
    async fn the_speech_queue_is_depth_one_and_drops_stale_clips() {
        // Three turns finish while the worker is busy with the first; only the
        // NEWEST survives, so speech tracks the conversation.
        let (tx, mut rx) = tokio::sync::watch::channel(None::<String>);
        tx.send(Some("first".to_string())).expect("send");
        tx.send(Some("second".to_string())).expect("send");
        tx.send(Some("third".to_string())).expect("send");

        rx.changed().await.expect("changed");
        assert_eq!(rx.borrow_and_update().clone(), Some("third".to_string()));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), rx.changed())
                .await
                .is_err(),
            "the overtaken clips are gone, not queued behind the newest"
        );
    }

    #[test]
    fn the_push_to_talk_key_ignores_modified_presses() {
        let host = host_for_tests();
        assert!(host.is_push_to_talk(&CrosstermEvent::Key(KeyEvent::new(
            KeyCode::F(4),
            KeyModifiers::NONE
        ))));
        assert!(!host.is_push_to_talk(&CrosstermEvent::Key(KeyEvent::new(
            KeyCode::F(4),
            KeyModifiers::SHIFT
        ))));
        assert!(!host.is_push_to_talk(&CrosstermEvent::Key(KeyEvent::new(
            KeyCode::F(5),
            KeyModifiers::NONE
        ))));
    }

    #[tokio::test]
    async fn with_no_recorder_the_key_produces_an_actionable_error_not_silence() {
        let mut host = host_for_tests();
        match host.toggle().await {
            CaptureOutcome::Failed(message) => {
                assert!(message.contains("install sox"), "{message}");
                assert!(message.contains("record_command"), "{message}");
            }
            other => panic!("expected an actionable failure, got {other:?}"),
        }
        assert!(!host.is_recording());
    }

    #[tokio::test]
    async fn a_capture_that_writes_nothing_is_reported_rather_than_submitted() {
        // `true` exits immediately writing no file — the shape of a recorder
        // that cannot open the input device. NOTE: this container has no audio
        // hardware, so this is the ONLY capture path exercised here.
        let mut host = host_for_tests();
        host.recorder = Some(Recorder {
            command: vec!["true".to_string(), "{path}".to_string()],
            source: RecorderSource::Configured,
        });
        assert!(matches!(host.toggle().await, CaptureOutcome::Started));
        assert!(host.is_recording());
        match host.toggle().await {
            CaptureOutcome::Failed(message) => assert!(
                message.contains("no recording") || message.contains("no audio"),
                "{message}"
            ),
            other => panic!("expected a failure, got {other:?}"),
        }
        assert!(!host.is_recording());
    }

    #[tokio::test]
    async fn a_capture_that_writes_a_wav_is_returned_with_its_duration() {
        // A fake "recorder" that writes a >44-byte WAV, standing in for a real
        // one. This proves the capture plumbing, NOT that any microphone works.
        let mut host = host_for_tests();
        host.recorder = Some(Recorder {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf 'RIFF____WAVEfmt ________________________________data____SAMPLES' > \"$1\""
                    .to_string(),
                "sh".to_string(),
                "{path}".to_string(),
            ],
            source: RecorderSource::Configured,
        });
        assert!(matches!(host.toggle().await, CaptureOutcome::Started));
        // Wait for the fake recorder to actually write, rather than sleeping a
        // fixed 150ms and hoping. Under a full parallel `cargo test --workspace`
        // even spawning `sh` can take longer than that, which made this fail with
        // "no recording was written" while proving nothing about the plumbing.
        let capture_path = host
            .capture
            .as_ref()
            .expect("capture is active after Started")
            .path
            .clone();
        for _ in 0..600 {
            if std::fs::metadata(&capture_path).is_ok_and(|m| m.len() > WAV_HEADER_BYTES as u64) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        match host.toggle().await {
            CaptureOutcome::Captured { bytes, .. } => {
                assert!(bytes.starts_with(b"RIFF"));
                assert!(bytes.len() > WAV_HEADER_BYTES);
            }
            other => panic!("expected captured audio, got {other:?}"),
        }
    }

    #[test]
    fn a_voice_envelope_carries_no_transcript_and_defaults_to_confidential() {
        let artifact = ArtifactRef {
            id: codypendent_protocol::ArtifactId::new(),
            media_type: CAPTURE_MEDIA_TYPE.to_string(),
            byte_length: 64_000,
            sha256: "a".repeat(64),
            sensitivity: capture_classification(),
        };
        let envelope = voice_envelope(artifact.clone(), 4_000);
        assert_eq!(envelope.source, InputSource::Voice);
        let InputBlock::Audio(audio) = &envelope.blocks[0] else {
            panic!("expected an audio block");
        };
        assert!(
            audio.transcript.is_none(),
            "the client never pretends to know what was said"
        );
        assert_eq!(audio.duration_ms, Some(4_000));
        assert_eq!(
            artifact.sensitivity,
            codypendent_protocol::DataClassification::Confidential
        );
    }

    // -----------------------------------------------------------------
    // F8: the outbound privacy gate.
    // -----------------------------------------------------------------

    #[test]
    fn speech_mode_reads_the_tables_own_local_flag() {
        let mut config = AudioModelConfig {
            base_url: "https://example.invalid".to_string(),
            model: "tts".to_string(),
            api_key_env: String::new(),
            voice: None,
            format: None,
            local: false,
            timeout_secs: 30,
        };
        assert_eq!(speech_mode(&config), TranscriptionMode::Remote);
        config.local = true;
        assert_eq!(speech_mode(&config), TranscriptionMode::Local);
    }

    #[test]
    fn the_default_speech_classification_matches_the_default_capture_classification() {
        // Both directions assume the worst case by default; there is exactly
        // one constant for "unclassified media/text", not two that could drift.
        assert_eq!(speech_classification(), capture_classification());
        assert_eq!(speech_classification(), DataClassification::Confidential);
    }

    #[test]
    fn load_off_device_ceiling_defaults_to_confidential_when_absent_or_malformed() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            load_off_device_ceiling(dir.path()),
            DataClassification::Confidential,
            "no routing.toml at all must not gag every reply"
        );

        std::fs::write(dir.path().join("routing.toml"), "not = [valid toml").expect("write");
        assert_eq!(
            load_off_device_ceiling(dir.path()),
            DataClassification::Confidential,
            "a typo must degrade exactly like STT's ceiling does, not fail closed differently"
        );

        std::fs::write(
            dir.path().join("routing.toml"),
            "enabled = true\n\n[policy]\nname = \"tight\"\nversion = 1\n\
             quality_threshold = 0.7\nmax_off_device = { type = \"Internal\" }\n\n\
             [policy.lambdas]\ncost = 1.0\nlatency = 1.0\nprivacy = 1.0\nfailure = 1.0\n",
        )
        .expect("write");
        assert_eq!(
            load_off_device_ceiling(dir.path()),
            DataClassification::Internal,
            "an explicit ceiling in routing.toml must be honored"
        );
    }

    /// Write a `[speech]`-only `models.toml` pointed at `base_url`.
    fn speech_models_toml(
        dir: &tempfile::TempDir,
        base_url: &str,
        local: bool,
    ) -> std::path::PathBuf {
        let path = dir.path().join("models.toml");
        std::fs::write(
            &path,
            format!(
                "[speech]\nbase_url = \"{base_url}\"\nmodel = \"tts-1\"\n\
                 api_key_env = \"\"\nlocal = {local}\n"
            ),
        )
        .expect("write models.toml");
        path
    }

    fn write_routing_ceiling(dir: &tempfile::TempDir, max_off_device: &str) {
        std::fs::write(
            dir.path().join("routing.toml"),
            format!(
                "enabled = true\n\n[policy]\nname = \"t\"\nversion = 1\n\
                 quality_threshold = 0.7\nmax_off_device = {{ type = \"{max_off_device}\" }}\n\n\
                 [policy.lambdas]\ncost = 1.0\nlatency = 1.0\nprivacy = 1.0\nfailure = 1.0\n"
            ),
        )
        .expect("write routing.toml");
    }

    /// A stand-in "player": a shell that reads stdin and discards it, exactly
    /// like `crates/runtime/tests/audio.rs`'s convention (no audio device
    /// exists in this container).
    fn discard_player() -> Vec<String> {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "cat > /dev/null".to_string(),
        ]
    }

    /// Poll `server`'s recorded requests until `at_least` arrive or `timeout`
    /// elapses, so the assertion does not race the background worker task.
    async fn wait_for_requests(
        server: &wiremock::MockServer,
        at_least: usize,
        timeout: std::time::Duration,
    ) -> Vec<wiremock::Request> {
        let deadline = Instant::now() + timeout;
        loop {
            let requests = server.received_requests().await.expect("recorded requests");
            if requests.len() >= at_least || Instant::now() >= deadline {
                return requests;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn confidential_text_never_reaches_a_speech_endpoint_under_a_restrictive_ceiling() {
        // THE proof requested by the outcome-8 assignment: a stub speech
        // endpoint that records what it receives, under a policy that permits
        // only Internal off-device — below the Confidential default every
        // assistant turn is assumed to carry.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/audio/speech"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_bytes(b"fake-mp3".to_vec())
                    .insert_header("content-type", "audio/mpeg"),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let models_toml = speech_models_toml(&dir, &server.uri(), false);
        write_routing_ceiling(&dir, "Internal");

        let errors: SpeechError = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sender =
            start_speech_worker(&models_toml, dir.path(), &discard_player(), errors.clone())
                .expect("both [speech] and play_command are configured");

        sender
            .send(Some("fix the auth bug in src/secrets.rs".to_string()))
            .expect("send");
        // Give the worker a bounded window to (wrongly) complete a full HTTP
        // round trip if the gate were absent, then assert it did not.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let requests = server.received_requests().await.expect("recorded requests");
        assert!(
            requests.is_empty(),
            "confidential text must NEVER reach the speech endpoint under a restrictive ceiling, got: {requests:?}"
        );
        let error = errors
            .lock()
            .expect("lock")
            .clone()
            .expect("the refusal is reported, not silently swallowed");
        assert!(error.contains("not spoken"), "{error}");
    }

    #[tokio::test]
    async fn a_permissive_ceiling_lets_the_reply_reach_the_stub_endpoint() {
        // The counterpart proof: the gate is a real gate, not a wire that
        // always refuses. Under a ceiling that permits Confidential off-device
        // (the default, unconfigured posture — see
        // `load_off_device_ceiling`'s own default), speech proceeds.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/audio/speech"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_bytes(b"fake-mp3".to_vec())
                    .insert_header("content-type", "audio/mpeg"),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let models_toml = speech_models_toml(&dir, &server.uri(), false);
        write_routing_ceiling(&dir, "Confidential");

        let errors: SpeechError = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sender =
            start_speech_worker(&models_toml, dir.path(), &discard_player(), errors.clone())
                .expect("both [speech] and play_command are configured");

        sender
            .send(Some("all tests pass".to_string()))
            .expect("send");
        let requests = wait_for_requests(&server, 1, std::time::Duration::from_secs(3)).await;

        assert_eq!(requests.len(), 1, "the permitted reply must reach the stub");
        let body: serde_json::Value =
            serde_json::from_slice(&requests[0].body).expect("json request body");
        assert_eq!(body["input"], "all tests pass");
        assert!(
            errors.lock().expect("lock").is_none(),
            "a permitted reply must not report a privacy refusal"
        );
    }

    #[tokio::test]
    async fn a_speech_endpoint_marked_local_bypasses_the_ceiling_entirely() {
        // Mirrors STT's `a_local_endpoint_is_local_under_any_ceiling`: an
        // operator who marks their OWN `[speech]` endpoint `local = true` gets
        // it honored even under the tightest possible ceiling.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/audio/speech"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_bytes(b"fake-mp3".to_vec())
                    .insert_header("content-type", "audio/mpeg"),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let models_toml = speech_models_toml(&dir, &server.uri(), true);
        write_routing_ceiling(&dir, "Public");

        let errors: SpeechError = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sender =
            start_speech_worker(&models_toml, dir.path(), &discard_player(), errors.clone())
                .expect("both [speech] and play_command are configured");

        sender
            .send(Some("a local endpoint is always permitted".to_string()))
            .expect("send");
        let requests = wait_for_requests(&server, 1, std::time::Duration::from_secs(3)).await;
        assert_eq!(
            requests.len(),
            1,
            "a local speech endpoint bypasses the off-device ceiling"
        );
    }
}
