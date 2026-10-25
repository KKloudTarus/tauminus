// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Sending files to `rz` on a host, sans I/O (ADR 0026).

use zmodem2::{Action, Event, FileInfo, Position, Sender};

/// Bytes of a name that is sent; zmodem2 holds at most 255.
pub const MAX_SEND_NAME: usize = 255;
/// Host input kept while the sender waits for file data: replies are a
/// few headers; past this, the host is not speaking Zmodem.
const MAX_KEPT: usize = 4 << 10;
/// Subpackets sent per acknowledgement when the host takes a stream: 1 MiB
/// per round trip (128 held a 100 ms link to 0.9 MB/s; this gives 4.9).
/// SSH is reliable, the pump's backlog bounds what is queued, and no
/// line end leaves raw (see [`Escaper`]), so what is in flight when `rz`
/// dies runs nothing.
const STREAMING_WINDOW: usize = 1024;
/// Timeouts in a row before a send gives up: `rz` answers each window at
/// once, and the sender has nothing to ask again.
const MAX_TIMEOUTS: u32 = 3;
/// Bytes the host may print while the send does not move before it is
/// taken for a shell echoing what we send: `rz` says a few headers.
const MAX_HEARD: usize = 64 << 10;
/// Times the host may ask for a file's bytes again (ZRPOS back) before
/// the send stops: `rz` does on a damaged subpacket, not without end.
const MAX_REWINDS: u32 = 16;
/// Our own ZRQINIT, as a shell echoes it when `rz` is gone: its start,
/// raw or as a tty with ECHOCTL shows CAN.
const OWN_HEADER: &[u8] = b"**\x18B00";
const OWN_HEADER_SHOWN: &[u8] = b"**^XB00";
/// The host's ZSKIP, a hex header of type 05.
const ZSKIP_HEADER: &[u8] = b"**\x18B05";
const ZDLE: u8 = 0x18;
/// After our cancel, for a shell that reads what we sent: Ctrl+E then
/// Ctrl+U drop the line instead of keeping it for the next Enter. Ctrl+U
/// alone kills only what is before the cursor, and the cancel's CAN pairs
/// (`C-x C-x` in bash) put the cursor at the start.
const KILL_LINE: &[u8] = &[0x05, 0x15];

/// Escapes every control character `zmodem2` leaves raw (CR, LF, Ctrl+D
/// among them) as ZDLE sequences, as `sz -e` does. Every receiver decodes
/// them; and should `rz` die mid-file, what still reaches its shell holds
/// no line end, so no line of the file runs there. Hex headers, whose CR
/// and LF the protocol needs raw, pass as they are. O(n).
#[derive(Debug, Default)]
struct Escaper {
    state: EscapeState,
    /// The sender's last two bytes, to tell `**` ZDLE `B`, a hex header.
    last: [u8; 2],
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
enum EscapeState {
    #[default]
    Data,
    /// After the sender's ZDLE: its next byte is already escaped.
    Zdle,
    /// In a hex header, to its line end.
    Hex,
    /// After a hex header's line end: an XON may follow.
    HexEnd,
}

impl Escaper {
    fn push(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        out.reserve(bytes.len() + bytes.len() / 4);
        for &b in bytes {
            match self.state {
                EscapeState::Zdle => {
                    out.push(b);
                    self.state = if self.last == *b"**" && b == b'B' {
                        EscapeState::Hex
                    } else {
                        EscapeState::Data
                    };
                    self.last = [0, 0];
                    continue;
                }
                EscapeState::Hex => {
                    out.push(b);
                    if b == b'\n' || b == 0x8a {
                        self.state = EscapeState::HexEnd;
                    }
                    continue;
                }
                EscapeState::HexEnd => {
                    self.state = EscapeState::Data;
                    if b == 0x11 {
                        out.push(b);
                        continue;
                    }
                }
                EscapeState::Data => {}
            }
            if b == ZDLE {
                out.push(b);
                self.state = EscapeState::Zdle;
                continue;
            }
            self.last = [self.last[1], b];
            match b {
                0x7f => out.extend_from_slice(&[ZDLE, b'l']),
                0xff => out.extend_from_slice(&[ZDLE, b'm']),
                0x00..=0x1f | 0x80..=0x9f => out.extend_from_slice(&[ZDLE, b ^ 0x40]),
                _ => out.push(b),
            }
        }
    }
}

/// What a send needs from the caller next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// At most `max_len` bytes of the current file, from `offset`.
    Read { offset: u64, max_len: usize },
    /// The host's next bytes.
    Wait,
    /// Bytes for the host were written: poll again. A long line goes out
    /// a part at a time, so the caller's queue stays bounded.
    Write,
    /// The rest waits until the caller's queue is empty; then release it
    /// and poll again.
    Hold,
    /// The send ended.
    Ended,
}

/// A step of a send the caller acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sent {
    /// The host has the current file.
    FileDone,
    /// The host skipped it (ZSKIP): `rz` does for a name it has.
    FileSkipped,
    /// Every file went.
    Done,
    Aborted {
        reason: std::borrow::Cow<'static, str>,
    },
}

/// What a step produced: bytes for the host, and steps for the caller.
#[derive(Debug, Default)]
pub struct TransmitOutput {
    pub wire: Vec<u8>,
    pub sent: Vec<Sent>,
}

/// Why a file cannot be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransmitError {
    #[error("Zmodem sends files up to 4 GiB")]
    TooLarge,
    #[error("the name is longer than {MAX_SEND_NAME} bytes")]
    NameTooLong,
    #[error("the name is empty")]
    NoName,
    #[error("the send has ended")]
    Ended,
}

/// One send, from our ZRQINIT to the host's last ZRINIT.
#[expect(
    clippy::struct_excessive_bools,
    reason = "four separate facts of the protocol, not states of one"
)]
pub struct Transmit {
    sender: Sender,
    /// Host bytes the sender has not taken yet.
    kept: Vec<u8>,
    ended: bool,
    completed: bool,
    idle: u32,
    /// The host answered as `rz` does: it asked for data or took a file.
    answered: bool,
    /// The furthest byte of the current file asked for, and how often
    /// the host asked again for bytes before it.
    reached: u64,
    rewinds: u32,
    escaper: Escaper,
    /// Host bytes since the send last moved.
    heard: usize,
    /// The sender read a damaged header: it would answer with ZNAK, whose
    /// CR a shell takes for Enter.
    damaged: bool,
    /// The host skipped the current file: `zmodem2` reports it complete.
    skipping: bool,
}

impl std::fmt::Debug for Transmit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transmit")
            .field("kept", &self.kept.len())
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

impl Default for Transmit {
    fn default() -> Self {
        Self::new()
    }
}

impl Transmit {
    /// A send that starts by asking the host to receive (ZRQINIT): a host
    /// whose `rz` gave up meanwhile sees a line of `**`, never file data.
    ///
    /// # Panics
    ///
    /// Never: `zmodem2`'s buffers hold the handshake.
    pub fn new() -> Self {
        let mut sender = Sender::new().expect("the handshake fits");
        sender.set_streaming_window(STREAMING_WINDOW);
        Self {
            sender,
            kept: Vec::new(),
            ended: false,
            completed: false,
            idle: 0,
            answered: false,
            reached: 0,
            rewinds: 0,
            escaper: Escaper::default(),
            heard: 0,
            damaged: false,
            skipping: false,
        }
    }

    pub fn ended(&self) -> bool {
        self.ended
    }

    /// Whether every file went: the end was the protocol's.
    pub fn completed(&self) -> bool {
        self.completed
    }

    /// Whether the host answered as `rz` does at all.
    pub fn answered(&self) -> bool {
        self.answered
    }

    /// Offers the next file: its name as sent (one path component, which
    /// the caller chose) and size. Call before the first [`Self::poll`],
    /// then after each [`Sent::FileDone`].
    ///
    /// # Errors
    ///
    /// A file past 4 GiB, a name too long or empty, or a send that ended.
    pub fn start(&mut self, name: &str, size: u64) -> Result<(), TransmitError> {
        if self.ended {
            return Err(TransmitError::Ended);
        }
        if name.is_empty() {
            return Err(TransmitError::NoName);
        }
        if name.len() > MAX_SEND_NAME {
            return Err(TransmitError::NameTooLong);
        }
        let size = u32::try_from(size).map_err(|_| TransmitError::TooLarge)?;
        self.sender
            .start_file(FileInfo::new(name.as_bytes(), Some(Position::new(size))))
            .map_err(|_| TransmitError::Ended)
    }

    /// No more files: the host is told so once the last one went.
    pub fn finish(&mut self, out: &mut TransmitOutput) {
        if !self.ended {
            let _ = self.sender.finish();
            self.drain(out);
        }
    }

    /// Runs the protocol until it needs something. O(bytes produced).
    pub fn poll(&mut self, out: &mut TransmitOutput) -> Need {
        loop {
            if self.ended {
                return Need::Ended;
            }
            match self.sender.poll() {
                Action::WriteWire(bytes) => {
                    let n = bytes.len();
                    self.escaper.push(bytes, &mut out.wire);
                    self.sender.wire_written(n);
                }
                Action::ReadFile { offset, max_len } => {
                    self.answered = true;
                    self.idle = 0;
                    self.heard = 0;
                    let offset = u64::from(offset.get());
                    if offset < self.reached {
                        self.rewinds += 1;
                        if self.rewinds > MAX_REWINDS {
                            self.abort(out, "the host asked for the same bytes again and again");
                            continue;
                        }
                    }
                    self.reached = offset + max_len as u64;
                    return Need::Read { offset, max_len };
                }
                Action::Event(Event::FileCompleted) => {
                    self.answered = true;
                    self.idle = 0;
                    self.heard = 0;
                    self.reached = 0;
                    self.rewinds = 0;
                    out.sent.push(if std::mem::take(&mut self.skipping) {
                        Sent::FileSkipped
                    } else {
                        Sent::FileDone
                    });
                }
                Action::Event(Event::SessionCompleted) => {
                    self.drain(out);
                    self.ended = true;
                    self.completed = true;
                    out.sent.push(Sent::Done);
                }
                Action::Event(Event::Aborted) => {
                    self.drain(out);
                    out.wire.extend_from_slice(KILL_LINE);
                    self.end(out, "the host stopped the transfer");
                }
                // Steps of a receiver, or idle: wait for the host.
                _ => {
                    if self.kept.is_empty() {
                        return Need::Wait;
                    }
                    let mut kept = std::mem::take(&mut self.kept);
                    let used = self.submit(&kept);
                    kept.drain(..used);
                    self.kept = kept;
                    if self.stop_if_damaged(out) {
                        continue;
                    }
                    if used == 0 {
                        return Need::Wait;
                    }
                }
            }
        }
    }

    /// The file data [`Need::Read`] asked for: at most `max_len` bytes,
    /// none past the size given. A file that came back shorter than its
    /// size ends the send: it is never padded.
    pub fn provide(&mut self, data: &[u8], out: &mut TransmitOutput) {
        if self.ended {
            return;
        }
        if data.is_empty() || self.sender.submit_file(data).is_err() {
            self.abort(out, "the file changed while it was sent");
        }
    }

    /// Bytes the host sent. What the sender cannot take yet is kept for
    /// it, up to 4 KiB; once the send ends, [`Self::rest`] is the
    /// terminal's. O(n).
    pub fn feed(&mut self, input: &[u8], out: &mut TransmitOutput) {
        if self.ended {
            self.kept.extend_from_slice(input);
            return;
        }
        // The host's cancel: Zmodem escapes CAN in data, so five in a row
        // are it. Seen at once, even while the sender waits for file data
        // and takes no input.
        if input.windows(5).any(|w| w == [0x18; 5]) {
            self.sender.abort();
            self.drain_cancel(out);
            self.end(out, "the host stopped the transfer");
            return;
        }
        if input.windows(ZSKIP_HEADER.len()).any(|w| w == ZSKIP_HEADER) {
            self.skipping = true;
        }
        // `rz` that died without a cancel leaves a shell echoing what
        // still comes: far more than `rz` would say.
        self.heard += input.len();
        if self.answered && self.heard > MAX_HEARD {
            self.abort(out, "rz on the host stopped");
            return;
        }
        // `rz` turns echo off: our own header coming back means a shell
        // reads what we send. Nothing more goes.
        let echoed = |own: &[u8]| input.windows(own.len()).any(|w| w == own);
        if !self.answered && (echoed(OWN_HEADER) || echoed(OWN_HEADER_SHOWN)) {
            self.abort(out, "rz on the host is not waiting any more: run it again");
            return;
        }
        // After what is kept, in order.
        let used = if self.kept.is_empty() {
            self.submit(input)
        } else {
            0
        };
        self.kept.extend_from_slice(&input[used..]);
        if self.stop_if_damaged(out) {
            return;
        }
        if self.kept.len() > MAX_KEPT {
            self.abort(out, "the host does not answer as rz does");
        }
    }

    /// Once the send ended, what the host sent after it: the terminal's.
    pub fn rest(&mut self) -> Vec<u8> {
        if self.ended {
            std::mem::take(&mut self.kept)
        } else {
            Vec::new()
        }
    }

    /// Hands host bytes to the sender while it takes them: it stops at a
    /// header it must answer first, and [`Self::poll`] gives it the rest.
    fn submit(&mut self, input: &[u8]) -> usize {
        let mut used = 0;
        while used < input.len() && !self.ended {
            match self.sender.submit_wire(&input[used..]) {
                Ok(0) => break,
                Ok(n) => used += n,
                // A damaged header: the sender would ask again (ZNAK).
                Err(_) => {
                    self.damaged = true;
                    used = input.len();
                }
            }
        }
        used
    }

    /// Once files go, a damaged header ends the send: its ZNAK, a hex
    /// header with a raw line end, must not reach a shell that took over
    /// from `rz`. Before, it asks again, as `rz` may still be starting.
    fn stop_if_damaged(&mut self, out: &mut TransmitOutput) -> bool {
        if std::mem::take(&mut self.damaged) && self.answered {
            self.abort(out, "the host's replies were damaged");
            return true;
        }
        false
    }

    /// Nothing moved for a while: no data asked for, no file taken. Host
    /// output alone (a shell's clock, say) does not count.
    pub fn timeout(&mut self, out: &mut TransmitOutput) {
        if self.ended {
            return;
        }
        // `rz` answers at once: one that has not, gave up before the
        // user chose.
        if !self.answered {
            self.abort(out, "rz on the host is not waiting any more: run it again");
            return;
        }
        self.idle += 1;
        if self.idle > MAX_TIMEOUTS || self.sender.timeout().is_err() {
            self.abort(out, "the host stopped answering");
        }
    }

    /// Stops the send, telling the host to stop too.
    pub fn abort(&mut self, out: &mut TransmitOutput, reason: &'static str) {
        if !self.ended {
            self.sender.abort();
            self.drain_cancel(out);
            self.end(out, reason);
        }
    }

    /// The bytes still for the host, and nothing else.
    fn drain(&mut self, out: &mut TransmitOutput) {
        while let Action::WriteWire(bytes) = self.sender.poll() {
            let n = bytes.len();
            self.escaper.push(bytes, &mut out.wire);
            self.sender.wire_written(n);
        }
    }

    /// The cancel sequence as it is (ten CAN, ten backspaces), then
    /// Ctrl+U for a shell that may be reading.
    fn drain_cancel(&mut self, out: &mut TransmitOutput) {
        while let Action::WriteWire(bytes) = self.sender.poll() {
            let n = bytes.len();
            out.wire.extend_from_slice(bytes);
            self.sender.wire_written(n);
        }
        out.wire.extend_from_slice(KILL_LINE);
    }

    fn end(&mut self, out: &mut TransmitOutput, reason: &'static str) {
        if !self.ended {
            self.ended = true;
            out.sent.push(Sent::Aborted {
                reason: reason.into(),
            });
        }
    }
}

#[cfg(test)]
mod tests;
