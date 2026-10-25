// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! trzsz, protocol 1 in base64, files only (ADR 0031): a host's `trz`
//! takes files from the terminal, its `tsz` sends them. The host prints a
//! trigger line, then the two sides exchange lines `#TYPE:payload` and LF,
//! numbers in decimal and text as base64 of zlib.
//!
//! Nothing here does I/O: the pump feeds output in and writes out what
//! comes back.

use std::sync::LazyLock;

use memchr::memmem::Finder;
use thiserror::Error;

mod codec;
mod download;
mod json;
mod line;
mod upload;

pub use codec::{decode, encode};
pub use download::{Download, MAX_FILES, closing};
pub use json::{Config, MAX_BUFSIZE, action};
pub use line::{Line, Lines, MAX_DATA_LINE, MAX_LINE, frame};
#[cfg(test)]
use upload::next_chunk;
pub use upload::{MAX_UPLOAD_NAME, Upload};

/// What the host wants, from the trigger's mode letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `tsz`: the host sends files.
    Send,
    /// `trz`: the host takes files.
    Receive,
    /// `trz -d`: the host takes files and folders; not supported.
    Folders,
}

/// A whole trigger line in the host's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    /// Where `::TRZSZ:TRANSFER:` starts.
    pub at: usize,
    /// Just past its line end.
    pub end: usize,
    pub mode: Mode,
    /// The host's trzsz, `x.y.z`.
    pub version: String,
    /// The host's id for this transfer: a repeat is old output.
    pub id: String,
    /// The host runs Windows: another line reader, not supported.
    pub windows: bool,
    /// It came inside a tmux control-mode line (`%output`): printed for a
    /// pane, never answered from this session.
    pub tmux_control: bool,
}

const MARK: &[u8] = b"::TRZSZ:TRANSFER:";
/// The longest trigger: the mark, a mode and its colon, a version of
/// three five-digit parts, a colon and a 13-digit id, a colon and a
/// five-digit port, and CR CR LF.
pub const TRIGGER_KEEP: usize = MARK.len() + 2 + 17 + 1 + MAX_ID + 1 + MAX_PART + 3;
const MAX_PART: usize = 5;
/// An id is 13 digits; old Windows hosts sent `1`.
const MAX_ID: usize = 13;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TrzszError {
    #[error("the host interrupted the transfer")]
    Interrupted,
    #[error("a line from the host is longer than {0} bytes")]
    TooLong(usize),
    #[error("the host sent what this transfer does not read: {0}")]
    Protocol(String),
    #[error("the host's trzsz asks for {0}, which Tauminus does not do")]
    Unsupported(String),
    #[error("the host stopped the transfer: {0}")]
    Remote(String),
}

/// The first whole trigger in `bytes`, if one is. One cut at the end of
/// `bytes` is not found: [`TRIGGER_KEEP`] bytes kept for the next read
/// find it then. O(n): a SIMD search.
#[must_use]
pub fn find_trigger(bytes: &[u8]) -> Option<Trigger> {
    static FINDER: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(MARK));
    // One inside a tmux control-mode line was printed for a pane, never
    // for this session to answer.
    FINDER
        .find_iter(bytes)
        .find_map(|at| parse(bytes, at).filter(|trigger| !trigger.tmux_control))
}

/// The trigger whose mark starts at `at`, whole.
fn parse(bytes: &[u8], at: usize) -> Option<Trigger> {
    let mut rest = &bytes[at + MARK.len()..];
    let mode = match rest.first()? {
        b'S' => Mode::Send,
        b'R' => Mode::Receive,
        b'D' => Mode::Folders,
        _ => return None,
    };
    rest = rest.get(1..)?.strip_prefix(b":")?;
    let mut version = String::new();
    for part in 0..3 {
        let digits = digits(rest, MAX_PART)?;
        version.push_str(std::str::from_utf8(&rest[..digits]).ok()?);
        rest = &rest[digits..];
        if part < 2 {
            rest = rest.strip_prefix(b".")?;
            version.push('.');
        }
    }
    rest = rest.strip_prefix(b":")?;
    let id_len = digits(rest, MAX_ID)?;
    let id = std::str::from_utf8(&rest[..id_len]).ok()?.to_owned();
    rest = &rest[id_len..];
    if let Some(port) = rest.strip_prefix(b":") {
        let len = digits(port, MAX_PART)?;
        rest = &port[len..];
    }
    // `tsz` writes CR LF; a terminal in cooked mode turns its LF into CR
    // LF again.
    let crs = rest.iter().take_while(|&&b| b == b'\r').count();
    if crs > 2 {
        return None;
    }
    rest = rest[crs..].strip_prefix(b"\n")?;
    let windows = id == "1" || (id.len() == 13 && id.ends_with("10"));
    if !windows && id.len() != 13 {
        return None;
    }
    let line = bytes[..at]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
    let head = &bytes[line..at];
    let tmux_control = head.starts_with(b"%output ") || head.starts_with(b"%extended-output ");
    Some(Trigger {
        at,
        end: bytes.len() - rest.len(),
        mode,
        version,
        id,
        windows,
        tmux_control,
    })
}

/// How many ASCII digits `bytes` starts with, 1 to `max`; `None` for
/// none or more.
fn digits(bytes: &[u8], max: usize) -> Option<usize> {
    let n = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    (1..=max).contains(&n).then_some(n)
}

/// The MD5 digest trzsz checks a file with: error detection, not
/// authentication. O(n).
#[must_use]
pub fn md5(bytes: &[u8]) -> [u8; 16] {
    ::md5::compute(bytes).0
}

/// A decimal count from the host: ASCII digits only, at most 20.
///
/// # Errors
/// Anything else.
pub fn number(payload: &[u8]) -> Result<u64, TrzszError> {
    if payload.is_empty() || payload.len() > 20 || !payload.iter().all(u8::is_ascii_digit) {
        return Err(TrzszError::Protocol("a number".into()));
    }
    std::str::from_utf8(payload)
        .ok()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| TrzszError::Protocol("a number".into()))
}

/// A message from the host, for the user, as other text from outside is
/// shown: control, zero-width and reordering characters out, at most 512
/// bytes.
#[must_use]
pub fn shown_message(text: &str) -> String {
    crate::history::shown_safely(&text.replace(['\r', '\n'], " "), 512)
}

#[cfg(test)]
mod tests;
