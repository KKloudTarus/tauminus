// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Zmodem over IPC. Files a host sends with `sz` (ADR 0025) go to
//! `Downloads/Tauminus`: the view asks the user first, and only names the
//! host sent, checked in the domain, are joined to that folder. Files sent
//! to `rz` (ADR 0026) are chosen in a native dialog this module opens: no
//! path comes from the view.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use specta::Type;
use tauminus_app::SessionManager;
use tauminus_app::ports::{
    OutgoingFile, PartFile, ReceiveFolder, TransferAnswer, TransferEvent, TransferHost,
};
use tauminus_domain::{LocalOs, SessionId, Wants, local_name};
use tauri::State;
use tauri::ipc::Channel;

use crate::ipc::{CommandError, ErrorCode, SessionHandle};

mod outgoing;

const PART: &str = ".tauminus-part";
/// Numbered names tried before a file is refused: `a (1).txt` and on.
const MAX_NUMBER: u32 = 999;
const BUFFER: usize = 64 << 10;

/// What a session's transfer tells the view.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub(crate) enum ZmodemView {
    /// The host offers to send files (`sz`) or to take them (`rz`);
    /// nothing is sent back until the user answers this offer, by its
    /// number.
    Offered {
        session: SessionHandle,
        offer: f64,
        wants: Direction,
    },
    /// Files come into `folder`, or, with none, go to the host.
    Started {
        session: SessionHandle,
        folder: Option<String>,
    },
    File {
        session: SessionHandle,
        name: String,
        size: Option<f64>,
    },
    Progress {
        session: SessionHandle,
        bytes: f64,
        size: Option<f64>,
    },
    /// The first 100 names of each list, and how many more.
    #[serde(rename_all = "camelCase")]
    Finished {
        session: SessionHandle,
        folder: Option<String>,
        files: Vec<String>,
        more_files: f64,
        skipped: Vec<String>,
        more_skipped: f64,
    },
    Failed {
        session: SessionHandle,
        reason: String,
    },
    Declined {
        session: SessionHandle,
    },
}

/// Which way an offer's files go.
#[derive(Debug, Clone, Copy, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Direction {
    /// The host sends (`sz`): the user receives.
    Receive,
    /// The host takes (`rz`): the user sends.
    Send,
}

/// Where transfer events go: the view's one channel, once it listens.
#[derive(Default)]
pub(crate) struct Transfers {
    channel: Mutex<Option<Channel<ZmodemView>>>,
}

impl std::fmt::Debug for Transfers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transfers").finish_non_exhaustive()
    }
}

impl Transfers {
    fn send(&self, view: ZmodemView) {
        let channel = self
            .channel
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(channel) = channel {
            let _ = channel.send(view);
        }
    }
}

/// Bytes as a JS number: exact to 2^53, far past any file.
#[expect(clippy::cast_precision_loss, reason = "exact below 2^53")]
fn number(bytes: u64) -> f64 {
    bytes as f64
}

impl TransferHost for Transfers {
    fn offered(&self, id: SessionId, offer: u64, wants: Wants) {
        if let Ok(session) = SessionHandle::try_from(id) {
            self.send(ZmodemView::Offered {
                session,
                offer: number(offer),
                wants: match wants {
                    Wants::Receive => Direction::Receive,
                    Wants::Send => Direction::Send,
                },
            });
        }
    }

    fn event(&self, id: SessionId, event: TransferEvent) {
        let Ok(session) = SessionHandle::try_from(id) else {
            return;
        };
        self.send(match event {
            TransferEvent::Started { folder } => ZmodemView::Started { session, folder },
            TransferEvent::File { name, size } => ZmodemView::File {
                session,
                name,
                size: size.map(number),
            },
            TransferEvent::Progress { bytes, size } => ZmodemView::Progress {
                session,
                bytes: number(bytes),
                size: size.map(number),
            },
            TransferEvent::Finished {
                folder,
                files,
                more_files,
                skipped,
                more_skipped,
            } => ZmodemView::Finished {
                session,
                folder,
                files,
                more_files: number(more_files),
                skipped,
                more_skipped: number(more_skipped),
            },
            TransferEvent::Failed { reason } => ZmodemView::Failed { session, reason },
            TransferEvent::Declined => ZmodemView::Declined { session },
        });
    }
}

/// The folder files are received into.
#[derive(Debug, Clone)]
pub(crate) struct ReceiveRoot(pub(crate) PathBuf);

/// A folder on disk. Each file is written as `<name>.tauminus-part`,
/// created new and private, then linked to a name no file has.
#[derive(Debug)]
pub(crate) struct DiskFolder {
    root: PathBuf,
    /// The number to try first for a name: a host sending one name many
    /// times costs one look per file, not one per number taken.
    next: HashMap<String, u32>,
}

impl DiskFolder {
    pub(crate) fn open(root: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            next: HashMap::new(),
        })
    }
}

/// Bytes in a name most file systems take.
const MAX_NAME: usize = 255;

/// `name`, then `stem (1).ext`, `stem (2).ext` and on, its stem cut so
/// that with [`PART`] it fits in [`MAX_NAME`] bytes. O(len).
fn numbered(name: &str, n: u32) -> String {
    let number = if n == 0 {
        String::new()
    } else {
        format!(" ({n})")
    };
    // An extension that leaves the stem no room is cut with it.
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty() && PART.len() + number.len() + ext.len() + 2 <= MAX_NAME =>
        {
            (stem, format!(".{ext}"))
        }
        _ => (name, String::new()),
    };
    let room = MAX_NAME.saturating_sub(PART.len() + number.len() + ext.len());
    let mut cut = stem.len().min(room);
    while !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{number}{ext}", &stem[..cut])
}

fn create_new(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

impl ReceiveFolder for DiskFolder {
    /// O(k) for the k names taken before a free one, at most 1000.
    fn create(&mut self, name: &str) -> std::io::Result<(String, Box<dyn PartFile>)> {
        let first = self.next.get(name).copied().unwrap_or(0);
        for n in first..=MAX_NUMBER {
            let saved = numbered(name, n);
            // Cut to fit, a name could become one the host may not use: a
            // device name, or hidden.
            if saved.starts_with('.') || local_name(&saved, LocalOs::current()).is_err() {
                continue;
            }
            let path = self.root.join(&saved);
            if std::fs::symlink_metadata(&path).is_ok() {
                continue;
            }
            let part = self.root.join(format!("{saved}{PART}"));
            match create_new(&part) {
                Ok(file) => {
                    self.next.insert(name.to_owned(), n + 1);
                    return Ok((
                        saved,
                        Box::new(DiskPart {
                            file: Some(BufWriter::with_capacity(BUFFER, file)),
                            part,
                            path,
                        }),
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "every numbered name is taken",
        ))
    }

    fn shown(&self) -> String {
        self.root.display().to_string()
    }
}

/// A file being written. Dropped before it finished, it is removed.
#[derive(Debug)]
struct DiskPart {
    /// `None` once finished.
    file: Option<BufWriter<File>>,
    part: PathBuf,
    path: PathBuf,
}

impl PartFile for DiskPart {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        match &mut self.file {
            Some(file) => file.write_all(bytes),
            None => Err(std::io::ErrorKind::BrokenPipe.into()),
        }
    }

    fn finish(mut self: Box<Self>) -> std::io::Result<()> {
        let Some(file) = self.file.take() else {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        };
        let done = file
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)
            .and_then(|file| {
                // Closed first: Windows renames no open file.
                drop(file);
                put_in_place(&self.part, &self.path)
            });
        // Whatever happened, the part file goes.
        let _ = std::fs::remove_file(&self.part);
        if done.is_ok() {
            mark_downloaded(&self.path);
        }
        done
    }

    /// Dropping it removes what was written.
    fn discard(self: Box<Self>) {}
}

impl Drop for DiskPart {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            let _ = std::fs::remove_file(&self.part);
        }
    }
}

/// Puts a complete part file at `path`, never over a file there.
fn put_in_place(part: &Path, path: &Path) -> std::io::Result<()> {
    // A link fails if a file appeared there meanwhile: nothing is replaced.
    match std::fs::hard_link(part, path) {
        Ok(()) => Ok(()),
        // FAT, exFAT, and some shares have no hard links: a last look, then
        // a rename. A local process could create `path` in between; the
        // host cannot.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::Unsupported | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            if std::fs::symlink_metadata(path).is_ok() {
                return Err(std::io::ErrorKind::AlreadyExists.into());
            }
            std::fs::rename(part, path)
        }
        Err(error) => Err(error),
    }
}

/// Marks a received file as from the internet, as browsers do: Windows
/// then asks before running it (`SmartScreen`) and opens documents in
/// Protected View.
#[cfg(windows)]
fn mark_downloaded(path: &Path) {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    let _ = std::fs::write(stream, "[ZoneTransfer]\r\nZoneId=3\r\n");
}

#[cfg(not(windows))]
fn mark_downloaded(_path: &Path) {}

/// Where transfer events go from now on.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn zmodem_listen(transfers: State<'_, Arc<Transfers>>, on_event: Channel<ZmodemView>) {
    *transfers
        .channel
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(on_event);
}

/// The user's answer to `session`'s offer: receive into the folder, or
/// not.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn zmodem_receive(
    sessions: State<'_, Arc<SessionManager>>,
    root: State<'_, ReceiveRoot>,
    session: SessionHandle,
    offer: f64,
    accept: bool,
) -> Result<(), CommandError> {
    if !(offer >= 1.0 && offer.fract() == 0.0 && offer < 2f64.powi(53)) {
        return Err(CommandError::new(
            ErrorCode::InvalidArgument,
            "an offer is a whole number",
        ));
    }
    let offer = offer_number(offer)?;
    let answer = if accept {
        let folder = DiskFolder::open(&root.0).map_err(|error| {
            CommandError::new(
                ErrorCode::Internal,
                format!("could not open {}: {error}", root.0.display()),
            )
        })?;
        TransferAnswer::Receive(Box::new(folder))
    } else {
        TransferAnswer::Decline
    };
    Ok(sessions.answer_transfer(session.into(), offer, answer)?)
}

/// An offer's number from the view: a whole number from 1.
fn offer_number(offer: f64) -> Result<u64, CommandError> {
    if !(offer >= 1.0 && offer.fract() == 0.0 && offer < 2f64.powi(53)) {
        return Err(CommandError::new(
            ErrorCode::InvalidArgument,
            "an offer is a whole number",
        ));
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a whole number in range, checked above"
    )]
    Ok(offer as u64)
}

/// One file dialog at a time, in the whole app: a host cannot stack them.
#[derive(Debug, Default)]
pub(crate) struct Choosing(pub(crate) AtomicBool);

/// Opens the system's file dialog for `rz` waiting in `session` (offer
/// `offer`), and sends what the user chose; nothing chosen declines. The
/// paths never reach the view. `true` when files went to the session.
#[tauri::command]
#[specta::specta]
pub(crate) async fn zmodem_choose<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    sessions: State<'_, Arc<SessionManager>>,
    choosing: State<'_, Choosing>,
    session: SessionHandle,
    offer: f64,
) -> Result<bool, CommandError> {
    let offer = offer_number(offer)?;
    let id = SessionId::from(session);
    // Only for `rz` waiting now, with this number.
    let waits = || -> Result<(), CommandError> {
        if sessions.waiting_offer(id)? == Some((offer, Wants::Send)) {
            Ok(())
        } else {
            Err(CommandError::new(
                ErrorCode::NotFound,
                "rz is not waiting for files in this session",
            ))
        }
    };
    waits()?;
    if choosing.0.swap(true, Ordering::AcqRel) {
        return Err(CommandError::new(
            ErrorCode::NotAllowed,
            "a file dialog is open already",
        ));
    }
    let picked = tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        app.dialog()
            .file()
            .set_title("Send files to rz (Zmodem)")
            .blocking_pick_files()
    })
    .await;
    choosing.0.store(false, Ordering::Release);
    let paths: Vec<PathBuf> = picked
        .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))?
        .unwrap_or_default()
        .into_iter()
        .filter_map(|path| path.into_path().ok())
        .collect();
    // The offer may have gone while the dialog was open.
    waits()?;
    if paths.is_empty() {
        sessions.answer_transfer(id, offer, TransferAnswer::Decline)?;
        return Ok(false);
    }
    let files = tauri::async_runtime::spawn_blocking(move || open_outgoing(&paths))
        .await
        .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()));
    match files {
        Ok(Ok(files)) => {
            sessions.answer_transfer(id, offer, TransferAnswer::Send(files))?;
            Ok(true)
        }
        Ok(Err(message)) | Err(CommandError { message, .. }) => {
            sessions.answer_transfer(id, offer, TransferAnswer::Decline)?;
            Err(CommandError::new(ErrorCode::InvalidArgument, message))
        }
    }
}

/// Opens every chosen file, or says which cannot be sent.
fn open_outgoing(paths: &[PathBuf]) -> Result<Vec<Box<dyn OutgoingFile>>, String> {
    if paths.len() > outgoing::MAX_FILES {
        return Err(format!("at most {} files go at once", outgoing::MAX_FILES));
    }
    paths
        .iter()
        .map(|path| {
            outgoing::DiskOutgoing::open(path).map(|file| Box::new(file) as Box<dyn OutgoingFile>)
        })
        .collect()
}

/// Stops `session`'s transfer, or declines its offer.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn zmodem_cancel(
    sessions: State<'_, Arc<SessionManager>>,
    session: SessionHandle,
) -> Result<(), CommandError> {
    Ok(sessions.cancel_transfer(session.into())?)
}

/// Opens the folder files are received into.
#[tauri::command]
#[specta::specta]
pub(crate) async fn zmodem_reveal(root: State<'_, ReceiveRoot>) -> Result<(), CommandError> {
    let root = root.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        std::fs::create_dir_all(&root)?;
        open::that_detached(&root)
    })
    .await
    .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))?
    .map_err(|e| {
        CommandError::new(
            ErrorCode::Internal,
            format!("could not open the folder: {e}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tauminus-receive-{}-{}",
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
    fn a_file_is_written_beside_its_name_then_put_in_place() {
        let dir = scratch();
        let mut folder = DiskFolder::open(&dir).unwrap();
        let (name, mut part) = folder.create("a.txt").unwrap();
        assert_eq!(name, "a.txt");
        part.write(b"hello").unwrap();
        assert!(dir.join("a.txt.tauminus-part").exists());
        assert!(!dir.join("a.txt").exists());
        part.finish().unwrap();
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"hello");
        assert!(!dir.join("a.txt.tauminus-part").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("a.txt"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_taken_name_gets_a_number_and_nothing_is_replaced() {
        let dir = scratch();
        std::fs::write(dir.join("a.txt"), b"mine").unwrap();
        std::fs::write(dir.join("notes"), b"mine").unwrap();
        let mut folder = DiskFolder::open(&dir).unwrap();
        let (name, part) = folder.create("a.txt").unwrap();
        assert_eq!(name, "a (1).txt");
        part.finish().unwrap();
        let (name, part) = folder.create("notes").unwrap();
        assert_eq!(name, "notes (1)");
        // A file that appears at the name meanwhile is kept.
        std::fs::write(dir.join("notes (1)"), b"theirs").unwrap();
        assert!(part.finish().is_err());
        assert_eq!(std::fs::read(dir.join("notes (1)")).unwrap(), b"theirs");
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"mine");
        assert!(!dir.join("notes (1).tauminus-part").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_discarded_file_leaves_nothing() {
        let dir = scratch();
        let mut folder = DiskFolder::open(&dir).unwrap();
        let (_, mut part) = folder.create("b.bin").unwrap();
        part.write(b"half").unwrap();
        part.discard();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn numbers_go_before_the_extension_and_long_names_are_cut() {
        assert_eq!(numbered("a.tar.gz", 2), "a.tar (2).gz");
        assert_eq!(numbered("_bashrc", 1), "_bashrc (1)");
        assert_eq!(numbered("x", 0), "x");
        let long = format!("{}.log", "é".repeat(125));
        for n in [0, 7, 999] {
            let name = numbered(&long, n);
            assert!(name.len() + PART.len() <= MAX_NAME, "{n}: {}", name.len());
            assert_eq!(name.rsplit_once('.').map(|(_, ext)| ext), Some("log"));
        }
    }

    #[test]
    fn a_cut_name_never_becomes_hidden_or_a_device() {
        let dir = scratch();
        let mut folder = DiskFolder::open(&dir).unwrap();
        for name in [
            format!("a.{}", "b".repeat(240)),
            format!("CONX.{}", "b".repeat(237)),
        ] {
            let (saved, part) = folder.create(&name).unwrap();
            assert!(!saved.starts_with('.'), "{saved}");
            // On Windows that rules out `CON.…` too.
            assert!(local_name(&saved, LocalOs::current()).is_ok(), "{saved}");
            assert!(saved.len() + PART.len() <= MAX_NAME);
            part.finish().unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_part_dropped_before_it_finished_is_removed() {
        let dir = scratch();
        let mut folder = DiskFolder::open(&dir).unwrap();
        let (_, mut part) = folder.create("c.bin").unwrap();
        part.write(b"half").unwrap();
        drop(part);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn many_files_of_one_name_each_get_the_next_number() {
        let dir = scratch();
        let mut folder = DiskFolder::open(&dir).unwrap();
        for n in 0..5 {
            let (name, part) = folder.create("same.txt").unwrap();
            assert_eq!(name, numbered("same.txt", n));
            part.finish().unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
