// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Files over IPC: SFTP sessions to saved hosts, folders on this machine,
//! and the transfers between them.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauminus_app::ports::{EntryKind, RemoteEntry, SftpError};
use tauminus_app::{Direction, Files, FilesError, TransferState, TransferStatus};
use tauri::State;
use tauri::ipc::Channel;

use crate::ipc::{CommandError, ErrorCode};
use crate::ssh::{PromptBroker, PromptRequest};

/// Longest path accepted from the view, in bytes.
const MAX_PATH: usize = 4096;
/// Most entries a local listing keeps, as for a remote one.
const MAX_LOCAL_ENTRIES: usize = 100_000;

/// Where uploads keep what a resume needs.
pub(crate) struct UploadState(pub(crate) PathBuf);

/// The user's home folder, if known.
pub(crate) struct Home(pub(crate) Option<PathBuf>);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum EntryKindSpec {
    File,
    Dir,
    Link,
    Other,
}

impl From<EntryKind> for EntryKindSpec {
    fn from(kind: EntryKind) -> Self {
        match kind {
            EntryKind::File => Self::File,
            EntryKind::Dir => Self::Dir,
            EntryKind::Link => Self::Link,
            EntryKind::Other => Self::Other,
        }
    }
}

/// One entry of a folder, here or on the server.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EntryView {
    /// As the system or the server named it; shown as text only.
    pub(crate) name: String,
    pub(crate) kind: EntryKindSpec,
    /// Bytes; a JS number is exact up to 8 PiB.
    pub(crate) size: f64,
    /// Seconds since the epoch.
    pub(crate) modified: Option<f64>,
    pub(crate) mode: Option<u32>,
}

#[expect(
    clippy::cast_precision_loss,
    reason = "sizes and times above 2^53 do not occur; the view shows them rounded"
)]
fn as_number(value: u64) -> f64 {
    value as f64
}

impl From<RemoteEntry> for EntryView {
    fn from(entry: RemoteEntry) -> Self {
        Self {
            name: entry.name,
            kind: entry.kind.into(),
            size: as_number(entry.size),
            modified: entry.modified.map(as_number),
            mode: entry.mode,
        }
    }
}

/// A folder's entries and where it is, so the view can go up.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Listing {
    pub(crate) path: String,
    pub(crate) parent: Option<String>,
    pub(crate) entries: Vec<EntryView>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SftpHandle {
    pub(crate) id: u32,
    /// The login folder, where browsing starts.
    pub(crate) home: String,
}

#[derive(Debug, Clone, Copy, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DirectionSpec {
    Download,
    Upload,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum TransferStatusSpec {
    Waiting,
    Running,
    Done,
    Cancelled,
    Failed { reason: String },
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TransferView {
    pub(crate) id: u32,
    pub(crate) session: u32,
    pub(crate) direction: DirectionSpec,
    pub(crate) name: String,
    pub(crate) done: f64,
    pub(crate) total: f64,
    pub(crate) resumed_from: f64,
    pub(crate) status: TransferStatusSpec,
}

impl From<TransferState> for TransferView {
    fn from(state: TransferState) -> Self {
        Self {
            id: u32::try_from(state.id).unwrap_or(u32::MAX),
            session: u32::try_from(state.session).unwrap_or(u32::MAX),
            direction: match state.direction {
                Direction::Download => DirectionSpec::Download,
                Direction::Upload => DirectionSpec::Upload,
            },
            name: state.name,
            done: as_number(state.progress.done),
            total: as_number(state.progress.total),
            resumed_from: as_number(state.progress.resumed_from),
            status: match state.status {
                TransferStatus::Waiting => TransferStatusSpec::Waiting,
                TransferStatus::Running => TransferStatusSpec::Running,
                TransferStatus::Done => TransferStatusSpec::Done,
                TransferStatus::Cancelled => TransferStatusSpec::Cancelled,
                TransferStatus::Failed(reason) => TransferStatusSpec::Failed { reason },
            },
        }
    }
}

impl From<FilesError> for CommandError {
    fn from(error: FilesError) -> Self {
        let code = match &error {
            FilesError::Connect(error) => connect_code(error),
            FilesError::NoSession => ErrorCode::NotFound,
            FilesError::TooManySessions | FilesError::TooManyTransfers | FilesError::Busy(_) => {
                ErrorCode::LimitReached
            }
            FilesError::Name(..) => ErrorCode::InvalidArgument,
            FilesError::Sftp(SftpError::Cancelled) => ErrorCode::Cancelled,
            FilesError::Sftp(SftpError::Closed) => ErrorCode::ConnectFailed,
            FilesError::Sftp(SftpError::Exists(_) | SftpError::TooLarge(_)) => {
                ErrorCode::InvalidArgument
            }
            FilesError::Sftp(_) => ErrorCode::LaunchFailed,
        };
        Self::new(code, error.to_string())
    }
}

fn connect_code(error: &tauminus_app::ports::ConnectError) -> ErrorCode {
    use tauminus_app::ports::ConnectError;
    match error {
        ConnectError::HostKeyChanged { .. } => ErrorCode::HostKeyChanged,
        ConnectError::HostKeyRevoked { .. } => ErrorCode::HostKeyRevoked,
        ConnectError::HostKeyRejected { .. } | ConnectError::Cancelled => ErrorCode::Cancelled,
        ConnectError::AuthFailed { .. } => ErrorCode::AuthFailed,
        ConnectError::Protocol(_) => ErrorCode::ProtocolError,
        _ => ErrorCode::ConnectFailed,
    }
}

fn invalid(error: impl std::fmt::Display) -> CommandError {
    CommandError::new(ErrorCode::InvalidArgument, error.to_string())
}

/// A path from the view: not empty, not huge, no NUL.
pub(crate) fn checked(path: &str) -> Result<&str, CommandError> {
    if path.is_empty() || path.len() > MAX_PATH || path.contains('\0') {
        return Err(invalid("the path is empty, too long, or holds a NUL"));
    }
    Ok(path)
}

fn session(id: u32) -> u64 {
    u64::from(id)
}

fn sftp_failed(error: SftpError) -> CommandError {
    CommandError::from(FilesError::Sftp(error))
}

/// Opens SFTP to a saved host, on the connection its forwards share, with
/// any question on `on_prompt`.
#[tauri::command]
#[specta::specta]
pub(crate) async fn sftp_open(
    hosts: State<'_, crate::hosts::Hosts>,
    vault: State<'_, Arc<tauminus_app::vault::Vault>>,
    broker: State<'_, Arc<PromptBroker>>,
    files: State<'_, Arc<Files>>,
    host: String,
    on_prompt: Channel<PromptRequest>,
) -> Result<SftpHandle, CommandError> {
    let login = crate::ssh::login(&hosts, &vault, &host).await?;
    let prompter = crate::ssh::prompter(&broker, on_prompt);
    let id = files.open(&login, prompter).await?;
    let home = match files.sftp(id)?.canonicalize(".").await {
        Ok(home) => home,
        Err(error) => {
            files.close(id);
            return Err(sftp_failed(error));
        }
    };
    let id = u32::try_from(id)
        .map_err(|_| CommandError::new(ErrorCode::LimitReached, "too many sessions"))?;
    Ok(SftpHandle { id, home })
}

#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn sftp_close(files: State<'_, Arc<Files>>, id: u32) {
    files.close(session(id));
}

/// A remote folder's entries, at most 100 000.
#[tauri::command]
#[specta::specta]
pub(crate) async fn sftp_list(
    files: State<'_, Arc<Files>>,
    id: u32,
    path: String,
) -> Result<Listing, CommandError> {
    let sftp = files.sftp(session(id))?;
    let path = sftp
        .canonicalize(checked(&path)?)
        .await
        .map_err(sftp_failed)?;
    let entries = sftp.list(&path).await.map_err(sftp_failed)?;
    Ok(Listing {
        parent: tauminus_domain::remote_parent(&path)
            .filter(|p| *p != path)
            .map(str::to_owned),
        entries: entries.into_iter().map(EntryView::from).collect(),
        path,
    })
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum ChangeSpec {
    Mkdir { path: String },
    RemoveFile { path: String },
    RemoveDir { path: String },
    Rename { from: String, to: String },
    Chmod { path: String, mode: u32 },
}

/// One change to the server's files.
#[tauri::command]
#[specta::specta]
pub(crate) async fn sftp_change(
    files: State<'_, Arc<Files>>,
    id: u32,
    change: ChangeSpec,
) -> Result<(), CommandError> {
    let sftp = files.sftp(session(id))?;
    let result = match &change {
        ChangeSpec::Mkdir { path } => sftp.mkdir(checked(path)?).await,
        ChangeSpec::RemoveFile { path } => sftp.remove_file(checked(path)?).await,
        ChangeSpec::RemoveDir { path } => sftp.remove_dir(checked(path)?).await,
        ChangeSpec::Rename { from, to } => sftp.rename(checked(from)?, checked(to)?).await,
        ChangeSpec::Chmod { path, mode } => {
            if *mode > 0o7777 {
                return Err(invalid("the mode must be at most 7777 in octal"));
            }
            sftp.chmod(checked(path)?, *mode).await
        }
    };
    result.map_err(sftp_failed)
}

/// A folder on this machine. Links are listed as links, not followed.
#[tauri::command]
#[specta::specta]
pub(crate) async fn local_list(path: String) -> Result<Listing, CommandError> {
    let path = PathBuf::from(checked(&path)?);
    tauri::async_runtime::spawn_blocking(move || read_local(&path))
        .await
        .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))?
}

fn read_local(path: &Path) -> Result<Listing, CommandError> {
    let io = |e: std::io::Error| CommandError::new(ErrorCode::NotFound, e.to_string());
    let path = path.canonicalize().map_err(io)?;
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&path).map_err(io)? {
        if entries.len() == MAX_LOCAL_ENTRIES {
            break;
        }
        let Ok(entry) = entry else { continue };
        let Ok(metadata) = entry.path().symlink_metadata() else {
            continue;
        };
        let kind = if metadata.file_type().is_symlink() {
            EntryKindSpec::Link
        } else if metadata.is_dir() {
            EntryKindSpec::Dir
        } else if metadata.is_file() {
            EntryKindSpec::File
        } else {
            EntryKindSpec::Other
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| as_number(d.as_secs()));
        entries.push(EntryView {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind,
            size: as_number(metadata.len()),
            modified,
            mode: mode_of(&metadata),
        });
    }
    Ok(Listing {
        parent: path.parent().map(|p| p.to_string_lossy().into_owned()),
        path: path.to_string_lossy().into_owned(),
        entries,
    })
}

#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "Windows has no mode, and both versions share this signature"
)]
fn mode_of(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode_of(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}

/// The user's home folder, where the local side starts.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn local_home(home: State<'_, Home>) -> Result<String, CommandError> {
    home.0
        .as_ref()
        .map(|home| home.to_string_lossy().into_owned())
        .ok_or_else(|| CommandError::new(ErrorCode::NotFound, "no home folder"))
}

/// Copies a remote file into a local folder under its checked name.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn transfer_download(
    files: State<'_, Arc<Files>>,
    id: u32,
    remote: String,
    into: String,
    overwrite: bool,
) -> Result<u32, CommandError> {
    let transfer = files.download(
        session(id),
        checked(&remote)?,
        Path::new(checked(&into)?),
        overwrite,
    )?;
    u32::try_from(transfer)
        .map_err(|_| CommandError::new(ErrorCode::LimitReached, "too many transfers"))
}

/// Copies a local file into a remote folder under its name.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn transfer_upload(
    files: State<'_, Arc<Files>>,
    state: State<'_, UploadState>,
    id: u32,
    local: String,
    into: String,
    overwrite: bool,
) -> Result<u32, CommandError> {
    let transfer = files.upload(
        session(id),
        Path::new(checked(&local)?),
        checked(&into)?,
        overwrite,
        state.0.clone(),
    )?;
    u32::try_from(transfer)
        .map_err(|_| CommandError::new(ErrorCode::LimitReached, "too many transfers"))
}

#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn transfers_status(files: State<'_, Arc<Files>>) -> Vec<TransferView> {
    files
        .transfers()
        .into_iter()
        .map(TransferView::from)
        .collect()
}

/// Stops a transfer; a later one of the same file resumes it.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn transfer_cancel(files: State<'_, Arc<Files>>, id: u32) -> bool {
    files.cancel(u64::from(id))
}

#[cfg(test)]
mod tests;
