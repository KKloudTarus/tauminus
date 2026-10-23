// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! An append-only file of frames sealed to an X25519 public key, for
//! session recordings and what goes with them. Writing needs only the
//! public key, so a locked app can record; reading needs the secret one,
//! which only the vault holds.
//!
//! The file is `magic (8) | ephemeral public key (32)`, then frames of
//! `u32 length (LE) | nonce (24) | XChaCha20-Poly1305 ciphertext`. The key
//! is HKDF-SHA256 of the X25519 shared secret, salted with both public
//! keys. Each frame's additional data names the stream's kind and file id,
//! its index, and whether it is the last: a frame the app wrote, moved,
//! dropped, copied from another file, or put after the end fails to open,
//! and a file whose last frame is missing reads as cut short. Sealing says
//! who can read a file, not who wrote it: anyone with the public key can
//! make one, so what is read back is checked as any input is.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use curve25519_dalek::montgomery::MontgomeryPoint;
use rand::TryRng;
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"TMSEAL\x00\x01";
const NONCE: usize = 24;
const TAG: usize = 16;
/// Plain bytes per frame at most; a larger write is split.
pub(crate) const FRAME_PLAIN: usize = 256 * 1024;

#[derive(Debug, thiserror::Error)]
pub(crate) enum SealedError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("not a sealed Tauminus file")]
    NotSealed,
    #[error("frame {0} fails its check: the file was changed or the key is wrong")]
    Tampered(u64),
    #[error("the system random number generator failed")]
    Random,
}

/// What a stream holds; part of every frame's additional data, so a frame
/// of one kind never opens as another.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Kind {
    Recording,
    Checkpoints,
    Notes,
}

impl Kind {
    const fn label(self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::Checkpoints => "checkpoints",
            Self::Notes => "notes",
        }
    }
}

fn aad(kind: Kind, id: &str, index: u64, last: bool) -> Vec<u8> {
    format!(
        "tauminus-sealed/1/{}/{id}/{index}/{}",
        kind.label(),
        u8::from(last)
    )
    .into_bytes()
}

fn cipher(
    shared: &[u8; 32],
    ephemeral: &[u8; 32],
    public: &[u8; 32],
) -> Result<XChaCha20Poly1305, SealedError> {
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(ephemeral);
    salt[32..].copy_from_slice(public);
    let mut key = Zeroizing::new([0u8; 32]);
    hkdf::Hkdf::<sha2::Sha256>::new(Some(&salt), shared)
        .expand(b"tauminus sealed stream v1", key.as_mut())
        .map_err(|_| SealedError::Random)?;
    XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| SealedError::Random)
}

fn fill(bytes: &mut [u8]) -> Result<(), SealedError> {
    rand::rngs::SysRng
        .try_fill_bytes(bytes)
        .map_err(|_| SealedError::Random)
}

/// Writes frames to a new file. Dropped without [`Self::finish`], the
/// file reads back as cut short.
pub(crate) struct SealedWriter {
    file: BufWriter<File>,
    cipher: XChaCha20Poly1305,
    kind: Kind,
    id: String,
    index: u64,
}

impl SealedWriter {
    /// Creates `path` (refusing to replace a file), readable by this user
    /// only.
    pub(crate) fn create(
        path: &Path,
        kind: Kind,
        id: &str,
        public: &[u8; 32],
    ) -> Result<Self, SealedError> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options.open(path)?;
        let mut secret = Zeroizing::new([0u8; 32]);
        fill(secret.as_mut())?;
        let ephemeral = MontgomeryPoint::mul_base_clamped(*secret).to_bytes();
        let shared = Zeroizing::new(MontgomeryPoint(*public).mul_clamped(*secret).to_bytes());
        // A key of low order makes the secret known to anyone.
        if *shared == [0; 32] {
            return Err(SealedError::NotSealed);
        }
        let cipher = cipher(&shared, &ephemeral, public)?;
        let mut file = BufWriter::new(file);
        file.write_all(MAGIC)?;
        file.write_all(&ephemeral)?;
        Ok(Self {
            file,
            cipher,
            kind,
            id: id.to_owned(),
            index: 0,
        })
    }

    /// Seals `plain` as frames of at most [`FRAME_PLAIN`] bytes. O(n).
    pub(crate) fn write(&mut self, plain: &[u8]) -> Result<(), SealedError> {
        for chunk in plain.chunks(FRAME_PLAIN) {
            self.frame(chunk, false)?;
        }
        Ok(())
    }

    /// Makes what was written so far survive a crash.
    pub(crate) fn flush(&mut self) -> Result<(), SealedError> {
        self.file.flush()?;
        Ok(())
    }

    /// Seals the last frame, which may be empty, and syncs the file.
    pub(crate) fn finish(mut self, plain: &[u8]) -> Result<(), SealedError> {
        let (head, last) = plain.split_at(plain.len().saturating_sub(FRAME_PLAIN));
        self.write(head)?;
        self.frame(last, true)?;
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
        Ok(())
    }

    fn frame(&mut self, plain: &[u8], last: bool) -> Result<(), SealedError> {
        let mut nonce = [0u8; NONCE];
        fill(&mut nonce)?;
        let sealed = self
            .cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plain,
                    aad: &aad(self.kind, &self.id, self.index, last),
                },
            )
            .map_err(|_| SealedError::Tampered(self.index))?;
        let len = u32::try_from(NONCE + sealed.len()).map_err(|_| SealedError::NotSealed)?;
        self.file.write_all(&len.to_le_bytes())?;
        self.file.write_all(&nonce)?;
        self.file.write_all(&sealed)?;
        self.index += 1;
        Ok(())
    }
}

/// How a read ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    /// The last frame was found.
    Complete,
    /// The file stops early, as after a crash: what came before is whole.
    CutShort,
}

/// Opens `path` with `secret` and hands each frame's plain bytes to
/// `each`, in order, until it returns `false`. O(file).
pub(crate) fn read(
    path: &Path,
    kind: Kind,
    id: &str,
    secret: &[u8; 32],
    each: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<Ending, SealedError> {
    let mut file = BufReader::new(File::open(path)?);
    let mut magic = [0u8; 8];
    let mut ephemeral = [0u8; 32];
    if file.read_exact(&mut magic).is_err() || &magic != MAGIC {
        return Err(SealedError::NotSealed);
    }
    file.read_exact(&mut ephemeral)
        .map_err(|_| SealedError::NotSealed)?;
    let public = MontgomeryPoint::mul_base_clamped(*secret).to_bytes();
    let shared = Zeroizing::new(MontgomeryPoint(ephemeral).mul_clamped(*secret).to_bytes());
    // One made with a key of low order could be made by anyone.
    if *shared == [0; 32] {
        return Err(SealedError::NotSealed);
    }
    let cipher = cipher(&shared, &ephemeral, &public)?;
    let mut index = 0u64;
    let mut body = Vec::new();
    loop {
        let mut len = [0u8; 4];
        if !read_all(&mut file, &mut len)? {
            return Ok(Ending::CutShort);
        }
        let len = u32::from_le_bytes(len) as usize;
        if !(NONCE + TAG..=NONCE + TAG + FRAME_PLAIN).contains(&len) {
            return Err(SealedError::Tampered(index));
        }
        body.resize(len, 0);
        if !read_all(&mut file, &mut body)? {
            return Ok(Ending::CutShort);
        }
        let (nonce, sealed) = body.split_at(NONCE);
        let nonce =
            XNonce::from(<[u8; NONCE]>::try_from(nonce).map_err(|_| SealedError::NotSealed)?);
        // The last frame says so in its additional data: try the common
        // case first.
        let open = |last| {
            cipher.decrypt(
                &nonce,
                Payload {
                    msg: sealed,
                    aad: &aad(kind, id, index, last),
                },
            )
        };
        let (plain, last) = match open(false) {
            Ok(plain) => (Zeroizing::new(plain), false),
            Err(_) => (
                Zeroizing::new(open(true).map_err(|_| SealedError::Tampered(index))?),
                true,
            ),
        };
        if !each(&plain) {
            return Ok(Ending::Complete);
        }
        if last {
            // Anything after the last frame was put there.
            let mut extra = [0u8; 1];
            return if file.read(&mut extra)? == 0 {
                Ok(Ending::Complete)
            } else {
                Err(SealedError::Tampered(index + 1))
            };
        }
        index += 1;
    }
}

/// Fills `buf`, or returns `false` at a clean or torn end of file.
fn read_all(file: &mut impl Read, buf: &mut [u8]) -> Result<bool, SealedError> {
    let mut at = 0;
    while at < buf.len() {
        match file.read(&mut buf[at..])? {
            0 => return Ok(false),
            n => at += n,
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
