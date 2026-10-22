// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;

use tauminus_domain::{SessionId, SessionKind, SessionMeta, SshTarget, TerminalSize};
use thiserror::Error;

use crate::ports::{ConnectError, Credentials, OutputSink, Prompter, ScriptOutcome, SshConnector};
use crate::session::{SessionError, SessionManager};

#[derive(Debug, Error)]
pub enum SshOpenError {
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Connect(#[from] ConnectError),
}

/// Opens SSH shell sessions.
#[derive(Clone)]
pub struct SshSessions {
    connector: Arc<dyn SshConnector>,
    sessions: Arc<SessionManager>,
}

impl std::fmt::Debug for SshSessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshSessions").finish_non_exhaustive()
    }
}

impl SshSessions {
    pub fn new(connector: Arc<dyn SshConnector>, sessions: Arc<SessionManager>) -> Self {
        Self {
            connector,
            sessions,
        }
    }

    /// Connects, authenticates, and starts a shell, then streams its output to
    /// `sink`. A slot is reserved before any network work, so a full session
    /// table fails fast and never leaves a half-open connection.
    pub async fn open(
        &self,
        target: &SshTarget,
        size: TerminalSize,
        sink: impl OutputSink,
        prompter: Arc<dyn Prompter>,
    ) -> Result<SessionId, SshOpenError> {
        self.open_with(target, size, sink, prompter, Arc::default())
            .await
    }

    /// Runs a one-off script on `target`, such as adding a key to
    /// `authorized_keys`. No session is kept.
    pub async fn run_script(
        &self,
        target: &SshTarget,
        prompter: Arc<dyn Prompter>,
        credentials: Arc<Credentials>,
        script: &str,
    ) -> Result<ScriptOutcome, ConnectError> {
        self.connector
            .run_script(target, prompter, credentials, script)
            .await
    }

    /// As [`SshSessions::open`], with a vault identity's credentials.
    pub async fn open_with(
        &self,
        target: &SshTarget,
        size: TerminalSize,
        sink: impl OutputSink,
        prompter: Arc<dyn Prompter>,
        credentials: Arc<Credentials>,
    ) -> Result<SessionId, SshOpenError> {
        let slot = self.sessions.reserve()?;
        let stream = self
            .connector
            .connect_with(target, size, prompter, credentials)
            .await?;
        Ok(self.sessions.attach(
            slot,
            stream,
            sink,
            SessionMeta {
                kind: SessionKind::Ssh,
            },
        ))
    }
}
