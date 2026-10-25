// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Files sent to a host's `trz`, protocol 1 (ADR 0031): after `#ACT` and
//! `#CFG`, the terminal sends `#NUM`, then per file `#NAME`, `#SIZE`,
//! `#DATA` lines and `#MD5`, each answered with `#SUCC`, and ends with
//! `#EXIT`. Lines are base64: if `trz` is gone, a shell reads each as one
//! word, a comment in bash and fish.

use std::borrow::Cow;
use std::time::{Duration, Instant};

use super::{
    Config, Lines, MAX_LINE, TrzszError, action, decode, encode, frame, number, shown_message,
};
use crate::zmodem::{Need, Sent, TransmitError, TransmitOutput};

/// Bytes of a file read per line at first, and at most.
const FIRST_CHUNK: usize = 64 << 10;
const MAX_CHUNK: usize = 1 << 20;
/// A chunk acknowledged sooner than this doubles.
const QUICK: Duration = Duration::from_millis(500);
/// Bytes of a line written per poll: the caller's queue stays bounded.
const PART: usize = 1 << 10;
/// How long the host's configuration may take.
const FIRST: Duration = Duration::from_secs(20);
/// Longest name sent: one path component.
pub const MAX_UPLOAD_NAME: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Config,
    Count,
    /// Waits for the caller's next file.
    Idle,
    Name,
    Size,
    /// Asks the caller for the file's next bytes.
    Read,
    /// A data line went; its acknowledgement is awaited.
    Data,
    Digest,
    Ended,
}

/// One `trz` transfer from the terminal.
pub struct Upload {
    files: u64,
    lines: Lines,
    step: Step,
    config: Option<Config>,
    /// The line being written, and how much of it went.
    outbox: Vec<u8>,
    written: usize,
    size: u64,
    sent: u64,
    /// The bytes of the last data line, for its acknowledgement.
    chunk_len: usize,
    chunk: usize,
    chunk_at: Instant,
    md5: md5::Context,
    /// The digest sent for the current file, to compare with the host's.
    digest: [u8; 16],
    names: Vec<String>,
    done: u64,
    answered: bool,
    completed: bool,
}

impl std::fmt::Debug for Upload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upload")
            .field("step", &self.step)
            .field("sent", &self.sent)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl Upload {
    /// The user chose `files` files: the answer goes to the host.
    #[must_use]
    pub fn start(files: u64, out: &mut TransmitOutput) -> Self {
        out.wire
            .extend_from_slice(&frame("ACT", &encode(action(true).as_bytes())));
        Self {
            files,
            lines: Lines::new(),
            step: Step::Config,
            config: None,
            outbox: Vec::new(),
            written: 0,
            size: 0,
            sent: 0,
            chunk_len: 0,
            chunk: FIRST_CHUNK,
            chunk_at: Instant::now(),
            md5: md5::Context::new(),
            digest: [0; 16],
            names: Vec::new(),
            done: 0,
            answered: false,
            completed: false,
        }
    }

    #[must_use]
    pub fn ended(&self) -> bool {
        self.step == Step::Ended
    }

    /// Every file went: the end was the protocol's.
    #[must_use]
    pub fn completed(&self) -> bool {
        self.completed
    }

    /// Whether the host answered as `trz` does: its configuration came.
    #[must_use]
    pub fn answered(&self) -> bool {
        self.answered
    }

    /// How long the host may take to answer one line.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.config.as_ref().map_or(FIRST, |config| config.timeout)
    }

    /// Offers the next file: its name, one path component, and size. Call
    /// at the start and after each [`Sent::FileDone`].
    ///
    /// # Errors
    /// A name empty or too long, or a send that ended or has its files.
    pub fn next_file(&mut self, name: &str, size: u64) -> Result<(), TransmitError> {
        if self.ended() || self.names.len() as u64 >= self.files {
            return Err(TransmitError::Ended);
        }
        if name.is_empty() {
            return Err(TransmitError::NoName);
        }
        if name.len() > MAX_UPLOAD_NAME {
            return Err(TransmitError::NameTooLong);
        }
        self.names.push(name.to_owned());
        self.size = size;
        if self.step == Step::Idle {
            self.send_name();
        }
        Ok(())
    }

    /// No more files: once the last went, the host hears the end.
    pub fn finish(&mut self, out: &mut TransmitOutput) {
        if self.step == Step::Idle && self.done == self.files {
            self.end_well(out);
        }
    }

    /// Runs until it needs something: the host's next line, or the file's
    /// bytes. Writes at most [`PART`] bytes of a line per call. O(bytes).
    pub fn poll(&mut self, out: &mut TransmitOutput) -> Need {
        if self.written < self.outbox.len() {
            let end = (self.written + PART).min(self.outbox.len());
            out.wire.extend_from_slice(&self.outbox[self.written..end]);
            self.written = end;
            if self.written == self.outbox.len() {
                self.outbox.clear();
                self.written = 0;
            }
            return Need::Write;
        }
        match self.step {
            Step::Ended => Need::Ended,
            Step::Read => {
                let left = self.size - self.sent;
                let max_len = usize::try_from(left).map_or(self.chunk, |left| left.min(self.chunk));
                Need::Read {
                    offset: self.sent,
                    max_len,
                }
            }
            _ => Need::Wait,
        }
    }

    /// The bytes asked for by the last [`Need::Read`].
    pub fn provide(&mut self, data: &[u8], out: &mut TransmitOutput) {
        if self.step != Step::Read {
            return;
        }
        if data.is_empty() {
            self.abort(out, "a file got shorter while it went");
            return;
        }
        self.md5.consume(data);
        self.sent += data.len() as u64;
        self.chunk_len = data.len();
        self.chunk_at = Instant::now();
        self.queue(&frame("DATA", &encode(data)));
        self.step = Step::Data;
    }

    /// Feeds bytes the host sent. O(n).
    pub fn feed(&mut self, input: &[u8], out: &mut TransmitOutput) {
        if self.ended() {
            return;
        }
        self.lines.push(input);
        while !matches!(self.step, Step::Ended | Step::Idle | Step::Read) {
            let expect = match self.step {
                Step::Config => "CFG",
                _ => "SUCC",
            };
            match self.lines.next(expect, MAX_LINE) {
                Ok(None) => return,
                Ok(Some(line)) if line.kind == expect => {
                    if let Err(error) = self.take(&line.payload, out) {
                        self.fail(out, error);
                    }
                }
                Ok(Some(line)) if matches!(line.kind.as_str(), "fail" | "FAIL" | "EXIT") => {
                    let text = decode(&line.payload, MAX_LINE)
                        .map(|bytes| shown_message(&String::from_utf8_lossy(&bytes)))
                        .unwrap_or_default();
                    // The host is leaving: nothing more is written to it.
                    self.end(out, format!("the host stopped the transfer: {text}").into());
                }
                Ok(Some(line)) => self.fail(
                    out,
                    TrzszError::Protocol(format!("#{} where it waited for another", line.kind)),
                ),
                Err(error) => self.fail(out, error),
            }
        }
    }

    /// What the host printed after the transfer ended.
    pub fn rest(&mut self) -> Vec<u8> {
        self.lines.take_rest()
    }

    /// Nothing came for [`Self::timeout`].
    pub fn timed_out(&mut self, out: &mut TransmitOutput) {
        if !matches!(self.step, Step::Ended | Step::Idle | Step::Read) {
            self.stop(out, "the host stopped answering".into());
        }
    }

    /// Stops the transfer.
    pub fn abort(&mut self, out: &mut TransmitOutput, reason: &'static str) {
        self.stop(out, reason.into());
    }

    fn fail(&mut self, out: &mut TransmitOutput, error: TrzszError) {
        let reason = match error {
            TrzszError::Interrupted => "the host interrupted the transfer".to_owned(),
            other => other.to_string(),
        };
        self.stop(out, reason.into());
    }

    /// What was not written goes, and Ctrl+C reaches `trz` as its
    /// interrupt; Ctrl+E and Ctrl+U then drop a line a shell read, should
    /// `trz` be gone (ADR 0026).
    fn stop(&mut self, out: &mut TransmitOutput, reason: Cow<'static, str>) {
        if self.ended() {
            return;
        }
        self.outbox.clear();
        self.written = 0;
        out.wire.extend_from_slice(b"\x03\x05\x15");
        self.end(out, reason);
    }

    fn end(&mut self, out: &mut TransmitOutput, reason: Cow<'static, str>) {
        self.step = Step::Ended;
        out.sent.push(Sent::Aborted { reason });
    }

    fn queue(&mut self, line: &[u8]) {
        self.outbox.extend_from_slice(line);
    }

    fn send_name(&mut self) {
        let name = self.names.last().cloned().unwrap_or_default();
        self.queue(&frame("NAME", &encode(name.as_bytes())));
        self.step = Step::Name;
    }

    /// A `#SUCC` for the current step.
    fn take(&mut self, payload: &[u8], out: &mut TransmitOutput) -> Result<(), TrzszError> {
        match self.step {
            Step::Config => {
                let json = decode(payload, MAX_LINE)?;
                let config = Config::read(&json)?;
                self.lines.set_junk(config.junk);
                let most = usize::try_from(config.bufsize).unwrap_or(MAX_CHUNK);
                self.chunk = FIRST_CHUNK.min(most);
                self.config = Some(config);
                self.answered = true;
                self.queue(&frame("NUM", &self.files.to_string()));
                self.step = Step::Count;
            }
            Step::Count => {
                if number(payload)? != self.files {
                    return Err(TrzszError::Protocol("another number of files".into()));
                }
                self.step = Step::Idle;
                if self.names.len() as u64 > self.done {
                    self.send_name();
                }
            }
            Step::Name => {
                // The name the host saved under: `trz` lists it itself.
                decode(payload, MAX_LINE)?;
                self.queue(&frame("SIZE", &self.size.to_string()));
                self.step = Step::Size;
            }
            Step::Size => {
                if number(payload)? != self.size {
                    return Err(TrzszError::Protocol("another size".into()));
                }
                self.sent = 0;
                self.md5 = md5::Context::new();
                if self.size == 0 {
                    self.send_digest();
                } else {
                    self.step = Step::Read;
                }
            }
            Step::Data => {
                if number(payload)? != self.chunk_len as u64 {
                    return Err(TrzszError::Protocol("another chunk size".into()));
                }
                self.grow();
                if self.sent == self.size {
                    self.send_digest();
                } else {
                    self.step = Step::Read;
                }
            }
            Step::Digest => {
                let theirs = decode(payload, 16)?;
                if theirs != self.digest {
                    return Err(TrzszError::Protocol(
                        "a file that went damaged (MD5)".into(),
                    ));
                }
                self.done += 1;
                self.step = Step::Idle;
                out.sent.push(Sent::FileDone);
                if self.names.len() as u64 > self.done {
                    self.send_name();
                }
            }
            Step::Idle | Step::Read | Step::Ended => {}
        }
        Ok(())
    }

    fn send_digest(&mut self) {
        self.digest = std::mem::take(&mut self.md5).finalize().0;
        self.queue(&frame("MD5", &encode(&self.digest)));
        self.step = Step::Digest;
    }

    /// A chunk taken soon doubles the next, up to the host's chunk size
    /// and 1 MiB.
    fn grow(&mut self) {
        let most = self
            .config
            .as_ref()
            .and_then(|config| usize::try_from(config.bufsize).ok())
            .unwrap_or(MAX_CHUNK)
            .min(MAX_CHUNK);
        if self.chunk_len == self.chunk && self.chunk_at.elapsed() < QUICK {
            self.chunk = (self.chunk * 2).min(most);
        }
    }

    fn end_well(&mut self, out: &mut TransmitOutput) {
        let mut text = format!("Sent {} file(s)", self.names.len());
        for name in &self.names {
            text.push_str("\r\n- ");
            text.push_str(name);
        }
        // Every line before it was answered: nothing is queued.
        out.wire
            .extend_from_slice(&frame("EXIT", &encode(text.as_bytes())));
        self.step = Step::Ended;
        self.completed = true;
        out.sent.push(Sent::Done);
    }
}
