// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Mosh sessions: SSH signs in and starts `mosh-server`, then the local
//! `mosh-client` carries the terminal over UDP, as the `mosh` script does.

use std::sync::Arc;

use tauminus_domain::{
    MOSH_SERVER_SCRIPT, MoshConnect, MoshError, Route, SessionId, SessionKind, SessionMeta,
    TerminalSize,
};
use thiserror::Error;

use crate::connections::Login;
use crate::ports::{ConnectError, LaunchError, MoshClient, OutputSink, Prompter, SshConnector};
use crate::session::{SessionError, SessionManager};

#[derive(Debug, Error)]
pub enum MoshOpenError {
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error(transparent)]
    Server(#[from] MoshError),
    #[error("Mosh talks UDP straight to the host, so it cannot go through a jump host or a proxy")]
    Routed,
    #[error("could not start mosh-client: {0}")]
    Client(#[source] LaunchError),
}

/// Opens Mosh sessions to saved hosts.
#[derive(Clone)]
pub struct MoshSessions {
    connector: Arc<dyn SshConnector>,
    client: Arc<dyn MoshClient>,
    sessions: Arc<SessionManager>,
}

impl std::fmt::Debug for MoshSessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoshSessions").finish_non_exhaustive()
    }
}

impl MoshSessions {
    pub fn new(
        connector: Arc<dyn SshConnector>,
        client: Arc<dyn MoshClient>,
        sessions: Arc<SessionManager>,
    ) -> Self {
        Self {
            connector,
            client,
            sessions,
        }
    }

    /// Signs in as an SSH connect would, starts the server, and streams the
    /// client's terminal to `sink`. The SSH connection ends once the server
    /// answered; a session slot is held from the start.
    pub async fn open_saved(
        &self,
        login: &Login,
        size: TerminalSize,
        sink: impl OutputSink,
        prompter: Arc<dyn Prompter>,
    ) -> Result<SessionId, MoshOpenError> {
        let slot = self.sessions.reserve()?;
        let target = &login.target;
        if !target.jumps.is_empty() || target.route != Route::Direct {
            return Err(MoshOpenError::Routed);
        }
        // No client, no point signing in and starting a server.
        self.client.ready().map_err(MoshOpenError::Client)?;
        let outcome = self
            .connector
            .run_script(
                target,
                prompter,
                Arc::clone(&login.credentials),
                MOSH_SERVER_SCRIPT,
            )
            .await?;
        let connect = MoshConnect::parse(&outcome.output)?;
        let address = outcome.peer.ok_or(MoshOpenError::Routed)?;
        let stream = self
            .client
            .launch(address, &connect, size)
            .await
            .map_err(MoshOpenError::Client)?;
        Ok(self.sessions.attach(
            slot,
            stream,
            sink,
            SessionMeta {
                kind: SessionKind::Mosh,
                host: Some(login.saved()),
            },
        ))
    }
}

#[cfg(test)]
mod tests;
