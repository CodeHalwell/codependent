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
}
