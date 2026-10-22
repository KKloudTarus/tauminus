// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The vault file: secrets encrypted at rest.
//!
//! A random 256-bit data key encrypts each record with XChaCha20-Poly1305
//! under a fresh 192-bit random nonce, so nonces never repeat in practice.
//! The data key itself is stored only wrapped, once per way of unlocking:
//! by a key Argon2id derives from the master password, and optionally by a
//! random key the OS keychain holds. Each record's additional data binds it
//! to this vault and to its record id, so records cannot be swapped between
//! places or vaults without failing authentication.
//!
//! The file is JSON, written atomically and private to the user, like the
//! host list. Reading refuses KDF parameters outside fixed bounds, so a
//! tampered file cannot make unlocking take gigabytes or hours.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::TryRng;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::{MAX_FILE, fingerprint, read_limited, write_atomic};

const FORMAT: &str = "tauminus-vault";
const VERSION: u32 = 1;
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;
const SALT_LEN: usize = 16;
const ID_LEN: usize = 16;
const TAG_LEN: usize = 16;

/// Argon2id costs for a new vault: 64 MiB, 3 passes, 1 lane, the second
/// recommended setting of RFC 9106 with one lane. About 0.3 s on a laptop.
pub const DEFAULT_KDF: Kdf = Kdf {
    memory_kib: 64 * 1024,
    iterations: 3,
    parallelism: 1,
};
/// Bounds on costs read from a file. The floor refuses a downgrade below
/// OWASP's minimum (19 MiB, 2 passes); the ceiling refuses a file that
/// would make unlocking take most of the machine's memory or minutes.
const MIN_MEMORY_KIB: u32 = 19 * 1024;
const MAX_MEMORY_KIB: u32 = 1024 * 1024;
const MIN_ITERATIONS: u32 = 2;
const MAX_ITERATIONS: u32 = 16;
const MAX_PARALLELISM: u32 = 8;
/// Most records, and largest record, a vault holds.
pub const MAX_RECORDS: usize = 4096;
pub const MAX_RECORD: usize = 64 * 1024;
/// Most ways to unlock one vault: the password and a few keychains.
const MAX_SLOTS: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("no vault exists yet")]
    Missing,
    #[error("a vault already exists")]
    Exists,
    /// The password, or the keychain key, does not open this vault.
    #[error("the password is wrong")]
    WrongKey,
    #[error("the vault file is damaged: {0}")]
    Corrupt(String),
    #[error("vault: {0}")]
    Limit(String),
    #[error("could not read or write the vault: {0}")]
    Io(#[from] std::io::Error),
    /// Another window saved the vault since this one opened it.
    #[error("the vault changed in another window; lock it and unlock again to reload")]
    Conflict,
    #[error("the system's random number source failed")]
    Random,
}

impl From<crate::StoreError> for VaultError {
    fn from(error: crate::StoreError) -> Self {
        match error {
            crate::StoreError::Io(e) => Self::Io(e),
            crate::StoreError::Corrupt(message) => Self::Corrupt(message),
            crate::StoreError::Conflict => Self::Conflict,
        }
    }
}

/// Argon2id costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Kdf {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

impl Kdf {
    fn check(self) -> Result<Self, VaultError> {
        let ok = (MIN_MEMORY_KIB..=MAX_MEMORY_KIB).contains(&self.memory_kib)
            && (MIN_ITERATIONS..=MAX_ITERATIONS).contains(&self.iterations)
            && (1..=MAX_PARALLELISM).contains(&self.parallelism);
        if ok {
            Ok(self)
        } else {
            Err(VaultError::Corrupt(format!(
                "key derivation costs {self:?} are outside the accepted range"
            )))
        }
    }
}

/// How a slot unwraps the data key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SlotKind {
    Password,
    Keychain,
}

impl SlotKind {
    fn label(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Keychain => "keychain",
        }
    }
}

/// A 256-bit key, wiped when dropped.
#[derive(Clone)]
pub struct SecretKey(Zeroizing<[u8; KEY_LEN]>);

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretKey(..)")
    }
}

impl SecretKey {
    /// A fresh key from the OS random source.
    pub fn random() -> Result<Self, VaultError> {
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        fill(key.as_mut())?;
        Ok(Self(key))
    }

    /// From 32 bytes, such as a keychain entry; anything else is refused.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, VaultError> {
        let array: [u8; KEY_LEN] = bytes
            .try_into()
            .map_err(|_| VaultError::Corrupt("a key is not 32 bytes".into()))?;
        Ok(Self(Zeroizing::new(array)))
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_ref()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct File {
    format: String,
    version: u32,
    id: String,
    kdf: KdfRecord,
    slots: Vec<SlotRecord>,
    records: Vec<Record>,
    /// The record ids and nonces, sealed under the data key: deleting a
    /// record or putting back an older one fails the check on open.
    manifest: Sealed,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Sealed {
    nonce: String,
    data: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KdfRecord {
    algorithm: String,
    #[serde(flatten)]
    costs: Kdf,
    salt: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SlotRecord {
    kind: SlotKind,
    nonce: String,
    key: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    id: String,
    nonce: String,
    data: String,
}

/// An unlocked vault: the data key and the decrypted records, all wiped
/// when dropped. Changes stay in memory until [`OpenVault::save`].
pub struct OpenVault {
    id: String,
    kdf: Kdf,
    salt: [u8; SALT_LEN],
    data_key: SecretKey,
    slots: Vec<SlotRecord>,
    records: BTreeMap<String, Zeroizing<Vec<u8>>>,
    /// The file as this vault last read or wrote it, so a save never
    /// replaces another window's change (such as a new password).
    seen: Option<u64>,
}

impl std::fmt::Debug for OpenVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenVault")
            .field("id", &self.id)
            .field("records", &self.records.len())
            .finish_non_exhaustive()
    }
}

/// The vault at `path`, usually `vault.json` next to the host list.
#[derive(Debug, Clone)]
pub struct VaultFile {
    path: PathBuf,
}

impl VaultFile {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// A new empty vault opened by `password`, written to disk. Runs
    /// Argon2id, so call it off the async workers. Refuses to replace an
    /// existing vault.
    pub fn create(&self, password: &[u8], kdf: Kdf) -> Result<OpenVault, VaultError> {
        if self.exists() {
            return Err(VaultError::Exists);
        }
        let kdf = kdf.check()?;
        let mut id = [0u8; ID_LEN];
        fill(&mut id)?;
        let mut salt = [0u8; SALT_LEN];
        fill(&mut salt)?;
        let mut vault = OpenVault {
            id: hex(&id),
            kdf,
            salt,
            data_key: SecretKey::random()?,
            slots: Vec::new(),
            records: BTreeMap::new(),
            seen: None,
        };
        let wrapping = derive(password, &salt, kdf)?;
        vault.set_slot(SlotKind::Password, &wrapping)?;
        vault.save(self)?;
        Ok(vault)
    }

    /// Opens the vault with the master password. Runs Argon2id.
    pub fn open_with_password(&self, password: &[u8]) -> Result<OpenVault, VaultError> {
        let (file, seen) = self.read()?;
        let costs = file.kdf.costs.check()?;
        let salt = decode_exact::<SALT_LEN>(&file.kdf.salt, "salt")?;
        let wrapping = derive(password, &salt, costs)?;
        open(file, seen, SlotKind::Password, &wrapping)
    }

    /// Opens the vault with the key a keychain holds.
    pub fn open_with_key(&self, key: &SecretKey) -> Result<OpenVault, VaultError> {
        let (file, seen) = self.read()?;
        open(file, seen, SlotKind::Keychain, key)
    }

    /// The vault's id, without unlocking.
    pub fn vault_id(&self) -> Result<String, VaultError> {
        Ok(self.read()?.0.id)
    }

    /// Whether a keychain slot exists, without unlocking.
    pub fn has_keychain_slot(&self) -> Result<bool, VaultError> {
        Ok(self
            .read()?
            .0
            .slots
            .iter()
            .any(|s| s.kind == SlotKind::Keychain))
    }

    /// The parsed file and the fingerprint of its bytes.
    fn read(&self) -> Result<(File, u64), VaultError> {
        let bytes = read_limited(&self.path)?.ok_or(VaultError::Missing)?;
        let file: File =
            serde_json::from_slice(&bytes).map_err(|e| VaultError::Corrupt(e.to_string()))?;
        if file.format != FORMAT {
            return Err(VaultError::Corrupt("this is not a Tauminus vault".into()));
        }
        if file.version != VERSION {
            return Err(VaultError::Corrupt(format!(
                "it is version {}, and this build reads version {VERSION}; update Tauminus",
                file.version
            )));
        }
        if file.kdf.algorithm != "argon2id" {
            return Err(VaultError::Corrupt(format!(
                "unknown key derivation {}",
                file.kdf.algorithm
            )));
        }
        if file.slots.len() > MAX_SLOTS || file.records.len() > MAX_RECORDS {
            return Err(VaultError::Corrupt("too many slots or records".into()));
        }
        check_id(&file.id)?;
        Ok((file, fingerprint(&bytes)))
    }
}

fn open(
    file: File,
    seen: u64,
    kind: SlotKind,
    wrapping: &SecretKey,
) -> Result<OpenVault, VaultError> {
    let File {
        id,
        kdf,
        slots,
        records,
        manifest,
        ..
    } = file;
    // Checked on every path, so a keychain unlock never carries costs the
    // password path would refuse into the next save.
    let costs = kdf.costs.check()?;
    let salt = decode_exact::<SALT_LEN>(&kdf.salt, "salt")?;
    let aad = slot_aad(&id, kind, costs, &salt);
    let mut data_key = None;
    for slot in slots.iter().filter(|s| s.kind == kind) {
        let nonce = decode_exact::<NONCE_LEN>(&slot.nonce, "slot nonce")?;
        let sealed = decode(&slot.key)?;
        if let Ok(key) = unseal(wrapping, &nonce, &sealed, &aad) {
            data_key = Some(SecretKey::from_bytes(&key)?);
            break;
        }
    }
    let data_key = data_key.ok_or(VaultError::WrongKey)?;
    let nonce = decode_exact::<NONCE_LEN>(&manifest.nonce, "manifest nonce")?;
    let listed = unseal(
        &data_key,
        &nonce,
        &decode(&manifest.data)?,
        &manifest_aad(&id, costs, &salt),
    )
    .map_err(|()| VaultError::Corrupt("the record list fails its check".into()))?;
    if *listed != manifest_of(&records) {
        return Err(VaultError::Corrupt(
            "records were removed, added, or replaced by older ones".into(),
        ));
    }
    let mut opened = BTreeMap::new();
    for record in records {
        if !valid_record_id(&record.id) {
            return Err(VaultError::Corrupt("a record id is not valid".into()));
        }
        let nonce = decode_exact::<NONCE_LEN>(&record.nonce, "record nonce")?;
        let sealed = decode(&record.data)?;
        // The data key opened a slot, so a record that fails to open was
        // changed or truncated, not unlocked with the wrong key.
        let plain = unseal(&data_key, &nonce, &sealed, &record_aad(&id, &record.id))
            .map_err(|()| VaultError::Corrupt(format!("record {} fails its check", record.id)))?;
        if opened.insert(record.id.clone(), plain).is_some() {
            return Err(VaultError::Corrupt(format!(
                "record {} is listed twice",
                record.id
            )));
        }
    }
    Ok(OpenVault {
        id,
        kdf: costs,
        salt,
        data_key,
        slots,
        records: opened,
        seen: Some(seen),
    })
}

impl OpenVault {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.records.keys().map(String::as_str)
    }

    pub fn get(&self, id: &str) -> Option<&[u8]> {
        self.records.get(id).map(|r| r.as_slice())
    }

    /// Adds or replaces a record. Ids are 1 to 64 ASCII letters, digits, `-`,
    /// or `_`.
    pub fn put(&mut self, id: &str, plain: Vec<u8>) -> Result<(), VaultError> {
        let plain = Zeroizing::new(plain);
        if !valid_record_id(id) {
            return Err(VaultError::Limit(format!("record id {id:?} is not valid")));
        }
        if plain.len() > MAX_RECORD {
            return Err(VaultError::Limit("a record is over 64 KiB".into()));
        }
        if !self.records.contains_key(id) && self.records.len() >= MAX_RECORDS {
            return Err(VaultError::Limit(format!("at most {MAX_RECORDS} records")));
        }
        self.records.insert(id.to_owned(), plain);
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> bool {
        self.records.remove(id).is_some()
    }

    /// A new keychain slot opening this vault with `key`, replacing any
    /// keychain slot before it.
    pub fn set_keychain_key(&mut self, key: &SecretKey) -> Result<(), VaultError> {
        self.set_slot(SlotKind::Keychain, key)
    }

    pub fn remove_keychain_key(&mut self) {
        self.slots.retain(|s| s.kind != SlotKind::Keychain);
    }

    pub fn has_keychain_key(&self) -> bool {
        self.slots.iter().any(|s| s.kind == SlotKind::Keychain)
    }

    /// Replaces the master password, with a fresh salt, and a fresh data
    /// key: an old copy of the file and the old password then open nothing
    /// saved after this. Every other slot is dropped, since it wraps the
    /// old key; the caller adds a new keychain slot if it wants one. The
    /// records are encrypted again on the next save. Runs Argon2id.
    pub fn change_password(&mut self, password: &[u8]) -> Result<(), VaultError> {
        // The manifest authenticated these costs when the vault opened.
        let kdf = self.kdf;
        let mut salt = [0u8; SALT_LEN];
        fill(&mut salt)?;
        let wrapping = derive(password, &salt, kdf)?;
        let data_key = SecretKey::random()?;
        // Everything that can fail runs before anything changes.
        let slot = wrap_slot(
            &self.id,
            SlotKind::Password,
            kdf,
            &salt,
            &data_key,
            &wrapping,
        )?;
        self.kdf = kdf;
        self.salt = salt;
        self.data_key = data_key;
        self.slots = vec![slot];
        Ok(())
    }

    fn set_slot(&mut self, kind: SlotKind, wrapping: &SecretKey) -> Result<(), VaultError> {
        let slot = wrap_slot(
            &self.id,
            kind,
            self.kdf,
            &self.salt,
            &self.data_key,
            wrapping,
        )?;
        self.slots.retain(|s| s.kind != kind);
        self.slots.push(slot);
        self.slots.sort_by_key(|s| s.kind);
        Ok(())
    }

    /// Encrypts every record with a fresh nonce and writes the file
    /// atomically. Refuses when another window saved the file since this
    /// vault read it. O(r) for r records.
    pub fn save(&mut self, file: &VaultFile) -> Result<(), VaultError> {
        let mut records = Vec::with_capacity(self.records.len());
        for (id, plain) in &self.records {
            let mut nonce = [0u8; NONCE_LEN];
            fill(&mut nonce)?;
            let sealed = seal(&self.data_key, &nonce, plain, &record_aad(&self.id, id))?;
            records.push(Record {
                id: id.clone(),
                nonce: encode(&nonce),
                data: encode(&sealed),
            });
        }
        let mut nonce = [0u8; NONCE_LEN];
        fill(&mut nonce)?;
        let listed = seal(
            &self.data_key,
            &nonce,
            &manifest_of(&records),
            &manifest_aad(&self.id, self.kdf, &self.salt),
        )?;
        let out = File {
            manifest: Sealed {
                nonce: encode(&nonce),
                data: encode(&listed),
            },
            format: FORMAT.into(),
            version: VERSION,
            id: self.id.clone(),
            kdf: KdfRecord {
                algorithm: "argon2id".into(),
                costs: self.kdf,
                salt: encode(&self.salt),
            },
            slots: self.slots.clone(),
            records,
        };
        let mut bytes = serde_json::to_vec_pretty(&out)
            .map_err(|e| VaultError::Io(std::io::Error::other(e)))?;
        bytes.push(b'\n');
        // A file reading refuses would lose every record at the next unlock.
        if bytes.len() as u64 > MAX_FILE {
            return Err(VaultError::Limit(format!(
                "the vault would be over {} MiB",
                MAX_FILE / 1024 / 1024
            )));
        }
        // Another process saving between the check and the rename would be
        // lost; the lock file makes check and write one step.
        let _lock = lock_file(&file.path)?;
        let current = read_limited(&file.path)?.as_deref().map(fingerprint);
        if current != self.seen {
            return Err(VaultError::Conflict);
        }
        let mut name = [0u8; 16];
        fill(&mut name)?;
        write_atomic(&file.path, &bytes, &hex(&name))?;
        self.seen = Some(fingerprint(&bytes));
        Ok(())
    }
}

/// An exclusive lock on `<vault>.lock`, held until the handle drops. The
/// lock file holds nothing; it only orders writers.
fn lock_file(path: &Path) -> Result<std::fs::File, VaultError> {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    if let Some(dir) = path.parent() {
        // Private, as `write_atomic` makes it: this may create it first.
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let lock = options.open(PathBuf::from(name))?;
    // Bounded: a writer stuck elsewhere must not hang every vault call here.
    for _ in 0..50 {
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            Err(std::fs::TryLockError::WouldBlock) => {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "saves run on a blocking thread, never on an async worker"
                )]
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
    }
    Err(VaultError::Io(std::io::Error::other(
        "another Tauminus window is saving the vault; try again",
    )))
}

/// A slot's additional data. The password slot also binds the KDF costs
/// and salt, so editing them fails authentication instead of being saved
/// again.
fn slot_aad(vault: &str, kind: SlotKind, kdf: Kdf, salt: &[u8]) -> Vec<u8> {
    let mut aad = format!("{FORMAT}/{VERSION}/slot/{}/{vault}", kind.label());
    if kind == SlotKind::Password {
        let Kdf {
            memory_kib,
            iterations,
            parallelism,
        } = kdf;
        let _ = write!(
            aad,
            "/argon2id/{memory_kib}/{iterations}/{parallelism}/{}",
            hex(salt)
        );
    }
    aad.into_bytes()
}

/// Binds the KDF costs and salt too, so every unlock path, the keychain's
/// included, refuses a file whose costs or salt were edited.
fn manifest_aad(vault: &str, kdf: Kdf, salt: &[u8]) -> Vec<u8> {
    let Kdf {
        memory_kib,
        iterations,
        parallelism,
    } = kdf;
    format!(
        "{FORMAT}/{VERSION}/manifest/{vault}/argon2id/{memory_kib}/{iterations}/{parallelism}/{}",
        hex(salt)
    )
    .into_bytes()
}

/// The data key wrapped by `wrapping` for one slot.
fn wrap_slot(
    vault: &str,
    kind: SlotKind,
    kdf: Kdf,
    salt: &[u8],
    data_key: &SecretKey,
    wrapping: &SecretKey,
) -> Result<SlotRecord, VaultError> {
    let mut nonce = [0u8; NONCE_LEN];
    fill(&mut nonce)?;
    let sealed = seal(
        wrapping,
        &nonce,
        data_key.as_bytes(),
        &slot_aad(vault, kind, kdf, salt),
    )?;
    Ok(SlotRecord {
        kind,
        nonce: encode(&nonce),
        key: encode(&sealed),
    })
}

/// Each record's id and nonce, in file order. A nonce is fresh on every
/// save, so it names one version of a record. O(r).
fn manifest_of(records: &[Record]) -> Vec<u8> {
    let mut out = Vec::with_capacity(records.len() * 64);
    for record in records {
        out.extend_from_slice(record.id.as_bytes());
        out.push(b' ');
        out.extend_from_slice(record.nonce.as_bytes());
        out.push(b'\n');
    }
    out
}

fn record_aad(vault: &str, record: &str) -> Vec<u8> {
    format!("{FORMAT}/{VERSION}/record/{vault}/{record}").into_bytes()
}

fn derive(password: &[u8], salt: &[u8], kdf: Kdf) -> Result<SecretKey, VaultError> {
    let params = argon2::Params::new(
        kdf.memory_kib,
        kdf.iterations,
        kdf.parallelism,
        Some(KEY_LEN),
    )
    .map_err(|e| VaultError::Corrupt(format!("key derivation costs: {e}")))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    argon
        .hash_password_into(password, salt, key.as_mut())
        .map_err(|e| VaultError::Corrupt(format!("key derivation: {e}")))?;
    Ok(SecretKey(key))
}

fn seal(
    key: &SecretKey,
    nonce: &[u8; NONCE_LEN],
    plain: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, VaultError> {
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_bytes())
        .map_err(|_| VaultError::Corrupt("a key is not 32 bytes".into()))?;
    cipher
        .encrypt(&XNonce::from(*nonce), Payload { msg: plain, aad })
        .map_err(|_| VaultError::Limit("a record could not be encrypted".into()))
}

/// The plaintext, or `Err(())` when authentication fails.
fn unseal(
    key: &SecretKey,
    nonce: &[u8; NONCE_LEN],
    sealed: &[u8],
    aad: &[u8],
) -> Result<Zeroizing<Vec<u8>>, ()> {
    if sealed.len() < TAG_LEN {
        return Err(());
    }
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_bytes()).map_err(|_| ())?;
    cipher
        .decrypt(&XNonce::from(*nonce), Payload { msg: sealed, aad })
        .map(Zeroizing::new)
        .map_err(|_| ())
}

fn fill(bytes: &mut [u8]) -> Result<(), VaultError> {
    rand::rngs::SysRng
        .try_fill_bytes(bytes)
        .map_err(|_| VaultError::Random)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(crate::HEX[usize::from(b >> 4)]));
        out.push(char::from(crate::HEX[usize::from(b & 0x0f)]));
    }
    out
}

fn encode(bytes: &[u8]) -> String {
    use base64ct::Encoding as _;
    base64ct::Base64::encode_string(bytes)
}

fn decode(text: &str) -> Result<Vec<u8>, VaultError> {
    use base64ct::Encoding as _;
    base64ct::Base64::decode_vec(text).map_err(|_| VaultError::Corrupt("bad base64".into()))
}

fn decode_exact<const N: usize>(text: &str, what: &str) -> Result<[u8; N], VaultError> {
    let mut bytes = decode(text)?;
    let array: Result<[u8; N], _> = bytes.as_slice().try_into();
    bytes.zeroize();
    array.map_err(|_| VaultError::Corrupt(format!("{what} is not {N} bytes")))
}

/// 1 to 64 ASCII letters, digits, `-`, or `_`: never a space or newline,
/// which separate entries in the manifest.
fn valid_record_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn check_id(text: &str) -> Result<(), VaultError> {
    let valid = text.len() == ID_LEN * 2 && text.bytes().all(|b| b.is_ascii_hexdigit());
    if valid {
        Ok(())
    } else {
        Err(VaultError::Corrupt(
            "the vault id is not 16 hex bytes".into(),
        ))
    }
}

#[cfg(test)]
mod tests;
