# tui-shell — review report

Commit 535a2f5 (v0.4.5). Files read in full: `crates/tui/src/{lib,state,reduce,render,action,
input,dag,markdown,palette,theme,theme_pack,accessible,terminal,remote_ui*}.rs`,
`crates/cli/src/{tui,theme_select,stream,client,connection}.rs`.

**Everything below was RUN, not read.** The TUI was driven under a real pty (`pty.fork` +
`pyte` VT emulation) against a live `codypendentd` in an isolated `CODYPENDENT_DATA_DIR`,
with a local OpenAI-compatible model server so real runs streamed real events. Probe
harness: `<scratchpad>/{drive,sweep,narrow,sizes,chat,folds,crossrun,toolfold,wire,dagrun,
edges}.py`.

---

## Verdicts

**OUTCOME 1: PARTIAL** — the shell, splash, pickers and chat surface are genuinely well
made and every overlay is reachable, but the Help overlay (the product's own
documentation) is mis-formatted and silently drops a third of its content, every browser
hard-cuts its own empty-state copy mid-word, and the first-run gate swallows `/` so the
command palette is unreachable exactly when a new user needs it most.

**OUTCOME 7: PARTIAL** — markdown, tool cards, folds and transcript virtualization are
real and good; but the conversation renders every run stacked while only the *selected*
run's cards are interactive, so from the second message onward every tool card and diff
on screen is permanently un-expandable by keyboard and by mouse.

**OUTCOME 3 (TUI side): WORKS** — `/model` and `/provider` are real, prefilled,
honest about readiness, and the add-model flow reaches a live `/models` list with a
catalog fallback. One header lies about counts.

**OUTCOME 5: PARTIAL** — the only DAG renderer (`dag.rs`) draws *workflow manifests*
and does so live and correctly; the *code-context* graph it is supposed to serve is
rendered as an alphabetical paginated table with no graph layout, and the agent half of
"user + agent" is ABSENT — no code-graph tool exists.

**OUTCOME 20: CONFIRMED (no reader — and no data)** — the TUI has no ledger consumer,
the wire protocol carries no token/cost/latency record, the one cost channel has no
producer outside the eval harness, and the run chronicle is dropped at the destructure.

---

## Overlay / menu inventory (all 50 `Overlay` variants)

Enumerated from `state.rs:212-566` and each one opened under the pty. **Every variant is
reachable**; there is no unreachable overlay dead code. Reach paths:

| Overlay | Reached by | Verified |
|---|---|---|
| `Onboard{Triage}` | auto at boot when `runnable_models` is empty (`cli/tui.rs:575`), then Enter past the splash | yes |
| `Onboard{SkipConfirm}` | `Esc` from Triage | yes |
| `Onboard{Validating}` | submitting a model during onboarding | code |
| `OnboardProviderPicker` | `Enter` on a Triage row | yes |
| `Palette` | `/` on an empty composer, or `/` in `Normal` mode | yes |
| `Help` | palette "Help", or `?` in `Normal` | yes |
| `Issues` | palette "Setup & diagnostics", or the footer `/ diagnostics` chip | yes |
| `NewRun` / `Steering` / `ConfirmCancel` | palette Run group, or `n` / `s` / `c` in `Normal` | yes |
| `Skills` / `Memory` / `Journey` | palette, or `S` / `M` / `J` in `Normal` | yes |
| `LearningEdit`, `ConfirmLearningDelete` | `e` / `d` inside Journey | code |
| `Docs` + `DocNew` `DocEdit` `DocInsert` `DocDeleteConfirm` `DocPublishPath` | palette `/docs` (or `D`), then `n` `e` `i` `X` `P` | `Docs`,`DocNew` yes |
| `Edges`, `EdgeSearch` | palette `/edges` (or `G`), then `/` | yes |
| `Workflow`, `WorkflowInputs`, `ConfirmWorkflowCancel` | palette `/workflow` (or `W`), then `n` / `c` | Workflow+Inputs yes |
| `Blackboard`, `BlackboardPost` | palette `/blackboard` (or `B`), then `n` | Blackboard yes |
| `Kanban`, `KanbanNew` | palette `/board` (or `K`), then `n` | yes (card created) |
| `UiPlugins` + 4 confirms | palette `/plugins`, then `a` `r` `t`/`u` `x` | UiPlugins yes |
| `CouncilBrowser`, `CouncilBuilder`, `CouncilRunObjective`, `ConfirmCouncilDelete` | palette `/council` (or `C`), then `n` / `r` / `d` | Browser yes |
| `CouncilResults` | palette "/council result", or `Enter` on a council row | code |
| `ModelPicker`, `ConfirmModelRemove` | palette `/model`, then `Delete`/`Ctrl-D` | picker yes |
| `ProviderPicker` → `AddModelProviderKey` → `AddModelQuerying` → `AddModelPick` / `AddModelId` → `AddModelKey` | palette `/provider`, then `Enter`/`Tab` | picker + querying yes |
| `ModePicker`, `ThemePicker` | palette `/mode`, `/theme` | yes |
| `ApiKeys`, `ApiKeySet`, `ApiKeyRemoveConfirm` | palette `/keys`, then `Enter` / `Delete` | ApiKeys yes |
| `UnslothRepos` → `UnslothQuants` → `UnslothConfirmPull` → `UnslothPulling` | palette "Local models: browse Unsloth catalog" | Repos yes (30 live HF repos) |

**Caveat that the help table hides:** the single-key bindings
(`S M J D G W B C K P e i X o t u x d y ?` and `n p c s`) are live only in
`InputMode::Normal` — i.e. only when an overlay is *already* open (`state.rs:2506-2522`).
In the default base view (`InputMode::Composer`) they are literal text typed into the
message box. The palette is the only real front door, and `KEY_BINDINGS`
(`input.rs:78-224`) advertises them as if they were global.

---

## Findings

### F1 — Help overlay: 12 of 29 key labels run into their descriptions (class c)

`crates/tui/src/render.rs:7235` — `format!("  {:<12}", binding.keys)`. Twelve of the
twenty-nine `KEY_BINDINGS` entries are wider than 12 columns, so the pad does nothing and
the label butts straight into the description with no separator:

```
Ctrl-↑ / Ctrl-↓switch to the previous / next run
Tab / e / a / rDocs: switch rail · edit block · accept / reject suggestion
↑ / ↓ (composer)recall the previous / next composer message
Alt-↑ / Alt-↓browse transcript folds: tool cards, diffs, long notes
Delete / Ctrl-Dremove a configured model/key, or clear resolved diagnostics
W · n / p / r / cexecutable persisted workflow: open · run/create · pause · retry · cancel
B · n (Blackboard)open the workflow evidence/decision/artifact stream · post a question
```

User types `?` (or picks Help), sees a garbled key reference, expects an aligned table.
Verbatim from a 120x40 pty capture.

### F2 — Help overlay silently truncates ~8 of its 29 bindings and cannot be scrolled (class c)

`crates/tui/src/render.rs:7259-7273` renders the whole table as one
`Paragraph::new(lines).block(block).wrap(Wrap { trim: false })` — **no `.scroll()`** —
into `centered_rect(70, 80, area)`. At 120x40 the modal is 32 rows and the content is
~40 lines, so the list stops dead at `B · n (Blackboard)`. Never shown:
`K · n · ← / → (Kanban)`, `P (Docs)`, `/ · PgUp/PgDn (Graph)`, `F6 / Shift-F6 / Esc`,
`C`, `n / r / d (Council)`, `K · ← / → (Board)`, plus the closing
"Ctrl-C detaches this client" line.

There is no scroll affordance and no key that scrolls it: `Overlay::Help` resolves to
`InputMode::Normal` (`state.rs:2506`), where `PageUp`/`PageDown` map to
`Action::ScrollPageUp/Down` (`input.rs:341-342`) → `scroll_page` → `scroll_transcript`
(`reduce.rs:2503,2533`), which scrolls the **transcript behind the modal**. The user
pages and the help text does not move.

Consequence: the F6 Remote-UI binding, the Docs publish key, the graph search key and the
whole Council key set are undiscoverable from inside the product.

### F3 — The command palette is unreachable while first-run setup is open (class c)

With zero runnable models, `apply_post_boot_onboard_gate` (`crates/cli/src/tui.rs:575-581`)
opens `Overlay::Onboard` once at boot. That overlay resolves to `InputMode::Palette`
(`state.rs:2492-2494`), where `input.rs:436` maps `/` to `Action::InputChar('/')`.
`reduce::input_char` (`reduce.rs:3838-3845`) opens the palette **only** when
`overlay == Overlay::None`; otherwise it calls `edit_prompt`, whose match
(`reduce.rs:3715-3816`) has **no `Overlay::Onboard` arm** and falls through to `_ => {}`.

The keystroke is dropped with no feedback. Verified: from a fresh data dir I typed
`/`, `m`, `o`, `d`, `e`, `l`, `Enter` and got the *Hosted API providers* onboarding
picker — six keystrokes silently swallowed, then Enter activating the highlighted triage
row. The only escapes are the three triage rows or `Esc` → skip-forever confirm.
The splash itself advertises `/ commands`.

### F4 — Every list pane hard-truncates its own empty-state copy mid-word, no ellipsis (class c)

Verbatim, from pty captures at 120 columns:

```
Blackboard   No Blackboard evidence, decisions, or artif
             Start a workflow, then press n to post an o
Memory       Durable facts appear after complete
Journey      Explicit preferences and verified outco
UI plugins   Install one with `codypendent plugin i
Docs         Press n to create one, or ask an
```

The palette and the pickers *do* ellipsis (`…`, via `truncate_display_width`,
`render.rs:9518`), so the product is inconsistent with itself. The strings are written
for a full-width pane and then clipped by the two-column list layout. Every one of these
is the first thing a user sees in that surface.

Same class, different surface: the splash boot-warning line is cut mid-path —
`! model picker unavailable (reading /root/.local/share/codypendent/models.to` — and the
status notice is cut one column short of the frame:
`● command rejected: … plugin management fails closed (plugin.runt`.

### F5 — Cross-run transcript folds are inert: every card older than the current turn is dead (class c)

`crates/tui/src/render.rs:1480-1484`:

```rust
let hit = if run_idx == selected_run {
    fold_hit_entry(other, idx)
} else {
    None
};
```

and `crates/tui/src/reduce.rs:2583-2619` (`browse_fold`) walks only
`state.selected_run_mut()`.

But the conversation renders **every** run stacked (`render.rs:1392`,
`for (run_idx, run) in runs.iter().enumerate()`), and each follow-up message creates a
**new** run: `Intent::SubmitUserInput` (`reduce.rs:5055`) makes the daemon seed a
continuation run, whose `RunStarted` lands in `ensure_run` (`state.rs:2845`) and moves
`selected_run` to the new one.

Consequence: from the second message onward, every tool card, patch diff, backstage fold
and long note visible on screen from an earlier turn is unreachable — no click target is
registered for it, and `Alt-↑`/`Alt-↓` cannot select it. `Alt-Enter` then falls through
to `edit_prompt(Insert("\n"))` (`reduce.rs:676-683`) and inserts a newline in the
composer instead.

Verified end-to-end: turn 1 produced a tool card; `Alt-↑` + `Alt-Enter` expanded it
(`▸ ⏺ workspace_read_file ✗` → `▾` + `error: unknown tool …` + `Alt-Y copy card · Alt-Enter
collapse`). After sending turn 2, the same card is still on screen and
`Alt-↑ Alt-↑ Alt-Enter` does nothing to it — the composer gains a blank line and the
footer flips to `● Message ready`.

The escape hatch (`Ctrl-↑` to re-select the older run) is undiscoverable and *invisible*:
in Chat layout nothing on screen indicates which run is selected. Pressing `Ctrl-↑`
changed only the header's `ctx 37%` → `ctx 35%`; the transcript, footer and composer were
byte-identical. `F2` reveals the truth: an 8-message conversation is listed as
**"Runs (8)"**.

### F6 — Outcome 20: the TUI has no ledger reader, and there is nothing to read (class a + b)

Confirmed as the brief suspected, with the mechanism:

1. **No ledger consumer.** `grep -rn ledger crates/tui/src/` returns only doc comments
   (`state.rs:712, 2035, 2907`; `action.rs:678`). No overlay, no palette command, no
   status field reads a usage record.
2. **The one cost channel has no producer.** The TUI's only cost input is
   `EventBody::BudgetWarning { dimension: BudgetDimension::Cost, .. }` →
   `reduce.rs:1879` `run.cost_minor = Some(used)`. The agent loop emits **only**
   `BudgetDimension::Tokens` (`crates/runtime/src/agent.rs:446`) and
   `BudgetDimension::WallClock` (`agent.rs:2134`). The only construction of
   `EventBody::BudgetWarning{Cost}` in the whole workspace is
   `crates/cli/src/eval.rs:614` — the eval harness — plus its test.
   `crates/workflow/src/budget.rs` emits a *different* type
   (`codypendent_workflow::BudgetWarning`) that never becomes a session event.
3. **Therefore the cost field is permanently `—`.** `format_cost(None)` →
   `"—"` (`render.rs:9504-9509`), rendered at `render.rs:436`, `:613` (footer) and
   `:1052` (workspace Run detail). Observed after 8 real streamed runs: header, footer
   and Run detail all show `cost: —`.
4. **The run chronicle is produced, referenced on the wire, and dropped.**
   `EventBody::RunCompleted { run_id, disposition, chronicle: ArtifactRef }`
   (`crates/protocol/src/events.rs:186-192`). `crates/tui/src/reduce.rs:1893-1897`
   destructures it as `{ run_id, disposition, .. }` — the chronicle artifact is
   discarded. In the entire TUI, `chronicle` appears only in test fixtures and in one
   confirm-dialog sentence (`render.rs:7945` "a chronicle and any artifacts are kept").
   Class (b): the artifact is assembled by the daemon, named on the wire, and has no
   consumer.
5. **The protocol carries no usage record at all.** `EventBody`
   (`crates/protocol/src/events.rs:70-220`) has no token/cost/latency variant. Making
   the ledger visible needs a wire addition, not just a TUI screen.

Single partial exception: council runs carry `CouncilRunSummary::cost_line`
(`state.rs:1085`), a measured-only aggregate the CLI fills in — the only cost text the
TUI can ever show, and only for councils.

### F7 — Outcome 5: the DAG viewer draws workflows, not code context; the agent half is absent

**What exists and works.** `crates/tui/src/dag.rs` (`lay_out`) is a clean, tested
lane-layout with explicit degradation. Its **only** caller in the workspace is
`render.rs:6384 workflow_lanes` → `render_workflow` (`render.rs:6110`). It renders
**real, live daemon data**: I started `repair-github-check` from the pane (`n` →
`{"pull_request":…}` → Enter) and watched node `inspect` move `pending` → `failed`, with
the durable run id (`wfrun-1a6df572f7e056412f471726623bc847`) and the real failure reason
folded in via `Action::WorkflowNodeUpdated`. Not a fixture.

**What the outcome asked for and is missing.** Outcome 5 is "DAG viewer for
*code-context* management". The code graph is real — 28,198 nodes / 94,466 edges built
by the daemon's scanner, read directly from SQLite by
`crates/cli/src/tui.rs:6619 load_edge_page`. But `/edges` renders it as a **flat
alphabetical table**: 100 rows per page, 945 pages, navigated only by `PgUp`/`PgDn` and a
`LIKE` substring filter. No lanes, no traversal, no neighbourhood expansion, no
"callers of X". `dag.rs` is never applied to it. (Its filter is at least honest —
`Code graph (1–64 of 64 · filter 'render_conversation')`, and a no-match search shows an
explicit empty state rather than a silent drop.)

**Agent side: ABSENT.** There is no code-graph tool in `crates/runtime/src/tools/`
(artifact, blackboard, council, docs, edit_file, git, github, label, memory, read_file,
registry_search, repository, salient, search, secure_fs, shell, task, web_search,
workflow_control, workflow_query, write_file). The 21 registered skills the Skill Studio
lists contain no graph query. So the "(user + agent)" half of outcome 5 has no engine at
all — class (a).

### F8 — Outcome 3 (TUI side): the pickers are real; one header lies

Working, verified live:
- `/model` lists real `models.toml` rows with truthful readiness — an unreachable local
  endpoint shows `! ollama/qwen3-8b` and the detail rail explains
  `readiness: unavailable · connection check to http://127.0.0.1:11434/v1 failed …` plus
  "This model cannot be staged until it is available"; `Enter` refuses to stage it
  (`reduce.rs:4870`).
- `/provider` lists the full **32-provider hosted catalog, prefilled**, with protocol,
  auth env-var name, `live list ✓` and curated model counts (DeepInfra 41, Baseten 12,
  Cohere 9, Qwen 7 …). Not an empty box.
- The add-model flow reaches a live `/models` query and a filterable pick-list, with a
  catalog fallback path (`reduce.rs:5580-5650`).
- `/keys` shows `● / ◐ env NAME / ○` per model plus the Tavily row, `Ctrl-T` verify,
  guarded `Delete`.
- `/theme` previews live across the whole shell as the cursor moves
  (`state.rs:2770 effective_theme`) and persists: picking `light` wrote
  `"theme": "light"` into `tui-sessions.json`.

Defect: the picker header counts filter matches and calls them availability —
`Model picker · 3 of 3 available` while two of the three rows are `!` unavailable
(`render.rs`, picker header). Same shape as `Hosted API providers · 32 of 32`.

### F9 — Two different context figures on one screen

Header: `ctx 41%` (`render.rs:607`). Footer at the same instant: `ctx 41/59%`
(`render.rs:411-414`) — two numbers sharing one `%` with no labels, easily misread as
"41 of 59". Only at width ≥ 160 does it expand to the legible
`41% used/59% left/16k` (`render.rs:304-316`).

### F10 — Accessible mode repeats every assistant reply verbatim

`crates/tui/src/accessible.rs:193-195` prints `Completed: {summary}` for
`RunDisposition::Completed`, and the daemon fills that summary with the whole final
assistant message — which `accessible_snapshot` has already printed as `Assistant: …`.
Measured: an 8-turn conversation produces a **505-line** snapshot per redraw with the
full reply text appearing 8 times as `Assistant:` and 8 times again as `Completed:`.
A screen-reader user hears every answer twice, and the entire history is re-emitted on
each update.

### F11 — Dead actions (class b, not user-visible)

`Action::OpenUiPlugins` and `Action::SmokeTestUiPlugin` are handled in the reducer
(`reduce.rs:773-774`) but **produced nowhere** — not by `input.rs`, not by any
`register_hit` in `render.rs`, not by `cli/tui.rs`. `/plugins` is opened by
`run_palette_command` calling `open_ui_plugins` directly, and smoke-test is reached via
`Action::Steer` (`s`) with the overlay open (`reduce.rs:600-604`).

### Render robustness — mostly clean

Checked deliberately, since the brief asked:
- **No panics from narrow terminals.** Production `render.rs` has zero `unwrap()`,
  `expect()`, `panic!()` outside `#[cfg(test)]` (the only `unreachable!` at `:3465` is
  guarded by an early return at `:3391`). `render()` bails to a compact frame below
  10 rows / 20 columns (`render.rs:66-81`). Swept 24x10, 28x10, 32x12, 36x14, 44x16,
  52x18, 60x20, 72x24, 80x24, 40x12, 20x8, 10x5 through nine overlays each — no
  reproducible crash. (One 20x8 run early on died with a Rust backtrace, but it happened
  while the filesystem was full and I could not reproduce it in ten later attempts; see
  below.)
- **Unicode width is handled correctly.** `truncate_display_width` is used everywhere
  (`render.rs:9518`), the markdown table layout measures in display columns and truncates
  on grapheme boundaries (`markdown.rs:393-436`), and the composer cursor steps whole
  graphemes with `UnicodeWidthStr` columns (`reduce.rs:3544-3675`).
- **No hard-coded colours.** `Color::` does not appear once in `render.rs` outside the
  test module — RULE 7 holds.
- **Scrolling exists where it matters** (transcript with virtualization, pickers,
  council results, edge pages) and is missing in exactly one place: the Help overlay (F2).
- Cosmetic: modal bottom borders overlap the composer separator without clearing it
  (`╰────╯ ───`, `└────┘─────────`) in the Unsloth and Diagnostics overlays.
- Cosmetic: the Docs review rail draws `Borders::LEFT | Borders::TOP` only
  (`render.rs:5645-5646`), which reads on screen as an unclosed box
  (`┌────────────────│`). Intentional, but it looks broken.

### What the rich chat stream actually renders (outcome 7 positive evidence)

Driven with a real streaming SSE model. Cell types that exist **and were observed
emitted**: `User`, `Model` (rich), `Tool`, `Budget`, `Completed`, `Note`, `Backstage`.
Verbatim capture:

```
  You                                                            00:48
    Summarise the renderer architecture in this repo.
  ⋯ context · 129 lines · memory updated
  ⚠ budget tokens: 4926/16384

  ⏺ codypendent · probe/local                                    00:49
  ▌ Let me read the README first.
  ▸ ⏺ workspace_read_file ✗
  ▌ Findings
    Here is what I found in crates/tui/src/render.rs:
    1. The renderer is a pure projection of AppState.
    file      │ lines │ role
    ──────────┼───────┼─────
    render.rs │ 15226 │ draw
    fn render(frame: &mut Frame, state: &AppState, theme: &Theme) { …
    ▏ A blockquote for good measure.
    See the docs (https://example.invalid/docs) and inline code.
    ────────────────────────
  ✓ completed
```

Headings, ordered lists, aligned tables with a rule row, syntax-highlighted fenced code,
blockquotes, links with the URL appended, thematic breaks, inline code — all real, parsed
once at finalize (`reduce.rs:47 finalize_streamed_models`) and cached. The streaming tail
renders plain and flips rich on finalize. This half of outcome 7 is genuinely good.

Never-hit render paths I could not trigger: `TranscriptEntry::Patch` (needs
`EventBody::PatchProposed`), `TranscriptEntry::Steering`, `TranscriptEntry::Unsupported`,
and the approval modal (`render_approval`) — see below.

---

## The single structural pattern

**"Projected for everything, wired for the selection only."**

Almost every finding is the same shape: a projection is correctly built for the whole
session/list, then only the currently-selected slice is connected to input or to a
consumer, and the rest is drawn but inert.

- The transcript renders all 8 runs; only `selected_run`'s cards register hit targets or
  answer `Alt-↑` (F5).
- The help table is built from all 29 bindings; only the ~21 that fit the modal are ever
  shown, and nothing scrolls to the rest (F2).
- Empty-state copy is written for the full pane; only the first 35-42 columns survive the
  two-column split, with no ellipsis to admit it (F4).
- `RunCompleted` carries the chronicle; the destructure keeps two fields and drops the
  third (F6).
- The palette is the advertised universal front door; the one overlay that opens before a
  user has done anything has no arm in `edit_prompt`, so `/` is dropped (F3).

The engines are built, documented and unit-tested — `dag.rs` is a nice piece of work,
`markdown.rs` is grapheme- and width-correct, the reducer is genuinely pure. What is
repeatedly missing is the last hop from *"the data is on screen"* to *"the user can act on
all of it and read all of it."* That is the cheap class (b)/(c) work, and it is where
almost all of this vertical's value sits.

---

## What I could not exercise, and why

- **`TranscriptEntry::Patch` / the approval modal / `Steering`.** These need the agent to
  propose a real tool. The daemon advertised **zero** tools to the model on every request
  (captured request body: `keys: ['model','messages','stream','stream_options']`,
  `tools: []`), so no `ToolProposed` / `ApprovalRequested` / `PatchProposed` ever fired.
  That is a retrieval/tool-selection issue outside this vertical (outcome 9) but it
  blocks the approval and patch render paths from being exercised end-to-end. Their
  reducer arms and render functions are present and unit-tested; I verified them by code,
  not by running.
- **Remote UI surfaces** (`remote_ui*.rs`, `F6`). The daemon refuses to start Remote UI
  workers here: `Remote UI worker runtime unavailable … bubblewrap (bwrap) not found on
  PATH; refusing to run unconfined`. With no mounted documents, `F6` correctly no-ops
  (`reduce.rs:181`). I read the module but could not drive a real plugin surface.
- **Voice (F4 push-to-talk, "speak replies").** Needs an audio device; not available.
- **The 20x8 crash.** One early pty run died with a Rust backtrace tail
  (`__libc_start_main_impl` / `_start`) right after opening the model picker at 20x8. It
  occurred while the filesystem was at 100% and I could not reproduce it in ten
  subsequent attempts across twelve geometries with `RUST_BACKTRACE=1`. Reported as
  unconfirmed; production render code contains no `unwrap`/`expect`, so a disk-write
  panic in a logging/persistence path is the more likely cause.
- **`Onboard{Validating}`, `CouncilResults`, `UnslothQuants`→`Pulling`, the Docs
  `DocEdit`/`DocInsert`/`DocPublishPath` prompts, the four UI-plugin confirms.** Reached
  their parent overlays live, but the child steps need a real model add, a saved council,
  a multi-gigabyte `ollama pull`, a non-empty document, and an installed plugin
  respectively. Traced by code; not run.
- **Multi-client presence / handoff.** One client at a time in this environment.
