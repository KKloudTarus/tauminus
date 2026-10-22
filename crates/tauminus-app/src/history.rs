// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The command history: recording and searching, while the vault is open
//! or locked.
//!
//! With the vault open, commands go straight into the encrypted database.
//! With it locked, each is sealed to the history's public key and kept in
//! a pending file that only the vault's private key opens; the next unlock
//! moves them into the database. Searching needs the vault open.

use std::sync::{Arc, Weak};

use tauminus_domain::{HistoryEntry, HistoryHit, HistoryQuery, Retention};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::vault::{HistoryKeys, Vault, VaultError};

/// Prune after this many inserts, so the cap holds without a full scan per
/// command.
const PRUNE_EVERY: u32 = 500;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HistoryError {
    /// Search needs the vault open.
    #[error("unlock the vault to search the history")]
    Locked,
    /// No vault yet, so nowhere to keep history encrypted.
    #[error("create a vault to keep a history")]
    NoVault,
    #[error("{0}")]
    Invalid(String),
    #[error("the history: {0}")]
    Store(String),
}

impl From<VaultError> for HistoryError {
    fn from(error: VaultError) -> Self {
        match error {
            VaultError::Locked => Self::Locked,
            VaultError::Missing => Self::NoVault,
            other => Self::Store(other.to_string()),
        }
    }
}

/// What happened to a recorded command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    /// In the database.
    Stored,
    /// Sealed in the pending file until the next unlock.
    Sealed,
    /// Not kept: no vault, an empty command, or a full pending file.
    Dropped,
}

/// The encrypted database, open.
pub trait HistoryStore: Send + Sync {
    /// Stores `entries` when `open` still holds once the database's writer
    /// is taken; `false` when it does not, and nothing was stored.
    fn insert(
        &self,
        entries: &[HistoryEntry],
        open: &dyn Fn() -> bool,
    ) -> Result<bool, HistoryError>;
    fn search(&self, query: &HistoryQuery) -> Result<Vec<HistoryHit>, HistoryError>;
    fn output(&self, id: i64) -> Result<Option<String>, HistoryError>;
    fn prune(&self, retention: Retention, now_ms: i64) -> Result<u64, HistoryError>;
    fn clear(&self, host: Option<&str>) -> Result<u64, HistoryError>;
}

/// Where the history lives: the database and the pending file.
pub trait HistoryBackend: Send + Sync + 'static {
    fn open(&self, keys: &HistoryKeys) -> Result<Arc<dyn HistoryStore>, HistoryError>;
    /// Seals `entry` to `public` in the pending file; `false` when full.
    fn seal(&self, public: &[u8; 32], entry: &HistoryEntry) -> Result<bool, HistoryError>;
    /// Hands the pending entries `keys` opens to `store`, then, once it
    /// succeeds, removes them from the pending file, all as one step that
    /// another window cannot interleave with. Returns how many entries would
    /// not open (damaged, or sealed to another key).
    fn import(
        &self,
        keys: &HistoryKeys,
        store: &mut dyn FnMut(&[HistoryEntry]) -> Result<(), HistoryError>,
    ) -> Result<usize, HistoryError>;
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> i64;
}

struct State {
    db: Option<Arc<dyn HistoryStore>>,
    since_prune: u32,
}

/// Records and searches the history. Recording and moving pending entries
/// share one lock, so an entry sealed while an unlock imports the pending
/// file is never cleared unread.
pub struct History {
    backend: Arc<dyn HistoryBackend>,
    vault: Arc<Vault>,
    retention: Retention,
    state: Mutex<State>,
    /// The vault's open state, updated as the vault locks: every use checks
    /// it, so nothing goes into the database after a lock even before the
    /// follower closes it.
    unlocked: tokio::sync::watch::Receiver<bool>,
    /// A prune is running: another would only wait on it.
    pruning: Arc<std::sync::atomic::AtomicBool>,
}

impl std::fmt::Debug for History {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("History").finish_non_exhaustive()
    }
}

impl History {
    /// The history, following the vault: it opens on unlock and closes on
    /// lock. The task that follows stops when the history is dropped.
    pub async fn new(
        backend: Arc<dyn HistoryBackend>,
        vault: Arc<Vault>,
        retention: Retention,
    ) -> Arc<Self> {
        let open = vault.watch().await;
        let history = Arc::new(Self {
            backend,
            vault: Arc::clone(&vault),
            retention,
            state: Mutex::new(State {
                db: None,
                since_prune: 0,
            }),
            unlocked: open.clone(),
            pruning: Arc::default(),
        });
        tokio::spawn(follow(Arc::downgrade(&history), open));
        history
    }

    /// Keeps `entry`: in the database while the vault is open, sealed
    /// otherwise. Commands are typed by people, so one at a time is fine.
    pub async fn record(&self, entry: HistoryEntry) -> Result<Recorded, HistoryError> {
        // Up to 64 KiB scanned for secrets: off the async workers.
        let entry = blocking(move || Ok(entry.bounded())).await?;
        if entry.command.is_empty() {
            return Ok(Recorded::Dropped);
        }
        let db = {
            let mut state = self.state.lock().await;
            match self.open_locked(&mut state).await {
                Ok(db) => db,
                // The database will not open: keep the command sealed instead.
                Err(e) => {
                    tracing::warn!(error = %e, "history did not open; sealing the command");
                    None
                }
            }
        };
        // The state is not held while the insert waits for the writer, which
        // a forget may hold for seconds, so searches go on; the vault is
        // checked again once the writer is taken.
        if let Some(db) = db {
            let batch = [entry.clone()];
            let unlocked = self.unlocked.clone();
            let inserted = blocking({
                let db = Arc::clone(&db);
                move || db.insert(&batch, &|| *unlocked.borrow())
            })
            .await;
            match inserted {
                Ok(true) => {
                    let prune = {
                        let mut state = self.state.lock().await;
                        state.since_prune += 1;
                        let prune = state.since_prune >= PRUNE_EVERY;
                        if prune {
                            state.since_prune = 0;
                        }
                        prune
                    };
                    if prune {
                        self.prune(db);
                    }
                    return Ok(Recorded::Stored);
                }
                // The vault locked while the insert waited: sealed instead.
                Ok(false) => {}
                // A full or failing disk: keep the command sealed instead.
                Err(e) => tracing::warn!(error = %e, "history insert failed; sealing the command"),
            }
        }
        let Some(public) = self.vault.history_public().await else {
            return Ok(Recorded::Dropped);
        };
        let backend = Arc::clone(&self.backend);
        let sealed = blocking(move || backend.seal(&public, &entry)).await?;
        Ok(if sealed {
            Recorded::Sealed
        } else {
            tracing::warn!("history pending file full; a command was not kept");
            Recorded::Dropped
        })
    }

    /// Newest first. Needs the vault open.
    pub async fn search(&self, query: HistoryQuery) -> Result<Vec<HistoryHit>, HistoryError> {
        query
            .check()
            .map_err(|e| HistoryError::Invalid(e.to_string()))?;
        let db = self.db().await?;
        blocking(move || db.search(&query)).await
    }

    /// One command's full output.
    pub async fn output(&self, id: i64) -> Result<Option<String>, HistoryError> {
        let db = self.db().await?;
        blocking(move || db.output(id)).await
    }

    /// Forgets one saved host's commands, or all of them.
    pub async fn clear(&self, host: Option<String>) -> Result<u64, HistoryError> {
        let db = self.db().await?;
        blocking(move || db.clear(host.as_deref())).await
    }

    async fn db(&self) -> Result<Arc<dyn HistoryStore>, HistoryError> {
        let mut state = self.state.lock().await;
        self.open_locked(&mut state)
            .await?
            .ok_or(HistoryError::Locked)
    }

    /// The database, opening it if the vault is open: the pending file is
    /// imported, then the retention applied. `None` while locked.
    async fn open_locked(
        &self,
        state: &mut State,
    ) -> Result<Option<Arc<dyn HistoryStore>>, HistoryError> {
        // The vault as it is now, idle time included, not as last told.
        if !*self.unlocked.borrow() || !self.vault.is_open().await {
            state.db = None;
            return Ok(None);
        }
        if let Some(db) = &state.db {
            return Ok(Some(Arc::clone(db)));
        }
        let keys = match self.vault.history_keys().await {
            Ok(keys) => keys,
            Err(VaultError::Locked | VaultError::Missing) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let backend = Arc::clone(&self.backend);
        let db = blocking(move || {
            let db = backend.open(&keys)?;
            let unreadable = backend.import(&keys, &mut |entries| {
                tracing::info!(imported = entries.len(), "history pending entries imported");
                db.insert(entries, &|| true).map(|_| ())
            })?;
            if unreadable > 0 {
                tracing::warn!(unreadable, "history pending entries could not be opened");
            }
            Ok(db)
        })
        .await?;
        state.db = Some(Arc::clone(&db));
        self.prune(Arc::clone(&db));
        Ok(Some(db))
    }

    /// Applies the retention in the background: it may take seconds on a
    /// large history, and records and searches take turns with it. One at
    /// a time.
    fn prune(&self, db: Arc<dyn HistoryStore>) {
        use std::sync::atomic::Ordering;
        if self.pruning.swap(true, Ordering::AcqRel) {
            return;
        }
        let pruning = Arc::clone(&self.pruning);
        let retention = self.retention;
        let now = self.backend.now_ms();
        tokio::task::spawn_blocking(move || {
            match db.prune(retention, now) {
                Ok(0) => {}
                Ok(removed) => tracing::info!(removed, "history pruned"),
                Err(e) => tracing::warn!(error = %e, "history prune failed"),
            }
            pruning.store(false, Ordering::Release);
        });
    }

    async fn close(&self) {
        self.state.lock().await.db = None;
    }
}

/// Opens the history when the vault unlocks and closes it when it locks.
async fn follow(history: Weak<History>, mut open: tokio::sync::watch::Receiver<bool>) {
    while open.changed().await.is_ok() {
        let unlocked = *open.borrow_and_update();
        let Some(history) = history.upgrade() else {
            return;
        };
        if unlocked {
            let mut state = history.state.lock().await;
            if let Err(e) = history.open_locked(&mut state).await {
                tracing::warn!(error = %e, "history did not open");
            }
        } else {
            history.close().await;
        }
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, HistoryError> + Send + 'static,
) -> Result<T, HistoryError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| HistoryError::Store(e.to_string()))?
}

#[cfg(test)]
mod tests;
