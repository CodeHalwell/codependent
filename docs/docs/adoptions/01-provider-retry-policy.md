# Adoption 01 — Provider Retry Policy

**Effort:** S · **Depends on:** nothing · **Reference:** `reference-repos/opencode/packages/opencode/src/session/retry.ts`, `reference-repos/opencode/packages/opencode/src/session/message-v2.ts` (`fromError`, line 606)
**Ported from:** opencode · **Status:** ⬜ not started

## 1. Summary

opencode wraps every model request in a pure, well-tuned retry policy: a message/status classifier that recognizes the full zoo of transient provider failures, exponential backoff (2 s base, factor 2, 25% jitter) that honors `Retry-After` hints, a hard cap of 5 retries, and — critically — a **live status event per attempt** so the TUI shows "retrying (2/5)" with the provider's reason instead of a silent stall. codypendent already has a retry loop (contrary to the adoption plan's "currently NO retry" note — see §3), but it is weaker on every axis: fixed 1 s/2 s/4 s schedule, max 3 retries, no jitter, no `Retry-After` handling, a thinner failure-phrase list, and zero user visibility. This adoption ports opencode's decision logic into `crates/providers` as a pure leaf module (matching that crate's "daemon-free, network-free" charter), rewires the existing retry loop in `crates/runtime` onto it, and adds a `ModelRetrying` protocol event that the TUI folds into a "retrying (n/m)…" status row.

## 2. Reference implementation

All in `reference-repos/opencode/packages/opencode/src/session/retry.ts` unless noted.

**Constants (lines 26–31):**

```
RETRY_INITIAL_DELAY   = 2000 ms
RETRY_BACKOFF_FACTOR  = 2
RETRY_JITTER_FACTOR   = 0.25
RETRY_MAX_DELAY_NO_HEADERS = 30_000 ms   // cap when no Retry-After info
RETRY_MAX_DELAY       = 2_147_483_647 ms // setTimeout i32 cap
RETRY_MAX_RETRIES     = 5
```

**Classifier — `retryable(error, provider)` (line 84).** Operates on the *typed* error persisted on the assistant message (built by `message-v2.ts fromError`, line 606, which pre-marks `ECONNRESET`, zlib decompression failures, header timeouts, and response-stream errors as `isRetryable: true` `APIError`s). Rules:

- `ContextOverflowError` → never retried (has its own compaction path).
- `APIError` → retried if the SDK marked it retryable, **or status ≥ 500** ("5xx errors are transient server failures and should always be retried, even when the provider SDK doesn't explicitly mark them as retryable" — line 89 comment), **or** the message/body matches `RETRYABLE_MESSAGE_PATTERNS`.
- Any other error whose message matches the patterns → retried.
- Two opencode-platform body markers (`FreeUsageLimitError`, `GoUsageLimitError`) return the retry decision with an attached upsell `action` — platform-specific, not ported.

**`RETRYABLE_MESSAGE_PATTERNS` (lines 33–40)** — six regexes covering:

1. Status codes: `429|500|502|503|504|524` (note **524**, Cloudflare origin-timeout).
2. Rate limits: `rate increased too quickly | rate limit | rate-limit | rate_limit | too many requests`.
3. Overload/server errors: `overloaded | service unavailable/_/- | internal error/_ | internal server error | server error/_/- | provider returned error/_/-`.
4. Network failures: `terminated | fetch failed | failed to fetch | network error | upstream connect | connection error | connection refused | connection lost | socket connection was closed | socket hang up | reset before headers | getaddrinfo | enotfound | eai_again | econnrefused | econnreset | etimedout`.
5. Timeouts: `^timeout$` or `(request|response|connection|network|stream|read) (timeout|timed out|time out)`.
6. Politeness phrases: `try your request again | retry your request | resource exhausted | resource_exhausted`.

**Delay — `delay(attempt, error, random)` (lines 46–82).** Precedence:

1. `retry-after-ms` response header → use it verbatim (capped at `RETRY_MAX_DELAY`).
2. `retry-after` header → seconds, or an HTTP date parsed relative to now.
3. Headers present but no retry-after → exponential, capped only at `RETRY_MAX_DELAY`.
4. No header info at all → exponential capped at **30 s**.

Exponential: `base = 2000 · 2^(attempt−1)`, `delay = ceil(base + base · 0.25 · random)` — so attempt 1..5 waits ≈ 2 s, 4 s, 8 s, 16 s, 32 s→30 s plus up to 25% jitter.

**Status surfacing — `policy(opts)` (line 182).** An Effect `Schedule` applied by `processor.ts` via `Effect.retry` around the whole stream. Each attempt calls `opts.set({attempt, message, action, next: now + wait})` → `SessionStatus.set(type: "retry", …)`, which the TUI renders as a live countdown. Retry stops when the classifier declines or `meta.attempt > RETRY_MAX_RETRIES`.

## 3. Current state in codypendent (verified)

**`crates/providers/` has no retry** — it is a pure data crate (`catalog.rs`, `credential.rs`, `model.rs`; `lib.rs` calls it "a daemon-free, network-free leaf crate"). But the adoption plan's claim that codypendent has *no* retry is wrong at the system level:

- **`crates/runtime/src/agent.rs` `FrameworkModelDriver::next_step` (~line 7108)** already wraps `stream_once` in a retry loop: fixed schedule `MODEL_RETRY_BACKOFF: [Duration; 3] = [1 s, 2 s, 4 s]` (~line 7161), so at most 3 retries. Classification is `classify_provider_message(&error.to_string())` — the framework seam surfaces errors as formatted strings, not typed values.
- **The streamed-veto rule** (~line 7130): once ANY text delta has reached the `DeltaSink`, the failure is unretryable regardless of class, because the loop has already journaled and published that text as `ModelStreamDelta`s and a second attempt would duplicate the reply. This is codypendent-specific and **must be preserved** (opencode can drop the partial assistant message; codypendent's ledger events are immutable evidence).
- **`crates/runtime/src/models.rs` `classify_provider_message` (line 447)**: 14 transient phrases + 6 status codes (`408 429 500 502 503 504`) matched as standalone digit runs (never substrings — "a `500` inside an id like `15005` cannot misclassify"). Also consumed by `ModelsError::failure_class` (line 427), which candidate resolution uses.
- **Backoff waits are cancellation-safe**: the loop races `next_step` against `cancel.cancelled()` and the wall clock in a `tokio::select!` (~agent.rs line 2664), so dropping the step future cancels a pending backoff sleep. Preserved for free.
- **No retry visibility.** `EventBody` (`crates/protocol/src/events.rs`) has no retry variant; the TUI's `RunActivity` (`crates/tui/src/state.rs` line 947: `Idle | Thinking | Streaming | RunningTool(String)`) renders `Thinking` as "working…" (`crates/tui/src/render.rs` `activity_status_line`, line 2239) during the entire backoff.
- **Existing paused-clock tests** (`crates/runtime/src/agent.rs` mod at ~line 12757): `a_permanent_failure_is_surfaced_without_a_single_retry`, `retries_stop_after_the_backoff_schedule_is_exhausted`, and a streamed-veto test, driven by a scripted `ChatClient` that counts requests under `#[tokio::test(start_paused = true)]` (the dev-dep comment in `crates/runtime/Cargo.toml` names the 1 s/2 s/4 s schedule). These tests must be updated, not deleted.
- **Retry-after headers do not currently cross the seam.** The stock `agent_framework_openai::OpenAIChatCompletionClient` and `agent_framework_anthropic::AnthropicClient` errors are opaque strings. The one client codypendent owns end-to-end — `HeaderAuthChatClient::post` (`crates/runtime/src/models.rs` ~line 1234) — formats `"OpenAI-compatible API error {status}: {text}"` and discards response headers.
- **`EventBody` is internally tagged with `#[serde(other)] Unknown`** (events.rs line 67): a new variant is additive; an older client renders a placeholder. No migration needed — the ledger stores event bodies as JSON.

## 4. Design

Three moves, smallest possible surface:

```
crates/providers/src/retry.rs      pure decision logic (classifier + delay math)   ← port of retry.ts
        ↑ called by
crates/runtime  FrameworkModelDriver::next_step   (replaces MODEL_RETRY_BACKOFF +
                                                   classify_provider_message dispatch)
        ↓ notifies via DeltaSink::on_retry
crates/runtime  agent loop select-drain  → EventBody::ModelRetrying (persist + publish)
        ↓
crates/tui      reduce.rs → RunActivity::Retrying → render "retrying (2/5)…"
```

- **`codypendent_providers::retry`** holds the constants, the classifier (`retryable`), the delay math (`delay_ms`), and a retry-after hint parser. Pure functions over `&str` — no tokio, no reqwest — honoring the crate's leaf charter. The classifier is the **superset** of today's `classify_provider_message` plus opencode's patterns, implemented in the existing house idiom (case-insensitive substring lists + standalone digit-run status matching), not regex (`regex` is not a direct workspace dependency and the house avoids adding deps for this).
- **`classify_provider_message` in `models.rs` becomes a thin delegate** to the new classifier, so `ModelsError::failure_class` and candidate resolution stay in agreement with the driver — one taxonomy, exactly opencode's "one typed error taxonomy shared by persistence, retry policy, and compaction triggering" property, adapted to string-shaped errors.
- **Retry-After**: `HeaderAuthChatClient::post` is extended to append a machine-parseable marker `" [retry-after-ms=N]"` to its error text when the response carries `retry-after-ms` or `retry-after` (seconds or HTTP-date, converted to ms). `retry::parse_retry_after_hint` extracts it. Stock framework clients get exponential backoff only — an honest degradation, not a fabricated hint.
- **Visibility**: `DeltaSink` gains a default-no-op `on_retry(&RetryNotice)`; `ChannelSink`'s channel payload becomes an enum so retry notices ride the same ordered queue as text chunks; the loop's select-drain maps a notice to `EventBody::ModelRetrying` (persist-before-publish, like every event). A retry notice must NOT set `streamed_this_step` and must not touch the `pending` text buffer.

**Deviations from the reference, and why:**

1. **Streamed-veto kept** (stronger than opencode): a mid-stream failure after first byte is final. opencode re-streams from the top because its transcript can drop the partial message; codypendent's ledger cannot.
2. **No upsell `action`s** (`FreeUsageLimitError` / `GoUsageLimitError`, `GO_UPSELL_*`): opencode-platform-specific.
3. **Bare `unavailable` / `exhausted` are NOT matched** (opencode's non-APIError branch matches them): codypendent's `ModelUnavailable` reason "provider did not list this model" contains "unavailable" and is genuinely permanent. Only the specific phrases (`service unavailable`, `temporarily unavailable`, `resource exhausted`, …) match.
4. **Substring lists instead of regexes**, matching `classify_provider_message`'s existing idiom, including its standalone-digit-run rule for status codes. The timeout regex `\b(request|response|…) (timeout|timed out|time out)\b` collapses to the substrings `timeout` / `timed out` / `time out`, which today's list already half-does.
5. **Jitter source**: no `rand` dependency in the workspace; `entropy_jitter()` derives a `[0,1)` value from `SystemTime` subsecond nanos. Tests inject a fixed jitter.

## 5. Changes, file by file

### 5.1 `crates/providers/src/retry.rs` (new)

The pure decision module. Names and behavior below are normative.

```rust
//! Retry policy for transient provider failures — pure decision logic,
//! ported from opencode's `session/retry.ts`. No I/O: the runtime's model
//! driver asks "is this retryable?" and "how long do I wait?", then owns the
//! sleeping, the cancellation race, and the status event itself.

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

/// Classify an opaque provider/stream failure message. `Some` iff the message
/// plainly names a transient condition; anything unrecognized is permanent so
/// a novel failure surfaces immediately instead of being silently retried.
pub fn retryable(message: &str) -> Option<RetryDecision>;

/// Extract a `[retry-after-ms=N]` hint embedded in an error message, if any.
pub fn parse_retry_after_hint(message: &str) -> Option<u64>;

/// The wait before retry number `attempt` (1-based). `retry_after_ms` (a
/// provider hint) wins outright, clamped to [`RETRY_MAX_DELAY_MS`]; otherwise
/// exponential backoff with jitter, clamped to [`RETRY_MAX_DELAY_NO_HINT_MS`].
/// `jitter` must be in `[0.0, 1.0)`.
pub fn delay_ms(attempt: u32, retry_after_ms: Option<u64>, jitter: f64) -> u64;

/// A `[0,1)` jitter value from the system clock's subsecond nanos (the
/// workspace carries no `rand`; cryptographic quality is not needed here).
pub fn entropy_jitter() -> f64;
```

Skeleton bodies for the non-obvious parts:

```rust
/// Transient phrases, matched case-insensitively as substrings. The superset
/// of `classify_provider_message`'s original 14 plus opencode's
/// RETRYABLE_MESSAGE_PATTERNS, minus bare "unavailable"/"exhausted" (§4.3).
const TRANSIENT_PHRASES: &[&str] = &[
    // -- today's list (crates/runtime/src/models.rs) --
    "connection refused", "connection reset", "connection closed",
    "broken pipe", "timed out", "timeout", "temporarily unavailable",
    "overloaded", "rate limit", "too many requests",
    "internal server error", "bad gateway", "service unavailable",
    "gateway timeout",
    // -- opencode additions --
    "rate increased too quickly", "rate-limit", "rate_limit",
    "internal error", "internal_error", "server error", "server_error",
    "server-error", "service_unavailable", "service-unavailable",
    "provider returned error", "provider_returned_error",
    "provider-returned-error",
    "terminated", "fetch failed", "failed to fetch", "network error",
    "upstream connect", "connection error", "connection lost",
    "socket connection was closed", "socket hang up",
    "reset before headers", "getaddrinfo", "enotfound", "eai_again",
    "econnrefused", "econnreset", "etimedout",
    "time out",
    "try your request again", "retry your request",
    "resource exhausted", "resource_exhausted",
];

/// Retryable HTTP statuses, matched as standalone digit runs (never
/// substrings — a `500` inside an id like `15005` must not match).
/// 524 is Cloudflare's origin-timeout, from opencode's pattern list.
const TRANSIENT_STATUS: &[&str] = &["408", "429", "500", "502", "503", "504", "524"];

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
        m.truncate(200); // status events are UI strings, not artifacts
        m
    };
    Some(RetryDecision { message })
}

pub fn delay_ms(attempt: u32, retry_after_ms: Option<u64>, jitter: f64) -> u64 {
    if let Some(hint) = retry_after_ms {
        return hint.min(RETRY_MAX_DELAY_MS);
    }
    let exp = attempt.saturating_sub(1).min(10); // 2^10 caps the shift safely
    let base = RETRY_INITIAL_DELAY_MS.saturating_mul(RETRY_BACKOFF_FACTOR.saturating_pow(exp));
    let jittered = (base as f64 + base as f64 * RETRY_JITTER_FACTOR * jitter).ceil() as u64;
    jittered.min(RETRY_MAX_DELAY_NO_HINT_MS)
}
```

`parse_retry_after_hint` scans for `RETRY_AFTER_MARKER`, parses the digits up to the closing `]`, and returns `None` on any malformation.

### 5.2 `crates/providers/src/lib.rs`

Add the module and re-exports:

```rust
pub mod retry;

pub use retry::{
    delay_ms, entropy_jitter, parse_retry_after_hint, retryable, RetryDecision,
    RETRY_MAX_RETRIES,
};
```

### 5.3 `crates/runtime/src/models.rs`

1. **Delegate `classify_provider_message`** (line 447) so the taxonomy exists in one place; the public signature and doc comment stay (its dead local const tables are removed):

```rust
#[must_use]
pub fn classify_provider_message(message: &str) -> FailureClass {
    match codypendent_providers::retry::retryable(message) {
        Some(_) => FailureClass::Transient,
        None => FailureClass::Permanent,
    }
}
```

2. **`HeaderAuthChatClient::post`** (~line 1234): before consuming the failed response's body, capture the retry-after hint and append the marker to the error text:

```rust
if !resp.status().is_success() {
    let status = resp.status();
    let retry_after_ms = retry_after_hint_ms(resp.headers()); // Option<u64>
    let text = resp.text().await.unwrap_or_default();
    let mut message = format!("OpenAI-compatible API error {status}: {text}");
    if let Some(ms) = retry_after_ms {
        message.push_str(&format!(" [retry-after-ms={ms}]"));
    }
    return Err(agent_framework_openai::classify_service_error(
        status.as_u16(), &text, message, None,
    ));
}
```

with a private helper:

```rust
/// `retry-after-ms` (ms) wins over `retry-after` (integer/float seconds, or
/// an HTTP date relative to now). `None` when absent or unparseable.
fn retry_after_hint_ms(headers: &reqwest::header::HeaderMap) -> Option<u64>
```

(HTTP-date parsing via `chrono`'s RFC 2822 parser — `chrono` is already a runtime dependency; a date in the past yields `None`.)

### 5.4 `crates/runtime/src/agent.rs`

1. **`RetryNotice` + `DeltaSink::on_retry`** (at the DeltaSink seam, ~line 741):

```rust
/// A driver's announcement that the previous model request failed transiently
/// and it is waiting `delay_ms` before retry `attempt` of `max_attempts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryNotice {
    pub attempt: u32,
    pub max_attempts: u32,
    /// The classifier's bounded reason (e.g. "provider is overloaded").
    pub message: String,
    pub delay_ms: u64,
}

pub trait DeltaSink: Send {
    fn on_text(&mut self, chunk: &str);
    /// Default no-op so `NullDeltaSink` and every test sink compile unchanged.
    fn on_retry(&mut self, _notice: &RetryNotice) {}
}
```

2. **`ChannelSink` carries an enum** (~line 780) — one ordered queue for both:

```rust
enum SinkEvent {
    Text(String),
    Retry(RetryNotice),
}

struct ChannelSink {
    tx: mpsc::UnboundedSender<SinkEvent>,
}

impl DeltaSink for ChannelSink {
    fn on_text(&mut self, chunk: &str) {
        if chunk.is_empty() { return; }
        let _ = self.tx.send(SinkEvent::Text(chunk.to_string()));
    }
    fn on_retry(&mut self, notice: &RetryNotice) {
        let _ = self.tx.send(SinkEvent::Retry(notice.clone()));
    }
}
```

3. **The loop's channel and select-drain** (~lines 2642–2735): the channel becomes `mpsc::unbounded_channel::<SinkEvent>()`. The `Some(chunk) = rx.recv()` arm and the post-step `try_recv` drain both match on the enum. `SinkEvent::Text` keeps today's behavior byte-for-byte (sets `streamed_this_step`, newline-flush, coalesce window). `SinkEvent::Retry` first flushes `pending` (so text the reader already saw lands in the ledger before the retry marker), then emits — and does **not** set `streamed_this_step`:

```rust
SinkEvent::Retry(notice) => {
    self.flush_deltas(&run, &run_actor, &mut pending).await?;
    self.emit(
        run.session_id,
        run_actor.clone(),
        EventBody::ModelRetrying {
            run_id: run.run_id,
            attempt: notice.attempt,
            max_attempts: notice.max_attempts,
            message: notice.message,
            delay_ms: notice.delay_ms,
        },
    )
    .await?;
}
```

(In the synchronous post-step `try_recv` drain, a queued `Retry` is emitted the same way; a `Text` appends to `pending` as today.)

4. **`FrameworkModelDriver::next_step`** (~line 7108) — the rewrite; `MODEL_RETRY_BACKOFF` (~line 7161) is deleted:

```rust
async fn next_step(&self, transcript: &[TurnItem], tools: &[ToolDefinition],
                   sink: &mut dyn DeltaSink) -> anyhow::Result<StepOutcome> {
    use codypendent_providers::retry;
    let mut attempt: u32 = 0;
    loop {
        let mut streamed = false;
        let error = match self.stream_once(transcript, tools, sink, &mut streamed).await {
            Ok(outcome) => return Ok(outcome),
            Err(error) => error,
        };
        // THE hard rule (unchanged): once any delta reached the sink, the
        // failure is final — a retry would re-stream the reply from the top
        // and the ledger would carry it twice.
        attempt += 1;
        let text = error.to_string();
        let decision = match retry::retryable(&text) {
            Some(d) if !streamed && attempt <= retry::RETRY_MAX_RETRIES => d,
            _ => return Err(error),
        };
        let wait = retry::delay_ms(
            attempt,
            retry::parse_retry_after_hint(&text),
            retry::entropy_jitter(),
        );
        sink.on_retry(&RetryNotice {
            attempt,
            max_attempts: retry::RETRY_MAX_RETRIES,
            message: decision.message,
            delay_ms: wait,
        });
        // The loop races `next_step` against cancellation and the wall clock,
        // so dropping this future cancels the wait (unchanged property).
        tokio::time::sleep(Duration::from_millis(wait)).await;
    }
}
```

### 5.5 `crates/protocol/src/events.rs`

New `EventBody` variant, placed with the Phase 1 run events:

```rust
/// The daemon's model request failed transiently and the driver is waiting
/// out a backoff before retry `attempt` of `max_attempts`. Purely
/// informational: a run that ultimately fails still ends with its own
/// `RunStateChanged`/`RunCompleted`; a retry that succeeds is followed by
/// ordinary `ModelStreamDelta`s. Additive: an older client deserializes
/// this to `Unknown` (RULE 1) and renders a placeholder.
ModelRetrying {
    run_id: RunId,
    attempt: u32,
    max_attempts: u32,
    /// Bounded classifier reason (e.g. "provider is overloaded").
    message: String,
    /// The wait before the retry fires, in milliseconds.
    delay_ms: u64,
},
```

No `Option` fields — every field is always measured (the notice is only created when a retry is actually scheduled).

### 5.6 `crates/tui/src/state.rs`

Extend `RunActivity` (line 947):

```rust
pub enum RunActivity {
    #[default]
    Idle,
    Thinking,
    Streaming,
    RunningTool(String),
    /// The model request hit a transient failure; the daemon is backing off
    /// before retry `attempt` of `max_attempts`.
    Retrying { attempt: u32, max_attempts: u32 },
}
```

### 5.7 `crates/tui/src/reduce.rs`

New arm beside `EventBody::ModelStreamDelta` (line 1717):

```rust
EventBody::ModelRetrying { run_id, attempt, max_attempts, .. } => {
    if let Some(run) = state.run_mut(run_id) {
        run.activity = RunActivity::Retrying { attempt, max_attempts };
    }
}
```

(No transcript entry — the retry is status, not content. A subsequent `ModelStreamDelta` flips activity to `Streaming`; a terminal failure arrives as `RunStateChanged`, which already resets activity.)

### 5.8 `crates/tui/src/render.rs`

`activity_status_line` (line 2239) gains an arm:

```rust
RunActivity::Retrying { attempt, max_attempts } => {
    format!("retrying ({attempt}/{max_attempts})…")
}
```

### 5.9 Dependencies

None. No new crates anywhere (`chrono`, `reqwest`, `tokio` are already runtime deps; the providers module is std-only).

## 6. Protocol & persistence

- **New event**: `EventBody::ModelRetrying` as in §5.5. Wire shape (internally tagged):

```json
{"type":"ModelRetrying","run_id":"…","attempt":2,"max_attempts":5,
 "message":"provider is overloaded","delay_ms":4231}
```

- **Back-compat**: additive under the existing `#[serde(other)] Unknown` fallback; old ledger bytes and old daemons are unaffected. The Phase 0 fixture test must keep passing untouched.
- **SQLite migrations**: none. Event bodies are stored as JSON in the existing ledger; no schema change.

## 7. Acceptance criteria

1. `codypendent_providers::retry::retryable("connection refused")`, `…("HTTP 524 from upstream")`, `…("Overloaded")`, `…("socket hang up")`, `…("resource_exhausted")` all return `Some`; `…("invalid api key")`, `…("model `x` is not registered")`, `…("provider did not list this model")`, and `…("id 15005 not found")` return `None`.
   RUN: `cargo test -p codypendent-providers retry` EXPECT: all pass.
2. `delay_ms(1, None, 0.0) == 2_000`, `delay_ms(5, None, 0.0) == 30_000` (capped), `delay_ms(2, None, 0.5) == 4_500`, `delay_ms(1, Some(1_234), 0.9) == 1_234`, `delay_ms(1, Some(u64::MAX), 0.0) == RETRY_MAX_DELAY_MS`.
3. A transient pre-stream failure is retried up to 5 times (6 requests total) and then surfaced; a permanent failure gets exactly 1 request; a mid-stream failure after first byte gets exactly 1 request regardless of class.
   RUN: `cargo test -p codypendent-runtime retr` EXPECT: the updated paused-clock tests pass.
4. Every retry attempt persists and publishes exactly one `ModelRetrying` event with the correct `attempt`/`max_attempts`, before the backoff sleep completes, and never marks the step as streamed.
5. `EventBody::ModelRetrying` round-trips through serde, and a payload with an unknown future tag still parses to `Unknown`.
   RUN: `cargo test -p codypendent-protocol` EXPECT: pass, including the untouched Phase 0 fixture test.
6. The TUI reducer folds `ModelRetrying` into `RunActivity::Retrying`, and `activity_status_line` renders `retrying (2/5)…`.
   RUN: `cargo test -p codypendent-tui` EXPECT: pass.
7. `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace` EXPECT: green.

## 8. Tests

**`crates/providers/src/retry.rs` (inline `#[cfg(test)]`, matching `model.rs`'s style):**

- `transient_phrases_and_statuses_are_retryable` — the positive list from AC 1.
- `unrecognized_and_permanent_messages_are_not_retryable` — the negative list, explicitly including `"provider did not list this model"` (the bare-"unavailable" deviation) and the digit-run guard (`15005`).
- `delay_follows_the_reference_schedule` — AC 2 values, including jitter arithmetic and both caps.
- `retry_after_hint_wins_over_backoff` — `Some(hint)` bypasses exponential entirely.
- `retry_after_marker_round_trips` — `parse_retry_after_hint("… [retry-after-ms=2500]") == Some(2500)`; garbage and absent markers yield `None`.

**`crates/runtime/src/agent.rs` (update the existing paused-clock retry mod at ~line 12757, keeping its scripted `ChatClient` idiom):**

- `a_permanent_failure_is_surfaced_without_a_single_retry` — unchanged assertion (1 request).
- `retries_stop_after_the_backoff_schedule_is_exhausted` — updated: expect `1 + RETRY_MAX_RETRIES = 6` requests (was 4).
- `streamed_text_vetoes_the_retry_even_for_a_transient_class` — unchanged assertion.
- `each_retry_attempt_reaches_the_sink_as_a_notice` (new) — a recording `DeltaSink` sees `on_retry` with attempts `1..=5` and the exhausted run still fails; `on_retry` never precedes a duplicate `on_text`.

**`crates/runtime/src/models.rs`:**

- `header_auth_client_embeds_retry_after_hint` (wiremock, alongside the existing HTTP tests) — a 429 with `retry-after: 3` yields an error message containing `[retry-after-ms=3000]`.
- `classify_provider_message_still_agrees_with_failure_class` — the delegate returns `Transient`/`Permanent` for the same fixtures the old tests used.

**`crates/protocol/src/events.rs`:** add `ModelRetrying` to `every_phase1_event_body_round_trips` (the existing `round_trip` helper).

**`crates/tui/src/reduce.rs` / `render.rs`:** `model_retrying_sets_retrying_activity` (reducer folds the event; a following `ModelStreamDelta` restores `Streaming`); `activity_status_line` snapshot for the new arm.

## 9. Gotchas

1. **The streamed-veto is load-bearing** — do not "improve" it away to match opencode. Re-streaming duplicates ledger evidence; a failed step is recoverable, a corrupted transcript is not.
2. **Longer stalls are now possible**: worst case ≈ 2+4+8+16+30 s of backoff (vs 7 s today). This is safe only because the backoff sleep runs inside `step_fut`, which the loop races against `cancel.cancelled()` and the `MAX_WALL_CLOCK_SECS` deadline (agent.rs ~line 2664) — a cancelled run or exhausted wall clock drops the future mid-sleep. Do not move the retry loop outside that race.
3. **Changing the classifier changes candidate resolution too**: `ModelsError::failure_class` → `classify_provider_message` → the new table. The added phrases (e.g. `internal error`, `network error`) make more `/models`-probe failures Transient. That is the intended one-taxonomy property; just don't fork the lists back apart.
4. **Digit-run status matching, never substrings** — the reference regexes happily match `500` inside larger tokens; the existing codypendent guard (`15005` must not match) is stricter and must be kept.
5. **Paused-clock tests and jitter**: `#[tokio::test(start_paused = true)]` auto-advances any sleep, so nondeterministic jitter does not slow tests — but assertions must count *requests*, never wall time.
6. **Retry notices must not disturb delta coalescing**: emit-before-sleep, flush `pending` first, never set `streamed_this_step`, never push into `pending`. Getting this wrong either loses journaled text on the abnormal path or makes a retried step un-retryable.
7. **Retry-After is best-effort**: only `HeaderAuthChatClient` carries the hint. Anthropic's SDK client and the stock OpenAI client surface strings without headers; they silently fall back to exponential. Don't fabricate hints from body text (bodies like "try again in 20s" are not contract).
8. **`Retry-After` as HTTP-date**: parse relative to now and clamp negatives to `None` (the reference does `Date.parse(...) - Date.now()` guarded `> 0`).
9. **Event ordering**: `ModelRetrying` must go through the loop's `emit` (persist-before-publish). Emitting from inside the driver directly would bypass the journal and violate invariant 5.

## 10. Out of scope

- A typed provider error taxonomy (opencode's `APIError` with `statusCode`/`responseHeaders`/`responseBody` on persisted messages). The seam stays string-shaped; this port meets it there.
- Upsell/limit `action`s on the retry status (`FreeUsageLimitError`/`GoUsageLimitError`, `GO_UPSELL_URL`) — opencode-platform features.
- Retrying ACP-driven runs (external agents own their loop), the embeddings client, `check_model` probes, or non-model tools.
- Context-overflow handling (opencode routes it to compaction; codypendent's compaction path is separate and unchanged).
- A TUI live countdown to the next attempt (`next` timestamp rendering); `retrying (n/m)…` is the deliverable — a countdown can layer on `delay_ms` later.
