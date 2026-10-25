// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Sync with the user's other devices (ADR 0030): merge the saved lists
//! with theirs, save the result here unless an edit came in meanwhile,
//! then publish it. No lock is held while the target is read or written.

use thiserror::Error;
use tokio::sync::Mutex;

use std::sync::Arc;

use crate::ports::{KeyDeriver, Merged, StoreError, SyncData, SyncError, SyncPart, SyncRemote};
use crate::trust::Prints;
use crate::trust::Trust;
use crate::vault::Vault;
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
    #[error(transparent)]
    Vault(#[from] crate::vault::VaultError),
}

impl From<StoreError> for SyncRunError {
    fn from(error: StoreError) -> Self {
        Self::Hosts(HostBookError::Store(error))
    }
}

/// The vault identities a sync carries: those this device shares, and
/// how it derives a key's public half.
#[derive(Clone)]
pub struct SharedIdentities<'a> {
    pub vault: &'a Vault,
    pub deriver: Arc<dyn KeyDeriver>,
}

impl std::fmt::Debug for SharedIdentities<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedIdentities").finish_non_exhaustive()
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
    /// times. The identities go first, then the hosts, then the snippets,
    /// each adopted once saved. O(H·(d + k) + G + S + I) plus the target.
    ///
    /// # Errors
    /// The target, its objects, a newer device, invalid merged lists, the
    /// vault locked, or a save failing.
    pub async fn run(
        &self,
        hosts: &HostBook,
        snippets: &SnippetBook,
        identities: Option<&SharedIdentities<'_>>,
        trust: &Trust,
        remote: &dyn SyncRemote,
    ) -> Result<Merged, SyncRunError> {
        let _one = self.running.lock().await;
        for _ in 0..ATTEMPTS {
            let local = SyncData {
                hosts: hosts.snapshot().await,
                snippets: snippets.snapshot().await,
                identities: match identities {
                    Some(shared) => shared.vault.synced_identities().await?,
                    None => Vec::new(),
                },
            };
            let mut merged = remote.merge(local.clone()).await?;
            let (before, after) = match identities {
                Some(shared) => {
                    // Read under one lock with the check that the shared
                    // identities are still what the merge began with.
                    match shared
                        .vault
                        .sync_prints(local.identities.clone(), merged.data.identities.clone())
                        .await?
                    {
                        Some(prints) => prints,
                        None => continue,
                    }
                }
                None => (Prints::new(), Prints::new()),
            };
            // Noted before anything is saved: a crash after the save
            // leaves no change unseen.
            trust
                .note((&local, &before), (&merged.data, &after))
                .await
                .map_err(SyncRunError::Review)?;
            // Each part saved is adopted at once: a sync that stops after
            // this compares against what this device holds, so it never
            // takes another device's values for edits of its own.
            if let Some(shared) = identities {
                match shared
                    .vault
                    .apply_synced(
                        local.identities.clone(),
                        merged.data.identities.clone(),
                        Arc::clone(&shared.deriver),
                    )
                    .await?
                {
                    Some(applied) => {
                        merged.notices.extend(applied.notices);
                        if !applied.refused.is_empty() {
                            remote.decline(applied.refused).await?;
                        }
                    }
                    None => continue,
                }
            }
            remote.adopt(SyncPart::Identities).await?;
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
