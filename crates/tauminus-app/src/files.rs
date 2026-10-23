// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SFTP sessions to saved hosts, and the transfers they run.
//!
//! A session is SFTP on the connection forwards share for its host, so it
//! needs no second login when one is live. Transfers run one at a time per
//! session and at most [`MAX_RUNNING`] at once in all; each reports its
//! progress through a watch and stops when cancelled, leaving what it
//! copied for a resume.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use tauminus_domain::{HostId, LocalOs, local_name, remote_join};
use thiserror::Error;
use tokio::sync::{Semaphore, watch};

use crate::connections::{Connections, Login};
use crate::ports::{
    ConnectError, Download, Prompter, SftpError, SftpPort, TransferProgress, Upload,
};

/// Most SFTP sessions open at once.
pub const MAX_SESSIONS: usize = 16;
/// Most transfers moving bytes at once, across every session.
pub const MAX_RUNNING: usize = 4;
/// Most transfers at once on one session. SFTP carries many requests on
/// one channel, so small files need not wait their turn one by one.
pub const PER_SESSION: usize = 4;
/// Finished transfers kept for the list, oldest dropped first.
const KEEP_FINISHED: usize = 200;
/// Most transfers waiting or running at once.
const MAX_QUEUED: usize = 1_000;

#[derive(Debug, Error)]
pub enum FilesError {
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error(transparent)]
    Sftp(#[from] SftpError),
    #[error("no such file session")]
    NoSession,
    #[error("at most {MAX_SESSIONS} file sessions at once")]
    TooManySessions,
    #[error("at most {MAX_QUEUED} transfers waiting at once")]
    TooManyTransfers,
    #[error("{0:?} cannot be a local file name: {1}")]
    Name(String, tauminus_domain::SftpNameError),
    #[error("{0} is already being downloaded")]
    Busy(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Download,
    Upload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferStatus {
    Waiting,
    Running,
    Done,
    Cancelled,
    Failed(String),
}

/// One transfer as the view lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferState {
    pub id: u64,
    pub session: u64,
    pub direction: Direction,
    /// The file's name, for the list.
    pub name: String,
    pub progress: TransferProgress,
    pub status: TransferStatus,
}

struct Session {
    host: HostId,
    sftp: Arc<dyn SftpPort>,
    /// To log in again when the connection drops mid-transfer.
    login: Login,
    prompter: Arc<dyn Prompter>,
    /// One new login at a time.
    reconnecting: Arc<tokio::sync::Mutex<()>>,
    /// A new login was refused or cancelled; no more are tried.
    gave_up: bool,
    /// [`PER_SESSION`] transfers at a time on this session.
    turn: Arc<Semaphore>,
}

/// What the list shows of a transfer, and the local file it writes.
struct Meta {
    session: u64,
    direction: Direction,
    name: String,
    writes: Option<String>,
}

/// What a queued transfer takes from its session.
struct Parts {
    host: HostId,
    sftp: Arc<dyn SftpPort>,
    turn: Arc<Semaphore>,
}

struct Transfer {
    session: u64,
    direction: Direction,
    name: String,
    progress: watch::Receiver<TransferProgress>,
    status: TransferStatus,
    /// Dropped to cancel.
    cancel: Option<watch::Sender<()>>,
    /// The file it writes, as `writing` keys it, freed when it ends.
    writes: Option<String>,
}

#[derive(Default)]
struct State {
    sessions: HashMap<u64, Session>,
    transfers: BTreeMap<u64, Transfer>,
    finished: VecDeque<u64>,
    next: u64,
    /// Files a transfer is writing: local paths, and `session/remote path`
    /// on a server, so two never share a partial copy or race to rename.
    writing: std::collections::HashSet<String>,
}

pub struct Files {
    connections: Arc<Connections>,
    state: Arc<Mutex<State>>,
    running: Arc<Semaphore>,
}

impl std::fmt::Debug for Files {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Files").finish_non_exhaustive()
    }
}

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Files {
    pub fn new(connections: Arc<Connections>) -> Self {
        Self {
            connections,
            state: Arc::default(),
            running: Arc::new(Semaphore::new(MAX_RUNNING)),
        }
    }

    /// Opens SFTP to `login`'s host, on its shared connection.
    pub async fn open(
        &self,
        login: &Login,
        prompter: Arc<dyn Prompter>,
    ) -> Result<u64, FilesError> {
        if lock(&self.state).sessions.len() >= MAX_SESSIONS {
            return Err(FilesError::TooManySessions);
        }
        let connection = self
            .connections
            .acquire(login, Arc::clone(&prompter))
            .await?;
        let sftp = connection.open_sftp().await?;
        let mut state = lock(&self.state);
        if state.sessions.len() >= MAX_SESSIONS {
            return Err(FilesError::TooManySessions);
        }
        state.next += 1;
        let id = state.next;
        state.sessions.insert(
            id,
            Session {
                host: login.host.clone(),
                sftp,
                login: login.clone(),
                prompter,
                reconnecting: Arc::default(),
                gave_up: false,
                turn: Arc::new(Semaphore::new(PER_SESSION)),
            },
        );
        Ok(id)
    }

    /// Ends a session and cancels its transfers.
    pub fn close(&self, id: u64) {
        let mut state = lock(&self.state);
        state.sessions.remove(&id);
        for transfer in state.transfers.values_mut() {
            if transfer.session == id {
                transfer.cancel = None;
            }
        }
    }

    /// The session's files, for listing and changes.
    pub fn sftp(&self, id: u64) -> Result<Arc<dyn SftpPort>, FilesError> {
        lock(&self.state)
            .sessions
            .get(&id)
            .map(|s| Arc::clone(&s.sftp))
            .ok_or(FilesError::NoSession)
    }

    /// Queues `remote` to be copied into the local folder `into`, under its
    /// own name once that is checked: a name the server chose never leaves
    /// the folder or hides what it is.
    pub fn download(
        &self,
        id: u64,
        remote: &str,
        into: &Path,
        overwrite: bool,
    ) -> Result<u64, FilesError> {
        let base = remote
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(remote);
        let name = local_name(base, LocalOs::current())
            .map_err(|e| FilesError::Name(base.to_owned(), e))?
            .to_owned();
        let Parts { host, sftp, turn } = self.session_parts(id)?;
        let local = into.join(&name);
        let key = target_key(&local.display().to_string());
        if !lock(&self.state).writing.insert(key.clone()) {
            return Err(FilesError::Busy(key));
        }
        let job = Download {
            remote: remote.to_owned(),
            local: local.clone(),
            host: host.to_string(),
            overwrite,
            resume: true,
            max: None,
        };
        let queued = self.queue(
            Meta {
                session: id,
                direction: Direction::Download,
                name,
                writes: Some(key.clone()),
            },
            turn,
            sftp,
            move |sftp, progress, cancel| {
                let job = job.clone();
                Box::pin(async move { sftp.download(&job, progress, cancel).await })
            },
        );
        if queued.is_err() {
            lock(&self.state).writing.remove(&key);
        }
        queued
    }

    /// Queues the local file `local` to be copied into the remote folder
    /// `into`, under its name. `state` keeps what a resume needs.
    pub fn upload(
        &self,
        id: u64,
        local: &Path,
        into: &str,
        overwrite: bool,
        state: PathBuf,
    ) -> Result<u64, FilesError> {
        let name = local
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| {
                FilesError::Sftp(SftpError::Local(std::io::Error::other("not a file")))
            })?;
        let Parts { host, sftp, turn } = self.session_parts(id)?;
        let remote = remote_join(into, &name);
        // By host, not session: two sessions to one host share the file.
        let key = format!("{host}/{remote}");
        if !lock(&self.state).writing.insert(key.clone()) {
            return Err(FilesError::Busy(remote));
        }
        let job = Upload {
            local: local.to_owned(),
            remote,
            host: host.to_string(),
            overwrite,
            state,
            resume: true,
        };
        let queued = self.queue(
            Meta {
                session: id,
                direction: Direction::Upload,
                name,
                writes: Some(key.clone()),
            },
            turn,
            sftp,
            move |sftp, progress, cancel| {
                let job = job.clone();
                Box::pin(async move { sftp.upload(&job, progress, cancel).await })
            },
        );
        if queued.is_err() {
            lock(&self.state).writing.remove(&key);
        }
        queued
    }

    /// Stops a transfer; what it copied stays for a resume. False when it
    /// had already finished.
    pub fn cancel(&self, transfer: u64) -> bool {
        lock(&self.state)
            .transfers
            .get_mut(&transfer)
            .and_then(|t| t.cancel.take())
            .is_some()
    }

    /// Every transfer waiting, running, or finished lately, oldest first.
    /// O(n), n at most the queue plus [`KEEP_FINISHED`].
    pub fn transfers(&self) -> Vec<TransferState> {
        lock(&self.state)
            .transfers
            .iter()
            .map(|(id, t)| TransferState {
                id: *id,
                session: t.session,
                direction: t.direction,
                name: t.name.clone(),
                progress: *t.progress.borrow(),
                status: t.status.clone(),
            })
            .collect()
    }

    fn session_parts(&self, id: u64) -> Result<Parts, FilesError> {
        let state = lock(&self.state);
        let session = state.sessions.get(&id).ok_or(FilesError::NoSession)?;
        Ok(Parts {
            host: session.host.clone(),
            sftp: Arc::clone(&session.sftp),
            turn: Arc::clone(&session.turn),
        })
    }

    fn queue<F>(
        &self,
        meta: Meta,
        turn: Arc<Semaphore>,
        sftp: Arc<dyn SftpPort>,
        run: F,
    ) -> Result<u64, FilesError>
    where
        F: Fn(
                Arc<dyn SftpPort>,
                watch::Sender<TransferProgress>,
                watch::Receiver<()>,
            ) -> crate::ports::BoxFuture<'static, Result<u64, SftpError>>
            + Send
            + Sync
            + 'static,
    {
        let Meta {
            session,
            direction,
            name,
            writes,
        } = meta;
        let (progress_tx, progress) = watch::channel(TransferProgress::default());
        let (cancel_tx, cancel) = watch::channel(());
        let id = {
            let mut state = lock(&self.state);
            let waiting = state
                .transfers
                .values()
                .filter(|t| matches!(t.status, TransferStatus::Waiting | TransferStatus::Running))
                .count();
            if waiting >= MAX_QUEUED {
                return Err(FilesError::TooManyTransfers);
            }
            state.next += 1;
            let id = state.next;
            state.transfers.insert(
                id,
                Transfer {
                    session,
                    direction,
                    name,
                    progress,
                    status: TransferStatus::Waiting,
                    cancel: Some(cancel_tx),
                    writes,
                },
            );
            id
        };
        let state = Arc::clone(&self.state);
        let running = Arc::clone(&self.running);
        let connections = Arc::clone(&self.connections);
        tokio::spawn(async move {
            let mut cancelled = cancel.clone();
            // Only the wait for a turn listens for a cancel here; once it
            // runs, the transfer sees the cancel itself and closes its
            // handle and saves its record before it ends.
            let permits = tokio::select! {
                biased;
                _ = cancelled.changed() => None,
                permits = async {
                    let turn = Arc::clone(&turn).acquire_owned().await.ok()?;
                    let slot = Arc::clone(&running).acquire_owned().await.ok()?;
                    Some((turn, slot))
                } => permits,
            };
            let result = match permits {
                None => Some(Err(SftpError::Cancelled)),
                Some(_permits) => {
                    set_status(&state, id, TransferStatus::Running);
                    let mut sftp = sftp;
                    let mut outcome = None;
                    // A dropped connection logs in again and resumes from
                    // the partial copy, a few times, waiting longer each time.
                    for wait in RETRY_AFTER.iter().map(Some).chain([None]) {
                        match run(Arc::clone(&sftp), progress_tx.clone(), cancel.clone()).await {
                            Err(SftpError::Closed) => {
                                let Some(wait) = wait else { break };
                                let slept = tokio::select! {
                                    () = tokio::time::sleep(*wait) => true,
                                    _ = cancelled.changed() => false,
                                };
                                if !slept {
                                    outcome = Some(Err(SftpError::Cancelled));
                                    break;
                                }
                                // A login that fails now may work after the
                                // next wait: the old session fails fast meanwhile.
                                match reconnect(&connections, &state, session, &sftp).await {
                                    Reconnect::Again(again) => sftp = again,
                                    Reconnect::Later => {}
                                    Reconnect::Stop => {
                                        outcome = Some(Err(SftpError::Closed));
                                        break;
                                    }
                                }
                            }
                            other => {
                                outcome = Some(other);
                                break;
                            }
                        }
                    }
                    outcome
                }
            };
            let status = match result {
                Some(Ok(_)) => TransferStatus::Done,
                Some(Err(SftpError::Cancelled)) => TransferStatus::Cancelled,
                None if cancel_closed(&cancelled) => TransferStatus::Cancelled,
                None => TransferStatus::Failed(SftpError::Closed.to_string()),
                Some(Err(error)) => TransferStatus::Failed(error.to_string()),
            };
            finish(&state, id, status);
        });
        Ok(id)
    }
}

/// How long to wait before each new login after a connection drops.
#[cfg(not(test))]
const RETRY_AFTER: [std::time::Duration; 3] = [
    std::time::Duration::from_secs(2),
    std::time::Duration::from_secs(5),
    std::time::Duration::from_secs(10),
];
/// The same number of tries, quickly, for tests.
#[cfg(test)]
const RETRY_AFTER: [std::time::Duration; 3] = [std::time::Duration::from_millis(5); 3];

/// True once the transfer's cancel sender is gone.
fn cancel_closed(cancel: &watch::Receiver<()>) -> bool {
    cancel.has_changed().is_err()
}

/// SFTP on a new login for a session whose connection dropped, or `None`
/// when the session is gone or the login fails.
/// A target as `writing` keys it. On macOS and Windows, file names ignore
/// case, so `Report.pdf` and `report.pdf` are one file there.
fn target_key(path: &str) -> String {
    if cfg!(any(target_os = "macos", windows)) {
        path.to_lowercase()
    } else {
        path.to_owned()
    }
}

/// What a transfer cut off by a dropped connection does next.
enum Reconnect {
    /// Go on with this SFTP session.
    Again(Arc<dyn SftpPort>),
    /// The login failed for now; try after the next wait.
    Later,
    /// The login was refused or cancelled: logging in again would only
    /// ask again or count against the account. The session needs the user.
    Stop,
}

/// A new login for a session whose connection dropped. One at a time per
/// session: a transfer that waited finds the one already made.
async fn reconnect(
    connections: &Connections,
    state: &Mutex<State>,
    session: u64,
    failed: &Arc<dyn SftpPort>,
) -> Reconnect {
    let gate = {
        let state = lock(state);
        let Some(open) = state.sessions.get(&session) else {
            return Reconnect::Stop;
        };
        Arc::clone(&open.reconnecting)
    };
    let _one = gate.lock().await;
    let (login, prompter) = {
        let state = lock(state);
        let Some(open) = state.sessions.get(&session) else {
            return Reconnect::Stop;
        };
        if open.gave_up {
            return Reconnect::Stop;
        }
        if !Arc::ptr_eq(&open.sftp, failed) {
            return Reconnect::Again(Arc::clone(&open.sftp));
        }
        (open.login.clone(), Arc::clone(&open.prompter))
    };
    let connection = match connections.acquire(&login, prompter).await {
        Ok(connection) => connection,
        Err(
            ConnectError::AuthFailed { .. }
            | ConnectError::Cancelled
            | ConnectError::HostKeyChanged { .. }
            | ConnectError::HostKeyRevoked { .. }
            | ConnectError::HostKeyRejected { .. },
        ) => {
            if let Some(open) = lock(state).sessions.get_mut(&session) {
                open.gave_up = true;
            }
            return Reconnect::Stop;
        }
        Err(_) => return Reconnect::Later,
    };
    let Ok(sftp) = connection.open_sftp().await else {
        return Reconnect::Later;
    };
    let mut state = lock(state);
    let Some(open) = state.sessions.get_mut(&session) else {
        return Reconnect::Stop;
    };
    open.sftp = Arc::clone(&sftp);
    Reconnect::Again(sftp)
}

fn set_status(state: &Mutex<State>, id: u64, status: TransferStatus) {
    if let Some(transfer) = lock(state).transfers.get_mut(&id) {
        transfer.status = status;
    }
}

/// Records how a transfer ended and drops the oldest finished ones past
/// [`KEEP_FINISHED`].
fn finish(state: &Mutex<State>, id: u64, status: TransferStatus) {
    let mut state = lock(state);
    let mut freed = None;
    if let Some(transfer) = state.transfers.get_mut(&id) {
        transfer.status = status;
        transfer.cancel = None;
        freed = transfer.writes.take();
    }
    if let Some(path) = freed {
        state.writing.remove(&path);
    }
    state.finished.push_back(id);
    while state.finished.len() > KEEP_FINISHED {
        if let Some(old) = state.finished.pop_front() {
            state.transfers.remove(&old);
        }
    }
}

#[cfg(test)]
mod tests;
