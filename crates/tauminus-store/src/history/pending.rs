// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Commands recorded while the vault is locked. Each is sealed to the
//! history's X25519 public key and appended to `history.pending`; only the
//! private key, kept in the vault, opens them. So recording never needs a
//! secret, and a locked app cannot read back what it wrote.
//!
//! A frame is `u32 length (LE) | ephemeral public key (32) | nonce (24) |
//! XChaCha20-Poly1305 ciphertext`. The cipher key is HKDF-SHA256 of the
//! X25519 shared secret, salted with both public keys. A crash mid-append
//! leaves a short last frame: the next append cuts it off before writing,
//! and reading stops at it. Every operation holds an exclusive lock on the
//! file, so two windows never interleave frames.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use curve25519_dalek::montgomery::MontgomeryPoint;
use rand::TryRng;
use serde::{Deserialize, Serialize};
use tauminus_domain::HistoryEntry;
use zeroize::Zeroizing;

use super::HistoryError;
use crate::vault::SecretKey;

const INFO: &[u8] = b"tauminus history pending v1";
const AAD: &[u8] = b"tauminus-history-pending/1";
const HEADER: usize = 32 + 24;
/// Largest frame: a bounded entry is under 70 KiB as JSON.
const MAX_FRAME: usize = 256 * 1024;
/// Largest pending file. Past it new entries are dropped until the vault
/// is unlocked and the file imported: about 250 commands with full output,
/// or about 15,000 short ones. Importing it takes a couple of seconds.
const MAX_FILE: u64 = 16 * 1024 * 1024;

/// The history's key pair. The secret half lives in the vault.
#[derive(Clone)]
pub struct HistoryKeyPair {
    secret: SecretKey,
    public: [u8; 32],
}

impl std::fmt::Debug for HistoryKeyPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryKeyPair")
            .field("public", &self.public)
            .finish_non_exhaustive()
    }
}

impl HistoryKeyPair {
    pub fn random() -> Result<Self, HistoryError> {
        Ok(Self::from_secret(random_key()?))
    }

    pub fn from_secret(secret: SecretKey) -> Self {
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(secret.as_bytes());
        let public = MontgomeryPoint::mul_base_clamped(bytes).to_bytes();
        zeroize::Zeroize::zeroize(&mut bytes);
        Self { secret, public }
    }

    pub fn secret(&self) -> &SecretKey {
        &self.secret
    }

    pub fn public(&self) -> [u8; 32] {
        self.public
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EntryRecord {
    host: Option<String>,
    label: String,
    at_ms: i64,
    command: String,
    output: String,
    exit: Option<i32>,
}

/// What reading found: the entries, how many frames would not open, and how
/// many bytes were read, which is all `clear` may remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub entries: Vec<HistoryEntry>,
    pub unreadable: usize,
    pub consumed: u64,
}

/// The pending file at `path`, usually `history.pending` next to the
/// history database.
#[derive(Debug)]
pub struct PendingLog {
    path: PathBuf,
    /// The file's length after this process's last append: when it still
    /// matches, the file ends on a whole frame and need not be walked.
    checked: std::sync::Mutex<Option<u64>>,
}

impl PendingLog {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            checked: std::sync::Mutex::new(None),
        }
    }

    fn checked(&self) -> std::sync::MutexGuard<'_, Option<u64>> {
        self.checked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn open_dir(&self) -> Result<(), HistoryError> {
        if let Some(dir) = self.path.parent() {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder.create(dir)?;
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Seals `entry` to `public` and appends it. Returns `false` when the
    /// file is full and the entry was dropped. O(entry).
    pub fn append(&self, public: &[u8; 32], entry: &HistoryEntry) -> Result<bool, HistoryError> {
        let entry = entry.clone().bounded();
        let record = EntryRecord {
            host: entry.host,
            label: entry.label,
            at_ms: entry.at_ms,
            command: entry.command,
            output: entry.output,
            exit: entry.exit,
        };
        let plain = Zeroizing::new(
            serde_json::to_vec(&record).map_err(|e| HistoryError::Corrupt(e.to_string()))?,
        );
        let ephemeral = random_key()?;
        let mut ephemeral_bytes = Zeroizing::new([0u8; 32]);
        ephemeral_bytes.copy_from_slice(ephemeral.as_bytes());
        let ephemeral_public = MontgomeryPoint::mul_base_clamped(*ephemeral_bytes).to_bytes();
        let shared = Zeroizing::new(
            MontgomeryPoint(*public)
                .mul_clamped(*ephemeral_bytes)
                .to_bytes(),
        );
        let cipher = cipher(&shared, &ephemeral_public, public)?;
        let mut nonce = [0u8; 24];
        fill(&mut nonce)?;
        let sealed = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &plain,
                    aad: AAD,
                },
            )
            .map_err(|_| HistoryError::Corrupt("an entry could not be sealed".into()))?;
        let len = HEADER + sealed.len();
        if len > MAX_FRAME {
            return Err(HistoryError::Corrupt("an entry is too large".into()));
        }
        let mut frame = Vec::with_capacity(4 + len);
        frame.extend_from_slice(&u32::try_from(len).unwrap_or(u32::MAX).to_le_bytes());
        frame.extend_from_slice(&ephemeral_public);
        frame.extend_from_slice(&nonce);
        frame.extend_from_slice(&sealed);

        // Read and write, not append: cutting a torn tail needs write
        // access on every platform, and the write goes where the tail ends.
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        self.open_dir()?;
        let mut file = self.open_locked(&options)?;
        let len = file.metadata()?.len();
        let mut checked = self.checked();
        // A frame cut short by a crash would swallow this one: end the file
        // at the last whole frame first. O(frames) reading headers, and only
        // when another process wrote since this one last did.
        let whole = if *checked == Some(len) {
            len
        } else {
            match whole_frames(&mut file)? {
                Frames::Whole(at) => at,
                // A length no frame has, before the end: what follows it
                // cannot be framed, and cutting it would lose it. The file
                // is set aside as it is, and a new one started.
                Frames::Damaged(good) => {
                    file = self.set_aside(file, &options, good)?;
                    good
                }
            }
        };
        if whole != len {
            file.set_len(whole)?;
        }
        if whole + frame.len() as u64 > MAX_FILE {
            *checked = Some(whole);
            return Ok(false);
        }
        file.seek(SeekFrom::Start(whole))?;
        file.write_all(&frame)?;
        file.sync_data()?;
        *checked = Some(whole + frame.len() as u64);
        Ok(true)
    }

    /// The file at the path, locked. Another process may have set the file
    /// aside while this one waited for the lock: then the lock is on the
    /// old file, and the frame belongs in the new one.
    fn open_locked(&self, options: &std::fs::OpenOptions) -> Result<std::fs::File, HistoryError> {
        let mut file = options.open(&self.path)?;
        file.lock()?;
        #[cfg(unix)]
        for _ in 0..3 {
            use std::os::unix::fs::MetadataExt;
            let held = file.metadata()?;
            match std::fs::metadata(&self.path) {
                Ok(named) if named.ino() == held.ino() && named.dev() == held.dev() => break,
                _ => {
                    file = options.open(&self.path)?;
                    file.lock()?;
                }
            }
        }
        Ok(file)
    }

    /// Sets a damaged file aside as it is, as `history.pending.<ms>.damaged`,
    /// and starts a new one holding the `good` bytes of whole frames before
    /// the damage. Returns the new file, locked.
    fn set_aside(
        &self,
        mut file: std::fs::File,
        options: &std::fs::OpenOptions,
        good: u64,
    ) -> Result<std::fs::File, HistoryError> {
        let aside = self.path.with_extension(format!(
            "pending.{}.damaged",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis())
        ));
        tracing::warn!(path = %aside.display(), "the pending history file is damaged; set aside");
        let mut before = vec![0u8; usize::try_from(good).unwrap_or(0)];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut before)?;
        std::fs::rename(&self.path, &aside)?;
        drop(file);
        let mut file = options.open(&self.path)?;
        file.lock()?;
        file.set_len(0)?;
        file.write_all(&before)?;
        Ok(file)
    }

    /// Every entry `keys` opens, oldest first, and how many frames could not
    /// be opened (damaged, or sealed to another key). A short last frame,
    /// from a crash mid-append, is skipped. O(file).
    pub fn read(&self, keys: &HistoryKeyPair) -> Result<Pending, HistoryError> {
        let mut bytes = Vec::new();
        match std::fs::File::open(&self.path) {
            Ok(file) => {
                file.lock_shared()?;
                (&file)
                    .take(MAX_FILE + MAX_FRAME as u64)
                    .read_to_end(&mut bytes)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Pending {
                    entries: Vec::new(),
                    unreadable: 0,
                    consumed: 0,
                });
            }
            Err(e) => return Err(e.into()),
        }
        Ok(open_frames(&bytes, &[keys]))
    }

    /// Opens every entry `keys` opens and hands them to `store`; once it
    /// succeeds, removes the frames it was given. One exclusive lock covers
    /// it all, so two windows unlocking at once import each entry once, and
    /// frames appended meanwhile wait for the lock. Returns how many frames
    /// would not open. O(file).
    pub fn import(
        &self,
        keys: &[&HistoryKeyPair],
        store: &mut dyn FnMut(&[HistoryEntry]) -> Result<(), HistoryError>,
    ) -> Result<usize, HistoryError> {
        let mut file = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        file.lock()?;
        let mut bytes = Vec::new();
        (&file)
            .take(MAX_FILE + MAX_FRAME as u64)
            .read_to_end(&mut bytes)?;
        let read = open_frames(&bytes, keys);
        if !read.entries.is_empty() {
            store(&read.entries)?;
        }
        remove_front(&mut file, read.consumed)?;
        *self.checked() = None;
        Ok(read.unreadable)
    }

    /// Removes the first `consumed` bytes, once their entries are in the
    /// database. Frames another window appended since the read stay.
    pub fn clear(&self, consumed: u64) -> Result<(), HistoryError> {
        let mut file = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        file.lock()?;
        remove_front(&mut file, consumed)?;
        *self.checked() = None;
        Ok(())
    }
}

/// Opens the whole frames at the start of `bytes`: the entries `keys`
/// opens, how many would not open, and how many bytes they took. Stops at a
/// short last frame, or at a length no frame has.
/// One frame's plaintext, when `keys` sealed it.
fn open_frame(frame: &[u8], keys: &HistoryKeyPair) -> Option<Zeroizing<Vec<u8>>> {
    let mut secret = Zeroizing::new([0u8; 32]);
    secret.copy_from_slice(keys.secret.as_bytes());
    let mut ephemeral = [0u8; 32];
    ephemeral.copy_from_slice(&frame[..32]);
    let mut nonce = [0u8; 24];
    nonce.copy_from_slice(&frame[32..HEADER]);
    let shared = Zeroizing::new(MontgomeryPoint(ephemeral).mul_clamped(*secret).to_bytes());
    let cipher = cipher(&shared, &ephemeral, &keys.public).ok()?;
    cipher
        .decrypt(
            &XNonce::from(nonce),
            Payload {
                msg: &frame[HEADER..],
                aad: AAD,
            },
        )
        .ok()
        .map(Zeroizing::new)
}

/// The entries in `bytes` any of `keys` opens: the current pair, and the
/// one before a password change. O(frames · keys).
fn open_frames(bytes: &[u8], keys: &[&HistoryKeyPair]) -> Pending {
    let mut entries = Vec::new();
    let mut unreadable = 0;
    let mut rest = bytes;
    while rest.len() >= 4 {
        let len = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        if !(HEADER + 16..=MAX_FRAME).contains(&len) {
            // A length that cannot be right: nothing after it can be
            // framed either.
            unreadable += 1;
            break;
        }
        let Some(frame) = rest.get(4..4 + len) else {
            break;
        };
        rest = &rest[4 + len..];
        let Some(plain) = keys.iter().find_map(|pair| open_frame(frame, pair)) else {
            unreadable += 1;
            continue;
        };
        match serde_json::from_slice::<EntryRecord>(&plain) {
            Ok(r) => entries.push(HistoryEntry {
                host: r.host,
                label: r.label,
                at_ms: r.at_ms,
                command: r.command,
                output: r.output,
                exit: r.exit,
            }),
            Err(_) => unreadable += 1,
        }
    }
    let consumed = (bytes.len() - rest.len()) as u64;
    Pending {
        entries,
        unreadable,
        consumed,
    }
}

/// Drops the first `consumed` bytes of `file`, keeping what follows.
fn remove_front(file: &mut std::fs::File, consumed: u64) -> Result<(), HistoryError> {
    let len = file.metadata()?.len();
    if consumed >= len {
        file.set_len(0)?;
    } else if consumed > 0 {
        // The frames after `consumed` move to the front, then the end is
        // cut: a crash between the two never loses a frame, and may leave
        // a copy of the last ones after them, imported twice.
        let mut rest = Vec::new();
        file.seek(SeekFrom::Start(consumed))?;
        (&*file).take(MAX_FILE).read_to_end(&mut rest)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&rest)?;
        file.sync_data()?;
        file.set_len(rest.len() as u64)?;
    }
    file.sync_all()?;
    Ok(())
}

/// The length of the file's whole frames, from their headers. Stops at a
/// frame that runs past the end or has a length no frame has.
/// Where the whole frames end, or that the file is damaged before then.
enum Frames {
    /// The end of the last whole frame; past it, at most a torn frame.
    Whole(u64),
    /// A frame's length no frame can have, after this many bytes of whole
    /// frames.
    Damaged(u64),
}

fn whole_frames(file: &mut std::fs::File) -> Result<Frames, HistoryError> {
    let len = file.metadata()?.len();
    let mut at = 0u64;
    let mut header = [0u8; 4];
    loop {
        if at + 4 > len {
            return Ok(Frames::Whole(at));
        }
        file.seek(SeekFrom::Start(at))?;
        file.read_exact(&mut header)?;
        let frame = u64::from(u32::from_le_bytes(header));
        if !(HEADER as u64 + 16..=MAX_FRAME as u64).contains(&frame) {
            return Ok(Frames::Damaged(at));
        }
        // A whole length running past the end: a crash cut this frame.
        if at + 4 + frame > len {
            return Ok(Frames::Whole(at));
        }
        at += 4 + frame;
    }
}

/// XChaCha20-Poly1305 under HKDF-SHA256 of the shared secret. An all-zero
/// shared secret means a low-order public key, which is refused.
fn cipher(
    shared: &[u8; 32],
    ephemeral: &[u8; 32],
    public: &[u8; 32],
) -> Result<XChaCha20Poly1305, HistoryError> {
    if shared.iter().all(|b| *b == 0) {
        return Err(HistoryError::Corrupt("a key is not usable".into()));
    }
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(ephemeral);
    salt[32..].copy_from_slice(public);
    let mut key = Zeroizing::new([0u8; 32]);
    hkdf::Hkdf::<sha2::Sha256>::new(Some(&salt), shared)
        .expand(INFO, key.as_mut())
        .map_err(|_| HistoryError::Corrupt("key derivation".into()))?;
    XChaCha20Poly1305::new_from_slice(key.as_ref())
        .map_err(|_| HistoryError::Corrupt("key length".into()))
}

fn random_key() -> Result<SecretKey, HistoryError> {
    SecretKey::random().map_err(|_| HistoryError::Corrupt("no random source".into()))
}

fn fill(buf: &mut [u8]) -> Result<(), HistoryError> {
    rand::rngs::SysRng
        .try_fill_bytes(buf)
        .map_err(|_| HistoryError::Corrupt("no random source".into()))
}

#[cfg(test)]
mod tests;
