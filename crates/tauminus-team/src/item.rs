// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! A shared item, sealed under its epoch's key and signed by the device
//! that wrote it. The header is the associated data and part of what is
//! signed, so an item cannot be moved to another vault, id, epoch or log
//! position, and only a member who could edit at that position wrote it.

use zeroize::Zeroizing;

use crate::encode::{Encoder, sha256};
use crate::keys::{DeviceId, DeviceKeys, VaultId};
use crate::log::{Keyring, Log, Role};
use crate::seal::{self, KeyKind};
use crate::{Hash, TeamError};

const ITEM: &[u8] = b"tauminus-team/item/1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemHeader {
    pub vault: VaultId,
    pub item: [u8; 16],
    pub kind: KeyKind,
    pub epoch: u32,
    /// The log entry the writer held, and its hash.
    pub seq: u64,
    pub head: Hash,
    pub writer: DeviceId,
    /// The writer's count of its own writes: two versions a device wrote
    /// never seal alike. Which version is newer is the sync layer's to
    /// say, from the stamps inside the sealed record (ADR 0030).
    pub counter: u64,
}

impl ItemHeader {
    fn encode(&self) -> Vec<u8> {
        let mut out = Encoder::new(ITEM);
        out.fixed(&self.vault.0)
            .fixed(&self.item)
            .u8(self.kind.tag())
            .u32(self.epoch)
            .u64(self.seq)
            .fixed(&self.head)
            .fixed(&self.writer.0)
            .u64(self.counter);
        out.finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedItem {
    pub header: ItemHeader,
    pub nonce: [u8; 24],
    pub sealed: Vec<u8>,
    pub signature: [u8; 64],
}

impl SealedItem {
    fn signed_bytes(&self) -> Vec<u8> {
        signed_bytes(&self.header, &self.nonce, &self.sealed)
    }

    /// What the Hub stores.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = self.signed_bytes();
        bytes.extend_from_slice(&self.signature);
        bytes
    }

    /// This version's SHA-256, by which an admin keeps it.
    pub fn digest(&self) -> Hash {
        sha256(&[&self.to_bytes()])
    }
}

fn signed_bytes(header: &ItemHeader, nonce: &[u8; 24], sealed: &[u8]) -> Vec<u8> {
    let mut out = Encoder::new(b"tauminus-team/item-signed/1");
    out.bytes(&header.encode()).fixed(nonce).bytes(sealed);
    out.finish()
}

/// Seals `plain` as this device's write of `item`, against the newest
/// log it holds and that log's epoch.
pub fn seal_item(
    log: &Log,
    ring: &Keyring,
    me: &DeviceKeys,
    item: [u8; 16],
    kind: KeyKind,
    counter: u64,
    plain: &[u8],
) -> Result<SealedItem, TeamError> {
    let (seq, head) = log.head();
    may_write(log, me.id(), seq, kind)?;
    let epoch = log.epoch();
    let header = ItemHeader {
        vault: log.vault(),
        item,
        kind,
        epoch,
        seq,
        head,
        writer: me.id(),
        counter,
    };
    let (nonce, sealed) = seal::seal(ring.key(kind, epoch)?, &header.encode(), plain)?;
    let signature = me.sign(&signed_bytes(&header, &nonce, &sealed));
    Ok(SealedItem {
        header,
        nonce,
        sealed,
        signature,
    })
}

/// Opens an item once its writer, log position and epoch check against
/// this device's log. A writer names the log position it wrote against,
/// so one who may no longer write could name an older position: an item
/// from such a writer opens only when an admin kept that very version
/// when the right to write was taken.
pub fn open_item(
    log: &Log,
    ring: &Keyring,
    item: &SealedItem,
) -> Result<Zeroizing<Vec<u8>>, TeamError> {
    let header = &item.header;
    if header.vault != log.vault() || !log.knows(header.seq, &header.head) {
        return Err(TeamError::Fork(header.seq));
    }
    if header.epoch != log.epoch_at(header.seq) {
        return Err(TeamError::NotAllowed(
            "the item is not sealed under the epoch of its log entry",
        ));
    }
    let writer = may_write(log, header.writer, header.seq, header.kind)?;
    writer.verify(&item.signed_bytes(), &item.signature)?;
    let (now, _) = log.head();
    if may_write(log, header.writer, now, header.kind).is_err()
        && !log.kept_after(&item.digest(), header.seq)
    {
        return Err(TeamError::NotAllowed(
            "the writer may no longer write, and no admin kept this version",
        ));
    }
    seal::open(
        ring.key(header.kind, header.epoch)?,
        &header.encode(),
        &item.nonce,
        &item.sealed,
    )
}

/// The writer's card when it was in the vault at `seq` as a member who
/// edits, and, for a secret, who holds credentials.
pub(crate) fn may_write(
    log: &Log,
    writer: DeviceId,
    seq: u64,
    kind: KeyKind,
) -> Result<&crate::DeviceCard, TeamError> {
    let card = log.device_at(writer, seq).ok_or(TeamError::NotAllowed(
        "the writer's device was not in the vault",
    ))?;
    let (role, credentials) = log
        .role_at(&card.user, seq)
        .ok_or(TeamError::NotAllowed("the writer was not a member"))?;
    if role < Role::Edit {
        return Err(TeamError::NotAllowed("the writer could only view"));
    }
    if kind == KeyKind::Secrets && !credentials {
        return Err(TeamError::NotAllowed("the writer held no credentials"));
    }
    Ok(card)
}

#[cfg(test)]
pub(crate) fn header_bytes(header: &ItemHeader) -> Vec<u8> {
    header.encode()
}

#[cfg(test)]
pub(crate) fn signed(item: &SealedItem) -> Vec<u8> {
    item.signed_bytes()
}
