# Vertical review (round 4) — retrieval & skills

Reviewer: **retrieval-skills**. Pinned commit `c255bec8b175d62942b3312cff2335b97d43a59a`
(v0.5.1). Nothing in the repository was modified. Owned outcomes: **9** (vector
top-k tool/skill selection instead of injecting all descriptions) and **4**
(agent skill-writer and doc-writer).

Files read in full or in the sections that carry behaviour:
`crates/knowledge/src/{registry,skills,skill_exec,builtin,extractor,manifest}.rs`,
`crates/knowledge/src/retrieval/**` (mod, config, embed, bm25, vector, persist),
`crates/knowledge/src/docs/**`, `crates/runtime/src/tools/**`,
`crates/runtime/src/agent.rs` (the advertisement/dispatch path in full; the
12 321-line file was read selectively elsewhere — see "What I did not verify"),
`crates/cli/src/skill_writer.rs`, plus the daemon-side consumers that decide what
reaches a user (`crates/codypendentd/src/{executor,retrieval,docs_channel,lib}.rs`,
`crates/cli/src/commands.rs`).

---

## Verdicts

**OUTCOME 9 — WORKING (with a real quality caveat).** The previous round's
headline defect is gone. `select_builtin_tools` now runs the knowledge funnel over
the run's *offered* built-ins and `advertised_tool_definitions` consumes its
output, so the tool array in the actual provider request is a genuine top-k
selection, not full injection. Measured live: **28 offered → 15 advertised**, and
three unrelated objectives produced three *different* 15-tool arrays. The caveat:
the "embedding" arm of this specific gate is hard-coded to the character-trigram
`HashingEmbedder` and cannot use a configured embedding model — proven by
configuring one and getting a byte-identical advertisement.

**OUTCOME 4 — PARTIAL.** Both halves now exist and both are reachable, but each is
broken at the last wire in the *default* mode:

* **doc-writer**: the four `docs.*` tools are now in `static_tool_definitions()`,
  are advertised, and a real agent run created a real document with
  `DocumentAuthor::Agent` in SQLite. But in `--mode build` (the CLI/TUI default)
  the document is scoped to the run's **throwaway worktree**, so
  `codypendent docs list` from the checkout prints *"No documents yet."* — the
  agent's output is permanently orphaned. `--mode explore` writes it correctly.
* **skill-writer**: `codypendent skill new` exists, authors a valid package, and
  registers it as `draft`. But following the command's **own printed promotion
  instruction** registers the active version under a repository identity no run
  ever queries, so the promoted skill is invisible. Proven live.

---

## How this was exercised

Everything below was produced against real binaries — `target/debug/codypendentd`
+ `target/debug/codypendent` — pointed at a stub OpenAI-compatible server on
`127.0.0.1:8199` that writes every inbound request body to disk and can serve
scripted SSE tool calls. Scratch state under `/tmp/rrs` (data dir, config, and a
one-file git repo); nothing in the repository or the orchestrator's `target/` was
touched. No `cargo` invocation of any kind was run.

---

## OUTCOME 9 — findings

### F9.1 — The gate is real. Two unrelated objectives, two different tool arrays. **(fixed)**

`crates/runtime/src/agent.rs:2116-2195` (`select_builtin_tools`),
`:2307-2314` (computed once per run in `execute_run`),
`:2039-2073` (`advertised_tool_definitions` filters by `run.tools_advertised`),
`:5307-5315` (`ALWAYS_ADVERTISED_TOOLS`, the 7-tool floor),
`crates/runtime/src/models.rs:238` (`DEFAULT_BUILTIN_TOP_K = 8`),
`crates/codypendentd/src/executor.rs:862-863` (the daemon passes both budgets in).

The command actually run, twice, against the same daemon and the same repo:

```
codypendent run --objective "<X>" --repo /tmp/rrs/repo --jsonl
```

The `tools[].function.name` array taken verbatim from the captured request body:

```
A) "document the charge path and write it up as a knowledge doc"    15 definitions
["docs.create", "docs.edit", "docs.read", "docs.suggest", "git.apply_patch",
 "repository.test", "shell.run", "skills.search", "task.move", "task.update",
 "web.search", "workspace.edit_file", "workspace.read_file", "workspace.search",
 "workspace.write_file"]

B) "the CI is red on my pull request, please fix the failing github check"  15 definitions
["council.run", "docs.edit", "docs.suggest", "git.apply_patch", "repository.test",
 "shell.run", "skills.search", "task.create", "task.list", "web.search",
 "workflow.query", "workspace.edit_file", "workspace.read_file", "workspace.search",
 "workspace.write_file"]

A only: docs.create, docs.read, task.move, task.update
B only: council.run, task.create, task.list, workflow.query
identical? False
```

A third objective ("who calls the main function and what is the blast radius of
changing it") produced a third distinct set including `graph.callers_of` and
`graph.blast_radius`. The daemon log confirms the mechanism:

```
INFO codypendent_runtime::agent: retrieval narrowed this run's built-in tool
     advertisement run_id=019ffd4d-9b43-… offered=28 advertised=15
```

7 of the 15 are the unconditional floor; the other 8 are the funnel's top-k over
the 21 discretionary candidates. The floor is deliberately excluded from ranking
(`agent.rs:2138-2145`) so the budget is not spent on tools that were guaranteed
anyway — the exact anti-pattern this review was told to hunt, and it is closed on
this path.

Sanity sweep over five more objectives (top-k shown, floor elided):

```
"show me what changed in my working copy"        -> git.diff ✓, memory.remember, workflow.query, …
"remember that we decided to use tokio for async"-> memory.remember ✓, docs.create, …
"search the web for the latest tokio release …"  -> web.search ✓, repository.test, …
"run the repository test suite …"                -> repository.test ✓, workflow.run, …
"asdfghjkl zxcvbnm qwertyuiop"                   -> still exactly 15 (docs.*, workflow.*, web.search)
```

The obviously-correct tool made the cut in every meaningful case, including
`git.diff` for "what changed in my working copy" — the case the previous round
recorded as a ranking failure. The last line is the honest limit: the budget is
always spent in full, so a query with no signal still ships 8 arbitrary
specialists.

### F9.2 — The tool-selection funnel can never use a configured embedding model. **class (b)**

`crates/runtime/src/agent.rs:2147` and `:2231`:

```rust
let indexes = match RetrievalIndexes::build(&items, HashingEmbedder::new()) {
```

Both `select_builtin_tools` and `select_mcp_tools` construct a
`HashingEmbedder` literal. `FrameworkAgentRuntime` has no `SemanticEmbedder` seam
at all (grep for `SemanticEmbedder` in `crates/runtime/src/agent.rs` returns
nothing). The real embedder (`crates/runtime/src/embedding.rs`, built by
`crates/codypendentd/src/retrieval.rs:62-85` from `models.toml`'s `[embedding]`)
is injected only into the *context-manifest* funnel and the outbox drain
(`executor.rs` `emit_context` → `assemble_context_with`, and
`PoolRegistrySearch::search` → `semantic_indexes`).

Proven live rather than inferred. I added a real `[embedding]` table pointing at
the stub's `/embeddings` endpoint and restarted the daemon:

```
INFO codypendent_codypendentd::retrieval: semantic embeddings enabled for retrieval
     model=stub-embed endpoint=http://127.0.0.1:8199/v1/embeddings
INFO codypendent_codypendentd::retrieval: backfilled registry embeddings at startup written=33
```

The same objective, before and after, produced a **byte-identical** advertised
top-k:

```
hashing embedder : ['council.run','docs.read','repository.test','task.create',
                    'task.list','task.move','workflow.query','workflow.run']
semantic embedder: ['council.run','docs.read','repository.test','task.create',
                    'task.list','task.move','workflow.query','workflow.run']
```

(The manifest cards *did* change, confirming the semantic path was live.)
User-visible consequence: outcome 9 asks for "keyword + embedding". On the path
that actually decides what the model is shown, the embedding half is a
character-trigram hash — `cos("automobile","car") = 0` — and no configuration can
change it. The stand-in is honest about being one
(`crates/knowledge/src/retrieval/embed.rs:56-63`); the gap is that the real one
was never given a seam here.

Related and unchanged from the previous round: **nothing in the product writes an
`[embedding]` table.** Grepping `crates/`, `install.sh` and `docs/` for
`[embedding]` outside doc comments and tests finds no writer — not `doctor`, not
the TUI pickers, not the provider catalog. A user must hand-author TOML they are
never told about.

### F9.3 — The context manifest still advertises tools the run cannot call. **class (c)**

`crates/knowledge/src/builtin.rs:193-234` registers `blackboard.post` /
`blackboard.query`; `crates/runtime/src/agent.rs:1895-1901` offers them only
inside a workflow node. The manifest funnel does not know that, so a plain run's
manifest cards include them.

Reproduced end to end. Objective *"post a finding to the blackboard about the
charge path"*; the manifest note emitted to the run's ledger contained:

```
tool blackboard.post [safe, first-party]
tool blackboard.query [safe, first-party]
```

…while the advertised `tools` array contained neither (`bb advertised? False`).
A scripted `blackboard.post` call then returned:

```
[tool result: blackboard.post]
tool error: unknown tool `blackboard.post`
"outcome":{"type":"Failed","message":"unknown tool `blackboard.post`"}
```

User-visible consequence: the model is told a tool exists, spends a step calling
it, and gets "unknown tool". The card's own text ("Available only inside a
workflow agent node") is the only mitigation, and a model that reads the card as a
catalog entry will still try.

### F9.4 — Nine advertised tools have no registry row, so retrieval can never card them. **class (b), narrowed but not closed**

Computed directly from the two catalogs (36 `decl(...)` entries in
`crates/runtime/src/agent.rs:6217-6900`; 27 `tool(...)` + 2 `command(...)` entries
in `crates/knowledge/src/builtin.rs:42-487`):

```
static_tool_definitions entries: 36
knowledge registry tools: 27   commands: 2
advertised but NOT in registry: artifact.read, github.create_check_run,
  github.create_draft_pull_request, github.get_pull_request,
  github.list_check_runs, github.update_pull_request,
  graph.blast_radius, graph.callers_of, graph.tests_covering
in registry but NOT advertisable: (none)
```

The previous round's worst instances (`workspace.write_file`,
`workspace.edit_file`, `skills.search`, `web.search`, all four `docs.*`) are now
registered — a real repair. What remains: the `github.*` family and the three
`graph.*` tools can never appear as a card in the context manifest or in a
`skills.search` result, and their curated intents/keywords do not exist, so
`builtin_registry_items` (`agent.rs:5332-5384`) ranks them on their schema
description and dotted name alone. Observably, a graph-flavoured objective *did*
still select two of the three, so the consequence here is degraded ranking and
zero progressive disclosure rather than invisibility.

### F9.5 — Commands are disclosed now. **(fixed)**

`crates/knowledge/src/retrieval/mod.rs:321-331` partitions `Command` items into
their own budget (`disclose_commands_max = 2`,
`crates/knowledge/src/retrieval/config.rs:103`). Live confirmation, both in the
context manifest and through `skills.search`:

```
command /update-docs — Bring documentation in line with code changes …
  permissions: filesystem-read:$REPOSITORY
```

The previous round's F9.3 (commands scored then dropped) is closed.

### F9.6 — `skills.search` works in build mode now. **(fixed)**

`crates/runtime/src/agent.rs:4249-4252` now derives the search scope from
`run.board_repository` (the repository IDENTITY) and only falls back to
`run.repository` (the worktree). A/B against a repository-scoped skill installed
from inside the checkout, `--mode build` (the default):

```
[tool result: skills.search]
skill rust.fix-ci — Diagnose and repair Rust GitHub Actions failures.
  permissions: filesystem-read:$REPOSITORY, command:cargo
=== SKILL rust.fix-ci (procedure) ===
# Fix Rust CI
1. Read the failing job log. …
```

The previous round's F9.4 is closed. Note the fix was applied to `skills.search`
only — see F4.2 and F9.9 for the same defect surviving in `docs.*`.

### F9.7 — Silent status filter, unchanged. **class (c)**

`crates/knowledge/src/retrieval/mod.rs:458-461` still drops everything but
`Active` with no signal, and `skills.search` renders the drop as
`"(open: X is not among the skills this search disclosed)"` — which reads as "no
such skill", not "your skill is a draft". Observed verbatim for a skill that
exists, is `active`, and is simply anchored elsewhere (F4.2):

```
(open: `ops.rotate-creds` is not among the skills this search disclosed)
```

This is the honest-answer problem the brief asks about: the message cannot
distinguish *absent*, *draft*, *deprecated*, and *wrong scope*, and in my
reproduction the true cause was the fourth.

### F9.8 — `disclose_skills_min = 1` forces an irrelevant skill card. **class (c), minor**

`crates/knowledge/src/retrieval/config.rs:101`. The manifest for the objective
"post a finding to the blackboard about the charge path" disclosed
`skill rust.fix-ci [medium, first-party]` — the only installed skill, disclosed
regardless of relevance. Harmless at one skill; misleading at ten.

### F9.9 — Retrieval is still frozen at the first turn for the *manifest*, but not for the advertisement. **class (c), improved**

`crates/codypendentd/src/executor.rs:1657-1677` (`emit_run_opening`): a
continuation run still emits `CONTINUATION_CONTEXT_NOTE` and re-carries the stored
manifest instead of re-assembling. That part is unchanged. What *did* improve:
`select_builtin_tools` runs inside `execute_run` (`agent.rs:2314`), so it runs for
every run including a continuation, and `retrieval_query_text`
(`agent.rs:5436-5445`) folds in the latest user turn. So a follow-up gets a fresh
tool advertisement even though the prose cards are stale. **Inferred from
reading** — `codypendent run` always creates a new session and I found no CLI flag
to continue one, so I did not exercise a continuation.

---

## OUTCOME 4 — findings

### F4.1 — The doc-writer is wired, works, and then writes into a directory that is about to be deleted. **class (c) — highest-value finding in this vertical**

Producer: `crates/runtime/src/agent.rs:4346-4380` (`execute_docs_create`) →
`crates/codypendentd/src/docs_channel.rs:140-181`. The advertisement half of the
previous round's F4.1 is genuinely fixed: `docs.*` are in
`static_tool_definitions()` (`agent.rs:6712-6790`) and reach the model.

The good half, proven live. Scripted `docs.create` + `docs.read` through the stub:

```
ToolStarted    tool=docs.create
ToolCompleted  tool=docs.create  outcome=Succeeded
[tool result: docs.create] created document "Charge path runbook"
                           (019ffd4f-4dcb-7500-afd6-496aa63858bf). …
ToolStarted    tool=docs.read
[tool result: docs.read] documents (newest first):
                         - 019ffd4f-4dcb-… — Charge path runbook [draft] r1
```

and the row really is in SQLite, with real agent authorship:

```
documents:            ('019ffd4f-4dcb-…','Charge path runbook',
                       '{"tier":"repository","key":"feabe822-fe5b-683d-5706-ce1f0b270b5c"}',
                       'draft', metadata.created_by = {"kind":"agent","run_id":…,"model":"stub/local"})
document_authorship:  author_json = {"kind":"agent","run_id":…}  mutation=insert_block
```

The break. The scope key is `feabe822-…`, but the run's repository identity — the
one printed in that same run's own context manifest — is
`15eca4ca-bc08-8434-9742-30e9d639f1d0`. The four `docs.*` handlers read
`run.repository` (`agent.rs:4356`, `:4391`, `:4412`, `:4446`), and
`crates/codypendentd/src/executor.rs:919-937` sets `RunContext::repository` to the
**operating tree**, which for a writing mode is a dedicated linked worktree:

```rust
let operating_tree = binding.worktree.clone();
let mut ctx = RunContext::new(…, operating_tree.clone(), operating_tree);
```

`AssemblyDocsChannel` then derives the document's scope with
`repository_id_for(&self.root(repository))` (`docs_channel.rs:93`), hashing the
worktree path. `codypendent docs list` filters on
`[Scope::Repository(anchor_repository_id(cwd)), Scope::System]`
(`crates/cli/src/commands.rs:866-868`), which never matches.

The decisive A/B, same objective, same daemon, same repo:

```
$ codypendent run --objective "document the charge path as a knowledge doc" --repo /tmp/rrs/repo
  (mode defaults to build)
$ codypendent docs list          # run from /tmp/rrs/repo
No documents yet. Create one with `codypendent docs new "<title>"`.

$ codypendent run --mode explore --objective "document the charge path as a knowledge doc" …
$ codypendent docs list
ID                                     STATUS      REV  TITLE
019ffd50-f07a-7ab3-b6e8-793792b254f6   draft         1  Charge path runbook
```

and the two rows side by side:

```
build   scope_json = {"tier":"repository","key":"feabe822-fe5b-683d-5706-ce1f0b270b5c"}   <- worktree
explore scope_json = {"tier":"repository","key":"15eca4ca-bc08-8434-9742-30e9d639f1d0"}   <- checkout
```

Because every build run binds a *fresh* worktree at a fresh path, the identity is
different every run and the worktree is released when the run ends. The document
is therefore not merely mis-filed — it is unreachable from any checkout, any later
run, and any client, forever. The user asked the agent to write documentation, the
agent said it did, the ledger says it did, the row exists — and Docs Studio and
`docs list` show nothing, with no error and no note that anything was filtered.

This is the *same* defect the previous round recorded as F9.4 and that
`skills.search` was repaired for (`agent.rs:4249-4252`, whose comment explains the
worktree-identity trap in detail). The repair was applied to that one call site.
`docs.*` — and `graph.*` at `agent.rs:4282` — still pass `run.repository`.

### F4.2 — The skill-writer's own printed promotion instruction produces an invisible skill. **class (c)**

`crates/cli/src/skill_writer.rs` (556 lines) is real and good: it renders a
`skill.toml`, TOML-escapes free text, validates through the *same*
`manifest::load_package` the installer uses, installs through the same
`install_package`, and starts `draft` by construction. It is reachable:
`codypendent skill new <ID> --name … --description … --procedure <FILE>`
(`crates/cli/src/main.rs:336-361`, dispatched at `:1121-1139`, implemented at
`crates/cli/src/commands.rs:693-762`). The previous round's F4.2 ("there is no
skill-writer of any kind") is therefore no longer true.

But the round trip fails. Live, from the checkout:

```
$ codypendent skill new ops.rotate-creds --name "Rotate credentials" \
    --description "Rotate the production database credentials safely." \
    --scope repository --procedure /tmp/rrs/proc.md
authored skill ops.rotate-creds 0.1.0 (repository) -> /tmp/rrs/d/skills/ops.rotate-creds
status is Draft: registered, but retrieval will not disclose it until it is promoted —
  in /tmp/rrs/d/skills/ops.rotate-creds/skill.toml set `status = "active"`, bump `version`,
  then re-run `codypendent skill add /tmp/rrs/d/skills/ops.rotate-creds`
```

Following that instruction exactly:

```
$ (edit skill.toml: status = "active", version = "0.2.0")
$ codypendent skill add /tmp/rrs/d/skills/ops.rotate-creds
installed skill ops.rotate-creds 0.2.0 (repository) -> /tmp/rrs/d/skills/ops.rotate-creds
```

Registry after:

```
('ops.rotate-creds', {"tier":"repository","key":"15eca4ca-…"}, 'draft',  '0.1.0')   <- checkout id
('ops.rotate-creds', {"tier":"repository","key":"9abf45d0-…"}, 'active', '0.2.0')   <- data-dir path id
```

The cause is `crates/cli/src/commands.rs:659`:

```rust
let anchor = anchor_repository_id(dir);      // `dir` is the PACKAGE directory
```

`skill_add` anchors a `scope = "repository"` package to the *package directory's*
Git toplevel, not the checkout the user is standing in. The command's own doc
comment (`commands.rs:640-643`) states the intent — "anchors to the checkout `dir`
lives in" — which only holds when the package happens to live inside the checkout.
`<data_dir>/skills/…` never does. `skill_new` gets this right
(`commands.rs:708` uses `current_dir()`), which is exactly why the draft row lands
correctly and the promoted row does not.

Confirmed end to end with a real run whose objective is verbatim the skill's
description:

```
$ codypendent run --objective "rotate the production database credentials safely" …
[tool result: skills.search]
  command /update-docs — …
  skill rust.fix-ci — Diagnose and repair Rust GitHub Actions failures.
  tool shell.run — …
  …
(open: `ops.rotate-creds` is not among the skills this search disclosed)
```

An irrelevant skill is disclosed; the one whose description *is* the query is not.

Same root cause, second instance: `codypendent skill add` on a package kept
outside the checkout (the natural place to keep one) silently registers under a
path-derived identity. Adding the identical package from outside and then from
inside the checkout produced two rows and two identical success messages:

```
$ codypendent skill add /tmp/rrs/skillpkg          # outside the checkout
installed skill rust.fix-ci 0.1.0 (repository) -> /tmp/rrs/d/skills/rust.fix-ci
  -> {"tier":"repository","key":"1cc2f32a-aabe-4e87-2680-bcc1d040c6d3"}   (invisible)
$ codypendent skill add /tmp/rrs/repo/skillpkg     # inside the checkout
installed skill rust.fix-ci 0.1.0 (repository) -> /tmp/rrs/d/skills/rust.fix-ci
  -> {"tier":"repository","key":"15eca4ca-bc08-8434-9742-30e9d639f1d0"}   (works)
```

Third instance, no user action required at all: the daemon's boot scan
(`crates/codypendentd/src/lib.rs:338-352`) walks **both** skill roots with a
single `repository` id derived from the daemon's startup working directory, so
every `scope = "repository"` package in the *global* `<data_dir>/skills/` root is
re-registered under whatever checkout the daemon was launched from. Observed:
after one daemon restart from `/home/user/codypendent`, both skills gained a
fourth identity:

```
('ops.rotate-creds', {"tier":"repository","key":"6102effd-e232-32bd-825a-8f090d5ad025"}, 'active', '0.2.0')
('rust.fix-ci',      {"tier":"repository","key":"6102effd-e232-32bd-825a-8f090d5ad025"}, 'active', '0.1.0')
```

Net: one skill package accumulated **three** distinct repository identities from
three code paths that each derive it differently, and only one of them is the
identity a run queries. Nothing warns.

### F4.3 — `skill_exec.rs` still executes nothing in production. **class (b), unchanged**

`crates/knowledge/src/skill_exec.rs` (823 lines: `run_script`, `run_module`,
`profile_for_permissions`, placeholder substitution) is complete and tested. Its
only callers in the entire workspace are `crates/knowledge/src/lib.rs` (the
re-export) and `crates/knowledge/tests/skill_exec_it.rs`. No daemon, runtime, CLI,
or workflow code calls `run_script`/`run_module`, and `crates/knowledge/src/manifest.rs:364`
still marks script-bearing skills `executable = true` on the grounds that the OS
sandbox confines them. A skill remains a Markdown procedure disclosed as prose.

### F4.4 — An agent's edits to a repository document need no review. **class (c), minor / by-design-but-worth-stating**

`crates/knowledge/src/docs/collab.rs:54-59`:

```rust
match scope {
    Scope::Organization(_) => CollaborationMode::Suggest,
    _ => CollaborationMode::Edit,
}
```

`crates/codypendentd/src/docs_channel.rs:9-19` describes the collaboration mode as
"the REAL gate on an agent's document writes", citing organization scope's
`Suggest` default. The scope an agent actually reaches from a run is
`Repository`, which defaults to `Edit` → `EditDisposition::Direct` → a silent CRDT
mutation with no suggestion and no approval. `docs.suggest` exists but is opt-in
for the model. The gate described in the module docs is not the gate on the path
an agent takes.

### F4.5 — `DocumentReplica` has no production consumer. **class (b), noted for the docs vertical**

`crates/knowledge/src/docs/replica.rs` (257 lines, the client-side CRDT replica)
is referenced only from `crates/codypendentd/tests/docs_sync_it.rs` and a comment
in `crates/protocol/src/document.rs:109`. Not my outcome, recorded because it is
the same producer-with-no-consumer shape and it sits in my read set.

---

## The pattern

**Repository identity is derived independently at six call sites from three
different inputs, and only some of them were fixed.** The knowledge fabric keys
almost everything — skills, documents, memories, the code graph, the board — on a
`RepositoryId` that is `sha256(canonical git toplevel)`. That derivation is
correct and shared. What is not shared is *which path you hand it*:
`skills.search` was repaired to use the run's identity
(`agent.rs:4249-4252`, with a long comment explaining the trap);
`docs.*` and `graph.*` three hundred lines away still pass the throwaway worktree;
`codypendent skill add` passes the package directory; the daemon's boot scan
passes its own startup cwd; `docs list` passes the user's cwd. Every one of these
is "obviously right" locally, and each pair disagrees. The symptom is always the
same and always silent: a `WHERE scope_key = ?` matches nothing, and the surface
reports *"No documents yet"*, *"not among the skills this search disclosed"* — the
"not found" that should have said "filtered".

This is precisely the failure mode the standing synthesis names: **the fix was
applied to the instance rather than to the class.** The previous round found the
worktree-identity bug in `skills.search`; the repair fixed `skills.search`. The
class fix — one server-side accessor on `RunContext` that returns the run's
repository identity, with `RunContext::repository` renamed to something that
cannot be mistaken for it, and one `anchor_for(checkout)` the CLI uses everywhere
— was not taken, so the same bug is still live in the outcome that was *also*
being repaired in the same round. Outcome 9's repair, by contrast, was made at the
class level (the funnel now feeds the advertisement, floor excluded from ranking,
budget honoured) and it holds under every test I could put to it.

---

## What I did not verify

* **A continuation run's advertisement.** F9.9's claim that top-k re-runs on a
  follow-up is read from `execute_run`'s call order, not observed. `codypendent
  run` always opens a new session and I found no CLI path to continue one; driving
  the TUI in a pty to get a second turn was out of budget.
* **`graph.*` under the worktree identity.** `agent.rs:4282` passes
  `run.repository` exactly as the `docs.*` handlers do, so I expect the same
  orphaning. My one-file demo repository has a single symbol with no callers, so
  `graph.callers_of main` legitimately returns "no results" in both modes and the
  test cannot distinguish the two causes. **Inferred from reading only.**
* **MCP top-k in anger.** `select_mcp_tools` needs an operator-declared MCP server
  offering more than `mcp_top_k` tools. I read it and it mirrors the built-in gate
  correctly, but I did not stand up an MCP server. (Its embedder is hard-coded the
  same way — F9.2 applies to it identically, and that half *is* read from the
  source.)
* **A real embedding model's ranking quality.** My `/embeddings` stub returns a
  constant vector, which is sufficient to prove the advertisement ignores it
  (F9.2) but says nothing about whether a real model would rank better.
* **A real model's behaviour.** The stub returns canned text and canned tool
  calls. I can prove exactly what reaches the model and what a call does; I cannot
  prove a real model would pick the retrieved skill.
* **Docs Studio (the TUI pane).** I verified the orphaning through
  `codypendent docs list` and a direct SQLite query. I did not drive the TUI; the
  pane is another vertical's, and its own scope filter may differ.
* **`crates/runtime/src/agent.rs` in full.** I read the advertisement, dispatch,
  `docs.*`, `skills.search`, `graph.*`, `RunContext`, `static_tool_definitions`,
  the retrieval projections and the run loop's opening — roughly 3 000 of its
  12 321 lines closely, and grepped the remainder for every symbol relevant to my
  two outcomes. I did not read its ~4 000 lines of inline tests line by line.
* **Promotion via `codypendent promote`.** Out of scope for these two outcomes and
  not exercised; the previous round's finding about `ActiveVersions` never
  touching `registry_items.status` was neither confirmed nor refuted here.

No `cargo build` or `cargo test` was run at any point. Disk stayed at 28% used.
