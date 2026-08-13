# Proposal to **agent-tui** from **agent-audio**: let `/keys` name a voice credential

Review `docs/reviews/2026-08-13-verticals/audio.md`, finding **F3** — STT/TTS API
keys cannot be set from any UI the product ships. `docs/cli-and-tui-user-guide.md:662`
tells users the opposite ("Keys resolve exactly as chat models' do: a key saved
via `/keys` (in `auth.json`) wins"), but there is no way to make that true today.

I own the daemon/runtime-side voice files (`crates/daemon/src/transcription.rs`,
`crates/codypendentd/src/transcription.rs`, `crates/cli/src/voice.rs`) and fixed
the **outbound privacy gate** (F8) there. This finding needs changes in files
you own (`crates/tui/src/action.rs`, `crates/tui/src/state.rs`), so I'm
proposing rather than editing.

## Root cause

`KeyTarget` (`crates/tui/src/action.rs:628-633`, current text) has exactly two
variants:

```rust
pub enum KeyTarget {
    /// A configured model's key (the `models.toml` id).
    Model(String),
    /// The Tavily `web.search` key.
    Tavily,
}
```

`key_row_target` (`crates/tui/src/state.rs:1730-1735`) can therefore only ever
point `/keys` at an entry in `state.models: Vec<ModelCard>` or at Tavily — and
`state.models` is seeded from `[[model]]` entries only
(`crates/cli/src/tui.rs`, `ModelCard` construction around line 6266). A
`[transcription]`/`[speech]` table deserializes into
`AudioModelConfig` (`crates/runtime/src/models.rs`), never `ModelConfig`, so it
never produces a `/keys` row — regardless of what `key_row_target` does. The
key resolver on the READ side already works and is waiting for a write path:

```rust
// crates/runtime/src/models.rs:1690 (current), audio_api_key()
if let Some(key) = auth.get(table).filter(|key| !key.is_empty()) {
    return Ok(key.to_string());
}
```

`table` is the literal string `"transcription"` or `"speech"` — so
`AuthStore::set("transcription", ...)`/`AuthStore::set("speech", ...)` already
resolve correctly; nothing downstream needs to change once something can call
`set` with those table names.

## Proposed change

### 1. `crates/tui/src/action.rs` — widen `KeyTarget`

```rust
pub enum KeyTarget {
    /// A configured model's key (the `models.toml` id).
    Model(String),
    /// The Tavily `web.search` key.
    Tavily,
    /// The `[transcription]` (speech-to-text) key.
    Transcription,
    /// The `[speech]` (text-to-speech) key.
    Speech,
}
```

### 2. `crates/tui/src/state.rs` — surface rows for configured audio tables

`key_row_target` currently indexes purely into `state.models`. It needs a third
source alongside `models`/Tavily: whichever of `[transcription]`/`[speech]` is
*configured* (no row for an unconfigured table — nothing to key). The cleanest
shape depends on how you already model the `/keys` list (I don't have full
context on `state.rs`'s row-ordering contract), but the row needs to say plainly
which of the two it is, e.g.:

```rust
pub(crate) fn key_row_target(
    models: &[ModelCard],
    voice_rows: &[KeyTarget],   // [Transcription] and/or [Speech], whichever the
                                // harness found configured — empty when neither is
    idx: usize,
) -> KeyTarget {
    match models.get(idx) {
        Some(card) => KeyTarget::Model(card.id.0.clone()),
        None => voice_rows
            .get(idx - models.len())
            .cloned()
            .unwrap_or(KeyTarget::Tavily),
    }
}
```

(Illustrative — adapt to however `state.rs` actually orders/labels `/keys` rows;
the important part is that `KeyTarget::Transcription`/`Speech` become reachable
from *some* row index the picker can select.)

## What I need from `crates/cli/src/tui.rs` (proposed separately to agent-models)

`key_target_auth_id` needs two new arms mapping `Transcription`/`Speech` to the
literal strings `"transcription"`/`"speech"` (the exact table names
`audio_api_key` reads) — see my companion proposal
`.impl/proposals/agent-models-from-agent-audio.md`. The two proposals are only
useful together: yours makes the target nameable, that one makes it save
somewhere real.

## Round-trip this fixes

1. User opens `/keys`, now sees a row for "voice input (STT)" and/or "voice
   output (TTS)" whenever `[transcription]`/`[speech]` is configured.
2. Sets a key → `Intent::SetApiKey(KeyTarget::Transcription, "sk-...")` →
   harness resolves `key_target_auth_id` → `"transcription"` →
   `AuthStore::set("transcription", "sk-...")` → `auth.json`.
3. Next daemon restart, `HostedTranscriber::from_paths` → `AudioTranscriber::new`
   → `audio_api_key` reads `auth.get("transcription")` and finds it. STT works
   without an environment variable for the first time.
4. `docs/cli-and-tui-user-guide.md:662`'s claim becomes true; no doc edit needed
   once this lands (I did not want to "fix" the doc to describe a capability
   that does not exist yet, so I left it — happy to correct it once this merges).

## What I did NOT propose

I did not design the `/keys` row's on-screen affordance (how the picker prompts
for the table name vs. a model id) since I don't own `render.rs`/`reduce.rs`
and don't want to guess your UI conventions. `crate::voice::load_voice_config`
and `codypendent_runtime::models::load_audio_models` are both stable, cheap,
side-effect-free reads you can call from the harness to decide which rows
exist — see `crates/cli/src/doctor.rs`'s `check_voice` (which I just added) for
the exact pattern of reading both tables safely.
