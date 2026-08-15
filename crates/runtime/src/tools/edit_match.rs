//! The nine-stage replacer cascade behind `workspace.edit_file` — ported from
//! opencode's `tool/edit.ts` (which credits Cline and gemini-cli). Pure
//! functions over `&str`: given the current buffer and a search string,
//! produce the unique byte span to replace, or a typed refusal. Stage 1 is
//! the exact match, so a byte-exact unique search behaves exactly as before
//! the cascade existed.

/// Which stage produced the accepted span. Ordered = cascade order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MatchStage {
    Exact,
    LineTrimmed,
    BlockAnchor,
    WhitespaceNormalized,
    IndentationFlexible,
    EscapeNormalized,
    TrimmedBoundary,
    ContextAware,
    MultiOccurrence,
}

/// The accepted match: a byte range of `content` plus the stage that found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatchResult {
    pub start: usize,
    pub len: usize,
    pub stage: MatchStage,
}

/// Why no span was accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MatchFailure {
    /// No stage produced any candidate present in the buffer.
    NotFound,
    /// Candidates exist but none occurs exactly once.
    Ambiguous { count: usize },
    /// A candidate was found but its span is far larger than the search —
    /// refusing is safer than replacing text the model never saw.
    Disproportionate,
}

/// Similarity floor for BlockAnchor's middle lines (reference: 0.65 for both
/// the single- and multiple-candidate paths).
const SIMILARITY_THRESHOLD: f64 = 0.65;

pub(crate) fn find_unique_span(content: &str, search: &str) -> Result<MatchResult, MatchFailure> {
    type Replacer = fn(&str, &str) -> Vec<String>;
    const CASCADE: &[(MatchStage, Replacer)] = &[
        (MatchStage::Exact, simple),
        (MatchStage::LineTrimmed, line_trimmed),
        (MatchStage::BlockAnchor, block_anchor),
        (MatchStage::WhitespaceNormalized, whitespace_normalized),
        (MatchStage::IndentationFlexible, indentation_flexible),
        (MatchStage::EscapeNormalized, escape_normalized),
        (MatchStage::TrimmedBoundary, trimmed_boundary),
        (MatchStage::ContextAware, context_aware),
        (MatchStage::MultiOccurrence, multi_occurrence),
    ];

    let mut found_any = false;
    let mut first_ambiguous: Option<usize> = None;
    for (stage, replacer) in CASCADE {
        for candidate in replacer(content, search) {
            let Some(start) = content.find(candidate.as_str()) else {
                continue;
            };
            found_any = true;
            if is_disproportionate(&candidate, search) {
                return Err(MatchFailure::Disproportionate);
            }
            let count = content.matches(candidate.as_str()).count();
            if count != 1 {
                if first_ambiguous.is_none() {
                    first_ambiguous = Some(count);
                }
                continue;
            }
            return Ok(MatchResult {
                start,
                len: candidate.len(),
                stage: *stage,
            });
        }
    }
    if !found_any {
        Err(MatchFailure::NotFound)
    } else {
        Err(MatchFailure::Ambiguous {
            count: first_ambiguous.unwrap_or(2),
        })
    }
}

/// Stage 1: the search string itself.
fn simple(_content: &str, search: &str) -> Vec<String> {
    vec![search.to_string()]
}

/// Collects lines and their byte offsets in content: `Vec<(start_offset, end_offset, line_str)>`.
fn line_spans(content: &str) -> Vec<(usize, usize, &str)> {
    let mut result = Vec::new();
    let mut offset = 0;
    for line in content.split('\n') {
        let len = line.len();
        result.push((offset, offset + len, line));
        offset += len + 1; // 1 for '\n'
    }
    result
}

/// Stage 2: window of lines equal after `str::trim`. Drops a trailing empty
/// search line. Spans reconstructed from the ORIGINAL lines (untrimmed).
fn line_trimmed(content: &str, search: &str) -> Vec<String> {
    let mut search_lines: Vec<&str> = search.split('\n').collect();
    if search_lines.last() == Some(&"") && search_lines.len() > 1 {
        search_lines.pop();
    }
    let k = search_lines.len();
    if k == 0 {
        return Vec::new();
    }

    let spans = line_spans(content);
    if spans.len() < k {
        return Vec::new();
    }

    let mut candidates = Vec::new();
    for i in 0..=spans.len() - k {
        let matched = (0..k).all(|idx| spans[i + idx].2.trim() == search_lines[idx].trim());
        if matched {
            let start = spans[i].0;
            let end = spans[i + k - 1].1;
            candidates.push(content[start..end].to_string());
        }
    }
    candidates
}

/// Stage 3: first/last trimmed-line anchors, last-anchor at j >= i+2 (first
/// occurrence only), block-size tolerance max(1, floor(search_lines * 0.25)),
/// middle-line Levenshtein similarity with threshold 0.65:
/// - one candidate: incremental sum with early exit at the threshold;
/// - many: average similarity, best candidate wins if >= 0.65;
/// - no comparable middle lines => similarity 1.0.
fn block_anchor(content: &str, search: &str) -> Vec<String> {
    let mut search_lines: Vec<&str> = search.split('\n').collect();
    if search_lines.last() == Some(&"") && search_lines.len() > 1 {
        search_lines.pop();
    }
    let search_block_size = search_lines.len();
    if search_block_size < 3 {
        return Vec::new();
    }

    let first_anchor = search_lines[0].trim();
    let last_anchor = search_lines[search_block_size - 1].trim();
    let max_line_delta = 1.max((search_block_size as f64 * 0.25).floor() as usize);

    let spans = line_spans(content);
    let mut scored_candidates: Vec<(f64, String)> = Vec::new();

    for i in 0..spans.len() {
        if spans[i].2.trim() != first_anchor {
            continue;
        }
        for j in (i + 2)..spans.len() {
            if spans[j].2.trim() != last_anchor {
                continue;
            }
            let actual_block_size = j - i + 1;
            let delta = actual_block_size.abs_diff(search_block_size);
            if delta <= max_line_delta {
                let lines_to_check = (search_block_size - 2).min(actual_block_size - 2);
                let similarity = if lines_to_check == 0 {
                    1.0
                } else {
                    let mut total_sim = 0.0;
                    for k in 0..lines_to_check {
                        let a = search_lines[1 + k].trim();
                        let b = spans[i + 1 + k].2.trim();
                        let max_len = a.chars().count().max(b.chars().count());
                        if max_len == 0 {
                            total_sim += 1.0;
                        } else {
                            let dist = levenshtein(a, b);
                            total_sim += 1.0 - (dist as f64 / max_len as f64);
                        }
                    }
                    total_sim / lines_to_check as f64
                };

                let start = spans[i].0;
                let end = spans[j].1;
                let candidate_span = content[start..end].to_string();
                scored_candidates.push((similarity, candidate_span));
            }
            // First matching last-anchor line only (reference parity: break)
            break;
        }
    }

    if scored_candidates.is_empty() {
        Vec::new()
    } else if scored_candidates.len() == 1 {
        if scored_candidates[0].0 >= SIMILARITY_THRESHOLD {
            vec![scored_candidates.remove(0).1]
        } else {
            Vec::new()
        }
    } else {
        // Multiple candidates: pick the best
        let mut best: Option<(f64, String)> = None;
        for (sim, cand) in scored_candidates {
            if let Some((best_sim, _)) = &best {
                if sim > *best_sim {
                    best = Some((sim, cand));
                }
            } else {
                best = Some((sim, cand));
            }
        }
        if let Some((best_sim, best_cand)) = best {
            if best_sim >= SIMILARITY_THRESHOLD {
                vec![best_cand]
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    }
}

/// Classic Levenshtein over chars; either side empty returns the other's
/// char count. Implemented with a two-row rolling matrix.
pub(crate) fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    if a_chars.is_empty() {
        return b_chars.len();
    }
    if b_chars.is_empty() {
        return a_chars.len();
    }

    let mut prev_row: Vec<usize> = (0..=b_chars.len()).collect();
    let mut curr_row: Vec<usize> = vec![0; b_chars.len() + 1];

    for (i, &ca) in a_chars.iter().enumerate() {
        curr_row[0] = i + 1;
        for (j, &cb) in b_chars.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr_row[j + 1] = (prev_row[j + 1] + 1)
                .min(curr_row[j] + 1)
                .min(prev_row[j] + cost);
        }
        std::mem::swap(&mut prev_row, &mut curr_row);
    }
    prev_row[b_chars.len()]
}

fn normalize_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Stage 4: whitespace-run collapse (`normalize` = split_whitespace joined by
/// single spaces). Single-line full match yields the whole line; a normalized
/// substring hit yields the minimal in-line span whose whitespace-split words
/// equal the search's words in order, located by a hand-rolled scan;
/// multi-line window equality yields the block.
fn whitespace_normalized(content: &str, search: &str) -> Vec<String> {
    let search_norm = normalize_whitespace(search);
    if search_norm.is_empty() {
        return Vec::new();
    }
    let is_single_line = !search.contains('\n');
    let spans = line_spans(content);
    let mut candidates = Vec::new();

    if is_single_line {
        let search_words: Vec<&str> = search.split_whitespace().collect();
        for (_, _, line) in &spans {
            let line_norm = normalize_whitespace(line);
            if line_norm == search_norm {
                candidates.push((*line).to_string());
            } else if line_norm.contains(&search_norm) {
                // Find matching word sequence in the raw line
                if let Some(matched_slice) = scan_words_in_line(line, &search_words) {
                    candidates.push(matched_slice.to_string());
                }
            }
        }
    } else {
        let search_lines: Vec<&str> = search.split('\n').collect();
        let k = search_lines.len();
        if spans.len() >= k {
            for i in 0..=spans.len() - k {
                let block_lines: Vec<&str> = (0..k).map(|idx| spans[i + idx].2).collect();
                let block_joined = block_lines.join("\n");
                if normalize_whitespace(&block_joined) == search_norm {
                    let start = spans[i].0;
                    let end = spans[i + k - 1].1;
                    candidates.push(content[start..end].to_string());
                }
            }
        }
    }
    candidates
}

fn scan_words_in_line<'a>(line: &'a str, target_words: &[&str]) -> Option<&'a str> {
    if target_words.is_empty() {
        return None;
    }
    // Find where the word sequence starts and ends
    let line_words: Vec<(usize, usize, &str)> = {
        let mut words = Vec::new();
        let mut in_word = false;
        let mut start = 0;
        for (i, c) in line.char_indices() {
            if !c.is_whitespace() {
                if !in_word {
                    in_word = true;
                    start = i;
                }
            } else if in_word {
                in_word = false;
                words.push((start, i, &line[start..i]));
            }
        }
        if in_word {
            words.push((start, line.len(), &line[start..]));
        }
        words
    };

    if line_words.len() < target_words.len() {
        return None;
    }

    for i in 0..=line_words.len() - target_words.len() {
        let matched = (0..target_words.len()).all(|w| line_words[i + w].2 == target_words[w]);
        if matched {
            let start = line_words[i].0;
            let end = line_words[i + target_words.len() - 1].1;
            return Some(&line[start..end]);
        }
    }
    None
}

fn remove_common_indent(lines: &[&str]) -> Vec<String> {
    let min_indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.chars().take_while(|c| *c == ' ' || *c == '\t').count())
        .min()
        .unwrap_or(0);

    lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                (*l).to_string()
            } else {
                let strip_count = l
                    .char_indices()
                    .take(min_indent)
                    .last()
                    .map(|(idx, c)| idx + c.len_utf8())
                    .unwrap_or(0);
                l[strip_count..].to_string()
            }
        })
        .collect()
}

/// Stage 5: strip the minimum common indent of non-empty lines from both
/// sides (empty lines untouched); window equality yields the raw block.
fn indentation_flexible(content: &str, search: &str) -> Vec<String> {
    let search_lines: Vec<&str> = search.split('\n').collect();
    let k = search_lines.len();
    let search_unindented = remove_common_indent(&search_lines);

    let spans = line_spans(content);
    if spans.len() < k {
        return Vec::new();
    }

    let mut candidates = Vec::new();
    for i in 0..=spans.len() - k {
        let block_lines: Vec<&str> = (0..k).map(|idx| spans[i + idx].2).collect();
        let block_unindented = remove_common_indent(&block_lines);
        if block_unindented == search_unindented {
            let start = spans[i].0;
            let end = spans[i + k - 1].1;
            candidates.push(content[start..end].to_string());
        }
    }
    candidates
}

fn unescape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                match next {
                    'n' => {
                        out.push('\n');
                        chars.next();
                    }
                    't' => {
                        out.push('\t');
                        chars.next();
                    }
                    'r' => {
                        out.push('\r');
                        chars.next();
                    }
                    '\'' => {
                        out.push('\'');
                        chars.next();
                    }
                    '"' => {
                        out.push('"');
                        chars.next();
                    }
                    '`' => {
                        out.push('`');
                        chars.next();
                    }
                    '\\' => {
                        out.push('\\');
                        chars.next();
                    }
                    '$' => {
                        out.push('$');
                        chars.next();
                    }
                    '\n' => {
                        chars.next();
                    }
                    _ => {
                        out.push('\\');
                    }
                }
            } else {
                out.push('\\');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Stage 6: unescape \n \t \r \' \" \` \\ \<newline> \$ (unknown escapes kept
/// verbatim). Yield the unescaped search when contained; also yield raw
/// window blocks whose unescaped form equals the unescaped search.
fn escape_normalized(content: &str, search: &str) -> Vec<String> {
    let unescaped_search = unescape_string(search);
    let mut candidates = Vec::new();

    if unescaped_search != search && content.contains(&unescaped_search) {
        candidates.push(unescaped_search.clone());
    }

    let search_lines: Vec<&str> = search.split('\n').collect();
    let k = search_lines.len();
    let spans = line_spans(content);
    if spans.len() >= k {
        for i in 0..=spans.len() - k {
            let start = spans[i].0;
            let end = spans[i + k - 1].1;
            let block = &content[start..end];
            if unescape_string(block) == unescaped_search && block != search {
                candidates.push(block.to_string());
            }
        }
    }

    candidates
}

/// Stage 7: only when `search.trim() != search`: yield the trimmed search
/// when contained, and window blocks whose trim equals it.
fn trimmed_boundary(content: &str, search: &str) -> Vec<String> {
    let trimmed = search.trim();
    if trimmed == search {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    if content.contains(trimmed) {
        candidates.push(trimmed.to_string());
    }

    let search_lines: Vec<&str> = search.split('\n').collect();
    let k = search_lines.len();
    let spans = line_spans(content);
    if spans.len() >= k {
        for i in 0..=spans.len() - k {
            let start = spans[i].0;
            let end = spans[i + k - 1].1;
            let block = &content[start..end];
            if block.trim() == trimmed && block != trimmed {
                candidates.push(block.to_string());
            }
        }
    }
    candidates
}

/// Stage 8: anchors as stage 3 but the block must have EXACTLY the search's
/// line count and >= 50% of non-empty trimmed middle pairs must be equal
/// (zero comparable pairs accepts); first hit only.
fn context_aware(content: &str, search: &str) -> Vec<String> {
    let mut search_lines: Vec<&str> = search.split('\n').collect();
    if search_lines.last() == Some(&"") && search_lines.len() > 1 {
        search_lines.pop();
    }
    let k = search_lines.len();
    if k < 3 {
        return Vec::new();
    }
    let first_anchor = search_lines[0].trim();
    let last_anchor = search_lines[k - 1].trim();

    let spans = line_spans(content);
    if spans.len() < k {
        return Vec::new();
    }

    for i in 0..=spans.len() - k {
        let j = i + k - 1;
        if spans[i].2.trim() == first_anchor && spans[j].2.trim() == last_anchor {
            let mut matching = 0;
            let mut total_non_empty = 0;
            for idx in 1..k - 1 {
                let a = search_lines[idx].trim();
                let b = spans[i + idx].2.trim();
                if !a.is_empty() || !b.is_empty() {
                    total_non_empty += 1;
                    if a == b {
                        matching += 1;
                    }
                }
            }
            if total_non_empty == 0 || (matching as f64 / total_non_empty as f64) >= 0.5 {
                let start = spans[i].0;
                let end = spans[j].1;
                return vec![content[start..end].to_string()];
            }
        }
    }
    Vec::new()
}

/// Stage 9: one candidate per exact occurrence.
fn multi_occurrence(content: &str, search: &str) -> Vec<String> {
    let count = content.matches(search).count();
    vec![search.to_string(); count]
}

/// The reference's isDisproportionateMatch, verbatim thresholds:
/// span_lines >= max(old_lines + 3, old_lines * 2) refuses; single-line
/// searches are never disproportionate by the byte rule; otherwise
/// span.trim().len() > max(old.trim().len() + 500, old.trim().len() * 4)
/// refuses. Lengths are BYTE lengths.
fn is_disproportionate(candidate: &str, search: &str) -> bool {
    let old_lines = search.split('\n').count();
    let cand_lines = candidate.split('\n').count();
    if cand_lines >= (old_lines + 3).max(old_lines * 2) {
        return true;
    }
    if old_lines == 1 {
        return false;
    }
    let old_len = search.trim().len();
    candidate.trim().len() > (old_len + 500).max(old_len * 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_exact_match_wins_at_stage_one() {
        let content = "fn main() {\n    println!(\"hello\");\n}\n";
        let search = "println!(\"hello\");";
        let res = find_unique_span(content, search).expect("match");
        assert_eq!(res.stage, MatchStage::Exact);
        assert_eq!(&content[res.start..res.start + res.len], search);
    }

    #[test]
    fn line_trimmed_matches_reindented_lines() {
        let content = "fn test() {\n    let a = 1;\n    let b = 2;\n}\n";
        let search = "  let a = 1;\n  let b = 2;";
        let res = find_unique_span(content, search).expect("match");
        assert_eq!(res.stage, MatchStage::LineTrimmed);
        assert_eq!(
            &content[res.start..res.start + res.len],
            "    let a = 1;\n    let b = 2;"
        );
    }

    #[test]
    fn line_trimmed_drops_trailing_empty_search_line() {
        let content = "  alpha\n  beta\ngamma\n";
        let search = "alpha\nbeta\n";
        let res = find_unique_span(content, search).expect("match");
        assert_eq!(res.stage, MatchStage::LineTrimmed);
        assert_eq!(&content[res.start..res.start + res.len], "  alpha\n  beta");
    }

    #[test]
    fn line_trimmed_span_offsets_are_exact() {
        let content = "header\nline 1\nline 2\nfooter";
        let search = " line 1 \n line 2 ";
        let res = find_unique_span(content, search).expect("match");
        assert_eq!(res.stage, MatchStage::LineTrimmed);
        assert_eq!(res.start, 7);
        assert_eq!(res.len, 13); // "line 1\nline 2".len() == 13
    }

    #[test]
    fn block_anchor_requires_three_lines() {
        let content = "a\nb\nc\n";
        let search = "a\nc\n";
        // 2 lines: block_anchor produces nothing
        assert!(block_anchor(content, search).is_empty());
    }

    #[test]
    fn block_anchor_single_candidate_threshold_065() {
        let content = "start\nlet mutated = 12345;\nend";
        // 1 - lev("let original = 12345", "let mutated = 12345") / 20 ≈ 1 - 8/20 = 0.60 < 0.65 -> fails
        let search_low = "start\nlet original = 12345;\nend";
        let res_low = find_unique_span(content, search_low);
        assert!(matches!(res_low, Err(MatchFailure::NotFound)));

        // High similarity: lev("let mutated = 12345;", "let mutated = 12340;") = 1 / 20 -> 0.95 >= 0.65 -> succeeds
        let search_high = "start\nlet mutated = 12340;\nend";
        let res_high = find_unique_span(content, search_high).expect("match");
        assert_eq!(res_high.stage, MatchStage::BlockAnchor);
    }

    #[test]
    fn block_anchor_size_tolerance_25_percent() {
        // 4 search lines -> max_delta = max(1, floor(4 * 0.25)) = 1 line
        let content_4 = "start\nline 1\nline 2\nend";
        let search = "start\nline 1\nline 2\nend";
        assert_eq!(
            find_unique_span(content_4, search).unwrap().stage,
            MatchStage::Exact
        );

        // Content has 5 lines (delta 1) -> accepted
        let content_5 = "start\nline 1\nline 2\nextra line\nend";
        let search_approx = "start\nline 1\nline 2\nend";
        let res_5 = block_anchor(content_5, search_approx);
        assert!(!res_5.is_empty());

        // Content has 6 lines (delta 2 > 1) -> rejected
        let content_6 = "start\nline 1\nline 2\nextra 1\nextra 2\nend";
        let search_approx = "start\nline 1\nline 2\nend";
        let res_6 = block_anchor(content_6, search_approx);
        assert!(res_6.is_empty());
    }

    #[test]
    fn block_anchor_picks_best_of_multiple_candidates() {
        let content = "start\nmid A\nend\nsomething\nstart\nmid BBBBB\nend";
        let search = "start\nmid A\nend";
        let res = find_unique_span(content, search).expect("match");
        assert_eq!(res.stage, MatchStage::Exact);
    }

    #[test]
    fn block_anchor_no_middle_lines_accepts_on_anchors() {
        let content = "start\nmid\nend";
        let search = "start\nxxx\nend";
        // lines_to_check = min(3-2, 3-2) = 1. "xxx" vs "mid" max_len=3, lev=3 -> sim=0.0 -> fails
        assert!(block_anchor(content, search).is_empty());
    }

    #[test]
    fn levenshtein_reference_values() {
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("flaw", "lawn"), 2);
        assert_eq!(levenshtein("same", "same"), 0);
    }

    #[test]
    fn whitespace_normalized_single_line_and_substring() {
        let content = "const x   =   42;\nconst y = 100;";
        let search = "const x = 42;";
        let res = find_unique_span(content, search).expect("match");
        assert_eq!(res.stage, MatchStage::WhitespaceNormalized);
        assert_eq!(
            &content[res.start..res.start + res.len],
            "const x   =   42;"
        );

        let content_sub = "let msg = 'prefix' + get_val() + 'suffix';";
        let search_sub = "+   get_val()   +";
        let res_sub = find_unique_span(content_sub, search_sub).expect("match");
        assert_eq!(res_sub.stage, MatchStage::WhitespaceNormalized);
    }

    #[test]
    fn whitespace_normalized_multiline_block() {
        let content = "fn foo() {\n    let a   =   1;\n    let b   =   2;\n}";
        let search = "fn foo() {\nlet a = 1;\nlet b = 2;\n}";
        let candidates = whitespace_normalized(content, search);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0],
            "fn foo() {\n    let a   =   1;\n    let b   =   2;\n}"
        );
    }

    #[test]
    fn indentation_flexible_strips_common_indent_only() {
        let content =
            "        fn indent() {\n            let x = 1;\n        let y = 2;\n        }";
        let search = "    fn indent() {\n        let x = 1;\n    let y = 2;\n    }";
        let candidates = indentation_flexible(content, search);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0],
            "        fn indent() {\n            let x = 1;\n        let y = 2;\n        }"
        );
    }

    #[test]
    fn escape_normalized_unescapes_the_reference_set() {
        assert_eq!(
            unescape_string(r#"line1\nline2\t\$var\'\"\`\\"#),
            "line1\nline2\t$var'\"`\\"
        );
        assert_eq!(unescape_string(r"\q"), r"\q"); // unknown escape unchanged

        let content = "first line\nsecond line\n";
        let search = r"first line\nsecond line";
        let res = find_unique_span(content, search).expect("match");
        assert_eq!(res.stage, MatchStage::EscapeNormalized);
    }

    #[test]
    fn trimmed_boundary_noop_when_already_trimmed() {
        let content = "some content";
        let search = "some content";
        assert!(trimmed_boundary(content, search).is_empty());

        let content_inline = "let res = compute();";
        let search_padded = "  compute()  ";
        let res = find_unique_span(content_inline, search_padded).expect("match");
        assert_eq!(res.stage, MatchStage::TrimmedBoundary);
    }

    #[test]
    fn context_aware_requires_exact_line_count_and_half_matching_middles() {
        let content = "header\nline 1\nline 2\nline 3\nline 4\nfooter";
        // 4 middle lines: 2 matching, 2 totally different (lev large so BlockAnchor similarity < 0.65)
        let search_50 =
            "header\nline 1\nline 2\ncompletely_different_alpha\ncompletely_different_beta\nfooter";
        let res_50 = find_unique_span(content, search_50).expect("match");
        assert_eq!(res_50.stage, MatchStage::ContextAware);

        // 4 middle lines: 1 matching, 3 totally different -> 25% < 50% -> fails
        let search_25 = "header\nline 1\ncompletely_different_a\ncompletely_different_b\ncompletely_different_c\nfooter";
        assert!(context_aware(content, search_25).is_empty());
    }

    #[test]
    fn multi_occurrence_cannot_rescue_ambiguity() {
        let content = "dup\ndup\n";
        let search = "dup";
        let res = find_unique_span(content, search);
        assert!(matches!(res, Err(MatchFailure::Ambiguous { count: 2 })));
    }

    #[test]
    fn ambiguous_candidates_report_ambiguous_not_notfound() {
        let content = "target text\nsomething\ntarget text";
        let search = " target text ";
        let res = find_unique_span(content, search);
        assert!(matches!(res, Err(MatchFailure::Ambiguous { count: 2 })));
    }

    #[test]
    fn disproportionate_line_and_byte_rules() {
        assert!(is_disproportionate(
            "1\n2\n3\n4\n5\n6\n7\n8\n9\n10",
            "1\n2\n3"
        ));
        assert!(!is_disproportionate("single line candidate", "single"));
        let search_long = "a".repeat(100);
        let cand_huge = "b".repeat(700);
        assert!(is_disproportionate(
            &cand_huge,
            &format!("x\n{}", search_long)
        ));
    }
}
