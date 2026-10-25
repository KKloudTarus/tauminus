// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Sync as the app offers it: set up with a passphrase, kept in the vault,
//! run on demand or on a schedule, with the outcome of the last run.

use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::SystemTime;

use tauminus_domain::Repair;
use thiserror::Error;
use tokio::sync::Mutex;

use super::{SyncRunError, Syncer};
use crate::ports::{Merged, Secret, SyncClash, SyncConnector, SyncError, SyncTarget};
use crate::trust::Trust;
use crate::vault::{SyncSetup, Vault, VaultError};
use crate::{HostBook, SnippetBook};

/// Whether this device syncs, as far as can be told now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Setup {
    /// The setup is in the vault.
    Locked,
    Off,
    On {
        git: bool,
        /// The folder, or the remote without any password or token in it.
        shown: String,
    },
}

/// What a sync that went through brought.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub clashes: Vec<SyncClash>,
    /// What was moved to keep the lists valid, as sentences.
    pub repairs: Vec<String>,
    pub notices: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub at: SystemTime,
    /// The saved hosts or snippets changed while it ran, by its merge or
    /// an edit: the view reads them again.
    pub changed: bool,
    /// The error's message when it failed.
    pub outcome: Result<Summary, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub setup: Setup,
    pub running: bool,
    pub last: Option<Report>,
}

#[derive(Debug, Error)]
pub enum SyncServiceError {
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error("sync is not set up on this device")]
    Off,
    #[error("this device already syncs; stop syncing first")]
    On,
    #[error("a sync is running")]
    Running,
    #[error(transparent)]
    Sync(#[from] SyncError),
    #[error(transparent)]
    Run(#[from] SyncRunError),
}

/// Sync, with the books it merges into and the vault holding its key.
pub struct SyncService {
    syncer: Syncer,
    pub(super) vault: Arc<Vault>,
    pub(super) hosts: Arc<HostBook>,
    pub(super) snippets: Arc<SnippetBook>,
    trust: Arc<Trust>,
    connector: Arc<dyn SyncConnector>,
    /// Setup, leaving, and runs, one at a time.
    busy: Mutex<()>,
    last: StdMutex<Option<Report>>,
    /// The books' revisions when the last run ended: the schedule takes
    /// a run's own saves for no edit.
    pub(super) settled: StdMutex<(u64, u64)>,
}

impl std::fmt::Debug for SyncService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncService").finish_non_exhaustive()
    }
}

impl SyncService {
    pub fn new(
        vault: Arc<Vault>,
        hosts: Arc<HostBook>,
        snippets: Arc<SnippetBook>,
        trust: Arc<Trust>,
        connector: Arc<dyn SyncConnector>,
    ) -> Self {
        Self {
            syncer: Syncer::default(),
            vault,
            hosts,
            snippets,
            trust,
            connector,
            busy: Mutex::new(()),
            last: StdMutex::new(None),
            settled: StdMutex::new((0, 0)),
        }
    }

    /// The books' revisions now.
    pub(super) fn revisions(&self) -> (u64, u64) {
        (
            *self.hosts.changes().borrow(),
            *self.snippets.changes().borrow(),
        )
    }

    pub async fn status(&self) -> Status {
        let setup = match self.vault.sync_setup().await {
            Err(_) => Setup::Locked,
            Ok(None) => Setup::Off,
            Ok(Some(setup)) => Setup::On {
                git: matches!(setup.target, SyncTarget::Git(_)),
                shown: shown(&setup.target),
            },
        };
        Status {
            setup,
            running: self.busy.try_lock().is_err(),
            last: self
                .last
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        }
    }

    /// Starts sync on an empty `target`, with a new key wrapped there
    /// under `passphrase`, and keeps both in the vault.
    ///
    /// # Errors
    /// The vault locked, sync already on, a short passphrase, or the
    /// target holding a key or failing.
    pub async fn create(
        &self,
        target: SyncTarget,
        passphrase: Secret,
    ) -> Result<(), SyncServiceError> {
        let _one = self.busy.lock().await;
        self.start().await?;
        let key = self.connector.create(&target, &passphrase).await?;
        self.keep(target, key).await
    }

    /// Joins the sync `target` holds, with its passphrase.
    ///
    /// # Errors
    /// The vault locked, sync already on, a wrong passphrase, or the
    /// target holding no key or failing.
    pub async fn join(
        &self,
        target: SyncTarget,
        passphrase: Secret,
    ) -> Result<(), SyncServiceError> {
        let _one = self.busy.lock().await;
        self.start().await?;
        let key = self.connector.join(&target, &passphrase).await?;
        self.keep(target, key).await
    }

    /// The vault open, sync off, and nothing kept from an earlier target:
    /// a device id and numbering from there would not fit this one.
    async fn start(&self) -> Result<(), SyncServiceError> {
        if self.vault.sync_setup().await?.is_some() {
            return Err(SyncServiceError::On);
        }
        self.connector.close();
        self.connector.wipe().await?;
        Ok(())
    }

    async fn keep(
        &self,
        target: SyncTarget,
        key: crate::ports::SyncKeyBytes,
    ) -> Result<(), SyncServiceError> {
        self.vault
            .save_sync_setup(Some(SyncSetup { target, key }))
            .await?;
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = None;
        Ok(())
    }

    /// Stops syncing here: the vault forgets the target and key first,
    /// then what this device kept about the target goes. The target and
    /// the other devices keep everything.
    ///
    /// # Errors
    /// The vault locked or not saving; the local state not going.
    pub async fn leave(&self) -> Result<(), SyncServiceError> {
        let _one = self.busy.lock().await;
        self.vault.save_sync_setup(None).await?;
        self.connector.close();
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.connector.wipe().await?;
        Ok(())
    }

    /// Syncs once, unless a sync is running, and keeps the outcome.
    ///
    /// # Errors
    /// [`SyncServiceError::Running`], the vault locked, sync off, or the
    /// sync failing.
    pub async fn run(&self) -> Result<Summary, SyncServiceError> {
        let Ok(_one) = self.busy.try_lock() else {
            return Err(SyncServiceError::Running);
        };
        let Some(setup) = self.vault.sync_setup().await? else {
            return Err(SyncServiceError::Off);
        };
        let before = self.revisions();
        let result = self.sync(&setup).await;
        let after = self.revisions();
        *self.settled.lock().unwrap_or_else(PoisonError::into_inner) = after;
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Some(Report {
            at: SystemTime::now(),
            changed: before != after,
            outcome: result
                .as_ref()
                .map(Clone::clone)
                .map_err(ToString::to_string),
        });
        result
    }

    async fn sync(&self, setup: &SyncSetup) -> Result<Summary, SyncServiceError> {
        let remote = self.connector.open(&setup.target, &setup.key)?;
        let merged = self
            .syncer
            .run(&self.hosts, &self.snippets, &self.trust, &*remote)
            .await?;
        Ok(summary(merged))
    }

    /// Drops the target kept open, with any token in its address: the
    /// vault locked.
    pub fn close(&self) {
        self.connector.close();
    }
}

fn summary(merged: Merged) -> Summary {
    let inventory = &merged.data.hosts;
    let repairs = merged
        .repairs
        .iter()
        .map(|repair| match repair {
            Repair::GroupToRoot(id) => format!(
                "group {} moved to the top: its parent was gone, too deep, or inside it",
                inventory.group(id).map_or(id.as_str(), |g| g.name.as_str())
            ),
            Repair::HostToRoot(id) => format!(
                "host {} moved to the top: its group was gone",
                inventory.host(id).map_or(id.as_str(), |h| h.label.as_str())
            ),
        })
        .collect();
    Summary {
        clashes: merged.clashes,
        repairs,
        notices: merged.notices,
    }
}

/// `target` as the user may see it: a remote's address loses whatever
/// stands before `@` in its authority, and its query and fragment, where
/// a password or a token goes.
#[must_use]
pub fn shown(target: &SyncTarget) -> String {
    match target {
        SyncTarget::Folder(path) => path.clone(),
        SyncTarget::Git(url) => {
            let Some((scheme, rest)) = url.split_once("://") else {
                // `user@host:path`: ssh, with no password in it.
                return url.as_str().to_owned();
            };
            let rest = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
            let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
            let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
            format!("{scheme}://{host}{path}")
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
