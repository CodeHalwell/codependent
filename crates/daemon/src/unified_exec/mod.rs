use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

use codypendent_protocol::{RunId, SessionId};

pub mod head_tail_buffer;
pub mod manager;
pub mod process;
pub mod process_state;

pub use head_tail_buffer::HeadTailBuffer;
pub use manager::UnifiedExecManager;
pub use process::{OutputHandles, UnifiedExecProcess};
pub use process_state::ProcessState;

pub const MIN_YIELD_TIME_MS: u64 = 250;
pub const WINDOWS_INITIAL_EXEC_YIELD_TIME_FLOOR_MS: u64 = 10_000;
pub const MIN_EMPTY_YIELD_TIME_MS: u64 = 5_000;
pub const MAX_YIELD_TIME_MS: u64 = 30_000;
pub const DEFAULT_MAX_BACKGROUND_TERMINAL_TIMEOUT_MS: u64 = 300_000;
pub const DEFAULT_MAX_OUTPUT_TOKENS: usize = 10_000;
pub const UNIFIED_EXEC_OUTPUT_MAX_BYTES: usize = 1024 * 1024; // 1 MiB
pub const UNIFIED_EXEC_OUTPUT_MAX_TOKENS: usize = UNIFIED_EXEC_OUTPUT_MAX_BYTES / 4;
pub const MAX_UNIFIED_EXEC_PROCESSES: usize = 64;
pub const EARLY_EXIT_GRACE_PERIOD_MS: u64 = 150;
pub const POST_EXIT_CLOSE_WAIT_CAP_MS: u64 = 50;

pub const UNIFIED_EXEC_ENV: [(&str, &str); 10] = [
    ("NO_COLOR", "1"),
    ("TERM", "dumb"),
    ("LANG", "C.UTF-8"),
    ("LC_CTYPE", "C.UTF-8"),
    ("LC_ALL", "C.UTF-8"),
    ("COLORTERM", ""),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("GH_PAGER", "cat"),
    ("CODYPENDENT_CI", "1"),
];

/// Specification to open an interactive PTY process.
#[derive(Debug, Clone)]
pub struct OpenProcessSpec {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub environment: Vec<(String, String)>,
}

/// Budget for reading output from a process during a yield window.
#[derive(Debug, Clone, Copy)]
pub struct ReadBudget {
    pub yield_time_ms: u64,
    pub max_output_tokens: usize,
}

impl Default for ReadBudget {
    fn default() -> Self {
        Self {
            yield_time_ms: MIN_YIELD_TIME_MS,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        }
    }
}

/// Output returned by an exec or write_stdin call.
#[derive(Debug, Clone)]
pub struct ExecOutput {
    pub process_id: Option<i32>,
    pub exit_code: Option<i32>,
    pub wall_time: Duration,
    pub output: String,
    pub original_token_count: usize,
    pub omitted_bytes: usize,
}

/// Summary information for an active or recorded process.
#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub process_id: i32,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub command: String,
    pub cwd: PathBuf,
    pub running: bool,
    pub last_used: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Error)]
pub enum UnifiedExecError {
    #[error("Failed to create unified exec process: {message}")]
    CreateProcess { message: String },
    #[error("Unified exec process failed: {message}")]
    ProcessFailed { message: String },
    #[error("Unknown process id {process_id}")]
    UnknownProcessId { process_id: i32 },
    #[error("failed to write to stdin")]
    WriteToStdin,
    #[error("stdin is closed for this session; rerun shell.exec to keep stdin open")]
    StdinClosed,
    #[error("missing command line for unified exec request")]
    MissingCommandLine,
}

/// Clamp yield time to valid range according to operation type.
pub fn clamp_yield_time(requested: Option<u64>, is_empty_input: bool, max_empty: u64) -> Duration {
    let mut min = if is_empty_input {
        MIN_EMPTY_YIELD_TIME_MS
    } else {
        MIN_YIELD_TIME_MS
    };

    if cfg!(windows) && min < WINDOWS_INITIAL_EXEC_YIELD_TIME_FLOOR_MS {
        min = WINDOWS_INITIAL_EXEC_YIELD_TIME_FLOOR_MS;
    }

    let max = if is_empty_input {
        max_empty.max(min)
    } else {
        MAX_YIELD_TIME_MS
    };

    let raw = requested.unwrap_or(min);
    let clamped = raw.clamp(min, max);
    Duration::from_millis(clamped)
}

/// Resolve max output tokens to clamped value (1..=UNIFIED_EXEC_OUTPUT_MAX_TOKENS).
pub fn resolve_max_tokens(requested: Option<usize>) -> usize {
    match requested {
        Some(t) => t.clamp(1, UNIFIED_EXEC_OUTPUT_MAX_TOKENS),
        None => DEFAULT_MAX_OUTPUT_TOKENS,
    }
}

/// Format the omission marker inserted between retained head and tail.
pub fn format_output_omission_marker(omitted_bytes: usize) -> String {
    format!("\n... {omitted_bytes} bytes omitted ...\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_yield_time_bounds() {
        // Non-empty input clamps to 250ms..30000ms
        assert_eq!(
            clamp_yield_time(Some(1), false, 300_000),
            Duration::from_millis(250)
        );
        assert_eq!(
            clamp_yield_time(Some(600_000), false, 300_000),
            Duration::from_millis(30_000)
        );
        assert_eq!(
            clamp_yield_time(Some(5_000), false, 300_000),
            Duration::from_millis(5_000)
        );

        // Empty input (poll) clamps to 5000ms..max_empty
        assert_eq!(
            clamp_yield_time(Some(1), true, 300_000),
            Duration::from_millis(5_000)
        );
        assert_eq!(
            clamp_yield_time(Some(100_000), true, 300_000),
            Duration::from_millis(100_000)
        );
        assert_eq!(
            clamp_yield_time(Some(600_000), true, 300_000),
            Duration::from_millis(300_000)
        );
    }

    #[test]
    fn resolve_max_tokens_bounds() {
        assert_eq!(resolve_max_tokens(None), DEFAULT_MAX_OUTPUT_TOKENS);
        assert_eq!(resolve_max_tokens(Some(500)), 500);
        assert_eq!(resolve_max_tokens(Some(0)), 1);
        assert_eq!(
            resolve_max_tokens(Some(1_000_000)),
            UNIFIED_EXEC_OUTPUT_MAX_TOKENS
        );
    }
}
