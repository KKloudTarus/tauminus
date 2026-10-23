// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Copies bytes both ways between a local socket and an SSH channel, for
//! forwarding.
//!
//! russh gives a channel's window back when data arrives, and its session
//! loop waits while the channel's queue is full. So a local peer that stops
//! reading would stall every channel on the connection: shells, SFTP, and
//! keepalives. The channel is therefore always read at once, into a buffer
//! of at most [`MAX_BUFFERED`] for this channel and a byte budget shared by
//! the whole connection. A peer that falls that far behind, or takes
//! nothing for [`STALL`], is reset; when the shared budget runs out, the
//! channel holding the most of it goes, not the one that asked last. The
//! session loop never waits on a local socket.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use russh::client::Msg;
use russh::{Channel, ChannelMsg};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, watch};

/// Most bytes one forwarded connection may have waiting for its peer.
pub(crate) const MAX_BUFFERED: usize = 4 * 1024 * 1024;
/// Most bytes all forwarded connections on one SSH connection may have
/// waiting.
pub(crate) const BUDGET: usize = 64 * 1024 * 1024;
/// What one queued message costs at least, for its queue entry: without a
/// floor, a flood of tiny packets would hold memory the budget never saw.
const MESSAGE_COST: usize = 128;
/// How long a write to the peer may make no progress.
pub(crate) const STALL: Duration = Duration::from_secs(30);
/// How long a channel waits for the budget after it evicted a bigger
/// holder.
const EVICT_WAIT: Duration = Duration::from_secs(1);
/// Room kept for each read from the local peer.
const CHUNK: usize = 64 * 1024;

/// The byte budget one connection's forwarded channels share, and who
/// holds how much of it.
pub(crate) struct Ledger {
    budget: Arc<Semaphore>,
    /// How long a write to a peer may make no progress.
    stall: Duration,
    next: AtomicU64,
    holders: Mutex<Vec<Holder>>,
}

#[derive(Clone)]
struct Holder {
    id: u64,
    buffered: Arc<AtomicUsize>,
    evict: Arc<Notify>,
}

/// One channel's place in the ledger; leaving drops it from the list.
struct Member<'a> {
    ledger: &'a Ledger,
    holder: Holder,
}

impl Drop for Member<'_> {
    fn drop(&mut self) {
        self.ledger.lock().retain(|h| h.id != self.holder.id);
    }
}

impl Ledger {
    /// `budget` bytes shared, [`BUDGET`] normally; `stall` as [`STALL`].
    pub(crate) fn new(budget: usize, stall: Duration) -> Self {
        Self {
            budget: Arc::new(Semaphore::new(budget)),
            stall,
            next: AtomicU64::new(0),
            holders: Mutex::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Holder>> {
        self.holders.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn join(&self) -> Member<'_> {
        let holder = Holder {
            id: self.next.fetch_add(1, Ordering::Relaxed),
            buffered: Arc::new(AtomicUsize::new(0)),
            evict: Arc::new(Notify::new()),
        };
        self.lock().push(holder.clone());
        Member {
            ledger: self,
            holder,
        }
    }

    /// `cost` of the budget for `me`. When it has run out, the holder of
    /// the most is told to go, if that is someone holding more than `me`,
    /// and `me` waits a moment for what it frees. O(c) in the channels
    /// on the connection, only when the budget is out.
    async fn take(&self, me: &Holder, cost: u32) -> Result<OwnedSemaphorePermit, PipeError> {
        if let Ok(permit) = Arc::clone(&self.budget).try_acquire_many_owned(cost) {
            return Ok(permit);
        }
        let mine = me.buffered.load(Ordering::Relaxed);
        let victim = self
            .lock()
            .iter()
            .max_by_key(|h| h.buffered.load(Ordering::Relaxed))
            .filter(|h| h.id != me.id && h.buffered.load(Ordering::Relaxed) > mine)
            .map(|h| Arc::clone(&h.evict));
        let Some(victim) = victim else {
            return Err(PipeError::Budget);
        };
        victim.notify_one();
        match tokio::time::timeout(
            EVICT_WAIT,
            Arc::clone(&self.budget).acquire_many_owned(cost),
        )
        .await
        {
            Ok(Ok(permit)) => Ok(permit),
            _ => Err(PipeError::Budget),
        }
    }
}

impl PipeError {
    /// True when the peer was reset for falling behind or stalling.
    pub(crate) fn reset_peer(&self) -> bool {
        matches!(
            self,
            Self::Behind | Self::Budget | Self::Evicted | Self::Stalled
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PipeError {
    #[error("the local peer fell more than {} MiB behind", MAX_BUFFERED / (1024 * 1024))]
    Behind,
    #[error("the connection's forwards hold all the memory they may")]
    Budget,
    #[error("another connection needed the memory this one held unread")]
    Evicted,
    #[error("the local peer took nothing for too long")]
    Stalled,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Ssh(#[from] russh::Error),
    /// The server closed the channel; not a failure, but it ends both ways.
    #[error("the channel closed")]
    Closed,
}

/// Runs until both sides have finished, either side fails, or `stop`
/// changes or closes. Half-closes pass through: EOF from the peer becomes
/// channel EOF, and channel EOF shuts the peer's write side. A peer closed
/// for falling behind gets a reset, not a FIN, so it cannot take the bytes
/// it got for the whole stream.
pub(crate) async fn pipe(
    mut peer: TcpStream,
    channel: Channel<Msg>,
    ledger: Arc<Ledger>,
    mut stop: watch::Receiver<()>,
) -> Result<(), PipeError> {
    let result = run(&mut peer, channel, &ledger, &mut stop).await;
    if result.as_ref().is_err_and(PipeError::reset_peer) {
        let _ = peer.set_zero_linger();
    }
    result
}

async fn run(
    peer: &mut TcpStream,
    channel: Channel<Msg>,
    ledger: &Ledger,
    stop: &mut watch::Receiver<()>,
) -> Result<(), PipeError> {
    let member = ledger.join();
    let me = &member.holder;
    let (mut from_peer, mut to_peer) = peer.split();
    let (mut from_server, to_server) = channel.split();
    let up = async {
        // Read straight into owned bytes that russh slices into packets
        // without copying them again.
        let mut buffer = bytes::BytesMut::with_capacity(CHUNK);
        loop {
            buffer.reserve(CHUNK);
            let read = from_peer.read_buf(&mut buffer).await?;
            if read == 0 {
                to_server.eof().await?;
                return Ok::<_, PipeError>(());
            }
            to_server.data_bytes(buffer.split().freeze()).await?;
        }
    };
    // One task reads the channel and writes the peer, picking whichever is
    // ready: no hand-off between tasks per packet. Unbiased, so neither
    // side starves the other.
    let down = async {
        let mut queue: VecDeque<(bytes::Bytes, OwnedSemaphorePermit, usize)> = VecDeque::new();
        let mut eof = false;
        let mut closed = false;
        let stall = tokio::time::sleep(ledger.stall);
        tokio::pin!(stall);
        loop {
            if queue.is_empty() {
                if eof {
                    let _ = to_peer.shutdown().await;
                    eof = false;
                }
                if closed {
                    return Err::<(), _>(PipeError::Closed);
                }
            }
            let pending = !queue.is_empty();
            tokio::select! {
                message = from_server.wait(), if !closed => match message {
                    // Empty data carries nothing; a second EOF means nothing.
                    Some(ChannelMsg::Data { data }) if data.is_empty() => {}
                    Some(ChannelMsg::Data { data }) => {
                        let cost = data.len().max(MESSAGE_COST);
                        if me.buffered.fetch_add(cost, Ordering::Relaxed) + cost > MAX_BUFFERED {
                            return Err(PipeError::Behind);
                        }
                        let permit = ledger
                            .take(me, u32::try_from(cost).unwrap_or(u32::MAX))
                            .await?;
                        if queue.is_empty() {
                            stall.as_mut().reset(tokio::time::Instant::now() + ledger.stall);
                        }
                        queue.push_back((data, permit, cost));
                    }
                    Some(ChannelMsg::Eof) => eof = true,
                    Some(ChannelMsg::Close) | None => closed = true,
                    Some(_) => {}
                },
                written = write_some(&mut to_peer, &queue), if pending => {
                    let mut written = written?;
                    if written == 0 {
                        return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into());
                    }
                    stall.as_mut().reset(tokio::time::Instant::now() + ledger.stall);
                    while written > 0 {
                        let front = queue.front_mut().expect("what was written was queued");
                        if written >= front.0.len() {
                            written -= front.0.len();
                            let (_, permit, cost) = queue.pop_front().expect("front exists");
                            me.buffered.fetch_sub(cost, Ordering::Relaxed);
                            drop(permit);
                        } else {
                            bytes::Buf::advance(&mut front.0, written);
                            written = 0;
                        }
                    }
                }
                () = &mut stall, if pending => return Err(PipeError::Stalled),
            }
        }
    };
    let result = tokio::select! {
        result = async { tokio::try_join!(up, down).map(|_| ()) } => result,
        _ = stop.changed() => Ok(()),
        () = me.evict.notified() => Err(PipeError::Evicted),
    };
    let _ = to_server.close().await;
    match result {
        Err(PipeError::Closed) => Ok(()),
        other => other,
    }
}

/// Writes the front of `queue`, up to 16 packets in one call.
async fn write_some(
    to_peer: &mut tokio::net::tcp::WriteHalf<'_>,
    queue: &VecDeque<(bytes::Bytes, OwnedSemaphorePermit, usize)>,
) -> std::io::Result<usize> {
    let slices: Vec<std::io::IoSlice<'_>> = queue
        .iter()
        .take(16)
        .map(|(data, ..)| std::io::IoSlice::new(data))
        .collect();
    to_peer.write_vectored(&slices).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger(budget: usize) -> Ledger {
        Ledger::new(budget, STALL)
    }

    #[tokio::test]
    async fn out_of_budget_the_biggest_holder_is_told_to_go_and_the_asker_waits() {
        let ledger = ledger(1000);
        let big = ledger.join();
        let small = ledger.join();
        let held = ledger.take(&big.holder, 900).await.unwrap();
        big.holder.buffered.store(900, Ordering::Relaxed);
        small.holder.buffered.store(100, Ordering::Relaxed);
        let asked = ledger.take(&small.holder, 300);
        tokio::pin!(asked);
        // Not yet: the big one has to let go first.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut asked)
                .await
                .is_err()
        );
        tokio::time::timeout(Duration::from_millis(50), big.holder.evict.notified())
            .await
            .expect("the biggest holder was told");
        drop(held);
        assert!(asked.await.is_ok(), "and the asker got what it freed");
    }

    #[tokio::test]
    async fn the_biggest_holder_asking_is_refused_at_once() {
        let ledger = ledger(1000);
        let big = ledger.join();
        let small = ledger.join();
        let _held = ledger.take(&big.holder, 900).await.unwrap();
        big.holder.buffered.store(900, Ordering::Relaxed);
        small.holder.buffered.store(100, Ordering::Relaxed);
        let started = tokio::time::Instant::now();
        assert!(matches!(
            ledger.take(&big.holder, 300).await,
            Err(PipeError::Budget)
        ));
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn a_holder_that_frees_nothing_fails_the_asker_after_the_wait() {
        let ledger = ledger(1000);
        let big = ledger.join();
        let small = ledger.join();
        let _held = ledger.take(&big.holder, 1000).await.unwrap();
        big.holder.buffered.store(1000, Ordering::Relaxed);
        assert!(matches!(
            ledger.take(&small.holder, 1).await,
            Err(PipeError::Budget)
        ));
    }

    #[tokio::test]
    async fn leaving_drops_a_member_from_the_list() {
        let ledger = ledger(1000);
        let first = ledger.join();
        let second = ledger.join();
        assert_eq!(ledger.lock().len(), 2);
        drop(first);
        assert_eq!(ledger.lock().len(), 1);
        drop(second);
        assert_eq!(ledger.lock().len(), 0);
    }
}
