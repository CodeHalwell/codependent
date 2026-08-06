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

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use synoptic::{from_extension, TokOpt};

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

/// Parse a finalized message's raw text into semantic `RichLine`s. Theme- and
/// width-independent; called exactly once per message on finalize (never per
/// frame). `pulldown-cmark` is total — malformed input degrades to best-effort
/// text, never a panic.
pub fn parse(text: &str) -> Vec<RichLine> {
    #[cfg(test)]
    PARSE_CALLS.set(PARSE_CALLS.get() + 1);

    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let mut b = Builder::default();
    for ev in Parser::new_ext(text, opts) {
        b.event(ev);
    }
    b.finish()
}

/// Width of a thematic break's rule (cosmetic; the Paragraph does not stretch it).
const RULE_WIDTH: usize = 24;

/// Cap a table's total content width so a pathological table cannot blow the pane.
const MAX_TABLE_WIDTH: usize = 100;

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
    // Table state, collected across the Table/TableHead/TableRow/TableCell
    // events and laid out into aligned rows by `layout_table` at `TagEnd::Table`.
    table: Option<TableState>,
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
        self.push_line(body);
    }

    fn push_line(&mut self, body: Vec<RichSpan>) {
        let gutter = if self.produced_line { "  " } else { "▌ " };
        self.produced_line = true;
        let mut spans = Vec::with_capacity(body.len() + 2);
        spans.push(RichSpan {
            text: gutter.to_string(),
            role: SpanRole::Gutter,
        });
        if self.blockquote > 0 {
            spans.push(RichSpan {
                text: "▏ ".repeat(self.blockquote),
                role: SpanRole::Gutter,
            });
        }
        spans.extend(body);
        self.lines.push(RichLine { spans });
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let role = self.inline_role();
        self.cur.push(RichSpan {
            text: text.to_string(),
            role,
        });
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
                    self.table.as_mut().unwrap().cell.push(RichSpan {
                        text: t.to_string(),
                        role,
                    });
                } else {
                    self.push_text(&t);
                }
            }
            Event::Code(c) => {
                self.cur.push(RichSpan {
                    text: c.to_string(),
                    role: SpanRole::InlineCode,
                });
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

    /// Fenced code → per-language highlighted lines, re-wrapped with the gutter.
    fn emit_code(&mut self, lang: &str, code: &str) {
        for rl in highlight(lang, code) {
            self.push_line(rl.spans);
        }
    }

    /// Table layout: lay the collected `TableState` out into aligned rows and
    /// splice each into the output via `push_line` (gutter-prefixed).
    fn emit_table(&mut self) {
        let Some(t) = self.table.take() else { return };
        for line in layout_table(&t) {
            self.push_line(line);
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
/// display width, capped so the total stays within `MAX_TABLE_WIDTH`; overlong
/// cells are truncated with a trailing "…".
fn layout_table(t: &TableState) -> Vec<Vec<RichSpan>> {
    let n_cols = t.rows.iter().map(Vec::len).max().unwrap_or(0);
    if n_cols == 0 {
        return Vec::new();
    }
    let cell_text =
        |cell: &[RichSpan]| -> String { cell.iter().map(|s| s.text.as_str()).collect() };
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
            spans.push(pad_cell(cell, w, align_of(c), role));
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
                RichLine { spans }
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
        let lines = parse("## Title");
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
        assert!(lines[0]
            .spans
            .iter()
            .any(|s| s.text.contains("https://zed.dev")));
    }

    #[test]
    fn fenced_code_is_plain_for_now() {
        let lines = parse("```\nhello\n```");
        assert!(lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.role == SpanRole::CodePlain)));
    }

    #[test]
    fn soft_break_splits_paragraph_into_lines() {
        let lines = parse("a\nb");
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
        let lines = parse("```rust\nfn a() {}\n```");
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
        let lines = parse(md);
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
}
