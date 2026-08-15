//! Canonical pipe-table structure detection and fenced-code-block tracking for
//! raw markdown source (Adoption 11 M3).

/// Split a pipe-delimited line into trimmed segments.
///
/// Returns `None` if the line is empty or has no unescaped separator marker.
/// Leading/trailing pipes are stripped before splitting.
#[must_use]
pub fn parse_table_segments(line: &str) -> Option<Vec<&str>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let has_outer_pipe = trimmed.starts_with('|') || trimmed.ends_with('|');
    let content = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let content = content.strip_suffix('|').unwrap_or(content);
    let raw_segments = split_unescaped_pipe(content);
    if !has_outer_pipe && raw_segments.len() <= 1 {
        return None;
    }

    let segments: Vec<&str> = raw_segments.into_iter().map(str::trim).collect();
    (!segments.is_empty()).then_some(segments)
}

/// Split `content` on unescaped `|` characters.
#[must_use]
pub fn split_unescaped_pipe(content: &str) -> Vec<&str> {
    let mut segments = Vec::with_capacity(8);
    let mut start = 0;
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
        } else if bytes[i] == b'|' {
            segments.push(&content[start..i]);
            start = i + 1;
            i += 1;
        } else {
            i += 1;
        }
    }
    segments.push(&content[start..]);
    segments
}

/// Whether `line` looks like a table header row (has pipe-separated
/// segments with at least one non-empty cell).
#[inline]
#[must_use]
pub fn is_table_header_line(line: &str) -> bool {
    parse_table_segments(line).is_some_and(|segments| segments.iter().any(|s| !s.is_empty()))
}

/// Whether a single segment matches the `---`, `:---`, `---:`, or `:---:`
/// alignment-colon syntax used in markdown table delimiter rows.
#[inline]
#[must_use]
pub fn is_table_delimiter_segment(segment: &str) -> bool {
    let trimmed = segment.trim();
    if trimmed.is_empty() {
        return false;
    }
    let without_leading = trimmed.strip_prefix(':').unwrap_or(trimmed);
    let without_ends = without_leading.strip_suffix(':').unwrap_or(without_leading);
    without_ends.len() >= 3 && without_ends.chars().all(|c| c == '-')
}

/// Whether `line` is a valid table delimiter row (every segment passes
/// [`is_table_delimiter_segment`]).
#[inline]
#[must_use]
pub fn is_table_delimiter_line(line: &str) -> bool {
    parse_table_segments(line)
        .is_some_and(|segments| segments.into_iter().all(is_table_delimiter_segment))
}

/// Where a source line sits relative to fenced code blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FenceKind {
    /// Not inside any fenced code block.
    Outside,
    /// Inside a ```md or ```markdown fence.
    Markdown,
    /// Inside a fence with a non-markdown info string.
    Other,
}

/// Incremental tracker for fenced-code-block open/close transitions.
#[derive(Debug, Clone, Default)]
pub struct FenceTracker {
    state: Option<(char, usize, FenceKind)>,
}

impl FenceTracker {
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self { state: None }
    }

    /// Process one raw source line and update fence state.
    pub fn advance(&mut self, raw_line: &str) {
        let leading_spaces = raw_line
            .as_bytes()
            .iter()
            .take_while(|byte| **byte == b' ')
            .count();
        if leading_spaces > 3 {
            return;
        }

        let trimmed = &raw_line[leading_spaces..];
        let fence_scan_text = strip_blockquote_prefix(trimmed);
        if let Some((marker, len)) = parse_fence_marker(fence_scan_text) {
            if let Some((open_char, open_len, _)) = self.state {
                if marker == open_char
                    && len >= open_len
                    && fence_scan_text[len..].trim().is_empty()
                {
                    self.state = None;
                }
            } else {
                let kind = if is_markdown_fence_info(fence_scan_text, len) {
                    FenceKind::Markdown
                } else {
                    FenceKind::Other
                };
                self.state = Some((marker, len, kind));
            }
        }
    }

    /// Current fence context for the most-recently-advanced line.
    #[inline]
    #[must_use]
    pub fn kind(&self) -> FenceKind {
        self.state.map_or(FenceKind::Outside, |(_, _, k)| k)
    }
}

/// Return fence marker character and run length for a potential fence line.
#[inline]
#[must_use]
pub fn parse_fence_marker(line: &str) -> Option<(char, usize)> {
    let first = line.as_bytes().first().copied()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let len = line.bytes().take_while(|&b| b == first).count();
    if len < 3 {
        return None;
    }
    Some((first as char, len))
}

/// Whether the info string after a fence marker indicates markdown content.
#[inline]
#[must_use]
pub fn is_markdown_fence_info(trimmed_line: &str, marker_len: usize) -> bool {
    let info = trimmed_line[marker_len..]
        .split_whitespace()
        .next()
        .unwrap_or_default();
    info.eq_ignore_ascii_case("md") || info.eq_ignore_ascii_case("markdown")
}

/// Peel all leading `>` blockquote markers from a line.
#[inline]
#[must_use]
pub fn strip_blockquote_prefix(line: &str) -> &str {
    let mut rest = line.trim_start();
    loop {
        let Some(stripped) = rest.strip_prefix('>') else {
            return rest;
        };
        rest = stripped.strip_prefix(' ').unwrap_or(stripped).trim_start();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_table_segments_basic() {
        assert_eq!(
            parse_table_segments("| A | B | C |"),
            Some(vec!["A", "B", "C"])
        );
    }

    #[test]
    fn parse_table_segments_no_outer_pipes() {
        assert_eq!(parse_table_segments("A | B | C"), Some(vec!["A", "B", "C"]));
    }

    #[test]
    fn is_table_header_line_valid() {
        assert!(is_table_header_line("| A | B |"));
        assert!(is_table_header_line("Name | Value"));
        assert!(!is_table_header_line("| | |"));
    }

    #[test]
    fn is_table_delimiter_line_valid() {
        assert!(is_table_delimiter_line("| --- | :---: | ---: |"));
        assert!(is_table_delimiter_line("--- | ---"));
        assert!(!is_table_delimiter_line("| A | B |"));
    }

    #[test]
    fn fence_tracker_detects_markdown_vs_other() {
        let mut tracker = FenceTracker::new();
        assert_eq!(tracker.kind(), FenceKind::Outside);
        tracker.advance("```markdown");
        assert_eq!(tracker.kind(), FenceKind::Markdown);
        tracker.advance("| A | B |");
        assert_eq!(tracker.kind(), FenceKind::Markdown);
        tracker.advance("```");
        assert_eq!(tracker.kind(), FenceKind::Outside);

        tracker.advance("```rust");
        assert_eq!(tracker.kind(), FenceKind::Other);
        tracker.advance("```");
        assert_eq!(tracker.kind(), FenceKind::Outside);
    }
}
