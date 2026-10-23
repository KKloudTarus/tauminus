// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The AI assistant over IPC. The view names a session and types requests;
//! it never sends terminal output, and never gets the API key back.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauminus_app::ports::{AiTransportError, Secret};
use tauminus_app::{AiSettings, AiSettingsInput, Assistant, AssistantError, Reply};
use tauri::State;
use zeroize::Zeroize as _;

use crate::hosts::GuardFinding;
use crate::ipc::{CommandError, ErrorCode, SessionHandle};

/// The assistant, or why it is missing.
pub(crate) struct AiState(pub(crate) Result<Arc<Assistant>, String>);

impl AiState {
    fn assistant(&self) -> Result<&Assistant, CommandError> {
        self.0
            .as_deref()
            .map_err(|reason| CommandError::new(ErrorCode::Unavailable, reason.clone()))
    }
}

impl From<AssistantError> for CommandError {
    fn from(error: AssistantError) -> Self {
        let code = match error {
            AssistantError::Session(error) => return error.into(),
            AssistantError::Vault(error) => return error.into(),
            AssistantError::Off | AssistantError::OutputNotAllowed => ErrorCode::NotAllowed,
            AssistantError::NoEndpoint
            | AssistantError::Key
            | AssistantError::KeyOrigin
            | AssistantError::Ask(_) => ErrorCode::InvalidArgument,
            AssistantError::NoOutput => ErrorCode::NotFound,
            AssistantError::Busy => ErrorCode::LimitReached,
            AssistantError::NoPreview | AssistantError::ServerChanged => ErrorCode::Cancelled,
            AssistantError::Store(_) => ErrorCode::StoreFailed,
            AssistantError::Transport(
                AiTransportError::Unreachable(_) | AiTransportError::Timeout,
            ) => ErrorCode::ConnectFailed,
            AssistantError::Transport(AiTransportError::Unauthorized(_)) => ErrorCode::AuthFailed,
            AssistantError::Transport(
                AiTransportError::Status(_) | AiTransportError::Malformed(_),
            ) => ErrorCode::ProtocolError,
        };
        Self::new(code, error.to_string())
    }
}

/// The assistant's settings. The key is never shown, only whether one is
/// saved for the server as it is now.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent switches and facts the settings form shows"
)]
pub(crate) struct AiSettingsView {
    pub(crate) enabled: bool,
    pub(crate) base_url: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) local_output: bool,
    pub(crate) has_key: bool,
    /// The server is on a loopback address (it may still be a tunnel).
    pub(crate) loopback: bool,
}

impl From<AiSettings> for AiSettingsView {
    fn from(settings: AiSettings) -> Self {
        Self {
            enabled: settings.enabled,
            has_key: settings.has_key(),
            base_url: settings
                .endpoint
                .as_ref()
                .map(tauminus_domain::AiEndpoint::base_url),
            model: settings.endpoint.as_ref().map(|e| e.model().to_owned()),
            loopback: settings
                .endpoint
                .as_ref()
                .is_some_and(tauminus_domain::AiEndpoint::is_loopback),
            local_output: settings.local_output,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AiSettingsSpec {
    pub(crate) enabled: bool,
    /// Empty for no server.
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) local_output: bool,
}

/// A model's answer, as plain text.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AiReplyView {
    pub(crate) text: String,
    /// One line the view may put on the prompt line without Enter.
    pub(crate) command: Option<String>,
    /// The prod guard's reasons to look twice at `command`, with the
    /// built-in rules on for every host.
    pub(crate) findings: Vec<GuardFinding>,
}

impl From<Reply> for AiReplyView {
    fn from(reply: Reply) -> Self {
        Self {
            text: reply.text,
            command: reply.command,
            findings: reply
                .findings
                .into_iter()
                .map(|f| GuardFinding {
                    rule: f.rule.id().to_owned(),
                    reason: f.rule.reason().to_owned(),
                    command: f.command,
                })
                .collect(),
        }
    }
}

/// What an explanation would send, for the user to read before sending.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AiPreviewView {
    pub(crate) preview: u32,
    pub(crate) text: String,
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn ai_settings_get(
    ai: State<'_, AiState>,
) -> Result<AiSettingsView, CommandError> {
    Ok(ai.assistant()?.settings().await?.into())
}

/// Saves the settings. A new server address forgets the saved key.
#[tauri::command]
#[specta::specta]
pub(crate) async fn ai_settings_set(
    ai: State<'_, AiState>,
    settings: AiSettingsSpec,
) -> Result<AiSettingsView, CommandError> {
    let input = AiSettingsInput {
        enabled: settings.enabled,
        base_url: settings.base_url,
        model: settings.model,
        local_output: settings.local_output,
    };
    Ok(ai.assistant()?.configure(input).await?.into())
}

/// Saves the API key in the vault, for the server's address only;
/// `base_url` is the address the form shows, which must be the saved one.
#[tauri::command]
#[specta::specta]
pub(crate) async fn ai_key_set(
    ai: State<'_, AiState>,
    mut key: String,
    base_url: String,
) -> Result<AiSettingsView, CommandError> {
    let secret = Secret::new(key.clone());
    key.zeroize();
    Ok(ai.assistant()?.set_key(secret, &base_url).await?.into())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn ai_key_clear(ai: State<'_, AiState>) -> Result<AiSettingsView, CommandError> {
    Ok(ai.assistant()?.clear_key().await?.into())
}

/// A command for what the user typed in session `id`. Sends nothing the
/// session printed.
#[tauri::command]
#[specta::specta]
pub(crate) async fn ai_ask(
    ai: State<'_, AiState>,
    id: SessionHandle,
    request: String,
) -> Result<AiReplyView, CommandError> {
    Ok(ai.assistant()?.ask(id.into(), &request).await?.into())
}

/// The output an explanation of session `id` would send, read by the
/// backend from what the session showed; `not_allowed` where the session's
/// host does not let the assistant read it.
#[tauri::command]
#[specta::specta]
pub(crate) async fn ai_preview(
    ai: State<'_, AiState>,
    id: SessionHandle,
) -> Result<AiPreviewView, CommandError> {
    let preview = ai.assistant()?.preview(id.into()).await?;
    Ok(AiPreviewView {
        preview: preview.id,
        text: preview.text,
    })
}

/// Sends the explanation `preview` showed.
#[tauri::command]
#[specta::specta]
pub(crate) async fn ai_explain(
    ai: State<'_, AiState>,
    id: SessionHandle,
    preview: u32,
) -> Result<AiReplyView, CommandError> {
    Ok(ai.assistant()?.explain(id.into(), preview).await?.into())
}
