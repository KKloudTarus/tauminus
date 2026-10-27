// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The entries an admin's device writes. Each comes back signed and is
//! applied through [`Log::extend`] like any other, so a device checks its
//! own changes the way it checks everyone else's.

use super::{Access, Entry, Keyring, Log, Op, Role, Rotation, checked_access};
use crate::item::{SealedItem, may_write, open_item};
use crate::keys::{DeviceCard, DeviceId, DeviceKeys, UserId, VaultId};
use crate::seal::{self, KeyKind, Wrap};
use crate::{Hash, TeamError};

impl Log {
    /// A new vault with `creator` as its admin, and its first entry,
    /// whose hash the invites carry.
    pub fn create(vault: VaultId, creator: &DeviceKeys) -> Result<(Self, Entry), TeamError> {
        let data = seal::random_key()?;
        let secrets = seal::random_key()?;
        let card = creator.card();
        let wraps = vec![
            seal::wrap(card, vault, 0, KeyKind::Data, &data)?,
            seal::wrap(card, vault, 0, KeyKind::Secrets, &secrets)?,
        ];
        let entry = sign(
            vault,
            0,
            [0; 32],
            creator,
            Op::Create {
                creator: card.clone(),
                wraps,
            },
        );
        let log = Self::verify(&entry.hash(), [&entry])?;
        Ok((log, entry))
    }

    pub fn add_member(
        &self,
        admin: &DeviceKeys,
        ring: &Keyring,
        card: &DeviceCard,
        role: Role,
        credentials: bool,
    ) -> Result<Entry, TeamError> {
        let access = checked_access(role, credentials)?;
        self.unseen(card)?;
        let wraps = self.current_wraps(ring, &[(card.clone(), access)])?;
        Ok(self.signed(
            admin,
            Op::AddMember {
                card: card.clone(),
                role,
                credentials,
                wraps,
            },
        ))
    }

    pub fn add_device(
        &self,
        admin: &DeviceKeys,
        ring: &Keyring,
        card: &DeviceCard,
    ) -> Result<Entry, TeamError> {
        let access = self
            .current(&card.user)
            .ok_or(TeamError::BadEntry("a device of no member"))?;
        self.unseen(card)?;
        let wraps = self.current_wraps(ring, &[(card.clone(), access)])?;
        Ok(self.signed(
            admin,
            Op::AddDevice {
                card: card.clone(),
                wraps,
            },
        ))
    }

    /// A new role or credentials grant. `items` are the vault's items this
    /// device holds; the versions whose writer loses the right to write
    /// are kept, so readers go on accepting them.
    pub fn set_access(
        &self,
        admin: &DeviceKeys,
        ring: &Keyring,
        user: &UserId,
        (role, credentials): (Role, bool),
        items: &[SealedItem],
    ) -> Result<Entry, TeamError> {
        let access = checked_access(role, credentials)?;
        let before = self
            .current(user)
            .ok_or(TeamError::BadEntry("not a member"))?;
        let mut wraps = Vec::new();
        let mut rotation = None;
        if before.credentials && !credentials {
            let remaining: Vec<(DeviceCard, Access)> = self
                .present()
                .map(|(card, a)| {
                    let a = if &card.user == user { access } else { a };
                    (card.clone(), a)
                })
                .collect();
            rotation = Some(self.rotation(ring, &remaining)?);
        } else if !before.credentials && credentials {
            let secrets = ring.key(KeyKind::Secrets, self.epoch())?;
            for (card, _) in self.present().filter(|(card, _)| &card.user == user) {
                wraps.push(seal::wrap(
                    card,
                    self.vault,
                    self.epoch(),
                    KeyKind::Secrets,
                    secrets,
                )?);
            }
        }
        let mut op = Op::SetAccess {
            user: user.clone(),
            role,
            credentials,
            wraps,
            rotation,
            kept: Vec::new(),
        };
        let digests = self.still_valid(ring, &op, items)?;
        if let Op::SetAccess { kept, .. } = &mut op {
            *kept = digests;
        }
        Ok(self.signed(admin, op))
    }

    /// Removes a member and every device of theirs, and starts an epoch
    /// whose keys none of them holds. Their versions among `items` are
    /// kept.
    pub fn remove_member(
        &self,
        admin: &DeviceKeys,
        ring: &Keyring,
        user: &UserId,
        items: &[SealedItem],
    ) -> Result<Entry, TeamError> {
        let remaining: Vec<(DeviceCard, Access)> = self
            .present()
            .filter(|(card, _)| &card.user != user)
            .map(|(card, a)| (card.clone(), a))
            .collect();
        let mut op = Op::RemoveMember {
            user: user.clone(),
            rotation: self.rotation(ring, &remaining)?,
            kept: Vec::new(),
        };
        let digests = self.still_valid(ring, &op, items)?;
        if let Op::RemoveMember { kept, .. } = &mut op {
            *kept = digests;
        }
        Ok(self.signed(admin, op))
    }

    pub fn remove_device(
        &self,
        admin: &DeviceKeys,
        ring: &Keyring,
        device: DeviceId,
        items: &[SealedItem],
    ) -> Result<Entry, TeamError> {
        let remaining: Vec<(DeviceCard, Access)> = self
            .present()
            .filter(|(card, _)| card.id() != device)
            .map(|(card, a)| (card.clone(), a))
            .collect();
        let mut op = Op::RemoveDevice {
            device,
            rotation: self.rotation(ring, &remaining)?,
            kept: Vec::new(),
        };
        let digests = self.still_valid(ring, &op, items)?;
        if let Op::RemoveDevice { kept, .. } = &mut op {
            *kept = digests;
        }
        Ok(self.signed(admin, op))
    }

    /// A device enters a vault once: it is never added back after it was
    /// removed, since its removal is what kept its later writes out.
    fn unseen(&self, card: &DeviceCard) -> Result<(), TeamError> {
        if self.devices.contains_key(&card.id()) {
            return Err(TeamError::BadEntry("the device was in the vault before"));
        }
        Ok(())
    }

    /// The digests of the `items` that open now, whose writer may no
    /// longer write once `op` applies, and that no entry kept before.
    /// O(items) signature checks and openings.
    fn still_valid(
        &self,
        ring: &Keyring,
        op: &Op,
        items: &[SealedItem],
    ) -> Result<Vec<Hash>, TeamError> {
        let seq = self.now();
        let mut after = self.clone();
        after.change(seq, op)?;
        Ok(items
            .iter()
            .filter(|item| {
                let h = &item.header;
                may_write(&after, h.writer, seq, h.kind).is_err()
                    && !self.kept.contains_key(&item.digest())
                    && open_item(self, ring, item).is_ok()
            })
            .map(SealedItem::digest)
            .collect())
    }

    /// The current epoch's keys sealed to new devices.
    fn current_wraps(
        &self,
        ring: &Keyring,
        devices: &[(DeviceCard, Access)],
    ) -> Result<Vec<Wrap>, TeamError> {
        let epoch = self.epoch();
        let data = ring.key(KeyKind::Data, epoch)?;
        let mut wraps = Vec::with_capacity(devices.len() * 2);
        for (card, access) in devices {
            wraps.push(seal::wrap(card, self.vault, epoch, KeyKind::Data, data)?);
            if access.credentials {
                let secrets = ring.key(KeyKind::Secrets, epoch)?;
                wraps.push(seal::wrap(
                    card,
                    self.vault,
                    epoch,
                    KeyKind::Secrets,
                    secrets,
                )?);
            }
        }
        Ok(wraps)
    }

    /// Fresh keys sealed to `remaining`, and this epoch's sealed under
    /// them. O(devices) wraps.
    fn rotation(
        &self,
        ring: &Keyring,
        remaining: &[(DeviceCard, Access)],
    ) -> Result<Rotation, TeamError> {
        let epoch = self.epoch() + 1;
        let data = seal::random_key()?;
        let secrets = seal::random_key()?;
        let mut wraps = Vec::with_capacity(remaining.len() * 2);
        for (card, access) in remaining {
            wraps.push(seal::wrap(card, self.vault, epoch, KeyKind::Data, &data)?);
            if access.credentials {
                wraps.push(seal::wrap(
                    card,
                    self.vault,
                    epoch,
                    KeyKind::Secrets,
                    &secrets,
                )?);
            }
        }
        let previous_data = seal::seal_previous(
            &data,
            self.vault,
            KeyKind::Data,
            epoch,
            ring.key(KeyKind::Data, epoch - 1)?,
        )?;
        let previous_secrets = seal::seal_previous(
            &secrets,
            self.vault,
            KeyKind::Secrets,
            epoch,
            ring.key(KeyKind::Secrets, epoch - 1)?,
        )?;
        Ok(Rotation {
            epoch,
            wraps,
            previous_data,
            previous_secrets,
        })
    }

    fn signed(&self, admin: &DeviceKeys, op: Op) -> Entry {
        let (seq, prev) = self.head();
        sign(self.vault, seq + 1, prev, admin, op)
    }
}

fn sign(vault: VaultId, seq: u64, prev: Hash, signer: &DeviceKeys, op: Op) -> Entry {
    let message = Entry::signed_bytes(vault, seq, &prev, signer.id(), &op);
    Entry {
        vault,
        seq,
        prev,
        signer: signer.id(),
        op,
        signature: signer.sign(&message),
    }
}
