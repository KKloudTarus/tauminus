// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Ports between the use cases and the outside world.
//!
//! A terminal stream is two bounded channels plus a terminator, not an async
//! trait object. Writing to a PTY or an SSH channel can block, so each adapter
//! owns its I/O on its own thread or task, and the use case talks to it by
//! message. Bounded channels carry backpressure in both directions.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tauminus_domain::{Forward, LocalShell, SessionOptions, SshTarget, TerminalSize};
use thiserror::Error;
use tokio::sync::{mpsc, watch};
use zeroize::Zeroizing;

pub use crate::vault::Credentials;

/// Messages from the use case to a terminal stream, applied in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalInput {
    /// Text the user typed or pasted. A stream whose shell speaks another
    /// character set converts it; the rest send it as UTF-8.
    Text(String),
    /// Bytes as they are, such as legacy mouse reports.
    Data(Vec<u8>),
    Resize(TerminalSize),
}

/// Messages from a terminal stream to the use case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    Output(Vec<u8>),
    /// The remote side ended. `code` is `None` when the platform reports no
    /// exit status, for example after a signal.
    Exited {
        code: Option<u32>,
    },
    /// The connection was lost before the remote side ended: the shell
    /// may still be running there, and a new connection can reach it.
    Lost,
}

/// Ends a terminal stream from outside its input queue.
///
/// Termination cannot wait behind queued input: when the remote process stops
/// reading, the writer blocks and the queue stops moving. Implementations act
/// on the process or channel directly, and both methods must be idempotent and
/// safe to call after the stream has ended.
pub trait Terminator: Send + Sync + 'static {
    /// Asks the remote side to end, as closing a terminal window does.
    fn terminate(&self);

    /// Ends the remote side forcibly, for when `terminate` was ignored.
    fn kill(&self) {
        self.terminate();
    }

    /// Checks that the link still answers, as after a wake or a network
    /// change; one that does not ends the stream with
    /// [`TerminalEvent::Lost`]. A stream with no link to check ignores it.
    fn probe(&self) {}
}

/// One interactive byte stream: a local shell on a PTY, or a shell channel on
/// an SSH connection.
pub struct TerminalStream {
    pub input: mpsc::Sender<TerminalInput>,
    pub events: mpsc::Receiver<TerminalEvent>,
    pub terminator: Box<dyn Terminator>,
}

impl fmt::Debug for TerminalStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalStream")
            .field("input", &self.input)
            .field("events", &self.events)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error)]
pub enum LaunchError {
    #[error("failed to start {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Starts local shells.
pub trait PtyLauncher: Send + Sync + 'static {
    fn launch(&self, shell: &LocalShell, size: TerminalSize)
    -> Result<TerminalStream, LaunchError>;
}

/// The view closed its end of the output stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("output sink closed")]
pub struct SinkClosed;

/// Where a session's output goes, usually an IPC channel to the terminal view.
pub trait OutputSink: Send + Sync + 'static {
    fn send_output(&self, batch: Vec<u8>) -> Result<(), SinkClosed>;
    fn send_exit(&self, code: Option<u32>);
    /// The connection was lost; the view may connect again.
    fn send_lost(&self) {
        self.send_exit(None);
    }
}

/// A future the adapter returns from an object-safe trait. SSH connects and
/// prompts happen once per session, so the box costs nothing that matters.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A string that is wiped from memory when dropped: passwords, passphrases,
/// one-time codes.
pub type Secret = Zeroizing<String>;

/// One field of a keyboard-interactive challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChallengeField {
    pub text: String,
    /// False for secrets such as passwords and one-time codes.
    pub echo: bool,
}

/// Something only the user can decide or supply during a connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// Nothing about the server's key is on record. `certificate_expected`
    /// warns that a `@cert-authority` covers the host but it presented no
    /// certificate that checks out, as an interceptor would.
    UnknownHostKey {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
        certificate_expected: bool,
    },
    Password {
        endpoint: String,
    },
    Passphrase {
        key_path: String,
    },
    KeyboardInteractive {
        endpoint: String,
        name: String,
        instructions: String,
        fields: Vec<ChallengeField>,
    },
}

#[derive(Debug)]
pub enum Answer {
    /// Trust the host key; `remember` records it in Tauminus's `known_hosts`.
    TrustHostKey {
        remember: bool,
    },
    Secret(Secret),
    Responses(Vec<Secret>),
    /// The user declined, closed the dialog, or did not answer in time.
    Cancel,
}

/// Asks the user. Implementations must answer `Cancel` rather than wait
/// forever.
pub trait Prompter: Send + Sync + 'static {
    fn ask(&self, prompt: Prompt) -> BoxFuture<'_, Answer>;
}

/// Why an SSH session could not start. Each variant says enough for the UI
/// to explain the cause and suggest a fix.
#[derive(Debug, Error)]
pub enum ConnectError {
    #[error("could not resolve {host}")]
    Resolve { host: String },
    #[error("{host}:{port} refused the connection")]
    Refused { host: String, port: u16 },
    #[error("{host}:{port} did not answer within {seconds} s")]
    Timeout {
        host: String,
        port: u16,
        seconds: u64,
    },
    #[error("proxy failed: {0}")]
    Proxy(String),
    #[error(
        "the host key for {host}:{port} has changed: the server presented {fingerprint}, \
         which does not match {recorded}; this can mean someone is intercepting the connection"
    )]
    HostKeyChanged {
        host: String,
        port: u16,
        fingerprint: String,
        recorded: String,
    },
    #[error("the host key for {host}:{port} is revoked ({recorded})")]
    HostKeyRevoked {
        host: String,
        port: u16,
        recorded: String,
    },
    #[error("the host key for {host}:{port} was not trusted")]
    HostKeyRejected { host: String, port: u16 },
    #[error("authentication to {endpoint} failed after trying {}", tried.join(", "))]
    AuthFailed {
        endpoint: String,
        tried: Vec<String>,
    },
    #[error("cancelled")]
    Cancelled,
    #[error("SSH protocol error: {0}")]
    Protocol(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// How a script run with [`SshConnector::run_script`] ended.
#[derive(Clone, PartialEq, Eq)]
pub struct ScriptOutcome {
    /// `None` when the server sent no exit status.
    pub status: Option<u32>,
    /// Standard output and error, at most 64 KiB.
    pub output: String,
    /// The address the host was reached at, when straight over TCP.
    pub peer: Option<std::net::IpAddr>,
}

impl fmt::Debug for ScriptOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The output may hold a secret, as Mosh's session key.
        f.debug_struct("ScriptOutcome")
            .field("status", &self.status)
            .field("output_bytes", &self.output.len())
            .field("peer", &self.peer)
            .finish()
    }
}

/// How a running forward is doing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForwardStatus {
    /// Connections it carries now.
    pub open: usize,
    /// Connections it has carried since it started.
    pub total: u64,
    /// Of those, how many were reset because their client fell too far
    /// behind or stopped reading.
    pub resets: u64,
    /// Why it stopped on its own, such as the connection dropping. `None`
    /// while it runs.
    pub ended: Option<String>,
}

/// A running forward. Dropping it stops the listener and closes every
/// connection it carries.
#[derive(Debug)]
pub struct ForwardHandle {
    port: u16,
    status: watch::Receiver<ForwardStatus>,
    stop: watch::Sender<()>,
    finished: watch::Receiver<()>,
}

impl ForwardHandle {
    /// `stop` is dropped with the handle, which the adapter's tasks watch.
    /// The senders of `finished` live in the tasks that hold the listener
    /// (or, for a remote rule, until the server cancelled it), so it closes
    /// once the port is free again.
    pub fn new(
        port: u16,
        status: watch::Receiver<ForwardStatus>,
        stop: watch::Sender<()>,
        finished: watch::Receiver<()>,
    ) -> Self {
        Self {
            port,
            status,
            stop,
            finished,
        }
    }

    /// Stops the forward and waits, at most `limit`, until its port is
    /// free, so the same rule can listen there again at once.
    pub async fn stop(self, limit: std::time::Duration) {
        let Self {
            stop, mut finished, ..
        } = self;
        drop(stop);
        let _ =
            tokio::time::timeout(limit, async { while finished.changed().await.is_ok() {} }).await;
    }

    /// The port listening: the rule's, or the one the OS or the server
    /// picked when the rule asked for 0.
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn status(&self) -> ForwardStatus {
        self.status.borrow().clone()
    }

    /// A receiver that sees every change, for the view.
    pub fn watch(&self) -> watch::Receiver<ForwardStatus> {
        self.status.clone()
    }
}

#[derive(Debug, Error)]
pub enum TunnelError {
    #[error("could not listen on {address}: {source}")]
    Bind {
        address: String,
        #[source]
        source: std::io::Error,
    },
    #[error("the server refused to listen on {0}")]
    Denied(String),
    #[error("the connection is closed")]
    Closed,
}

/// One login to a host, shared by the shell, SFTP, and forwards that use
/// it. It closes when the last of them drops it.
pub trait SshConnection: Send + Sync + 'static {
    /// Opens a shell with a PTY. Agent forwarding is decided when the
    /// connection is made, not here.
    fn open_shell<'a>(
        &'a self,
        size: TerminalSize,
        options: &'a SessionOptions,
    ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>>;

    fn start_forward<'a>(
        &'a self,
        forward: &'a Forward,
    ) -> BoxFuture<'a, Result<ForwardHandle, TunnelError>>;

    /// Starts the SFTP subsystem on a channel of its own.
    fn open_sftp(&self) -> BoxFuture<'_, Result<Arc<dyn SftpPort>, ConnectError>>;

    /// False once the session has ended.
    fn is_open(&self) -> bool;

    /// Resolves when the session ends, at once if it already has.
    fn closed(&self) -> BoxFuture<'_, ()>;
}

/// Opens SSH shell sessions.
pub trait SshConnector: Send + Sync + 'static {
    /// Signs in on every hop and returns the connection, with agent
    /// forwarding served on it when `forward_agent` is set.
    fn connection<'a>(
        &'a self,
        target: &'a SshTarget,
        prompter: Arc<dyn Prompter>,
        credentials: Arc<Credentials>,
        forward_agent: bool,
    ) -> BoxFuture<'a, Result<Arc<dyn SshConnection>, ConnectError>>;

    /// Connects with `credentials` from a vault identity, used on the last
    /// hop before any key file or prompt, and opens a shell on a connection
    /// of its own.
    fn connect_with<'a>(
        &'a self,
        target: &'a SshTarget,
        size: TerminalSize,
        prompter: Arc<dyn Prompter>,
        credentials: Arc<Credentials>,
    ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>> {
        Box::pin(async move {
            let connection = self
                .connection(target, prompter, credentials, target.session.forward_agent)
                .await?;
            connection.open_shell(size, &target.session).await
        })
    }

    /// Runs `script` with `sh -s` on the target, without a PTY or agent
    /// forwarding, signing in as a connect would.
    fn run_script<'a>(
        &'a self,
        target: &'a SshTarget,
        prompter: Arc<dyn Prompter>,
        credentials: Arc<Credentials>,
        script: &'a str,
    ) -> BoxFuture<'a, Result<ScriptOutcome, ConnectError>>;

    /// Connects with no identity.
    fn connect<'a>(
        &'a self,
        target: &'a SshTarget,
        size: TerminalSize,
        prompter: Arc<dyn Prompter>,
    ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>> {
        self.connect_with(target, size, prompter, Arc::default())
    }
}

/// Why the saved hosts could not be read or written.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The file exists but is not a host list this version understands.
    #[error("the saved hosts file is unreadable: {0}")]
    Corrupt(String),
    /// Someone else, such as another Tauminus window, changed the file since
    /// it was read. Nothing was written.
    #[error("the saved hosts changed in another window; the list was reloaded, try again")]
    Conflict,
}

/// The system's serial ports: what is attached, and a session on one.
pub trait SerialPorts: Send + Sync + 'static {
    /// Ports the system lists now. Blocking; at most a few hundred.
    fn list(&self) -> Vec<tauminus_domain::SerialPortInfo>;
    /// Opens a port with its line settings. Blocking.
    fn open(
        &self,
        settings: &tauminus_domain::SerialSettings,
    ) -> Result<TerminalStream, LaunchError>;
}

/// Opens Telnet sessions: a TCP connection and the Telnet protocol over
/// it, converting the host's character set.
pub trait TelnetConnector: Send + Sync + 'static {
    fn connect<'a>(
        &'a self,
        target: &'a tauminus_domain::TelnetTarget,
        size: TerminalSize,
    ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>>;
}

/// Hears a session's output as the view gets it, and its size changes; for
/// recording. Called on the session's pump: it must never wait.
pub trait OutputTap: Send + Sync + 'static {
    fn output(&self, bytes: &[u8]);
    fn resize(&self, size: TerminalSize);
    /// The session ended, or the tap was taken off.
    fn end(&self);
}

/// Starts the local `mosh-client` toward a server the bootstrap started
/// on `host`, on a PTY of its own.
pub trait MoshClient: Send + Sync + 'static {
    /// Fails when no client can run here, before any sign-in.
    fn ready(&self) -> Result<(), LaunchError> {
        Ok(())
    }

    /// `address` is where SSH reached the host, which `mosh-server -s`
    /// listens on; a second lookup of the name could answer otherwise.
    fn launch<'a>(
        &'a self,
        address: std::net::IpAddr,
        connect: &'a tauminus_domain::MoshConnect,
        size: TerminalSize,
    ) -> BoxFuture<'a, Result<TerminalStream, LaunchError>>;
}

/// Keeps the saved hosts. `save` replaces everything at once, so a crash
/// leaves either the old list or the new one, never a mix.
pub trait HostStore: Send + Sync + 'static {
    /// An empty inventory when nothing was saved yet.
    fn load(&self) -> BoxFuture<'_, Result<tauminus_domain::Inventory, StoreError>>;
    fn save<'a>(
        &'a self,
        inventory: &'a tauminus_domain::Inventory,
    ) -> BoxFuture<'a, Result<(), StoreError>>;
    /// A fresh, unguessable id for a new host or group.
    fn new_id(&self) -> String;
}

/// Keeps the saved snippets, replacing everything at once as
/// [`HostStore`] does.
pub trait SnippetStore: Send + Sync + 'static {
    /// An empty library when nothing was saved yet.
    fn load(&self) -> BoxFuture<'_, Result<tauminus_domain::Library, StoreError>>;
    fn save<'a>(
        &'a self,
        library: &'a tauminus_domain::Library,
    ) -> BoxFuture<'a, Result<(), StoreError>>;
    /// A fresh, unguessable id for a new snippet.
    fn new_id(&self) -> String;
}

/// A host as `~/.ssh/config` describes it, before validation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigHost {
    /// The `Host` alias, which becomes the label.
    pub alias: String,
    /// `HostName`, or the alias when none is set.
    pub hostname: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    /// `ProxyJump`, as written.
    pub proxy_jump: Option<String>,
    pub proxy_command: Option<String>,
    pub identity_files: Vec<std::path::PathBuf>,
    pub identities_only: bool,
    /// `ForwardAgent`; a socket path counts as yes.
    pub forward_agent: Option<bool>,
    /// `SetEnv`, as `NAME=value` pairs in order.
    pub set_env: Vec<(String, String)>,
    /// Why this alias could not be read, for example OpenSSH rejecting the
    /// config. Such an entry is reported, not imported.
    pub problem: Option<String>,
}

/// Reads the hosts an OpenSSH client config defines.
pub trait SshConfigSource: Send + Sync + 'static {
    fn hosts(&self) -> BoxFuture<'_, Result<Vec<ConfigHost>, std::io::Error>>;
}

/// What a directory entry on the server is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Link,
    Other,
}

/// One entry of a remote directory, as the server describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    /// As the server sent it: check it with `tauminus_domain::local_name`
    /// before it names anything on this machine.
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Seconds since the epoch.
    pub modified: Option<u64>,
    /// Permission bits, as `chmod` takes them.
    pub mode: Option<u32>,
}

#[derive(Debug, Error)]
pub enum SftpError {
    /// The server refused, with its own words.
    #[error("the server said: {0}")]
    Server(String),
    #[error("the directory has more than {0} entries")]
    TooMany(usize),
    #[error("{0}")]
    Local(#[from] std::io::Error),
    /// The source changed size while it was copied.
    #[error("the file changed while it was copied")]
    Changed,
    #[error("{0} already exists")]
    Exists(String),
    #[error("{0} is larger than allowed here")]
    TooLarge(String),
    #[error("cancelled")]
    Cancelled,
    #[error("the connection is closed")]
    Closed,
}

/// How far a transfer has come.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransferProgress {
    /// Bytes in place at the destination, counting a resumed start.
    pub done: u64,
    pub total: u64,
    /// Where it resumed, or 0 when it started from the beginning.
    pub resumed_from: u64,
}

/// A file to copy from the server. `local` is the final path; a partial
/// copy waits beside it until it is complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub remote: String,
    pub local: std::path::PathBuf,
    /// The saved host, recorded with a partial copy so it only resumes
    /// against the same file.
    pub host: String,
    pub overwrite: bool,
    /// Continue a partial copy of the same file; off, it always starts over.
    pub resume: bool,
    /// Refuse a file larger than this.
    pub max: Option<u64>,
}

/// A file to copy to the server. It is written beside `remote` under a
/// temporary name and renamed over it once complete; `state` is a local
/// directory that keeps what a resume needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    pub local: std::path::PathBuf,
    pub remote: String,
    pub host: String,
    pub overwrite: bool,
    pub state: std::path::PathBuf,
    /// Continue a partial upload of the same file; off, it starts over.
    pub resume: bool,
}

/// Files on one server, over the SFTP subsystem of a connection.
pub trait SftpPort: Send + Sync + 'static {
    /// At most 100 000 entries, without `.` and `..`.
    fn list<'a>(&'a self, dir: &'a str) -> BoxFuture<'a, Result<Vec<RemoteEntry>, SftpError>>;
    /// Follows links; `name` is the last component.
    fn stat<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<RemoteEntry, SftpError>>;
    /// The absolute form of `path`; `.` is the login directory.
    fn canonicalize<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String, SftpError>>;
    fn mkdir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>>;
    fn remove_file<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>>;
    fn remove_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>>;
    /// Fails when `to` exists: renaming never replaces a file.
    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFuture<'a, Result<(), SftpError>>;
    fn chmod<'a>(&'a self, path: &'a str, mode: u32) -> BoxFuture<'a, Result<(), SftpError>>;
    /// Copies a file here, resuming a partial copy of the same file.
    /// Returns the bytes moved this time.
    fn download<'a>(
        &'a self,
        job: &'a Download,
        progress: watch::Sender<TransferProgress>,
        cancel: watch::Receiver<()>,
    ) -> BoxFuture<'a, Result<u64, SftpError>>;
    /// Copies a file there, resuming a partial upload of the same file.
    fn upload<'a>(
        &'a self,
        job: &'a Upload,
        progress: watch::Sender<TransferProgress>,
        cancel: watch::Receiver<()>,
    ) -> BoxFuture<'a, Result<u64, SftpError>>;
}
