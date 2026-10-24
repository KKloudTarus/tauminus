// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Local files the user chose to send to `rz` (ADR 0026): opened once, and
//! checked on the open handle, not the path, so what is sent is what was
//! checked.

use std::fs::{File, OpenOptions};
use std::path::Path;

use tauminus_app::ports::OutgoingFile;
use tauminus_domain::MAX_SEND_NAME;

/// Files one choice may send, as many as a finished transfer lists.
pub(crate) const MAX_FILES: usize = 100;
/// The largest file Zmodem's 32-bit offsets reach.
const MAX_SIZE: u64 = u32::MAX as u64;
/// Bytes read at once: zmodem2 asks for 1 KiB at a time.
const READ_AHEAD: usize = 64 << 10;

/// A regular file, open for reading.
#[derive(Debug)]
pub(crate) struct DiskOutgoing {
    file: File,
    name: String,
    size: u64,
    /// What was last read ahead, and from where.
    ahead: Vec<u8>,
    ahead_at: u64,
}

/// Why a chosen file is not sent, by its name only: the view never gets
/// a local path.
fn refused(path: &Path, why: &str) -> String {
    let name = path
        .file_name()
        .map_or_else(|| "a file".into(), |name| name.to_string_lossy());
    format!("{name} is not sent: {why}")
}

impl DiskOutgoing {
    /// Opens `path` to send. A link, a folder, a device or a pipe is
    /// refused: the type is read from the open handle, and on Unix the
    /// handle must be the file the path named when it was looked at.
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| refused(path, "its name is not UTF-8"))?
            .to_owned();
        if name.len() > MAX_SEND_NAME {
            return Err(refused(path, "its name is longer than 255 bytes"));
        }
        let before = std::fs::symlink_metadata(path).map_err(|e| refused(path, &e.to_string()))?;
        if !before.file_type().is_file() {
            return Err(refused(path, "it is not a regular file"));
        }
        let mut options = OpenOptions::new();
        options.read(true);
        // A link swapped in is not followed, a pipe swapped in does not
        // block the open, and a terminal device never becomes ours.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::custom_flags(
            &mut options,
            nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_NOCTTY,
        );
        #[cfg(windows)]
        {
            // FILE_FLAG_OPEN_REPARSE_POINT: a link is opened as itself.
            std::os::windows::fs::OpenOptionsExt::custom_flags(&mut options, 0x0020_0000);
        }
        let file = options
            .open(path)
            .map_err(|e| refused(path, &e.to_string()))?;
        let meta = file.metadata().map_err(|e| refused(path, &e.to_string()))?;
        if !meta.file_type().is_file() {
            return Err(refused(path, "it is not a regular file"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // Swapped for a link between the look and the open.
            if (meta.dev(), meta.ino()) != (before.dev(), before.ino()) {
                return Err(refused(path, "it changed while it was opened"));
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            const REPARSE_POINT: u32 = 0x400;
            if meta.file_attributes() & REPARSE_POINT != 0 {
                return Err(refused(path, "it is a link"));
            }
        }
        let size = meta.len();
        if size > MAX_SIZE {
            return Err(refused(path, "Zmodem sends files up to 4 GiB"));
        }
        Ok(Self {
            file,
            name,
            size,
            ahead: Vec::new(),
            ahead_at: 0,
        })
    }
}

#[cfg(unix)]
fn read_raw(file: &File, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

#[cfg(windows)]
fn read_raw(file: &File, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

impl OutgoingFile for DiskOutgoing {
    fn name(&self) -> &str {
        &self.name
    }

    fn size(&self) -> u64 {
        self.size
    }

    /// From the read-ahead when it holds `offset`; else 64 KiB more are
    /// read from there. O(len) per call, one system call per 64 KiB.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let end = self.ahead_at + self.ahead.len() as u64;
        if offset < self.ahead_at || offset >= end {
            self.ahead.resize(READ_AHEAD, 0);
            let mut filled = 0;
            while filled < READ_AHEAD {
                let n = read_raw(
                    &self.file,
                    offset + filled as u64,
                    &mut self.ahead[filled..],
                )?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            self.ahead.truncate(filled);
            self.ahead_at = offset;
        }
        let from = usize::try_from(offset - self.ahead_at).unwrap_or(usize::MAX);
        let available = self.ahead.get(from..).unwrap_or_default();
        let n = buf.len().min(available.len());
        buf[..n].copy_from_slice(&available[..n]);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tauminus-outgoing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_file_reads_whole_in_any_steps() {
        let dir = scratch();
        let data: Vec<u8> = (0..200_000u32)
            .map(|i| (i % 251).to_le_bytes()[0])
            .collect();
        std::fs::write(dir.join("a.bin"), &data).unwrap();
        let mut file = DiskOutgoing::open(&dir.join("a.bin")).unwrap();
        assert_eq!((file.name(), file.size()), ("a.bin", 200_000));
        let mut got = Vec::new();
        let mut buf = [0; 1000];
        loop {
            let n = file.read_at(got.len() as u64, &mut buf).unwrap();
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }
        assert_eq!(got, data);
        // Asked again from an earlier offset, as `rz` may.
        let n = file.read_at(65_000, &mut buf).unwrap();
        assert_eq!(&buf[..n], &data[65_000..65_000 + n]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn folders_and_missing_files_are_refused() {
        let dir = scratch();
        assert!(DiskOutgoing::open(&dir).is_err());
        assert!(DiskOutgoing::open(&dir.join("none")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn links_and_pipes_are_refused() {
        let dir = scratch();
        std::fs::write(dir.join("real"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        assert!(DiskOutgoing::open(&dir.join("link")).is_err());
        let fifo = dir.join("fifo");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        // Refused from its type, before an open that would block.
        assert!(DiskOutgoing::open(&fifo).is_err());
        assert!(DiskOutgoing::open(Path::new("/dev/zero")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
