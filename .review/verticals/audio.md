# Vertical: audio (outcome 8 — built-in TTS + STT for immersive interaction)

Reviewer pass against pinned commit `535a2f5e3848b256536ddee94883dc0010ecdcb8` (v0.4.5).
Every file in the vertical was read in full. The daemon was built, started, and driven
end-to-end over its Unix socket against a mock OpenAI-compatible speech provider.

---

## Verdict

**OUTCOME 8: PARTIAL** — the speech-to-text pipeline genuinely works end-to-end against a
configured cloud endpoint (proven live: audio → multipart POST → transcript → run
objective → ledger note), but nothing about it is "built-in": there are zero audio crates
in the workspace, no capture code, no playback code, and no local model — capture shells
out to `rec`/`arecord`/`ffmpeg`, playback shells out to a command the user must name, and
both STT and TTS are HTTP calls to a provider whose API key cannot be set from any UI the
product ships.

---

## What I actually ran

Live, in this container, with real binaries:

| # | Action | Result |
|---|---|---|
| 1 | `codypendent --help` | 22 subcommands; **no `voice` subcommand** |
| 2 | `codypendent voice` | `error: unrecognized subcommand 'voice'`, **exit 2** |
| 3 | `codypendent models add openai gpt-4o-mini` over a models.toml containing `[transcription]`, `[speech]`, `[voice]`, `[embedding]` | printed `added model openai/gpt-4o-mini`; **all four tables deleted from the file** |
| 4 | Real `codypendentd` + mock `/v1/audio/transcriptions` + hand-written socket client | STT worked: mock hit with a 346-byte `multipart/form-data` body, transcript became the run objective, `NoteAppended` written |
| 5 | Same client, envelope with only an `image` block and empty text | `CommandRejected` `voice.empty-transcript` — "the submitted input produced no text to run" |
| 6 | Same client, envelope with only an `editor-selection` block and empty text | `CommandRejected` `voice.empty-transcript` |

Step 4's DB state after the run:

```
runs:     ('019ff87e-…', 'fix the flaky retry test', 'Failed')
command:  text='fix the flaky retry test'
          block: audio | transcript: {"text":"fix the flaky retry test","mode":{"type":"local"},
                                      "model":"whisper-large-v3-turbo","reviewed":false,…}
event:    {"type":"NoteAppended","text":"transcribed 4.0 s of audio (model whisper-large-v3-turbo)"}
```

The `Failed` state is the mock chat model, not the voice path. Transcription itself is
correct: the original artifact is preserved, the transcript is *added* and linked back by
`source_audio`, and the note names the duration and model. **That half of outcome 8 works.**

---

## Answers to the specific questions asked

**STT — what produces text from audio?** A cloud HTTP call. `AudioTranscriber::transcribe`
at `crates/runtime/src/models.rs:1632` POSTs a hand-assembled `multipart/form-data` body to
`{base_url}/audio/transcriptions` and reads `.text` out of the JSON reply. There is no
model, no local inference, no bundled binary. `[transcription].local = true`
(`crates/runtime/src/models.rs:1411`) is *only* a policy assertion — it changes the
classification gate's verdict, it does not change how anything runs. A user setting
`local = true` while pointing at `api.groq.com` gets remote transcription of Confidential
audio with the gate satisfied.

**Where does audio come FROM?** A subprocess. `crates/cli/src/voice.rs:151-201` defines
three hardcoded argv templates (`rec`, `arecord`, `ffmpeg`), `select_recorder`
(`voice.rs:220`) picks the first present on `$PATH`, and `VoiceHost::start`
(`voice.rs:397`) spawns it writing to a temp `.wav`. Stopping sends SIGINT by shelling out
to `kill` (`voice.rs:539`). There is **no capture code** — no `cpal`, no device
enumeration, no PCM handling anywhere in the repo. It does not accept a pre-recorded file
path either: there is no CLI or config surface for supplying one.

**TTS — is there any?** Yes, but it is a second cloud call, not built-in speech.
`AudioSynthesizer::synthesize` at `crates/runtime/src/models.rs:1765` POSTs
`{model, input, voice, response_format}` to `{base_url}/audio/speech` and takes the raw
bytes. Playback is `AudioPlayer::play` (`models.rs:1867`), which pipes those bytes to a
user-configured command's stdin and drops the child handle. No synthesis, no audio output
device, no decoder.

**Are the audio deps in Cargo.toml?** No. Zero. `cpal`, `rodio`, `hound`, `symphonia`,
`whisper-rs`, `sherpa`, `espeak`, `coqui` — none appear in the workspace `Cargo.toml` or
any crate `Cargo.toml`. The only audio-adjacent line in any manifest is a comment in
`crates/runtime/Cargo.toml:60` explaining that wiremock stands in for a provider. The
feature is not feature-gated off — it compiles and ships by default; there is simply no
audio code to gate. Note: `piper 0.2.5` in `Cargo.lock:3146` is the async-pipe utility
crate (a transitive dep of `blocking`/`async-process`), **not** the Piper TTS engine.

**Which cloud provider, and can the user set the key?** Any OpenAI-compatible one — the
docs name Groq, OpenAI, DeepInfra, Together. The key resolution path
(`crates/runtime/src/models.rs:1543`) tries `auth.json` first, then the named env var.
**The `auth.json` half is unreachable** — see F3. So the only working path is an
environment variable that must already be exported in the *daemon's* process for STT and
in the *TUI's* process for TTS.

---

## Findings

### F1 — `codypendent models add` silently deletes the entire voice configuration
`crates/cli/src/commands.rs:2894-2905` · **class (c) — wire attached, wrong behaviour**

`models_add` reconstructs `models.toml` from a struct that knows only about `[[model]]`:

```rust
#[derive(serde::Serialize)]
struct ModelsToml {
    #[serde(rename = "model")]
    model: Vec<ModelConfig>,
}
let rendered = toml::to_string_pretty(&ModelsToml { model: configs })
```

…then atomically renames it over the real file. `[transcription]`, `[speech]`, `[voice]`,
`[embedding]` and `[retrieval]` are all destroyed.

**Proven live.** A models.toml with all five tables, after `codypendent models add openai
gpt-4o-mini`, contained two `[[model]]` blocks and nothing else. The command printed
`added model openai/gpt-4o-mini` and exited 0 with no warning.

This is not a case nobody thought about — it is a case that was thought about three times
and missed once. Every other writer of this file guards against it explicitly, by name:

- `crates/cli/src/models_pull.rs:285` — *"`models.toml` is not only `[[model]]`: it also
  carries `[voice]`, `[transcription]`, `[speech]`, `[embedding]` and `[retrieval]`.
  Serializing a struct that knows only about models would silently delete every one of
  them, disabling a user's voice and retrieval setup on the next load."*
- `crates/cli/src/acp_clients.rs:487` — *"Replacing the whole file from a model-only struct
  silently erased those tables whenever an ACP client connected or disconnected."*
  (past tense — this was a live bug once already)
- `crates/cli/src/tui.rs:4262` — merges into a parsed `toml::Value` root.

All three read the existing document into a `toml::Value` and replace only the `model` key.
`models_add` is the one that does not.

**User-visible consequence, made worse by timing:** the daemon builds `HostedTranscriber`
once at startup (`crates/codypendentd/src/lib.rs:209`), so voice keeps working after the
config is destroyed. It dies on the *next* daemon restart, with `voice.transport-unavailable`
and no connection to the command that caused it — hours or days later.

### F2 — "Built-in" is absent; every audio primitive is delegated to something not shipped
`Cargo.toml` (no audio deps) · `crates/cli/src/voice.rs:151-201` · `crates/runtime/src/models.rs:1839` · **class (a) — engine missing entirely**

Outcome 8 says *built-in* TTS + STT. What exists is four delegations:

| Capability | Delegated to | Ships with product? |
|---|---|---|
| Microphone capture | `rec` / `arecord` / `ffmpeg` subprocess | No |
| Speech→text | HTTP `/audio/transcriptions` | No |
| Text→speech | HTTP `/audio/speech` | No |
| Audio playback | user-named subprocess on stdin | No |

The code is candid about this — `voice.rs:12` says "There is **no bundled recorder**",
`models.rs:1827` says "There is no bundled audio backend on purpose". Those are defensible
engineering choices, but they are not the outcome as stated. On a fresh machine with no
sox/ffmpeg, no `mpv`, and no provider key, pressing F4 produces
`voice: no recorder found — install sox (rec), alsa-utils (arecord), or ffmpeg`
(`voice.rs:240`) and nothing else. Nothing in outcome 8 is reachable out of the box.

### F3 — STT/TTS API keys cannot be set from the TUI, contradicting the shipped docs
`crates/runtime/src/models.rs:1548` · `crates/tui/src/action.rs:617` · `crates/cli/src/tui.rs:4676` · **class (b) — engine built, wire never attached**

The key resolver reads `auth.json` keyed by the literal table name:

```rust
// crates/runtime/src/models.rs:1548
if let Some(key) = auth.get(table).filter(|key| !key.is_empty()) {
```

…where `table` is `"transcription"` or `"speech"`. But nothing can ever write those keys:

- `KeyTarget` (`crates/tui/src/action.rs:617`) has exactly two variants: `Model(String)`
  and `Tavily`.
- `key_target_auth_id` (`crates/cli/src/tui.rs:4676-4681`) maps them to a model id or the
  Tavily id.
- `key_row_target` (`crates/tui/src/state.rs:1719`) indexes into `state.models`, which is
  seeded from `ModelCard`s built from `ModelConfig`s (`crates/cli/src/tui.rs:6284`) — i.e.
  from `[[model]]` entries only. `[transcription]` and `[speech]` deserialize into
  `AudioModelConfig`, never `ModelConfig`, so they never produce a `/keys` row.

The user guide states the opposite at `docs/cli-and-tui-user-guide.md:662`:
*"Keys resolve exactly as chat models' do: a key saved via `/keys` (in `auth.json`) wins."*
A user following that sentence will open `/keys`, find no row for their speech provider,
and have no supported way to supply the credential.

The only working path is an environment variable — and it must be set in the right
*process*: `GROQ_API_KEY` for STT must exist in the **daemon's** environment (resolved in
`crates/codypendentd/src/transcription.rs:66`), while `OPENAI_API_KEY` for TTS must exist
in the **TUI's** environment (`crates/cli/src/voice.rs:566`). Exporting either after the
daemon is already running has no effect and produces no diagnostic.

### F4 — Five of seven `InputBlock` variants are inert, and the error blames the user
`crates/daemon/src/transcription.rs:265-281` · `crates/daemon/src/server.rs:1038-1042` · **class (a) + SILENT FILTER**

`envelope_text` is the only function that turns an envelope into runnable text:

```rust
match block {
    InputBlock::Text { text } if !text.trim().is_empty() => parts.push(text),
    InputBlock::Audio(audio) => { /* push transcript */ }
    _ => {}
}
```

`Image`, `File`, `EditorSelection`, `CodeSymbol` and `GitHubReference` fall into `_ => {}`.
An envelope carrying only those, with empty text, then hits:

```rust
// crates/daemon/src/server.rs:1038
CodypendentError::new("voice.empty-transcript",
    "the submitted input produced no text to run", false)
```

**Proven live** — steps 5 and 6 of the run table above. The message says the input produced
no text. The truth is that this build understands two of seven block types and dropped the
rest without saying so. A VS Code extension author sending an `editor-selection` block
(the extension already has the wire type, `extensions/vscode/src/protocol/types.ts`) gets
told their input was empty.

ROADMAP is honest about this at line 469 — *"6.5/6.7 (client capture + setup assistant) —
TUI clipboard/voice capture and IDE drag-drop feeding the input model"* is unchecked. The
README is not: `README.md:120-131` lists all seven block types as supported and adds
*"Image ingestion preserves source image plus extracted/OCR interpretation."* There is no
OCR and no image ingestion anywhere in the repo — `ImageArtifact` appears only as a
protocol re-export in `crates/protocol/src/lib.rs:54` and in tests.

### F5 — The envelope never reaches the model; it is flattened to a string
`crates/daemon/src/server.rs:2562-2572` · **class (a)**

```rust
// `envelope` is deliberately ignored here: by
// this point `resolve_voice_input` has already
// folded any transcript into `text`, which is
// what the run's objective must be.
CommandBody::SubmitUserInput { session_id, text, mode, model, .. } => {
    executor.spawn_run(RunLaunch { objective: text.clone(), … })
}
```

`RunLaunch` carries an `objective: String`. The audio artifact, the image artifact, the
observations, the regions, the editor range — none of it crosses the executor boundary.
This is the structural reason F4 cannot be fixed by teaching `envelope_text` about more
block types: even a perfect OCR pipeline would have nowhere to deliver an image. The
multimodal input model of roadmap 6.5 is a serialization format with a text-only consumer.

### F6 — `codypendent doctor` has no voice checks at all
`crates/cli/src/doctor.rs` · **class (b)**

`doctor` is described as *"Diagnose the local setup: binary + build id, daemon health,
runtime paths, model config, and provider reachability."* A case-insensitive grep of the
whole file for `voice|speech|transcription|audio` returns **nothing**. Given F1 (config
silently deleted), F3 (key unsettable from the UI) and the two-process env-var split, this
is precisely the feature that most needs a diagnostic and is the only major one without
one. A user whose voice stopped working has no supported way to find out why short of
reading `models.toml` by hand.

### F7 — The `audio_capture` capability is declared, serialized, golden-vectored, and always false
`crates/protocol/src/capabilities.rs:23` · `crates/council/src/connection.rs:71` · **class (b) — data produced, never consumed**

`ClientCapabilities.audio_capture` exists in the Rust protocol, the TypeScript extension
types (`extensions/vscode/src/protocol/types.ts:52`), the UI SDK
(`sdk/ui/src/protocol.ts:80`), and the golden vectors. Every one of the eight construction
sites in the repo hardcodes `false`. The shared handshake used by *every* Rust client —
including the TUI, the only client that has push-to-talk — sends
`ClientCapabilities::default()` (`crates/council/src/connection.rs:71`), so the one client
that can capture audio advertises that it cannot. Nothing in the Rust daemon ever reads the
field to gate anything, so no behaviour changes; it is dead weight that will read as a
working capability negotiation to the next person who touches it.

### F8 — Spoken replies leave the machine with no privacy gate, in a system built around one
`crates/cli/src/voice.rs:556-592` · **class (c) — wrong behaviour**

The input direction is carefully gated: `transcribe_envelope`
(`crates/daemon/src/transcription.rs:181`) re-reads the *stored* classification out of the
artifact table (explicitly rejecting the client's claimed `ArtifactRef.sensitivity` as
untrusted wire data — a genuinely good trust-boundary read) and runs
`transcription_allowed` against the operator's `max_off_device` ceiling before a single
byte is read.

The output direction has no gate whatsoever. `start_speech_worker` builds an
`AudioSynthesizer` and `VoiceHost::speak` (`voice.rs:506`) hands it the full text of every
finished assistant turn. A grep of the entire `crates/cli/src/voice.rs` for
`policy|ceiling|classification|allowed|off_device` finds only the *capture* classification
constant; `transcription_allowed` has no output-side counterpart anywhere in the repo.

Assistant turns routinely contain repository source code, file paths, diffs and command
output. With `[speech]` configured and "Voice: speak replies" toggled on, all of it is
POSTed verbatim to a hosted provider regardless of `routing.toml`. The user guide's
§5.4 is titled *"Privacy: when audio may leave your machine"* and covers only the inbound
direction; a user who reads it and tightens `max_off_device` will reasonably believe they
have covered voice.

### F9 — Minor: a TOML typo silently reverts explicit voice settings and then misreports why
`crates/cli/src/voice.rs:105-111`, `380-386`

`load_voice_config` swallows every parse error (`.ok()` twice) and returns defaults. If
`models.toml` has a syntax error anywhere, the user's explicit `record_command` silently
reverts to `$PATH` auto-probing and `play_command` becomes empty — at which point
`speech_unavailable_message` (`voice.rs:381`) checks `play_command.is_empty()` first and
tells the user to *"set voice.play_command in models.toml"*, which they already did. The
existing test at `voice.rs:763-768` asserts this swallow as intended behaviour.

### F10 — Minor: `artifact_ids()` has no production consumer
`crates/protocol/src/input.rs:60-77`

Walks an envelope collecting every artifact id from image/audio/file blocks. Called by
nothing outside `input.rs`'s own tests.

### F11 — Minor: `HostedTranscriber::transcribe` had no test exercising it
`crates/codypendentd/src/transcription.rs:122-155`

The unit tests in that file only check config parsing and mode selection; `voice_it.rs`
substitutes a fake `Transcriber`; `crates/runtime/tests/audio.rs` tests the layer below.
Nothing joined the two. I exercised it live against a mock provider and it worked
correctly, including media-type→extension mapping — so this is a coverage gap, not a
defect, but it was the one seam in the chain with no test at all.

### F12 — Context: voice is TUI-only, with no headless path
`crates/cli/src/main.rs` (no `Voice` subcommand) · `crates/cli/src/tui.rs:1335`

`codypendent voice` exits 2 with `error: unrecognized subcommand 'voice'`. `VoiceHost` is
constructed only inside the interactive TUI event loop. There is no way to transcribe a
file, script a voice interaction, or exercise any of this from CI or an IDE. Not a defect
against the outcome as written, but it means the only testable surface is a raw-mode TUI.

---

## What is genuinely good here

Worth recording so the implementation phase does not tear it out:

- The STT pipeline is **correct and works** — proven live end-to-end.
- The trust-boundary read at `crates/daemon/src/transcription.rs:163-180` re-derives
  classification from the stored row rather than believing the client's `ArtifactRef`,
  with a comment that names the exact attack it closes. That is the pattern the brief
  asks reviewers to hunt for, done right.
- Original-audio preservation is structural, not conventional: the transcript is added
  alongside a untouched `original` ref and linked by `source_audio`.
- Reusing `routing.toml`'s `max_off_device` rather than inventing a second privacy knob is
  the right call (F8 is that it was only applied in one direction).
- The dependency inversion (`Transcriber` trait in `daemon`, impl in `codypendentd`) is
  clean and matches the document/workflow/promotion seams.
- The docs are unusually honest: `docs/cli-and-tui-user-guide.md:624-630` carries an
  explicit warning that no part of capture or playback has run against real hardware.

---

## Grounded recommendation for the implementation phase

If outcome 8 is to mean *built-in*, these are the current realistic Rust options:

**Capture + playback (the gap that makes everything else moot):**
- [`cpal`](https://crates.io/crates/cpal) — the standard cross-platform audio I/O crate;
  on Linux it tries PipeWire → PulseAudio → ALSA. Building it needs ALSA dev headers
  (`libasound2-dev`) even when PipeWire is the runtime host, which is the platform-native
  dependency the current design was avoiding — the honest fix is an optional cargo feature,
  not permanent subprocess delegation.
- [`rodio`](https://crates.io/crates/rodio) — playback on top of `cpal`, with decoding via
  Symphonia (or `hound` for WAV, `minimp3` for MP3). Replaces `AudioPlayer`'s
  pipe-to-`mpv`.

**Local STT:**
- [`whisper-rs`](https://crates.io/crates/whisper-rs) — whisper.cpp bindings, ~115k
  downloads/month, CUDA and ROCm/hipBLAS support. The mature default.
- [`whisper-cpp-plus`](https://docs.rs/whisper-cpp-plus/) — safe bindings with real-time
  PCM streaming and VAD, which is what would be needed to make the README's
  "streaming transcription" claim true.

**Local TTS:**
- [`kokoro-en`](https://lib.rs/crates/kokoro-en) — offline inference for Kokoro-82M
  (Apache 2.0, ~2-3 GB VRAM, runs on CPU), with CoreML on macOS and CUDA elsewhere,
  automatic CPU fallback. Note the catalog *already* lists `hexgrad/Kokoro-82M` at
  `crates/providers/builtin_catalog.toml:1332` — as a hosted DeepInfra/Together row, not a
  local engine.
- [`tts-rs`](https://github.com/rishiskhare/tts-rs) — Kokoro on ONNX Runtime, 26 voices /
  9 languages, f32/f16/int8 variants.
- Piper (OHF-Voice) — VITS exported to ONNX; the option when compute is near zero
  (runs on a Pi 4).

**Cheapest high-value fixes, in order:**
1. F1 — three lines: read `models.toml` into a `toml::Value` and replace only the `model`
   key, exactly as the other three writers already do. This is a data-loss bug shipping today.
2. F3 — add `KeyTarget::Transcription`/`KeyTarget::Speech` rows to `/keys`, or delete the
   unreachable `auth.json` branch and correct the doc sentence.
3. F6 — add voice to `doctor`: is `[transcription]` present, does its key resolve *in the
   daemon's environment*, is a recorder on `$PATH`, is `play_command` set.
4. F8 — run the assistant text through the same `max_off_device` ceiling before synthesis.
5. F4/F5 — either teach `RunLaunch` to carry the envelope, or make the rejection say which
   block types were dropped instead of "produced no text".

---

## What I could not exercise, and why

- **Any real microphone or speaker.** No audio device in this container. Everything above
  the device boundary — recorder selection, argv templating, SIGINT-then-read, WAV size
  guard, upload, transcription, transcript→objective, the drop-stale speech queue — is
  testable without one, and I tested it. What remains unverified is exactly what the
  authors already flag in `docs/cli-and-tui-user-guide.md:624`: whether `rec -q -r 16000
  -c 1 -b 16 file.wav` and the platform-specific `ffmpeg -f alsa -i default` incantation
  actually capture usable audio on a real device, and whether SIGINT finalizes the header
  in practice. **Exercise limitation, not a finding.**
- **The TTS round trip end-to-end.** The synthesis half is reachable only from inside the
  interactive TUI's palette (`PaletteCommand::VoiceSpeak`,
  `crates/tui/src/reduce.rs:5901`) with a live daemon, a live model producing
  `ModelStreamDelta` events, and a raw-mode terminal. I verified by reading that the
  trigger is wired to events the real agent loop does emit
  (`crates/runtime/src/agent.rs:2383`, `2656`) and that the TUI's default subscriptions
  (`crates/cli/src/tui.rs:107`) include them, and I confirmed the HTTP client's shape
  against `/audio/speech`; I did not drive a clip from a finished turn to a player.
- **A real provider.** No credentials. Every provider interaction above used a local mock
  answering the documented response shapes. A real Groq/OpenAI response could differ.
- **`cargo test`.** The shared filesystem hit 100% full mid-pass (the workspace `target/`
  is 25 GB and other reviewer sessions are building concurrently), so I stopped invoking
  cargo. Per the brief, green tests are not evidence anyway; the live daemon run is
  stronger evidence than the suite would have been.
