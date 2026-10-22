// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SSH sessions for Tauminus, built on russh.
//!
//! A connect walks the hops in order: the first over the target's route
//! (direct TCP, `ProxyCommand`, SOCKS5, or HTTP CONNECT), each later one
//! through a `direct-tcpip` channel of the hop before it, as OpenSSH
//! `ProxyJump` does. Every hop verifies its host key and authenticates on its
//! own. The last hop opens a shell with a PTY, after sending the host's
//! environment variables and, when asked, a request to forward the agent.

mod auth;
mod bridge;
mod config;
mod forward;
mod host_keys;
pub mod known_hosts;
mod transport;
#[cfg_attr(
    not(any(windows, test)),
    expect(dead_code, reason = "agent pipes and Pageant exist only on Windows")
)]
mod windows_agent;

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use russh::client::{self, Handle};
use russh::keys::{Algorithm, PublicKeyOrCertificate};
use tauminus_app::ports::{
    BoxFuture, ConnectError, Credentials, Prompter, SshConnector, TerminalStream,
};
use tauminus_domain::{Endpoint, SshTarget, TerminalSize};
use tokio::sync::{Notify, watch};

pub use auth::AgentSource;
pub use config::OpenSshConfig;
pub use host_keys::HostKeyStore;

use crate::known_hosts::KnownHosts;

/// How long each TCP connect, proxy handshake, and SSH handshake up to the
/// host key check may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the rest of the handshake may take once the host key check has
/// started. It covers the user reading a fingerprint, which the prompt bounds
/// on its own; this only stops a server that stalls after it.
const AFTER_HOST_KEY_TIMEOUT: Duration = Duration::from_mins(15);

/// Opens SSH shell sessions with russh.
#[derive(Debug, Clone)]
pub struct RusshConnector {
    host_keys: HostKeyStore,
    home: Option<PathBuf>,
    agent: AgentSource,
    connect_timeout: Duration,
}

impl RusshConnector {
    /// `home` locates default key files and expands `~` in identity paths.
    pub fn new(host_keys: HostKeyStore, home: Option<PathBuf>, agent: AgentSource) -> Self {
        Self {
            host_keys,
            home,
            agent,
            connect_timeout: CONNECT_TIMEOUT,
        }
    }

    /// Replaces the 15 s limit on each TCP connect, proxy handshake, and SSH
    /// handshake up to the host key check.
    #[must_use]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    async fn open(
        &self,
        target: &SshTarget,
        size: TerminalSize,
        prompter: Arc<dyn Prompter>,
        credentials: Arc<Credentials>,
    ) -> Result<TerminalStream, ConnectError> {
        let none = Credentials::default();
        // Dropped when this future ends or is dropped, which cancels a host
        // key prompt still waiting inside a handshake nobody awaits anymore.
        let (_alive, alive) = watch::channel(());
        let hops: Vec<&Endpoint> = target
            .jumps
            .iter()
            .chain(std::iter::once(&target.destination))
            .collect();
        let mut connections: Vec<Handle<Client>> = Vec::with_capacity(hops.len());
        for (index, hop) in hops.iter().enumerate() {
            let io = match connections.last() {
                None => transport::open(&target.route, hop, self.connect_timeout).await?,
                Some(previous) => {
                    let tunnel = previous
                        .channel_open_direct_tcpip(
                            hop.host.as_str(),
                            u32::from(hop.port),
                            "127.0.0.1",
                            0,
                        )
                        .await
                        .map_err(|e| {
                            ConnectError::Proxy(format!("jump host could not reach {hop}: {e}"))
                        })?;
                    Box::new(tunnel.into_stream())
                }
            };
            let known = self.host_keys.load().await?;
            let config = Arc::new(client_config(&known, hop));
            let reached = Arc::new(Notify::new());
            let last = index + 1 == hops.len();
            let handler = Client {
                forward: (last && target.session.forward_agent)
                    .then(|| forward::AgentForward::new(self.agent.clone())),
                host: hop.host.to_string(),
                port: hop.port,
                known,
                host_keys: self.host_keys.clone(),
                prompter: Arc::clone(&prompter),
                reached: Arc::clone(&reached),
                alive: alive.clone(),
            };
            let mut handle =
                handshake(config, io, handler, &reached, hop, self.connect_timeout).await?;
            auth::Authenticator {
                handle: &mut handle,
                endpoint: hop,
                options: &target.auth,
                prompter: &prompter,
                home: self.home.as_deref(),
                agent: &self.agent,
                // A jump host is another login; the identity is for the target.
                credentials: if last { &credentials } else { &none },
            }
            .run()
            .await?;
            tracing::debug!(hop = %hop, index, "authenticated");
            connections.push(handle);
        }
        let last = connections
            .last()
            .ok_or_else(|| ConnectError::Protocol("no hops".into()))?;
        let channel = last.channel_open_session().await.map_err(protocol)?;
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
        for var in &target.session.env {
            channel
                .set_env(false, var.name(), var.value())
                .await
                .map_err(protocol)?;
        }
        if target.session.forward_agent {
            channel.agent_forward(false).await.map_err(protocol)?;
        }
        channel.request_shell(true).await.map_err(protocol)?;
        Ok(bridge::spawn(channel, connections))
    }
}

/// Runs the SSH handshake. `limit` applies until the host key check starts,
/// so a slow user is not reported as a slow server.
async fn handshake(
    config: Arc<client::Config>,
    io: transport::BoxedIo,
    handler: Client,
    reached: &Notify,
    hop: &Endpoint,
    limit: Duration,
) -> Result<Handle<Client>, ConnectError> {
    let timeout = |seconds: u64| ConnectError::Timeout {
        host: hop.host.to_string(),
        port: hop.port,
        seconds,
    };
    let handshake = client::connect_stream(config, io, handler);
    tokio::pin!(handshake);
    let result = tokio::select! {
        result = &mut handshake => result,
        () = reached.notified() => tokio::time::timeout(AFTER_HOST_KEY_TIMEOUT, &mut handshake)
            .await
            .map_err(|_| timeout(AFTER_HOST_KEY_TIMEOUT.as_secs()))?,
        () = tokio::time::sleep(limit) => return Err(timeout(limit.as_secs())),
    };
    result.map_err(HandlerError::into_connect_error)
}

impl SshConnector for RusshConnector {
    fn connect_with<'a>(
        &'a self,
        target: &'a SshTarget,
        size: TerminalSize,
        prompter: Arc<dyn Prompter>,
        credentials: Arc<Credentials>,
    ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>> {
        Box::pin(self.open(target, size, prompter, credentials))
    }
}

/// Output messages russh buffers per channel before it stops reading the
/// socket and TCP slows the server. 32 packets of at most 32 KiB bound a
/// paused session at 1 MiB instead of the default 3.2 MiB.
const CHANNEL_BUFFER: usize = 32;

fn client_config(known: &KnownHosts, hop: &Endpoint) -> client::Config {
    let recorded = known.recorded_algorithms(hop.host.as_str(), hop.port);
    let certificates = !known
        .authorities_for(hop.host.as_str(), hop.port)
        .is_empty();
    client::Config {
        preferred: preferred(&recorded, certificates),
        // Notice a dead link within about 90 s instead of hanging forever.
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        inactivity_timeout: None,
        channel_buffer_size: CHANNEL_BUFFER,
        ..client::Config::default()
    }
}

/// Host key algorithms to offer, as OpenSSH `order_hostkeyalgs` does: types
/// already recorded for the host first, so a server with several keys
/// presents the one on record. SHA-1 `ssh-rsa` is left out, as in OpenSSH
/// 8.8 and later; RSA keys still work through `rsa-sha2-*`. Certificate
/// variants are offered only when an authority covers the host.
fn preferred(recorded: &[Algorithm], certificates: bool) -> russh::Preferred {
    let mut keys: Vec<Algorithm> = russh::Preferred::DEFAULT
        .key
        .iter()
        .filter(|a| !matches!(a, Algorithm::Rsa { hash: None }))
        .cloned()
        .collect();
    let rsa = |a: &Algorithm| matches!(a, Algorithm::Rsa { .. });
    let is_recorded = |offered: &Algorithm| {
        recorded
            .iter()
            .any(|r| if rsa(r) { rsa(offered) } else { r == offered })
    };
    // Stable, so the default order holds within each group.
    keys.sort_by_key(|a| !is_recorded(a));
    russh::Preferred {
        host_key_certificates: if certificates {
            Cow::Owned(keys.clone())
        } else {
            Cow::Borrowed(&[])
        },
        key: Cow::Owned(keys),
        ..russh::Preferred::default()
    }
}

/// The `authorized_keys` line of an OpenSSH private key. An encrypted key
/// runs its KDF to decode (about 0.2 s), so call this off the async workers.
pub fn public_key_line(pem: &str, passphrase: Option<&str>) -> Result<String, String> {
    let key = russh::keys::decode_secret_key(pem, passphrase).map_err(|e| e.to_string())?;
    key.public_key().to_openssh().map_err(|e| e.to_string())
}

fn protocol(error: impl std::fmt::Display) -> ConnectError {
    ConnectError::Protocol(error.to_string())
}

/// The russh client handler for one hop: it decides about host keys, and on
/// the last hop serves forwarded agent channels when the host allows it.
pub(crate) struct Client {
    /// Set only on the last hop of a host that forwards the agent; any
    /// other agent channel a server opens is refused.
    forward: Option<forward::AgentForward>,
    host: String,
    port: u16,
    known: Arc<KnownHosts>,
    host_keys: HostKeyStore,
    prompter: Arc<dyn Prompter>,
    /// Signalled when the host key check starts.
    reached: Arc<Notify>,
    /// Closed when the connect is abandoned.
    alive: watch::Receiver<()>,
}

/// Errors from inside a handshake: russh's own, or our host key decision.
#[derive(Debug)]
pub(crate) enum HandlerError {
    Russh(russh::Error),
    HostKey(ConnectError),
}

impl From<russh::Error> for HandlerError {
    fn from(error: russh::Error) -> Self {
        Self::Russh(error)
    }
}

impl HandlerError {
    fn into_connect_error(self) -> ConnectError {
        match self {
            Self::HostKey(error) => error,
            Self::Russh(russh::Error::IO(error)) => ConnectError::Io(error),
            Self::Russh(error) => protocol(error),
        }
    }
}

impl client::Handler for Client {
    type Error = HandlerError;

    async fn check_server_key(
        &mut self,
        presented: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        self.reached.notify_one();
        let presented = match presented {
            PublicKeyOrCertificate::PublicKey { key, .. } => host_keys::Presented::Key(key),
            PublicKeyOrCertificate::Certificate(cert) => host_keys::Presented::Certificate(cert),
        };
        let check = self.host_keys.check(
            &self.known,
            &self.host,
            self.port,
            &presented,
            &self.prompter,
        );
        tokio::select! {
            result = check => result.map(|()| true).map_err(HandlerError::HostKey),
            _ = self.alive.changed() => Err(HandlerError::HostKey(ConnectError::Cancelled)),
        }
    }

    async fn server_channel_open_agent_forward(
        &mut self,
        channel: russh::Channel<client::Msg>,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let slot = self
            .forward
            .as_ref()
            .and_then(forward::AgentForward::reserve);
        match (&self.forward, slot) {
            (Some(forward), Some(slot)) => {
                reply.accept().await;
                forward.relay(channel, slot);
            }
            (Some(_), None) => {
                tracing::warn!(host = %self.host, "refusing an agent channel past the limit");
                reply
                    .reject(russh::ChannelOpenFailure::ResourceShortage)
                    .await;
            }
            (None, _) => {
                tracing::warn!(host = %self.host, "refusing an agent channel the host did not get");
                reply
                    .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                    .await;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
