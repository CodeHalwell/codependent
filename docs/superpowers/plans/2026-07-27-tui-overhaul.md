# TUI overhaul — visual refresh + clickable everything + transcript virtualization — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refresh the Codypendent TUI to a bottom-anchored, role-differentiated, quiet, Codex-like chat with a derived shortcut footer and clickable surfaces, and fix the large-transcript freeze/crash by virtualizing the transcript render — all client-only, no protocol/daemon change.

**Architecture:** Pure-reducer TUI (`crates/tui`): widgets do no I/O; `render` caches geometry (scroll metrics, a new hit-test map) into interior-mutable state; `reduce` stays a pure fn of `(state, action)`. The transcript is virtualized — each frame measures wrapped-row height cheaply (borrowed widths, no per-row allocation) and builds only the visible window of `Line`s (± the partial edge lines), eliminating the O(n²) per-frame `String` churn that froze/killed the client on a 256 KB review. Clicks route through a render-time hit-test map to the **same** `Action`s the keyboard produces (plus two client-only view Actions), preserving the mouse-parity invariant.

**Tech Stack:** Rust, `ratatui` 0.29, `crossterm` 0.28. No new crate dependency. Tests via `ratatui::backend::TestBackend` + a `render_to_string` snapshot helper (render side) and pure-fn unit tests (reduce/input/palette).

## Global Constraints

Every task's requirements implicitly include this section (values copied verbatim from `docs/superpowers/specs/2026-07-27-tui-overhaul-design.md`):

- **Pure-reducer TUI:** widgets do NO I/O; render caches geometry into interior-mutable state; the reducer stays a pure fn of `(state, action)`. The hit-map and scroll metrics are DATA produced by render and read by input — same pattern as the existing `transcript_max_scroll: Cell<u16>`.
- **Client-only:** NO protocol / daemon / wire / golden-vector change. Clicks route to EXISTING `Action`s/`Intent`s plus exactly two client-only *view* Actions (`ActivateRow(usize)`, `SelectRun(usize)`) — no new `Intent`s, no daemon commands.
- **Mouse-parity invariant preserved:** every clickable Action is reproducible by keyboard.
- **Honesty invariant:** unmeasured `ctx`/`cost`/`wt` stay `—`, never a fabricated number; never emit a placeholder model when unknown.
- **Theme-aware:** ALL styling via `Theme` tokens (there is not one literal color in `render.rs`); must read well in BOTH light and dark.
- **Virtualization MUST bound per-frame allocation** to viewport size: the unbounded 256 KB model-text row churn is eliminated (its rows are measured by borrowed width and built only within the viewport). Scroll stays correct via a cheap measure pass.
- **Every existing render/input test updated** (kept meaningful, not deleted); new tests for bottom-anchor, virtualization, folded notes, error collapse, palette layout, footer derivation, hit-test click→Action, and the parity invariant.
- **Scope discipline (YAGNI):** no unrelated refactors; hover-highlight is deferred.
- **Clippy runs on Linux CI:** no dead code (an unused `fn` fails the lint); gate any macOS-only test helper.
- **Foreign files never touched:** `README.md`, `docs/cli-and-tui-user-guide.md`, `docs/docs/*`, `ROADMAP.md`, `.superpowers/`.

---

## Shared Interfaces

Exact signatures tasks depend on. A task's implementer sees only their own task; this block is how they learn neighboring names and types.

**Task 1 → Tasks 2, 3, 8 (transcript virtualization core, in `render.rs`):**

```rust
/// Wrapped-row height of a line `columns` display-columns wide in an
/// `inner_width` viewport: ceil(columns/inner_width), min 1 (a blank row still
/// occupies a row). Replaces the inline sum in the removed `max_scroll_offset`.
fn line_rows(columns: usize, inner_width: usize) -> u16;

/// One transcript row, before placement. `Built` wraps an already-styled `Line`
/// (structural rows + every non-`Model` entry, reusing the existing builders);
/// `Model` borrows a streamed source line so measuring the whole transcript
/// allocates nothing per model-text row (the crash fix). `hit_entry` tags a
/// fold head as a click target (set in Task 8; `None` otherwise).
struct Row<'a> { kind: RowKind<'a>, hit_entry: Option<usize> }
enum RowKind<'a> {
    Built(Line<'a>),
    Model { prefix: &'static str, text: &'a str, caret: bool, style: Style },
}
impl<'a> Row<'a> {
    fn built(line: Line<'a>) -> Self;
    fn model(prefix: &'static str, text: &'a str, caret: bool, style: Style) -> Self;
    fn columns(&self) -> usize;          // alloc-free display width
    fn rows(&self, inner_width: u16) -> u16;
    fn into_line(self, theme: &Theme) -> Line<'a>;
}

/// Visit every transcript row in scroll order as a cheap `Row`. Both the measure
/// pass and the windowed build drive THIS single walk, so their row sequences
/// can never drift. `selected_run` is used only to tag fold-head click targets
/// (Task 8); pass `usize::MAX` to tag nothing.
fn for_each_row<'a>(
    runs: &'a [RunView], theme: &Theme, selected_run: usize, visit: impl FnMut(Row<'a>),
);

/// Total wrapped-row height of the whole transcript (measure pass; O(#non-Model
/// entries) bounded allocation, zero per-model-row allocation).
fn transcript_rows(runs: &[RunView], theme: &Theme, inner_width: u16) -> u16;

/// Build ONLY the rows whose wrapped range intersects `[first_row, first_row+height)`.
/// Returns (visible lines, intra-first-line scroll r0, fold-head hits as
/// (index-into-lines, entry-index)). Per-frame `Line`/`String` allocation is
/// bounded by the viewport (plus the few bounded non-Model entries).
fn build_transcript_window<'a>(
    runs: &'a [RunView], theme: &Theme, inner_width: u16,
    first_row: u16, height: u16, selected_run: usize,
) -> (Vec<Line<'a>>, u16, Vec<(usize, usize)>);
```

**Task 3 → (in `render.rs`):**

```rust
/// Map a nested error chain to a concise, friendly one-line summary. Pure; never
/// panics; unknown chains degrade to the outermost segment. The full raw chain
/// is always available on expand, so no detail is lost.
fn summarize_error(raw: &str) -> String;
```

**Task 3 → (in `state.rs`):** `TranscriptEntry::Completed { disposition: RunDisposition, expanded: bool }`.

**Task 4 → (in `palette.rs`):** `PaletteEntry { command, title, description, key, group: &'static str }`.

**Task 5 → Task 8 (in `input.rs` / `render.rs`):**

```rust
/// One footer chip: a compact display label + the Action a click fires, keyed to
/// a real KEY_BINDINGS entry so it can never drift from the bindings.
pub struct FooterHint { pub binding: &'static KeyBinding, pub label: &'static str, pub action: Action }
pub fn footer_hints() -> &'static [FooterHint];
fn render_shortcuts_bar(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme);
```

**Task 7 → Task 8 + `crates/cli/src/tui.rs` (hit-map + routing):**

```rust
// state.rs
pub hit_map: RefCell<Vec<(Rect, Action)>>,          // field on AppState
pub fn register_hit(&self, rect: Rect, action: Action);   // self.hit_map.borrow_mut().push(..)

// action.rs
Action::ActivateRow(usize),   // "activate row N of the active list surface / transcript"
Action::SelectRun(usize),     // "select run N in the runs pane"

// input.rs
pub fn hit_test(hit_map: &[(Rect, Action)], col: u16, row: u16) -> Option<Action>;
pub fn map_event(event: &Event, mode: InputMode, width: u16, hit_map: &[(Rect, Action)]) -> Action;

// reduce.rs
fn activate_row(state: &mut AppState, n: usize);          // overlay row OR base-transcript fold
fn set_overlay_selected(state: &mut AppState, n: usize);  // mirrors `nav`'s picker resolution
```

---

## Task 1: Transcript virtualization + bottom-anchor + scroll-offset clamp

The crash/freeze fix and the empty-space fix. Rewrites `render_conversation`; replaces `conversation_lines`/`max_scroll_offset` with a measure/build split that bounds per-frame allocation to the viewport.

**Files:**
- Modify: `crates/tui/src/render.rs` — add `line_rows`, `Row`/`RowKind`, `for_each_row`, `transcript_rows`, `build_transcript_window`; rewrite `render_conversation` (221-280); delete `conversation_lines` (387-463) and `max_scroll_offset` (305-320). Keep `entry_lines`, `model_entry_lines`, `activity_status_line`, `tool_card_lines`, `patch_lines`, `note_lines`, `backstage_lines` unchanged (entry_lines still references model_entry_lines, so nothing goes dead).
- Test: `crates/tui/src/render.rs` `#[cfg(test)] mod tests`.

**Interfaces:**
- Produces: `line_rows`, `Row`, `for_each_row`, `transcript_rows`, `build_transcript_window` (see Shared Interfaces). Consumed by Tasks 2, 3, 8.
- Consumes: existing `RunView`, `RunActivity`, `TranscriptEntry`, `entry_lines`, `activity_status_line`, `Theme`.

**Design notes (why it bounds allocation):** `for_each_row` walks the same structure the old `conversation_lines` did. For every entry EXCEPT `Model` it calls the existing `entry_lines` into a reused scratch and wraps each `Line` as `Row::Built` (bounded: ≤ `MAX_TRANSCRIPT_ENTRIES` per run, each small). The `Model` entry — the one unbounded body (up to `MAX_MODEL_ENTRY_BYTES` = 256 KB) — is emitted as `Row::Model` per source line, **borrowing** the `&str` (no `String`). The **measure** pass (`transcript_rows`) sums `Row::rows` using `Row::columns`, which for a `Model` row is `Span::raw(prefix).width() + Span::raw(text).width() + caret` — `Span::raw` borrows, `.width()` is display width, so measuring the 256 KB review allocates nothing per row. The **build** pass (`build_transcript_window`) calls `into_line` (which `format!`s the model line) only for rows inside the viewport window, so per-frame `Line`/`String` allocation is O(viewport). The `Paragraph::scroll((r0,0))` clips the partially-visible first window line so alignment is exact.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `render.rs`:

```rust
#[test]
fn a_short_transcript_is_anchored_to_the_bottom() {
    // One brief turn in a tall viewport: quiet space pools at the TOP, content
    // sits flush above the composer.
    let mut s = AppState::new();
    let run_id = RunId::new();
    reduce(&mut s, system_ev(EventBody::RunStarted {
        run_id, objective: "tiny".to_owned(), mode: AgentMode::Build,
    }));
    reduce(&mut s, system_ev(EventBody::ModelStreamDelta {
        run_id, text: "one short reply".to_owned(),
    }));
    let out = render_to_string(&s, 80, 24);
    let rows: Vec<&str> = out.lines().collect();
    // The transcript pane is rows[1..] under the 1-row border; its first content
    // row (row index 1) is blank (top padding), and the reply is in the lower
    // rows just above the composer.
    assert!(rows[2].trim().is_empty(), "quiet space at the top:\n{out}");
    let reply_row = rows.iter().position(|r| r.contains("one short reply"))
        .expect("reply rendered");
    assert!(reply_row > 12, "content anchored toward the bottom (row {reply_row}):\n{out}");
}

#[test]
fn build_transcript_window_materializes_only_the_viewport() {
    // A pathological single Model entry of thousands of source lines. The build
    // pass must materialize O(viewport) lines, not O(history).
    let mut s = AppState::new();
    let run_id = RunId::new();
    reduce(&mut s, system_ev(EventBody::RunStarted {
        run_id, objective: "huge".to_owned(), mode: AgentMode::Build,
    }));
    let mut big = String::new();
    for i in 0..5000 { big.push_str(&format!("line {i}\n")); }
    reduce(&mut s, system_ev(EventBody::ModelStreamDelta { run_id, text: big }));

    let theme = Theme::dark();
    let inner_width = 78;
    let height = 20;
    let total = transcript_rows(&s.runs, &theme, inner_width);
    assert!(total >= 5000, "measure sees the whole history: {total}");
    let (lines, _r0, _hits) =
        build_transcript_window(&s.runs, &theme, inner_width, total.saturating_sub(height), height, 0);
    assert!(
        lines.len() <= height as usize + 4,
        "the build materializes O(viewport) lines, not O(history): {}",
        lines.len()
    );
}

#[test]
fn a_tall_transcript_still_tails_the_latest_row() {
    // Overflow path unchanged: following pins to the tail.
    let mut s = AppState::new();
    let run_id = RunId::new();
    reduce(&mut s, system_ev(EventBody::RunStarted {
        run_id, objective: "scrolling".to_owned(), mode: AgentMode::Build,
    }));
    let mut big = String::new();
    for i in 0..200 { big.push_str(&format!("body line {i}\n")); }
    big.push_str("THE FINAL LINE");
    reduce(&mut s, system_ev(EventBody::ModelStreamDelta { run_id, text: big }));
    let out = render_to_string(&s, 80, 20);
    assert!(out.contains("THE FINAL LINE"), "the tail is visible while following:\n{out}");
    assert!(!out.contains("body line 0"), "the head has scrolled off:\n{out}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p codypendent-tui build_transcript_window_materializes_only_the_viewport`
Expected: FAIL — `transcript_rows` / `build_transcript_window` not defined.

- [ ] **Step 3: Add `line_rows`, `Row`, and `for_each_row`**

Add near the top of `render.rs` (after the `use` block, `use std::borrow::Cow;` is NOT needed — `Row::Model` borrows `&'a str` directly):

```rust
/// Wrapped-row height of a line `columns` display-columns wide in an
/// `inner_width` viewport: ceil(columns/inner_width), min 1.
fn line_rows(columns: usize, inner_width: usize) -> u16 {
    let iw = inner_width.max(1);
    let rows = if columns == 0 { 1 } else { columns.div_ceil(iw) };
    u16::try_from(rows).unwrap_or(u16::MAX)
}

/// One transcript row before placement (see module-level virtualization note).
struct Row<'a> {
    kind: RowKind<'a>,
    /// The transcript-entry index this row is a click target for (fold heads in
    /// the selected run). `None` unless tagged (Task 8).
    hit_entry: Option<usize>,
}

enum RowKind<'a> {
    /// An already-styled line (structural rows + every non-`Model` entry).
    Built(Line<'a>),
    /// A streamed model-text source line, borrowed so measuring allocates nothing.
    Model { prefix: &'static str, text: &'a str, caret: bool, style: Style },
}

impl<'a> Row<'a> {
    fn built(line: Line<'a>) -> Self {
        Row { kind: RowKind::Built(line), hit_entry: None }
    }
    fn model(prefix: &'static str, text: &'a str, caret: bool, style: Style) -> Self {
        Row { kind: RowKind::Model { prefix, text, caret, style }, hit_entry: None }
    }
    /// Display width, allocation-free (`Span::raw` borrows; `.width()` is unicode width).
    fn columns(&self) -> usize {
        match &self.kind {
            RowKind::Built(line) => line.width(),
            RowKind::Model { prefix, text, caret, .. } => {
                Span::raw(*prefix).width() + Span::raw(*text).width() + usize::from(*caret)
            }
        }
    }
    fn rows(&self, inner_width: u16) -> u16 {
        line_rows(self.columns(), inner_width as usize)
    }
    fn into_line(self, theme: &Theme) -> Line<'a> {
        match self.kind {
            RowKind::Built(line) => line,
            RowKind::Model { prefix, text, caret, style } => {
                if caret {
                    Line::from(vec![
                        Span::styled(format!("{prefix}{text}"), style),
                        Span::styled("▋", Style::default().fg(theme.text.muted)),
                    ])
                } else {
                    Line::styled(format!("{prefix}{text}"), style)
                }
            }
        }
    }
}

/// Walk the whole session transcript in scroll order, emitting one `Row` per
/// logical line. Mirrors the old `conversation_lines` walk exactly; the `Model`
/// entry is emitted as borrowed `Row::Model` rows (measured cheaply, built only
/// when visible), every other entry reuses the existing `entry_lines` builders.
fn for_each_row<'a>(
    runs: &'a [RunView],
    theme: &Theme,
    selected_run: usize,
    mut visit: impl FnMut(Row<'a>),
) {
    let mut awaiting_header = false;
    let mut seen_user_turn = false;
    let last_run_idx = runs.len().checked_sub(1);
    let mut scratch: Vec<Line> = Vec::new();
    for (run_idx, run) in runs.iter().enumerate() {
        let is_last_run = Some(run_idx) == last_run_idx;
        let last_entry_idx = run.transcript.len().checked_sub(1);
        let mut produced = false;
        for (idx, entry) in run.transcript.iter().enumerate() {
            let streaming_tail = is_last_run
                && last_entry_idx == Some(idx)
                && run.activity == RunActivity::Streaming;
            let is_agent_cell = matches!(
                entry,
                TranscriptEntry::Model { .. }
                    | TranscriptEntry::Tool(_)
                    | TranscriptEntry::Patch(_)
            );
            if matches!(entry, TranscriptEntry::User { .. }) {
                if seen_user_turn {
                    visit(Row::built(Line::raw("")));
                    produced = true;
                }
                seen_user_turn = true;
                awaiting_header = true;
            } else if is_agent_cell && awaiting_header {
                visit(Row::built(Line::styled(
                    "⏺ codypendent",
                    Style::default().fg(theme.focus.active),
                )));
                produced = true;
                awaiting_header = false;
            }
            match entry {
                TranscriptEntry::Model { text } => {
                    let mut rows: Vec<&str> = text.lines().collect();
                    if rows.is_empty() {
                        rows.push("");
                    }
                    let last = rows.len() - 1;
                    let style = Style::default().fg(theme.agent.model_text);
                    for (i, l) in rows.into_iter().enumerate() {
                        let prefix = if i == 0 { "▌ " } else { "  " };
                        visit(Row::model(prefix, l, streaming_tail && i == last, style));
                        produced = true;
                    }
                }
                other => {
                    scratch.clear();
                    entry_lines(other, theme, false, false, &mut scratch);
                    let _ = (run_idx, selected_run, idx); // hit tagging wired in Task 8
                    for line in scratch.drain(..) {
                        visit(Row::built(line));
                        produced = true;
                    }
                }
            }
        }
        if !produced {
            visit(Row::built(Line::styled(
                "(waiting for the agent…)",
                Style::default().fg(theme.text.muted),
            )));
        }
        if let Some(status) = activity_status_line(&run.activity, theme) {
            visit(Row::built(status));
        }
    }
}
```

- [ ] **Step 4: Add `transcript_rows` and `build_transcript_window`; rewrite `render_conversation`; delete the old fns**

Add:

```rust
/// Total wrapped-row height of the whole transcript (the measure pass).
fn transcript_rows(runs: &[RunView], theme: &Theme, inner_width: u16) -> u16 {
    let mut total: u16 = 0;
    for_each_row(runs, theme, usize::MAX, |row| {
        total = total.saturating_add(row.rows(inner_width));
    });
    total
}

/// Build only the rows whose wrapped range intersects `[first_row, first_row+height)`.
fn build_transcript_window<'a>(
    runs: &'a [RunView],
    theme: &Theme,
    inner_width: u16,
    first_row: u16,
    height: u16,
    selected_run: usize,
) -> (Vec<Line<'a>>, u16, Vec<(usize, usize)>) {
    let last_row = first_row.saturating_add(height);
    let mut out: Vec<Line> = Vec::with_capacity(height as usize + 2);
    let mut hits: Vec<(usize, usize)> = Vec::new();
    let mut cursor: u16 = 0;
    let mut scroll: u16 = 0;
    let mut first_seen = false;
    for_each_row(runs, theme, selected_run, |row| {
        let h = row.rows(inner_width);
        let row_start = cursor;
        let row_end = cursor.saturating_add(h);
        cursor = row_end;
        if row_end > first_row && row_start < last_row {
            if !first_seen {
                scroll = first_row.saturating_sub(row_start);
                first_seen = true;
            }
            let hit = row.hit_entry;
            let index = out.len();
            out.push(row.into_line(theme));
            if let Some(entry) = hit {
                hits.push((index, entry));
            }
        }
    });
    (out, scroll, hits)
}
```

Rewrite `render_conversation`'s body from the `let lines = conversation_lines(...)` line (256) onward — keep the `title`/`block`/`inner` setup (221-241) and the empty-runs hint (243-254) exactly as they are — with:

```rust
    let inner_width = inner.width;
    // Measure the whole transcript cheaply, cache the bottom offset (so the
    // reducer's paging leaves/enters follow mode precisely), then BUILD only the
    // visible window — per-frame allocation is bounded by the viewport, not the
    // transcript length (the crash fix).
    let content_rows = transcript_rows(&state.runs, theme, inner_width);
    let max_scroll = content_rows.saturating_sub(inner.height);
    state.transcript_max_scroll.set(max_scroll);
    let (follow, scroll) = state
        .selected_run()
        .map_or((true, 0), |run| (run.follow, run.scroll));
    let mut offset = if follow { max_scroll } else { scroll.min(max_scroll) };
    // Guard the u16 handed to `Paragraph::scroll` — the rewrite must not
    // reintroduce the overflow the old implicit coupling merely avoided.
    offset = offset.min(u16::MAX.saturating_sub(inner.height));

    let (mut lines, r0, _hits) = build_transcript_window(
        &state.runs, theme, inner_width, offset, inner.height, state.selected_run,
    );

    // Bottom-anchor: when the transcript is shorter than the viewport, pool the
    // quiet space at the TOP so content sits flush above the composer. `top_pad`
    // is 0 whenever content overflows, so the follow/scroll path is untouched.
    let top_pad = inner.height.saturating_sub(content_rows);
    if top_pad > 0 {
        let mut padded = Vec::with_capacity(top_pad as usize + lines.len());
        padded.resize(top_pad as usize, Line::raw(""));
        padded.append(&mut lines);
        lines = padded;
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((r0, 0));
    frame.render_widget(paragraph, area);
```

Then DELETE `fn conversation_lines` (387-463) and `fn max_scroll_offset` (305-320). (`activity_status_line` moves logically into `for_each_row`'s caller graph but stays defined; `entry_lines`/`model_entry_lines` stay — `entry_lines` still references `model_entry_lines`, so nothing goes dead.)

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p codypendent-tui`
Expected: the three new tests PASS. Existing transcript tests (`transcript_snapshot_shows_model_tool_and_status`, `the_conversation_renders_every_run_in_one_continuous_scroll`, `a_streaming_cell_shows_a_caret_then_drops_it_on_completion`, `an_idle_run_shows_no_status_row`, tool/patch/note/backstage snapshots) still PASS — the row sequence and styling are byte-identical to the old `conversation_lines` for the same state, only anchored to the bottom when short. If a short-transcript snapshot now finds its content lower in the buffer, that is the intended bottom-anchor; adjust only positional asserts, never `contains` asserts.

Run: `cargo clippy -p codypendent-tui --all-targets -- -D warnings`
Expected: clean (no dead-code from the deletions).

- [ ] **Step 6: Commit**

```bash
git add crates/tui/src/render.rs
git commit -m "perf(tui): virtualize the transcript render + bottom-anchor + scroll clamp

Build only the visible window of transcript lines each frame (measure wrapped
height by borrowed width, build only viewport rows), fixing the O(n^2) String
churn that froze/killed the client on a 256 KB review. Anchor short transcripts
to the bottom; clamp the scroll offset before Paragraph::scroll."
```

---

## Task 2: Turn / role renderer

Distinct `You` and `⏺ codypendent · <model>` headers, gutter indent, breathing spacing.

**Files:**
- Modify: `crates/tui/src/render.rs` — `entry_lines` `User` arm (495-497); the assistant-header emission in `for_each_row` (from Task 1).
- Test: `render.rs` `mod tests`.

**Interfaces:**
- Consumes: Task 1's `for_each_row` (edits its header emission) and `entry_lines`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_user_turn_renders_a_role_header_and_indented_body() {
    let mut s = AppState::new();
    let run_id = RunId::new();
    reduce(&mut s, system_ev(EventBody::RunStarted {
        run_id, objective: "add a test".to_owned(), mode: AgentMode::Build,
    }));
    let out = render_to_string(&s, 80, 14);
    assert!(out.contains("You"), "user role header:\n{out}");
    assert!(out.contains("  add a test"), "indented body (two-space gutter):\n{out}");
    assert!(!out.contains("› add a test"), "the old caret user line is gone:\n{out}");
}

#[test]
fn the_assistant_header_names_the_model_when_known() {
    let s = running_build_state(); // serves "gpt-5.1-codex"
    let out = render_to_string(&s, 110, 30);
    assert!(out.contains("⏺ codypendent · gpt-5.1-codex"), "model in the turn header:\n{out}");
}

#[test]
fn the_assistant_header_omits_the_model_when_unknown() {
    // A run with a tool cell but no agent-authored (model-bearing) event.
    let mut s = AppState::new();
    let run_id = RunId::new();
    reduce(&mut s, system_ev(EventBody::RunStarted {
        run_id, objective: "hi".to_owned(), mode: AgentMode::Build,
    }));
    reduce(&mut s, system_ev(EventBody::ToolStarted {
        run_id, tool: "shell.run".to_owned(), args_digest: "abc".to_owned(),
    }));
    let out = render_to_string(&s, 90, 20);
    assert!(out.contains("⏺ codypendent"), "bare header:\n{out}");
    assert!(!out.contains("codypendent ·"), "no ` · <model>` when unknown (honesty):\n{out}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p codypendent-tui the_assistant_header_names_the_model_when_known`
Expected: FAIL — the header is currently bare `⏺ codypendent`.

- [ ] **Step 3: Restyle the User arm and the assistant header**

In `entry_lines`, replace the `TranscriptEntry::User` arm (495-497) with:

```rust
        TranscriptEntry::User { text } => {
            out.push(Line::styled(
                "You",
                Style::default()
                    .fg(theme.focus.active)
                    .add_modifier(Modifier::BOLD),
            ));
            let mut wrote_body = false;
            for line in text.lines() {
                out.push(Line::styled(
                    format!("  {line}"),
                    Style::default().fg(theme.text.primary),
                ));
                wrote_body = true;
            }
            if !wrote_body {
                out.push(Line::styled("  ", Style::default().fg(theme.text.primary)));
            }
        }
```

In `for_each_row` (Task 1), replace the assistant-header emission (`visit(Row::built(Line::styled("⏺ codypendent", ...)));`) with a model-aware two-span line:

```rust
            } else if is_agent_cell && awaiting_header {
                let mut spans = vec![Span::styled(
                    "⏺ codypendent",
                    Style::default()
                        .fg(theme.agent.tool)
                        .add_modifier(Modifier::BOLD),
                )];
                if let Some(model) = &run.model {
                    spans.push(Span::styled(
                        format!(" · {model}"),
                        Style::default().fg(theme.text.muted),
                    ));
                }
                visit(Row::built(Line::from(spans)));
                produced = true;
                awaiting_header = false;
            }
```

- [ ] **Step 4: Update existing tests to the new turn shape**

- `a_user_turn_renders_with_a_caret_marker` (4121): delete it (superseded by `a_user_turn_renders_a_role_header_and_indented_body`).
- `a_blank_line_separates_turns_after_the_first` (3709): replace the `› alpha` / `› beta` lookups. New body:

```rust
    let out = render_to_string(&s, 80, 20);
    let rows: Vec<&str> = out.lines().collect();
    let alpha_body = rows.iter().position(|r| r.contains("  alpha")).expect("first turn body");
    let beta_header = rows.iter().skip(alpha_body).position(|r| r.trim() == "You")
        .map(|p| p + alpha_body).expect("second turn header");
    assert_eq!(beta_header, alpha_body + 2, "one blank row separates the turns:\n{out}");
```

- `the_conversation_renders_every_run_in_one_continuous_scroll` (3750): change `out.contains("› alpha")` → `out.contains("  alpha")` and `out.contains("› beta")` → `out.contains("  beta")`; keep the `alpha reply`/`beta reply`, `⏺ codypendent` count == 2, and `2 turns` asserts.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p codypendent-tui`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/tui/src/render.rs
git commit -m "feat(tui): role-differentiated turns — You / codypendent · <model> headers"
```

---

## Task 3: Backstage quiet-fold (verify) + error collapse/expand

The backstage fold already renders as one dim, expandable line (`backstage_lines`, unchanged). This task adds the concise-error line with the full raw chain on expand, and the `expanded` flag on `Completed`.

**Files:**
- Modify: `crates/tui/src/state.rs` — `TranscriptEntry::Completed` (281).
- Modify: `crates/tui/src/reduce.rs` — the `RunCompleted` fold (525); `expand_selected` (766-773).
- Modify: `crates/tui/src/render.rs` — the `Completed` arm of `entry_lines` (521-541); add `summarize_error`.
- Test: `render.rs` `mod tests`, `reduce.rs` `mod tests`.

**Interfaces:**
- Produces: `summarize_error(raw: &str) -> String`; `TranscriptEntry::Completed { disposition, expanded }`.
- Consumes: `expand_selected` toggling (used by keyboard `Enter` and, in Task 8, a click).

- [ ] **Step 1: Write the failing tests**

In `render.rs` `mod tests`:

```rust
#[test]
fn summarize_error_maps_known_chains_and_degrades_unknown() {
    assert_eq!(
        summarize_error("model driver error: model stream failed: service error: request failed: builder error"),
        "model error — the provider request failed"
    );
    assert_eq!(summarize_error("service error: request failed"), "provider request failed");
    // Unknown outermost segment degrades to that segment verbatim (never a crash).
    assert_eq!(summarize_error("no model configured"), "no model configured");
    assert_eq!(summarize_error(""), "run failed");
}

#[test]
fn a_failed_run_collapses_the_chain_and_expands_to_the_raw() {
    let mut s = AppState::new();
    let run_id = RunId::new();
    reduce(&mut s, system_ev(EventBody::RunStarted {
        run_id, objective: "hi".to_owned(), mode: AgentMode::Build,
    }));
    reduce(&mut s, system_ev(EventBody::RunCompleted {
        run_id,
        disposition: RunDisposition::Failed {
            reason: "model driver error: model stream failed: service error: request failed: builder error".to_owned(),
        },
        chronicle: filler_chronicle(),
    }));

    let collapsed = render_to_string(&s, 90, 20);
    assert!(collapsed.contains("✗ model error — the provider request failed"), "summary:\n{collapsed}");
    assert!(!collapsed.contains("builder error"), "raw chain hidden while collapsed:\n{collapsed}");

    // Select the Completed entry and expand it.
    s.focus = Pane::Transcript;
    let last = s.runs[0].transcript.len() - 1;
    s.runs[0].transcript_selected = last;
    reduce(&mut s, Action::Expand);

    let expanded = render_to_string(&s, 90, 20);
    assert!(expanded.contains("✗ model error — the provider request failed"), "summary kept:\n{expanded}");
    assert!(expanded.contains("builder error"), "raw chain revealed on expand:\n{expanded}");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p codypendent-tui summarize_error_maps_known_chains_and_degrades_unknown`
Expected: FAIL — `summarize_error` not defined.

- [ ] **Step 3: Add the `expanded` field to `Completed`**

In `state.rs`, change the variant (281):

```rust
    /// The run's terminal marker. `expanded` (client-only view state, mirroring
    /// the other fold flags) reveals the full raw failure chain beneath the
    /// concise summary; `disposition` is the only wire data.
    Completed { disposition: RunDisposition, expanded: bool },
```

In `reduce.rs`, the `RunCompleted` fold (525) — add `expanded: false`:

```rust
                    TranscriptEntry::Completed {
                        disposition: disposition.clone(),
                        expanded: false,
                    },
```

In `reduce.rs`, `expand_selected` (767-772) — add the toggle arm:

```rust
                TranscriptEntry::Tool(card) => card.expanded = !card.expanded,
                TranscriptEntry::Patch(patch) => patch.expanded = !patch.expanded,
                TranscriptEntry::Note { expanded, .. } => *expanded = !*expanded,
                TranscriptEntry::Backstage { expanded, .. } => *expanded = !*expanded,
                TranscriptEntry::Completed { expanded, .. } => *expanded = !*expanded,
                _ => {}
```

- [ ] **Step 4: Add `summarize_error` and rewrite the `Completed` render arm**

In `render.rs`, add:

```rust
/// Map a nested error chain (`": "`-joined segments) to a concise summary. Pure
/// heuristic: recognized outermost segments map to a friendly category; anything
/// else degrades to the outermost segment verbatim. The full raw chain is one
/// expand away, so no detail is lost.
fn summarize_error(raw: &str) -> String {
    let outer = raw.split(": ").next().unwrap_or("").trim();
    // Recognized categories, checked against any segment of the chain.
    for segment in raw.split(": ") {
        match segment.trim() {
            "model driver error" | "model stream failed" => {
                return "model error — the provider request failed".to_owned();
            }
            _ => {}
        }
    }
    for segment in raw.split(": ") {
        match segment.trim() {
            "service error" | "request failed" => return "provider request failed".to_owned(),
            _ => {}
        }
    }
    if outer.is_empty() {
        "run failed".to_owned()
    } else {
        outer.to_owned()
    }
}
```

Rewrite the `Completed` arm of `entry_lines` (521-541). The success and forward-compat cases are unchanged; the `Failed` case folds:

```rust
        TranscriptEntry::Completed { disposition, expanded } => match disposition {
            RunDisposition::Completed { .. } => {}
            RunDisposition::Failed { reason } => {
                let marker = if *expanded { "▾" } else { "▸" };
                out.push(head(
                    format!("{marker} ✗ {}", summarize_error(reason)),
                    theme.status.error,
                ));
                if *expanded {
                    out.push(Line::styled(
                        format!("    {reason}"),
                        Style::default().fg(theme.text.muted),
                    ));
                }
            }
            RunDisposition::Cancelled { reason } => {
                let text = reason
                    .as_ref()
                    .map_or_else(|| "✗ cancelled".to_owned(), |r| format!("✗ cancelled: {r}"));
                out.push(head(text, theme.text.muted));
            }
            _ => {
                out.push(head("✗ run ended".to_owned(), theme.text.muted));
            }
        },
```

- [ ] **Step 5: Verify existing failed/cancelled tests still pass**

`a_failed_run_shows_its_reason` (3536) asserts `out.contains("no model configured")` and `!out.contains("run failed:")` and `!out.contains("⏺ codypendent")` — all still hold (a single-segment reason summarizes to itself, shown collapsed as `▸ ✗ no model configured`; no agent cell ran). `a_cancelled_run_shows_its_reason_tersely` (3576) unchanged. No test constructs `TranscriptEntry::Completed` directly (grep-confirmed), so the field addition needs no test edits.

Run: `cargo test -p codypendent-tui`
Run: `cargo test -p codypendent-tui --lib reduce`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/tui/src/state.rs crates/tui/src/reduce.rs crates/tui/src/render.rs
git commit -m "feat(tui): concise one-line errors with the full raw chain on expand"
```

---

## Task 4: Command-palette restyle ("command centre")

Aligned columns, grouped commands, no `[—]`, stronger selected row.

**Files:**
- Modify: `crates/tui/src/palette.rs` — add `group` to `PaletteEntry` (58-71); reorder + tag `COMMANDS` (75-178) into contiguous groups.
- Modify: `crates/tui/src/render.rs` — rewrite `render_palette` (2365-2447).
- Test: `render.rs` `mod tests`; `palette.rs` `mod tests` (unchanged asserts, verify).

**Interfaces:**
- Produces: `PaletteEntry.group`.
- Consumes: `crate::palette::filtered` (unchanged: filtering is order-preserving over the reordered `COMMANDS`).

**Reconciliation note:** the group label ("render a dim label whenever the group changes") requires each group's commands to be contiguous, so `COMMANDS` is REORDERED into `Run → Models → Browse → Session` (matching the spec's mockup). `filtered()` preserves table order, so the selectable index math is unchanged; only the render order and the palette snapshot change.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn command_palette_aligns_columns_groups_and_drops_the_dash() {
    let mut state = running_build_state();
    reduce(&mut state, Action::OpenPalette);
    let all = render_to_string(&state, 120, 40);
    assert!(all.contains("Command palette"), "title:\n{all}");
    assert!(all.contains("New run"), "command:\n{all}");
    assert!(all.contains("Model picker"), "command:\n{all}");
    // Group labels appear on the empty query.
    assert!(all.contains("Run") && all.contains("Models"), "group labels:\n{all}");
    // The confusing unbound-key marker is gone.
    assert!(!all.contains("[—]"), "no [—] marker:\n{all}");
    // The header hint invites clicking.
    assert!(all.contains("click a row"), "click hint:\n{all}");

    // Filtering hides the group labels.
    for c in "docs".chars() { reduce(&mut state, Action::InputChar(c)); }
    let filtered = render_to_string(&state, 120, 40);
    assert!(filtered.contains("Docs Studio"), "match:\n{filtered}");
    assert!(!filtered.contains("New run"), "non-match filtered:\n{filtered}");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p codypendent-tui command_palette_aligns_columns_groups_and_drops_the_dash`
Expected: FAIL — no group labels / `click a row` hint yet.

- [ ] **Step 3: Add `group` and reorder `COMMANDS`**

In `palette.rs`, add the field to `PaletteEntry` (after `key`, 70):

```rust
    /// The command's group, for the palette's dim group-label rows.
    pub group: &'static str,
```

Reorder `COMMANDS` (75-178) so groups are contiguous, adding `group:` to each entry, in this order (keys/titles/descriptions/`command` unchanged from the current table; only order + the new field):

1. `group: "Run"` — `NewRun` (`n`), `Steer` (`s`), `PauseResume` (`p`), `Cancel` (`c`).
2. `group: "Models"` — `Model` (`—`), `Provider` (`—`).
3. `group: "Browse"` — `Docs` (`D`), `Edges` (`G`), `Workflow` (`W`), `Blackboard` (`B`), `Skills` (`S`), `Memory` (`M`).
4. `group: "Session"` — `ToggleLayout` (`F2`), `Help` (`?`), `Detach` (`q`), `NewConversation` (`—`).

- [ ] **Step 4: Rewrite `render_palette`**

Replace the row-building loop and header hint (2400-2446) — keep the outer block + filter line setup (2365-2408) except extend the hint text:

```rust
    frame.render_widget(
        Paragraph::new(vec![
            filter,
            Line::styled(
                "  ↑/↓ select · Enter run · Esc close · click a row",
                Style::default().fg(theme.text.muted),
            ),
        ])
        .style(Style::default().bg(theme.surface.overlay)),
        rows[0],
    );

    let matches = crate::palette::filtered(query);
    let inner_w = rows[1].width as usize;
    let title_w = 20usize;
    let key_w = 4usize;
    // marker(2) + title + space + description(fill) + key
    let desc_w = inner_w.saturating_sub(2 + title_w + 1 + key_w).max(1);
    let show_groups = query.trim().is_empty();

    let mut items: Vec<ListItem> = Vec::new();
    if matches.is_empty() {
        items.push(ListItem::new(Line::styled(
            "  no matching command",
            Style::default().fg(theme.text.muted),
        )));
    }
    let mut last_group: Option<&str> = None;
    for (idx, entry) in matches.iter().enumerate() {
        if show_groups && last_group != Some(entry.group) {
            items.push(ListItem::new(Line::styled(
                format!("  {}", entry.group),
                Style::default().fg(theme.text.muted),
            )));
            last_group = Some(entry.group);
        }
        let is_selected = idx == selected;
        let marker = if is_selected { "› " } else { "  " };
        let key = if entry.key == "—" {
            " ".repeat(key_w)
        } else {
            format!("{:>width$}", entry.key, width = key_w)
        };
        let head = Line::from(vec![
            Span::styled(marker, Style::default().fg(theme.focus.active)),
            Span::styled(format!("{:<width$}", entry.title, width = title_w), Style::default().fg(theme.text.primary)),
            Span::raw(" "),
            Span::styled(format!("{:<width$}", truncate(entry.description, desc_w), width = desc_w), Style::default().fg(theme.text.muted)),
            Span::styled(key, Style::default().fg(theme.status.info)),
        ]);
        let item = ListItem::new(head);
        items.push(if is_selected { item.style(theme.selection_style()) } else { item });
    }
    frame.render_widget(
        List::new(items).style(Style::default().bg(theme.surface.overlay)),
        rows[1],
    );
```

- [ ] **Step 5: Update the palette snapshot; verify palette unit tests**

Update `command_palette_snapshot_lists_and_filters_commands` (4358): replace the `all.contains("[n]")` assert with `assert!(!all.contains("[—]"), "no dash marker:\n{all}");` (the bracketed key form is gone). Keep the `New run` / `Docs Studio` / filter asserts. The `palette.rs` unit tests (`empty_query_matches_every_command`, the substring filters, `every_command_has_a_nonempty_title_and_key`) are order-independent and stay green.

Run: `cargo test -p codypendent-tui`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/tui/src/palette.rs crates/tui/src/render.rs
git commit -m "feat(tui): restyle the command palette — aligned columns, groups, no [—]"
```

---

## Task 5: Shortcuts footer bar

A persistent 1-row shortcut strip, derived from `KEY_BINDINGS`, below the status line.

**Files:**
- Modify: `crates/tui/src/input.rs` — add `FooterHint` + `footer_hints()`.
- Modify: `crates/tui/src/render.rs` — add the 4th root-layout row (43-59); add `render_shortcuts_bar`.
- Test: `input.rs` `mod tests`; `render.rs` `mod tests`.

**Interfaces:**
- Produces: `FooterHint`, `footer_hints()`, `render_shortcuts_bar`.
- Consumes: `KEY_BINDINGS`; the Actions each chip fires (Task 8 registers the chip rects).

- [ ] **Step 1: Write the failing tests**

In `input.rs` `mod tests`:

```rust
#[test]
fn footer_hints_are_all_backed_by_real_bindings() {
    for hint in footer_hints() {
        assert!(
            KEY_BINDINGS.iter().any(|b| b.keys == hint.binding.keys),
            "footer hint {:?} must derive from a real KEY_BINDINGS entry",
            hint.label
        );
        assert!(!hint.label.is_empty());
    }
}
```

In `render.rs` `mod tests`:

```rust
#[test]
fn the_shortcuts_footer_renders_derived_chips() {
    let state = running_build_state();
    let out = render_to_string(&state, 100, 30);
    // The persistent footer strip (its own row, below the status line).
    assert!(out.contains("send"), "send chip:\n{out}");
    assert!(out.contains("commands"), "commands chip:\n{out}");
    assert!(out.contains("layout"), "layout chip:\n{out}");
    assert!(out.contains("help"), "help chip:\n{out}");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p codypendent-tui footer_hints_are_all_backed_by_real_bindings`
Expected: FAIL — `footer_hints` not defined.

- [ ] **Step 3: Add `FooterHint` + `footer_hints()`**

In `input.rs`, after `KEY_BINDINGS` (100):

```rust
/// One footer chip: a compact display label paired with the real `KEY_BINDINGS`
/// entry it derives from (so it can never drift) and the `Action` a click fires.
#[derive(Debug, Clone)]
pub struct FooterHint {
    pub binding: &'static KeyBinding,
    pub label: &'static str,
    pub action: Action,
}

/// The curated, ordered footer strip. Each entry references a real binding by
/// index (the drift-guard test asserts each is present in `KEY_BINDINGS`).
static FOOTER_HINTS: &[FooterHint] = &[
    FooterHint { binding: &KEY_BINDINGS[1],  label: "⏎ send",     action: Action::InputSubmit },
    FooterHint { binding: &KEY_BINDINGS[2],  label: "/ commands", action: Action::OpenPalette },
    FooterHint { binding: &KEY_BINDINGS[3],  label: "↑↓ scroll",  action: Action::ScrollPageDown },
    FooterHint { binding: &KEY_BINDINGS[5],  label: "F2 layout",  action: Action::ToggleLayout },
    FooterHint { binding: &KEY_BINDINGS[9],  label: "? help",     action: Action::Help },
    FooterHint { binding: &KEY_BINDINGS[12], label: "q detach",   action: Action::Detach },
];

/// The persistent, derived shortcut strip (curated subset of `KEY_BINDINGS`).
#[must_use]
pub fn footer_hints() -> &'static [FooterHint] {
    FOOTER_HINTS
}
```

(Indices map to the current `KEY_BINDINGS`: `[1]`=Enter, `[2]`=`/`, `[3]`=PgUp/PgDn, `[5]`=F2, `[9]`=`?`, `[12]`=Ctrl-C. If `Action` is not `Sync` on this toolchain — it should be, being plain data — the executor makes `footer_hints()` build and return a `Vec<FooterHint>` instead; the drift test is unaffected.)

- [ ] **Step 4: Add the 4th layout row + `render_shortcuts_bar`**

In `render.rs` `render` (43-59), add the footer row and call:

```rust
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),                  // conversation transcript
            Constraint::Length(COMPOSER_HEIGHT), // persistent composer
            Constraint::Length(1),               // status footer
            Constraint::Length(1),               // shortcuts footer (derived)
        ])
        .split(area);

    match state.layout {
        LayoutMode::Chat => render_conversation(frame, rows[0], state, theme),
        LayoutMode::Workspace => render_workspace(frame, rows[0], state, theme),
    }
    render_composer(frame, rows[1], state, theme);
    render_status_line(frame, rows[2], state, theme);
    render_shortcuts_bar(frame, rows[3], state, theme);

    render_overlays(frame, area, state, theme);
```

Add (import `crate::input::footer_hints` at the top: `use crate::input::footer_hints;`):

```rust
/// The persistent, derived shortcut strip: `·`-separated chips built from
/// `input::footer_hints()`, so it never drifts from the real key bindings. Chips
/// drop right-to-left on narrow terminals.
fn render_shortcuts_bar(frame: &mut Frame, area: Rect, _state: &AppState, theme: &Theme) {
    let bg = Style::default().bg(theme.surface.overlay);
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    let mut used = 1usize;
    let width = area.width as usize;
    for (i, hint) in footer_hints().iter().enumerate() {
        let sep = if i > 0 { 3 } else { 0 }; // " · "
        let chip = Span::raw(hint.label).width(); // DISPLAY width (labels have ⏎/↑↓)
        if used + sep + chip + 1 > width {
            break; // drop the rest on a narrow terminal
        }
        if i > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(theme.text.muted)));
            used += 3;
        }
        // Split "glyph rest" so the key glyph reads as the accent.
        let (glyph, rest) = hint.label.split_once(' ').unwrap_or((hint.label, ""));
        spans.push(Span::styled(glyph.to_owned(), Style::default().fg(theme.focus.active)));
        if !rest.is_empty() {
            spans.push(Span::styled(format!(" {rest}"), Style::default().fg(theme.text.muted)));
        }
        used += chip;
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(bg), area);
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p codypendent-tui`
Expected: PASS. Existing layout tests (`conversation_shell_shows_transcript_composer_and_footer`, `workspace_layout_adds_runs_and_approvals_panes`) still pass — they use `contains` and heights ≥ 30.

- [ ] **Step 6: Commit**

```bash
git add crates/tui/src/input.rs crates/tui/src/render.rs
git commit -m "feat(tui): derived shortcuts footer bar below the status line"
```

---

## Task 6: Status-bar & header tidy (dedupe model)

Drop the model from the status line (it stays in the header chrome + each turn header); keep only the state-dependent right cue there (the static shortcuts now live in the footer).

**Files:**
- Modify: `crates/tui/src/render.rs` — `render_status_line` (865-874 model block; 918-933 hint).
- Test: `render.rs` `mod tests` (update two footer tests).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_status_line_drops_the_model_but_keeps_honest_placeholders() {
    let state = running_build_state();
    let out = render_to_string(&state, 120, 30);
    // The model is deduped out of the status line's ambient fields — but still
    // shown in the header chrome + the assistant turn header.
    let status_row = out.lines().rev().nth(1).unwrap_or(""); // status line is 2nd from bottom
    assert!(!status_row.contains("model"), "no `model` field on the status line:\n{status_row}");
    assert!(out.contains("gpt-5.1-codex"), "model still shown in chrome/turn header:\n{out}");
    // Honesty: unmeasured cost/wt still render `—` (running_build_state measures
    // neither), never a fabricated number.
    assert!(status_row.contains("—"), "unmeasured fields stay em-dash:\n{status_row}");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p codypendent-tui the_status_line_drops_the_model_but_keeps_honest_placeholders`
Expected: FAIL — the status line currently shows `model gpt-5.1-codex` at width 120.

- [ ] **Step 3: Remove the model field and reduce the hint**

In `render_status_line`, DELETE the `full` model block (865-874):

```rust
    if full {
        ambient.push(field(
            "model",
            status.model.as_ref().map_or("—".to_owned(), ToString::to_string),
            theme.text.secondary,
        ));
    }
```

Replace the `hint` computation (918-933) — the static common shortcuts now live in the footer; the status line keeps ONLY the state-dependent cue:

```rust
    let hint: Vec<Span> = if status.pending_approvals > 0 {
        vec![key("a"), word(" approve  "), key("A"), word(" run  "), key("r"), word(" reject")]
    } else if scrolled_up {
        vec![key("PgDn"), word(" ↧ latest")]
    } else {
        Vec::new()
    };
```

- [ ] **Step 4: Update the two footer-context tests**

- `contextual_footer_switches_hint_by_context` (4175): remove the `idle.contains("model")` assert (model is deduped). The `cmds`/`F2` and `send` cues now come from the shortcuts footer — repoint them:

```rust
    let idle = render_to_string(&state, 120, 30);
    assert!(idle.contains("mode"), "ambient fields:\n{idle}");
    assert!(idle.contains("commands") || idle.contains("F2"), "footer command chips:\n{idle}");
    // Drafting still shows the ambient state; the send affordance is in the footer.
    for c in "hello".chars() { reduce(&mut state, Action::InputChar(c)); }
    let drafting = render_to_string(&state, 120, 30);
    assert!(drafting.contains("send"), "send chip in the footer:\n{drafting}");
```

- `contextual_footer_narrows_by_dropping_low_priority_fields` (4198): still valid (`state` kept, `model` absent) — verify it passes unchanged.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p codypendent-tui`
Expected: PASS. `transcript_snapshot_shows_model_tool_and_status` still passes (model is in the header chrome + turn header).

- [ ] **Step 6: Commit**

```bash
git add crates/tui/src/render.rs
git commit -m "feat(tui): dedupe the model out of the status line; keep state cues only"
```

---

## Task 7: Hit-test map infrastructure + client Actions + click routing

The render→input geometry cache (mirroring `transcript_max_scroll`), the two client-only view Actions, their reducer arms, and left-click resolution.

**Files:**
- Modify: `crates/tui/src/state.rs` — `AppState.hit_map` field + `register_hit`; imports.
- Modify: `crates/tui/src/action.rs` — `ActivateRow(usize)`, `SelectRun(usize)`.
- Modify: `crates/tui/src/input.rs` — `hit_test`; `map_event`/`map_mouse` take `hit_map`; update every test call site.
- Modify: `crates/tui/src/reduce.rs` — `ActivateRow`/`SelectRun` arms + `activate_row`/`set_overlay_selected`.
- Modify: `crates/tui/src/render.rs` — `render` clears the map at the start.
- Modify: `crates/cli/src/tui.rs` — the `map_event` call site (591).
- Test: `input.rs` `mod tests`; `reduce.rs` `mod tests`.

**Interfaces:** see Shared Interfaces (Task 7 block).

**Design note (Open Question 3 resolved):** `ActivateRow(n)` means "activate row N of the active list surface." When a list overlay is open (`Palette`/`ModelPicker`/`ProviderPicker`/`AddModelPick`), it sets that overlay's `selected` (mirroring `nav`'s picker resolution) and runs the same `submit_prompt` the keyboard's `Enter` runs. When no overlay is open (`Overlay::None`, the base transcript), it focuses the transcript, points `transcript_selected` at entry N of the selected run, and toggles its fold — the exact effect `Enter` on that selected entry has. The render layer registers transcript fold-line hits only for the selected run and only when no overlay is open (Task 8), so the two meanings never collide.

- [ ] **Step 1: Write the failing tests**

In `input.rs` `mod tests`:

```rust
#[test]
fn hit_test_returns_the_topmost_registered_action() {
    use ratatui::layout::Rect;
    let base = Rect { x: 0, y: 0, width: 20, height: 10 };
    let overlay = Rect { x: 2, y: 2, width: 6, height: 3 };
    let map = vec![
        (base, Action::FocusPane(Pane::Transcript)),
        (overlay, Action::ActivateRow(1)), // later-registered = topmost
    ];
    // Inside the overlay: the topmost wins.
    assert_eq!(hit_test(&map, 3, 3), Some(Action::ActivateRow(1)));
    // Over the base only: the base wins.
    assert_eq!(hit_test(&map, 15, 8), Some(Action::FocusPane(Pane::Transcript)));
    // Outside everything: None.
    assert_eq!(hit_test(&map, 40, 40), None);
}

#[test]
fn a_left_click_over_a_registered_rect_resolves_to_its_action() {
    use ratatui::layout::Rect;
    let map = vec![(Rect { x: 0, y: 0, width: 10, height: 2 }, Action::SelectRun(2))];
    let click = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 3, row: 1, modifiers: KeyModifiers::NONE,
    });
    assert_eq!(map_event(&click, InputMode::Composer, W, &map), Action::SelectRun(2));
    // No registered rect under the click → NoOp.
    assert_eq!(map_event(&click, InputMode::Composer, W, &[]), Action::NoOp);
}
```

In `reduce.rs` `mod tests`:

```rust
#[test]
fn select_run_sets_the_selected_run_clamped() {
    let mut s = AppState::new();
    for obj in ["a", "b", "c"] {
        reduce(&mut s, system_ev(EventBody::RunStarted {
            run_id: RunId::new(), objective: obj.to_owned(), mode: AgentMode::Build,
        }));
    }
    reduce(&mut s, Action::SelectRun(1));
    assert_eq!(s.selected_run, 1);
    reduce(&mut s, Action::SelectRun(99)); // clamps to last
    assert_eq!(s.selected_run, 2);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p codypendent-tui hit_test_returns_the_topmost_registered_action`
Expected: FAIL — `hit_test` / `Action::ActivateRow` not defined.

- [ ] **Step 3: Add the `hit_map` field + `register_hit`**

In `state.rs`, extend imports (8, 15):

```rust
use std::cell::{Cell, RefCell};
use ratatui::layout::Rect;
// ... existing ...
use crate::action::{Action, Intent, SecretKey};
```

Add the field to `AppState` (after `transcript_max_scroll`, 940):

```rust
    /// A render→input geometry cache (mirrors `transcript_max_scroll`): every
    /// interactive widget registers its `Rect` + the `Action` a click fires here
    /// during render; the input layer resolves a left click to the topmost hit.
    /// A one-frame-fresh layout metric, cleared at the start of every render —
    /// never domain state. `RefCell` (not `Cell`) because the payload is a
    /// non-`Copy` `Vec`. Default/clone/eq are harmless: it defaults empty and is
    /// only populated during render, so reducer-only tests keep comparing equal.
    pub hit_map: RefCell<Vec<(Rect, Action)>>,
```

Initialize in `new()` (after `transcript_max_scroll: Cell::new(0),`, 1010):

```rust
            hit_map: RefCell::new(Vec::new()),
```

Add the helper (in `impl AppState`, near `drain_outbox`):

```rust
    /// Register an interactive rect → the Action a left click on it fires. Called
    /// by the renderer (interior mutability; the reducer never touches it).
    pub fn register_hit(&self, rect: Rect, action: Action) {
        self.hit_map.borrow_mut().push((rect, action));
    }
```

- [ ] **Step 4: Add the two Actions**

In `action.rs`, in the `Action` enum navigation section (after `FocusPane(Pane)`, 42):

```rust
    /// Activate row N of the active list surface (mouse click): the open overlay
    /// list, or — with no overlay — the transcript fold line at entry N of the
    /// selected run. Folds to the same effect the keyboard's selection + `Enter`
    /// produces. Client-only (no `Intent`, no wire).
    ActivateRow(usize),
    /// Select run N in the runs pane (mouse click). Client-only.
    SelectRun(usize),
```

- [ ] **Step 5: Add `hit_test` + thread `hit_map` through `map_event`/`map_mouse`**

In `input.rs`, import `Rect` (10) and add:

```rust
use ratatui::layout::Rect;

/// Resolve a left click at `(col,row)` to the topmost registered rect's Action.
/// Iterates in reverse so the last-registered (top-of-z-order) rect wins.
#[must_use]
pub fn hit_test(hit_map: &[(Rect, Action)], col: u16, row: u16) -> Option<Action> {
    hit_map
        .iter()
        .rev()
        .find(|(r, _)| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height)
        .map(|(_, action)| action.clone())
}
```

Change `map_event` (108-112) to take the map and pass it to `map_mouse`:

```rust
#[must_use]
pub fn map_event(event: &Event, mode: InputMode, width: u16, hit_map: &[(Rect, Action)]) -> Action {
    match event {
        Event::Key(key) => map_key(key, mode),
        Event::Mouse(mouse) => map_mouse(mouse, mode, width, hit_map),
        // ... paste/resize arms unchanged ...
```

Change `map_mouse` (270-290) signature and the two left-click arms:

```rust
fn map_mouse(mouse: &MouseEvent, mode: InputMode, _width: u16, hit_map: &[(Rect, Action)]) -> Action {
    match mode {
        InputMode::Editing | InputMode::Confirm => Action::NoOp,
        InputMode::Composer => match mouse.kind {
            MouseEventKind::ScrollUp => Action::ScrollPageUp,
            MouseEventKind::ScrollDown => Action::ScrollPageDown,
            MouseEventKind::Down(MouseButton::Left) => {
                hit_test(hit_map, mouse.column, mouse.row).unwrap_or(Action::NoOp)
            }
            _ => Action::NoOp,
        },
        InputMode::Normal | InputMode::Palette | InputMode::Approval => match mouse.kind {
            MouseEventKind::ScrollUp => Action::SelectPrev,
            MouseEventKind::ScrollDown => Action::SelectNext,
            MouseEventKind::Down(MouseButton::Left) => {
                hit_test(hit_map, mouse.column, mouse.row).unwrap_or(Action::NoOp)
            }
            _ => Action::NoOp,
        },
    }
}
```

Update EVERY `map_event(...)` call in `input.rs`'s `mod tests` to pass a fourth arg. The mechanical change: every existing call `map_event(&EV, MODE, W)` becomes `map_event(&EV, MODE, W, &[])` (there are ~30, in `normal_command_keys_map`, `palette_mode_filters_but_stays_navigable`, `tab_in_palette_mode_begins_add_model`, `editing_mode_routes_text_not_commands`, `confirm_mode_yes_no`, `key_releases_are_ignored`, `composer_mode_captures_text_and_controls`, `approval_mode_only_decision_keys`, and `every_mouse_gesture_has_a_keyboard_equivalent`). In `every_mouse_gesture_has_a_keyboard_equivalent`, the final `click` assertion (584-589) stays `Action::NoOp` with `&[]` (an empty map).

- [ ] **Step 6: Add the reducer arms**

In `reduce.rs` `reduce` (after `Action::FocusPane(pane) => state.focus = pane,`, 62):

```rust
        Action::ActivateRow(n) => activate_row(state, n),
        Action::SelectRun(n) => {
            let mut idx = n;
            clamp(&mut idx, state.runs.len());
            state.selected_run = idx;
        }
```

Add the helpers (near `cycle_run`):

```rust
/// Set the open list overlay's `selected` to `n`, mirroring `nav`'s picker
/// resolution (keeps `selected_model`/`selected_provider` pointed at the same
/// filtered card). A no-op for a non-list overlay.
fn set_overlay_selected(state: &mut AppState, n: usize) {
    match state.overlay {
        Overlay::Palette { ref mut selected, .. }
        | Overlay::AddModelPick { ref mut selected, .. } => *selected = n,
        Overlay::ModelPicker { ref query, ref mut selected } => {
            *selected = n;
            let indices = filter_models(&state.models, query);
            state.selected_model = indices.get(n).copied().unwrap_or(0);
        }
        Overlay::ProviderPicker { ref query, ref mut selected } => {
            *selected = n;
            let indices = filter_providers(&state.providers, query);
            state.selected_provider = indices.get(n).copied().unwrap_or(0);
        }
        _ => {}
    }
}

/// A click on row N: activate the open list overlay's row N (same effect as
/// selecting it + `Enter`), or — with no overlay — toggle the transcript fold
/// line at entry N of the selected run (same effect as `Enter` on that entry).
fn activate_row(state: &mut AppState, n: usize) {
    match state.overlay {
        Overlay::Palette { .. }
        | Overlay::ModelPicker { .. }
        | Overlay::ProviderPicker { .. }
        | Overlay::AddModelPick { .. } => {
            set_overlay_selected(state, n);
            submit_prompt(state);
        }
        Overlay::None => {
            state.focus = Pane::Transcript;
            let idx = state.selected_run;
            if let Some(run) = state.runs.get_mut(idx) {
                if n < run.transcript.len() {
                    run.transcript_selected = n;
                }
            }
            expand_selected(state);
        }
        _ => {}
    }
}
```

- [ ] **Step 7: Clear the map each frame + update the call site**

In `render.rs` `render` (right after `let area = frame.area();`, 33):

```rust
    state.hit_map.borrow_mut().clear();
```

In `crates/cli/src/tui.rs`, the call site (591) becomes:

```rust
                Some(event) => map_event(&event, state.input_mode(), *width, &state.hit_map.borrow()),
```

(Both `state.input_mode()` and `state.hit_map.borrow()` are shared borrows of `state`; the `Ref` deref-coerces to `&[(Rect, Action)]` and is dropped at the end of the statement, before the following `reduce`.)

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p codypendent-tui`
Run: `cargo build -p codypendent-cli`
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS / clean.

- [ ] **Step 9: Commit**

```bash
git add crates/tui/src/state.rs crates/tui/src/action.rs crates/tui/src/input.rs crates/tui/src/reduce.rs crates/tui/src/render.rs crates/cli/src/tui.rs
git commit -m "feat(tui): hit-test map + ActivateRow/SelectRun + left-click routing"
```

---

## Task 8: Register clickable surfaces + parity

Register each interactive surface's rect → its Action, add the modal scrim, and extend the mouse-parity invariant.

**Files:**
- Modify: `crates/tui/src/render.rs` — `render_palette` (+`state` param), `render_model_picker`, `render_provider_picker`, `render_add_model_pick` (+`state` param), `render_runs_pane`, `render_shortcuts_bar`, `render_overlays` (scrim), `render_workspace` panes, `render_composer`; `for_each_row` fold-head tagging + `render_conversation` fold-hit registration.
- Modify: `crates/tui/src/input.rs` — extend `KEY_BINDINGS` with click gestures; extend the parity test.
- Test: `render.rs` `mod tests`; `input.rs` `mod tests`.

**Interfaces:** consumes Task 7's `register_hit`/`ActivateRow`/`SelectRun`, Task 5's `footer_hints`, Task 1's `build_transcript_window` hits.

- [ ] **Step 1: Write the failing tests**

In `render.rs` `mod tests`:

```rust
#[test]
fn clicking_a_palette_row_registers_activate_row() {
    let mut state = running_build_state();
    reduce(&mut state, Action::OpenPalette);
    let _ = render_to_string(&state, 120, 40); // populates the hit map
    let map = state.hit_map.borrow();
    assert!(
        map.iter().any(|(_, a)| matches!(a, Action::ActivateRow(_))),
        "a palette row registered ActivateRow"
    );
    // A full-screen scrim closes the overlay on an outside click (registered first).
    assert!(map.iter().any(|(_, a)| matches!(a, Action::Dismiss)), "modal scrim");
}

#[test]
fn clicking_a_run_entry_registers_select_run() {
    let mut state = running_build_state();
    reduce(&mut state, Action::ToggleLayout); // workspace shows the runs pane
    let _ = render_to_string(&state, 120, 30);
    let map = state.hit_map.borrow();
    assert!(map.iter().any(|(_, a)| matches!(a, Action::SelectRun(0))), "run row → SelectRun");
}

#[test]
fn clicking_a_footer_chip_registers_its_action() {
    let state = running_build_state();
    let _ = render_to_string(&state, 120, 30);
    let map = state.hit_map.borrow();
    assert!(map.iter().any(|(_, a)| matches!(a, Action::OpenPalette)), "footer chip → its Action");
}
```

In `input.rs` `mod tests`, extend the parity test (see Step 6).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p codypendent-tui clicking_a_palette_row_registers_activate_row`
Expected: FAIL — nothing registers hits yet.

- [ ] **Step 3: Register overlay list rows + the modal scrim**

Add `state: &AppState` params: change `render_palette(frame, area, theme, query, selected)` → `render_palette(frame, area, state, theme, query, selected)` and its call in `render_overlays` (972-974); likewise `render_add_model_pick` → add `state` and update its call (1029). (`render_model_picker`/`render_provider_picker` already take `state`.)

In each list-row loop, after `items.push(...)`, register the row's rect. Rows render inside `rows[1]`/`cols[0]`; a list item occupies consecutive rows. Register a 1-row rect per selectable row at its offset. For `render_palette`, inside the `for (idx, entry)` loop track the rendered row offset and register:

```rust
    // after building `items`, register each selectable row's rect for clicks.
    let list_area = rows[1];
    let mut y = list_area.y;
    // Re-walk in the same order as `items` to map filtered index → screen row.
    let mut fi = 0usize;
    let mut last_group: Option<&str> = None;
    for entry in &matches {
        if show_groups && last_group != Some(entry.group) {
            y = y.saturating_add(1); // the (non-clickable) group label row
            last_group = Some(entry.group);
        }
        if y >= list_area.y + list_area.height { break; }
        state.register_hit(
            Rect { x: list_area.x, y, width: list_area.width, height: 1 },
            Action::ActivateRow(fi),
        );
        y = y.saturating_add(1);
        fi += 1;
    }
```

For `render_model_picker` / `render_provider_picker` / `render_add_model_pick`, each row spans a KNOWN number of lines (model: 3, provider: 4, add-model: 1). Register a rect of that height per `row` in `matches` at its offset in `cols[0]`/`rows[1]` → `Action::ActivateRow(row)`. Example (model picker, rows are 3 lines each in `cols[0]`):

```rust
    let list_area = cols[0];
    for (row, _) in matches.iter().enumerate() {
        let y = list_area.y + (row as u16) * 3;
        if y >= list_area.y + list_area.height { break; }
        state.register_hit(
            Rect { x: list_area.x, y, width: list_area.width, height: 3 },
            Action::ActivateRow(row),
        );
    }
```

In `render_overlays`, register a full-screen scrim → `Dismiss` FIRST for any non-`None` overlay (so it sits at the BOTTOM of the overlay z-order; the overlay's own rows, registered later, win inside the box). The approval modal (the `Overlay::None` + pending-approval branch) registers NO scrim:

```rust
fn render_overlays(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    if !matches!(state.overlay, Overlay::None) {
        state.register_hit(area, Action::Dismiss); // modal scrim (outside-click closes)
    }
    match &state.overlay {
        // ... existing arms, with render_palette/render_add_model_pick now taking `state` ...
```

- [ ] **Step 4: Register runs-pane rows, footer chips, workspace panes, composer**

`render_runs_pane` — after building each run's `ListItem`, register a 1-row rect. The list renders inside `block.inner(area)`; empty-state occupies row 0. Register per run:

```rust
    let inner = block.inner(area);
    frame.render_widget(List::new(items).block(block), area);
    let base = inner.y + if state.runs.is_empty() { 1 } else { 0 };
    for (idx, _) in state.runs.iter().enumerate() {
        let y = base + idx as u16;
        if y >= inner.y + inner.height { break; }
        state.register_hit(
            Rect { x: inner.x, y, width: inner.width, height: 1 },
            Action::SelectRun(idx),
        );
    }
```

`render_shortcuts_bar` — track each chip's start column and register its rect → `hint.action` (change the `_state` param to `state`):

```rust
    // inside the chip loop, before pushing the chip spans:
    let chip_start = used as u16 + area.x;
    // ... push spans, advance `used` ...
    let chip_end = used as u16 + area.x;
    state.register_hit(
        Rect { x: chip_start, y: area.y, width: chip_end.saturating_sub(chip_start), height: 1 },
        hint.action.clone(),
    );
```

`render_workspace` — register each of the three panes → `FocusPane`. NOTE: `Pane` is currently imported only in the test module, so add `Pane` to the production `use crate::state::{...}` list at the top of `render.rs` (24-28) first:

```rust
    render_runs_pane(frame, cols[0], state, theme);
    render_conversation(frame, cols[1], state, theme);
    render_context_pane(frame, cols[2], state, theme);
    state.register_hit(cols[0], Action::FocusPane(Pane::Sessions));
    state.register_hit(cols[1], Action::FocusPane(Pane::Transcript));
    state.register_hit(cols[2], Action::FocusPane(Pane::Approvals));
```

(The runs-pane row hits, registered earlier, win over the pane hit at the same point because they are registered first only within `render_runs_pane`; register the pane rects AFTER the row rects so a click on a run row still resolves to `SelectRun`. To guarantee row-over-pane precedence, register the runs-pane rows LAST: move the three `FocusPane` registrations to BEFORE `render_runs_pane`/`render_context_pane`, or register the run-row hits after. Simplest: register the three pane rects first, then the pane renderers register their finer row hits — reorder so `state.register_hit(cols[..], FocusPane(..))` runs BEFORE `render_runs_pane`.) Final order:

```rust
    state.register_hit(cols[0], Action::FocusPane(Pane::Sessions));
    state.register_hit(cols[1], Action::FocusPane(Pane::Transcript));
    state.register_hit(cols[2], Action::FocusPane(Pane::Approvals));
    render_runs_pane(frame, cols[0], state, theme);   // finer run-row hits win (later)
    render_conversation(frame, cols[1], state, theme);
    render_context_pane(frame, cols[2], state, theme);
```

`render_composer` — when a non-modal overlay is open, clicking the composer returns to typing (`Dismiss`); the full-screen scrim already delivers this, so this is belt-and-braces and harmless. Add at the end of `render_composer`:

```rust
    if !matches!(state.overlay, Overlay::None) {
        state.register_hit(area, Action::Dismiss);
    }
```

- [ ] **Step 5: Tag + register transcript fold-line hits**

In `for_each_row` (Task 1), replace the placeholder `let _ = (run_idx, selected_run, idx);` in the `other =>` arm with fold-head tagging (only the selected run; only foldable heads):

```rust
                other => {
                    scratch.clear();
                    entry_lines(other, theme, false, false, &mut scratch);
                    let hit = if run_idx == selected_run { fold_hit_entry(other, idx) } else { None };
                    for (j, line) in scratch.drain(..).enumerate() {
                        let mut row = Row::built(line);
                        if j == 0 { row.hit_entry = hit; }
                        visit(row);
                        produced = true;
                    }
                }
```

Add the helper:

```rust
/// The entry index if this entry renders a clickable fold HEAD (its first line):
/// a backstage summary, a folded (multi-line) note, or a failed-run summary.
fn fold_hit_entry(entry: &TranscriptEntry, idx: usize) -> Option<usize> {
    match entry {
        TranscriptEntry::Backstage { .. } => Some(idx),
        TranscriptEntry::Note { text, .. }
            if text.lines().count() > NOTE_INLINE_LINE_THRESHOLD => Some(idx),
        TranscriptEntry::Completed { disposition: RunDisposition::Failed { .. }, .. } => Some(idx),
        _ => None,
    }
}
```

In `render_conversation` (Task 1), replace `let (mut lines, r0, _hits) = ...` with `let (mut lines, r0, hits) = ...` and, after computing `top_pad` and before building the `Paragraph`, register each fold hit at its screen row (one of `top_pad`/`r0` is always 0, so the formula is exact for a single-row head):

```rust
    for (line_index, entry) in &hits {
        let screen_y = inner.y as i32 + top_pad as i32 + *line_index as i32 - r0 as i32;
        if screen_y >= inner.y as i32 && screen_y < (inner.y + inner.height) as i32 {
            state.register_hit(
                Rect { x: inner.x, y: screen_y as u16, width: inner.width, height: 1 },
                Action::ActivateRow(*entry),
            );
        }
    }
```

- [ ] **Step 6: Extend `KEY_BINDINGS` + the parity test**

In `input.rs`, append two documented click gestures to `KEY_BINDINGS` (before the closing `]`, after the Ctrl-C entry):

```rust
    KeyBinding {
        keys: "↑↓ + Enter",
        description: "activate a list row / transcript fold (same as clicking it)",
        mouse: Some("click a row"),
    },
    KeyBinding {
        keys: "Tab",
        description: "focus a pane (same as clicking it)",
        mouse: Some("click a pane"),
    },
```

(These are appended AFTER index 12, so `FOOTER_HINTS`' indices are unaffected.) Extend `every_mouse_gesture_has_a_keyboard_equivalent`: after the existing table + wheel assertions, add click resolution + keyboard-reachability:

```rust
        // (3) A left click resolves to the topmost registered rect's Action, and
        // each such Action is keyboard-reachable.
        use ratatui::layout::Rect;
        let map = vec![
            (Rect { x: 0, y: 0, width: 10, height: 1 }, Action::ActivateRow(0)),
        ];
        let click = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2, row: 0, modifiers: KeyModifiers::NONE,
        });
        assert_eq!(map_event(&click, InputMode::Palette, W, &map), Action::ActivateRow(0));
        // ActivateRow ≡ SelectNext×k then InputSubmit; SelectRun ≡ Prev/NextRun;
        // FocusPane ≡ Tab (CyclePane); Dismiss ≡ Esc — all in the keyboard table.
        assert_eq!(map_event(&key(KeyCode::Enter), InputMode::Palette, W, &[]), Action::InputSubmit);
        assert_eq!(map_event(&key(KeyCode::Tab), InputMode::Normal, W, &[]), Action::CyclePane);
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p codypendent-tui`
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS / clean. The model/provider/add-model/palette snapshot tests still pass (registration is additive; screen output unchanged).

- [ ] **Step 8: Commit**

```bash
git add crates/tui/src/render.rs crates/tui/src/input.rs
git commit -m "feat(tui): register clickable surfaces (palette/pickers/runs/footer/panes/folds) + parity"
```

---

## Final verification

- [ ] Run the whole suite + lints:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Expected: all green. No protocol/daemon/golden-vector file was touched (`git diff --name-only main` shows only `crates/tui/src/*` and `crates/cli/src/tui.rs`). The mouse-parity invariant, the honesty invariant (`—` placeholders), and theme-only styling hold.

---

## Plan self-review

- **Spec coverage:** Component 1 (bottom-anchor) + the added virtualization → Task 1; Component 2 (turn/role) → Task 2; Component 3 (backstage fold already present; error fold) → Task 3; Component 4 (palette) → Task 4; Component 5 (footer) → Task 5; Component 6 (status/header dedupe) → Task 6; Component 7 (hit-map + Actions + routing) → Task 7; Component 8 (registration + parity) → Task 8. Every "Testing" bullet in the spec maps to a task test above (bottom-anchor, virtualization O(viewport), turns/roles, backstage/error fold, palette layout, footer derivation, status dedupe honesty, hit-test/click routing, parity).
- **Signature consistency:** `line_rows(columns, inner_width)`, `for_each_row(runs, theme, selected_run, visit)`, `transcript_rows(runs, theme, inner_width)`, `build_transcript_window(runs, theme, inner_width, first_row, height, selected_run) -> (Vec<Line>, u16, Vec<(usize,usize)>)`, `hit_test(&[(Rect,Action)], u16, u16) -> Option<Action>`, `map_event(&Event, InputMode, u16, &[(Rect,Action)]) -> Action`, `register_hit(&self, Rect, Action)`, `summarize_error(&str) -> String`, `footer_hints() -> &'static [FooterHint]` — used identically across the tasks that consume them. `Action::ActivateRow(usize)` / `SelectRun(usize)` referenced consistently by input, reduce, and render.
- **Reconciliations vs. the spec:** (a) the spec's `wrapped_rows(&[Line])` refactor is superseded by `transcript_rows(runs, …)` — virtualization means we never hold all `Line`s, so the measure walks the run tree by borrowed width instead of a built `Vec<Line>`; `max_scroll_offset` is deleted, its `Cell` semantics preserved via `transcript_rows(...).saturating_sub(height)`. (b) `PaletteEntry` gains `group` AND `COMMANDS` is reordered into contiguous groups so a single label renders per group (the spec's mockup ordering). (c) `footer_hints()` returns `&'static` backed by unit-variant Actions; a `Vec` fallback is noted if `Action: !Sync`. (d) Open Question 3 is resolved by overloading `ActivateRow` (overlay row when an overlay is open; selected-run transcript fold when not) — within the two-new-Action budget, no new `Intent`.
- **Placeholder scan:** every code step shows complete code; the one deferred line (`let _ = (run_idx, selected_run, idx);` in Task 1) is explicitly replaced in Task 8. No "TBD"/"handle errors"/"similar to".
- **Top ambiguities flagged for the executor:** (1) transcript fold-line clicks are registered for the SELECTED run only (earlier runs' folds stay keyboard-reachable — scope discipline; the hit-map makes them trivial to add later). (2) fold-head hit rects assume a single-row head (true for backstage/note/error summaries); the ±1-row wrap tolerance the old `max_scroll_offset` already accepted applies. (3) list-row hit rects use fixed per-row heights (model 3, provider 4, add-model 1, palette 1) matching the current renderers — if a renderer's per-row line count changes, its registration height must change with it.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-07-27-tui-overhaul.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration (REQUIRED SUB-SKILL: superpowers:subagent-driven-development).

**2. Inline Execution** — execute tasks in this session using superpowers:executing-plans, batch execution with checkpoints.

**Which approach?**
