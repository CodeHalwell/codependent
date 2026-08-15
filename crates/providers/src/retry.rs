//! Retry policy for transient provider failures — pure decision logic,
//! ported from opencode's `session/retry.ts`. No I/O: the runtime's model
//! driver asks "is this retryable?" and "how long do I wait?", then owns the
//! sleeping, the cancellation race, and the status event itself.

use std::time::SystemTime;

/// Base delay before the first retry.
pub const RETRY_INITIAL_DELAY_MS: u64 = 2_000;
/// Exponential growth factor between attempts.
pub const RETRY_BACKOFF_FACTOR: u64 = 2;
/// Up to this fraction of the base delay is added as jitter.
pub const RETRY_JITTER_FACTOR: f64 = 0.25;
/// Delay cap when the provider gave no Retry-After hint.
pub const RETRY_MAX_DELAY_NO_HINT_MS: u64 = 30_000;
/// Absolute delay cap (a provider-supplied Retry-After beyond this is clamped).
pub const RETRY_MAX_DELAY_MS: u64 = 2_147_483_647;
/// Maximum number of retries after the initial attempt.
pub const RETRY_MAX_RETRIES: u32 = 5;

/// The marker a header-aware client embeds in its error text when the
/// response carried a Retry-After header: ` [retry-after-ms=N]`.
pub const RETRY_AFTER_MARKER: &str = "[retry-after-ms=";

/// A positive retry decision: the failure is transient and worth repeating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryDecision {
    /// The human/model-facing reason (bounded; fed into the status event).
    pub message: String,
}

/// Transient phrases, matched case-insensitively as substrings. The superset
/// of `classify_provider_message`'s original 14 plus opencode's
/// RETRYABLE_MESSAGE_PATTERNS, minus bare "unavailable"/"exhausted".
const TRANSIENT_PHRASES: &[&str] = &[
    // -- today's list (crates/runtime/src/models.rs) --
    "connection refused",
    "connection reset",
    "connection closed",
    "broken pipe",
    "timed out",
    "timeout",
    "temporarily unavailable",
    "overloaded",
    "rate limit",
    "too many requests",
    "internal server error",
    "bad gateway",
    "service unavailable",
    "gateway timeout",
    // -- opencode additions --
    "rate increased too quickly",
    "rate-limit",
    "rate_limit",
    "internal error",
    "internal_error",
    "server error",
    "server_error",
    "server-error",
    "service_unavailable",
    "service-unavailable",
    "provider returned error",
    "provider_returned_error",
    "provider-returned-error",
    "terminated",
    "fetch failed",
    "failed to fetch",
    "network error",
    "upstream connect",
    "connection error",
    "connection lost",
    "socket connection was closed",
    "socket hang up",
    "reset before headers",
    "getaddrinfo",
    "enotfound",
    "eai_again",
    "econnrefused",
    "econnreset",
    "etimedout",
    "time out",
    "try your request again",
    "retry your request",
    "resource exhausted",
    "resource_exhausted",
];

/// Retryable HTTP statuses, matched as standalone digit runs (never
/// substrings — a `500` inside an id like `15005` must not match).
/// 524 is Cloudflare's origin-timeout, from opencode's pattern list.
const TRANSIENT_STATUS: &[&str] = &["408", "429", "500", "502", "503", "504", "524"];

/// Classify an opaque provider/stream failure message. `Some` iff the message
/// plainly names a transient condition; anything unrecognized is permanent so
/// a novel failure surfaces immediately instead of being silently retried.
pub fn retryable(message: &str) -> Option<RetryDecision> {
    let lower = message.to_ascii_lowercase();
    let phrase_hit = TRANSIENT_PHRASES.iter().any(|p| lower.contains(p));
    let status_hit = lower
        .split(|c: char| !c.is_ascii_digit())
        .any(|run| TRANSIENT_STATUS.contains(&run));
    if !phrase_hit && !status_hit {
        return None;
    }
    // Reference parity (retry.ts line 144): a bounded, legible reason.
    let message = if lower.contains("overloaded") {
        "provider is overloaded".to_string()
    } else if lower.contains("too many requests") || lower.contains("429") {
        "too many requests".to_string()
    } else {
        let mut m = message.trim().to_string();
        // status events are UI strings, not artifacts. Truncate to <=200 bytes on
        // a UTF-8 char boundary — `String::truncate` panics if byte 200 lands
        // mid-codepoint, which real provider bodies (embedded multibyte text) do
        // hit. Walk back to the nearest boundary at or before 200.
        if m.len() > 200 {
            let end = (0..=200)
                .rev()
                .find(|&i| m.is_char_boundary(i))
                .unwrap_or(0);
            m.truncate(end);
        }
        m
    };
    Some(RetryDecision { message })
}

/// Extract a `[retry-after-ms=N]` hint embedded in an error message, if any.
pub fn parse_retry_after_hint(message: &str) -> Option<u64> {
    let start_idx = message.find(RETRY_AFTER_MARKER)?;
    let after_marker = &message[start_idx + RETRY_AFTER_MARKER.len()..];
    let end_idx = after_marker.find(']')?;
    let digits = &after_marker[..end_idx];
    digits.parse::<u64>().ok()
}

/// The wait before retry number `attempt` (1-based). `retry_after_ms` (a
/// provider hint) wins outright, clamped to [`RETRY_MAX_DELAY_MS`]; otherwise
/// exponential backoff with jitter, clamped to [`RETRY_MAX_DELAY_NO_HINT_MS`].
/// `jitter` must be in `[0.0, 1.0)`.
pub fn delay_ms(attempt: u32, retry_after_ms: Option<u64>, jitter: f64) -> u64 {
    if let Some(hint) = retry_after_ms {
        return hint.min(RETRY_MAX_DELAY_MS);
    }
    let exp = attempt.saturating_sub(1).min(10); // 2^10 caps the shift safely
    let base = RETRY_INITIAL_DELAY_MS.saturating_mul(RETRY_BACKOFF_FACTOR.saturating_pow(exp));
    let jitter_clamped = jitter.clamp(0.0, 1.0);
    let jittered = (base as f64 + base as f64 * RETRY_JITTER_FACTOR * jitter_clamped).ceil() as u64;
    jittered.min(RETRY_MAX_DELAY_NO_HINT_MS)
}

/// A `[0,1)` jitter value from the system clock's subsecond nanos (the
/// workspace carries no `rand`; cryptographic quality is not needed here).
pub fn entropy_jitter() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos % 1_000_000) as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_phrases_and_statuses_are_retryable() {
        assert!(retryable("connection refused").is_some());
        assert!(retryable("HTTP 524 from upstream").is_some());
        assert!(retryable("Overloaded").is_some());
        assert!(retryable("socket hang up").is_some());
        assert!(retryable("resource_exhausted").is_some());
        assert!(retryable("Error 429 Too Many Requests").is_some());
        assert!(retryable("500 Internal Server Error").is_some());
        assert!(retryable("502 Bad Gateway").is_some());
        assert!(retryable("503 Service Unavailable").is_some());
        assert!(retryable("504 Gateway Timeout").is_some());
    }

    #[test]
    fn unrecognized_and_permanent_messages_are_not_retryable() {
        assert!(retryable("invalid api key").is_none());
        assert!(retryable("model `x` is not registered").is_none());
        assert!(retryable("provider did not list this model").is_none());
        assert!(retryable("id 15005 not found").is_none());
        assert!(retryable("unauthorized: bad token").is_none());
    }

    #[test]
    fn delay_follows_the_reference_schedule() {
        assert_eq!(delay_ms(1, None, 0.0), 2_000);
        assert_eq!(delay_ms(5, None, 0.0), 30_000); // capped at 30_000
        assert_eq!(delay_ms(2, None, 0.5), 4_500); // 4000 + 4000*0.25*0.5 = 4500
        assert_eq!(delay_ms(1, Some(1_234), 0.9), 1_234);
        assert_eq!(delay_ms(1, Some(u64::MAX), 0.0), RETRY_MAX_DELAY_MS);
    }

    #[test]
    fn retry_after_hint_wins_over_backoff() {
        assert_eq!(delay_ms(3, Some(500), 0.5), 500);
        assert_eq!(delay_ms(1, Some(10_000), 0.0), 10_000);
    }

    #[test]
    fn retryable_truncates_multibyte_message_without_panicking() {
        // A message whose byte 200 lands mid-codepoint previously panicked
        // `String::truncate(200)`. Build one: 198 ASCII bytes, then a 3-byte
        // multibyte char ("€") straddling byte 200. Prefix a transient phrase so
        // the message classifies as retryable and reaches the truncation branch.
        let message = format!("connection refused {}{}", "a".repeat(180), "€".repeat(20));
        assert!(message.len() > 200);
        let decision = retryable(&message).expect("classified transient");
        // No panic, and the truncated reason is valid UTF-8 <= 200 bytes.
        assert!(decision.message.len() <= 200);
        assert!(std::str::from_utf8(decision.message.as_bytes()).is_ok());
    }

    #[test]
    fn retry_after_marker_round_trips() {
        assert_eq!(
            parse_retry_after_hint("OpenAI error 429 [retry-after-ms=2500]"),
            Some(2500)
        );
        assert_eq!(parse_retry_after_hint("No marker here"), None);
        assert_eq!(
            parse_retry_after_hint("Malformed [retry-after-ms=abc]"),
            None
        );
        assert_eq!(
            parse_retry_after_hint("Incomplete [retry-after-ms=123"),
            None
        );
    }
}
