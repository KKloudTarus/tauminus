// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Epoch keys sealed to a device, and to the next epoch's key.
//!
//! A wrap is an ephemeral X25519 key, HKDF-SHA256 over the shared secret
//! salted with both public keys, and XChaCha20-Poly1305 with the vault,
//! epoch, device and key kind as associated data: a wrap opens only for
//! the device, epoch and kind it was made for.

use chacha20poly1305::aead::{Aead as _, KeyInit as _, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use curve25519_dalek::montgomery::MontgomeryPoint;
use rand::TryRng as _;
use zeroize::Zeroizing;

use crate::TeamError;
use crate::encode::Encoder;
use crate::keys::{DeviceCard, DeviceId, DeviceKeys, VaultId};

pub(crate) type Key = Zeroizing<[u8; 32]>;

const WRAP_INFO: &[u8] = b"tauminus-team/wrap/1";

/// Which of an epoch's two keys: the data key every member's devices
/// hold, or the secrets key only members granted credentials hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyKind {
    Data,
    Secrets,
}

impl KeyKind {
    pub(crate) fn tag(self) -> u8 {
        match self {
            Self::Data => 1,
            Self::Secrets => 2,
        }
    }
}

/// One epoch key sealed to one device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrap {
    pub device: DeviceId,
    pub kind: KeyKind,
    pub epoch: u32,
    pub ephemeral: [u8; 32],
    pub nonce: [u8; 24],
    pub sealed: Vec<u8>,
}

impl Wrap {
    pub(crate) fn encode(&self, out: &mut Encoder) {
        out.fixed(&self.device.0)
            .u8(self.kind.tag())
            .u32(self.epoch)
            .fixed(&self.ephemeral)
            .fixed(&self.nonce)
            .bytes(&self.sealed);
    }
}

/// A key sealed under another key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedKey {
    pub nonce: [u8; 24],
    pub sealed: Vec<u8>,
}

impl SealedKey {
    pub(crate) fn encode(&self, out: &mut Encoder) {
        out.fixed(&self.nonce).bytes(&self.sealed);
    }
}

fn wrap_aad(vault: VaultId, device: DeviceId, kind: KeyKind, epoch: u32) -> Vec<u8> {
    let mut out = Encoder::new(WRAP_INFO);
    out.fixed(&vault.0)
        .fixed(&device.0)
        .u8(kind.tag())
        .u32(epoch);
    out.finish()
}

/// XChaCha20-Poly1305 under HKDF-SHA256 of an X25519 shared secret. An
/// all-zero shared secret means a low-order public key, which is refused.
fn wrap_cipher(
    shared: &[u8; 32],
    ephemeral: &[u8; 32],
    public: &[u8; 32],
) -> Result<XChaCha20Poly1305, TeamError> {
    if shared.iter().all(|b| *b == 0) {
        return Err(TeamError::Sealed);
    }
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(ephemeral);
    salt[32..].copy_from_slice(public);
    let mut key = Zeroizing::new([0u8; 32]);
    hkdf::Hkdf::<sha2::Sha256>::new(Some(&salt), shared)
        .expand(WRAP_INFO, key.as_mut())
        .map_err(|_| TeamError::Sealed)?;
    XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| TeamError::Sealed)
}

pub(crate) fn wrap(
    to: &DeviceCard,
    vault: VaultId,
    epoch: u32,
    kind: KeyKind,
    key: &[u8; 32],
) -> Result<Wrap, TeamError> {
    let device = to.id();
    let mut secret = Zeroizing::new([0u8; 32]);
    fill(secret.as_mut())?;
    let ephemeral = MontgomeryPoint::mul_base_clamped(*secret).to_bytes();
    let shared = Zeroizing::new(MontgomeryPoint(to.seal).mul_clamped(*secret).to_bytes());
    let cipher = wrap_cipher(&shared, &ephemeral, &to.seal)?;
    let mut nonce = [0u8; 24];
    fill(&mut nonce)?;
    let aad = wrap_aad(vault, device, kind, epoch);
    let sealed = cipher
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: key,
                aad: &aad,
            },
        )
        .map_err(|_| TeamError::Sealed)?;
    Ok(Wrap {
        device,
        kind,
        epoch,
        ephemeral,
        nonce,
        sealed,
    })
}

pub(crate) fn unwrap(keys: &DeviceKeys, vault: VaultId, wrap: &Wrap) -> Result<Key, TeamError> {
    let shared = Zeroizing::new(
        MontgomeryPoint(wrap.ephemeral)
            .mul_clamped(**keys.seal_secret())
            .to_bytes(),
    );
    let cipher = wrap_cipher(&shared, &wrap.ephemeral, &keys.card().seal)?;
    let aad = wrap_aad(vault, wrap.device, wrap.kind, wrap.epoch);
    let plain = Zeroizing::new(
        cipher
            .decrypt(
                &XNonce::from(wrap.nonce),
                Payload {
                    msg: &wrap.sealed,
                    aad: &aad,
                },
            )
            .map_err(|_| TeamError::Sealed)?,
    );
    to_key(&plain)
}

fn chain_aad(vault: VaultId, kind: KeyKind, epoch: u32) -> Vec<u8> {
    let mut out = Encoder::new(b"tauminus-team/previous/1");
    out.fixed(&vault.0).u8(kind.tag()).u32(epoch);
    out.finish()
}

/// Seals epoch `epoch - 1`'s key under epoch `epoch`'s, so whoever holds
/// a key holds every one before it.
pub(crate) fn seal_previous(
    under: &[u8; 32],
    vault: VaultId,
    kind: KeyKind,
    epoch: u32,
    previous: &[u8; 32],
) -> Result<SealedKey, TeamError> {
    let (nonce, sealed) = seal(under, &chain_aad(vault, kind, epoch), previous)?;
    Ok(SealedKey { nonce, sealed })
}

pub(crate) fn open_previous(
    under: &[u8; 32],
    vault: VaultId,
    kind: KeyKind,
    epoch: u32,
    sealed: &SealedKey,
) -> Result<Key, TeamError> {
    let plain = open(
        under,
        &chain_aad(vault, kind, epoch),
        &sealed.nonce,
        &sealed.sealed,
    )?;
    to_key(&plain)
}

/// XChaCha20-Poly1305 under `key` with a random nonce.
pub(crate) fn seal(
    key: &[u8; 32],
    aad: &[u8],
    plain: &[u8],
) -> Result<([u8; 24], Vec<u8>), TeamError> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| TeamError::Sealed)?;
    let mut nonce = [0u8; 24];
    fill(&mut nonce)?;
    let sealed = cipher
        .encrypt(&XNonce::from(nonce), Payload { msg: plain, aad })
        .map_err(|_| TeamError::Sealed)?;
    Ok((nonce, sealed))
}

pub(crate) fn open(
    key: &[u8; 32],
    aad: &[u8],
    nonce: &[u8; 24],
    sealed: &[u8],
) -> Result<Zeroizing<Vec<u8>>, TeamError> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| TeamError::Sealed)?;
    cipher
        .decrypt(&XNonce::from(*nonce), Payload { msg: sealed, aad })
        .map(Zeroizing::new)
        .map_err(|_| TeamError::Sealed)
}

fn to_key(plain: &[u8]) -> Result<Key, TeamError> {
    let mut key = Zeroizing::new([0u8; 32]);
    if plain.len() != 32 {
        return Err(TeamError::Sealed);
    }
    key.copy_from_slice(plain);
    Ok(key)
}

pub(crate) fn random_key() -> Result<Key, TeamError> {
    let mut key = Zeroizing::new([0u8; 32]);
    fill(key.as_mut())?;
    Ok(key)
}

pub(crate) fn fill(buf: &mut [u8]) -> Result<(), TeamError> {
    rand::rngs::SysRng
        .try_fill_bytes(buf)
        .map_err(|_| TeamError::Random)
}
