// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Files the user chose, sent to `rz` on the host (ADR 0026).

use std::collections::VecDeque;
use std::time::Instant;

use tauminus_domain::Transmit;

use crate::ports::OutgoingFile;

/// One send: the protocol, the files still to go, and what went.
pub(super) struct Sending {
    pub(super) transmit: Transmit,
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
    pub(super) fn new(files: Vec<Box<dyn OutgoingFile>>, held: Vec<u8>) -> Self {
        Self {
            transmit: Transmit::new(),
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
