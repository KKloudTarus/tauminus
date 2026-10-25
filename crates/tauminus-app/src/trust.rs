// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Trust between devices (ADR 0030): a host or vault identity another
//! device changed is not used until the user has seen it.
//!
//! A sync notes each host whose exposure it changes, and each identity
//! whose contents it changes, with the fingerprint of what this device
//! had approved, before the merged lists are saved. Each use reads, then
//! checks: a value read after a sync saved it was noted before it, so the
//! check sees it. A check never clears a note; accepting moves it to the
//! fingerprint the user was shown.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
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

/// What a connect to a host acts on, or what an identity holds, hashed:
/// SHA-256, so another device cannot write a different one alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fingerprint(pub [u8; 32]);

/// What a note is about: a saved host, or a vault identity by id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Subject {
    Host(HostId),
    Identity(String),
}

const IDENTITY: &str = "identity:";

/// Whether `id` is one a vault identity may have: 1 to 64 of
/// `[A-Za-z0-9_-]`.
#[must_use]
pub fn identity_id_valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Subject {
    /// `identity:<id>` for an identity, the bare id for a host: a host id
    /// holds no `:`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.strip_prefix(IDENTITY) {
            Some(id) => identity_id_valid(id).then(|| Self::Identity(id.to_owned())),
            None => HostId::parse(text).ok().map(Self::Host),
        }
    }
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host(id) => write!(f, "{id}"),
            Self::Identity(id) => write!(f, "{IDENTITY}{id}"),
        }
    }
}

/// What a sync changed, each with what this device approved for it last:
/// `None` for one that came from another device.
pub type Approvals = BTreeMap<Subject, Option<Fingerprint>>;

/// What a vault identity is, for the review: a digest keyed by a key the
/// vault keeps, so the review list on disk tells nothing of a password,
/// and what the user may be shown of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityPrint {
    pub digest: [u8; 32],
    /// The name, user and kind of login, and the public key: no secret.
    pub lines: Vec<(&'static str, String)>,
}

impl IdentityPrint {
    fn fingerprint(&self) -> Fingerprint {
        Fingerprint(self.digest)
    }
}

/// The vault's identities by id, or those asked for.
pub type Prints = BTreeMap<String, IdentityPrint>;

/// HMAC-SHA256 of `data` under `key`, each part length-prefixed. O(n).
#[must_use]
pub fn keyed_digest(key: &[u8; 32], data: &[&[u8]]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut inner_pad = [0x36u8; BLOCK];
    let mut outer_pad = [0x5cu8; BLOCK];
    for (i, b) in key.iter().enumerate() {
        inner_pad[i] ^= b;
        outer_pad[i] ^= b;
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    for part in data {
        inner.update((part.len() as u64).to_le_bytes());
        inner.update(part);
    }
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// A host or identity that waits for the user, with what it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub subject: Subject,
    pub label: String,
    /// It came from another device: nothing was approved before.
    pub new: bool,
    pub lines: Vec<Exposed>,
    /// What a host's startup snippets type, as a new shell would get it.
    pub startup: String,
    pub fingerprint: Fingerprint,
}

/// A page of what waits, and how many notes there are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pending {
    pub reviews: Vec<Review>,
    /// Notes in all; a few may turn out approved at the next sync.
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
/// each use: another window of the app may have noted something since.
pub struct Trust {
    store: Arc<dyn TrustStore>,
    /// Changes to the notes, one at a time in this process.
    writing: Mutex<()>,
}

/// Tries at a change another window keeps beating with its own.
const ATTEMPTS: usize = 3;

impl fmt::Debug for Trust {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Trust").finish_non_exhaustive()
    }
}

/// The fingerprint of host `id`, with the print of the identity it logs
/// in with from `prints`. Each startup snippet is hashed as it would be
/// typed, one at a time. O(d + Σ startup), d the depth.
///
/// # Errors
/// No such host.
pub fn fingerprint(
    inventory: &Inventory,
    library: &Library,
    prints: &Prints,
    id: &HostId,
) -> Result<Fingerprint, HostsError> {
    let lines = inventory.exposure(id)?;
    let identity = identity_of(inventory, prints, id)?;
    Ok(hash(&lines, startup_of(inventory, id), library, identity))
}

/// The print of the identity a connect to host `id` logs in with, if it
/// has one and `prints` holds it.
fn identity_of<'a>(
    inventory: &Inventory,
    prints: &'a Prints,
    id: &HostId,
) -> Result<Option<&'a IdentityPrint>, HostsError> {
    Ok(inventory
        .identity(id)?
        .and_then(|identity| prints.get(identity.as_str())))
}

fn startup_of<'a>(inventory: &'a Inventory, id: &HostId) -> &'a [SnippetId] {
    inventory.host(id).map_or(&[], |h| h.startup.as_slice())
}

fn hash(
    lines: &[Exposed],
    startup: &[SnippetId],
    library: &Library,
    identity: Option<&IdentityPrint>,
) -> Fingerprint {
    let mut hash = Sha256::new();
    let mut field = |bytes: &[u8]| {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    field(b"tauminus exposure 4");
    // The identity's contents count; its id is among the lines.
    match identity {
        Some(print) => {
            field(b"identity");
            field(&print.digest);
        }
        None => field(b"no identity"),
    }
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

fn print_lines(print: &IdentityPrint) -> Vec<Exposed> {
    print
        .lines
        .iter()
        .map(|(field, value)| Exposed {
            level: 0,
            place: "identity".to_owned(),
            field,
            value: value.clone(),
        })
        .collect()
}

/// The review of host `id`: its lines, its identity's, startup text and
/// fingerprint.
fn review(
    inventory: &Inventory,
    library: &Library,
    prints: &Prints,
    id: &HostId,
    kept: Option<Fingerprint>,
) -> Result<Review, HostsError> {
    let mut lines = inventory.exposure(id)?;
    let startup = startup_of(inventory, id);
    let identity = identity_of(inventory, prints, id)?;
    let fingerprint = hash(&lines, startup, library, identity);
    lines.extend(identity.map(print_lines).unwrap_or_default());
    let label = inventory
        .host(id)
        .map(|h| h.label.as_str().to_owned())
        .unwrap_or_default();
    Ok(Review {
        subject: Subject::Host(id.clone()),
        label,
        new: kept.is_none(),
        lines,
        startup: startup_text(library, startup).0,
        fingerprint,
    })
}

/// The review of identity `id`, as `print` shows it.
fn identity_review(id: &str, print: &IdentityPrint, kept: Option<Fingerprint>) -> Review {
    let label = print
        .lines
        .iter()
        .find(|(field, _)| *field == "identity")
        .map_or_else(|| id.to_owned(), |(_, name)| name.clone());
    Review {
        subject: Subject::Identity(id.to_owned()),
        label,
        new: kept.is_none(),
        lines: print_lines(print),
        startup: String::new(),
        fingerprint: print.fingerprint(),
    }
}

/// `Ok` when identity `id`, noted with `kept`, is as approved in `prints`.
fn identity_held(prints: &Prints, id: &str, kept: Option<Fingerprint>) -> Result<(), TrustError> {
    let Some(print) = prints.get(id) else {
        // Gone, or the vault locked: nothing of it is used.
        return Err(TrustError::NeedsReview(Box::new(Review {
            subject: Subject::Identity(id.to_owned()),
            label: id.to_owned(),
            new: kept.is_none(),
            lines: Vec::new(),
            startup: String::new(),
            fingerprint: Fingerprint([0; 32]),
        })));
    };
    if kept.is_some() && kept == Some(print.fingerprint()) {
        return Ok(());
    }
    Err(TrustError::NeedsReview(Box::new(identity_review(
        id, print, kept,
    ))))
}

/// Whether anything a connect to host `id` acts on may differ between
/// `before` and `after`: the host, a group above it in either, the
/// identity it logs in with, or one of its startup snippets. O(d + k)
/// comparisons, no hashing.
fn touched(before: (&SyncData, &Prints), after: (&SyncData, &Prints), id: &HostId) -> bool {
    let ((before, prints_before), (after, prints_after)) = (before, after);
    let (Some(was), Some(now)) = (before.hosts.host(id), after.hosts.host(id)) else {
        return true;
    };
    if was != now {
        return true;
    }
    for identity in [before.hosts.identity(id), after.hosts.identity(id)]
        .into_iter()
        .flatten()
        .flatten()
    {
        if prints_before.get(identity.as_str()) != prints_after.get(identity.as_str()) {
            return true;
        }
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

    /// Whether anything is noted: a local edit with nothing noted moves
    /// nothing.
    ///
    /// # Errors
    /// The notes cannot be read.
    pub async fn any(&self) -> Result<bool, StoreError> {
        Ok(!self.store.load().await?.0.is_empty())
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

    /// Notes each host whose exposure, and each identity whose contents,
    /// differ between `before`, this device's lists and identity prints,
    /// and `after`, the merge about to be saved; drops the notes of what
    /// went, and of what this merge leaves as approved. Saved before it
    /// returns: call it before anything is saved. O(H·(d + k) + I)
    /// comparisons, hashing only what the merge touched and what was
    /// approved since the last sync; the notes are locked only to apply
    /// the result.
    ///
    /// # Errors
    /// The notes could not be saved; the merge must not be saved either.
    pub async fn note(
        &self,
        before: (&SyncData, &Prints),
        after: (&SyncData, &Prints),
    ) -> Result<(), StoreError> {
        let (noted, _) = self.store.load().await?;
        let print = |(data, prints): (&SyncData, &Prints), id: &HostId| {
            fingerprint(&data.hosts, &data.snippets, prints, id).ok()
        };
        let identity = |prints: &Prints, id: &str| prints.get(id).map(IdentityPrint::fingerprint);
        let mut add = Vec::new();
        let mut drop = BTreeSet::new();
        if before != after {
            // Noted ones too: another window may drop a note meanwhile,
            // and `or_insert` below keeps one that stays.
            for host in after.0.hosts.hosts() {
                if !touched(before, after, &host.id) {
                    continue;
                }
                let was = print(before, &host.id);
                if was != print(after, &host.id) {
                    add.push((Subject::Host(host.id.clone()), was));
                }
            }
            // An id no vault holds is never noted: the list would not
            // read back.
            for id in after.1.keys().filter(|id| identity_id_valid(id)) {
                let was = identity(before.1, id);
                if was != identity(after.1, id) {
                    add.push((Subject::Identity(id.clone()), was));
                }
            }
        }
        for (subject, kept) in noted.iter() {
            let (gone, approved) = match subject {
                Subject::Host(id) => (
                    after.0.hosts.host(id).is_none(),
                    // Untouched here, and back to what the user approved.
                    kept.is_some() && !touched(before, after, id) && *kept == print(after, id),
                ),
                Subject::Identity(id) => {
                    let now = identity(after.1, id);
                    (
                        now.is_none(),
                        kept.is_some() && identity(before.1, id) == now && *kept == now,
                    )
                }
            };
            if gone || approved {
                drop.insert((subject.clone(), *kept));
            }
        }
        if add.is_empty() && drop.is_empty() {
            return Ok(());
        }
        let _one = self.writing.lock().await;
        self.update(|next| {
            // An entry changed since the copy, by an accept, stays.
            next.retain(|subject, kept| !drop.contains(&(subject.clone(), *kept)));
            for (subject, was) in &add {
                next.entry(subject.clone()).or_insert(*was);
            }
        })
        .await
    }

    /// Moves the notes a local edit touched along with it: a host or
    /// identity whose note held what it was just before the edit holds
    /// what it is now, so the user's own edit is no change to review.
    /// `before` and `after` are what the edit changed, read with it; the
    /// rest is read once for both, so a sync saving there meanwhile leaves
    /// what it changed held. A print missing from both leaves what uses
    /// it as it was. O(n·(d + Σ startup)) for n notes.
    ///
    /// # Errors
    /// The notes not saving; what was noted stays held, which is safe.
    pub async fn rebase(
        &self,
        before: (&Inventory, &Library, &Prints),
        after: (&Inventory, &Library, &Prints),
    ) -> Result<(), StoreError> {
        let _one = self.writing.lock().await;
        self.update(|next| {
            for (subject, kept) in next.iter_mut() {
                let Some(approved) = *kept else { continue };
                let (was, now) = match subject {
                    Subject::Host(id) => (
                        fingerprint(before.0, before.1, before.2, id).ok(),
                        fingerprint(after.0, after.1, after.2, id).ok(),
                    ),
                    Subject::Identity(id) => (
                        before.2.get(id).map(IdentityPrint::fingerprint),
                        after.2.get(id).map(IdentityPrint::fingerprint),
                    ),
                };
                if was == Some(approved) {
                    *kept = now.or(*kept);
                }
            }
        })
        .await
    }

    /// Whether host `id` may be used as saved now: the host, and the
    /// identity it logs in with, each as the user approved it. Call it
    /// after reading what the use acts on. O(log n) with nothing noted for
    /// either; a noted host is hashed in place, O(d + Σ startup), under
    /// the books' locks, with `prints` asked for the identity it logs in
    /// with only. A print missing, as with the vault locked, fails closed.
    ///
    /// # Errors
    /// [`TrustError::NeedsReview`] with what the user must see.
    pub async fn check(
        &self,
        hosts: &HostBook,
        snippets: Option<&SnippetBook>,
        prints: impl AsyncFnOnce(Vec<String>) -> Prints,
        id: &HostId,
    ) -> Result<(), TrustError> {
        let (notes, _) = self.store.load().await?;
        if notes.is_empty() {
            return Ok(());
        }
        let host = notes.get(&Subject::Host(id.clone())).copied();
        let identity = hosts
            .reading()
            .await
            .identity(id)?
            .map(|i| i.as_str().to_owned());
        let identity_note = identity
            .as_ref()
            .and_then(|i| notes.get(&Subject::Identity(i.clone())).copied());
        if host.is_none() && identity_note.is_none() {
            return Ok(());
        }
        // Read before the books' locks: the vault may take a while. A host
        // that logs in with another identity by then has no print: held.
        let prints = &prints(identity.iter().cloned().collect()).await;
        if let (Some(identity), Some(kept)) = (&identity, identity_note) {
            identity_held(prints, identity, kept)?;
        }
        let Some(kept) = host else {
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
        if kept.is_some() && kept == Some(fingerprint(&inventory, library, prints, id)?) {
            return Ok(());
        }
        Err(TrustError::NeedsReview(Box::new(review(
            &inventory, library, prints, id, kept,
        )?)))
    }

    /// Whether identity `id` may be used as saved now, as when its key is
    /// installed on a host. Call it after reading the identity. O(log n),
    /// and its print when it is noted.
    ///
    /// # Errors
    /// [`TrustError::NeedsReview`] with what the user must see.
    pub async fn check_identity(
        &self,
        prints: impl AsyncFnOnce(Vec<String>) -> Prints,
        id: &str,
    ) -> Result<(), TrustError> {
        let subject = Subject::Identity(id.to_owned());
        let Some(kept) = self.store.load().await?.0.get(&subject).copied() else {
            return Ok(());
        };
        identity_held(&prints(vec![id.to_owned()]).await, id, kept)
    }

    /// Up to `limit` of what waits for the user, identities first, and how
    /// many notes there are. O(limit·(d + Σ startup)) plus the approved
    /// ones passed.
    ///
    /// # Errors
    /// None in practice: a host gone since is skipped.
    pub async fn pending(
        &self,
        hosts: &HostBook,
        snippets: Option<&SnippetBook>,
        prints: &Prints,
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
        // Identities sort after hosts in the map; the few there are come
        // first, as each host using one shows it again.
        let (identities, hosts): (Vec<_>, Vec<_>) = noted
            .iter()
            .partition(|(subject, _)| matches!(subject, Subject::Identity(_)));
        for (subject, kept) in identities.into_iter().chain(hosts) {
            if reviews.len() == limit {
                break;
            }
            match subject {
                Subject::Host(id) => {
                    if inventory.host(id).is_none() {
                        continue;
                    }
                    if kept.is_some()
                        && *kept == Some(fingerprint(&inventory, library, prints, id)?)
                    {
                        continue;
                    }
                    reviews.push(review(&inventory, library, prints, id, *kept)?);
                }
                Subject::Identity(id) => {
                    let Some(print) = prints.get(id) else {
                        continue;
                    };
                    if kept.is_some() && *kept == Some(print.fingerprint()) {
                        continue;
                    }
                    reviews.push(identity_review(id, print, *kept));
                }
            }
        }
        Ok(Pending {
            reviews,
            noted: noted.len(),
        })
    }

    /// Approves each as the user saw it, by its fingerprint, and saves
    /// once. Returns those that changed since they were shown, which stay
    /// as they were. O(m·(d + Σ startup)) for m.
    ///
    /// # Errors
    /// The notes not saving.
    pub async fn accept(
        &self,
        hosts: &HostBook,
        snippets: Option<&SnippetBook>,
        prints: &Prints,
        seen: &[(Subject, Fingerprint)],
    ) -> Result<Vec<Subject>, TrustError> {
        // The note stays, holding what was shown: a sync that saves other
        // values after the hash, its note skipped as noted already,
        // leaves them held. The next sync that leaves it as shown drops
        // the note.
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
            for (subject, shown) in seen {
                let now = match subject {
                    Subject::Host(id) => fingerprint(&inventory, library, prints, id).ok(),
                    Subject::Identity(id) => prints.get(id).map(IdentityPrint::fingerprint),
                };
                if now == Some(*shown) {
                    ok.insert(subject.clone(), *shown);
                } else {
                    stale.push(subject.clone());
                }
            }
        }
        if !ok.is_empty() {
            self.update(|next| {
                for (subject, shown) in &ok {
                    if let Some(kept) = next.get_mut(subject) {
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
