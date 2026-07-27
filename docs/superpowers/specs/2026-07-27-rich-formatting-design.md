# Rich output formatting (markdown + syntax highlighting + user container) — design

**Date:** 2026-07-27 · **Status:** approved (pre-implementation) · **Branch:** `claude/rich-formatting`

## Problem

The agent's replies render as **plain text**. The transcript walker emits the assistant
message one raw line at a time (`for_each_row`'s `TranscriptEntry::Model` arm,
`crates/tui/src/render.rs:383-395`), so the operator sees literal `**bold**`, `## headers`,
` ```code``` ` fences, and `| pipe | tables |` instead of rendered formatting. There is no
syntax highlighting for code the agent emits, even though the theme already carries a
`syntax` token group (`crates/tui/src/theme.rs:52-59`) used by tool cards.

Separately, the **user turn is not visually contained**. A `You` turn is a bold `"You"`
label followed by `"  {line}"` body lines (`entry_lines`, `render.rs:708-726`), styled with
foreground colour only — nothing sets it apart as its own block, so human input and agent
output blur together in one flat column.

Goal: one client-only PR that (A) renders the **finalized** assistant message as full
markdown — headers, bold/italic, inline code, bullet + numbered lists, block quotes, tables,
and fenced code with per-language syntax highlighting — all coloured from the semantic
`Theme`; and (B) gives the `You` turn a distinct background container. **Without
re-introducing the O(n²)/OOM crash the transcript virtualization just fixed.**

## The critical constraint (read first)

The transcript render was rewritten to fix an OOM. `render.rs` models every logical line as a
`Row { kind: RowKind }` (`render.rs:256-273`) walked by one visitor, `for_each_row`
(`render.rs:334-425`), that drives two passes:

- **MEASURE** — `transcript_rows` (`render.rs:444-450`) visits *every* row and sums
  `Row::columns()` (`render.rs:294-304`). `columns()` **must allocate nothing**: the streamed
  assistant text is `RowKind::Model { text: &'a str, .. }` (borrowed), measured with
  `Span::raw(text).width()`.
- **BUILD** — `build_transcript_window` (`render.rs:453-486`) visits every row to accumulate
  the wrapped cursor but only materializes (`Row::into_line`, `render.rs:308-327`) the rows
  intersecting the visible window — **O(viewport)**, never O(transcript).

**Markdown parsing is expensive. Parsing it in the per-frame render path re-introduces the
crash** (measure walks the whole transcript every frame at 5 fps — `TICK`, `tui.rs:62-63`).
Therefore rich formatting is computed **once, when the message finalizes**, cached on the
transcript entry, and fed to the render as cheap borrowed rows. The steady-state per-frame
cost stays exactly what it is today: MEASURE allocation-free, BUILD O(viewport).

## Scope

**In:** markdown rendering of the finalized `TranscriptEntry::Model` prose; per-language
syntax highlighting of fenced code; a background container for the `You` turn; the cache +
finalization machinery; `Theme` mapping for every new element; one new theme token; one new
markdown dependency (`pulldown-cmark`); an in-crate code tokenizer.

**Out (non-goals):** any protocol/daemon/wire/golden change (this is pure `crates/tui` +
`crates/cli` theme wiring); markdown in tool-card bodies, notes, or the composer; rendering
markdown *while streaming* (streaming stays plain — see §Data flow); images/HTML/footnotes/
math; clickable links (link text is styled, the URL is shown inline as dim text, no mouse
target). Honesty and all other reducer invariants are untouched.

## Architecture

### Two-stage cache (the core decision)

Rendering is split so the **expensive, once** step is theme- and width-**independent**, and
the **cheap, per-frame** step is theme-aware:

1. **Parse (expensive · once · on finalize · theme-independent).** Parse the message's raw
   text into an owned `Vec<RichLine>` of **semantic** spans — each span carries a `SpanRole`
   (a role such as `Heading(2)`, `Strong`, `InlineCode`, `CodeToken(SyntaxRole::Keyword)`),
   **not** a concrete `Color`. Fenced code is tokenized here. Cached on the entry. No `Theme`
   needed.
2. **Style (cheap · per visible row · on build · theme-aware).** In `Row::into_line`, map
   each visible span's `SpanRole` → a `Style` built from the **live** `Theme`, then emit the
   `Line`. Allocation bounded by the viewport, exactly like today's `RowKind::Model` path.

This split is what makes the two hardest requirements fall out for free:

- **Theme change needs no cache invalidation** — colours are applied at build from the
  current theme, so a new theme simply produces new colours on the next frame. The cache
  (semantic roles) is unchanged.
- **Resize needs no cache invalidation** — `RichLine`s are logical (one per markdown line,
  width-independent). MEASURE recomputes wrapped height from the current width via the
  existing `line_rows` (`render.rs:245-253`); BUILD re-emits and the existing
  `Paragraph::wrap(Wrap { trim: false })` (`render.rs:590-593`) soft-wraps — every frame,
  from current width, as today.
- **The reducer stays theme-free** — parsing carries no `Theme`, so finalization lives in the
  reducer (which has no theme handle; the theme is owned by `event_loop`,
  `tui.rs:584-586`) with no signature change to `reduce`.

> The literal `rendered: Option<Vec<Line<'static>>>` (colours baked at finalize) was
> considered and rejected — see §Alternatives. It forces threading `&Theme` into the reducer
> and a theme-keyed invalidation; the semantic-role cache removes both.

### Data model (new types)

New module `crates/tui/src/markdown.rs` (parse + tokenize + the role enums), re-exported from
`lib.rs`. All types derive `Debug, Clone, PartialEq` (so `TranscriptEntry` keeps its derives,
`state.rs:264`):

```rust
/// One rendered logical line: an owned, theme- and width-independent span list.
pub struct RichLine { pub spans: Vec<RichSpan> }

pub struct RichSpan { pub text: String, pub role: SpanRole }

/// Semantic role — mapped to a concrete Style at BUILD time (§Style map).
pub enum SpanRole {
    Gutter,                 // the "▌ " / "  " left rail (mirrors the plain Model path)
    Body,                   // default agent prose
    Heading(u8),            // 1..=6
    Strong,                 // **bold**
    Emphasis,               // *italic*
    StrongEmphasis,         // ***bold italic***
    InlineCode,             // `code`
    Link,                   // link text (URL appended as dim Body)
    ListMarker,             // "• " / "1. "
    BlockQuote,             // "> " body (with a "▏ " gutter)
    Rule,                   // thematic break "───"
    TableHeader,            // header cell text
    TableCell,              // body cell text
    TableRule,              // the "─┼─" separator row / cell borders "│"
    CodePlain,              // fenced-code text the tokenizer left unclassified
    CodeToken(SyntaxRole),  // a classified code token
}

/// Code-token classes — each maps 1:1 to a `theme.syntax.*` token.
pub enum SyntaxRole { Keyword, Literal, StringLit, Comment }
```

`TranscriptEntry::Model` (`state.rs:269`) gains the cache field:

```rust
Model { text: String, rendered: Option<Vec<RichLine>> },
```

`rendered == None` means "not finalized yet — render plain". It is populated exactly once by
the finalization sweep. Memory overhead ≈ the message text again plus per-span metadata,
bounded by `MAX_MODEL_ENTRY_BYTES` (256 KiB, `state.rs:825`) × `MAX_TRANSCRIPT_ENTRIES`
(2000, `state.rs:823`) — the same order as the existing `text` copy.

## Components

### C1 — Markdown parser (`markdown::parse`)

`pub fn parse(text: &str) -> Vec<RichLine>`. Drives `pulldown-cmark` (see §Dependencies) with
`Options::ENABLE_TABLES | ENABLE_STRIKETHROUGH`. Walks the pull-parser event stream, keeping a
small style stack (bold/italic/quote/heading depth, list nesting) and a "current line"
accumulator; `SoftBreak`/`HardBreak`/end-of-block flush a `RichLine`. Element handling:

- **Headings** `Start(Heading(n))..End` → one `RichLine`, spans role `Heading(n)`.
- **Emphasis / Strong / both** → push `Emphasis` / `Strong` / `StrongEmphasis` on the stack.
- **Inline code** `Code(s)` → a single `InlineCode` span.
- **Lists** — track ordinal + nesting; each item's first line gets a leading `ListMarker`
  span (`"• "` for bullets, `"{n}. "` for ordered), body indented 2 cols per nesting level.
- **Block quote** — prefix each line with a `BlockQuote` `"▏ "` gutter span; body spans role
  `BlockQuote`.
- **Thematic break** `Rule` → one `RichLine` of a single `Rule` span filled with `─`
  (rendered to a fixed 24-col rule; the Paragraph does not stretch it — cosmetic).
- **Link** `Start(Link)..End` → link text spans role `Link`; on `End`, append `" ({url})"` as
  a dim `Body` span (no mouse target — out of scope).
- **Fenced code** `Start(CodeBlock(Fenced(info)))` → hand the accumulated code + the language
  (first word of `info`) to C2; splice the returned `RichLine`s in verbatim.
- **Tables** → collected and laid out by C3.
- Every emitted `RichLine` is prepended a `Gutter` span: `"▌ "` for the message's first line,
  `"  "` otherwise — byte-for-byte the plain path's rail (`render.rs:391`).

Total by construction: `pulldown-cmark` never panics on malformed input (unclosed fence /
ragged table degrade to best-effort text). See §Error handling.

### C2 — Code tokenizer (`markdown::highlight`)

`pub fn highlight(lang: &str, src: &str) -> Vec<RichLine>`. **In-crate, zero-dependency**
lexer (see §Dependencies for why not `syntect`). Per line it scans for, in priority order:
line comments (`//`, `#`, `--`), block comments (`/* … */`), string/char literals (`"`, `'`,
`` ` `` with `\` escapes), numeric literals, and identifier runs matched against a
per-language keyword set. Each token becomes a `CodeToken(role)` span; everything else
(identifiers, punctuation, whitespace) is `CodePlain`. Languages with curated keyword +
comment/string rules: `rust`, `python`, `javascript`/`typescript`, `json`, `bash`/`sh`, `go`,
`toml`, `yaml`, `sql`, and a C-like family (`c`, `cpp`, `java`). An unknown or empty language
tag → every span `CodePlain` (still themed, still safe). The output maps **exactly** onto the
four `theme.syntax.*` tokens plus `text.primary`, so it is correct in all four colour depths
(`dark`/`light`/`ansi256`/`ansi16`).

### C3 — Table layout (`markdown::table`)

Collect `Table(alignments)` → `TableHead`/`TableRow` → `TableCell` events into a grid of
`Vec<RichSpan>` cells. Compute each column width = max cell display width (unicode width),
**capped at `MAX_TABLE_WIDTH` / n_cols** so a pathological table can't blow the pane; cells
over their column width are truncated with a trailing `…`. Emit, once, cached:

- a header `RichLine` — cells padded to column width per the parsed alignment (`Left`
  left-pads right, `Right` right-pads left, `Center` splits), joined by a `TableRule` `" │ "`;
- a `TableRule` rule line (`"─┼─"`-style, column widths of `─`);
- one `RichLine` per body row, same padding/joining, cells role `TableCell`.

`MAX_TABLE_WIDTH` is a module const (**100 columns**). Because the reducer has no pane width,
the table lays out to its own content width; if the pane is narrower the Paragraph soft-wraps
the row (degraded but never a crash). See §Width for why this is acceptable.

### C4 — Finalization hook (the parse-once trigger)

A Model message is the **live streaming tail** iff it is the last entry of the last run while
`run.activity == RunActivity::Streaming` — the exact predicate `for_each_row` already uses to
pick the plain path (`render.rs:349-351`, `RunActivity` at `state.rs:336-347`). Any Model that
is **not** the streaming tail is final.

New `fn finalize_streamed_models(state: &mut AppState)`, called **once at the tail of the
`DaemonEvent` reduction**, i.e. immediately after `apply_event` returns (`reduce`,
`reduce.rs:24-26`):

```rust
Action::DaemonEvent(event) => { apply_event(state, *event); finalize_streamed_models(state); }
```

For each run it identifies the live-streaming-tail entry (only possible in the last run), then
for every other `TranscriptEntry::Model { text, rendered }` with `rendered.is_none()`, sets
`rendered = Some(markdown::parse(text))`. Properties:

- **Parse-once** — idempotent (skips any entry whose cache is already `Some`); each entry is
  parsed at most once in its lifetime.
- **Covers every finalization path** — running after *every* folded event, it catches all
  transitions that end streaming without enumerating them: a new entry pushed after the Model
  (`ToolProposed`/`ToolStarted`/`PatchProposed`/`RunCompleted` push via `push_entry`,
  `reduce.rs:348/377/445/522`), an activity change off `Streaming`
  (`RunStateChanged`/`ToolCompleted`, `reduce.rs:324/410`), the byte-cap split
  (`append_model_text`, `state.rs:1238-1255`), and older runs.
- **Bounded** — per event it is O(total Model entries) cheap `Option::is_none` checks (capped
  by `MAX_TRANSCRIPT_ENTRIES`); real parse work happens once per entry, never per frame.
- **Never touches the tail** — the still-streaming entry keeps `rendered == None` and renders
  plain, so formatting "snaps" in only when the message stops (§Data flow).

`append_model_text` (`state.rs:1237-1255`) constructs `Model { text, rendered: None }` and, on
its fast append path, keeps `rendered` `None` (defensively re-asserts it — normally a no-op,
since only the never-finalized tail receives appends). Existing tests that construct
`TranscriptEntry::Model { text }` (e.g. `reduce.rs:2158`) add `rendered: None`.

### C5 — Render integration (`render.rs`)

Add a third, **borrowing** row kind — mirroring `Model` so MEASURE stays allocation-free:

```rust
enum RowKind<'a> {
    Built(Line<'a>),
    Model { prefix: &'static str, text: &'a str, caret: bool, style: Style },
    Rich(&'a RichLine),   // borrows the cached line
}
```

- `Row::columns()` (`render.rs:294-304`): `RowKind::Rich(rl)` → `rl.spans.iter().map(|s|
  Span::raw(&s.text).width()).sum()` — the same borrowed, **allocation-free** idiom the
  `Model` arm already uses (`render.rs:302`).
- `Row::into_line(theme)` (`render.rs:308-327`): `RowKind::Rich(rl)` → for each span build
  `Span::styled(span.text.clone(), style_for(span.role, theme))` and collect a `Line`.
  Allocation bounded by the visible spans — **O(viewport)**.

`for_each_row`'s Model arm (`render.rs:383-395`) branches:

```rust
TranscriptEntry::Model { text, rendered } => {
    match rendered {
        Some(lines) if !streaming_tail => {           // RICH
            for rl in lines { visit(Row::rich(rl)); produced = true; }
        }
        _ => { /* PLAIN — the existing borrowed RowKind::Model per source line */ }
    }
}
```

`streaming_tail || rendered.is_none()` ⇒ plain. `rendered.is_none()` is belt-and-braces: the
sweep fills the cache in the same `reduce` call that ends streaming, before the next render,
so by render time a finalized entry is `Some`; if it ever weren't, it renders plain (never
blank, never a parse in the render path).

### C6 — Style map (`style_for(role, theme) -> Style`)

Every colour is a `Theme` token — no hard-coded truecolor — so it is correct in `dark`,
`light`, `ansi256`, and `ansi16` (`theme.rs:114/…/331/385`):

| Element | `SpanRole` | Style (theme tokens + modifiers) |
| --- | --- | --- |
| body prose | `Body` | `fg agent.model_text` |
| left rail | `Gutter` | `fg text.muted` |
| h1 / h2 | `Heading(1..=2)` | `fg text.heading` · **BOLD** · UNDERLINED |
| h3–h6 | `Heading(3..=6)` | `fg text.heading` · **BOLD** |
| bold | `Strong` | `fg text.primary` · **BOLD** |
| italic | `Emphasis` | `fg agent.model_text` · *ITALIC* |
| bold italic | `StrongEmphasis` | `fg text.primary` · **BOLD** · *ITALIC* |
| inline code | `InlineCode` | `fg syntax.string` |
| link text | `Link` | `fg focus.active` · UNDERLINED |
| bullet / number | `ListMarker` | `fg agent.tool` |
| block quote | `BlockQuote` | `fg text.secondary` · *ITALIC* (gutter `"▏ "` in `text.muted`) |
| thematic break | `Rule` | `fg text.muted` |
| table header | `TableHeader` | `fg text.heading` · **BOLD** |
| table cell | `TableCell` | `fg agent.model_text` |
| table borders | `TableRule` | `fg surface.border` |
| code (plain) | `CodePlain` | `fg text.primary` |
| code keyword | `CodeToken(Keyword)` | `fg syntax.keyword` |
| code number/bool | `CodeToken(Literal)` | `fg syntax.literal` |
| code string | `CodeToken(StringLit)` | `fg syntax.string` |
| code comment | `CodeToken(Comment)` | `fg syntax.comment` |

### C7 — User-message container

Add one background token, `SurfaceTokens.user: Color` (`theme.rs:14-24`), a subtly raised
surface distinct from `panel`:

- `dark` `Rgb(0x20,0x24,0x2c)` · `light` a light-gray `Rgb(0xea,0xec,0xf1)` · `ansi256`
  `Indexed(236)` · `ansi16` — no distinct subtle bg exists in 16 colours, so the container
  degrades to a leading accent bar (see below), bg = `panel`.
- Wire the token through `theme_pack::set_token` (`theme_pack.rs:146+`) as `"surface.user"`
  so packs can override it; the manifest starts from a variant, so the field is populated by
  every `variant()`/`dark()`/… constructor and needs no manifest change to exist.

Render mechanism (virtualization- and resize-safe). The `Row` struct (`render.rs:256-261`)
gains `bg: Option<Color>` (default `None`; cosmetic — `columns()`/`rows()` ignore it). In
`for_each_row`, the `You` header + body rows (the `User` branch flows through the `other =>`
arm → `entry_lines`, `render.rs:708-726`) are tagged `row.bg = Some(theme.surface.user)`. In
`build_transcript_window` (`render.rs:453-486`), after `row.into_line(theme)`, if `row.bg` is
`Some(c)`: set the line's style `bg = c` **and** right-pad the line to `inner_width` with a
`c`-bg space span, so the block fills the pane width. This is done only for **visible** rows
(**O(viewport)**), uses the **current** `inner_width` (**resize-safe, no cache**), and never
adds a wrapped row (padding targets exactly `inner_width`). The surrounding blank separator
lines (`render.rs:360`) stay `panel`, so the container reads as a discrete block. On `ansi16`,
in place of the bg, prepend a `focus.active` `"▎"` accent bar to each user line.

## Data flow (streaming plain → finalize rich)

```
ModelStreamDelta ─▶ append_model_text (activity=Streaming)  entry = live streaming tail
                    └─ finalize sweep: tail skipped, rendered stays None
   render ─▶ for_each_row: streaming_tail ⇒ PLAIN RowKind::Model (borrowed, caret) — fast
            (repeat per delta — append only, NEVER a markdown parse)

stream ends (tool starts │ run completes │ activity leaves Streaming │ 256KiB split)
   apply_event mutates state ─▶ finalize_streamed_models:
            entry no longer the tail, rendered==None ─▶ markdown::parse ONCE ─▶ rendered=Some
   next render ─▶ for_each_row: !streaming_tail & Some ⇒ RICH RowKind::Rich(&cached)
            MEASURE alloc-free · BUILD O(viewport) · text "snaps" plain → formatted
```

The explicit UX: **while streaming, the message is plain** (fast, the growing text is its own
liveness signal); **on finalize it snaps to rich**. Markdown is parsed once per message,
never per delta and never per frame.

## Dependencies (cargo-deny surface)

`deny.toml` allows `MIT`, `Apache-2.0`, `BSD-3-Clause`, `ISC`, `Unicode-3.0`, `MPL-2.0`,
`Zlib`, `BSL-1.0`, `CC0-1.0`, `CDLA-Permissive-2.0` (+ LLVM-exception); sources must be
crates.io. Add one dependency, pinned centrally in root `[workspace.dependencies]` and
referenced `{ workspace = true }` (the crate convention, `crates/tui/Cargo.toml`):

**Markdown parser — `pulldown-cmark` (RECOMMENDED).** License **MIT** (allow-listed). Pure
Rust, no C, no runtime assets; the CommonMark parser behind rustdoc/mdBook (this repo already
ships a `book.toml`). Pull-parser event stream maps directly onto the `RichLine` builder;
GFM tables via `Options::ENABLE_TABLES`. Its library dependencies — `bitflags`, `memchr`,
`unicase`, `pulldown-cmark-escape` — are MIT/Apache/Unicode (deny-clean), and `bitflags`
(1.3.2) + `memchr` (2.8.3) are **already in the tree** (`Cargo.lock`), so the net addition is
small. Use `default-features = false` to drop its `getopts` CLI path.

**Syntax highlighter — in-crate tokenizer (C2), no new crate (RECOMMENDED).** Rationale:
- `syntect` (**REJECTED**): MIT, but heavy — it pulls a regex engine (`onig`/Oniguruma **C**
  via `onig-sys`, or `fancy-regex`) plus binary syntax/theme **assets** (`flate2` +
  `miniz_oxide`), inflating build time, binary size, and the supply-chain surface. Its themes
  are **truecolor `.tmTheme`** files that do **not** map to the four semantic `theme.syntax.*`
  tokens or the `ansi16`/`ansi256` depths this app requires — you would discard its themes and
  use only its scope stack, paying the whole weight for a tokenizer.
- `two-face` (**REJECTED**): an *asset pack layered on syntect* — strictly heavier, same
  truecolor-theme mismatch.
- `synoptic` (sanctioned **upgrade path**, not now): MIT, pure-Rust, asset-free, regex-based;
  emits token kinds you colour yourself (maps cleanly to `theme.syntax.*`). Its only heavy
  dep, `regex` (1.13.1), is **already in the tree**, so adopting it later is low-cost. Chosen
  against only because the in-crate lexer needs **zero** new deps and covers chat-scale
  snippets; escalate to `synoptic` if broad multi-language grammar fidelity is later required.

Net: **one** new deny-clean crate (`pulldown-cmark`, MIT). No advisory/licence/source
exception needed. `cargo deny check bans licenses sources` must stay green.

## Error handling

- **Malformed markdown** (unclosed fence, ragged table, stray `*`): `pulldown-cmark` is total
  — it emits best-effort events and never panics; the message renders as close-to-intended
  text. No crash path.
- **Unknown / empty code language**: C2 returns all-`CodePlain` (themed `text.primary`).
- **Oversized message**: parse cost is bounded by `MAX_MODEL_ENTRY_BYTES` (256 KiB) and paid
  once. A module const `RICH_MARKDOWN_MAX_BYTES` (**64 KiB**) guards worst-case single-parse
  latency: above it, `finalize_streamed_models` leaves `rendered = None` so the message stays
  on the fast plain path (documented, deterministic).
- **Pathological table**: `MAX_TABLE_WIDTH` cap + per-cell `…` truncation (C3).
- **Unicode width**: span widths use the same `unicode-width` measure ratatui's
  `Span::width` uses, so MEASURE (`columns`) and the wrap accounting stay consistent with the
  existing rows.
- **`rendered` unexpectedly `None` at render**: falls through to the plain path — never blank,
  never a parse in the render path.
- **`RunDisposition`/`EventBody` non-exhaustive variants**: untouched — the Model path change
  is orthogonal to the protocol enums.

## Constraints

1. **Virtualization preserved (the #1 requirement — the crash path).** No markdown parsing in
   the per-frame render path. Parse-once-on-finalize into a cached `Vec<RichLine>`; MEASURE
   (`Row::columns` for `RowKind::Rich`) allocation-free; BUILD O(viewport). The existing test
   `build_transcript_window_materializes_only_the_viewport` (`render.rs:5438`) must still hold
   with a large rich message.
2. **Client-only.** No protocol/daemon/wire/golden change. Edits confined to `crates/tui`
   (+ `crates/cli` theme wiring for the new token). `crates/tui/Cargo.toml` gains one dep.
3. **Theme-aware.** Every colour via a `Theme` token; syntax mapped to `theme.syntax.*`, never
   truecolor; correct in `dark`/`light`/`ansi256`/`ansi16`. Live theme change (should it land;
   today the theme is resolved once at startup and threaded immutably, `tui.rs:237,584-586`)
   needs **no** cache invalidation — colours are applied at build.
4. **Streaming = plain, finalize = rich** (explicit UX, §Data flow).
5. **Deps deny-clean + licence-compatible.** One new crate: `pulldown-cmark` (MIT). Highlighter
   in-crate (no crate). No new advisory/licence/source exceptions; minimal bundle/build weight.
6. **Honesty / other invariants untouched.** No new placeholders; the reducer stays a pure
   projection; `reduce` keeps its signature (finalize is theme-free).

## Testing

Unit tests in `markdown.rs`, `render.rs`, and `reduce.rs` (the crate's existing test style):

- **Parse-once (asserts NOT parsed per frame).** A `#[cfg(test)]` `AtomicUsize` counter in
  `markdown::parse`. Finalize a Model, then call `build_transcript_window` N (≥ 20) times;
  assert the counter incremented **exactly once** and `entry.rendered.is_some()` before any
  render. (Complements: the sweep leaves the streaming tail `rendered == None`.)
- **Virtualization bounded with a large rich message.** Build a Model of thousands of markdown
  lines, finalize, assert `build_transcript_window` returns ≤ `viewport + overscan` lines and
  that `transcript_rows` (measure) walks without materializing — mirroring
  `build_transcript_window_materializes_only_the_viewport` (`render.rs:5438`).
- **Elements render with the right styles.** Parse fixtures and assert the mapped `Style`:
  `# h1` → `text.heading` + BOLD; `**b**` → BOLD; `` `c` `` → `syntax.string`; a bullet list →
  `ListMarker` in `agent.tool`; `> q` → `BlockQuote` italic.
- **A code fence highlights.** Parse a ` ```rust ` fence; assert at least one span is
  `CodeToken(Keyword)` and maps to `theme.syntax.keyword`; a `json` fence highlights strings.
- **A table renders aligned.** Parse a GFM table; assert equal per-column padded widths, a
  `TableRule` separator row, and header cells styled `TableHeader`.
- **User-message container bg applied.** Build a `You` turn through the window builder; assert
  the built user lines carry `bg == theme.surface.user` and are padded to `inner_width`
  (and the `ansi16` accent-bar fallback).
- **Theme change re-renders without re-parsing.** Finalize once, build under `Theme::dark()`
  then `Theme::light()`; assert the parse counter is unchanged (1) but the produced span
  colours differ — proving parse-once and theme-awareness together.

## Alternatives considered

- **Cache baked `Vec<Line<'static>>` (colours at finalize).** The literal task suggestion.
  Rejected: `Line` bakes concrete `Color`s, so a theme change requires storing the theme
  identity (`Theme` is `Copy + PartialEq`, `theme.rs:99`) and re-parsing on mismatch; and
  finalize would need `&Theme`, forcing it out of the reducer (which has none) or a `reduce`
  signature change. The semantic-role cache removes both problems at the cost of a cheap
  per-frame role→`Style` map (same allocation class as today's `format!` in `into_line`).
- **Lazy parse in the BUILD pass (memoized `OnceCell`).** Parses at most once per entry but
  puts the parse in the render path (guarded); rejected in favour of the explicit reducer
  finalize so no frame can ever trigger a parse.
- **Pre-wrap the cache to the pane width.** Cleaner columns but bakes width in → resize
  invalidation. Rejected: logical `RichLine`s + the existing `line_rows` measure + Paragraph
  soft-wrap already handle over-width lines exactly as today's plain Model path, with zero
  resize invalidation. (Trade-off: fenced code and tables soft-wrap unattractively when the
  pane is narrower than the content — a graceful, non-crashing degradation.)
- **`syntect` / `two-face`.** Rejected — see §Dependencies (C regex + truecolor-only assets
  vs. the semantic 4-token palette).

## Component decomposition (plan-task seeds)

- **T1 — Data model.** New `markdown.rs` with `RichLine`/`RichSpan`/`SpanRole`/`SyntaxRole`;
  extend `TranscriptEntry::Model { text, rendered }`; update `append_model_text` +
  constructing tests. (No behaviour yet.)
- **T2 — Markdown parser (C1).** Add `pulldown-cmark` (workspace dep, `default-features =
  false`); `markdown::parse` for headings/emphasis/inline code/lists/blockquotes/rule/links
  (+ Gutter rail); the fenced-code body plain for now. Element unit tests.
- **T3 — Code tokenizer (C2).** In-crate `highlight`; per-language keyword/comment/string
  rules; splice into T2's fence handling. Fence-highlight tests.
- **T4 — Table layout (C3).** Collect + align + truncate to `MAX_TABLE_WIDTH`. Table test.
- **T5 — Finalize hook (C4).** `finalize_streamed_models` + the `reduce` call site; the
  streaming-tail predicate; `RICH_MARKDOWN_MAX_BYTES` guard. Parse-once tests.
- **T6 — Render integration (C5/C6).** `RowKind::Rich`; alloc-free `columns`; `style_for`
  map; `for_each_row` plain/rich branch. Style + virtualization-bound tests.
- **T7 — User container (C7).** `surface.user` token (4 variants + `theme_pack::set_token`);
  `Row.bg`; pad-to-width in build; `ansi16` accent fallback. Container-bg test.
- **T8 — Hygiene.** `cargo deny check bans licenses sources` green; confirm no
  protocol/golden diff; theme-change-re-renders test.

## Open decisions to confirm before planning

1. **Cache representation.** Confirm the **semantic-role cache** (recommended:
   theme-invalidation-free, reducer stays theme-free) over the simpler baked
   `Vec<Line<'static>>` cache (needs `&Theme` in finalize + theme-keyed invalidation).
2. **Highlighter.** Confirm the **in-crate tokenizer** (zero new deps, maps to the four
   `theme.syntax.*` tokens, curated language set) over adding `synoptic` now — with `synoptic`
   sanctioned as the later upgrade path and `syntect`/`two-face` rejected.
3. **User container.** Confirm adding a **new `surface.user` theme token** (a field on
   `SurfaceTokens` set across all four variants + `theme_pack`) rather than reusing
   `surface.overlay` (zero-schema, but semantically "modal"), and the **full-width bg via
   build-time pad-to-width** with the `ansi16` accent-bar fallback.
