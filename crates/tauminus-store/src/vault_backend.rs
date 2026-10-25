// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The vault use case's backend: the encrypted file, identities kept as one
//! JSON record each, and the OS keychain holding a second key.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauminus_app::ports::{S3Target, Secret, SyncTarget};
use tauminus_app::vault::{
    AiKey, HistoryKeys, Identity, KeychainState, OpenVault as OpenPort, PreviousHistoryKeys,
    RecordingKeys, SyncSetup, VaultBackend, VaultError,
};
use zeroize::Zeroizing;

use crate::history::HistoryKeyPair;
use crate::vault::{
    DEFAULT_KDF, Kdf, OpenVault, SecretKey, SlotKind, VaultError as FileError, VaultFile,
};

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
            kdf: self.kdf,
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

    fn unlock_with_keychain(&self, presence: bool) -> Result<Box<dyn OpenPort>, VaultError> {
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
        let kind = SlotKind::keychain(presence);
        let vault = self
            .file
            .open_with_key(&key, kind)
            .map_err(|error| match error {
                // The password was never asked; say what is wrong.
                FileError::WrongKey => VaultError::KeychainUnavailable(
                    "the keychain's key no longer opens this vault; unlock with the password"
                        .into(),
                ),
                other => port_error(other),
            })?;
        Ok(self.wrap(vault))
    }

    fn keychain(&self) -> KeychainState {
        if let Err(reason) = self.keys.available() {
            return KeychainState::Unavailable(reason);
        }
        match self.file.keychain_slot() {
            Ok(Some(kind)) => KeychainState::On {
                presence: kind == SlotKind::KeychainPresence,
            },
            _ => KeychainState::Off,
        }
    }

    fn history_public(&self) -> Option<[u8; 32]> {
        self.file.history_public().ok().flatten()
    }

    fn recordings_public(&self) -> Option<[u8; 32]> {
        self.file.recordings_public().ok().flatten()
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
    /// The least Argon2id costs a new password gets.
    kdf: Kdf,
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
    /// The history's keys are replaced too, so an old copy of the vault
    /// and the old password open nothing recorded after the change; the
    /// old ones stay as `previous` until the history has moved.
    fn change_password(&mut self, password: &Secret) -> Result<(), VaultError> {
        let keychain = self.vault.keychain_kind();
        self.vault
            .change_password(password.as_bytes(), self.kdf)
            .map_err(port_error)?;
        if let Some(kind) = keychain {
            self.new_keychain_key(kind)?;
        }
        self.rotate_history_keys()
    }

    fn set_keychain(&mut self, on: bool) -> Result<(), VaultError> {
        if on {
            self.keys
                .available()
                .map_err(VaultError::KeychainUnavailable)?;
            // A fresh key keeps the slot's kind: turning the keychain on
            // again never drops the presence check.
            let kind = self.vault.keychain_kind().unwrap_or(SlotKind::Keychain);
            self.new_keychain_key(kind)
        } else {
            self.vault.remove_keychain_key();
            self.pending = Some(Pending::Delete);
            Ok(())
        }
    }

    /// A fresh keychain key with a slot of the other kind: the old key
    /// opens nothing after the save.
    fn set_presence(&mut self, on: bool) -> Result<(), VaultError> {
        if self.vault.keychain_kind().is_none() {
            return Err(VaultError::Invalid(
                "turn on unlocking with the keychain first".into(),
            ));
        }
        self.keys
            .available()
            .map_err(VaultError::KeychainUnavailable)?;
        self.new_keychain_key(SlotKind::keychain(on))
    }

    fn forget_previous_history_keys(&mut self, known: &[PreviousHistoryKeys]) -> bool {
        let Ok(stored) = self.previous_history_keys() else {
            return false;
        };
        let same = |a: &PreviousHistoryKeys, b: &PreviousHistoryKeys| {
            a.database[..] == b.database[..] && a.sealing[..] == b.sealing[..]
        };
        let (gone, kept): (Vec<_>, Vec<_>) = stored
            .into_iter()
            .partition(|generation| known.iter().any(|k| same(k, generation)));
        if gone.is_empty() {
            return false;
        }
        self.store_previous_history_keys(&kept).is_ok()
    }

    fn history_keys(&mut self) -> Result<(HistoryKeys, bool), VaultError> {
        let stored = (
            self.vault.get(HISTORY_DATABASE).map(SecretKey::from_bytes),
            self.vault.get(HISTORY_SEALING).map(SecretKey::from_bytes),
        );
        match stored {
            (Some(Ok(database)), Some(Ok(sealing))) => {
                let pair = HistoryKeyPair::from_secret(sealing);
                if self.vault.history_public() != Some(pair.public()) {
                    return Err(VaultError::Corrupt(
                        "the history key does not match the vault".into(),
                    ));
                }
                let mut keys = history_keys(&database, &pair);
                keys.previous = self.previous_history_keys()?;
                return Ok((keys, false));
            }
            (None, None) => {}
            // Making new keys would orphan the history: say so instead.
            _ => {
                return Err(VaultError::Corrupt(
                    "the history keys are damaged or half missing".into(),
                ));
            }
        }
        let database = SecretKey::random().map_err(port_error)?;
        let pair = HistoryKeyPair::random().map_err(|e| VaultError::Io(e.to_string()))?;
        self.vault
            .put(HISTORY_DATABASE, database.as_bytes().to_vec())
            .map_err(port_error)?;
        self.vault
            .put(HISTORY_SEALING, pair.secret().as_bytes().to_vec())
            .map_err(port_error)?;
        self.vault.set_history_public(pair.public());
        Ok((history_keys(&database, &pair), true))
    }

    fn recording_keys(&mut self) -> Result<(RecordingKeys, bool), VaultError> {
        if let Some(stored) = self.vault.get(RECORDINGS_SEALING) {
            let pair =
                HistoryKeyPair::from_secret(SecretKey::from_bytes(stored).map_err(port_error)?);
            if self.vault.recordings_public() != Some(pair.public()) {
                return Err(VaultError::Corrupt(
                    "the recordings key does not match the vault".into(),
                ));
            }
            return Ok((recording_keys(&pair), false));
        }
        let pair = HistoryKeyPair::random().map_err(|e| VaultError::Io(e.to_string()))?;
        self.vault
            .put(RECORDINGS_SEALING, pair.secret().as_bytes().to_vec())
            .map_err(port_error)?;
        self.vault.set_recordings_public(pair.public());
        Ok((recording_keys(&pair), true))
    }

    /// Kept as the origin, a NUL, and the key.
    fn ai_key(&self) -> Result<Option<AiKey>, VaultError> {
        let Some(stored) = self.vault.get(AI_KEY) else {
            return Ok(None);
        };
        let corrupt = || VaultError::Corrupt("the AI key record is damaged".into());
        let split = stored.iter().position(|&b| b == 0).ok_or_else(corrupt)?;
        let origin = std::str::from_utf8(&stored[..split]).map_err(|_| corrupt())?;
        let key = std::str::from_utf8(&stored[split + 1..]).map_err(|_| corrupt())?;
        Ok(Some(AiKey {
            origin: origin.to_owned(),
            key: Secret::new(key.to_owned()),
        }))
    }

    fn set_ai_key(&mut self, key: Option<&AiKey>) -> Result<(), VaultError> {
        let Some(key) = key else {
            self.vault.remove(AI_KEY);
            return Ok(());
        };
        if key.origin.contains('\0') {
            return Err(VaultError::Invalid("the origin holds a NUL".into()));
        }
        let mut plain = Vec::with_capacity(key.origin.len() + 1 + key.key.len());
        plain.extend_from_slice(key.origin.as_bytes());
        plain.push(0);
        plain.extend_from_slice(key.key.as_bytes());
        // `put` wipes what it is given.
        self.vault.put(AI_KEY, plain).map_err(port_error)
    }

    /// Kept as the kind (`f`, `g` or `s`), the 32-byte key, and the
    /// address; a bucket's is its endpoint, region, bucket, folder, key id
    /// and secret, each ended by a NUL but the last.
    fn sync_setup(&self) -> Result<Option<SyncSetup>, VaultError> {
        let Some(stored) = self.vault.get(SYNC) else {
            return Ok(None);
        };
        let corrupt = || VaultError::Corrupt("the sync record is damaged".into());
        let (&kind, rest) = stored.split_first().ok_or_else(corrupt)?;
        if rest.len() < 32 {
            return Err(corrupt());
        }
        let (key, address) = rest.split_at(32);
        let address = std::str::from_utf8(address).map_err(|_| corrupt())?;
        let target = match kind {
            b'f' => SyncTarget::Folder(address.to_owned()),
            b'g' => SyncTarget::Git(Secret::new(address.to_owned())),
            b's' => {
                let parts: Vec<&str> = address.split('\0').collect();
                let [endpoint, region, bucket, prefix, key_id, secret] = parts[..] else {
                    return Err(corrupt());
                };
                SyncTarget::S3(S3Target {
                    endpoint: endpoint.to_owned(),
                    region: region.to_owned(),
                    bucket: bucket.to_owned(),
                    prefix: prefix.to_owned(),
                    key_id: key_id.to_owned(),
                    secret: Secret::new(secret.to_owned()),
                })
            }
            _ => return Err(corrupt()),
        };
        let mut bytes = Zeroizing::new([0; 32]);
        bytes.copy_from_slice(key);
        Ok(Some(SyncSetup { target, key: bytes }))
    }

    fn set_sync_setup(&mut self, setup: Option<&SyncSetup>) -> Result<(), VaultError> {
        let Some(setup) = setup else {
            self.vault.remove(SYNC);
            return Ok(());
        };
        let parts: Vec<&str> = match &setup.target {
            SyncTarget::Folder(path) => vec![path],
            SyncTarget::Git(url) => vec![url],
            SyncTarget::S3(s3) => vec![
                &s3.endpoint,
                &s3.region,
                &s3.bucket,
                &s3.prefix,
                &s3.key_id,
                &s3.secret,
            ],
        };
        if parts.iter().any(|p| p.contains('\0')) {
            return Err(VaultError::Invalid("a sync setting holds a NUL".into()));
        }
        let kind = match &setup.target {
            SyncTarget::Folder(_) => b'f',
            SyncTarget::Git(_) => b'g',
            SyncTarget::S3(_) => b's',
        };
        let address = Zeroizing::new(parts.join("\0"));
        let mut plain = Vec::with_capacity(1 + 32 + address.len());
        plain.push(kind);
        plain.extend_from_slice(&*setup.key);
        plain.extend_from_slice(address.as_bytes());
        // `put` wipes what it is given.
        self.vault.put(SYNC, plain).map_err(port_error)
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
    /// The history's keys from before each password change since it last
    /// moved, newest first: one 32-byte key after another in each record.
    fn previous_history_keys(&self) -> Result<Vec<PreviousHistoryKeys>, VaultError> {
        let (database, sealing) = (
            self.vault
                .get(HISTORY_DATABASE_PREVIOUS)
                .unwrap_or_default(),
            self.vault.get(HISTORY_SEALING_PREVIOUS).unwrap_or_default(),
        );
        if database.len() != sealing.len()
            || database.len() % 32 != 0
            || database.len() > 32 * MAX_GENERATIONS
        {
            return Err(VaultError::Corrupt(
                "the history's previous keys are damaged".into(),
            ));
        }
        let (databases, _) = database.as_chunks::<32>();
        let (sealings, _) = sealing.as_chunks::<32>();
        Ok(databases
            .iter()
            .zip(sealings)
            .map(|(database, sealing)| PreviousHistoryKeys {
                database: Zeroizing::new(*database),
                sealing: Zeroizing::new(*sealing),
            })
            .collect())
    }

    /// Writes the history's earlier key generations, or removes the records
    /// when there are none.
    fn store_previous_history_keys(
        &mut self,
        generations: &[PreviousHistoryKeys],
    ) -> Result<(), VaultError> {
        if generations.is_empty() {
            self.vault.remove(HISTORY_DATABASE_PREVIOUS);
            self.vault.remove(HISTORY_SEALING_PREVIOUS);
            return Ok(());
        }
        let mut databases = Zeroizing::new(Vec::with_capacity(32 * generations.len()));
        let mut sealings = Zeroizing::new(Vec::with_capacity(32 * generations.len()));
        for generation in generations {
            databases.extend_from_slice(&generation.database[..]);
            sealings.extend_from_slice(&generation.sealing[..]);
        }
        self.vault
            .put(HISTORY_DATABASE_PREVIOUS, databases.to_vec())
            .map_err(port_error)?;
        self.vault
            .put(HISTORY_SEALING_PREVIOUS, sealings.to_vec())
            .map_err(port_error)
    }

    /// New history keys, with the current ones put first among the
    /// previous: every generation the history has not confirmed stays, since
    /// the database may be under any of them and entries sealed to any.
    /// Nothing to do before the history's first use.
    fn rotate_history_keys(&mut self) -> Result<(), VaultError> {
        let key = |id: &str| {
            self.vault.get(id).map(|bytes| {
                <[u8; 32]>::try_from(bytes)
                    .map(Zeroizing::new)
                    .map_err(|_| VaultError::Corrupt("a history key is not 32 bytes".into()))
            })
        };
        let (Some(database), Some(sealing)) = (key(HISTORY_DATABASE), key(HISTORY_SEALING)) else {
            return Ok(());
        };
        let mut generations = vec![PreviousHistoryKeys {
            database: database?,
            sealing: sealing?,
        }];
        generations.extend(self.previous_history_keys()?);
        if generations.len() > MAX_GENERATIONS {
            // Eight changes and the history never opened: the oldest goes.
            tracing::warn!("the history's oldest key is dropped; what it holds may be lost");
            generations.truncate(MAX_GENERATIONS);
        }
        self.store_previous_history_keys(&generations)?;
        let database = SecretKey::random().map_err(port_error)?;
        let pair = HistoryKeyPair::random().map_err(|e| VaultError::Io(e.to_string()))?;
        self.vault
            .put(HISTORY_DATABASE, database.as_bytes().to_vec())
            .map_err(port_error)?;
        self.vault
            .put(HISTORY_SEALING, pair.secret().as_bytes().to_vec())
            .map_err(port_error)?;
        self.vault.set_history_public(pair.public());
        Ok(())
    }

    /// A fresh keychain slot of `kind`, stored in the keychain at the next
    /// save.
    fn new_keychain_key(&mut self, kind: SlotKind) -> Result<(), VaultError> {
        let key = SecretKey::random().map_err(port_error)?;
        self.vault
            .set_keychain_key(&key, kind)
            .map_err(port_error)?;
        self.pending = Some(Pending::Set(key));
        Ok(())
    }
}

/// The AI assistant's API key.
const AI_KEY: &str = "ai-key";
/// Where this device syncs, and the sync key.
const SYNC: &str = "sync";
/// Vault records holding the history's keys.
const HISTORY_DATABASE: &str = "history-database";
/// Never rotated: every recording made is sealed to it.
const RECORDINGS_SEALING: &str = "recordings-sealing";
const HISTORY_SEALING: &str = "history-sealing";
/// The same, from before each password change the history has not moved past.
const HISTORY_DATABASE_PREVIOUS: &str = "history-database-previous";
const HISTORY_SEALING_PREVIOUS: &str = "history-sealing-previous";
/// Most key generations kept for a history that has not moved: eight
/// password changes without the history opening once.
const MAX_GENERATIONS: usize = 8;

fn recording_keys(pair: &HistoryKeyPair) -> RecordingKeys {
    let mut secret = Zeroizing::new([0u8; 32]);
    secret.copy_from_slice(pair.secret().as_bytes());
    RecordingKeys {
        secret,
        public: pair.public(),
    }
}

fn history_keys(database: &SecretKey, pair: &HistoryKeyPair) -> HistoryKeys {
    let key = |secret: &SecretKey| {
        let mut out = Zeroizing::new([0u8; 32]);
        out.copy_from_slice(secret.as_bytes());
        out
    };
    HistoryKeys {
        database: key(database),
        sealing: key(pair.secret()),
        public: pair.public(),
        previous: Vec::new(),
    }
}

#[cfg(test)]
mod tests;
