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
#[derive(Clone, PartialEq, Eq)]
pub enum TerminalInput {
    /// Text the user typed or pasted. A stream whose shell speaks another
    /// character set converts it; the rest send it as UTF-8.
    Text(String),
    /// Bytes as they are, such as legacy mouse reports.
    Data(Vec<u8>),
    /// A password from the vault, typed as text is; wiped once sent.
    Secret(Secret),
    Resize(TerminalSize),
}

/// Shows a secret as `Secret(..)`, so no log or panic prints it.
impl fmt::Debug for TerminalInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(text) => f.debug_tuple("Text").field(text).finish(),
            Self::Data(bytes) => f.debug_tuple("Data").field(bytes).finish(),
            Self::Secret(_) => f.write_str("Secret(..)"),
            Self::Resize(size) => f.debug_tuple("Resize").field(size).finish(),
        }
    }
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
    /// The PIN of the FIDO2 security key the key file names.
    SecurityKeyPin {
        key_path: String,
    },
    /// Touch the FIDO2 security key: the key, not the user, ends this one,
    /// so it is withdrawn once the key answers; the user can only cancel.
    SecurityKeyTouch {
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
/// forever. Dropping the future withdraws the question: the user no
/// longer sees it.
pub trait Prompter: Send + Sync + 'static {
    fn ask(&self, prompt: Prompt) -> BoxFuture<'_, Answer>;
}

/// The methods an `AuthFailed` tried, or that none could be.
fn tried_text(tried: &[String]) -> String {
    if tried.is_empty() {
        "with nothing to offer: no key, agent key or password the server takes".to_owned()
    } else {
        format!("after trying {}", tried.join(", "))
    }
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
    #[error("authentication to {endpoint} failed {}", tried_text(tried))]
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

    /// Runs `command` on a channel with no PTY, its bytes passed as they
    /// are: a tmux control client (`tauminus_domain::TmuxCommand`).
    fn open_control<'a>(
        &'a self,
        command: &'a str,
    ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>>;

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

/// What sync carries: the saved hosts and groups, and the snippets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncData {
    pub hosts: tauminus_domain::Inventory,
    pub snippets: tauminus_domain::Library,
}

/// A field this device and another both changed, to different values:
/// the value kept and the one that lost, as text. A record deleted on one
/// device and edited on the other clashes on field `(deleted)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncClash {
    /// The record's name: a host's label, a group's or a snippet's name.
    pub record: String,
    pub field: String,
    pub kept: String,
    pub lost: String,
}

/// What a merge with the other devices gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub data: SyncData,
    /// What was moved to make the merged lists valid.
    pub repairs: Vec<tauminus_domain::Repair>,
    pub clashes: Vec<SyncClash>,
    /// What the target did that the user should know: an object that was
    /// not read, a device whose objects went back or away.
    pub notices: Vec<String>,
}

/// Why a sync did not happen.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SyncError {
    /// The target could not be read or written.
    #[error("the sync target: {0}")]
    Target(String),
    /// An object, or the key file, is not this sync key's.
    #[error("{0}")]
    Sealed(String),
    /// A device wrote with a newer Tauminus; syncing stops until this one
    /// is updated, so nothing it cannot read is lost.
    #[error("another device syncs with a newer Tauminus ({0}); update this one to sync")]
    Newer(String),
    /// The merged lists are not valid, and cannot be made so.
    #[error("the merged lists are not valid: {0}")]
    Invalid(String),
}

/// One of the lists a sync saves on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPart {
    Hosts,
    Snippets,
}

/// The other devices' state, through a sync target. `merge` reads and
/// merges; `adopt` records that a part of what it gave was saved here,
/// so the next sync compares against it; `publish` writes this device's
/// state for the others. They block on files and the network.
pub trait SyncRemote: Send + Sync + 'static {
    fn merge(&self, local: SyncData) -> BoxFuture<'_, Result<Merged, SyncError>>;
    /// Called once `part` of the last merge is saved here.
    fn adopt(&self, part: SyncPart) -> BoxFuture<'_, Result<(), SyncError>>;
    /// Writes the state adopted, if the target lacks it.
    fn publish(&self) -> BoxFuture<'_, Result<(), SyncError>>;
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

/// Keeps the highlight and trigger sets, replacing everything at once as
/// [`SnippetStore`] does.
pub trait TriggerStore: Send + Sync + 'static {
    /// An empty library when nothing was saved yet.
    fn load(&self) -> BoxFuture<'_, Result<tauminus_domain::TriggerLibrary, StoreError>>;
    fn save<'a>(
        &'a self,
        library: &'a tauminus_domain::TriggerLibrary,
    ) -> BoxFuture<'a, Result<(), StoreError>>;
    /// A fresh, unguessable id for a new set.
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
    /// `IdentityAgent` as written: `none`, `SSH_AUTH_SOCK`, a path, or a
    /// `$NAME` read when connecting, as ssh reads it, so a path that moves
    /// each login is never saved stale.
    pub identity_agent: Option<String>,
    /// `SetEnv`, as `NAME=value` pairs in order.
    pub set_env: Vec<(String, String)>,
    /// Why this alias could not be read, for example OpenSSH rejecting the
    /// config. Such an entry is reported, not imported.
    pub problem: Option<String>,
    /// The folder the other client kept it in, outermost first: it becomes
    /// groups inside the import's group.
    pub folder: Vec<String>,
}

/// Reads the hosts another client keeps: an OpenSSH config, or a file
/// another SSH client wrote.
pub trait SshConfigSource: Send + Sync + 'static {
    fn hosts(&self) -> BoxFuture<'_, Result<Vec<ConfigHost>, std::io::Error>>;

    /// The group its hosts go into.
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "a source may name itself from what it read"
    )]
    fn group(&self) -> &str {
        "SSH config"
    }

    /// What it reads, as an error names it.
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "a source may name itself from what it read"
    )]
    fn what(&self) -> &str {
        "~/.ssh/config"
    }
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

/// Where the AI assistant's settings are kept. Holds no secret: the key is
/// in the vault.
pub trait AiSettingsStore: Send + Sync + 'static {
    /// The defaults (off) when nothing was saved yet.
    fn load(&self) -> BoxFuture<'_, Result<crate::ai::AiSettings, StoreError>>;
    fn save<'a>(
        &'a self,
        settings: &'a crate::ai::AiSettings,
    ) -> BoxFuture<'a, Result<(), StoreError>>;
}

/// Why a model could not answer. Never holds the server's body, which may
/// echo the request.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AiTransportError {
    #[error("the AI server could not be reached: {0}")]
    Unreachable(String),
    #[error("the AI server took too long to answer")]
    Timeout,
    #[error("the AI server refused the key (HTTP {0})")]
    Unauthorized(u16),
    #[error("the AI server answered HTTP {0}")]
    Status(u16),
    #[error("the AI server's answer could not be read: {0}")]
    Malformed(String),
}

/// Asks a model one question.
pub trait AiTransport: Send + Sync + 'static {
    /// Posts `prompt` to `endpoint`, with `key` as the bearer token if
    /// given, and returns the answer's text.
    fn complete<'a>(
        &'a self,
        endpoint: &'a tauminus_domain::AiEndpoint,
        key: Option<&'a Secret>,
        prompt: &'a tauminus_domain::AiPrompt,
    ) -> BoxFuture<'a, Result<String, AiTransportError>>;
}

/// Where a session's file transfers ask the user and keep files (ADRs
/// 0025, 0026).
pub trait TransferHost: Send + Sync + fmt::Debug + 'static {
    /// The host behind session `id` offers a transfer, its offer number
    /// `offer`: to send files (`sz`) or to take them (`rz`). The answer
    /// comes back through `SessionManager::answer_transfer` with that
    /// number; until then the session shows nothing more and takes no
    /// keys.
    fn offered(&self, id: tauminus_domain::SessionId, offer: u64, wants: tauminus_domain::Wants);
    fn event(&self, id: tauminus_domain::SessionId, event: TransferEvent);
}

/// A folder the user chose to receive into. Only names a host sent,
/// checked, are joined to it.
pub trait ReceiveFolder: Send + fmt::Debug + 'static {
    /// A new file for `name`, never over one there: a taken name gets a
    /// number. The name it is saved as, and the file, written beside it
    /// under a temporary name until [`PartFile::finish`].
    fn create(&mut self, name: &str) -> std::io::Result<(String, Box<dyn PartFile>)>;
    /// The folder, as the user knows it.
    fn shown(&self) -> String;
}

/// A file being received.
pub trait PartFile: Send + 'static {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()>;
    /// Moves it to its name, never over a file there.
    fn finish(self: Box<Self>) -> std::io::Result<()>;
    /// Removes what was written.
    fn discard(self: Box<Self>);
}

/// A local file the user chose to send, opened: what it is was checked
/// on the open handle, and it is read through that handle only.
pub trait OutgoingFile: Send + fmt::Debug + 'static {
    /// One path component, as the host is to see it.
    fn name(&self) -> &str;
    fn size(&self) -> u64;
    /// Up to `buf.len()` bytes from `offset`; 0 only past the end.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize>;
}

/// The user's answer to an offer.
#[derive(Debug)]
pub enum TransferAnswer {
    /// To `sz`: receive into this folder.
    Receive(Box<dyn ReceiveFolder>),
    /// To `rz`: send these files.
    Send(Vec<Box<dyn OutgoingFile>>),
    Decline,
}

/// What a transfer tells the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferEvent {
    /// Files come into `folder`, or, with none, go to the host.
    Started {
        folder: Option<String>,
    },
    File {
        name: String,
        size: Option<u64>,
    },
    Progress {
        bytes: u64,
        size: Option<u64>,
    },
    /// The first 100 names of what came (or went) and what was skipped,
    /// and how many more.
    Finished {
        folder: Option<String>,
        files: Vec<String>,
        more_files: u64,
        skipped: Vec<String>,
        more_skipped: u64,
    },
    Failed {
        reason: String,
    },
    /// The offer was declined, or came too soon after one was.
    Declined,
}
