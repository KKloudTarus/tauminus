// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! When sync runs by itself: on unlock, every few minutes while the vault
//! is open, and a little after the lists change.

use std::sync::{Arc, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::{Instant, MissedTickBehavior};

use super::service::{Status, SyncService, SyncServiceError};

/// Between syncs while nothing changes here.
pub const EVERY: Duration = Duration::from_mins(5);
/// After an edit, for more edits to come before syncing them.
pub const SETTLE: Duration = Duration::from_secs(30);

impl SyncService {
    /// Runs syncs as the module says, until the books go; `kick` runs one
    /// at once, as after setup. `report` hears each run's status. A sync
    /// already running is not run again. The vault locking drops the
    /// target kept open.
    pub async fn keep_in_sync(self: Arc<Self>, kick: Arc<Notify>, report: impl Fn(Status) + Send) {
        let mut open = self.vault.watch().await;
        let mut hosts = self.hosts.changes();
        let mut snippets = self.snippets.changes();
        let mut every = tokio::time::interval_at(Instant::now() + EVERY, EVERY);
        every.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut due: Option<Instant> = None;
        loop {
            let settle = async {
                match due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = every.tick() => {}
                () = kick.notified() => {}
                () = settle => {
                    // A run since the edit, as Sync now, took it.
                    if self.revisions() == *self.settled.lock().unwrap_or_else(PoisonError::into_inner) {
                        due = None;
                        continue;
                    }
                }
                changed = open.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    if !*open.borrow_and_update() {
                        self.close();
                        continue;
                    }
                }
                changed = hosts.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    hosts.borrow_and_update();
                    if self.revisions() != *self.settled.lock().unwrap_or_else(PoisonError::into_inner) {
                        due = Some(Instant::now() + SETTLE);
                    }
                    continue;
                }
                changed = snippets.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    snippets.borrow_and_update();
                    if self.revisions() != *self.settled.lock().unwrap_or_else(PoisonError::into_inner) {
                        due = Some(Instant::now() + SETTLE);
                    }
                    continue;
                }
            }
            due = None;
            let result = self.run().await;
            // Nothing to do, or nothing new to tell.
            if matches!(
                result,
                Err(SyncServiceError::Off | SyncServiceError::Running | SyncServiceError::Vault(_))
            ) {
                continue;
            }
            every.reset();
            report(self.status().await);
        }
    }
}

#[cfg(test)]
mod tests;
