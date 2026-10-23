// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;

use tauminus_domain::{SerialPortInfo, SerialSettings, SessionId, SessionKind, SessionMeta};

use crate::ports::{OutputSink, SerialPorts};
use crate::session::{SessionError, SessionManager};

/// Opens serial port sessions.
#[derive(Clone)]
pub struct SerialSessions {
    ports: Arc<dyn SerialPorts>,
    sessions: Arc<SessionManager>,
}

impl std::fmt::Debug for SerialSessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SerialSessions").finish_non_exhaustive()
    }
}

impl SerialSessions {
    pub fn new(ports: Arc<dyn SerialPorts>, sessions: Arc<SessionManager>) -> Self {
        Self { ports, sessions }
    }

    /// What is attached now. Blocks while the system lists them.
    pub fn list(&self) -> Vec<SerialPortInfo> {
        self.ports.list()
    }

    /// Opens `settings` and streams the port to `sink`. A slot is reserved
    /// first, so a full session table fails before the port is touched.
    /// Blocks while the port opens.
    pub fn open(
        &self,
        settings: &SerialSettings,
        sink: impl OutputSink,
    ) -> Result<SessionId, SessionError> {
        let slot = self.sessions.reserve()?;
        let stream = self.ports.open(settings).map_err(SessionError::Launch)?;
        Ok(self
            .sessions
            .attach(slot, stream, sink, SessionMeta::of(SessionKind::Serial)))
    }
}
