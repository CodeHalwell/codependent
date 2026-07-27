# TUI overhaul — visual refresh + clickable everything — design

**Date:** 2026-07-27 · **Status:** approved (pre-implementation) · **Branch:** `claude/tui-overhaul`

## Problem

The Codypendent TUI chat works but reads as an event log, not a Codex/Claude-Code-style
conversation, and the mouse does almost nothing:

- **Transcript sits at the top of an empty pane.** `render_conversation`
  (`crates/tui/src/render.rs:221`) draws a `Paragraph` from the top; when the wrapped
  content is shorter than the viewport, the empty space pools at the *bottom*, just above
  the composer, which looks broken.
- **Turns don't breathe and roles blur.** A user turn is one `› {text}` line
  (`entry_lines`, `render.rs:495`); the assistant turn opens with a bare `⏺ codypendent`
  header (`conversation_lines`, `render.rs:434`). There is one blank line before each new
  user turn and nothing else — the two roles are not visually distinct.
- **Backstage noise on every turn.** `⋯ context · N lines · memory updated`
  (`backstage_lines`, `render.rs:766`) and short `• note:` lines (`note_lines`,
  `render.rs:723`) render on every turn.
- **Raw error chains.** A failed run renders `✗ {reason}` verbatim (`render.rs:526-528`),
  where `reason` is a nested chain like `model driver error: model stream failed: service
  error: request failed: builder error`.
- **Unstyled command palette.** `render_palette` (`render.rs:2365`) prints ragged rows and
  a confusing `[—]` marker for unbound commands (`palette.rs:130,138,176`).
- **No global shortcut hints; duplicated fields.** The model appears in both the header
  chrome (`header_chrome`, `render.rs:289`) and the status line (`render_status_line`,
  `render.rs:865-874`). There is no persistent shortcut footer.
- **Mouse is wheel + nothing.** Mouse capture is on (`terminal.rs:41`), `map_event`
  (`input.rs:109`) handles the wheel, and a left click resolves to `Action::NoOp`
  (`input.rs:286`). `pane_at` (`input.rs:295`) and `Action::FocusPane` exist but are wired
  to nothing (no callers in `crates/cli/src/tui.rs`). Palette rows, picker rows, run
  entries, and shortcut chips are not clickable.

Goal: one PR that (A) refreshes the visuals to a bottom-anchored, role-differentiated,
quiet, Codex-like chat, and (B) makes every interactive surface clickable by extending the
existing pure-reducer mouse framework — every click routing to the **same** `Action` a key
already produces, with no protocol/daemon/wire change.

## Approved design

### A. Visual refresh (pure render)

Bottom-anchored transcript, breathing turns, distinct roles, quiet backstage, one-line
errors, styled palette, derived shortcut footer, deduped status/header:

```
┌ codypendent — diagnose the failing test · gpt-5.1-codex · Build ┐
│                                                                  │  ← quiet space at top
│                                                                  │     (content flush to bottom)
│  You                                                             │
│    add a round-trip test for the parser                          │
│                                                                  │
│  ⏺ codypendent · gpt-5.1-codex                                   │
│    I'll add one — let me check the parser first.                 │
│    ▸ ⏺ read   parser.rs                            ✓             │
│    ▸ ⏺ ran    cargo test                           ✓             │
│    ▸ ❖ patch  parser_test.rs                       ⟳ review      │
│    Added a round-trip test; all green. ▌                         │
│  ⋯ context · 131 lines · memory updated                          │
└ › message the agent…                                           ──┘
  mode Build   state Running   ctx —   cost —   wt —   approvals 0
  ⏎ send · / commands · ↑↓ scroll · F2 layout · ? help · q detach
```

Concise error, full chain on expand:

```
│  ⏺ codypendent · gpt-5.1-codex                                   │
│  ▸ ✗ model error — the provider request failed                   │   ← collapsed
```
```
│  ▾ ✗ model error — the provider request failed                   │   ← expanded (click / Enter)
│      model driver error: model stream failed: service error:     │
│      request failed: builder error                               │
```

Restyled command palette ("command centre") — aligned columns, grouped, no `[—]`:

```
┌ Command palette ─────────────────────────────────────────────┐
│ › ▏                                                           │
│   ↑/↓ select · Enter run · Esc close · click a row            │
│                                                               │
│   Run                                                         │
│ › New run             start a new run in this session     n   │
│   Steer run           queue a message for a safe point    s   │
│   Pause / resume run  pause or resume the selected run    p   │
│   Cancel run          cancel the selected run             c   │
│                                                               │
│   Models                                                     │
│   Model picker        stage a model for the next run          │   ← no [—]
│   Provider catalog    add a model from a provider             │
└───────────────────────────────────────────────────────────────┘
```

### B. Clickable everything

Render caches each interactive element's `Rect` + the `Action` it fires into an
interior-mutable **hit-test map** on `AppState` (mirroring the existing
`transcript_max_scroll: Cell<u16>` render→input cache). The input layer resolves a left
click at `(col,row)` to the topmost registered rect's `Action`. The reducer stays pure —
render produces geometry as data; input reads it. Every clickable action already has (or is
given) a keyboard equivalent, so the mouse-parity invariant holds.

## Goals

1. **Bottom-anchored transcript** when content is shorter than the viewport; overflow
   follow/scroll unchanged.
2. **Breathing, role-differentiated turns:** `You` and `codypendent · <model>` headers,
   distinct color/gutter/indent, blank-line gaps between turns.
3. **Quiet backstage:** one dim, collapsed, expandable line per turn (reusing the existing
   fold machinery), raw hidden until expanded.
4. **One-line errors:** a concise readable summary with the full raw chain available on
   expand — no information lost.
5. **Styled command palette:** aligned name/description/key columns, grouped commands, no
   `[—]` markers, a stronger selected-row highlight.
6. **Derived shortcut footer** from `KEY_BINDINGS`, so it never drifts.
7. **Tidy status/header:** grouped fields, model deduped, honest `—` preserved.
8. **Clickable surfaces:** palette rows, model/provider/add-model rows, run entries, footer
   chips, composer, panes — each firing the same `Action` its keyboard path fires.

## Non-goals

- **No protocol / daemon / wire / golden-vector change.** Clicks route to existing
  `Action`s/`Intent`s; the two new `Action` variants are client-only view actions.
- No new crate dependency (`ratatui`/`crossterm` already provide `Rect`, `Position`,
  `MouseEvent.column/.row`).
- No rewrite of the composer editor (multiline/history/mentions stay out).
- Hover highlight of the row under the cursor is **optional** (YAGNI — see Open questions).
- The Skills/Memory/Docs/Edges/Workflow/Blackboard browser *rows* are not made clickable in
  this PR (scope discipline; the hit-map makes them trivial to add later).

## Architecture

All changes are in `crates/tui` (`render.rs`, `input.rs`, `state.rs`, `action.rs`,
`reduce.rs`, `palette.rs`) plus one call-site change in `crates/cli/src/tui.rs` (the event
loop resolves clicks). Pure reducer preserved: widgets do no I/O; render caches geometry;
`reduce` is a pure fn of `(state, action)`.

Decomposed into eight single-responsibility components (these seed the plan's tasks).

### Component 1 — Bottom-anchor layout

**File/fn:** `render.rs::render_conversation` (221), `render.rs::max_scroll_offset` (305).

Factor the wrapped-row sum out of `max_scroll_offset` into
`fn wrapped_rows(lines: &[Line], width: u16) -> u16` (the existing `ceil(line_width /
inner_width)` estimate, min 1 per line). `max_scroll_offset` becomes
`wrapped_rows(lines, width).saturating_sub(height)` (behavior identical).

In `render_conversation`, after computing `lines`:

```
let content_rows = wrapped_rows(&lines, inner.width);
let max_scroll   = content_rows.saturating_sub(inner.height);
state.transcript_max_scroll.set(max_scroll);          // unchanged semantics
let top_pad      = inner.height.saturating_sub(content_rows);   // > 0 only when content fits
```

When `top_pad > 0`, prepend `top_pad` blank `Line::raw("")` rows to `lines` before building
the `Paragraph`; the `(follow, scroll) → offset` block and `.scroll((offset, 0))` are
**unchanged**. `top_pad` is `0` whenever content overflows (`content_rows >= inner.height`),
so the overflow follow/scroll path — `run.follow`, `run.scroll`, `transcript_max_scroll` —
is untouched; when content fits, `max_scroll == 0` and `offset == 0`, so the prepended
blanks push the content flush to the bottom with quiet space at the top.

The empty-session hint (`render.rs:243-254`, "No runs yet") **may** be bottom-anchored the
same way for consistency (optional; see Open questions).

### Component 2 — Turn / role renderer

**Files/fns:** `render.rs::conversation_lines` (387), `entry_lines` User arm (495), a new
`user_turn_lines`, and the assistant-header push site (434-437).

- **User turn.** Replace the single `› {text}` line (`entry_lines::User`) with a two-part
  block via `user_turn_lines`:
  - header line `You` styled `Style::default().fg(theme.focus.active).add_modifier(BOLD)`;
  - body: the message text, each wrapped source line prefixed with a two-space indent
    (gutter), styled `theme.text.primary`.
- **Assistant header.** At the header push in `conversation_lines` (where `run` is in
  scope), render `⏺ codypendent` + (` · {model}` only when `run.model` is `Some`) — the
  name in `theme.agent.tool` (or `theme.text.heading`) bold, the model in `theme.text.muted`.
  Never emit a placeholder model when `run.model` is `None` (honesty).
- **Spacing.** Emit one blank `Line::raw("")` before each turn boundary — before a `You`
  header (as today, after the first turn) and before a `⏺ codypendent` header — with no
  double blanks and no leading blank on the very first turn. `awaiting_header` /
  `seen_user_turn` threading across runs stays as-is.

Role differentiation uses only `Theme` tokens (readable in light and dark): user =
`theme.focus.active`; assistant name = `theme.agent.tool`/`theme.text.heading`, model =
`theme.text.muted`; assistant prose stays `theme.agent.model_text` (`model_entry_lines`,
578).

### Component 3 — Backstage + error fold

**Files/fns:** `render.rs::backstage_lines` (766), `note_lines` (723), the `Completed`
arm of `entry_lines` (521-541); `state.rs::TranscriptEntry::Completed` (280-281);
`reduce.rs` RunCompleted fold (525).

- **Backstage (quiet, one line, expandable).** Keep the existing `backstage_lines` fold:
  collapsed = one dim line `⋯ context · N lines · memory updated` (`theme.text.muted`);
  expanded (`▾`) = the raw note bodies indented beneath. Confirm at most one Backstage entry
  per turn (already enforced by the reducer). Register the collapsed line's `Rect` in the
  hit-map → `Action::Expand` (Component 7/8), so a click toggles it exactly as `Enter` on a
  selected entry does today. `note_lines` short-note inline rendering is unchanged; only its
  `Rect` is registered when foldable (`line_count > NOTE_INLINE_LINE_THRESHOLD`).
- **Concise error line + raw on expand.** Add a client-only `expanded: bool` field to
  `TranscriptEntry::Completed` (`Completed { disposition, expanded }`) — `TranscriptEntry`
  is a tui-side type; only `disposition` is wire data, so this mirrors the `expanded` flags
  already on `Note`/`Backstage`/`ToolCard`/`PatchSummary`. The reducer pushes
  `expanded: false` (`reduce.rs:525`).
  - Rendering the `Failed { reason }` case: a marker (`▸`/`▾`) + `✗ {summary}` in
    `theme.status.error`, where `summary = summarize_error(reason)`. Collapsed shows only
    the summary; expanded shows the summary plus the full raw `reason` wrapped and indented
    (`theme.text.muted`), so **no information is lost**.
  - `fn summarize_error(raw: &str) -> String`: a pure heuristic. Split the chain on `": "`;
    map a recognized outermost segment to a friendly category (e.g. `model driver error` /
    `model stream failed` → `model error — the provider request failed`; `service error` /
    `request failed` → `provider request failed`; otherwise use the outermost segment
    verbatim as the summary). The mapping table lives beside the fn; unknown chains degrade
    to the first segment (never a crash, never lost detail — the raw is one expand away).
  - `Completed { Completed { .. } }` (success) still renders nothing; `Cancelled` is
    unchanged (already short). The `#[non_exhaustive]` catch-all (`render.rs:538`) stays.

### Component 4 — Palette styling ("command centre")

**Files/fns:** `render.rs::render_palette` (2365); `palette.rs::PaletteEntry` (58),
`COMMANDS` (75).

- **Columns.** Render each row as aligned columns computed from `rows[1]` inner width:
  `marker(2) │ title (fixed width, e.g. 20) │ description (fills, truncated with `…`) │
  key (right-aligned, fixed width e.g. 4)`. Titles/descriptions/keys line up down the list.
- **Drop `[—]`.** When `entry.key == "—"` render **no** key chip (blank), not `[—]`. Keep
  `"—"` as the data sentinel in `palette.rs` so `every_command_has_a_nonempty_title_and_key`
  stays green; the renderer treats `"—"` as "no binding".
- **Groups.** Add `pub group: &'static str` to `PaletteEntry`; tag each command:
  `Run` (New run, Steer, Pause/resume, Cancel), `Models` (Model picker, Provider catalog),
  `Browse` (Docs, Edges, Workflow, Blackboard, Skills, Memory), `Session` (Toggle layout,
  Help, Detach, New conversation). When the query is **empty**, render a dim non-selectable
  group-label row (`theme.text.muted`) whenever the group changes; when filtering, hide the
  labels. Group labels are render-only decoration and are **not** part of `filtered()`, so
  the selectable index math is unchanged.
- **Selection + padding.** Selected row: full-width `theme.selection_style()` with a leading
  `›` in `theme.focus.active` and one cell of interior padding; non-selected rows get a
  leading space. The header hint gains `· click a row`.
- **Click.** Register each selectable row's `Rect` → `Action::ActivateRow(filtered_index)`.

### Component 5 — Shortcuts footer bar

**Files/fns:** new `render.rs::render_shortcuts_bar`; new `input::footer_hints()`; the root
layout in `render.rs::render` (43-59).

- **Placement.** Add a fourth row to the root vertical layout, **below** `render_status_line`:
  `[ Min(3) transcript, Length(COMPOSER_HEIGHT) composer, Length(1) status line, Length(1)
  shortcuts footer ]`. `render_status_line` is otherwise unchanged (its ambient fields and
  modal right-hand cue — approve/reject, "PgDn ↧ latest" — stay, keeping the honesty
  invariant intact). The footer is the persistent, derived, clickable shortcut strip.
- **Derivation.** `input::footer_hints() -> &'static [FooterHint]` returns an ordered,
  curated subset. Each `FooterHint { binding: &'static KeyBinding, label: &'static str,
  action: Action }` **references** a real `KEY_BINDINGS` entry (not a free-text key), pairs
  it with a compact display `label`, and names the `Action` a click fires. Default set (with
  the binding each derives from): `⏎ send` (`Enter` → `InputSubmit`), `/ commands`
  (`/` → `OpenPalette`), `↑↓ scroll` (`PgUp / PgDn` → `ScrollPageDown`), `F2 layout`
  (`F2` → `ToggleLayout`), `? help` (`?` → `Help`), `q detach` (`Ctrl-C`/`q` → `Detach`).
  The drift-guard test asserts each `FooterHint.binding` is one of `KEY_BINDINGS` (by
  pointer/`keys` identity), so the footer can never drift from the real bindings even though
  its display labels are compacted.
- **Rendering.** `·`-separated chips in `theme.surface.overlay` background, key glyph in
  `theme.focus.active`, label in `theme.text.muted`; drop chips right-to-left on narrow
  terminals (reuse the status-line width-tier idea).
- **Click.** Register each chip's `Rect` → its `Action` (all existing Actions).

### Component 6 — Status-bar & header tidy

**Files/fns:** `render.rs::render_status_line` (812), `header_chrome` (289).

- **Dedupe model.** Remove the `model` field from `render_status_line` (drop the `full`
  block at 865-874). The model stays in `header_chrome` (the persistent pane-title anchor:
  `model · mode[ · cost]`) and on each assistant turn header (Component 2). This satisfies
  the explicit "model in both header chrome and status line — dedupe" (status line loses
  it). See Open questions for the header-chrome-vs-turn-header overlap decision.
- **Group fields.** Keep the ambient left group ordered `mode · state · ctx · cost · wt ·
  approvals` with the existing width tiers; keep the sep/field helpers. **Honesty invariant
  preserved:** unmeasured `ctx`/`cost`/`wt` still render `—`, never a fabricated number.
- The right-hand modal cue (approve/reject when an approval is pending; "PgDn ↧ latest" when
  scrolled up) is retained — it is state-dependent, not a "common shortcut", so it stays on
  the status line while the derived common shortcuts live in the footer (Component 5).

### Component 7 — Hit-test map

**Files:** `state.rs` (new field + helpers), `input.rs` (resolution), `action.rs` (new
variants), `crates/cli/src/tui.rs` (event-loop wiring).

- **Cache (mirror of `transcript_max_scroll`).** Add to `AppState`:
  ```
  pub hit_map: RefCell<Vec<(Rect, Action)>>,
  ```
  `RefCell` (not `Cell`) because the payload is a non-`Copy` `Vec`. It satisfies AppState's
  derived `Clone`/`PartialEq`/`Debug` (`Rect`: Copy+Eq+Debug; `Action`: Clone+PartialEq+
  Debug). Imports: `use std::cell::RefCell; use ratatui::layout::Rect; use
  crate::action::Action;` (no dependency cycle — `action.rs` already imports from
  `state.rs`; same-crate modules may import mutually). `Default`/`new()` initialize it empty.
  Cloning/`PartialEq` on this transient render cache is harmless — exactly like
  `transcript_max_scroll` (both default empty/zero and are only populated during render, so
  reducer-only tests keep comparing equal).
- **Write path.** `render.rs::render` (the single entry point) clears the map at the very
  start (`state.hit_map.borrow_mut().clear()`), then each widget registers via a helper
  `AppState::register_hit(&self, rect: Rect, action: Action)`
  (`self.hit_map.borrow_mut().push((rect, action))`). Registration order is z-order: base
  layout first, overlays last (so an open overlay's rows sit on top).
- **Read path.** `input.rs::hit_test(hit_map: &[(Rect, Action)], col: u16, row: u16) ->
  Option<Action>` iterates in **reverse** (last-registered = topmost wins) and returns the
  first rect containing the point, where "contains" is `col >= r.x && col < r.x + r.width &&
  row >= r.y && row < r.y + r.height` (equivalently `Rect::contains(Position { x: col, y:
  row })`, ratatui 0.29).
- **Click routing.** Extend `map_event`/`map_mouse` (`input.rs:109/270`) to take
  `hit_map: &[(Rect, Action)]`. Only the `Down(MouseButton::Left)` arm changes: from
  `Action::NoOp` to `hit_test(hit_map, mouse.column, mouse.row).unwrap_or(Action::NoOp)`.
  Wheel arms (scroll/select) are unchanged. `mouse.row` is already carried by the
  `MouseEvent` (no signature gap). The event loop call site
  (`crates/cli/src/tui.rs:591`) becomes
  `map_event(&event, state.input_mode(), *width, &state.hit_map.borrow())` — all shared
  borrows of `state`, no conflict. `width` and `pane_at` are retained (their test stays
  green); pane click-to-focus now flows through the hit-map (Component 8) rather than
  `pane_at`, which remains the pure column→pane helper.
- **Modal scrim.** When a non-`None` overlay is open, register a full-screen `Rect` →
  `Action::Dismiss` **first** (bottom of z-order) as a scrim, so a click outside the overlay
  box closes it rather than leaking to the transcript underneath. The overlay's own rows,
  registered later, win inside the box. (The approval modal registers **no** scrim — an
  approval must be decided, not dismissed by an outside click.)
- **New client Actions** (`action.rs`, client-only — no `Intent`, no wire):
  - `ActivateRow(usize)` — "the user clicked list row N in the open overlay/list".
  - `SelectRun(usize)` — "the user clicked run N in the workspace runs pane".

### Component 8 — Click router (registration + reducer + parity)

**Files:** each render fn (registration), `reduce.rs` (new arms), `input.rs` (parity test).

- **Reducer arms** (`reduce.rs::reduce`):
  - `Action::ActivateRow(n)` → set the open overlay's `selected` (and, for the model/
    provider pickers, the mirrored `selected_model`/`selected_provider` via the existing
    `filter_*`/`nav` resolution) to `n`, then run the **same** activation the keyboard's
    `Enter`/`InputSubmit` runs for that overlay — i.e. delegate to `submit_prompt` for
    `Palette`/`ModelPicker`/`ProviderPicker`/`AddModelPick`. This reuses
    `run_palette_command`, model staging, `enter_add_model_flow`, and the `Intent::AddModel`
    path unchanged. A no-op for any other overlay.
  - `Action::SelectRun(n)` → `state.selected_run = n` clamped to `runs.len()` (mirrors
    `cycle_run`/`clamp`).
- **Registration (each surface → the Action it fires):**

  | Surface | Render fn | Registered Action |
  |---|---|---|
  | Command-palette row | `render_palette` (2418) | `ActivateRow(filtered_index)` |
  | Model-picker row | `render_model_picker` (1247) | `ActivateRow(row)` |
  | Provider-picker row | `render_provider_picker` (1466) | `ActivateRow(row)` |
  | Add-model pick row | `render_add_model_pick` (2753) | `ActivateRow(row)` |
  | Run-list entry (workspace) | `render_runs_pane` (92) | `SelectRun(idx)` |
  | Footer shortcut chip | `render_shortcuts_bar` (new) | the chip's existing `Action` |
  | Backstage collapsed line | `backstage_lines` (766) | `Expand` |
  | Foldable note line | `note_lines` (723) | `Expand` |
  | Failed-error line | `entry_lines::Completed` (526) | `Expand` |
  | Workspace pane | `render_workspace`/pane fns (67-114) | `FocusPane(pane)` |
  | Composer (overlay open) | `render_composer` (325) | `Dismiss` |
  | Wheel / scroll | `map_mouse` (already) | `ScrollPage*` / `SelectPrev/Next` |

  Notes: `ActivateRow`/`SelectRun`/`Expand`/`FocusPane`/`Dismiss` all have keyboard paths.
  Composer registers `Dismiss` **only** when a non-modal overlay is open (so clicking the
  composer returns you to typing); with no overlay it is already the text sink and registers
  nothing. `Expand` on a backstage/note/error line requires that entry to be the selected
  one for the existing `expand_selected` to toggle it — the click therefore also needs the
  entry's index; where the selected-entry model does not already point at it, register
  `ActivateRow`-style selection first (plan task pins the exact `expand_selected`
  interaction; see Open questions).

- **Mouse-parity invariant.** Every registered `Action` is reproducible by keyboard:
  `ActivateRow(n)` ≡ `SelectNext`×k then `InputSubmit`; `SelectRun(n)` ≡ `PrevRun`/`NextRun`;
  `Expand` ≡ `Enter`; `FocusPane` ≡ `Tab`; footer chips ≡ their documented keys;
  `Dismiss` ≡ `Esc`. Extend `KEY_BINDINGS` with a documented `click a row` /
  `click a pane` gesture (`mouse: Some(...)`) whose `keys` name the equivalent
  (`↑↓ + Enter`, `Tab`), and extend `every_mouse_gesture_has_a_keyboard_equivalent` to
  assert click resolution (a rect resolves to its Action) plus the keyboard reachability of
  each such Action.

## Data flow

```
render(frame, state, theme)
  ├─ state.hit_map.borrow_mut().clear()
  ├─ draws transcript/composer/status/footer/overlays
  └─ each interactive widget → state.register_hit(rect, action)      (geometry as data)

crossterm Event::Mouse(Down(Left)) at (col,row)
  └─ event loop: map_event(&ev, mode, width, &state.hit_map.borrow())
       └─ map_mouse → hit_test(hit_map, col, row) → topmost Action
            └─ reduce(state, action)   (pure; ActivateRow/SelectRun/Expand/… fold to the
                                        same helpers the keyboard path uses)
```

`transcript_max_scroll` and `hit_map` are the two render→input caches; both are
interior-mutable, one-frame-fresh layout metrics, never domain state.

## Error handling / edge cases

- **Content exactly fills the viewport:** `top_pad == 0`, `max_scroll == 0` — renders from
  the top with no scrolling (correct; no off-by-one padding).
- **Overflowing transcript:** `top_pad == 0` — follow/scroll path byte-for-byte unchanged.
- **Unknown model:** assistant header shows `⏺ codypendent` with no ` · <model>` (honesty).
- **Empty backstage:** `backstage_lines` still renders nothing when both counts are empty.
- **Unknown error chain:** `summarize_error` degrades to the outermost segment; the raw
  chain is always available on expand — no lost detail, no crash.
- **Empty palette filter / no matches:** the "no matching command" row registers no click
  target; group labels are suppressed while filtering.
- **Click on the overlay scrim (outside the box):** `Dismiss` (closes the overlay); a click
  inside hits the box's rows. Approval modal has no scrim (must be decided).
- **Click on empty transcript space / gutter:** no registered rect → `NoOp`.
- **Stale geometry:** the map is cleared every frame before registration, so a resize or
  layout flip can never resolve a click against last frame's rects.
- **Narrow terminals:** footer and status drop fields right-to-left; the palette columns
  truncate the description with `…` (title/key columns preserved).
- **Forward-compat:** `Unsupported` cells and the `#[non_exhaustive]` disposition catch-all
  still render (never crash).

## Testing

`render.rs` already tests via `TestBackend` + `render_to_string` (contains-asserts,
`render.rs:3102`); `reduce.rs`/`input.rs`/`palette.rs` unit-test the pure fns. New/updated:

- **Bottom-anchor:** a short transcript renders its content in the bottom rows with blank
  rows above (assert leading blank lines then the last turn on the final content row); a
  tall transcript still tails-follows (existing scroll tests stay green); `wrapped_rows`
  refactor keeps `max_scroll_offset` values identical.
- **Turns/roles:** `You` header + indented body renders; `⏺ codypendent · <model>` renders,
  and `⏺ codypendent` (no model) when `run.model` is `None`; a blank line separates turns;
  no double blanks; first turn has no leading blank.
- **Backstage/notes fold:** collapsed `⋯ context …` line renders quiet; `Expand` (and a
  click) toggles to raw; a short note still renders inline.
- **Error fold:** a `Failed` disposition renders `✗ {summary}` collapsed (assert the raw
  chain is absent); `Expand` reveals the full raw chain (assert both summary and raw
  present); `summarize_error` unit tests map known chains and degrade unknown ones.
- **Palette layout:** columns align; `[—]` never appears; unbound commands render a blank
  key; group labels appear on empty query and vanish while filtering; the selected row is
  highlighted; filtering/selection indices unchanged (existing palette tests green).
- **Footer derivation:** `footer_hints()` every key is present in `KEY_BINDINGS` (drift
  guard); the footer row renders the expected chips.
- **Status/header dedupe:** the status line no longer contains the model; `ctx`/`cost`/`wt`
  still render `—` when unmeasured (honesty regression guard).
- **Hit-test + click routing:** `hit_test` returns the topmost rect's Action (z-order:
  a later-registered overlay rect wins over an earlier base rect at the same point);
  outside-all → `None`; `map_event` on a left click over a registered rect returns that
  Action; the reducer arms: `ActivateRow(n)` runs the same effect as selecting row `n` +
  `InputSubmit` for palette/pickers/add-model; `SelectRun(n)` sets `selected_run` clamped.
- **Mouse parity:** the extended `every_mouse_gesture_has_a_keyboard_equivalent` asserts
  each clickable Action is keyboard-reachable and the `KEY_BINDINGS` mouse entries name a
  non-empty key.
- **Existing tests:** every render/input test asserting the old top-anchored layout, the old
  `› {text}` user line, the old palette row shape, or the model-in-status-line is updated to
  the new shape (kept meaningful, not deleted). `crossterm 0.28`/`ratatui 0.29` confirmed.

## Constraints

- **Pure-reducer TUI:** widgets do NO I/O; render caches geometry into interior-mutable
  state; the reducer stays a pure fn of `(state, action)`. The hit-map is DATA produced by
  render and read by input — same as `transcript_max_scroll`.
- **Client-only:** NO protocol / daemon / wire / golden-vector change. Clicks route to
  EXISTING `Action`s/`Intent`s (plus two client-only view Actions — no new Intents).
- **Mouse-parity invariant preserved** (every click has a keyboard equivalent).
- **Honesty invariant:** unmeasured ctx/cost/wt stay `—` — never a fabricated number.
- **Theme-aware:** all new styling uses `Theme` colors and must read well in BOTH light and
  dark.
- **Every existing render/input test updated;** new tests for bottom-anchoring, folded
  notes, error collapse, palette layout, footer derivation, and click→action hit-testing +
  the parity invariant.
- **Scope discipline (YAGNI):** no unrelated refactors; hover-highlight is optional.
- Clippy runs on Linux CI — gate any macOS-only test helper.
- Foreign files never touched: `README.md`, `docs/cli-and-tui-user-guide.md`, `docs/docs/*`,
  `ROADMAP.md`, `.superpowers/`.

## Spec-vs-real reconciliation (verified against the code)

- `render_conversation` **221**, `max_scroll_offset` **305**, `conversation_lines` **387**,
  `entry_lines` **478**, `model_entry_lines` **578**, `backstage_lines` **766**, `note_lines`
  **723**, `render_status_line` **812**, `header_chrome` **289** — all confirmed at the given
  (or ±a few) lines.
- Command palette: `render_palette` is at **2365** (not ~2410; that is the `filtered()`
  call). Command data is `palette.rs::COMMANDS`; `[—]` is `PaletteEntry.key == "—"`
  (`palette.rs:130,138,176`).
- `map_event` **109**, `map_mouse` **270**, `pane_at` **295**, `KeyBinding`/`KEY_BINDINGS`
  **22/34**, parity test **546** — confirmed. **Correction:** `pane_at`/`FocusPane` are
  currently **dead** (no callers in `crates/cli/src/tui.rs`; `map_mouse`'s left-click arm
  returns `NoOp`). The spec revives click-to-focus through the hit-map, keeping `pane_at` +
  its test as the pure helper.
- `transcript_max_scroll: Cell<u16>` is set in render (**render.rs:265**) and read by the
  reducer — confirmed as the interior-mutability pattern to mirror. The hit-map uses
  `RefCell<Vec<(Rect, Action)>>` because its payload is non-`Copy`.
- Event loop: `event_loop` **463**; the click needs `(col,row)` — the `MouseEvent` already
  carries both (`input.rs` test constructs `MouseEvent { column, row, .. }`), so `map_event`
  only lacks the hit-map, not the row. Width tracked at **tui.rs:590**; `map_event` call at
  **591**.
- Overlay set, `Overlay::Palette/ModelPicker/ProviderPicker/AddModelPick`, `RunView.follow/
  scroll`, `AppState` fields — confirmed in `state.rs`; palette/picker submit paths
  (`submit_prompt` **1067**, `run_palette_command` **1461**, `nav` **613**) confirmed as the
  reuse targets for `ActivateRow`.

## Open questions / decisions for the controller

1. **New client Actions.** Click-to-activate-a-row needs a "select row N then activate"
   semantic that no single existing `Action` expresses (`SelectPrev/Next` can't target an
   arbitrary row). The spec adds **two client-only view Actions** — `ActivateRow(usize)` and
   `SelectRun(usize)` — that fold into the exact keyboard reducer helpers; **no new
   `Intent`s, no wire/daemon change.** This is consistent with "do NOT invent new daemon
   commands" but is not a literal "reuse existing Actions". Confirm this is acceptable (the
   only alternative — decomposing into repeated `SelectNext` + `InputSubmit` — cannot
   address a clicked row directly and is uglier).

2. **Model dedupe placement.** The spec drops the model from the **status line** and keeps
   it in **header_chrome** *and* on each **assistant turn header** (the latter is the
   approved A.2 shape). That satisfies the literal "header-chrome vs status-line" dedupe but
   leaves header-chrome + turn-header both showing the model (a persistent anchor vs
   per-turn attribution — deliberate, like Codex/Claude-Code). If tighter dedupe is wanted,
   the fallback is: drop model from `header_chrome` (title becomes `mode[ · cost]`) and let
   the turn headers be the only model readout. Which do you prefer?

3. **Click-to-expand vs click-to-activate on transcript fold lines.** Backstage/note/error
   fold lines toggle via the existing `expand_selected` (`Enter` on the *selected* entry).
   A click should expand the clicked line regardless of the current selection — which means
   the click must also move the transcript selection to that entry before toggling. The plan
   task will pin the exact interaction (either extend `Expand` to carry an index, or select-
   then-expand). Flagging because it is the one place the existing selected-entry model and a
   direct click don't line up. (Optional hover-highlight is deferred — YAGNI.)
