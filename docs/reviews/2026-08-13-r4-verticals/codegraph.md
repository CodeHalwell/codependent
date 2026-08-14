# Vertical: codegraph — round 4

Pinned commit `c255bec8b175d62942b3312cff2335b97d43a59a` (v0.5.1). Nothing edited, nothing committed.

Scope read in full: `crates/knowledge/src/{codegraph,repomap,observer,context,adapter}.rs`,
`crates/tui/src/dag.rs`, `crates/codypendentd/src/scan.rs`, and every construction/query site
across `crates/daemon/**`, `crates/codypendentd/**`, `crates/runtime/**`, `crates/cli/**`,
`crates/tui/**`.

Owned outcomes: **5** (DAG viewer for code-context management, user + agent) and **14** (live
code graph).

---

## Verdicts

**OUTCOME 14: WORKING for a human editor, BROKEN for the agent's own edits.**
The watcher is armed on a live path and folds an uncommitted mid-session edit into the graph in
about one second. I measured the full round trip through the agent's own tool: a `graph.callers_of`
call at 23:05:06 returned 1 caller, an uncommitted append at 23:05:12 moved no `HEAD`, and the very
next tool call at 23:05:23 returned 2 — the new symbol included, stamped `…+workdir`. The previous
round's F1 (watcher constructed by nobody) and F2 (HEAD-gated graph) are genuinely repaired. But
the agent's *own* `workspace.write_file` lands in a run worktree that is a **sibling of the
repository**, outside every watch, so the one scenario outcome 14 names by name — "including by the
agent's own `edit_file`" — still never reaches the graph. And "retrieval stays current", the second
clause, has no mechanism at all: 4,071 `symbol_changed` outbox rows in my live database were routed
to a `tracing::trace!` no-op.

**OUTCOME 5 (agent): BROKEN in the default mode.**
The `graph.*` tools exist, are declared with real schemas, are dispatched, and work — in the
read-only modes. In **Build**, the default mode of both `codypendent run` and the TUI, all three
return the string `no results` for every question, because the tool derives its repository identity
from the run's throwaway worktree instead of the repository. Proven by computing both ids and by
running the identical query in both modes.

**OUTCOME 5 (user): PARTIAL.**
The subdirectory bug (last round's F5) is really fixed — I drove the graphical TUI in a pty from
`repo/crates/demo` and got all 28 edges. Discoverability is fixed too: there is a `/edges` palette
entry, so the code graph is now reachable in `--accessible` mode. But the surface is still a flat,
alphabetically-sorted, paginated edge table, not a navigable graph: `dag::lay_out` still has exactly
one consumer and it is the workflow pane. And in `--accessible` mode the overlay opens and renders
**zero rows** — a named empty dialog.

---

## What I ran

Isolated daemon on `CODYPENDENT_DATA_DIR=/tmp/review-codegraph/data`,
`CODYPENDENT_SOCKET=/tmp/review-codegraph/cg.sock`, against a scratch git repo
`/tmp/review-codegraph/repo` (3 `.rs` files, one `Router::decide` method, one `#[cfg(test)]` module).
Binaries: the orchestrator's `target/debug/{codypendent,codypendentd}` (v0.5.1+c255bec8b175).
No workspace `cargo` command was run.

A stub OpenAI-compatible server (`/tmp/review-codegraph/stub.py`, SSE streaming, 127.0.0.1:8740)
logged **every** request body and replied with scripted tool calls, so the "advertised tool array"
and "tool result text" below are the real wire bytes, not inference. A pty driver
(`/tmp/review-codegraph/pty_drive.py`, `TIOCSWINSZ` 200x60) rendered real TUI frames.

---

## Findings, by user-visible consequence

### F1 — In the default Build mode, every `graph.*` question answers `no results`. (class **c**)

`crates/runtime/src/agent.rs:4282`

```rust
Some(graph) => match graph.ask(&run.repository, question).await {
```

`run.repository` is set from the run's **operating tree**, not its repository identity —
`crates/codypendentd/src/executor.rs:931-938`:

```rust
let operating_tree = binding.worktree.clone();
let mut ctx = RunContext::new(
    launch.session_id, launch.run_id, launch.objective.clone(), launch.mode,
    operating_tree.clone(),
    operating_tree,
);
```

For a Build run (`run_writes_to_worktree(AgentMode::Build) == true`,
`executor.rs:2833-2835`) that tree is a **linked git worktree outside the checkout**.
`PoolCodeGraph::ask` then re-derives identity from it — `crates/codypendentd/src/scan.rs:735`,
`repository_id_for` → `git rev-parse --show-toplevel`, which inside a linked worktree returns the
worktree itself (verified: `/tmp/wtprobe/wt`). Different path, different SHA-256, different
`RepositoryId`, zero rows.

The file two lines above the bug says exactly this must not happen —
`crates/codypendentd/src/executor.rs:923-926`:

> *"Repository IDENTITY (the code graph, curated memories, and the GitHub target) stays the run's
> repository `R` … never conflated with this policy read root."*

**Measured.** Same repository, same daemon, same question, two modes:

```
### mode=build  (the DEFAULT)
[tool result: graph.callers_of]
callers of `Router::decide`
no results

[tool result: graph.tests_covering]
tests covering `src/lib.rs` (depth 2)
no results

### mode=explore
[tool result: graph.callers_of]
callers of `Router::decide`
resolved to: Router::decide (src/lib.rs)
2 results
  function boot — src/lib.rs @64f47a27…+workdir
  function uncommitted_symbol_plugh — src/lib.rs @64f47a27…+workdir
```

The two ids, computed from `stable_repository_id` (`codegraph.rs:49-54`) over the two paths:

```
scan wrote under (repo root):       1b3c7b3b-1440-0ad2-89ce-d89d2edb000b
Build-mode tool queries (worktree): 541c2e0f-99ca-a2ae-6502-1c15cac330df
DB actually contains:               1b3c7b3b-1440-0ad2-89ce-d89d2edb000b
```

And the worktree, captured live during a Build run:

```
$ git worktree list
/tmp/review-codegraph/repo                                         64f47a2 [master]
/tmp/review-codegraph/codypendent-worktrees/repo/run-65dc16b1fd5e  64f47a2 [codypendent/run-65dc16b1fd5e]
```

**User-visible consequence.** Build is the default for `codypendent run --mode` and for the TUI
(`crates/tui/src/state.rs:2583`, whose own mode card reads *"full worktree access … (the
default)"*). A developer asks "who calls `Router::decide` before I rename it", the model reaches
for exactly the tool the description tells it to use *"before changing a signature"*, and gets
`no results` — indistinguishable from "nothing calls it". The honest answer is "I asked the wrong
repository." There is not even a `did you mean:` line, because the near-miss fallback
(`nearby_symbols`, `codegraph.rs:1063`) queries the same empty repository id.

**This is the round's cleanest illustration of the brief's thesis.** The daemon's own live test
`crates/codypendentd/tests/codegraph_live_it.rs:198` calls `codegraph::answer(&pool, repository,
…)` with a `RepositoryId` it derived from the **repository root**. It is green. Production reaches
the same function through `graph.ask(&run.repository, …)` with the **worktree**. Done was scored at
the library boundary; the last wire is on the wrong terminal.

### F2 — The agent's own edits are written outside every watch, so they never enter the graph. (class **c**)

`crates/codypendentd/src/executor.rs:557` arms the watcher over the **repository**
(`ensure_watching(repository, root)`), and `scan::arm_watcher` explicitly normalises to the
checkout top level (`crates/codypendentd/src/scan.rs:371`). The Build-mode agent writes to the
worktree, which is a sibling directory.

**Measured** — scripted `workspace.write_file` followed by `graph.callers_of` on the symbol it
just wrote:

```
[tool result: workspace.write_file]
created /tmp/review-codegraph/codypendent-worktrees/repo/run-80765e85614b/src/agentmade.rs (45 bytes)

[tool result: graph.callers_of]
callers of `agent_wrote_this_grue`
no results

$ find /tmp/review-codegraph -name agentmade.rs
/tmp/review-codegraph/codypendent-worktrees/repo/run-80765e85614b/src/agentmade.rs
$ # code_nodes matching '%grue%', 6s later: 0
```

Two independent causes stack here — the write is never *observed* (F2) and the query is aimed at
the wrong repository anyway (F1) — so fixing either alone still leaves the scenario broken.

`crates/codypendentd/src/scan.rs:343-345` states the goal: *"a file edited during a session —
including by the agent's own `edit_file` — never enters the graph"*. For a human editor that is now
false (good). For the agent it is still true.

### F3 — The `--accessible` code-graph overlay renders no content. (class **c**)

Reachable now — `crates/tui/src/palette.rs:209-215` adds `/edges  Code-graph edges`, and the
accessible client maps `"/"` to `Action::OpenPalette` (`accessible.rs:917`). But
`crates/tui/src/accessible.rs` gives `Overlay::Edges` only a *name*
(`overlay_name`, `accessible.rs:823`) and no content arm, unlike `Overlay::Workflow`
(`accessible.rs:560`), `Overlay::Blackboard` (`:571`), `Overlay::Kanban` (`:578`) and
`Overlay::CouncilBrowser` (`:609`), which all emit counts and rows.

**Measured**, driven from `repo/crates/demo` with 28 edges in the graph:

```
command> /
command> edges
command> enter
--- accessible update ---
Open dialog: code graph
Controls: up, down, Enter, Esc, help, quit
command>
```

Waiting six seconds and pressing `down` changed nothing. A screen-reader user finds the code graph
and is told only that it is open.

### F4 — Still an edge table, not a viewer; the DAG renderer has still never seen a `CodeNode`. (class **c**)

`crates/tui/src/dag.rs` is a good lane-layout engine. Its module doc still reads *"ASCII DAG layout
for the workflow pane (rubric 5)"* (`dag.rs:1`), and `lay_out`'s only consumer in the workspace is
`crates/tui/src/render.rs:6579` (`workflow_lanes`), fed `WorkflowNodeCard`.

The code-graph surface is `render_edges` (`crates/tui/src/render.rs:6001`): a two-column modal —
list of `relation` / `from → to`, plus a detail pane — paginated at `EDGE_PAGE_SIZE = 100`
(`crates/tui/src/state.rs:26`), ordered `f.qualified_name COLLATE NOCASE`
(`crates/cli/src/tui.rs:6838-6840`). Real frame, captured from a pty:

```
┌ Code graph (1–28 of 28) ───────────────────────────────────────┐
│› calls                          │Edge                          │
│    boot → Router::decide        │  from: boot                  │
│  calls                          │  to: Router::decide          │
│    boot → to_string             │  relation: calls             │
│  contains                       │  confidence: 0.45            │
│    crates/demo/src… → main      │                              │
│  defines                        │Evidence                      │
│    crates/demo/src… → main      │  kind: syntax_inferred        │
│  calls                          │  source: artifact 599e02a0-… │
│    Router::decide → util::score │(src/lib.rs#329-342)          │
│  …                              │  revision: 64f47a27…+workdir │
│  ↑/↓ edge · / search · PgUp prev · PgDn next · Esc close       │
└────────────────────────────────────────────────────────────────┘
```

No layout, no traversal, no expand/collapse, no "callers of the focused node", no jump-to-source.
The controls line is the whole interaction model. It is *"paginated alphabetical rows"*, precisely.

Two aggravating details from the same capture: 20 of those 28 rows are `contains`/`defines`
structural pairs (every symbol appears twice), so the call graph a user actually wants is 7 rows
buried in 21 rows of scaffolding; and the alphabetical order by source symbol means a chain
(`boot → Router::decide → util::score`) is scattered across the page rather than adjacent.

**What does work here:** the subdirectory case. `crates/cli/src/tui.rs:2963` and `:6027` now use
`codypendent_knowledge::anchor_repository_id`, which resolves the git toplevel first
(`crates/knowledge/src/skills.rs:65-69`) exactly as `scan::repository_id_for` does. The frame above
was rendered from `repo/crates/demo`, two levels down, and shows all 28 edges. Last round's F5 is
repaired.

### F5 — The graph tools are advertised only when a trigram ranker picks them, and the word "graph" belongs to a different tool. (class **c**)

They *are* declared (`crates/runtime/src/agent.rs:6666-6703`, with real schemas — I read them off
the wire) and offered whenever the seam is wired (`agent.rs:1884-1894`). But advertisement is
narrowed by `select_builtin_tools` (`agent.rs:2116-2195`) to `ALWAYS_ADVERTISED_TOOLS` plus the top
`builtin_top_k`. `ALWAYS_ADVERTISED_TOOLS` (`agent.rs:5307-5315`) holds seven names and no
`graph.*`; `DEFAULT_BUILTIN_TOP_K = 8` (`crates/runtime/src/models.rs:238`).

**Measured** — the `tools` array of the first `/v1/chat/completions` request, per objective:

```
objective: "Which symbols call decide? Use the code graph."
  15 tools, graph.* present: ['graph.callers_of','graph.blast_radius','graph.tests_covering']

objective: "the CI is red, please fix the failing tests"
  11 tools, graph.* present: ['graph.tests_covering']

objective: "update the changelog and bump the version number for the next release"
  11 tools, graph.* present: NONE
```

Competing for a slot is a defensible design for a specialist tool. What is not defensible is that
`graph.*` has **no entry in the curated catalog at all**: `crates/knowledge/src/builtin.rs` holds
27 `tool(...)` registrations and not one is `graph.*`, so the family ranks on its description and
its dotted-name segments alone, with no intents or keywords — the very mechanism `agent.rs:5322-5324`
calls *"the whole value of keeping the two catalogs in sync"*.

Worse, the keyword `"graph"` in that catalog is owned by **`workflow.query`**
(`crates/knowledge/src/builtin.rs:247`: `&["workflow","query","dag","graph","persisted","run"]`).
So asking about the code graph surfaces the workflow-DAG tool. In my probe run whose objective was
literally *"Which symbols call decide? Use the code graph."*, the context card's `=== TOOLS ===`
block opened with:

```
tool workflow.query [safe, first-party] — Inspect a persisted executable workflow run: … one run's
DAG, node state, dependencies, and cost.
```

…and never mentioned `graph.callers_of` at all — in any of the eight runs I drove. The model is
shown the schemas (sometimes) but is never *told in prose* that a code graph is queryable.

### F6 — A full rescan of a dirty tree stamps working-tree symbols with a commit they were never in. (class **c**)

`crates/codypendentd/src/scan.rs:82` — `scan_repository` takes `head_revision(&root)` (bare `HEAD`),
then reads every file from the **working tree** (`:190`). The incremental path is careful to be
honest about this — `working_tree_revision` (`:676-683`) appends `+workdir` precisely so *"the graph
says out loud that it is describing an uncommitted tree"* (`:616-619`). The full path never does.

**Measured.** After a full rescan of a tree with four uncommitted changes:

```
$ git status --porcelain
 D crates/demo/src/main.rs
 M src/lib.rs
RM src/util.rs -> src/scoring.rs
?? newcrate/

# code_nodes:
brand_new_symbol_xyzzy   | newcrate/src/lib.rs | 64f47a2725118b3c8482d9105f44d9a0f962f7b7
uncommitted_symbol_plugh | src/lib.rs          | 64f47a2725118b3c8482d9105f44d9a0f962f7b7
rapid_8                  | src/scoring.rs      | 64f47a2725118b3c8482d9105f44d9a0f962f7b7
```

None of those three symbols exists at `64f47a27`; `newcrate/` is not even tracked. The revision
column is printed to the user in the TUI edge detail (`render.rs`, `revision:` row — visible in the
F4 capture) and to the model in every `GraphAnswer::render` line (`codegraph.rs:899-905`). The same
symbol flips between `…` and `…+workdir` depending only on which code path folded it last, with no
change to the file.

### F7 — `SCAN_FILE_CAP` truncates the authoritative graph and reports a clean number. (class **c**)

`crates/codypendentd/src/scan.rs:28` — `SCAN_FILE_CAP = 2000`. The `info!` at `:109-115` logs
`files = scanned` with no cap indication, and nothing surfaces the truncation to a user at all.
Last round marked this latent. **I made it bite.**

I added 5,000 `.rs` files (a plausible generated-code or vendored tree). The watcher correctly
detected a bulk change and rebuilt:

```
INFO scan: code-graph watcher: bulk change, rebuilding repository=1b3c7b3b-… changed=5000 dropped_events=0
INFO scan: code-graph scan complete repository=1b3c7b3b-… revision=64f47a27… files=2000 nodes=4020
```

Repository truth: **5,004** `.rs` files. Graph: 2,000. And the sorted walk (`collect_rust_paths`,
`:201-241`) is deterministic in the wrong direction — `src/bulk/*` sorts before `src/nested/*`, so
`src/nested/deep/mod.rs`, which had been correctly folded minutes earlier, **silently vanished from
the graph while still present on disk**:

```
=== real source files still in the graph ===
newcrate/src/lib.rs
src/lib.rs
src/scoring.rs                 ← src/nested/deep/mod.rs is gone
=== symbols matching '%frotz%' (defined in src/nested/deep/mod.rs) ===
0
=== file exists on disk ===
file EXISTS on disk
```

`graph.tests_covering src/nested/deep/mod.rs` would now answer "no results" for a file that exists.
The log line a maintainer would read says `files=2000`, which is true and useless.

This is self-healing in the good direction: after `rm -rf src/bulk` the watcher rebuilt and all four
real files returned (28 nodes). But a repository that is *genuinely* over 2,000 files never heals.

### F8 — A new top-level directory is invisible until something else changes. (class **c**)

`crates/codypendentd/src/scan.rs:553-556` claims:

> *"A new top-level directory (a crate added mid-session) is not covered by any recursive watch yet
> — the root's own non-recursive watch is what reported it. One `read_dir` of the root per batch
> closes that gap."*

The root watch cannot report it. Every raw event passes `is_candidate_path` (`scan.rs:410-419`)
**before** the channel (`scan.rs:383`), and that predicate requires a `.rs` extension — a directory
creation has none, so it is dropped and no batch is ever started, so the sweep never runs.

**Measured**, three steps:

```
mkdir -p newcrate/src && echo 'pub fn brand_new_symbol_xyzzy() -> u32 { 42 }' > newcrate/src/lib.rs
sleep 6  → nodes matching '%xyzzy%': 0
printf '\n// poke\n' >> src/lib.rs   # forces a batch, which runs the sweep
sleep 5  → nodes matching '%xyzzy%': 0        ← still invisible
printf '\npub fn xyzzy_two()…' >> newcrate/src/lib.rs
sleep 5  → brand_new_symbol_xyzzy, xyzzy_two  ← both appear
```

So a new crate added mid-session enters the graph only after *two* unrelated things happen: some
other watched file changes, and then a file in the new directory is touched again. Adding a crate
and asking about it is not an exotic workflow.

A *nested* new directory inside an already-recursive subtree is fine — `src/nested/deep/mod.rs`
appeared within 6 seconds, unaided.

### F9 — `symbol_changed` outbox events are still discarded; retrieval still cannot see the graph. (class **b**, unchanged from last round)

`crates/knowledge/src/codegraph.rs:307-310` enqueues one `SymbolChanged` per durable node, inside
the write transaction. `crates/knowledge/src/retrieval/persist.rs:225` routes everything that is not
`registry_item_changed` to:

```rust
fn handle_non_registry_event(row: &OutboxRow) {                     // persist.rs:183-185
    tracing::trace!(kind = %row.event_kind, entity = %row.entity_id,
                    "no derived index consumes this outbox kind yet");
}
```

Live database after this session's work:

```
memory_changed         |    5 total |   5 processed
registry_item_changed  |   29 total |  29 processed
symbol_changed         | 4071 total | 565 processed
```

`grep -rn "code_nodes\|code_edges\|codegraph" crates/knowledge/src/retrieval/` returns **nothing**.
The graph and the retrieval funnel share a database file and nothing else. Outcome 14's second
clause — "and retrieval stay current" — has no mechanism, current or stale.

### F10 — Capabilities with no production consumer (class **b**)

Re-verified by grep across `crates/`, excluding each capability's own module and
`crates/knowledge/tests/`:

| capability | file:line | production consumers |
|---|---|---|
| `hierarchical_map` | `repomap.rs:247` | **0** (a `lib.rs:84` re-export only) |
| `upsert_semantic_edges` | `codegraph.rs:469` | **0** (4 refs, all `semantic_it.rs`) |
| `changed_between` | `codegraph.rs:~532` | **0** (a `lib.rs:79` re-export only) |
| `LanguageAdapter` / `RustAdapter` / `ScriptAdapter` | `adapter.rs` | **0** (a `lib.rs:75-76` re-export only) |
| `codegraph::watch` (the `&Path` convenience wrapper) | `codegraph.rs:1299` | **0** — the daemon calls `codegraph::watcher` (`:1282`) directly |

`callers_of`/`blast_radius`/`tests_covering` have graduated out of this table — they are reached
through `codegraph::answer` from `PoolCodeGraph` (`scan.rs:729-740`) from the runtime tools. That
is the round's real repair.

### F11 — `change_surface` is still a hardcoded empty vector rendered into every run. (class **a**, unchanged)

`crates/knowledge/src/repomap.rs:153` — `change_surface: Vec::new()`. Every repository map the model
receives ends with the literal line `change surface: (none)`, confirmed in all eight of my runs.
The populated render path (`repomap.rs:199-204`) is reachable only from a unit test. With the live
watcher now folding working-tree edits and stamping them `+workdir`, the data needed to populate
this field exists for the first time — the diff between `…` and `…+workdir` nodes *is* the change
surface — and is still thrown away.

### F12 — Non-Rust source still cannot reach the graph, and the script scanners are unchanged. (class **a/b**, unchanged)

Two independent `.rs`-only gates, now: `collect_rust_paths` (`scan.rs:223`,
`path.extension().is_some_and(|ext| ext == "rs")`) for the full scan, and `is_candidate_path`
(`scan.rs:411`) for the watcher. `build_file_graph` (`codegraph.rs:1485`) still unconditionally
stamps `RUST_MEDIA_TYPE` and the Rust grammar, with no language dispatch.

`scan_python` (`adapter.rs:482-509`) still skips every indented line (so **all methods**) and misses
`async def` (`"async def f"` does not `strip_prefix("def ")`). `scan_typescript`
(`adapter.rs:512-538`) still misses `interface`, `type`, `enum`, `const`, arrow functions, and
`export async function`. Every `ParsedSymbol` from both carries `signature_hash: None`
(`:495, :503, :524, :532`), so `changed_between` could never observe a Python/TypeScript signature
change even if it had a caller. Moot in production either way — no non-`.rs` file is ever offered.

### F13 — Genuinely repaired since last round (recorded so the next reviewer does not re-litigate)

* **F1/F2 (watcher unwired, HEAD-gated graph)** → fixed. `scan::arm_watcher` (`scan.rs:362`) is
  called from `RuntimeExecutor::ensure_watching` (`executor.rs:596-611`) on the live
  `ensure_scanned` success path (`executor.rs:557`). Log from a cold daemon:
  `INFO scan: code-graph watcher armed repository=1b3c7b3b-…`, 5 ms after the scan.
* **F5 (TUI derived a different repository id)** → fixed via `anchor_repository_id`
  (`cli/tui.rs:2963`, `:6027`). Verified by rendering 28 edges from two directories down.
* **F6 (two concurrent clear-and-rebuild scans)** → fixed by `scan::lock_repository`
  (`scan.rs:46-59`) plus a re-check under the lock (`executor.rs:536-546`). Across this whole
  session: `database is locked` occurrences **0**, `code-graph scan failed` **0**, full scans **3**
  (one warm-up, two overflow rebuilds).
* **F10 (blast radius walked across repositories then dropped the hops)** → fixed;
  `direct_caller_ids` (`codegraph.rs:757-770`) now joins `code_nodes` and filters
  `n.repository = ?` **during** the walk.
* **F4 discoverability** → partially fixed by the `/edges` palette entry (`palette.rs:209-215`).
  `'G'` (`input.rs:371`) is still the only key binding, and `Action::OpenEdges` still has exactly
  those two production routes.

### F14 — Watcher robustness, as measured

Asked directly by the brief. All measured against the live daemon.

| scenario | result |
|---|---|
| single uncommitted edit | folded in ~1 s (`22:45:54` edit → `22:45:55.147` fold log) |
| **8 rapid appends at 150 ms**, each inside the 400 ms debounce | **all 8 landed, including the last** (`rapid_1 … rapid_8`) |
| `git mv src/util.rs src/scoring.rs` | old path retired to 0 nodes, new path folded (11 nodes) — same batch |
| outright `rm` of a file | retired to 0 nodes |
| revert (`git checkout --`) | symbol retired within 4 s |
| new **nested** dir in a watched subtree | folded within 6 s |
| new **top-level** dir | **F8** — invisible until two further edits |
| 5,000 files at once (channel cap 4,096) | `dropped_events=0`; the debouncer drained faster than the producer, so the `pending.len() > WATCH_BATCH_CAP` arm fired, not the overflow arm. Collapsed to one rebuild — which then hit **F7** |
| recovery after `rm -rf` of the 5,000 | full graph restored (28 nodes, all 4 files) |

**No debounce drops the last edit.** I read the loop closely for this
(`scan.rs:486-504`): events arriving while a batch is being folded stay in the bounded channel and
are picked up by the next `rx.recv()`; `WATCH_MAX_WINDOW` (3 s) only caps how long collection runs,
it never discards. The `folded: HashMap<path, sha256>` dedup (`scan.rs:485`, `:630-643`) skips a
reparse only when the bytes are byte-identical to what was folded, which is correct. The one real
overflow risk (`try_send` failure at `scan.rs:383`) is counted and converted into a full rescan
(`:510-511`), with a deferred-rebuild path outside the lock (`:565-585`) so a cooldown cannot strand
a dropped event forever. I did **not** manage to force an actual `dropped_events > 0`.

---

## The pattern

Last round the gap was between the *library* and the *product*: engines built and never called.
That gap is largely closed — the watcher is armed, the tools are declared and dispatched, the TUI
derives the right id. What replaced it is subtler and, for a user, worse: **every remaining defect
is a place where two code paths derive the same fact differently, and the disagreement is reported
as an empty result rather than as a disagreement.** The graph tool derives repository identity from
the worktree while the scanner derives it from the checkout (F1); the agent writes to the worktree
while the watcher watches the checkout (F2); the full scan stamps `HEAD` while the incremental fold
stamps `HEAD+workdir` for the identical bytes (F6); the event filter tests `.rs` while the
directory sweep it gates needs directories (F8); the schema catalog knows `graph.*` while the
retrieval catalog does not, and hands the word "graph" to the workflow tool instead (F5). In every
case both halves are individually correct, individually tested, and individually documented — and
in every case the seam between them surfaces as `no results`, `(none)`, `files=2000`, or an empty
dialog, which the user reads as *an answer about their code* rather than as *a failure of the
system to ask itself the right question*. The previous round's slogan should be updated: the wire
is now attached, and it is attached to a terminal that looks identical to the right one.

---

## What I did **not** verify

* **`--accessible` code-graph content beyond the overlay header.** I drove the real client and read
  `overlay_name`/`append_overlay`, but I did not exhaustively prove no other code path could emit
  edge rows; I read the whole `overlay` match arm and found no `Overlay::Edges` case. Reported as
  observed (empty screen) plus inferred (no arm exists).
* **`dropped_events > 0` (true notify-queue overflow).** My 5,000-file burst was drained faster
  than it was produced. The overflow → full-rescan and the deferred-rebuild-after-cooldown paths
  (`scan.rs:510-511`, `:565-585`) are read, not run.
* **Multi-repository isolation of `blast_radius`** (last round's F10). The fix at
  `codegraph.rs:757-770` is read and looks right, but producing a genuine cross-repository edge
  needs the semantic layer, which has no production constructor (F10 table).
* **The `graph.*` tools against a real model.** Every tool call above was scripted through my stub,
  so I proved *the tool answers correctly when called*, not *a real model chooses to call it*. The
  advertisement measurements in F5 are the closest proxy and are real wire bytes.
* **`SCAN_FILE_CAP` on a repository genuinely over 2,000 files.** I forced the cap with synthetic
  files; the truncation and the misleading log line are observed, the claim that a large real
  monorepo is permanently partial is inferred from the same code path.
* **Whether `codypendent attach` / a long-lived TUI session refreshes its edge overlay** when the
  watcher folds a change, versus only on re-open. I re-opened the overlay each time. `load_edge_page`
  (`cli/tui.rs:6783`) queries SQLite per `Intent::SearchEdges`, so a re-open is always current; a
  *held-open* overlay is untested.
* **`cargo test`.** Per the brief I ran no workspace build or test command; the only compiled
  artifacts used were the orchestrator's `target/debug/{codypendent,codypendentd}`. Disk stayed at
  37% throughout.
