// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! A saved host's tmux windows as tabs, over IPC (ADR 0024). The view opens
//! the control client, then a session per window's pane as it shows it.

use std::sync::Arc;

use serde::Serialize;
use specta::Type;
use tauminus_app::ports::SinkClosed;
use tauminus_app::{
    ControlId, Forwards, Opened, SshOpenError, TabWindow, TmuxError, TmuxSessions, TopologyEvent,
    TopologySink,
};
use tauminus_domain::PaneId;
use tauri::State;
use tauri::ipc::Channel;

use crate::ipc::{
    ChannelSink, CommandError, ErrorCode, GridSize, OutputChunk, SessionExit, SessionHandle,
};
use crate::ssh::{PromptBroker, PromptRequest, login, prompter};

impl From<TmuxError> for CommandError {
    fn from(error: TmuxError) -> Self {
        let code = match error {
            TmuxError::Connect(error) => return SshOpenError::Connect(error).into(),
            TmuxError::Session(error) => return error.into(),
            TmuxError::NoSession
            | TmuxError::NotStarted
            | TmuxError::Encoding
            | TmuxError::Version(_)
            | TmuxError::Split => ErrorCode::Unsupported,
            TmuxError::Refused(_) => ErrorCode::ProtocolError,
            TmuxError::Ended | TmuxError::NoSuchPane => ErrorCode::NotFound,
        };
        Self::new(code, error.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TabWindowView {
    pub(crate) window: u32,
    pub(crate) pane: u32,
    pub(crate) name: String,
}

impl From<TabWindow> for TabWindowView {
    fn from(window: TabWindow) -> Self {
        Self {
            window: window.window.0,
            pane: window.pane.0,
            name: window.name,
        }
    }
}

/// What changes in a tmux session shown as tabs.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub(crate) enum TopologyView {
    Window(TabWindowView),
    Renamed {
        window: u32,
        name: String,
    },
    Closed {
        window: u32,
    },
    /// Detached, the session gone, or the link lost.
    Ended {
        reason: Option<String>,
        lost: bool,
    },
}

struct ChannelTopology(Channel<TopologyView>);

impl TopologySink for ChannelTopology {
    fn send(&self, event: TopologyEvent) -> Result<(), SinkClosed> {
        let view = match event {
            TopologyEvent::Window(window) => TopologyView::Window(window.into()),
            TopologyEvent::Renamed { window, name } => TopologyView::Renamed {
                window: window.0,
                name,
            },
            TopologyEvent::Closed { window } => TopologyView::Closed { window: window.0 },
            TopologyEvent::Ended { reason, lost } => TopologyView::Ended { reason, lost },
        };
        self.0.send(view).map_err(|_| SinkClosed)
    }
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TmuxOpenedView {
    pub(crate) control: u32,
    pub(crate) windows: Vec<TabWindowView>,
}

fn control_id(control: u32) -> ControlId {
    ControlId::from(control)
}

/// Joins saved host `id`'s tmux session as tabs: the windows to show, and
/// what changes on `on_topology`. `unsupported` when the host's tmux cannot
/// (before 3.2, a split window, not UTF-8): the view joins it the plain
/// way instead.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::too_many_arguments,
    reason = "Tauri passes each IPC argument separately"
)]
pub(crate) async fn tmux_open(
    tmux: State<'_, Arc<TmuxSessions>>,
    broker: State<'_, Arc<PromptBroker>>,
    hosts: State<'_, crate::hosts::Hosts>,
    vault: State<'_, Arc<tauminus_app::vault::Vault>>,
    gate: State<'_, crate::sync::Gate>,
    forwards: State<'_, Arc<Forwards>>,
    id: String,
    size: GridSize,
    on_topology: Channel<TopologyView>,
    on_prompt: Channel<PromptRequest>,
) -> Result<TmuxOpenedView, CommandError> {
    let mark = tauminus_domain::HostId::parse(&id)
        .ok()
        .map(|host| forwards.host_mark(&host));
    let login = login(&hosts, &vault, &gate, &id).await?;
    // The host's settings decide, not the view.
    if !hosts
        .book()?
        .shows_tmux_tabs(&login.host)
        .await
        .unwrap_or(false)
    {
        return Err(CommandError::new(
            ErrorCode::Unsupported,
            "this host does not show its tmux windows as tabs",
        ));
    }
    // That setting was read after the login's check.
    gate.check(&hosts, &login.host).await?;
    let prompter = prompter(&broker, on_prompt);
    let Opened { control, windows } = tmux
        .open(
            &login,
            size.try_into()?,
            Arc::clone(&prompter),
            ChannelTopology(on_topology),
        )
        .await?;
    if let Some(mark) = mark {
        crate::forwards::start_with_host(&hosts, &gate, &forwards, login, mark, prompter);
    }
    Ok(TmuxOpenedView {
        control: u32::try_from(control)
            .map_err(|_| CommandError::new(ErrorCode::Internal, "control id space exhausted"))?,
        windows: windows.into_iter().map(Into::into).collect(),
    })
}

/// Shows `pane` of control client `control` in a session of its own.
#[tauri::command]
#[specta::specta]
pub(crate) async fn tmux_pane_attach(
    tmux: State<'_, Arc<TmuxSessions>>,
    control: u32,
    pane: u32,
    size: GridSize,
    on_output: Channel<OutputChunk>,
    on_exit: Channel<SessionExit>,
) -> Result<SessionHandle, CommandError> {
    let sink = ChannelSink {
        output: on_output,
        exit: on_exit,
    };
    let session = tmux
        .attach_pane(control_id(control), PaneId(pane), size.try_into()?, sink)
        .await?;
    SessionHandle::try_from(session)
}

/// A new tmux window; it comes back as a topology event.
#[tauri::command]
#[specta::specta]
pub(crate) async fn tmux_new_window(
    tmux: State<'_, Arc<TmuxSessions>>,
    control: u32,
) -> Result<(), CommandError> {
    Ok(tmux.new_window(control_id(control))?)
}

/// Leaves the tmux session running and stops showing it.
#[tauri::command]
#[specta::specta]
pub(crate) async fn tmux_detach(
    tmux: State<'_, Arc<TmuxSessions>>,
    control: u32,
) -> Result<(), CommandError> {
    Ok(tmux.detach(control_id(control))?)
}
