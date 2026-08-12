//! `artifact.read` — rehydrate a content-addressed artifact by id.
//!
//! Salient views and folded tool results cite `artifact <id> sha256:…`
//! (see [`super::salient`] and the agent loop's mid-run compaction), telling
//! the model exactly where the full bytes live — but until this tool existed
//! it had no way to open them. `artifact.read` closes that loop: given an id,
//! it loads the stored bytes through the pool-erased
//! [`ArtifactReader`](super::ArtifactReader) seam and returns a BOUNDED
//! rendering (64 KiB, salient-style head + tail beyond that), so one huge
//! spill can never re-enter the context whole through the very tool meant to
//! relieve context pressure.
//!
//! Offered only when a reader is wired (`FrameworkAgentRuntime::
//! with_artifact_reader` — the `web.search` configured-gate idiom), and in
//! EVERY mode: it is a read of the daemon's own store, so no overlay filters
//! it, exactly like `workspace.read_file`.

use std::str::FromStr;

use codypendent_protocol::{ArtifactId, ProposedAction};
use serde_json::Value;

/// The `artifact.read` tool: re-open a stored artifact the transcript cites.
pub struct ArtifactRead;

impl ArtifactRead {
    /// The stable dotted tool name.
    pub const NAME: &'static str = "artifact.read";

    /// Byte budget on the rendered observation. An artifact is bulk spill by
    /// definition (that is why it was spilled), so the rendering keeps the
    /// head and tail halves of this budget with an omission marker between —
    /// the same "consult the edges" shape a salient view uses. Matches the
    /// `web.search` observation cap: context-budget-sized, not the 8 MiB
    /// bulk cap.
    pub const MAX_OBSERVATION_BYTES: usize = 64 * 1024;

    /// The action policy evaluates. The artifact store is the daemon's OWN
    /// content-addressed store — no worktree path, command, or network is
    /// touched, and it holds only content this pipeline already stored (tool
    /// spill, staged writes, chronicles). The wire protocol has no dedicated
    /// variant for it (adding one is a protocol change with golden vectors,
    /// deliberately not taken here), so the call is evaluated as the
    /// read-class action it is: a [`ProposedAction::ReadFiles`] with no
    /// filesystem paths — allowed under every mode overlay (reads survive
    /// every overlay) while the access is still traced like any other call.
    #[must_use]
    pub fn proposed_action() -> ProposedAction {
        ProposedAction::ReadFiles { paths: Vec::new() }
    }

    /// Render loaded artifact bytes as the bounded, model-facing observation:
    /// a header naming the id, media type, and true byte length, then the
    /// content (lossy UTF-8 — artifacts are overwhelmingly text: spilled
    /// stdout, diffs, JSON). Content beyond [`Self::MAX_OBSERVATION_BYTES`]
    /// keeps the head and tail halves with an explicit omission marker, so
    /// the model always sees the true size and both edges, never a silently
    /// clipped whole.
    #[must_use]
    pub fn render(id: ArtifactId, media_type: &str, bytes: &[u8]) -> String {
        let text = String::from_utf8_lossy(bytes);
        let header = format!("artifact {id} ({media_type}, {} bytes)\n", bytes.len());
        if text.len() <= Self::MAX_OBSERVATION_BYTES {
            return format!("{header}{text}");
        }
        let half = Self::MAX_OBSERVATION_BYTES / 2;
        let head_end = floor_char_boundary(&text, half);
        let tail_start = ceil_char_boundary(&text, text.len() - half);
        format!(
            "{header}{}\n… {} bytes omitted …\n{}",
            &text[..head_end],
            tail_start - head_end,
            &text[tail_start..]
        )
    }
}

/// The largest char boundary in `text` at or below `at` (a stable-Rust stand-in
/// for the unstable `str::floor_char_boundary`).
fn floor_char_boundary(text: &str, at: usize) -> usize {
    let mut end = at.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// The smallest char boundary in `text` at or above `at`.
fn ceil_char_boundary(text: &str, at: usize) -> usize {
    let mut start = at.min(text.len());
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    start
}

/// The parsed, model-supplied arguments of an `artifact.read` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactReadInput {
    /// The artifact to rehydrate — the id a salient view or folded result
    /// cited.
    pub id: ArtifactId,
}

/// Parse `artifact.read` arguments: `id` is required and must be a valid
/// artifact id (the UUID a salient view cites). A malformed id is a legible
/// tool error the model can correct, never a panic.
pub fn parse_artifact_read(args: &Value) -> Result<ArtifactReadInput, String> {
    let raw = args
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("artifact.read requires a non-empty string `id`")?;
    let id = ArtifactId::from_str(raw)
        .map_err(|_| format!("artifact.read: `{raw}` is not a valid artifact id"))?;
    Ok(ArtifactReadInput { id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_requires_a_valid_id() {
        assert!(parse_artifact_read(&json!({})).is_err());
        assert!(parse_artifact_read(&json!({"id": ""})).is_err());
        assert!(parse_artifact_read(&json!({"id": "not-a-uuid"})).is_err());

        let id = ArtifactId::new();
        let parsed = parse_artifact_read(&json!({"id": id.to_string()})).expect("a real id parses");
        assert_eq!(parsed.id, id);
        // Surrounding whitespace (a model quoting the citation loosely) is
        // tolerated.
        let padded = parse_artifact_read(&json!({"id": format!("  {id}  ")})).expect("parses");
        assert_eq!(padded.id, id);
    }

    #[test]
    fn render_small_artifact_is_complete_with_header() {
        let id = ArtifactId::new();
        let rendered = ArtifactRead::render(id, "text/plain", b"hello artifact\n");
        assert!(rendered.starts_with(&format!("artifact {id} (text/plain, 15 bytes)\n")));
        assert!(rendered.ends_with("hello artifact\n"));
        assert!(!rendered.contains("omitted"));
    }

    #[test]
    fn render_large_artifact_keeps_head_and_tail_within_budget() {
        let id = ArtifactId::new();
        let mut body = String::new();
        for i in 0..20_000 {
            body.push_str(&format!("line {i}\n"));
        }
        assert!(body.len() > ArtifactRead::MAX_OBSERVATION_BYTES);
        let rendered = ArtifactRead::render(id, "text/plain", body.as_bytes());
        // The true size is reported, both edges survive, and the middle is
        // marked omitted — never a silently clipped whole.
        assert!(rendered.contains(&format!("{} bytes", body.len())));
        assert!(rendered.contains("line 0\n"));
        assert!(rendered.contains("line 19999\n"));
        assert!(rendered.contains("bytes omitted"));
        // Bounded: budget + header/marker slack, far below the input.
        assert!(rendered.len() < ArtifactRead::MAX_OBSERVATION_BYTES + 256);
    }

    #[test]
    fn render_truncation_respects_char_boundaries() {
        let id = ArtifactId::new();
        // A multi-byte-only body forces both cut points onto non-ASCII
        // boundaries; rendering must not panic or split a char.
        let body = "é".repeat(ArtifactRead::MAX_OBSERVATION_BYTES);
        let rendered = ArtifactRead::render(id, "text/plain", body.as_bytes());
        assert!(rendered.contains("bytes omitted"));
    }
}
