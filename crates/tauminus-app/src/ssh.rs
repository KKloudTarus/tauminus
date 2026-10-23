// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;

use tauminus_domain::{SavedHost, SessionId, SessionKind, SessionMeta, SshTarget, TerminalSize};
use thiserror::Error;

use crate::connections::Login;
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
        self.open_for(target, size, sink, prompter, credentials, None)
            .await
    }

    async fn open_for(
        &self,
        target: &SshTarget,
        size: TerminalSize,
        sink: impl OutputSink,
        prompter: Arc<dyn Prompter>,
        credentials: Arc<Credentials>,
        host: Option<SavedHost>,
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
                host,
            },
        ))
    }

    /// Opens a shell on a saved host, on a connection of its own; SFTP
    /// and forwards never share it (see `Connections`).
    pub async fn open_saved(
        &self,
        login: &Login,
        size: TerminalSize,
        sink: impl OutputSink,
        prompter: Arc<dyn Prompter>,
    ) -> Result<SessionId, SshOpenError> {
        self.open_for(
            &login.target,
            size,
            sink,
            prompter,
            Arc::clone(&login.credentials),
            Some(login.saved()),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;
    use crate::SessionConfig;
    use crate::fakes::{login, prompter};
    use crate::ports::{
        BoxFuture, ConnectError, ScriptOutcome, SinkClosed, SshConnection, TerminalStream,
        Terminator,
    };

    struct Quiet;

    impl OutputSink for Quiet {
        fn send_output(&self, _batch: Vec<u8>) -> Result<(), SinkClosed> {
            Ok(())
        }

        fn send_exit(&self, _code: Option<u32>) {}
    }

    struct Nothing;

    impl Terminator for Nothing {
        fn terminate(&self) {}
    }

    /// Opens a shell that says nothing.
    struct Shells;

    impl SshConnector for Shells {
        fn connection<'a>(
            &'a self,
            _target: &'a SshTarget,
            _prompter: Arc<dyn Prompter>,
            _credentials: Arc<Credentials>,
            _forward_agent: bool,
        ) -> BoxFuture<'a, Result<Arc<dyn SshConnection>, ConnectError>> {
            Box::pin(async { Err(ConnectError::Cancelled) })
        }

        fn connect_with<'a>(
            &'a self,
            _target: &'a SshTarget,
            _size: TerminalSize,
            _prompter: Arc<dyn Prompter>,
            _credentials: Arc<Credentials>,
        ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>> {
            Box::pin(async {
                let (input, mut drain) = mpsc::channel(8);
                let (keep, events) = mpsc::channel(8);
                tokio::spawn(async move {
                    let _keep = keep;
                    while drain.recv().await.is_some() {}
                });
                Ok(TerminalStream {
                    input,
                    events,
                    terminator: Box::new(Nothing),
                })
            })
        }

        fn run_script<'a>(
            &'a self,
            _target: &'a SshTarget,
            _prompter: Arc<dyn Prompter>,
            _credentials: Arc<Credentials>,
            _script: &'a str,
        ) -> BoxFuture<'a, Result<ScriptOutcome, ConnectError>> {
            Box::pin(async { Err(ConnectError::Cancelled) })
        }
    }

    #[tokio::test]
    async fn a_saved_host_s_session_records_its_login_and_an_ad_hoc_one_none() {
        let manager = Arc::new(SessionManager::new(SessionConfig::default()));
        let ssh = SshSessions::new(Arc::new(Shells), Arc::clone(&manager));
        let saved = login("db", "db.example");
        let id = ssh
            .open_saved(&saved, TerminalSize::default(), Quiet, prompter())
            .await
            .unwrap();
        assert_eq!(manager.meta(id).unwrap().host, Some(saved.saved()));
        let ad_hoc = ssh
            .open(&saved.target, TerminalSize::default(), Quiet, prompter())
            .await
            .unwrap();
        assert_eq!(manager.meta(ad_hoc).unwrap().host, None);
    }
}
