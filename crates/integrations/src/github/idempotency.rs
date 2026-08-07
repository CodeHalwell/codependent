//! Hidden-marker idempotency for GitHub writes (Phase 3 STEP 3.1).
//!
//! GitHub's create endpoints are not idempotent: replaying a command that
//! failed to persist its result would open a duplicate PR or comment. This
//! module embeds a stable, invisible marker into the created object's body —
//! an HTML comment that renders as nothing on GitHub — carrying the command's
//! idempotency key. Before creating, the client lists existing objects and, if
//! one already carries the marker for this key, returns it instead of creating
//! a duplicate.

const MARKER_PREFIX: &str = "<!-- codypendent-idempotency:";
const MARKER_SUFFIX: &str = " -->";

/// The hidden marker for an idempotency key, e.g.
/// `<!-- codypendent-idempotency:KEY -->`.
pub fn marker(key: &str) -> String {
    format!("{MARKER_PREFIX}{key}{MARKER_SUFFIX}")
}

/// Append the hidden marker to `body` on its own trailing line.
pub fn body_with_marker(body: &str, key: &str) -> String {
    let trimmed = body.trim_end();
    if trimmed.is_empty() {
        marker(key)
    } else {
        format!("{trimmed}\n{}", marker(key))
    }
}

/// Extract the idempotency key from a body that carries the marker, if any.
pub fn extract_key(body: &str) -> Option<String> {
    extract_keys(body).last()
}

/// Whether `body` carries the marker for exactly `key`.
pub fn body_matches_key(body: &str, key: &str) -> bool {
    extract_keys(body).any(|candidate| candidate == key)
}

/// Iterate every syntactically valid marker. Scanning the complete body avoids
/// an earlier quoted/example marker hiding the real trailing marker we wrote.
fn extract_keys(body: &str) -> impl Iterator<Item = String> + '_ {
    body.match_indices(MARKER_PREFIX).filter_map(|(start, _)| {
        let rest = &body[start + MARKER_PREFIX.len()..];
        let end = rest.find(MARKER_SUFFIX)?;
        Some(rest[..end].trim().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_has_the_documented_shape() {
        assert_eq!(marker("abc"), "<!-- codypendent-idempotency:abc -->");
    }

    #[test]
    fn round_trips_through_a_body() {
        let key = "01890c7e-idem-key";
        let body = body_with_marker("Please review this change.", key);
        assert!(body.contains("Please review this change."));
        assert_eq!(extract_key(&body).as_deref(), Some(key));
        assert!(body_matches_key(&body, key));
    }

    #[test]
    fn empty_body_is_just_the_marker() {
        let key = "k";
        assert_eq!(body_with_marker("", key), marker(key));
        assert_eq!(body_with_marker("   \n", key), marker(key));
    }

    #[test]
    fn non_matching_and_absent_markers() {
        let body = body_with_marker("hello", "key-one");
        assert!(!body_matches_key(&body, "key-two"));
        assert_eq!(extract_key("no marker here"), None);
        assert!(!body_matches_key("no marker here", "key-one"));
    }

    #[test]
    fn an_earlier_quoted_marker_cannot_hide_the_real_one() {
        let body = format!("example: {}\n{}", marker("quoted"), marker("real"));
        assert!(body_matches_key(&body, "real"));
        assert_eq!(extract_key(&body).as_deref(), Some("real"));
    }
}
