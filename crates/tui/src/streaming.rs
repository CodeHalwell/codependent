//! Adaptive stream chunking and table holdback for agent responses (Adoption 11 M3).
//!
//! # Symptom -> Constant Tuning Guide (Adoption 12 A5)
//!
//! | Observed Symptom | Tuning Action | Primary Constants |
//! | :--- | :--- | :--- |
//! | Streaming feels bursty or staggered | Lower line hold thresholds | [`ENTER_QUEUE_DEPTH_LINES`], [`ENTER_OLDEST_AGE`] |
//! | Tables flicker or render partially | Adjust table holdback detector | [`TableHoldbackScanner`], [`is_table_delimiter_line`] |
//! | Backpressure lag on fast models | Tighten catch-up entry/exit hysteresis | [`ENTER_QUEUE_DEPTH_LINES`], [`EXIT_QUEUE_DEPTH_LINES`], [`EXIT_HOLD`] |
//! | Rapid thrashing between smooth & catch-up | Increase re-entry cooldown | [`REENTER_CATCH_UP_HOLD`], [`EXIT_HOLD`] |

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::table_detect::{
    is_table_delimiter_line, is_table_header_line, strip_blockquote_prefix, FenceKind, FenceTracker,
};

/// Queue-depth threshold that allows entering catch-up mode.
const ENTER_QUEUE_DEPTH_LINES: usize = 8;
/// Oldest-line age threshold that allows entering catch-up mode.
const ENTER_OLDEST_AGE: Duration = Duration::from_millis(120);
/// Queue-depth threshold used when evaluating catch-up exit hysteresis.
const EXIT_QUEUE_DEPTH_LINES: usize = 2;
/// Oldest-line age threshold used when evaluating catch-up exit hysteresis.
const EXIT_OLDEST_AGE: Duration = Duration::from_millis(40);
/// Minimum duration queue pressure must stay below exit thresholds to leave catch-up mode.
const EXIT_HOLD: Duration = Duration::from_millis(250);
/// Cooldown window after a catch-up exit that suppresses immediate re-entry.
const REENTER_CATCH_UP_HOLD: Duration = Duration::from_millis(250);
/// Queue-depth cutoff that marks backlog as severe for faster convergence.
const SEVERE_QUEUE_DEPTH_LINES: usize = 64;
/// Oldest-line age cutoff that marks backlog as severe for faster convergence.
const SEVERE_OLDEST_AGE: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChunkingMode {
    #[default]
    Smooth,
    CatchUp,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueueSnapshot {
    pub queued_lines: usize,
    pub oldest_age: Option<Duration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrainPlan {
    Single,
    Batch(usize),
}

#[derive(Debug, Clone)]
pub struct AdaptiveChunkingPolicy {
    mode: ChunkingMode,
    below_exit_since: Option<Instant>,
    last_exit: Option<Instant>,
}

impl Default for AdaptiveChunkingPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl AdaptiveChunkingPolicy {
    #[must_use]
    pub fn new() -> Self {
        Self {
            mode: ChunkingMode::Smooth,
            below_exit_since: None,
            last_exit: None,
        }
    }

    pub fn decide(&mut self, snapshot: QueueSnapshot, now: Instant) -> DrainPlan {
        if snapshot.queued_lines == 0 {
            self.mode = ChunkingMode::Smooth;
            self.below_exit_since = None;
            return DrainPlan::Single;
        }

        match self.mode {
            ChunkingMode::Smooth => {
                let severe = snapshot.queued_lines >= SEVERE_QUEUE_DEPTH_LINES
                    || snapshot.oldest_age.is_some_and(|a| a >= SEVERE_OLDEST_AGE);
                let standard = snapshot.queued_lines >= ENTER_QUEUE_DEPTH_LINES
                    || snapshot.oldest_age.is_some_and(|a| a >= ENTER_OLDEST_AGE);

                let cooldown_active = self
                    .last_exit
                    .is_some_and(|exit| now.duration_since(exit) < REENTER_CATCH_UP_HOLD);

                if severe || (standard && !cooldown_active) {
                    self.mode = ChunkingMode::CatchUp;
                    self.below_exit_since = None;
                }
            }
            ChunkingMode::CatchUp => {
                let below_depth = snapshot.queued_lines <= EXIT_QUEUE_DEPTH_LINES;
                let below_age = snapshot.oldest_age.is_none_or(|age| age <= EXIT_OLDEST_AGE);

                if below_depth && below_age {
                    let exit_start = *self.below_exit_since.get_or_insert(now);
                    if now.duration_since(exit_start) >= EXIT_HOLD {
                        self.mode = ChunkingMode::Smooth;
                        self.below_exit_since = None;
                        self.last_exit = Some(now);
                    }
                } else {
                    self.below_exit_since = None;
                }
            }
        }

        match self.mode {
            ChunkingMode::Smooth => DrainPlan::Single,
            ChunkingMode::CatchUp => DrainPlan::Batch(snapshot.queued_lines),
        }
    }
}

/// Result of scanning accumulated raw source for pipe-table patterns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableHoldbackState {
    /// No table detected -- all rendered lines can flow into the stable view.
    None,
    /// The last non-blank line looks like a table header row but no delimiter
    /// row has followed yet. Hold back in case the next delta is a delimiter.
    PendingHeader { header_start: usize },
    /// A header + delimiter pair was found -- the source contains a confirmed
    /// table. Content from the table header onward stays mutable.
    Confirmed { table_start: usize },
}

#[derive(Clone, Copy)]
struct PreviousLineState {
    source_start: usize,
    #[allow(dead_code)]
    fence_kind: FenceKind,
    is_header: bool,
}

/// Incremental scanner for table holdback state on append-only source streams.
pub struct TableHoldbackScanner {
    source_offset: usize,
    fence_tracker: FenceTracker,
    previous_line: Option<PreviousLineState>,
    pending_header_start: Option<usize>,
    confirmed_table_start: Option<usize>,
}

impl Default for TableHoldbackScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl TableHoldbackScanner {
    #[must_use]
    pub fn new() -> Self {
        Self {
            source_offset: 0,
            fence_tracker: FenceTracker::new(),
            previous_line: None,
            pending_header_start: None,
            confirmed_table_start: None,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    #[must_use]
    pub fn state(&self) -> TableHoldbackState {
        if let Some(table_start) = self.confirmed_table_start {
            TableHoldbackState::Confirmed { table_start }
        } else if let Some(header_start) = self.pending_header_start {
            TableHoldbackState::PendingHeader { header_start }
        } else {
            TableHoldbackState::None
        }
    }

    pub fn push_source_chunk(&mut self, source_chunk: &str) {
        if source_chunk.is_empty() {
            return;
        }

        for source_line in source_chunk.split_inclusive('\n') {
            self.push_line(source_line);
        }
    }

    fn push_line(&mut self, source_line: &str) {
        let line = source_line.strip_suffix('\n').unwrap_or(source_line);
        let line_start = self.source_offset;
        self.source_offset += source_line.len();

        let fence_kind = self.fence_tracker.kind();
        self.fence_tracker.advance(line);

        if fence_kind == FenceKind::Other {
            self.previous_line = None;
            self.pending_header_start = None;
            return;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            // A blank line closes an active table.
            self.confirmed_table_start = None;
            self.pending_header_start = None;
            self.previous_line = None;
            return;
        }

        let candidate_text = strip_blockquote_prefix(trimmed);
        let is_header = is_table_header_line(candidate_text);
        let is_delimiter = is_table_delimiter_line(candidate_text);

        if let Some(prev) = self.previous_line {
            if prev.is_header && is_delimiter {
                self.confirmed_table_start = Some(prev.source_start);
                self.pending_header_start = None;
            }
        }

        if self.confirmed_table_start.is_none() {
            if is_header && !is_delimiter {
                self.pending_header_start = Some(line_start);
            } else {
                self.pending_header_start = None;
            }
        }

        self.previous_line = Some(PreviousLineState {
            source_start: line_start,
            fence_kind,
            is_header,
        });
    }
}

/// One queued line in the streaming collector.
#[derive(Debug, Clone)]
pub struct PendingStreamLine {
    pub end_offset: usize,
    pub arrived: Instant,
}

/// Collects streamed markdown deltas, gates on complete newlines, and holds
/// back unconfirmed/active tables during progressive rendering.
pub struct MarkdownStreamCollector {
    raw_source: String,
    committed_offset: usize,
    pending_queue: VecDeque<PendingStreamLine>,
    policy: AdaptiveChunkingPolicy,
    holdback: TableHoldbackScanner,
}

impl Default for MarkdownStreamCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkdownStreamCollector {
    #[must_use]
    pub fn new() -> Self {
        Self {
            raw_source: String::new(),
            committed_offset: 0,
            pending_queue: VecDeque::new(),
            policy: AdaptiveChunkingPolicy::new(),
            holdback: TableHoldbackScanner::new(),
        }
    }

    /// Push an incoming streaming delta.
    pub fn push_delta(&mut self, delta: &str, now: Instant) {
        if delta.is_empty() {
            return;
        }
        let prev_len = self.raw_source.len();
        self.raw_source.push_str(delta);
        let mut search_from = prev_len;

        while let Some(rel) = self.raw_source[search_from..].find('\n') {
            let newline_end = search_from + rel + 1;
            let line_slice = &self.raw_source[search_from..newline_end];
            self.holdback.push_source_chunk(line_slice);
            self.pending_queue.push_back(PendingStreamLine {
                end_offset: newline_end,
                arrived: now,
            });
            search_from = newline_end;
        }
    }

    /// Advance the collector on a timer tick according to pacing policy.
    pub fn tick(&mut self, now: Instant) -> &str {
        let snapshot = QueueSnapshot {
            queued_lines: self.pending_queue.len(),
            oldest_age: self
                .pending_queue
                .front()
                .map(|l| now.duration_since(l.arrived)),
        };
        let plan = self.policy.decide(snapshot, now);

        let lines_to_drain = match plan {
            DrainPlan::Single => 1.min(self.pending_queue.len()),
            DrainPlan::Batch(n) => n.min(self.pending_queue.len()),
        };

        for _ in 0..lines_to_drain {
            if let Some(line) = self.pending_queue.pop_front() {
                self.committed_offset = line.end_offset;
            }
        }

        self.visible_text()
    }

    /// Complete and finalize all remaining raw source.
    pub fn finalize(&mut self) -> &str {
        self.committed_offset = self.raw_source.len();
        self.pending_queue.clear();
        &self.raw_source
    }

    /// Visible text safe to parse and render.
    #[must_use]
    pub fn visible_text(&self) -> &str {
        let cap = match self.holdback.state() {
            TableHoldbackState::PendingHeader { header_start } => {
                self.committed_offset.min(header_start)
            }
            TableHoldbackState::Confirmed { .. } => self.raw_source.len(),
            TableHoldbackState::None => self.committed_offset,
        };
        let end = cap.min(self.raw_source.len());
        &self.raw_source[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_holdback_detects_confirmed_table() {
        let mut scanner = TableHoldbackScanner::new();
        scanner.push_source_chunk("Some intro text\n");
        assert_eq!(scanner.state(), TableHoldbackState::None);

        scanner.push_source_chunk("| Header 1 | Header 2 |\n");
        assert!(matches!(
            scanner.state(),
            TableHoldbackState::PendingHeader { .. }
        ));

        scanner.push_source_chunk("| --- | --- |\n");
        assert!(matches!(
            scanner.state(),
            TableHoldbackState::Confirmed { .. }
        ));

        scanner.push_source_chunk("| Val 1 | Val 2 |\n");
        assert!(matches!(
            scanner.state(),
            TableHoldbackState::Confirmed { .. }
        ));

        scanner.push_source_chunk("\n");
        assert_eq!(scanner.state(), TableHoldbackState::None);
    }

    #[test]
    fn adaptive_policy_switches_to_catchup_on_deep_queue() {
        let mut policy = AdaptiveChunkingPolicy::new();
        let now = Instant::now();
        let plan = policy.decide(
            QueueSnapshot {
                queued_lines: 10,
                oldest_age: Some(Duration::from_millis(150)),
            },
            now,
        );
        assert_eq!(plan, DrainPlan::Batch(10));
    }
}
