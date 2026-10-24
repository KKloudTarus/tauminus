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
mod connection;
mod forward;
mod host_keys;
pub mod known_hosts;
mod pipe;
mod rejected;
mod sftp;
mod socks;
mod transport;
mod tunnel;
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
use russh::keys::{Algorithm, PrivateKey, PublicKeyOrCertificate};
use tauminus_app::ports::{
    BoxFuture, ConnectError, Credentials, Prompter, ScriptOutcome, SshConnection, SshConnector,
};
use tauminus_domain::{Endpoint, KeyKind, SshTarget};
use tokio::sync::{Notify, watch};

pub use auth::AgentSource;
pub use config::OpenSshConfig;
pub use host_keys::HostKeyStore;

use crate::connection::{Alarm, Connection};
use crate::known_hosts::KnownHosts;
use crate::tunnel::Routes;

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
    /// Bytes a connection's forwards may hold unread, and how long one
    /// write to a local peer may make no progress.
    forward_budget: usize,
    forward_stall: Duration,
    /// Shared by every connect, so a password one host turned down is not
    /// sent to the next.
    rejected: Arc<rejected::RejectedPasswords>,
}

impl RusshConnector {
    /// `home` locates default key files and expands `~` in identity paths.
    pub fn new(host_keys: HostKeyStore, home: Option<PathBuf>, agent: AgentSource) -> Self {
        Self {
            host_keys,
            home,
            agent,
            connect_timeout: CONNECT_TIMEOUT,
            forward_budget: pipe::BUDGET,
            forward_stall: pipe::STALL,
            rejected: Arc::default(),
        }
    }

    /// Replaces the 15 s limit on each TCP connect, proxy handshake, and SSH
    /// handshake up to the host key check.
    #[must_use]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Replaces how much forwarded data a connection may hold for local
    /// peers that read slowly (64 MiB), and how long one write to such a
    /// peer may make no progress (30 s), past which it is reset.
    #[must_use]
    pub fn with_forward_limits(mut self, budget: usize, stall: Duration) -> Self {
        self.forward_budget = budget;
        self.forward_stall = stall;
        self
    }

    /// Connects every hop in order and signs in on each; the last hop
    /// gets `credentials`.
    async fn connect(
        &self,
        target: &SshTarget,
        prompter: Arc<dyn Prompter>,
        credentials: Arc<Credentials>,
        forward_agent: bool,
    ) -> Result<Arc<Connection>, ConnectError> {
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
        let (alarm, closed) = watch::channel(false);
        let alarm = Arc::new(alarm);
        let routes = Arc::new(Routes::default());
        // The address the destination was reached at, when it was reached
        // straight over TCP.
        let mut peer = None;
        for (index, hop) in hops.iter().enumerate() {
            let io = match connections.last() {
                None => {
                    let (io, reached) =
                        transport::open(&target.route, hop, self.connect_timeout).await?;
                    if hops.len() == 1 {
                        peer = reached;
                    }
                    io
                }
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
            let goodbye = Arc::new(std::sync::OnceLock::new());
            let reached = Arc::new(Notify::new());
            let last = index + 1 == hops.len();
            let handler = Client {
                forward: (last && forward_agent)
                    .then(|| forward::AgentForward::new(self.agent.clone())),
                routes: last.then(|| Arc::clone(&routes)),
                _alarm: Alarm(Arc::clone(&alarm)),
                host: hop.host.to_string(),
                port: hop.port,
                known,
                host_keys: self.host_keys.clone(),
                prompter: Arc::clone(&prompter),
                reached: Arc::clone(&reached),
                alive: alive.clone(),
                goodbye: Arc::clone(&goodbye),
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
                rejected: &self.rejected,
                goodbye: &goodbye,
                turn: None,
                stored_last: false,
                skipped_fido: false,
            }
            .run()
            .await
            .map_err(|error| match (error, goodbye.get()) {
                // However the sign-in noticed the link gone, a server that
                // said goodbye first refused.
                (ConnectError::Io(_), Some(said)) => {
                    ConnectError::Protocol(format!("the server closed the connection: {said}"))
                }
                (error, _) => error,
            })?;
            tracing::debug!(hop = %hop, index, "authenticated");
            connections.push(handle);
        }
        Ok(Connection::new(
            connections,
            closed,
            routes,
            forward_agent,
            pipe::Ledger::new(self.forward_budget, self.forward_stall),
            peer,
        ))
    }

    /// Runs `script` with `sh -s` on the last hop, without a PTY, and
    /// returns its exit status and up to 64 KiB of its output. For one-off
    /// jobs such as adding a key to `authorized_keys`.
    async fn run(
        &self,
        target: &SshTarget,
        prompter: Arc<dyn Prompter>,
        credentials: Arc<Credentials>,
        script: &str,
    ) -> Result<ScriptOutcome, ConnectError> {
        const MAX_OUTPUT: usize = 64 * 1024;
        /// A script that has not finished by then never will; a server that
        /// keeps the channel open must not hold the caller forever.
        const SCRIPT_TIMEOUT: Duration = Duration::from_mins(1);
        let connection = self.connect(target, prompter, credentials, false).await?;
        let last = connection.last();
        // One bound over everything after sign-in: a live server can stall
        // the channel open, the exec, or the output, and keepalives would
        // keep it from ever timing out.
        let exchange = async {
            let mut channel = last.channel_open_session().await.map_err(protocol)?;
            // `sh` whatever the login shell is, with the script on stdin, so
            // nothing in it is quoted twice.
            channel.exec(true, "exec sh -s").await.map_err(protocol)?;
            channel.data(script.as_bytes()).await.map_err(protocol)?;
            channel.eof().await.map_err(protocol)?;
            let mut output = Vec::new();
            let mut status = None;
            let mut eof = false;
            while let Some(message) = channel.wait().await {
                match message {
                    russh::ChannelMsg::Data { data }
                    | russh::ChannelMsg::ExtendedData { data, .. } => {
                        // Past the cap the rest is dropped, but still read,
                        // so the exit status arrives.
                        let room = MAX_OUTPUT.saturating_sub(output.len());
                        output.extend_from_slice(&data[..data.len().min(room)]);
                    }
                    russh::ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                    russh::ChannelMsg::Eof => eof = true,
                    russh::ChannelMsg::Close => break,
                    _ => {}
                }
                // A child left running in the background can hold the
                // channel open; the script itself is done.
                if eof && status.is_some() {
                    break;
                }
            }
            let _ = channel.close().await;
            Ok::<_, ConnectError>((output, status))
        };
        let (output, status) = tokio::time::timeout(SCRIPT_TIMEOUT, exchange)
            .await
            .map_err(|_| {
                ConnectError::Protocol(format!(
                    "the script did not finish within {} s",
                    SCRIPT_TIMEOUT.as_secs()
                ))
            })??;
        Ok(ScriptOutcome {
            status,
            output: String::from_utf8_lossy(&output).into_owned(),
            peer: connection.peer(),
        })
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
    fn connection<'a>(
        &'a self,
        target: &'a SshTarget,
        prompter: Arc<dyn Prompter>,
        credentials: Arc<Credentials>,
        forward_agent: bool,
    ) -> BoxFuture<'a, Result<Arc<dyn SshConnection>, ConnectError>> {
        Box::pin(async move {
            let connection = self
                .connect(target, prompter, credentials, forward_agent)
                .await?;
            Ok(connection as Arc<dyn SshConnection>)
        })
    }

    fn run_script<'a>(
        &'a self,
        target: &'a SshTarget,
        prompter: Arc<dyn Prompter>,
        credentials: Arc<Credentials>,
        script: &'a str,
    ) -> BoxFuture<'a, Result<ScriptOutcome, ConnectError>> {
        Box::pin(self.run(target, prompter, credentials, script))
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
        // A quiet link is checked every 15 s and given up after about a
        // minute: each keepalive wakes the radio, so they stay rare. A wake
        // or a network change asks for a check at once instead (a probe).
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 3,
        inactivity_timeout: None,
        channel_buffer_size: CHANNEL_BUFFER,
        // The default 2 MiB caps one transfer at 2 MiB per round trip,
        // 19 MiB/s at 50 ms; 16 MiB carries a gigabit at 50 ms. russh grants
        // window on receipt, so this bounds no memory; the queue above does.
        window_size: 16 * 1024 * 1024,
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

/// A new key, as an OpenSSH private key (unencrypted: the vault encrypts
/// it) and its `authorized_keys` line. RSA takes a few seconds, so call this
/// off the async workers.
pub fn generate_key(kind: KeyKind, comment: &str) -> Result<GeneratedKey, String> {
    use russh::keys::ssh_key::private::{KeypairData, RsaKeypair};
    use russh::keys::ssh_key::{EcdsaCurve, LineEnding};
    let mut rng = rand::rng();
    let ecdsa = |curve| Algorithm::Ecdsa { curve };
    let mut key = match kind {
        KeyKind::Ed25519 => PrivateKey::random(&mut rng, Algorithm::Ed25519),
        KeyKind::EcdsaP256 => PrivateKey::random(&mut rng, ecdsa(EcdsaCurve::NistP256)),
        KeyKind::EcdsaP384 => PrivateKey::random(&mut rng, ecdsa(EcdsaCurve::NistP384)),
        KeyKind::EcdsaP521 => PrivateKey::random(&mut rng, ecdsa(EcdsaCurve::NistP521)),
        KeyKind::Rsa3072 | KeyKind::Rsa4096 => {
            let bits = if kind == KeyKind::Rsa3072 { 3072 } else { 4096 };
            RsaKeypair::random(&mut rng, bits)
                .and_then(|pair| PrivateKey::new(KeypairData::from(pair), ""))
        }
    }
    .map_err(|e| e.to_string())?;
    key.set_comment(comment);
    let private_key = key.to_openssh(LineEnding::LF).map_err(|e| e.to_string())?;
    let public_key = key.public_key().to_openssh().map_err(|e| e.to_string())?;
    Ok(GeneratedKey {
        private_key,
        public_key,
    })
}

/// A key [`generate_key`] made. The private key is wiped when dropped.
pub struct GeneratedKey {
    pub private_key: zeroize::Zeroizing<String>,
    pub public_key: String,
}

impl std::fmt::Debug for GeneratedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeneratedKey")
            .field("public_key", &self.public_key)
            .finish_non_exhaustive()
    }
}

fn protocol(error: impl std::fmt::Display) -> ConnectError {
    ConnectError::Protocol(error.to_string())
}

/// The russh client handler for one hop: it decides about host keys, and on
/// the last hop serves forwarded agent channels when the host allows it and
/// remote forwards that are running. Every other channel a server opens is
/// refused.
pub(crate) struct Client {
    /// Set only on the last hop of a host that forwards the agent; any
    /// other agent channel a server opens is refused.
    forward: Option<forward::AgentForward>,
    /// Set only on the last hop.
    routes: Option<Arc<Routes>>,
    /// Marks the connection closed when russh drops this handler.
    _alarm: Alarm,
    host: String,
    port: u16,
    known: Arc<KnownHosts>,
    host_keys: HostKeyStore,
    prompter: Arc<dyn Prompter>,
    /// Signalled when the host key check starts.
    reached: Arc<Notify>,
    /// Closed when the connect is abandoned.
    alive: watch::Receiver<()>,
    /// What the server said when it closed the connection itself, as
    /// after too many failed sign-in tries; a network drop says nothing.
    goodbye: Arc<std::sync::OnceLock<String>>,
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

/// The link went away (as a server at its `MaxStartups`, one restarting,
/// or Wi-Fi dropping mid-login does), which is worth trying again, or the
/// server broke the protocol, which is not.
pub(crate) fn link_or_protocol(error: russh::Error) -> ConnectError {
    match error {
        russh::Error::IO(error) => ConnectError::Io(error),
        error @ (russh::Error::Disconnect
        | russh::Error::SendError
        | russh::Error::RecvError
        | russh::Error::ConnectionTimeout
        | russh::Error::KeepaliveTimeout
        | russh::Error::InactivityTimeout) => ConnectError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            error.to_string(),
        )),
        error => protocol(error),
    }
}

impl HandlerError {
    fn into_connect_error(self) -> ConnectError {
        match self {
            Self::HostKey(error) => error,
            Self::Russh(error) => link_or_protocol(error),
        }
    }
}

/// Unicode format characters (bidi overrides, zero-width marks), which
/// would let a server reorder how its message reads.
fn is_format(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}

impl client::Handler for Client {
    type Error = HandlerError;

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the trait is async; this answers at once"
    )]
    async fn disconnected(
        &mut self,
        reason: client::DisconnectReason<Self::Error>,
    ) -> Result<(), Self::Error> {
        match reason {
            client::DisconnectReason::ReceivedDisconnect(info) => {
                // Shown to the user: printable, and short.
                let said: String = info
                    .message
                    .chars()
                    .filter(|c| !c.is_control() && !is_format(*c))
                    .take(200)
                    .collect();
                let _ = self.goodbye.set(said);
                Ok(())
            }
            client::DisconnectReason::Error(error) => Err(error),
        }
    }

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

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<client::Msg>,
        connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        match &self.routes {
            Some(routes) => routes.dispatch(connected_address, connected_port, channel, reply),
            None => refuse(reply, &self.host, "forwarded-tcpip").await,
        }
        Ok(())
    }

    async fn server_channel_open_forwarded_streamlocal(
        &mut self,
        _channel: russh::Channel<client::Msg>,
        _socket_path: &str,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        refuse(reply, &self.host, "forwarded-streamlocal").await;
        Ok(())
    }

    async fn server_channel_open_session(
        &mut self,
        _channel: russh::Channel<client::Msg>,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        refuse(reply, &self.host, "session").await;
        Ok(())
    }

    async fn server_channel_open_direct_tcpip(
        &mut self,
        _channel: russh::Channel<client::Msg>,
        _host_to_connect: &str,
        _port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        refuse(reply, &self.host, "direct-tcpip").await;
        Ok(())
    }

    async fn server_channel_open_direct_streamlocal(
        &mut self,
        _channel: russh::Channel<client::Msg>,
        _socket_path: &str,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        refuse(reply, &self.host, "direct-streamlocal").await;
        Ok(())
    }

    async fn server_channel_open_x11(
        &mut self,
        _channel: russh::Channel<client::Msg>,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        refuse(reply, &self.host, "x11").await;
        Ok(())
    }
}

/// Refuses a channel the server opened that nothing here asked for.
/// russh's defaults would accept it and keep it open with no reader.
async fn refuse(reply: client::ChannelOpenHandle, host: &str, kind: &str) {
    tracing::warn!(host, kind, "refusing a channel the server opened");
    reply
        .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
        .await;
}

#[cfg(test)]
mod tests;
