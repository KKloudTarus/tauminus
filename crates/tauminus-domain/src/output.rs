// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/// Coalesces small output reads into larger batches before they cross the IPC
/// boundary.
///
/// Each IPC message has a fixed cost, and Tauri sends payloads under 1 KiB as a
/// JSON number array. A busy shell produces many small reads, so the session
/// pump pushes every read here and sends only what this returns. The pump also
/// calls [`OutputBatcher::take`] on a short timer so a quiet prompt is never
/// held back.
///
/// Bounds, with `F` the flush size and `c` the chunk being pushed:
/// - a returned batch is at most `max(F, c)` bytes;
/// - the internal buffer never holds more than `F` bytes.
///
/// Cost: a chunk of at least `F` bytes pushed onto an empty buffer is returned
/// without copying. Other bytes are copied twice, into the buffer and into the
/// exact-size batch, so a push is O(c) time.
#[derive(Debug)]
pub struct OutputBatcher {
    buffer: Vec<u8>,
    flush_at: usize,
}

impl OutputBatcher {
    /// `flush_at` is the size that triggers an immediate batch. It must be at
    /// least 1; smaller values are raised to 1.
    pub fn new(flush_at: usize) -> Self {
        let flush_at = flush_at.max(1);
        Self {
            buffer: Vec::with_capacity(flush_at),
            flush_at,
        }
    }

    /// Adds `chunk`. Returns the batches that are ready, oldest first: none,
    /// one, or two when the buffered bytes and `chunk` cannot share a batch.
    pub fn push(&mut self, chunk: Vec<u8>) -> ReadyBatches {
        let mut ready = ReadyBatches::default();
        if self.buffer.len() + chunk.len() > self.flush_at && !self.buffer.is_empty() {
            ready.first = self.take();
        }
        if self.buffer.is_empty() && chunk.len() >= self.flush_at {
            ready.push(chunk);
            return ready;
        }
        self.buffer.extend_from_slice(&chunk);
        if self.buffer.len() >= self.flush_at {
            let batch = self.take();
            if let Some(batch) = batch {
                ready.push(batch);
            }
        }
        ready
    }

    /// Returns everything buffered, or `None` when the buffer is empty.
    ///
    /// The batch is allocated at its exact length and the internal buffer
    /// keeps its capacity, so a one-byte echo does not pin a 64 KiB
    /// allocation while it waits in the IPC queue.
    pub fn take(&mut self) -> Option<Vec<u8>> {
        if self.buffer.is_empty() {
            return None;
        }
        let batch = self.buffer.clone();
        self.buffer.clear();
        Some(batch)
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

/// Up to two batches produced by one [`OutputBatcher::push`], oldest first.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReadyBatches {
    first: Option<Vec<u8>>,
    second: Option<Vec<u8>>,
}

impl ReadyBatches {
    fn push(&mut self, batch: Vec<u8>) {
        if self.first.is_none() {
            self.first = Some(batch);
        } else {
            self.second = Some(batch);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.first.is_none()
    }
}

impl Iterator for ReadyBatches {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Vec<u8>> {
        self.first.take().or_else(|| self.second.take())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(batcher: &mut OutputBatcher, chunk: &[u8]) -> Vec<Vec<u8>> {
        batcher.push(chunk.to_vec()).collect()
    }

    #[test]
    fn holds_chunks_below_flush_size() {
        let mut batcher = OutputBatcher::new(8);
        assert_eq!(push(&mut batcher, b"abc"), Vec::<Vec<u8>>::new());
        assert_eq!(push(&mut batcher, b"de"), Vec::<Vec<u8>>::new());
        assert_eq!(batcher.len(), 5);
    }

    #[test]
    fn emits_batch_when_flush_size_reached() {
        let mut batcher = OutputBatcher::new(4);
        assert_eq!(push(&mut batcher, b"ab"), Vec::<Vec<u8>>::new());
        assert_eq!(push(&mut batcher, b"cd"), vec![b"abcd".to_vec()]);
        assert!(batcher.is_empty());
    }

    #[test]
    fn chunk_that_would_overflow_sends_the_buffer_first() {
        let mut batcher = OutputBatcher::new(4);
        push(&mut batcher, b"abc");
        assert_eq!(push(&mut batcher, b"de"), vec![b"abc".to_vec()]);
        assert_eq!(batcher.take(), Some(b"de".to_vec()));
    }

    #[test]
    fn oversized_chunk_passes_through_after_the_buffer() {
        let mut batcher = OutputBatcher::new(4);
        push(&mut batcher, b"a");
        assert_eq!(
            push(&mut batcher, b"bcdefg"),
            vec![b"a".to_vec(), b"bcdefg".to_vec()]
        );
        assert!(batcher.is_empty());
    }

    #[test]
    fn full_chunk_on_empty_buffer_is_not_copied() {
        let mut batcher = OutputBatcher::new(4);
        let chunk = b"wxyz".to_vec();
        let address = chunk.as_ptr();
        let batch = batcher.push(chunk).next().unwrap();
        assert_eq!(batch.as_ptr(), address, "same allocation, no copy");
    }

    #[test]
    fn batches_never_exceed_max_of_flush_size_and_chunk() {
        let mut batcher = OutputBatcher::new(10);
        for len in [3, 9, 1, 12, 7, 7, 10, 2] {
            let chunk = vec![b'x'; len];
            for batch in batcher.push(chunk) {
                assert!(
                    batch.len() <= len.max(10),
                    "{} > max({len}, 10)",
                    batch.len()
                );
            }
            assert!(batcher.len() <= 10);
        }
    }

    #[test]
    fn take_returns_pending_bytes_then_none() {
        let mut batcher = OutputBatcher::new(64);
        push(&mut batcher, b"prompt$ ");
        assert_eq!(batcher.take(), Some(b"prompt$ ".to_vec()));
        assert_eq!(batcher.take(), None);
    }

    #[test]
    fn empty_push_emits_nothing() {
        let mut batcher = OutputBatcher::new(4);
        assert_eq!(push(&mut batcher, b""), Vec::<Vec<u8>>::new());
        assert!(batcher.is_empty());
    }

    #[test]
    fn zero_flush_size_is_raised_to_one() {
        let mut batcher = OutputBatcher::new(0);
        assert_eq!(push(&mut batcher, b"x"), vec![b"x".to_vec()]);
    }

    #[test]
    fn taken_batch_has_exact_length_and_buffer_is_reused() {
        let mut batcher = OutputBatcher::new(64 * 1024);
        push(&mut batcher, b"x");
        let batch = batcher.take().unwrap();
        assert_eq!(batch.capacity(), 1);
        assert_eq!(batcher.len(), 0);
        push(&mut batcher, b"yz");
        assert_eq!(batcher.take(), Some(b"yz".to_vec()));
    }

    #[test]
    fn preserves_byte_order_across_batches() {
        let mut batcher = OutputBatcher::new(3);
        let mut out = Vec::new();
        for chunk in [&b"he"[..], b"llo", b" ", b"world", b"!"] {
            for batch in batcher.push(chunk.to_vec()) {
                out.extend(batch);
            }
        }
        out.extend(batcher.take().unwrap_or_default());
        assert_eq!(out, b"hello world!");
    }
}
