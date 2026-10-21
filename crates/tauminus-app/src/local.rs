// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;

use tauminus_domain::{LocalShell, SessionId, SessionMeta, TerminalSize};

use crate::ports::{OutputSink, PtyLauncher};
use crate::session::{SessionError, SessionManager};

/// Opens local shell sessions.
#[derive(Clone)]
pub struct LocalShells {
    launcher: Arc<dyn PtyLauncher>,
    sessions: Arc<SessionManager>,
}

impl std::fmt::Debug for LocalShells {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalShells").finish_non_exhaustive()
    }
}

impl LocalShells {
    pub fn new(launcher: Arc<dyn PtyLauncher>, sessions: Arc<SessionManager>) -> Self {
        Self { launcher, sessions }
    }

    /// Starts a local shell and streams its output to `sink` until it exits.
    ///
    /// Launching a process blocks for the length of a fork and exec, so async
    /// callers should run this on a blocking thread. Must run inside a tokio
    /// runtime, which blocking threads spawned by tokio are.
    pub fn open(
        &self,
        shell: &LocalShell,
        size: TerminalSize,
        sink: impl OutputSink,
    ) -> Result<SessionId, SessionError> {
        let slot = self.sessions.reserve()?;
        let stream = self
            .launcher
            .launch(shell, size)
            .map_err(SessionError::Launch)?;
        Ok(self
            .sessions
            .attach(slot, stream, sink, SessionMeta::local()))
    }
}
