// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The AI assistant: a command for what the user asks, or what went wrong
//! in a pane's output. Off until the user turns it on. See ADR 0022.
//!
//! The view names a session and nothing else. What the session printed
//! reaches a model only when this side finds, at the moment of sending,
//! that the session's saved host (or, for a local shell, the settings) lets
//! the assistant read it; the output comes from what this side kept, not
//! from the view. A loopback server gets the same rule: it may be a tunnel.
//!
//! An answer never runs. Its command, if it is one line of visible
//! characters, is checked against the host's prod guard with the built-in
//! rules on, whatever the host's guard says, and the view may put it on the
//! line without Enter.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};

use tauminus_domain::{
    AiEndpoint, AiError, AiPrompt, Ask, AskContext, Finding, Policy, SessionId, SessionKind,
    SessionMeta, answer, plain_output, prompt,
};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::hosts::HostBook;
use crate::ports::{AiSettingsStore, AiTransport, AiTransportError, Secret, StoreError};
use crate::session::{SessionError, SessionManager};
use crate::vault::{AiKey, Vault, VaultError};

/// Longest API key kept.
const MAX_KEY: usize = 4096;

/// The assistant's settings. The default is off, with no server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AiSettings {
    pub enabled: bool,
    pub endpoint: Option<AiEndpoint>,
    /// Local shells let the assistant read their output.
    pub local_output: bool,
    /// The origin the vault's key was saved for; `None` without a key, or
    /// when the server changed since.
    pub key_origin: Option<String>,
}

impl AiSettings {
    /// A key is saved for the server as it is now.
    pub fn has_key(&self) -> bool {
        self.key_origin.is_some()
            && self.key_origin.as_deref() == self.endpoint.as_ref().map(AiEndpoint::origin)
    }
}

/// The settings as the user edits them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AiSettingsInput {
    pub enabled: bool,
    /// Empty for no server.
    pub base_url: String,
    pub model: String,
    pub local_output: bool,
}

#[derive(Debug, Error)]
pub enum AssistantError {
    #[error("the AI assistant is off: turn it on in Settings")]
    Off,
    #[error("set the AI server's address and model first")]
    NoEndpoint,
    #[error("this session does not let the AI read its output: turn it on in the host's settings")]
    OutputNotAllowed,
    #[error("nothing to explain yet: the session has shown no output")]
    NoOutput,
    #[error("the AI is still answering in this pane")]
    Busy,
    #[error("that preview is gone: ask again")]
    NoPreview,
    #[error("the API key must be 1 to {MAX_KEY} visible characters")]
    Key,
    #[error("the key is for another address than the saved one: save the address first")]
    KeyOrigin,
    #[error("the AI server changed since the preview: ask again")]
    ServerChanged,
    #[error(transparent)]
    Ask(#[from] AiError),
    #[error(transparent)]
    Transport(#[from] AiTransportError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// A model's answer, safe to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    /// A command the view may put on the line, never run.
    pub command: Option<String>,
    /// Why the command needs a second look: the host's prod guard, with
    /// the built-in rules on.
    pub findings: Vec<Finding>,
}

/// The output an explanation would send, for the user to read first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub id: u32,
    pub text: String,
}

/// Asks a model for commands and explanations.
pub struct Assistant {
    sessions: Arc<SessionManager>,
    hosts: Arc<HostBook>,
    vault: Arc<Vault>,
    store: Arc<dyn AiSettingsStore>,
    transport: Arc<dyn AiTransport>,
    /// Loaded on first use.
    settings: Mutex<Option<AiSettings>>,
    /// Held across each change to the settings, which reads them, may
    /// await the vault, and writes them back.
    editing: Mutex<()>,
    /// Sessions with a request in flight: one each.
    busy: StdMutex<HashSet<SessionId>>,
    /// The sync review list, and the snippets it hashes: a host another
    /// device changed, unseen, lends nothing it printed.
    trust: Option<(Arc<crate::trust::Trust>, Option<Arc<crate::SnippetBook>>)>,
    /// The explanation each session's user was shown and may send, at most
    /// one per session.
    previews: StdMutex<HashMap<SessionId, Pending>>,
    next: AtomicU32,
}

impl std::fmt::Debug for Assistant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Assistant").finish_non_exhaustive()
    }
}

/// An explanation shown to the user, and the server it was shown going to.
struct Pending {
    id: u32,
    /// The base URL the user was shown.
    server: String,
    prompt: AiPrompt,
}

/// Takes a session off the busy list when its request ends, however.
struct Busy<'a>(&'a StdMutex<HashSet<SessionId>>, SessionId);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        lock(self.0).remove(&self.1);
    }
}

fn lock<T>(mutex: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Assistant {
    pub fn new(
        sessions: Arc<SessionManager>,
        hosts: Arc<HostBook>,
        vault: Arc<Vault>,
        store: Arc<dyn AiSettingsStore>,
        transport: Arc<dyn AiTransport>,
    ) -> Self {
        Self {
            sessions,
            hosts,
            vault,
            store,
            transport,
            settings: Mutex::new(None),
            editing: Mutex::new(()),
            trust: None,
            busy: StdMutex::default(),
            previews: StdMutex::default(),
            next: AtomicU32::new(1),
        }
    }

    /// The settings as saved.
    pub async fn settings(&self) -> Result<AiSettings, AssistantError> {
        let mut slot = self.settings.lock().await;
        if slot.is_none() {
            *slot = Some(self.store.load().await?);
        }
        Ok(slot.clone().unwrap_or_default())
    }

    /// Saves the settings. A new server address forgets the key, which was
    /// saved for the old one only.
    pub async fn configure(&self, input: AiSettingsInput) -> Result<AiSettings, AssistantError> {
        let endpoint = if input.base_url.trim().is_empty() {
            None
        } else {
            Some(AiEndpoint::parse(&input.base_url, &input.model)?)
        };
        if input.enabled && endpoint.is_none() {
            return Err(AssistantError::NoEndpoint);
        }
        let _editing = self.editing.lock().await;
        let current = self.settings().await?;
        let mut next = AiSettings {
            enabled: input.enabled,
            endpoint,
            local_output: input.local_output,
            key_origin: current.key_origin.clone(),
        };
        if current.key_origin.is_some() && !next.has_key() {
            next.key_origin = None;
            // The record is bound to its origin and never sent elsewhere;
            // removing it is tidiness, so a locked vault keeps it for now.
            match self.vault.save_ai_key(None).await {
                Ok(()) | Err(VaultError::Locked) => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.save(next).await
    }

    /// Saves the API key in the vault, for the server's origin only.
    /// `base_url` is the address the user saw the key go with; a key for
    /// an address not saved yet is refused, so it never goes to the old one.
    pub async fn set_key(&self, key: Secret, base_url: &str) -> Result<AiSettings, AssistantError> {
        let key_ok =
            !key.is_empty() && key.len() <= MAX_KEY && key.bytes().all(|b| b.is_ascii_graphic());
        if !key_ok {
            return Err(AssistantError::Key);
        }
        let _editing = self.editing.lock().await;
        let mut settings = self.settings().await?;
        let origin = settings
            .endpoint
            .as_ref()
            .ok_or(AssistantError::NoEndpoint)?
            .origin()
            .to_owned();
        let shown = AiEndpoint::parse(base_url, "m")?;
        if shown.origin() != origin {
            return Err(AssistantError::KeyOrigin);
        }
        self.vault
            .save_ai_key(Some(AiKey {
                origin: origin.clone(),
                key,
            }))
            .await?;
        settings.key_origin = Some(origin);
        self.save(settings).await
    }

    /// Forgets the API key.
    pub async fn clear_key(&self) -> Result<AiSettings, AssistantError> {
        let _editing = self.editing.lock().await;
        self.vault.save_ai_key(None).await?;
        let mut settings = self.settings().await?;
        settings.key_origin = None;
        self.save(settings).await
    }

    async fn save(&self, settings: AiSettings) -> Result<AiSettings, AssistantError> {
        let mut slot = self.settings.lock().await;
        self.store.save(&settings).await?;
        *slot = Some(settings.clone());
        Ok(settings)
    }

    /// A command for `request`, typed in session `id`. Sends the request,
    /// the kind of session, and this computer's OS family; nothing the
    /// session printed.
    pub async fn ask(&self, id: SessionId, request: &str) -> Result<Reply, AssistantError> {
        let (settings, endpoint) = self.ready().await?;
        let meta = self.sessions.meta(id)?;
        let prompt = prompt(
            &Ask::Command {
                request: request.to_owned(),
            },
            context(&meta),
        )?;
        self.send(id, &meta, &settings, &endpoint, &prompt).await
    }

    /// The output an explanation of session `id` would send, cleaned and
    /// with secrets hidden, kept for [`Assistant::explain`]. Refused, with
    /// nothing read, where the session does not let the assistant read it.
    pub async fn preview(&self, id: SessionId) -> Result<Preview, AssistantError> {
        let (settings, endpoint) = self.ready().await?;
        let meta = self.sessions.meta(id)?;
        if !self.reads_output(&meta, &settings).await {
            return Err(AssistantError::OutputNotAllowed);
        }
        let recent = self.sessions.recent(id)?;
        // More came than is kept: the first line may start mid-sequence.
        let cut = self.sessions.received(id)? > recent.len() as u64;
        let output = plain_output(&recent, cut);
        if output.is_empty() {
            return Err(AssistantError::NoOutput);
        }
        let prompt = prompt(
            &Ask::Explain {
                output: output.clone(),
            },
            context(&meta),
        )?;
        let preview = self.next.fetch_add(1, Ordering::Relaxed);
        let mut previews = lock(&self.previews);
        // A closed session's preview goes with it. O(P).
        previews.retain(|session, _| self.sessions.meta(*session).is_ok());
        previews.insert(
            id,
            Pending {
                id: preview,
                server: endpoint.base_url(),
                prompt,
            },
        );
        Ok(Preview {
            id: preview,
            text: output,
        })
    }

    /// Sends the explanation `preview` showed, if the session still lets
    /// the assistant read its output.
    pub async fn explain(&self, id: SessionId, preview: u32) -> Result<Reply, AssistantError> {
        let (settings, endpoint) = self.ready().await?;
        let meta = self.sessions.meta(id)?;
        let (server, prompt) = match lock(&self.previews).get(&id) {
            Some(pending) if pending.id == preview => {
                (pending.server.clone(), pending.prompt.clone())
            }
            _ => return Err(AssistantError::NoPreview),
        };
        // The user read where it goes: not to a server set since.
        if server != endpoint.base_url() {
            lock(&self.previews).remove(&id);
            return Err(AssistantError::ServerChanged);
        }
        // Asked again: the host's settings may have changed since.
        if !self.reads_output(&meta, &settings).await {
            lock(&self.previews).remove(&id);
            return Err(AssistantError::OutputNotAllowed);
        }
        let reply = self.send(id, &meta, &settings, &endpoint, &prompt).await;
        // Kept for another try when nothing went: the vault was locked, or
        // the pane was busy.
        if !matches!(reply, Err(AssistantError::Vault(_) | AssistantError::Busy)) {
            let mut previews = lock(&self.previews);
            if previews.get(&id).is_some_and(|p| p.id == preview) {
                previews.remove(&id);
            }
        }
        reply
    }

    async fn ready(&self) -> Result<(AiSettings, AiEndpoint), AssistantError> {
        let settings = self.settings().await?;
        if !settings.enabled {
            return Err(AssistantError::Off);
        }
        let endpoint = settings
            .endpoint
            .clone()
            .ok_or(AssistantError::NoEndpoint)?;
        Ok((settings, endpoint))
    }

    /// Whether the assistant may read what session `meta` printed, by the
    /// settings as they are now. A saved host's leave holds while the host
    /// is still saved with the address the session logged in to. O(log H +
    /// d).
    /// Holds back the output of hosts the sync review list holds.
    #[must_use]
    pub fn with_trust(
        mut self,
        trust: Arc<crate::trust::Trust>,
        snippets: Option<Arc<crate::SnippetBook>>,
    ) -> Self {
        self.trust = Some((trust, snippets));
        self
    }

    async fn reads_output(&self, meta: &SessionMeta, settings: &AiSettings) -> bool {
        match meta.kind {
            SessionKind::Local => settings.local_output,
            SessionKind::Ssh | SessionKind::Mosh => {
                let Some(saved) = &meta.host else {
                    return false;
                };
                self.hosts
                    .address(&saved.id)
                    .await
                    .is_some_and(|address| address == saved.login.host)
                    && self
                        .hosts
                        .sends_output_to_ai(&saved.id)
                        .await
                        .unwrap_or(false)
                    // Read before the check, as every use is.
                    && match &self.trust {
                        Some((trust, snippets)) => trust
                            .check(
                                &self.hosts,
                                snippets.as_deref(),
                                async |ids| {
                                    self.vault
                                        .identity_prints(Some(ids))
                                        .await
                                        .unwrap_or_default()
                                },
                                &saved.id,
                            )
                            .await
                            .is_ok(),
                        None => true,
                    }
            }
            SessionKind::Telnet | SessionKind::Serial => false,
        }
    }

    async fn send(
        &self,
        id: SessionId,
        meta: &SessionMeta,
        settings: &AiSettings,
        endpoint: &AiEndpoint,
        prompt: &AiPrompt,
    ) -> Result<Reply, AssistantError> {
        let _busy = {
            let mut busy = lock(&self.busy);
            if !busy.insert(id) {
                return Err(AssistantError::Busy);
            }
            Busy(&self.busy, id)
        };
        let key = self.key(settings, endpoint).await?;
        let raw = self
            .transport
            .complete(endpoint, key.as_ref(), prompt)
            .await?;
        let answer = answer(&raw);
        let findings = match &answer.command {
            Some(command) => self.check(meta, command).await,
            None => Vec::new(),
        };
        Ok(Reply {
            text: answer.text,
            command: answer.command,
            findings,
        })
    }

    /// The key saved for `endpoint`'s origin, if any. A key goes to the
    /// origin it was saved for and nowhere else.
    async fn key(
        &self,
        settings: &AiSettings,
        endpoint: &AiEndpoint,
    ) -> Result<Option<Secret>, AssistantError> {
        if !settings.has_key() {
            return Ok(None);
        }
        match self.vault.ai_key().await? {
            Some(saved) if saved.origin == endpoint.origin() => Ok(Some(saved.key)),
            Some(_) => Err(AssistantError::KeyOrigin),
            None => Ok(None),
        }
    }

    /// The host's prod guard with the built-in rules on, whether the host
    /// turned its guard on or not: the command came from a model that may
    /// have read what a hostile host printed.
    async fn check(&self, meta: &SessionMeta, command: &str) -> Vec<Finding> {
        let mut policy = match &meta.host {
            Some(saved) => self.hosts.guard_policy(&saved.id).await.unwrap_or_default(),
            None => Policy::default(),
        };
        policy.active = true;
        policy.check(command).findings
    }
}

fn context(meta: &SessionMeta) -> AskContext {
    AskContext {
        kind: meta.kind,
        os: std::env::consts::OS,
    }
}

#[cfg(test)]
mod tests;
