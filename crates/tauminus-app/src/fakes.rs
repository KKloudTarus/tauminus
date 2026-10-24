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
    /// Fails logins as an unreachable network would, for a while.
    pub(crate) unreachable: AtomicBool,
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
            if self.unreachable.load(Ordering::Relaxed) {
                return Err(ConnectError::Timeout {
                    host: "db".into(),
                    port: 22,
                    seconds: 15,
                });
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
    pub(crate) sftp: Arc<FakeSftp>,
}

/// SFTP whose transfers take `delay` each and remember what ran.
#[derive(Default)]
pub(crate) struct FakeSftp {
    pub(crate) delay: Mutex<Duration>,
    pub(crate) running: AtomicUsize,
    pub(crate) most_at_once: AtomicUsize,
    pub(crate) downloads: Mutex<Vec<crate::ports::Download>>,
    pub(crate) uploads: Mutex<Vec<crate::ports::Upload>>,
    /// Transfers that fail as a dropped connection would, before one works.
    pub(crate) drops: AtomicUsize,
}

impl FakeSftp {
    async fn transfer(
        &self,
        progress: watch::Sender<crate::ports::TransferProgress>,
        mut cancel: watch::Receiver<()>,
    ) -> Result<u64, crate::ports::SftpError> {
        let dropped = self
            .drops
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if dropped {
            return Err(crate::ports::SftpError::Closed);
        }
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.most_at_once.fetch_max(now, Ordering::SeqCst);
        let delay = *self.delay.lock().unwrap();
        let result = tokio::select! {
            () = tokio::time::sleep(delay) => {
                progress.send_modify(|p| { p.done = 10; p.total = 10; });
                Ok(10)
            }
            _ = cancel.changed() => Err(crate::ports::SftpError::Cancelled),
        };
        self.running.fetch_sub(1, Ordering::SeqCst);
        result
    }
}

impl crate::ports::SftpPort for FakeSftp {
    fn list<'a>(
        &'a self,
        _dir: &'a str,
    ) -> BoxFuture<'a, Result<Vec<crate::ports::RemoteEntry>, crate::ports::SftpError>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn stat<'a>(
        &'a self,
        path: &'a str,
    ) -> BoxFuture<'a, Result<crate::ports::RemoteEntry, crate::ports::SftpError>> {
        Box::pin(async move { Err(crate::ports::SftpError::Server(format!("no {path}"))) })
    }
    fn canonicalize<'a>(
        &'a self,
        _path: &'a str,
    ) -> BoxFuture<'a, Result<String, crate::ports::SftpError>> {
        Box::pin(async { Ok("/home/ops".to_owned()) })
    }
    fn mkdir<'a>(&'a self, _path: &'a str) -> BoxFuture<'a, Result<(), crate::ports::SftpError>> {
        Box::pin(async { Ok(()) })
    }
    fn remove_file<'a>(
        &'a self,
        _path: &'a str,
    ) -> BoxFuture<'a, Result<(), crate::ports::SftpError>> {
        Box::pin(async { Ok(()) })
    }
    fn remove_dir<'a>(
        &'a self,
        _path: &'a str,
    ) -> BoxFuture<'a, Result<(), crate::ports::SftpError>> {
        Box::pin(async { Ok(()) })
    }
    fn rename<'a>(
        &'a self,
        _from: &'a str,
        _to: &'a str,
    ) -> BoxFuture<'a, Result<(), crate::ports::SftpError>> {
        Box::pin(async { Ok(()) })
    }
    fn chmod<'a>(
        &'a self,
        _path: &'a str,
        _mode: u32,
    ) -> BoxFuture<'a, Result<(), crate::ports::SftpError>> {
        Box::pin(async { Ok(()) })
    }
    fn download<'a>(
        &'a self,
        job: &'a crate::ports::Download,
        progress: watch::Sender<crate::ports::TransferProgress>,
        cancel: watch::Receiver<()>,
    ) -> BoxFuture<'a, Result<u64, crate::ports::SftpError>> {
        self.downloads.lock().unwrap().push(job.clone());
        Box::pin(self.transfer(progress, cancel))
    }
    fn upload<'a>(
        &'a self,
        job: &'a crate::ports::Upload,
        progress: watch::Sender<crate::ports::TransferProgress>,
        cancel: watch::Receiver<()>,
    ) -> BoxFuture<'a, Result<u64, crate::ports::SftpError>> {
        self.uploads.lock().unwrap().push(job.clone());
        Box::pin(self.transfer(progress, cancel))
    }
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

    fn open_control<'a>(
        &'a self,
        _command: &'a str,
    ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>> {
        Box::pin(async { Err(ConnectError::Cancelled) })
    }

    fn open_sftp(&self) -> BoxFuture<'_, Result<Arc<dyn crate::ports::SftpPort>, ConnectError>> {
        let sftp = Arc::clone(&self.sftp) as Arc<dyn crate::ports::SftpPort>;
        Box::pin(async move { Ok(sftp) })
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

/// A snippet library in memory. `fail_next` makes the next save fail.
#[derive(Default)]
pub(crate) struct FakeSnippetStore {
    saved: Mutex<Option<tauminus_domain::Library>>,
    fail: AtomicBool,
    ids: AtomicUsize,
}

impl FakeSnippetStore {
    pub(crate) fn saved(&self) -> Option<tauminus_domain::Library> {
        self.saved.lock().unwrap().clone()
    }

    pub(crate) fn fail_next(&self) {
        self.fail.store(true, Ordering::SeqCst);
    }
}

impl crate::ports::SnippetStore for FakeSnippetStore {
    fn load(&self) -> BoxFuture<'_, Result<tauminus_domain::Library, crate::ports::StoreError>> {
        let saved = self.saved().unwrap_or_default();
        Box::pin(async move { Ok(saved) })
    }

    fn save<'a>(
        &'a self,
        library: &'a tauminus_domain::Library,
    ) -> BoxFuture<'a, Result<(), crate::ports::StoreError>> {
        Box::pin(async move {
            if self.fail.swap(false, Ordering::SeqCst) {
                return Err(crate::ports::StoreError::Io(std::io::Error::other(
                    "disk full",
                )));
            }
            *self.saved.lock().unwrap() = Some(library.clone());
            Ok(())
        })
    }

    fn new_id(&self) -> String {
        format!("snippet{}", self.ids.fetch_add(1, Ordering::SeqCst))
    }
}

/// A trigger library in memory. `fail_next` makes the next save fail.
#[derive(Default)]
pub(crate) struct FakeTriggerStore {
    saved: Mutex<Option<tauminus_domain::TriggerLibrary>>,
    fail: AtomicBool,
    ids: AtomicUsize,
}

impl FakeTriggerStore {
    pub(crate) fn saved(&self) -> Option<tauminus_domain::TriggerLibrary> {
        self.saved.lock().unwrap().clone()
    }

    pub(crate) fn fail_next(&self) {
        self.fail.store(true, Ordering::SeqCst);
    }
}

impl crate::ports::TriggerStore for FakeTriggerStore {
    fn load(
        &self,
    ) -> BoxFuture<'_, Result<tauminus_domain::TriggerLibrary, crate::ports::StoreError>> {
        let saved = self.saved().unwrap_or_default();
        Box::pin(async move { Ok(saved) })
    }

    fn save<'a>(
        &'a self,
        library: &'a tauminus_domain::TriggerLibrary,
    ) -> BoxFuture<'a, Result<(), crate::ports::StoreError>> {
        Box::pin(async move {
            if self.fail.swap(false, Ordering::SeqCst) {
                return Err(crate::ports::StoreError::Io(std::io::Error::other(
                    "disk full",
                )));
            }
            *self.saved.lock().unwrap() = Some(library.clone());
            Ok(())
        })
    }

    fn new_id(&self) -> String {
        format!("set{}", self.ids.fetch_add(1, Ordering::SeqCst))
    }
}
