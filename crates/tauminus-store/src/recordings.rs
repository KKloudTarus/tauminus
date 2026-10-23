// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Session recordings on disk, in one folder:
//!
//! - `<id>.json`: what the list shows (title, host, time, length, size),
//!   in the clear so the list needs no unlock;
//! - `<id>.cast.sealed`: the asciicast text;
//! - `<id>.checkpoints.sealed`: the seek checkpoints the player made,
//!   replaced whole when it makes new ones;
//! - `<id>.bookmarks.sealed`: the user's bookmarks and notes.
//!
//! The sealed files are sealed to the vault's recordings key (see
//! `sealed_stream`), so only the vault opens them. Files are this user's
//! only; the folder is too.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauminus_app::recording::{RecordingSink, RecordingStore};
use tauminus_domain::{Bookmark, RecordingMeta};

use crate::sealed_stream::{Ending, Kind, SealedError, SealedWriter, read};

/// Most recordings listed; the list is newest first.
const MAX_LISTED: usize = 10_000;

#[derive(Debug, Clone)]
pub struct FileRecordingStore {
    dir: PathBuf,
}

impl FileRecordingStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The folder, this user's only, even if it was there before.
    fn ensure_dir(&self) -> Result<(), String> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&self.dir).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// The file `id` names, after checking `id` is one this store made, so
    /// an id from the view never reaches outside the folder.
    fn path(&self, id: &str, suffix: &str) -> Result<PathBuf, String> {
        if !valid_id(id) {
            return Err(format!("{id:?} is not a recording"));
        }
        Ok(self.dir.join(format!("{id}{suffix}")))
    }
}

/// 32 lowercase hex digits, as [`FileRecordingStore::new_id`] makes.
fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MetaRecord {
    version: u32,
    title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    started_ms: i64,
    duration_ms: u64,
    bytes: u64,
    truncated: bool,
    complete: bool,
}

impl MetaRecord {
    fn of(meta: &RecordingMeta) -> Self {
        Self {
            version: 1,
            title: meta.title.clone(),
            host: meta.host.clone(),
            started_ms: meta.started_ms,
            duration_ms: meta.duration_ms,
            bytes: meta.bytes,
            truncated: meta.truncated,
            complete: meta.complete,
        }
    }

    fn into_meta(self, id: String) -> RecordingMeta {
        RecordingMeta {
            id,
            title: self.title,
            host: self.host,
            started_ms: self.started_ms,
            duration_ms: self.duration_ms,
            bytes: self.bytes,
            truncated: self.truncated,
            complete: self.complete,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BookmarkRecord {
    at_ms: u64,
    label: String,
    comment: String,
}

/// Writes `bytes` to `path` whole or not at all: a temporary file, synced,
/// then renamed over it.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temp = temp_beside(path);
    let mut options = std::fs::OpenOptions::new();
    // A new file of a name no one could guess: never one planted there.
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&temp).map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut file, bytes).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(&temp, path).map_err(|e| e.to_string())
}

fn save_meta(path: &Path, meta: &RecordingMeta) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(&MetaRecord::of(meta)).map_err(|e| e.to_string())?;
    write_atomic(path, &bytes)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "used as map_err(sealed), which hands the error by value"
)]
fn sealed(error: SealedError) -> String {
    error.to_string()
}

/// Plain bytes gathered before a frame is sealed: a frame per batch of
/// output would cost 44 bytes and a seal for every keystroke's echo.
const GATHER: usize = 64 * 1024;

struct FileSink {
    writer: SealedWriter,
    meta_path: PathBuf,
    /// Lines not yet sealed; wiped as they go.
    pending: zeroize::Zeroizing<Vec<u8>>,
}

impl FileSink {
    fn seal_pending(&mut self) -> Result<(), String> {
        if !self.pending.is_empty() {
            self.writer.write(&self.pending).map_err(sealed)?;
            zeroize::Zeroize::zeroize(&mut *self.pending);
            self.pending.clear();
        }
        Ok(())
    }
}

impl RecordingSink for FileSink {
    fn write(&mut self, lines: &[u8]) -> Result<(), String> {
        self.pending.extend_from_slice(lines);
        if self.pending.len() >= GATHER {
            self.seal_pending()?;
        }
        Ok(())
    }

    /// Seals what is gathered and writes it out, so a crash loses at most
    /// the time since.
    fn flush(&mut self) -> Result<(), String> {
        self.seal_pending()?;
        self.writer.flush().map_err(sealed)
    }

    fn finish(mut self: Box<Self>, meta: &RecordingMeta) -> Result<(), String> {
        self.seal_pending()?;
        let Self {
            writer, meta_path, ..
        } = *self;
        writer.finish(b"").map_err(sealed)?;
        save_meta(&meta_path, meta)
    }
}

impl RecordingStore for FileRecordingStore {
    fn new_id(&self) -> String {
        let mut bytes = [0u8; 16];
        if rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut bytes).is_err() {
            // Unique enough for a file name; not a secret.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            bytes = now.to_le_bytes();
        }
        crate::vault::hex(&bytes)
    }

    fn create(
        &self,
        meta: &RecordingMeta,
        public: &[u8; 32],
    ) -> Result<Box<dyn RecordingSink>, String> {
        self.ensure_dir()?;
        let meta_path = self.path(&meta.id, ".json")?;
        let writer = SealedWriter::create(
            &self.path(&meta.id, ".cast.sealed")?,
            Kind::Recording,
            &meta.id,
            public,
        )
        .map_err(sealed)?;
        // Listed while it records, as not complete: a crash leaves it so.
        save_meta(&meta_path, meta)?;
        Ok(Box::new(FileSink {
            writer,
            meta_path,
            pending: zeroize::Zeroizing::new(Vec::with_capacity(GATHER)),
        }))
    }

    fn list(&self) -> Result<Vec<RecordingMeta>, String> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        let mut found = Vec::new();
        for entry in entries.flatten().take(MAX_LISTED * 4) {
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
                continue;
            };
            if !valid_id(id) {
                continue;
            }
            // A meta file that does not read is skipped, not fatal.
            let Ok(bytes) = std::fs::read(entry.path()) else {
                continue;
            };
            if let Ok(record) = serde_json::from_slice::<MetaRecord>(&bytes) {
                found.push(record.into_meta(id.to_owned()));
            }
        }
        found.sort_by(|a, b| b.started_ms.cmp(&a.started_ms).then(a.id.cmp(&b.id)));
        found.truncate(MAX_LISTED);
        Ok(found)
    }

    fn read(
        &self,
        id: &str,
        secret: &[u8; 32],
        each: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<bool, String> {
        let path = self.path(id, ".cast.sealed")?;
        let ending = read(&path, Kind::Recording, id, secret, each).map_err(sealed)?;
        Ok(ending == Ending::Complete)
    }

    fn delete(&self, id: &str) -> Result<bool, String> {
        let mut any = false;
        for suffix in [
            ".cast.sealed",
            ".checkpoints.sealed",
            ".bookmarks.sealed",
            ".json",
        ] {
            match std::fs::remove_file(self.path(id, suffix)?) {
                Ok(()) => any = true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(any)
    }

    fn save_checkpoints(
        &self,
        id: &str,
        public: &[u8; 32],
        checkpoints: &[u8],
    ) -> Result<(), String> {
        if !self.path(id, ".json")?.exists() {
            return Err(format!("no recording {id}"));
        }
        replace_sealed(
            &self.path(id, ".checkpoints.sealed")?,
            Kind::Checkpoints,
            id,
            public,
            checkpoints,
        )
    }

    fn checkpoints(&self, id: &str, secret: &[u8; 32]) -> Result<Option<Vec<u8>>, String> {
        let path = self.path(id, ".checkpoints.sealed")?;
        if !path.exists() {
            return Ok(None);
        }
        let mut whole = Vec::new();
        let mut too_large = false;
        let ending = read(&path, Kind::Checkpoints, id, secret, &mut |plain| {
            too_large = whole.len() + plain.len() > MAX_CHECKPOINT_BYTES;
            whole.extend_from_slice(plain);
            !too_large
        })
        .map_err(sealed)?;
        // Checkpoints cut short by a crash, or larger than the player ever
        // stores, are no checkpoints: the player builds them again.
        Ok((ending == Ending::Complete && !too_large).then_some(whole))
    }

    fn save_bookmarks(
        &self,
        id: &str,
        public: &[u8; 32],
        bookmarks: &[Bookmark],
    ) -> Result<(), String> {
        if !self.path(id, ".json")?.exists() {
            return Err(format!("no recording {id}"));
        }
        let records: Vec<BookmarkRecord> = bookmarks
            .iter()
            .map(|b| BookmarkRecord {
                at_ms: b.at_ms,
                label: b.label.clone(),
                comment: b.comment.clone(),
            })
            .collect();
        let plain =
            zeroize::Zeroizing::new(serde_json::to_vec(&records).map_err(|e| e.to_string())?);
        replace_sealed(
            &self.path(id, ".bookmarks.sealed")?,
            Kind::Notes,
            id,
            public,
            &plain,
        )
    }

    fn bookmarks(&self, id: &str, secret: &[u8; 32]) -> Result<Vec<Bookmark>, String> {
        let path = self.path(id, ".bookmarks.sealed")?;
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut plain = zeroize::Zeroizing::new(Vec::new());
        let mut too_large = false;
        let ending = read(&path, Kind::Notes, id, secret, &mut |bytes| {
            too_large = plain.len() + bytes.len() > MAX_BOOKMARK_BYTES;
            plain.extend_from_slice(bytes);
            !too_large
        })
        .map_err(sealed)?;
        if too_large {
            return Err("the bookmarks are larger than any saved here".into());
        }
        if ending != Ending::Complete {
            return Err("the bookmarks were cut short".into());
        }
        let records: Vec<BookmarkRecord> =
            serde_json::from_slice(&plain).map_err(|e| e.to_string())?;
        if records.len() > tauminus_domain::MAX_BOOKMARKS {
            return Err("more bookmarks than any saved here".into());
        }
        Ok(records
            .into_iter()
            .map(|r| Bookmark {
                at_ms: r.at_ms,
                label: r.label,
                comment: r.comment,
            })
            .collect())
    }
}

/// Seals `plain` to a new file and puts it in place of `path`, whole or
/// not at all. Two saves at once each write their own file; the later
/// rename wins.
fn replace_sealed(
    path: &Path,
    kind: Kind,
    id: &str,
    public: &[u8; 32],
    plain: &[u8],
) -> Result<(), String> {
    let temp = temp_beside(path);
    let written = SealedWriter::create(&temp, kind, id, public)
        .and_then(|writer| writer.finish(plain))
        .map_err(sealed)
        .and_then(|()| std::fs::rename(&temp, path).map_err(|e| e.to_string()));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// `path`'s name with a random suffix, in its folder.
fn temp_beside(path: &Path) -> PathBuf {
    let mut bytes = [0u8; 8];
    let _ = rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut bytes);
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}.tmp", crate::vault::hex(&bytes)));
    PathBuf::from(name)
}

/// Most checkpoint bytes read back, as the IPC lets the player store.
const MAX_CHECKPOINT_BYTES: usize = 64 * 1024 * 1024;
/// Most bookmark bytes read back: a thousand of 4 KiB, as JSON.
const MAX_BOOKMARK_BYTES: usize = 8 * 1024 * 1024;

#[cfg(test)]
mod tests;
