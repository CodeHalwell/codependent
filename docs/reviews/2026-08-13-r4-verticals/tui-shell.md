# tui-shell — review report (round 4)

Commit `c255bec8b175d62942b3312cff2335b97d43a59a` (v0.5.1, branch
`claude/review-repair-twenty-outcomes-5fynno`).

Production code read in full: `crates/tui/src/{lib,state,reduce,render,action,input,dag,
markdown,palette,theme,theme_pack,accessible,terminal,remote_ui_host}.rs` and
`crates/tui/src/remote_ui/{mod,paint,codec,layout,text,accessibility}.rs`; plus
`crates/cli/src/{tui,theme_select,stream}.rs`. (The crate is 48,899 lines, of which
~19,000 are `#[cfg(test)]` modules — `render.rs` production code ends at line 9810,
`reduce.rs` at 6500, `state.rs` at 3122.)

**Everything marked OBSERVED below was run**, not read. Method: the real
`target/debug/codypendent` binary driven under a `pty.fork` pseudo-terminal with a
`pyte` VT emulator (`/tmp/review-tui-shell/drive.py`), against a live
`target/debug/codypendentd` in an isolated `CODYPENDENT_DATA_DIR`, pointed at a
hand-written OpenAI-compatible stub server that streams SSE, reports
`usage.{prompt_tokens,completion_tokens}`, and emits tool calls on demand
(`/tmp/review-tui-shell/stub_model.py`). Terminal geometries swept: 120x40, 120x44,
120x10, 100x40, 80x24, 70x30, 60x20, 45x16, 36x12, 28x10, 20x8. The daemon's SQLite
ledger was queried directly after each run to establish ground truth.

---

## Verdicts

**OUTCOME 1 (beautiful, well-formatted, easy-to-use TUI — every menu polished):
PARTIAL.** Every overlay is reachable and the three worst defects the previous round
found (Help alignment, Help truncation, first-run `/`) are genuinely repaired.
But: the command palette, the model picker, the `/keys` overlay and the theme
picker render as **completely empty boxes** on a 10-row terminal; every list
sub-line in four pickers still hard-cuts mid-identifier with no ellipsis; **every
transient status notice in the product is invisible for the whole life of any
session that has a run**; the Help table's wrapped lines have no hanging indent
and become an unreadable wall below ~80 columns; and losing the daemon dumps a
36-frame Rust backtrace over the user's terminal.

**OUTCOME 7 (rich chat stream): WORKING, with one formatting defect.** The
previous round's headline defect — "from the second message onward every card from
an earlier turn is permanently un-expandable" — **is fixed, and I drove it**:
`Alt-↑` now walks folds across every run in the stacked conversation and
`Alt-Enter` expands a card from turn 1 while turn 2 is on screen. Markdown, tool
cards, folds, virtualization and the streaming caret are all real. The one
remaining defect is that markdown tables are laid out to a fixed 100-column budget
regardless of terminal width, so any wide table shears on a narrow terminal.

**OUTCOME 20 (the ledger made visible — TUI side): BROKEN, and worse than
absent.** The engine now exists end-to-end on the daemon side: the executor
measures usage, writes `runs.prompt_tokens / completion_tokens / cost_micros`, and
**publishes a new `EventBody::RunUsage` on the wire**. The TUI reducer has no arm
for it. The event therefore falls into the forward-compatibility catch-all and the
TUI prints the literal words **`? unsupported event`** into the middle of the chat
transcript at the end of every single run, while the header, footer and Run detail
all still show `cost: —`. This is the exact final-wire failure the review's own
synthesis describes, except the missing wire now produces visible garbage rather
than silence.

---

## F1 — `RunUsage` is on the wire, the TUI renders it as `? unsupported event` (class b)

**The single highest-value finding in this vertical.**

The producer is real and on a live path:

- `crates/codypendentd/src/executor.rs:1013-1032` — after every run, if the
  provider reported usage, the daemon appends and publishes
  `EventBody::RunUsage { run_id, prompt_tokens, completion_tokens, cost_micros }`.
- `crates/protocol/src/events.rs:209-217` — the wire variant, with a doc comment
  that says exactly why it exists: *"without it a client can only ever learn a
  run's cost by reading the daemon's database directly, which no client does."*
- `crates/cli/src/stream.rs:276` — the CLI's `event_run_id` resolves it to its run,
  with a unit test pinning the rule.

The consumer does not exist. `crates/tui/src/reduce.rs:1540-2003` (`apply_event`)
has arms for `SessionCreated`, `NoteAppended`, `SessionClosed`, `RunStarted`,
`RunStateChanged`, `ModelStreamDelta`, `ToolProposed`, `ToolDenied`, `ToolStarted`,
`ToolCompleted`, `PatchProposed`, `ApprovalRequested`, `ApprovalResolved`,
`SteeringQueued`, `SteeringApplied`, `BudgetWarning`, `RunCompleted`,
`LearningsCaptured`, `ClientPresenceChanged` — and **no `RunUsage` arm**. It falls
to the catch-all at `crates/tui/src/reduce.rs:1992-2002`:

```rust
// `Unknown` and any future event type this build predates render a
// placeholder and keep going (protocol RULE 1).
_ => {
    if let Some(run) = state.selected_run_mut() {
        AppState::push_entry(run, TranscriptEntry::Unsupported {
            label: "unsupported event".to_owned(), }, at);
    }
}
```

which `crates/tui/src/render.rs:2338-2340` draws as `? {label}`.

**OBSERVED.** Ground truth from the daemon's own database, after one turn:

```
$ sqlite3 /tmp/review-tui-shell/data/codypendent.db 'select * from runs'
{'id': '019ffd4e-377a-…', 'state': 'Completed', …,
 'prompt_tokens': 1234, 'completion_tokens': 567, 'cost_micros': None}

seq 19 {"type": "RunUsage", "run_id": "019ffd4e-ca8a-…",
         "prompt_tokens": 1234, "completion_tokens": 567}
```

What the user sees at the same moment, verbatim from the pty at 120x40:

```
   ⏺ codypendent · stub/fast                                                     22:47
   ▌ Findings
     Here is a rich reply with inline code, a table and a fence.
     …
     Done.
   ? unsupported event
   ✓ completed
   MESSAGE · Enter sends ───────────────────────────────────────────────────────────
  ❯ Ask Codypendent to build, fix, explain, or explore…

  ▲ Setup needs attention · 1 issue(s)                                / diagnostics
  model stub/fast · Build · ctx 2/98% · agents 0+0 · via openai-compatible · — · …
```

Note `· — ·` in the footer: that is the cost field. `format_cost(None)` →
`"—"` (`crates/tui/src/render.rs:9782-9787`), rendered at `render.rs:436` (footer),
`render.rs:613` (header) and `render.rs:1052` (workspace Run detail). The TUI's only
cost input remains `EventBody::BudgetWarning { dimension: Cost }` →
`reduce.rs:1918 run.cost_minor = Some(used)`, and the agent loop still never emits
that dimension — so the field is permanently `—` even though the number the user
wants (1234 prompt / 567 completion tokens) arrived on the very same connection two
events earlier and was thrown away.

The same defect is in the accessible client. **OBSERVED**, `--accessible`:

```
Assistant: ## Findings
…
Completed: ## Findings
…
Unsupported event: unsupported event
```

(35 occurrences of `Unsupported event` across one 7-snapshot session.)

**What a repair costs:** one match arm in `reduce.rs` and one `RunView` field. The
measurement, the schema, the migration, the wire type, the CLI's run-scoping and its
regression test are all already there.

### F1b — three more measured facts reach the TUI and are dropped

- `EventBody::RunCompleted { chronicle: ArtifactRef }`
  (`crates/protocol/src/events.rs:187-192`) is still destructured as
  `{ run_id, disposition, .. }` at `crates/tui/src/reduce.rs:1932-1936`. The run
  chronicle has no consumer anywhere in the TUI.
- `ArtifactRef.sensitivity` (a `DataClassification` — the "classification decision"
  half of outcome 20) rides on every tool-output artifact and every patch. The
  expanded tool card renders `media_type` and `byte_length`
  (`crates/tui/src/render.rs:2519-2528`) and never `sensitivity`. It appears in the
  TUI only inside test fixtures.
- No latency/duration is rendered anywhere. `grep -n 'latency\|duration\|elapsed'`
  over `render.rs`, `state.rs` and `reduce.rs` production code returns nothing. Tool
  cards carry no elapsed time; `WorkflowNodeCard::cost` is a pre-rendered string
  that reads `cost: —` in the live workflow pane (**OBSERVED**).

### F1c — what outcome 20 *does* get right in the TUI

Policy denials are genuinely surfaced, with the full reason. **OBSERVED** — I made
the stub propose `shell.run rm -rf /`, and `Alt-↑` + `Alt-Enter` gave:

```
   ▾ ⏺ shell.run ✗
       command: rm -rf /
       cwd: /tmp/review-tui-shell/codypendent-worktrees/repo/run-3dec24915854
       error: policy denied: `rm` is not in the shell allow-list — to inspect the repo…
       Alt-Y copy card · Alt-Enter collapse
```

`EventBody::ToolDenied` is handled at `crates/tui/src/reduce.rs:1705-1741`, and the
tool-call trace (proposed → running → completed, with args digest and artifact) is
complete. So the outcome-20 gap in this vertical is precisely: **tokens, cost,
latency and classification — not denials or traces.**

---

## F2 — every transient notice in the product is invisible once a run exists (class c)

`crates/tui/src/render.rs:2747`:

```rust
} else if status.pending_approvals == 0 && status.run_state.is_none() && state.notice.is_some()
```

The notice branch of `render_status_line` requires `status.run_state.is_none()` —
i.e. **no run at all**. `AppState::status()` (`state.rs:2948-2959`) sets `run_state`
from the selected run, so from the first message onward it is always `Some(_)` and
the notice branch can never be taken. Three later branches (`!issues.is_empty()`,
`session_closed`, `!composer.is_empty()`) mask it further. The only other reader of
`state.notice` in the whole renderer is the council-builder wizard
(`render.rs:8884`).

**OBSERVED.** With one completed run and the diagnostics list cleared (so no branch
except the notice one should apply), I pressed `Alt-Y` with nothing browsed — which
sets `notice = "browse a card with Alt-↑/↓ before copying"` at
`reduce.rs:2844-2848` — and then browsed a card and pressed `Alt-Y` again
(`notice = "copied focused card"`, `reduce.rs:2858`). The footer was byte-identical
in all three frames:

```
  ✓ Completed                                                   n new · / commands
  model stub/fast · Build · ctx 2/98% · agents 0+0 · via openai-compatible · — · …
```

Consequence: the user gets no confirmation that a copy happened, no explanation
when it did not, and — most seriously — **no warning when the connection drops**:
`crates/cli/src/tui.rs:1689` sets `Action::Notice("connection lost · reconnecting…")`
and calls `presentation.draw()` immediately, and that notice is not drawn either
(see F3). Every `state.notice = Some(...)` in `reduce.rs` (there are 40+ of them:
command rejections, model-stage refusals, theme/key confirmations, retry messages)
is dead on the graphical client. The accessible client prints them correctly
(`accessible.rs:33-35`), so this is a graphical-only regression.

---

## F3 — losing the daemon kills the TUI with a raw Rust backtrace (class c)

**OBSERVED, deterministic.** With a live session attached, I sent `SIGTERM` to the
daemon and watched. Within 9 seconds the TUI exited and dumped this over the user's
terminal (36 frames; head and tail shown, ANSI stripped):

```
Error: connecting to daemon socket /tmp/review-tui-shell/data/run/daemon.sock

Caused by:
    No such file or directory (os error 2)

Stack backtrace:
   0: <E as anyhow::context::ext::StdError>::ext_context
             at …/anyhow-1.0.103/src/backtrace.rs:10:14
…
  18: codypendent::main
             at /home/user/codypendent/crates/cli/src/main.rs:1077:19
…
  35: _start
```

The path is `crates/cli/src/tui.rs:1688-1718`: on `ReaderSignal::Closed` the loop
sets the "connection lost · reconnecting…" notice (invisible — F2), retries
`reconnect_live_session` five times with exponential backoff (~8 s total), then

```rust
let Some((next_live, catchup, pending)) = replacement else {
    return Err(failure.unwrap_or_else(|| anyhow!("reconnect failed")));
};
```

That `anyhow::Error` propagates out of `event_loop` → `tui::run` → `main`, where
Rust's default `Termination` impl prints the debug form including the captured
backtrace. There is no attempt to restart the daemon the CLI itself started at boot,
no in-TUI "daemon unavailable" screen, and no clean one-line message.

Note this is **not** a panic, so the crash-log machinery added for exactly this class
of problem (`crates/cli/src/tui.rs:142-236`, writing `<data_dir>/logs/tui-crash.log`)
does not fire: after the reproduction, `data/logs/` contained only `daemon.log`. Per
that module's own stated diagnostic contract, an empty crash log means "aborted or
OS-killed" — which would send the next investigator down the wrong path.

---

## F4 — four menus are completely empty on a short terminal (class c)

**OBSERVED at 120x10** — a normal split-pane / tmux geometry. The command palette,
the model picker, the `/keys` overlay and the theme picker all draw their chrome and
zero rows. Verbatim:

```
### Palette  [120x10]
  ✦ codypendent  /  repo                                                             Build
  ╭ Command palette ──────────────────────────────────────────────────────────────────────╮
  │╭ Search ─────────────────────────────────────────────────────────────────────────────╮│
  ││  ⌕  Type to filter…▏                                                                ││
  │╰─────────────────────────────────────────────────────────────────────────────────────╯│
  │╭ Commands  ·  27 of 27 results ──────────────────────────────────────────────────────╮│
  │╰─────────────────────────────────────────────────────────────────────────────────────╯│
  │                     ↑/↓ select  ·  Enter run  ·  Esc close  ·  click a row            │
  ╰───────────────────────────────────────────────────────────────────────────────────────╯
```

```
### ModelPicker  [120x10]
  ╭ Model picker  ·  2 of 2 available ────────────────────────────────────────────────────╮
  │╭ Search ─────────────────────────────────────────────────────────────────────────────╮│
  ││  ⌕  Type to filter…▏                                                                ││
  │╰─────────────────────────────────────────────────────────────────────────────────────╯│
  │╭ Models ───────────────────────────────╮ ╭ Model details ────────────────────────────╮│
  │╰───────────────────────────────────────╯ ╰───────────────────────────────────────────╯│
  │        ↑/↓ or wheel · PgUp/PgDn · Home/End · Enter stage · Ctrl-D remove · Esc close  │
  ╰───────────────────────────────────────────────────────────────────────────────────────╯
```

The header truthfully says "27 of 27 results" / "2 of 2 available" while showing
nothing at all, and nothing scrolls into view (the list rect has height 0, so
`↑/↓` moves an invisible selection). The mechanism is the same in all four:
`centered_modal(area, W, H)` (`render.rs:9364-9373`) clamps height to
`area.height - 2` = 8, `modal_surface`'s border takes 2, then the fixed
`[Length(3) search, Min(0) list, Length(1) hint]` split
(`render.rs:7057-7064` palette, `render.rs:4310-4317` model picker,
`render.rs:5115` `/keys`, `render.rs:4911` theme picker) leaves 2 rows for the list
block, whose own border consumes both. `Min(0)` guarantees the list is the loser. (`/keys` sizes itself with
`centered_modal(area, 84, 24)` at `render.rs:5112`; the theme picker with
`centered_modal(area, 72, 22)` at `render.rs:4906`.)

By contrast the browser-style overlays (Docs, Kanban, Workflow, Edges, Council)
degrade correctly at the same size and still show rows — so the fix pattern already
exists in this file; it was simply not applied to the picker family.

Adjacent, same geometry class:

- At **45x16** the model picker's entire "Model details" rail is gone — readiness,
  cost, context and the "This model cannot be staged until it is available" warning
  are all invisible with no indication that a detail pane exists.
- At **20x8** the compact fallback (`render.rs:66-81`) fires on the *height* check
  but its copy talks only about columns, and the copy is itself hard-cut:
  ```
  codypendent
  resize terminal to a
  ```
  The user is told "resize terminal to at least 20 columns" while already at 20
  columns, and cannot read the whole sentence.

No crash was reproducible at any size: 11 geometries × 10 overlays each, and the
child process was still alive at the end of every sweep.

---

## F5 — the picker sub-lines hard-cut mid-identifier, with no ellipsis (class c)

The row *titles* go through `truncate_display_width`. The row *sub-lines* do not,
so ratatui's `List` clips them at the pane edge with no `…`. **OBSERVED at 120x40**:

```
### ProviderPicker
││  ✓ antigravity-acp                         │
││      Google Antigravity (community bridge) │
││      local ✓ · acp: verified install · thir│      ← "third-party ToS risk"
││  ✓ ollama                                  │
││      local ✓ · none · live list ✓ · catalog│
││  ✓ amazon-bedrock                          │
││      AWS Bedrock (mantle, bearer key) · ope│      ← "openai-chat"
││      hosted · api-key: AWS_BEARER_TOKEN_BED│      ← the env-var NAME, cut
```

```
### ModePicker
││    Plan                                     ││
││      investigate read-only, then finish with a numbered implementat││
││▎ ● Build                                    ││
││      full worktree access — writes, commands, and network (the defa││
```

```
### ThemePicker
││      ████████████  Okabe–Ito hues, safe for all common colour visio││
```

```
### ApiKeys
││▎ ○ stub/fast                                                       ││
││      openai-compatible · no key configured · connection check to `http://127.0.││
```

Sites, all a bare `format!` into a `Line::styled` with no width parameter:

| picker | file:line |
|---|---|
| model picker · provider line | `crates/tui/src/render.rs:4374-4376` |
| provider picker · name+protocol | `crates/tui/src/render.rs:4658-4661` |
| provider picker · metadata | `crates/tui/src/render.rs:4662-4670` |
| mode picker · summary | `crates/tui/src/render.rs:4853-4856` |
| theme picker · summary | `crates/tui/src/render.rs:4978-4980` |
| `/keys` · detail | `crates/tui/src/render.rs:5207-5210` |

The previous round reported this exact class against the *empty-state* copy in the
browsers. That instance **is fixed** — the empty states now wrap cleanly
("No curated memories yet / Durable facts appear after / completed runs.") — and the
onboarding triage rows do ellipsis correctly (`…`). The class was not fixed; it
simply moved to the sibling call sites. This is the review's own meta-pattern
reproducing inside a single file.

Same shape in the status line: `render.rs:2769` renders `state.notice` untruncated,
and at 36x12 the issue banner reads `▲ Setup needs attention · 1 issue(` — cut, no
ellipsis.

---

## F6 — the Help overlay: two of three defects repaired, the third is worse (class c)

**Repaired, verified by driving:**

- Key-column alignment. `render.rs:7423-7431` now computes
  `key_width = KEY_BINDINGS.iter().map(|b| b.keys.chars().count()).max()` instead of
  the old fixed `{:<12}`. Every label now has a real gutter.
- Scrolling. `render.rs:7464-7485` measures wrapped height, publishes
  `state.help_max_scroll`, and applies `.scroll((offset, 0))`; `reduce.rs:2541-2550`
  and `2564-2573` route wheel and PgUp/PgDn to `help_scroll` when `Overlay::Help` is
  open. **OBSERVED:** PgDn twice reaches the final line
  `Ctrl-C detaches this client — it never stops the run.  ? or Esc closes.`, and
  `K · ← / → (Board)`, `C`, `n / r / d (Council)`, `F6 / Shift-F6 / Esc` — all
  previously unreachable — are now visible.

**Still broken:** the wrapped continuation lines have **no hanging indent**, so the
table is ragged and, on narrow terminals, illegible. Verbatim at 120x40 (the modal
is 84 columns because it is `centered_rect(70, 80, …)`):

```
│  PgUp / PgDn             page the focused pane (or scroll the conversation)      │
│(mouse: wheel)                                                                    │
│  ↑ / ↓                   move selection in a browser, palette, or workspace side │
│pane  (mouse: wheel)                                                              │
│  Tab / e / a / r         Docs: switch rail · edit block · accept / reject        │
│suggestion                                                                        │
│  Alt-Enter               expand / collapse the browsed fold, else insert a line  │
│break  (mouse: click a fold line)                                                 │
│  F4 (default)            push to talk; press again to stop and send the voice    │
│note                                                                              │
```

At 60x20 the 24-column key gutter leaves ~14 columns for the description and the
whole overlay becomes a wall of unindented fragments:

```
┌ Help ──────────────────────────────────┐
│Keys — every mouse action has a keyboard│
│equivalent                              │
│  type…                   compose a     │
│message in the bottom composer          │
│  Enter                   send: start a │
│run, or steer the active one            │
│  /                       command       │
│palette — every command, searchable     │
```

There is also no scroll affordance: no scrollbar, no "page 1 of 3", and the modal's
own footer line does not mention PgUp/PgDn. A user who does not guess still sees a
truncated table.

---

## F7 — the Help overlay documents keys that do not work where a user will press them (class c)

`KEY_BINDINGS` (`crates/tui/src/input.rs:78-224`) is presented as the product's
key reference. Its entry at `input.rs:124-128` is:

```
?    show / hide this help overlay
```

**OBSERVED:** from the default base view (a normal chat screen, `InputMode::Composer`),
pressing `?` types a literal question mark into the message box:

```
   MESSAGE · Enter sends ────────────────────────────────────────────────────────
  ❯ ?
```

The mechanism: every single-key command lives in `map_normal_char`
(`crates/tui/src/input.rs:350-394`), reached only from `InputMode::Normal` —
which `AppState::input_mode` (`state.rs:2685-2701`) returns **only when an overlay
is already open**. `map_composer_key` (`input.rs:446-495`) has no `?` arm and falls
through to `KeyCode::Char(c) => Action::InputChar(c)`. The same is true of
`S M J D G W B C K P e i X o t u x d y` and `n p c s` — 23 of the 29 documented
bindings are inert in the view a user spends all their time in. The palette (`/`) is
the only working front door.

This is the same caveat the previous round flagged and it is unchanged. It matters
more than it looks: the one key the help table promises will open the help table is
the one that demonstrably does not.

---

## F8 — a broken or forbidden theme pack disappears with no diagnostic (class c, silent filter)

`crates/cli/src/theme_select.rs:158-175`:

```rust
.filter_map(|entry| {
    let path = entry.path();
    let id = path.file_stem()?.to_str()?.to_owned();
    let source = std::fs::read_to_string(&path).ok()?;
    let theme = load_theme_pack(&source).ok()?;
    Some((id, theme))
})
```

Every failure mode is `.ok()?` — the pack is dropped and nothing is reported. There
is no `push_boot_warning`, no `AppState::issues` entry, and no notice.

**OBSERVED.** I installed three packs under `<data-dir>/themes/`:

| file | content | outcome |
|---|---|---|
| `reviewer-good.toml` | `base = "Dark"` (correct field, wrong case — `ThemeVariant` is `#[serde(rename_all = "kebab-case")]`, `theme.rs:699-709`) | silently absent |
| `reviewer-broken.toml` | `"status.error" = "not-a-color"` | silently absent |
| `reviewer-evil.toml` | declares `[permissions] filesystem = "all"` | silently absent |

The theme picker showed **`Theme picker · 7 of 7 themes`** — the seven built-ins
only — and `Setup & diagnostics` listed one unrelated pre-existing issue and nothing
about themes. Changing `base = "Dark"` to `base = "dark"` and re-running gave
`Theme picker · 8 of 8 themes`, proving the loader works and the silence is the
whole defect.

The third row is the one that matters: `theme_pack.rs` enforces the README's
absolute rule ("theme plugins get no execution permissions") with a dedicated error
variant, `ExecutionPermissionsForbidden` — and the operator is never told that a
pack on their disk asked for filesystem access and was refused. The security check
is real; its evidence is discarded.

---

## F9 — the model picker's header calls filter matches "available" (class c)

`crates/tui/src/render.rs:4300-4305`:

```rust
format!("Model picker  ·  {} of {} available", matches.len(), state.models.len())
```

`matches` is `filter_models(&state.models, query)` — a substring filter. It has
nothing to do with `ModelReadiness`. **OBSERVED**, one screen, two contradictory
claims:

```
╭ Model picker  ·  2 of 2 available ──────────────────────────────────────────╮
││▎ ● ! stub/fast                    │ │  readiness: unavailable · connection ││
││    ! stub/smart                   │ │  This model cannot be staged until   ││
                                       │ │  it is available                   ││
```

Both rows are `!` unavailable, the detail rail says so, `Enter` correctly refuses to
stage them (`reduce.rs:4870`) — and the title bar says "2 of 2 available". The
provider picker has the same shape (`render.rs:4594`, `"Provider catalog · Step 1 of
2 · 38 of 38 adapters"` — that one is at least honestly named "adapters").

---

## F10 — the accessible client says every reply twice, and repeats the whole history each frame (class c)

Unchanged from the previous round. `crates/tui/src/accessible.rs:153-155` prints
`Assistant: {text}` for a `Model` entry; `accessible.rs:192-196` then prints
`Completed: {summary}` for `RunDisposition::Completed`, and the daemon fills that
summary with the entire final assistant message (confirmed in the ledger:
`"disposition": {"type": "Completed", "summary": "## Findings\n\nHere is a **rich**
reply…"}`).

**OBSERVED**, `codypendent --accessible`, one turn:

```
Assistant: ## Findings

Here is a **rich** reply with `inline code`, a table and a fence.
…
Done.

Completed: ## Findings

Here is a **rich** reply with `inline code`, a table and a fence.
…
```

Measured: one session produced 7 full snapshots totalling 46,985 bytes / 2,033
lines, containing 42 `Assistant:` and 35 `Completed:` lines. The final snapshot alone
was 290 lines — the *entire* conversation re-emitted on every redraw, with each reply
appearing twice inside it, plus one `Unsupported event: unsupported event` per run
(F1).

---

## F11 — markdown tables are laid out to a fixed 100 columns, ignoring the terminal (class c)

`crates/tui/src/markdown.rs:97`: `const MAX_TABLE_WIDTH: usize = 100;`, used at
`markdown.rs:396` to cap column widths. `markdown::parse(text)` takes no width
argument — the rich cache is built once at finalize (`reduce.rs:47-67`) and is
width-independent by construction — so a table can never adapt to the viewport.
Rows wider than the terminal are then cell-wrapped by the transcript renderer with
the continuation starting at column 0.

**OBSERVED at 70x30**, a four-column table:

```
   column-one-is-long │ column-two-is-long │ column-three-is-long │ c
 olumn-four-is-long
   ───────────────────┼────────────────────┼──────────────────────┼──
 ──────────────────
   aaaaaaaaaaaaaaaaaa │ bbbbbbbbbbbbbbbbbb │ cccccccccccccccccccc │ d
 ddddddddddddddddd
```

The alignment machinery inside `layout_table` is careful and grapheme/width-correct
(`markdown.rs:382-450`); it is simply measuring against the wrong number.

---

## What was verified as REPAIRED (drive-tested, previous round's findings)

- **Cross-run transcript folds (prev. F5) — FIXED.** `render.rs:1486-1492` now
  registers a click target for `(run_idx, entry)` on *every* run's fold heads, not
  just `selected_run`'s; `reduce.rs:2656-2718` (`session_folds` / `set_fold_cursor` /
  `browse_fold`) walks the whole session; `AppState::transcript_focus_run` +
  `fold_focus_run()` (`state.rs:2423-2754`) carry the cursor's run.
  **OBSERVED:** turn 1 produced a `workspace.read_file` card; I sent turn 2 (a new
  run), then pressed `Alt-↑` four times and `Alt-Enter`, and the *turn-1* card
  expanded in place:
  ```
   ▾ ⏺ workspace.read_file · README.md ✓
       args-digest: 7d6441497d2a000b8143602a7817c90abe7db88e139f89c062a1c36cfe0ad9d6
       output: text/plain (135 bytes)
       Alt-Y copy card · Alt-Enter collapse
  ```
- **Help alignment (prev. F1) and Help scrolling (prev. F2) — FIXED** (see F6 for
  what remains).
- **`/` swallowed by the first-run gate (prev. F3) — FIXED.** `reduce.rs:4010-4024`
  adds an explicit `Overlay::Onboard { step: Triage }` arm for `/`, setting
  `palette_from_onboard` so `Esc` returns to the gate. **OBSERVED** from a zero-model
  data dir: `/` opened the full 27-command palette, and `Esc` returned to
  `Connect a model`.
- **Browser empty-state copy (prev. F4) — FIXED for the browsers.** Memory, Journey,
  Docs, Council, Blackboard and UI-plugins empty states now wrap onto multiple lines
  instead of being cut mid-word. (The class survives elsewhere — F5.)
- **Palette reachability of all 27 commands.** **OBSERVED:** 26 × `Down` walks from
  `Setup & diagnostics` to `New conversation`; the six commands below the fold at
  120x40 are reachable (though there is no scrollbar or overflow hint).

## What still works well

- **The rich chat stream (outcome 7).** Headings, ordered lists, aligned tables with
  a rule row, syntax-highlighted fences, blockquotes, links with the URL appended,
  thematic breaks and inline code all render, parsed once at finalize
  (`reduce.rs:47-67`) and cached; the streaming tail renders plain with a `▋` caret
  and flips rich on completion. All **OBSERVED** live against a streaming SSE stub.
- **Transcript virtualization.** `measure_transcript` / `build_transcript_window`
  (`render.rs:1691-1761`) split measure from build so per-frame allocation is bounded
  by the viewport; `CellWrap` (`render.rs:1124-1202`) is the single wrapping rule
  shared by both, so follow-mode cannot clip. Browsing a fold far above the tail
  pins the viewport to it without mutating `run.scroll` (`render.rs:1877-1883`) —
  **OBSERVED** walking back into turn 1 from turn 2.
- **Policy denials, tool traces, backstage folds, failure recovery affordances**
  (`Alt-R retry · Alt-M choose model · / diagnostics · Alt-D disable · Alt-Y copy`)
  all render with real data.
- **Theme previewing** across the whole shell as the picker cursor moves
  (`state.effective_theme`, `state.rs:2967-2980`).
- **Robustness.** Production code contains no `unwrap()`, `expect()`, `panic!()` or
  `todo!()`; the three `unreachable!()`s (`render.rs:3541`, `state.rs:2704`,
  `state.rs:2707`) are each guarded by an earlier arm. `Color::` never appears in
  production `render.rs` — RULE 7 holds. No crash was reproducible across the size
  sweep.
- **Cosmetic, unchanged from last round:** the Docs review rail draws
  `Borders::LEFT | Borders::TOP` only (`render.rs:5841`), which reads on screen as an
  unclosed box `┌──────────────────│`.

---

## The pattern

**A defect is repaired where it was *reported*, never where it can *occur*.** Every
one of this round's findings is a surviving sibling of a fix that landed:

- The empty-state copy was taught to wrap; the six picker sub-lines beside it were
  not (F5) — same file, same widget family, same missing `truncate_display_width`.
- The Help overlay learned to scroll; nothing else learned that a modal on a
  10-row terminal must reserve rows for its list, so four pickers render empty (F4).
- The onboarding gate learned to accept `/`; the base view still swallows `?` while
  the help table promises it (F7).
- `RunUsage` was designed, migrated, serialized, published, run-scoped in the CLI
  and unit-tested — and the one `match` arm that makes a user see it was never
  written, so the TUI prints `? unsupported event` instead (F1).
- `ToolDenied` got a reducer arm *and* a rich card; `RunUsage`, `chronicle` and
  `ArtifactRef.sensitivity` — the other three measured facts on the same wire — got
  none (F1b).
- The notice mechanism exists, is set from 40+ call sites, and is rendered behind a
  condition (`run_state.is_none()`) that is false for the entire life of any real
  session (F2).

The engines here are good. `dag.rs` is clean, `markdown.rs` is grapheme- and
width-correct, the reducer is genuinely pure, `CellWrap` is a careful piece of work,
and the security invariant in `theme_pack.rs` is enforced structurally. What is
missing, every single time, is the last hop from *"the data exists and is correct"*
to *"a user in a terminal can see it."* Three of those hops (F1, F2, F4) are each
roughly a ten-line change.

---

## What I did not verify

- **The approval modal (`render_approval_modal`, `render.rs:7312`) and
  `TranscriptEntry::Patch`.** I made the stub propose `workspace.write_file`, but in
  Build mode the daemon executed it without parking for approval, so no
  `ApprovalRequested` / `PatchProposed` ever fired. Their reducer arms and render
  functions are present and unit-tested; **read, not run.**
- **Remote UI surfaces** (`remote_ui*.rs`, `F6`). The daemon refuses to start Remote
  UI workers in this container: `bubblewrap (bwrap) not found on PATH; refusing to
  run unconfined`. With no mounted documents `F6` correctly no-ops
  (`reduce.rs:181-189`). I read all six `remote_ui` modules and `remote_ui_host.rs`;
  **no live plugin surface was driven.**
- **Voice (F4 push-to-talk, "speak replies").** No audio device.
- **The Unsloth catalog flow.** `/`-palette "Local models: browse Unsloth catalog"
  did not open its overlay in my sweep; the Hugging Face Hub is reached through the
  agent proxy in this container and I did not chase it. **Not verified either way.**
- **Council results, the Docs edit/insert/publish prompts, the four UI-plugin
  confirms, `Onboard{Validating}`, `ConfirmModelRemove`, `ConfirmCommunityAcpInstall`.**
  I reached each parent overlay live; the child steps need a saved council, a
  non-empty document, an installed plugin, a real model add, and a real ACP
  download respectively. **Traced by code; not run.**
- **Multi-client presence / handoff.** One client at a time in this environment.
- **Whether `cost_micros` would render if `RunUsage` were consumed.** The stub
  reported token counts but no price, so `cost_micros` was `None` in every event I
  produced (the daemon needs a priced model profile). The *claim* that the TUI shows
  `—` is verified; the claim that a priced run would show a figure is **inferred**,
  and moot while there is no reducer arm at all.
- **The one crash I saw early on** (a 36-frame backtrace mid-test) turned out to be
  my own environment killing the daemon, and I reproduced the same output
  deliberately in F3. I found **no** crash attributable to the TUI itself in 11
  geometries × ~10 overlays plus 6 full conversation runs.
