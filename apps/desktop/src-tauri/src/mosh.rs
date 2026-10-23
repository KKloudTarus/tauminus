// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Mosh sessions over IPC: a saved Mosh host, signed in to over SSH and
//! then carried by the local `mosh-client`.

use std::sync::Arc;

use tauminus_app::{MoshOpenError, MoshSessions, SshOpenError};
use tauri::State;
use tauri::ipc::Channel;

use crate::ipc::{
    ChannelSink, CommandError, ErrorCode, GridSize, OutputChunk, SessionExit, SessionHandle,
};
use crate::ssh::{PromptBroker, PromptRequest, login, prompter};

impl From<MoshOpenError> for CommandError {
    fn from(error: MoshOpenError) -> Self {
        match error {
            MoshOpenError::Session(error) => error.into(),
            MoshOpenError::Connect(error) => SshOpenError::Connect(error).into(),
            MoshOpenError::Routed => Self::new(ErrorCode::InvalidArgument, error.to_string()),
            MoshOpenError::Server(_) | MoshOpenError::Client(_) => {
                let mut message = error.to_string();
                let mut source = std::error::Error::source(&error);
                while let Some(cause) = source {
                    message.push_str(": ");
                    message.push_str(&cause.to_string());
                    source = cause.source();
                }
                Self::new(ErrorCode::LaunchFailed, message)
            }
        }
    }
}

/// Signs in to a saved Mosh host, starts `mosh-server` there, and streams
/// the local `mosh-client` to the view. Questions the sign-in raises
/// arrive on `on_prompt`, as for an SSH connect.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::too_many_arguments,
    reason = "Tauri commands take their state and channels as arguments"
)]
pub(crate) async fn mosh_connect(
    mosh: State<'_, MoshSessions>,
    broker: State<'_, Arc<PromptBroker>>,
    hosts: State<'_, crate::hosts::Hosts>,
    vault: State<'_, Arc<tauminus_app::vault::Vault>>,
    id: String,
    size: GridSize,
    on_output: Channel<OutputChunk>,
    on_exit: Channel<SessionExit>,
    on_prompt: Channel<PromptRequest>,
) -> Result<SessionHandle, CommandError> {
    let login = login(&hosts, &vault, &id).await?;
    let prompter = prompter(&broker, on_prompt);
    let sink = ChannelSink {
        output: on_output,
        exit: on_exit,
    };
    let session = mosh
        .open_saved(&login, size.try_into()?, sink, prompter)
        .await?;
    SessionHandle::try_from(session)
}

#[cfg(test)]
mod tests {
    use tauminus_app::MoshOpenError;
    use tauminus_domain::MoshError;

    use crate::ipc::{CommandError, ErrorCode};

    #[test]
    fn mosh_failures_say_what_to_fix() {
        let routed = CommandError::from(MoshOpenError::Routed);
        assert_eq!(routed.code, ErrorCode::InvalidArgument);
        let missing = CommandError::from(MoshOpenError::Server(MoshError::NoServer));
        assert_eq!(missing.code, ErrorCode::LaunchFailed);
        assert!(
            missing.message.contains("mosh-server is not installed"),
            "{}",
            missing.message
        );
        let client = CommandError::from(MoshOpenError::Client(
            tauminus_app::ports::LaunchError::Spawn {
                program: "mosh-client".into(),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "install Mosh"),
            },
        ));
        assert!(
            client.message.ends_with("install Mosh"),
            "{}",
            client.message
        );
    }
}
