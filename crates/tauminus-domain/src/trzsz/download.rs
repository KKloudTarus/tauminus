// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Files a host sends with `tsz`, protocol 1 (ADR 0031): after `#ACT`
//! and `#CFG`, the host sends `#NUM`, then per file `#NAME`, `#SIZE`,
//! `#DATA` lines and `#MD5`, each answered with `#SUCC`; the terminal
//! ends with `#EXIT`.

use std::borrow::Cow;
use std::time::Duration;

use super::{
    Config, Line, Lines, MAX_DATA_LINE, MAX_LINE, TrzszError, action, decode, encode, frame,
    number, shown_message,
};
use crate::sftp::LocalOs;
use crate::zmodem::{Output, Received, incoming_name};

/// Most files one transfer brings.
pub const MAX_FILES: u64 = 100;
/// How long the host's configuration may take.
const FIRST: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Config,
    Count,
    Name,
    /// The caller is saving the file under a name: see
    /// [`Download::named`].
    Saving,
    Size,
    Data,
    Digest,
    Ended,
}

/// One `tsz` transfer into the terminal.
pub struct Download {
    os: LocalOs,
    lines: Lines,
    step: Step,
    config: Option<Config>,
    files: u64,
    done: u64,
    size: u64,
    got: u64,
    md5: md5::Context,
    /// The names the host sent, made safe: what it hears back. The names
    /// files were saved under stay here, as they tell what the folder
    /// held.
    names: Vec<String>,
    completed: bool,
}

impl std::fmt::Debug for Download {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Download")
            .field("step", &self.step)
            .field("size", &self.size)
            .field("got", &self.got)
            .finish_non_exhaustive()
    }
}

impl Download {
    /// The user accepted: the answer goes to the host.
    #[must_use]
    pub fn start(os: LocalOs, out: &mut Output) -> Self {
        out.wire
            .extend_from_slice(&frame("ACT", &encode(action(true).as_bytes())));
        Self {
            os,
            lines: Lines::new(),
            step: Step::Config,
            config: None,
            files: 0,
            done: 0,
            size: 0,
            got: 0,
            md5: md5::Context::new(),
            names: Vec::new(),
            completed: false,
        }
    }

    #[must_use]
    pub fn ended(&self) -> bool {
        self.step == Step::Ended
    }

    /// Every file came: the end was the protocol's.
    #[must_use]
    pub fn completed(&self) -> bool {
        self.completed
    }

    /// How long one line may take before the transfer gives up.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.config.as_ref().map_or(FIRST, |config| config.timeout)
    }

    /// Feeds bytes the host sent. Returns how many were the transfer's:
    /// once it ends, the rest is the terminal's again. O(n).
    pub fn feed(&mut self, input: &[u8], out: &mut Output) -> usize {
        if self.ended() {
            return 0;
        }
        self.lines.push(input);
        self.run(out);
        if self.ended() {
            let rest = self.lines.pending();
            return input.len().saturating_sub(rest);
        }
        input.len()
    }

    /// The caller saved the file the last [`Received::File`] named: the
    /// transfer goes on. The host hears the name it sent, made safe, not
    /// the one saved, which would tell it what names the folder holds.
    pub fn named(&mut self, out: &mut Output) {
        if self.step != Step::Saving {
            return;
        }
        let name = self.names.last().cloned().unwrap_or_default();
        out.wire
            .extend_from_slice(&frame("SUCC", &encode(name.as_bytes())));
        self.step = Step::Size;
        self.run(out);
    }

    /// Nothing came for [`Self::timeout`]. Before the host's
    /// configuration, it may have been no `tsz` at all: nothing more is
    /// written to it.
    pub fn timed_out(&mut self, out: &mut Output) {
        if self.config.is_none() {
            if !self.ended() {
                self.end(out, "the host never answered as tsz does".into());
            }
            return;
        }
        self.abort(out, "the host stopped answering");
    }

    /// Stops the transfer: Ctrl+C reaches `tsz` as its interrupt.
    pub fn abort(&mut self, out: &mut Output, reason: &'static str) {
        if self.ended() {
            return;
        }
        out.wire.push(0x03);
        self.end(out, reason.into());
    }

    fn end(&mut self, out: &mut Output, reason: Cow<'static, str>) {
        self.step = Step::Ended;
        out.received.push(Received::Aborted { reason });
    }

    fn run(&mut self, out: &mut Output) {
        while !matches!(self.step, Step::Ended | Step::Saving) {
            let (expect, max) = match self.step {
                Step::Config => ("CFG", MAX_LINE),
                Step::Count => ("NUM", MAX_LINE),
                Step::Name => ("NAME", MAX_LINE),
                Step::Size => ("SIZE", MAX_LINE),
                Step::Data => ("DATA", MAX_DATA_LINE),
                Step::Digest => ("MD5", MAX_LINE),
                Step::Saving | Step::Ended => return,
            };
            match self.lines.next(expect, max) {
                Ok(None) => return,
                Ok(Some(line)) if line.kind == expect => {
                    if let Err(error) = self.take(&line, out) {
                        self.fail(out, error);
                    }
                }
                Ok(Some(line)) => self.unexpected(&line, out),
                Err(error) => self.fail(out, error),
            }
        }
    }

    /// A line of another type than the step's: the host's own end, or a
    /// fault.
    fn unexpected(&mut self, line: &Line, out: &mut Output) {
        if matches!(line.kind.as_str(), "fail" | "FAIL" | "EXIT") {
            let text = decode(&line.payload, MAX_LINE)
                .map(|bytes| shown_message(&String::from_utf8_lossy(&bytes)))
                .unwrap_or_default();
            // The host is leaving: nothing more is written to it.
            self.end(out, format!("the host stopped the transfer: {text}").into());
            return;
        }
        self.fail(
            out,
            TrzszError::Protocol(format!("#{} where it waited for another", line.kind)),
        );
    }

    fn fail(&mut self, out: &mut Output, error: TrzszError) {
        if self.ended() {
            return;
        }
        // The host is told, and stops, as at Ctrl+C.
        out.wire.push(0x03);
        let reason = match error {
            TrzszError::Interrupted => "the host interrupted the transfer".to_owned(),
            other => other.to_string(),
        };
        self.end(out, reason.into());
    }

    fn take(&mut self, line: &Line, out: &mut Output) -> Result<(), TrzszError> {
        match self.step {
            Step::Config => {
                let json = decode(&line.payload, MAX_LINE)?;
                let config = Config::read(&json)?;
                self.lines.set_junk(config.junk);
                self.config = Some(config);
                self.step = Step::Count;
            }
            Step::Count => {
                self.files = number(&line.payload)?;
                if self.files > MAX_FILES {
                    return Err(TrzszError::Unsupported(format!(
                        "{} files at once; at most {MAX_FILES}",
                        self.files
                    )));
                }
                out.wire
                    .extend_from_slice(&frame("SUCC", &self.files.to_string()));
                self.next_file(out);
            }
            Step::Name => {
                let raw = decode(&line.payload, MAX_LINE)?;
                let raw = String::from_utf8_lossy(&raw);
                let name = incoming_name(&raw, self.os).map_err(|shown| {
                    TrzszError::Protocol(format!("a name that cannot be saved: {shown}"))
                })?;
                self.names.push(name.clone());
                out.received.push(Received::File { name, size: None });
                self.step = Step::Saving;
            }
            Step::Size => {
                self.size = number(&line.payload)?;
                self.got = 0;
                self.md5 = md5::Context::new();
                out.wire
                    .extend_from_slice(&frame("SUCC", &self.size.to_string()));
                self.step = if self.size == 0 {
                    Step::Digest
                } else {
                    Step::Data
                };
            }
            Step::Data => {
                let chunk = self.config.as_ref().map_or(0, |config| config.bufsize);
                let left = self.size - self.got;
                let max = usize::try_from(left.min(chunk)).unwrap_or(usize::MAX);
                let bytes = decode(&line.payload, max).map_err(|error| match error {
                    TrzszError::TooLong(_) => {
                        TrzszError::Protocol("more than the size it gave".into())
                    }
                    other => other,
                })?;
                self.got += bytes.len() as u64;
                self.md5.consume(&bytes);
                out.wire
                    .extend_from_slice(&frame("SUCC", &bytes.len().to_string()));
                out.received.push(Received::Data(bytes));
                if self.got == self.size {
                    self.step = Step::Digest;
                }
            }
            Step::Digest => {
                let theirs = decode(&line.payload, 16)?;
                let ours = std::mem::take(&mut self.md5).finalize();
                if theirs != ours.0 {
                    return Err(TrzszError::Protocol(
                        "a file that came damaged (MD5)".into(),
                    ));
                }
                out.wire.extend_from_slice(&frame("SUCC", &encode(&ours.0)));
                out.received.push(Received::FileDone);
                self.done += 1;
                self.next_file(out);
            }
            Step::Saving | Step::Ended => {}
        }
        Ok(())
    }

    /// The next file, or the end: the host is told what was saved, by
    /// name only.
    fn next_file(&mut self, out: &mut Output) {
        if self.done < self.files {
            self.step = Step::Name;
            return;
        }
        let mut text = format!("Saved {} file(s)", self.names.len());
        for name in &self.names {
            text.push_str("\r\n- ");
            text.push_str(name);
        }
        out.wire
            .extend_from_slice(&frame("EXIT", &encode(text.as_bytes())));
        self.step = Step::Ended;
        self.completed = true;
        out.received.push(Received::Done);
    }
}

/// Where the host's closing message starts in `bytes`, if it does: after
/// a stop, output is dropped until it. `tsz` and `trz` restore the cursor
/// they saved with the trigger and clear below. O(n).
#[must_use]
pub fn closing(bytes: &[u8]) -> Option<usize> {
    memchr::memmem::find(bytes, b"\x1b[u\x1b[0J")
        .into_iter()
        .chain(memchr::memmem::find(bytes, b"\x1b8\x1b[0J"))
        .min()
}
