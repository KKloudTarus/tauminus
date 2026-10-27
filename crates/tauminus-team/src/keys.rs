// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! A device's keys, and the card that names them to others.

use std::fmt::Write as _;

use curve25519_dalek::montgomery::MontgomeryPoint;
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use zeroize::Zeroizing;

use crate::TeamError;
use crate::encode::{Encoder, sha256};

/// The longest user id: an OIDC subject or an email address.
const MAX_USER: usize = 256;

/// A member, as the identity provider names them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UserId(String);

impl UserId {
    pub fn new(id: impl Into<String>) -> Result<Self, TeamError> {
        let id = id.into();
        if id.is_empty() || id.len() > MAX_USER || id.chars().any(char::is_control) {
            return Err(TeamError::BadUser);
        }
        Ok(Self(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(pub [u8; 16]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VaultId(pub [u8; 16]);

impl VaultId {
    pub fn random() -> Result<Self, TeamError> {
        let mut id = [0u8; 16];
        crate::seal::fill(&mut id)?;
        Ok(Self(id))
    }
}

/// What others know of a device: whose it is, the key it signs with and
/// the key epoch keys are sealed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCard {
    pub user: UserId,
    pub sign: [u8; 32],
    pub seal: [u8; 32],
}

impl DeviceCard {
    pub(crate) fn encode(&self, out: &mut Encoder) {
        out.bytes(self.user.as_str().as_bytes())
            .fixed(&self.sign)
            .fixed(&self.seal);
    }

    fn digest(&self) -> [u8; 32] {
        let mut out = Encoder::new(b"tauminus-team/card/1");
        self.encode(&mut out);
        sha256(&[&out.finish()])
    }

    /// The first 16 bytes of the card's SHA-256: a device's id is bound to
    /// its keys.
    pub fn id(&self) -> DeviceId {
        let mut id = [0u8; 16];
        id.copy_from_slice(&self.digest()[..16]);
        DeviceId(id)
    }

    /// What two people compare out of band before an admin adds a device:
    /// 160 bits of the card's SHA-256, in ten groups of four hex digits.
    pub fn fingerprint(&self) -> String {
        let digest = self.digest();
        let mut out = String::with_capacity(49);
        for (i, pair) in digest[..20].chunks(2).enumerate() {
            if i > 0 {
                out.push(' ');
            }
            let _ = write!(out, "{:02x}{:02x}", pair[0], pair[1]);
        }
        out
    }

    pub(crate) fn verify(&self, message: &[u8], signature: &[u8; 64]) -> Result<(), TeamError> {
        let key = VerifyingKey::from_bytes(&self.sign).map_err(|_| TeamError::BadSignature)?;
        key.verify_strict(message, &Signature::from_bytes(signature))
            .map_err(|_| TeamError::BadSignature)
    }
}

/// A device's secret keys. They never leave the device.
pub struct DeviceKeys {
    sign: SigningKey,
    seal: Zeroizing<[u8; 32]>,
    card: DeviceCard,
}

impl std::fmt::Debug for DeviceKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceKeys")
            .field("card", &self.card)
            .finish_non_exhaustive()
    }
}

impl DeviceKeys {
    pub fn generate(user: UserId) -> Result<Self, TeamError> {
        let mut sign = Zeroizing::new([0u8; 32]);
        crate::seal::fill(sign.as_mut())?;
        let mut seal = Zeroizing::new([0u8; 32]);
        crate::seal::fill(seal.as_mut())?;
        Ok(Self::from_secrets(user, &sign, seal))
    }

    /// Keys from secrets this device kept.
    pub fn from_secrets(user: UserId, sign: &[u8; 32], seal: Zeroizing<[u8; 32]>) -> Self {
        let sign = SigningKey::from_bytes(sign);
        let card = DeviceCard {
            user,
            sign: sign.verifying_key().to_bytes(),
            seal: MontgomeryPoint::mul_base_clamped(*seal).to_bytes(),
        };
        Self { sign, seal, card }
    }

    pub fn card(&self) -> &DeviceCard {
        &self.card
    }

    pub fn id(&self) -> DeviceId {
        self.card.id()
    }

    pub(crate) fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.sign.sign(message).to_bytes()
    }

    pub(crate) fn seal_secret(&self) -> &Zeroizing<[u8; 32]> {
        &self.seal
    }
}
