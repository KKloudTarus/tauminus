// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SFTP on a channel of a [`Connection`], with russh-sftp's raw session.
//!
//! Its higher-level `read_dir` gathers a whole directory with no bound,
//! re-collecting at every batch, so listings use the raw requests with a
//! cap. Transfers keep requests going, as many bytes as about five
//! seconds of their own measured rate (see [`Pace`]), and take replies in
//! offset order; a short read asks again for the rest,
//! and only `SSH_FX_EOF` ends a file. Every name and size the server sends
//! is data: the rules for what may become a local file are in the domain.

mod transfer;

use std::collections::VecDeque;
use std::sync::Arc;

use russh_sftp::client::RawSftpSession;
use russh_sftp::client::error::Error as RawError;
use russh_sftp::protocol::{FileAttributes, StatusCode};
use tauminus_app::ports::{
    BoxFuture, ConnectError, Download, EntryKind, RemoteEntry, SftpError, SftpPort,
    TransferProgress, Upload,
};
use tokio::sync::watch;

use crate::connection::Connection;
use crate::protocol;

/// Most entries one listing keeps.
pub(crate) const MAX_ENTRIES: usize = 100_000;
/// Most bytes of names one listing keeps, and the longest name kept.
const MAX_NAME_BYTES: usize = 32 * 1024 * 1024;
const MAX_NAME: usize = 4096;
/// Replies in a row with no entries before a listing gives up: a server
/// that never says it is done must not keep it going.
const MAX_EMPTY_BATCHES: usize = 16;
/// Bytes asked for by each read or carried by each write when the server
/// says nothing of its limits.
pub(crate) const CHUNK: u32 = 64 * 1024;
/// The largest request size taken from a server's `limits@openssh.com`.
const MAX_CHUNK: u32 = 256 * 1024;
/// The smallest: the SFTP draft has servers take 34 000-byte packets, so
/// a smaller limit is a server trying to multiply requests.
pub(crate) const MIN_CHUNK: u32 = 4096;
/// READDIR requests kept in flight: a listing of n entries takes about
/// n / batch / 16 round trips.
const LIST_AHEAD: usize = 64;
/// Seconds a request may wait for its reply.
const REQUEST_TIMEOUT: u64 = 30;

const POSIX_RENAME: &str = "posix-rename@openssh.com";

/// One SFTP session. It holds a lease on its connection.
pub(crate) struct Sftp {
    raw: Arc<RawSftpSession>,
    posix_rename: bool,
    /// The read and write sizes the server allows, at most [`CHUNK`].
    read_len: u32,
    write_len: u32,
    /// Transfers running on this session, for [`transfer::Pace`].
    active: Arc<std::sync::atomic::AtomicUsize>,
    _connection: Arc<Connection>,
}

impl std::fmt::Debug for Sftp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sftp").finish_non_exhaustive()
    }
}

pub(crate) async fn open(connection: Arc<Connection>) -> Result<Sftp, ConnectError> {
    let channel = connection
        .last()
        .channel_open_session()
        .await
        .map_err(protocol)?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(protocol)?;
    let config = russh_sftp::client::Config {
        request_timeout_secs: REQUEST_TIMEOUT,
        ..russh_sftp::client::Config::default()
    };
    let mut raw = RawSftpSession::new_with_config(channel.into_stream(), config);
    let version = raw.init().await.map_err(|e| sftp_protocol(&e))?;
    let has = |name: &str, value: &str| version.extensions.get(name).is_some_and(|v| v == value);
    let (mut read_len, mut write_len) = (CHUNK, CHUNK);
    if has(russh_sftp::extensions::LIMITS, "1") {
        let limits = raw.limits().await.map_err(|e| sftp_protocol(&e))?;
        let cap = |limit: u64| u32::try_from(limit).unwrap_or(MAX_CHUNK).min(MAX_CHUNK);
        if limits.max_read_len > 0 {
            read_len = cap(limits.max_read_len);
        }
        if limits.max_write_len > 0 {
            write_len = cap(limits.max_write_len);
        }
        raw.set_limits(limits.into());
    }
    Ok(Sftp {
        raw: Arc::new(raw),
        posix_rename: has(POSIX_RENAME, "1"),
        read_len: read_len.max(MIN_CHUNK),
        write_len: write_len.max(MIN_CHUNK),
        active: Arc::default(),
        _connection: connection,
    })
}

fn sftp_protocol(error: &RawError) -> ConnectError {
    let text: String = error.to_string().chars().take(512).collect();
    ConnectError::Protocol(format!("SFTP: {text}"))
}

/// The server's answer as an error the view can show. Its message is the
/// server's text; React shows it as text, and it is cut to 512 characters.
pub(crate) fn server_error(error: RawError) -> SftpError {
    match error {
        RawError::Status(status) => {
            let words: String = status.error_message.chars().take(512).collect();
            if words.trim().is_empty() {
                SftpError::Server(status.status_code.to_string())
            } else {
                SftpError::Server(words)
            }
        }
        // A request that waited past its time means the link is gone or
        // stalled: a transfer logs in again and resumes.
        RawError::IO(_) | RawError::UnexpectedBehavior(_) | RawError::Timeout => SftpError::Closed,
        other => SftpError::Server(other.to_string()),
    }
}

fn is_eof(error: &RawError) -> bool {
    matches!(error, RawError::Status(status) if status.status_code == StatusCode::Eof)
}

pub(crate) fn kind(attrs: &FileAttributes) -> EntryKind {
    if attrs.is_symlink() {
        EntryKind::Link
    } else if attrs.is_dir() {
        EntryKind::Dir
    } else if attrs.is_regular() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

fn entry(name: String, attrs: &FileAttributes) -> RemoteEntry {
    RemoteEntry {
        name,
        kind: kind(attrs),
        size: attrs.size.unwrap_or(0),
        modified: attrs.mtime.map(u64::from),
        mode: attrs.permissions.map(|p| p & 0o7777),
    }
}

/// The last component of a remote path.
fn base_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// An SSH string: its length, then its bytes.
fn ssh_string(out: &mut Vec<u8>, text: &str) {
    let len = u32::try_from(text.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(text.as_bytes());
}

impl Sftp {
    async fn list_dir(&self, dir: &str) -> Result<Vec<RemoteEntry>, SftpError> {
        let handle = self.raw.opendir(dir).await.map_err(server_error)?.handle;
        let result = self.read_entries(&handle).await;
        let _ = self.raw.close(handle).await;
        result
    }

    /// Keeps [`LIST_AHEAD`] READDIR requests going on `handle`, taking the
    /// replies in order; once one says the end, the rest are drained.
    async fn read_entries(&self, handle: &str) -> Result<Vec<RemoteEntry>, SftpError> {
        let mut entries = Vec::new();
        let mut bytes = 0;
        let mut empty = 0;
        let mut seen = 0;
        let mut ahead: InFlight<Result<russh_sftp::protocol::Name, RawError>> = InFlight::new();
        let ask = || {
            let raw = Arc::clone(&self.raw);
            let handle = handle.to_owned();
            tokio::spawn(async move { raw.readdir(handle).await })
        };
        let mut ended = false;
        let mut outcome = Ok(());
        loop {
            while !ended && ahead.len() < LIST_AHEAD {
                ahead.push_back(ask());
            }
            let Some(next) = ahead.pop_front() else {
                break;
            };
            let reply = next.await.map_err(|_| SftpError::Closed)?;
            if ended || outcome.is_err() {
                continue;
            }
            match reply {
                Ok(name) if name.files.is_empty() => {
                    empty += 1;
                    if empty == MAX_EMPTY_BATCHES {
                        outcome = Err(SftpError::Server("the listing never ended".into()));
                        ended = true;
                    }
                }
                Ok(name) => {
                    empty = 0;
                    // Every name counts, shown or skipped, so a server
                    // sending only skipped ones still reaches the end.
                    seen += name.files.len();
                    if seen > 2 * MAX_ENTRIES {
                        outcome = Err(SftpError::TooMany(MAX_ENTRIES));
                        ended = true;
                        continue;
                    }
                    for file in name.files {
                        if file.filename == "." || file.filename == ".." {
                            continue;
                        }
                        // A name no file system uses is not shown.
                        if file.filename.len() > MAX_NAME {
                            continue;
                        }
                        bytes += file.filename.len();
                        if entries.len() == MAX_ENTRIES || bytes > MAX_NAME_BYTES {
                            outcome = Err(SftpError::TooMany(MAX_ENTRIES));
                            ended = true;
                            break;
                        }
                        entries.push(entry(file.filename, &file.attrs));
                    }
                }
                Err(error) if is_eof(&error) => ended = true,
                Err(error) => {
                    outcome = Err(server_error(error));
                    ended = true;
                }
            }
        }
        outcome.map(|()| entries)
    }

    async fn stat_path(&self, path: &str) -> Result<RemoteEntry, SftpError> {
        let attrs = self.raw.stat(path).await.map_err(server_error)?.attrs;
        Ok(entry(base_name(path).to_owned(), &attrs))
    }

    async fn rename_over(&self, from: &str, to: &str) -> Result<(), SftpError> {
        if self.posix_rename {
            let mut data = Vec::with_capacity(from.len() + to.len() + 8);
            ssh_string(&mut data, from);
            ssh_string(&mut data, to);
            match self.raw.extended(POSIX_RENAME, data).await {
                Ok(russh_sftp::protocol::Packet::Status(status))
                    if status.status_code == StatusCode::Ok =>
                {
                    return Ok(());
                }
                Ok(russh_sftp::protocol::Packet::Status(status)) => {
                    return Err(server_error(RawError::Status(status)));
                }
                Ok(_) => return Err(SftpError::Closed),
                Err(error) => return Err(server_error(error)),
            }
        }
        // SFTP 3's rename refuses an existing target: remove it first.
        if self.raw.lstat(to).await.is_ok() {
            self.raw.remove(to).await.map_err(server_error)?;
        }
        self.raw.rename(from, to).await.map_err(server_error)?;
        Ok(())
    }
}

impl SftpPort for Sftp {
    fn list<'a>(&'a self, dir: &'a str) -> BoxFuture<'a, Result<Vec<RemoteEntry>, SftpError>> {
        Box::pin(self.list_dir(dir))
    }

    fn stat<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<RemoteEntry, SftpError>> {
        Box::pin(self.stat_path(path))
    }

    fn canonicalize<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String, SftpError>> {
        Box::pin(async move {
            let name = self.raw.realpath(path).await.map_err(server_error)?;
            name.files
                .into_iter()
                .next()
                .map(|file| file.filename)
                .ok_or_else(|| SftpError::Server("no path in the reply".into()))
        })
    }

    fn mkdir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            self.raw
                .mkdir(path, FileAttributes::default())
                .await
                .map(drop)
                .map_err(server_error)
        })
    }

    fn remove_file<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move { self.raw.remove(path).await.map(drop).map_err(server_error) })
    }

    fn remove_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move { self.raw.rmdir(path).await.map(drop).map_err(server_error) })
    }

    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
        // SFTP 3's rename refuses an existing target, so nothing is replaced.
        Box::pin(async move {
            self.raw
                .rename(from, to)
                .await
                .map(drop)
                .map_err(server_error)
        })
    }

    fn chmod<'a>(&'a self, path: &'a str, mode: u32) -> BoxFuture<'a, Result<(), SftpError>> {
        Box::pin(async move {
            let attrs = FileAttributes {
                permissions: Some(mode & 0o7777),
                ..FileAttributes::default()
            };
            self.raw
                .setstat(path, attrs)
                .await
                .map(drop)
                .map_err(server_error)
        })
    }

    fn download<'a>(
        &'a self,
        job: &'a Download,
        progress: watch::Sender<TransferProgress>,
        cancel: watch::Receiver<()>,
    ) -> BoxFuture<'a, Result<u64, SftpError>> {
        Box::pin(transfer::download(self, job, progress, cancel))
    }

    fn upload<'a>(
        &'a self,
        job: &'a Upload,
        progress: watch::Sender<TransferProgress>,
        cancel: watch::Receiver<()>,
    ) -> BoxFuture<'a, Result<u64, SftpError>> {
        Box::pin(transfer::upload(self, job, progress, cancel))
    }
}

/// Requests in flight, answered in the order they went out.
type InFlight<T> = VecDeque<tokio::task::JoinHandle<T>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_names_and_ssh_strings() {
        assert_eq!(base_name("/srv/app/"), "app");
        assert_eq!(base_name("notes.txt"), "notes.txt");
        let mut out = Vec::new();
        ssh_string(&mut out, "ab");
        assert_eq!(out, [0, 0, 0, 2, b'a', b'b']);
    }
}
