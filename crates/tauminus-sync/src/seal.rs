// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! What the target sees: sealed objects, and one key file.
//!
//! Every device holds one random 256-bit sync key. An object is sealed
//! with XChaCha20-Poly1305 under a key HKDF-SHA256 derives from it, its
//! name bound in as associated data, so a sealed object can be neither
//! read, forged, nor passed off under another name without the key. The
//! key file holds the sync key wrapped the same way under a key Argon2id
//! derives from the sync passphrase, which is how a new device joins.

use chacha20poly1305::aead::{Aead, KeyInit as _, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::TryRng as _;
use zeroize::Zeroizing;

const OBJECT: &[u8; 4] = b"TMS1";
/// This device's own sync state on its disk, under its own derived key.
const LOCAL: &[u8; 4] = b"TML1";

/// Whether `file` is a local state file sealed by this build, whatever
/// its key.
#[must_use]
pub fn is_sealed_local(file: &[u8]) -> bool {
    file.starts_with(LOCAL)
}
const KEY_FILE: &[u8; 4] = b"TMK1";
const NONCE: usize = 24;
const SALT: usize = 16;
const KEY: usize = 32;

/// Argon2id costs: the vault's, 64 MiB and three passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kdf {
    pub memory_kib: u32,
    pub iterations: u32,
}

impl Kdf {
    pub const DEFAULT: Self = Self {
        memory_kib: 64 * 1024,
        iterations: 3,
    };
    /// The least a key file may ask for, so a target cannot make the
    /// passphrase cheap to guess.
    pub const LEAST: Self = Self {
        memory_kib: 19 * 1024,
        iterations: 2,
    };
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SealError {
    #[error("the sync passphrase is wrong, or the key file is damaged")]
    Passphrase,
    #[error("an object on the sync target was not written with this sync key, or is damaged")]
    Object,
    #[error("the key file asks for key derivation costs below the least allowed")]
    WeakKdf,
    #[error("no randomness from the system")]
    Random,
}

/// The sync key. Wiped when dropped.
#[derive(Clone)]
pub struct SyncKey(Zeroizing<[u8; KEY]>);

impl std::fmt::Debug for SyncKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SyncKey(..)")
    }
}

fn fill(bytes: &mut [u8]) -> Result<(), SealError> {
    rand::rngs::SysRng
        .try_fill_bytes(bytes)
        .map_err(|_| SealError::Random)
}

impl SyncKey {
    /// A new random key.
    ///
    /// # Errors
    /// No randomness from the system.
    pub fn random() -> Result<Self, SealError> {
        let mut key = Zeroizing::new([0u8; KEY]);
        fill(key.as_mut())?;
        Ok(Self(key))
    }

    /// The key as bytes, to keep in the vault.
    #[must_use]
    pub fn bytes(&self) -> &[u8; KEY] {
        &self.0
    }

    #[must_use]
    pub fn from_bytes(bytes: [u8; KEY]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    fn cipher(&self) -> Result<XChaCha20Poly1305, SealError> {
        self.derived(b"tauminus sync v1 objects")
    }

    fn derived(&self, info: &[u8]) -> Result<XChaCha20Poly1305, SealError> {
        let mut key = Zeroizing::new([0u8; KEY]);
        hkdf::Hkdf::<sha2::Sha256>::new(None, self.0.as_ref())
            .expand(info, key.as_mut())
            .map_err(|_| SealError::Object)?;
        cipher(&key)
    }

    /// `plain`, sealed as this device's local state file `kind`, under a
    /// key apart from the objects': a local file passes for no object.
    /// O(n).
    ///
    /// # Errors
    /// No randomness for the nonce.
    pub fn seal_local(&self, kind: &str, plain: &[u8]) -> Result<Vec<u8>, SealError> {
        let mut nonce = [0u8; NONCE];
        fill(&mut nonce)?;
        let aad = [LOCAL.as_slice(), kind.as_bytes()].concat();
        let sealed = self
            .derived(b"tauminus sync v1 local state")?
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plain,
                    aad: &aad,
                },
            )
            .map_err(|_| SealError::Object)?;
        Ok([LOCAL.as_slice(), &nonce, &sealed].concat())
    }

    /// The plaintext of local state file `kind`. O(n).
    ///
    /// # Errors
    /// A file not sealed with this key as that kind, or damaged.
    pub fn open_local(&self, kind: &str, file: &[u8]) -> Result<Zeroizing<Vec<u8>>, SealError> {
        let rest = file.strip_prefix(LOCAL).ok_or(SealError::Object)?;
        if rest.len() < NONCE {
            return Err(SealError::Object);
        }
        let (nonce, sealed) = rest.split_at(NONCE);
        let nonce: [u8; NONCE] = nonce.try_into().map_err(|_| SealError::Object)?;
        let aad = [LOCAL.as_slice(), kind.as_bytes()].concat();
        self.derived(b"tauminus sync v1 local state")?
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: sealed,
                    aad: &aad,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| SealError::Object)
    }

    /// `plain`, sealed as the object named `name`. O(n).
    ///
    /// # Errors
    /// No randomness for the nonce.
    pub fn seal(&self, name: &str, plain: &[u8]) -> Result<Vec<u8>, SealError> {
        let mut nonce = [0u8; NONCE];
        fill(&mut nonce)?;
        let aad = [OBJECT.as_slice(), name.as_bytes()].concat();
        let sealed = self
            .cipher()?
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plain,
                    aad: &aad,
                },
            )
            .map_err(|_| SealError::Object)?;
        Ok([OBJECT.as_slice(), &nonce, &sealed].concat())
    }

    /// The plaintext of the object named `name`. O(n).
    ///
    /// # Errors
    /// An object not sealed with this key, under this name, or damaged.
    pub fn open(&self, name: &str, object: &[u8]) -> Result<Zeroizing<Vec<u8>>, SealError> {
        let rest = object.strip_prefix(OBJECT).ok_or(SealError::Object)?;
        if rest.len() < NONCE {
            return Err(SealError::Object);
        }
        let (nonce, sealed) = rest.split_at(NONCE);
        let nonce: [u8; NONCE] = nonce.try_into().map_err(|_| SealError::Object)?;
        let aad = [OBJECT.as_slice(), name.as_bytes()].concat();
        self.cipher()?
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: sealed,
                    aad: &aad,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| SealError::Object)
    }

    /// The key file: this key wrapped under `passphrase`. Runs Argon2id,
    /// so call it off the async workers.
    ///
    /// # Errors
    /// No randomness, or costs Argon2id refuses.
    pub fn wrap(&self, passphrase: &str, kdf: Kdf) -> Result<Vec<u8>, SealError> {
        if kdf.memory_kib < Kdf::LEAST.memory_kib || kdf.iterations < Kdf::LEAST.iterations {
            return Err(SealError::WeakKdf);
        }
        let mut salt = [0u8; SALT];
        let mut nonce = [0u8; NONCE];
        fill(&mut salt)?;
        fill(&mut nonce)?;
        let header = [
            KEY_FILE.as_slice(),
            &kdf.memory_kib.to_be_bytes(),
            &kdf.iterations.to_be_bytes(),
            &salt,
            &nonce,
        ]
        .concat();
        let wrapping = derive(passphrase, &salt, kdf)?;
        let sealed = cipher(&wrapping)?
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: self.0.as_ref(),
                    aad: &header,
                },
            )
            .map_err(|_| SealError::Passphrase)?;
        Ok([header, sealed].concat())
    }

    /// The key a key file holds, unwrapped with `passphrase`. Runs
    /// Argon2id at the costs the file names, never below
    /// [`Kdf::LEAST`].
    ///
    /// # Errors
    /// A wrong passphrase, a damaged file, or costs below the least.
    pub fn unwrap(file: &[u8], passphrase: &str) -> Result<Self, SealError> {
        let header_len = 4 + 4 + 4 + SALT + NONCE;
        if file.len() < header_len || !file.starts_with(KEY_FILE) {
            return Err(SealError::Passphrase);
        }
        let (header, sealed) = file.split_at(header_len);
        let word = |at: usize| {
            u32::from_be_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
        };
        let kdf = Kdf {
            memory_kib: word(4),
            iterations: word(8),
        };
        if kdf.memory_kib < Kdf::LEAST.memory_kib || kdf.iterations < Kdf::LEAST.iterations {
            return Err(SealError::WeakKdf);
        }
        // A file asking for more than 1 GiB would stall the device.
        if kdf.memory_kib > 1024 * 1024 || kdf.iterations > 64 {
            return Err(SealError::Passphrase);
        }
        let salt = &header[12..12 + SALT];
        let nonce: [u8; NONCE] = header[12 + SALT..]
            .try_into()
            .map_err(|_| SealError::Passphrase)?;
        let wrapping = derive(passphrase, salt, kdf)?;
        let plain = Zeroizing::new(
            cipher(&wrapping)?
                .decrypt(
                    &XNonce::from(nonce),
                    Payload {
                        msg: sealed,
                        aad: header,
                    },
                )
                .map_err(|_| SealError::Passphrase)?,
        );
        let key: [u8; KEY] = plain
            .as_slice()
            .try_into()
            .map_err(|_| SealError::Passphrase)?;
        Ok(Self::from_bytes(key))
    }
}

fn cipher(key: &[u8; KEY]) -> Result<XChaCha20Poly1305, SealError> {
    XChaCha20Poly1305::new_from_slice(key).map_err(|_| SealError::Object)
}

fn derive(passphrase: &str, salt: &[u8], kdf: Kdf) -> Result<Zeroizing<[u8; KEY]>, SealError> {
    let params = argon2::Params::new(kdf.memory_kib, kdf.iterations, 1, Some(KEY))
        .map_err(|_| SealError::WeakKdf)?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; KEY]);
    argon
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|_| SealError::Passphrase)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST: Kdf = Kdf::LEAST;

    #[test]
    fn an_object_opens_only_with_its_key_and_its_name() {
        let key = SyncKey::random().unwrap();
        let sealed = key.seal("devices/a/1", b"hosts").unwrap();
        assert_eq!(&*key.open("devices/a/1", &sealed).unwrap(), b"hosts");
        assert_eq!(key.open("devices/b/1", &sealed), Err(SealError::Object));
        let other = SyncKey::random().unwrap();
        assert_eq!(other.open("devices/a/1", &sealed), Err(SealError::Object));
        let mut flipped = sealed.clone();
        *flipped.last_mut().unwrap() ^= 1;
        assert_eq!(key.open("devices/a/1", &flipped), Err(SealError::Object));
        // The plaintext is not in the object.
        assert!(!sealed.windows(5).any(|w| w == b"hosts"));
    }

    #[test]
    fn a_local_file_opens_only_as_its_kind_and_never_as_an_object() {
        let key = SyncKey::random().unwrap();
        let sealed = key.seal_local("doc", b"base").unwrap();
        assert_eq!(&*key.open_local("doc", &sealed).unwrap(), b"base");
        assert!(key.open_local("state", &sealed).is_err());
        assert!(key.open("doc", &sealed).is_err());
        let object = key.seal("doc", b"base").unwrap();
        assert!(key.open_local("doc", &object).is_err());
        assert!(
            SyncKey::random()
                .unwrap()
                .open_local("doc", &sealed)
                .is_err()
        );
    }

    #[test]
    fn the_key_file_opens_with_the_passphrase_only() {
        let key = SyncKey::random().unwrap();
        let file = key.wrap("correct horse", FAST).unwrap();
        assert_eq!(
            SyncKey::unwrap(&file, "correct horse").unwrap().bytes(),
            key.bytes()
        );
        assert_eq!(
            SyncKey::unwrap(&file, "wrong").err(),
            Some(SealError::Passphrase)
        );
        // Costs lowered in the file are refused, not run.
        let mut cheap = file.clone();
        cheap[4..8].copy_from_slice(&8u32.to_be_bytes());
        assert_eq!(
            SyncKey::unwrap(&cheap, "correct horse").err(),
            Some(SealError::WeakKdf)
        );
    }
}
