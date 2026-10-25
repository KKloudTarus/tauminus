// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Hosts from cloud accounts (ADR 0032). A provider's listing becomes a
//! plan, shown to the user, then saved: hosts to add, hosts whose address
//! moved, hosts back in the account, hosts gone from it, and instances
//! skipped with the reason. A host found again keeps what the user set;
//! only its address follows the cloud.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

use tauminus_domain::{
    GroupId, Host, HostId, HostName, HostsError, Inventory, Label, MAX_HOSTS, MAX_TAGS, Origin,
    Tag, UserName,
};

use crate::ports::BoxFuture;

/// The tag a host gets when its instance is no longer listed.
pub const GONE_TAG: &str = "cloud-gone";

/// One instance a provider listed, as its adapter read it: the name
/// cleaned of characters that change how it reads, nothing else trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub origin: Origin,
    pub name: String,
    pub public: Option<String>,
    pub private: Option<String>,
    /// The user the provider says logs in, where it says one.
    pub user: Option<String>,
    /// Candidate tags; those that are not valid tags are dropped.
    pub tags: Vec<String>,
    pub region: String,
}

/// What a provider listed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// The group for its hosts, as `Provider`, then the account.
    pub provider: String,
    pub account: String,
    pub instances: Vec<Instance>,
    /// Origin prefixes listed whole: a saved host under one of them that
    /// is not listed is gone. One that failed is not here.
    pub whole: Vec<String>,
}

/// Which address a new host takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Address {
    Public,
    Private,
}

/// A host to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub origin: Origin,
    pub label: Label,
    pub address: HostName,
    pub user: Option<UserName>,
    pub tags: Vec<Tag>,
    pub region: String,
}

/// What an import would do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub add: Vec<Planned>,
    /// Saved hosts whose address moved, with the new one.
    pub moved: Vec<(HostId, HostName)>,
    /// Saved hosts marked gone that are listed again.
    pub back: Vec<HostId>,
    /// Saved hosts no longer listed.
    pub gone: Vec<HostId>,
    /// Instances not taken, by name, with the reason.
    pub skipped: Vec<(String, String)>,
}

/// Which provider to ask, and the profile, subscription or project it is
/// asked for, when the user named one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudRequest {
    pub provider: Provider,
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Ec2,
    Lightsail,
    DigitalOcean,
    Azure,
    Gcp,
}

/// A provider's instances, read through its CLI (ADR 0032).
pub trait CloudInventory: Send + Sync {
    /// # Errors
    /// The CLI missing, failing, too slow, or saying too much.
    fn list<'a>(&'a self, request: &'a CloudRequest) -> BoxFuture<'a, Result<Listing, String>>;
}

/// Whether an address is one an import may give a host: never loopback,
/// unspecified, multicast or link-local (the metadata service's
/// `169.254.169.254` among them).
fn usable(address: &str) -> Result<HostName, String> {
    let name = HostName::parse(address).map_err(|e| e.to_string())?;
    if let Ok(ip) = address.parse::<IpAddr>() {
        let odd = match ip {
            IpAddr::V4(ip) => {
                ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || ip.is_link_local()
            }
            IpAddr::V6(ip) => {
                ip.is_loopback()
                    || ip.is_unspecified()
                    || ip.is_multicast()
                    || (ip.segments()[0] & 0xffc0) == 0xfe80
            }
        };
        if odd {
            return Err(format!("{address} is not an address to connect to"));
        }
    }
    Ok(name)
}

/// What importing `listing` into `inventory` would do. O(n + h) for n
/// instances and h saved hosts.
#[must_use]
pub fn plan(inventory: &Inventory, listing: &Listing, choice: Address) -> Plan {
    let saved: HashMap<&Origin, &Host> = inventory
        .hosts()
        .filter_map(|h| h.origin.as_ref().map(|o| (o, h)))
        .collect();
    let labels: HashSet<&str> = inventory.hosts().map(|h| h.label.as_str()).collect();
    let mut taken = HashSet::new();
    let mut out = Plan::default();
    let mut new_labels = Vec::new();
    for instance in &listing.instances {
        if !taken.insert(&instance.origin) {
            continue;
        }
        let address = match choice {
            Address::Public => instance.public.as_deref().or(instance.private.as_deref()),
            Address::Private => instance.private.as_deref(),
        };
        let address = match address.map(usable) {
            Some(Ok(address)) => address,
            Some(Err(reason)) => {
                out.skipped.push((instance.name.clone(), reason));
                continue;
            }
            None => {
                let which = if choice == Address::Private {
                    "private"
                } else {
                    "an"
                };
                out.skipped
                    .push((instance.name.clone(), format!("it has no {which} address")));
                continue;
            }
        };
        if let Some(host) = saved.get(&instance.origin) {
            if host.address != address {
                out.moved.push((host.id.clone(), address));
            }
            if host.tags().iter().any(|t| t.as_str() == GONE_TAG) {
                out.back.push(host.id.clone());
            }
            continue;
        }
        let Some(label) = free_label(instance, &labels, &new_labels) else {
            out.skipped
                .push((instance.name.clone(), "its name is taken".into()));
            continue;
        };
        new_labels.push(label.as_str().to_owned());
        let mut tags: Vec<Tag> = instance
            .tags
            .iter()
            .filter_map(|t| Tag::parse(t).ok())
            .collect();
        tags.sort();
        tags.dedup();
        tags.truncate(MAX_TAGS);
        out.add.push(Planned {
            origin: instance.origin.clone(),
            label,
            address,
            user: instance
                .user
                .as_deref()
                .and_then(|u| UserName::parse(u).ok()),
            tags,
            region: instance.region.clone(),
        });
    }
    for (origin, host) in &saved {
        let whole = listing.whole.iter().any(|scope| {
            origin
                .as_str()
                .strip_prefix(scope.as_str())
                .is_some_and(|rest| rest.starts_with(':'))
        });
        let tagged = host.tags().iter().any(|t| t.as_str() == GONE_TAG);
        if whole && !taken.contains(origin) && !tagged {
            out.gone.push(host.id.clone());
        }
    }
    out.gone.sort();
    out
}

/// The instance's name as a label, or with its id when taken; `None` if
/// both are.
fn free_label(instance: &Instance, saved: &HashSet<&str>, new: &[String]) -> Option<Label> {
    let id = instance
        .origin
        .as_str()
        .rsplit([':', '/'])
        .next()
        .unwrap_or_default();
    let name = if instance.name.trim().is_empty() {
        id.to_owned()
    } else {
        instance.name.trim().to_owned()
    };
    let free = |label: &str| !saved.contains(label) && !new.iter().any(|n| n == label);
    [name.clone(), format!("{name} ({id})")]
        .into_iter()
        .filter_map(|text| Label::parse(&text).ok())
        .find(|label| free(label.as_str()))
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudReport {
    pub added: Vec<HostId>,
    pub moved: usize,
    pub back: usize,
    pub gone: usize,
    pub skipped: Vec<(String, String)>,
}

/// Applies `plan` to `inventory`: new hosts under `Provider / account /
/// region` groups made from `fresh` ids. O(n + h + G).
///
/// # Errors
/// The inventory refusing a change other than a full list.
pub fn apply(
    inventory: &mut Inventory,
    listing: &Listing,
    plan: Plan,
    mut hosts: impl Iterator<Item = HostId>,
    mut fresh: impl Iterator<Item = GroupId>,
) -> Result<CloudReport, HostsError> {
    let mut report = CloudReport {
        skipped: plan.skipped,
        ..CloudReport::default()
    };
    for (id, address) in plan.moved {
        if let Some(mut host) = inventory.host(&id).cloned() {
            host.address = address;
            inventory.put_host(host)?;
            report.moved += 1;
        }
    }
    for (ids, gone) in [(plan.back, false), (plan.gone, true)] {
        for id in ids {
            let Some(mut host) = inventory.host(&id).cloned() else {
                continue;
            };
            let mut tags: Vec<Tag> = host
                .tags()
                .iter()
                .filter(|t| t.as_str() != GONE_TAG)
                .cloned()
                .collect();
            if gone {
                if tags.len() >= MAX_TAGS {
                    tags.pop();
                }
                tags.push(Tag::parse(GONE_TAG)?);
                report.gone += 1;
            } else {
                report.back += 1;
            }
            host.set_tags(tags)?;
            inventory.put_host(host)?;
        }
    }
    let mut groups: HashMap<String, GroupId> = HashMap::new();
    for planned in plan.add {
        let name = planned.label.as_str().to_owned();
        if inventory.hosts().count() >= MAX_HOSTS {
            report.skipped.push((name, "the host list is full".into()));
            continue;
        }
        let group = match region_group(inventory, listing, &planned.region, &mut groups, &mut fresh)
        {
            Ok(group) => group,
            Err(HostsError::TooMany) => {
                report.skipped.push((name, "the group list is full".into()));
                continue;
            }
            Err(e) => return Err(e),
        };
        let id = hosts.next().ok_or(HostsError::TooMany)?;
        let mut host = Host::new(id.clone(), planned.label, planned.address);
        host.origin = Some(planned.origin);
        host.group = Some(group);
        host.set_tags(planned.tags)?;
        host.settings.user = planned.user;
        match inventory.put_host(host) {
            Ok(()) => report.added.push(id),
            Err(HostsError::TooMany) => {
                report.skipped.push((name, "the host list is full".into()));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(report)
}

/// `Provider / account / region`, each made when missing.
fn region_group(
    inventory: &mut Inventory,
    listing: &Listing,
    region: &str,
    made: &mut HashMap<String, GroupId>,
    fresh: &mut impl Iterator<Item = GroupId>,
) -> Result<GroupId, HostsError> {
    if let Some(id) = made.get(region) {
        return Ok(id.clone());
    }
    let mut parent: Option<GroupId> = None;
    for name in [listing.provider.as_str(), listing.account.as_str(), region] {
        if name.is_empty() {
            continue;
        }
        let name = Label::parse(name)?;
        let found = inventory
            .groups()
            .find(|g| g.parent == parent && g.name == name)
            .map(|g| g.id.clone());
        let id = if let Some(id) = found {
            id
        } else {
            let id = fresh.next().ok_or(HostsError::TooMany)?;
            inventory.put_group(tauminus_domain::Group {
                id: id.clone(),
                name,
                parent: parent.clone(),
                settings: tauminus_domain::Settings::default(),
            })?;
            id
        };
        parent = Some(id);
    }
    let id = parent.ok_or(HostsError::TooMany)?;
    made.insert(region.to_owned(), id.clone());
    Ok(id)
}

#[cfg(test)]
mod tests;
