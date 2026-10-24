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
    LocalOs, OFFER_KEEP, Receive, Received, SessionId, ZmodemOutput, find_offer,
};
use tokio::sync::mpsc;

use crate::ports::{PartFile, ReceiveFolder, TransferEvent, TransferHost};

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

/// One session's transfers.
pub(super) struct Transfer {
    id: SessionId,
    host: Arc<dyn TransferHost>,
    /// To the session's input, through a task: the pump never waits on it.
    to_host: mpsc::UnboundedSender<Vec<u8>>,
    /// Bytes in `to_host` the task has not written yet.
    pending: Arc<AtomicUsize>,
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
    },
    Receiving(Box<Receiving>),
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
        to_host: mpsc::UnboundedSender<Vec<u8>>,
        pending: Arc<AtomicUsize>,
        inputs: Arc<AtomicU64>,
    ) -> Self {
        Self {
            id,
            host,
            to_host,
            pending,
            inputs,
            state: State::Idle,
            tail: Vec::new(),
            declined_at: None,
            offers: 0,
            unannounced: false,
        }
    }

    /// An offer waits, or files come: the session takes no keys.
    pub(super) fn busy(&self) -> bool {
        matches!(self.state, State::Offered { .. } | State::Receiving(_))
    }

    pub(super) fn offered(&self) -> bool {
        matches!(self.state, State::Offered { .. })
    }

    /// Tells the user of a new offer. The pump calls it once it has
    /// published the offer's number, so an answer always finds it.
    pub(super) fn announce(&mut self) {
        if std::mem::take(&mut self.unannounced) && self.offered() {
            self.host.offered(self.id, self.offers);
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

    /// When [`Self::timeout`] is due if nothing comes: while files come or
    /// their rest is dropped.
    pub(super) fn deadline(&self) -> Option<Duration> {
        match self.state {
            State::Receiving(_) => Some(IDLE),
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
            find_offer(&seam)
                .filter(|&at| at < tail.len())
                .map(|at| (at, tail.len() - at))
        };
        let found = found.or_else(|| find_offer(&chunk).map(|at| (tail.len() + at, 0)));
        match found {
            Some((at, shown)) if !cooling => {
                let start = at + shown - tail.len();
                let mut held = tail[tail.len() - shown..].to_vec();
                held.extend_from_slice(&chunk[start..]);
                let before = chunk[..start].to_vec();
                self.offers += 1;
                let offer = self.offers;
                self.state = State::Offered { held, shown, offer };
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

    /// The user's answer to offer `offer`: a folder to receive into, or
    /// none. An answer to another offer, an earlier one, changes nothing.
    /// Returns what the view is to show.
    pub(super) fn answer(&mut self, offer: u64, folder: Option<Box<dyn ReceiveFolder>>) -> Vec<u8> {
        let State::Offered {
            held, offer: open, ..
        } = &mut self.state
        else {
            return Vec::new();
        };
        if *open != offer {
            return Vec::new();
        }
        let Some(folder) = folder else {
            return self.decline();
        };
        let held = std::mem::take(held);
        self.host.event(
            self.id,
            TransferEvent::Started {
                folder: folder.shown(),
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
        let State::Receiving(receiving) = &mut self.state else {
            return Vec::new();
        };
        let mut out = ZmodemOutput::default();
        receiving.receive.timeout(&mut out);
        self.handle(out);
        self.after_end(&[])
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

    /// Queues bytes for the host, counted until the writer task sends them.
    fn send(&self, wire: Vec<u8>) {
        if wire.is_empty() {
            return;
        }
        self.pending.fetch_add(wire.len(), Ordering::Relaxed);
        if self.to_host.send(wire).is_err() {
            self.pending.store(0, Ordering::Relaxed);
        }
    }

    /// Sends the protocol's replies, and keeps what came.
    fn handle(&mut self, out: ZmodemOutput) {
        let backlog = self.pending.load(Ordering::Relaxed) > MAX_PENDING;
        if !backlog {
            self.send(out.wire);
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
                        folder: receiving.folder.shown(),
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
            self.send(out.wire);
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
