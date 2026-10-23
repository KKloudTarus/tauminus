// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;

use tauminus_domain::{SessionId, SessionKind, SessionMeta, TelnetTarget, TerminalSize};

use crate::ports::{OutputSink, TelnetConnector};
use crate::session::SessionManager;
use crate::ssh::SshOpenError;

/// Opens Telnet sessions.
#[derive(Clone)]
pub struct TelnetSessions {
    connector: Arc<dyn TelnetConnector>,
    sessions: Arc<SessionManager>,
}

impl std::fmt::Debug for TelnetSessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelnetSessions").finish_non_exhaustive()
    }
}

impl TelnetSessions {
    pub fn new(connector: Arc<dyn TelnetConnector>, sessions: Arc<SessionManager>) -> Self {
        Self {
            connector,
            sessions,
        }
    }

    /// Connects and streams the session to `sink`. A slot is reserved
    /// first, so a full session table fails before any network work.
    pub async fn open(
        &self,
        target: &TelnetTarget,
        size: TerminalSize,
        sink: impl OutputSink,
    ) -> Result<SessionId, SshOpenError> {
        let slot = self.sessions.reserve()?;
        let stream = self.connector.connect(target, size).await?;
        Ok(self.sessions.attach(
            slot,
            stream,
            sink,
            SessionMeta {
                kind: SessionKind::Telnet,
            },
        ))
    }
}
