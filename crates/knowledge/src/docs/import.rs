//! Markdown → blocks import (the lossy-but-reasonable inverse of
//! [`render`](super::render)).
//!
//! [`markdown_to_blocks`] parses a Markdown document into typed
//! [`DocumentBlock`]s at BLOCK granularity — headings, paragraphs, fenced code,
//! tables, callouts, checklists, and the `{{ kind:target }}` embed markers the
//! renderer emits — so `codypendent docs new --from file.md` and the agent's
//! `docs.create` seed a real block-structured document rather than one giant
//! paragraph. It is deliberately NOT a full CommonMark parser: inline styling
//! stays inside a block's text verbatim (blocks carry flat text, ADR-016), an
//! unrecognized construct degrades to a paragraph, and
//! `import(render(blocks))` reproduces the block CONTENT (ids are fresh) for
//! every kind the renderer emits — the round-trip property the tests pin.

use super::model::{BlockContent, ChecklistItem, DocumentBlock};

/// Parse `markdown` into typed document blocks. Total: any input produces a
/// (possibly empty) block list; nothing errors. See the module docs for the
/// dialect.
#[must_use]
pub fn markdown_to_blocks(markdown: &str) -> Vec<DocumentBlock> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut blocks = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut i = 0;

    // Flush the accumulated paragraph lines (joined by \n) as one block.
    let flush = |paragraph: &mut Vec<&str>, blocks: &mut Vec<DocumentBlock>| {
        if !paragraph.is_empty() {
            let text = paragraph.join("\n");
            paragraph.clear();
            blocks.push(DocumentBlock::new(BlockContent::Paragraph { text }));
        }
    };

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_end();

        // Blank line: paragraph separator.
        if trimmed.trim().is_empty() {
            flush(&mut paragraph, &mut blocks);
            i += 1;
            continue;
        }

        // Fenced code: ```lang … ``` — the renderer's Code/Diagram/Query form.
        if let Some(info) = trimmed.strip_prefix("```") {
            flush(&mut paragraph, &mut blocks);
            let info = info.trim().to_string();
            let mut body: Vec<&str> = Vec::new();
            i += 1;
            while i < lines.len() && lines[i].trim_end() != "```" {
                body.push(lines[i]);
                i += 1;
            }
            // Skip the closing fence when present; an unclosed fence consumes
            // the rest of the input as code (lossy-but-reasonable).
            if i < lines.len() {
                i += 1;
            }
            let text = body.join("\n");
            blocks.push(DocumentBlock::new(fenced_content(&info, text)));
            continue;
        }

        // Heading: 1-6 leading #s then a space.
        if let Some((level, text)) = parse_heading(trimmed) {
            flush(&mut paragraph, &mut blocks);
            blocks.push(DocumentBlock::new(BlockContent::Heading { level, text }));
            i += 1;
            continue;
        }

        // Callout: `> [!KIND]` then `> …` continuation lines (the renderer's
        // multi-line form). A plain blockquote (no [!KIND]) stays a paragraph.
        if let Some(kind) = parse_callout_opener(trimmed) {
            flush(&mut paragraph, &mut blocks);
            let mut body: Vec<String> = Vec::new();
            i += 1;
            while i < lines.len() {
                let quoted = lines[i].trim_end();
                if let Some(rest) = quoted.strip_prefix("> ") {
                    body.push(rest.to_string());
                } else if quoted == ">" {
                    body.push(String::new());
                } else {
                    break;
                }
                i += 1;
            }
            blocks.push(DocumentBlock::new(BlockContent::Callout {
                kind,
                text: body.join("\n"),
            }));
            continue;
        }

        // Checklist run: `- [ ] …` / `- [x] …` lines.
        if parse_checklist_item(trimmed).is_some() {
            flush(&mut paragraph, &mut blocks);
            let mut items = Vec::new();
            while i < lines.len() {
                match parse_checklist_item(lines[i].trim_end()) {
                    Some(item) => {
                        items.push(item);
                        i += 1;
                    }
                    None => break,
                }
            }
            blocks.push(DocumentBlock::new(BlockContent::Checklist { items }));
            continue;
        }

        // Table run: consecutive `| … |` lines; the `| --- |` separator row is
        // structural and dropped.
        if is_table_row(trimmed) {
            flush(&mut paragraph, &mut blocks);
            let mut rows = Vec::new();
            while i < lines.len() && is_table_row(lines[i].trim_end()) {
                let row = parse_table_row(lines[i].trim_end());
                if !row.iter().all(|cell| is_separator_cell(cell)) {
                    rows.push(row.into_iter().map(|cell| unescape_cell(&cell)).collect());
                }
                i += 1;
            }
            blocks.push(DocumentBlock::new(BlockContent::Table { rows }));
            continue;
        }

        // Embed markers, alone on their line (the renderer's verbatim form).
        if let Some(content) = parse_embed(trimmed) {
            flush(&mut paragraph, &mut blocks);
            blocks.push(DocumentBlock::new(content));
            i += 1;
            continue;
        }

        // A `[path](path)` link alone on its line whose text equals its target
        // is the renderer's EmbeddedFile form.
        if let Some(path) = parse_self_link(trimmed) {
            flush(&mut paragraph, &mut blocks);
            blocks.push(DocumentBlock::new(BlockContent::EmbeddedFile { path }));
            i += 1;
            continue;
        }

        // Anything else accumulates into the current paragraph.
        paragraph.push(trimmed);
        i += 1;
    }
    flush(&mut paragraph, &mut blocks);
    blocks
}

/// Parse `markdown` as the initial content of a NEW document titled `title`:
/// [`markdown_to_blocks`], minus a leading H1 that repeats the title (the
/// renderer emits `# <title>` first, so a file produced by `render_document` —
/// or written by an author who titled it in the file — round-trips without a
/// duplicated title heading).
#[must_use]
pub fn import_markdown(title: &str, markdown: &str) -> Vec<DocumentBlock> {
    let mut blocks = markdown_to_blocks(markdown);
    if let Some(first) = blocks.first() {
        if matches!(&first.content, BlockContent::Heading { level: 1, text } if text == title) {
            blocks.remove(0);
        }
    }
    blocks
}

/// The typed content of a fenced block: the renderer writes queries as
/// ` ```query `, diagrams as ` ```<format> ` for its known formats, and code as
/// ` ```<language> `.
fn fenced_content(info: &str, text: String) -> BlockContent {
    match info {
        "query" => BlockContent::Query { query: text },
        // The diagram formats the fabric renders as diagram blocks. Anything
        // else stays code — a `rust` fence must never round-trip as a diagram.
        "mermaid" | "dot" | "graphviz" | "plantuml" | "d2" => BlockContent::Diagram {
            format: info.to_string(),
            source: text,
        },
        "" => BlockContent::Code {
            language: None,
            text,
        },
        language => BlockContent::Code {
            language: Some(language.to_string()),
            text,
        },
    }
}

/// `## Heading` → `(2, "Heading")`. At most six `#`s, and the space after them
/// is required (so `#hashtag` stays paragraph text).
fn parse_heading(line: &str) -> Option<(u8, String)> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = line.get(hashes..)?;
    let text = rest.strip_prefix(' ')?;
    Some((hashes as u8, text.to_string()))
}

/// `> [!WARNING]` → `Some("warning")`.
fn parse_callout_opener(line: &str) -> Option<String> {
    let rest = line.strip_prefix("> [!")?;
    let kind = rest.strip_suffix(']')?;
    if kind.is_empty() || kind.contains(']') {
        return None;
    }
    Some(kind.to_lowercase())
}

/// `- [x] text` / `- [ ] text` → a checklist item.
fn parse_checklist_item(line: &str) -> Option<ChecklistItem> {
    let checked = if let Some(rest) = line.strip_prefix("- [x] ") {
        Some((true, rest))
    } else {
        line.strip_prefix("- [ ] ").map(|rest| (false, rest))
    }?;
    Some(ChecklistItem {
        text: checked.1.to_string(),
        checked: checked.0,
    })
}

/// A table row starts and ends with `|` (after trimming) and has content.
fn is_table_row(line: &str) -> bool {
    let line = line.trim();
    line.len() >= 2 && line.starts_with('|') && line.ends_with('|')
}

/// Split a `| a | b |` row into raw cells, honouring `\|` escapes (a `\|` never
/// splits) so the renderer's escaping is inverted by [`unescape_cell`].
fn parse_table_row(line: &str) -> Vec<String> {
    let inner = line.trim();
    let inner = &inner[1..inner.len() - 1];
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                // Keep the escape pair intact for `unescape_cell`.
                cell.push('\\');
                if let Some(next) = chars.next() {
                    cell.push(next);
                }
            }
            '|' => {
                cells.push(cell.trim().to_string());
                cell = String::new();
            }
            other => cell.push(other),
        }
    }
    cells.push(cell.trim().to_string());
    cells
}

/// A separator cell is dashes with optional alignment colons (e.g. `---`,
/// `:---:`), which marks the header/body divider row.
fn is_separator_cell(cell: &str) -> bool {
    !cell.is_empty() && cell.chars().all(|c| c == '-' || c == ':') && cell.contains('-')
}

/// Invert the renderer's cell escaping: `\|` → `|`, `\\` → `\`. Any other
/// escape pair keeps its backslash verbatim.
fn unescape_cell(cell: &str) -> String {
    let mut out = String::with_capacity(cell.len());
    let mut chars = cell.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('|') => out.push('|'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `{{ symbol:X }}` / `{{ workflow:X }}` / `{{ skill:X }}` alone on a line.
fn parse_embed(line: &str) -> Option<BlockContent> {
    let inner = line.strip_prefix("{{ ")?.strip_suffix(" }}")?;
    let (kind, target) = inner.split_once(':')?;
    let target = target.trim().to_string();
    if target.is_empty() {
        return None;
    }
    match kind {
        "symbol" => Some(BlockContent::EmbeddedSymbol { symbol: target }),
        "workflow" => Some(BlockContent::EmbeddedWorkflow { workflow: target }),
        "skill" => Some(BlockContent::EmbeddedSkill { skill: target }),
        _ => None,
    }
}

/// `[path](path)` alone on a line, text equal to target — the renderer's
/// EmbeddedFile form. Any other link stays paragraph text.
fn parse_self_link(line: &str) -> Option<String> {
    let rest = line.strip_prefix('[')?;
    let (text, rest) = rest.split_once("](")?;
    let target = rest.strip_suffix(')')?;
    (text == target && !text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::super::render::render_document;
    use super::*;

    fn contents(blocks: &[DocumentBlock]) -> Vec<BlockContent> {
        blocks.iter().map(|b| b.content.clone()).collect()
    }

    /// The golden import case: one Markdown document covering every construct
    /// the importer recognizes maps to exactly the expected typed block
    /// sequence.
    #[test]
    fn golden_markdown_imports_to_typed_blocks() {
        let markdown = "\
# Payments Runbook

Intro paragraph
spanning two lines.

## Charging

```rust
fn charge() {}
```

> [!WARNING]
> Retries are not idempotent
> below revision 3.

- [x] rotate keys
- [ ] add alerting

| Field | Type |
| --- | --- |
| amount | u64 |
| op | a\\|b |

{{ symbol:payments::charge_customer }}

[docs/api.md](docs/api.md)

Closing paragraph.
";
        let blocks = markdown_to_blocks(markdown);
        assert_eq!(
            contents(&blocks),
            vec![
                BlockContent::Heading {
                    level: 1,
                    text: "Payments Runbook".into()
                },
                BlockContent::Paragraph {
                    text: "Intro paragraph\nspanning two lines.".into()
                },
                BlockContent::Heading {
                    level: 2,
                    text: "Charging".into()
                },
                BlockContent::Code {
                    language: Some("rust".into()),
                    text: "fn charge() {}".into()
                },
                BlockContent::Callout {
                    kind: "warning".into(),
                    text: "Retries are not idempotent\nbelow revision 3.".into()
                },
                BlockContent::Checklist {
                    items: vec![
                        ChecklistItem {
                            text: "rotate keys".into(),
                            checked: true
                        },
                        ChecklistItem {
                            text: "add alerting".into(),
                            checked: false
                        },
                    ]
                },
                BlockContent::Table {
                    rows: vec![
                        vec!["Field".into(), "Type".into()],
                        vec!["amount".into(), "u64".into()],
                        vec!["op".into(), "a|b".into()],
                    ]
                },
                BlockContent::EmbeddedSymbol {
                    symbol: "payments::charge_customer".into()
                },
                BlockContent::EmbeddedFile {
                    path: "docs/api.md".into()
                },
                BlockContent::Paragraph {
                    text: "Closing paragraph.".into()
                },
            ]
        );
    }

    /// `import(render(blocks))` reproduces the block content for every kind the
    /// renderer emits — the lossy-but-reasonable round-trip property.
    #[test]
    fn import_inverts_render_at_block_granularity() {
        let original = vec![
            BlockContent::Heading {
                level: 2,
                text: "Service".into(),
            },
            BlockContent::Paragraph {
                text: "Charges customers.".into(),
            },
            BlockContent::Code {
                language: Some("rust".into()),
                text: "fn charge() {}".into(),
            },
            BlockContent::Diagram {
                format: "mermaid".into(),
                source: "graph TD; a-->b".into(),
            },
            BlockContent::Table {
                rows: vec![
                    vec!["Field".into(), "Type".into()],
                    vec!["pattern".into(), "a|b".into()],
                ],
            },
            BlockContent::Callout {
                kind: "note".into(),
                text: "line one\nline two".into(),
            },
            BlockContent::Checklist {
                items: vec![ChecklistItem {
                    text: "retry".into(),
                    checked: true,
                }],
            },
            BlockContent::Query {
                query: "stale-docs".into(),
            },
            BlockContent::EmbeddedFile {
                path: "docs/api.md".into(),
            },
            BlockContent::EmbeddedSymbol {
                symbol: "payments::charge".into(),
            },
            BlockContent::EmbeddedWorkflow {
                workflow: "repair-github-check".into(),
            },
            BlockContent::EmbeddedSkill {
                skill: "fix-ci".into(),
            },
        ];
        let blocks: Vec<DocumentBlock> = original.iter().cloned().map(DocumentBlock::new).collect();
        let rendered = render_document("Runbook", &blocks);
        // `import_markdown` strips the `# Runbook` title heading the renderer
        // prepended, leaving exactly the original content sequence.
        let reimported = import_markdown("Runbook", &rendered);
        assert_eq!(contents(&reimported), original);
    }

    #[test]
    fn leading_title_heading_is_stripped_only_when_it_matches() {
        let markdown = "# Runbook\n\nBody.\n";
        assert_eq!(
            contents(&import_markdown("Runbook", markdown)),
            vec![BlockContent::Paragraph {
                text: "Body.".into()
            }]
        );
        // A different H1 is real content and stays.
        assert_eq!(contents(&import_markdown("Other Title", markdown)).len(), 2);
    }

    #[test]
    fn unclosed_fence_consumes_the_rest_as_code() {
        let blocks = markdown_to_blocks("```sh\necho hi\n");
        assert_eq!(
            contents(&blocks),
            vec![BlockContent::Code {
                language: Some("sh".into()),
                text: "echo hi".into()
            }]
        );
    }

    #[test]
    fn plain_blockquote_and_hashtag_degrade_to_paragraphs() {
        let blocks = markdown_to_blocks("> just a quote\n\n#hashtag not a heading\n");
        assert_eq!(
            contents(&blocks),
            vec![
                BlockContent::Paragraph {
                    text: "> just a quote".into()
                },
                BlockContent::Paragraph {
                    text: "#hashtag not a heading".into()
                },
            ]
        );
    }

    #[test]
    fn empty_input_imports_to_no_blocks() {
        assert!(markdown_to_blocks("").is_empty());
        assert!(markdown_to_blocks("\n\n\n").is_empty());
    }
}
