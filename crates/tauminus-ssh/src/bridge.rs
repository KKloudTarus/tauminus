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

use russh::client::Msg;
use russh::{Channel, ChannelMsg, ChannelWriteHalf};
use tauminus_app::ports::{TerminalEvent, TerminalInput, TerminalStream, Terminator};
use tauminus_domain::Encoding;
use tokio::sync::{Notify, mpsc};
use tracing::debug;

use crate::codec::Transcoder;
use crate::connection::Connection;

/// Output chunks queued toward the session pump before the bridge stops
/// reading the channel. russh then stops reading the socket, and TCP flow
/// control slows the server.
const EVENT_QUEUE: usize = 4;
const INPUT_QUEUE: usize = 64;

/// Starts the bridge. It holds `connection` until the shell ends; the
/// connection closes when nothing else holds it either.
pub(crate) fn spawn(
    channel: Channel<Msg>,
    connection: Arc<Connection>,
    encoding: Encoding,
) -> TerminalStream {
    let (events_tx, events_rx) = mpsc::channel(EVENT_QUEUE);
    let (input_tx, input_rx) = mpsc::channel(INPUT_QUEUE);
    let close = Arc::new(Notify::new());
    tokio::spawn(run(
        channel,
        connection,
        input_rx,
        events_tx,
        Arc::clone(&close),
        encoding,
    ));
    TerminalStream {
        input: input_tx,
        events: events_rx,
        terminator: Box::new(SshTerminator(close)),
    }
}

/// Closes the channel, and the connection unless something else holds it. A
/// network peer cannot ignore a closed channel, so `kill` is the same as
/// `terminate`.
struct SshTerminator(Arc<Notify>);

impl Terminator for SshTerminator {
    fn terminate(&self) {
        self.0.notify_one();
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
    close: Arc<Notify>,
    encoding: Encoding,
) {
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
    // Output goes out in the order it arrived; while the pump has no room,
    // the channel is not read, but a terminate still gets through.
    loop {
        let message = tokio::select! {
            biased;
            () = close.notified() => {
                debug!("closing ssh session");
                break;
            }
            message = reader.wait() => message,
        };
        match message {
            Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                tokio::select! {
                    biased;
                    () = close.notified() => break,
                    sent = events.send(TerminalEvent::Output(output.decode(&data, false))) => {
                        if sent.is_err() {
                            break;
                        }
                    }
                }
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => code = Some(exit_status),
            Some(ChannelMsg::ExitSignal { .. }) => code = None,
            Some(ChannelMsg::Close) | None => break,
            Some(_) => {}
        }
    }
    writing.abort();
    let _ = writer.close().await;
    drop(connection);
    // A character cut off by the end shows as a replacement, not nothing.
    let rest = output.decode(&[], true);
    if !rest.is_empty() {
        let _ = events.send(TerminalEvent::Output(rest)).await;
    }
    let _ = events.send(TerminalEvent::Exited { code }).await;
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
