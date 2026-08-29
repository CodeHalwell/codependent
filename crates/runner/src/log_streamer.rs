//! Bounded live log streaming and untrusted output sanitization.

use codypendent_sandbox::sanitize_untrusted;
use uuid::Uuid;

use crate::types::LogChunk;

/// Manages streaming bounded log chunks with monotonic sequence numbers.
pub struct LogStreamer {
    attempt_id: Uuid,
    max_output_bytes: usize,
    chunk_size: usize,
    sequence: u64,
    stdout_buffer: Vec<u8>,
    stderr_buffer: Vec<u8>,
    total_stdout_bytes: usize,
    total_stderr_bytes: usize,
    truncated: bool,
}

impl LogStreamer {
    /// Create a new log streamer for a specific job attempt.
    #[must_use]
    pub fn new(attempt_id: Uuid, max_output_mb: u64) -> Self {
        let max_output_bytes =
            usize::try_from(max_output_mb.saturating_mul(1024 * 1024)).unwrap_or(usize::MAX);
        Self {
            attempt_id,
            max_output_bytes,
            chunk_size: 64 * 1024, // 64 KiB chunk size
            sequence: 0,
            stdout_buffer: Vec::new(),
            stderr_buffer: Vec::new(),
            total_stdout_bytes: 0,
            total_stderr_bytes: 0,
            truncated: false,
        }
    }

    /// Ingest raw stdout bytes, sanitize them, and produce ready log chunks.
    pub fn ingest_stdout(&mut self, raw: &[u8]) -> Vec<LogChunk> {
        self.ingest_stream("stdout", raw)
    }

    /// Ingest raw stderr bytes, sanitize them, and produce ready log chunks.
    pub fn ingest_stderr(&mut self, raw: &[u8]) -> Vec<LogChunk> {
        self.ingest_stream("stderr", raw)
    }

    fn ingest_stream(&mut self, stream_name: &str, raw: &[u8]) -> Vec<LogChunk> {
        let mut chunks = Vec::new();
        let current_total = self.total_stdout_bytes + self.total_stderr_bytes;

        if current_total >= self.max_output_bytes {
            self.truncated = true;
            return chunks;
        }

        let available_budget = self.max_output_bytes - current_total;
        let bytes_to_take = raw.len().min(available_budget);

        if bytes_to_take < raw.len() {
            self.truncated = true;
        }

        let slice = &raw[..bytes_to_take];

        // Sanitize untrusted output (strip control sequences & bidi overrides)
        let text = String::from_utf8_lossy(slice);
        let sanitized =
            sanitize_untrusted(format!("runner:{}", self.attempt_id), &text, slice.len());
        let sanitized_bytes = sanitized.text.into_bytes();

        let buffer = if stream_name == "stdout" {
            self.total_stdout_bytes += slice.len();
            &mut self.stdout_buffer
        } else {
            self.total_stderr_bytes += slice.len();
            &mut self.stderr_buffer
        };

        buffer.extend_from_slice(&sanitized_bytes);

        while buffer.len() >= self.chunk_size {
            let chunk_data: Vec<u8> = buffer.drain(..self.chunk_size).collect();
            let chunk = LogChunk {
                attempt_id: self.attempt_id,
                sequence: self.sequence,
                stream: stream_name.to_string(),
                body: Some(chunk_data.clone()),
                object_key: None,
                byte_length: chunk_data.len(),
                truncated: self.truncated,
            };
            self.sequence += 1;
            chunks.push(chunk);
        }

        chunks
    }

    /// Flush any remaining buffered bytes as final chunks.
    pub fn flush(&mut self) -> Vec<LogChunk> {
        let mut chunks = Vec::new();

        if !self.stdout_buffer.is_empty() {
            let chunk_data = std::mem::take(&mut self.stdout_buffer);
            chunks.push(LogChunk {
                attempt_id: self.attempt_id,
                sequence: self.sequence,
                stream: "stdout".to_string(),
                body: Some(chunk_data.clone()),
                object_key: None,
                byte_length: chunk_data.len(),
                truncated: self.truncated,
            });
            self.sequence += 1;
        }

        if !self.stderr_buffer.is_empty() {
            let chunk_data = std::mem::take(&mut self.stderr_buffer);
            chunks.push(LogChunk {
                attempt_id: self.attempt_id,
                sequence: self.sequence,
                stream: "stderr".to_string(),
                body: Some(chunk_data.clone()),
                object_key: None,
                byte_length: chunk_data.len(),
                truncated: self.truncated,
            });
            self.sequence += 1;
        }

        chunks
    }

    /// Whether output was truncated due to exceeding size limits.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }
}
