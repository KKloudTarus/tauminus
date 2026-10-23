// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tauminus_domain::{
    FlowControl, FlowSignal, OutputBatcher, PROMPT_TAIL, SessionId, SessionMeta, TerminalSize,
    Watermarks,
};
use thiserror::Error;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::time::Instant;
use tracing::{debug, warn};

use crate::ports::{
    LaunchError, OutputSink, OutputTap, Secret, TerminalEvent, TerminalInput, TerminalStream,
    Terminator,
};

/// Limits and timings for terminal sessions.
#[derive(Debug, Clone, Copy)]
pub struct SessionConfig {
    /// Open sessions allowed at once. Each one holds a pump task and the
    /// adapter's I/O threads or tasks, so the count must be bounded.
    pub max_sessions: usize,
    /// Batch size that is sent without waiting for the flush interval.
    pub flush_bytes: usize,
    /// Under sustained output, the longest time bytes wait in the batcher.
    /// Output after a quiet period is sent at once.
    pub flush_interval: Duration,
    pub watermarks: Watermarks,
    /// How long a closed session waits for its stream to report an exit.
    /// A process that ignores hang-up must not hold a session slot forever.
    pub close_grace: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_sessions: 64,
            flush_bytes: 64 * 1024,
            flush_interval: Duration::from_millis(8),
            watermarks: Watermarks::default(),
            close_grace: Duration::from_secs(2),
        }
    }
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("{0} does not exist or has ended")]
    NotFound(SessionId),
    #[error("cannot open more than {max} sessions")]
    LimitReached { max: usize },
    #[error("failed to start session")]
    Launch(#[source] LaunchError),
}

/// A reserved place for one session. Reserve before starting a process or a
/// connection, so a full session table never leaves a stray process behind.
/// Dropping the slot returns it; the pump holds it until the stream ends.
#[derive(Debug)]
pub struct SessionSlot {
    _permit: OwnedSemaphorePermit,
}

/// Runs terminal sessions: routes input and resizes to their streams, and
/// output to their sinks, with batching and flow control.
///
/// The manager does not know how a stream was started. Use cases such as
/// [`crate::LocalShells`] start a stream and [`attach`](Self::attach) it.
///
/// Shared state:
/// - `sessions` maps ids to live sessions under a `std` mutex. The lock is held
///   only to insert, look up, or remove an entry, never across an `.await`.
/// - `slots` bounds live sessions. A session's permit lives in its pump task,
///   so a slot frees only when the stream has actually ended.
/// - Each session's [`SessionControl`] is shared between the manager and the
///   pump through atomics and `Notify`.
///
/// Each pump task owns its batcher and flow control state, so they need no
/// lock.
#[derive(Debug)]
pub struct SessionManager {
    config: SessionConfig,
    next_id: AtomicU64,
    slots: Arc<Semaphore>,
    sessions: Arc<Mutex<HashMap<SessionId, SessionEntry>>>,
}

#[derive(Debug)]
struct SessionEntry {
    input: mpsc::Sender<TerminalInput>,
    control: Arc<SessionControl>,
    meta: SessionMeta,
}

/// Signals from the manager to one session's pump.
#[derive(Debug, Default)]
struct SessionControl {
    /// Acknowledged bytes not yet applied by the pump. A flood of small acks
    /// costs one wake-up.
    acked_bytes: AtomicUsize,
    acked: Notify,
    closed: Notify,
    probe: Notify,
    /// A recording listening to the output, if any.
    tap: TapSlot,
    /// Bytes of output the session has shown.
    received: AtomicU64,
    /// The last of it, where a prompt would be.
    tail: Mutex<Vec<u8>>,
    /// `received` when a password was last filled. 0 at first, which no
    /// fill sees: a fill needs a prompt, so output.
    filled_at: AtomicU64,
}

/// The tap, and whether the pump has ended: a tap put on after that would
/// never be ended, so none goes on.
#[derive(Default)]
struct TapSlot(Mutex<(Option<Arc<dyn OutputTap>>, bool)>);

impl std::fmt::Debug for TapSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if lock(&self.0).0.is_some() {
            "TapSlot(on)"
        } else {
            "TapSlot(off)"
        })
    }
}

impl SessionManager {
    pub fn new(config: SessionConfig) -> Self {
        Self {
            config,
            next_id: AtomicU64::new(1),
            slots: Arc::new(Semaphore::new(config.max_sessions)),
            sessions: Arc::default(),
        }
    }

    pub fn reserve(&self) -> Result<SessionSlot, SessionError> {
        Arc::clone(&self.slots)
            .try_acquire_owned()
            .map(|permit| SessionSlot { _permit: permit })
            .map_err(|_| SessionError::LimitReached {
                max: self.config.max_sessions,
            })
    }

    /// Starts moving `stream`'s output to `sink` until the stream ends.
    ///
    /// Must be called inside a tokio runtime: it spawns the session's pump.
    pub fn attach(
        &self,
        slot: SessionSlot,
        stream: TerminalStream,
        sink: impl OutputSink,
        meta: SessionMeta,
    ) -> SessionId {
        let TerminalStream {
            input,
            events,
            terminator,
        } = stream;
        let id = SessionId::new(self.next_id.fetch_add(1, Ordering::Relaxed));
        let control = Arc::new(SessionControl::default());
        lock(&self.sessions).insert(
            id,
            SessionEntry {
                input,
                control: Arc::clone(&control),
                meta,
            },
        );

        let pump = Pump {
            id,
            events,
            sink,
            control,
            terminator,
            batcher: OutputBatcher::new(self.config.flush_bytes),
            flow: FlowControl::new(self.config.watermarks),
            flush_interval: self.config.flush_interval,
            close_grace: self.config.close_grace,
            last_flush: None,
            closing: false,
        };
        let registry = Arc::clone(&self.sessions);
        let ending = Arc::clone(&pump.control);
        tokio::spawn(async move {
            pump.run().await;
            // Taken out first: `end` may take a while to write its last lines.
            let tap = {
                let mut slot = lock(&ending.tap.0);
                slot.1 = true;
                slot.0.take()
            };
            if let Some(tap) = tap {
                tap.end();
            }
            lock(&registry).remove(&id);
            drop(slot);
            debug!(%id, "session ended");
        });
        debug!(%id, "session opened");
        id
    }

    /// Sends keystrokes or pasted bytes. Waits when the stream's input queue
    /// is full, so a stalled remote slows the caller instead of dropping input.
    pub async fn write(&self, id: SessionId, data: Vec<u8>) -> Result<(), SessionError> {
        self.send(id, TerminalInput::Data(data)).await
    }

    /// Sends typed or pasted text, which the stream converts to its
    /// shell's character set. Waits as [`Self::write`] does.
    pub async fn write_text(&self, id: SessionId, text: String) -> Result<(), SessionError> {
        self.send(id, TerminalInput::Text(text)).await
    }

    /// Sends a secret as typed text; the stream wipes it once sent.
    pub async fn write_secret(&self, id: SessionId, secret: Secret) -> Result<(), SessionError> {
        self.send(id, TerminalInput::Secret(secret)).await
    }

    /// Bytes of output the session has shown so far, counted as they come,
    /// before they are batched for the view: a view that has parsed fewer
    /// has not seen all of it.
    pub fn received(&self, id: SessionId) -> Result<u64, SessionError> {
        Ok(self.control(id)?.received.load(Ordering::Relaxed))
    }

    /// The last [`PROMPT_TAIL`] bytes of output, and how many came in all.
    pub fn tail(&self, id: SessionId) -> Result<(Vec<u8>, u64), SessionError> {
        let control = self.control(id)?;
        let tail = lock(&control.tail);
        Ok((tail.clone(), control.received.load(Ordering::Relaxed)))
    }

    /// Marks the output so far as filled at: `false` when it was already,
    /// with nothing shown since, so one prompt is filled once.
    pub fn mark_filled(&self, id: SessionId, at: u64) -> Result<bool, SessionError> {
        let control = self.control(id)?;
        Ok(control.filled_at.swap(at, Ordering::Relaxed) != at)
    }

    pub async fn resize(&self, id: SessionId, size: TerminalSize) -> Result<(), SessionError> {
        self.send(id, TerminalInput::Resize(size)).await?;
        if let Ok(control) = self.control(id)
            && let Some(tap) = lock(&control.tap.0).0.clone()
        {
            tap.resize(size);
        }
        Ok(())
    }

    /// Records that the view has processed `bytes` of output.
    pub fn acknowledge(&self, id: SessionId, bytes: usize) -> Result<(), SessionError> {
        let control = self.control(id)?;
        control.acked_bytes.fetch_add(bytes, Ordering::Relaxed);
        control.acked.notify_one();
        Ok(())
    }

    /// Puts `tap` on the session's output, or takes it off with `None`,
    /// returning the one it replaces; that one hears nothing more but is
    /// not ended, which is the caller's to do.
    pub fn set_tap(
        &self,
        id: SessionId,
        tap: Option<Arc<dyn OutputTap>>,
    ) -> Result<Option<Arc<dyn OutputTap>>, SessionError> {
        let control = self.control(id)?;
        let mut slot = lock(&control.tap.0);
        if slot.1 {
            return Err(SessionError::NotFound(id));
        }
        Ok(std::mem::replace(&mut slot.0, tap))
    }

    /// Asks the stream to check its link; a dead one ends the session as
    /// lost. Never waits: it goes around the input queue, which a dead link
    /// may have filled.
    pub fn probe(&self, id: SessionId) -> Result<(), SessionError> {
        self.control(id)?.probe.notify_one();
        Ok(())
    }

    /// Ends the session. The stream is terminated even while output is
    /// paused; output already in flight and the exit notification still
    /// reach the sink.
    pub fn close(&self, id: SessionId) -> Result<(), SessionError> {
        let entry = lock(&self.sessions)
            .remove(&id)
            .ok_or(SessionError::NotFound(id))?;
        entry.control.closed.notify_one();
        Ok(())
    }

    /// Ends every session, for example when the webview reloads and no view
    /// can attach to them again.
    pub fn close_all(&self) -> usize {
        let entries: Vec<SessionEntry> = lock(&self.sessions).drain().map(|(_, e)| e).collect();
        for entry in &entries {
            entry.control.closed.notify_one();
        }
        entries.len()
    }

    pub fn meta(&self, id: SessionId) -> Result<SessionMeta, SessionError> {
        lock(&self.sessions)
            .get(&id)
            .map(|entry| entry.meta.clone())
            .ok_or(SessionError::NotFound(id))
    }

    pub fn open_count(&self) -> usize {
        lock(&self.sessions).len()
    }

    fn control(&self, id: SessionId) -> Result<Arc<SessionControl>, SessionError> {
        lock(&self.sessions)
            .get(&id)
            .map(|entry| Arc::clone(&entry.control))
            .ok_or(SessionError::NotFound(id))
    }

    async fn send(&self, id: SessionId, message: TerminalInput) -> Result<(), SessionError> {
        let input = lock(&self.sessions)
            .get(&id)
            .map(|entry| entry.input.clone())
            .ok_or(SessionError::NotFound(id))?;
        input
            .send(message)
            .await
            .map_err(|_| SessionError::NotFound(id))
    }
}

/// A poisoned lock only means another thread panicked while holding it. The
/// map holds no invariant that a panic can break halfway, so keep using it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Moves one session's output from the stream to the sink.
struct Pump<S> {
    id: SessionId,
    events: mpsc::Receiver<TerminalEvent>,
    sink: S,
    control: Arc<SessionControl>,
    terminator: Box<dyn Terminator>,
    batcher: OutputBatcher,
    flow: FlowControl,
    flush_interval: Duration,
    close_grace: Duration,
    last_flush: Option<Instant>,
    /// Set once the session is closed. A closing session drains its stream
    /// regardless of flow control, so the stream's reader can never block
    /// forever on a queue nobody reads.
    closing: bool,
}

enum Step {
    Continue,
    Stop,
}

impl<S: OutputSink> Pump<S> {
    async fn run(mut self) {
        // One timer, re-armed with `reset`, instead of a new future per loop.
        let timer = tokio::time::sleep(Duration::ZERO);
        tokio::pin!(timer);
        let mut timer_armed = false;
        let grace = tokio::time::sleep(Duration::ZERO);
        tokio::pin!(grace);

        loop {
            let step = tokio::select! {
                biased;
                () = self.control.closed.notified(), if !self.closing => {
                    self.closing = true;
                    self.terminator.terminate();
                    grace.as_mut().reset(Instant::now() + self.close_grace);
                    Step::Continue
                }
                () = &mut grace, if self.closing => {
                    warn!(id = %self.id, "stream ignored termination; killing it and releasing the session");
                    self.terminator.kill();
                    self.finish(None);
                    Step::Stop
                }
                () = self.control.probe.notified(), if !self.closing => {
                    self.terminator.probe();
                    Step::Continue
                }
                () = self.control.acked.notified() => {
                    let acked = self.control.acked_bytes.swap(0, Ordering::Relaxed);
                    if self.flow.record_ack(acked) == FlowSignal::Resume {
                        debug!(id = %self.id, "output resumed");
                    }
                    Step::Continue
                }
                () = &mut timer, if timer_armed => {
                    timer_armed = false;
                    self.flush()
                }
                // While paused, events stay in the bounded channel. The adapter's
                // reader then blocks, and the remote process blocks on its
                // terminal write.
                event = self.events.recv(), if self.accepting() => {
                    let step = self.absorb_burst(event);
                    if matches!(step, Step::Continue) && !self.batcher.is_empty() {
                        match self.flush_deadline() {
                            None => self.flush(),
                            Some(deadline) => {
                                timer.as_mut().reset(deadline);
                                timer_armed = true;
                                Step::Continue
                            }
                        }
                    } else {
                        step
                    }
                }
            };
            if matches!(step, Step::Stop) {
                return;
            }
        }
    }

    fn accepting(&self) -> bool {
        self.closing || !self.flow.is_paused()
    }

    /// Handles `first` and every event already queued behind it, so a burst
    /// of small reads becomes one batch instead of one IPC message per read.
    fn absorb_burst(&mut self, first: Option<TerminalEvent>) -> Step {
        let mut next = first;
        loop {
            let step = match next {
                Some(TerminalEvent::Output(chunk)) => self.absorb_output(chunk),
                Some(TerminalEvent::Exited { code }) => {
                    self.finish(code);
                    Step::Stop
                }
                Some(TerminalEvent::Lost) => {
                    self.flush_output();
                    self.sink.send_lost();
                    Step::Stop
                }
                None => {
                    self.finish(None);
                    Step::Stop
                }
            };
            if matches!(step, Step::Stop) || !self.accepting() {
                return step;
            }
            next = match self.events.try_recv() {
                Ok(event) => Some(event),
                Err(TryRecvError::Empty) => return Step::Continue,
                Err(TryRecvError::Disconnected) => None,
            };
        }
    }

    fn absorb_output(&mut self, chunk: Vec<u8>) -> Step {
        // A closed session keeps draining so the reader never blocks forever,
        // but once the view is behind, the rest is discarded: a process that
        // ignores hang-up must not flood the webview during the grace period.
        if self.closing && self.flow.is_paused() {
            return Step::Continue;
        }
        self.control
            .received
            .fetch_add(chunk.len() as u64, Ordering::Relaxed);
        {
            // O(PROMPT_TAIL) per chunk.
            let mut tail = lock(&self.control.tail);
            tail.extend_from_slice(&chunk[chunk.len().saturating_sub(PROMPT_TAIL)..]);
            let over = tail.len().saturating_sub(PROMPT_TAIL);
            tail.drain(..over);
        }
        for batch in self.batcher.push(chunk) {
            if matches!(self.deliver(batch), Step::Stop) {
                return Step::Stop;
            }
        }
        Step::Continue
    }

    /// `None` means flush now: the last flush was long enough ago that this
    /// output is likely interactive, such as a keystroke echo.
    fn flush_deadline(&self) -> Option<Instant> {
        let last = self.last_flush?;
        let deadline = last + self.flush_interval;
        (deadline > Instant::now()).then_some(deadline)
    }

    fn flush(&mut self) -> Step {
        match self.batcher.take() {
            Some(batch) => self.deliver(batch),
            None => Step::Continue,
        }
    }

    /// What the view is about to get, for a recording.
    fn tap(&self, batch: &[u8]) {
        let tap = lock(&self.control.tap.0).0.clone();
        if let Some(tap) = tap {
            tap.output(batch);
        }
    }

    fn deliver(&mut self, batch: Vec<u8>) -> Step {
        self.tap(&batch);
        let len = batch.len();
        self.last_flush = Some(Instant::now());
        if self.sink.send_output(batch).is_err() {
            warn!(id = %self.id, "view closed; terminating session");
            self.terminator.terminate();
            return Step::Stop;
        }
        if self.flow.record_sent(len) == FlowSignal::Pause {
            debug!(id = %self.id, in_flight = self.flow.in_flight(), "output paused");
        }
        Step::Continue
    }

    fn finish(&mut self, code: Option<u32>) {
        self.flush_output();
        self.sink.send_exit(code);
    }

    fn flush_output(&mut self) {
        if let Some(batch) = self.batcher.take() {
            self.tap(&batch);
            let _ = self.sink.send_output(batch);
        }
    }
}

#[cfg(test)]
mod tests;
