// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Session recordings in asciicast v2: a header line, then one JSON array
//! per event, `[seconds, "o", text]` for output and `[seconds, "r",
//! "COLSxROWS"]` for a resize. Only output is recorded, never what the
//! user typed: a password typed at a prompt that does not echo stays out.

use std::fmt::Write as _;

use crate::TerminalSize;

/// A recording, as listed: what can be shown without opening it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingMeta {
    pub id: String,
    /// What the pane was called: a host's label, or "Local".
    pub title: String,
    /// The saved host, when it was one.
    pub host: Option<String>,
    /// Milliseconds since the Unix epoch.
    pub started_ms: i64,
    pub duration_ms: u64,
    /// Output bytes recorded.
    pub bytes: u64,
    /// Recording stopped before the session did: the app was busy writing,
    /// or the size limit was reached.
    pub truncated: bool,
    /// The recording was ended cleanly; a crash leaves it `false`.
    pub complete: bool,
}

/// A point in a recording the user marked, with an optional note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub at_ms: u64,
    pub label: String,
    pub comment: String,
}

/// Bookmarks per recording, and their text, at most this many and long.
pub const MAX_BOOKMARKS: usize = 1000;
pub const MAX_BOOKMARK_TEXT: usize = 4096;

/// The header line: the first terminal size and when recording began.
pub fn cast_header(size: TerminalSize, started_ms: i64, title: &str) -> String {
    let mut line = format!(
        r#"{{"version":2,"width":{},"height":{},"timestamp":{},"title":"#,
        size.cols(),
        size.rows(),
        started_ms.div_euclid(1000),
    );
    push_json_string(&mut line, title);
    line.push_str(r#","env":{"TERM":"xterm-256color"}}"#);
    line.push('\n');
    line
}

/// An output event line.
pub fn cast_output(at_us: u64, text: &str) -> String {
    let mut line = event_start(at_us, 'o');
    push_json_string(&mut line, text);
    line.push_str("]\n");
    line
}

/// A resize event line.
pub fn cast_resize(at_us: u64, size: TerminalSize) -> String {
    let mut line = event_start(at_us, 'r');
    let _ = write!(line, r#""{}x{}"]"#, size.cols(), size.rows());
    line.push('\n');
    line
}

fn event_start(at_us: u64, kind: char) -> String {
    // Seconds with microseconds, as asciinema writes them.
    format!(
        "[{}.{:06}, \"{kind}\", ",
        at_us / 1_000_000,
        at_us % 1_000_000
    )
}

/// Appends `text` as a JSON string, copying runs that need no escape
/// whole. O(n).
fn push_json_string(out: &mut String, text: &str) {
    out.reserve(text.len() + 2);
    out.push('"');
    let bytes = text.as_bytes();
    let mut run = 0;
    let mut at = 0;
    while at < bytes.len() {
        let b = bytes[at];
        // U+2028 and U+2029, which JavaScript once read as line ends.
        let separator = b == 0xe2
            && bytes.get(at + 1) == Some(&0x80)
            && matches!(bytes.get(at + 2), Some(0xa8 | 0xa9));
        if b < 0x20 || b == b'"' || b == b'\\' || b == 0x7f || separator {
            // Escapes are ASCII or whole characters: `run..at` ends on a
            // character boundary.
            out.push_str(&text[run..at]);
            match b {
                b'"' => out.push_str("\\\""),
                b'\\' => out.push_str("\\\\"),
                b'\n' => out.push_str("\\n"),
                b'\r' => out.push_str("\\r"),
                b'\t' => out.push_str("\\t"),
                0xe2 => {
                    let _ = write!(
                        out,
                        "\\u{:04x}",
                        if bytes[at + 2] == 0xa8 {
                            0x2028
                        } else {
                            0x2029
                        }
                    );
                    at += 2;
                }
                _ => {
                    let _ = write!(out, "\\u{b:04x}");
                }
            }
            at += 1;
            run = at;
        } else {
            at += 1;
        }
    }
    out.push_str(&text[run..]);
    out.push('"');
}

/// Turns output bytes into text event by event. A character cut across two
/// chunks waits for its rest; bytes that are not UTF-8 are dropped, as
/// xterm.js drops them, so the replay lines up with what the pane showed.
#[derive(Debug, Default)]
pub struct Utf8Carry {
    held: Vec<u8>,
}

impl Utf8Carry {
    /// The text `bytes` completes, holding back at most 3 bytes of a
    /// character still being cut. O(n).
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.held.extend_from_slice(bytes);
        let keep = incomplete_tail(&self.held);
        let ready = self.held.len() - keep;
        let text = valid_only(&self.held[..ready]);
        self.held.drain(..ready);
        text
    }

    /// What is left at the end; a character cut off is dropped.
    pub fn finish(&mut self) -> String {
        let text = valid_only(&self.held);
        self.held.clear();
        text
    }
}

/// The UTF-8 in `bytes`, without what is not. O(n).
fn valid_only(mut bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    loop {
        match std::str::from_utf8(bytes) {
            Ok(text) => {
                out.push_str(text);
                return out;
            }
            Err(error) => {
                let (good, rest) = bytes.split_at(error.valid_up_to());
                // Checked just now: whole characters.
                out.push_str(std::str::from_utf8(good).unwrap_or_default());
                let skip = error.error_len().unwrap_or(rest.len()).max(1);
                bytes = &rest[skip.min(rest.len())..];
            }
        }
    }
}

/// How many bytes at the end begin a character the next chunk may finish.
fn incomplete_tail(bytes: &[u8]) -> usize {
    // A UTF-8 character is at most 4 bytes: look at the last 3 for a lead.
    for back in 1..=bytes.len().min(3) {
        let b = bytes[bytes.len() - back];
        if b & 0xC0 == 0x80 {
            continue;
        }
        let needs = match b {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => return 0,
        };
        return if back < needs { back } else { 0 };
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_asciicast_v2_lines() {
        let size = TerminalSize::new(120, 40).unwrap();
        assert_eq!(
            cast_header(size, 1_700_000_123_456, "db \"prod\""),
            "{\"version\":2,\"width\":120,\"height\":40,\"timestamp\":1700000123,\
             \"title\":\"db \\\"prod\\\"\",\"env\":{\"TERM\":\"xterm-256color\"}}\n"
        );
        assert_eq!(
            cast_output(1_500_042, "a\u{1b}[1m\\\r\n\u{0}é\u{2028}\"\u{7f}"),
            "[1.500042, \"o\", \"a\\u001b[1m\\\\\\r\\n\\u0000é\\u2028\\\"\\u007f\"]\n"
        );
        assert_eq!(
            cast_resize(61_000_000, TerminalSize::new(80, 24).unwrap()),
            "[61.000000, \"r\", \"80x24\"]\n"
        );
    }

    #[test]
    fn a_character_cut_across_chunks_comes_out_whole() {
        let text = "é漢🙂";
        let bytes = text.as_bytes();
        // Cut at every byte: the text comes out the same.
        for cut in 0..=bytes.len() {
            let mut carry = Utf8Carry::default();
            let mut out = carry.push(&bytes[..cut]);
            out.push_str(&carry.push(&bytes[cut..]));
            out.push_str(&carry.finish());
            assert_eq!(out, text, "cut at {cut}");
        }
    }

    #[test]
    fn bytes_that_are_not_utf8_are_dropped_as_the_terminal_drops_them() {
        let mut carry = Utf8Carry::default();
        // Latin-1, and an overlong form.
        assert_eq!(carry.push(b"ol\xe9 x"), "ol x");
        assert_eq!(carry.push(b"a\xc0\xafb"), "ab");
        // A lone lead at the end waits, and goes at the finish.
        assert_eq!(carry.push(b"c\xe6"), "c");
        assert_eq!(carry.finish(), "");
    }
}
