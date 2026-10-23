// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Moves bytes between an SSH shell channel and a `TerminalStream`.
//!
//! Reading and writing run on separate tasks. A write waits whenever the
//! server's receive window is full, and the server only reopens it after
//! russh has read what came before; russh's reader stops when its channel
//! buffer is full. One task doing both would wait on itself after a paste
//! larger than the window into a program that echoes it.

use std::sync::Arc;
use std::time::Duration;

use russh::client::Msg;
use russh::{Channel, ChannelMsg, ChannelWriteHalf};
use tauminus_app::ports::{TerminalEvent, TerminalInput, TerminalStream, Terminator};
use tauminus_charset::Transcoder;
use tauminus_domain::Encoding;
use tokio::sync::{Notify, mpsc};
use tracing::debug;

use crate::connection::Connection;

/// Output chunks queued toward the session pump before the bridge stops
/// reading the channel. russh then stops reading the socket, and TCP flow
/// control slows the server.
const EVENT_QUEUE: usize = 4;
const INPUT_QUEUE: usize = 64;
/// How long a probe waits for the server to answer before the link counts
/// as lost: a live link answers in a round trip.
const PROBE_DEADLINE: Duration = Duration::from_secs(5);
const CLOSE_DEADLINE: Duration = Duration::from_secs(1);

/// Starts the bridge. It holds `connection` until the shell ends; the
/// connection closes when nothing else holds it either.
pub(crate) fn spawn(
    channel: Channel<Msg>,
    connection: Arc<Connection>,
    encoding: Encoding,
) -> TerminalStream {
    let (events_tx, events_rx) = mpsc::channel(EVENT_QUEUE);
    let (input_tx, input_rx) = mpsc::channel(INPUT_QUEUE);
    let signals = Arc::new(Signals::default());
    tokio::spawn(run(
        channel,
        connection,
        input_rx,
        events_tx,
        Arc::clone(&signals),
        encoding,
    ));
    TerminalStream {
        input: input_tx,
        events: events_rx,
        terminator: Box::new(SshTerminator(signals)),
    }
}

#[derive(Default)]
struct Signals {
    close: Notify,
    probe: Notify,
}

/// Closes the channel, and the connection unless something else holds it. A
/// network peer cannot ignore a closed channel, so `kill` is the same as
/// `terminate`.
struct SshTerminator(Arc<Signals>);

impl Terminator for SshTerminator {
    fn terminate(&self) {
        self.0.close.notify_one();
    }

    fn probe(&self) {
        self.0.probe.notify_one();
    }
}

impl Drop for SshTerminator {
    fn drop(&mut self) {
        self.terminate();
    }
}

async fn run(
    channel: Channel<Msg>,
    connection: Arc<Connection>,
    input: mpsc::Receiver<TerminalInput>,
    events: mpsc::Sender<TerminalEvent>,
    signals: Arc<Signals>,
    encoding: Encoding,
) {
    let close = &signals.close;
    let (mut reader, writer) = channel.split();
    let writer = Arc::new(writer);
    // Each direction keeps its own: output's decoder holds a character
    // split across chunks.
    let mut output = Transcoder::new(encoding);
    let writing = tokio::spawn(forward_input(
        Arc::clone(&writer),
        input,
        Transcoder::new(encoding),
    ));
    let mut code: Option<u32> = None;
    // The remote side said the shell ended, or this side closed: anything
    // else that ends the channel is the connection going away.
    let mut ended = false;
    // One ping in flight at most; a probe while one waits runs after it.
    // Data arriving is as good as an answer: the answer may be queued
    // behind it, so the deadline counts from the last byte, and a dead
    // link is still found 5 s after it went quiet.
    let ping = tokio::time::sleep(Duration::ZERO);
    tokio::pin!(ping);
    let mut pinging: Option<tokio::task::JoinHandle<()>> = None;
    // Output goes out in the order it arrived; while the pump has no room,
    // the channel is not read, but a terminate still gets through.
    loop {
        let message = tokio::select! {
            biased;
            () = close.notified() => {
                debug!("closing ssh session");
                ended = true;
                break;
            }
            () = signals.probe.notified(), if pinging.is_none() => {
                let connection = Arc::clone(&connection);
                pinging = Some(tokio::spawn(async move {
                    // An error means the transport is gone; the channel
                    // ends on its own then.
                    let _ = connection.last().send_ping().await;
                }));
                ping.as_mut().reset(tokio::time::Instant::now() + PROBE_DEADLINE);
                continue;
            }
            done = async { pinging.as_mut().expect("guarded").await }, if pinging.is_some() => {
                let _ = done;
                pinging = None;
                continue;
            }
            () = &mut ping, if pinging.is_some() => {
                debug!("ssh probe unanswered");
                break;
            }
            message = reader.wait() => message,
        };
        match message {
            Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                tokio::select! {
                    biased;
                    () = close.notified() => {
                        ended = true;
                        break;
                    }
                    sent = events.send(TerminalEvent::Output(output.decode(&data, false))) => {
                        if sent.is_err() {
                            break;
                        }
                    }
                }
                if pinging.is_some() {
                    ping.as_mut()
                        .reset(tokio::time::Instant::now() + PROBE_DEADLINE);
                }
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => {
                code = Some(exit_status);
                ended = true;
            }
            Some(ChannelMsg::ExitSignal { .. }) => {
                code = None;
                ended = true;
            }
            Some(ChannelMsg::Close) => {
                ended = true;
                break;
            }
            None => break,
            Some(_) => {}
        }
    }
    writing.abort();
    if let Some(pinging) = pinging {
        pinging.abort();
    }
    // A link that only seems dead still gets the close, but a dead one
    // is not waited on.
    let _ = tokio::time::timeout(CLOSE_DEADLINE, writer.close()).await;
    drop(connection);
    // A character cut off by the end shows as a replacement, not nothing.
    let rest = output.decode(&[], true);
    if !rest.is_empty() {
        let _ = events.send(TerminalEvent::Output(rest)).await;
    }
    let last = if ended {
        TerminalEvent::Exited { code }
    } else {
        debug!("ssh connection lost");
        TerminalEvent::Lost
    };
    let _ = events.send(last).await;
}

async fn forward_input(
    writer: Arc<ChannelWriteHalf<Msg>>,
    mut input: mpsc::Receiver<TerminalInput>,
    codec: Transcoder,
) {
    while let Some(message) = input.recv().await {
        let sent = match message {
            TerminalInput::Text(text) => writer.data_bytes(codec.encode(&text)).await,
            TerminalInput::Data(bytes) => writer.data_bytes(bytes).await,
            TerminalInput::Resize(size) => {
                writer
                    .window_change(u32::from(size.cols()), u32::from(size.rows()), 0, 0)
                    .await
            }
        };
        if sent.is_err() {
            break;
        }
    }
}
