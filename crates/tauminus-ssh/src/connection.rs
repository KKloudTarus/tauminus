// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! One login, shared by the shell, SFTP, and forwards that hold it.
//!
//! Each user keeps an `Arc<Connection>`. When the last one drops, the hops
//! disconnect innermost first, on the runtime the connection was made on,
//! so a tunnel closes before the hop that carries it.

use std::sync::{Arc, Weak};

use russh::Disconnect;
use russh::client::Handle;
use tauminus_app::ports::{
    BoxFuture, ConnectError, ForwardHandle, SftpPort, SshConnection, TerminalStream, TunnelError,
};
use tauminus_domain::{Encoding, Forward, SessionOptions, TerminalSize};
use tokio::sync::watch;

use crate::tunnel::Routes;
use crate::{Client, bridge, protocol, tunnel};

pub(crate) struct Connection {
    me: Weak<Connection>,
    /// Every hop, innermost last; never empty.
    hops: Vec<Handle<Client>>,
    /// Turns true when any hop's session ends.
    closed: watch::Receiver<bool>,
    /// Remote forwards running on the last hop.
    pub(crate) routes: Arc<Routes>,
    /// Bytes forwarded connections may hold for their peers; see `pipe`.
    pub(crate) ledger: Arc<crate::pipe::Ledger>,
    /// Whether the last hop serves agent channels for a shell that asks.
    forward_agent: bool,
    runtime: tokio::runtime::Handle,
    /// The destination's address, when it was reached straight over TCP.
    peer: Option<std::net::IpAddr>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("hops", &self.hops.len())
            .field("open", &self.is_open())
            .finish_non_exhaustive()
    }
}

impl Connection {
    /// Must be called on a Tokio runtime, which also runs the teardown.
    pub(crate) fn new(
        hops: Vec<Handle<Client>>,
        closed: watch::Receiver<bool>,
        routes: Arc<Routes>,
        forward_agent: bool,
        ledger: crate::pipe::Ledger,
        peer: Option<std::net::IpAddr>,
    ) -> Arc<Self> {
        assert!(!hops.is_empty(), "a connection has at least one hop");
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            hops,
            closed,
            routes,
            ledger: Arc::new(ledger),
            forward_agent,
            runtime: tokio::runtime::Handle::current(),
            peer,
        })
    }

    /// The address the destination was reached at, if straight over TCP.
    pub(crate) const fn peer(&self) -> Option<std::net::IpAddr> {
        self.peer
    }

    /// The hop that reaches the target.
    pub(crate) fn last(&self) -> &Handle<Client> {
        self.hops.last().expect("never empty")
    }

    /// A lease for something that outlives the call that made it.
    pub(crate) fn lease(&self) -> Arc<Self> {
        self.me.upgrade().expect("called through a live Arc")
    }

    pub(crate) fn closed_watch(&self) -> watch::Receiver<bool> {
        self.closed.clone()
    }

    async fn shell(
        &self,
        size: TerminalSize,
        options: &SessionOptions,
    ) -> Result<TerminalStream, ConnectError> {
        let channel = self.last().channel_open_session().await.map_err(protocol)?;
        channel
            .request_pty(
                true,
                "xterm-256color",
                u32::from(size.cols()),
                u32::from(size.rows()),
                0,
                0,
                &[],
            )
            .await
            .map_err(protocol)?;
        // Like OpenSSH, without asking for a reply: a server refusing a
        // name, as `AcceptEnv` makes most do, is not an error.
        for var in &options.env {
            channel
                .set_env(false, var.name(), var.value())
                .await
                .map_err(protocol)?;
        }
        if self.forward_agent && options.forward_agent {
            channel.agent_forward(false).await.map_err(protocol)?;
        }
        match &options.tmux {
            // The session survives the connection: the next connect joins it.
            Some(tmux) => channel
                .exec(true, tmux.command(options.encoding == Encoding::Utf8))
                .await
                .map_err(protocol)?,
            None => channel.request_shell(true).await.map_err(protocol)?,
        }
        Ok(bridge::spawn(channel, self.lease(), options.encoding))
    }

    /// No PTY, no environment, no agent: the command alone, its output
    /// passed through as bytes.
    async fn control(&self, command: &str) -> Result<TerminalStream, ConnectError> {
        let channel = self.last().channel_open_session().await.map_err(protocol)?;
        channel.exec(true, command).await.map_err(protocol)?;
        Ok(bridge::spawn(channel, self.lease(), Encoding::Utf8))
    }
}

impl SshConnection for Connection {
    fn open_shell<'a>(
        &'a self,
        size: TerminalSize,
        options: &'a SessionOptions,
    ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>> {
        Box::pin(self.shell(size, options))
    }

    fn open_control<'a>(
        &'a self,
        command: &'a str,
    ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>> {
        Box::pin(self.control(command))
    }

    fn start_forward<'a>(
        &'a self,
        forward: &'a Forward,
    ) -> BoxFuture<'a, Result<ForwardHandle, TunnelError>> {
        Box::pin(tunnel::start(self.lease(), forward))
    }

    fn open_sftp(&self) -> BoxFuture<'_, Result<Arc<dyn SftpPort>, ConnectError>> {
        Box::pin(async move {
            let sftp = crate::sftp::open(self.lease()).await?;
            Ok(Arc::new(sftp) as Arc<dyn SftpPort>)
        })
    }

    fn is_open(&self) -> bool {
        !*self.closed.borrow() && !self.last().is_closed()
    }

    fn closed(&self) -> BoxFuture<'_, ()> {
        let mut closed = self.closed.clone();
        Box::pin(async move {
            // An error means every hop's handler is gone: closed as well.
            let _ = closed.wait_for(|closed| *closed).await;
        })
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let hops = std::mem::take(&mut self.hops);
        self.runtime.spawn(async move {
            for hop in hops.iter().rev() {
                let _ = hop.disconnect(Disconnect::ByApplication, "", "en").await;
            }
        });
    }
}

/// Held by each hop's handler. russh drops the handler when that hop's
/// session ends, however it ends, which marks the connection closed.
#[derive(Debug)]
pub(crate) struct Alarm(pub(crate) Arc<watch::Sender<bool>>);

impl Drop for Alarm {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}
