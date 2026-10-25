// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The host's lines, `#TYPE:payload` and LF, cut from its output as it
//! comes.

use super::TrzszError;

/// One line from the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub kind: String,
    pub payload: Vec<u8>,
}

/// The host's output, cut into lines. Scanning resumes where it stopped,
/// so a long line arriving in small reads costs O(n) in all.
#[derive(Debug, Default)]
pub struct Lines {
    buf: Vec<u8>,
    /// Bytes of `buf` known to hold no line end.
    scanned: usize,
    /// tmux runs on the host: lines it wrapped are joined, its status
    /// sequences dropped, and a line is read from its last frame.
    junk: bool,
}

/// What a line may have, at most, unless it is `#DATA`.
pub const MAX_LINE: usize = 64 << 10;
/// What a `#DATA` line may have, at most: a 10 MiB chunk, zlib and base64.
pub const MAX_DATA_LINE: usize = 16 << 20;

impl Lines {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// tmux runs on the host, its `#CFG` said.
    pub fn set_junk(&mut self, junk: bool) {
        self.junk = junk;
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    #[cfg(test)]
    pub(super) fn scanned_for_test(&self) -> usize {
        self.scanned
    }

    /// The bytes not read as a line, which are no longer the protocol's.
    pub fn take_rest(&mut self) -> Vec<u8> {
        self.scanned = 0;
        std::mem::take(&mut self.buf)
    }

    /// Bytes taken and not yet read as a line.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    /// The next whole line, read as a frame of type `expect` (which may
    /// end it in tmux's case), or `None` until its end comes. A line
    /// holding Ctrl+C is the host's interrupt. O(n) in new bytes.
    ///
    /// # Errors
    /// [`TrzszError::Interrupted`], [`TrzszError::TooLong`] past `max`,
    /// or a line that is no frame.
    pub fn next(&mut self, expect: &str, max: usize) -> Result<Option<Line>, TrzszError> {
        loop {
            let Some(found) = memchr::memchr(b'\n', &self.buf[self.scanned..]) else {
                if self.buf[self.scanned..].contains(&0x03) {
                    return Err(TrzszError::Interrupted);
                }
                self.scanned = self.buf.len();
                if self.buf.len() > max {
                    return Err(TrzszError::TooLong(max));
                }
                return Ok(None);
            };
            let end = self.scanned + found;
            if end > max {
                return Err(TrzszError::TooLong(max));
            }
            // tmux breaks a long line with CR LF: the line goes on.
            if self.junk && end > 0 && self.buf[end - 1] == b'\r' {
                self.buf.drain(end - 1..=end);
                self.scanned = end - 1;
                continue;
            }
            let mut line: Vec<u8> = self.buf.drain(..=end).collect();
            self.scanned = 0;
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.contains(&0x03) {
                return Err(TrzszError::Interrupted);
            }
            return read_frame(line, expect, self.junk).map(Some);
        }
    }
}

/// `line` as a frame. In tmux's case the frame starts at the last
/// `#<expect>:`, or the last `#`, and tmux's own sequences go.
fn read_frame(mut line: Vec<u8>, expect: &str, junk: bool) -> Result<Line, TrzszError> {
    if junk {
        line = without_status(&line);
        let wanted = format!("#{expect}:");
        let start = memchr::memmem::rfind(&line, wanted.as_bytes())
            .or_else(|| memchr::memrchr(b'#', &line))
            .unwrap_or(0);
        line.drain(..start);
    }
    let rest = line
        .strip_prefix(b"#")
        .ok_or_else(|| TrzszError::Protocol("a line that is no frame".into()))?;
    let colon = memchr::memchr(b':', rest)
        .ok_or_else(|| TrzszError::Protocol("a line that is no frame".into()))?;
    let kind = &rest[..colon];
    if kind.is_empty() || kind.len() > 8 || !kind.iter().all(u8::is_ascii_alphanumeric) {
        return Err(TrzszError::Protocol("a line that is no frame".into()));
    }
    Ok(Line {
        kind: String::from_utf8_lossy(kind).into_owned(),
        payload: rest[colon + 1..].to_vec(),
    })
}

/// `line` without tmux's `ESC P = ... ESC \` sequences. O(n).
fn without_status(line: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = memchr::memmem::find(rest, b"\x1bP=") {
        out.extend_from_slice(&rest[..at]);
        match memchr::memmem::find(&rest[at..], b"\x1b\\") {
            Some(end) => rest = &rest[at + end + 2..],
            None => {
                rest = &[];
            }
        }
    }
    out.extend_from_slice(rest);
    out
}

/// One line for the host: `#<kind>:<payload>` and LF.
#[must_use]
pub fn frame(kind: &str, payload: &str) -> Vec<u8> {
    format!("#{kind}:{payload}\n").into_bytes()
}
