//! Rich-text data model + markdown parse/highlight for the finalized agent
//! message (client-only; see docs/superpowers/plans/2026-07-27-rich-formatting.md).
//! Types are semantic (a `SpanRole`, never a concrete `Color`) so the cache is
//! theme-independent; styling happens at build time in `render.rs`. Width is
//! the one thing it cannot defer: a table's columns are padded into the span
//! text, so [`parse`] takes the pane width and the cache is keyed on it.

/// One rendered logical line: an owned, theme-independent span list.
#[derive(Debug, Clone, PartialEq)]
pub struct RichLine {
    pub spans: Vec<RichSpan>,
    pub links: Vec<LinkAnnotation>,
}

impl RichLine {
    #[must_use]
    pub fn new(spans: Vec<RichSpan>) -> Self {
        Self {
            spans,
            links: Vec::new(),
        }
    }
}

/// Byte range into the concatenated span text of this line (Adoption 11 M5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkAnnotation {
    pub range: std::ops::Range<usize>,
    pub destination: String,
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

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use synoptic::{from_extension, TokOpt};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

// Test-only, per-test-thread instrumentation. A thread-local counter keeps
// parallel renderer tests from being mistaken for work performed by the
// code path under test.
#[cfg(test)]
std::thread_local! {
    static PARSE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub fn reset_parse_calls() {
    PARSE_CALLS.set(0);
}

#[cfg(test)]
pub fn parse_calls() -> usize {
    PARSE_CALLS.get()
}

/// Parse a finalized message's raw text into semantic `RichLine`s for a pane
/// `width` columns wide. Theme-independent, and called once per message per
/// width (never per frame).  `pulldown-cmark` is total — malformed input
/// degrades to best-effort text, never a panic.
///
/// `width` is the FULL row width, gutter included: a table is the one block
/// whose layout cannot be deferred to the renderer (its columns are padded into
/// the span text), so it is the one thing the parse must be told the viewport
/// for. It used to be laid out to a fixed 100 columns whatever the terminal
/// was, which sheared every wide table on a narrow pane; passing the width in
/// is what keeps the cache correct instead of merely cheap. Pass 0 when no pane
/// has been measured yet — that means "unknown", not "zero", and falls back to
/// [`DEFAULT_TABLE_WIDTH`].
pub fn parse(text: &str, width: usize) -> Vec<RichLine> {
    #[cfg(test)]
    PARSE_CALLS.set(PARSE_CALLS.get() + 1);

    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let mut b = Builder {
        table_width: table_budget(width),
        ..Builder::default()
    };
    for ev in Parser::new_ext(text, opts) {
        b.event(ev);
    }
    b.finish()
}

/// Width of a thematic break's rule (cosmetic; the Paragraph does not stretch it).
const RULE_WIDTH: usize = 24;

/// The columns every rendered line spends on its left rail (`"▌ "` / `"  "`),
/// which a table's own columns do not get to use.
const GUTTER_WIDTH: usize = 2;

/// The table budget used when the caller has no measured pane yet.
const DEFAULT_TABLE_WIDTH: usize = 100;

/// Narrowest a column may be squeezed to: two columns of content plus the "…".
const MIN_COLUMN_WIDTH: usize = 3;

/// The content columns a table may lay itself out into, given the full row
/// width. Never below `RULE_WIDTH`: a pane too narrow for any table still has
/// to produce readable cells rather than a column of ellipses.
fn table_budget(width: usize) -> usize {
    if width == 0 {
        return DEFAULT_TABLE_WIDTH;
    }
    width.saturating_sub(GUTTER_WIDTH).max(RULE_WIDTH)
}

#[derive(Default)]
struct Builder {
    lines: Vec<RichLine>,
    cur: Vec<RichSpan>,
    cur_links: Vec<LinkAnnotation>,
    produced_line: bool, // false until the first line is pushed (gutter "▌ " vs "  ")
    pending_blank: bool, // a block ended; the next line separates itself from it
    heading: Option<u8>,
    strong: usize,
    emphasis: usize,
    blockquote: usize,
    in_link: bool,
    link_url: String,
    link_start_byte: usize,
    // Ordered-list ordinals (or `None` for a bullet list), innermost last.
    list_stack: Vec<Option<u64>>,
    // Fenced code: `Some(lang)` while inside a fence; `code` accumulates its body.
    code_lang: Option<String>,
    code: String,
    // Table state, collected across the Table/TableHead/TableRow/TableCell
    // events and laid out into aligned rows by `layout_table` at `TagEnd::Table`.
    table: Option<TableState>,
    // Content columns a table may occupy — the viewport, less the gutter.
    table_width: usize,
}

#[derive(Default)]
struct TableState {
    // Collected here (Task 2); read by `layout_table` for column alignment.
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
        let links: Vec<LinkAnnotation> = std::mem::take(&mut self.cur_links);
        self.push_line(body, links);
    }

    /// Separate the next block from the one before it. Consumed by `push_line`
    /// rather than emitted eagerly, so a message never opens or closes on a
    /// blank line and consecutive blocks are never double-spaced.
    fn end_block(&mut self) {
        self.pending_blank = true;
    }

    fn push_line(&mut self, body: Vec<RichSpan>, links: Vec<LinkAnnotation>) {
        if std::mem::take(&mut self.pending_blank) && self.produced_line {
            self.lines.push(RichLine {
                spans: vec![RichSpan {
                    text: "  ".to_string(),
                    role: SpanRole::Gutter,
                }],
                links: Vec::new(),
            });
        }
        let gutter = if self.produced_line { "  " } else { "▌ " };
        self.produced_line = true;
        let mut gutter_len = gutter.len();
        let mut spans = Vec::with_capacity(body.len() + 2);
        spans.push(RichSpan {
            text: gutter.to_string(),
            role: SpanRole::Gutter,
        });
        if self.blockquote > 0 {
            let bq = "▏ ".repeat(self.blockquote);
            gutter_len += bq.len();
            spans.push(RichSpan {
                text: bq,
                role: SpanRole::Gutter,
            });
        }
        spans.extend(body);
        let shifted_links = links
            .into_iter()
            .map(|l| LinkAnnotation {
                range: (l.range.start + gutter_len)..(l.range.end + gutter_len),
                destination: l.destination,
            })
            .collect();
        self.lines.push(RichLine {
            spans,
            links: shifted_links,
        });
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let role = self.inline_role();
        self.push_inline(RichSpan {
            text: text.to_string(),
            role,
        });
    }

    /// The one sink for every inline span, so that "where does this go" is
    /// answered in a single place. Inside a table it is the current cell;
    /// otherwise it is the line being accumulated.
    ///
    /// `Event::Text` used to carry this routing alone, and `Event::Code` — the
    /// arm directly below it — did not, so a `` `code` `` span inside a cell
    /// left the cell empty and was flushed as a paragraph after the table:
    /// eight file names rendered as `main.pycore/config.pycore/database.py…`.
    fn push_inline(&mut self, span: RichSpan) {
        match self.table.as_mut() {
            Some(t) if t.in_cell => t.cell.push(span),
            _ => self.cur.push(span),
        }
    }

    fn event(&mut self, ev: Event) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                if self.code_lang.is_some() {
                    self.code.push_str(&t);
                } else {
                    self.push_text(&t);
                }
            }
            Event::Code(c) => self.push_inline(RichSpan {
                text: c.to_string(),
                role: SpanRole::InlineCode,
            }),
            // A cell is one line by construction, so a break inside one is the
            // space between its words — flushing there would split the table.
            Event::SoftBreak | Event::HardBreak => {
                if self.table.as_ref().is_some_and(|t| t.in_cell) {
                    self.push_inline(RichSpan {
                        text: " ".to_string(),
                        role: SpanRole::Body,
                    });
                } else {
                    self.flush();
                }
            }
            Event::Rule => {
                self.push_line(
                    vec![RichSpan {
                        text: "─".repeat(RULE_WIDTH),
                        role: SpanRole::Rule,
                    }],
                    Vec::new(),
                );
                self.end_block();
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
                let dest = dest_url.to_string();
                if dest.starts_with("http://") || dest.starts_with("https://") {
                    self.in_link = true;
                    self.link_url = dest;
                    self.link_start_byte = self.cur.iter().map(|s| s.text.len()).sum();
                }
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
                self.cur.push(RichSpan {
                    text: marker,
                    role: SpanRole::ListMarker,
                });
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                self.code_lang = Some(match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or("").to_string()
                    }
                    CodeBlockKind::Indented => String::new(),
                });
                self.code.clear();
            }
            Tag::Table(aligns) => {
                self.table = Some(TableState {
                    aligns,
                    ..Default::default()
                })
            }
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
                self.end_block();
            }
            TagEnd::Paragraph => {
                self.flush();
                // A loose list emits a paragraph per item; separating those
                // would space the items apart rather than the list from what
                // follows it.
                if self.list_stack.is_empty() {
                    self.end_block();
                }
            }
            TagEnd::Emphasis => self.emphasis = self.emphasis.saturating_sub(1),
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Strikethrough => {}
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.blockquote = self.blockquote.saturating_sub(1);
                self.end_block();
            }
            TagEnd::Link => {
                if self.in_link {
                    let link_end_byte: usize = self.cur.iter().map(|s| s.text.len()).sum();
                    if link_end_byte > self.link_start_byte {
                        self.cur_links.push(LinkAnnotation {
                            range: self.link_start_byte..link_end_byte,
                            destination: self.link_url.clone(),
                        });
                    }
                    self.cur.push(RichSpan {
                        text: format!(" ({})", self.link_url),
                        role: SpanRole::Gutter,
                    });
                    self.in_link = false;
                }
            }
            TagEnd::Item => self.flush(),
            TagEnd::List(_) => {
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.end_block();
                }
            }
            TagEnd::CodeBlock => {
                let lang = self.code_lang.take().unwrap_or_default();
                let code = std::mem::take(&mut self.code);
                self.emit_code(&lang, &code);
                self.end_block();
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
            TagEnd::Table => {
                self.emit_table();
                self.end_block();
            }
            _ => {}
        }
    }

    /// Fenced code → per-language highlighted lines, re-wrapped with the gutter.
    fn emit_code(&mut self, lang: &str, code: &str) {
        for rl in highlight(lang, code) {
            self.push_line(rl.spans, rl.links);
        }
    }

    /// Table layout: lay the collected `TableState` out into aligned rows and
    /// splice each into the output via `push_line` (gutter-prefixed).
    fn emit_table(&mut self) {
        let Some(t) = self.table.take() else { return };
        for line in layout_table(&t, self.table_width) {
            self.push_line(line, Vec::new());
        }
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

/// Lay a collected table out into gutter-less `RichLine` bodies: a header row, a
/// "─┼─" rule row, then one row per body line. Column widths are the max cell
/// display width, capped so the total stays within `budget` — the pane the
/// message is being laid out for; overlong cells are truncated with a
/// trailing "…".
fn layout_table(t: &TableState, budget: usize) -> Vec<Vec<RichSpan>> {
    let n_cols = t.rows.iter().map(Vec::len).max().unwrap_or(0);
    if n_cols == 0 {
        return Vec::new();
    }
    let cell_text =
        |cell: &[RichSpan]| -> String { cell.iter().map(|s| s.text.as_str()).collect() };
    // Column budgets are terminal CELLS, so widths and padding must both be
    // measured in display columns: a CJK or emoji cell counted by `char`s
    // occupies twice the space it was allotted and shears every column to its
    // right out of alignment.
    let cell_width =
        |cell: &[RichSpan]| -> usize { UnicodeWidthStr::width(cell_text(cell).as_str()) };

    // What each column would need to show every cell in full.
    let joins = n_cols.saturating_sub(1).saturating_mul(3);
    let mut natural = vec![0usize; n_cols];
    for row in &t.rows {
        for (c, cell) in row.iter().enumerate() {
            natural[c] = natural[c].max(cell_width(cell));
        }
    }

    // Share the budget by need, not equally. An equal cap truncated the wide
    // column while the narrow one sat half empty — a two-column table on a
    // 200-column terminal still ellipsised its prose at 45 columns because the
    // file-name column beside it was allotted the same 45 and used 16.
    let avail = budget.saturating_sub(joins);
    let widths = if natural.iter().sum::<usize>() <= avail {
        natural
    } else {
        // Water-fill: settle every column that fits inside an equal share, then
        // redistribute the slack it did not use among the columns still over.
        let mut widths = vec![None; n_cols];
        let mut remaining = avail;
        let mut unsettled = n_cols;
        loop {
            let share = (remaining / unsettled.max(1)).max(MIN_COLUMN_WIDTH);
            let settling: Vec<usize> = (0..n_cols)
                .filter(|&c| widths[c].is_none() && natural[c] <= share)
                .collect();
            if settling.is_empty() {
                // Everyone left wants more than its share: split what is left.
                let over: Vec<usize> = (0..n_cols).filter(|&c| widths[c].is_none()).collect();
                let mut rest = remaining;
                for c in over {
                    let w = share.min(rest);
                    widths[c] = Some(w.max(MIN_COLUMN_WIDTH));
                    rest = rest.saturating_sub(w);
                }
                break;
            }
            for c in settling {
                widths[c] = Some(natural[c]);
                remaining = remaining.saturating_sub(natural[c]);
                unsettled -= 1;
            }
            if unsettled == 0 {
                break;
            }
        }
        widths
            .into_iter()
            .map(|w| w.unwrap_or(MIN_COLUMN_WIDTH))
            .collect()
    };

    // A cell keeps its inline spans rather than collapsing to one string, so
    // `code`, **bold** and *italic* inside a cell stay styled. Only spans that
    // carry no styling of their own take the header/body role of the row.
    let pad_cell =
        |cell: &[RichSpan], w: usize, align: Alignment, plain: SpanRole| -> Vec<RichSpan> {
            let role_of = |s: &RichSpan| {
                if s.role == SpanRole::Body {
                    plain
                } else {
                    s.role
                }
            };
            let mut out: Vec<RichSpan> = Vec::with_capacity(cell.len() + 2);
            let total = cell_width(cell);
            let mut width = 0usize;

            if total > w {
                // Drop whole graphemes until the "…" fits: a wide glyph may free
                // two columns at once, and half a grapheme is not a character.
                let limit = w.saturating_sub(1);
                for s in cell {
                    let mut kept = String::new();
                    for grapheme in UnicodeSegmentation::graphemes(s.text.as_str(), true) {
                        let next = UnicodeWidthStr::width(grapheme);
                        if width + next > limit {
                            break;
                        }
                        kept.push_str(grapheme);
                        width += next;
                    }
                    if !kept.is_empty() {
                        out.push(RichSpan {
                            text: kept,
                            role: role_of(s),
                        });
                    }
                    if width >= limit {
                        break;
                    }
                }
                out.push(RichSpan {
                    text: "…".to_string(),
                    role: plain,
                });
                width += 1;
            } else {
                out.extend(
                    cell.iter()
                        .filter(|s| !s.text.is_empty())
                        .map(|s| RichSpan {
                            text: s.text.clone(),
                            role: role_of(s),
                        }),
                );
                width = total;
            }

            // Whatever it holds, the cell occupies exactly `w` columns, or every
            // column to its right shears out of alignment.
            let fill = w.saturating_sub(width);
            if fill > 0 {
                let pad = |n: usize| RichSpan {
                    text: " ".repeat(n),
                    role: plain,
                };
                match align {
                    Alignment::Right => out.insert(0, pad(fill)),
                    Alignment::Center => {
                        let left = fill / 2;
                        if left > 0 {
                            out.insert(0, pad(left));
                        }
                        out.push(pad(fill - left));
                    }
                    _ => out.push(pad(fill)), // None/Left
                }
            }
            out
        };

    let align_of = |c: usize| -> Alignment { t.aligns.get(c).copied().unwrap_or(Alignment::None) };
    let empty: Vec<RichSpan> = Vec::new();
    let mut out: Vec<Vec<RichSpan>> = Vec::with_capacity(t.rows.len() + 1);

    for (r, row) in t.rows.iter().enumerate() {
        let is_header = r < t.head_rows;
        let role = if is_header {
            SpanRole::TableHeader
        } else {
            SpanRole::TableCell
        };
        let mut spans: Vec<RichSpan> = Vec::with_capacity(n_cols * 2);
        for (c, &w) in widths.iter().enumerate() {
            if c > 0 {
                spans.push(RichSpan {
                    text: " │ ".to_string(),
                    role: SpanRole::TableRule,
                });
            }
            let cell = row.get(c).unwrap_or(&empty);
            spans.extend(pad_cell(cell, w, align_of(c), role));
        }
        out.push(spans);
        // Emit the "─┼─" rule directly after the header block.
        if is_header && r + 1 == t.head_rows {
            let mut rule: Vec<RichSpan> = Vec::with_capacity(n_cols * 2);
            for (c, &w) in widths.iter().enumerate() {
                if c > 0 {
                    rule.push(RichSpan {
                        text: "─┼─".to_string(),
                        role: SpanRole::TableRule,
                    });
                }
                rule.push(RichSpan {
                    text: "─".repeat(w),
                    role: SpanRole::TableRule,
                });
            }
            out.push(rule);
        }
    }
    out
}

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
                            Some(role) => RichSpan {
                                text,
                                role: SpanRole::CodeToken(role),
                            },
                            None => RichSpan {
                                text,
                                role: SpanRole::CodePlain,
                            },
                        },
                        TokOpt::None(text) => RichSpan {
                            text,
                            role: SpanRole::CodePlain,
                        },
                    })
                    .collect();
                RichLine {
                    spans,
                    links: Vec::new(),
                }
            })
            .collect()
    } else {
        lines
            .into_iter()
            .map(|raw| RichLine {
                spans: vec![RichSpan {
                    text: raw,
                    role: SpanRole::CodePlain,
                }],
                links: Vec::new(),
            })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The pane these tests parse for: `DEFAULT_TABLE_WIDTH` plus the gutter,
    /// so the existing assertions measure exactly the layout they always did.
    const TEST_WIDTH: usize = DEFAULT_TABLE_WIDTH + GUTTER_WIDTH;

    #[test]
    fn richline_holds_roled_spans() {
        let line = RichLine {
            spans: vec![
                RichSpan {
                    text: "▌ ".into(),
                    role: SpanRole::Gutter,
                },
                RichSpan {
                    text: "hi".into(),
                    role: SpanRole::Heading(1),
                },
                RichSpan {
                    text: "x".into(),
                    role: SpanRole::CodeToken(SyntaxRole::Keyword),
                },
            ],
            links: Vec::new(),
        };
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[1].role, SpanRole::Heading(1));
        assert_eq!(line.spans[2].role, SpanRole::CodeToken(SyntaxRole::Keyword));
        assert_eq!(line, line.clone());
    }

    // Every line starts with the left rail: "▌ " on the message's first line, "  " after.
    fn body_roles(line: &RichLine) -> Vec<SpanRole> {
        line.spans.iter().skip(1).map(|s| s.role).collect() // skip the Gutter span
    }

    #[test]
    fn heading_becomes_one_line_of_heading_role() {
        let lines = parse("## Title", TEST_WIDTH);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].role, SpanRole::Gutter);
        assert_eq!(lines[0].spans[0].text, "▌ ");
        assert!(body_roles(&lines[0])
            .iter()
            .all(|r| *r == SpanRole::Heading(2)));
        assert!(lines[0].spans.iter().any(|s| s.text.contains("Title")));
    }

    #[test]
    fn emphasis_strong_and_inline_code_get_their_roles() {
        let lines = parse("plain **bold** *it* `code`", TEST_WIDTH);
        let roles = body_roles(&lines[0]);
        assert!(roles.contains(&SpanRole::Body));
        assert!(roles.contains(&SpanRole::Strong));
        assert!(roles.contains(&SpanRole::Emphasis));
        assert!(roles.contains(&SpanRole::InlineCode));
    }

    #[test]
    fn bullet_list_item_starts_with_a_list_marker() {
        let lines = parse("- one\n- two", TEST_WIDTH);
        assert_eq!(lines.len(), 2);
        assert_eq!(body_roles(&lines[0])[0], SpanRole::ListMarker);
        assert_eq!(lines[0].spans[1].text, "• ");
    }

    #[test]
    fn ordered_list_numbers_items() {
        let lines = parse("1. a\n2. b", TEST_WIDTH);
        assert_eq!(lines[0].spans[1].text, "1. ");
        assert_eq!(lines[1].spans[1].text, "2. ");
        assert_eq!(body_roles(&lines[0])[0], SpanRole::ListMarker);
    }

    #[test]
    fn block_quote_gets_a_bar_and_quote_body() {
        let lines = parse("> quoted", TEST_WIDTH);
        assert_eq!(lines[0].spans[1].text, "▏ ");
        assert_eq!(lines[0].spans[1].role, SpanRole::Gutter);
        assert!(body_roles(&lines[0]).contains(&SpanRole::BlockQuote));
    }

    #[test]
    fn thematic_break_is_a_rule_line() {
        let lines = parse("---", TEST_WIDTH);
        assert!(lines[0].spans.iter().any(|s| s.role == SpanRole::Rule));
    }

    #[test]
    fn link_text_is_roled_and_the_url_trails() {
        let lines = parse("[Zed](https://zed.dev)", TEST_WIDTH);
        let roles = body_roles(&lines[0]);
        assert!(roles.contains(&SpanRole::Link));
        assert!(lines[0]
            .spans
            .iter()
            .any(|s| s.text.contains("https://zed.dev")));
    }

    #[test]
    fn fenced_code_is_plain_for_now() {
        let lines = parse("```\nhello\n```", TEST_WIDTH);
        assert!(lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.role == SpanRole::CodePlain)));
    }

    #[test]
    fn soft_break_splits_paragraph_into_lines() {
        let lines = parse("a\nb", TEST_WIDTH);
        assert_eq!(lines.len(), 2);
    }

    fn code_roles(lines: &[RichLine]) -> Vec<SpanRole> {
        lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.role))
            .collect()
    }

    #[test]
    fn rust_fence_highlights_keyword_and_comment() {
        let out = highlight("rust", "fn main() {\n    let x = 5; // note\n}");
        assert_eq!(out.len(), 3, "one RichLine per source line");
        let roles = code_roles(&out);
        assert!(
            roles.contains(&SpanRole::CodeToken(SyntaxRole::Keyword)),
            "fn/let are keywords"
        );
        assert!(
            roles.contains(&SpanRole::CodeToken(SyntaxRole::Comment)),
            "// note is a comment"
        );
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
        let lines = parse("```rust\nfn a() {}\n```", TEST_WIDTH);
        // parse re-wraps highlight's lines with the gutter, then a CodeToken(Keyword) shows.
        let roles: Vec<SpanRole> = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.role))
            .collect();
        assert!(roles.contains(&SpanRole::CodeToken(SyntaxRole::Keyword)));
        assert_eq!(lines[0].spans[0].role, SpanRole::Gutter);
    }

    #[test]
    fn table_renders_aligned_header_rule_and_rows() {
        let md = "| a | bb |\n| :- | -: |\n| 1 | 2 |\n| 33 | 4 |";
        let lines = parse(md, TEST_WIDTH);
        // header + rule + 2 body rows (each a RichLine).
        assert!(lines.len() >= 4, "got {} lines", lines.len());
        // A header cell carries the TableHeader role.
        assert!(lines[0]
            .spans
            .iter()
            .any(|s| s.role == SpanRole::TableHeader));
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

    /// The defect a user reported from a real session: a code-graph summary
    /// whose first column was every file name in backticks rendered the column
    /// blank and dumped the names, run together, in a paragraph under the
    /// table — `main.pycore/config.pycore/database.py…`.
    #[test]
    fn inline_code_in_a_cell_stays_in_the_cell() {
        let md = "| File | Purpose |\n| - | - |\n| `main.py` | App entrypoint |\n\
                  | `core/config.py` | Settings |";
        let lines = parse(md, TEST_WIDTH);
        let text_of =
            |l: &RichLine| -> String { l.spans.iter().map(|s| s.text.as_str()).collect() };

        let body: Vec<String> = lines.iter().map(text_of).collect();
        assert!(
            body.iter().any(|l| l.contains("main.py")),
            "the cell lost its code span: {body:#?}"
        );
        // The run-on paragraph is the signature of the bug: both names on one
        // line with no cell padding between them.
        assert!(
            !body.iter().any(|l| l.contains("main.pycore/config.py")),
            "code spans were flushed after the table: {body:#?}"
        );
        // Every row is still exactly as wide as the header, so the column the
        // code spans live in did not collapse.
        let width = |l: &RichLine| -> usize { UnicodeWidthStr::width(text_of(l).as_str()) };
        assert_eq!(width(&lines[0]), width(&lines[2]), "columns not aligned");
        assert_eq!(width(&lines[0]), width(&lines[3]), "columns not aligned");
        // And it is still styled as code rather than flattened into body text.
        assert!(
            lines[2]
                .spans
                .iter()
                .any(|s| s.role == SpanRole::InlineCode),
            "cell code span lost its role: {:#?}",
            lines[2].spans
        );
    }

    #[test]
    fn blocks_are_separated_by_one_blank_line_and_lists_stay_tight() {
        let md = "# Title\n\nFirst paragraph.\n\n- one\n- two\n\nAfter the list.";
        let lines = parse(md, TEST_WIDTH);
        let text_of =
            |l: &RichLine| -> String { l.spans.iter().map(|s| s.text.as_str()).collect() };
        let rendered: Vec<String> = lines.iter().map(text_of).collect();
        let blank = |s: &String| s.trim().is_empty();

        assert!(!blank(&rendered[0]), "leads with a blank: {rendered:#?}");
        assert!(
            !blank(rendered.last().unwrap()),
            "trails with a blank: {rendered:#?}"
        );
        // Heading, paragraph, list and the closing paragraph: three separators.
        assert_eq!(
            rendered.iter().filter(|s| blank(s)).count(),
            3,
            "expected one blank between each of four blocks: {rendered:#?}"
        );
        // The two list items are adjacent — a list is one block, not two.
        let one = rendered.iter().position(|l| l.contains("one")).unwrap();
        let two = rendered.iter().position(|l| l.contains("two")).unwrap();
        assert_eq!(two, one + 1, "list items were spaced apart: {rendered:#?}");
    }

    /// Table columns are terminal CELLS. A CJK or emoji cell counted by `char`s
    /// takes twice the columns it was allotted, shearing every column to its
    /// right; widths and padding are therefore measured in display columns.
    #[test]
    fn table_columns_align_by_display_width_not_char_count() {
        let md = "| name | n |\n| :- | -: |\n| 日本語 | 1 |\n| ab | 2 |\n| 🚀🚀 | 3 |";
        let lines = parse(md, TEST_WIDTH);
        let display_width = |l: &RichLine| -> usize {
            l.spans
                .iter()
                .map(|s| unicode_width::UnicodeWidthStr::width(s.text.as_str()))
                .sum()
        };
        let header = display_width(&lines[0]);
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(
                display_width(line),
                header,
                "row {i} is a different number of terminal columns:\n{:?}",
                line.spans.iter().map(|s| &s.text).collect::<Vec<_>>()
            );
        }
    }

    /// An overlong wide cell is ellipsed on a grapheme boundary and padded back
    /// out to its full column budget — never half a glyph, never a short cell.
    #[test]
    fn an_overlong_wide_cell_truncates_on_a_grapheme_boundary() {
        let long = "日".repeat(80);
        let md = format!("| a |\n| :- |\n| {long} |");
        let lines = parse(&md, TEST_WIDTH);
        let body = lines.last().expect("a body row");
        let text: String = body.spans.iter().map(|s| s.text.as_str()).collect();
        assert!(text.contains('…'), "an overlong cell is ellipsed: {text:?}");
        // Every kept glyph is whole (the string is valid, and re-splitting it
        // into graphemes round-trips).
        let rejoined: String =
            unicode_segmentation::UnicodeSegmentation::graphemes(text.as_str(), true).collect();
        assert_eq!(rejoined, text);
        let header_width: usize = lines[0]
            .spans
            .iter()
            .map(|s| unicode_width::UnicodeWidthStr::width(s.text.as_str()))
            .sum();
        let body_width: usize = body
            .spans
            .iter()
            .map(|s| unicode_width::UnicodeWidthStr::width(s.text.as_str()))
            .sum();
        assert_eq!(
            body_width, header_width,
            "the ellipsed cell keeps its budget"
        );
    }

    /// The budget used to be a fixed 100 columns whatever the terminal was, so
    /// any wide table sheared on a narrow pane (the renderer then wrapped the
    /// overflow back to column 0, breaking the alignment the layout is for).
    #[test]
    fn a_table_is_laid_out_to_the_pane_it_was_parsed_for() {
        let md = "| column-one-is-long | column-two-is-long | column-three-is-long | \
column-four-is-long |\n| --- | --- | --- | --- |\n\
| aaaaaaaaaaaaaaaaaa | bbbbbbbbbbbbbbbbbb | cccccccccccccccccccc | dddddddddddddddddd |";
        for pane in [40usize, 70, 120] {
            let lines = parse(md, pane);
            for (i, line) in lines.iter().enumerate() {
                let width: usize = line
                    .spans
                    .iter()
                    .map(|s| UnicodeWidthStr::width(s.text.as_str()))
                    .sum();
                assert!(
                    width <= pane,
                    "row {i} is {width} columns wide in a {pane}-column pane"
                );
            }
        }
    }

    /// Narrowing must not silently drop a column: every row keeps its cells and
    /// its alignment, just ellipsed harder.
    #[test]
    fn a_narrow_table_keeps_every_column_aligned() {
        let md = "| a | bb | ccc |\n| :- | :-: | -: |\n| 1111111 | 2222222 | 3333333 |";
        let lines = parse(md, 30);
        let width = |l: &RichLine| -> usize {
            l.spans
                .iter()
                .map(|s| UnicodeWidthStr::width(s.text.as_str()))
                .sum()
        };
        let header = width(&lines[0]);
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(width(line), header, "row {i} lost alignment when narrowed");
            assert_eq!(
                line.spans
                    .iter()
                    .filter(|s| s.text.contains('│') || s.text.contains('┼'))
                    .count(),
                2,
                "row {i} lost a column separator"
            );
        }
    }

    #[test]
    fn parses_http_link_annotations() {
        let lines = parse(
            "Check [website](https://example.com) for details.",
            TEST_WIDTH,
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].links.len(), 1);
        assert_eq!(lines[0].links[0].destination, "https://example.com");
    }
}
