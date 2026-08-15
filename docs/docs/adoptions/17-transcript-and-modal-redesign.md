# Adoption 17 — Transcript & Modal Redesign

**Effort:** M · **Depends on:** nothing (v0.8 lead) · **Reference:** `reference-repos/codex/codex-rs/tui/src/{markdown_render.rs,diff_render.rs,exec_cell/,history_cell/}`, `reference-repos/opencode/packages/tui/src/ui/dialog.tsx`, `reference-repos/cline/apps/cli/src/tui/components/{chat-entry.tsx,tool-output.tsx}`
**Ported from:** codex+opencode+cline · **Status:** ⬜ not started

---

## 1. Summary

This is the flagship spec of v0.8 "Feels finished" (master plan §v0.8, Track A items A1+A2). A
screenshot review of the shipped ratatui TUI found two structural gaps:

1. The chat **transcript** reads as a flat wall of monochrome text — turns run together, tool
   calls and diffs are dim prose rather than structured cards, and the markdown heading ladder is
   nearly indistinguishable.
2. The **overlay/modal** system is inconsistent — the command palette / model picker float as
   small centred cards while Skills / Kanban / Memory / Docs are near-fullscreen, each built with
   its own geometry; the composer label clips (`ESSAGE` / `MESSAG` / `AGE · En`) under the tall
   overlays; and a dimmed strip of transcript shows above the "fullscreen" surfaces.

**What the code already does (verified — see §3), and how this spec is scoped around it:**

- Markdown is **not** flat text: `crates/tui/src/markdown.rs` already parses to semantic
  `RichLine`/`SpanRole`, and `render.rs::style_for` already styles headings (bold+underlined),
  bold, italic, inline code, lists, blockquotes, tables and highlighted code fences. The gap is
  narrow: **H1 and H2 render identically** and there is no per-level ladder (Action 2 refines
  `style_for`, it does not build markdown from scratch).
- A **scrim already exists**: `render_overlays` dims the whole base with `Modifier::DIM` for every
  overlay, and `modal_scrim_dims_the_base_and_the_interior_shields_click_through` locks it. Action 6
  is therefore mostly *verification*; the real defect the review saw ("transcript competes") is the
  Action 7 layering bug, not a missing scrim.
- A **modal primitive already exists**: `centered_modal` + `modal_surface` + `modal_panel` +
  `modal_rows` + `render_modal_search`. The pickers use it; the list/detail overlays (Skills,
  Kanban, …) bypass it with per-overlay `centered_rect(84,84)` / `centered_rect(90,86)`. Action 5
  routes the stragglers through the existing primitive — it does not invent a new one.
- **Tool cards already exist** (`tool_card_lines`) with a collapsed head + expandable body, and
  **diff cards already exist** (`patch_lines`) with `+N −N` and foreground diff colours. Actions 3
  and 4 enrich these (bounded head+tail output body; add/remove background tints), they do not
  replace them.
- `RunView.mode: AgentMode` already exists, so the plan-mode accent (Action 1) is fully
  client-derivable — **no protocol field is needed** for it.

The one genuine protocol gap is flagged in §6: a bounded tool **output preview** for bash/read
cards (Action 3). Everything else is TUI-only, pure-reducer + vt100-snapshot tested.

---

## 2. Reference implementations

### codex (`codex-rs/tui/src/`)
- **`markdown_render.rs`** — `MarkdownStyles::default()` is the heading ladder we port:
  `h1 = bold+underlined`, `h2 = bold`, `h3 = bold+italic`, `h4/h5/h6 = italic`; `code = cyan`,
  `emphasis = italic`, `strong = bold`, `ordered_list_marker = light_blue`,
  `unordered_list_marker = default`, `link = cyan+underlined`, `blockquote = green`. Codex keeps the
  literal `#` markers in the heading text; we deliberately do **not** (§4 Action 2).
- **`exec_cell/render.rs` + `model.rs`** — the tool-call command card. Status bullet is
  `•` green (exit 0) / red (exit≠0) / animated spinner (running). Header = `bullet + verb.bold() +
  command` on one line; continuation command lines under a dim `  │ ` bar (max 2); output under a
  dim `  └ ` elbow. **Output is head+tail truncated** to `TOOL_CALL_MAX_LINES = 5`
  (`USER_SHELL_TOOL_CALL_MAX_LINES = 50`) with a middle `… +N lines` ellipsis, and it wraps
  **before** truncating so a few long lines can't flood the viewport.
- **`diff_render.rs`** — `calculate_add_remove_from_diff` folds `diffy::Patch` hunk lines to
  `(added, removed)`; `render_line_count_summary` prints `(+N -M)` with `+N` green / `-M` red.
  Add/Delete line **backgrounds** come from theme-probed palettes
  (`DARK_TC_ADD_LINE_BG_RGB = #213A2B`, `DARK_TC_DEL_LINE_BG_RGB = #4A221D`,
  `LIGHT_TC_ADD_LINE_BG_RGB = #dafbe1`, `LIGHT_TC_DEL_LINE_BG_RGB = #ffebe9`, plus 256-index
  fallbacks; **Ansi16 gets no background tint**). A full-width `line_bg` reaches the terminal edge;
  the gutter number gets an opaque saturated bg on light themes; hunk breaks render a dim `⋮` (there
  is no textual `@@` header).
- **`history_cell/{messages,session}.rs`** — role identity is **prefix + dimming**, not accent
  bars: user body prefixed `› ` (bold+dim), assistant first line `• ` (dim), reasoning
  dim+italic. The one "mode-stamped" cell is the session header, which stamps `YOLO mode`
  magenta+bold when the policy is unrestricted — the pattern we copy for the plan-mode accent.

### opencode (`packages/tui/src/ui/dialog.tsx`)
One-slot "current modal" (`replace`, `onClose` chaining) gated by a pushed `"modal"` input mode.
Width presets **60 / 88 / 116** columns clamped to `term_width - 2`; horizontally centred; top =
`term_height / 4`; height = content. Scrim = full-screen `rgba(0,0,0,150/255)` (~59%) with an
**opaque** panel. Esc / Ctrl+C close; click-outside closes unless a text selection was dragging;
focus is captured on open and restored on close if the widget still exists.

### cline (`apps/cli/src/tui/components/`)
- **`chat-entry.tsx`** — one `accent` colour prop encodes plan-vs-act (`theme.accents.plan` vs
  `.act`), the marker glyph encodes role/state (`❯` user, `*` assistant, spinner while streaming);
  user rows get a filled background band bled to the edge (`marginX=-1`). Tool header =
  `toolName(params).accent.bold` where `formatToolParams` is a per-tool switch
  (`read_files → shortenPath`, `run_commands → cmd && cmd`, `editor → path`, …).
- **`tool-output.tsx`** — bespoke per-tool card body (`isBashTool`/`isReadTool`/`isEditTool`),
  `MAX_COLLAPSED_LINES = 5`, `⎿` leader, default-collapsed for bash/generic, default-expanded for
  edit diffs; `DiffStats` prints `+A` (success) `-R` (error) ` lines`.

---

## 3. Current state in codypendent (verified)

All line numbers are from the files as read for this spec.

**Overlay routing.** `crates/tui/src/state.rs:216` — `enum Overlay` has ~55 variants (the "~25" in
the brief is an undercount; every overlay routes through this one enum). `render.rs:3354`
`render_overlays` is the single dispatch: it applies the scrim then `match &state.overlay { … }`.

**Scrim (Action 6 — already present).** `render.rs:3355-3372`:
```rust
let has_modal = !matches!(state.overlay, Overlay::None) || state.show_approval_modal();
if has_modal {
    frame.buffer_mut().set_style(area, Style::default().add_modifier(Modifier::DIM));
}
if !matches!(state.overlay, Overlay::None) {
    state.register_hit(area, Action::Dismiss); // click-outside dismiss, registered first (bottom of z)
}
```
Locked by `modal_scrim_dims_the_base_and_the_interior_shields_click_through` (`render.rs:11631`) and
`approval_preemption_owns_the_scrim` (`render.rs:13034`). The dim intensity is the terminal's `DIM`
SGR, **not** a computed 40% blend (see §9).

**The modal primitive (Action 5 — partially present).** `render.rs:10212` `centered_modal(area,
w, h)` caps a card to `w × h` (min `area-4 × area-2`), centred. `render.rs:10226` `modal_surface`
paints the shared depth: a one-cell shadow filled with `surface.background`, then `Clear` + an
explicit full-rectangle space-fill in `surface.overlay` (the belt-and-braces that defeats
ghost-text), then a rounded `Borders::ALL` block titled in `text.heading` bold, and returns the
inner rect. `modal_rows` (`10323`) splits a search/list/hint interior list-first. **Pickers use
this** (Palette `render.rs:3509`, ModelPicker `4682`, ModePicker/ThemePicker/etc.).

**The stragglers (Action 5 + 7 — the bug).** The list/detail overlays do **not** use
`modal_surface`; each computes its own percentage rect and builds its own outer block:
- `render_skills` (`4490`): `centered_rect(84, 84, area)` + own `Borders::ALL` block.
- `render_kanban` (`7014`): `centered_rect(90, 86, area)` + own block.
- `render_memory` (`5859`), `render_docs` (`6048`), `render_journey` (`5746`),
  `render_workflow`, `render_blackboard`, `render_edges`, `render_ui_plugins`,
  `render_council_browser` (`9216`): same pattern, each its own percentages.
- Prompts/confirms (`render_prompt` `8250`, `render_masked_prompt` `8292`, `render_confirm_box`
  `5644`): `centered_rect_min(70,20,48,7,area)` + own block.

`centered_rect(84,84)` on a 30-row terminal yields height ≈ 25, `y ≈ 2`, `bottom ≈ 27`. The base
composer is drawn at `rows[4]` (`render.rs:137`) near row 27-28 and the status at `rows[5]`. So the
tall overlays' rect + one-cell shadow **overlap and overpaint the composer's top-border title**,
clipping ` MESSAGE · Enter sends ` to `ESSAGE` / `AGE · En` depending on each overlay's exact
height. Above the overlay, the ~2-row top margin shows the **dimmed** transcript — the "strip of
chat above Skills/Kanban" the review flagged. Both are the same root cause: inconsistent per-overlay
geometry over a still-painted base.

**Composer (Action 7).** `render_composer` (`render.rs:2236`) draws the label as a **Block title**
on `Borders::TOP` (`2244-2249`): ` MESSAGE · Enter sends ` / ` STEER · Enter queues `. At full
width, correct (locked by `conversation_shell_shows_transcript_composer_and_footer`,
`render.rs:13669`). It clips only when a taller overlay overpaints it — see above.

**Top-level compose order (Action 7).** `render.rs:45` `render(frame, state, theme)`: paints the
background, lays out `[header, transcript, composer-accessory, pending-prompts, composer, status]`,
draws each, then `render_remote_overlays` and `render_overlays` **last** over the full `area`. The
composer is base content under the scrim; overlays paint on top.

**Transcript role identity (Action 1 — partially present).** `render.rs:1551` `for_each_row` walks
every run and every `TranscriptEntry`:
- **User** turns: a blank separator before each non-first user turn (`1591-1596`), a `You` header
  carrying a right-aligned clock (`entry_lines_with_run` `render.rs:2561`), and either a full-width
  `surface.user` background (`row.bg`, `1685`) or — where `surface.user == surface.panel`
  (ansi16/monochrome) — a leading `▎` accent bar in `focus.active` (`1660-1667`).
- **Assistant** turns: a `⏺ codypendent · <model>` header, bold in `agent.tool` (`1602-1615`), then
  model text. Finalized rich lines borrow the `RichLine` cache (`Row::rich`, `1622-1626`); the
  streaming tail renders plain with a `▌ `/`  ` gutter (`1636-1640`).
- **No mode stamp**: the header and rail colours are identical for a Plan run and a Build run,
  though `RunView.mode` (`state.rs:1046`) already distinguishes them.
- **No continuous assistant rail**: the `▌ ` gutter is muted (`SpanRole::Gutter → text.muted`,
  `style_for` `1492`) and only leads the first model line; there is no per-turn accent bar the way
  the user container has one.

**Markdown styling (Action 2 — mostly present).** `style_for` (`render.rs:1489`):
```rust
SpanRole::Heading(1..=2) => base.fg(theme.text.heading).add_modifier(BOLD | UNDERLINED),
SpanRole::Heading(_)     => base.fg(theme.text.heading).add_modifier(BOLD),
SpanRole::Strong         => base.fg(theme.text.primary).add_modifier(BOLD),
SpanRole::Emphasis       => base.fg(theme.agent.model_text).add_modifier(ITALIC),
SpanRole::InlineCode     => base.fg(theme.syntax.string),
SpanRole::ListMarker     => base.fg(theme.agent.tool),
SpanRole::BlockQuote     => base.fg(theme.text.secondary).add_modifier(ITALIC),
// … CodeToken(_) → theme.syntax.*
```
`markdown.rs` already emits `Heading(1..=6)`, `Strong`, `Emphasis`, `StrongEmphasis`, `InlineCode`,
`ListMarker` (`•`/`N.`), `BlockQuote` (`▏ ` bar), `Rule`, `Table*`, `CodeToken`. So the **only**
Action-2 gap is: H1==H2, no H3/H4-6 differentiation, and heading colour does not step down the
hierarchy. Ordered vs unordered list markers share one colour (`agent.tool`).

**Tool cards (Action 3 — present, thin body).** `tool_card_lines` (`render.rs:2810`): collapsed
head `{▸|▾} ⏺ {tool} · {label} {✓|✗|running|⟳ review}` (status/outcome drive the mark, `2815-2861`);
expanded shows `describe_action` details, `args-digest`, a sanitised failure `error:`, and an
`output: {media_type} ({bytes})` line (`2863-2898`). `ToolCard` (`state.rs:635`) carries `tool`,
`status`, `action`, `args_digest`, `label`, `outcome`, `artifact: Option<ArtifactRef>`,
`approval_id`, `expanded`. **There is no output *text*** — only the artifact's `media_type` +
`byte_length`. `ToolCard.expanded` is a real fold (walked by `Alt-↑/↓`, click via `fold_hit_entry`
`1827`, `is_foldable` `state.rs:751`).

**Diff cards (Action 4 — present, no bg tints).** `patch_lines` (`render.rs:2901`): head
`{▸|▾} ◆ {target}  +A −B  changes ready`; expanded prints the file list then `patch.preview` lines
coloured **foreground-only** by prefix (`+`→`diff.added`, `-`→`diff.removed`, `@@`→`diff.header`,
else `diff.context`, `2938-2951`). `PatchSummary` (`state.rs:665`) has `additions`, `deletions`,
`preview`, `preview_truncated`, `expanded`. `DiffTokens` (`theme.rs:79`) is
`{added, removed, context, header}` — **no background tokens**.

**Theme.** `theme.rs` is the ONLY module allowed `Color::Rgb`/`Indexed` (`#![allow(clippy::
disallowed_methods)]`, `theme.rs:10`); adoption 12/A4 clippy bans them everywhere else. Eight
variants (`dark/light/high_contrast/color_blind_safe/ansi256/ansi16/monochrome/system`), each a
full literal palette, plus WCAG-contrast tests (`theme.rs:950+`) that every new token must pass.

**vt100 test backend.** `crates/tui/src/vt100_backend.rs` — `VT100Backend::new(w,h)` renders real
ANSI through `vt100::Parser`; `Display` yields `screen().contents()` for `insta::assert_snapshot!`.
The existing helper `render_buffer(&state, w, h, &theme) -> Buffer` (used across `render.rs` tests)
gives per-cell `Style` for modifier/colour assertions.

---

## 4. Design

Guiding rule: **build on the shipped modules, restyle rather than rewrite.** Every colour is a
`theme.*` token; every new token lands in `theme.rs` and passes the contrast tests.

### Action 1 — Per-turn separation + role identity + mode stamp

Keep the shipped structure (user container, `⏺ codypendent` header, blank separators). Add:

1. **A continuous assistant rail.** Give every assistant *row* (header + model lines + tool/patch
   cards belonging to the turn) a one-cell left rail in the turn's **mode accent**, mirroring the
   user container's `▎` bar. Implemented in `for_each_row` by tagging each assistant `Row` with a
   `rail: Option<Color>` and painting it in `build_transcript_window` next to the existing `bg`
   fill, so it is part of measured geometry (the rail occupies no *extra* column — it replaces the
   first cell of the existing `▌ `/`  ` gutter, recoloured; width is unchanged, so
   `measure_transcript` stays exact).
2. **Mode stamping.** A pure `mode_accent(mode, theme) -> Color` maps `AgentMode` onto existing
   semantic tokens (no new palette entries): `Plan → theme.status.info`, `Build → theme.agent.tool`,
   `Explore → theme.status.success`, `Ask → theme.text.secondary`, `Review → theme.status.warning`,
   `_ → theme.agent.tool`. The `⏺ codypendent` header's glyph and the rail take this colour; the
   header also gains a terse mode tag `· plan` (dim) when `mode != Build`, echoing codex's `YOLO
   mode` stamp and cline's plan/act accent. The default Build turn is visually unchanged.
3. **Separators unchanged.** The blank-line rules already read well; do not add horizontal rules
   between ordinary turns (codex/cline both avoid them — spacing + the accent are the delimiter).

### Action 2 — Markdown heading hierarchy

Refine `style_for` only. Port codex's ladder onto `SpanRole::Heading(n)` and split list markers:
```
Heading(1) => heading fg, BOLD | UNDERLINED
Heading(2) => heading fg, BOLD
Heading(3) => heading fg, BOLD | ITALIC
Heading(_) => secondary fg, ITALIC          // H4-6 recede
```
`markdown.rs` already carries the level in `Heading(u8)`, so no parser change. Unordered vs ordered
list markers are already one `SpanRole::ListMarker`; to match codex (`ordered = light_blue`,
`unordered = default`) without a parser change, keep `ListMarker → agent.tool` (it already reads as
a marker); this is optional polish, not required by acceptance. Do **not** re-introduce the literal
`#`/`##` markers — codypendent strips them at parse time (`markdown.rs` `start_heading`), and the
underline/weight ladder carries the hierarchy without them; that is a deliberate divergence from
codex (see §9).

### Action 3 — Tool-call cards

Enrich `tool_card_lines`; do not restructure the fold. Two changes:
1. **Header already is `toolName(params)`** in spirit (`⏺ {tool} · {label}`). Keep it; ensure the
   `label` path is the cline `formatToolParams` shape (it already is: the daemon's `tool_label`
   yields `services/main.py` / `cargo test`). No change required beyond confirming the collapsed
   head reads as one line.
2. **Bounded output body (expanded).** When a card is expanded and an output preview is available
   (see §6 protocol flag), render it under a dim `  └ ` elbow, **head+tail truncated to 5 on-screen
   lines** with a middle `    … +N lines` ellipsis — the codex `TOOL_CALL_MAX_LINES` rule. Wrap to
   the pane before counting so long lines can't flood. Until the protocol field lands, the existing
   `output: {media_type} ({bytes})` line stays as the fallback (no regression). Collapsed state is
   unchanged (one line), matching the shipped default-collapsed behaviour.

### Action 4 — Diff cards

Enrich `patch_lines`; keep `+A −B` and the fold. Two changes:
1. **Add/remove backgrounds.** Introduce `DiffTokens.added_bg` / `removed_bg` (new tokens, §5) and
   paint added/removed preview lines with a full-width background tint (the whole row, reaching the
   pane edge) plus the existing foreground `diff.added`/`diff.removed`. `context`/`@@`/`diff --git`
   lines keep foreground-only styling. On ansi16/monochrome, `added_bg == removed_bg == panel`
   (i.e. *no tint*) — codex's rule that low-colour terminals fall back to foreground cues only, so
   the WCAG tests don't have to certify a coloured fill that the terminal can't separate.
2. **Diffstat colour split.** Render the head's `+A` in `status.success` and `−B` in `status.error`
   (cline `DiffStats`), instead of both muted, so the stat reads at a glance.

Syntax highlighting *within* the diff (codex's per-hunk syntect pass) is **out of scope** for this
adoption (see §10) — the preview is already daemon-truncated and the background tint is the
high-value change.

### Action 5 — Unified modal component

There is already a primitive; the work is to make **all** overlays use it and to give it named
sizes so geometry is identical across overlays of the same class.

Add a `ModalSize` and a `modal(area, size) -> Rect` that supersedes the ad-hoc `centered_rect(84,
84)` calls, sitting on top of the shipped `centered_modal`:
```
ModalSize::Small   -> a compact confirm/prompt card   (opencode "medium" 60)
ModalSize::Medium  -> a picker card                    (opencode "large" 88)
ModalSize::Large   -> a list/detail surface            (opencode "xlarge" 116), tall
```
Every list/detail overlay (`render_skills`/`render_kanban`/`render_memory`/`render_docs`/
`render_journey`/`render_workflow`/`render_blackboard`/`render_edges`/`render_ui_plugins`/
`render_council_browser`) switches from its bespoke `centered_rect(..)` + hand-rolled outer block to
`let inner = modal_surface(frame, modal(area, ModalSize::Large), title, state, theme);` and renders
its list/detail *inside* `inner`. Prompts/confirms move to `ModalSize::Small`; pickers to
`ModalSize::Medium`. Because `modal_surface` already `Clear`s + opaquely fills its rect, adopting it
everywhere makes each overlay opaque and identically positioned — the "individually built" feeling
disappears. Vertical placement: keep true centring (`centered_modal` already centres); opencode's
`height/4` top offset is not ported (centre reads better in a terminal shell and matches the shipped
pickers).

### Action 6 — Scrim / dim behind overlays

The DIM scrim is shipped and correct. The Action-6 deliverable is to **lock it for the list/detail
overlays too** (they currently render outside the tested set) and to keep the mechanism uniform:
after Action 5, every overlay is drawn by `modal_surface` over the same globally-dimmed base, so a
single mechanism covers all of them. Keep `Modifier::DIM` as the dimming mechanism (it works on all
eight depths and is already tested); do not switch to a computed 40% blend (§9). Add snapshot
coverage asserting the base is dimmed and each `Large` overlay's interior is crisp.

### Action 7 — Fix layering bugs

Both symptoms share the root cause diagnosed in §3: tall per-overlay rects overpaint the base
composer, and their top margin shows dimmed transcript. Action 5 fixes the *geometry* (one `Large`
rect, opaque). The remaining, explicit fixes:

1. **Composer label never clips.** Stop relying on a `Block` title (ratatui aligns/truncates titles
   unpredictably when the row is overpainted or the width varies). Render the label as an explicit
   left-aligned `Span` on the composer's top border row, at a fixed column, so it is either fully
   present or absent — never `ESSAGE`. Combined with Action 5 (the `Large` overlay no longer reaches
   the composer row because it repaints opaquely and the composer beneath it is simply not visible),
   the clip cannot recur.
2. **No transcript strip through a fullscreen surface.** The `Large` modal covers the full `area`
   minus a symmetric 1-cell inset and repaints opaquely, so nothing of the base shows *inside* it;
   the intentional scrim is only the thin dimmed border, identical for every overlay. This replaces
   the ragged 8%/7% per-overlay margins that read as "chat bleeding through the top."

---

## 5. Changes, file by file (literal Rust)

### `crates/tui/src/theme.rs`

Extend `DiffTokens` with two background tokens (Action 4). This is the only file that may name
`Color::Rgb`/`Indexed`.

```rust
pub struct DiffTokens {
    pub added: Color,
    pub removed: Color,
    pub context: Color,
    pub header: Color,
    /// Full-row background tint behind an added line. `== surface.panel` on
    /// ansi16/monochrome (no tint — foreground cue only), mirroring codex's
    /// "Ansi16 gets no background tint" rule.
    pub added_bg: Color,
    /// Full-row background tint behind a removed line. See `added_bg`.
    pub removed_bg: Color,
}
```
Fill every variant. Suggested truecolor values port codex's GitHub-matched palette:
```rust
// dark():        added_bg: Color::Rgb(0x21, 0x3a, 0x2b), removed_bg: Color::Rgb(0x4a, 0x22, 0x1d),
// light():       added_bg: Color::Rgb(0xda, 0xfb, 0xe1), removed_bg: Color::Rgb(0xff, 0xeb, 0xe9),
// high_contrast: added_bg: Color::Rgb(0x00, 0x33, 0x00), removed_bg: Color::Rgb(0x33, 0x00, 0x00),
// color_blind:   added_bg: Color::Rgb(0x0e, 0x2f, 0x24), removed_bg: Color::Rgb(0x3a, 0x1e, 0x0a), // green/vermillion-tinted, not red
// ansi256:       added_bg: Color::Indexed(22),           removed_bg: Color::Indexed(52),
// ansi16:        added_bg: Color::Black,                  removed_bg: Color::Black,   // == panel: no tint
// monochrome:    added_bg: Color::Black,                  removed_bg: Color::Black,   // == panel: no tint
// system():      added_bg: step1,                         removed_bg: step1,          // subtle blend off real bg
```
Add a token invariant test alongside the existing ones:
```rust
#[test]
fn diff_bg_tints_are_absent_only_on_low_colour_depths() {
    for v in [ThemeVariant::Dark, ThemeVariant::Light, ThemeVariant::HighContrast,
              ThemeVariant::ColorBlindSafe, ThemeVariant::Ansi256] {
        let t = Theme::variant(v);
        assert_ne!(t.diff.added_bg, t.surface.panel, "{v:?}: added tint collapsed to panel");
        assert_ne!(t.diff.removed_bg, t.surface.panel, "{v:?}: removed tint collapsed to panel");
        assert_ne!(t.diff.added_bg, t.diff.removed_bg, "{v:?}: add/remove tints identical");
    }
    assert_eq!(Theme::ansi16().diff.added_bg, Theme::ansi16().surface.panel);
    assert_eq!(Theme::monochrome().diff.removed_bg, Theme::monochrome().surface.panel);
}
```

### `crates/tui/src/render.rs`

**Mode accent (Action 1).** New pure helper near `mode_label` (`render.rs:10626`):
```rust
/// The turn's accent colour, mapped onto existing semantic tokens so no new
/// palette entry is needed and every depth already certifies it.
fn mode_accent(mode: AgentMode, theme: &Theme) -> Color {
    match mode {
        AgentMode::Plan    => theme.status.info,
        AgentMode::Explore => theme.status.success,
        AgentMode::Ask     => theme.text.secondary,
        AgentMode::Review  => theme.status.warning,
        _                  => theme.agent.tool, // Build and any future default
    }
}
```

**Assistant header + rail (Action 1).** In `for_each_row` (`render.rs:1602-1618`), colour the header
glyph and add the tag:
```rust
let accent = mode_accent(run.mode, theme);
let mut spans = vec![Span::styled(
    "⏺ codypendent",
    Style::default().fg(accent).add_modifier(Modifier::BOLD),
)];
if let Some(model) = &run.model {
    spans.push(Span::styled(format!(" · {model}"), Style::default().fg(theme.text.muted)));
}
if run.mode != AgentMode::Build {
    spans.push(Span::styled(
        format!(" · {}", mode_label(run.mode).to_ascii_lowercase()),
        Style::default().fg(theme.text.muted),
    ));
}
push_turn_time(&mut spans, run.entry_time(idx), inner_width, theme);
```
Tag the assistant rows with the rail colour. Extend `Row` (`render.rs:1375`) with
`rail: Option<Color>` (defaulted `None` in `built`/`model`/`rich`), set it on every row emitted for
an assistant turn, and in `build_transcript_window` (`render.rs:1902-1913`) recolour the first
gutter cell:
```rust
if let Some(c) = row.rail {
    // Repaint the leading gutter cell as the mode-accent rail (no extra column;
    // measured width is unchanged, so measure == draw still holds).
    if let Some(first) = visual.spans.first_mut() {
        if first.content.chars().next().is_some_and(|ch| ch == '▌' || ch == ' ') {
            *first = Span::styled("▎", Style::default().fg(c));
        }
    }
}
```

**Heading ladder (Action 2).** Replace the two `Heading` arms in `style_for` (`render.rs:1494-1497`):
```rust
SpanRole::Heading(1) => base.fg(theme.text.heading).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
SpanRole::Heading(2) => base.fg(theme.text.heading).add_modifier(Modifier::BOLD),
SpanRole::Heading(3) => base.fg(theme.text.heading).add_modifier(Modifier::BOLD | Modifier::ITALIC),
SpanRole::Heading(_) => base.fg(theme.text.secondary).add_modifier(Modifier::ITALIC),
```

**Tool output body (Action 3).** In the `card.expanded` block of `tool_card_lines`
(`render.rs:2863-2898`), before the `output:` fallback, when an output preview is present:
```rust
/// codex TOOL_CALL_MAX_LINES.
const TOOL_OUTPUT_MAX_LINES: usize = 5;

if let Some(preview) = card.output_preview.as_deref().filter(|p| !p.is_empty()) {
    let wrapped: Vec<String> = preview
        .lines()
        .flat_map(|l| wrap_display_width(l, width.saturating_sub(6)))
        .collect();
    for line in bounded_head_tail(&wrapped, TOOL_OUTPUT_MAX_LINES) {
        out.push(match line {
            HeadTail::Line(s) => Line::styled(format!("  └ {s}"), Style::default().fg(theme.text.muted)),
            HeadTail::Ellipsis(n) => Line::styled(format!("    … +{n} lines"), Style::default().fg(theme.text.muted)),
        });
    }
}
```
with a small pure helper (unit-testable) that mirrors codex `truncate_lines_middle`:
```rust
enum HeadTail<'a> { Line(&'a str), Ellipsis(usize) }
/// Head+tail slice of `lines` to at most `max` rows, inserting one middle ellipsis.
fn bounded_head_tail(lines: &[String], max: usize) -> Vec<HeadTail<'_>> { /* head n, tail m, `… +omitted` */ }
```
(`tool_card_lines` gains a `width: u16` parameter, threaded from its one caller in
`entry_lines_with_run`.)

**Diff backgrounds (Action 4).** In `patch_lines` (`render.rs:2938-2951`), replace the
foreground-only preview loop:
```rust
for line in patch.preview.lines() {
    let (fg, bg) = if line.starts_with('+') && !line.starts_with("+++") {
        (theme.diff.added, Some(theme.diff.added_bg))
    } else if line.starts_with('-') && !line.starts_with("---") {
        (theme.diff.removed, Some(theme.diff.removed_bg))
    } else if line.starts_with("@@") || line.starts_with("diff --git") {
        (theme.diff.header, None)
    } else {
        (theme.diff.context, None)
    };
    let mut style = Style::default().fg(fg);
    if let Some(bg) = bg { style = style.bg(bg); }
    // Full-row tint: the caller (`build_transcript_window`) already pads the row
    // background to the pane edge when `Row::bg` is set; here the line style
    // reaches only its own cells, so pad to `inner_width` for an edge-to-edge fill.
    out.push(pad_line_to_width(Line::styled(format!("    {line}"), style), theme, bg));
}
```
And the diffstat split in the head (`render.rs:2918-2926`):
```rust
out.push(Line::from(vec![
    Span::styled(format!("{marker} ◆ {target}"), head_style),
    Span::styled(format!("  +{}", patch.additions), Style::default().fg(theme.status.success)),
    Span::styled(format!(" −{}", patch.deletions), Style::default().fg(theme.status.error)),
    Span::styled("  changes ready", Style::default().fg(theme.status.success)),
]));
```

**Unified modal (Action 5).** New enum + entry near `centered_modal` (`render.rs:10212`):
```rust
/// Named modal sizes so overlays of the same class share exact geometry
/// (ported from opencode dialog.tsx presets 60/88/116).
#[derive(Clone, Copy)]
enum ModalSize { Small, Medium, Large }

/// The rect for a modal of `size`, centred and capped so it shrinks safely to
/// 80x24. `Large` is a near-fullscreen list/detail surface with a symmetric
/// 1-cell inset (so the only base showing is the thin dimmed scrim border);
/// `Medium` is a picker; `Small` a confirm/prompt.
fn modal(area: Rect, size: ModalSize) -> Rect {
    match size {
        ModalSize::Small  => centered_modal(area, 60, 12),
        ModalSize::Medium => centered_modal(area, 88, 22),
        ModalSize::Large  => Rect {
            x: area.x + 1, y: area.y + 1,
            width: area.width.saturating_sub(2).max(1),
            height: area.height.saturating_sub(2).max(1),
        },
    }
}
```
Each list/detail overlay's head changes from, e.g. (`render_skills` `render.rs:4490-4510`):
```rust
let rect = centered_rect(84, 84, area);
shield_modal(state, rect);
frame.render_widget(Clear, rect);
let outer = Block::default().borders(Borders::ALL)./*…own title/style…*/;
let inner = outer.inner(rect);
frame.render_widget(outer, rect);
```
to:
```rust
let title = format!("Skill Studio · read only ({})", state.skills.len());
let inner = modal_surface(frame, modal(area, ModalSize::Large), title, state, theme);
```
(`modal_surface` already does `shield_modal` + `Clear` + opaque fill + rounded titled block.) The
same one-line substitution applies to Kanban/Memory/Docs/Journey/Workflow/Blackboard/Edges/
UiPlugins/CouncilBrowser; prompts/confirms use `modal(area, ModalSize::Small)`.

**Composer label (Action 7).** In `render_composer` (`render.rs:2242-2255`), drop the block title
and draw the label as an explicit span on the top border row:
```rust
let label = if steering { " STEER · Enter queues " } else { " MESSAGE · Enter sends " };
let block = Block::default()
    .borders(Borders::TOP)
    .border_style(Style::default().fg(theme.surface.border))
    .style(Style::default().bg(theme.surface.background));
// … render `block` + rows as today …
// Then overlay the label at a fixed column on the border row, so ratatui title
// truncation can never render it as `ESSAGE`:
if area.width > label.len() as u16 {
    let label_rect = Rect { x: area.x, y: area.y, width: label.len() as u16, height: 1 };
    frame.render_widget(
        Paragraph::new(Line::styled(label, Style::default().fg(theme.focus.active).add_modifier(Modifier::BOLD))),
        label_rect,
    );
}
```

---

## 6. Protocol & persistence

**No persistence changes.** All new state (rail colour, mode tag, diff tints, modal geometry) is
derived per-frame from existing `AppState`/`RunView`/`Theme`; nothing is stored, nothing crosses the
wire for Actions 1, 2, 4, 5, 6, 7. `RunView.mode` (`state.rs:1046`) already carries the submission
mode Action 1 stamps — **no protocol field is required for mode stamping** (a plan assumption that
does not hold; see §1).

**One read-only protocol field, flagged not added (Action 3).** `ToolCard` (`state.rs:635`) has
`artifact: Option<ArtifactRef>` (media type + byte length) but **no output text**, so a bash/read
card cannot show a bounded output body today. Rendering the codex-style `  └ ` body needs a bounded,
already-sanitised **output preview** (a few KiB cap, control-stripped like `sanitize_failure_text`)
carried on the `ToolCompleted`/`ToolStarted` wire event and folded onto `ToolCard.output_preview:
Option<String>`. This spec is TUI-only: it **does not** add that field. The Action-3 body renders
only when the field is present; until then the shipped `output: {media_type} ({bytes})` line stays,
so the card is never worse than today. The field belongs to a protocol adoption (flag it to the
protocol owner); its addition is read-only and additive (protocol RULE 1: an older daemon that omits
it → the fallback line, no crash).

---

## 7. Acceptance criteria

Each ties to a vt100 snapshot (`render_buffer` for per-cell style, `VT100Backend` for text) or a
pure reducer/`style_for`/`theme` assertion.

1. **Turn identity + mode stamp (A1).** A transcript with a `Plan` run renders `⏺ codypendent`
   with its glyph in `status.info` and a ` · plan` tag; a `Build` run renders the glyph in
   `agent.tool` with **no** tag. *(vt100 snapshot + per-cell fg assertion.)*
2. **Assistant rail (A1).** Every row of an assistant turn carries a `▎` rail cell in the turn's
   mode accent; the measured transcript height is byte-identical to pre-change (rail replaces the
   gutter cell, adds no column). *(reducer: `transcript_rows` equality; per-cell fg snapshot.)*
3. **Heading ladder (A2).** `style_for(Heading(1))` = BOLD|UNDERLINED, `Heading(2)` = BOLD,
   `Heading(3)` = BOLD|ITALIC, `Heading(4)` = ITALIC on `text.secondary`; the four are pairwise
   distinct `Style`s. *(direct `style_for` assertion.)*
4. **Tool card body (A3).** With `output_preview` present, an expanded bash card shows a `  └ `
   body capped at 5 on-screen lines with a `… +N lines` middle ellipsis; with it absent, the
   `output: …` fallback renders and the card is otherwise unchanged. *(reducer: `bounded_head_tail`
   unit test; vt100 snapshot both ways.)*
5. **Diff background + stat split (A4).** Expanded patch preview: an added line's cells carry
   `bg == theme.diff.added_bg` and `fg == theme.diff.added`; a removed line `removed_bg`/`removed`;
   context lines have no bg; the head shows `+A` in `status.success` and `−B` in `status.error`. On
   ansi16 the tint equals `panel` (no fill). *(per-cell style snapshot in dark + ansi16.)*
6. **Unified geometry (A5).** Skills, Kanban, Memory and the command palette, rendered at the same
   terminal size, place their outer frame at the geometry `modal(area, size)` returns for their
   class; the three `Large` overlays are pixel-identical in outer rect. *(reducer: compare drawn
   border rect / snapshot equality of the frame chrome.)*
7. **Scrim covers list/detail overlays (A6).** With `Overlay::Skills` open, a base cell outside the
   modal has `Modifier::DIM`; a cell inside the modal interior does not. *(per-cell modifier
   assertion, mirroring `modal_scrim_dims_the_base_and_the_interior_shields_click_through`.)*
8. **No composer clip (A7).** With `Overlay::Kanban` open on an 80×24 and a 120×40 terminal, the
   composer label is never a partial substring: the frame contains either the full
   `MESSAGE · Enter sends` or none of it, never `ESSAGE`/`MESSAG`. *(vt100 text snapshot + substring
   assertions at two sizes.)*
9. **No transcript strip (A7).** With `Overlay::Skills` open, no row *above* the modal's top border
   contains transcript glyphs from a seeded run — the `Large` inset border is the only base showing.
   *(vt100 snapshot: rows `0..modal.y` are blank/scrim.)*

---

## 8. Tests

All in `crates/tui` `#[cfg(test)]`, matching the ~625 existing idioms (descriptive snake_case, seed
via `reduce`, assert via `render_buffer`/`VT100Backend`).

**Action 1**
- `plan_run_stamps_the_turn_header_with_a_plan_accent` (per-cell fg on `⏺` + ` · plan` text present).
- `build_run_header_is_unstamped_and_uses_the_tool_accent`.
- `assistant_rows_carry_a_mode_accent_rail` (vt100 snapshot).
- `mode_accent_rail_does_not_change_measured_transcript_height` (reducer: `transcript_rows` before/after seeding equal).

**Action 2**
- `heading_levels_form_a_distinct_ladder` (`style_for` on H1..H4 pairwise `assert_ne!`).
- `assistant_markdown_headings_render_the_ladder` (vt100 snapshot of a `##`/`###` reply's modifiers).

**Action 3**
- `bounded_head_tail_keeps_head_and_tail_with_a_middle_ellipsis` (pure).
- `expanded_bash_card_bounds_its_output_to_five_lines` (vt100 snapshot with a seeded `output_preview`).
- `a_card_without_an_output_preview_keeps_the_artifact_fallback` (no regression).

**Action 4**
- `added_and_removed_diff_lines_carry_background_tints` (per-cell bg in dark).
- `ansi16_diff_lines_have_no_background_tint` (per-cell bg == panel).
- `patch_head_splits_the_diffstat_colours` (per-cell fg on `+A`/`−B`).

**Action 5**
- `every_list_detail_overlay_uses_the_large_modal_geometry` (loop over Skills/Kanban/Memory/Docs/…,
  assert the drawn outer rect == `modal(area, Large)`).
- `pickers_and_prompts_use_their_named_sizes`.

**Action 6**
- `skills_overlay_dims_the_base_and_keeps_its_interior_crisp` (per-cell `DIM`, mirroring the shipped
  ModePicker scrim test).

**Action 7**
- `composer_label_is_never_clipped_under_a_tall_overlay` (Kanban open, 80×24 and 120×40, substring).
- `no_transcript_shows_above_a_fullscreen_overlay` (Skills open, rows above modal are scrim-only).

Keep every existing reducer/snapshot test green; where a snapshot's chrome legitimately changes
(the list/detail overlays' outer frame, the composer border row), re-accept the `.snap` with `cargo
insta review` and note it in the PR.

---

## 9. Gotchas

- **Scrim is DIM, not a 40% blend.** The brief asks for "~40%"; the shipped and ported mechanism is
  the terminal's `Modifier::DIM` SGR, whose intensity is implementation-defined (roughly half on
  most emulators). It is kept because it works on all eight depths and needs no per-cell fg/bg
  rewrite. Do **not** "improve" it into a computed blend — that would break `Color::Reset`
  backgrounds (the `system` theme keeps terminal transparency, `theme.rs:604`) and re-introduce the
  ghost-text class the opaque `modal_surface` fill was written to defeat.
- **Streaming commit discipline (so cards don't reflow).** codex's hardest-won rule
  (`messages.rs` `StreamingAgentTailCell`, `AgentMarkdownCell`): **wrap once at commit width, store
  raw source, never re-wrap the live tail** (re-wrapping splits table borders / OSC-8 links /
  gutters). codypendent already honours this — `TranscriptEntry::Model.rendered` is `None` while
  streaming (render plain, `for_each_row:1628`) and `Some(Vec<RichLine>)` once finalized
  (`markdown.rs::parse`, keyed on width). Do not add per-frame parsing to the tool/diff cards
  either: they render from already-settled `ToolCard`/`PatchSummary` state, so the mode-accent rail
  and diff tint are pure restyles of committed rows — they cannot cause reflow.
- **Rail width must not change measured geometry.** The rail *replaces* the first gutter cell; it
  must not prepend a column, or `measure_transcript` (`render.rs:1853`) and
  `build_transcript_window` will disagree (the crash-class the virtualization tests guard). Assert
  height-equality (test #2/A1).
- **`Large` height math + Kitty images.** The `Large` modal is `area - 2` tall with a 1-cell inset.
  On an 80×24 terminal that is 22 rows — verify list/detail panes still show their floor (the
  `modal_rows`/`picker_regions` logic already degrades gracefully; run the existing
  `issues_overlay_remains_actionable_at_80x24`-style check for the newly-unified overlays). Remote-UI
  documents that draw Kitty/iTerm inline images sit in the *base* layer and are dimmed but not
  cleared by the scrim; the opaque `modal_surface` fill covers any image under a `Large` modal, so no
  half-image bleeds through (the old `centered_rect(84,84)` left an 8% strip where an image could).
- **Colour-blind diff tints.** `color_blind_safe` must keep add/remove tints on the Okabe–Ito axis
  (bluish-green vs vermillion), never red/green — the existing
  `color_blind_safe_avoids_pure_red_green_for_diffs` test (`theme.rs:1077`) covers the foregrounds;
  extend the intent to the new `*_bg` tokens in `diff_bg_tints_are_absent_only_on_low_colour_depths`.
- **We keep no `#` heading markers.** codex renders `## Title` literally; codypendent strips markers
  at parse time and carries the hierarchy in weight/underline. Do not "port faithfully" here — the
  strip is deliberate and several `markdown.rs` tests assert the marker is gone.
- **`tool_card_lines` gains a `width` param.** Its sole caller is `entry_lines_with_run`; thread the
  pane `inner_width` through so the output body wraps to the real pane (the same value
  `render_conversation` publishes to `state.transcript_width`).

---

## 10. Out of scope

- **Per-hunk syntax highlighting inside diffs** (codex `diff_render.rs` syntect pass) — the diff
  preview is daemon-truncated and the background tint is the high-value change; highlighting the
  diff body is a follow-up.
- **Adding the tool `output_preview` protocol field** — flagged in §6, owned by a protocol adoption;
  this spec renders it only when present.
- **A pushable modal *stack*** — opencode's stack is effectively one-slot; codypendent overlays are
  already a single `Overlay` enum with unambiguous Esc destinations. No stack is introduced.
- **opencode's `height/4` top offset and click-drag copy-on-select** — the shipped centred geometry
  and hit-map already serve; not ported.
- **Empty states, status-bar `ctx ——` hiding, splash centring, selection bar** — those are Adoption
  18 (A3/A4), a separate v0.8 spec.
- **Any reducer/keymap behaviour change** — this is a pure *render/style* adoption; overlay open/close,
  fold toggles, and the mode picker are unchanged.
