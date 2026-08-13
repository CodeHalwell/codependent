# Vertical: codegraph

Reviewer scope: `crates/knowledge/src/{codegraph,repomap,extractor,observer,context,manifest}.rs`,
`crates/knowledge/src/adapter.rs`, `crates/codypendentd/src/scan.rs`, `crates/tui/src/dag.rs`,
plus a whole-repo hunt for filesystem-watcher code.
Owned outcomes: **5** (DAG viewer for code-context management, user + agent) and
**14** (live code graph — graph + retrieval current *during* a session).

Pinned commit 535a2f5e3848b256536ddee94883dc0010ecdcb8 (v0.4.5). Code unchanged.

---

## Verdicts

**OUTCOME 5: BROKEN** — the code graph is real and correctly parsed, but the user gets a flat
paginated *edge table* (not a DAG) that silently shows nothing whenever the TUI is opened outside
the repo root, and the agent gets no query tool at all: all four named graph queries
(`callers_of` / `blast_radius` / `tests_covering` / `changed_between`) have zero production callers.

**OUTCOME 14: ABSENT** — the filesystem watcher exists as a complete, documented function with
**zero call sites anywhere in the workspace, tests included**; the graph's only refresh trigger is a
*git HEAD change* observed at run launch, so a file edited during a session — including by the
agent's own `edit_file` — never enters the graph. Proven by experiment below.

---

## What I ran

Built workspace artifacts were reused (`./target/debug/{codypendent,codypendentd}`).

* Scratch git repo seeded from `crates/routing/` (7 Rust files, 2410 LOC), isolated daemon on
  `CODYPENDENT_DATA_DIR=<scratch>` / `CODYPENDENT_SOCKET=/tmp/cg-probe.sock`.
* `codypendent run --objective … --repo <scratch> --jsonl` to drive `CreateSession` + `StartRun`.
* Direct SQLite reads of `code_nodes` / `code_edges` / `index_outbox` / `registry_items`.

**Resulting graph (plausible, and the parser is genuinely good):**

| | |
|---|---|
| nodes | **346** |
| edges | **798** |
| kinds | external_dependency 186, test 60, method 48, type 28, module 12, file 7, function 2, constant 1 |
| relations | calls 403, defines 151, contains 151, imports 89 |
| confidence | 403 edges @ 0.45 (`SYNTAX_CALL_CONFIDENCE`), 391 @ 1.0 — all `syntax_inferred` |
| per-file | router.rs 110, arms.rs 59, policy.rs 48, classify.rs 36, profile.rs 35, lib.rs 34, capability.rs 22 |

These counts are believable for the input: ~50 symbols/file, `#[cfg(test)]` modules correctly
classified as `Test`, `impl` blocks correctly scoped under the self type. **The tree-sitter Rust
walk in `codegraph.rs` is the one part of this vertical that works end to end.** Everything
downstream of it is the problem.

---

## Findings

### F1 — The filesystem watcher is constructed by nobody. (class **b**)

`crates/knowledge/src/codegraph.rs:746-753`

```rust
pub fn watch<F>(root: &Path, handler: F) -> Result<notify::RecommendedWatcher, CodeGraphError>
```

A complete recursive `notify` watcher: dependency declared (`Cargo.toml:138`, `crates/knowledge/Cargo.toml:34`),
error variant carved out (`codegraph.rs:102-104` `CodeGraphError::Watch`), doc comment describing
the intended debounce/ignore policy for its caller (`codegraph.rs:739-745`).

**Call sites: none.** An exhaustive grep of `crates/ benches/ examples/ sdk/ evals/` for `watch`
returns only `tokio::sync::watch` channels (approvals, shutdown, cancellation), `watch_focused_doc`
/ `watch_focused_workflow` TUI helpers, and `watch_process_resources` in `ui-host`. Not one
reference to `codegraph::watch`. Its own doc comment concedes it: *"tests never start one (no
background threads in tests)"* — so it is neither wired nor exercised. `CodeGraphError::Watch` is a
variant that can never be constructed by any reachable code path.

This is exactly the class-(b) shape outcome 14 describes, in its purest form: the engine is
written, typed, documented and dependency-approved; the final wire was never attached.

The module's own header is honest about it — `codegraph.rs:61-62`: *"The Phase-2 pipeline rebuilds
the graph with a full working-tree scan on each startup (there is no live per-file watcher yet)."*

### F2 — The graph is gated on git HEAD, so an edit made during a session never lands. (class **c**)

`crates/codypendentd/src/executor.rs:505-511` (`ensure_scanned`), gate value from
`crates/codypendentd/src/scan.rs:111-123` (`head_revision` = `git rev-parse HEAD`).

```rust
let revision = scan::head_revision(root);
let folded_current = { seen.get(&repository) == Some(&revision) };
if folded_current { return; }
```

**Experiment (decisive).** Starting from a scanned repo at HEAD `fdafdd0`, 345 nodes:

1. Appended `pub fn uncommitted_symbol_plugh() -> u32 { 7 }` to `src/lib.rs`. Did not commit.
2. Launched **two** further runs against the repo.
3. Result: `plugh` present = **0**. Total nodes still **345**.

HEAD does not move when a file is edited, so the gate never opens. Every subsequent run's opening
context, and the TUI's "Code graph" overlay, keep serving the pre-edit snapshot for the whole
session. The agent's own `edit_file` / `write_file` / `git.apply_patch` writes are exactly this
case: an agent that renames a function spends the rest of the session reasoning about a repository
map naming the old symbol, with no signal that the map is stale.

**User-visible consequence:** user asks the agent to refactor `Router::decide` into
`Router::choose`; every later turn's repository map still says `method decide`, and the code-graph
overlay still lists `decide` edges, until the user commits *and* starts a new run.

There is also no manual escape hatch. `codypendent index rebuild`
(`crates/cli/src/commands.rs:575-611`) rebuilds only the BM25/vector retrieval indexes; its own
help text says so (`crates/cli/src/main.rs:118`, *"This is search, NOT the code graph"*). Nothing
in the CLI or the TUI can force a code-graph refresh.

### F3 — No tool exposes the graph to the model. (class **b**)

The store has four purpose-built, working queries:

| query | file:line | production consumers |
|---|---|---|
| `callers_of` | `crates/knowledge/src/codegraph.rs:576` | **0** |
| `blast_radius` | `crates/knowledge/src/codegraph.rs:602` | **0** |
| `tests_covering` | `crates/knowledge/src/codegraph.rs:618` | **0** |
| `changed_between` | `crates/knowledge/src/codegraph.rs:532` | **0** |
| `upsert_semantic_edges` | `crates/knowledge/src/codegraph.rs:426` | **0** |
| `hierarchical_map` | `crates/knowledge/src/repomap.rs:247` | **0** |

Every one of them is referenced only by `crates/knowledge/tests/semantic_it.rs`. (The single
exception in the neighbourhood is `symbol_snapshot`, `codegraph.rs:512`, which *is* consumed by
`crates/codypendentd/src/docs_job.rs:249`.)

I dumped the live registry the model actually sees — **21 items**: `blackboard.post/query`,
`council.create/result/run`, `git.apply_patch`, `git.diff`, `memory.remember`, `repository.test`,
`shell.run`, `task.create/list/move/update`, `workflow.create/query/run`, `workspace.read_file`,
`workspace.search`, plus the `fix-ci` / `update-docs` commands. **There is no `graph.*` tool.**
`crates/runtime/src/tools/` contains 22 modules and not one of them touches `codegraph`.

The model's *only* exposure to the code graph is the static `RepositoryMap::render()` text block
folded once into the run's opening note (`crates/knowledge/src/context.rs:277`). It cannot ask
"who calls this", "what breaks if I change this", or "which tests cover this file". Outcome 5's
"accessible to the agent" is unmet at the tool layer, not at the store layer — which is the
cheapest possible fix and the highest-value finding here.

### F4 — The "DAG viewer" is a flat edge table; the actual DAG renderer is wired to workflows. (class **c**)

`crates/tui/src/dag.rs` is a genuinely good ASCII lane-layout engine (`lay_out`, `MAX_LANES`,
box-drawing connectors, bounded degradation). Its **only** consumer is
`crates/tui/src/render.rs:6384 workflow_lanes`, which feeds it `state::WorkflowNodeCard` —
workflow steps and their `depends_on`. It has never seen a `CodeNode`. Its own module doc
(`dag.rs:1`) says "ASCII DAG layout for the workflow pane".

The code-graph surface is `render_edges` (`crates/tui/src/render.rs:5806`): a modal listing 100
edges per page (`crates/tui/src/state.rs:26 EDGE_PAGE_SIZE`) as `from → to`, relation, confidence,
evidence, revision, with a substring filter. No layout, no traversal, no expand/collapse, no
"show callers of the focused node", no jump-to-source. It is a table, not a viewer, and certainly
not a DAG. For 798 edges that is 8 pages of alphabetically-sorted rows.

Discoverability is one undocumented keystroke: `'G'` in Normal mode
(`crates/tui/src/input.rs:371`). The `--accessible` client has no command that produces
`Action::OpenEdges` at all — `map_accessible_input` (`crates/tui/src/accessible.rs:902-975`)
maps no line to it, though `accessible.rs:820` dutifully labels the overlay "code graph" if you
somehow get there. **The code graph is unreachable in accessible mode.**

### F5 — SILENT FILTER: the TUI derives a different repository id than the daemon stored under. (class **c**)

* Daemon: `crates/codypendentd/src/scan.rs:178-182` — `repository_id_for` = `git rev-parse --show-toplevel` → canonicalize → `stable_repository_id`.
* CLI/TUI: `crates/cli/src/tui.rs:249` canonicalizes the *given* directory only; `tui.rs:394` stringifies it; `tui.rs:5906` and `tui.rs:6949-6950` then call `stable_repository_id(Path::new(repository))` directly. **No `--show-toplevel` step.**

The bare `codypendent` invocation passes `std::env::current_dir()` (`crates/cli/src/main.rs:976`).

**Proven against the live database**, running the exact SQL from `load_edge_page`
(`crates/cli/src/tui.rs:6632-6635`):

```
TUI opened at repo root    id=dd2e862d-8667-a03e-1f33-ca0267795e1b   edges shown = 796
TUI opened at repo/src     id=976abcc7-45b5-74c8-6e02-7b0737f7f038   edges shown = 0
```

**User-visible consequence:** developer runs `codypendent` from `myrepo/crates/routing/` (the
normal thing to do). The daemon indexes 798 edges under the toplevel id. The overlay renders
`render_empty_edges` (`render.rs:6037`) with the title **"No relationships indexed yet"** and the
line "Edges appear here as Codypendent gathers evidence across the repository." Both statements are
false; the real answer is "you queried the wrong repository id." The comment at `tui.rs:5904`
claims the id is *"derived from the same canonical path the daemon uses"* — it is not.

The same mismatch silently empties the memories and documents browsers, which share `scopes` built
from that id (`tui.rs:5906-5912`).

### F6 — A single run fires two concurrent full clear-and-rebuild scans of the same repository. (class **c**)

Two independent trigger paths, neither aware of the other's in-flight work:

* `crates/daemon/src/server.rs:2632` — on `CreateSession`, `maybe_scan_repository` →
  `RuntimeExecutor::ensure_repository_scanned` (`crates/codypendentd/src/executor.rs:2524`), which
  `tokio::spawn`s `ensure_scanned`.
* `crates/codypendentd/src/executor.rs:2254` — `spawn_run` awaits `ensure_scanned` directly.

`codypendent run` issues `CreateSession` then `StartRun` back to back, so both fire together.
`ensure_scanned` deliberately releases its `std::Mutex` before the await
(`executor.rs:505-511`, "a `std` mutex is never held across an await"), so both observe
"not folded", and both then run `clear_repository` (wipes every node and edge —
`crates/knowledge/src/codegraph.rs:67`) followed by a 7-file rebuild.

**Reproduced on every fresh daemon (3/3):**

```
WARN codypendent_codypendentd::executor: code-graph scan failed; a later run will retry
     repository=dd2e862d-… error=sqlite error: (code: 5) database is locked
INFO codypendent_codypendentd::scan: code-graph scan complete … files=7 nodes=346
```

Two consequences, both user-visible:

1. **The guard the whole design rests on is defeated on the first run.** `ensure_scanned`'s error
   arm (`executor.rs:544-546`) does not record the revision, so the repository stays permanently
   "unfolded" as far as one of the two racers is concerned, and every subsequent run re-scans.
2. **The model can be handed a torn repository map.** Because the map is read
   (`crates/knowledge/src/context.rs:277`) while the other racer is between `clear_repository` and
   the end of its rebuild, I observed a run whose opening note carried:
   `module (crate root) — 7 APIs, 0 tests` with no method modules at all, against a graph that in
   fact held 32 crate-root APIs and 60 tests. There is no revision or generation guard on the read —
   `codegraph::nodes` (`codegraph.rs:336`) takes whatever rows exist at that instant.

`scan_repository` (`crates/codypendentd/src/scan.rs:52-66`) takes care to preflight-parse every
file before clearing, precisely so a bad parse cannot leave a half-built graph — and then the
clear+rebuild is issued with no transaction, no lock and no exclusion against a second scanner.

### F7 — DATA PRODUCED BUT NEVER CONSUMED: 476 `SymbolChanged` events into a trace call. (class **b**)

`crates/knowledge/src/codegraph.rs:307-310` enqueues one `KnowledgeIndexEvent::SymbolChanged` per
durable node, inside the same transaction as the graph write, "so the authoritative write and its
`SymbolChanged` events are atomic" (`codegraph.rs:15-19`). `upsert_semantic_edges` does the same
(`codegraph.rs:477`).

The consumer, `drain_outbox` (`crates/knowledge/src/retrieval/persist.rs:200-231`), handles
`registry_item_changed` and routes **everything else** to:

```rust
fn handle_non_registry_event(row: &OutboxRow) {          // persist.rs:183-185
    tracing::trace!(kind = %row.event_kind, entity = %row.entity_id,
                    "no derived index consumes this outbox kind yet");
}
```
…then marks the row processed. My probe database:

```
kind=symbol_changed          total=476  processed=476
kind=registry_item_changed   total=42   processed=42
```

476 atomically-committed change notifications, discarded at `trace` level. **Retrieval never reads
the code graph**: no `code_nodes` / `code_edges` reference exists anywhere under
`crates/knowledge/src/retrieval/`. The second half of outcome 14 — "retrieval stays current" — has
no mechanism at all, current or stale: the graph and the retrieval funnel are disjoint subsystems
that happen to share a database file.

### F8 — Language adapters cannot reach the graph, and produce very little when called. (class **a/b**)

Two hard blocks, before any question of quality:

* `crates/codypendentd/src/scan.rs:153` — `collect_rust_sources` accepts only
  `path.extension() == "rs"`. No `.py`/`.ts`/`.tsx`/`.js` file is ever offered to the graph.
* `crates/knowledge/src/codegraph.rs:946-949` — `build_file_graph` unconditionally sets
  `tree_sitter_rust::LANGUAGE`, whatever the path. There is no language dispatch.

Consequently `RustAdapter` and `ScriptAdapter` have **no production constructor**: grep for
`RustAdapter|ScriptAdapter|LanguageAdapter|SemanticCapability` outside `adapter.rs` yields only the
`lib.rs:72-74` re-export and `crates/knowledge/tests/semantic_it.rs`. The whole
`LanguageAdapter` trait — `parse`, `symbols`, `diagnostics`, `build_metadata`, the `cargo metadata`
and `cargo check --message-format=json` parsers — is a test-only surface.

**Quality, when it is called.** The Rust adapter delegates to the real tree-sitter walk and is
good (346 nodes on 7 files). Python and TypeScript are **not tree-sitter** — they are line-prefix
string matching (`adapter.rs:482-538`). I transcribed `scan_python` and `scan_typescript`
line-for-line and ran them on realistic inputs:

```
Python file: 2 classes, 4 defs (one of them `async def` at module scope), 1 constant
  yields -> Widget (Type), make_widget (Function), Service (Type)          # 3 of 7

TypeScript file: interface, type alias, enum, const, arrow fn, 2 fns, 2 classes
  yields -> classic (Function), Widget (Type), Other (Type)               # 3 of 9
```

Specifically dropped, silently: `async def` at module scope (`"async def f"` does not start with
`"def "`, `adapter.rs:490`); **all methods** (indented lines are skipped outright,
`adapter.rs:486`); Python constants; TS `interface`, `type`, `enum`, `const`, arrow functions; and
`export async function` (after stripping `export `, `"async function loadAll"` does not start with
`"function "`, `adapter.rs:519`) — async functions being roughly half of real TypeScript. Every
`ParsedSymbol` from these scanners carries `signature_hash: None` (`adapter.rs:495`, `:503`,
`:524`, `:532`), so `changed_between` and the docs-staleness detector can never observe a
Python/TypeScript signature change even in principle.

`scan_typescript` also does not skip indented lines, so a `class` or `function` nested inside a
function body is reported as a top-level symbol.

The tests that cover this (`crates/knowledge/tests/semantic_it.rs:265-302`) assert on
`"def charge(x):\nclass Wallet:"` and `"export function charge(…)\nexport class Wallet {}"` — the
four inputs that happen to be the only shapes the scanners handle. Green, and evidence of nothing.
"Graceful syntax-only degradation" here means "returns a small, unmarked subset with no indication
that anything was dropped."

### F9 — `change_surface` is a permanently empty field rendered into every run's context. (class **a**)

`crates/knowledge/src/repomap.rs:153` — `change_surface: Vec::new()`, hardcoded, with the field
documented at `:35-37` as "the symbols touched by the active change… Left empty in v1". Every
repository map the model receives therefore ends with the literal line `change surface: (none)`
(confirmed in the live run's opening note). The `render` path for a populated surface
(`repomap.rs:199-204`) is reachable only from `repomap.rs:640` — a unit test.

### F10 — `blast_radius` walks across repository boundaries, then silently drops what it found. (class **c**)

`crates/knowledge/src/codegraph.rs:664-691` — `reverse_reachable` takes a `repository` parameter
and discards it: `let _ = repository;` at `:686`. `direct_caller_ids` (`:694-708`) queries
`code_edges` by `to_node` alone with no repository join. So a BFS seeded in repository A will
traverse an edge into repository B if one exists, and `nodes_by_ids` (`:711`) then filters
`WHERE repository = ?`, dropping those hops without reporting them. Depth is consumed by nodes the
caller never sees. No production caller today (F3), so this is latent rather than live — but it is
the same silent-filter shape as F5.

### F11 — Minor / notes

* `crates/codypendentd/src/scan.rs:21` — `SCAN_FILE_CAP = 2000`. When the cap bites, the
  *authoritative* graph is truncated and the `info!` at `:67-73` reports `files = <capped count>`
  with no indication a cap applied. This repo has 307 `.rs` files, so it does not bite here; a
  larger monorepo gets a silently partial graph. The sorted walk (`:140`) at least makes the
  truncation deterministic.
* `crates/knowledge/src/repomap.rs:10-14` — "public APIs" is every durable symbol; there is no
  visibility column, so private helpers are rendered to the model as public API. Documented, but
  it means the map's headline number ("32 APIs") is not what it says.
* `crates/knowledge/src/extractor.rs` and `observer.rs` are memory-harvest machinery, not code
  graph. Both are genuinely wired: `extract_candidates` / `chronicle_candidates` are consumed at
  `crates/codypendentd/src/executor.rs:1749,1764`. `observer.rs:8-10` still claims "the live
  daemon subscription that feeds it is a later integration step" — stale comment, the wire exists.
  `manifest.rs` is the skill-package loader and touches nothing in this vertical.
* `crates/knowledge/src/codegraph.rs:781` — legacy `code_nodes` rows with a NULL `source_path`
  decode to `""` and are only healed by a full rescan. Harmless given F6 rescans constantly.

---

## The structural pattern

**Everything between the parser and the consumer is a stub with a good comment on it.**

The tree-sitter walk is real, and it is the only thing in this vertical that was ever run against
real input. Immediately downstream, four distinct hand-offs were built to spec, unit-tested, and
never connected:

| producer (works) | intended consumer | actual consumer |
|---|---|---|
| `codegraph::watch` | a debouncing reparse loop | nothing, not even a test |
| `SymbolChanged` outbox rows | a derived index | `tracing::trace!` |
| `callers_of` / `blast_radius` / `tests_covering` / `changed_between` | an agent tool | `semantic_it.rs` |
| `dag::lay_out` | the code-graph viewer | the workflow pane |
| `RustAdapter` / `ScriptAdapter` | a multi-language scan | `semantic_it.rs` |

Each gap is individually cheap to close — a `tokio::spawn` around `watch`, a `graph.*` tool module,
one `--show-toplevel` call in `tui.rs` — and each is *documented as if already closed*, which is
what makes them hard to see. `scan.rs` and `codegraph.rs` carry careful prose about revision
gating, atomic outbox commits, and graceful degradation; the prose describes a system whose last
link is missing in every case. The result is a code graph that is accurate at the moment of a
commit, invisible to the model, empty in the UI from any subdirectory, and frozen for the entire
duration of the session in which it would actually be useful.

---

## What I could not exercise, and why

* **The Rust adapter's async surface (`symbols` / `diagnostics` / `build_metadata`) and
  `codegraph::parse_symbols` on non-Rust source.** Building a probe binary against
  `codypendent-knowledge` needed either the workspace `target/` (held continuously by another
  reviewer's `cargo` — "Blocking waiting for file lock on artifact directory" across ~4 minutes of
  polling) or a private target dir, which twice exhausted the shared filesystem (100% of 252G) and
  had to be killed and deleted. I substituted a line-for-line transcription of `scan_python` /
  `scan_typescript` (pure functions, no I/O, no hidden state) — reported as transcription, not as
  a live run. The claim that tree-sitter-Rust on a `.py` file yields no symbols is *reasoned* from
  `codegraph.rs:1044-1084` (only Rust item kinds are matched; everything else hits
  `_ => pending.clear()`), **not measured**. It is moot for production behaviour either way, since
  `scan.rs:153` never offers a non-`.rs` file.
* **The graphical TUI's code-graph overlay driven through a real pty.** I traced the wire
  completely (`input.rs:371` → `reduce.rs:732` → `Intent::SearchEdges` → `tui.rs:2947` →
  `load_edge_page` `tui.rs:6616` → SQLite → `Action::EdgesLoaded` → `render_edges`
  `render.rs:5806`) and executed the terminal SQL against the live database, which is where the
  F5 emptiness originates. I did not render actual frames; the tui reviewer owns that half.
* **LSP-tier behaviour.** `upsert_semantic_edges` supersession, `SemanticCapability::LspResolved`,
  and the confidence-tier interaction could not be observed because no production code path
  constructs an adapter or produces a `SemanticEdge`. Every edge in the live graph is
  `syntax_inferred`. The supersession logic itself (`codegraph.rs:448-453`, delete-then-insert on
  `(from, to, relation)`) reads correct, and its `skipped` counter is returned rather than
  swallowed — but with no caller, that is unverified.
* **Multi-repository isolation under a shared daemon.** F10 needs two repositories with a
  cross-repository edge, which only the (dead) semantic layer can create.
