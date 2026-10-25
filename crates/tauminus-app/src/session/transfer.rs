// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Files a host sends with Zmodem (`sz`), taken from a session's output in
//! its pump, before the view, the recording, or the recent ring see them
//! (ADR 0025).
//!
//! Nothing goes back to the host until the user accepts: a host printing
//! what looks like an offer (`cat` of a file) gets no reply, and declining
//! shows what was held back as it was. While an offer waits or files come,
//! the session shows nothing more and takes no keys but Ctrl+C, which
//! cancels.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tauminus_domain::{
    LocalOs, Need, OFFER_KEEP, Receive, Received, Sent, SessionId, TransmitOutput, Wants,
    ZmodemOutput, find_offer,
};
use tokio::sync::mpsc;

use crate::ports::{PartFile, ReceiveFolder, TransferAnswer, TransferEvent, TransferHost};

mod send;

use send::Sending;

/// Output held back while an offer waits, at most: past it, the offer is
/// declined, and the output shown.
const MAX_HELD: usize = 256 << 10;
/// After a decline, offers within this go unanswered: `sz` asks again.
const COOLDOWN: Duration = Duration::from_secs(10);
/// A progress event per this many bytes and this long, at most.
const PROGRESS_STEP: u64 = 256 << 10;
const PROGRESS_EVERY: Duration = Duration::from_millis(100);
/// Nothing from the host for this long: the protocol asks again.
const IDLE: Duration = Duration::from_secs(5);
/// Replies the host has not read yet, at most: a real `sz` leaves a few
/// hundred bytes; a host that sends headers and reads nothing would grow
/// the queue without end.
const MAX_PENDING: usize = 256 << 10;
/// After an abort, the file data the host had sent still comes: output is
/// dropped until text comes after the user typed (Ctrl+C aside), the host
/// is quiet this long,
const QUIET: Duration = Duration::from_millis(300);
/// or this long has passed. What the shell printed as `sz` exited goes
/// with it: Enter brings a prompt.
const MAX_DRAIN: Duration = Duration::from_secs(30);
/// Names a finished transfer lists; past these it counts.
const MAX_NAMES: usize = 100;
/// While sending: bytes queued for the host past which no more file is
/// read, and below which reading starts again. They bound what an abort
/// leaves on the way.
pub(super) const HIGH: usize = 128 << 10;
pub(super) const LOW: usize = 32 << 10;
/// Bytes per write to the session's input while sending. The transport
/// holds 64 writes before it writes them (`tauminus-ssh` `INPUT_QUEUE`),
/// beyond an epoch's reach: small writes keep what a stop cannot recall
/// near 64 KiB.
const CHUNK: usize = 1 << 10;

/// Bytes for the host, through the writer task. Each carries the epoch it
/// was queued in: an abort starts a new one, and the task drops what is
/// older, so the cancel does not wait behind file data.
pub(super) struct Wire {
    pub(super) to_host: mpsc::UnboundedSender<(u64, Vec<u8>)>,
    /// Bytes queued and not written yet.
    pub(super) pending: Arc<AtomicUsize>,
    pub(super) epoch: Arc<AtomicU64>,
}

impl Wire {
    fn send(&self, wire: Vec<u8>) {
        if wire.is_empty() {
            return;
        }
        self.pending.fetch_add(wire.len(), Ordering::Relaxed);
        let epoch = self.epoch.load(Ordering::Relaxed);
        if self.to_host.send((epoch, wire)).is_err() {
            self.pending.store(0, Ordering::Relaxed);
        }
    }

    /// Drops what is queued and not written yet.
    fn cut(&self) {
        self.epoch.fetch_add(1, Ordering::Relaxed);
    }

    fn pending(&self) -> usize {
        self.pending.load(Ordering::Relaxed)
    }
}

/// Whether `chunk` reads as a terminal's text: UTF-8 with no control
/// characters but those a shell prints, and no ZDLE (CAN), which Zmodem
/// data carries every few hundred bytes. O(n).
fn text(chunk: &[u8]) -> bool {
    !chunk.contains(&0x18)
        && std::str::from_utf8(chunk).is_ok_and(|text| {
            text.chars().all(|c| {
                !c.is_control() || matches!(c, '\t' | '\n' | '\r' | '\x07' | '\x08' | '\x1b')
            })
        })
}

/// A Zmodem offer in `bytes`: trzsz ones are not answered yet.
fn zmodem_offer(bytes: &[u8]) -> Option<tauminus_domain::Offer> {
    find_offer(bytes).filter(|offer| offer.protocol == tauminus_domain::TransferProtocol::Zmodem)
}

/// One session's transfers.
pub(super) struct Transfer {
    id: SessionId,
    host: Arc<dyn TransferHost>,
    /// To the session's input, through a task: the pump never waits on it.
    wire: Wire,
    /// Writes that went to the session, counted by `SessionManager`.
    inputs: Arc<AtomicU64>,
    state: State,
    /// The end of what was last shown, where an offer may have begun.
    tail: Vec<u8>,
    declined_at: Option<Instant>,
    /// Offers so far: an answer names the one it is for.
    offers: u64,
    /// An offer the user has not heard of yet: see [`Self::announce`].
    unannounced: bool,
}

enum State {
    Idle,
    /// Offer number `offer` waits for the user, with what came since: the
    /// header's first `shown` bytes, cut across reads, were shown already.
    Offered {
        held: Vec<u8>,
        shown: usize,
        offer: u64,
        wants: Wants,
    },
    Receiving(Box<Receiving>),
    Sending(Box<Sending>),
    /// Done: the protocol's last bytes may still come.
    Ending(Box<Receive>),
    /// Stopped: what the host had sent of the files is dropped, until the
    /// user types (`inputs` changes) or the host is quiet.
    Draining {
        since: Instant,
        last: Instant,
        inputs: u64,
    },
}

struct Receiving {
    receive: Receive,
    folder: Box<dyn ReceiveFolder>,
    part: Option<(String, Box<dyn PartFile>)>,
    size: Option<u64>,
    bytes: u64,
    reported: u64,
    reported_at: Instant,
    files: Vec<String>,
    more_files: u64,
    skipped: Vec<String>,
    more_skipped: u64,
}

/// Keeps `name` in `names` while there are fewer than [`MAX_NAMES`];
/// counts it otherwise.
fn list(names: &mut Vec<String>, more: &mut u64, name: String) {
    if names.len() < MAX_NAMES {
        names.push(name);
    } else {
        *more += 1;
    }
}

impl Transfer {
    pub(super) fn new(
        id: SessionId,
        host: Arc<dyn TransferHost>,
        wire: Wire,
        inputs: Arc<AtomicU64>,
    ) -> Self {
        Self {
            id,
            host,
            wire,
            inputs,
            state: State::Idle,
            tail: Vec::new(),
            declined_at: None,
            offers: 0,
            unannounced: false,
        }
    }

    /// An offer waits, or files come or go: the session takes no keys.
    pub(super) fn busy(&self) -> bool {
        matches!(
            self.state,
            State::Offered { .. } | State::Receiving(_) | State::Sending(_)
        )
    }

    /// Files go: the pump wakes when the host has taken what was queued.
    pub(super) fn sending(&self) -> bool {
        matches!(self.state, State::Sending(_))
    }

    pub(super) fn offered(&self) -> bool {
        matches!(self.state, State::Offered { .. })
    }

    /// Tells the user of a new offer. The pump calls it once it has
    /// published the offer's number, so an answer always finds it.
    pub(super) fn announce(&mut self) {
        if !std::mem::take(&mut self.unannounced) {
            return;
        }
        if let State::Offered { offer, wants, .. } = self.state {
            self.host.offered(self.id, offer, wants);
        }
    }

    /// The offer waiting for the user, if one is: its number and kind.
    pub(super) fn waiting(&self) -> Option<(u64, Wants)> {
        match self.state {
            State::Offered { offer, wants, .. } => Some((offer, wants)),
            _ => None,
        }
    }

    /// The number of the last offer: the one waiting or being received.
    pub(super) fn offer(&self) -> u64 {
        self.offers
    }

    /// Output passes to the view as it comes.
    pub(super) fn idle(&self) -> bool {
        matches!(self.state, State::Idle)
    }

    /// When [`Self::timeout`] is due, fixed: while files go, it counts
    /// from when the host last moved the send on, not from its output.
    pub(super) fn due(&self) -> Option<Instant> {
        match &self.state {
            State::Sending(sending) => Some(sending.moved_at + IDLE),
            _ => None,
        }
    }

    /// When [`Self::timeout`] is due if nothing comes: while files come or
    /// their rest is dropped.
    pub(super) fn deadline(&self) -> Option<Duration> {
        match self.state {
            State::Receiving(_) | State::Sending(_) => Some(IDLE),
            State::Draining { .. } => Some(QUIET),
            _ => None,
        }
    }

    /// Takes a chunk of output; returns what the view is to show of it.
    /// O(n).
    pub(super) fn absorb(&mut self, chunk: Vec<u8>) -> Vec<u8> {
        let (out, used) = match &mut self.state {
            State::Idle => return self.look(chunk),
            State::Offered { held, .. } => {
                held.extend_from_slice(&chunk);
                if held.len() > MAX_HELD {
                    return self.decline();
                }
                return Vec::new();
            }
            State::Draining {
                since,
                last,
                inputs,
            } => {
                // Typed after the stop, and text: what comes now answers
                // the user. File data still coming carries ZDLE.
                let typed = self.inputs.load(Ordering::Relaxed) != *inputs;
                if since.elapsed() > MAX_DRAIN || (typed && text(&chunk)) {
                    self.state = State::Idle;
                    return self.look(chunk);
                }
                *last = Instant::now();
                return Vec::new();
            }
            State::Ending(receive) => {
                let shown = receive.after(&chunk).to_vec();
                if shown.is_empty() {
                    return Vec::new();
                }
                self.state = State::Idle;
                return self.look(shown);
            }
            State::Receiving(receiving) => {
                let mut out = ZmodemOutput::default();
                let used = receiving.receive.feed(&chunk, &mut out);
                (out, used)
            }
            State::Sending(sending) => {
                let mut out = TransmitOutput::default();
                sending.transmit.feed(&chunk, &mut out);
                self.handle_sent(out);
                return self.step();
            }
        };
        self.handle(out);
        self.after_end(&chunk[used.min(chunk.len())..])
    }

    /// Output while idle: shown, unless it holds an offer, which then waits
    /// for the user with what follows it. O(n): a SIMD search of the chunk,
    /// and copies of at most 2 × [`OFFER_KEEP`] + 1 bytes at the seam.
    fn look(&mut self, chunk: Vec<u8>) -> Vec<u8> {
        let tail = std::mem::take(&mut self.tail);
        let cooling = self.declined_at.is_some_and(|at| at.elapsed() < COOLDOWN);
        // A header that starts in the last read ends within this one's
        // first OFFER_KEEP + 1 bytes.
        let found = if tail.is_empty() {
            None
        } else {
            let mut seam = tail.clone();
            seam.extend_from_slice(&chunk[..chunk.len().min(OFFER_KEEP + 1)]);
            zmodem_offer(&seam)
                .filter(|offer| offer.at < tail.len())
                .map(|offer| (offer.at, tail.len() - offer.at, offer.wants))
        };
        let found = found
            .or_else(|| zmodem_offer(&chunk).map(|offer| (tail.len() + offer.at, 0, offer.wants)));
        match found {
            Some((at, shown, wants)) if !cooling => {
                let start = at + shown - tail.len();
                let mut held = tail[tail.len() - shown..].to_vec();
                held.extend_from_slice(&chunk[start..]);
                let before = chunk[..start].to_vec();
                self.offers += 1;
                let offer = self.offers;
                self.state = State::Offered {
                    held,
                    shown,
                    offer,
                    wants,
                };
                self.unannounced = true;
                before
            }
            _ => {
                self.tail = if chunk.len() >= OFFER_KEEP {
                    chunk[chunk.len() - OFFER_KEEP..].to_vec()
                } else {
                    let mut kept = tail;
                    kept.extend_from_slice(&chunk);
                    kept.drain(..kept.len().saturating_sub(OFFER_KEEP));
                    kept
                };
                chunk
            }
        }
    }

    /// The user's answer to offer `offer`: a folder to receive into, files
    /// to send, or no. An answer to another offer (an earlier one), or of
    /// the other kind, changes nothing. Returns what the view is to show.
    pub(super) fn answer(&mut self, offer: u64, answer: TransferAnswer) -> Vec<u8> {
        let State::Offered {
            held,
            offer: open,
            wants,
            ..
        } = &mut self.state
        else {
            return Vec::new();
        };
        if *open != offer {
            return Vec::new();
        }
        match (answer, *wants) {
            (TransferAnswer::Decline, _) => self.decline(),
            (TransferAnswer::Receive(folder), Wants::Receive) => {
                let held = std::mem::take(held);
                self.host.event(
                    self.id,
                    TransferEvent::Started {
                        folder: Some(folder.shown()),
                    },
                );
                self.state = State::Receiving(Box::new(Receiving {
                    receive: Receive::new(LocalOs::current()),
                    folder,
                    part: None,
                    size: None,
                    bytes: 0,
                    reported: 0,
                    reported_at: Instant::now(),
                    files: Vec::new(),
                    more_files: 0,
                    skipped: Vec::new(),
                    more_skipped: 0,
                }));
                self.absorb(held)
            }
            (TransferAnswer::Send(files), Wants::Send) if !files.is_empty() => {
                // The ZRINIT held is not replayed: `rz` may have given up
                // while the user chose. A new sender asks again, and sends
                // a file only to an `rz` that answers it.
                self.host
                    .event(self.id, TransferEvent::Started { folder: None });
                // Past the header and its line end: what the shell printed.
                let held = std::mem::take(held);
                let after = held
                    .iter()
                    .position(|&b| b == b'\n' || b == 0x8a)
                    .map_or(held.len(), |end| end + 1);
                let after = after + usize::from(held.get(after) == Some(&0x11));
                self.state = State::Sending(Box::new(Sending::new(files, held[after..].to_vec())));
                self.next_file();
                self.step()
            }
            _ => Vec::new(),
        }
    }

    /// Nothing goes to the host: what was held back is shown as it came.
    fn decline(&mut self) -> Vec<u8> {
        let State::Offered {
            mut held, shown, ..
        } = std::mem::replace(&mut self.state, State::Idle)
        else {
            return Vec::new();
        };
        self.declined_at = Some(Instant::now());
        self.host.event(self.id, TransferEvent::Declined);
        held.drain(..shown);
        held
    }

    /// Ctrl+C, or the view's cancel: an offer is declined, a receive
    /// stopped, its partial file removed.
    pub(super) fn cancel(&mut self) -> Vec<u8> {
        let out = match &mut self.state {
            State::Offered { .. } => return self.decline(),
            State::Sending(sending) => {
                let mut out = TransmitOutput::default();
                self.wire.cut();
                sending.transmit.abort(&mut out, "cancelled");
                self.handle_sent(out);
                return self.step();
            }
            State::Receiving(receiving) => {
                let mut out = ZmodemOutput::default();
                receiving.receive.abort(&mut out, "cancelled");
                out
            }
            State::Idle | State::Ending(_) | State::Draining { .. } => return Vec::new(),
        };
        self.handle(out);
        self.after_end(&[])
    }

    /// Nothing came from the host for [`Self::deadline`].
    pub(super) fn timeout(&mut self) -> Vec<u8> {
        if let State::Draining { last, .. } = self.state {
            if last.elapsed() >= QUIET {
                self.state = State::Idle;
            }
            return Vec::new();
        }
        if let State::Sending(sending) = &mut self.state {
            if sending.moved_at.elapsed() < IDLE {
                return Vec::new();
            }
            sending.moved_at = Instant::now();
            let mut out = TransmitOutput::default();
            sending.transmit.timeout(&mut out);
            if sending.transmit.ended() {
                self.wire.cut();
            }
            self.handle_sent(out);
            return self.step();
        }
        let State::Receiving(receiving) = &mut self.state else {
            return Vec::new();
        };
        let mut out = ZmodemOutput::default();
        receiving.receive.timeout(&mut out);
        self.handle(out);
        self.after_end(&[])
    }

    /// The host took what was queued: more of the files can go.
    pub(super) fn drained(&mut self) -> Vec<u8> {
        self.step()
    }

    /// Runs the send until it waits for the host, has [`HIGH`] bytes
    /// queued, or ends; then, if it ended, returns what the host printed
    /// after. O(bytes sent).
    fn step(&mut self) -> Vec<u8> {
        loop {
            let State::Sending(sending) = &mut self.state else {
                return Vec::new();
            };
            if sending.transmit.ended() {
                return self.after_send();
            }
            if self.wire.pending() >= HIGH {
                return Vec::new();
            }
            let mut out = TransmitOutput {
                wire: Vec::with_capacity(CHUNK + (2 << 10)),
                sent: Vec::new(),
            };
            // A chunk at a time, so the queue is checked between them.
            while out.wire.len() < CHUNK && out.sent.is_empty() {
                match sending.transmit.poll(&mut out) {
                    Need::Read { offset, max_len } => {
                        sending.moved_at = Instant::now();
                        let Some(file) = sending.current.as_mut() else {
                            sending.transmit.abort(&mut out, "no file to read");
                            break;
                        };
                        sending.buffer.resize(max_len, 0);
                        if let Ok(n) = file.read_at(offset, &mut sending.buffer) {
                            sending.reached = sending.reached.max(offset + n as u64);
                            sending.transmit.provide(&sending.buffer[..n], &mut out);
                        } else {
                            self.wire.cut();
                            sending.transmit.abort(&mut out, "a file could not be read");
                        }
                    }
                    Need::Wait | Need::Ended => break,
                }
            }
            let waiting = out.wire.is_empty() && out.sent.is_empty();
            self.handle_sent(out);
            if waiting {
                return self.step_end();
            }
        }
    }

    /// The send waits for the host, unless it ended.
    fn step_end(&mut self) -> Vec<u8> {
        match &self.state {
            State::Sending(sending) if sending.transmit.ended() => self.after_send(),
            _ => Vec::new(),
        }
    }

    /// Offers the next file the user chose, or tells the host there are
    /// no more.
    fn next_file(&mut self) {
        let State::Sending(sending) = &mut self.state else {
            return;
        };
        let mut out = TransmitOutput::default();
        let Some(file) = sending.queue.pop_front() else {
            sending.current = None;
            sending.transmit.finish(&mut out);
            self.wire.send(out.wire);
            return;
        };
        let (name, size) = (file.name().to_owned(), file.size());
        if let Err(error) = sending.transmit.start(&name, size) {
            self.wire.cut();
            sending.transmit.abort(&mut out, "a file could not be sent");
            self.host.event(
                self.id,
                TransferEvent::Failed {
                    reason: format!("{name}: {error}"),
                },
            );
            self.wire.send(out.wire);
            return;
        }
        sending.current = Some(file);
        sending.reached = 0;
        sending.reported = 0;
        self.host.event(
            self.id,
            TransferEvent::File {
                name,
                size: Some(size),
            },
        );
    }

    /// Queues the protocol's bytes, and acts on what the send reported.
    /// However the send stopped, what is queued and not written goes
    /// first, so the cancel follows at once and no file data reaches a
    /// shell after `rz` is gone.
    fn handle_sent(&mut self, out: TransmitOutput) {
        if out
            .sent
            .iter()
            .any(|step| matches!(step, Sent::Aborted { .. }))
        {
            self.wire.cut();
        }
        self.wire.send(out.wire);
        for step in out.sent {
            let State::Sending(sending) = &mut self.state else {
                return;
            };
            sending.moved_at = Instant::now();
            match step {
                Sent::FileDone | Sent::FileSkipped => {
                    if let Some(file) = sending.current.take() {
                        let name = file.name().to_owned();
                        // Skipped: the host said so, or asked for none of it.
                        if step == Sent::FileSkipped || sending.reached < file.size() {
                            list(&mut sending.skipped, &mut sending.more_skipped, name);
                        } else {
                            list(&mut sending.files, &mut sending.more_files, name);
                        }
                    }
                    self.next_file();
                }
                Sent::Done => self.host.event(
                    self.id,
                    TransferEvent::Finished {
                        folder: None,
                        files: std::mem::take(&mut sending.files),
                        more_files: sending.more_files,
                        skipped: std::mem::take(&mut sending.skipped),
                        more_skipped: sending.more_skipped,
                    },
                ),
                Sent::Aborted { reason } => self.host.event(
                    self.id,
                    TransferEvent::Failed {
                        reason: reason.to_owned(),
                    },
                ),
            }
        }
        if let State::Sending(sending) = &mut self.state
            && sending.reached - sending.reported >= PROGRESS_STEP
            && sending.reported_at.elapsed() >= PROGRESS_EVERY
        {
            sending.reported = sending.reached;
            sending.reported_at = Instant::now();
            let size = sending.current.as_ref().map(|file| file.size());
            self.host.event(
                self.id,
                TransferEvent::Progress {
                    bytes: sending.reached,
                    size,
                },
            );
        }
    }

    /// The send ended: done, what the host printed after is shown; stopped,
    /// what is still on its way is dropped, as after a stopped receive.
    fn after_send(&mut self) -> Vec<u8> {
        let State::Sending(mut sending) = std::mem::replace(&mut self.state, State::Idle) else {
            return Vec::new();
        };
        if sending.transmit.completed() {
            // The end of `rz`'s last header (line end, XON) is the
            // protocol's; the shell's prompt after it is not.
            let rest = sending.transmit.rest();
            let start = rest
                .iter()
                .position(|b| !matches!(b, b'\r' | b'\n' | 0x8a | 0x11))
                .unwrap_or(rest.len());
            return self.look(rest[start..].to_vec());
        }
        // `rz` never answered: none of the files went, and what the shell
        // printed while the user chose is its own.
        if !sending.transmit.answered() {
            let held = std::mem::take(&mut sending.held);
            return self.look(held);
        }
        let now = Instant::now();
        self.state = State::Draining {
            since: now,
            last: now,
            inputs: self.inputs.load(Ordering::Relaxed),
        };
        Vec::new()
    }

    /// Once the receive ended, what is left of the chunk is the terminal's.
    fn after_end(&mut self, rest: &[u8]) -> Vec<u8> {
        let State::Receiving(receiving) = &mut self.state else {
            return Vec::new();
        };
        if !receiving.receive.ended() {
            return Vec::new();
        }
        let State::Receiving(receiving) = std::mem::replace(&mut self.state, State::Idle) else {
            return Vec::new();
        };
        if !receiving.receive.completed() {
            let now = Instant::now();
            self.state = State::Draining {
                since: now,
                last: now,
                inputs: self.inputs.load(Ordering::Relaxed),
            };
            return Vec::new();
        }
        let mut receive = Box::new(receiving.receive);
        let shown = receive.after(rest).to_vec();
        self.state = if shown.is_empty() {
            State::Ending(receive)
        } else {
            State::Idle
        };
        shown
    }

    /// Sends the protocol's replies, and keeps what came.
    fn handle(&mut self, out: ZmodemOutput) {
        let backlog = self.wire.pending() > MAX_PENDING;
        if !backlog {
            self.wire.send(out.wire);
        }
        let State::Receiving(receiving) = &mut self.state else {
            return;
        };
        let mut failed: Option<String> =
            backlog.then(|| "the host stopped reading what the transfer sends".to_owned());
        for step in out.received {
            if failed.is_some() {
                break;
            }
            match step {
                Received::File { name, size } => match receiving.folder.create(&name) {
                    Ok((saved, part)) => {
                        self.host.event(
                            self.id,
                            TransferEvent::File {
                                name: saved.clone(),
                                size,
                            },
                        );
                        receiving.part = Some((saved, part));
                        receiving.size = size;
                        receiving.bytes = 0;
                        receiving.reported = 0;
                    }
                    Err(error) => failed = Some(format!("{name} could not be saved: {error}")),
                },
                Received::Data(bytes) => {
                    if let Some((name, part)) = &mut receiving.part {
                        if let Err(error) = part.write(&bytes) {
                            failed = Some(format!("{name} could not be written: {error}"));
                            continue;
                        }
                        receiving.bytes += bytes.len() as u64;
                        if receiving.bytes - receiving.reported >= PROGRESS_STEP
                            && receiving.reported_at.elapsed() >= PROGRESS_EVERY
                        {
                            receiving.reported = receiving.bytes;
                            receiving.reported_at = Instant::now();
                            self.host.event(
                                self.id,
                                TransferEvent::Progress {
                                    bytes: receiving.bytes,
                                    size: receiving.size,
                                },
                            );
                        }
                    }
                }
                Received::FileDone => {
                    if let Some((name, part)) = receiving.part.take() {
                        match part.finish() {
                            Ok(()) => list(&mut receiving.files, &mut receiving.more_files, name),
                            Err(error) => {
                                failed = Some(format!("{name} could not be saved: {error}"));
                            }
                        }
                    }
                }
                Received::Skipped { name } => {
                    list(&mut receiving.skipped, &mut receiving.more_skipped, name);
                }
                Received::Done => self.host.event(
                    self.id,
                    TransferEvent::Finished {
                        folder: Some(receiving.folder.shown()),
                        files: std::mem::take(&mut receiving.files),
                        more_files: receiving.more_files,
                        skipped: std::mem::take(&mut receiving.skipped),
                        more_skipped: receiving.more_skipped,
                    },
                ),
                Received::Aborted { reason } => {
                    if let Some((_, part)) = receiving.part.take() {
                        part.discard();
                    }
                    self.host.event(
                        self.id,
                        TransferEvent::Failed {
                            reason: reason.to_owned(),
                        },
                    );
                }
            }
        }
        if let Some(reason) = failed {
            // The host is told to stop, and nothing half written stays.
            let mut out = ZmodemOutput::default();
            receiving.receive.abort(&mut out, "stopped");
            if let Some((_, part)) = receiving.part.take() {
                part.discard();
            }
            self.wire.send(out.wire);
            self.host.event(self.id, TransferEvent::Failed { reason });
        }
    }
}

impl Drop for Transfer {
    /// The session ended mid-way: nothing half written stays, and the view
    /// hears it.
    fn drop(&mut self) {
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Offered { .. } => self.host.event(self.id, TransferEvent::Declined),
            State::Receiving(mut receiving) => {
                if let Some((_, part)) = receiving.part.take() {
                    part.discard();
                }
                self.host.event(
                    self.id,
                    TransferEvent::Failed {
                        reason: "the session ended".to_owned(),
                    },
                );
            }
            State::Sending(_) => self.host.event(
                self.id,
                TransferEvent::Failed {
                    reason: "the session ended".to_owned(),
                },
            ),
            State::Idle | State::Ending(_) | State::Draining { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::text;

    #[test]
    fn a_shell_s_output_is_text_and_zmodem_data_is_not() {
        assert!(text(b"\r\n\x1b[1;32mme@box\x1b[0m:~$ "));
        assert!(text("sz: transfer incomplete \u{2014} ok\r\n".as_bytes()));
        // A data subpacket's end: ZDLE ZCRCG.
        assert!(!text(b"abc\x18h\x12\x34"));
        assert!(!text(&[0xc3, 0x28, b'a']));
        assert!(!text(b"\x00\x01raw"));
    }
}
