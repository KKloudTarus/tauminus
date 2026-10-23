// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! An SSH connector and connection for use case tests, with no network.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauminus_domain::{
    AuthOptions, Endpoint, Forward, HostId, HostName, Route, SessionOptions, SshTarget,
    TerminalSize, UserName,
};
use tokio::sync::watch;

use crate::connections::Login;
use crate::ports::{
    Answer, BoxFuture, ConnectError, Credentials, ForwardHandle, ForwardStatus, Prompt, Prompter,
    ScriptOutcome, SshConnection, SshConnector, TerminalStream, TunnelError,
};

#[derive(Default)]
pub(crate) struct FakeConnector {
    /// Logins made so far, with whether each forwards the agent.
    pub(crate) logins: Mutex<Vec<bool>>,
    /// How long each login takes.
    pub(crate) delay: Duration,
    /// Every connection made, so a test can close one.
    pub(crate) made: Mutex<Vec<Arc<FakeConnection>>>,
    pub(crate) refuse: AtomicBool,
}

impl SshConnector for FakeConnector {
    fn connection<'a>(
        &'a self,
        _target: &'a SshTarget,
        _prompter: Arc<dyn Prompter>,
        _credentials: Arc<Credentials>,
        forward_agent: bool,
    ) -> BoxFuture<'a, Result<Arc<dyn SshConnection>, ConnectError>> {
        Box::pin(async move {
            tokio::time::sleep(self.delay).await;
            if self.refuse.load(Ordering::Relaxed) {
                return Err(ConnectError::Cancelled);
            }
            self.logins.lock().unwrap().push(forward_agent);
            let connection = Arc::new(FakeConnection::default());
            self.made.lock().unwrap().push(Arc::clone(&connection));
            Ok(connection as Arc<dyn SshConnection>)
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

impl FakeConnector {
    pub(crate) fn logins(&self) -> usize {
        self.logins.lock().unwrap().len()
    }
}

#[derive(Default)]
pub(crate) struct FakeConnection {
    pub(crate) closed: AtomicBool,
    pub(crate) next_port: AtomicUsize,
    /// Forwards started, kept so a test can end them.
    pub(crate) forwards: Mutex<Vec<Arc<watch::Sender<ForwardStatus>>>>,
    /// One per forward started; closed once its handle drops.
    pub(crate) stops: Mutex<Vec<watch::Receiver<()>>>,
}

impl FakeConnection {
    /// Forwards whose handle is still held.
    pub(crate) fn live_forwards(&self) -> usize {
        self.stops
            .lock()
            .unwrap()
            .iter()
            .filter(|stop| stop.has_changed().is_ok())
            .count()
    }
}

impl FakeConnection {
    /// Ends every forward this connection runs, as a dropped link does.
    pub(crate) fn drop_link(&self) {
        self.closed.store(true, Ordering::Relaxed);
        for status in self.forwards.lock().unwrap().iter() {
            status.send_modify(|s| s.ended = Some("the connection closed".into()));
        }
    }
}

impl SshConnection for FakeConnection {
    fn open_shell<'a>(
        &'a self,
        _size: TerminalSize,
        _options: &'a SessionOptions,
    ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>> {
        Box::pin(async { Err(ConnectError::Cancelled) })
    }

    fn start_forward<'a>(
        &'a self,
        forward: &'a Forward,
    ) -> BoxFuture<'a, Result<ForwardHandle, TunnelError>> {
        Box::pin(async move {
            if self.closed.load(Ordering::Relaxed) {
                return Err(TunnelError::Closed);
            }
            let port = match forward.bind().port {
                0 => {
                    40_000 + u16::try_from(self.next_port.fetch_add(1, Ordering::Relaxed)).unwrap()
                }
                port => port,
            };
            let (status_tx, status) = watch::channel(ForwardStatus::default());
            let status_tx = Arc::new(status_tx);
            self.forwards.lock().unwrap().push(Arc::clone(&status_tx));
            let (stop, stopped) = watch::channel(());
            self.stops.lock().unwrap().push(stopped);
            let (_, finished) = watch::channel(());
            Ok(ForwardHandle::new(port, status, stop, finished))
        })
    }

    fn is_open(&self) -> bool {
        !self.closed.load(Ordering::Relaxed)
    }

    fn closed(&self) -> BoxFuture<'_, ()> {
        Box::pin(std::future::pending())
    }
}

pub(crate) struct NoPrompts;

impl Prompter for NoPrompts {
    fn ask(&self, _prompt: Prompt) -> BoxFuture<'_, Answer> {
        Box::pin(async { Answer::Cancel })
    }
}

pub(crate) fn prompter() -> Arc<dyn Prompter> {
    Arc::new(NoPrompts)
}

/// A login for host `id` to `address`.
pub(crate) fn login(id: &str, address: &str) -> Login {
    let destination = Endpoint::new(
        UserName::parse("ops").unwrap(),
        HostName::parse(address).unwrap(),
        22,
    )
    .unwrap();
    Login {
        host: HostId::parse(id).unwrap(),
        target: SshTarget::new(destination, vec![], Route::Direct, AuthOptions::default()).unwrap(),
        identity: None,
        credentials: Arc::default(),
    }
}
