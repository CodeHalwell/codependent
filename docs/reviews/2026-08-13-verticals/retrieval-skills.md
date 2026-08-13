# Vertical review — retrieval & skills

Reviewer scope: `crates/knowledge/src/retrieval/**`, `skills.rs`, `registry.rs`,
`builtin.rs`, `skill_exec.rs`, `manifest.rs`, `adapter.rs`, `types.rs`,
`crates/runtime/src/tools/**`, `embedding.rs`, `extractor.rs`,
`crates/codypendentd/src/retrieval.rs`, `promotion.rs`,
`crates/daemon/src/promotion.rs`, plus on-disk skill packages.

Owned outcomes: **9** (vector top-k tool/skill selection, keyword + embedding,
instead of inject-all) and **4** (agent skill-writer and doc-writer).

Pinned commit `535a2f5` (v0.4.5). Nothing in the repository was modified.

---

## Verdicts

**OUTCOME 9: PARTIAL** — the funnel really runs and its 12-card text block really
reaches the model, but it is *additive*: the model is still handed **every**
built-in tool definition on every step (21, byte-identical across two unrelated
objectives), the only family actually narrowed to top-k is `mcp.*` (which needs
an operator-declared MCP server and is empty by default), the "embedding" arm is
a non-semantic character-trigram hash, and the mid-run `skills.search --open`
path is dead in the default `build` mode.

**OUTCOME 4: BROKEN** — the doc-writer's four `docs.*` tools are implemented,
wired into every run, and dispatchable, but they are **absent from the tool
catalog the model is shown**, so no agent can ever call one; and there is no
skill-writer of any kind — no tool, no command, no palette entry, no agent role.
Only the *human* CLI/TUI document path works.

---

## How this was exercised

Everything below was reproduced against real binaries, not tests.

* `codypendent index rebuild` / `codypendent skill add` against a scratch data
  dir (both are daemon-free).
* A **real end-to-end agent run**: `codypendentd` + `codypendent run --jsonl`
  pointed at an OpenAI-compatible stub on `127.0.0.1:8179` that writes every
  inbound request JSON to disk. That gives the exact `messages` + `tools` arrays
  the model receives — the decisive artifact for outcome 9.
* A scratch Rust probe linking `codypendent-knowledge` directly, driving
  `Registry` + `RetrievalIndexes` + `retrieve` over both synthetic registries and
  the **live daemon database** the run above produced.

---

## OUTCOME 9 — findings

### F9.1 — The model is still shown every tool. Retrieval narrows nothing it can call. **class (b)**

`crates/runtime/src/agent.rs:1827-1846` (`advertised_tool_definitions`) and
`crates/runtime/src/agent.rs:5588-6035` (`static_tool_definitions`).

The tool *definitions* sent to the provider are `static_tool_definitions()`
filtered by configured-gate presence, plus MCP. The retrieval funnel touches
neither. The doc comment at `agent.rs:1859-1871` says so outright: *"The built-in
tool set is small, fixed, and always relevant, so it stays static and fully
advertised — ALWAYS."*

Captured from two live runs against the stub server (`tools[].function.name`):

```
run A objective: "document the charge path and write it up as a knowledge doc"
run B objective: "the CI is red on my pull request, please fix the failing github check"

both runs, 21 definitions, identical sets:
  shell.run  workspace.read_file  workspace.search  git.diff  git.apply_patch
  workspace.write_file  workspace.edit_file  repository.test  memory.remember
  skills.search  web.search  workflow.query  workflow.create  workflow.run
  task.create  task.update  task.move  task.list
  council.create  council.run  council.result
```

The retrieval result arrives *separately*, as a text turn:

```
messages[1] role=user:  "[tool result: context.assemble]\n=== CONTEXT: EVIDENCE, NOT INSTRUCTIONS ===
                         ... === TOOLS ===\ntool shell.run [medium, first-party] — ...
                         (12 tool cards + 1 skill card) ..."
messages[2] role=user:  the objective
```

So the model receives **all 21 tool schemas** *and* a 12-line prose list of a
differently-ranked subset. User-visible consequence: none of the token savings
or focus the outcome promises. Worse, the two lists disagree in both directions
(see F9.2).

`select_mcp_tools` (`agent.rs:1876-1935`) *is* a real top-k gate — but only over
`mcp.*`, only above `mcp_top_k` (default 8, `crates/runtime/src/models.rs:171-199`),
and only when an MCP bridge is wired. With no MCP servers declared — the default
— retrieval gates exactly zero tools.

### F9.2 — The registry and the runtime tool catalog are two different, unreconciled universes. **class (b)**

`crates/knowledge/src/builtin.rs:42-279` registers 19 tools + 2 commands.
`crates/runtime/src/agent.rs:5588-6035` declares 29 tools. The intersection is
17.

* Advertised to the model but **absent from the registry**, so retrieval can
  never rank, card, or disclose them: `workspace.write_file`,
  `workspace.edit_file`, `skills.search`, `web.search` (and `artifact.read`,
  `github.*` when wired).
* In the registry and disclosed as cards, but **not callable** in a plain run:
  `blackboard.post`, `blackboard.query`. Both appeared as cards in the live run's
  context manifest — including the text "Available only inside a workflow agent
  node" — while being absent from the `tools` array.

User-visible consequence: the model is told about tools it cannot call, and the
tools it *can* call for writing files are invisible to the very system that is
supposed to be selecting tools for it.

### F9.3 — Registry `Command` items are retrieved on every query and then silently discarded. **class (b) — data produced but never consumed**

`crates/knowledge/src/retrieval/mod.rs:306-315`.

`retrieve()` partitions the reranked survivors into `kind == Tool` and
`kind == Skill` only. `RegistryItemKind::Command`, `Plugin`, and `Hook` pass
`passes_hard_filters`, enter the candidate union, get scored, occupy rerank-pool
slots — and are then dropped before disclosure. Probe output against a fresh
registry:

```
query "the ci is red, fix the failing github check"
  registry COMMAND items: ["fix-ci", "update-docs"]   disclosed: []
  commands in the candidate union (found, then dropped before disclosure): 2

query "/fix-ci"                     -> disclosed: []   candidates: 2
query "update the docs, ..."        -> disclosed: []   candidates: 2
```

`crates/codypendentd/src/retrieval.rs:405-410` even carries a
`RegistryItemKind::Command => "command"` arm for rendering a command card — code
that is unreachable, because `retrieve` can never return one.

User-visible consequence: a user types "the CI is red"; `/fix-ci` — the built-in
command written precisely for that sentence, with those exact intents
(`builtin.rs:300-305`) — can never be surfaced by retrieval, in the context
manifest or through `skills.search`.

### F9.4 — In the default `build` mode, `skills.search` cannot see any repository-scoped skill, and says "not among the skills this search disclosed". **class (c)**

`crates/codypendentd/src/executor.rs:854-861` → `crates/runtime/src/agent.rs:3795-3801`
→ `crates/codypendentd/src/retrieval.rs:438,451-463`.

```rust
// executor.rs:850-861
let operating_tree = binding.worktree.clone();
let mut ctx = RunContext::new(
    launch.session_id, launch.run_id, launch.objective.clone(), launch.mode,
    operating_tree.clone(),   // <- RunContext::repository
    operating_tree,           // <- RunContext::worktree
);
```

A writing mode (`Build`, the CLI/TUI default) binds a **dedicated isolated
worktree**; `RunContext::repository` is set to that worktree. `skills.search`
then passes `&run.repository` (`agent.rs:3800`, commented "Server-derived, never
model-supplied") to `PoolRegistrySearch`, which re-derives the identity with
`crate::scan::repository_id_for(request.repository)` (`retrieval.rs:438`). Inside
a linked git worktree `git rev-parse --show-toplevel` returns the *worktree*, so
the derived `RepositoryId` is not the run's repository, and
`Scope::Repository(...)` matches nothing.

The comment three lines above the defect warns about exactly this
(`executor.rs:846-849`: *"Repository IDENTITY ... never conflated with this
policy read root"*), and the board and GitHub wiring do it correctly
(`executor.rs:876`, `882` use `launch.repository`). Only the registry search was
missed.

A/B reproduced live, same skill, same query, same daemon:

```
--mode build   (default; isolated worktree)
  [tool result: skills.search]  -> 8 tool cards, ZERO skill cards
                                   "(open: `rust.fix-ci` is not among the skills this search disclosed)"
  ...while the SAME run's context manifest contained:
                                   "skill rust.fix-ci [medium, first-party] — Diagnose and repair Rust GitHub Actions failures."

--mode explore (read-only; operating tree == checkout)
  [tool result: skills.search]  -> "skill rust.fix-ci — Diagnose and repair Rust GitHub Actions failures.
                                     permissions: filesystem-read:$REPOSITORY, ... command:cargo, ..."
                                   "=== SKILL rust.fix-ci (procedure) ===  # Fix R..."
```

Confirmed against the live database with a direct probe: with
`Scope::Repository(<run repo>)` the same funnel + the same
`SEARCH_CARD_LIMIT` config discloses `rust.fix-ci`; with `System`-only it
discloses nothing — matching the live build-mode output exactly.

User-visible consequence: a user installs a skill, starts a normal (build) run,
the context manifest advertises the skill card, the agent asks for its procedure,
and is told the skill does not exist. Progressive disclosure — the whole point of
cards — is dead in the mode where a skill would actually be used.

### F9.5 — Silent status filter: an edited skill disappears from retrieval, and the printed advice does not fix it. **class (c) — silent filter**

`crates/knowledge/src/retrieval/mod.rs:436-439` (`status != Active` ⇒ drop),
`crates/knowledge/src/registry.rs:252-254` (same version + changed hash ⇒
`Modified`), `crates/cli/src/commands.rs:650-655` (the warning text).

Reproduced with a probe over a real pool:

```
register probe.active (status=active)                -> skills disclosed: ["probe.active"]
edit SKILL.md, re-register (no version bump)         -> status=Modified
  same query                                         -> skills disclosed: []      <-- vanished
re-register a SECOND time, no further change         -> status=Active
  same query                                         -> skills disclosed: ["probe.active"]
```

`codypendent skill add` does print a warning — but its text is
`"set \`status = \"active\"\` in skill.toml and re-add it"`, which is wrong for
`Modified`: the manifest already says `active`. What actually clears it is
running `skill add` a second time with no changes (the stored hash then matches),
or bumping the version. A user following the printed advice loops.

Draft and deprecated are dropped with no signal at all:

```
register probe.draft + probe.deprecated, description "Rotate the production database credentials safely."
  query "rotate database credentials"  -> skills: []      (no note, no reason)
  query "probe.deprecated" (exact name) -> skills: []      (no note, no reason)
```

`skills.search` renders `"(no registry item matched)"` / `"X is not among the
skills this search disclosed"` — both read as "no such skill" rather than "your
skill is a draft".

### F9.6 — The "embedding" arm is a character-trigram hash with no semantics, and nothing in the product ever configures a real one. **class (a) for the embedding half**

`crates/knowledge/src/retrieval/embed.rs:57-135` (`HashingEmbedder`),
`crates/runtime/src/embedding.rs` (`HttpEmbedder`, the real one),
`crates/codypendentd/src/retrieval.rs:62-85` (`build_embedder`).

Measured cosine similarities from the shipped default embedder:

```
cos("run the tests", "execute the test suite") = 0.5477   (shared trigrams "test")
cos("run the tests", "run the tests")          = 1.0000
cos("delete all files", "run the tests")       = 0.0000
cos("automobile", "car")                       = 0.0000   <-- no semantics at all
```

It is deterministic and honest about being a stand-in, but it is a *fuzzy lexical*
signal, not an embedding — so "keyword + embedding hybrid" is in practice
"keyword + fuzzy keyword". `HttpEmbedder` is a genuine, well-built
OpenAI-`/embeddings` client with batching, index-reordering and dims checks — but
it is only constructed from an `[embedding]` table in `<data_dir>/models.toml`,
and **no code path anywhere writes that table**: not `codypendent doctor`, not the
provider catalog, not the TUI `/model` / `/provider` / `/keys` pickers, not
`install.sh`. Grep for `[embedding]` outside doc comments and tests returns
nothing. The user must hand-author TOML they are never told about.

Consequence: out of the box, outcome 9's "vector" is a hash.

### F9.7 — The hybrid is hybrid, but BM25 stop-words dominate, so top-k is close to noise. **class (c)**

`crates/knowledge/src/retrieval/bm25.rs:57-91` (Tantivy `TEXT` fields — default
tokenizer, no stemming, no stop-word filter) and
`crates/knowledge/src/retrieval/mod.rs:293-298` (weighted sum).

Both arms are alive. Ablation over the built-in registry (top-5 tools):

```
query "run the tests"
  full         : shell.run, repository.test, workflow.run, council.run, workflow.query
  only dense   : repository.test, shell.run, workflow.run, git.diff, blackboard.query
  only lexical : shell.run, workflow.run, council.run, repository.test, workflow.query
  exact=0      : (identical to full — the exact-overlap arm changes nothing here)

query "I need to look at what changed in my working copy"
  full         : task.list, memory.remember, blackboard.post, blackboard.query, task.move
  only dense   : workflow.query, git.diff, blackboard.query, workflow.run, repository.test
```

`git.diff` — the obviously correct answer to "what changed in my working copy" —
does not make the top 5 of the *hybrid*, because BM25 has no stemming
("changed" ≠ "changes") and no stop-word list, so `task.list` wins on
`"what … in …"` from its intent `"what work is in progress"`. The dense arm alone
gets it right; the weighted sum drowns it.

The budget is also barely a budget: 19 registry tools, `disclose_tools_max = 12`
(`crates/knowledge/src/retrieval/config.rs:90-93`) — 63 % of the catalog — and it
always emits 12, even for a query with no signal at all:

```
query "asdfghjkl zxcvbnm"                 -> 12 tool cards
query ""                                  -> 12 tool cards
query "what is the weather in paris"      -> 12 tool cards (top card: task.list)
```

Similarly `disclose_skills_min = 1` means the single installed skill is disclosed
regardless of relevance — the live run "document the charge path…" was handed
`skill rust.fix-ci`.

### F9.8 — Retrieval is frozen at the first turn of a session. **class (c)**

`crates/codypendentd/src/executor.rs:1521-1547` (`emit_run_opening`) and
`1553-1581` (`build_run_seed`).

Only a run whose reconstructed prior is empty (the first run of a session) calls
`emit_context`; a continuation emits `CONTINUATION_CONTEXT_NOTE` and re-carries
the *stored* manifest note. The disclosed tool/skill cards therefore reflect the
session's **first** objective forever. Deliberate (token economy), and the model
can re-query with `skills.search` — except that `skills.search` is broken in
build mode (F9.4).

### F9.9 — Minor, but real

* `crates/codypendentd/src/retrieval.rs:264-299` (`snapshot_open_package`)
  **errors** on any symlink inside a skill package, while
  `crates/knowledge/src/manifest.rs:409-430` (`collect_files`) silently *skips*
  symlinks when hashing. A package containing any symlink registers fine and then
  can never be opened: *"could not verify `X`'s package: package contains symlink"*.
* `crates/runtime/src/tools/registry_search.rs:45` — `SEARCH_CARD_LIMIT = 8` is
  documented as *"Matches the context manifest's tool budget"*; the manifest
  budget is 12 (`config.rs:92`).
* `crates/knowledge/src/retrieval/persist.rs:183` — `handle_non_registry_event`
  acknowledges `memory_changed` / `document_changed` / `symbol_changed` /
  `artifact_created` outbox rows and does nothing. Honest about it, but the outbox
  is therefore a registry-only queue.
* `crates/protocol/src/discovery.rs` socket-length check runs for *every*
  subcommand: `codypendent index rebuild`, which never opens a socket, fails
  outright with *"socket path … is 105 bytes"* under a long `CODYPENDENT_DATA_DIR`.

---

## OUTCOME 4 — findings

### F4.1 — The doc-writer's tools are never shown to the model. **class (b)** — the single highest-value finding in this vertical

`crates/runtime/src/agent.rs:1700-1712` adds `docs.create` / `docs.read` /
`docs.edit` / `docs.suggest` to `offered_tool_names` whenever a docs channel is
wired. `crates/runtime/src/agent.rs:3140-3180` dispatches all four.
`crates/codypendentd/src/executor.rs:806-809` wires the channel
**unconditionally** on every run.

But `advertised_tool_definitions` (`agent.rs:1827-1846`) is:

```rust
let offered = self.offered_tool_names(run);
let mut definitions: Vec<ToolDefinition> = static_tool_definitions()
    .into_iter()
    .filter(|def| offered.contains(&def.name))
    .collect();
```

and `static_tool_definitions()` (`agent.rs:5588-6035`) contains **no `docs.*`
entry** — its `vec![]` ends at `CouncilResultTool::NAME`. Intersection ⇒ dropped.

Proven live. Objective: *"document the charge path and write it up as a knowledge
doc"*; docs channel wired; captured request:

```
TOOL DEFINITIONS ADVERTISED TO THE MODEL (21):
  council.create council.result council.run git.apply_patch git.diff
  memory.remember repository.test shell.run skills.search task.create task.list
  task.move task.update web.search workflow.create workflow.query workflow.run
  workspace.edit_file workspace.read_file workspace.search workspace.write_file

  -- no docs.create, no docs.read, no docs.edit, no docs.suggest --
```

The system prompt (`agent.rs:386`) names no tools either, so the model has no
route to discover them. `skills.search` cannot help: `docs.*` is not in the
knowledge registry (F9.2), so no card for it can ever be returned.

The proof test for this outcome,
`crates/codypendentd/tests/docs_agent_it.rs:200-205`, asserts on
`offered_tool_names` and then drives the calls with a `ScriptedDriver` — it
verifies the dispatch half and skips the advertisement half. The sibling
`task.*` feature *does* assert on `advertised_tool_definitions`
(`agent.rs:7231-7247`); `docs.*` does not.

User-visible consequence: a user asks the agent to write documentation. The agent
has no document tool, so at best it writes a Markdown file with
`workspace.write_file`. Docs Studio stays empty of anything an agent produced;
`DocumentAuthor::Agent` is never constructed in a real run. The human paths work
(`codypendent docs new/list/check` all verified live), so the feature looks alive
from the CLI while being unreachable from the agent — which is what "agent
doc-writer" means.

### F4.2 — There is no skill-writer. **class (a)**

Exhaustive check of every surface a user could type:

* Tools: the complete runtime tool-name list (grepped from every
  `const NAME: &'static str` in `crates/runtime/src/tools/`) contains
  `skills.search` and nothing else skill-related. No create, draft, edit,
  promote, or deprecate.
* CLI: `codypendent skill --help` → **one** subcommand, `add`. No `new`, `list`,
  `edit`, `promote`.
* TUI: `crates/tui/src/palette.rs:231-237` — *"/skills  Skill Studio · read
  only — inspect registered skills and their permissions"*.
* Workflows: a workflow node may name a `skill`
  (`crates/workflow/src/compile.rs:194`), and
  `compile_with_registry` validates that it exists — but
  `crates/codypendentd/src/workflow_exec.rs:2238-2240` destructures
  `NodeAction::Agent { role, model_policy, .. }` and **never reads `skill`**.
  `synthesize_agent_objective` (`workflow_exec.rs:2254-2294`) builds the node's
  prompt from workflow id, node id, role, outputs and inputs only. A workflow can
  declare a skill; the agent never sees it.
* Promotion: `crates/codypendentd/src/promotion.rs:80` forces
  `requires_permission_review` for `ArtifactKind::Skill`, and the
  `Candidate`/`ActiveVersions` state machine in `crates/eval/src/promote.rs` is
  real and human-gated — but `ActiveVersions` is an in-memory map; approving a
  "skill" candidate never touches `registry_items.status`. There is no path from
  a promoted candidate to a retrievable skill.

The only way to get a skill into the system is to hand-write a `skill.toml` +
`SKILL.md` on disk and run `codypendent skill add <dir>`. That path does work
(verified: install → registry row → disclosed card), and its guardrails are good
(traversal-safe install dir name, validate-before-copy, staging swap,
post-registration hash re-verification on open). But it is a package installer,
not a writer.

### F4.3 — `skill_exec.rs` executes nothing in production (context for outcome 12). **class (b)**

`crates/knowledge/src/skill_exec.rs:142-183` (`run_script`) lowers a skill's
`[permissions]` into a `SandboxProfile` and runs a named script through a
`SandboxExecutor`. It is complete and tested. Its only callers in the entire
workspace are `crates/knowledge/tests/skill_exec_it.rs`. No daemon, runtime, CLI,
or workflow code ever calls `run_script` or `profile_for_permissions`, and no
production code constructs a `SandboxExecutor` for skills (the only production
sandbox construction is `crates/ui-host/src/runtime.rs:1024`, for UI plugins).

`crates/knowledge/src/manifest.rs:239-243` sets `executable = true` for every
skill on the grounds that "the OS sandbox now confines skill scripts" — so
retrieval treats script-bearing skills as runnable behaviours while nothing can
run them. Today a skill is a Markdown procedure, disclosed as prose; there is no
WASM and no script execution on any live path.

---

## The single structural pattern

**There are two tool universes and they were never reconciled.**

`codypendent-knowledge` owns a governed registry with scopes, trust tiers, risk
classes, dependencies, permissions, BM25 + vector indexes, an outbox, persisted
embeddings and a versioned rerank config. `codypendent-runtime` owns a
hard-coded `static_tool_definitions()` vec that is the *only* thing the model
ever sees.

Every finding above is one instance of that split:

* the retrieved cards are prose *about* one universe, injected next to the full
  schemas of the other (F9.1);
* four advertised tools have no registry row and four registry kinds
  (`Command`/`Plugin`/`Hook`, plus workflow-only tools) have no advertised
  counterpart (F9.2, F9.3);
* `docs.*` was added to the offered set and the dispatcher — both runtime-side —
  and to neither universe's catalog (F4.1);
* the skill's own identity scope is derived twice, from two different values, in
  the two universes (F9.4);
* skill *execution* lives entirely in the knowledge universe, where nothing
  executes (F4.3).

Fixing outcome 9 is not a ranking problem. It is: make the registry the source of
truth for what the model is offered, and make `advertised_tool_definitions`
consume the funnel's output.

---

## What I could not exercise, and why

* **A real embedding model.** `HttpEmbedder` needs a live
  OpenAI-compatible `/embeddings` endpoint. I exercised the code path shape
  (`build_embedder` builds from a valid `[embedding]` entry, rejects a bad
  provider) but did not stand up an embeddings server, so the persisted-vector /
  `semantic_indexes` / drain path is reviewed by reading, not running. The
  finding that matters (nothing writes `[embedding]`, so the default is the hash)
  does not depend on it.
* **`advertised_tool_definitions` as a unit.** I staged a probe crate linking
  `codypendent-runtime` to print `offered` vs `advertised` side by side; the
  build filled the shared disk (97 % used, shared with six other reviewers) and I
  aborted it rather than risk the workspace. The claim is instead proven by the
  stronger evidence — the actual captured provider request — plus the exhaustive
  grep of the `static_tool_definitions()` body.
* **MCP top-k in anger.** `select_mcp_tools` needs an operator-declared MCP
  server offering more than `mcp_top_k` tools. I read it closely and it looks
  correct (`RunContext.mcp_advertised` computed once per run, honoured by both
  `offered_tool_names` and `advertised_tool_definitions`, `None` = advertise all),
  but I did not stand up an MCP server.
* **A live model's *behaviour*.** The stub server returns canned text and canned
  tool calls, so I can prove exactly what reaches the model and what a tool call
  does with it, but not whether a real model would choose the retrieved skill.
* **The promotion pipeline end to end.** `codypendent promote` needs eval suite
  reports (`promotion.regression-evidence-missing` otherwise). I read the
  gateway and the state machine; the finding recorded (an approved skill
  candidate never changes `registry_items.status`) is from reading
  `crates/eval/src/promote.rs`'s `ActiveVersions`, not from a run.
