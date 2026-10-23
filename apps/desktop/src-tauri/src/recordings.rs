// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Session recordings over IPC: start and stop on a pane, list, play back
//! (which needs the vault open), bookmarks, and the player's checkpoints.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauminus_app::recording::{RecordingError, Recordings};
use tauminus_domain::{Bookmark, MAX_BOOKMARK_TEXT, RecordingMeta};
use tauri::State;
use tauri::ipc::Channel;

use crate::ipc::{CommandError, ErrorCode, GridSize, OutputChunk, SessionHandle};

/// A recording, as the list shows it.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecordingView {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) host: Option<String>,
    /// Milliseconds since the Unix epoch, as a JavaScript `Date` takes.
    pub(crate) started_ms: f64,
    pub(crate) duration_ms: f64,
    pub(crate) bytes: f64,
    /// Stopped before the session did: the disk fell behind, or the size
    /// limit was reached.
    pub(crate) truncated: bool,
    /// Ended cleanly; `false` after a crash, or while still recording.
    pub(crate) complete: bool,
}

#[expect(
    clippy::cast_precision_loss,
    reason = "times and sizes stay far below 2^53"
)]
impl From<RecordingMeta> for RecordingView {
    fn from(meta: RecordingMeta) -> Self {
        Self {
            id: meta.id,
            title: meta.title,
            host: meta.host,
            started_ms: meta.started_ms as f64,
            duration_ms: meta.duration_ms as f64,
            bytes: meta.bytes as f64,
            truncated: meta.truncated,
            complete: meta.complete,
        }
    }
}

/// A recording in progress, and the pane's session.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActiveRecording {
    pub(crate) session: SessionHandle,
    pub(crate) recording: RecordingView,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BookmarkSpec {
    pub(crate) at_ms: f64,
    pub(crate) label: String,
    pub(crate) comment: String,
}

/// How a playback read ended.
#[derive(Debug, Clone, Copy, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReadDone {
    /// The recording ended cleanly; `false`: it stops where a crash cut it.
    pub(crate) complete: bool,
}

/// Longest title a pane can give its recording.
const MAX_TITLE: usize = 200;
/// Largest set of checkpoints the player may store: an hour of heavy output
/// needs about 13 MiB of them.
const MAX_CHECKPOINTS: usize = 64 * 1024 * 1024;
/// Plain bytes per message sent to the view while playing back: fewer,
/// larger messages share the IPC queue with live panes better.
const READ_CHUNK: usize = 4 * 1024 * 1024;

impl From<RecordingError> for CommandError {
    fn from(error: RecordingError) -> Self {
        match error {
            RecordingError::Vault(error) => error.into(),
            // The pane's session ended before the recording could start.
            RecordingError::Session(tauminus_app::SessionError::NotFound(_)) => {
                Self::new(ErrorCode::InvalidArgument, "the pane's session has ended")
            }
            RecordingError::Session(error) => error.into(),
            RecordingError::NotFound(_) => Self::new(ErrorCode::NotFound, error.to_string()),
            RecordingError::AlreadyRecording
            | RecordingError::TooMany
            | RecordingError::TooLarge => Self::new(ErrorCode::InvalidArgument, error.to_string()),
            RecordingError::Store(_) => Self::new(ErrorCode::StoreFailed, error.to_string()),
        }
    }
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(ErrorCode::InvalidArgument, message)
}

/// Starts recording a pane's session from now on. Works with the vault
/// locked once the recordings key exists; the very first recording needs
/// the vault open (`vault_locked` otherwise).
#[tauri::command]
#[specta::specta]
pub(crate) async fn recording_start(
    recordings: State<'_, Arc<Recordings>>,
    session: SessionHandle,
    size: GridSize,
    title: String,
    host: Option<String>,
    initial: Option<String>,
) -> Result<RecordingView, CommandError> {
    let title: String = title
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TITLE)
        .collect();
    if host.as_ref().is_some_and(|h| h.len() > MAX_TITLE) {
        return Err(invalid("the host id is too long"));
    }
    let meta = recordings
        .start(session.into(), size.try_into()?, title, host, initial)
        .await?;
    Ok(meta.into())
}

/// Stops recording a pane; `null` when it was not recording.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn recording_stop(
    recordings: State<'_, Arc<Recordings>>,
    session: SessionHandle,
) -> Option<RecordingView> {
    recordings.stop(session.into()).map(Into::into)
}

/// The panes recording now.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn recordings_active(recordings: State<'_, Arc<Recordings>>) -> Vec<ActiveRecording> {
    recordings
        .recording()
        .into_iter()
        .filter_map(|(session, meta)| {
            Some(ActiveRecording {
                session: SessionHandle::try_from(session).ok()?,
                recording: meta.into(),
            })
        })
        .collect()
}

/// Every recording, newest first. Needs no unlock.
#[tauri::command]
#[specta::specta]
pub(crate) async fn recordings_list(
    recordings: State<'_, Arc<Recordings>>,
) -> Result<Vec<RecordingView>, CommandError> {
    Ok(recordings
        .list()
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
}

/// Sends recording `id`'s asciicast text on `on_chunk`, in order, in
/// pieces of at most 256 KiB. Needs the vault open.
#[tauri::command]
#[specta::specta]
pub(crate) async fn recording_read(
    recordings: State<'_, Arc<Recordings>>,
    id: String,
    on_chunk: Channel<OutputChunk>,
) -> Result<ReadDone, CommandError> {
    // Frames are 64 KiB: gather them into larger messages, and send what
    // is left once the read is done.
    let gathered = Arc::new(std::sync::Mutex::new(Vec::with_capacity(READ_CHUNK)));
    let filling = Arc::clone(&gathered);
    let sending = on_chunk.clone();
    let complete = recordings
        .read(id, move |bytes| {
            let mut buffer = filling
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            buffer.extend_from_slice(bytes);
            if buffer.len() < READ_CHUNK {
                return true;
            }
            let chunk = std::mem::replace(&mut *buffer, Vec::with_capacity(READ_CHUNK));
            sending.send(OutputChunk(chunk)).is_ok()
        })
        .await?;
    let rest = std::mem::take(
        &mut *gathered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    if !rest.is_empty() {
        // The view may be gone already.
        let _ = on_chunk.send(OutputChunk(rest));
    }
    Ok(ReadDone { complete })
}

/// The player's saved checkpoints for recording `id`, sent on
/// `on_checkpoints` as one piece; `false` and nothing sent when there are
/// none. Needs the vault open.
#[tauri::command]
#[specta::specta]
pub(crate) async fn recording_checkpoints(
    recordings: State<'_, Arc<Recordings>>,
    id: String,
    on_checkpoints: Channel<OutputChunk>,
) -> Result<bool, CommandError> {
    match recordings.checkpoints(id).await? {
        Some(bytes) => Ok(on_checkpoints.send(OutputChunk(bytes)).is_ok()),
        None => Ok(false),
    }
}

/// Replaces the player's checkpoints for recording `id`. Needs no unlock.
#[tauri::command]
#[specta::specta]
pub(crate) async fn recording_save_checkpoints(
    recordings: State<'_, Arc<Recordings>>,
    id: String,
    checkpoints: String,
) -> Result<(), CommandError> {
    if checkpoints.len() > MAX_CHECKPOINTS {
        return Err(invalid("the checkpoints are too large"));
    }
    Ok(recordings
        .save_checkpoints(id, checkpoints.into_bytes())
        .await?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn recording_bookmarks(
    recordings: State<'_, Arc<Recordings>>,
    id: String,
) -> Result<Vec<BookmarkSpec>, CommandError> {
    Ok(recordings
        .bookmarks(id)
        .await?
        .into_iter()
        .map(|b| BookmarkSpec {
            #[expect(clippy::cast_precision_loss, reason = "times stay far below 2^53 ms")]
            at_ms: b.at_ms as f64,
            label: b.label,
            comment: b.comment,
        })
        .collect())
}

/// Replaces recording `id`'s bookmarks.
#[tauri::command]
#[specta::specta]
pub(crate) async fn recording_save_bookmarks(
    recordings: State<'_, Arc<Recordings>>,
    id: String,
    bookmarks: Vec<BookmarkSpec>,
) -> Result<(), CommandError> {
    let bookmarks = bookmarks
        .into_iter()
        .map(|b| {
            if !(b.at_ms.is_finite() && b.at_ms >= 0.0) {
                return Err(invalid("a bookmark's time is not a time"));
            }
            if b.label.len() + b.comment.len() > MAX_BOOKMARK_TEXT {
                return Err(invalid("a bookmark is too long"));
            }
            Ok(Bookmark {
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "checked finite and not negative"
                )]
                at_ms: b.at_ms as u64,
                label: b.label,
                comment: b.comment,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(recordings.save_bookmarks(id, bookmarks).await?)
}

/// Deletes recording `id` and everything that goes with it.
#[tauri::command]
#[specta::specta]
pub(crate) async fn recording_delete(
    recordings: State<'_, Arc<Recordings>>,
    id: String,
) -> Result<(), CommandError> {
    Ok(recordings.delete(id).await?)
}
