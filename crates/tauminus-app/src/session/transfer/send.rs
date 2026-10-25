// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Files the user chose, sent to `rz` on the host (ADR 0026).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tauminus_domain::trzsz::Upload;
use tauminus_domain::{Need, Transmit, TransmitError, TransmitOutput};

use super::IDLE;
use crate::ports::OutgoingFile;

/// What sends the files: one protocol or the other.
pub(super) enum Outgoing {
    Zmodem(Box<Transmit>),
    Trzsz(Box<Upload>),
}

impl Outgoing {
    pub(super) fn feed(&mut self, input: &[u8], out: &mut TransmitOutput) {
        match self {
            Self::Zmodem(transmit) => transmit.feed(input, out),
            Self::Trzsz(upload) => upload.feed(input, out),
        }
    }

    pub(super) fn abort(&mut self, out: &mut TransmitOutput, reason: &'static str) {
        match self {
            Self::Zmodem(transmit) => transmit.abort(out, reason),
            Self::Trzsz(upload) => upload.abort(out, reason),
        }
    }

    pub(super) fn timeout(&mut self, out: &mut TransmitOutput) {
        match self {
            Self::Zmodem(transmit) => transmit.timeout(out),
            Self::Trzsz(upload) => upload.timed_out(out),
        }
    }

    pub(super) fn ended(&self) -> bool {
        match self {
            Self::Zmodem(transmit) => transmit.ended(),
            Self::Trzsz(upload) => upload.ended(),
        }
    }

    pub(super) fn completed(&self) -> bool {
        match self {
            Self::Zmodem(transmit) => transmit.completed(),
            Self::Trzsz(upload) => upload.completed(),
        }
    }

    pub(super) fn answered(&self) -> bool {
        match self {
            Self::Zmodem(transmit) => transmit.answered(),
            Self::Trzsz(upload) => upload.answered(),
        }
    }

    pub(super) fn poll(&mut self, out: &mut TransmitOutput) -> Need {
        match self {
            Self::Zmodem(transmit) => transmit.poll(out),
            Self::Trzsz(upload) => upload.poll(out),
        }
    }

    pub(super) fn provide(&mut self, data: &[u8], out: &mut TransmitOutput) {
        match self {
            Self::Zmodem(transmit) => transmit.provide(data, out),
            Self::Trzsz(upload) => upload.provide(data, out),
        }
    }

    pub(super) fn finish(&mut self, out: &mut TransmitOutput) {
        match self {
            Self::Zmodem(transmit) => transmit.finish(out),
            Self::Trzsz(upload) => upload.finish(out),
        }
    }

    pub(super) fn start(&mut self, name: &str, size: u64) -> Result<(), TransmitError> {
        match self {
            Self::Zmodem(transmit) => transmit.start(name, size),
            Self::Trzsz(upload) => upload.next_file(name, size),
        }
    }

    /// The queue is empty: what a [`Need::Hold`] kept may go.
    pub(super) fn release(&mut self) {
        if let Self::Trzsz(upload) = self {
            upload.release();
        }
    }

    pub(super) fn rest(&mut self) -> Vec<u8> {
        match self {
            Self::Zmodem(transmit) => transmit.rest(),
            Self::Trzsz(upload) => upload.rest(),
        }
    }

    /// How long the host may leave the send where it is.
    pub(super) fn idle(&self) -> Duration {
        match self {
            Self::Zmodem(_) => IDLE,
            Self::Trzsz(upload) => upload.timeout(),
        }
    }

    pub(super) fn trzsz(&self) -> bool {
        matches!(self, Self::Trzsz(_))
    }
}

/// One send: the protocol, the files still to go, and what went.
pub(super) struct Sending {
    pub(super) engine: Outgoing,
    pub(super) queue: VecDeque<Box<dyn OutgoingFile>>,
    pub(super) current: Option<Box<dyn OutgoingFile>>,
    /// The furthest byte of the current file the host asked for.
    pub(super) reached: u64,
    pub(super) reported: u64,
    pub(super) reported_at: Instant,
    pub(super) buffer: Vec<u8>,
    pub(super) files: Vec<String>,
    pub(super) more_files: u64,
    pub(super) skipped: Vec<String>,
    pub(super) more_skipped: u64,
    /// When the host last moved the send on: asked for data, or took a
    /// file. Its output alone does not count.
    pub(super) moved_at: Instant,
    /// What came after the host's ZRINIT while the user chose: shown if
    /// `rz` never answers, as it gave up meanwhile.
    pub(super) held: Vec<u8>,
}

impl Sending {
    pub(super) fn new(engine: Outgoing, files: Vec<Box<dyn OutgoingFile>>, held: Vec<u8>) -> Self {
        Self {
            engine,
            queue: files.into(),
            current: None,
            reached: 0,
            reported: 0,
            reported_at: Instant::now(),
            buffer: Vec::new(),
            files: Vec::new(),
            more_files: 0,
            skipped: Vec::new(),
            more_skipped: 0,
            moved_at: Instant::now(),
            held,
        }
    }
}
