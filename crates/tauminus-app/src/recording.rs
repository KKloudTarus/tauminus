// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Session recordings: a pane's output written as asciicast v2, sealed to
//! the vault's recordings key, so recording works with the vault locked and
//! playing back needs it open.
//!
//! The pump hands each batch to a [`Recorder`], which turns it into event
//! lines and passes them to a writer thread over a bounded queue. The pump
//! never waits on the disk: when the writer falls that far behind, or the
//! recording reaches [`MAX_BYTES`], the recording stops there and says so.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tauminus_domain::{
    Bookmark, MAX_BOOKMARK_TEXT, MAX_BOOKMARKS, RecordingMeta, SessionId, TerminalSize, Utf8Carry,
    cast_header, cast_output, cast_resize,
};
use thiserror::Error;
use tracing::warn;

use crate::ports::OutputTap;
use crate::session::{SessionError, SessionManager};
use crate::vault::{Vault, VaultError};

/// The most a recording holds, as written (escapes and all): what the
/// player can load in one piece. Past it the recording stops.
pub const MAX_BYTES: u64 = 512 * 1024 * 1024;
/// The screen a recording starts from, serialized, at most.
pub const MAX_INITIAL: usize = 4 * 1024 * 1024;
/// Event lines queued for the writer before the recording gives up.
const QUEUE: usize = 1024;
/// Bytes queued for the writer before the recording gives up: lines are
/// up to a batch long, so the count alone could hold a hundred MB.
const QUEUE_BYTES: usize = 16 * 1024 * 1024;
/// The writer makes what it has survive a crash this often.
const FLUSH_EVERY: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum RecordingError {
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("could not read or write the recording: {0}")]
    Store(String),
    #[error("no recording {0}")]
    NotFound(String),
    #[error("this pane is already recording")]
    AlreadyRecording,
    #[error("at most {MAX_BOOKMARKS} bookmarks, each under {MAX_BOOKMARK_TEXT} bytes")]
    TooMany,
    #[error("the screen to start from is over {} MiB", MAX_INITIAL / 1024 / 1024)]
    TooLarge,
}

/// A recording being written. Every call is on the writer thread.
pub trait RecordingSink: Send + 'static {
    fn write(&mut self, lines: &[u8]) -> Result<(), String>;
    /// Makes what was written so far survive a crash.
    fn flush(&mut self) -> Result<(), String>;
    /// Seals the end and stores `meta` with the recording.
    fn finish(self: Box<Self>, meta: &RecordingMeta) -> Result<(), String>;
}

/// Where recordings are kept. Calls block on the disk: callers run them
/// off the async workers.
pub trait RecordingStore: Send + Sync + 'static {
    fn new_id(&self) -> String;
    /// Starts a recording sealed to `public`, its `meta` stored as begun.
    fn create(
        &self,
        meta: &RecordingMeta,
        public: &[u8; 32],
    ) -> Result<Box<dyn RecordingSink>, String>;
    /// Newest first.
    fn list(&self) -> Result<Vec<RecordingMeta>, String>;
    /// Hands the asciicast text to `each`, piece by piece, until it returns
    /// `false`. `Ok(false)`: the recording ends early, as after a crash.
    fn read(
        &self,
        id: &str,
        secret: &[u8; 32],
        each: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<bool, String>;
    fn delete(&self, id: &str) -> Result<bool, String>;
    /// Replaces the recording's seek checkpoints, sealed like it.
    fn save_checkpoints(
        &self,
        id: &str,
        public: &[u8; 32],
        checkpoints: &[u8],
    ) -> Result<(), String>;
    /// The checkpoints saved last; `None` when there are none.
    fn checkpoints(&self, id: &str, secret: &[u8; 32]) -> Result<Option<Vec<u8>>, String>;
    fn save_bookmarks(
        &self,
        id: &str,
        public: &[u8; 32],
        bookmarks: &[Bookmark],
    ) -> Result<(), String>;
    fn bookmarks(&self, id: &str, secret: &[u8; 32]) -> Result<Vec<Bookmark>, String>;
}

/// Starts and stops recordings, and reads them back.
pub struct Recordings {
    store: Arc<dyn RecordingStore>,
    vault: Arc<Vault>,
    sessions: Arc<SessionManager>,
    active: Mutex<HashMap<SessionId, Arc<Recorder>>>,
}

impl std::fmt::Debug for Recordings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recordings").finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Recordings {
    pub fn new(
        store: Arc<dyn RecordingStore>,
        vault: Arc<Vault>,
        sessions: Arc<SessionManager>,
    ) -> Self {
        Self {
            store,
            vault,
            sessions,
            active: Mutex::default(),
        }
    }

    /// The key recordings are sealed to. While locked it is read from the
    /// vault file; the first recording ever needs the vault open, once, to
    /// make it.
    async fn public(&self) -> Result<[u8; 32], RecordingError> {
        // Open, the key the vault checked; locked, the one its file names,
        // which the next unlock checks.
        match self.vault.recording_keys().await {
            Ok(keys) => return Ok(keys.public),
            Err(VaultError::Locked) => {}
            Err(error) => return Err(error.into()),
        }
        self.vault
            .recordings_public()
            .await
            .ok_or(RecordingError::Vault(VaultError::Locked))
    }

    /// Starts recording `session` from now on. `size` is the terminal's
    /// size, for the header; `title` and `host` name it in the list.
    /// `initial`, the screen as it is now (escape sequences that draw it),
    /// is the first event, so a recording begun mid-session plays back
    /// from what was on screen.
    pub async fn start(
        &self,
        session: SessionId,
        size: TerminalSize,
        title: String,
        host: Option<String>,
        initial: Option<String>,
    ) -> Result<RecordingMeta, RecordingError> {
        if initial.as_ref().is_some_and(|i| i.len() > MAX_INITIAL) {
            return Err(RecordingError::TooLarge);
        }
        if self.active().contains_key(&session) {
            return Err(RecordingError::AlreadyRecording);
        }
        let public = self.public().await?;
        let store = Arc::clone(&self.store);
        let started_ms = now_ms();
        let meta = RecordingMeta {
            id: store.new_id(),
            title,
            host,
            started_ms,
            duration_ms: 0,
            bytes: 0,
            truncated: false,
            complete: false,
        };
        // The tap goes on first and the file is made on the writer thread:
        // output between the screen the view sent and the tap is what a
        // recording misses, so that gap stays one IPC call long. Lines wait
        // in the queue meanwhile.
        let begun = meta.clone();
        let create = move || store.create(&begun, &public);
        {
            let mut active = self.active();
            if active.contains_key(&session) {
                return Err(RecordingError::AlreadyRecording);
            }
            let recorder = Recorder::start(Box::new(create), meta.clone(), size, initial)?;
            active.insert(session, recorder);
        }
        let recorder = Arc::clone(&self.active()[&session]);
        let tap = Arc::clone(&recorder) as Arc<dyn OutputTap>;
        if let Err(error) = self.sessions.set_tap(session, Some(tap)) {
            lock(&self.active).remove(&session);
            recorder.end();
            return Err(error.into());
        }
        Ok(meta)
    }

    /// The recordings in progress, without those whose session ended.
    fn active(&self) -> MutexGuard<'_, HashMap<SessionId, Arc<Recorder>>> {
        let mut active = lock(&self.active);
        active.retain(|_, recorder| !recorder.ended.load(Ordering::Acquire));
        active
    }

    /// Stops recording `session`; nothing when it is not recording.
    pub fn stop(&self, session: SessionId) -> Option<RecordingMeta> {
        let recorder = self.active().remove(&session)?;
        // The session may be gone already; its tap ended with it.
        let _ = self.sessions.set_tap(session, None);
        recorder.end();
        Some(recorder.meta())
    }

    /// Which sessions are recording, with what so far.
    pub fn recording(&self) -> Vec<(SessionId, RecordingMeta)> {
        self.active()
            .iter()
            .map(|(id, recorder)| (*id, recorder.meta()))
            .collect()
    }

    pub async fn list(&self) -> Result<Vec<RecordingMeta>, RecordingError> {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || store.list())
            .await
            .map_err(|e| RecordingError::Store(e.to_string()))?
            .map_err(RecordingError::Store)
    }

    /// Opens recording `id` and hands its text to `each` on a blocking
    /// thread. Needs the vault open. `Ok(false)`: it ends early.
    pub async fn read(
        &self,
        id: String,
        mut each: impl FnMut(&[u8]) -> bool + Send + 'static,
    ) -> Result<bool, RecordingError> {
        let keys = self.vault.recording_keys().await?;
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || store.read(&id, &keys.secret, &mut each))
            .await
            .map_err(|e| RecordingError::Store(e.to_string()))?
            .map_err(RecordingError::Store)
    }

    /// The player's checkpoints for recording `id`, as it saved them.
    pub async fn checkpoints(&self, id: String) -> Result<Option<Vec<u8>>, RecordingError> {
        let keys = self.vault.recording_keys().await?;
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || store.checkpoints(&id, &keys.secret))
            .await
            .map_err(|e| RecordingError::Store(e.to_string()))?
            .map_err(RecordingError::Store)
    }

    /// Replaces the player's checkpoints for recording `id`; needs no
    /// unlock.
    pub async fn save_checkpoints(
        &self,
        id: String,
        checkpoints: Vec<u8>,
    ) -> Result<(), RecordingError> {
        let public = self.public().await?;
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || store.save_checkpoints(&id, &public, &checkpoints))
            .await
            .map_err(|e| RecordingError::Store(e.to_string()))?
            .map_err(RecordingError::Store)
    }

    pub async fn bookmarks(&self, id: String) -> Result<Vec<Bookmark>, RecordingError> {
        let keys = self.vault.recording_keys().await?;
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || store.bookmarks(&id, &keys.secret))
            .await
            .map_err(|e| RecordingError::Store(e.to_string()))?
            .map_err(RecordingError::Store)
    }

    /// Replaces recording `id`'s bookmarks.
    pub async fn save_bookmarks(
        &self,
        id: String,
        bookmarks: Vec<Bookmark>,
    ) -> Result<(), RecordingError> {
        if bookmarks.len() > MAX_BOOKMARKS
            || bookmarks
                .iter()
                .any(|b| b.label.len() + b.comment.len() > MAX_BOOKMARK_TEXT)
        {
            return Err(RecordingError::TooMany);
        }
        let public = self.public().await?;
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || store.save_bookmarks(&id, &public, &bookmarks))
            .await
            .map_err(|e| RecordingError::Store(e.to_string()))?
            .map_err(RecordingError::Store)
    }

    /// Deletes recording `id`, unless it is being written.
    pub async fn delete(&self, id: String) -> Result<(), RecordingError> {
        if self.active().values().any(|r| r.meta().id == id) {
            return Err(RecordingError::AlreadyRecording);
        }
        let store = Arc::clone(&self.store);
        let gone = id.clone();
        let deleted = tokio::task::spawn_blocking(move || store.delete(&gone))
            .await
            .map_err(|e| RecordingError::Store(e.to_string()))?
            .map_err(RecordingError::Store)?;
        if deleted {
            Ok(())
        } else {
            Err(RecordingError::NotFound(id))
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

enum Line {
    Events(Vec<u8>),
}

/// One recording in progress: the tap on the session, and the writer
/// thread's queue.
struct Recorder {
    started: Instant,
    state: Mutex<RecorderState>,
    bytes: AtomicU64,
    /// What the file holds so far, as written.
    written: AtomicU64,
    truncated: AtomicBool,
    /// The writer could not write: the recording stops.
    failed: Arc<AtomicBool>,
    /// Bytes in the writer's queue.
    queued: Arc<AtomicUsize>,
    ended: AtomicBool,
    meta: Mutex<RecordingMeta>,
}

struct RecorderState {
    carry: Utf8Carry,
    /// `None` once stopped: the writer finishes when it sees the queue
    /// closed.
    queue: Option<SyncSender<Line>>,
    final_meta: Arc<Mutex<Option<RecordingMeta>>>,
}

impl Recorder {
    fn start(
        create: Box<dyn FnOnce() -> Result<Box<dyn RecordingSink>, String> + Send>,
        meta: RecordingMeta,
        size: TerminalSize,
        initial: Option<String>,
    ) -> Result<Arc<Self>, RecordingError> {
        let (queue, lines) = sync_channel::<Line>(QUEUE);
        let mut header = cast_header(size, meta.started_ms, &meta.title);
        if let Some(screen) = initial.filter(|s| !s.is_empty()) {
            header.push_str(&cast_output(0, &screen));
        }
        let final_meta = Arc::new(Mutex::new(None::<RecordingMeta>));
        let finished = Arc::clone(&final_meta);
        let fallback = meta.clone();
        let failed = Arc::new(AtomicBool::new(false));
        let failing = Arc::clone(&failed);
        let queued = Arc::new(AtomicUsize::new(0));
        let draining = Arc::clone(&queued);
        std::thread::Builder::new()
            .name("recording-writer".into())
            .spawn(move || {
                let fail = |error: &str| {
                    warn!(%error, "recording stopped: could not write it");
                    failing.store(true, Ordering::Release);
                };
                let mut sink = match create() {
                    Ok(sink) => Some(sink),
                    Err(error) => {
                        fail(&error);
                        None
                    }
                };
                if let Some(writing) = sink.as_mut()
                    && let Err(error) = writing.write(header.as_bytes())
                {
                    fail(&error);
                }
                let mut dirty = false;
                let mut flushed = Instant::now();
                loop {
                    // Woken at least once a second, so what came before a
                    // quiet spell is flushed within a second of it.
                    let line = lines.recv_timeout(FLUSH_EVERY);
                    if let Ok(Line::Events(bytes)) = &line {
                        draining.fetch_sub(bytes.len(), Ordering::AcqRel);
                    }
                    let stopped = matches!(line, Err(RecvTimeoutError::Disconnected));
                    if failing.load(Ordering::Acquire) {
                        if stopped {
                            break;
                        }
                        continue;
                    }
                    let Some(writing) = sink.as_mut() else { break };
                    if let Ok(Line::Events(bytes)) = line {
                        match writing.write(&bytes) {
                            Ok(()) => dirty = true,
                            Err(error) => fail(&error),
                        }
                    }
                    if dirty && (stopped || flushed.elapsed() >= FLUSH_EVERY) {
                        if let Err(error) = writing.flush() {
                            fail(&error);
                        }
                        dirty = false;
                        flushed = Instant::now();
                    }
                    if stopped {
                        break;
                    }
                }
                let mut meta = lock(&finished).take().unwrap_or(fallback);
                if failing.load(Ordering::Acquire) {
                    meta.truncated = true;
                }
                if let Some(sink) = sink
                    && let Err(error) = sink.finish(&meta)
                {
                    warn!(%error, "could not finish a recording");
                }
            })
            .map_err(|e| RecordingError::Store(e.to_string()))?;
        Ok(Arc::new(Self {
            started: Instant::now(),
            state: Mutex::new(RecorderState {
                carry: Utf8Carry::default(),
                queue: Some(queue),
                final_meta,
            }),
            bytes: AtomicU64::new(0),
            written: AtomicU64::new(0),
            truncated: AtomicBool::new(false),
            failed,
            queued,
            ended: AtomicBool::new(false),
            meta: Mutex::new(meta),
        }))
    }

    fn elapsed_us(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX)
    }

    fn meta(&self) -> RecordingMeta {
        let mut meta = lock(&self.meta).clone();
        meta.duration_ms = self.elapsed_us() / 1000;
        meta.bytes = self.bytes.load(Ordering::Relaxed);
        meta.truncated =
            self.truncated.load(Ordering::Relaxed) || self.failed.load(Ordering::Acquire);
        meta
    }

    /// Queues `line`, or stops the recording when the writer is that far
    /// behind. Never waits.
    fn queue(&self, state: &mut RecorderState, line: String) {
        let Some(queue) = &state.queue else {
            return;
        };
        let size = line.len();
        if self.queued.load(Ordering::Acquire) + size > QUEUE_BYTES {
            warn!("recording stopped: the disk could not keep up");
            self.truncated.store(true, Ordering::Relaxed);
            self.close(state);
            return;
        }
        self.queued.fetch_add(size, Ordering::AcqRel);
        match queue.try_send(Line::Events(line.into_bytes())) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                warn!("recording stopped: the disk could not keep up");
                self.truncated.store(true, Ordering::Relaxed);
                self.close(state);
            }
        }
    }

    /// Ends the queue; the writer then seals the end with the last meta.
    fn close(&self, state: &mut RecorderState) {
        if state.queue.is_none() {
            return;
        }
        let mut meta = self.meta();
        meta.complete = true;
        *lock(&state.final_meta) = Some(meta);
        state.queue = None;
    }
}

impl OutputTap for Recorder {
    fn output(&self, bytes: &[u8]) {
        let mut state = lock(&self.state);
        if state.queue.is_none() {
            return;
        }
        if self.failed.load(Ordering::Acquire) {
            self.truncated.store(true, Ordering::Relaxed);
            self.close(&mut state);
            return;
        }
        self.bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        let text = state.carry.push(bytes);
        let mut written = self.written.load(Ordering::Relaxed);
        if !text.is_empty() {
            let line = cast_output(self.elapsed_us(), &text);
            written =
                self.written.fetch_add(line.len() as u64, Ordering::Relaxed) + line.len() as u64;
            self.queue(&mut state, line);
        }
        if written >= MAX_BYTES {
            warn!("recording stopped at its size limit");
            self.truncated.store(true, Ordering::Relaxed);
            let mut state = state;
            self.close(&mut state);
        }
    }

    fn resize(&self, size: TerminalSize) {
        let mut state = lock(&self.state);
        let line = cast_resize(self.elapsed_us(), size);
        self.queue(&mut state, line);
    }

    fn end(&self) {
        let mut state = lock(&self.state);
        let rest = state.carry.finish();
        if !rest.is_empty() {
            let line = cast_output(self.elapsed_us(), &rest);
            self.queue(&mut state, line);
        }
        self.close(&mut state);
        self.ended.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests;
