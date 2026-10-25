// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Telnet sessions over IPC: a saved Telnet host, opened in a pane.

use tauminus_app::TelnetSessions;
use tauminus_domain::HostId;
use tauri::State;
use tauri::ipc::Channel;

use crate::hosts::Hosts;
use crate::ipc::{
    ChannelSink, CommandError, ErrorCode, GridSize, OutputChunk, SessionExit, SessionHandle,
};

/// Connects to a saved Telnet host and streams it to the view. Nothing
/// is asked: Telnet has no host key, and a login, if any, is the
/// remote shell's own prompt.
#[tauri::command]
#[specta::specta]
pub(crate) async fn telnet_connect(
    telnet: State<'_, TelnetSessions>,
    hosts: State<'_, Hosts>,
    gate: State<'_, crate::sync::Gate>,
    id: String,
    size: GridSize,
    on_output: Channel<OutputChunk>,
    on_exit: Channel<SessionExit>,
) -> Result<SessionHandle, CommandError> {
    let id = HostId::parse(&id)
        .map_err(|e| CommandError::new(ErrorCode::InvalidArgument, e.to_string()))?;
    let target = hosts
        .book()?
        .telnet_target(&id)
        .await
        .map_err(|e| CommandError::new(ErrorCode::NotFound, e.to_string()))?;
    gate.check(&hosts, &id).await?;
    let sink = ChannelSink {
        output: on_output,
        exit: on_exit,
    };
    let session = telnet.open(&target, size.try_into()?, sink).await?;
    SessionHandle::try_from(session)
}
