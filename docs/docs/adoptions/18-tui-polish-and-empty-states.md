# Adoption 18 — TUI Polish & Empty States

**Effort:** S · **Depends on:** 17 (modal/scrim; independent otherwise) · **Reference:** screenshot review + reference-repos where useful · **Ported from:** original + codex/cline conventions · **Status:** ⬜ not started

---

## 1. Summary

This is the S-tier polish batch of the v0.8 "Feels finished" release (master plan Track A,
items A3 + A4). It composes over spec 17's transcript/modal/scrim work but shares no files of
substance with it — spec 17 owns the modal component and the scrim; **this spec owns the
contents of the overlays and the persistent chrome**. Seven independent actions, numbered
8–14 to continue spec 17's numbering (spec 17 uses Actions 1–7):

- **Action 8 — Centered empty states + CTAs.** Docs Studio, Blackboard, Kanban, Journey and
  Remote-UI-plugins each render their "nothing here yet" copy as a single low-contrast line
  pinned to the top-left of the list column. Center it, enlarge the call-to-action, keep the
  existing copy. Also remove the stray boxed artifact in the lower-right of the empty Docs
  view (the orphaned editor/review-rail borders).
- **Action 9 — Kanban column backgrounds.** Give the four status columns a subtle tinted
  background so the board reads as a board even when every column is empty.
- **Action 10 — Hide empty `—` fields.** The persistent telemetry strip shows `cost —`,
  `branch/worktree —`, `reasoning —` even when there is nothing to show; hide those. The run
  detail pane's honest dashes (`tokens: —`, `cost: —`, `ctx: —`) are a **protected signal**
  (tests assert them) — do **not** hide those; instead render the bare dash in the muted
  token rather than a semantic colour. The `ctx ——` meter is left rendering (spec 20 / Action
  19 wires the data); here it only needs to render gracefully when unknown, which it already
  does.
- **Action 11 — Splash centering + version inset.** The version string is drawn *on* the
  panel's bottom border via `title_bottom`, colliding with the rule; inset it into the panel
  body. Lock the card's vertical centering with a snapshot.
- **Action 12 — Selection accent bar.** Replace the heavy flat selection bar with a left
  accent bar (the `focus.active` accent token) plus a subtle tinted background, in `theme.rs`
  and the shared row-marker convention, so it applies to every list / master-detail overlay.
- **Action 13 — Muted-tier contrast lift.** Lift the `text.muted` semantic token (dark,
  color-blind-safe, ansi256) so list subtitles, footer legends and empty-state hints are
  readable on the near-black background. One place: `theme.rs`.
- **Action 14 — List-width vs sparse-detail balance.** Master-detail overlays truncate list
  rows aggressively while the detail pane sits mostly empty. Derive row truncation from the
  real column width instead of magic constants, and widen the list column when the detail is
  sparse.

All changes are TUI-only (`crates/tui`). No `unsafe`. Semantic tokens only (clippy bans
`Color::Rgb`/`Color::Indexed` outside `theme.rs`); Action 13 changes token *definitions* in
`theme.rs`, every other action consumes tokens.

---

## 2. Reference

The vendored reference repos (git-ignored, under `reference-repos/`) are consulted where a
pattern helps; none is ported verbatim — the destination idioms already exist.

- **cline `status-bar.tsx`** — the model / context / cost strip only renders a segment when
  it has a value; empty segments are omitted, not dashed. This is the exact rule Action 10
  applies to `render_run_telemetry`'s optional items.
- **opencode empty states** — a centered card with a headline + one-line hint + a keycap CTA,
  rather than a log line. Action 8's centered layout follows this shape. opencode's synthesized
  theme (already ported as `Theme::system`) also informs Action 13: secondary/muted are blends
  toward the real foreground (0.45 / 0.70), i.e. deliberately lifted off the background — the
  built-in dark theme's muted was left too close to it.
- **codex selection** — a left accent column plus a faint row tint rather than a full-strength
  inverse bar; Action 12 mirrors this with the existing `focus.active` + `selection.background`
  tokens.
- **cline / codex truncation** — list labels are ellipsed to the measured column width, never a
  fixed character count; Action 14 replaces the magic `truncate(&x, 28)` constants with
  width-derived budgets, the same rule `picker_sub_line` already applies to sub-lines.

---

## 3. Current state (verified — exact render fns)

All line numbers are `crates/tui/src/render.rs` at the time of writing (v0.7.0, `main`).

**Empty states (Action 8).** A shared helper builds them:

- `empty_state_item(headline, hint, width, theme) -> ListItem` (line 10540) — two-space
  indent, headline in `text.secondary`, hint wrapped in `text.muted`. It returns a **list
  item**, so it is rendered as the first (and only) row of the left list column: top-left,
  not centered.
- Consumers: `render_docs` (line 6085), `render_blackboard` (7222), `render_journey` (5775),
  `render_ui_plugins` (4368). `render_kanban` (7014) does **not** use the helper — each empty
  column draws `"  —"` in `text.muted` (line 7075) and a footer sentence (7131).
- **Docs "stray thumbnail".** `render_docs` always splits the inner area into `34% / 66%`
  columns (line 6072) and the right column into editor (60%) over review (40%) rails
  (6129). The rails draw `Borders::LEFT` (editor, 6135) and `Borders::LEFT | Borders::TOP`
  (review, 6221) blocks **unconditionally**. When `state.docs.is_empty()`, `focused_doc()`
  is `None`, so those bordered rails render as empty L-shaped frames — the lower-right one is
  the "stray tiny thumbnail" boxed artifact the screenshot flags.

**Kanban columns (Action 9).** `render_kanban` (7014). Columns are laid out by
`kanban_columns()` into equal `Constraint::Ratio` lanes (7042). Each lane is a
`Block::default().borders(Borders::LEFT)` with `.style(bg(theme.surface.overlay))` (7059–7068)
— i.e. every column shares the same overlay background, so an empty board reads as four bare
vertical rules. `kanban_column_color(status, theme)` (6998) already maps
todo→info / doing→running / review→warning / done→success for the **column title**; nothing
tints the column **body**.

**Persistent chrome (Action 10).** `render_footer` (296) stacks `render_status_line` (3113,
the transient action/notice/chip row) over `render_run_telemetry` (336, the durable strip).
The dashes live in `render_run_telemetry`:

- `ctx` meter: `context_meter(percent)` (308) returns `("", "──────")` when `percent` is
  `None`; the strip prints `ctx ──────  —` (446). This is intentionally kept (spec 20 wires
  the data) and asserted by `the persistent strip should disclose unknown context` (≈13832).
- Optional items built at 480–545: `cost` uses `usage_label(...)`/`format_cost(...)` and reads
  a bare dash when unmeasured (498–499); `branch/worktree {workspace}` where
  `workspace = status.worktree.unwrap_or("—")` (389, 513); `reasoning —` / `r:—` is a
  **hard-coded placeholder** (534–538) — reasoning is never wired.
- These optional items are appended only if they fit (546–555), but when they fit they render
  the dash.

**Protected dashes (Action 10, do NOT hide).** `render_context_pane`'s Run detail (1090–1144)
prints `ctx`, `tokens`, `cost`, `wt` fields, each `—` when unmeasured, coloured with
`status.info` / `status.warning`. Two tests pin this:

- `an_unmeasured_run_still_reads_as_unmeasured` (17224) asserts the Run detail **contains**
  `tokens: —` and `cost: —`.
- `header_footer_and_run_detail_all_carry_the_measurement` (17195) asserts a *measured* run
  does **not** read `cost: —`.

**Splash (Action 11).** `render_splash` (743). The card rect is already vertically centered:
`card.y = area.y + area.height.saturating_sub(card_height) / 2` (870), `card_height =
lines.len() + 2` (865). The concrete defect is the version: `.title_bottom(" v{BUILD_ID} ")`
(884) draws it **on the bottom border rule**. `splash_shows_wordmark_tagline_version_and_stage`
(11862) asserts the version text is present at 100×30; `splash_drops_tagline_and_version_on_short_terminals`
(11907) only asserts the *tagline* drops at 6 rows (it does not assert the version drops).

**Selection (Action 12).** `Theme::selection_style()` (theme.rs 714) =
`fg(selection.foreground).bg(selection.background).add_modifier(BOLD)` — a full-width flat bar.
`selection.background` is `0x30294a` (dark), `Color::Indexed(60)` (ansi256), `Color::Gray`
(ansi16 — the literal "heavy flat grey bar"). It is applied at **40 call sites** via
`item.style(theme.selection_style())`. Most list rows already prepend a caret marker
`if selected { "› " } else { "  " }` styled with `selection_aware_text_style(selected,
theme.focus.active)` (e.g. docs 6095, kanban 7080, blackboard 7238, ui_plugins 4386). Tests
`everyday_selection_is_tonal_not_the_focus_accent` and `normal_muted_and_selection_text_meet_wcag_aa...`
(theme.rs 1034, 950) constrain the tokens.

**Muted tier (Action 13).** `text.muted` = `0x858d9d` (dark), `0x868e9d`
(color_blind_safe), `Color::Indexed(248)` (ansi256). The WCAG test
`normal_muted_and_selection_text_meet_wcag_aa_in_every_builtin_theme` (theme.rs 950) asserts
muted ≥ 4.5 contrast against **panel** and **overlay** only — not against `surface.background`
(the near-black `0x0b0d12`), which is where the footer strip and in-transcript empty states
actually draw. Dark muted-on-panel is ≈4.9 (barely AA); muted-on-background is lower — the
screenshot's "low-contrast grey".

**Master-detail split (Action 14).** Fixed percentage splits and fixed truncation constants:
`render_docs` 34/66 + `truncate(&doc.title, 28)` (6102); `render_blackboard` 44/56 +
`truncate(&card.run, 36)` (7240); `render_journey` 42/58 + `truncate(&card.statement, 34)`
(5789); `render_ui_plugins` 38/62 + `truncate(&plugin.id, 24)` (4390); `render_council_results`
32/68 (9384); `render_runs_pane` `truncate(&run.objective, 18)` (1021, the "Run failed: ACP
prompt fa…" case). None derive the budget from the actual column width, and none widen the list
when `focused_*()` is `None` (detail empty).

**Test harness.** Tests live in the `#[cfg(test)] mod tests` of `render.rs`. `render_to_string(state, w, h)` (10953) → `buffer_text` (10919) returns plain text (no styling) for
`.contains` assertions. `render_buffer(state, w, h, theme)` (10958) returns the ratatui
`Buffer` — cell styles (`buf[(x,y)].fg` / `.bg` / `.symbol()`) are inspectable for colour
assertions. `render_splash_to_string` (11845) is the splash equivalent. The vt100 backend
(`vt100_backend.rs`) is available for full ANSI snapshotting; the existing render tests use the
`TestBackend` + `buffer_text`/`render_buffer` idiom, which this spec follows.

---

## 4. Design (per Action)

### Action 8 — Centered empty states + CTAs

Add one shared helper that renders a **centered** empty-state card into an arbitrary rectangle,
and switch each empty overlay to call it *instead of* laying out its list/detail columns.

```
fn render_empty_state(
    frame, area, headline: &str, hint: &str, cta: &str, theme,
)
```

- Compute a centered sub-rect (reuse `centered_rect_min` or a small inline calc): width
  `min(area.width, 60)`, height enough for headline + blank + wrapped hint + blank + CTA.
- Draw, centered (`Alignment::Center`):
  - headline in `text.secondary` **bold**,
  - the existing hint in the lifted `text.muted` (Action 13),
  - a blank line,
  - the **CTA** as a keycap span (`" n "` reverse-video style: `fg(surface.background).bg(focus.active).bold`) + label in `text.primary` — the same keycap treatment `render_splash`'s ENTER uses (837–851).
- Keep the existing copy verbatim: Docs "No collaborative documents yet" / "Press n to create
  one, or ask an agent to draft it from this session."; Blackboard, Journey, UI-plugins copy as
  today; CTA text is the affordance already named in each footer (`n new`, `n post`, etc.).

Each consumer gets an early branch: when its collection is empty, render the centered card into
the **inner area** and return, *before* building any list/detail columns. That is what removes
the Docs "stray thumbnail": the editor/review rails are never drawn on an empty doc set. Kanban
keeps its columns (Action 9) but replaces the per-column `"  —"` + footer sentence with the
centered card drawn across `rows[0]` when `state.kanban.is_empty()`.

### Action 9 — Kanban column backgrounds

In `render_kanban`, tint each lane body with a per-status wash derived from the existing
`kanban_column_color`. There is no `Color` arithmetic available outside `theme.rs` (clippy),
so the wash is `surface.overlay` for the fill plus a **top accent rule** already coloured by
`kanban_column_color`, and the column body background alternates between `surface.overlay` and
`surface.panel` so adjacent columns are visually separated even when empty:

- Give the lane `Block` a full background of `surface.panel` (instead of `surface.overlay`),
  and paint a one-row header band in the column colour behind the ` status (n) ` title so each
  column reads as a labelled tray. Neighbouring columns already differ from the modal body
  (`surface.overlay`) because the lanes now use `surface.panel`.
- The empty column no longer prints `"  —"`; the tinted tray + title is enough. (When the whole
  board is empty, Action 8's centered card is shown instead — see above.)

### Action 10 — Hide empty `—` fields

`render_run_telemetry` only:

- Build the optional items into a `Vec`, then **filter out** any whose value is empty/dash
  *before* the fit loop:
  - `cost`: skip when `usage_label(...)` is `None` **and** `format_cost(status.cost_minor)`
    is the empty/dash sentinel — i.e. skip when there is no measured usage and no cost.
  - `branch/worktree`: skip when `status.worktree` is `None` (don't render the `—`).
  - `reasoning`: **remove entirely** — it is a hard-coded placeholder with no data source
    until spec 20. (Re-added by spec 20/Action 19 when reasoning is wired.)
- Leave the `ctx` meter untouched: it renders `ctx ──────  —` when unknown and is asserted by
  the disclosure test; spec 20 populates it.

Run detail pane (`render_context_pane`): **keep** the fields (tests require the dash), but when
a field's value is the bare `—`, colour it `text.muted` instead of `status.info` /
`status.warning`. Implement by having the `field(...)` closure (or its callers) pass
`text.muted` when the value is `"—"`. This satisfies "make the dash far more muted" while
keeping `tokens: —` / `cost: —` present.

### Action 11 — Splash centering + version inset

In `render_splash`:

- Drop `.title_bottom(...)` (884). Instead append the version as the **last content line**,
  inside the panel: a right-aligned muted line ` v{BUILD_ID}`. Because the content `Paragraph`
  is centre-aligned, render the version as its own line pushed into `lines` (so it participates
  in `card_height`), styled `text.muted`. Guard it behind `area.height >= 8` so short terminals
  drop it (keeping `splash_drops_tagline_and_version_on_short_terminals` honest).
- Vertical centering already holds via the existing rect math; add a snapshot asserting the
  blank rows above and below the card differ by ≤1 (locks it against regression). No rect change
  is required — see Gotchas / §9 for the finding.

### Action 12 — Selection accent bar

Two coordinated changes:

1. **`theme.rs` (one place):** change `selection_style()` to drop the blanket `BOLD` (bold is
   applied per-child by `selection_aware_text_style`, which every selected row already uses) and
   keep `fg(selection.foreground).bg(selection.background)`. Retune `selection.background` to a
   clearly-subtle tint where a true tint exists (dark/256/color-blind-safe); ansi16/monochrome
   keep their block (no subtle surface — the accent bar carries the signal there). Add a small
   accessor `pub fn selection_accent(&self) -> Color { self.focus.active }` for the bar glyph.
2. **`render.rs` shared marker:** add `fn selection_marker(selected: bool) -> &'static str
   { if selected { "▌ " } else { "  " } }` and use it at the row builders that currently inline
   `if selected { "› " } else { "  " }`, styled `selection_aware_text_style(selected,
   theme.selection_accent())`. The caret `›` becomes a left accent bar `▌` in the accent token;
   with the subtler background the row reads as "accent bar + faint tint" rather than a flat
   slab. This is mechanical across the master-detail overlays (docs, blackboard, kanban,
   journey, ui_plugins, memory, skills, council); the 40 `selection_style()` sites are otherwise
   unchanged.

### Action 13 — Muted-tier contrast lift

`theme.rs` token change only. Lift `text.muted` in the true-color variants that render on a
near-black background:

- dark: `0x85_8d_9d` → `0x9b_a3_b5`
- color_blind_safe: `0x86_8e_9d` → `0x9b_a3_b3`
- ansi256: `Color::Indexed(248)` → `Color::Indexed(249)` (still below `secondary` = 250, so
  the tier stays distinct)

light / high_contrast / ansi16 / monochrome are unchanged (light muted is already dark-on-white;
ansi16/monochrome muted is `DarkGray`, constrained by the grayscale test). Extend the WCAG test
to also assert muted ≥ 4.5 against `surface.background` (not just panel/overlay) so the near-black
case is guaranteed going forward.

### Action 14 — List-width vs sparse-detail balance

Two rules, applied to the master-detail overlays:

1. **Width-derived truncation.** Replace fixed `truncate(&x, N)` on list labels with
   `truncate_display_width(&x, budget)` where `budget = usize::from(col.width).saturating_sub(indent)`.
   This is the rule `picker_sub_line` (10289) already uses; the labels stop cutting at 28/34/36
   characters on wide terminals.
2. **Sparse-detail rebalance.** Add
   `fn master_detail_split(area, detail_populated: bool) -> (Rect /*list*/, Rect /*detail*/)`
   that returns a wider list column (≈55/45) when `!detail_populated` and the current ratio
   (≈40/60) when the detail has content. `detail_populated` is `state.focused_doc().is_some()`
   etc. Apply to `render_docs`, `render_blackboard`, `render_journey`, `render_ui_plugins`,
   `render_council_results`. The runs pane (`render_runs_pane`, `truncate(&run.objective, 18)`)
   just gets width-derived truncation.

---

## 5. Changes file-by-file

### `crates/tui/src/theme.rs`

**Action 13 — token lift (literal):**

```rust
// in `dark()`
text: TextTokens {
    primary: Color::Rgb(0xe8, 0xec, 0xf4),
    secondary: Color::Rgb(0xb2, 0xb9, 0xc8),
    muted: Color::Rgb(0x9b, 0xa3, 0xb5), // was 0x85_8d_9d — lifted for near-black bg (A13)
    heading: Color::Rgb(0xf8, 0xfa, 0xfc),
},

// in `color_blind_safe()`
muted: Color::Rgb(0x9b, 0xa3, 0xb3), // was 0x86_8e_9d (A13)

// in `ansi256()`
muted: Color::Indexed(249), // was 248 (A13); secondary stays 250
```

**Action 12 — selection style + accent accessor:**

```rust
#[must_use]
pub fn selection_style(&self) -> Style {
    // Subtle tint; the accent bar (selection_accent + selection_marker) carries the
    // "this row is selected" signal. Per-child BOLD is applied by
    // selection_aware_text_style, so it is not re-applied to the whole row here.
    Style::default()
        .fg(self.selection.foreground)
        .bg(self.selection.background)
}

/// The left accent-bar colour for a selected row (reuses the focus accent).
#[must_use]
pub fn selection_accent(&self) -> Color {
    self.focus.active
}
```

Retune `selection.background` where a true tint exists (values illustrative — must keep
`normal_muted_and_selection_text_meet_wcag_aa...` and `everyday_selection_is_tonal...` green):
dark `0x30294a` → a slightly lighter blue-tint `0x24_2c_44`; ansi256 `Indexed(60)` retained;
ansi16/monochrome retained (block fallback). Re-run the theme test module after tuning.

### `crates/tui/src/render.rs`

**Action 8 — shared centered empty state + accent-bar marker (new helpers):**

```rust
/// A centered empty-state card: headline, one-line hint, and a keycap CTA.
/// Drawn into `area` (usually an overlay's inner rect) instead of a top-left list row.
fn render_empty_state(
    frame: &mut Frame,
    area: Rect,
    headline: &str,
    hint: &str,
    cta_key: &str,   // e.g. "n"
    cta_label: &str, // e.g. "create a document"
    theme: &Theme,
) {
    let mut lines = vec![
        Line::styled(
            headline.to_owned(),
            Style::default().fg(theme.text.secondary).add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
    ];
    for row in wrap_display_width(hint, usize::from(area.width).min(56)) {
        lines.push(Line::styled(row, Style::default().fg(theme.text.muted)));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {cta_key} "),
            Style::default()
                .fg(theme.surface.background)
                .bg(theme.focus.active)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {cta_label}"),
            Style::default().fg(theme.text.primary).add_modifier(Modifier::BOLD),
        ),
    ]));
    let card = centered_rect_min(60, 40, 24, lines.len() as u16, area);
    frame.render_widget(Clear, card);
    frame.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center).wrap(Wrap { trim: false }),
        card,
    );
}

/// The selected-row left accent bar (Action 12). Replaces the inline
/// `if selected { "› " } else { "  " }` caret across the master-detail overlays.
fn selection_marker(selected: bool) -> &'static str {
    if selected { "▌ " } else { "  " }
}
```

**Action 8 — consumer edits (skeleton, Docs shown; Blackboard/Journey/UI-plugins/Kanban
follow the same shape):**

```rust
fn render_docs(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let rect = centered_rect(86, 86, area);
    shield_modal(state, rect);
    frame.render_widget(Clear, rect);
    let outer = /* unchanged block */;
    let inner = outer.inner(rect);
    frame.render_widget(outer, rect);

    // A8: empty → one centered card; do NOT draw the list/editor/review rails
    // (that is what leaves the stray lower-right boxed artifact).
    if state.docs.is_empty() {
        render_empty_state(
            frame, inner,
            "No collaborative documents yet",
            "Press n to create one, or ask an agent to draft it from this session.",
            "n", "create a document",
            theme,
        );
        return;
    }

    // A14: widen the list when nothing is focused.
    let (list_area, detail_col) = master_detail_split(inner, state.focused_doc().is_some());
    // ... existing list build, using selection_marker(selected) and
    //     truncate_display_width(&doc.title, list_area.width.saturating_sub(4) as usize) ...
}
```

`render_kanban`: add, right after computing `rows`:

```rust
if state.kanban.is_empty() {
    render_empty_state(
        frame, rows[0],
        "No Kanban tasks yet",
        "Press n to create a task, or let a workflow post one.",
        "n", "create a task",
        theme,
    );
    // still draw the footer row (rows[1]) for the create hit-target
} else {
    // existing lane layout, with A9 tinted lane blocks
}
```

**Action 9 — lane block (inside the non-empty Kanban branch):**

```rust
let block = Block::default()
    .borders(Borders::LEFT)
    .border_style(Style::default().fg(theme.focus.inactive))
    .title(Span::styled(
        format!(" {status} ({}) ", cards.len()),
        Style::default().fg(column_color).add_modifier(Modifier::BOLD),
    ))
    .style(Style::default().bg(theme.surface.panel)); // A9: panel tray, distinct from overlay body
```

**Action 10 — telemetry filter (inside `render_run_telemetry`, replacing the `optional` array +
fit loop):**

```rust
let mut optional: Vec<TelemetryItem> = Vec::new();
optional.push(/* via {provider} */);
if let Some(usage) = usage_label(status.prompt_tokens, status.completion_tokens, status.cost_micros) {
    optional.push(TelemetryItem { text: if verbose { format!("usage {usage}") } else { usage }, color: theme.status.warning });
} else if let Some(cost) = status.cost_minor {
    optional.push(TelemetryItem { text: if verbose { format!("cost {}", format_cost(Some(cost))) } else { format_cost(Some(cost)) }, color: theme.status.warning });
} // else: no cost segment at all (A10) — no `cost —`
optional.push(/* permissions {permission} */);
if let Some(wt) = status.worktree.as_deref() {
    optional.push(TelemetryItem { text: if verbose { format!("branch/worktree {}", truncate_display_width(wt, 18)) } else { format!("wt:{}", truncate_display_width(wt, 10)) }, color: theme.text.secondary });
} // else: no worktree segment (A10) — no `branch/worktree —`
optional.push(/* health */);
// `reasoning —` removed entirely (A10); re-added by spec 20 when reasoning is wired.
optional.push(TelemetryItem { text: "Shift-drag copy".to_owned(), color: theme.text.muted });
```

**Action 10 — run-detail muted dash (inside `render_context_pane`):**

```rust
let field = |k: &str, v: String, color: Color| -> Line {
    let value_color = if v == "—" { theme.text.muted } else { color };
    Line::from(vec![
        Span::styled(format!("  {k}: "), Style::default().fg(theme.text.muted)),
        Span::styled(v, Style::default().fg(value_color)),
    ])
};
```

**Action 11 — splash version inset (inside `render_splash`):** delete `.title_bottom(...)` on
the block; before computing `card_height`, push the version into `lines`:

```rust
if area.height >= 8 {
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        format!("v{BUILD_ID}"),
        Style::default().fg(theme.text.muted),
    ));
}
```

**Action 14 — split + truncation helpers:**

```rust
fn master_detail_split(area: Rect, detail_populated: bool) -> (Rect, Rect) {
    let (list_pct, detail_pct) = if detail_populated { (40, 60) } else { (55, 45) };
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(list_pct), Constraint::Percentage(detail_pct)])
        .split(area);
    (cols[0], cols[1])
}
```

and replace fixed `truncate(&x, N)` on list labels with
`truncate_display_width(&x, usize::from(col.width).saturating_sub(indent))`.

---

## 6. Protocol & persistence

**None.** No protocol messages, no wire vectors, no migrations, no persisted state. Every action
is a pure render-layer or theme-token change. (The `ctx` meter's *data* — the one field this
batch deliberately leaves empty — is wired by spec 20 / Action 19 and carries the protocol
change; nothing here touches `crates/protocol`.)

---

## 7. Acceptance criteria

Each criterion is a `render.rs`/`theme.rs` test (text via `render_to_string`/`render_splash_to_string`,
colour via `render_buffer` cell inspection, tokens via direct `Theme` assertions).

- **A8.1** With `state.docs` empty, `render_to_string(&state_workspace_docs_open, 120, 40)`
  contains "No collaborative documents yet" **and** the headline row's column index is centered
  (leading blanks ≈ trailing blanks, ±2). *(centered empty state)*
- **A8.2** The empty Docs view contains **no** editor/review rail border glyph in the lower
  right — assert the bottom-right quadrant of `render_buffer` holds only spaces/card. *(stray
  thumbnail removed)*
- **A8.3** Blackboard, Journey, UI-plugins and Kanban empty views each render their headline
  centered with a keycap CTA (`n`) present.
- **A9.1** With an empty board, adjacent Kanban lanes differ in background token
  (`surface.panel` lane vs `surface.overlay` modal body) — assert via `render_buffer` cell `.bg`.
- **A10.1** A run with no measured cost, no worktree: the last strip line contains neither
  `cost —` / `cost:` nor `branch/worktree —` nor `reasoning`. *(hidden)*
- **A10.2** `header_footer_and_run_detail_all_carry_the_measurement` still green (a *measured*
  run shows `$0.0034`).
- **A10.3** `an_unmeasured_run_still_reads_as_unmeasured` still green (Run detail keeps
  `tokens: —` and `cost: —`), and the dash cell's `.fg` equals `theme.text.muted`. *(muted dash)*
- **A10.4** `the persistent strip should disclose unknown context` still green (`ctx ──────  —`
  untouched).
- **A11.1** `splash_shows_wordmark_tagline_version_and_stage` still contains `v{BUILD_ID}`, and
  the version cell row is **inside** the card border (not on the bottom rule) — assert the row
  above the card's bottom border carries the version glyphs.
- **A11.2** New: blank rows above and below the splash card differ by ≤1 at 100×30. *(centering)*
- **A12.1** A selected list row's leftmost content cell `.fg == theme.focus.active` and its
  `.symbol()` is the bar glyph; the row background is `theme.selection.background` (not a
  full-strength `focus.active` bar). `everyday_selection_is_tonal...` still green.
- **A13.1** Extended WCAG test: `text.muted` ≥ 4.5 contrast against `surface.panel`,
  `surface.overlay` **and** `surface.background` for dark / color_blind_safe / ansi256.
- **A13.2** `normal_muted_and_selection_text_meet_wcag_aa_in_every_builtin_theme` and
  `monochrome_is_purely_grayscale` still green.
- **A14.1** With a long doc title on a wide terminal (160 cols), the list row shows more than 28
  glyphs of the title (width-derived, not the old fixed cut).
- **A14.2** With no doc focused, the Docs list column is wider than with a doc focused (assert
  the border column x-position moves right).

---

## 8. Tests

Add to `render.rs`'s `mod tests` (and `theme.rs`'s), one focused test per criterion above,
named descriptively in snake_case to match the module (e.g.
`empty_docs_view_centers_its_cta_and_drops_the_rail_artifact`,
`telemetry_strip_hides_empty_cost_worktree_and_reasoning`,
`run_detail_keeps_the_dash_but_paints_it_muted`,
`splash_version_is_inset_inside_the_panel_body`,
`selected_row_shows_a_left_accent_bar_over_a_subtle_tint`,
`muted_text_is_legible_on_the_near_black_background`,
`sparse_detail_widens_the_master_list`). Reuse `render_to_string` / `render_buffer` /
`render_splash_to_string` and the existing `measured_run_state` / `header_state` fixtures. Colour
assertions read `render_buffer(...)[(x, y)].fg` / `.bg`. Keep every currently-green test green
except the one documented churn in §9 (the wide-strip fixture must stop asserting `reasoning —`).

- **doc-count / ROADMAP markers.** Adding tests drifts the crate's test-count marker; run
  `check_docs_manifest.py --fix` and update `docs/MANIFEST.json` + any test-count marker as part
  of the change (the `codypendent-doc-gates` note). This spec adds no wire vectors, so the
  extension/protocol-vector partition is untouched.

---

## 9. Gotchas

- **`competitive_session_strip_prioritizes_and_expands_across_widths` must change.** At 240 cols
  it asserts the strip contains `"reasoning —"` (render.rs ≈13654). Action 10 removes that
  placeholder, so this assertion **must be updated** — drop `"reasoning —"` from the expected
  field list and instead assert its **absence**. This is the one intentional break to the
  "keep existing tests green" rule; it is required by the action and is not incidental churn.
- **Run-detail dashes are protected.** Do not "helpfully" hide `tokens: —` / `cost: —` in
  `render_context_pane` — `an_unmeasured_run_still_reads_as_unmeasured` guards them. Action 10's
  treatment there is *colour only* (muted), never omission.
- **Splash "floats low" is largely a misread.** The card rect is already vertically centered
  (§3); the real defect is the version-on-border. Do not add a second centering offset — that
  would push the card off-center. A11.2 locks the existing centering.
- **`ColorDepth` fallbacks for contrast (A12/A13).** ansi16/monochrome have no subtle tint:
  `selection.background` stays a block there and the accent bar is the differentiator; `text.muted`
  stays `DarkGray` (grayscale test). Only tune the true-color / 256 variants. After any token
  tune, re-run the whole `theme.rs` test module — it enforces AA contrast, semantic-pair
  distinctness, grayscale purity, and tonal-vs-accent selection across **all seven** variants.
- **Selection-marker churn (A12).** Switching the inline caret to `selection_marker` touches
  many row builders. Do them mechanically and leave the 40 `selection_style()` sites otherwise
  untouched; a row that has no caret today (a few pickers) can keep `selection_style()` alone —
  the subtler tint still improves it.
- **Snapshot churn across overlays.** Actions 8/12/13/14 shift text positions and colours in
  many overlays. These tests are `.contains`/cell-probe, not full-screen `insta` snapshots, so
  churn is contained; if a full-frame snapshot is added, expect to re-accept it once per action.
- **`render_empty_state` clears its card.** Use `Clear` before the paragraph so a previous
  frame's list rows can't ghost through (the same reason `modal_surface` double-clears).

---

## 10. Out of scope

- **The modal component, the scrim, transcript redesign, tool/diff cards** — spec 17 (Actions
  1–7). This spec does not define or modify the modal wrapper or the background-dimming scrim;
  it only fills overlay interiors and persistent chrome. General modal bleed-through is the
  scrim's job (spec 17), not Action 8's.
- **Wiring the `/context` meter data** (`ctx ──────`) — spec 20 / Action 19 (`crates/protocol`
  + reducer). Here the empty meter only needs to render gracefully, which it already does.
- **Reasoning telemetry** — no data source exists; Action 10 removes the placeholder and spec 20
  re-adds the field when reasoning is wired.
- **New theme variants or a full palette redesign** — Action 13 lifts one tier
  (`text.muted`) in three variants; it does not restructure the token set.
- **Non-TUI surfaces** (CLI, extensions, Tauri) — untouched.
