// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Logins to saved hosts that SFTP and forwards share, so they need no
//! second password or one-time code.
//!
//! SFTP and forwards acquire connections here; shells never do. A
//! connection is reused only while it is open and the host still resolves
//! to the target and identity it was opened for, and none of them
//! forwards the agent.
//!
//! Shells stay on connections of their own because russh returns a
//! channel's window when data arrives, not when it is read: a shell whose
//! view reads slowly would pace every forward and transfer on its
//! connection (ADR 0010).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use tauminus_domain::{
    Endpoint, HostId, HostsError, IdentityId, NameError, SavedHost, SshTarget, UserName,
};
use thiserror::Error;

use crate::hosts::HostBook;
use crate::ports::{ConnectError, Credentials, Prompter, SshConnection, SshConnector};
use crate::vault::{Vault, VaultError};

/// A saved host, resolved for one login.
#[derive(Debug, Clone)]
pub struct Login {
    pub host: HostId,
    pub target: SshTarget,
    pub identity: Option<IdentityId>,
    pub credentials: Arc<Credentials>,
}

impl Login {
    /// What a session opened with this login records of it.
    pub fn saved(&self) -> SavedHost {
        SavedHost {
            id: self.host.clone(),
            identity: self.identity.clone(),
            login: self.target.destination.clone(),
        }
    }
}

#[derive(Debug, Error)]
pub enum LoginError {
    #[error(transparent)]
    Hosts(#[from] HostsError),
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error(transparent)]
    Name(#[from] NameError),
}

impl Login {
    /// Resolves `host` with its groups' settings applied. A host with an
    /// identity needs the vault open, and the identity's user replaces the
    /// host's.
    pub async fn resolve(
        book: &HostBook,
        vault: &Vault,
        host: &HostId,
        local_user: &UserName,
    ) -> Result<Self, LoginError> {
        let mut target = book.resolve(host, local_user).await?;
        let identity = book.identity(host).await?;
        let credentials = match &identity {
            Some(identity) => vault.credentials(identity.as_str()).await?,
            None => Credentials::default(),
        };
        if let Some(user) = credentials.user.as_deref() {
            let destination = &target.destination;
            target.destination = Endpoint::new(
                UserName::parse(user)?,
                destination.host.clone(),
                destination.port,
            )?;
        }
        Ok(Self {
            host: host.clone(),
            target,
            identity,
            credentials: Arc::new(credentials),
        })
    }
}

struct Live {
    target: SshTarget,
    identity: Option<IdentityId>,
    connection: Weak<dyn SshConnection>,
}

impl Live {
    fn serves(&self, login: &Login) -> Option<Arc<dyn SshConnection>> {
        if self.target != login.target || self.identity != login.identity {
            return None;
        }
        self.connection.upgrade().filter(|c| c.is_open())
    }
}

/// The registry. Holds weak references only: a connection lives as long
/// as a shell, SFTP session, or forward holds it.
pub struct Connections {
    connector: Arc<dyn SshConnector>,
    live: Mutex<HashMap<HostId, Live>>,
    /// One per host, so two acquires at once share one login. At most one
    /// entry per saved host, which the inventory bounds.
    gates: Mutex<HashMap<HostId, Arc<tokio::sync::Mutex<()>>>>,
}

impl std::fmt::Debug for Connections {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connections").finish_non_exhaustive()
    }
}

impl Connections {
    pub fn new(connector: Arc<dyn SshConnector>) -> Self {
        Self {
            connector,
            live: Mutex::default(),
            gates: Mutex::default(),
        }
    }

    /// A live connection for `login`, or a new one without agent
    /// forwarding. O(1) besides the login itself.
    pub async fn acquire(
        &self,
        login: &Login,
        prompter: Arc<dyn Prompter>,
    ) -> Result<Arc<dyn SshConnection>, ConnectError> {
        let gate = Arc::clone(
            self.gates
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(login.host.clone())
                .or_default(),
        );
        let _one_login = gate.lock().await;
        if let Some(connection) = self.find(login) {
            return Ok(connection);
        }
        let connection = self
            .connector
            .connection(
                &login.target,
                prompter,
                Arc::clone(&login.credentials),
                false,
            )
            .await?;
        self.offer(login, &connection);
        Ok(connection)
    }

    /// Keeps `connection` for reuse; the newest one for a host wins.
    fn offer(&self, login: &Login, connection: &Arc<dyn SshConnection>) {
        self.lock().insert(
            login.host.clone(),
            Live {
                target: login.target.clone(),
                identity: login.identity.clone(),
                connection: Arc::downgrade(connection),
            },
        );
    }

    fn find(&self, login: &Login) -> Option<Arc<dyn SshConnection>> {
        let mut live = self.lock();
        let found = live.get(&login.host).and_then(|l| l.serves(login));
        if found.is_none() {
            // Gone, closed, or for an older version of the host.
            if live
                .get(&login.host)
                .is_some_and(|l| l.connection.strong_count() == 0)
            {
                live.remove(&login.host);
            }
        }
        found
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<HostId, Live>> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests;
