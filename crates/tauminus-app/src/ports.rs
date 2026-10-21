// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Ports between the use cases and the outside world.
//!
//! A terminal stream is two bounded channels plus a terminator, not an async
//! trait object. Writing to a PTY or an SSH channel can block, so each adapter
//! owns its I/O on its own thread or task, and the use case talks to it by
//! message. Bounded channels carry backpressure in both directions.

use std::fmt;

use tauminus_domain::{LocalShell, TerminalSize};
use thiserror::Error;
use tokio::sync::mpsc;

/// Messages from the use case to a terminal stream, applied in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalInput {
    Data(Vec<u8>),
    Resize(TerminalSize),
}

/// Messages from a terminal stream to the use case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    Output(Vec<u8>),
    /// The remote side ended. `code` is `None` when the platform reports no
    /// exit status, for example after a signal.
    Exited {
        code: Option<u32>,
    },
}

/// Ends a terminal stream from outside its input queue.
///
/// Termination cannot wait behind queued input: when the remote process stops
/// reading, the writer blocks and the queue stops moving. Implementations act
/// on the process or channel directly, and both methods must be idempotent and
/// safe to call after the stream has ended.
pub trait Terminator: Send + Sync + 'static {
    /// Asks the remote side to end, as closing a terminal window does.
    fn terminate(&self);

    /// Ends the remote side forcibly, for when `terminate` was ignored.
    fn kill(&self) {
        self.terminate();
    }
}

/// One interactive byte stream: a local shell on a PTY, or a shell channel on
/// an SSH connection.
pub struct TerminalStream {
    pub input: mpsc::Sender<TerminalInput>,
    pub events: mpsc::Receiver<TerminalEvent>,
    pub terminator: Box<dyn Terminator>,
}

impl fmt::Debug for TerminalStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalStream")
            .field("input", &self.input)
            .field("events", &self.events)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error)]
pub enum LaunchError {
    #[error("failed to start {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Starts local shells.
pub trait PtyLauncher: Send + Sync + 'static {
    fn launch(&self, shell: &LocalShell, size: TerminalSize)
    -> Result<TerminalStream, LaunchError>;
}

/// The view closed its end of the output stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("output sink closed")]
pub struct SinkClosed;

/// Where a session's output goes, usually an IPC channel to the terminal view.
pub trait OutputSink: Send + Sync + 'static {
    fn send_output(&self, batch: Vec<u8>) -> Result<(), SinkClosed>;
    fn send_exit(&self, code: Option<u32>);
}
