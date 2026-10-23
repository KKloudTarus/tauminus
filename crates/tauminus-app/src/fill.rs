// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Typing a saved host's login password at its password prompt, for `sudo`
//! or `su` on the host.
//!
//! The view never holds the password, and names nothing but the session.
//! The session recorded, when it opened, which saved host it logs in to,
//! with the vault identity and the login it used. A password goes only:
//!
//! - into that session, and not over Telnet, which carries it in the
//!   clear;
//! - while the host is still saved with that identity and address (an
//!   edited host does not send its new password to the old server);
//! - when the session's own last output, as the backend received it, is a
//!   login password prompt that names no other user or machine (a prompt
//!   from a second hop, `ops@other's password:`, gets nothing);
//! - once per prompt, and not after output the view had not yet parsed.
//!
//! Enter is left to the user: if the prompt was not one, and the host
//! shows what is typed, it is on the screen but has not run.

use std::sync::Arc;

use tauminus_domain::{PasswordPrompt, SavedHost, SessionId, SessionKind, password_prompt};
use thiserror::Error;

use crate::hosts::HostBook;
use crate::session::{SessionError, SessionManager};
use crate::vault::{Vault, VaultError};

#[derive(Debug, Error)]
pub enum FillError {
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error("this session is not a saved host's: no vault password goes to it")]
    NoHost,
    #[error("the host names no vault identity to log in with")]
    NoIdentity,
    #[error("the host's vault identity holds no password")]
    NoPassword,
    #[error("Telnet sends what is typed in the clear: no vault password goes over it")]
    Telnet,
    #[error("the host was edited since this session opened: connect again to use its password")]
    Changed,
    #[error("the session is not asking for this host's login password")]
    NoPrompt,
    #[error("the session showed more since the prompt was read: look again")]
    Stale,
}

/// What a pane may offer to type: the identity's name, or `None` for the
/// name while the vault is locked (the offer stands; typing unlocks
/// first), and the login it is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub identity: Option<String>,
    /// `user@address`.
    pub login: String,
}

/// Fills password prompts from the vault.
pub struct Fill {
    sessions: Arc<SessionManager>,
    hosts: Arc<HostBook>,
    vault: Arc<Vault>,
}

impl std::fmt::Debug for Fill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fill").finish_non_exhaustive()
    }
}

impl Fill {
    pub fn new(sessions: Arc<SessionManager>, hosts: Arc<HostBook>, vault: Arc<Vault>) -> Self {
        Self {
            sessions,
            hosts,
            vault,
        }
    }

    /// The saved host session `id` logs in to, still saved as it was, and
    /// the identity to type. O(log H + d).
    async fn saved(&self, id: SessionId) -> Result<(SavedHost, String), FillError> {
        let meta = self.sessions.meta(id)?;
        if meta.kind == SessionKind::Telnet {
            return Err(FillError::Telnet);
        }
        let saved = meta.host.ok_or(FillError::NoHost)?;
        let identity = saved.identity.clone().ok_or(FillError::NoIdentity)?;
        let now = self
            .hosts
            .identity(&saved.id)
            .await
            .map_err(|_| FillError::NoHost)?;
        let address = self
            .hosts
            .address(&saved.id)
            .await
            .ok_or(FillError::NoHost)?;
        if now.as_ref() != Some(&identity) || address != saved.login.host {
            return Err(FillError::Changed);
        }
        Ok((saved, identity.as_str().to_owned()))
    }

    /// The prompt session `id`'s output ends on, if it asks for `saved`'s
    /// login, and how much output came in all.
    fn prompt(&self, id: SessionId, saved: &SavedHost) -> Result<u64, FillError> {
        let (tail, received) = self.sessions.tail(id)?;
        let asks = password_prompt(&tail).is_some_and(|prompt: PasswordPrompt| {
            prompt.asks_for(saved.login.user.as_str(), saved.login.host.as_str())
        });
        if asks {
            Ok(received)
        } else {
            Err(FillError::NoPrompt)
        }
    }

    /// Whether session `id` may have a password typed into it now, and
    /// whose. Does not count as use of the vault: output from the host
    /// leads to it, and must not keep the vault open.
    pub async fn offer(&self, id: SessionId) -> Result<Offer, FillError> {
        let (saved, identity) = self.saved(id).await?;
        self.prompt(id, &saved)?;
        let login = format!(
            "{}@{}",
            saved.login.user.as_str(),
            saved.login.host.as_str()
        );
        match self.vault.peek(&identity).await {
            Ok(Some(found)) if found.has_password => Ok(Offer {
                identity: Some(found.name),
                login,
            }),
            Ok(Some(_)) => Err(FillError::NoPassword),
            Ok(None) => Err(FillError::NoIdentity),
            Err(VaultError::Locked) => Ok(Offer {
                identity: None,
                login,
            }),
            Err(error) => Err(error.into()),
        }
    }

    /// Types the login password of session `id`'s host, without Enter, if
    /// the view has parsed `seen` bytes of output and the session showed no
    /// more, and its output ends on a prompt for that login, not filled
    /// before.
    pub async fn type_password(&self, id: SessionId, seen: u64) -> Result<(), FillError> {
        let (saved, identity) = self.saved(id).await?;
        let received = self.prompt(id, &saved)?;
        if received > seen {
            return Err(FillError::Stale);
        }
        let password = self
            .vault
            .credentials(&identity)
            .await?
            .password
            .ok_or(FillError::NoPassword)?;
        // Checked again past the vault's await: output may have come.
        if self.prompt(id, &saved)? != received || !self.sessions.mark_filled(id, received)? {
            return Err(FillError::Stale);
        }
        self.sessions.write_secret(id, password).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
