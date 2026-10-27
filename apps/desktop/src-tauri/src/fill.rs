// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Typing a saved host's login password at a password prompt, over IPC.
//! The view names the session only: the backend finds its host, the host's
//! login identity, and the password, and never sends the password back.

use std::sync::Arc;

use serde::Serialize;
use specta::Type;
use tauminus_app::{Fill, FillError};
use tauri::State;

use crate::ipc::{CommandError, ErrorCode, SessionHandle};

/// The use case, when saved hosts could be read.
pub(crate) struct FillState(pub(crate) Option<Arc<Fill>>);

impl FillState {
    fn fill(&self) -> Result<&Fill, CommandError> {
        self.0.as_deref().ok_or_else(|| {
            CommandError::new(ErrorCode::Unavailable, "saved hosts could not be read")
        })
    }
}

impl From<FillError> for CommandError {
    fn from(error: FillError) -> Self {
        match error {
            FillError::Session(error) => error.into(),
            FillError::Vault(error) => error.into(),
            FillError::Trust(error) => error.into(),
            FillError::Stale | FillError::NoPrompt => {
                Self::new(ErrorCode::Cancelled, error.to_string())
            }
            FillError::NoHost | FillError::NoIdentity => {
                Self::new(ErrorCode::NotFound, error.to_string())
            }
            FillError::NoPassword | FillError::Telnet | FillError::Changed => {
                Self::new(ErrorCode::InvalidArgument, error.to_string())
            }
            FillError::Secret(_) => Self::new(ErrorCode::AuthFailed, error.to_string()),
        }
    }
}

/// What a pane may offer at a password prompt.
#[derive(Debug, Clone, Serialize, Type)]
pub(crate) struct FillOfferView {
    /// The identity's name; `null` while the vault is locked.
    pub(crate) identity: Option<String>,
    /// The login it is for: `user@address`.
    pub(crate) login: String,
}

/// Whether session `id`'s saved host has a login password to type, and
/// whose; `null` when it has none, or the session is not a saved host's.
#[tauri::command]
#[specta::specta]
pub(crate) async fn session_fill_offer(
    fill: State<'_, FillState>,
    id: SessionHandle,
) -> Result<Option<FillOfferView>, CommandError> {
    match fill.fill()?.offer(id.into()).await {
        Ok(offer) => Ok(Some(FillOfferView {
            identity: offer.identity,
            login: offer.login,
        })),
        Err(
            FillError::NoHost
            | FillError::NoIdentity
            | FillError::NoPassword
            | FillError::Telnet
            | FillError::Changed
            | FillError::NoPrompt,
        ) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Types session `id`'s host's login password, without Enter. `seen` is
/// the output the view had parsed when it read the prompt, in bytes; if
/// the session showed more since, nothing is typed (`cancelled`).
#[tauri::command]
#[specta::specta]
pub(crate) async fn session_fill_password(
    fill: State<'_, FillState>,
    id: SessionHandle,
    seen: f64,
) -> Result<(), CommandError> {
    if !(seen.is_finite() && seen >= 0.0) {
        return Err(CommandError::new(
            ErrorCode::InvalidArgument,
            "output seen is a count of bytes",
        ));
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked non-negative and finite; bytes stay far below 2^53"
    )]
    let seen = seen as u64;
    Ok(fill.fill()?.type_password(id.into(), seen).await?)
}
