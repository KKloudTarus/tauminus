// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Trust between devices (ADR 0030): a host another device changed is not
//! used until the user has seen what a connect to it now acts on.
//!
//! A sync notes each host whose exposure it changes, with the fingerprint
//! of what this device had approved, before the merged lists are saved.
//! Each use reads the host, then checks: a value read after a sync saved
//! it was noted before it, so the check sees it. A check never clears a
//! note; accepting does, for the fingerprint the user was shown.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tauminus_domain::{
    Exposed, HostId, HostsError, Inventory, Library, MAX_GROUP_DEPTH, SnippetError, SnippetId,
};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::ports::{StoreError, SyncData, TrustStore};
use crate::snippets::startup_text;
use crate::{HostBook, SnippetBook};

/// What a connect to a host acts on, hashed: SHA-256, so another device
/// cannot write a different exposure with the same one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fingerprint(pub [u8; 32]);

/// Hosts a sync changed, each with what this device approved for it last:
/// `None` for a host that came from another device.
pub type Approvals = BTreeMap<HostId, Option<Fingerprint>>;

/// A host that waits for the user, with what a connect to it acts on now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub host: HostId,
    pub label: String,
    /// The host came from another device: nothing was approved before.
    pub new: bool,
    pub lines: Vec<Exposed>,
    /// What its startup snippets type, as a new shell would get it.
    pub startup: String,
    pub fingerprint: Fingerprint,
}

/// A page of the hosts waiting, and how many hosts have a note.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pending {
    pub reviews: Vec<Review>,
    /// Hosts noted; a few may turn out approved at the next sync.
    pub noted: usize,
}

#[derive(Debug, Error)]
pub enum TrustError {
    #[error("another device changed {}; review it before it is used", .0.label)]
    NeedsReview(Box<Review>),
    #[error(transparent)]
    Hosts(#[from] HostsError),
    #[error("the sync review list: {0}")]
    Store(#[from] StoreError),
}

/// The notes, saved before they count. They are read from the store at
/// each use: another window of the app may have noted a host since.
pub struct Trust {
    store: Arc<dyn TrustStore>,
    /// Changes to the notes, one at a time in this process.
    writing: Mutex<()>,
}

/// Tries at a change another window keeps beating with its own.
const ATTEMPTS: usize = 3;

impl std::fmt::Debug for Trust {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Trust").finish_non_exhaustive()
    }
}

/// The fingerprint of host `id`. Each startup snippet is hashed as it
/// would be typed, one at a time. O(d + Σ startup), d the depth.
///
/// # Errors
/// No such host.
pub fn fingerprint(
    inventory: &Inventory,
    library: &Library,
    id: &HostId,
) -> Result<Fingerprint, HostsError> {
    let lines = inventory.exposure(id)?;
    Ok(hash(&lines, startup_of(inventory, id), library))
}

fn startup_of<'a>(inventory: &'a Inventory, id: &HostId) -> &'a [SnippetId] {
    inventory.host(id).map_or(&[], |h| h.startup.as_slice())
}

fn hash(lines: &[Exposed], startup: &[SnippetId], library: &Library) -> Fingerprint {
    let mut hash = Sha256::new();
    let mut field = |bytes: &[u8]| {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    field(b"tauminus exposure 2");
    for line in lines {
        field(&(line.level as u64).to_le_bytes());
        field(line.field.as_bytes());
        field(line.value.as_bytes());
    }
    for id in startup {
        field(id.as_str().as_bytes());
        // What a new shell types for it; one that cannot run types
        // nothing, which differs from any text by its marker.
        match library
            .get(id)
            .ok_or_else(|| SnippetError::NoSuch(id.clone()))
            .and_then(|s| s.command(&BTreeMap::new()))
        {
            Ok(text) => {
                field(b"typed");
                field(text.as_bytes());
            }
            Err(_) => field(b"skipped"),
        }
    }
    Fingerprint(hash.finalize().into())
}

/// The review of host `id`: its lines, startup text and fingerprint.
fn review(
    inventory: &Inventory,
    library: &Library,
    id: &HostId,
    kept: Option<Fingerprint>,
) -> Result<Review, HostsError> {
    let lines = inventory.exposure(id)?;
    let startup = startup_of(inventory, id);
    let fingerprint = hash(&lines, startup, library);
    let label = inventory
        .host(id)
        .map(|h| h.label.as_str().to_owned())
        .unwrap_or_default();
    Ok(Review {
        host: id.clone(),
        label,
        new: kept.is_none(),
        lines,
        startup: startup_text(library, startup).0,
        fingerprint,
    })
}

/// Whether anything a connect to host `id` acts on may differ between
/// `before` and `after`: the host, a group above it in either, or one of
/// its startup snippets. O(d + k) comparisons, no hashing.
fn touched(before: &SyncData, after: &SyncData, id: &HostId) -> bool {
    let (Some(was), Some(now)) = (before.hosts.host(id), after.hosts.host(id)) else {
        return true;
    };
    if was != now {
        return true;
    }
    let mut group = now.group.as_ref();
    for _ in 0..MAX_GROUP_DEPTH {
        let Some(id) = group else { break };
        let (a, b) = (before.hosts.group(id), after.hosts.group(id));
        if a != b {
            return true;
        }
        group = b.and_then(|g| g.parent.as_ref());
    }
    now.startup
        .iter()
        .any(|s| before.snippets.get(s) != after.snippets.get(s))
}

impl Trust {
    /// # Errors
    /// The saved notes cannot be read: then no saved host may be used, so
    /// the caller refuses every connect.
    pub async fn open(store: Arc<dyn TrustStore>) -> Result<Self, StoreError> {
        store.load().await?;
        Ok(Self {
            store,
            writing: Mutex::new(()),
        })
    }

    /// Applies `change` to the notes as saved now, and saves them if it
    /// changed them; read again when another window saved meanwhile.
    async fn update(&self, change: impl Fn(&mut Approvals)) -> Result<(), StoreError> {
        for attempt in 1..=ATTEMPTS {
            let (before, read) = self.store.load().await?;
            let mut next = (*before).clone();
            change(&mut next);
            if next == *before {
                return Ok(());
            }
            match self.store.save(&next, read).await {
                Err(StoreError::Conflict) if attempt < ATTEMPTS => {}
                result => return result,
            }
        }
        Err(StoreError::Conflict)
    }

    /// Notes each host whose exposure differs between `before`, this
    /// device's lists, and `after`, the merge about to be saved; drops the
    /// notes of hosts gone, and of hosts this merge leaves as approved.
    /// Saved before it returns: call it before the lists are saved.
    /// O(H·(d + k)) comparisons, and hashing only for the hosts the merge
    /// touched and those approved since the last sync; the notes are
    /// locked only to apply the result.
    ///
    /// # Errors
    /// The notes could not be saved; the merge must not be saved either.
    pub async fn note(&self, before: &SyncData, after: &SyncData) -> Result<(), StoreError> {
        let (noted, _) = self.store.load().await?;
        let print =
            |data: &SyncData, id: &HostId| fingerprint(&data.hosts, &data.snippets, id).ok();
        let mut add = Vec::new();
        let mut drop = BTreeSet::new();
        if before != after {
            // Noted hosts too: another window may drop a note meanwhile,
            // and `or_insert` below keeps one that stays.
            for host in after.hosts.hosts() {
                if !touched(before, after, &host.id) {
                    continue;
                }
                let was = print(before, &host.id);
                if was != print(after, &host.id) {
                    add.push((host.id.clone(), was));
                }
            }
        }
        for (id, kept) in noted.iter() {
            let gone = after.hosts.host(id).is_none();
            // Untouched here, and back to what the user approved.
            let approved =
                kept.is_some() && !touched(before, after, id) && *kept == print(after, id);
            if gone || approved {
                drop.insert((id.clone(), *kept));
            }
        }
        if add.is_empty() && drop.is_empty() {
            return Ok(());
        }
        let _one = self.writing.lock().await;
        self.update(|next| {
            // An entry changed since the copy, by an accept, stays.
            next.retain(|id, kept| !drop.contains(&(id.clone(), *kept)));
            for (id, was) in &add {
                next.entry(id.clone()).or_insert(*was);
            }
        })
        .await
    }

    /// Moves the notes a local edit touched along with it: a host whose
    /// note held what it was just before the edit holds what it is now,
    /// so the user's own edit is no change to review. `before` and
    /// `after` are what the edit changed, read under its book's lock; the
    /// other book is read once for both, so a sync saving there meanwhile
    /// leaves the host held. O(n·(d + Σ startup)) for n noted hosts.
    ///
    /// # Errors
    /// The notes not saving; the hosts stay held, which is safe.
    pub async fn rebase(
        &self,
        before: (&Inventory, &Library),
        after: (&Inventory, &Library),
    ) -> Result<(), StoreError> {
        let _one = self.writing.lock().await;
        self.update(|next| {
            for (id, kept) in next.iter_mut() {
                let Some(approved) = *kept else { continue };
                if fingerprint(before.0, before.1, id).ok() == Some(approved) {
                    *kept = fingerprint(after.0, after.1, id).ok().or(*kept);
                }
            }
        })
        .await
    }

    /// Whether host `id` may be used as saved now. Call it after reading
    /// what the use acts on. O(log n) for a host with no note; one with a
    /// note is hashed in place, O(d + Σ startup), under the books' locks.
    ///
    /// # Errors
    /// [`TrustError::NeedsReview`] with what the user must see.
    pub async fn check(
        &self,
        hosts: &HostBook,
        snippets: Option<&SnippetBook>,
        id: &HostId,
    ) -> Result<(), TrustError> {
        let Some(kept) = self.store.load().await?.0.get(id).copied() else {
            return Ok(());
        };
        let inventory = hosts.reading().await;
        let empty = Library::default();
        let library = match snippets {
            Some(book) => Some(book.reading().await),
            None => None,
        };
        // Unreadable snippets type nothing, which differs from any approved
        // startup: the check fails closed.
        let library = library.as_deref().unwrap_or(&empty);
        if kept.is_some() && kept == Some(fingerprint(&inventory, library, id)?) {
            return Ok(());
        }
        Err(TrustError::NeedsReview(Box::new(review(
            &inventory, library, id, kept,
        )?)))
    }

    /// Up to `limit` hosts waiting for the user, in id order, and how many
    /// are noted. O(limit·(d + Σ startup)) plus the approved ones passed.
    ///
    /// # Errors
    /// None in practice: a host gone since is skipped.
    pub async fn pending(
        &self,
        hosts: &HostBook,
        snippets: Option<&SnippetBook>,
        limit: usize,
    ) -> Result<Pending, TrustError> {
        let (noted, _) = self.store.load().await?;
        let inventory = hosts.reading().await;
        let empty = Library::default();
        let library = match snippets {
            Some(book) => Some(book.reading().await),
            None => None,
        };
        let library = library.as_deref().unwrap_or(&empty);
        let mut reviews = Vec::new();
        for (id, kept) in noted.iter() {
            if reviews.len() == limit {
                break;
            }
            if inventory.host(id).is_none() {
                continue;
            }
            if kept.is_some() && *kept == Some(fingerprint(&inventory, library, id)?) {
                continue;
            }
            reviews.push(review(&inventory, library, id, *kept)?);
        }
        Ok(Pending {
            reviews,
            noted: noted.len(),
        })
    }

    /// Approves each host as the user saw it, by its fingerprint, and
    /// saves once. Returns the hosts that changed since they were shown,
    /// which stay as they were. O(m·(d + Σ startup)) for m hosts.
    ///
    /// # Errors
    /// The notes not saving.
    pub async fn accept(
        &self,
        hosts: &HostBook,
        snippets: Option<&SnippetBook>,
        seen: &[(HostId, Fingerprint)],
    ) -> Result<Vec<HostId>, TrustError> {
        // The note stays, holding what was shown: a sync that saves other
        // values after the hash, its note skipped as the host was noted,
        // leaves them held. The next sync that leaves the host as shown
        // drops the note.
        let _one = self.writing.lock().await;
        let mut ok = BTreeMap::new();
        let mut stale = Vec::new();
        {
            let inventory = hosts.reading().await;
            let empty = Library::default();
            let library = match snippets {
                Some(book) => Some(book.reading().await),
                None => None,
            };
            let library = library.as_deref().unwrap_or(&empty);
            for (id, shown) in seen {
                match fingerprint(&inventory, library, id) {
                    Ok(now) if now == *shown => {
                        ok.insert(id.clone(), now);
                    }
                    _ => stale.push(id.clone()),
                }
            }
        }
        if !ok.is_empty() {
            self.update(|next| {
                for (id, shown) in &ok {
                    if let Some(kept) = next.get_mut(id) {
                        *kept = Some(*shown);
                    }
                }
            })
            .await?;
        }
        Ok(stale)
    }
}

#[cfg(test)]
mod tests;
