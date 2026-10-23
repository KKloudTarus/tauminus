// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Downloads and uploads, pipelined, resumable.
//!
//! A download writes `<file>.tauminus-part`, created new and private, with
//! a record beside it of the host, remote path, size, and modification
//! time it copies. An upload writes `<file>.tauminus-part` on the server
//! and keeps its record in a local state directory. A resume needs the
//! record to match the source as it is now and the last 64 KiB before the
//! resume point to match on both sides (`tauminus_domain::plan`);
//! anything else starts over, so two versions are never spliced together.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use russh_sftp::client::error::Error as RawError;
use russh_sftp::protocol::{FileAttributes, OpenFlags};
use tauminus_app::ports::{Download, EntryKind, SftpError, TransferProgress, Upload};
use tauminus_domain::{Source, Start, plan, tail_start};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::watch;

use super::{InFlight, Sftp, is_eof, server_error};

const PART: &str = ".tauminus-part";
const RECORD_VERSION: &str = "tauminus-part 1";

pub(super) async fn download(
    sftp: &Sftp,
    job: &Download,
    progress: watch::Sender<TransferProgress>,
    mut cancel: watch::Receiver<()>,
) -> Result<u64, SftpError> {
    // One stat: the size it reports is the most this copies.
    let remote = sftp.stat_path(&job.remote).await?;
    if remote.kind != EntryKind::File {
        return Err(SftpError::Server(format!("{} is not a file", job.remote)));
    }
    if job.max.is_some_and(|max| remote.size > max) {
        return Err(SftpError::TooLarge(job.remote.clone()));
    }
    if !job.overwrite && tokio::fs::symlink_metadata(&job.local).await.is_ok() {
        return Err(SftpError::Exists(job.local.display().to_string()));
    }
    let now = Source {
        host: job.host.clone(),
        path: job.remote.clone(),
        size: remote.size,
        modified: remote.modified.unwrap_or(0),
    };
    let part = with_suffix(&job.local, PART);
    let record = with_suffix(&part, ".info");
    let partial = private_file_len(&part).await;
    // Without a time from the server, a changed file cannot be told apart.
    let recorded = if job.resume && remote.modified.is_some() {
        read_record(&record).await
    } else {
        None
    };
    let mut start = plan(recorded.as_ref(), &now, partial);
    if let Start::Resume { offset } = start {
        let from = tail_start(offset);
        let here = read_local(&part, from, offset).await;
        let there = read_remote(sftp, &job.remote, from, offset).await;
        if here.is_none() || here != there {
            start = Start::Fresh;
        }
    }
    let (mut file, offset) = match start {
        Start::Resume { offset } => {
            let mut file = open_private(&part).await?;
            file.set_len(offset).await?;
            file.seek(std::io::SeekFrom::Start(offset)).await?;
            (file, offset)
        }
        Start::Fresh => {
            remove_if_file(&part).await?;
            remove_if_file(&record).await?;
            let file = create_private(&part).await?;
            write_record(&record, &now).await?;
            (file, 0)
        }
    };
    progress.send_replace(TransferProgress {
        done: offset,
        total: now.size,
        resumed_from: offset,
    });
    let handle = sftp
        .raw
        .open(
            job.remote.as_str(),
            OpenFlags::READ,
            FileAttributes::default(),
        )
        .await
        .map_err(server_error)?
        .handle;
    let copied = tokio::select! {
        copied = read_into(sftp, &handle, offset, now.size, &mut file, &progress) => copied,
        _ = cancel.changed() => Err(SftpError::Cancelled),
    };
    let _ = sftp.raw.close(handle.as_str()).await;
    let copied = copied?;
    // The copy is the file only if the file did not change while it ran.
    let after = sftp.stat_path(&job.remote).await?;
    if after.size != remote.size || after.modified != remote.modified {
        return Err(SftpError::Changed);
    }
    file.flush().await?;
    file.sync_all().await?;
    // The partial copy at that name must still be the file written: if
    // another transfer replaced it meanwhile, moving it would put the
    // wrong bytes in place.
    if !same_file(&file, &part).await {
        return Err(SftpError::Changed);
    }
    drop(file);
    commit(&part, &job.local, job.overwrite).await?;
    let _ = tokio::fs::remove_file(&record).await;
    Ok(copied)
}

#[cfg(unix)]
async fn same_file(open: &tokio::fs::File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (
        open.metadata().await,
        tokio::fs::symlink_metadata(path).await,
    ) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Windows refuses to delete a file another handle has open, so the name
/// cannot have been taken over.
#[cfg(not(unix))]
async fn same_file(_open: &tokio::fs::File, path: &Path) -> bool {
    tokio::fs::symlink_metadata(path).await.is_ok()
}

/// Puts a finished partial copy in place: replacing what is there only
/// when asked to.
async fn commit(part: &Path, local: &Path, overwrite: bool) -> Result<(), SftpError> {
    if overwrite {
        // Replaces atomically, on Windows too.
        tokio::fs::rename(part, local).await?;
    } else {
        // A link fails if a file appeared there meanwhile; nothing is
        // replaced.
        match tokio::fs::hard_link(part, local).await {
            Ok(()) => tokio::fs::remove_file(part).await?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(SftpError::Exists(local.display().to_string()));
            }
            // FAT, exFAT, and some network shares have no hard links: a
            // last look, then a rename.
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Unsupported | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                if tokio::fs::symlink_metadata(local).await.is_ok() {
                    return Err(SftpError::Exists(local.display().to_string()));
                }
                tokio::fs::rename(part, local).await?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// A read request's offset, length, when it was sent, and its reply.
type Read = (u64, u32, Instant, Result<Vec<u8>, RawError>);

/// How many bytes a transfer keeps in flight: two round trips of the rate
/// measured over the last second, as BBR sizes its window. With the round
/// trip at its floor, each round doubles what is sent until the link's
/// queue starts to fill; then the rate stops growing and so does the
/// window. Data past that only waits in queues, where a folder listing or
/// a shell on the same connection would wait behind it, and a request's
/// 30 s timer starts when it is queued. Keeps 128 KiB for its first
/// second, then stays within 32 KiB
/// and 8 MiB; requests shrink to a quarter of the window when it holds
/// fewer than four, so a slow link still has a few in flight.
pub(super) struct Pace {
    min_rtt: Option<Duration>,
    /// (when, bytes moved so far), over the last second.
    samples: VecDeque<(Instant, u64)>,
    total: u64,
    /// When the first reply came; the rate means little before a second.
    first: Option<Instant>,
    /// Transfers warming up on the same session share one start, so four
    /// begun together on a slow link do not queue four starts at once.
    active: Arc<AtomicUsize>,
}

impl Drop for Pace {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Pace {
    const START: u64 = 128 * 1024;
    const FLOOR: u64 = 32 * 1024;
    const MAX: u64 = 8 * 1024 * 1024;
    const MAX_REQUESTS: usize = 256;
    /// Bounds the samples whatever the rate.
    const MAX_SAMPLES: usize = 4096;

    /// Counts itself in `active` until it is dropped.
    pub(super) fn new(active: &Arc<AtomicUsize>) -> Self {
        active.fetch_add(1, Ordering::Relaxed);
        Self {
            min_rtt: None,
            samples: VecDeque::new(),
            total: 0,
            first: None,
            active: Arc::clone(active),
        }
    }

    /// This transfer's share of the start.
    fn start(&self) -> u64 {
        let share = u64::try_from(self.active.load(Ordering::Relaxed).max(1)).unwrap_or(1);
        (Self::START / share).max(Self::FLOOR)
    }

    /// `bytes` came back `rtt` after they were asked for.
    pub(super) fn moved(&mut self, bytes: u64, rtt: Duration) {
        self.moved_at(bytes, rtt, Instant::now());
    }

    fn moved_at(&mut self, bytes: u64, rtt: Duration, now: Instant) {
        self.total += bytes;
        self.first.get_or_insert(now);
        self.min_rtt = Some(self.min_rtt.map_or(rtt, |m| m.min(rtt)));
        self.samples.push_back((now, self.total));
        while self.samples.len() > 2
            && (self.samples.len() > Self::MAX_SAMPLES
                || now.duration_since(self.samples[0].0) > Duration::from_secs(1))
        {
            self.samples.pop_front();
        }
    }

    /// Bytes that may be in flight now.
    fn budget(&self) -> u64 {
        let (Some(rtt), Some(first), Some(last)) =
            (self.min_rtt, self.samples.front(), self.samples.back())
        else {
            return self.start();
        };
        let span = last
            .0
            .duration_since(first.0)
            .as_secs_f64()
            .max(rtt.as_secs_f64())
            .max(1e-3);
        #[allow(clippy::cast_precision_loss, reason = "a rate estimate")]
        let rate = (last.1 - first.1) as f64 / span;
        // Two round trips of the recent rate, and never under 20 ms of it.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "positive, and clamped below"
        )]
        let bdp = (rate * (2.0 * rtt.as_secs_f64()).max(0.02)) as u64;
        let warming = self
            .first
            .is_none_or(|first| last.0.duration_since(first) < Duration::from_secs(1));
        let floor = if warming { self.start() } else { Self::FLOOR };
        bdp.clamp(floor, Self::MAX)
    }

    /// The size of the next request, at most `chunk`.
    pub(super) fn len(&self, chunk: u32) -> u32 {
        let budget = self.budget();
        if budget >= 4 * u64::from(chunk) {
            return chunk;
        }
        u32::try_from((budget / 4).max(Self::FLOOR))
            .unwrap_or(chunk)
            .min(chunk)
    }

    /// Requests of [`Self::len`] bytes that may be in flight now: at least
    /// one, at most [`Self::MAX_REQUESTS`].
    pub(super) fn requests(&self, chunk: u32) -> usize {
        let len = u64::from(self.len(chunk).max(1));
        usize::try_from(self.budget() / len).map_or(1, |n| n.clamp(1, Self::MAX_REQUESTS))
    }
}

/// Copies `[offset, size)` of the open remote file into `file`, keeping
/// reads going as [`Pace`] allows and writing their replies in order.
async fn read_into(
    sftp: &Sftp,
    handle: &str,
    offset: u64,
    size: u64,
    file: &mut tokio::fs::File,
    progress: &watch::Sender<TransferProgress>,
) -> Result<u64, SftpError> {
    let mut inflight: InFlight<Read> = InFlight::new();
    let mut next = offset;
    let mut copied = 0;
    let mut pace = Pace::new(&sftp.active);
    // Lowered to what a server returns when it gives less than asked, so
    // later requests are not short too.
    let mut chunk = sftp.read_len;
    let read = |at: u64, len: u32| {
        let raw = Arc::clone(&sftp.raw);
        let handle = handle.to_owned();
        let sent = Instant::now();
        tokio::spawn(async move {
            let r = raw.read(handle, at, len).await.map(|d| d.data);
            (at, len, sent, r)
        })
    };
    loop {
        while inflight.len() < pace.requests(chunk) && next < size {
            let len = u32::try_from((size - next).min(u64::from(pace.len(chunk)))).unwrap_or(1);
            inflight.push_back(read(next, len));
            next += u64::from(len);
        }
        let Some(front) = inflight.pop_front() else {
            return Ok(copied);
        };
        let (at, len, sent, result) = front.await.map_err(|_| SftpError::Closed)?;
        let rtt = sent.elapsed();
        let data = match result {
            Ok(data) => data,
            // The file is shorter than it was when the copy began.
            Err(error) if is_eof(&error) => return Err(SftpError::Changed),
            Err(error) => return Err(server_error(error)),
        };
        if data.len() > len as usize {
            return Err(SftpError::Server(
                "the server sent more than was asked".into(),
            ));
        }
        let mut got = data.len() as u64;
        if !data.is_empty() && (data.len() as u64) < u64::from(len) && next < size {
            // Never below the floor, so a server answering one byte at a
            // time cannot make a request of each byte.
            chunk = u32::try_from(data.len())
                .unwrap_or(chunk)
                .clamp(super::MIN_CHUNK, chunk.max(super::MIN_CHUNK));
        }
        file.write_all(&data).await?;
        // A short read is not the end: ask for the rest of this range
        // before the replies after it are written.
        while got < u64::from(len) {
            let rest = u32::try_from(u64::from(len) - got).unwrap_or(len);
            match sftp.raw.read(handle, at + got, rest).await {
                Ok(more) if !more.data.is_empty() && more.data.len() <= rest as usize => {
                    file.write_all(&more.data).await?;
                    got += more.data.len() as u64;
                }
                Ok(more) if more.data.is_empty() => return Err(SftpError::Changed),
                Ok(_) => {
                    return Err(SftpError::Server(
                        "the server sent more than was asked".into(),
                    ));
                }
                Err(error) if is_eof(&error) => return Err(SftpError::Changed),
                Err(error) => return Err(server_error(error)),
            }
        }
        copied += got;
        pace.moved(got, rtt);
        progress.send_modify(|p| p.done += got);
    }
}

pub(super) async fn upload(
    sftp: &Sftp,
    job: &Upload,
    progress: watch::Sender<TransferProgress>,
    mut cancel: watch::Receiver<()>,
) -> Result<u64, SftpError> {
    let local = tokio::fs::metadata(&job.local).await?;
    if !local.is_file() {
        return Err(SftpError::Local(std::io::Error::other(format!(
            "{} is not a file",
            job.local.display()
        ))));
    }
    // `lstat`: a link at the target is replaced by the file, never written
    // through.
    let existing = sftp
        .raw
        .lstat(job.remote.as_str())
        .await
        .ok()
        .map(|a| a.attrs);
    if !job.overwrite && existing.is_some() {
        return Err(SftpError::Exists(job.remote.clone()));
    }
    let now = Source {
        host: job.host.clone(),
        path: format!("{}\n{}", job.remote, job.local.display()),
        size: local.len(),
        modified: modified_nanos(&local),
    };
    tokio::fs::create_dir_all(&job.state).await?;
    let record = job.state.join(record_name(&job.host, &job.remote));
    let part_record = with_suffix(&record, ".part");
    let recorded = if job.resume {
        read_record(&record)
            .await
            .zip(read_part_record(&part_record).await)
    } else {
        None
    };
    let (part, handle, offset, owner) =
        prepare_part(sftp, job, recorded, &now, &record, &part_record).await?;
    progress.send_replace(TransferProgress {
        done: offset,
        total: now.size,
        resumed_from: offset,
    });
    let mut file = tokio::fs::File::open(&job.local).await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let acked = AtomicU64::new(offset);
    let saved = PartRecord {
        path: &part_record,
        part: &part,
        acked: &acked,
        uid: owner,
    };
    let copied = tokio::select! {
        copied = write_from(sftp, &handle, offset, now.size, &mut file, &progress, &saved) => copied,
        _ = cancel.changed() => Err(SftpError::Cancelled),
    };
    let copied = match copied {
        Ok(copied) => copied,
        Err(error) => {
            let _ = sftp.raw.close(handle.as_str()).await;
            if job.resume {
                // A resume starts where every byte before is known written.
                let _ = saved.save().await;
            } else {
                // Nothing will come back for it: leave no partial file.
                let _ = sftp.raw.remove(part.as_str()).await;
            }
            return Err(error);
        }
    };
    // What was sent is what the file holds now, or it changed meanwhile.
    let after = tokio::fs::metadata(&job.local).await?;
    if after.len() != now.size || modified_nanos(&after) != now.modified {
        let _ = sftp.raw.close(handle.as_str()).await;
        return Err(SftpError::Changed);
    }
    finish_upload(sftp, &handle, existing, local_mode(&local)).await?;
    if job.overwrite {
        sftp.rename_over(&part, &job.remote).await?;
    } else if sftp
        .raw
        .rename(part.as_str(), job.remote.as_str())
        .await
        .is_err()
    {
        // A file appeared there meanwhile: SFTP 3's rename does not replace
        // it, and neither does this.
        let _ = sftp.raw.remove(part.as_str()).await;
        return Err(SftpError::Exists(job.remote.clone()));
    }
    let _ = tokio::fs::remove_file(&record).await;
    let _ = tokio::fs::remove_file(&part_record).await;
    Ok(copied)
}

/// Continues the partial upload its record names, when it may, or starts
/// a new one. Returns its name, an open handle, and where writing starts.
async fn prepare_part(
    sftp: &Sftp,
    job: &Upload,
    recorded: Option<(Source, Recorded)>,
    now: &Source,
    record: &Path,
    part_record: &Path,
) -> Result<(String, String, u64, Option<u32>), SftpError> {
    if let Some((source, (part, acked, owner))) = &recorded
        && let Some(size) = resumable(sftp, part).await
        // Writes may land out of order; only the prefix acknowledged in
        // order is known whole.
        && let Start::Resume { offset } = plan(Some(source), now, size.min(*acked))
    {
        let from = tail_start(offset);
        let here = read_local(&job.local, from, offset).await;
        let there = read_remote(sftp, part, from, offset).await;
        if here.is_some()
            && here == there
            && let Some(handle) = open_resumable(sftp, part, offset, *owner).await
        {
            return Ok((part.clone(), handle, offset, *owner));
        }
    }
    if let Some((_, (old, _, _))) = &recorded {
        let _ = sftp.raw.remove(old.as_str()).await;
    }
    fresh_part(sftp, job, record, part_record, now).await
}

/// Sets the finished file's permissions on its handle, so nothing at the
/// path is followed, and closes it: the target's when it replaces one, the
/// local file's for a new one, never with setuid, setgid, or sticky bits
/// and never wider than what was there.
async fn finish_upload(
    sftp: &Sftp,
    handle: &str,
    existing: Option<FileAttributes>,
    local_mode: u32,
) -> Result<(), SftpError> {
    let mode = existing
        .filter(|e| !e.is_symlink())
        .and_then(|e| e.permissions)
        .unwrap_or(local_mode)
        & 0o777;
    let attrs = FileAttributes {
        permissions: Some(mode),
        ..FileAttributes::default()
    };
    // Best effort: some servers refuse it (AWS Transfer by default), and
    // the file is the point; the handle closes either way.
    let _ = sftp.raw.fsetstat(handle, attrs).await;
    sftp.raw.close(handle).await.map_err(server_error)?;
    Ok(())
}

#[cfg(unix)]
fn local_mode(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o777
}

/// Windows has no mode; a new file gets what a server usually gives.
#[cfg(not(unix))]
fn local_mode(_metadata: &std::fs::Metadata) -> u32 {
    0o644
}

/// The size of a partial upload that is a regular file of its own, not a
/// link someone put in its place, or `None`.
async fn resumable(sftp: &Sftp, part: &str) -> Option<u64> {
    let attrs = sftp.raw.lstat(part).await.ok()?.attrs;
    (attrs.is_regular() && !attrs.is_symlink()).then(|| attrs.size.unwrap_or(0))
}

/// Opens a partial upload to continue it, checking through the handle that
/// it is still the regular file just checked by name: if a link took its
/// place in between, the sizes or times differ and it starts over.
async fn open_resumable(
    sftp: &Sftp,
    part: &str,
    offset: u64,
    owner: Option<u32>,
) -> Option<String> {
    let named = sftp.raw.lstat(part).await.ok()?.attrs;
    if !named.is_regular() || named.is_symlink() || named.size.is_none_or(|size| size < offset) {
        return None;
    }
    let handle = sftp
        .raw
        .open(part, OpenFlags::WRITE, FileAttributes::default())
        .await
        .ok()?
        .handle;
    let opened = sftp.raw.fstat(handle.as_str()).await.ok()?.attrs;
    if opened.size != named.size
        || opened.mtime != named.mtime
        || opened.uid != named.uid
        // A server that names owners must name the one that made it.
        || (owner.is_some() && opened.uid != owner)
    {
        let _ = sftp.raw.close(handle.as_str()).await;
        return None;
    }
    Some(handle)
}

/// A new partial upload under an unguessable name, created exclusively so
/// a link planted there is refused, and its records.
async fn fresh_part(
    sftp: &Sftp,
    job: &Upload,
    record: &Path,
    part_record: &Path,
    now: &Source,
) -> Result<(String, String, u64, Option<u32>), SftpError> {
    use rand::RngExt;
    let tag: u64 = rand::rng().random();
    let part = format!("{}{PART}-{tag:016x}", job.remote);
    remove_if_file(record).await?;
    remove_if_file(part_record).await?;
    let private = FileAttributes {
        permissions: Some(0o600),
        ..FileAttributes::default()
    };
    let handle = sftp
        .raw
        .open(
            part.as_str(),
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
            private,
        )
        .await
        .map_err(server_error)?
        .handle;
    let owner = sftp
        .raw
        .fstat(handle.as_str())
        .await
        .ok()
        .and_then(|a| a.attrs.uid);
    write_record(record, now).await?;
    let acked = AtomicU64::new(0);
    PartRecord {
        path: part_record,
        part: &part,
        acked: &acked,
        uid: owner,
    }
    .save()
    .await?;
    Ok((part, handle, 0, owner))
}

/// Where a partial upload is and how much of it is known whole, kept so a
/// resume finds it.
struct PartRecord<'a> {
    path: &'a Path,
    part: &'a str,
    acked: &'a AtomicU64,
    /// The owner the server gave the part when it was created; a resume
    /// refuses a file of the same name owned by anyone else.
    uid: Option<u32>,
}

impl PartRecord<'_> {
    /// Written beside the old record and renamed over it, so a crash
    /// leaves one or the other, never neither.
    async fn save(&self) -> std::io::Result<()> {
        let text = format!(
            "{}\n{}\n{}\n",
            data_encoding::HEXLOWER.encode(self.part.as_bytes()),
            self.acked.load(Ordering::Relaxed),
            self.uid.map_or_else(String::new, |uid| uid.to_string())
        );
        let next = with_suffix(self.path, ".next");
        remove_if_file(&next).await?;
        let mut file = create_private(&next).await?;
        file.write_all(text.as_bytes()).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&next, self.path).await
    }
}

/// The partial upload a record names: its name, the prefix known whole,
/// and its owner when created.
type Recorded = (String, u64, Option<u32>);

async fn read_part_record(path: &Path) -> Option<Recorded> {
    if private_file_len(path).await > MAX_RECORD {
        return None;
    }
    let mut file = open_private(path).await.ok()?;
    let mut text = String::new();
    file.read_to_string(&mut text).await.ok()?;
    let mut lines = text.lines();
    let bytes = data_encoding::HEXLOWER
        .decode(lines.next()?.as_bytes())
        .ok()?;
    let acked = lines.next()?.parse().ok()?;
    let uid = lines.next().and_then(|line| line.parse().ok());
    Some((String::from_utf8(bytes).ok()?, acked, uid))
}

/// Sends `[offset, size)` of `file`, keeping writes going as [`Pace`]
/// allows.
async fn write_from(
    sftp: &Sftp,
    handle: &str,
    offset: u64,
    size: u64,
    file: &mut tokio::fs::File,
    progress: &watch::Sender<TransferProgress>,
    saved: &PartRecord<'_>,
) -> Result<u64, SftpError> {
    /// How often the acknowledged prefix is saved, so a crash loses little.
    const SAVE_EVERY: u64 = 32 * 1024 * 1024;
    let mut inflight: InFlight<(u64, Duration, Result<(), RawError>)> = InFlight::new();
    let mut at = offset;
    // Replies are taken oldest first, so each one extends the prefix.
    let settle =
        |done: (u64, Duration, Result<(), RawError>)| -> Result<(u64, Duration), SftpError> {
            let (len, rtt, result) = done;
            result.map_err(server_error)?;
            progress.send_modify(|p| p.done += len);
            saved.acked.fetch_add(len, Ordering::Relaxed);
            Ok((len, rtt))
        };
    let mut last_saved = offset;
    let mut pace = Pace::new(&sftp.active);
    let mut copied = 0;
    while at < size {
        let len =
            usize::try_from((size - at).min(u64::from(pace.len(sftp.write_len)))).unwrap_or(1);
        let mut chunk = vec![0_u8; len];
        let read = read_full(file, &mut chunk).await?;
        if read < len {
            // Shorter than when the upload began.
            return Err(SftpError::Changed);
        }
        let raw = Arc::clone(&sftp.raw);
        let handle = handle.to_owned();
        let written_at = at;
        let sent = Instant::now();
        inflight.push_back(tokio::spawn(async move {
            let result = raw.write(handle, written_at, chunk).await.map(drop);
            (len as u64, sent.elapsed(), result)
        }));
        at += len as u64;
        while inflight.len() >= pace.requests(sftp.write_len)
            && let Some(oldest) = inflight.pop_front()
        {
            let (len, rtt) = settle(oldest.await.map_err(|_| SftpError::Closed)?)?;
            copied += len;
            pace.moved(len, rtt);
        }
        let acked = saved.acked.load(Ordering::Relaxed);
        if acked - last_saved >= SAVE_EVERY {
            saved.save().await?;
            last_saved = acked;
        }
    }
    while let Some(oldest) = inflight.pop_front() {
        copied += settle(oldest.await.map_err(|_| SftpError::Closed)?)?.0;
    }
    Ok(copied)
}

/// Reads until `buffer` is full or the file ends. Returns the bytes read.
async fn read_full(file: &mut tokio::fs::File, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = file.read(&mut buffer[filled..]).await?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

/// `[from, to)` of a local file, or `None` when it cannot be read whole.
async fn read_local(path: &Path, from: u64, to: u64) -> Option<Vec<u8>> {
    let mut file = tokio::fs::File::open(path).await.ok()?;
    file.seek(std::io::SeekFrom::Start(from)).await.ok()?;
    let mut buffer = vec![0_u8; usize::try_from(to - from).ok()?];
    file.read_exact(&mut buffer).await.ok()?;
    Some(buffer)
}

/// `[from, to)` of a remote file, short reads included, or `None`.
async fn read_remote(sftp: &Sftp, path: &str, from: u64, to: u64) -> Option<Vec<u8>> {
    let handle = sftp
        .raw
        .open(path, OpenFlags::READ, FileAttributes::default())
        .await
        .ok()?
        .handle;
    let mut out = Vec::new();
    let mut at = from;
    while at < to {
        let len = u32::try_from((to - at).min(u64::from(sftp.read_len))).ok()?;
        match sftp.raw.read(handle.as_str(), at, len).await {
            Ok(data) if !data.data.is_empty() && data.data.len() <= len as usize => {
                at += data.data.len() as u64;
                out.extend_from_slice(&data.data);
            }
            _ => break,
        }
    }
    let _ = sftp.raw.close(handle.as_str()).await;
    (at == to).then_some(out)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The local file's modification time in nanoseconds, so two saves in one
/// second still differ.
fn modified_nanos(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// The upload record's file name: a hash, so a remote path cannot name
/// anything in the state directory.
fn record_name(host: &str, remote: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::new()
        .chain_update(host.as_bytes())
        .chain_update([0])
        .chain_update(remote.as_bytes())
        .finalize();
    format!("{}.upload", data_encoding::HEXLOWER.encode(&digest))
}

fn record_text(source: &Source) -> String {
    let hex = |text: &str| data_encoding::HEXLOWER.encode(text.as_bytes());
    format!(
        "{RECORD_VERSION}\n{}\n{}\n{}\n{}\n",
        hex(&source.host),
        hex(&source.path),
        source.size,
        source.modified
    )
}

fn parse_record(text: &str) -> Option<Source> {
    let unhex = |line: &str| {
        let bytes = data_encoding::HEXLOWER.decode(line.as_bytes()).ok()?;
        String::from_utf8(bytes).ok()
    };
    let mut lines = text.lines();
    if lines.next()? != RECORD_VERSION {
        return None;
    }
    Some(Source {
        host: unhex(lines.next()?)?,
        path: unhex(lines.next()?)?,
        size: lines.next()?.parse().ok()?,
        modified: lines.next()?.parse().ok()?,
    })
}

/// Most bytes a record may have; anything larger is not ours.
const MAX_RECORD: u64 = 64 * 1024;

async fn read_record(path: &Path) -> Option<Source> {
    if private_file_len(path).await > MAX_RECORD {
        return None;
    }
    let mut file = open_private(path).await.ok()?;
    let mut text = String::new();
    file.read_to_string(&mut text).await.ok()?;
    parse_record(&text)
}

async fn write_record(path: &Path, source: &Source) -> Result<(), SftpError> {
    let mut file = create_private(path).await?;
    file.write_all(record_text(source).as_bytes()).await?;
    file.sync_all().await?;
    Ok(())
}

/// The length of a regular file this user owns and only it may write, not
/// reached through a link; 0 for anything else.
async fn private_file_len(path: &Path) -> u64 {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_file() && private(&metadata) => metadata.len(),
        _ => 0,
    }
}

#[cfg(unix)]
fn private(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.uid() == nix::unistd::getuid().as_raw()
        && metadata.mode() & 0o022 == 0
        // A second name for it could be anyone's.
        && metadata.nlink() == 1
}

#[cfg(not(unix))]
fn private(_metadata: &std::fs::Metadata) -> bool {
    true
}

/// A new file only this user can read, never through an existing file or
/// link.
async fn create_private(path: &Path) -> std::io::Result<tokio::fs::File> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path).await
}

/// An existing private file, refusing a link in its place.
async fn open_private(path: &Path) -> std::io::Result<tokio::fs::File> {
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    options.custom_flags(nix::libc::O_NOFOLLOW);
    let file = options.open(path).await?;
    let metadata = file.metadata().await?;
    if !metadata.is_file() || !private(&metadata) {
        return Err(std::io::Error::other(format!(
            "{} is not a private file",
            path.display()
        )));
    }
    Ok(file)
}

/// Removes `path` when it is a file or a link (never what a link points
/// to); a missing one is fine.
async fn remove_if_file(path: &Path) -> std::io::Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_dir() => Err(std::io::Error::other(format!(
            "{} is a directory",
            path.display()
        ))),
        Ok(_) => tokio::fs::remove_file(path).await,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_any_path() {
        let source = Source {
            host: "db".into(),
            path: "/var/x\nweird name".into(),
            size: 12,
            modified: 7,
        };
        assert_eq!(parse_record(&record_text(&source)), Some(source));
        assert_eq!(parse_record("something else\n"), None);
        assert_eq!(parse_record(&format!("{RECORD_VERSION}\nzz\n")), None);
    }

    #[test]
    fn upload_records_are_named_by_hash() {
        let name = record_name("db", "../../etc/passwd");
        assert!(name.ends_with(".upload"));
        assert!(!name.contains('/'));
        assert_ne!(name, record_name("web", "../../etc/passwd"));
    }
}

#[cfg(test)]
mod pace_tests {
    use std::time::{Duration, Instant};

    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    use super::Pace;

    const RTT: Duration = Duration::from_millis(50);

    /// A transfer moving `rate` bytes a second for `secs` at round trip `rtt`.
    fn steady(rate: u64, rtt: Duration, secs: u64) -> Pace {
        let mut pace = Pace::new(&Arc::default());
        let start = Instant::now();
        for tenth in 1..=secs * 10 {
            pace.moved_at(rate / 10, rtt, start + Duration::from_millis(tenth * 100));
        }
        pace
    }

    #[test]
    fn it_starts_small_and_follows_two_round_trips_of_the_rate() {
        assert_eq!(Pace::new(&Arc::default()).budget(), Pace::START);
        // 10 MB/s at 50 ms: 1 MB in flight, two round trips' worth.
        let budget = steady(10_000_000, RTT, 3).budget();
        assert!((950_000..=1_050_000).contains(&budget), "{budget}");
    }

    #[test]
    fn a_fast_link_stops_at_the_ceiling_and_a_crawl_at_the_floor() {
        assert_eq!(steady(1 << 30, RTT, 3).budget(), Pace::MAX);
        let crawl = steady(12_000, RTT, 3);
        // 12 kB/s after its first second: the floor, so four on one link
        // still come back long before their 30 s timers.
        assert_eq!(crawl.budget(), Pace::FLOOR);
        // Within the first second it keeps the start.
        let mut young = Pace::new(&Arc::default());
        let start = Instant::now();
        young.moved_at(1, RTT, start);
        young.moved_at(1, RTT, start + Duration::from_millis(500));
        assert_eq!(young.budget(), Pace::START);
    }

    #[test]
    fn a_small_window_asks_for_smaller_requests() {
        let pace = Pace::new(&Arc::default());
        // 128 KiB holds two 64 KiB requests: four of 32 KiB instead.
        assert_eq!(pace.len(64 * 1024), 32 * 1024);
        assert_eq!(pace.requests(64 * 1024), 4);
        // A full window takes the server's size.
        let fast = steady(1 << 30, RTT, 3);
        assert_eq!(fast.len(256 * 1024), 256 * 1024);
        assert_eq!(fast.requests(256 * 1024), 32);
    }

    #[test]
    fn transfers_starting_together_share_the_start() {
        let active = Arc::new(AtomicUsize::new(0));
        let one = Pace::new(&active);
        assert_eq!(one.budget(), Pace::START);
        let others: Vec<Pace> = (0..3).map(|_| Pace::new(&active)).collect();
        assert_eq!(one.budget(), Pace::START / 4);
        drop(others);
        assert_eq!(
            one.budget(),
            Pace::START,
            "the share comes back as they end"
        );
    }

    #[test]
    fn samples_stay_bounded() {
        let mut pace = Pace::new(&Arc::default());
        let now = Instant::now();
        for _ in 0..10_000 {
            pace.moved_at(1, RTT, now);
        }
        assert!(pace.samples.len() <= Pace::MAX_SAMPLES);
    }
}
