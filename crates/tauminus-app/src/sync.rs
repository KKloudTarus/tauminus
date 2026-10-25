// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Sync with the user's other devices (ADR 0030): merge the saved lists
//! with theirs, save the result here unless an edit came in meanwhile,
//! then publish it. No lock is held while the target is read or written.

use thiserror::Error;
use tokio::sync::Mutex;

use crate::ports::{Merged, StoreError, SyncData, SyncError, SyncPart, SyncRemote};
use crate::trust::Trust;
use crate::{HostBook, HostBookError, SnippetBook, SnippetBookError};

/// Merges tried before giving up to edits that keep coming in.
const ATTEMPTS: usize = 3;

#[derive(Debug, Error)]
pub enum SyncRunError {
    #[error(transparent)]
    Sync(#[from] SyncError),
    #[error(transparent)]
    Hosts(#[from] HostBookError),
    #[error(transparent)]
    Snippets(#[from] SnippetBookError),
    #[error("the lists kept changing while syncing; try again")]
    Busy,
    #[error("the sync review list could not be saved: {0}")]
    Review(StoreError),
}

impl From<StoreError> for SyncRunError {
    fn from(error: StoreError) -> Self {
        Self::Hosts(HostBookError::Store(error))
    }
}

/// One sync at a time.
#[derive(Debug, Default)]
pub struct Syncer {
    running: Mutex<()>,
}

impl Syncer {
    /// Merges, notes what the merge changes for `trust`, saves, publishes.
    /// A list edited while the merge ran is merged again, at most three
    /// times. O(H·(d + Σ startup) + G + S) plus the target.
    ///
    /// # Errors
    /// The target, its objects, a newer device, invalid merged lists, or
    /// a save failing.
    pub async fn run(
        &self,
        hosts: &HostBook,
        snippets: &SnippetBook,
        trust: &Trust,
        remote: &dyn SyncRemote,
    ) -> Result<Merged, SyncRunError> {
        let _one = self.running.lock().await;
        for _ in 0..ATTEMPTS {
            let local = SyncData {
                hosts: hosts.snapshot().await,
                snippets: snippets.snapshot().await,
            };
            let merged = remote.merge(local.clone()).await?;
            // Noted before anything is saved: a crash after the save
            // leaves no change unseen.
            trust
                .note(&local, &merged.data)
                .await
                .map_err(SyncRunError::Review)?;
            // Each list saved is adopted at once: a sync that stops after
            // this compares against what this device holds, so it never
            // takes another device's values for edits of its own.
            if !hosts
                .replace_if(&local.hosts, merged.data.hosts.clone())
                .await?
            {
                continue;
            }
            remote.adopt(SyncPart::Hosts).await?;
            if !snippets
                .replace_if_hosts(
                    &local.snippets,
                    merged.data.snippets.clone(),
                    hosts,
                    &merged.data.hosts,
                )
                .await?
            {
                continue;
            }
            remote.adopt(SyncPart::Snippets).await?;
            remote.publish().await?;
            return Ok(merged);
        }
        Err(SyncRunError::Busy)
    }
}

mod schedule;
mod service;
pub use schedule::{EVERY, SETTLE};
pub use service::{Report, Setup, Status, Summary, SyncService, SyncServiceError, Through, shown};

#[cfg(test)]
mod tests;
