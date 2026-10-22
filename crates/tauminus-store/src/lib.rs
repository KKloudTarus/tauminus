// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Saved hosts in one JSON file.
//!
//! A save writes a temporary file next to the real one, flushes it to disk,
//! and renames it over the old file, so a crash or a full disk leaves either
//! the old list or the new one. The file is private to the user (0600 on
//! Unix): host names and jump hosts describe a network.
//!
//! A file that does not parse is reported and never overwritten, so a bug
//! or a newer version cannot destroy a user's list by saving over it.

mod format;
pub mod history;
mod history_backend;
mod keychain;
pub mod vault;
mod vault_backend;

pub use history_backend::FileHistoryBackend;
pub use keychain::OsKeychain;
pub use vault_backend::{FileVaultBackend, KeyStore};

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use rand::TryRng;
use tauminus_app::ports::{BoxFuture, HostStore, StoreError};
use tauminus_domain::Inventory;

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Largest file read or written. 10 000 typical hosts take 2.5 MB; every
/// field at its limit takes about 2 KB per host.
pub(crate) const MAX_FILE: u64 = 16 * 1024 * 1024;

/// What the file held when this store last read or wrote it: a hash of its
/// bytes, or `None` when it did not exist.
type Seen = Option<u64>;

/// Hosts kept in `path`, usually `hosts.json` in the app's config directory.
///
/// Before each write the file is read again; if it no longer holds what
/// this store last saw, another window changed it, and the write is refused
/// with [`StoreError::Conflict`] instead of replacing that change. The check
/// costs one read of the file per save.
#[derive(Debug, Clone)]
pub struct JsonHostStore {
    path: PathBuf,
    /// `None` until the first load or save.
    seen: Arc<Mutex<Option<Seen>>>,
}

impl JsonHostStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            seen: Arc::default(),
        }
    }
}

impl HostStore for JsonHostStore {
    fn load(&self) -> BoxFuture<'_, Result<Inventory, StoreError>> {
        let path = self.path.clone();
        let seen = Arc::clone(&self.seen);
        Box::pin(async move {
            blocking(move || {
                let bytes = read_limited(&path)?;
                *lock(&seen) = Some(bytes.as_deref().map(fingerprint));
                match bytes {
                    None => Ok(Inventory::default()),
                    Some(bytes) => format::from_json(&bytes).map_err(StoreError::Corrupt),
                }
            })
            .await
        })
    }

    fn save<'a>(&'a self, inventory: &'a Inventory) -> BoxFuture<'a, Result<(), StoreError>> {
        // Serialize here, write on the blocking pool. O(H + G).
        let bytes = format::to_json(inventory);
        let path = self.path.clone();
        let seen = Arc::clone(&self.seen);
        let temp_name = self.new_id();
        Box::pin(async move {
            let bytes = bytes.map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
            // A file that `load` would refuse must not replace one it reads.
            if bytes.len() as u64 > MAX_FILE {
                return Err(StoreError::Io(std::io::Error::other(format!(
                    "the host list would take {} MiB; the limit is {} MiB",
                    bytes.len() / 1024 / 1024,
                    MAX_FILE / 1024 / 1024
                ))));
            }
            blocking(move || {
                let mut seen = lock(&seen);
                if let Some(expected) = *seen {
                    let current = read_limited(&path)?.as_deref().map(fingerprint);
                    if current != expected {
                        return Err(StoreError::Conflict);
                    }
                }
                write_atomic(&path, &bytes, &temp_name)?;
                *seen = Some(Some(fingerprint(&bytes)));
                Ok(())
            })
            .await
        })
    }

    fn new_id(&self) -> String {
        let mut bytes = [0u8; 16];
        // The OS random source; it fails only on a broken system, and a
        // time-based id is still unique enough for one user's list.
        if rand::rngs::SysRng.try_fill_bytes(&mut bytes).is_err() {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            bytes = nanos.to_le_bytes();
        }
        let mut id = String::with_capacity(32);
        for b in bytes {
            id.push(char::from(HEX[usize::from(b >> 4)]));
            id.push(char::from(HEX[usize::from(b & 0x0f)]));
        }
        id
    }
}

fn lock(seen: &Mutex<Option<Seen>>) -> std::sync::MutexGuard<'_, Option<Seen>> {
    seen.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, StoreError> + Send + 'static,
) -> Result<T, StoreError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| StoreError::Io(std::io::Error::other(e)))?
}

/// The file's bytes, or `None` when it does not exist. Reads at most one
/// byte past the limit from the handle it opened, so swapping the file after
/// a size check cannot get a larger one read.
fn read_limited(path: &Path) -> Result<Option<Vec<u8>>, StoreError> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(StoreError::Corrupt(format!(
            "{} is larger than {} MiB",
            path.display(),
            MAX_FILE / 1024 / 1024
        )));
    }
    Ok(Some(bytes))
}

/// Writes `bytes` to `path` so that readers see the old file or the new one.
/// The temporary file has an unguessable name and is created exclusively,
/// which also refuses to follow a symlink planted at that name.
fn write_atomic(path: &Path, bytes: &[u8], temp_name: &str) -> Result<(), StoreError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    let name = path
        .file_name()
        .map_or_else(|| "hosts".into(), |n| n.to_string_lossy());
    let temp = dir.join(format!(".{name}.{temp_name}.tmp"));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        // The rename is durable only once the directory entry is on disk.
        #[cfg(unix)]
        std::fs::File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(StoreError::Io)
}

#[cfg(test)]
mod tests;
