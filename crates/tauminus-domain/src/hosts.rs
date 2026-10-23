// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Saved hosts, the groups that hold them, and the settings a host inherits
//! from its groups.
//!
//! A host sets what it needs and leaves the rest to its group, whose parent
//! groups fill in what it leaves open. The nearest value wins, as in an
//! `ssh_config` where the more specific block comes first.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use thiserror::Error;

use crate::forward::{ForwardError, ForwardRule, check_rules};
use crate::guard::{GuardSettings, PatternError, Policy};
use crate::ssh::{AuthMethod, AuthOptions, Endpoint, HostName, Route, SshTarget, UserName};
use crate::ssh::{EnvError, EnvVar, MAX_ENV, SessionOptions, env_list};

/// Deepest group nesting. Deeper trees are hard to read in a sidebar and
/// bound the cost of resolving a host.
pub const MAX_GROUP_DEPTH: usize = 16;
/// Most hosts in one inventory. Large fleets belong in an inventory source,
/// not a hand-kept list.
pub const MAX_HOSTS: usize = 10_000;
pub const MAX_GROUPS: usize = 1_000;
pub const MAX_TAGS: usize = 32;
/// The tag that marks a production host: a red frame, and the guard on
/// unless a setting turns it off.
pub const PROD_TAG: &str = "prod";
const MAX_ID: usize = 64;
const MAX_LABEL: usize = 128;
const MAX_TAG: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HostsError {
    #[error("id {0:?} must be 1 to 64 letters, digits, '-' or '_'")]
    Id(String),
    #[error("name must be 1 to 128 characters with no control characters")]
    Label,
    #[error("tag {0:?} must be 1 to 32 lowercase letters, digits, '-', '_' or '.'")]
    Tag(String),
    #[error("at most {MAX_TAGS} tags per host")]
    TooManyTags,
    #[error("no group {0}")]
    NoSuchGroup(GroupId),
    #[error("no host {0}")]
    NoSuchHost(HostId),
    #[error("{0} is listed twice")]
    Duplicate(String),
    #[error("group {0} would contain itself")]
    Cycle(GroupId),
    #[error("groups nest at most {MAX_GROUP_DEPTH} deep")]
    TooDeep,
    #[error("at most {MAX_HOSTS} hosts and {MAX_GROUPS} groups")]
    TooMany,
    #[error("invalid connection settings: {0}")]
    Target(String),
    #[error("prod guard: {0}")]
    Guard(#[from] PatternError),
    #[error("environment: {0}")]
    Env(#[from] EnvError),
    #[error("port forwarding: {0}")]
    Forward(#[from] ForwardError),
    #[error("at most {MAX_STARTUP} startup snippets per host, each once")]
    Startup,
}

/// Most startup snippets one host runs.
pub const MAX_STARTUP: usize = 8;

/// O(k²), k ≤ [`MAX_STARTUP`].
fn check_startup(startup: &[SnippetId]) -> Result<(), HostsError> {
    let repeated = startup
        .iter()
        .enumerate()
        .any(|(i, id)| startup[..i].contains(id));
    if startup.len() > MAX_STARTUP || repeated {
        return Err(HostsError::Startup);
    }
    Ok(())
}

macro_rules! id_type {
    ($name:ident, $what:literal) => {
        #[doc = concat!("Identifies one ", $what, ".")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// O(n) in the length of the id.
            pub fn parse(text: &str) -> Result<Self, HostsError> {
                let valid = !text.is_empty()
                    && text.len() <= MAX_ID
                    && text
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                if valid {
                    Ok(Self(text.to_owned()))
                } else {
                    Err(HostsError::Id(text.to_owned()))
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_type!(HostId, "saved host");
id_type!(GroupId, "group of hosts");
id_type!(ForwardId, "port forwarding rule");
id_type!(SnippetId, "saved command");
id_type!(
    IdentityId,
    "vault identity: a reusable user, password, and key"
);

/// A name shown to the user: 1 to 128 characters, no control characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Label(String);

impl Label {
    pub fn parse(text: &str) -> Result<Self, HostsError> {
        let text = text.trim();
        if text.is_empty() || text.chars().count() > MAX_LABEL || text.chars().any(char::is_control)
        {
            return Err(HostsError::Label);
        }
        Ok(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A tag such as `prod` or `eu-west`. Lowercase, so `Prod` and `prod` are one tag.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tag(String);

impl Tag {
    pub fn parse(text: &str) -> Result<Self, HostsError> {
        let tag = text.trim().to_ascii_lowercase();
        let valid = !tag.is_empty()
            && tag.len() <= MAX_TAG
            && tag.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.')
            });
        if valid {
            Ok(Self(tag))
        } else {
            Err(HostsError::Tag(text.to_owned()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A jump host as a host or group lists it. An omitted user or port is the
/// local user or 22, as for `ssh -J host`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hop {
    pub user: Option<UserName>,
    pub host: HostName,
    pub port: Option<u16>,
}

impl fmt::Display for Hop {
    /// The form [`Hop::parse`] reads back: `user@host:port`, with an IPv6
    /// address in brackets when a port follows.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(user) = &self.user {
            write!(f, "{user}@")?;
        }
        match (self.port, self.host.as_str().contains(':')) {
            (Some(port), true) => write!(f, "[{}]:{port}", self.host),
            (Some(port), false) => write!(f, "{}:{port}", self.host),
            (None, _) => write!(f, "{}", self.host),
        }
    }
}

impl Hop {
    /// Parses one hop as `ssh -J` and `ProxyJump` write it: `host`,
    /// `user@host`, `host:port`, `user@[2001:db8::1]:port`. O(n).
    pub fn parse(text: &str) -> Result<Self, HostsError> {
        let invalid = |e: &dyn fmt::Display| HostsError::Target(format!("jump host {text:?}: {e}"));
        let text = text.strip_prefix("ssh://").unwrap_or(text);
        let (user, rest) = match text.rsplit_once('@') {
            Some((user, rest)) => (Some(UserName::parse(user).map_err(|e| invalid(&e))?), rest),
            None => (None, text),
        };
        let (host, port) = if let Some(bracketed) = rest.strip_prefix('[') {
            let (host, after) = bracketed
                .split_once(']')
                .ok_or_else(|| invalid(&"missing ]"))?;
            let port = match after {
                "" => None,
                _ => Some(
                    after
                        .strip_prefix(':')
                        .ok_or_else(|| invalid(&"expected :port"))?,
                ),
            };
            (host, port)
        } else if rest.matches(':').count() == 1 {
            let (host, port) = rest.split_once(':').unwrap_or((rest, ""));
            (host, Some(port))
        } else {
            (rest, None)
        };
        let port = port
            .map(|p| {
                p.parse::<u16>()
                    .ok()
                    .filter(|&p| p > 0)
                    .ok_or_else(|| invalid(&"bad port"))
            })
            .transpose()?;
        Ok(Self {
            user,
            host: HostName::parse(host).map_err(|e| invalid(&e))?,
            port,
        })
    }

    /// Parses a comma-separated `ProxyJump` value. `none` means no jumps.
    pub fn parse_list(text: &str) -> Result<Vec<Self>, HostsError> {
        if text.trim().eq_ignore_ascii_case("none") {
            return Ok(Vec::new());
        }
        text.split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(Self::parse)
            .collect()
    }
}

/// Connection settings a host or group may set. `None` inherits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    pub user: Option<UserName>,
    pub port: Option<u16>,
    /// `Some(vec![])` means no jump hosts, overriding a group's.
    pub jumps: Option<Vec<Hop>>,
    pub route: Option<Route>,
    pub methods: Option<Vec<AuthMethod>>,
    pub identity_files: Option<Vec<PathBuf>>,
    pub identities_only: Option<bool>,
    /// Patterns add up from the host through every group; `enabled` is
    /// the nearest one set.
    pub guard: GuardSettings,
    /// `ssh -A` for this host; `None` inherits.
    pub forward_agent: Option<bool>,
    /// Variables sent before the shell starts, sorted by name, one per name.
    /// A host's value for a name wins over its groups'.
    pub env: Vec<EnvVar>,
    /// The vault identity to log in with; `None` inherits.
    pub identity: Option<IdentityId>,
    /// Whether commands run here go into the history; `None` inherits, and
    /// the root default is on.
    pub history: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub id: GroupId,
    pub name: Label,
    pub parent: Option<GroupId>,
    pub settings: Settings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub id: HostId,
    pub label: Label,
    pub address: HostName,
    pub group: Option<GroupId>,
    /// Sorted and without repeats; see [`Host::set_tags`].
    tags: Vec<Tag>,
    pub settings: Settings,
    /// Port forwarding rules. Each host keeps its own; groups have none.
    pub forwards: Vec<ForwardRule>,
    /// Snippets typed into each new shell on this host, in order. A
    /// snippet deleted since is skipped.
    pub startup: Vec<SnippetId>,
}

impl Host {
    pub fn new(id: HostId, label: Label, address: HostName) -> Self {
        Self {
            id,
            label,
            address,
            group: None,
            tags: Vec::new(),
            settings: Settings::default(),
            forwards: Vec::new(),
            startup: Vec::new(),
        }
    }

    pub fn tags(&self) -> &[Tag] {
        &self.tags
    }

    /// Sorts and removes repeats. O(t log t), t ≤ `MAX_TAGS`.
    pub fn set_tags(&mut self, mut tags: Vec<Tag>) -> Result<(), HostsError> {
        tags.sort();
        tags.dedup();
        if tags.len() > MAX_TAGS {
            return Err(HostsError::TooManyTags);
        }
        self.tags = tags;
        Ok(())
    }
}

/// Every saved host and group. Each change keeps four invariants: ids are
/// unique, every referenced group exists, groups form a forest no deeper
/// than [`MAX_GROUP_DEPTH`], and the counts stay under their limits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inventory {
    groups: BTreeMap<GroupId, Group>,
    hosts: BTreeMap<HostId, Host>,
}

impl Inventory {
    /// Builds an inventory, checking every invariant. O((H + G) log(H + G) + G·d).
    pub fn new(groups: Vec<Group>, hosts: Vec<Host>) -> Result<Self, HostsError> {
        if groups.len() > MAX_GROUPS || hosts.len() > MAX_HOSTS {
            return Err(HostsError::TooMany);
        }
        let mut inventory = Self::default();
        for group in groups {
            let id = group.id.clone();
            if inventory.groups.insert(id.clone(), group).is_some() {
                return Err(HostsError::Duplicate(format!("group {id}")));
            }
        }
        for group in inventory.groups.values() {
            check_settings(&group.settings)?;
            inventory.check_ancestry(&group.id)?;
        }
        for host in hosts {
            check_settings(&host.settings)?;
            check_rules(&host.forwards)?;
            check_startup(&host.startup)?;
            inventory.check_group(host.group.as_ref())?;
            let id = host.id.clone();
            if inventory.hosts.insert(id.clone(), host).is_some() {
                return Err(HostsError::Duplicate(format!("host {id}")));
            }
        }
        Ok(inventory)
    }

    pub fn hosts(&self) -> impl Iterator<Item = &Host> {
        self.hosts.values()
    }

    pub fn groups(&self) -> impl Iterator<Item = &Group> {
        self.groups.values()
    }

    pub fn host(&self, id: &HostId) -> Option<&Host> {
        self.hosts.get(id)
    }

    pub fn group(&self, id: &GroupId) -> Option<&Group> {
        self.groups.get(id)
    }

    /// Adds or replaces a host. O(log H).
    pub fn put_host(&mut self, host: Host) -> Result<(), HostsError> {
        check_settings(&host.settings)?;
        check_rules(&host.forwards)?;
        check_startup(&host.startup)?;
        self.check_group(host.group.as_ref())?;
        if !self.hosts.contains_key(&host.id) && self.hosts.len() >= MAX_HOSTS {
            return Err(HostsError::TooMany);
        }
        self.hosts.insert(host.id.clone(), host);
        Ok(())
    }

    pub fn remove_host(&mut self, id: &HostId) -> Result<Host, HostsError> {
        self.hosts
            .remove(id)
            .ok_or_else(|| HostsError::NoSuchHost(id.clone()))
    }

    /// Adds or replaces a group. Moving a group under one of its own
    /// descendants is refused. O(d) for the cycle and depth checks, plus
    /// O(G·d) when an existing group moves, since its subtree's depth
    /// changes.
    pub fn put_group(&mut self, group: Group) -> Result<(), HostsError> {
        check_settings(&group.settings)?;
        let id = group.id.clone();
        let existing = self.groups.get(&id).cloned();
        if existing.is_none() && self.groups.len() >= MAX_GROUPS {
            return Err(HostsError::TooMany);
        }
        let moved = existing
            .as_ref()
            .is_none_or(|old| old.parent != group.parent);
        self.groups.insert(id.clone(), group);
        let check = if moved {
            self.check_subtree(&id)
        } else {
            Ok(())
        };
        if let Err(error) = check {
            match existing {
                Some(old) => self.groups.insert(id, old),
                None => self.groups.remove(&id),
            };
            return Err(error);
        }
        Ok(())
    }

    /// Removes a group. Its hosts and child groups move to its parent and
    /// take over the settings they inherited from it, so removing a group
    /// never removes a host or changes how one connects. O(H + G).
    pub fn remove_group(&mut self, id: &GroupId) -> Result<Group, HostsError> {
        let group = self
            .groups
            .get(id)
            .ok_or_else(|| HostsError::NoSuchGroup(id.clone()))?;
        // Merged patterns and variables must fit before anything changes.
        let fits = |settings: &Settings| inherit(&mut settings.clone(), &group.settings);
        for child in self
            .groups
            .values()
            .filter(|g| g.parent.as_ref() == Some(id))
        {
            fits(&child.settings)?;
        }
        for host in self.hosts.values().filter(|h| h.group.as_ref() == Some(id)) {
            fits(&host.settings)?;
        }
        let group = self
            .groups
            .remove(id)
            .ok_or_else(|| HostsError::NoSuchGroup(id.clone()))?;
        for child in self.groups.values_mut() {
            if child.parent.as_ref() == Some(id) {
                child.parent.clone_from(&group.parent);
                inherit(&mut child.settings, &group.settings)?;
            }
        }
        for host in self.hosts.values_mut() {
            if host.group.as_ref() == Some(id) {
                host.group.clone_from(&group.parent);
                inherit(&mut host.settings, &group.settings)?;
            }
        }
        Ok(group)
    }

    /// The connection a host's settings and its groups' describe. Values
    /// come from the host first, then each group up the tree; an unset user
    /// is `local_user` and an unset port 22. Jump hosts and the route are
    /// one choice, how to reach the server, so both come from the nearest
    /// level that sets either. O(d) for d ≤ `MAX_GROUP_DEPTH`.
    pub fn resolve(&self, id: &HostId, local_user: &UserName) -> Result<SshTarget, HostsError> {
        let mut target = self.resolve_route(id, local_user)?;
        target.session = self.session(id)?;
        Ok(target)
    }

    /// [`Inventory::resolve`] without the session options: what the host
    /// list shows. O(d).
    pub fn resolve_route(
        &self,
        id: &HostId,
        local_user: &UserName,
    ) -> Result<SshTarget, HostsError> {
        let host = self
            .hosts
            .get(id)
            .ok_or_else(|| HostsError::NoSuchHost(id.clone()))?;
        let chain: Vec<&Settings> = std::iter::once(&host.settings)
            .chain(self.ancestry(host.group.as_ref()).map(|g| &g.settings))
            .collect();
        // The nearest setting that is set, for each field.
        macro_rules! nearest {
            ($field:ident) => {
                chain.iter().find_map(|s| s.$field.as_ref())
            };
        }
        let invalid = |e: &dyn fmt::Display| HostsError::Target(e.to_string());
        let user = nearest!(user).unwrap_or(local_user).clone();
        let port = nearest!(port).copied().unwrap_or(22);
        let destination =
            Endpoint::new(user, host.address.clone(), port).map_err(|e| invalid(&e))?;
        let path = chain
            .iter()
            .find(|s| s.jumps.is_some() || s.route.is_some());
        let jumps = path
            .and_then(|s| s.jumps.as_deref())
            .unwrap_or_default()
            .iter()
            .map(|hop| {
                let user = hop.user.clone().unwrap_or_else(|| local_user.clone());
                Endpoint::new(user, hop.host.clone(), hop.port.unwrap_or(22))
                    .map_err(|e| invalid(&e))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let route = path.and_then(|s| s.route.clone()).unwrap_or(Route::Direct);
        let defaults = AuthOptions::default();
        let auth = AuthOptions {
            methods: nearest!(methods).cloned().unwrap_or(defaults.methods),
            identity_files: nearest!(identity_files)
                .cloned()
                .unwrap_or(defaults.identity_files),
            identities_only: nearest!(identities_only)
                .copied()
                .unwrap_or(defaults.identities_only),
        };
        SshTarget::new(destination, jumps, route, auth).map_err(|e| invalid(&e))
    }

    /// Agent forwarding and the environment for a host. Forwarding is the
    /// nearest level's; variables add up from the root group down, a
    /// nearer level's value for a name winning. More than [`MAX_ENV`] in
    /// all is an error, as for one level. O(d·v) for v ≤ 32 per level.
    pub fn session(&self, id: &HostId) -> Result<SessionOptions, HostsError> {
        let chain = self.chain(id)?;
        let mut env: BTreeMap<&str, &EnvVar> = BTreeMap::new();
        for settings in chain.iter().rev() {
            for var in &settings.env {
                env.insert(var.name(), var);
            }
        }
        if env.len() > MAX_ENV {
            return Err(EnvError::TooMany.into());
        }
        Ok(SessionOptions {
            forward_agent: chain.iter().find_map(|s| s.forward_agent).unwrap_or(false),
            env: env.into_values().cloned().collect(),
        })
    }

    /// The vault identity a host logs in with: its own, or the nearest
    /// group's. O(d).
    pub fn identity(&self, id: &HostId) -> Result<Option<IdentityId>, HostsError> {
        Ok(self.chain(id)?.iter().find_map(|s| s.identity.clone()))
    }

    /// Whether a host forwards the agent, by its own setting or the nearest
    /// group's. O(d).
    pub fn forwards_agent(&self, id: &HostId) -> Result<bool, HostsError> {
        Ok(self
            .chain(id)?
            .iter()
            .find_map(|s| s.forward_agent)
            .unwrap_or(false))
    }

    /// Whether a host's commands are recorded: the nearest level's choice,
    /// on when none chooses. O(d).
    pub fn records_history(&self, id: &HostId) -> Result<bool, HostsError> {
        Ok(self
            .chain(id)?
            .iter()
            .find_map(|s| s.history)
            .unwrap_or(true))
    }

    /// A host's settings, then each group's up to the root.
    fn chain(&self, id: &HostId) -> Result<Vec<&Settings>, HostsError> {
        let host = self
            .hosts
            .get(id)
            .ok_or_else(|| HostsError::NoSuchHost(id.clone()))?;
        Ok(std::iter::once(&host.settings)
            .chain(self.ancestry(host.group.as_ref()).map(|g| &g.settings))
            .collect())
    }

    /// The prod guard for a host. It is on when the nearest level that
    /// says so turns it on, or, when none does, when the host is tagged
    /// `prod`. Patterns from every level apply. O(d·p) for p patterns.
    pub fn guard_policy(&self, id: &HostId) -> Result<Policy, HostsError> {
        let host = self
            .hosts
            .get(id)
            .ok_or_else(|| HostsError::NoSuchHost(id.clone()))?;
        let levels: Vec<&GuardSettings> = std::iter::once(&host.settings.guard)
            .chain(
                self.ancestry(host.group.as_ref())
                    .map(|g| &g.settings.guard),
            )
            .collect();
        let mut allow: Vec<_> = levels.iter().flat_map(|g| g.allow()).cloned().collect();
        let mut deny: Vec<_> = levels.iter().flat_map(|g| g.deny()).cloned().collect();
        for list in [&mut allow, &mut deny] {
            list.sort();
            list.dedup();
        }
        Ok(Policy {
            active: self.guarded(id)?,
            allow,
            deny,
        })
    }

    /// Whether the prod guard is on for a host, without collecting its
    /// patterns. O(d).
    pub fn guarded(&self, id: &HostId) -> Result<bool, HostsError> {
        let host = self
            .hosts
            .get(id)
            .ok_or_else(|| HostsError::NoSuchHost(id.clone()))?;
        let enabled = std::iter::once(&host.settings.guard)
            .chain(
                self.ancestry(host.group.as_ref())
                    .map(|g| &g.settings.guard),
            )
            .find_map(|g| g.enabled);
        Ok(enabled.unwrap_or_else(|| host.tags.iter().any(|t| t.as_str() == PROD_TAG)))
    }

    /// Groups from `start` up to the root. Bounded by the depth invariant.
    fn ancestry<'a>(&'a self, start: Option<&'a GroupId>) -> impl Iterator<Item = &'a Group> + 'a {
        std::iter::successors(start.and_then(|id| self.groups.get(id)), |group| {
            group.parent.as_ref().and_then(|id| self.groups.get(id))
        })
        .take(MAX_GROUP_DEPTH)
    }

    fn check_group(&self, group: Option<&GroupId>) -> Result<(), HostsError> {
        match group {
            Some(id) if !self.groups.contains_key(id) => Err(HostsError::NoSuchGroup(id.clone())),
            _ => Ok(()),
        }
    }

    /// Walks from `id` to the root: the parent must exist, the walk must not
    /// revisit a group, and it must end within the depth limit. O(d²) for
    /// d ≤ 16, from the visited list.
    fn check_ancestry(&self, id: &GroupId) -> Result<usize, HostsError> {
        let mut depth = 1;
        let mut visited = vec![id];
        let mut current = self.groups.get(id).and_then(|g| g.parent.as_ref());
        while let Some(parent) = current {
            if visited.contains(&parent) {
                return Err(HostsError::Cycle(parent.clone()));
            }
            visited.push(parent);
            let group = self
                .groups
                .get(parent)
                .ok_or_else(|| HostsError::NoSuchGroup(parent.clone()))?;
            depth += 1;
            if depth > MAX_GROUP_DEPTH {
                return Err(HostsError::TooDeep);
            }
            current = group.parent.as_ref();
        }
        Ok(depth)
    }

    /// Checks `id` and every group below it, whose depth moved with it.
    fn check_subtree(&self, id: &GroupId) -> Result<(), HostsError> {
        self.check_ancestry(id)?;
        for group in self.groups.values() {
            if self.ancestry(group.parent.as_ref()).any(|g| &g.id == id) {
                self.check_ancestry(&group.id)?;
            }
        }
        Ok(())
    }
}

/// Settings no level may hold: port 0, `ProxyCommand` together with jump
/// hosts, more jumps than a connect allows, or an environment list that is
/// unsorted, repeats a name, or is too long.
fn check_settings(settings: &Settings) -> Result<(), HostsError> {
    if settings.env.len() > MAX_ENV {
        return Err(EnvError::TooMany.into());
    }
    // Sorted by name, one per name, as `env_list` leaves them. O(v).
    if !settings.env.windows(2).all(|w| w[0].name() < w[1].name()) {
        return Err(HostsError::Target(
            "environment variables must be sorted and unique".into(),
        ));
    }
    let invalid = |message: &str| Err(HostsError::Target(message.to_owned()));
    if settings.port == Some(0) {
        return invalid("port must be between 1 and 65535");
    }
    let jumps = settings.jumps.as_ref().map_or(0, Vec::len);
    if jumps > SshTarget::MAX_JUMPS {
        return invalid("too many jump hosts");
    }
    if jumps > 0 && matches!(settings.route, Some(Route::ProxyCommand(_))) {
        return invalid("ProxyCommand and jump hosts cannot be combined");
    }
    Ok(())
}

/// Fills what `child` leaves unset from `parent`; jumps and route move as
/// one, and guard patterns add up.
fn inherit(child: &mut Settings, parent: &Settings) -> Result<(), HostsError> {
    macro_rules! fill {
        ($($field:ident),*) => {
            $(if child.$field.is_none() {
                child.$field.clone_from(&parent.$field);
            })*
        };
    }
    fill!(
        user,
        port,
        methods,
        identity_files,
        identities_only,
        forward_agent,
        identity,
        history
    );
    let mut env = child.env.clone();
    env.extend(parent.env.iter().cloned());
    // `env_list` keeps the child's value for a name both set.
    child.env = env_list(env)?;
    if child.jumps.is_none() && child.route.is_none() {
        child.jumps.clone_from(&parent.jumps);
        child.route.clone_from(&parent.route);
    }
    child.guard.inherit(&parent.guard)?;
    Ok(())
}

#[cfg(test)]
mod tests;
