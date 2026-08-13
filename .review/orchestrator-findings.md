# Orchestrator's own findings (pinned 535a2f5)

## F-ORCH-1 — `codypendent models add` silently destroys most of models.toml — class (c)

`crates/cli/src/commands.rs:2894-2905` (`models_add`)

`models.toml` is a multi-section file. `crates/runtime/src/models.rs` parses
`[[model]]`, `[embedding]` (l.133+), `[retrieval]` (l.177), `[transcription]`
and `[speech]` (l.1387-1444) out of it.

`models_add` serializes a locally-declared `struct ModelsToml { model: Vec<ModelConfig> }`
and atomically renames it over the file. Every other table is destroyed.

Reproduced at the pinned commit:

    $ cat /tmp/cpt1/models.toml     # [[model]] + [embedding] [retrieval] [transcription] [speech]
    $ CODYPENDENT_DATA_DIR=/tmp/cpt1 codypendent models add openai gpt-4o
    added model openai/gpt-4o (/tmp/cpt1/models.toml)
    $ cat /tmp/cpt1/models.toml     # ONLY [[model]] remains

User-visible consequence: adding a model turns off the user's embeddings
(degrading outcome 9 retrieval to keyword-only), resets retrieval tuning, and
disables both STT and TTS (outcome 8) — with a success message and no warning.

This is the exact hazard the other two writers of this file guard against, both
with comments explaining it:
  - `crates/cli/src/models_pull.rs:288-303` — edits the parsed document in place
  - `crates/cli/src/tui.rs:4255-4279`      — edits the parsed document in place
`commands.rs` was missed when the other two were fixed.

Secondary, same site: with no `--key-env` and no env var set, the entry is
written with `api_key_env = ""` (observed above) rather than the provider's
documented default or an error, so the model can never authenticate.

## F-ORCH-2 — long CODYPENDENT_DATA_DIR aborts offline commands — class (c), low

`crates/cli/src/main.rs:968` via `crates/protocol/src/discovery.rs:73`

`models add` is a pure local file edit but resolves the daemon socket path
first, so a data dir long enough to push `run/daemon.sock` past ~104 bytes
aborts the command. The error message is good and actionable; the defect is
that a command needing no daemon consults the socket at all.
Low severity — record, do not prioritise.

## F-ORCH-3 — the prefilled provider catalog has no enumeration command — class (b)

`crates/providers/builtin_catalog.toml` (2714 lines, 42 providers, 389 model rows)
`crates/providers/src/catalog.rs:10` loads it via `include_str!`.
`crates/cli/src/main.rs` — `models add <PROVIDER>` help text.

The catalog engine for outcome 3 is real and substantial. But the only CLI
consumer is `models add`, which requires the user to already know the provider
id. `models add --help` says:

    The catalog provider id (`codypendent models list-providers` spellings:
    `openai`, `nebius`, `azure-openai`, …)

`codypendent models list-providers` does not exist:

    $ codypendent models list-providers
    error: unrecognized subcommand 'list-providers'

`models list` lists only what is ALREADY in the user's models.toml, not what the
catalog offers. So there is no way from the CLI to discover any of the 42
providers or 389 models, and the help text sends the user to a command that was
never wired. Textbook class (b): engine built, documented in help, final wire
never attached.

## F-ORCH-4 — the models.toml clobber was found and fixed three times, missed once

Evidence for the synthesis. Three writers of `models.toml` carry comments
describing this precise bug:
  - `crates/cli/src/models_pull.rs:288`  "Serializing a struct that knows only
    about models would silently delete every one of them"
  - `crates/cli/src/acp_clients.rs:490`  "Replacing the whole file from a
    model-only struct silently erased those tables whenever an ACP client
    connected or disconnected."
  - `crates/cli/src/tui.rs:4254`         "retrieval and future top-level
    settings share this file and must survive adding a model from the TUI."
And `crates/cli/src/commands.rs:2894` does not — see F-ORCH-1.

The fix was applied per-site as each site was noticed, never as an invariant.
There is no single `write_models_toml` helper; four independent implementations
of the same atomic-write-with-merge exist. `crates/council/src/service.rs:1388`
is safe only incidentally (its `CouncilFile` models the entire schema).

## F-ORCH-5 — the code-graph query engine has no caller but its own test — class (b)

`crates/knowledge/src/codegraph.rs`:
  - `pub async fn callers_of`      l.576
  - `pub async fn blast_radius`    l.602
  - `pub async fn tests_covering`  l.618
  - `pub fn changed_between`       l.532

Their doc comments name them as the tools `graph.callers_of`,
`graph.blast_radius`, `graph.tests_covering` — i.e. they were designed to be
model-callable tools.

The ONLY callers in the entire workspace are in
`crates/knowledge/tests/semantic_it.rs:50,55,57,190`.

There is no tool with any of those names — the dispatched tool set in
`crates/runtime/src/agent.rs` is: artifact.read, blackboard.post/query,
council.create/result/run, docs.create/edit/read/suggest,
git.apply_patch/diff, github.*, memory.remember, repository.test,
task.create/list/move/update, web.search, workflow.create/query/run,
workspace.edit_file/read_file/search/write_file. No `graph.*` tool exists.
There is no protocol command, no CLI subcommand and no TUI surface for them
either.

User-visible consequence: outcome 5 requires the code-graph be "accessible to
user and agent". It is accessible to neither. A user cannot ask what calls a
function; the model cannot either. The engine is written, tested and
documented, and nothing reaches it.

`ROADMAP.md:222` marks 4.5 `[x]` and lists these four queries as delivered.
The functions exist, so the claim is not a fabrication — it is the class-(b)
blind spot: "implemented" was scored at the library boundary, not at the user.

## F-ORCH-6 — memory has a write tool and no read/delete tool — class (b), scoping outcome 17

The only memory tool is `memory.remember`. There is no `memory.recall`,
`memory.search`, `memory.forget` or `memory.list`. Outcome 17 requires memory
the user can inspect, edit and delete. Confirm against the memory-docs
vertical report before implementing.

## F-ORCH-7 — WITHDRAWN. The 0020/0021 migration gap is explained.

`docs/reviews/2026-08-11-wip-patches/README.md` reserved 0019 kanban, **0020
docs (only if needed)**, **0021 voice (only if needed)**, 0022 registry
embeddings. 0020 and 0021 were not needed. Not a defect. Numbers are still
assigned centrally in SHARED-SURFACE.md to avoid Phase 2 collisions.
