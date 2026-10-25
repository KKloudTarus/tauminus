// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Hosts from cloud accounts (ADR 0032). A provider's listing becomes a
//! plan, shown to the user, then saved: hosts to add, hosts whose address
//! moved, hosts back in the account, hosts gone from it, and instances
//! skipped with the reason. A host found again keeps what the user set;
//! only its address follows the cloud.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use tauminus_domain::{
    GroupId, Host, HostId, HostName, HostsError, Inventory, Label, MAX_HOSTS, MAX_LABEL, MAX_TAGS,
    Origin, PROD_TAG, Tag, UserName,
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
    /// Instances in the CLI's answer that could not be read: while there
    /// are any, no host is marked gone.
    pub unreadable: usize,
    /// Parts not listed, a region say, with why: shown with the skipped.
    pub failed: Vec<(String, String)>,
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

/// The EC2 metadata service's IPv6 address.
const IMDS_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254);

/// Whether an address is one an import may give a host: an IP literal, as
/// every provider gives, and never loopback, `0.0.0.0/8`, multicast or
/// link-local (the metadata service's `169.254.169.254` among them) or
/// broadcast,
/// IPv4 inside IPv6 judged as IPv4.
fn usable(address: &str) -> Result<HostName, String> {
    let odd = || format!("{address} is not an address to connect to");
    let ip: IpAddr = address.parse().map_err(|_| odd())?;
    let v4 = |ip: Ipv4Addr| {
        ip.is_loopback()
            || ip.octets()[0] == 0
            || ip.is_multicast()
            || ip.is_link_local()
            || ip.is_broadcast()
    };
    let bad = match ip {
        IpAddr::V4(ip) => v4(ip),
        IpAddr::V6(ip) => match ip.to_ipv4() {
            // `::1` reads as `0.0.0.1`: loopback either way.
            Some(inner) => v4(inner) || ip.is_loopback(),
            None => {
                ip.is_unspecified()
                    || ip.is_multicast()
                    || (ip.segments()[0] & 0xffc0) == 0xfe80
                    || ip == IMDS_V6
            }
        },
    };
    if bad {
        return Err(odd());
    }
    HostName::parse(address).map_err(|e| e.to_string())
}

/// What importing `listing` into `inventory` would do. O(n + h) for n
/// instances and h saved hosts.
#[must_use]
pub fn plan(inventory: &Inventory, listing: &Listing, choice: Address) -> Plan {
    // Two devices that imported one account before they synced hold two
    // hosts of one origin: both follow it.
    let mut saved: HashMap<&Origin, Vec<&Host>> = HashMap::new();
    for host in inventory.hosts() {
        if let Some(origin) = &host.origin {
            saved.entry(origin).or_default().push(host);
        }
    }
    let labels: HashSet<&str> = inventory.hosts().map(|h| h.label.as_str()).collect();
    let mut taken = HashSet::new();
    let mut out = Plan::default();
    let mut new_labels = HashSet::new();
    for instance in &listing.instances {
        if !taken.insert(&instance.origin) {
            continue;
        }
        if let Some(hosts) = saved.get(&instance.origin) {
            // A saved host keeps the kind of address it was given: an
            // instance stopped loses its public one, and the private one
            // is no move.
            let address = match choice {
                Address::Public => instance.public.as_deref(),
                Address::Private => instance.private.as_deref(),
            }
            .and_then(|a| usable(a).ok());
            for host in hosts {
                if let Some(address) = &address
                    && host.address != *address
                {
                    out.moved.push((host.id.clone(), address.clone()));
                }
                if host.tags().iter().any(|t| t.as_str() == GONE_TAG) {
                    out.back.push(host.id.clone());
                }
            }
            continue;
        }
        let address = match new_address(instance, choice) {
            Ok(address) => address,
            Err(why) => {
                out.skipped.push((instance.name.clone(), why));
                continue;
            }
        };
        let Some(label) = free_label(instance, &labels, &new_labels) else {
            out.skipped
                .push((instance.name.clone(), "its name is taken".into()));
            continue;
        };
        new_labels.insert(label.as_str().to_owned());
        // `prod` is the user's to set (ADR 0032), and `cloud-gone` the
        // import's, which keeps a tag free for it.
        let mut tags: Vec<Tag> = instance
            .tags
            .iter()
            .filter(|t| !matches!(t.as_str(), PROD_TAG | GONE_TAG))
            .filter_map(|t| Tag::parse(t).ok())
            .collect();
        tags.sort();
        tags.dedup();
        tags.truncate(MAX_TAGS - 1);
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
    out.skipped.extend(listing.failed.iter().cloned());
    if listing.unreadable > 0 {
        out.skipped.push((
            format!("{} more", listing.unreadable),
            "the CLI's answer for them could not be read, so no host is marked gone".into(),
        ));
    }
    gone(&saved, &taken, listing, &mut out);
    out
}

/// A new host's address: the kind chosen, a public choice falling back
/// to the private address; else why there is none.
fn new_address(instance: &Instance, choice: Address) -> Result<HostName, String> {
    let candidates = match choice {
        Address::Public => [instance.public.as_deref(), instance.private.as_deref()],
        Address::Private => [instance.private.as_deref(), None],
    };
    let mut reason = None;
    for candidate in candidates.into_iter().flatten() {
        match usable(candidate) {
            Ok(address) => return Ok(address),
            Err(why) => {
                reason.get_or_insert(why);
            }
        }
    }
    Err(reason.unwrap_or_else(|| match choice {
        Address::Public => "it has no address".into(),
        Address::Private => "it has no private address".into(),
    }))
}

/// Saved hosts under a scope listed whole whose instance was not listed.
fn gone(
    saved: &HashMap<&Origin, Vec<&Host>>,
    listed: &HashSet<&Origin>,
    listing: &Listing,
    out: &mut Plan,
) {
    for (origin, hosts) in saved {
        let whole = listing.unreadable == 0
            && listing.whole.iter().any(|scope| {
                origin
                    .as_str()
                    .strip_prefix(scope.as_str())
                    .is_some_and(|rest| rest.starts_with([':', '/']))
            });
        if !whole || listed.contains(origin) {
            continue;
        }
        for host in hosts {
            if host.tags().iter().any(|t| t.as_str() == GONE_TAG) {
                continue;
            }
            // Never at the cost of a tag the user keeps.
            if host.tags().len() >= MAX_TAGS {
                out.skipped.push((
                    host.label.as_str().to_owned(),
                    format!("gone from the account, with no room for the {GONE_TAG} tag"),
                ));
            } else {
                out.gone.push(host.id.clone());
            }
        }
    }
    out.gone.sort();
}

/// The instance's name as a label, or with what tells it apart when
/// taken: its id, or for Azure, whose id ends in the name, its resource
/// group. The name is cut so the whole fits a label. `None` if both are
/// taken.
fn free_label(instance: &Instance, saved: &HashSet<&str>, new: &HashSet<String>) -> Option<Label> {
    let origin = instance.origin.as_str();
    let apart = match origin.strip_prefix("azure:") {
        Some(id) => id
            .split('/')
            .skip_while(|part| *part != "resourcegroups")
            .nth(1)
            .unwrap_or_default(),
        None => origin.rsplit([':', '/']).next().unwrap_or_default(),
    };
    let name = if instance.name.trim().is_empty() {
        apart.to_owned()
    } else {
        instance.name.trim().to_owned()
    };
    let suffix = format!(" ({apart})");
    let room = MAX_LABEL.saturating_sub(suffix.chars().count());
    let short: String = name.chars().take(room).collect();
    let free = |label: &str| !saved.contains(label) && !new.contains(label);
    [name, format!("{}{suffix}", short.trim_end())]
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
                // The plan marks only a host with room for the tag.
                if tags.len() >= MAX_TAGS {
                    continue;
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
    let mut count = inventory.hosts().count();
    for planned in plan.add {
        let name = planned.label.as_str().to_owned();
        if count >= MAX_HOSTS {
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
            Ok(()) => {
                report.added.push(id);
                count += 1;
            }
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
