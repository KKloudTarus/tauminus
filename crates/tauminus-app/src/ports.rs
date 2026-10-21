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

use tauminus_domain::{LocalShell, SshTarget, TerminalSize};
use thiserror::Error;
use tokio::sync::mpsc;
use zeroize::Zeroizing;

/// Messages from the use case to a terminal stream, applied in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalInput {
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

/// Opens SSH shell sessions.
pub trait SshConnector: Send + Sync + 'static {
    fn connect<'a>(
        &'a self,
        target: &'a SshTarget,
        size: TerminalSize,
        prompter: Arc<dyn Prompter>,
    ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>>;
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
    /// Why this alias could not be read, for example OpenSSH rejecting the
    /// config. Such an entry is reported, not imported.
    pub problem: Option<String>,
}

/// Reads the hosts an OpenSSH client config defines.
pub trait SshConfigSource: Send + Sync + 'static {
    fn hosts(&self) -> BoxFuture<'_, Result<Vec<ConfigHost>, std::io::Error>>;
}
