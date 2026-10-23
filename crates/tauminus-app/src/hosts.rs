// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;

use tauminus_domain::{
    EnvVar, ForwardId, ForwardRule, Group, GroupId, Hop, Host, HostId, HostName, HostsError,
    IdentityId, Inventory, Label, Policy, Route, Settings, SnippetId, SshTarget, TelnetTarget,
    UserName, env_list,
};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::ports::{ConfigHost, HostStore, SshConfigSource, StoreError};

#[derive(Debug, Error)]
pub enum HostBookError {
    #[error(transparent)]
    Hosts(#[from] HostsError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("could not read ~/.ssh/config: {0}")]
    Config(std::io::Error),
}

/// What an import added and what it left out, with the reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub added: Vec<HostId>,
    pub skipped: Vec<(String, String)>,
}

/// Name of the group that imported hosts go into.
pub const IMPORT_GROUP: &str = "SSH config";

/// The saved hosts. Every change is saved before it becomes visible, so a
/// failed write leaves the list as it was. Changes are serialized; each
/// costs O(H + G) to copy and save, H ≤ 10 000.
pub struct HostBook {
    store: Arc<dyn HostStore>,
    inventory: Mutex<Inventory>,
}

impl std::fmt::Debug for HostBook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostBook").finish_non_exhaustive()
    }
}

impl HostBook {
    pub async fn open(store: Arc<dyn HostStore>) -> Result<Self, StoreError> {
        let inventory = store.load().await?;
        Ok(Self {
            store,
            inventory: Mutex::new(inventory),
        })
    }

    pub async fn snapshot(&self) -> Inventory {
        self.inventory.lock().await.clone()
    }

    pub fn new_host_id(&self) -> Result<HostId, HostsError> {
        HostId::parse(&self.store.new_id())
    }

    pub fn new_group_id(&self) -> Result<GroupId, HostsError> {
        GroupId::parse(&self.store.new_id())
    }

    pub fn new_forward_id(&self) -> Result<ForwardId, HostsError> {
        ForwardId::parse(&self.store.new_id())
    }

    /// A host's forwarding rules, as saved now. O(log H + r).
    pub async fn forward_rules(&self, id: &HostId) -> Result<Vec<ForwardRule>, HostsError> {
        let inventory = self.inventory.lock().await;
        let host = inventory
            .host(id)
            .ok_or_else(|| HostsError::NoSuchHost(id.clone()))?;
        Ok(host.forwards.clone())
    }

    /// A host's startup snippets, as saved now. O(log H + k).
    pub async fn startup(&self, id: &HostId) -> Result<Vec<SnippetId>, HostsError> {
        let inventory = self.inventory.lock().await;
        let host = inventory
            .host(id)
            .ok_or_else(|| HostsError::NoSuchHost(id.clone()))?;
        Ok(host.startup.clone())
    }

    /// Applies `change` to a copy, saves it, then publishes it. Returns the
    /// new inventory.
    pub async fn edit(
        &self,
        change: impl FnOnce(&mut Inventory) -> Result<(), HostsError>,
    ) -> Result<Inventory, HostBookError> {
        let mut current = self.inventory.lock().await;
        let mut next = current.clone();
        change(&mut next)?;
        match self.store.save(&next).await {
            Ok(()) => {}
            Err(StoreError::Conflict) => {
                // Take the other window's list, so the next try builds on it.
                *current = self.store.load().await?;
                return Err(StoreError::Conflict.into());
            }
            Err(e) => return Err(e.into()),
        }
        *current = next.clone();
        Ok(next)
    }

    pub async fn resolve(
        &self,
        id: &HostId,
        local_user: &UserName,
    ) -> Result<SshTarget, HostsError> {
        self.inventory.lock().await.resolve(id, local_user)
    }

    /// Where a saved Telnet host connects. O(d).
    pub async fn telnet_target(&self, id: &HostId) -> Result<TelnetTarget, HostsError> {
        self.inventory.lock().await.telnet_target(id)
    }

    /// Whether a host's commands go into the history. O(d).
    pub async fn records_history(&self, id: &HostId) -> Result<bool, HostsError> {
        self.inventory.lock().await.records_history(id)
    }

    /// The vault identity a host logs in with, if any. O(d).
    pub async fn identity(&self, id: &HostId) -> Result<Option<IdentityId>, HostsError> {
        self.inventory.lock().await.identity(id)
    }

    /// The prod guard for a host, read from the list as it is now, so a
    /// rule saved a moment ago applies to the next command. O(d·p).
    pub async fn guard_policy(&self, id: &HostId) -> Result<Policy, HostsError> {
        self.inventory.lock().await.guard_policy(id)
    }

    /// Adds the hosts `source` defines to the [`IMPORT_GROUP`] group, in one
    /// save. A host whose alias is already a saved name is skipped, so
    /// importing twice adds nothing. Aliases sharing an address stay apart:
    /// they often differ in user or jump host. O(H + n) for n entries.
    pub async fn import(
        &self,
        source: &dyn SshConfigSource,
    ) -> Result<(ImportReport, Inventory), HostBookError> {
        let entries = source.hosts().await.map_err(HostBookError::Config)?;
        let mut ids = Vec::with_capacity(entries.len() + 1);
        for _ in 0..=entries.len() {
            ids.push(self.new_host_id()?);
        }
        let group_id = self.new_group_id()?;
        let mut report = ImportReport::default();
        let inventory = self
            .edit(|inventory| {
                let existing = inventory
                    .groups()
                    .find(|g| g.name.as_str() == IMPORT_GROUP)
                    .map(|g| g.id.clone());
                let group = if let Some(id) = existing {
                    id
                } else {
                    inventory.put_group(Group {
                        id: group_id.clone(),
                        name: Label::parse(IMPORT_GROUP)?,
                        parent: None,
                        settings: Settings::default(),
                    })?;
                    group_id
                };
                let mut labels: std::collections::HashSet<String> = inventory
                    .hosts()
                    .map(|h| h.label.as_str().to_owned())
                    .collect();
                for (entry, id) in entries.into_iter().zip(ids) {
                    let alias = entry.alias.clone();
                    if let Some(problem) = entry.problem {
                        report.skipped.push((alias, problem));
                        continue;
                    }
                    match host_from_config(id, entry, &group) {
                        Ok(host) => {
                            if !labels.insert(host.label.as_str().to_owned()) {
                                report.skipped.push((alias, "already saved".into()));
                                continue;
                            }
                            let id = host.id.clone();
                            match inventory.put_host(host) {
                                Ok(()) => report.added.push(id),
                                // A full list keeps what fit and reports the rest.
                                Err(HostsError::TooMany) => {
                                    report.skipped.push((alias, "the host list is full".into()));
                                }
                                Err(e) => return Err(e),
                            }
                        }
                        Err(reason) => report.skipped.push((alias, reason.to_string())),
                    }
                }
                Ok(())
            })
            .await?;
        Ok((report, inventory))
    }
}

fn host_from_config(id: HostId, entry: ConfigHost, group: &GroupId) -> Result<Host, HostsError> {
    let address = HostName::parse(&entry.hostname)
        .map_err(|e| HostsError::Target(format!("HostName {:?}: {e}", entry.hostname)))?;
    let mut host = Host::new(id, Label::parse(&entry.alias)?, address);
    host.group = Some(group.clone());
    let settings = &mut host.settings;
    settings.user = entry
        .user
        .as_deref()
        .map(UserName::parse)
        .transpose()
        .map_err(|e| HostsError::Target(format!("User: {e}")))?;
    settings.port = entry.port.filter(|&p| p != 22);
    settings.jumps = entry
        .proxy_jump
        .as_deref()
        .map(Hop::parse_list)
        .transpose()?;
    settings.route = entry.proxy_command.map(Route::ProxyCommand);
    if !entry.identity_files.is_empty() {
        settings.identity_files = Some(entry.identity_files);
    }
    if entry.identities_only {
        settings.identities_only = Some(true);
    }
    settings.forward_agent = entry.forward_agent;
    let vars = entry
        .set_env
        .iter()
        .map(|(name, value)| EnvVar::parse(name, value))
        .collect::<Result<Vec<_>, _>>()?;
    settings.env = env_list(vars)?;
    Ok(host)
}

#[cfg(test)]
mod tests;
