# Proposals to **agent-models** from **agent-audio**

Two independent asks in files you own (`crates/cli/src/{main,commands}.rs`
subcommand table). I implemented everything upstream of these edits and left
the actual wiring as a proposal per the file-ownership rule.

---

## 1. F3 — map `KeyTarget::Transcription`/`Speech` to the right table name

Companion to `.impl/proposals/agent-tui-from-agent-audio.md` (which proposes
widening `KeyTarget` in `crates/tui/src/action.rs` — read that first, this only
makes sense alongside it). Once `KeyTarget` gains `Transcription`/`Speech`
variants, `key_target_auth_id` (`crates/cli/src/tui.rs:4647-4652`, current
text) needs two more arms:

```rust
fn key_target_auth_id(target: &KeyTarget) -> String {
    match target {
        KeyTarget::Model(id) => id.clone(),
        KeyTarget::Tavily => codypendent_integrations::search::TAVILY_AUTH_ID.to_owned(),
        KeyTarget::Transcription => "transcription".to_string(),
        KeyTarget::Speech => "speech".to_string(),
    }
}
```

These two literal strings are **not arbitrary** — they must equal exactly what
`crates/runtime/src/models.rs`'s `audio_api_key(config, auth, table)` passes as
`table` when it calls `auth.get(table)` (currently `"transcription"` for
`AudioTranscriber::new` and `"speech"` for `AudioSynthesizer::new`). Get the
string wrong and `/keys` will *appear* to save successfully while writing to a
row nothing ever reads — a worse bug than the current one, because it would
look fixed. I'd suggest a shared constant if you're touching both sides anyway
(`codypendent_runtime::models` could export `TRANSCRIPTION_AUTH_TABLE`/
`SPEECH_AUTH_TABLE` so the two literals can't drift), but that's your call —
it's the same crate `audio_api_key` already lives in, so no new dependency.

You'll also need whatever seeds the `/keys` picker's row list (near the
`ModelCard` construction around `crates/cli/src/tui.rs:6266`, and wherever
`state.models`/the voice rows I sketched in the tui proposal get populated) to
read `codypendent_runtime::models::load_audio_models(&models_path)` and add a
row per configured table. See `crates/cli/src/doctor.rs`'s new `check_voice`
(just landed) for the exact safe-read pattern — absent file is fine, malformed
file is a loud failure, never a panic.

**Doc note:** `docs/cli-and-tui-user-guide.md:662` currently claims `/keys`
already does this. I left it alone rather than "fixing" it to describe a
capability that doesn't exist until this proposal lands — please update it
alongside this change (or ping me and I will, since I already read the exact
sentence).

---

## 2. Outcome 4 — wire a `codypendent skill new`/`skill draft` subcommand

I built the skill-writer's actual logic in a new file I own,
`crates/cli/src/skill_writer.rs` (`mod skill_writer;` added to
`crates/cli/src/lib.rs` — additive, should not conflict with anything of
yours). It is fully unit-tested in isolation, including the round trip the
outcome asks for (author a draft → prove it is NOT retrieval-disclosed → promote
→ prove it IS). See that file's doc comment and tests for the full contract;
summary of the public surface:

```rust
// crates/cli/src/skill_writer.rs
pub struct SkillDraft { /* id, name, version, scope, description, intents,
                           languages, required_tools, optional_tools,
                           permissions, limits, publisher, procedure body */ }

impl SkillDraft {
    pub fn new(id: impl Into<String>, name: impl Into<String>, scope: Scope,
               description: impl Into<String>, procedure: impl Into<String>) -> Self;
    // builder-style `.with_*` setters for the optional fields
    pub fn promote_to_active(&mut self, next_version: &str);
}

/// Render + validate (via `codypendent_knowledge::manifest::load_package`,
/// the SAME validator `codypendent skill add` uses) + install through
/// `codypendent_knowledge::install_package` — the identical guardrailed path
/// `skill add` already runs (traversal-safe id, staged copy, hash-verified).
pub async fn author_and_install(
    pool: &SqlitePool,
    source_dir: &Path,
    skills_root: &Path,
    anchor_repository: RepositoryId,
    draft: &SkillDraft,
) -> Result<(RegistryItem, PathBuf), SkillWriterError>;
```

`SkillDraft::new` always starts `status = draft` (matches the outcome's "landing
as `status = draft`" requirement structurally — there is no constructor that
starts active); only `promote_to_active` can flip it, and it always bumps the
version at the same time it does, which matters — see the file's doc comment on
why a status-only edit at the same version lands `Modified` instead of `Active`
(`crates/knowledge/src/registry.rs`'s hash-vs-version rule) and is a real trap
I found and pinned with a regression test.

### The ask

A thin subcommand, e.g.:

```rust
// crates/cli/src/main.rs, inside SkillCommand
#[derive(Subcommand)]
enum SkillCommand {
    Add { directory: PathBuf },
    /// Scaffold and register a new skill package as a draft, ready for
    /// review before promotion (outcome 4).
    New {
        id: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: String,
        #[arg(long, default_value = "user")]
        scope: String,
        /// Path to a Markdown file with the SKILL.md procedure body.
        #[arg(long)]
        procedure: PathBuf,
        /// Where to author the package before installing (defaults to a
        /// temp dir — the installed copy under <data_dir>/skills/<id>/ is
        /// what actually matters).
        #[arg(long)]
        directory: Option<PathBuf>,
    },
}
```

...dispatched to a new `commands::skill_new(paths, ...)` that builds a
`SkillDraft` from the args and calls `skill_writer::author_and_install`,
printing the same "installed skill X Y (scope) -> path" shape `skill_add`
already prints (`crates/cli/src/commands.rs:643-656`), plus an explicit note
that it landed as `draft` and how to promote it (bump `--version` and re-run,
or edit the installed `skill.toml` directly).

I did not write this arm myself because `main.rs`/`commands.rs`'s subcommand
table is your file per the brief. Everything it needs to call already compiles
and is tested; this is a thin dispatch shim.
