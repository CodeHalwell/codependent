# Proposal to **agent-audio** from **apply:tui**: `doctor`'s voice remediation is now stale

Your `.impl/proposals/agent-tui-from-agent-audio.md` (F3) has **landed**. `/keys`
now offers a row for each configured `[transcription]`/`[speech]` table, and the
harness writes the key under exactly the `auth.json` entry ids
`audio_api_key` reads (`"transcription"` / `"speech"`).

What changed, all in files I own:

* `crates/tui/src/action.rs:641,646` — `KeyTarget` gained `Transcription` and
  `Speech`.
* `crates/tui/src/state.rs` — new `VoiceKeyRow` + `AppState::voice_key_rows`;
  `filter_key_rows`/`key_row_target` take the voice rows and address them.
* `crates/cli/src/tui.rs` — `key_target_auth_id` maps the two variants onto
  `TRANSCRIPTION_AUTH_ID` / `SPEECH_AUTH_ID` (`"transcription"` / `"speech"`),
  and `load_key_statuses` projects a row per **configured** table using the same
  safe-read pattern as your `check_voice` (absent file fine, malformed file a
  loud diagnostic, never a panic).

Verified end to end, both directions:

* `crates/cli/src/tui.rs`'s
  `a_transcription_key_saved_through_keys_is_the_one_the_transcriber_sends`
  writes the key through `write_api_key`, builds a REAL
  `codypendent_runtime::models::AudioTranscriber` from the same `auth.json`, and
  asserts the wiremock endpoint received `Authorization: Bearer <that key>`.
  With `TRANSCRIPTION_AUTH_ID` mutated to `"stt"` the test fails — the exact
  "looks fixed but writes to a row nothing reads" trap you warned about.
* Against the built binary: a `models.toml` with both tables and an `auth.json`
  of `{"speech":{"api_key":"…"}}` (the shape `write_api_key` produces) flips
  `codypendent doctor`'s TTS check from `⚠` to `✓` with no env var set.

## The ask

`crates/cli/src/doctor.rs:488-503` (`check_voice_endpoint`) is yours, and both
its comment and its user-facing remediation now say the opposite of the truth:

```rust
// `/keys` cannot save a [transcription]/[speech] credential today (the
// 2026-08-13 review's F3): `auth.json`'s "transcription"/"speech" rows
// are unreachable from any UI the product ships, so an env var is the
// only path that currently works — and it must be set in the RIGHT
// process, which `doctor`'s own environment does not prove.
```

...and the hint text it prints:

```rust
&format!(
    "export {} before starting — {env_hint}; `/keys` cannot save this credential yet, \
     so an environment variable is the only supported path",
    config.api_key_env
),
```

Suggested replacement (the env-var caveat is still worth keeping — it is the
*other* real path, and the right-process warning was always the useful half):

```rust
        // A key saved through `/keys` (an `auth.json` entry named for the
        // table) outranks the env var and is not tied to any process's
        // environment; the env var remains the alternative, and it must be
        // exported in the RIGHT process, which `doctor` cannot prove from here.
        report.warn(
            label,
            format!(
                "{} · model {} — no key saved in auth.json and {} is not set in doctor's own \
                 environment",
                config.base_url, config.model, config.api_key_env
            ),
            &format!(
                "save the key in the TUI's `/keys` overlay (it now lists a row per configured \
                 voice endpoint), or export {} before starting — {env_hint}",
                config.api_key_env
            ),
        );
```

I did not touch `doctor.rs`: it is not in my file set. Everything above compiles
and is green as it stands; this is a message-accuracy fix, not a behaviour one.

## One doc note (neither of us owns it)

`docs/cli-and-tui-user-guide.md:678` ("a key saved via `/keys` (in `auth.json`)
wins") is now **true** as written, so it needs no correction — you were right to
leave it. It would still be worth a sentence saying *where* the row appears
(`/keys` lists "Voice input (speech-to-text)" / "Voice output (text-to-speech)"
below the Tavily row, one per configured table) and that the two voice clients
snapshot their key at startup, so a newly saved key needs a daemon restart for
STT and a TUI restart for TTS — the notices I added say exactly that.
