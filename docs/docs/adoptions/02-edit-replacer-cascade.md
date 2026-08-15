# Adoption 02 — Nine-Stage Edit-Replacer Cascade

**Effort:** S · **Depends on:** nothing · **Reference:** `reference-repos/opencode/packages/opencode/src/tool/edit.ts` (lines 217–737)
**Ported from:** opencode (which credits Cline's diff-apply evals and gemini-cli's editCorrector) · **Status:** ⬜ not started

## 1. Summary

opencode's edit tool almost never fails on "search text not found": before giving up it runs the model's `oldString` through a nine-stage cascade of progressively fuzzier matchers — trimmed-line comparison, anchor-line matching with Levenshtein-scored middles, whitespace/indentation/escape normalization — each of which proposes candidate spans that are then re-verified against the real file, required to be **unique**, and refused when the matched span is disproportionately larger than what the model asked for. codypendent's `workspace.edit_file` is exact-match-only today, so a single stray space or a model that writes `\n` where the file has real newlines fails the whole call. This adoption ports the cascade — every algorithm and threshold exactly — as fallback stages behind the existing byte-exact match, while preserving `edit_file`'s contract completely: same containment boundary, same sequential-against-the-evolving-buffer semantics, same all-or-nothing atomicity, same error taxonomy (plus one new refusal variant), and byte-identical behavior whenever the exact match is unique.

## 2. Reference implementation

`reference-repos/opencode/packages/opencode/src/tool/edit.ts`. The tool's `execute` normalizes line endings, takes a per-file semaphore, and calls `replace(content, oldString, newString, replaceAll)` (line 682). `replace` iterates the replacers **in order**; each is a generator yielding candidate span strings; for each candidate:

- `content.indexOf(search)` — candidate not actually present → try next candidate.
- present → `notFound = false`; **`isDisproportionateMatch(search, oldString)` → throw immediately** ("Refusing replacement because the matched span is much larger than oldString…", line 709).
- `replaceAll` → replace every occurrence and return.
- occurrence not unique (`index !== lastIndex`) → try next candidate/stage.
- unique → splice `newString` in and return.

Exhausted: `notFound` → "Could not find oldString in the file. It must match exactly, including whitespace, indentation, and line endings."; else "Found multiple matches for oldString. Provide more surrounding context to make the match unique." (lines 723–728).

**The nine replacers, in cascade order:**

1. **`SimpleReplacer`** (line 244) — yields `find` itself (the exact match).
2. **`LineTrimmedReplacer`** (line 248) — split content and find into lines (drop a trailing empty find line); slide a window of `searchLines.len()` lines over the content; match iff every line pair is equal **after `trim()`**; yield the original (untrimmed) span, computed by summing line lengths + 1 per newline (no `+1` after the window's last line).
3. **`BlockAnchorReplacer`** (line 288) — requires ≥ 3 find lines (drop trailing empty). Anchors: first and last find lines, trimmed. Candidates: every content line `i` equal (trimmed) to the first anchor, paired with the **first** subsequent line `j ≥ i+2` equal (trimmed) to the last anchor, accepted only if `|actualBlockSize − searchBlockSize| ≤ maxLineDelta` where `maxLineDelta = max(1, floor(searchBlockSize · 0.25))` — the **25% block-size tolerance**. Then:
   - **Single candidate**: score the middle lines — per line, `similarity += (1 − levenshtein(a,b)/maxLen) / linesToCheck` over trimmed pairs (`linesToCheck = min(searchBlockSize−2, actualBlockSize−2)`; a `maxLen == 0` pair is skipped), with an early exit as soon as the running sum reaches the threshold; no middle lines at all ⇒ `similarity = 1.0`. Accept iff `similarity ≥ 0.65` (`SINGLE_CANDIDATE_SIMILARITY_THRESHOLD`, line 220).
   - **Multiple candidates**: compute each candidate's **average** middle-line similarity (sum of `1 − distance/maxLen`, divided by `linesToCheck`; no middles ⇒ 1.0); pick the best; accept iff `best ≥ 0.65` (`MULTIPLE_CANDIDATES_SIMILARITY_THRESHOLD`, line 221).
   - Yield the anchored span (start-of-line `i` through end-of-line `j`).
   `levenshtein` (line 226) is the classic full-matrix distance; empty-string shortcut returns the other's length.
4. **`WhitespaceNormalizedReplacer`** (line 427) — `normalize(s) = s.replace(/\s+/g," ").trim()`. Single-line pass: a content line whose normalization equals `normalize(find)` yields the whole line; otherwise if the normalized line *contains* the normalized find, build a regex from `find`'s whitespace-split words (each regex-escaped) joined by `\s+`, and yield the first in-line match. Multi-line pass: slide a `findLines.len()` window; yield blocks whose joined normalization equals the normalized find.
5. **`IndentationFlexibleReplacer`** (line 471) — `removeIndentation` strips the **minimum common indent** of non-empty lines (empty lines untouched); slide a window of `findLines.len()`; yield blocks where `removeIndentation(block) == removeIndentation(find)`.
6. **`EscapeNormalizedReplacer`** (line 499) — `unescape` maps `\n \t \r \' \" \` \\ \<newline> \$` to their literal characters (anything else stays). If `content` contains `unescape(find)`, yield it. Also slide a window and yield raw blocks whose *unescaped* form equals the unescaped find (handles content that itself contains escaped sequences).
7. **`TrimmedBoundaryReplacer`** (line 562) — if `find.trim() == find`, yield nothing. Else yield `find.trim()` when contained in content, and any window block whose `trim()` equals the trimmed find.
8. **`ContextAwareReplacer`** (line 588) — requires ≥ 3 find lines (drop trailing empty). Same first/last trimmed anchors as BlockAnchor (`j ≥ i+2`, first matching last line only), but the block must have **exactly** `findLines.len()` lines; accept when ≥ 50% of the non-empty trimmed middle-line pairs are equal (`matchingLines / totalNonEmptyLines ≥ 0.5`; zero comparable pairs ⇒ accept); yield the block and stop after the first hit.
9. **`MultiOccurrenceReplacer`** (line 548) — yields `find` once per exact occurrence; exists for `replaceAll` semantics (it cannot make a non-unique match unique).

**`isDisproportionateMatch(search, oldString)`** (line 731) — the safety valve:

```
oldLines   = oldString.split("\n").length
matchLines = search.split("\n").length
if matchLines >= max(oldLines + 3, oldLines * 2)        → disproportionate
if oldLines == 1                                        → not disproportionate
if search.trim().length > max(oldString.trim().length + 500,
                              oldString.trim().length * 4) → disproportionate
```

## 3. Current state in codypendent (verified)

**`crates/runtime/src/tools/edit_file.rs`** (631 lines) — the whole destination:

- `FileEdit { search, replace }`, `EditFileInput { path, edits }`, `EditFileOutcome { path, edits_applied }` with `observation()` = `"applied N edit(s) to <path>"`.
- `EditFile::execute(input, scope: &PathScope)` runs in `spawn_blocking`: `secure_fs::open_edit` (resolve-once + leaf-swap guard), `MAX_EDIT_BYTES = 64 MiB` cap, mandatory UTF-8, then per edit **in order against the evolving buffer**: empty search → `ToolError::EmptySearch`; `buffer.matches(search).count()` — `0` → `SearchNotFound { path, index }`, `>1` → `SearchAmbiguous { path, index, count }`, `1` → `replace_range`. Whole result computed in memory; **one** write only after every edit matched (atomicity). The module doc calls this the "exact, unique, sequential, atomic" contract and the containment section ("no-TOCTOU seam", leaf-swap guard) the security boundary — neither may change.
- Tests (in-module, `#[tokio::test]` + `tempfile`): unique replace, not-found leaves file unchanged, ambiguity leaves file unchanged, empty search, sequential-evolving-buffer, atomicity on later failure, scope escape/deny, leaf symlink/directory refusal, size cap, plus `parse_edit_file` cases. All must keep passing (one observation-string assertion may extend, see §5).
- `parse_edit_file` (line 193) validates `path` + non-empty `edits` with non-empty `search` strings.

**`crates/runtime/src/tools/mod.rs`** — `ToolError` (line 144) with the edit variants above and `code()` mapping (`tool.search-not-found`, `tool.search-ambiguous`, `tool.empty-search`, `tool.file-too-large`, …). New variants slot in here.

**`crates/runtime/src/agent.rs`** — dispatch: `EditFile::NAME` prepare arm at ~line 3506 (`parse_edit_file(args, &run.worktree)` roots relative paths at the run worktree, line 5994), execution at ~line 4135 (`EditFile::execute(&input, &write_scope)` → `(outcome.observation(), None, ToolOutcome::Succeeded)` or `("workspace.edit_file error: {e}", …, Failed { message: e.code() })`). Nothing here changes except that the richer errors/observations flow through automatically.

**No `replaceAll`** exists anywhere in the tool's schema — codypendent's contract is exactly-once per edit.

## 4. Design

The cascade becomes a **pure matching module** consulted by `EditFile::execute` in place of the raw `matches().count()` block. Nothing else moves: containment, the 64 MiB cap, UTF-8, sequential application, single-write atomicity, and the parse layer are untouched.

```
for each edit (in order, against the evolving buffer):
    find_unique_span(&buffer, &edit.search)
        stage 1 Exact            == today's byte-exact match
        stage 2 LineTrimmed
        stage 3 BlockAnchor      (Levenshtein ≥ 0.65, 25% size tolerance)
        stage 4 WhitespaceNormalized
        stage 5 IndentationFlexible
        stage 6 EscapeNormalized
        stage 7 TrimmedBoundary
        stage 8 ContextAware     (≥50% middle-line agreement)
        stage 9 MultiOccurrence
    Ok(span)                      → replace_range (count fuzzy stages)
    Err(NotFound)                 → ToolError::SearchNotFound      (unchanged)
    Err(Ambiguous{count})         → ToolError::SearchAmbiguous     (unchanged)
    Err(Disproportionate)         → ToolError::DisproportionateMatch (new)
```

**Contract preservation, spelled out:**

- A search that matches byte-exactly **exactly once** is handled by stage 1 identically to today — same span, same result, zero behavior change.
- A byte-exact **ambiguous** search still fails `SearchAmbiguous`: candidates are re-verified for uniqueness *as strings against the whole buffer* (the reference's `indexOf != lastIndexOf` check), so a fuzzy stage that "finds" a span whose text occurs twice stays ambiguous. The reported `count` is the exact-stage count when > 0, else the first ambiguous candidate's count — the model-facing guidance ("include more surrounding context") is unchanged and still correct.
- Atomicity: the cascade is pure; failures happen before any byte is written, exactly as today.
- The observation stays honest: fuzzy-applied edits are disclosed (`"applied 3 edit(s) to <path> (1 via fuzzy match)"`), never passed off as exact.

**Deviations from the reference, and why:**

1. **No `replaceAll`** — codypendent's tool has no such parameter; `MultiOccurrenceReplacer` is ported for order fidelity but is inert (its candidates equal stage 1's and can never be unique when stage 1 wasn't). Kept so a future `replace_all` lands in a cascade that already has its stage.
2. **No CRLF/BOM normalization pre-pass** (edit.ts lines 129–134 convert `oldString` to the file's dominant ending and re-attach BOMs). codypendent's write tools have no BOM handling anywhere; introducing it here would silently rewrite files. The trimmed/normalized stages absorb most CRLF mismatches anyway (`str::trim` removes `\r`). See gotcha 3.
3. **Uniqueness counts non-overlapping occurrences** (`str::matches`), matching today's `edit_file` semantics, where the reference's `indexOf != lastIndexOf` also catches *overlapping* repeats (`"aa"` in `"aaa"`). Preserving the existing contract wins over reference fidelity here — this exact behavior is already shipped and tested.
4. **Levenshtein runs over `char`s** (Unicode scalar values), not JS UTF-16 units; distances differ only for astral-plane text, always in the more-conservative direction.
5. **Disproportionate refusal is a typed `ToolError`**, not a thrown string, keeping the stable `code()` contract for `ToolCompleted` payloads.
6. **The regex in `WhitespaceNormalizedReplacer` is replaced by a hand-rolled whitespace-flexible scanner** — `regex` is not a direct workspace dependency and the pattern (escaped words joined by `\s+`) is trivially expressible as a scan.

## 5. Changes, file by file

### 5.1 `crates/runtime/src/tools/edit_match.rs` (new)

The pure cascade. `pub(crate)` — an implementation detail of `edit_file`, exported only for that module and its tests. Names below are normative.

```rust
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

pub(crate) fn find_unique_span(content: &str, search: &str)
    -> Result<MatchResult, MatchFailure>
{
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
            let Some(start) = content.find(candidate.as_str()) else { continue };
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
            return Ok(MatchResult { start, len: candidate.len(), stage: *stage });
        }
    }
    if !found_any {
        Err(MatchFailure::NotFound)
    } else {
        Err(MatchFailure::Ambiguous { count: first_ambiguous.unwrap_or(2) })
    }
}
```

**Stage implementations** (each `fn(&str, &str) -> Vec<String>`, candidates in discovery order; every algorithm as specified in §2 — the thresholds repeated here are normative):

```rust
/// Stage 1: the search string itself.
fn simple(_content: &str, search: &str) -> Vec<String> {
    vec![search.to_string()]
}

/// Stage 2: window of lines equal after `str::trim`. Drops a trailing empty
/// search line. Spans reconstructed from the ORIGINAL lines (untrimmed).
fn line_trimmed(content: &str, search: &str) -> Vec<String>;

/// Stage 3: first/last trimmed-line anchors, last-anchor at j >= i+2 (first
/// occurrence only), block-size tolerance max(1, floor(search_lines * 0.25)),
/// middle-line Levenshtein similarity with threshold 0.65:
/// - one candidate: incremental sum with early exit at the threshold;
/// - many: average similarity, best candidate wins if >= 0.65.
/// No comparable middle lines => similarity 1.0.
fn block_anchor(content: &str, search: &str) -> Vec<String>;

/// Classic Levenshtein over chars; either side empty returns the other's
/// char count. Implemented with a two-row rolling matrix (the reference's
/// full matrix is O(a*b) memory for no benefit).
fn levenshtein(a: &str, b: &str) -> usize;

/// Stage 4: whitespace-run collapse (`normalize` = split_whitespace joined by
/// single spaces). Single-line full match yields the whole line; a normalized
/// substring hit yields the minimal in-line span whose whitespace-split words
/// equal the search's words in order, located by a hand-rolled scan (NOT a
/// regex — see §4.6); multi-line window equality yields the block.
fn whitespace_normalized(content: &str, search: &str) -> Vec<String>;

/// Stage 5: strip the minimum common indent of non-empty lines from both
/// sides (empty lines untouched); window equality yields the raw block.
fn indentation_flexible(content: &str, search: &str) -> Vec<String>;

/// Stage 6: unescape \n \t \r \' \" \` \\ \<newline> \$ (unknown escapes kept
/// verbatim). Yield the unescaped search when contained; also yield raw
/// window blocks whose unescaped form equals the unescaped search.
fn escape_normalized(content: &str, search: &str) -> Vec<String>;

/// Stage 7: only when `search.trim() != search`: yield the trimmed search
/// when contained, and window blocks whose trim equals it.
fn trimmed_boundary(content: &str, search: &str) -> Vec<String>;

/// Stage 8: anchors as stage 3 but the block must have EXACTLY the search's
/// line count and >= 50% of non-empty trimmed middle pairs must be equal
/// (zero comparable pairs accepts); first hit only.
fn context_aware(content: &str, search: &str) -> Vec<String>;

/// Stage 9: one candidate per exact occurrence (reference parity; inert
/// without a replace-all mode — see the module docs).
fn multi_occurrence(content: &str, search: &str) -> Vec<String>;

/// The reference's isDisproportionateMatch, verbatim thresholds:
/// span_lines >= max(old_lines + 3, old_lines * 2) refuses; single-line
/// searches are never disproportionate by the byte rule; otherwise
/// span.trim().len() > max(old.trim().len() + 500, old.trim().len() * 4)
/// refuses. Lengths are BYTE lengths (deviation §4.4 noted in a comment).
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
```

Implementation notes that are part of the port (mined from the reference):

- Line-window span reconstruction: `start = Σ (len(line_k) + 1) for k < i`; `end = start + Σ len(line) + (window_len − 1)` newlines — i.e. **no trailing newline** in the span. In Rust, prefer collecting `(byte_offset, line)` pairs once (`content.split('\n')` with a running offset) and slicing `content[start..end]`, then pushing `content[start..end].to_string()`; do not re-join lines (that would corrupt spans if the file ends without a newline).
- Stages 2, 3, 8 drop a trailing empty search line (`search.split('\n')` ending in `""`); stages 4–7 do not.
- `BlockAnchor` and `ContextAware` take the **first** matching last-anchor line per start anchor and stop scanning that start (reference `break` semantics) — port the breaks exactly; widening them changes which candidate wins.
- `whitespace_normalized`'s in-line scanner: for each start position in the line where the first word matches at a word boundary, greedily match subsequent words separated by ≥ 1 whitespace char; yield the exact matched slice. First match per line only (regex `.match` parity).

### 5.2 `crates/runtime/src/tools/edit_file.rs`

1. Module docs: retitle the match-semantics section to "exact first, cascade fallback, unique, sequential, atomic" and document the stages + disclosure rule; the containment section is untouched.
2. Register the sibling module is not needed here (see §5.4 for `mod.rs`); `edit_file.rs` gains `use super::edit_match::{self, MatchFailure, MatchStage};`.
3. `EditFileOutcome` gains a field:

```rust
pub struct EditFileOutcome {
    pub path: PathBuf,
    pub edits_applied: usize,
    /// How many of the applied edits were matched by a fallback stage rather
    /// than byte-exactly. Disclosed in `observation()` — a fuzzy apply is
    /// never passed off as exact.
    pub fuzzy_matches: usize,
}

impl EditFileOutcome {
    #[must_use]
    pub fn observation(&self) -> String {
        let base = format!(
            "applied {} edit(s) to {}",
            self.edits_applied, self.path.display()
        );
        if self.fuzzy_matches == 0 {
            base
        } else {
            format!("{base} ({} via fuzzy match)", self.fuzzy_matches)
        }
    }
}
```

4. The per-edit block inside `execute` (currently lines 150–175) becomes:

```rust
let mut fuzzy_matches = 0usize;
for (zero_based, edit) in edits.iter().enumerate() {
    let index = zero_based + 1;
    if edit.search.is_empty() {
        return Err(ToolError::EmptySearch { index });
    }
    match edit_match::find_unique_span(&buffer, &edit.search) {
        Ok(m) => {
            if m.stage != MatchStage::Exact {
                fuzzy_matches += 1;
            }
            buffer.replace_range(m.start..m.start + m.len, &edit.replace);
        }
        Err(MatchFailure::NotFound) => {
            return Err(ToolError::SearchNotFound { path: scoped.path.clone(), index })
        }
        Err(MatchFailure::Ambiguous { count }) => {
            return Err(ToolError::SearchAmbiguous {
                path: scoped.path.clone(), index, count,
            })
        }
        Err(MatchFailure::Disproportionate) => {
            return Err(ToolError::DisproportionateMatch {
                path: scoped.path.clone(), index,
            })
        }
    }
}
```

and the success value carries `fuzzy_matches`.

### 5.3 `crates/runtime/src/tools/mod.rs`

1. `mod edit_match;` alongside `mod edit_file;` (not re-exported publicly).
2. New `ToolError` variant (with the edit family) and code:

```rust
/// `workspace.edit_file`: the closest match the fallback cascade found spans
/// far more text than the search — replacing it could destroy text the
/// model never saw. Nothing is written.
#[error(
    "edit {index}: the closest match spans far more text than the search — \
     re-read the file and provide the full exact text to replace"
)]
DisproportionateMatch {
    /// The file being edited.
    path: PathBuf,
    /// 1-based index of the failing edit.
    index: usize,
},
```

```rust
ToolError::DisproportionateMatch { .. } => "tool.disproportionate-match",
```

### 5.4 `crates/runtime/src/agent.rs`

No code change. The `EditFile` prepare arm (~line 3506) and execute arm (~line 4135) flow the new observation and error strings through unchanged. Verify only that no test in `agent.rs`/`agent_it.rs` asserts the old observation string for a fuzzy case (none does today — fuzzy cases could not previously succeed).

### 5.5 Dependencies

None. The cascade is std-only (hand-rolled Levenshtein and scanner; no `regex`, no external Levenshtein crate).

## 6. Protocol & persistence

None. `workspace.edit_file`'s wire schema (`path` + `edits[{search,replace}]`) is unchanged; the observation and the new `tool.disproportionate-match` code travel through the existing `ToolCompleted { outcome: ToolOutcome::Failed { message } }` shape. No new events, no ledger kinds, no SQLite migrations.

## 7. Acceptance criteria

1. Byte-exact unique searches behave identically to before: every pre-existing test in `edit_file.rs` passes without modification of its assertions (the observation string for exact-only edits is unchanged).
   RUN: `cargo test -p codypendent-runtime edit_file` EXPECT: pass.
2. A search differing from the file only in per-line leading/trailing whitespace is applied (LineTrimmed), and the replacement preserves the file's original surrounding text exactly outside the span.
3. A ≥ 3-line search whose first and last lines match but whose middles drifted ≤ 35% (Levenshtein similarity ≥ 0.65) is applied (BlockAnchor); one that drifted beyond the threshold, or whose block size differs by more than 25% of the search's line count, is not.
4. A search with collapsed internal whitespace (WhitespaceNormalized), a uniformly re-indented search (IndentationFlexible), a search with literal `\n`/`\t` escapes where the file has real newlines/tabs (EscapeNormalized), and a search with stray leading/trailing blank padding (TrimmedBoundary) are each applied by their stage.
5. A candidate span that occurs twice in the buffer fails `SearchAmbiguous` with `count ≥ 2`; nothing is written (atomicity test extended to a fuzzy case).
6. A single-line search whose only candidate spans ≥ `max(old+3, old·2)` lines fails with `tool.disproportionate-match` and nothing is written.
7. Fuzzy application is disclosed: an input whose edit 1 matches exactly and edit 2 matches via a fallback stage yields `observation()` ending in `"(1 via fuzzy match)"`; `EditFileOutcome::fuzzy_matches == 1`.
8. Sequential semantics hold across stages: an edit whose search only exists (fuzzily) in text produced by the previous edit applies.
9. `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace` EXPECT: green.

## 8. Tests

**`crates/runtime/src/tools/edit_match.rs`** (inline `#[cfg(test)]`, plain `#[test]` — the module is pure):

- `simple_exact_match_wins_at_stage_one` — asserts `stage == Exact` and the exact byte range.
- `line_trimmed_matches_reindented_lines` / `line_trimmed_drops_trailing_empty_search_line` / `line_trimmed_span_offsets_are_exact` (span reconstruction with and without trailing newline in the file).
- `block_anchor_requires_three_lines`, `block_anchor_single_candidate_threshold_065` (one case just above, one just below the similarity floor), `block_anchor_size_tolerance_25_percent` (delta of exactly `max(1, floor(n·0.25))` accepted; +1 rejected), `block_anchor_picks_best_of_multiple_candidates`, `block_anchor_no_middle_lines_accepts_on_anchors`.
- `levenshtein_reference_values` — `("", "abc") == 3`, `("kitten","sitting") == 3`, `("flaw","lawn") == 2`.
- `whitespace_normalized_single_line_and_substring`, `whitespace_normalized_multiline_block`.
- `indentation_flexible_strips_common_indent_only` (empty lines untouched; ragged extra indent on one line does not match).
- `escape_normalized_unescapes_the_reference_set` — each of `\n \t \r \' \" \` \\ \$` plus an unknown escape (`\q`) staying literal.
- `trimmed_boundary_noop_when_already_trimmed`.
- `context_aware_requires_exact_line_count_and_half_matching_middles` (49% fails, 50% passes; zero comparable middles passes).
- `multi_occurrence_cannot_rescue_ambiguity` — repeated text still ends `Ambiguous`.
- `ambiguous_candidates_report_ambiguous_not_notfound` — `first_ambiguous` count surfaces.
- `disproportionate_line_and_byte_rules` — all three branches of `is_disproportionate`, boundary values (`old+3` vs `old·2` crossover; the 500-byte and 4× byte rules; single-line exemption).

**`crates/runtime/src/tools/edit_file.rs`** (extend the existing `#[tokio::test]` + `tempfile` idiom):

- `fuzzy_line_trimmed_edit_applies_and_is_disclosed` — AC 2 + AC 7 (observation suffix, `fuzzy_matches`).
- `fuzzy_match_that_is_ambiguous_fails_and_writes_nothing` — AC 5, asserting the file's bytes are untouched.
- `disproportionate_match_is_refused_and_writes_nothing` — AC 6, asserting `ToolError::DisproportionateMatch` and error `code()`.
- `exact_ambiguity_still_reports_exact_count` — `"aa bb aa"` search `"aa"` still yields `SearchAmbiguous { count: 2 }` (contract preservation).
- `cascade_respects_the_evolving_buffer` — AC 8.

## 9. Gotchas

1. **Candidate uniqueness is checked against the WHOLE buffer as a string**, not "the stage found one window". A stage can discover a single window whose text occurs elsewhere too — that must stay ambiguous, or the edit lands at `content.find`'s first occurrence, which may not be the window the stage scored. This is why `find_unique_span` re-verifies with `content.matches(...)` instead of trusting stage-local positions.
2. **Disproportionate aborts the whole cascade immediately** (reference `throw`), even though a later stage might have found a proportionate span. Do not soften it to `continue` — the valve exists because a tiny `oldString` (e.g. `}`) can anchor-match an entire function body and silently delete it.
3. **CRLF files**: without the reference's line-ending pre-pass, a `\n`-only search against a CRLF file fails stage 1; `line_trimmed` rescues most cases (trim removes `\r`) but the replacement is written with whatever endings `replace` carries — a fuzzy edit can therefore mix endings within a CRLF file. Documented limitation; full CRLF/BOM preservation is out of scope (§10).
4. **Byte offsets, not char offsets**: `replace_range` and `MatchResult.start/len` are byte indices; every span must be produced by slicing `content` (guaranteed char-boundary-safe), never by re-joining lines or arithmetic on char counts.
5. **Performance cliff**: stages 2–9 only run when the exact match fails, but the file may be up to `MAX_EDIT_BYTES` (64 MiB) and `block_anchor` is O(candidates × middle-lines × line-len²) in Levenshtein. The 25% size tolerance and the first-last-anchor requirement bound candidates in practice; still, keep `levenshtein` two-row and early-exit the single-candidate loop at the threshold exactly as the reference does. If profiling ever shows a cliff, cap Levenshtein input lines at a few KiB before considering algorithmic changes.
6. **Trailing-empty-line pops apply only to stages 2/3/8** — applying them to stage 5 or 6 changes window sizes and breaks reference parity (the reference deliberately doesn't pop there).
7. **`is_disproportionate` runs on the CANDIDATE, compared to the SEARCH** — not vice versa. Swapping the arguments inverts the valve.
8. **`spawn_blocking` stays**: the cascade is CPU-bound string work over a possibly-64 MiB buffer; it must remain inside the existing `spawn_blocking` closure, not migrate to the async context.
9. **Do not let the cascade rescue an empty search** — `EmptySearch` is checked before matching, at both parse and execute time; several stages would otherwise happily "match" an empty string everywhere.
10. **Stage order is behavior.** LineTrimmed before BlockAnchor means a fully trim-equal block never reaches the similarity scorer; reordering changes which span (and which byte range) wins on real inputs. The `CASCADE` table's order is normative.

## 10. Out of scope

- A `replace_all` parameter (and with it, MultiOccurrence becoming meaningful).
- CRLF/BOM detection and preservation (opencode's `Bom.split/join`, `detectLineEnding`, `convertToLineEnding`).
- Per-file locking/semaphores (codypendent runs one tool at a time per run; the daemon-level story is separate).
- Rendering diffs into approvals/metadata (opencode attaches the diff to its permission ask — codypendent's approval flow is a different subsystem).
- Post-edit formatting hooks and post-edit LSP diagnostics (the latter is Adoption 10).
