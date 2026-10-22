// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The vault use case's backend: the encrypted file, identities kept as one
//! JSON record each, and the OS keychain holding a second key.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauminus_app::ports::Secret;
use tauminus_app::vault::{
    Identity, KeychainState, OpenVault as OpenPort, VaultBackend, VaultError,
};
use zeroize::Zeroizing;

use crate::vault::{DEFAULT_KDF, Kdf, OpenVault, SecretKey, VaultError as FileError, VaultFile};

/// Where the OS keeps secrets for this user: Keychain on macOS, Credential
/// Manager on Windows, the Secret Service on Linux.
pub trait KeyStore: Send + Sync + 'static {
    /// `Err` with the reason when there is no usable store.
    fn available(&self) -> Result<(), String>;
    fn get(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, String>;
    fn set(&self, name: &str, secret: &[u8]) -> Result<(), String>;
    fn delete(&self, name: &str) -> Result<(), String>;
}

const IDENTITY_PREFIX: &str = "identity-";

/// Identities as stored inside a record. Secrets are wiped when dropped.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdentityRecord {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password: Option<Zeroizing<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    private_key: Option<Zeroizing<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    passphrase: Option<Zeroizing<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    public_key: Option<String>,
}

/// The vault file at its path, with `keys` for the keychain slot.
pub struct FileVaultBackend {
    file: VaultFile,
    keys: Arc<dyn KeyStore>,
    kdf: Kdf,
}

impl std::fmt::Debug for FileVaultBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileVaultBackend")
            .field("file", &self.file)
            .finish_non_exhaustive()
    }
}

impl FileVaultBackend {
    pub fn new(file: VaultFile, keys: Arc<dyn KeyStore>) -> Self {
        Self {
            file,
            keys,
            kdf: DEFAULT_KDF,
        }
    }

    /// Other Argon2id costs, for tests.
    #[must_use]
    pub fn with_kdf(mut self, kdf: Kdf) -> Self {
        self.kdf = kdf;
        self
    }

    fn wrap(&self, vault: OpenVault) -> Box<dyn OpenPort> {
        Box::new(Opened {
            vault,
            file: self.file.clone(),
            keys: Arc::clone(&self.keys),
            pending: None,
        })
    }
}

/// The keychain entry for one vault, so two vaults never share a key.
fn key_name(vault_id: &str) -> String {
    format!("vault-{vault_id}")
}

fn port_error(error: FileError) -> VaultError {
    match error {
        FileError::Missing => VaultError::Missing,
        FileError::Exists => VaultError::Exists,
        FileError::WrongKey => VaultError::WrongKey,
        FileError::Corrupt(message) => VaultError::Corrupt(message),
        FileError::Limit(message) => VaultError::Invalid(message),
        FileError::Io(e) => VaultError::Io(e.to_string()),
        FileError::Random => VaultError::Io(FileError::Random.to_string()),
        FileError::Conflict => VaultError::Conflict,
    }
}

impl VaultBackend for FileVaultBackend {
    fn exists(&self) -> bool {
        self.file.exists()
    }

    fn create(&self, password: &Secret) -> Result<Box<dyn OpenPort>, VaultError> {
        let vault = self
            .file
            .create(password.as_bytes(), self.kdf)
            .map_err(port_error)?;
        Ok(self.wrap(vault))
    }

    fn unlock(&self, password: &Secret) -> Result<Box<dyn OpenPort>, VaultError> {
        let vault = self
            .file
            .open_with_password(password.as_bytes())
            .map_err(port_error)?;
        Ok(self.wrap(vault))
    }

    fn unlock_with_keychain(&self) -> Result<Box<dyn OpenPort>, VaultError> {
        self.keys
            .available()
            .map_err(VaultError::KeychainUnavailable)?;
        let id = self.file.vault_id().map_err(port_error)?;
        let bytes = self
            .keys
            .get(&key_name(&id))
            .map_err(VaultError::KeychainUnavailable)?
            .ok_or_else(|| VaultError::KeychainUnavailable("no key for this vault".into()))?;
        let key = SecretKey::from_bytes(&bytes).map_err(port_error)?;
        let vault = self.file.open_with_key(&key).map_err(|error| match error {
            // The password was never asked; say what is wrong.
            FileError::WrongKey => VaultError::KeychainUnavailable(
                "the keychain's key no longer opens this vault; unlock with the password".into(),
            ),
            other => port_error(other),
        })?;
        Ok(self.wrap(vault))
    }

    fn keychain(&self) -> KeychainState {
        if let Err(reason) = self.keys.available() {
            return KeychainState::Unavailable(reason);
        }
        match self.file.has_keychain_slot() {
            Ok(true) => KeychainState::On,
            _ => KeychainState::Off,
        }
    }

    fn new_id(&self) -> String {
        let mut bytes = [0u8; 16];
        if rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut bytes).is_err() {
            bytes = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
                .to_le_bytes();
        }
        bytes.iter().fold(String::with_capacity(32), |mut out, b| {
            out.push(char::from(crate::HEX[usize::from(b >> 4)]));
            out.push(char::from(crate::HEX[usize::from(b & 0x0f)]));
            out
        })
    }
}

struct Opened {
    vault: OpenVault,
    file: VaultFile,
    keys: Arc<dyn KeyStore>,
    /// The keychain change to make once the file is saved. The keychain
    /// entry must open the file on disk: a new key written before a save
    /// that fails would leave the old slot with nothing to open it.
    pending: Option<Pending>,
}

enum Pending {
    Set(SecretKey),
    Delete,
}

impl Opened {
    /// The identity record `id`, which must exist.
    fn decode(&self, id: &str) -> Result<Identity, VaultError> {
        let bytes = self
            .vault
            .get(&format!("{IDENTITY_PREFIX}{id}"))
            .unwrap_or_default();
        let record: IdentityRecord = serde_json::from_slice(bytes)
            .map_err(|e| VaultError::Corrupt(format!("identity {id}: {e}")))?;
        Ok(Identity {
            id: id.to_owned(),
            name: record.name,
            user: record.user,
            password: record.password,
            private_key: record.private_key,
            passphrase: record.passphrase,
            public_key: record.public_key,
        })
    }
}

impl OpenPort for Opened {
    fn identities(&self) -> Result<Vec<Identity>, VaultError> {
        self.vault
            .ids()
            .filter_map(|id| id.strip_prefix(IDENTITY_PREFIX))
            .map(|id| self.decode(id))
            .collect()
    }

    fn identity(&self, id: &str) -> Result<Option<Identity>, VaultError> {
        if self.vault.get(&format!("{IDENTITY_PREFIX}{id}")).is_none() {
            return Ok(None);
        }
        self.decode(id).map(Some)
    }

    fn identity_count(&self) -> usize {
        self.vault
            .ids()
            .filter(|id| id.starts_with(IDENTITY_PREFIX))
            .count()
    }

    fn put_identity(&mut self, identity: &Identity) -> Result<(), VaultError> {
        let record = IdentityRecord {
            name: identity.name.clone(),
            user: identity.user.clone(),
            password: identity.password.clone(),
            private_key: identity.private_key.clone(),
            passphrase: identity.passphrase.clone(),
            public_key: identity.public_key.clone(),
        };
        let bytes = serde_json::to_vec(&record).map_err(|e| VaultError::Io(e.to_string()))?;
        self.vault
            .put(&format!("{IDENTITY_PREFIX}{}", identity.id), bytes)
            .map_err(port_error)
    }

    fn remove_identity(&mut self, id: &str) -> bool {
        self.vault.remove(&format!("{IDENTITY_PREFIX}{id}"))
    }

    /// The new password comes with a new data key, which drops the keychain
    /// slot; a keychain that was on gets a new key for it.
    fn change_password(&mut self, password: &Secret) -> Result<(), VaultError> {
        let keychain = self.vault.has_keychain_key();
        self.vault
            .change_password(password.as_bytes())
            .map_err(port_error)?;
        if keychain {
            self.new_keychain_key()?;
        }
        Ok(())
    }

    fn set_keychain(&mut self, on: bool) -> Result<(), VaultError> {
        if on {
            self.keys
                .available()
                .map_err(VaultError::KeychainUnavailable)?;
            self.new_keychain_key()
        } else {
            self.vault.remove_keychain_key();
            self.pending = Some(Pending::Delete);
            Ok(())
        }
    }

    /// Saves, then brings the keychain in line with the file.
    fn save(&mut self) -> Result<(), VaultError> {
        self.vault.save(&self.file).map_err(port_error)?;
        let name = key_name(self.vault.id());
        match self.pending.take() {
            None => {}
            Some(Pending::Set(key)) => {
                if let Err(error) = self.keys.set(&name, key.as_bytes()) {
                    // The slot opens nothing now; drop it so the settings
                    // show the keychain off. The password still opens it.
                    tracing::warn!(%error, "could not store the vault key in the keychain");
                    // An older entry opens older copies of the file only;
                    // remove it, as turning the keychain off would.
                    if let Err(error) = self.keys.delete(&name) {
                        tracing::warn!(%error, "could not remove the old vault key");
                    }
                    self.vault.remove_keychain_key();
                    self.vault.save(&self.file).map_err(port_error)?;
                    // File and memory agree; the caller learns the keychain
                    // is off.
                    return Err(VaultError::KeychainUnavailable(error));
                }
            }
            Some(Pending::Delete) => {
                // The slot is gone from the file, so the entry opens only
                // older copies; changing the password replaces the data key.
                if let Err(error) = self.keys.delete(&name) {
                    tracing::warn!(%error, "could not remove the vault key from the keychain");
                }
            }
        }
        Ok(())
    }
}

impl Opened {
    /// A fresh keychain slot, stored in the keychain at the next save.
    fn new_keychain_key(&mut self) -> Result<(), VaultError> {
        let key = SecretKey::random().map_err(port_error)?;
        self.vault.set_keychain_key(&key).map_err(port_error)?;
        self.pending = Some(Pending::Set(key));
        Ok(())
    }
}

#[cfg(test)]
mod tests;
