# Vertical: audio (outcome 8 — built-in TTS + STT for immersive interaction)

Round 4, pinned commit `c255bec8b175d62942b3312cff2335b97d43a59a` (v0.5.1, branch
`claude/review-repair-twenty-outcomes-5fynno`). Every file in the vertical read in full.
The real `codypendentd` and the real TUI were driven live against stub HTTP providers;
SQLite was queried afterwards. No code was modified.

---

## Verdict

**OUTCOME 8: PARTIAL.** Both directions of the pipeline genuinely work end-to-end against a
configured endpoint — I proved each one live, including the privacy gate in both directions
and the trust-boundary read. Six of the prior round's twelve findings are really fixed, and
I verified each fix by running it, not by reading it.

But the word under test is **built-in**, and on that the answer is unchanged and
unambiguous: **there are zero audio crates in the workspace.** Nothing captures from a
microphone, nothing decodes, nothing plays to a speaker, and no model runs on the device.
Every one of the four audio primitives is delegated to something Codypendent does not ship.

Layered on top of that, the round-4 repairs introduced no new leak but left the feature
**mute**: the status-line notice that carries every voice error — including the new privacy
refusal — is structurally unreachable at the exact moment those errors occur. I proved this
live. It falsifies two explicit promises in the shipped user guide.

---

## 1. The dependency question, computed here

Not repeated from the prior report — recomputed.

```
$ find . -name Cargo.toml -not -path "./target/*" | wc -l
19

$ find . -name Cargo.toml -not -path "./target/*" -exec grep -niE \
  "^(cpal|rodio|symphonia|hound|whisper|whisper-rs|portaudio|alsa|libpulse|coreaudio|dasp|\
kira|awedio|tts|piper|espeak|sherpa|vosk|silero|opus|audiopus|vorbis|minimp3|claxon|lewton|\
creek|oddio|fon|rubato|webrtc-vad)\b" {} +
(exit 1)          # no matches in any of the 19 manifests

$ grep -cE '^name = ' Cargo.lock
559
$ grep -E '^name = ' Cargo.lock | grep -iE "cpal|rodio|symphonia|hound|whisper|portaudio|\
alsa|pulse|coreaudio|dasp|kira|awedio|^tts|piper|espeak|sherpa|vosk|silero|opus|vorbis|\
minimp3|audio|sound|voice|speech"
piper
```

The single hit, `piper`, is **not** the Piper TTS engine. `Cargo.lock` shows `piper 0.2.5`
with dependencies `atomic-waker`, `fastrand`, … and the reverse lookup gives its only
dependent as `blocking` — it is the async-pipe utility crate. Confirmed.

**Zero audio crates. This is not a feature-gate that is switched off; there is no audio code
to gate.** The only audio-adjacent line in any manifest is a comment at
`crates/runtime/Cargo.toml:60` about wiremock standing in for a provider.

`crates/integrations/**` — which my brief named as a place audio might live — contains **no
audio, TTS or STT path whatsoever**. Its 31 source files mention "transcript" only in the
conversation sense, plus two comments noting that ACP and MCP *audio content blocks are
dropped* (`crates/integrations/src/acp_client.rs:171-173`,
`crates/integrations/src/mcp/client.rs:348`).

### What actually provides each capability

| Capability | Provided by | Ships with the product? |
|---|---|---|
| Microphone capture | `rec` / `arecord` / `ffmpeg` subprocess (`crates/cli/src/voice.rs:180-230`) | **No** |
| Speech → text | HTTP POST `{base_url}/audio/transcriptions` (`crates/runtime/src/models.rs:1792`) | **No** |
| Text → speech | HTTP POST `{base_url}/audio/speech` (`crates/runtime/src/models.rs:1925`) | **No** |
| Audio playback | user-named subprocess fed on stdin (`crates/runtime/src/models.rs:2027`) | **No** |

The code is candid about this — `voice.rs:12` states "There is **no bundled recorder**",
`models.rs:1987` states "There is no bundled audio backend on purpose". Those are defensible
engineering choices. They are not the outcome as stated.

### Can a user speak into a microphone with no external tooling? No.

Plainly: **they must install a recorder binary, configure a remote endpoint, supply a
provider API key, and name a playback binary.** On a fresh machine none of outcome 8 is
reachable. The user guide is honest about this in a way the outcome statement is not
(`docs/cli-and-tui-user-guide.md:632-635`): *"Turning it on takes two things Codypendent
deliberately does not ship: a recorder binary already on your machine, and an API key for a
speech provider."*

There is also no headless surface. `codypendent voice` → `error: unrecognized subcommand
'voice'`, **exit 2**. Voice exists only inside the interactive TUI event loop.

---

## 2. What I ran

No audio device exists in this container — `/dev/snd` absent, `/proc/asound/cards` absent,
and none of `rec`/`arecord`/`ffmpeg`/`mpv`/`sox`/`ffplay`/`aplay` on `$PATH`. That absence
is itself the test the brief asked for.

| # | Action | Result |
|---|---|---|
| 1 | `codypendent doctor` (empty data dir) | `✓ voice: not configured …` — legible, no crash |
| 2 | `codypendent models add openai gpt-4o-mini` over a 5-table models.toml | **all tables survived** (prior F1 fixed) |
| 3 | `codypendent doctor` (full voice config, no recorder) | correct STT/TTS/recorder rows; **but a false ✓ for playback** |
| 4 | Real daemon + stub `/v1/audio/transcriptions` + raw socket client | STT worked: 366-byte multipart POST → transcript → run objective → ledger note |
| 5 | Same, ceiling `Internal` | `voice.off-device-forbidden`; **stub never contacted** |
| 6 | Upload `Secret` audio, resubmit the ref claiming `Public` | refused, message names **`Secret`** — trust-boundary read holds |
| 7 | Image-only / editor-selection-only envelopes, empty text | **accepted** with honest descriptions (prior F4 fixed) |
| 8 | Real TUI in a pty: `/keys` | lists both voice endpoint rows (prior F3 fixed) |
| 9 | Real TUI: save a key through `/keys` | `auth.json` → daemon → `Authorization: Bearer sk-review-transcription-key` |
| 10 | Real TUI: F4 with no recorder, **before** any run | actionable notice rendered |
| 11 | Real TUI: full turn with "speak replies" on | reply POSTed to `/v1/audio/speech`, clip piped to `play_command` |
| 12 | Same, ceiling `Internal` | **no `/audio/speech` request** — outbound gate works |
| 13 | Same, but the refusal notice | **never rendered** |
| 14 | F4 **after** a completed run, same process | **nothing rendered** |
| 15 | `local = true` on a non-loopback endpoint, ceiling `Public` | **audio shipped off-device anyway** |

### 2.1 The remote STT path, proven

Stub provider on `127.0.0.1:8732`; `[transcription]` pointed at it; no `routing.toml`.

Daemon startup log:

```
INFO codypendent_codypendentd::transcription: voice input enabled (speech-to-text)
     model=whisper-large-v3-turbo local=false ceiling=Confidential
```

Socket client (`PutArtifact` → `SubmitUserInput` with an audio envelope and **empty** text):

```
HELLO -> ServerHello 0.5.1
PUTARTIFACT -> ArtifactStored {"artifact":{"id":"019ffd51-4af3-…","media_type":"audio/wav",
                "byte_length":64,"sensitivity":{"type":"Confidential"}}}
SUBMIT -> CommandAccepted {"created_run":"019ffd51-4afd-…"}
  EVENT: {"type":"NoteAppended","text":"transcribed 4.0 s of audio (model whisper-large-v3-turbo)"}
```

What the stub actually received:

```
path        : /v1/audio/transcriptions
content-type: multipart/form-data; boundary=codypendentaudio127c8c3c3b1f98d7a6b3975d
body_len    : 366
excerpt     : '--codypendentaudio127c8c…\r\nContent-Disposition: form-data; name="file";
               filename="voice.wav"\r\nContent-Type: audio/wav\r\n\r\nRIFF8\x00\x00\x00WAVE…'
```

SQLite afterwards:

```
runs:     objective = 'fix the flaky retry test' | Failed
commands: envelope.blocks[0].transcript = {"text":"fix the flaky retry test",
            "mode":{"type":"remote"},"model":"whisper-large-v3-turbo","reviewed":false,
            "source_audio":"019ffd51-4af3-…"}
          envelope.blocks[0].original.id = "019ffd51-4af3-…"   ← preserved, untouched
```

(`Failed` is the stub chat model, not the voice path.) **The original is preserved, the
transcript is added and linked back by `source_audio`. This half works.**

### 2.2 The full TTS round trip, proven

Stub also serving `/v1/models` and `/v1/chat/completions`, so a real assistant turn is
produced. Real TUI in a pty, inside a git repo, "Voice: speak replies" toggled on:

```
$ python3 tui_drive.py …/dd4 ENTER "/" "Voice: speak" ENTER "find the auth bug" ENTER

screen: ⏺ codypendent · stub-chat 22:59
        ▌ Patched crates/auth/src/secrets.rs line 88; the API key was logged in plaintext.
        ✓ Completed

stub:   /v1/chat/completions | len 10397
        /v1/audio/speech     | len 124 |
          {"model":"tts-1","input":"Patched crates/auth/src/secrets.rs line 88;
            the API key was logged in plaintext.","voice":"alloy"}
        /v1/chat/completions | len 4627

$ ls -la played.bin
-rw-r--r-- 1 root root 144 …
$ head -c 60 played.bin
ID3-FAKE-MP3-BYTESID3-FAKE-MP3-BYTESID3-FAKE-MP3-BYTESID3-FA
```

The assistant reply — naming a source file and describing a credential-logging vulnerability
— was POSTed verbatim to a third-party endpoint, and the returned bytes reached the player.
**This half works too.** Note it happened under the *default* posture (no `routing.toml`).

---

## 3. Prior-round repairs: verified fixed

Recorded so the implementation phase does not undo them. Each was verified by running it.

**F1 — `models add` no longer destroys the voice tables.** There is now one writer,
`crates/cli/src/models_file.rs`, whose module doc names the invariant ("edit the parsed
document in place; never serialize the file from a struct that models only one section").
Live: a `models.toml` carrying `[[model]]`, `[transcription]`, `[speech]`, `[voice]`,
`[embedding]` still carried all five afterwards. **This was a shipping data-loss bug; it is
gone, and it is gone at the class level rather than the instance.**

**F3 — voice keys are settable from the UI, and the key actually arrives.** `KeyTarget`
gained `Transcription`/`Speech` (`crates/tui/src/action.rs:636-646`), seeded as `VoiceKeyRow`s
(`crates/tui/src/state.rs:1796`, `crates/cli/src/tui.rs:4942`). Driven live:

```
/keys shows:  ○ Voice input (speech-to-text)
                whisper-large-v3-turbo · 127.0.0.1:8731 · no key configured
              ○ Voice output (text-to-speech)
                tts-1 · 127.0.0.1:8731 · no key configured

after saving: auth.json = {"transcription": {"api_key": "sk-review-transcription-key"}}
status line:  "voice input key saved — restart the daemon to use it"
after restart: stub received  authorization: Bearer sk-review-transcription-key
```

The full chain works, and the restart requirement is disclosed at the moment it matters.

**F4 — every named block kind now contributes text.** `envelope_text`
(`crates/daemon/src/transcription.rs:286-318`) handles all seven variants. Live, with empty
text: image-only → `CommandAccepted`, objective `[attached image: image/png, 1280x720 — this
build has no image-reading pipeline, so its contents are not visible here]`;
editor-selection-only → `CommandAccepted`, objective `[editor selection:
crates/workflow/src/drive.rs lines 12-34 (0-based) — contents not included]`. The
descriptions name what was attached and explicitly disclaim what is not included.

**F6 — `doctor` has voice checks**, and they are good (`crates/cli/src/doctor.rs:378-506`).
The key-resolution row even names *which process's* environment must export the variable —
the daemon's for STT, the TUI's for TTS. That was the subtlest trap in the prior report and
it is now the diagnostic's headline. (One gap remains — see B4.)

**F8 — the outbound privacy gate exists and really gates.** `start_speech_worker`
(`crates/cli/src/voice.rs:668-708`) runs the *same* `transcription_allowed` the daemon runs
inbound, against the *same* `routing.toml` ceiling, re-read before every utterance. Live,
with `max_off_device = Internal`: the identical driving sequence that produced a
`/v1/audio/speech` request under the default posture produced **none**, and `played.bin` was
never written. The gate is real, not a wire that always allows.

**The trust-boundary read still holds** (`crates/daemon/src/transcription.rs:168-190`). Live:
audio stored as `Secret`, resubmitted with the ref rewritten to `Public` —

```
A. stored sensitivity : {'type': 'Secret'}
A. claimed Public -> CommandRejected voice.off-device-forbidden |
   audio may not be transcribed off-device: data classified Secret may not be processed
   off-device (policy allows up to Internal)
```

The daemon re-derived `Secret` from its own row and named it in the refusal. This is the
pattern the brief asks reviewers to hunt for, done right.

---

## 4. Findings

Ranked by user-visible consequence.

### A1 — Every voice error message is invisible once the session has run anything
`crates/tui/src/render.rs:2747` · **class (c) — wire attached, wrong behaviour**

The status-line notice is rendered only under this precondition:

```rust
// crates/tui/src/render.rs:2747
} else if status.pending_approvals == 0 && status.run_state.is_none() && state.notice.is_some()
```

`status.run_state` is `Some(state)` whenever a run is selected, including a **terminal**
one — `run_state: run.map(|r| r.state)` at `crates/tui/src/state.rs:2952`. So once a session
has run anything at all, `run_state.is_none()` is false and `state.notice` is never drawn
again.

Every voice message uses `Action::Notice`, which only sets `state.notice`
(`crates/tui/src/reduce.rs:138`). That includes:

* the speech worker's failures, drained at `crates/cli/src/tui.rs:1935` — **privacy
  refusals**, synthesis failures, playback failures;
* the capture failures at `crates/cli/src/tui.rs:1864` ("no recorder found …");
* `voice note sent — transcribing…` and `voice upload rejected: …`
  (`crates/cli/src/tui.rs:1426`, `:1497`).

**The timing makes this structural, not incidental.** A speech error can only ever arise
*after* a run reaches a terminal state — that is precisely when the host flushes the turn and
calls `speak()` (`crates/cli/src/voice.rs:514-527`). So the one class of error the round-4
repair was written to surface is the one class that can never be surfaced.

Proven live, one TUI process, F4 pressed twice — once before any run, once after a completed
run:

```
$ TRAIL=20 python3 tui_drive.py …/dd6 ENTER F4 "find the auth bug" ENTER F4
$ grep -ao "no recorder found" tui_ba.txt | wc -l
1
$ grep -ao "Completed" tui_ba.txt | head -1
Completed
```

Two presses, one message. And for the speech worker, zero:

```
# restrictive ceiling — gate refused, nothing sent:
$ grep -c "spoken" tui_gated2.txt
0
# permissive ceiling, play_command = ["definitely-not-a-real-player-binary"]:
#   /v1/audio/speech WAS hit, so playback was reached and must have failed
$ grep -c "playback" tui_playfail.txt
0
```

I ruled out the obvious alternatives before concluding. The *sibling* notice on the next
lines of the same block (`crates/cli/src/tui.rs:1938-1941`) renders correctly when no run
exists — proving the block executes, the reducer works, and my capture finds notices:

```
$ python3 tui_drive.py …/dd5 ENTER "/" "Voice: speak" ENTER
voice: add a [speech] entry to models.toml to hear replies
```

The redraw gate is also not the cause: `crates/cli/src/tui.rs:2041-2045` forces a redraw on
`state.notice != notice_before`.

**User-visible consequence.** Mid-session, a dead microphone does nothing at all. A reply
gagged by the privacy policy is indistinguishable from a reply the provider was slow to
return. A missing player is indistinguishable from silence. This directly falsifies two
promises in the shipped guide:

* `docs/cli-and-tui-user-guide.md:708-710` — *"If none is found, pressing the key tells you
  so and names what to install — it **never silently does nothing**."*
* `docs/cli-and-tui-user-guide.md:756-758` — *"otherwise a ceiling below Confidential
  silences that reply rather than sending it, and the status line reports why (`voice: reply
  not spoken — …`) **instead of going quiet with no explanation**."*

and the module's own doc, `crates/cli/src/voice.rs:52-54` — *"A refusal never reaches the
network: it is reported through `speech_error` … so a gagged reply is legible, not silently
swallowed"* — and `crates/cli/src/voice.rs:87-88` — *"a speech failure is reported through the
status line instead of being swallowed."*

Note this is a shared surface, so it will affect other verticals' notices too; but for voice
it is total, because voice's errors arrive only in the state where the notice is suppressed.

### A2 — `local = true` is an unverified assertion that disables the entire privacy gate, and `doctor` blesses it
`crates/runtime/src/models.rs:1786` · `crates/cli/src/doctor.rs:476-480` · **class (c)**

`AudioTranscriber::is_local` returns `self.config.local` and nothing else. Nothing anywhere
checks that the endpoint is actually on this device. `TranscriptionMode::Local` short-circuits
the gate unconditionally (`crates/protocol/src/input.rs:352`).

Proven live with the **tightest possible ceiling** and a **non-loopback** address:

```
models.toml : [transcription] base_url = "http://192.0.2.2:8733/v1"   local = true
routing.toml: max_off_device = { type = "Public" }

daemon: voice input enabled (speech-to-text) model=whisper-large-v3-turbo local=true ceiling=Public
client: SUBMIT -> CommandAccepted
stub on 192.0.2.2:  SENT TO: /v1/audio/transcriptions | bytes 366
```

The operator asked for `Public` — the strictest setting the product offers — and Confidential
audio left the machine.

`doctor` is the one place this could be caught, and it already owns the helper. It reports:

```
$ codypendent doctor      # [transcription] base_url = "https://api.groq.com/openai/v1", local = true
  ✓ voice input (STT): https://api.groq.com/openai/v1 · model whisper-large-v3-turbo · local
```

A green tick, and the word "local", for `api.groq.com`. `check_voice_endpoint`
(`crates/cli/src/doctor.rs:476-480`) derives `locality` from `config.local` alone —
while `is_local_url()` sits unused for this purpose eleven lines below at
`crates/cli/src/doctor.rs:509-514`, where it is applied to *chat* models. Cross-checking the
two is a three-line warning and closes the only silent bypass of the privacy posture.

### A3 — "Built-in" is absent; every audio primitive is delegated
`Cargo.toml` (×19, no audio deps) · `crates/cli/src/voice.rs:180-230` · `crates/runtime/src/models.rs:1987` · **class (a) — engine missing entirely**

See §1. Four delegations, none shipped. On a fresh machine with no sox/ffmpeg, no player, and
no provider key, nothing in outcome 8 is reachable — and, per A1, after the first run the
product will not even say so.

### A4 — "Speak replies" has no persistent indicator, while "recording" does
`crates/tui/src/render.rs:2717` vs. `speak_replies` (rendered nowhere) · **class (b)**

`state.voice.recording` gets a prominent, unconditional status-line branch — it is the
*first* branch, ahead of run state, with the comment at `crates/tui/src/state.rs:2490-2491`:
*"Rendered as a prominent status-line indicator: a hot microphone must never be invisible."*
That is exactly right, and it survives A1.

`state.voice.speak_replies` is rendered **nowhere**:

```
$ grep -rn "speak_replies" crates/tui/src/render.rs crates/tui/src/accessible.rs
(no output)
```

Its only uses are the palette toggle (`crates/tui/src/reduce.rs:6302`), the flush gate
(`crates/cli/src/tui.rs:1390`), and the auto-off check (`crates/cli/src/tui.rs:1938`). The
sole feedback is the transient "speaking replies aloud" notice — which, by A1, is itself
invisible after the first run.

**Consequence:** the outbound direction, which POSTs the full text of every assistant reply
(source, diffs, command output) to a third-party endpoint, has *no* on-screen indication that
it is on. The inbound direction, which is gated identically, gets a prominent one. The
asymmetry is backwards: a hot microphone is at least local until the user stops it; a hot TTS
toggle is exfiltrating on every turn. I confirmed this live — during the successful round
trip in §2.2 the only voice text on screen was the palette entry itself.

### A5 — `doctor` reports a green ✓ for a playback command that does not exist
`crates/cli/src/doctor.rs:447-461` · **class (b)**

The recorder check does a real `$PATH` probe, reusing the TUI's own `select_recorder` so the
two can never disagree (`crates/cli/src/doctor.rs:432-446`). The playback check, twelve lines
later, only tests `play_command.is_empty()`. Live, with **no `mpv` on `$PATH`**:

```
  ⚠ voice recorder: no recorder found on $PATH and no voice.record_command set
  ✓ voice playback: play_command: mpv --no-terminal -
```

The one row that could have warned the user is the one that does not check. Combined with A1,
the resulting playback failure is then invisible at runtime too — so a user whose replies are
never spoken has no diagnostic anywhere.

### A6 — The `--accessible` client cannot use voice at all, and is never told
`crates/cli/src/tui.rs:1826` vs. `:1874` · `crates/tui/src/accessible.rs` · **class (b)**

Push-to-talk is matched only on `ClientInput::Terminal(event)`
(`crates/cli/src/tui.rs:1826`). The accessible client's input arrives as
`ClientInput::AccessibleLine(line)` (`crates/cli/src/tui.rs:1874`) and never reaches
`voice.is_push_to_talk`. `crates/tui/src/accessible.rs` contains **zero** occurrences of
voice/speech/record. Live, the accessible client's own control listing never mentions it:

```
Controls: type TEXT filters, up/down/pageup/pagedown/home/end select, Enter chooses,
          Delete removes a saved model/key, Esc closes
```

For an outcome about *immersive interaction*, the client a screen-reader user runs is the one
that cannot speak or listen — and it does not say so.

### A7 — `audio_capture` is still declared, serialized, golden-vectored, and always false
`crates/protocol/src/capabilities.rs:23` · `crates/council/src/connection.rs:74` · **class (b) — data produced, never consumed**

Unchanged from the prior round. Every Rust construction site hardcodes `false`
(`crates/ui-host/src/runtime.rs:2710`, `crates/ui-host/src/session.rs:356`,
`crates/daemon/src/remote_ui.rs:2304`, `crates/tui/src/remote_ui_host.rs:311`), and the
shared handshake used by every Rust client sends `ClientCapabilities::default()`
(`crates/council/src/connection.rs:74`) — so the TUI, the *only* client with push-to-talk,
advertises that it cannot capture audio. Nothing in Rust ever reads it:

```
$ grep -rn "\.audio_capture" crates/ --include=*.rs
(no output)
```

The TypeScript SDK does read it (`sdk/ui/src/capabilities.ts:7,107`), which makes this worse
rather than better: a UI plugin can negotiate on a field the Rust daemon never sets truthfully.

### A8 — No CLI or scriptable surface
`crates/cli/src/main.rs` (no `Voice` subcommand) · `crates/cli/src/tui.rs:1344`

```
$ codypendent voice
error: unrecognized subcommand 'voice'
$ echo $?
2
```

`VoiceHost` is constructed only inside the interactive TUI event loop. There is no way to
transcribe a file, script a voice interaction, or exercise any of this from CI. Not a defect
against the outcome as literally worded, but it means the only testable surface is a raw-mode
TUI — which is why A1 survived a round of repair with tests green.

### A9 — `InputEnvelope::linked_artifacts()` still has no production consumer
`crates/protocol/src/input.rs:61-82` · **class (b)** — minor

Renamed from `artifact_ids()` since the last round; still called by nothing outside
`input.rs`'s own tests. `grep -rn "linked_artifacts" --include=*.rs crates/` returns only
`input.rs`.

### A10 — Context, not a defect: the gate defaults to permit
`crates/routing/src/policy.rs:217` · `crates/cli/src/voice.rs:601-603`

Worth stating plainly because it shapes what "gated" means here. Speech text is classified
`Confidential` (`speech_classification()`), the default ceiling is `Confidential`
(`RoutingPolicy::balanced()`), and `allowed_off_device` is `rank() <= rank()` — so out of the
box **the gate passes and everything is sent**. My §2.2 round trip ran under exactly that
posture. This is the same behaviour as STT, which is the right call (one posture, not two),
and it *is* disclosed for STT in the guide (`docs/cli-and-tui-user-guide.md:741-743`). It is
less explicit for TTS, and given A4 (no indicator) and A1 (no notice) a user has no runtime
signal at all. I record it as context rather than a finding because the behaviour is
documented and consistent.

---

## 5. The pattern

Every finding here is a **reporting** failure sitting on top of machinery that works.

The round-4 repairs were scored at the boundary where the value is *produced*: the gate
returns `Err`, the error string is formatted, the `Arc<Mutex<Option<String>>>` is populated,
the `Action::Notice` is reduced, `state.notice` is set. Six separate seams, each correct,
each unit-tested. Nobody followed the value one step further, to the render precondition that
throws it away (A1). The same shape recurs everywhere in this vertical: `audio_capture` is
produced into the wire and never read (A7); `linked_artifacts()` is produced and never
consumed (A9); `speak_replies` is produced into state and never rendered (A4);
`doctor` produces a locality label from a flag it never validates against the URL sitting
next to it (A2, A5). The prior round's diagnosis — *"done is scored at the library boundary"*
— is exactly right, and the repair inherited it: the fix for "the refusal was silent" was
tested at the mutex, not at the pixel.

Underneath all of it, the outcome word **built-in** remains false in a way no amount of
wiring will change. What exists is a competent, honestly-documented *client* for somebody
else's speech services, driven by somebody else's recorder and player. The pipeline is real
— I drove both directions of it end to end — but four separate things the user must install
or subscribe to stand between them and a single spoken word.

---

## 6. What I did not verify

* **Any real microphone or speaker.** No audio device exists here (`/dev/snd` absent, no
  recorder or player binary on `$PATH`). Everything above the device boundary is testable
  without one and I tested it; whether `rec -q -r 16000 -c 1 -b 16 file.wav` or `ffmpeg -f
  alsa -i default` actually capture usable audio, and whether SIGINT finalizes the WAV header
  in practice, remain unverified — exactly as the authors already warn at
  `docs/cli-and-tui-user-guide.md:637-643`. **Exercise limitation, not a finding.**
* **A real provider.** No credentials. Every provider interaction used my own stub answering
  the documented shapes. A real Groq/OpenAI response could differ.
* **The root cause of A1 is observed, not fully traced.** I proved the *consequence*
  exhaustively (14 live runs, an A/B against the sibling notice, and a before/after F4 in one
  process) and I identified the precondition at `render.rs:2747` by reading. I did not
  instrument the binary to confirm that `take_speech_error()` returns `Some` at that moment —
  I inferred it from the fact that the gate demonstrably fired (no HTTP request was made) and
  the code path between the gate and the mutex is straight-line. If that inference is wrong,
  the consequence is unchanged and the fix location merely moves.
* **A4's consequence for a sighted user watching closely.** I confirmed `speak_replies` is
  rendered nowhere by grep and by inspecting the captured frames; I did not exhaustively
  enumerate every overlay that might mention it.
* **`cargo test`.** Not run, per the brief's disk constraints and because green tests are not
  evidence here. Every claim above rests on a live run of the shipped binaries; where a claim
  rests on reading, I have marked it.
* **Other verticals' exposure to A1.** `render.rs:2747` is a shared surface and almost
  certainly suppresses non-voice notices too, but I scoped my testing to voice.
