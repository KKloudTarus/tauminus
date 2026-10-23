// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! IPC commands. Each one decodes its arguments, calls one use case, and maps
//! the result. No session logic lives here.

use std::sync::Arc;

use tauminus_app::{LocalShells, SessionManager};
use tauminus_domain::{LocalShell, TerminalSize};
use tauri::State;
use tauri::ipc::Channel;

use crate::ipc::{
    ChannelSink, CommandError, ErrorCode, GridSize, OutputChunk, SessionExit, SessionHandle,
};

/// Opens a local shell. Output arrives on `on_output`, and `on_exit` fires once
/// when the shell ends.
#[tauri::command]
#[specta::specta]
pub(crate) async fn session_open(
    shells: State<'_, LocalShells>,
    size: GridSize,
    on_output: Channel<OutputChunk>,
    on_exit: Channel<SessionExit>,
) -> Result<SessionHandle, CommandError> {
    let size = TerminalSize::try_from(size)?;
    let shells = shells.inner().clone();
    let sink = ChannelSink {
        output: on_output,
        exit: on_exit,
    };
    // Spawning a process blocks for a fork and exec; keep it off the async
    // worker threads.
    let id = tauri::async_runtime::spawn_blocking(move || {
        shells.open(&LocalShell::default(), size, sink)
    })
    .await
    .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))??;
    SessionHandle::try_from(id)
}

/// Sends text input: keystrokes, paste, or terminal replies.
#[tauri::command]
#[specta::specta]
pub(crate) async fn session_write(
    sessions: State<'_, Arc<SessionManager>>,
    id: SessionHandle,
    data: String,
) -> Result<(), CommandError> {
    Ok(sessions.write_text(id.into(), data).await?)
}

/// Sends bytes that are not valid UTF-8, such as legacy mouse reports.
#[tauri::command]
#[specta::specta]
pub(crate) async fn session_write_bytes(
    sessions: State<'_, Arc<SessionManager>>,
    id: SessionHandle,
    data: Vec<u8>,
) -> Result<(), CommandError> {
    Ok(sessions.write(id.into(), data).await?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn session_resize(
    sessions: State<'_, Arc<SessionManager>>,
    id: SessionHandle,
    size: GridSize,
) -> Result<(), CommandError> {
    Ok(sessions.resize(id.into(), size.try_into()?).await?)
}

/// Asks a session to check that its link still answers, after a wake or a
/// network change; a dead one ends with `lost`. Returns at once.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn session_probe(
    sessions: State<'_, Arc<SessionManager>>,
    id: SessionHandle,
) -> Result<(), CommandError> {
    Ok(sessions.probe(id.into())?)
}

/// Reports bytes the terminal view has finished parsing, for flow control.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn session_acknowledge(
    sessions: State<'_, Arc<SessionManager>>,
    id: SessionHandle,
    bytes: u32,
) -> Result<(), CommandError> {
    let bytes = usize::try_from(bytes).unwrap_or(usize::MAX);
    Ok(sessions.acknowledge(id.into(), bytes)?)
}

#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn session_close(
    sessions: State<'_, Arc<SessionManager>>,
    id: SessionHandle,
) -> Result<(), CommandError> {
    Ok(sessions.close(id.into())?)
}
