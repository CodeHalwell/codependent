# Rich Output Formatting (markdown + syntax highlighting + user container) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render the *finalized* agent message as full markdown (headers, bold/italic, inline code, lists, block quotes, tables, and fenced code with per-language syntax highlighting) and give the `You` turn a background container — all coloured from the semantic `Theme`, without re-introducing the transcript virtualization OOM.

**Architecture:** A two-stage cache. Parsing (expensive, once, on finalize, theme/width-independent) turns a message's raw text into an owned `Vec<RichLine>` of *semantic* spans (`SpanRole`, not `Color`), cached on the transcript entry. Styling (cheap, per visible row, on build, theme-aware) maps each visible span's `SpanRole` → a `Style` from the live `Theme`. Streaming stays plain (fast, borrowed); the message "snaps" to rich when it stops. Markdown is parsed with `pulldown-cmark`; code is highlighted with `synoptic`.

**Tech Stack:** Rust, `ratatui` 0.29, `pulldown-cmark` 0.13 (MIT), `synoptic` 2.2 (MIT). Client-only — `crates/tui` plus the root/`crates/tui` manifests. No protocol/daemon/wire/golden change.

## Global Constraints

- **VIRTUALIZATION PRESERVED (the #1 requirement — the crash path).** Parse-once-on-finalize into a cached `Vec<RichLine>`. NO markdown/highlight parse in the per-frame render path. MEASURE (`Row::columns`) allocation-free; BUILD (`Row::into_line`) O(viewport), never O(history). The existing test `build_transcript_window_materializes_only_the_viewport` (`crates/tui/src/render.rs`) must still hold with a large *rich* message.
- **Client-only.** NO change to `crates/protocol`, `crates/daemon`, any wire type, or `crates/protocol/tests/golden_vectors.rs`. Edits are confined to `crates/tui`, root `Cargo.toml`, `crates/tui/Cargo.toml`, and (if a license needs it) `deny.toml`. There is NO `crates/cli` theme-token code — the CLI consumes `Theme` wholesale, so a new field needs no CLI edit.
- **Theme-aware, every depth.** Every colour is a `Theme` token; syntax maps to `theme.syntax.*`; NO hardcoded truecolor in the render path. Correct in ALL SEVEN theme constructors: `dark`, `light`, `high_contrast`, `color_blind_safe`, `ansi256`, `ansi16`, `monochrome`. A theme change needs NO cache invalidation (colours applied at build).
- **Streaming = plain, finalize = rich** (explicit UX).
- **Deps deny-clean.** `pulldown-cmark` (MIT) + `synoptic` (MIT), both workspace-pinned and referenced `{ workspace = true }`. `cargo deny check bans licenses sources` stays green.
- **Honesty / other invariants untouched.** The reducer stays a pure projection; `reduce` keeps its signature (finalize is theme-free). No new placeholders.
- **NEVER `git add -A`.** Stage only the explicit paths named in each task's commit step. NEVER touch `docs/cli-and-tui-user-guide.md` (untracked), `README.md`, `docs/docs/*`, `ROADMAP.md`, or `.superpowers/`.
- Every commit trailer: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

### Spec-vs-real reconciliations baked into this plan

1. The design spec's highlighter section (in-crate tokenizer, 4 syntax slots) is **overridden** by the approved amendment: use `synoptic`, and **expand** the syntax palette. Everything else in the spec is kept.
2. The spec named "four colour depths" — the real `theme.rs` has **seven** `Theme` constructors. Every new struct field is set in all seven or the crate will not compile (Task 4).
3. The spec mentioned "crates/cli theme wiring for the new token" — there is **no** per-token theme code in `crates/cli` (verified). This plan touches only `crates/tui` + manifests.
4. `Tag::Heading`/`Tag::Link` are **struct** variants and `End` carries `TagEnd` (pulldown-cmark 0.13); the parser (Task 2) is written against that.
5. ansi16/monochrome have no distinct raised surface: `surface.user == surface.panel` there, and the render's user-container path detects that equality to fall back to a `focus.active` accent bar (Task 8) — depth-agnostic, no depth tag on `Theme`.

---

## Interfaces (shared signatures — depended on across tasks)

**New module `crates/tui/src/markdown.rs` (Task 1) — pure types, no `Theme`:**

```rust
/// One rendered logical line: an owned, theme- and width-independent span list.
#[derive(Debug, Clone, PartialEq)]
pub struct RichLine { pub spans: Vec<RichSpan> }

#[derive(Debug, Clone, PartialEq)]
pub struct RichSpan { pub text: String, pub role: SpanRole }

/// Semantic role — mapped to a concrete Style at BUILD time by `style_for` (Task 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanRole {
    Gutter,                 // "▌ " / "  " left rail, and the "▏ " block-quote bar
    Body,                   // default agent prose
    Heading(u8),            // 1..=6
    Strong,                 // **bold**
    Emphasis,               // *italic*
    StrongEmphasis,         // ***bold italic***
    InlineCode,             // `code`
    Link,                   // link text
    ListMarker,             // "• " / "1. "
    BlockQuote,             // "> " body
    Rule,                   // thematic break "───"
    TableHeader,            // header cell text
    TableCell,              // body cell text
    TableRule,              // "─┼─" separators / "│" borders
    CodePlain,              // fenced-code text the highlighter left unclassified
    CodeToken(SyntaxRole),  // a classified code token
}

/// Code-token classes — each maps 1:1 to a `theme.syntax.*` token (expanded palette).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxRole {
    Keyword, Literal, StringLit, Comment,   // the existing four
    Type, Function, Operator, Constant, Punctuation,  // the amendment's additions
}

pub fn parse(text: &str) -> Vec<RichLine>;            // Task 2 (fence plain) → Task 3/5 (highlight/table)
pub fn highlight(lang: &str, src: &str) -> Vec<RichLine>;  // Task 3 (gutter-less lines)

#[cfg(test)]
pub static PARSE_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
```

**`crates/tui/src/state.rs` (Task 1):** `TranscriptEntry::Model { text: String, rendered: Option<Vec<RichLine>> }`. `rendered == None` ⇒ render plain.

**`crates/tui/src/reduce.rs` (Task 6):**

```rust
const RICH_MARKDOWN_MAX_BYTES: usize = 64 * 1024;
pub(crate) fn finalize_streamed_models(state: &mut AppState);
```

**`crates/tui/src/render.rs` (Task 7 + 8):**

```rust
enum RowKind<'a> { Built(Line<'a>), Model { .. }, Rich(&'a RichLine) }
struct Row<'a> { kind: RowKind<'a>, hit_entry: Option<usize>, bg: Option<Color> }  // bg added Task 8
impl<'a> Row<'a> { fn rich(rl: &'a RichLine) -> Self; }
fn style_for(role: SpanRole, theme: &Theme) -> Style;   // Task 7
```

**`crates/tui/src/theme.rs` (Task 4):** `SyntaxTokens` gains `r#type, function, operator, constant, punctuation`. `SurfaceTokens` gains `user`. Both set in all seven constructors. `theme_pack::set_token` gains `"syntax.type" | "syntax.function" | "syntax.operator" | "syntax.constant" | "syntax.punctuation" | "surface.user"`.

---

## Task 1: Data model — `markdown.rs` types + `Model.rendered` cache

**Files:**
- Create: `crates/tui/src/markdown.rs`
- Modify: `crates/tui/src/lib.rs:30` (add `pub mod markdown;` + re-export)
- Modify: `crates/tui/src/state.rs:269` (`Model` variant), `crates/tui/src/state.rs:1238-1255` (`append_model_text`)
- Modify: `crates/tui/src/render.rs:383`, `crates/tui/src/render.rs:727` (`Model` match arms — add `rendered`), `crates/tui/src/reduce.rs:2158` (test)
- Test: inline `#[cfg(test)]` in `crates/tui/src/markdown.rs`

**Interfaces:**
- Produces: `RichLine`, `RichSpan`, `SpanRole`, `SyntaxRole`, `PARSE_CALLS` (see Interfaces block); `TranscriptEntry::Model { text, rendered }`.
- Consumes: nothing.

- [ ] **Step 1: Write the failing test** — append to a new `crates/tui/src/markdown.rs`:

```rust
//! Rich-text data model + markdown parse/highlight for the finalized agent
//! message (client-only; see docs/superpowers/plans/2026-07-27-rich-formatting.md).
//! Types are semantic (a `SpanRole`, never a concrete `Color`) so the cache is
//! theme- and width-independent; styling happens at build time in `render.rs`.

/// One rendered logical line: an owned, theme- and width-independent span list.
#[derive(Debug, Clone, PartialEq)]
pub struct RichLine {
    pub spans: Vec<RichSpan>,
}

/// One styled run of text, tagged with a semantic role (not a colour).
#[derive(Debug, Clone, PartialEq)]
pub struct RichSpan {
    pub text: String,
    pub role: SpanRole,
}

/// Semantic role — mapped to a concrete `Style` at BUILD time by `render::style_for`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanRole {
    Gutter,
    Body,
    Heading(u8),
    Strong,
    Emphasis,
    StrongEmphasis,
    InlineCode,
    Link,
    ListMarker,
    BlockQuote,
    Rule,
    TableHeader,
    TableCell,
    TableRule,
    CodePlain,
    CodeToken(SyntaxRole),
}

/// Code-token classes — each maps 1:1 to a `theme.syntax.*` token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxRole {
    Keyword,
    Literal,
    StringLit,
    Comment,
    Type,
    Function,
    Operator,
    Constant,
    Punctuation,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn richline_holds_roled_spans() {
        let line = RichLine {
            spans: vec![
                RichSpan { text: "▌ ".into(), role: SpanRole::Gutter },
                RichSpan { text: "hi".into(), role: SpanRole::Heading(1) },
                RichSpan { text: "x".into(), role: SpanRole::CodeToken(SyntaxRole::Keyword) },
            ],
        };
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[1].role, SpanRole::Heading(1));
        assert_eq!(line.spans[2].role, SpanRole::CodeToken(SyntaxRole::Keyword));
        assert_eq!(line, line.clone());
    }
}
```

- [ ] **Step 2: Wire the module + re-export.** In `crates/tui/src/lib.rs`, add `pub mod markdown;` (after `pub mod input;`, line 30) and, after `pub use reduce::reduce;`, add:

```rust
pub use markdown::{RichLine, RichSpan, SpanRole, SyntaxRole};
```

- [ ] **Step 3: Add the cache field.** In `crates/tui/src/state.rs`, change the `Model` variant (line 269) to:

```rust
    /// Coalesced streamed model prose. `rendered` is the parse-once rich cache:
    /// `None` while streaming (render plain); `Some` once finalized (render rich).
    Model {
        text: String,
        rendered: Option<Vec<crate::markdown::RichLine>>,
    },
```

- [ ] **Step 4: Update `append_model_text`.** In `crates/tui/src/state.rs` (1238-1255), match/construct the new shape and defensively keep the tail's cache `None`:

```rust
    pub(crate) fn append_model_text(run: &mut RunView, text: &str) {
        if let Some(TranscriptEntry::Model { text: existing, rendered }) = run.transcript.last_mut() {
            if existing.len() + text.len() <= MAX_MODEL_ENTRY_BYTES {
                existing.push_str(text);
                // The only entry that receives appends is the never-finalized
                // streaming tail; keep its cache empty so it renders plain.
                *rendered = None;
                return;
            }
        }
        Self::push_entry(
            run,
            TranscriptEntry::Model {
                text: text.to_owned(),
                rendered: None,
            },
        );
    }
```

- [ ] **Step 5: Fix the three other `Model` match sites so the crate compiles.**
  - `crates/tui/src/render.rs:383`: `TranscriptEntry::Model { text } =>` → `TranscriptEntry::Model { text, .. } =>` (Task 7 replaces this arm; `..` keeps it compiling now).
  - `crates/tui/src/render.rs:727`: `TranscriptEntry::Model { text } =>` → `TranscriptEntry::Model { text, .. } =>`.
  - `crates/tui/src/reduce.rs:2158`: `TranscriptEntry::Model { text } =>` → `TranscriptEntry::Model { text, .. } =>`.
  - (`render.rs:354` already uses `TranscriptEntry::Model { .. }` — leave it.)

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p codypendent-tui markdown:: && cargo build -p codypendent-tui`
Expected: `richline_holds_roled_spans` PASSES; the crate compiles (all `Model` sites updated).

- [ ] **Step 7: Commit**

```bash
git add crates/tui/src/markdown.rs crates/tui/src/lib.rs crates/tui/src/state.rs crates/tui/src/render.rs crates/tui/src/reduce.rs
git commit -m "feat(tui): rich-text data model + Model.rendered cache field" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 2: Markdown parser (`markdown::parse`)

**Files:**
- Modify: `Cargo.toml:130` (root `[workspace.dependencies]` — add `pulldown-cmark`)
- Modify: `crates/tui/Cargo.toml:20` (add `pulldown-cmark = { workspace = true }`)
- Modify: `crates/tui/src/markdown.rs` (add `parse` + a `Builder`)
- Test: inline `#[cfg(test)]` in `crates/tui/src/markdown.rs`

**Interfaces:**
- Consumes: `RichLine`, `RichSpan`, `SpanRole` (Task 1).
- Produces: `pub fn parse(text: &str) -> Vec<RichLine>` and `PARSE_CALLS`. Fenced code renders as `CodePlain` lines here (Task 3 swaps in highlighting; Task 5 fills the Table arms).

- [ ] **Step 1: Add the dependency.** In root `Cargo.toml` `[workspace.dependencies]` (near line 130), add:

```toml
# Rich TUI output formatting (client-only, Phase 6). pulldown-cmark: the
# CommonMark + GFM-tables parser behind rustdoc/mdBook (MIT). default-features
# off drops its getopts CLI path; the pull-parser (Parser/Event/Tag) is core.
pulldown-cmark = { version = "0.13", default-features = false }
```

In `crates/tui/Cargo.toml`, under `[dependencies]`, add:

```toml
pulldown-cmark = { workspace = true }
```

- [ ] **Step 2: Write the failing tests** — add to the `tests` module in `crates/tui/src/markdown.rs`:

```rust
    // Every line starts with the left rail: "▌ " on the message's first line, "  " after.
    fn body_roles(line: &RichLine) -> Vec<SpanRole> {
        line.spans.iter().skip(1).map(|s| s.role).collect() // skip the Gutter span
    }

    #[test]
    fn heading_becomes_one_line_of_heading_role() {
        let lines = parse("## Title");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].role, SpanRole::Gutter);
        assert_eq!(lines[0].spans[0].text, "▌ ");
        assert!(body_roles(&lines[0]).iter().all(|r| *r == SpanRole::Heading(2)));
        assert!(lines[0].spans.iter().any(|s| s.text.contains("Title")));
    }

    #[test]
    fn emphasis_strong_and_inline_code_get_their_roles() {
        let lines = parse("plain **bold** *it* `code`");
        let roles = body_roles(&lines[0]);
        assert!(roles.contains(&SpanRole::Body));
        assert!(roles.contains(&SpanRole::Strong));
        assert!(roles.contains(&SpanRole::Emphasis));
        assert!(roles.contains(&SpanRole::InlineCode));
    }

    #[test]
    fn bullet_list_item_starts_with_a_list_marker() {
        let lines = parse("- one\n- two");
        assert_eq!(lines.len(), 2);
        assert_eq!(body_roles(&lines[0])[0], SpanRole::ListMarker);
        assert_eq!(lines[0].spans[1].text, "• ");
    }

    #[test]
    fn ordered_list_numbers_items() {
        let lines = parse("1. a\n2. b");
        assert_eq!(lines[0].spans[1].text, "1. ");
        assert_eq!(lines[1].spans[1].text, "2. ");
        assert_eq!(body_roles(&lines[0])[0], SpanRole::ListMarker);
    }

    #[test]
    fn block_quote_gets_a_bar_and_quote_body() {
        let lines = parse("> quoted");
        assert_eq!(lines[0].spans[1].text, "▏ ");
        assert_eq!(lines[0].spans[1].role, SpanRole::Gutter);
        assert!(body_roles(&lines[0]).contains(&SpanRole::BlockQuote));
    }

    #[test]
    fn thematic_break_is_a_rule_line() {
        let lines = parse("---");
        assert!(lines[0].spans.iter().any(|s| s.role == SpanRole::Rule));
    }

    #[test]
    fn link_text_is_roled_and_the_url_trails() {
        let lines = parse("[Zed](https://zed.dev)");
        let roles = body_roles(&lines[0]);
        assert!(roles.contains(&SpanRole::Link));
        assert!(lines[0].spans.iter().any(|s| s.text.contains("https://zed.dev")));
    }

    #[test]
    fn fenced_code_is_plain_for_now() {
        let lines = parse("```\nhello\n```");
        assert!(lines.iter().any(|l| l.spans.iter().any(|s| s.role == SpanRole::CodePlain)));
    }

    #[test]
    fn soft_break_splits_paragraph_into_lines() {
        let lines = parse("a\nb");
        assert_eq!(lines.len(), 2);
    }
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p codypendent-tui markdown::tests::heading_becomes_one_line_of_heading_role`
Expected: FAIL — `parse` not found.

- [ ] **Step 4: Implement `parse` + `Builder`** — add to `crates/tui/src/markdown.rs` (above the `tests` module):

```rust
use pulldown_cmark::{
    Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};

/// Parse a finalized message's raw text into semantic `RichLine`s. Theme- and
/// width-independent; called exactly once per message on finalize (never per
/// frame). `pulldown-cmark` is total — malformed input degrades to best-effort
/// text, never a panic.
pub fn parse(text: &str) -> Vec<RichLine> {
    #[cfg(test)]
    PARSE_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let mut b = Builder::default();
    for ev in Parser::new_ext(text, opts) {
        b.event(ev);
    }
    b.finish()
}

/// Width of a thematic break's rule (cosmetic; the Paragraph does not stretch it).
const RULE_WIDTH: usize = 24;

#[derive(Default)]
struct Builder {
    lines: Vec<RichLine>,
    cur: Vec<RichSpan>,
    produced_line: bool, // false until the first line is pushed (gutter "▌ " vs "  ")
    heading: Option<u8>,
    strong: usize,
    emphasis: usize,
    blockquote: usize,
    in_link: bool,
    link_url: String,
    // Ordered-list ordinals (or `None` for a bullet list), innermost last.
    list_stack: Vec<Option<u64>>,
    // Fenced code: `Some(lang)` while inside a fence; `code` accumulates its body.
    code_lang: Option<String>,
    code: String,
    // Table state (populated by Task 5; the Table arms are stubs until then).
    table: Option<TableState>,
}

#[derive(Default)]
struct TableState {
    aligns: Vec<Alignment>,
    rows: Vec<Vec<Vec<RichSpan>>>, // rows[r][col] = cell spans
    row: Vec<Vec<RichSpan>>,
    cell: Vec<RichSpan>,
    in_cell: bool,
    head_rows: usize, // how many leading rows are header rows
}

impl Builder {
    /// The role inline text takes in the current context (heading/link/quote win
    /// over emphasis nesting).
    fn inline_role(&self) -> SpanRole {
        if let Some(n) = self.heading {
            return SpanRole::Heading(n);
        }
        if self.in_link {
            return SpanRole::Link;
        }
        if self.blockquote > 0 {
            return SpanRole::BlockQuote;
        }
        match (self.strong > 0, self.emphasis > 0) {
            (true, true) => SpanRole::StrongEmphasis,
            (true, false) => SpanRole::Strong,
            (false, true) => SpanRole::Emphasis,
            (false, false) => SpanRole::Body,
        }
    }

    /// Push accumulated inline spans as one logical line, prefixed with the left
    /// rail and (inside a quote) the "▏ " bar. Empty lines are dropped.
    fn flush(&mut self) {
        if self.cur.is_empty() {
            return;
        }
        let body: Vec<RichSpan> = std::mem::take(&mut self.cur);
        self.push_line(body);
    }

    fn push_line(&mut self, body: Vec<RichSpan>) {
        let gutter = if self.produced_line { "  " } else { "▌ " };
        self.produced_line = true;
        let mut spans = Vec::with_capacity(body.len() + 2);
        spans.push(RichSpan { text: gutter.to_string(), role: SpanRole::Gutter });
        if self.blockquote > 0 {
            spans.push(RichSpan { text: "▏ ".repeat(self.blockquote), role: SpanRole::Gutter });
        }
        spans.extend(body);
        self.lines.push(RichLine { spans });
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let role = self.inline_role();
        self.cur.push(RichSpan { text: text.to_string(), role });
    }

    fn event(&mut self, ev: Event) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                if self.code_lang.is_some() {
                    self.code.push_str(&t);
                } else if self.table.as_ref().is_some_and(|t| t.in_cell) {
                    let role = if self.table.as_ref().unwrap().head_rows
                        == self.table.as_ref().unwrap().rows.len()
                    {
                        SpanRole::TableHeader
                    } else {
                        SpanRole::TableCell
                    };
                    self.table.as_mut().unwrap().cell.push(RichSpan { text: t.to_string(), role });
                } else {
                    self.push_text(&t);
                }
            }
            Event::Code(c) => {
                self.cur.push(RichSpan { text: c.to_string(), role: SpanRole::InlineCode });
            }
            Event::SoftBreak | Event::HardBreak => self.flush(),
            Event::Rule => {
                self.push_line(vec![RichSpan {
                    text: "─".repeat(RULE_WIDTH),
                    role: SpanRole::Rule,
                }]);
            }
            _ => {} // Html, InlineHtml, FootnoteReference, TaskListMarker: ignored (out of scope)
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Heading { level, .. } => self.heading = Some(heading_num(level)),
            Tag::Emphasis => self.emphasis += 1,
            Tag::Strong => self.strong += 1,
            Tag::Strikethrough => {} // rendered as its inner text (styling out of scope)
            Tag::BlockQuote(_) => self.blockquote += 1,
            Tag::Link { dest_url, .. } => {
                self.in_link = true;
                self.link_url = dest_url.to_string();
            }
            Tag::List(start) => self.list_stack.push(start),
            Tag::Item => {
                let depth = self.list_stack.len().saturating_sub(1);
                let indent = "  ".repeat(depth);
                let marker = match self.list_stack.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{indent}{n}. ");
                        *n += 1;
                        m
                    }
                    _ => format!("{indent}• "),
                };
                self.cur.push(RichSpan { text: marker, role: SpanRole::ListMarker });
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                self.code_lang = Some(match kind {
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or("").to_string(),
                    CodeBlockKind::Indented => String::new(),
                });
                self.code.clear();
            }
            Tag::Table(aligns) => self.table = Some(TableState { aligns, ..Default::default() }),
            Tag::TableHead | Tag::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    t.row.clear();
                }
            }
            Tag::TableCell => {
                if let Some(t) = self.table.as_mut() {
                    t.cell.clear();
                    t.in_cell = true;
                }
            }
            _ => {} // Paragraph, Item's inner Paragraph, etc.: no state change
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.flush();
                self.heading = None;
            }
            TagEnd::Paragraph => self.flush(),
            TagEnd::Emphasis => self.emphasis = self.emphasis.saturating_sub(1),
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Strikethrough => {}
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.blockquote = self.blockquote.saturating_sub(1);
            }
            TagEnd::Link => {
                self.cur.push(RichSpan {
                    text: format!(" ({})", self.link_url),
                    role: SpanRole::Gutter, // dim (text.muted) — the trailing URL
                });
                self.in_link = false;
            }
            TagEnd::Item => self.flush(),
            TagEnd::List(_) => {
                self.list_stack.pop();
            }
            TagEnd::CodeBlock => {
                let lang = self.code_lang.take().unwrap_or_default();
                let code = std::mem::take(&mut self.code);
                self.emit_code(&lang, &code);
            }
            TagEnd::TableCell => {
                if let Some(t) = self.table.as_mut() {
                    t.in_cell = false;
                    let cell = std::mem::take(&mut t.cell);
                    t.row.push(cell);
                }
            }
            TagEnd::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    let row = std::mem::take(&mut t.row);
                    t.rows.push(row);
                    t.head_rows = t.rows.len();
                }
            }
            TagEnd::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    let row = std::mem::take(&mut t.row);
                    t.rows.push(row);
                }
            }
            TagEnd::Table => self.emit_table(),
            _ => {}
        }
    }

    /// Fenced code → plain lines for now (Task 3 overrides this with `highlight`).
    fn emit_code(&mut self, _lang: &str, code: &str) {
        for line in code.lines() {
            self.push_line(vec![RichSpan { text: line.to_string(), role: SpanRole::CodePlain }]);
        }
    }

    /// Table layout — stub until Task 5.
    fn emit_table(&mut self) {
        self.table = None;
    }

    fn finish(mut self) -> Vec<RichLine> {
        self.flush();
        self.lines
    }
}

fn heading_num(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p codypendent-tui markdown::`
Expected: all Task 2 parse tests PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/tui/Cargo.toml crates/tui/src/markdown.rs
git commit -m "feat(tui): markdown::parse — headings, emphasis, lists, quotes, links (pulldown-cmark)" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 3: Code highlight (`markdown::highlight`, synoptic)

**Files:**
- Modify: `Cargo.toml` (root — add `synoptic`)
- Modify: `crates/tui/Cargo.toml` (add `synoptic = { workspace = true }`)
- Modify: `crates/tui/src/markdown.rs` (add `highlight` + `map_kind` + `language_extension`; rewrite `Builder::emit_code`)
- Test: inline `#[cfg(test)]` in `crates/tui/src/markdown.rs`

**Interfaces:**
- Consumes: `SpanRole`, `SyntaxRole`, `RichLine`, `RichSpan` (Task 1); `Builder::emit_code` (Task 2).
- Produces: `pub fn highlight(lang: &str, src: &str) -> Vec<RichLine>` — gutter-less lines (`parse` re-wraps each via `push_line`). Unknown language ⇒ every span `CodePlain`.

- [ ] **Step 1: Add the dependency.** In root `Cargo.toml` `[workspace.dependencies]`, add:

```toml
# synoptic: pure-Rust, asset-free, per-language syntax highlighter (MIT). Its
# heavy deps (regex 1.x, unicode-width) are already in the tree; it emits token
# kinds we colour ourselves onto the semantic theme.syntax.* palette.
synoptic = "2.2"
```

In `crates/tui/Cargo.toml`, add:

```toml
synoptic = { workspace = true }
```

- [ ] **Step 2: Write the failing tests** — add to the `tests` module:

```rust
    fn code_roles(lines: &[RichLine]) -> Vec<SpanRole> {
        lines.iter().flat_map(|l| l.spans.iter().map(|s| s.role)).collect()
    }

    #[test]
    fn rust_fence_highlights_keyword_and_comment() {
        let out = highlight("rust", "fn main() {\n    let x = 5; // note\n}");
        assert_eq!(out.len(), 3, "one RichLine per source line");
        let roles = code_roles(&out);
        assert!(roles.contains(&SpanRole::CodeToken(SyntaxRole::Keyword)), "fn/let are keywords");
        assert!(roles.contains(&SpanRole::CodeToken(SyntaxRole::Comment)), "// note is a comment");
    }

    #[test]
    fn python_fence_highlights_string() {
        let out = highlight("python", "x = \"hello\"");
        let roles = code_roles(&out);
        assert!(roles.contains(&SpanRole::CodeToken(SyntaxRole::StringLit)));
    }

    #[test]
    fn unknown_language_is_all_plain() {
        let out = highlight("no-such-lang-xyz", "some text 123");
        assert!(out[0].spans.iter().all(|s| s.role == SpanRole::CodePlain));
    }

    #[test]
    fn fenced_code_in_parse_is_now_highlighted() {
        let lines = parse("```rust\nfn a() {}\n```");
        // parse re-wraps highlight's lines with the gutter, then a CodeToken(Keyword) shows.
        let roles: Vec<SpanRole> = lines.iter().flat_map(|l| l.spans.iter().map(|s| s.role)).collect();
        assert!(roles.contains(&SpanRole::CodeToken(SyntaxRole::Keyword)));
        assert_eq!(lines[0].spans[0].role, SpanRole::Gutter);
    }
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p codypendent-tui markdown::tests::rust_fence_highlights_keyword_and_comment`
Expected: FAIL — `highlight` not found.

- [ ] **Step 4: Implement `highlight` + swap `emit_code`.** Add to `crates/tui/src/markdown.rs`:

```rust
use synoptic::{from_extension, TokOpt};

/// Tab width synoptic uses when normalizing multi-line token state.
const HIGHLIGHT_TAB_WIDTH: usize = 4;

/// Highlight a fenced code block into gutter-less `RichLine`s (one per source
/// line). Maps synoptic's per-language token kinds onto the expanded
/// `theme.syntax.*` palette; an unknown/empty language → every span `CodePlain`
/// (still themed, still safe). Never panics.
pub fn highlight(lang: &str, src: &str) -> Vec<RichLine> {
    let lines: Vec<String> = src.lines().map(str::to_string).collect();
    let ext = language_extension(lang);
    if let Some(mut h) = from_extension(&ext, HIGHLIGHT_TAB_WIDTH) {
        h.run(&lines);
        lines
            .iter()
            .enumerate()
            .map(|(y, raw)| {
                let spans = h
                    .line(y, raw)
                    .into_iter()
                    .map(|tok| match tok {
                        TokOpt::Some(text, kind) => match map_kind(&kind) {
                            Some(role) => RichSpan { text, role: SpanRole::CodeToken(role) },
                            None => RichSpan { text, role: SpanRole::CodePlain },
                        },
                        TokOpt::None(text) => RichSpan { text, role: SpanRole::CodePlain },
                    })
                    .collect();
                RichLine { spans }
            })
            .collect()
    } else {
        lines
            .into_iter()
            .map(|raw| RichLine { spans: vec![RichSpan { text: raw, role: SpanRole::CodePlain }] })
            .collect()
    }
}

/// Map a fence's language tag to the file extension synoptic keys on. Common
/// word-forms are aliased; anything else is passed through (synoptic returns
/// `None` for an unknown extension → the all-plain path above).
fn language_extension(lang: &str) -> String {
    match lang.trim().to_ascii_lowercase().as_str() {
        "rust" | "rs" => "rs",
        "python" | "py" => "py",
        "javascript" | "js" | "node" => "js",
        "typescript" | "ts" => "ts",
        "json" => "json",
        "bash" | "sh" | "shell" | "zsh" => "sh",
        "go" | "golang" => "go",
        "toml" => "toml",
        "yaml" | "yml" => "yml",
        "sql" => "sql",
        "c" => "c",
        "cpp" | "c++" | "cxx" | "cc" | "hpp" => "cpp",
        "java" => "java",
        "html" => "html",
        "css" => "css",
        "markdown" | "md" => "md",
        other => other,
    }
    .to_string()
}

/// synoptic kind name → our `SyntaxRole`. `None` ⇒ render the token as `CodePlain`
/// (safe default for a kind this map does not recognize).
fn map_kind(kind: &str) -> Option<SyntaxRole> {
    Some(match kind {
        "keyword" => SyntaxRole::Keyword,
        "comment" => SyntaxRole::Comment,
        "string" | "character" => SyntaxRole::StringLit,
        "digit" | "number" | "float" => SyntaxRole::Literal,
        "boolean" | "reference" | "constant" => SyntaxRole::Constant,
        "type" | "struct" | "class" | "namespace" | "enum" => SyntaxRole::Type,
        "function" | "macros" | "macro" => SyntaxRole::Function,
        "operator" | "symbol" => SyntaxRole::Operator,
        "punctuation" => SyntaxRole::Punctuation,
        _ => return None,
    })
}
```

Then replace `Builder::emit_code` (from Task 2) with the highlighting version:

```rust
    /// Fenced code → per-language highlighted lines, re-wrapped with the gutter.
    fn emit_code(&mut self, lang: &str, code: &str) {
        for rl in highlight(lang, code) {
            self.push_line(rl.spans);
        }
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p codypendent-tui markdown::`
Expected: all Task 3 tests PASS (and Task 2's `fenced_code_is_plain_for_now` — a fence with no language still yields `CodePlain`, so it still holds).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/tui/Cargo.toml crates/tui/src/markdown.rs
git commit -m "feat(tui): markdown::highlight — per-language code tokens via synoptic" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 4: Expanded syntax palette + `surface.user` (all seven theme depths)

**Files:**
- Modify: `crates/tui/src/theme.rs` — `SyntaxTokens` (52-59), `SurfaceTokens` (13-24), and all seven constructors: `dark` (114), `light` (167), `high_contrast` (221), `color_blind_safe` (276), `ansi256` (331), `ansi16` (385), `monochrome` (439); extend the `monochrome_is_purely_grayscale` test (726)
- Modify: `crates/tui/src/theme_pack.rs` — `set_token` (144-178)
- Test: inline in `crates/tui/src/theme.rs` and `crates/tui/src/theme_pack.rs`

**Interfaces:**
- Produces: `theme.syntax.{r#type,function,operator,constant,punctuation}` and `theme.surface.user`, resolved in every depth; `set_token` names `"syntax.type" | "syntax.function" | "syntax.operator" | "syntax.constant" | "syntax.punctuation" | "surface.user"`.
- Consumes: nothing (Task 7's `style_for` and Task 8's user container consume these).

- [ ] **Step 1: Write the failing tests** — add to the `tests` module in `crates/tui/src/theme.rs`:

```rust
    /// Every depth must resolve every syntax slot to a colour visible on its panel,
    /// and light must differ from dark — the expanded palette is real everywhere.
    #[test]
    fn every_depth_resolves_every_syntax_slot() {
        for v in [
            ThemeVariant::Dark, ThemeVariant::Light, ThemeVariant::HighContrast,
            ThemeVariant::ColorBlindSafe, ThemeVariant::Ansi256, ThemeVariant::Ansi16,
            ThemeVariant::Monochrome,
        ] {
            let t = Theme::variant(v);
            for c in [
                t.syntax.keyword, t.syntax.literal, t.syntax.string, t.syntax.comment,
                t.syntax.r#type, t.syntax.function, t.syntax.operator, t.syntax.constant,
                t.syntax.punctuation,
            ] {
                assert_ne!(c, t.surface.panel, "{v:?}: a syntax slot is invisible on the panel");
            }
        }
        // A sensible light/dark distinction on the new slots.
        assert_ne!(Theme::dark().syntax.r#type, Theme::light().syntax.r#type);
        assert_ne!(Theme::dark().syntax.function, Theme::light().syntax.function);
    }

    /// The user container surface: distinct on the five raised-surface depths;
    /// deliberately == panel on ansi16/monochrome (the accent-bar fallback).
    #[test]
    fn surface_user_is_distinct_where_a_raised_surface_exists() {
        for v in [
            ThemeVariant::Dark, ThemeVariant::Light, ThemeVariant::HighContrast,
            ThemeVariant::ColorBlindSafe, ThemeVariant::Ansi256,
        ] {
            let t = Theme::variant(v);
            assert_ne!(t.surface.user, t.surface.panel, "{v:?}: user surface not distinct");
        }
        assert_eq!(Theme::ansi16().surface.user, Theme::ansi16().surface.panel);
        assert_eq!(Theme::monochrome().surface.user, Theme::monochrome().surface.panel);
    }
```

Extend the existing `monochrome_is_purely_grayscale` test's colour list (after `t.syntax.keyword,`) with the new slots:

```rust
            t.syntax.r#type,
            t.syntax.function,
            t.syntax.operator,
            t.syntax.constant,
            t.syntax.punctuation,
            t.surface.user,
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p codypendent-tui theme::tests::every_depth_resolves_every_syntax_slot`
Expected: FAIL — no field `r#type` on `SyntaxTokens`.

- [ ] **Step 3: Add the struct fields.** In `crates/tui/src/theme.rs`, extend `SyntaxTokens` (52-59):

```rust
pub struct SyntaxTokens {
    pub keyword: Color,
    pub literal: Color,
    pub string: Color,
    pub comment: Color,
    /// Type / struct / class / namespace names.
    pub r#type: Color,
    /// Function / method / macro names.
    pub function: Color,
    /// Operators (`+`, `=>`, `::`).
    pub operator: Color,
    /// Named constants / booleans / references.
    pub constant: Color,
    /// Brackets and separators.
    pub punctuation: Color,
}
```

Extend `SurfaceTokens` (13-24) with, after `overlay`:

```rust
    /// The background of the user's own message container (the `You` turn). A
    /// subtly-raised surface distinct from `panel`; == `panel` on depths with no
    /// distinct subtle surface (ansi16/monochrome), which fall back to an accent bar.
    pub user: Color,
```

- [ ] **Step 4: Set the new fields in all seven constructors.** Add these lines to each constructor's `syntax:` and `surface:` blocks (exact values per depth):

**`dark()`** — `syntax:` add `r#type: Color::Rgb(0x5c,0xc2,0xc0), function: Color::Rgb(0x6c,0xb0,0xf0), operator: Color::Rgb(0xc3,0xca,0xd6), constant: Color::Rgb(0xe8,0x8a,0x6a), punctuation: Color::Rgb(0x9a,0xa2,0xb1),`; `surface:` add `user: Color::Rgb(0x20,0x24,0x2c),`.

**`light()`** — `syntax:` add `r#type: Color::Rgb(0x0a,0x7a,0x78), function: Color::Rgb(0x08,0x4a,0xc0), operator: Color::Rgb(0x4a,0x52,0x5e), constant: Color::Rgb(0xb0,0x4a,0x00), punctuation: Color::Rgb(0x6b,0x73,0x82),`; `surface:` add `user: Color::Rgb(0xea,0xec,0xf1),`.

**`high_contrast()`** — `syntax:` add `r#type: Color::Rgb(0x00,0xff,0xd7), function: Color::Rgb(0x00,0xd7,0xff), operator: Color::Rgb(0xff,0xff,0xff), constant: Color::Rgb(0xff,0xa5,0x00), punctuation: Color::Rgb(0xe0,0xe0,0xe0),`; `surface:` add `user: Color::Rgb(0x1a,0x1a,0x1a),`.

**`color_blind_safe()`** — `syntax:` add `r#type: Color::Rgb(0x56,0xb4,0xe9), function: Color::Rgb(0x00,0x72,0xb2), operator: Color::Rgb(0xbc,0xc2,0xce), constant: Color::Rgb(0xd5,0x5e,0x00), punctuation: Color::Rgb(0x94,0x9c,0xac),`; `surface:` add `user: Color::Rgb(0x23,0x27,0x2f),`.

**`ansi256()`** — `syntax:` add `r#type: Color::Indexed(80), function: Color::Indexed(75), operator: Color::Indexed(249), constant: Color::Indexed(173), punctuation: Color::Indexed(245),`; `surface:` add `user: Color::Indexed(236),`.

**`ansi16()`** — `syntax:` add `r#type: Color::LightCyan, function: Color::LightBlue, operator: Color::Gray, constant: Color::Yellow, punctuation: Color::Gray,`; `surface:` add `user: Color::Black,` (== panel → accent-bar fallback).

**`monochrome()`** — `syntax:` add `r#type: Color::Gray, function: Color::Gray, operator: Color::DarkGray, constant: Color::Gray, punctuation: Color::DarkGray,`; `surface:` add `user: Color::Black,` (== panel → accent-bar fallback).

- [ ] **Step 5: Extend `set_token`.** In `crates/tui/src/theme_pack.rs`, add these arms after `"syntax.comment" => ...` (163) and `"surface.overlay" => ...` (149):

```rust
        "syntax.type" => theme.syntax.r#type = color,
        "syntax.function" => theme.syntax.function = color,
        "syntax.operator" => theme.syntax.operator = color,
        "syntax.constant" => theme.syntax.constant = color,
        "syntax.punctuation" => theme.syntax.punctuation = color,
        "surface.user" => theme.surface.user = color,
```

- [ ] **Step 6: Add a theme-pack test** — in `crates/tui/src/theme_pack.rs` tests:

```rust
    #[test]
    fn pack_can_override_new_syntax_and_user_tokens() {
        let toml = r##"
schema_version = 1
id = "expanded"
base = "dark"
[tokens]
"syntax.type" = "#00ffcc"
"surface.user" = "236"
"##;
        let theme = load_theme_pack(toml).expect("loads");
        assert_eq!(theme.syntax.r#type, Color::Rgb(0x00, 0xff, 0xcc));
        assert_eq!(theme.surface.user, Color::Indexed(236));
    }
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p codypendent-tui theme:: theme_pack::`
Expected: PASS — every depth resolves every slot; the pack overrides load; monochrome stays grayscale.

- [ ] **Step 8: Commit**

```bash
git add crates/tui/src/theme.rs crates/tui/src/theme_pack.rs
git commit -m "feat(tui): expand theme.syntax palette (+type/function/operator/constant/punctuation) and add surface.user across all seven depths" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 5: Table layout (`Builder::emit_table`)

**Files:**
- Modify: `crates/tui/src/markdown.rs` — implement `Builder::emit_table` + a `layout_table` helper (`MAX_TABLE_WIDTH`)
- Test: inline in `crates/tui/src/markdown.rs`

**Interfaces:**
- Consumes: `TableState` (collected by the Table arms in Task 2's `Builder`), `SpanRole::{TableHeader,TableCell,TableRule}`, `Alignment`.
- Produces: aligned table `RichLine`s (a header row, a rule row, one row per body line), spliced via `push_line`.

- [ ] **Step 1: Write the failing test** — add to the `tests` module:

```rust
    #[test]
    fn table_renders_aligned_header_rule_and_rows() {
        let md = "| a | bb |\n| :- | -: |\n| 1 | 2 |\n| 33 | 4 |";
        let lines = parse(md);
        // header + rule + 2 body rows (each a RichLine).
        assert!(lines.len() >= 4, "got {} lines", lines.len());
        // A header cell carries the TableHeader role.
        assert!(lines[0].spans.iter().any(|s| s.role == SpanRole::TableHeader));
        // The second line is a rule row (─ separators) with TableRule spans.
        assert!(lines[1].spans.iter().any(|s| s.role == SpanRole::TableRule));
        assert!(lines[1].spans.iter().any(|s| s.text.contains('─')));
        // Column widths are equal across the header and body rows (aligned).
        let widths = |l: &RichLine| -> usize {
            l.spans.iter().skip(1).map(|s| s.text.chars().count()).sum()
        };
        assert_eq!(widths(&lines[0]), widths(&lines[2]), "columns not aligned");
        assert!(lines[2].spans.iter().any(|s| s.role == SpanRole::TableCell));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p codypendent-tui markdown::tests::table_renders_aligned_header_rule_and_rows`
Expected: FAIL — `emit_table` is a stub, so no `TableRule`/aligned rows.

- [ ] **Step 3: Implement `emit_table` + `layout_table`.** In `crates/tui/src/markdown.rs`, add the const near `RULE_WIDTH`:

```rust
/// Cap a table's total content width so a pathological table cannot blow the pane.
const MAX_TABLE_WIDTH: usize = 100;
```

Replace `Builder::emit_table` (the Task 2 stub) with:

```rust
    fn emit_table(&mut self) {
        let Some(t) = self.table.take() else { return };
        for line in layout_table(&t) {
            self.push_line(line);
        }
    }
```

Add the free function (below `heading_num`):

```rust
/// Lay a collected table out into gutter-less `RichLine` bodies: a header row, a
/// "─┼─" rule row, then one row per body line. Column widths are the max cell
/// display width, capped so the total stays within `MAX_TABLE_WIDTH`; overlong
/// cells are truncated with a trailing "…".
fn layout_table(t: &TableState) -> Vec<Vec<RichSpan>> {
    let n_cols = t.rows.iter().map(Vec::len).max().unwrap_or(0);
    if n_cols == 0 {
        return Vec::new();
    }
    let cell_text = |cell: &[RichSpan]| -> String { cell.iter().map(|s| s.text.as_str()).collect() };
    let cell_width = |cell: &[RichSpan]| -> usize { cell_text(cell).chars().count() };

    // Column widths, capped so n_cols columns + " │ " joins fit MAX_TABLE_WIDTH.
    let cap = (MAX_TABLE_WIDTH / n_cols).max(3);
    let mut widths = vec![0usize; n_cols];
    for row in &t.rows {
        for (c, cell) in row.iter().enumerate() {
            widths[c] = widths[c].max(cell_width(cell).min(cap));
        }
    }

    let pad_cell = |cell: &[RichSpan], w: usize, align: Alignment, role: SpanRole| -> RichSpan {
        let mut text = cell_text(cell);
        let len = text.chars().count();
        if len > w {
            text = text.chars().take(w.saturating_sub(1)).collect::<String>() + "…";
        } else {
            let fill = w - len;
            match align {
                Alignment::Right => text = " ".repeat(fill) + &text,
                Alignment::Center => {
                    let l = fill / 2;
                    text = " ".repeat(l) + &text + &" ".repeat(fill - l);
                }
                _ => text = text + &" ".repeat(fill), // None/Left
            }
        }
        RichSpan { text, role }
    };

    let align_of = |c: usize| -> Alignment { t.aligns.get(c).copied().unwrap_or(Alignment::None) };
    let empty: Vec<RichSpan> = Vec::new();
    let mut out: Vec<Vec<RichSpan>> = Vec::with_capacity(t.rows.len() + 1);

    for (r, row) in t.rows.iter().enumerate() {
        let is_header = r < t.head_rows;
        let role = if is_header { SpanRole::TableHeader } else { SpanRole::TableCell };
        let mut spans: Vec<RichSpan> = Vec::with_capacity(n_cols * 2);
        for c in 0..n_cols {
            if c > 0 {
                spans.push(RichSpan { text: " │ ".to_string(), role: SpanRole::TableRule });
            }
            let cell = row.get(c).unwrap_or(&empty);
            spans.push(pad_cell(cell, widths[c], align_of(c), role));
        }
        out.push(spans);
        // Emit the "─┼─" rule directly after the header block.
        if is_header && r + 1 == t.head_rows {
            let mut rule: Vec<RichSpan> = Vec::with_capacity(n_cols * 2);
            for c in 0..n_cols {
                if c > 0 {
                    rule.push(RichSpan { text: "─┼─".to_string(), role: SpanRole::TableRule });
                }
                rule.push(RichSpan { text: "─".repeat(widths[c]), role: SpanRole::TableRule });
            }
            out.push(rule);
        }
    }
    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p codypendent-tui markdown::`
Expected: PASS — the table renders an aligned header, a `TableRule` row, and aligned body rows.

- [ ] **Step 5: Commit**

```bash
git add crates/tui/src/markdown.rs
git commit -m "feat(tui): markdown table layout — aligned, width-capped, truncating" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 6: Finalize hook + reduce call site (`finalize_streamed_models`)

**Files:**
- Modify: `crates/tui/src/reduce.rs:26` (the `DaemonEvent` arm) + add `finalize_streamed_models` + `RICH_MARKDOWN_MAX_BYTES`
- Test: inline `#[cfg(test)]` in `crates/tui/src/reduce.rs`

**Interfaces:**
- Consumes: `AppState`, `RunActivity`, `TranscriptEntry::Model { text, rendered }`, `markdown::parse`.
- Produces: `pub(crate) fn finalize_streamed_models(state: &mut AppState)` — idempotent; parses every non-streaming-tail `Model` with `rendered.is_none()` exactly once (skipping messages over `RICH_MARKDOWN_MAX_BYTES`).

- [ ] **Step 1: Write the failing tests** — add to the `tests` module in `crates/tui/src/reduce.rs` (reuse the file's existing `system_ev`/`ev`/`agent_actor` helpers and `RunId`):

```rust
    #[test]
    fn finalize_leaves_streaming_tail_plain_then_snaps_on_stop() {
        use crate::state::TranscriptEntry;
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(&mut s, system_ev(EventBody::RunStarted {
            run_id, objective: "go".to_owned(), mode: AgentMode::Build,
        }));
        reduce(&mut s, system_ev(EventBody::ModelStreamDelta {
            run_id, text: "# Title\n**bold**".to_owned(),
        }));
        // Still streaming ⇒ the tail Model stays plain (rendered None).
        let model = s.runs[0].transcript.iter().rev()
            .find(|e| matches!(e, TranscriptEntry::Model { .. })).unwrap();
        assert!(matches!(model, TranscriptEntry::Model { rendered: None, .. }));

        // Stream ends (activity leaves Streaming) ⇒ finalize parses it once.
        reduce(&mut s, system_ev(EventBody::RunStateChanged {
            run_id, state: RunState::Completed,
        }));
        let model = s.runs[0].transcript.iter().rev()
            .find(|e| matches!(e, TranscriptEntry::Model { .. })).unwrap();
        match model {
            TranscriptEntry::Model { rendered: Some(lines), .. } => assert!(!lines.is_empty()),
            other => panic!("expected finalized Model, got {other:?}"),
        }
    }

    #[test]
    fn finalize_is_idempotent() {
        use crate::markdown::PARSE_CALLS;
        use std::sync::atomic::Ordering;
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(&mut s, system_ev(EventBody::RunStarted {
            run_id, objective: "go".to_owned(), mode: AgentMode::Build,
        }));
        reduce(&mut s, system_ev(EventBody::ModelStreamDelta {
            run_id, text: "hello".to_owned(),
        }));
        reduce(&mut s, system_ev(EventBody::RunStateChanged {
            run_id, state: RunState::Completed,
        }));
        PARSE_CALLS.store(0, Ordering::Relaxed);
        // Further events run the sweep again; the finalized entry is not re-parsed.
        reduce(&mut s, system_ev(EventBody::RunStateChanged {
            run_id, state: RunState::Completed,
        }));
        assert_eq!(PARSE_CALLS.load(Ordering::Relaxed), 0, "already-cached entry re-parsed");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p codypendent-tui reduce::tests::finalize_leaves_streaming_tail_plain_then_snaps_on_stop`
Expected: FAIL — the tail is never finalized (no sweep wired).

- [ ] **Step 3: Wire the reduce call site.** In `crates/tui/src/reduce.rs`, change the `DaemonEvent` arm (line 26):

```rust
        Action::DaemonEvent(event) => {
            apply_event(state, *event);
            finalize_streamed_models(state);
        }
```

- [ ] **Step 4: Implement the sweep.** Add near the top of `crates/tui/src/reduce.rs` (after the `use` block), plus the const:

```rust
/// Above this size a message stays on the fast plain path (its single parse
/// would be too costly). 64 KiB — a quarter of `MAX_MODEL_ENTRY_BYTES`.
const RICH_MARKDOWN_MAX_BYTES: usize = 64 * 1024;

/// Parse every finalized (non-streaming-tail) `Model` entry into its rich cache
/// exactly once. Runs at the tail of every folded `DaemonEvent`, so it catches
/// all stream-ending transitions without enumerating them. Idempotent (skips any
/// entry already `Some`); bounded (O(total Model entries) cheap `is_none` checks).
pub(crate) fn finalize_streamed_models(state: &mut AppState) {
    let last_run = state.runs.len().checked_sub(1);
    for (idx, run) in state.runs.iter_mut().enumerate() {
        // The live streaming tail (only possible in the last run) is skipped.
        let tail = if Some(idx) == last_run && run.activity == RunActivity::Streaming {
            run.transcript.len().checked_sub(1)
        } else {
            None
        };
        for (i, entry) in run.transcript.iter_mut().enumerate() {
            if Some(i) == tail {
                continue;
            }
            if let TranscriptEntry::Model { text, rendered } = entry {
                if rendered.is_none() && text.len() <= RICH_MARKDOWN_MAX_BYTES {
                    *rendered = Some(crate::markdown::parse(text));
                }
            }
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p codypendent-tui reduce::`
Expected: PASS — the tail stays plain, snaps to `Some` on stop, and re-sweeps do not re-parse. (Existing reducer tests still pass — `finalize` only fills the cache.)

- [ ] **Step 6: Commit**

```bash
git add crates/tui/src/reduce.rs
git commit -m "feat(tui): finalize_streamed_models — parse-once rich cache on stream end" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 7: Render integration (`RowKind::Rich` + `style_for`)

**Files:**
- Modify: `crates/tui/src/render.rs` — `RowKind` (263), `Row::rich` (near 282), `columns` (294), `into_line` (308), `for_each_row` Model arm (383-395); add `style_for`
- Test: inline `#[cfg(test)]` in `crates/tui/src/render.rs`

**Interfaces:**
- Consumes: `RichLine`, `RichSpan`, `SpanRole`, `SyntaxRole` (Task 1); the expanded `theme.syntax.*` (Task 4); `Model.rendered` (Task 1/6).
- Produces: `enum RowKind::Rich(&'a RichLine)`, `Row::rich`, `fn style_for(role: SpanRole, theme: &Theme) -> Style`. MEASURE (`columns`) allocation-free; BUILD (`into_line`) O(viewport).

- [ ] **Step 1: Write the failing tests** — add to the `tests` module in `crates/tui/src/render.rs` (reuse `system_ev`, `RunId`, `transcript_rows`, `build_transcript_window`):

```rust
    /// Drive a run to a finalized rich Model; return the mutated state.
    fn finalized_model_state(markdown: &str) -> AppState {
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(&mut s, system_ev(EventBody::RunStarted {
            run_id, objective: "go".to_owned(), mode: AgentMode::Build,
        }));
        reduce(&mut s, system_ev(EventBody::ModelStreamDelta {
            run_id, text: markdown.to_owned(),
        }));
        reduce(&mut s, system_ev(EventBody::RunStateChanged {
            run_id, state: RunState::Completed,
        }));
        s
    }

    #[test]
    fn finalized_model_renders_styled_heading() {
        let s = finalized_model_state("# Heading");
        let theme = Theme::dark();
        let (lines, _r, _h) = build_transcript_window(&s.runs, &theme, 78, 0, 40, 0);
        // A heading span is bold and coloured text.heading.
        let styled = lines.iter().flat_map(|l| l.spans.iter()).any(|sp| {
            sp.style.fg == Some(theme.text.heading)
                && sp.style.add_modifier.contains(Modifier::BOLD)
        });
        assert!(styled, "the finalized heading is not styled from the theme");
    }

    #[test]
    fn keyword_span_maps_to_syntax_keyword() {
        let s = finalized_model_state("```rust\nfn a() {}\n```");
        let theme = Theme::dark();
        let (lines, _r, _h) = build_transcript_window(&s.runs, &theme, 78, 0, 40, 0);
        let has_kw = lines.iter().flat_map(|l| l.spans.iter())
            .any(|sp| sp.style.fg == Some(theme.syntax.keyword));
        assert!(has_kw, "no span coloured syntax.keyword");
    }

    #[test]
    fn rich_message_build_materializes_only_the_viewport() {
        // A large FINALIZED rich message — the crash-path invariant with rich rows.
        let mut big = String::new();
        for i in 0..4000 {
            big.push_str(&format!("- item {i}\n"));
        }
        let s = finalized_model_state(&big);
        // It really is finalized (rendered Some), so the rich path is exercised.
        assert!(s.runs[0].transcript.iter().any(|e|
            matches!(e, TranscriptEntry::Model { rendered: Some(_), .. })));

        let theme = Theme::dark();
        let (inner_width, height) = (78u16, 20u16);
        let total = transcript_rows(&s.runs, &theme, inner_width);
        assert!(total >= 4000, "measure sees the whole rich history: {total}");
        let (lines, _r, _h) = build_transcript_window(
            &s.runs, &theme, inner_width, total.saturating_sub(height), height, 0,
        );
        assert!(
            lines.len() <= height as usize + 4,
            "build materializes O(viewport), not O(history): {}", lines.len()
        );
    }

    #[test]
    fn theme_change_re_renders_without_re_parsing() {
        use crate::markdown::PARSE_CALLS;
        use std::sync::atomic::Ordering;
        let s = finalized_model_state("# H");
        PARSE_CALLS.store(0, Ordering::Relaxed);
        let (dark, _r, _h) = build_transcript_window(&s.runs, &Theme::dark(), 78, 0, 40, 0);
        let (light, _r, _h) = build_transcript_window(&s.runs, &Theme::light(), 78, 0, 40, 0);
        assert_eq!(PARSE_CALLS.load(Ordering::Relaxed), 0, "build re-parsed — cache not used");
        let dfg = dark.iter().flat_map(|l| l.spans.iter()).find_map(|s| s.style.fg);
        let lfg = light.iter().flat_map(|l| l.spans.iter()).find_map(|s| s.style.fg);
        assert_ne!(dfg, lfg, "theme change produced no colour change");
    }

    #[test]
    fn streaming_model_still_renders_plain() {
        // No RunStateChanged: the tail is still Streaming ⇒ plain path (rendered None).
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(&mut s, system_ev(EventBody::RunStarted {
            run_id, objective: "go".to_owned(), mode: AgentMode::Build,
        }));
        reduce(&mut s, system_ev(EventBody::ModelStreamDelta {
            run_id, text: "# still going".to_owned(),
        }));
        let out = render_to_string(&s, 80, 20);
        assert!(out.contains("# still going"), "streaming text should render as-is (plain):\n{out}");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p codypendent-tui render::tests::finalized_model_renders_styled_heading`
Expected: FAIL — `RowKind::Rich`/`style_for` do not exist; the finalized Model renders plain markdown.

- [ ] **Step 3: Add the `Rich` row kind + constructor.** In `crates/tui/src/render.rs`, extend `RowKind` (263-273):

```rust
enum RowKind<'a> {
    Built(Line<'a>),
    Model {
        prefix: &'static str,
        text: &'a str,
        caret: bool,
        style: Style,
    },
    /// A cached, finalized rich line — borrowed so MEASURE allocates nothing.
    Rich(&'a crate::markdown::RichLine),
}
```

Add the constructor in `impl<'a> Row<'a>` (after `fn model`, ~292):

```rust
    fn rich(rl: &'a crate::markdown::RichLine) -> Self {
        Row {
            kind: RowKind::Rich(rl),
            hit_entry: None,
        }
    }
```

- [ ] **Step 4: Extend `columns` (alloc-free) and `into_line` (O(viewport)).** In `columns` (294-304) add:

```rust
            RowKind::Rich(rl) => rl.spans.iter().map(|s| Span::raw(s.text.as_str()).width()).sum(),
```

In `into_line` (308-327) add:

```rust
            RowKind::Rich(rl) => Line::from(
                rl.spans
                    .iter()
                    .map(|s| Span::styled(s.text.clone(), style_for(s.role, theme)))
                    .collect::<Vec<_>>(),
            ),
```

- [ ] **Step 5: Add `style_for`.** Add near `into_line` in `crates/tui/src/render.rs` (imports `SpanRole`, `SyntaxRole` from `crate::markdown`):

```rust
use crate::markdown::{SpanRole, SyntaxRole};

/// Map a semantic `SpanRole` to a concrete `Style` from the live theme. Every
/// colour is a theme token — correct in all seven depths; a theme change simply
/// yields new colours on the next frame (no cache invalidation).
fn style_for(role: SpanRole, theme: &Theme) -> Style {
    let base = Style::default();
    match role {
        SpanRole::Gutter => base.fg(theme.text.muted),
        SpanRole::Body => base.fg(theme.agent.model_text),
        SpanRole::Heading(1..=2) => base
            .fg(theme.text.heading)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        SpanRole::Heading(_) => base.fg(theme.text.heading).add_modifier(Modifier::BOLD),
        SpanRole::Strong => base.fg(theme.text.primary).add_modifier(Modifier::BOLD),
        SpanRole::Emphasis => base.fg(theme.agent.model_text).add_modifier(Modifier::ITALIC),
        SpanRole::StrongEmphasis => base
            .fg(theme.text.primary)
            .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        SpanRole::InlineCode => base.fg(theme.syntax.string),
        SpanRole::Link => base.fg(theme.focus.active).add_modifier(Modifier::UNDERLINED),
        SpanRole::ListMarker => base.fg(theme.agent.tool),
        SpanRole::BlockQuote => base.fg(theme.text.secondary).add_modifier(Modifier::ITALIC),
        SpanRole::Rule => base.fg(theme.text.muted),
        SpanRole::TableHeader => base.fg(theme.text.heading).add_modifier(Modifier::BOLD),
        SpanRole::TableCell => base.fg(theme.agent.model_text),
        SpanRole::TableRule => base.fg(theme.surface.border),
        SpanRole::CodePlain => base.fg(theme.text.primary),
        SpanRole::CodeToken(SyntaxRole::Keyword) => base.fg(theme.syntax.keyword),
        SpanRole::CodeToken(SyntaxRole::Literal) => base.fg(theme.syntax.literal),
        SpanRole::CodeToken(SyntaxRole::StringLit) => base.fg(theme.syntax.string),
        SpanRole::CodeToken(SyntaxRole::Comment) => base.fg(theme.syntax.comment),
        SpanRole::CodeToken(SyntaxRole::Type) => base.fg(theme.syntax.r#type),
        SpanRole::CodeToken(SyntaxRole::Function) => base.fg(theme.syntax.function),
        SpanRole::CodeToken(SyntaxRole::Operator) => base.fg(theme.syntax.operator),
        SpanRole::CodeToken(SyntaxRole::Constant) => base.fg(theme.syntax.constant),
        SpanRole::CodeToken(SyntaxRole::Punctuation) => base.fg(theme.syntax.punctuation),
    }
}
```

- [ ] **Step 6: Branch `for_each_row`'s Model arm.** Replace the Model arm (`crates/tui/src/render.rs:383-395`) with:

```rust
                TranscriptEntry::Model { text, rendered } => {
                    match rendered {
                        // RICH: finalized and not the live tail → borrow cached lines.
                        Some(lines) if !streaming_tail => {
                            for rl in lines {
                                visit(Row::rich(rl));
                                produced = true;
                            }
                        }
                        // PLAIN: streaming tail, or not yet finalized (belt-and-braces).
                        _ => {
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
                    }
                }
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p codypendent-tui render::`
Expected: PASS — styled heading, keyword→syntax.keyword, O(viewport) build with a large rich message, theme change without re-parse, streaming stays plain. (The original `build_transcript_window_materializes_only_the_viewport` still passes — a streaming Model is plain.)

- [ ] **Step 8: Commit**

```bash
git add crates/tui/src/render.rs
git commit -m "feat(tui): RowKind::Rich + style_for — finalized rich rows, virtualization preserved" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 8: User-message container (`Row.bg`)

**Files:**
- Modify: `crates/tui/src/render.rs` — `Row` (256-261) add `bg`; `built`/`model`/`rich` constructors add `bg: None`; `for_each_row` `other =>` arm (396-412) tag user rows; `build_transcript_window` (453-486) apply bg/pad/accent
- Test: inline `#[cfg(test)]` in `crates/tui/src/render.rs`

**Interfaces:**
- Consumes: `theme.surface.user` (Task 4); `theme.focus.active`; `theme.surface.panel` (equality → accent fallback).
- Produces: `Row.bg: Option<Color>` (cosmetic — `columns`/`rows` ignore it). Full-width user block on raised-surface depths; a leading `focus.active` "▎" accent bar where `surface.user == surface.panel`.

- [ ] **Step 1: Write the failing tests** — add to the `tests` module in `crates/tui/src/render.rs`:

```rust
    fn user_turn_state() -> AppState {
        let mut s = AppState::new();
        let run_id = RunId::new();
        // RunStarted pushes the objective as a `User` transcript entry.
        reduce(&mut s, system_ev(EventBody::RunStarted {
            run_id, objective: "my question".to_owned(), mode: AgentMode::Build,
        }));
        s
    }

    #[test]
    fn user_rows_carry_the_container_bg_and_fill_width() {
        let s = user_turn_state();
        let theme = Theme::dark();
        let inner_width = 40u16;
        let (lines, _r, _h) = build_transcript_window(&s.runs, &theme, inner_width, 0, 40, 0);
        let user_line = lines.iter()
            .find(|l| l.spans.iter().any(|sp| sp.content.contains("my question")))
            .expect("user body line present");
        assert_eq!(user_line.style.bg, Some(theme.surface.user), "no container bg");
        assert_eq!(user_line.width(), inner_width as usize, "not padded to full width");
    }

    #[test]
    fn ansi16_user_row_uses_an_accent_bar_not_a_bg() {
        let s = user_turn_state();
        let theme = Theme::ansi16(); // surface.user == surface.panel here
        let (lines, _r, _h) = build_transcript_window(&s.runs, &theme, 40, 0, 40, 0);
        let user_line = lines.iter()
            .find(|l| l.spans.iter().any(|sp| sp.content.contains("my question")))
            .expect("user body line present");
        assert_eq!(user_line.spans[0].content, "▎", "no accent bar");
        assert_eq!(user_line.spans[0].style.fg, Some(theme.focus.active));
        assert_ne!(user_line.style.bg, Some(theme.surface.user), "should not bg-fill on ansi16");
    }

    #[test]
    fn user_container_does_not_break_virtualization() {
        let mut s = user_turn_state();
        // Add a long agent reply so the window must virtualize.
        let run_id = s.runs[0].run_id;
        let mut big = String::new();
        for i in 0..3000 { big.push_str(&format!("line {i}\n")); }
        reduce(&mut s, system_ev(EventBody::ModelStreamDelta { run_id, text: big }));
        let theme = Theme::dark();
        let (lines, _r, _h) = build_transcript_window(&s.runs, &theme, 78, 100, 20, 0);
        assert!(lines.len() <= 24, "build still O(viewport): {}", lines.len());
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p codypendent-tui render::tests::user_rows_carry_the_container_bg_and_fill_width`
Expected: FAIL — no `bg` field; user rows are un-contained.

- [ ] **Step 3: Add the `bg` field + default in all constructors.** In `crates/tui/src/render.rs`, extend `Row` (256-261):

```rust
struct Row<'a> {
    kind: RowKind<'a>,
    hit_entry: Option<usize>,
    /// A full-width background for this row (the `You` container). Cosmetic —
    /// `columns()`/`rows()` ignore it; applied only to visible rows at build.
    bg: Option<Color>,
}
```

Add `bg: None,` to each of the three constructors (`built`, `model`, `rich`).

- [ ] **Step 4: Tag user rows.** In `for_each_row`'s `other =>` arm (396-412), tag every row a `User` entry produces:

```rust
                other => {
                    scratch.clear();
                    entry_lines(other, theme, false, false, &mut scratch);
                    let hit = if run_idx == selected_run {
                        fold_hit_entry(other, idx)
                    } else {
                        None
                    };
                    let is_user = matches!(other, TranscriptEntry::User { .. });
                    for (j, line) in scratch.drain(..).enumerate() {
                        let mut row = Row::built(line);
                        if j == 0 {
                            row.hit_entry = hit;
                        }
                        if is_user {
                            row.bg = Some(theme.surface.user);
                        }
                        visit(row);
                        produced = true;
                    }
                }
```

- [ ] **Step 5: Apply bg/pad/accent at build.** In `build_transcript_window` (453-486), replace the visible-row block (the `if row_end > first_row && row_start < last_row { .. }`) with one that captures `bg` before `into_line` and applies it:

```rust
        if row_end > first_row && row_start < last_row {
            if !first_seen {
                scroll = first_row.saturating_sub(row_start);
                first_seen = true;
            }
            let hit = row.hit_entry;
            let bg = row.bg;
            let index = out.len();
            let mut line = row.into_line(theme);
            if let Some(c) = bg {
                if c == theme.surface.panel {
                    // No distinct raised surface (ansi16/monochrome): a leading accent bar.
                    line.spans.insert(
                        0,
                        Span::styled("▎", Style::default().fg(theme.focus.active)),
                    );
                } else {
                    line.style = line.style.bg(c);
                    let pad = (inner_width as usize).saturating_sub(line.width());
                    if pad > 0 {
                        line.spans.push(Span::styled(" ".repeat(pad), Style::default().bg(c)));
                    }
                }
            }
            out.push(line);
            if let Some(entry) = hit {
                hits.push((index, entry));
            }
        }
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p codypendent-tui render::`
Expected: PASS — user rows carry `surface.user` and fill width; ansi16 uses the accent bar; virtualization is unaffected. (Blank separator lines between turns stay `panel` — untagged.)

- [ ] **Step 7: Commit**

```bash
git add crates/tui/src/render.rs
git commit -m "feat(tui): user-message container — full-width surface.user bg, ansi16 accent fallback" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 9: Hygiene — deny-clean, client-only, final integration

**Files:**
- Modify (only if a license needs it): `deny.toml`
- Test: `crates/tui/src/render.rs` (one end-to-end integration test)

**Interfaces:**
- Consumes: everything above.
- Produces: a green `cargo deny check`, proof of no protocol/golden diff, and a full-pipeline test.

- [ ] **Step 1: Write the failing integration test** — add to the `tests` module in `crates/tui/src/render.rs`:

```rust
    #[test]
    fn full_markdown_message_snaps_to_rich_end_to_end() {
        let md = "# Report\n\nSome **bold** and `code`.\n\n- one\n- two\n\n\
                  ```rust\nfn main() { let x = 1; }\n```\n\n> a quote";
        let mut s = AppState::new();
        let run_id = RunId::new();
        reduce(&mut s, system_ev(EventBody::RunStarted {
            run_id, objective: "please report".to_owned(), mode: AgentMode::Build,
        }));
        reduce(&mut s, system_ev(EventBody::ModelStreamDelta {
            run_id, text: md.to_owned(),
        }));
        // While streaming: raw markdown is visible (plain path).
        assert!(render_to_string(&s, 80, 30).contains("# Report"));
        // Finalize.
        reduce(&mut s, system_ev(EventBody::RunStateChanged {
            run_id, state: RunState::Completed,
        }));
        let out = render_to_string(&s, 80, 30);
        // The literal "# " heading marker is gone (rendered as a styled heading).
        assert!(out.contains("Report"));
        assert!(!out.contains("# Report"), "heading markup should be consumed:\n{out}");
        // The user's own turn and the agent reply both rendered.
        assert!(out.contains("please report"));
    }
```

- [ ] **Step 2: Run to verify it passes** (the feature is already built)

Run: `cargo test -p codypendent-tui render::tests::full_markdown_message_snaps_to_rich_end_to_end`
Expected: PASS. If `# Report` still shows post-finalize, the finalize→rich wiring regressed — fix before continuing.

- [ ] **Step 3: Whole-crate + workspace tests**

Run: `cargo test -p codypendent-tui && cargo build --workspace`
Expected: PASS / clean build.

- [ ] **Step 4: Clippy (matches the workspace lint gate)**

Run: `cargo clippy -p codypendent-tui --all-targets -- -D warnings`
Expected: no warnings. (Note the crate uses `theme.syntax.r#type` — a raw identifier — which is fine.)

- [ ] **Step 5: cargo-deny — licenses/bans/sources.** Both new crates and their new transitive deps are permissive:
  - `pulldown-cmark` (MIT), `pulldown-cmark-escape` (MIT), `unicase` (MIT/Apache-2.0).
  - `synoptic` (MIT); its new transitives `char_index`, `if_chain`, `nohash-hasher` (all MIT/Apache-family). `regex` + `unicode-width` were already in-tree.

Run: `cargo deny check bans licenses sources`
Expected: PASS. If a transitive crate (most likely `char_index`) reports a license the allow-list lacks, add a minimal, dated `[licenses.exceptions]` entry in `deny.toml` for that exact crate — do NOT widen the global `allow` list.

- [ ] **Step 6: Prove client-only (no protocol/daemon/wire/golden change)**

Run: `git diff --stat main -- crates/protocol crates/daemon crates/codypendentd | cat`
Expected: **empty** (no output). Then confirm the golden vectors are untouched:

Run: `git diff --stat main -- crates/protocol/tests/golden_vectors.rs | cat`
Expected: **empty**.

- [ ] **Step 7: Commit** (only if `deny.toml` changed; otherwise this task added just the test in Step 1, already coverable)

```bash
git add crates/tui/src/render.rs deny.toml
git commit -m "test(tui): end-to-end rich-formatting integration; confirm deny-clean + client-only" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

(If `deny.toml` was not modified, drop it from the `git add`.)

---

## Self-Review (run against the spec + amendment)

**Spec coverage:**
- Two-stage semantic-role cache (§Architecture) → Tasks 1 (types), 6 (parse-once finalize), 7 (build-time style). ✅
- `RichLine`/`RichSpan`/`SpanRole`/`SyntaxRole` (§Data model) → Task 1. ✅
- `markdown::parse` C1 (headings/emphasis/inline code/lists/quote/rule/link/gutter) → Task 2. ✅
- Code highlight C2 — **amendment**: synoptic, not the in-crate tokenizer → Task 3. ✅
- Table layout C3 (`MAX_TABLE_WIDTH`, truncation, alignment) → Task 5. ✅
- Finalize hook C4 (`finalize_streamed_models`, streaming-tail predicate, `RICH_MARKDOWN_MAX_BYTES`, reduce call site) → Task 6. ✅
- Render integration C5 (`RowKind::Rich`, alloc-free `columns`, O(viewport) `into_line`, plain/rich branch) → Task 7. ✅
- Style map C6 (`style_for`, every colour a theme token) → Task 7. ✅
- User container C7 (`surface.user`, `Row.bg`, pad-to-width, ansi16 accent) → Tasks 4 + 8. ✅
- **Amendment**: expanded syntax palette across every depth + `theme_pack` → Task 4 (all seven constructors). ✅
- Virtualization preserved + parse-once + theme-change-no-reparse tests → Tasks 6, 7. ✅
- Deps deny-clean → Tasks 2, 3, 9. ✅

**Placeholder scan:** No "TBD"/"handle errors"/"similar to Task N". The Task 2 `Builder` ships with Table arms wired but `emit_table`/`layout_table` stubbed-then-filled in Task 5, and `emit_code` plain-then-highlighted in Task 3 — each an explicit, compiling, tested intermediate, not a placeholder. Every code step shows complete code.

**Type consistency:** `RichLine`/`RichSpan`/`SpanRole`/`SyntaxRole` identical across Tasks 1/2/3/5/7. `parse`/`highlight`/`finalize_streamed_models`/`style_for`/`Row::rich`/`RowKind::Rich` signatures match their Interfaces-block declarations. `Model { text, rendered }` shape consistent across state/reduce/render. Theme field `r#type` used identically in Task 4 (define/set) and Task 7 (`style_for`). `surface.user` set in Task 4, consumed in Task 8; the `== surface.panel` accent-fallback contract is defined in Task 4's test and honoured in Task 8's build code.

**Open ambiguities (flagged, with the choice taken):**
1. **synoptic kind-name vocabulary** — synoptic 2.2.9's exact per-language kind strings were not fully enumerable from docs; `map_kind` recognizes the conventional set and treats any unrecognized kind as `CodePlain` (still themed, safe). Task 3's assertions rely only on the two highest-confidence kinds (`keyword`, `comment`, plus `string` for Python), so an unexpected kind string degrades gracefully without failing the suite.
2. **Trailing link URL** — rendered with `SpanRole::Gutter` (dim `text.muted`) to match the spec's "dim" intent, since the fixed role set has no dedicated muted-body role.
3. **ansi16 accent bar width** — prepending "▎" adds one column that MEASURE did not count; for a user line exactly `inner_width` wide this could add one wrapped row (cosmetic, ansi16/monochrome only, non-crashing). The bg-fill path pads to exactly `inner_width` and never drifts.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-27-rich-formatting.md`. Two execution options:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — execute tasks in this session with checkpoints for review.

Which approach?
