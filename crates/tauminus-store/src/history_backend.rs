// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The history's files: `history.db` (SQLCipher) and `history.pending`
//! (entries sealed while the vault is locked), next to the vault.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauminus_app::history::{HistoryBackend, HistoryError, HistoryStore};
use tauminus_app::vault::HistoryKeys;
use tauminus_domain::{HistoryEntry, HistoryHit, HistoryQuery, Retention};

use crate::history::{HistoryDb, HistoryError as DbError, HistoryKeyPair, PendingLog};
use crate::vault::SecretKey;

/// The history in `dir`, usually the app's config directory.
#[derive(Debug)]
pub struct FileHistoryBackend {
    db: PathBuf,
    pending: PendingLog,
}

impl FileHistoryBackend {
    pub fn new(dir: &Path) -> Self {
        Self {
            db: dir.join("history.db"),
            pending: PendingLog::new(dir.join("history.pending")),
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "map_err hands over the error by value"
)]
fn store_error(error: DbError) -> HistoryError {
    HistoryError::Store(error.to_string())
}

fn secret(bytes: &[u8; 32]) -> Result<SecretKey, HistoryError> {
    SecretKey::from_bytes(bytes).map_err(|e| HistoryError::Store(e.to_string()))
}

struct Open(HistoryDb);

impl HistoryStore for Open {
    fn insert(
        &self,
        entries: &[HistoryEntry],
        open: &dyn Fn() -> bool,
    ) -> Result<bool, HistoryError> {
        self.0
            .insert_while(entries, now_ms(), open)
            .map_err(store_error)
    }

    fn search(&self, query: &HistoryQuery) -> Result<Vec<HistoryHit>, HistoryError> {
        self.0.search(query).map_err(store_error)
    }

    fn output(&self, id: i64) -> Result<Option<String>, HistoryError> {
        self.0.output(id).map_err(store_error)
    }

    fn prune(&self, retention: Retention, now_ms: i64) -> Result<u64, HistoryError> {
        self.0.prune(retention, now_ms).map_err(store_error)
    }

    fn clear(&self, host: Option<&str>) -> Result<u64, HistoryError> {
        self.0.clear(host).map_err(store_error)
    }
}

impl HistoryBackend for FileHistoryBackend {
    /// A database the key does not open was made under another vault: it
    /// is set aside, renamed, and a new one started, so recording goes on.
    fn open(&self, keys: &HistoryKeys) -> Result<Arc<dyn HistoryStore>, HistoryError> {
        let key = secret(&keys.database)?;
        match HistoryDb::open(&self.db, &key) {
            Ok(db) => Ok(Arc::new(Open(db))),
            Err(DbError::WrongKey) => {
                let aside = self
                    .db
                    .with_extension(format!("db.{}.unreadable", self.now_ms()));
                tracing::warn!(
                    path = %aside.display(),
                    "the history file belongs to another vault; starting a new one"
                );
                // The log and index go with it, so the copy keeps every
                // commit and the new file starts without them.
                for suffix in ["-wal", "-shm"] {
                    let with = |path: &Path| {
                        let mut name = path.as_os_str().to_owned();
                        name.push(suffix);
                        PathBuf::from(name)
                    };
                    let _ = std::fs::rename(with(&self.db), with(&aside));
                }
                std::fs::rename(&self.db, &aside)
                    .map_err(|e| HistoryError::Store(e.to_string()))?;
                let db = HistoryDb::open(&self.db, &key).map_err(store_error)?;
                Ok(Arc::new(Open(db)))
            }
            Err(e) => Err(store_error(e)),
        }
    }

    fn seal(&self, public: &[u8; 32], entry: &HistoryEntry) -> Result<bool, HistoryError> {
        self.pending.append(public, entry).map_err(store_error)
    }

    fn import(
        &self,
        keys: &HistoryKeys,
        store: &mut dyn FnMut(&[HistoryEntry]) -> Result<(), HistoryError>,
    ) -> Result<usize, HistoryError> {
        let pair = HistoryKeyPair::from_secret(secret(&keys.sealing)?);
        // The store's error crosses the file's own error type and back.
        let mut failed = None;
        let unreadable = self
            .pending
            .import(&pair, &mut |entries| {
                store(entries).map_err(|e| {
                    let text = e.to_string();
                    failed = Some(e);
                    DbError::Corrupt(text)
                })
            })
            .map_err(store_error);
        match failed {
            Some(e) => Err(e),
            None => unreadable,
        }
    }

    fn now_ms(&self) -> i64 {
        now_ms()
    }
}

#[cfg(test)]
mod tests;
