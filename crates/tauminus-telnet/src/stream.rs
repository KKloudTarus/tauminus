// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! A Telnet session over TCP as a `TerminalStream`.
//!
//! Reading and writing run on separate tasks, as for SSH: a write that
//! waits on a full socket must not stop the reads that would drain it.
//! Both share the protocol state; each holds its lock only to work out
//! bytes, never across a wait.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tauminus_app::ports::{
    BoxFuture, ConnectError, TelnetConnector, TerminalEvent, TerminalInput, TerminalStream,
    Terminator,
};
use tauminus_charset::Transcoder;
use tauminus_domain::{TelnetTarget, TerminalSize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Notify, mpsc};
use tracing::debug;

use crate::protocol::Telnet;

/// How long a connect may take, as for SSH.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Output chunks queued toward the session pump before reading stops,
/// and TCP flow control slows the server.
const EVENT_QUEUE: usize = 4;
const INPUT_QUEUE: usize = 64;
/// Bytes queued toward the socket: typed input and negotiation replies.
const WIRE_QUEUE: usize = 64;
const READ_BUFFER: usize = 32 * 1024;

/// Connects over TCP and answers TTYPE with `terminal`.
#[derive(Debug, Clone)]
pub struct TcpTelnetConnector {
    terminal: String,
}

impl TcpTelnetConnector {
    pub fn new(terminal: &str) -> Self {
        Self {
            terminal: terminal.to_owned(),
        }
    }
}

impl Default for TcpTelnetConnector {
    fn default() -> Self {
        Self::new("xterm-256color")
    }
}

impl TelnetConnector for TcpTelnetConnector {
    fn connect<'a>(
        &'a self,
        target: &'a TelnetTarget,
        size: TerminalSize,
    ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>> {
        Box::pin(async move {
            let socket = tcp(target).await?;
            Ok(spawn(socket, target, size, &self.terminal))
        })
    }
}

/// Resolves the host and tries each address until one connects.
async fn tcp(target: &TelnetTarget) -> Result<TcpStream, ConnectError> {
    let host = target.host.to_string();
    let port = target.port;
    let timeout = |_| ConnectError::Timeout {
        host: host.clone(),
        port,
        seconds: CONNECT_TIMEOUT.as_secs(),
    };
    let addresses: Vec<_> = tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio::net::lookup_host((target.host.as_str(), port)),
    )
    .await
    .map_err(timeout)?
    .map_err(|_| ConnectError::Resolve { host: host.clone() })?
    .collect();
    let mut last = ConnectError::Resolve { host: host.clone() };
    for address in addresses {
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => {
                // Keystrokes are tiny packets; Nagle would delay each one.
                stream.set_nodelay(true)?;
                return Ok(stream);
            }
            Ok(Err(e)) if e.kind() == io::ErrorKind::ConnectionRefused => {
                last = ConnectError::Refused {
                    host: host.clone(),
                    port,
                };
            }
            Ok(Err(e)) => last = ConnectError::Io(e),
            Err(elapsed) => last = timeout(elapsed),
        }
    }
    Err(last)
}

struct TelnetTerminator(Arc<Notify>);

impl Terminator for TelnetTerminator {
    fn terminate(&self) {
        self.0.notify_one();
    }
}

impl Drop for TelnetTerminator {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn lock(telnet: &Mutex<Telnet>) -> std::sync::MutexGuard<'_, Telnet> {
    telnet.lock().unwrap_or_else(PoisonError::into_inner)
}

fn spawn(
    socket: TcpStream,
    target: &TelnetTarget,
    size: TerminalSize,
    terminal: &str,
) -> TerminalStream {
    let (events_tx, events_rx) = mpsc::channel(EVENT_QUEUE);
    let (input_tx, input_rx) = mpsc::channel(INPUT_QUEUE);
    let close = Arc::new(Notify::new());
    let telnet = Arc::new(Mutex::new(Telnet::new(terminal, size)));
    tokio::spawn(run(
        socket,
        Arc::clone(&telnet),
        target.encoding,
        input_rx,
        events_tx,
        Arc::clone(&close),
    ));
    TerminalStream {
        input: input_tx,
        events: events_rx,
        terminator: Box::new(TelnetTerminator(close)),
    }
}

async fn run(
    socket: TcpStream,
    telnet: Arc<Mutex<Telnet>>,
    encoding: tauminus_domain::Encoding,
    input: mpsc::Receiver<TerminalInput>,
    events: mpsc::Sender<TerminalEvent>,
    close: Arc<Notify>,
) {
    let (mut reader, writer) = socket.into_split();
    let (wire_tx, wire_rx) = mpsc::channel::<Vec<u8>>(WIRE_QUEUE);
    let first = lock(&telnet).start();
    let _ = wire_tx.send(first).await;
    let writing = tokio::spawn(write_wire(writer, wire_rx));
    let typing = tokio::spawn(forward_input(
        Arc::clone(&telnet),
        input,
        wire_tx.clone(),
        Transcoder::new(encoding),
    ));
    let mut output = Transcoder::new(encoding);
    let mut buffer = vec![0_u8; READ_BUFFER];
    loop {
        let read = tokio::select! {
            biased;
            () = close.notified() => {
                debug!("closing telnet session");
                break;
            }
            read = reader.read(&mut buffer) => read,
        };
        let n = match read {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let received = lock(&telnet).receive(&buffer[..n]);
        if !received.reply.is_empty() {
            // A server that never reads fills the queue; a close must still
            // get through.
            tokio::select! {
                biased;
                () = close.notified() => break,
                sent = wire_tx.send(received.reply) => {
                    if sent.is_err() {
                        break;
                    }
                }
            }
        }
        if received.data.is_empty() {
            continue;
        }
        let shown = output.decode(&received.data, false);
        tokio::select! {
            biased;
            () = close.notified() => break,
            sent = events.send(TerminalEvent::Output(shown)) => {
                if sent.is_err() {
                    break;
                }
            }
        }
    }
    typing.abort();
    drop(wire_tx);
    // What is queued may never leave: a peer that stopped reading would
    // hold the writer forever. Dropping it closes the socket.
    writing.abort();
    let rest = output.decode(&[], true);
    if !rest.is_empty() {
        let _ = events.send(TerminalEvent::Output(rest)).await;
    }
    // Telnet has no exit status: the connection just closes.
    let _ = events.send(TerminalEvent::Exited { code: None }).await;
}

async fn write_wire(
    mut writer: tokio::net::tcp::OwnedWriteHalf,
    mut wire: mpsc::Receiver<Vec<u8>>,
) {
    while let Some(bytes) = wire.recv().await {
        if writer.write_all(&bytes).await.is_err() {
            return;
        }
    }
    let _ = writer.shutdown().await;
}

async fn forward_input(
    telnet: Arc<Mutex<Telnet>>,
    mut input: mpsc::Receiver<TerminalInput>,
    wire: mpsc::Sender<Vec<u8>>,
    codec: Transcoder,
) {
    while let Some(message) = input.recv().await {
        let bytes = match message {
            TerminalInput::Text(text) => lock(&telnet).send(&codec.encode(&text)),
            TerminalInput::Data(bytes) => lock(&telnet).send(&bytes),
            TerminalInput::Resize(size) => lock(&telnet).resize(size),
        };
        if !bytes.is_empty() && wire.send(bytes).await.is_err() {
            return;
        }
    }
}
