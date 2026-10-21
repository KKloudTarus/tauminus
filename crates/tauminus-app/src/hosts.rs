// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;

use tauminus_domain::{
    Group, GroupId, Hop, Host, HostId, HostName, HostsError, Inventory, Label, Route, Settings,
    SshTarget, UserName,
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
    Ok(host)
}

#[cfg(test)]
mod tests;
