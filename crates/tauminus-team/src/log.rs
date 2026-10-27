// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The membership log: who is in a vault, with which devices and roles,
//! and which key epoch is current. Each entry names the hash of the one
//! before it and is signed by an admin's device, so the Hub that stores
//! the log can neither add a key nor change a role, and a log that
//! differs from the one a device holds is refused.

use std::collections::BTreeMap;

use crate::encode::{Encoder, sha256};
use crate::keys::{DeviceCard, DeviceId, DeviceKeys, UserId, VaultId};
use crate::seal::{self, Key, KeyKind, SealedKey, Wrap};
use crate::{Hash, TeamError};

const ENTRY: &[u8] = b"tauminus-team/entry/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    View,
    Edit,
    Admin,
}

impl Role {
    fn tag(self) -> u8 {
        match self {
            Self::View => 1,
            Self::Edit => 2,
            Self::Admin => 3,
        }
    }
}

/// A new key epoch: fresh keys sealed to who remains, and the previous
/// epoch's keys sealed under the new ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rotation {
    pub epoch: u32,
    pub wraps: Vec<Wrap>,
    pub previous_data: SealedKey,
    pub previous_secrets: SealedKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// The first entry: the creator, an admin with credentials, and the
    /// first epoch's keys sealed to its device.
    Create {
        creator: DeviceCard,
        wraps: Vec<Wrap>,
    },
    AddMember {
        card: DeviceCard,
        role: Role,
        credentials: bool,
        wraps: Vec<Wrap>,
    },
    AddDevice {
        card: DeviceCard,
        wraps: Vec<Wrap>,
    },
    /// A new role or credentials grant. Granting credentials seals the
    /// secrets key to the member's devices; taking them starts an epoch.
    SetAccess {
        user: UserId,
        role: Role,
        credentials: bool,
        wraps: Vec<Wrap>,
        rotation: Option<Rotation>,
        kept: Vec<Hash>,
    },
    RemoveMember {
        user: UserId,
        rotation: Rotation,
        kept: Vec<Hash>,
    },
    RemoveDevice {
        device: DeviceId,
        rotation: Rotation,
        kept: Vec<Hash>,
    },
}

fn encode_wraps(out: &mut Encoder, wraps: &[Wrap]) {
    out.u64(wraps.len() as u64);
    for wrap in wraps {
        wrap.encode(out);
    }
}

fn encode_kept(out: &mut Encoder, kept: &[Hash]) {
    out.u64(kept.len() as u64);
    for digest in kept {
        out.fixed(digest);
    }
}

fn encode_rotation(out: &mut Encoder, rotation: &Rotation) {
    out.u32(rotation.epoch);
    encode_wraps(out, &rotation.wraps);
    rotation.previous_data.encode(out);
    rotation.previous_secrets.encode(out);
}

impl Op {
    /// Every wrap the entry carries, in order.
    pub fn wraps(&self) -> impl Iterator<Item = &Wrap> {
        let (own, rotation): (&[Wrap], Option<&Rotation>) = match self {
            Self::Create { wraps, .. }
            | Self::AddMember { wraps, .. }
            | Self::AddDevice { wraps, .. } => (wraps, None),
            Self::SetAccess {
                wraps, rotation, ..
            } => (wraps, rotation.as_ref()),
            Self::RemoveMember { rotation, .. } | Self::RemoveDevice { rotation, .. } => {
                (&[], Some(rotation))
            }
        };
        own.iter()
            .chain(rotation.into_iter().flat_map(|r| &r.wraps))
    }

    fn encode(&self, out: &mut Encoder) {
        match self {
            Self::Create { creator, wraps } => {
                out.u8(1);
                creator.encode(out);
                encode_wraps(out, wraps);
            }
            Self::AddMember {
                card,
                role,
                credentials,
                wraps,
            } => {
                out.u8(2);
                card.encode(out);
                out.u8(role.tag()).u8(u8::from(*credentials));
                encode_wraps(out, wraps);
            }
            Self::AddDevice { card, wraps } => {
                out.u8(3);
                card.encode(out);
                encode_wraps(out, wraps);
            }
            Self::SetAccess {
                user,
                role,
                credentials,
                wraps,
                rotation,
                kept,
            } => {
                out.u8(4).bytes(user.as_str().as_bytes());
                out.u8(role.tag()).u8(u8::from(*credentials));
                encode_wraps(out, wraps);
                match rotation {
                    Some(rotation) => {
                        out.u8(1);
                        encode_rotation(out, rotation);
                    }
                    None => {
                        out.u8(0);
                    }
                }
                encode_kept(out, kept);
            }
            Self::RemoveMember {
                user,
                rotation,
                kept,
            } => {
                out.u8(5).bytes(user.as_str().as_bytes());
                encode_rotation(out, rotation);
                encode_kept(out, kept);
            }
            Self::RemoveDevice {
                device,
                rotation,
                kept,
            } => {
                out.u8(6).fixed(&device.0);
                encode_rotation(out, rotation);
                encode_kept(out, kept);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub vault: VaultId,
    pub seq: u64,
    pub prev: Hash,
    pub signer: DeviceId,
    pub op: Op,
    pub signature: [u8; 64],
}

impl Entry {
    fn signed_bytes(vault: VaultId, seq: u64, prev: &Hash, signer: DeviceId, op: &Op) -> Vec<u8> {
        let mut out = Encoder::new(ENTRY);
        out.fixed(&vault.0).u64(seq).fixed(prev).fixed(&signer.0);
        op.encode(&mut out);
        out.finish()
    }

    /// Everything the entry says, signature included: what the Hub
    /// stores and the next entry's hash covers.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Self::signed_bytes(self.vault, self.seq, &self.prev, self.signer, &self.op);
        bytes.extend_from_slice(&self.signature);
        bytes
    }

    pub fn hash(&self) -> Hash {
        sha256(&[&self.to_bytes()])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Access {
    role: Role,
    credentials: bool,
}

#[derive(Debug, Clone)]
struct Device {
    card: DeviceCard,
    added: u64,
    removed: Option<u64>,
}

/// What an invite carries: the vault's first entry and the newest one
/// the inviting admin held, so a joining device refuses any other log,
/// including one a removed admin went on signing from an older entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Invite {
    pub first: Hash,
    pub seq: u64,
    pub head: Hash,
}

/// A vault's membership as of the newest entry this device verified.
#[derive(Debug, Clone)]
pub struct Log {
    vault: VaultId,
    /// Each entry's hash, by sequence number.
    hashes: Vec<Hash>,
    /// The entry each epoch began at.
    epochs: Vec<u64>,
    /// Each member's access from each entry on; `None` once removed.
    access: BTreeMap<UserId, Vec<(u64, Option<Access>)>>,
    devices: BTreeMap<DeviceId, Device>,
    /// Epoch `e`'s keys sealed under epoch `e + 1`'s, at index `e`.
    previous: Vec<(SealedKey, SealedKey)>,
    /// Item versions an admin kept when their writer lost the right to
    /// write, and the entry that kept them.
    kept: BTreeMap<Hash, u64>,
    /// Each device's newest wrap of each kind, so a keyring opens without
    /// the entries: 133 bytes for each device and kind ever added.
    newest: BTreeMap<(DeviceId, KeyKind), Wrap>,
}

impl Log {
    /// Verifies a whole log from its first entry, and that it holds the
    /// newest entry the invite names. O(entries + wraps).
    pub fn join<'a>(
        invite: &Invite,
        entries: impl IntoIterator<Item = &'a Entry>,
    ) -> Result<Self, TeamError> {
        let log = Self::verify(&invite.first, entries)?;
        if log.knows(invite.seq, &invite.head) {
            Ok(log)
        } else {
            Err(TeamError::Fork(invite.seq))
        }
    }

    /// What an admin hands a new member, out of band.
    pub fn invite(&self) -> Invite {
        let (seq, head) = self.head();
        Invite {
            first: self.hashes[0],
            seq,
            head,
        }
    }

    /// Verifies a whole log from its first entry: one this device created,
    /// or reloads from what it stored. A device joining a vault uses
    /// [`Log::join`], which also checks the newest entry the invite names.
    /// O(entries + wraps).
    pub fn verify<'a>(
        pin: &Hash,
        entries: impl IntoIterator<Item = &'a Entry>,
    ) -> Result<Self, TeamError> {
        let mut entries = entries.into_iter();
        let first = entries.next().ok_or(TeamError::BadEntry("an empty log"))?;
        if &first.hash() != pin {
            return Err(TeamError::Fork(0));
        }
        let mut log = Self {
            vault: first.vault,
            hashes: Vec::new(),
            epochs: Vec::new(),
            access: BTreeMap::new(),
            devices: BTreeMap::new(),
            previous: Vec::new(),
            kept: BTreeMap::new(),
            newest: BTreeMap::new(),
        };
        log.apply(first)?;
        for entry in entries {
            log.apply(entry)?;
        }
        Ok(log)
    }

    /// Adds the entries that follow the newest one this log holds; those
    /// it already holds, unchanged, are passed over. All or none: a log
    /// that fails to verify is left as it was. O(state) for the copy that
    /// makes it all or none, plus O(new entries + their wraps).
    pub fn extend<'a>(
        &mut self,
        entries: impl IntoIterator<Item = &'a Entry>,
    ) -> Result<(), TeamError> {
        let mut next: Option<Self> = None;
        for entry in entries {
            let log = next.as_ref().unwrap_or(self);
            if entry.seq < log.now() {
                if !log.knows(entry.seq, &entry.hash()) {
                    return Err(TeamError::Fork(entry.seq));
                }
                continue;
            }
            next.get_or_insert_with(|| self.clone()).apply(entry)?;
        }
        if let Some(next) = next {
            *self = next;
        }
        Ok(())
    }

    /// Whether an admin kept this item version, at an entry after `seq`.
    pub(crate) fn kept_after(&self, digest: &Hash, seq: u64) -> bool {
        self.kept.get(digest).is_some_and(|at| *at > seq)
    }

    pub fn vault(&self) -> VaultId {
        self.vault
    }

    /// The newest entry's sequence number and hash.
    pub fn head(&self) -> (u64, Hash) {
        let last = self.hashes.len() - 1;
        (last as u64, self.hashes[last])
    }

    /// Whether this log holds the entry `seq` with `hash`: a log position
    /// that is not in it means the Hub showed some device another log.
    pub fn knows(&self, seq: u64, hash: &Hash) -> bool {
        usize::try_from(seq)
            .ok()
            .and_then(|i| self.hashes.get(i))
            .is_some_and(|h| h == hash)
    }

    pub fn epoch(&self) -> u32 {
        u32::try_from(self.epochs.len() - 1).unwrap_or(u32::MAX)
    }

    /// The epoch current once entry `seq` was applied. O(log epochs).
    pub fn epoch_at(&self, seq: u64) -> u32 {
        let began = self.epochs.partition_point(|start| *start <= seq);
        u32::try_from(began.saturating_sub(1)).unwrap_or(u32::MAX)
    }

    fn access_at(&self, user: &UserId, seq: u64) -> Option<Access> {
        let changes = self.access.get(user)?;
        let at = changes.partition_point(|(from, _)| *from <= seq);
        changes.get(at.checked_sub(1)?)?.1
    }

    /// A member's role and credentials grant once entry `seq` was applied.
    pub fn role_at(&self, user: &UserId, seq: u64) -> Option<(Role, bool)> {
        self.access_at(user, seq)
            .map(|access| (access.role, access.credentials))
    }

    /// The device's card, when it was in the vault once entry `seq` was
    /// applied.
    pub fn device_at(&self, id: DeviceId, seq: u64) -> Option<&DeviceCard> {
        self.devices
            .get(&id)
            .filter(|d| d.added <= seq && d.removed.is_none_or(|removed| removed > seq))
            .map(|d| &d.card)
    }

    fn now(&self) -> u64 {
        self.hashes.len() as u64
    }

    fn current(&self, user: &UserId) -> Option<Access> {
        self.access
            .get(user)
            .and_then(|c| c.last())
            .and_then(|c| c.1)
    }

    /// The devices in the vault now, with their members' access.
    fn present(&self) -> impl Iterator<Item = (&DeviceCard, Access)> {
        self.devices
            .values()
            .filter(|d| d.removed.is_none())
            .filter_map(|d| Some((&d.card, self.current(&d.card.user)?)))
    }

    fn apply(&mut self, entry: &Entry) -> Result<(), TeamError> {
        let seq = self.now();
        if entry.vault != self.vault {
            return Err(TeamError::BadEntry("an entry of another vault"));
        }
        if entry.seq != seq {
            return Err(TeamError::BadEntry("an entry is missing before this one"));
        }
        let prev = self.hashes.last().copied().unwrap_or([0; 32]);
        if entry.prev != prev {
            return Err(TeamError::Fork(seq));
        }
        let message = Entry::signed_bytes(entry.vault, seq, &entry.prev, entry.signer, &entry.op);
        if let Op::Create { creator, wraps } = &entry.op {
            if seq != 0 || entry.signer != creator.id() {
                return Err(TeamError::BadEntry(
                    "only the first entry creates the vault",
                ));
            }
            creator.verify(&message, &entry.signature)?;
            self.start(creator, wraps)?;
        } else {
            if seq == 0 {
                return Err(TeamError::BadEntry("the first entry creates the vault"));
            }
            let signer = self
                .device_at(entry.signer, seq - 1)
                .ok_or(TeamError::NotAllowed(
                    "the signing device is not in the vault",
                ))?;
            if self.current(&signer.user).map(|a| a.role) != Some(Role::Admin) {
                return Err(TeamError::NotAllowed("only an admin changes the vault"));
            }
            signer.verify(&message, &entry.signature)?;
            self.change(seq, &entry.op)?;
        }
        for wrap in entry.op.wraps() {
            self.newest.insert((wrap.device, wrap.kind), wrap.clone());
        }
        self.hashes.push(entry.hash());
        Ok(())
    }

    fn start(&mut self, creator: &DeviceCard, wraps: &[Wrap]) -> Result<(), TeamError> {
        let access = Access {
            role: Role::Admin,
            credentials: true,
        };
        self.epochs.push(0);
        self.enter(0, creator, Some(access))?;
        check_wraps(wraps, &[(creator, access)], 0)
    }

    fn change(&mut self, seq: u64, op: &Op) -> Result<(), TeamError> {
        let epoch = self.epoch();
        match op {
            Op::Create { .. } => unreachable!("handled in apply"),
            Op::AddMember {
                card,
                role,
                credentials,
                wraps,
            } => {
                if self.current(&card.user).is_some() {
                    return Err(TeamError::BadEntry("the member is already in the vault"));
                }
                let access = checked_access(*role, *credentials)?;
                self.enter(seq, card, Some(access))?;
                check_wraps(wraps, &[(card, access)], epoch)
            }
            Op::AddDevice { card, wraps } => {
                let access = self
                    .current(&card.user)
                    .ok_or(TeamError::BadEntry("a device of no member"))?;
                self.enter(seq, card, None)?;
                check_wraps(wraps, &[(card, access)], epoch)
            }
            Op::SetAccess {
                user,
                role,
                credentials,
                wraps,
                rotation,
                kept,
            } => {
                self.keep(seq, kept);
                self.apply_access(
                    seq,
                    user,
                    checked_access(*role, *credentials)?,
                    wraps,
                    rotation.as_ref(),
                )
            }
            Op::RemoveMember {
                user,
                rotation,
                kept,
            } => {
                self.keep(seq, kept);
                if self.current(user).is_none() {
                    return Err(TeamError::BadEntry("not a member"));
                }
                self.access
                    .entry(user.clone())
                    .or_default()
                    .push((seq, None));
                for device in self.devices.values_mut() {
                    if &device.card.user == user && device.removed.is_none() {
                        device.removed = Some(seq);
                    }
                }
                self.rotate(seq, rotation)
            }
            Op::RemoveDevice {
                device,
                rotation,
                kept,
            } => {
                self.keep(seq, kept);
                let found = self
                    .devices
                    .get_mut(device)
                    .filter(|d| d.removed.is_none())
                    .ok_or(TeamError::BadEntry("not a device in the vault"))?;
                found.removed = Some(seq);
                self.rotate(seq, rotation)
            }
        }
    }

    fn keep(&mut self, seq: u64, kept: &[Hash]) {
        for digest in kept {
            self.kept.entry(*digest).or_insert(seq);
        }
    }

    fn apply_access(
        &mut self,
        seq: u64,
        user: &UserId,
        access: Access,
        wraps: &[Wrap],
        rotation: Option<&Rotation>,
    ) -> Result<(), TeamError> {
        let before = self
            .current(user)
            .ok_or(TeamError::BadEntry("not a member"))?;
        self.access
            .entry(user.clone())
            .or_default()
            .push((seq, Some(access)));
        match (before.credentials, access.credentials, rotation) {
            // Taking credentials: a new epoch whose secrets key the member
            // never holds.
            (true, false, Some(rotation)) if wraps.is_empty() => self.rotate(seq, rotation),
            // Granting them: the secrets key sealed to each of its devices.
            (false, true, None) => {
                let wanted: Vec<(DeviceId, KeyKind)> = self
                    .present()
                    .filter(|(card, _)| &card.user == user)
                    .map(|(card, _)| (card.id(), KeyKind::Secrets))
                    .collect();
                let epoch = self.epoch();
                exact_wraps(wraps, &wanted, epoch)?;
                self.admin_remains()
            }
            (a, b, None) if a == b && wraps.is_empty() => self.admin_remains(),
            _ => Err(TeamError::BadEntry(
                "keys do not match the change of access",
            )),
        }
    }

    fn enter(
        &mut self,
        seq: u64,
        card: &DeviceCard,
        access: Option<Access>,
    ) -> Result<(), TeamError> {
        let id = card.id();
        if self.devices.contains_key(&id) {
            return Err(TeamError::BadEntry("the device was in the vault before"));
        }
        self.devices.insert(
            id,
            Device {
                card: card.clone(),
                added: seq,
                removed: None,
            },
        );
        if let Some(access) = access {
            self.access
                .entry(card.user.clone())
                .or_default()
                .push((seq, Some(access)));
        }
        Ok(())
    }

    fn rotate(&mut self, seq: u64, rotation: &Rotation) -> Result<(), TeamError> {
        self.admin_remains()?;
        let epoch = self.epoch() + 1;
        if rotation.epoch != epoch {
            return Err(TeamError::BadEntry("a rotation starts the next epoch"));
        }
        let present: Vec<(DeviceCard, Access)> = self
            .present()
            .map(|(card, access)| (card.clone(), access))
            .collect();
        let devices: Vec<(&DeviceCard, Access)> = present.iter().map(|(c, a)| (c, *a)).collect();
        check_wraps(&rotation.wraps, &devices, epoch)?;
        self.epochs.push(seq);
        self.previous.push((
            rotation.previous_data.clone(),
            rotation.previous_secrets.clone(),
        ));
        Ok(())
    }

    /// Some admin keeps a device in the vault, so it can still be changed.
    fn admin_remains(&self) -> Result<(), TeamError> {
        if self.present().any(|(_, access)| access.role == Role::Admin) {
            Ok(())
        } else {
            Err(TeamError::NotAllowed(
                "the vault would have no admin device",
            ))
        }
    }
}

/// A new device's wraps: the data key, and the secrets key when its
/// member holds credentials, for the current epoch and nothing else.
fn check_wraps(
    wraps: &[Wrap],
    devices: &[(&DeviceCard, Access)],
    epoch: u32,
) -> Result<(), TeamError> {
    let mut wanted = Vec::new();
    for (card, access) in devices {
        wanted.push((card.id(), KeyKind::Data));
        if access.credentials {
            wanted.push((card.id(), KeyKind::Secrets));
        }
    }
    exact_wraps(wraps, &wanted, epoch)
}

/// Checks that `wraps` are exactly one per wanted device and kind, all
/// for `epoch`. O(w log w).
fn exact_wraps(
    wraps: &[Wrap],
    wanted: &[(DeviceId, KeyKind)],
    epoch: u32,
) -> Result<(), TeamError> {
    let mut want: Vec<_> = wanted.to_vec();
    want.sort_unstable();
    let mut got: Vec<_> = wraps.iter().map(|w| (w.device, w.kind)).collect();
    got.sort_unstable();
    if want != got || wraps.iter().any(|w| w.epoch != epoch) {
        return Err(TeamError::BadEntry(
            "the keys are not sealed to exactly the devices that hold them",
        ));
    }
    Ok(())
}

/// An admin always holds credentials: it seals the secrets key forward on
/// every rotation.
fn checked_access(role: Role, credentials: bool) -> Result<Access, TeamError> {
    if role == Role::Admin && !credentials {
        return Err(TeamError::BadEntry("an admin holds credentials"));
    }
    Ok(Access { role, credentials })
}

/// The epoch keys this device opens: its newest wraps, then each earlier
/// epoch through the keys sealed under the next one.
#[derive(Debug)]
pub struct Keyring {
    vault: VaultId,
    data: BTreeMap<u32, Key>,
    secrets: BTreeMap<u32, Key>,
}

impl Keyring {
    /// Opens this device's newest wrap of each kind, then walks back. A
    /// key an admin sealed wrong stops that kind's walk there: the keys
    /// after it, and the other kind's, still open. O(epochs) key openings.
    pub fn open(log: &Log, me: &DeviceKeys) -> Result<Self, TeamError> {
        let id = me.id();
        let newest = [KeyKind::Data, KeyKind::Secrets]
            .into_iter()
            .filter_map(|kind| Some((kind, log.newest.get(&(id, kind))?)));
        let mut ring = Self {
            vault: log.vault,
            data: BTreeMap::new(),
            secrets: BTreeMap::new(),
        };
        for (kind, wrap) in newest {
            let keys = match kind {
                KeyKind::Data => &mut ring.data,
                KeyKind::Secrets => &mut ring.secrets,
            };
            let Ok(mut key) = seal::unwrap(me, log.vault, wrap) else {
                continue;
            };
            let mut epoch = wrap.epoch;
            while epoch > 0 {
                let (data, secrets) = &log.previous[(epoch - 1) as usize];
                let sealed = if kind == KeyKind::Data { data } else { secrets };
                let Ok(older) = seal::open_previous(&key, log.vault, kind, epoch, sealed) else {
                    break;
                };
                keys.insert(epoch, key);
                key = older;
                epoch -= 1;
            }
            keys.insert(epoch, key);
        }
        Ok(ring)
    }

    pub(crate) fn key(&self, kind: KeyKind, epoch: u32) -> Result<&Key, TeamError> {
        let keys = match kind {
            KeyKind::Data => &self.data,
            KeyKind::Secrets => &self.secrets,
        };
        keys.get(&epoch).ok_or(TeamError::NoKey(epoch))
    }

    pub fn vault(&self) -> VaultId {
        self.vault
    }
}

mod build;

#[cfg(test)]
pub(crate) fn signed_for_tests(entry: &Entry) -> Vec<u8> {
    Entry::signed_bytes(entry.vault, entry.seq, &entry.prev, entry.signer, &entry.op)
}
