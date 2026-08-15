//! Observation compaction, Level 1 (Chapter 09).
//!
//! Large command output never enters model context whole. It is compacted to a
//! [`SalientView`]: the command, its exit code and duration, and — per stream —
//! the first and last lines, any error-matching lines, and the [`ArtifactRef`]
//! of the full output. The model reads this; if it needs more it rehydrates from
//! the artifact.

use codypendent_protocol::ArtifactRef;

use super::IN_MEMORY_CAP;

/// Lines kept from the head of a stream.
const SALIENT_HEAD: usize = 40;
/// Lines kept from the tail of a stream.
const SALIENT_TAIL: usize = 40;
/// A single salient line is clamped to this many bytes so one pathological line
/// cannot bloat the compacted view.
const SALIENT_MAX_LINE_LEN: usize = 2048;
/// Case-insensitive substrings that mark a line as salient regardless of its
/// position (Chapter 09 / STEP 1.7 rule 4).
const ERROR_MARKERS: [&str; 5] = ["error", "warning", "panic", "failed", "fatal"];
/// Cap on error-matching lines kept beyond head/tail. Without it a failing
/// `cargo build` (thousands of `error:`/`warning:` lines) would put *every* one
/// into the "compacted" view — which is re-sent to the model every step — so the
/// salient view could balloon to tens of MB exactly when the build is broken.
const SALIENT_MAX_ERROR_LINES: usize = 200;

/// The compacted, model-facing view of one command execution.
#[derive(Debug, Clone)]
pub struct SalientView {
    /// The command as `program arg arg …`.
    pub command: String,
    /// Process exit code, or `None` if the process was killed (e.g. on timeout).
    pub exit_code: Option<i32>,
    /// Whether the command was killed for exceeding its timeout.
    pub timed_out: bool,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u128,
    /// Compacted standard output.
    pub stdout: SalientStream,
    /// Compacted standard error.
    pub stderr: SalientStream,
}

/// The compacted view of a single output stream.
#[derive(Debug, Clone)]
pub struct SalientStream {
    /// Head + tail + error-matching lines, in original order, with
    /// `… N lines omitted …` markers where the selection is not contiguous.
    pub lines: Vec<String>,
    /// Total number of lines in the captured output.
    pub total_lines: usize,
    /// Bytes captured (== full length unless `overflowed`).
    pub captured_bytes: usize,
    /// Whether output exceeded [`MAX_CAPTURE_BYTES`](super::MAX_CAPTURE_BYTES)
    /// and the tail was dropped from capture.
    pub overflowed: bool,
    /// Whether any lines were omitted from `lines` (i.e. the model must consult
    /// the artifact to see everything).
    pub truncated: bool,
    /// Whether the captured output exceeded the 1 MiB in-memory soft cap.
    pub large: bool,
    /// The full captured output, if it was spilled to the store.
    pub artifact: Option<ArtifactRef>,
}

impl SalientStream {
    /// An empty stream (no output produced).
    pub fn empty() -> Self {
        Self {
            lines: Vec::new(),
            total_lines: 0,
            captured_bytes: 0,
            overflowed: false,
            truncated: false,
            large: false,
            artifact: None,
        }
    }
}

/// Whether a line mentions any error marker (case-insensitive) as a
/// STANDALONE word. A bare substring test flagged every line that merely
/// *names* something error-ish — `error.rs`, `src/error/mod.rs`,
/// `is_error_line`, `error-chain` — so a listing of such paths/identifiers
/// bloated the compacted view with lines that report nothing. A marker counts
/// only when it is not glued into a larger token (see
/// [`contains_marker_word`]); inflected suffixes still match (`errors`,
/// `panicked`, `warnings`) because they are the marker word itself, not a
/// different token that happens to contain it.
fn is_error_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    ERROR_MARKERS
        .iter()
        .any(|marker| contains_marker_word(&lower, marker))
}

/// Characters that glue a marker into a larger token when they sit directly
/// against it WITH a word character on their far side: `error.rs` (filename
/// extension), `src/error/` (path segment), `error-chain` (hyphenated name).
/// A trailing `.`/`-` with nothing word-like beyond it stays a boundary, so a
/// sentence-ending "unexpected error." still flags.
const WORD_JOINERS: [char; 3] = ['.', '/', '-'];

/// An identifier character: glued to a marker on either side it makes the
/// marker part of a larger name (`terror`, `is_error_line`).
fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Whether already-lowercased `lower` contains ASCII `marker` outside any
/// larger token. Before the match: no identifier char, and no joiner whose far
/// side is an identifier char. After the match: `_` never (identifier), a
/// joiner only when its far side is not an identifier char; a PLAIN
/// alphanumeric suffix is allowed — that is the marker's own inflection
/// (`panicked`), not a different word. Byte offsets from `find` are char
/// boundaries because the markers are ASCII.
fn contains_marker_word(lower: &str, marker: &str) -> bool {
    let mut from = 0;
    while let Some(found) = lower[from..].find(marker) {
        let start = from + found;
        let end = start + marker.len();
        let mut before = lower[..start].chars().rev();
        let before_ok = match before.next() {
            None => true,
            Some(c) if is_word_char(c) => false,
            Some(c) if WORD_JOINERS.contains(&c) => !before.next().is_some_and(is_word_char),
            Some(_) => true,
        };
        let mut after = lower[end..].chars();
        let after_ok = match after.next() {
            None => true,
            Some('_') => false,
            Some(c) if WORD_JOINERS.contains(&c) => !after.next().is_some_and(is_word_char),
            Some(_) => true,
        };
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

/// Clamp a single line to [`SALIENT_MAX_LINE_LEN`] bytes on a char boundary,
/// appending an ellipsis when truncated.
fn clamp_line(line: &str) -> String {
    if line.len() <= SALIENT_MAX_LINE_LEN {
        return line.to_string();
    }
    let mut end = SALIENT_MAX_LINE_LEN;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &line[..end])
}

/// Build the compacted [`SalientStream`] for `bytes`. `overflowed` marks that
/// capture hit the hard ceiling; `artifact` is the reference to the full output
/// (present whenever it was spilled).
pub(crate) fn compute_stream(
    bytes: &[u8],
    overflowed: bool,
    artifact: Option<ArtifactRef>,
) -> SalientStream {
    let captured_bytes = bytes.len();
    if bytes.is_empty() {
        return SalientStream {
            artifact,
            ..SalientStream::empty()
        };
    }
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().collect();
    let total_lines = lines.len();

    // Select indices: head, tail, and every error-matching line.
    let mut selected: Vec<usize> = Vec::new();
    let head_end = SALIENT_HEAD.min(total_lines);
    selected.extend(0..head_end);
    let tail_start = total_lines.saturating_sub(SALIENT_TAIL);
    selected.extend(tail_start..total_lines);
    let mut error_lines = 0;
    for (i, line) in lines.iter().enumerate() {
        if is_error_line(line) {
            selected.push(i);
            error_lines += 1;
            if error_lines >= SALIENT_MAX_ERROR_LINES {
                break;
            }
        }
    }
    selected.sort_unstable();
    selected.dedup();

    // Emit selected lines with omission markers across gaps.
    let mut out: Vec<String> = Vec::with_capacity(selected.len() + 4);
    let mut prev: Option<usize> = None;
    for &idx in &selected {
        if let Some(p) = prev {
            if idx > p + 1 {
                out.push(format!("… {} lines omitted …", idx - p - 1));
            }
        }
        out.push(clamp_line(lines[idx]));
        prev = Some(idx);
    }

    SalientStream {
        lines: out,
        total_lines,
        captured_bytes,
        overflowed,
        truncated: selected.len() < total_lines,
        large: captured_bytes > IN_MEMORY_CAP,
        artifact,
    }
}

/// What the current run can actually do about a truncated stream — decided
/// by the agent loop (which knows the offered tool set), not by the tool (Adoption 11 S5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrievalHint {
    /// `artifact.read` is offered to this run.
    pub artifact_read: bool,
}

impl SalientView {
    /// Render the compacted view as the plain-text block that enters model
    /// context: the command, its result, and each non-empty stream's salient
    /// lines with a reference to the full artifact.
    pub fn render(&self) -> String {
        self.render_with_hint(RetrievalHint {
            artifact_read: false,
        })
    }

    /// Render with agent-aware truncation hint (Adoption 11 S5).
    pub fn render_with_hint(&self, hint: RetrievalHint) -> String {
        let mut s = String::new();
        s.push_str(&format!("$ {}\n", self.command));
        match (self.exit_code, self.timed_out) {
            (_, true) => s.push_str(&format!("killed after timeout ({} ms)\n", self.duration_ms)),
            (Some(code), false) => {
                s.push_str(&format!("exit {} ({} ms)\n", code, self.duration_ms))
            }
            (None, false) => s.push_str(&format!("killed by signal ({} ms)\n", self.duration_ms)),
        }
        render_stream(&mut s, "stdout", &self.stdout);
        render_stream(&mut s, "stderr", &self.stderr);

        let is_truncated = self.stdout.truncated
            || self.stdout.overflowed
            || self.stderr.truncated
            || self.stderr.overflowed;

        if is_truncated {
            let art_id = self
                .stdout
                .artifact
                .as_ref()
                .map(|a| a.id.to_string())
                .or_else(|| self.stderr.artifact.as_ref().map(|a| a.id.to_string()));

            if hint.artifact_read {
                if let Some(ref id) = art_id {
                    s.push_str(&format!(
                        "\nfull output is preserved: call artifact.read {{\"artifact_id\":\"{id}\"}} to page through it — do not ask for it to be inlined and do not re-run the command to see more.\n"
                    ));
                } else {
                    s.push_str(
                        "\nfull output is preserved: call artifact.read to page through it — do not ask for it to be inlined and do not re-run the command to see more.\n",
                    );
                }
            } else {
                s.push_str(
                    "\noutput was truncated at capture; re-run with a narrower command (grep/head) instead of re-running the same command.\n",
                );
            }
        }

        s
    }
}

fn render_stream(s: &mut String, name: &str, stream: &SalientStream) {
    if stream.total_lines == 0 {
        s.push_str(&format!("--- {name}: empty ---\n"));
        return;
    }
    let art = stream
        .artifact
        .as_ref()
        .map(|a| {
            format!(
                ", artifact {} sha256:{}",
                a.id,
                &a.sha256[..a.sha256.len().min(12)]
            )
        })
        .unwrap_or_default();
    s.push_str(&format!(
        "--- {name}: {} lines, {} bytes{}{}{} ---\n",
        stream.total_lines,
        stream.captured_bytes,
        if stream.truncated { " (truncated)" } else { "" },
        if stream.overflowed {
            " (capture overflowed)"
        } else {
            ""
        },
        art,
    ));
    for line in &stream.lines {
        s.push_str(line);
        s.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_output_is_not_truncated() {
        let text = "line 1\nline 2\nline 3\n";
        let stream = compute_stream(text.as_bytes(), false, None);
        assert_eq!(stream.total_lines, 3);
        assert!(!stream.truncated);
        assert_eq!(stream.lines, vec!["line 1", "line 2", "line 3"]);
        assert!(!stream.large);
    }

    #[test]
    fn large_output_keeps_head_tail_and_error_lines() {
        let mut text = String::new();
        for i in 0..500 {
            if i == 250 {
                text.push_str("this line has an ERROR in the middle\n");
            } else {
                text.push_str(&format!("line {i}\n"));
            }
        }
        let stream = compute_stream(text.as_bytes(), false, None);
        assert_eq!(stream.total_lines, 500);
        assert!(stream.truncated);
        // Head, tail and the error line survive.
        assert!(stream.lines.iter().any(|l| l == "line 0"));
        assert!(stream.lines.iter().any(|l| l == "line 499"));
        assert!(stream
            .lines
            .iter()
            .any(|l| l.contains("ERROR in the middle")));
        // A gap marker appears.
        assert!(stream.lines.iter().any(|l| l.contains("lines omitted")));
        // Far fewer than 500 lines are retained.
        assert!(stream.lines.len() < 200);
    }

    #[test]
    fn error_line_selection_is_capped() {
        // A broken build that emits thousands of marker lines must not put all of
        // them into the compacted view.
        let mut text = String::new();
        for i in 0..5000 {
            text.push_str(&format!("error: problem {i}\n"));
        }
        let stream = compute_stream(text.as_bytes(), false, None);
        assert_eq!(stream.total_lines, 5000);
        assert!(stream.truncated);
        // Bounded to head + tail + the error cap (plus a few omission markers),
        // not all 5000 lines.
        assert!(stream.lines.len() <= SALIENT_HEAD + SALIENT_TAIL + SALIENT_MAX_ERROR_LINES + 8);
    }

    #[test]
    fn error_markers_are_case_insensitive() {
        assert!(is_error_line("build FAILED with 3 problems"));
        assert!(is_error_line("warning: unused variable"));
        assert!(is_error_line("thread 'main' panicked"));
        assert!(!is_error_line("all good here"));
    }

    #[test]
    fn error_markers_require_word_boundaries() {
        // Standalone mentions (including inflections and sentence-ending
        // punctuation) still flag the line...
        assert!(is_error_line("error: expected `;`"));
        assert!(is_error_line("error[E0308]: mismatched types"));
        assert!(is_error_line("unexpected error."));
        assert!(is_error_line("2 errors emitted"));
        // ...but a token that merely CONTAINS a marker — a filename, a path
        // segment, an identifier, a hyphenated crate name, a larger word —
        // does not.
        assert!(!is_error_line("   Compiling error.rs v0.1.0"));
        assert!(!is_error_line("src/error/mod.rs:12: fn new"));
        assert!(!is_error_line("fn is_error_line(line: &str)"));
        assert!(!is_error_line("error-chain v0.12.4"));
        assert!(!is_error_line("a non-fatal cleanup pass"));
        assert!(!is_error_line("the terror of unwinding"));
    }

    #[test]
    fn empty_output_reports_empty() {
        let stream = compute_stream(b"", false, None);
        assert_eq!(stream.total_lines, 0);
        assert!(stream.lines.is_empty());
    }

    #[test]
    fn render_with_hint_appends_correct_instruction_when_truncated() {
        let mut text = String::new();
        for i in 0..500 {
            text.push_str(&format!("line {i}\n"));
        }
        let art_id = codypendent_protocol::ids::ArtifactId::new();
        let art = ArtifactRef {
            id: art_id,
            sha256: "abcdef0123456789abcdef0123456789".to_string(),
            media_type: "text/plain".to_string(),
            byte_length: 5000,
            sensitivity: codypendent_protocol::DataClassification::Public,
        };
        let stream = compute_stream(text.as_bytes(), false, Some(art));
        let view = SalientView {
            command: "cargo test".to_string(),
            exit_code: Some(0),
            timed_out: false,
            duration_ms: 100,
            stdout: stream,
            stderr: SalientStream::empty(),
        };

        // With artifact_read = true
        let rendered_with_art = view.render_with_hint(RetrievalHint {
            artifact_read: true,
        });
        assert!(rendered_with_art.contains("full output is preserved: call artifact.read"));
        assert!(rendered_with_art.contains(&art_id.to_string()));

        // With artifact_read = false
        let rendered_without_art = view.render_with_hint(RetrievalHint {
            artifact_read: false,
        });
        assert!(rendered_without_art
            .contains("output was truncated at capture; re-run with a narrower command"));

        // Non-truncated view has no hint block
        let non_trunc_stream = compute_stream(b"line 1\n", false, None);
        let non_trunc_view = SalientView {
            command: "echo 1".to_string(),
            exit_code: Some(0),
            timed_out: false,
            duration_ms: 10,
            stdout: non_trunc_stream,
            stderr: SalientStream::empty(),
        };
        let clean = non_trunc_view.render_with_hint(RetrievalHint {
            artifact_read: true,
        });
        assert!(!clean.contains("full output is preserved"));
        assert!(!clean.contains("output was truncated"));
    }
}
