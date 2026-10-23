// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The saved highlight and trigger sets, and the compiled rules each open
//! pane matches with.
//!
//! A pane asks once for the rules its host names ([`TriggerBook::prepare`])
//! and gets a handle; each scan then names the handle, so a scan compiles
//! nothing and reads no host. A change to the library makes every handle
//! stale: a scan with one answers `None`, and the pane prepares again.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};

use tauminus_domain::{
    Action, Fired, Highlight, Hit, HostsError, Label, Matcher, TriggerError, TriggerLibrary,
    TriggerMatcher, TriggerSetId,
};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::ports::{StoreError, TriggerStore};

/// Compiled rule sets kept at once. Panes with the same sets share one, so
/// this is the number of distinct host setups open, not of panes.
const PREPARED: usize = 64;

#[derive(Debug, Error)]
pub enum TriggerBookError {
    #[error(transparent)]
    Trigger(#[from] TriggerError),
    #[error(transparent)]
    Id(#[from] HostsError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What a pane gets back from [`TriggerBook::prepare`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub handle: u32,
    /// How each highlight rule colours, by the index hits carry. Empty when
    /// none applies: the pane need not scan its screen.
    pub styles: Vec<Highlight>,
    /// Each trigger rule's name and action, by the index fired triggers
    /// carry. Empty when none applies: the pane need not watch its output.
    pub triggers: Vec<(Label, Action)>,
}

struct Entry {
    handle: u32,
    revision: u32,
    named: Vec<TriggerSetId>,
    matcher: Arc<Matcher>,
    triggers: Arc<TriggerMatcher>,
}

/// The saved sets. As with [`crate::SnippetBook`], every change is saved
/// before it becomes visible, and changes are serialized.
pub struct TriggerBook {
    store: Arc<dyn TriggerStore>,
    library: Mutex<TriggerLibrary>,
    /// Bumped on every change; compiled rules from an older revision are
    /// stale.
    revision: AtomicU32,
    next_handle: AtomicU32,
    /// Least recently prepared first; at most [`PREPARED`].
    prepared: StdMutex<Vec<Entry>>,
    /// Held while compiling, so panes that ask for the same sets at once
    /// (all of them, after an edit) compile them once.
    compiling: Mutex<()>,
    /// Sets that failed to compile together, at a revision: asked again,
    /// they fail at once instead of compiling again. At most [`PREPARED`].
    failed: StdMutex<Vec<(u32, Vec<TriggerSetId>, TriggerError)>>,
}

impl std::fmt::Debug for TriggerBook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TriggerBook").finish_non_exhaustive()
    }
}

impl TriggerBook {
    pub async fn open(store: Arc<dyn TriggerStore>) -> Result<Self, StoreError> {
        let library = store.load().await?;
        Ok(Self {
            store,
            library: Mutex::new(library),
            revision: AtomicU32::new(0),
            next_handle: AtomicU32::new(1),
            prepared: StdMutex::new(Vec::new()),
            failed: StdMutex::new(Vec::new()),
            compiling: Mutex::new(()),
        })
    }

    pub async fn snapshot(&self) -> TriggerLibrary {
        self.library.lock().await.clone()
    }

    pub fn new_id(&self) -> Result<TriggerSetId, HostsError> {
        TriggerSetId::parse(&self.store.new_id())
    }

    /// Applies `change` to a copy, saves it, then publishes it; every
    /// compiled handle goes stale.
    pub async fn edit(
        &self,
        change: impl FnOnce(&mut TriggerLibrary) -> Result<(), TriggerError>,
    ) -> Result<TriggerLibrary, TriggerBookError> {
        let mut current = self.library.lock().await;
        let mut next = current.clone();
        change(&mut next)?;
        match self.store.save(&next).await {
            Ok(()) => {}
            Err(StoreError::Conflict) => {
                // Take the other window's library, so the next try builds on it.
                *current = self.store.load().await?;
                self.revision.fetch_add(1, Ordering::AcqRel);
                return Err(StoreError::Conflict.into());
            }
            Err(e) => return Err(e.into()),
        }
        *current = next.clone();
        self.revision.fetch_add(1, Ordering::AcqRel);
        self.entries().clear();
        self.failures().clear();
        Ok(next)
    }

    /// Compiles the rules for a pane whose host and groups name `named`,
    /// nearest first, or reuses those compiled for the same sets. Compiling
    /// runs off the async threads: a large set takes a moment.
    pub async fn prepare(&self, named: Vec<TriggerSetId>) -> Result<Prepared, TriggerBookError> {
        // One compile at a time: a pane that waited finds what the one
        // before it compiled.
        let _compiling = self.compiling.lock().await;
        let library = self.library.lock().await;
        let revision = self.revision.load(Ordering::Acquire);
        if let Some(prepared) = self.reuse(revision, &named) {
            return Ok(prepared);
        }
        if let Some((_, _, error)) = self
            .failures()
            .iter()
            .find(|(at, sets, _)| *at == revision && *sets == named)
        {
            return Err(error.clone().into());
        }
        let rules: Vec<_> = library.applied(&named).into_iter().cloned().collect();
        drop(library);
        let compiled = tokio::task::spawn_blocking(move || {
            let refs: Vec<_> = rules.iter().collect();
            Ok::<_, TriggerError>((Matcher::new(&refs)?, TriggerMatcher::new(&refs)?))
        })
        .await
        .map_err(|e| TriggerError::TooLarge(e.to_string()))?;
        let (matcher, triggers) = match compiled {
            Ok(both) => both,
            Err(error) => {
                let mut failures = self.failures();
                if failures.len() == PREPARED {
                    failures.remove(0);
                }
                failures.push((revision, named, error.clone()));
                return Err(error.into());
            }
        };
        let prepared_triggers = triggers.actions();
        let styles = matcher.styles();
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        let mut entries = self.entries();
        if entries.len() == PREPARED {
            entries.remove(0);
        }
        entries.push(Entry {
            handle,
            revision,
            named,
            matcher: Arc::new(matcher),
            triggers: Arc::new(triggers),
        });
        Ok(Prepared {
            handle,
            styles,
            triggers: prepared_triggers,
        })
    }

    /// The entry for these sets at this revision, moved to the back as the
    /// most recently used. O(P) for P ≤ [`PREPARED`].
    fn reuse(&self, revision: u32, named: &[TriggerSetId]) -> Option<Prepared> {
        let mut entries = self.entries();
        let at = entries
            .iter()
            .position(|e| e.revision == revision && e.named == named)?;
        let entry = entries.remove(at);
        let prepared = Prepared {
            handle: entry.handle,
            styles: entry.matcher.styles(),
            triggers: entry.triggers.actions(),
        };
        entries.push(entry);
        Some(prepared)
    }

    /// Where the rules behind `handle` match `lines`; `None` when the handle
    /// is stale or unknown, and the pane should prepare again. See
    /// [`Matcher::scan`] for the cost and bounds.
    pub fn scan(&self, handle: u32, lines: &[&str]) -> Option<Vec<Hit>> {
        let revision = self.revision.load(Ordering::Acquire);
        let matcher = self
            .entries()
            .iter()
            .find(|e| e.handle == handle && e.revision == revision)
            .map(|e| Arc::clone(&e.matcher))?;
        Some(matcher.scan(lines))
    }

    /// Trigger rule `rule` of the rules behind `handle`: its name and
    /// action; `None` when the handle is stale or the rule unknown.
    pub fn trigger(&self, handle: u32, rule: u16) -> Option<(Label, Action)> {
        let revision = self.revision.load(Ordering::Acquire);
        let entries = self.entries();
        let entry = entries
            .iter()
            .find(|e| e.handle == handle && e.revision == revision)?;
        entry
            .triggers
            .action(usize::from(rule))
            .map(|(name, action)| (name.clone(), action.clone()))
    }

    /// The triggers behind `handle` that `lines` fire; `None` when the
    /// handle is stale or unknown. See [`TriggerMatcher::scan`].
    pub fn fire(&self, handle: u32, lines: &[&str]) -> Option<Vec<Fired>> {
        let revision = self.revision.load(Ordering::Acquire);
        let triggers = self
            .entries()
            .iter()
            .find(|e| e.handle == handle && e.revision == revision)
            .map(|e| Arc::clone(&e.triggers))?;
        Some(triggers.scan(lines))
    }

    fn failures(&self) -> std::sync::MutexGuard<'_, Vec<(u32, Vec<TriggerSetId>, TriggerError)>> {
        self.failed.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, Vec<Entry>> {
        self.prepared.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests;
