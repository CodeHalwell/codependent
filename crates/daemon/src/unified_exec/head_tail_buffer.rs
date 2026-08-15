use std::collections::VecDeque;

use super::UNIFIED_EXEC_OUTPUT_MAX_BYTES;

/// A capped buffer that preserves a stable prefix ("head") and suffix ("tail"),
/// dropping the middle once it exceeds the configured maximum. The buffer is
/// symmetric: 50% of capacity to head, 50% to tail.
#[derive(Debug, Clone)]
pub struct HeadTailBuffer {
    max_bytes: usize,
    head_budget: usize,
    tail_budget: usize,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    omitted_bytes: usize,
}

impl Default for HeadTailBuffer {
    fn default() -> Self {
        Self::new(UNIFIED_EXEC_OUTPUT_MAX_BYTES)
    }
}

impl HeadTailBuffer {
    /// Create a new buffer that retains at most `max_bytes` of output.
    pub fn new(max_bytes: usize) -> Self {
        let head_budget = max_bytes / 2;
        let tail_budget = max_bytes.saturating_sub(head_budget);
        Self {
            max_bytes,
            head_budget,
            tail_budget,
            head: Vec::new(),
            tail: VecDeque::new(),
            omitted_bytes: 0,
        }
    }

    /// Total bytes currently retained by the buffer (head + tail).
    pub fn retained_bytes(&self) -> usize {
        self.head.len().saturating_add(self.tail.len())
    }

    /// Total bytes dropped from the middle due to the size cap.
    pub fn omitted_bytes(&self) -> usize {
        self.omitted_bytes
    }

    /// Total bytes observed by the buffer, including omitted bytes.
    pub fn total_bytes(&self) -> usize {
        self.retained_bytes().saturating_add(self.omitted_bytes)
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.head.is_empty() && self.tail.is_empty() && self.omitted_bytes == 0
    }

    /// Append a chunk of bytes to the buffer.
    pub fn push_chunk(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        if self.max_bytes == 0 {
            self.omitted_bytes = self.omitted_bytes.saturating_add(chunk.len());
            return;
        }

        // Fill the head budget first, then keep a capped tail.
        let remaining_head = self.head_budget.saturating_sub(self.head.len());
        let head_len = remaining_head.min(chunk.len());
        if head_len > 0 {
            self.head.extend_from_slice(&chunk[..head_len]);
        }
        self.push_to_tail(&chunk[head_len..]);
    }

    fn push_to_tail(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if self.tail_budget == 0 {
            self.omitted_bytes = self.omitted_bytes.saturating_add(bytes.len());
            return;
        }

        let incoming_len = bytes.len();
        if incoming_len >= self.tail_budget {
            let dropped_from_tail = self.tail.len();
            let dropped_from_incoming = incoming_len - self.tail_budget;
            self.omitted_bytes = self
                .omitted_bytes
                .saturating_add(dropped_from_tail)
                .saturating_add(dropped_from_incoming);
            self.tail.clear();
            self.tail
                .extend(bytes[dropped_from_incoming..].iter().copied());
        } else {
            let total_tail = self.tail.len() + incoming_len;
            if total_tail > self.tail_budget {
                let overflow = total_tail - self.tail_budget;
                for _ in 0..overflow {
                    self.tail.pop_front();
                }
                self.omitted_bytes = self.omitted_bytes.saturating_add(overflow);
            }
            self.tail.extend(bytes.iter().copied());
        }
    }

    /// Return the retained output as a single byte vector without markers.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.retained_bytes());
        out.extend_from_slice(&self.head);
        out.extend(self.tail.iter().copied());
        out
    }

    /// Return the retained output with an explicit marker between head and tail
    /// when bytes were omitted.
    pub fn to_bytes_with_omission_marker(&self) -> Vec<u8> {
        if self.omitted_bytes == 0 {
            return self.to_bytes();
        }

        let marker = format!("\n... {} bytes omitted ...\n", self.omitted_bytes);
        let mut out = Vec::with_capacity(self.retained_bytes().saturating_add(marker.len()));
        out.extend_from_slice(&self.head);
        out.extend_from_slice(marker.as_bytes());
        out.extend(self.tail.iter().copied());
        out
    }

    /// Drain the retained output and omission metadata, resetting this buffer.
    pub fn drain(&mut self) -> Self {
        Self {
            max_bytes: self.max_bytes,
            head_budget: self.head_budget,
            tail_budget: self.tail_budget,
            head: std::mem::take(&mut self.head),
            tail: std::mem::take(&mut self.tail),
            omitted_bytes: std::mem::take(&mut self.omitted_bytes),
        }
    }

    /// Append retained output from another buffer, preserving omissions.
    pub fn push_buffer(&mut self, mut other: Self) {
        self.push_chunk(&other.head);
        self.omitted_bytes = self.omitted_bytes.saturating_add(other.omitted_bytes);
        let tail_vec: Vec<u8> = other.tail.drain(..).collect();
        self.push_to_tail(&tail_vec);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_output_fits_without_omission() {
        let mut buf = HeadTailBuffer::new(100);
        buf.push_chunk(b"hello world");
        assert_eq!(buf.retained_bytes(), 11);
        assert_eq!(buf.omitted_bytes(), 0);
        assert_eq!(buf.to_bytes_with_omission_marker(), b"hello world");
    }

    #[test]
    fn budget_splits_evenly_and_omits_middle() {
        let mut buf = HeadTailBuffer::new(10); // 5 head, 5 tail
        buf.push_chunk(b"0123456789abcdef"); // 16 bytes
        assert_eq!(buf.retained_bytes(), 10);
        assert_eq!(buf.omitted_bytes(), 6);
        assert_eq!(buf.head, b"01234");
        let tail_vec: Vec<u8> = buf.tail.iter().copied().collect();
        assert_eq!(tail_vec, b"bcdef");

        let rendered = buf.to_bytes_with_omission_marker();
        assert_eq!(
            String::from_utf8_lossy(&rendered),
            "01234\n... 6 bytes omitted ...\nbcdef"
        );
    }

    #[test]
    fn drain_and_push_buffer_round_trips() {
        let mut buf1 = HeadTailBuffer::new(10);
        buf1.push_chunk(b"0123456789abcdef");
        let drained = buf1.drain();
        assert!(buf1.is_empty());

        let mut buf2 = HeadTailBuffer::new(10);
        buf2.push_buffer(drained);
        assert_eq!(buf2.omitted_bytes(), 6);
        assert_eq!(buf2.head, b"01234");
    }

    #[test]
    fn single_chunk_larger_than_tail() {
        let mut buf = HeadTailBuffer::new(10);
        buf.push_chunk(b"01234");
        buf.push_chunk(b"567890123456789");
        assert_eq!(buf.head, b"01234");
        let tail_vec: Vec<u8> = buf.tail.iter().copied().collect();
        assert_eq!(tail_vec, b"56789");
    }
}
