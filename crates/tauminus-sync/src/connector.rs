// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The app's way to its sync targets: a folder or a git remote, and this
//! device's state for it in one folder of its own, `state` and `git`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use tauminus_app::ports::{
    BoxFuture, Secret, SyncConnector, SyncError, SyncKeyBytes, SyncRemote, SyncTarget,
};
use zeroize::Zeroizing;

use crate::engine::{Remote, create, join};
use crate::git::Git;
use crate::seal::{Kdf, SyncKey};
use crate::target::{Folder, Target};

/// Keeps the target last opened: a git remote remembers the tip it
/// refused, and its clone, from one sync to the next.
pub struct Connector {
    dir: PathBuf,
    kdf: Kdf,
    open: Mutex<Option<(SyncTarget, Arc<dyn Target>)>>,
}

impl std::fmt::Debug for Connector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connector")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

fn target_error(error: &std::io::Error) -> SyncError {
    SyncError::Target(error.to_string())
}

impl Connector {
    /// This device's sync state goes in `dir`, which nothing else uses.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            kdf: Kdf::DEFAULT,
            open: Mutex::new(None),
        }
    }

    /// Cheaper Argon2id, for tests.
    #[must_use]
    pub fn with_kdf(mut self, kdf: Kdf) -> Self {
        self.kdf = kdf;
        self
    }

    fn target(&self, spec: &SyncTarget) -> Result<Arc<dyn Target>, SyncError> {
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((kept, target)) = open.as_ref()
            && kept == spec
        {
            return Ok(Arc::clone(target));
        }
        let target: Arc<dyn Target> = match spec {
            SyncTarget::Folder(path) => {
                let path = PathBuf::from(path);
                // A share not mounted yet looks like an empty folder, or
                // none: refuse rather than start sync afresh in it.
                if !path.is_absolute() || !path.is_dir() {
                    return Err(SyncError::Target(format!(
                        "{} is not a folder here",
                        path.display()
                    )));
                }
                Arc::new(Folder::new(path))
            }
            SyncTarget::Git(url) => {
                Arc::new(Git::new(url, self.dir.join("git")).map_err(|e| target_error(&e))?)
            }
        };
        *open = Some((spec.clone(), Arc::clone(&target)));
        Ok(target)
    }

    /// Runs `work` with `spec`'s target on a blocking thread: Argon2id and
    /// the network.
    async fn blocking<T: Send + 'static>(
        &self,
        spec: &SyncTarget,
        work: impl FnOnce(&dyn Target) -> Result<T, SyncError> + Send + 'static,
    ) -> Result<T, SyncError> {
        let target = self.target(spec)?;
        tokio::task::spawn_blocking(move || work(&*target))
            .await
            .map_err(|e| SyncError::Target(e.to_string()))?
    }
}

fn key_bytes(key: &SyncKey) -> SyncKeyBytes {
    Zeroizing::new(*key.bytes())
}

impl SyncConnector for Connector {
    fn create<'a>(
        &'a self,
        target: &'a SyncTarget,
        passphrase: &'a Secret,
    ) -> BoxFuture<'a, Result<SyncKeyBytes, SyncError>> {
        let passphrase = passphrase.clone();
        let kdf = self.kdf;
        Box::pin(self.blocking(target, move |target| {
            create(target, &passphrase, kdf).map(|key| key_bytes(&key))
        }))
    }

    fn join<'a>(
        &'a self,
        target: &'a SyncTarget,
        passphrase: &'a Secret,
    ) -> BoxFuture<'a, Result<SyncKeyBytes, SyncError>> {
        let passphrase = passphrase.clone();
        Box::pin(self.blocking(target, move |target| {
            join(target, &passphrase).map(|key| key_bytes(&key))
        }))
    }

    fn open(
        &self,
        target: &SyncTarget,
        key: &SyncKeyBytes,
    ) -> Result<Arc<dyn SyncRemote>, SyncError> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&self.dir).map_err(|e| target_error(&e))?;
        Ok(Arc::new(Remote::new(
            self.target(target)?,
            SyncKey::from_bytes(**key),
            self.dir.join("state"),
        )))
    }

    fn close(&self) {
        *self.open.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn wipe(&self) -> BoxFuture<'_, Result<(), SyncError>> {
        self.close();
        let dir = self.dir.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || match std::fs::remove_dir_all(&dir) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(target_error(&e)),
                _ => Ok(()),
            })
            .await
            .map_err(|e| SyncError::Target(e.to_string()))?
        })
    }
}

#[cfg(test)]
mod tests;
