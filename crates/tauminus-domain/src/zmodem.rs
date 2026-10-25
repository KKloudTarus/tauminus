// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Zmodem file transfers in a terminal, sans I/O: files a host sends with
//! `sz` (ADR 0025), and files sent to `rz` on the host ([`Transmit`],
//! ADR 0026).
//!
//! `zmodem2` runs the protocol; this module adds what a terminal needs
//! around it: finding the moment the host asks to start, names made safe
//! to save or to send, and sizes held to.

use std::sync::LazyLock;

use memchr::memmem::Finder;
use zmodem2::{Action, Event, Receiver};

use crate::sftp::{LocalOs, local_name};

/// How a host starts a transfer: a hex header, its type's first digit 0.
const OFFER: &[u8] = b"**\x18B0";
/// Bytes after [`OFFER`] in the header: the type's second digit, then
/// flags and CRC.
const OFFER_TAIL: usize = 13;

/// What a host's offer asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wants {
    /// ZRQINIT: `sz` on the host sends files.
    Receive,
    /// ZRINIT: `rz` on the host waits for files.
    Send,
}

/// An offer found in output: where it starts, what it asks, and which
/// protocol makes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offer {
    pub at: usize,
    pub wants: Wants,
    pub protocol: TransferProtocol,
}

/// The protocol of an offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferProtocol {
    Zmodem,
    /// trzsz (ADR 0031): its trigger line ends at `end`.
    Trzsz {
        id: u64,
        end: usize,
        /// What the host asks that is not done: the offer is shown, not
        /// answered.
        refused: Option<Refused>,
    },
}

/// A trzsz offer that is not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// `trz -d`: folders.
    Folders,
    /// A Windows host's line ends.
    Windows,
}

/// The first offer in `bytes`, whole: a Zmodem ZRQINIT or ZRINIT header
/// with its flags and CRC, or a trzsz trigger with its line end. One cut
/// at the end of `bytes` is not found; [`OFFER_KEEP`] bytes kept for the
/// next read find it then. O(n): two SIMD searches.
pub fn find_offer(bytes: &[u8]) -> Option<Offer> {
    let zmodem = find_zmodem(bytes);
    // A trzsz trigger past a Zmodem header is not looked for: the first
    // one wins.
    let upto = zmodem.map_or(bytes.len(), |offer| offer.at);
    let trzsz = crate::trzsz::find_trigger(&bytes[..upto]).map(|trigger| Offer {
        at: trigger.at,
        wants: match trigger.mode {
            crate::trzsz::Mode::Send => Wants::Receive,
            crate::trzsz::Mode::Receive | crate::trzsz::Mode::Folders => Wants::Send,
        },
        protocol: TransferProtocol::Trzsz {
            id: trigger.id.parse().unwrap_or(0),
            end: trigger.end,
            refused: if trigger.windows {
                Some(Refused::Windows)
            } else if trigger.mode == crate::trzsz::Mode::Folders {
                Some(Refused::Folders)
            } else {
                None
            },
        },
    });
    trzsz.or(zmodem)
}

fn find_zmodem(bytes: &[u8]) -> Option<Offer> {
    // SIMD search, about 25 GB/s: every output chunk of a session that can
    // transfer passes here.
    static FINDER: LazyLock<Finder<'static>> = LazyLock::new(|| Finder::new(OFFER));
    FINDER.find_iter(bytes).find_map(|at| {
        let tail = bytes.get(at + OFFER.len()..at + OFFER.len() + OFFER_TAIL)?;
        let wants = match tail[0] {
            b'0' => Wants::Receive,
            b'1' => Wants::Send,
            _ => return None,
        };
        tail[1..]
            .iter()
            .all(u8::is_ascii_hexdigit)
            .then_some(Offer {
                at,
                wants,
                protocol: TransferProtocol::Zmodem,
            })
    })
}

/// Bytes at the end of a read that may begin an offer [`find_offer`]
/// would find once the rest comes: the longer of a Zmodem header and a
/// trzsz trigger.
pub const OFFER_KEEP: usize = if crate::trzsz::TRIGGER_KEEP > OFFER.len() + OFFER_TAIL {
    crate::trzsz::TRIGGER_KEEP - 1
} else {
    OFFER.len() + OFFER_TAIL - 1
};

/// A name a host sent, made safe to save: its last path component, a
/// leading `.` made `_`, then [`local_name`], and no character that reads
/// as nothing. `Err` with the name as it may be shown.
pub fn incoming_name(raw: &str, os: LocalOs) -> Result<String, String> {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let last = last
        .strip_prefix('.')
        .map_or(last.to_owned(), |rest| format!("_{rest}"));
    // Characters that read as nothing could hide what a name ends with.
    local_name(&last, os)
        .ok()
        .filter(|name| !name.chars().any(crate::history::invisible))
        .map(str::to_owned)
        .ok_or_else(|| crate::history::shown_safely(raw, MAX_SHOWN_NAME))
}

/// A step of a receive the caller acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    /// A file starts: its name, made safe to save, and the size the host
    /// declared.
    File {
        name: String,
        size: Option<u64>,
    },
    Data(Vec<u8>),
    /// The current file is complete.
    FileDone,
    /// The host sent a file whose name cannot be saved; it is skipped. The
    /// name as it may be shown: no control or reordering characters.
    Skipped {
        name: String,
    },
    /// Every file came.
    Done,
    /// The transfer ended before it was done.
    Aborted {
        reason: &'static str,
    },
}

/// One receive, from the host's offer to its end.
pub struct Receive {
    receiver: Receiver,
    os: LocalOs,
    /// The current file's declared size, and what came of it.
    expected: Option<u64>,
    got: u64,
    ended: bool,
    /// Every file came: the end was the protocol's, not an abort.
    completed: bool,
    /// Timeouts in a row with nothing from the host.
    idle: u32,
    /// After the end: the protocol's last bytes (line ends, and up to two
    /// `O`s) may still come, and are not the terminal's.
    tail: bool,
    oos: u8,
}

/// Bytes of a skipped file's name that are shown.
const MAX_SHOWN_NAME: usize = 255;

/// Timeouts in a row before a receive gives up.
const MAX_TIMEOUTS: u32 = 10;

/// What a step produced: bytes for the host, and steps for the caller.
#[derive(Debug, Default)]
pub struct Output {
    pub wire: Vec<u8>,
    pub received: Vec<Received>,
}

impl std::fmt::Debug for Receive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Receive")
            .field("expected", &self.expected)
            .field("got", &self.got)
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

impl Receive {
    /// # Panics
    ///
    /// Never: `zmodem2`'s default buffer is within its own limits.
    pub fn new(os: LocalOs) -> Self {
        let mut receiver = Receiver::new().expect("the default buffer fits");
        // Each file is looked at before it is taken.
        receiver.set_manual_file_accept(true);
        Self {
            receiver,
            os,
            expected: None,
            got: 0,
            ended: false,
            completed: false,
            idle: 0,
            tail: true,
            oos: 2,
        }
    }

    /// Whether the receive ended, done or not.
    pub fn ended(&self) -> bool {
        self.ended
    }

    /// Whether it ended with every file: after an abort, what the host
    /// had sent already still comes.
    pub fn completed(&self) -> bool {
        self.completed
    }

    /// Feeds bytes the host sent. Returns how many were the transfer's:
    /// once it ends, the rest is the terminal's again. O(n).
    pub fn feed(&mut self, input: &[u8], out: &mut Output) -> usize {
        if !input.is_empty() {
            self.idle = 0;
        }
        let mut used = 0;
        while !self.ended {
            if self.step(out) {
                continue;
            }
            if used == input.len() {
                break;
            }
            match self.receiver.submit_wire(&input[used..]) {
                Ok(0) => break,
                Ok(n) => used += n,
                // The host is told to stop: it would retry for minutes.
                Err(_) => self.abort(out, "the host's data was damaged or too large"),
            }
        }
        used
    }

    /// Nothing came for a while: the protocol asks again, and gives up
    /// after its retries.
    pub fn timeout(&mut self, out: &mut Output) {
        self.idle += 1;
        if self.idle > MAX_TIMEOUTS || self.receiver.timeout().is_err() {
            self.abort(out, "the host stopped answering");
            return;
        }
        while self.step(out) {}
    }

    /// Stops the receive, telling the host to stop too.
    pub fn abort(&mut self, out: &mut Output, reason: &'static str) {
        if !self.ended {
            let _ = self.receiver.abort();
            self.drain(out);
            self.end(out, reason);
        }
    }

    /// What the host printed after the receive ended, without the
    /// protocol's last bytes: line ends, XON, and `sz`'s closing `OO`.
    /// O(n).
    pub fn after<'a>(&mut self, bytes: &'a [u8]) -> &'a [u8] {
        if !self.tail {
            return bytes;
        }
        let mut skip = 0;
        for &b in bytes {
            match b {
                b'\r' | b'\n' | 0x8a | 0x11 => {}
                b'O' if self.oos > 0 => self.oos -= 1,
                _ => break,
            }
            skip += 1;
        }
        if skip < bytes.len() {
            self.tail = false;
        }
        &bytes[skip..]
    }

    /// The bytes still for the host, and nothing else: past the end, the
    /// protocol's own reports would come back for ever.
    fn drain(&mut self, out: &mut Output) {
        while let Action::WriteWire(bytes) = self.receiver.poll() {
            let n = bytes.len();
            out.wire.extend_from_slice(bytes);
            self.receiver.wire_written(n);
        }
    }

    fn end(&mut self, out: &mut Output, reason: &'static str) {
        if !self.ended {
            self.ended = true;
            out.received.push(Received::Aborted { reason });
        }
    }

    /// One action of the protocol; `false` when it waits for input or the
    /// receive ended.
    fn step(&mut self, out: &mut Output) -> bool {
        if self.ended {
            return false;
        }
        match self.receiver.poll() {
            Action::WriteWire(bytes) => {
                let n = bytes.len();
                out.wire.extend_from_slice(bytes);
                self.receiver.wire_written(n);
            }
            Action::WriteFile(bytes) => {
                let n = bytes.len();
                self.got += n as u64;
                if self.expected.is_some_and(|size| self.got > size) {
                    self.abort(out, "the host sent more than the size it gave");
                    return true;
                }
                out.received.push(Received::Data(bytes.to_vec()));
                let _ = self.receiver.file_written(n);
            }
            Action::Event(Event::FileStarted(info)) => {
                let raw = String::from_utf8_lossy(info.name).into_owned();
                match incoming_name(&raw, self.os) {
                    Ok(name) => {
                        // An empty size field reads as 0: no size given.
                        self.expected = info
                            .size
                            .map(|p| u64::from(p.get()))
                            .filter(|&size| size > 0);
                        self.got = 0;
                        out.received.push(Received::File {
                            name,
                            size: self.expected,
                        });
                        let _ = self.receiver.accept_file_at(0);
                    }
                    // Shown to the user: what hides or reorders text out.
                    Err(name) => {
                        out.received.push(Received::Skipped { name });
                        let _ = self.receiver.skip_file();
                    }
                }
            }
            Action::Event(Event::FileCompleted) => out.received.push(Received::FileDone),
            Action::Event(Event::SessionCompleted) => {
                // The reply to the host's ZFIN may come after the event.
                self.drain(out);
                self.ended = true;
                self.completed = true;
                out.received.push(Received::Done);
            }
            Action::Event(Event::Aborted) => self.end(out, "the host stopped the transfer"),
            // A sender's step: not one a receiver takes.
            Action::ReadFile { .. } | Action::Event(_) => {}
            // Idle: it waits for the host.
            _ => return false,
        }
        true
    }
}

mod send;

pub use send::{MAX_SEND_NAME, Need, Sent, Transmit, TransmitError, TransmitOutput};

#[cfg(test)]
mod tests;
