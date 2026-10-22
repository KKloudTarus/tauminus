// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The file format, version 1. Domain types stay free of serde; these types
//! translate at the boundary and validate everything they read.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauminus_domain::{
    AuthMethod, EnvVar, Group, GroupId, GuardSettings, Hop, Host, HostId, HostName, IdentityId,
    Inventory, Label, Pattern, Route, Settings, Tag, UserName,
};

/// Bump when a change would make an older build misread the file. An older
/// build refuses a newer file instead of dropping fields it does not know.
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    #[serde(default)]
    groups: Vec<GroupRecord>,
    #[serde(default)]
    hosts: Vec<HostRecord>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GroupRecord {
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
    #[serde(default, skip_serializing_if = "SettingsRecord::is_empty")]
    settings: SettingsRecord,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostRecord {
    id: String,
    label: String,
    address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(default, skip_serializing_if = "SettingsRecord::is_empty")]
    settings: SettingsRecord,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettingsRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    /// `ssh -J` form, one hop per entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    jumps: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    route: Option<RouteRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    methods: Option<Vec<MethodRecord>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity_files: Option<Vec<PathBuf>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identities_only: Option<bool>,
    #[serde(default, skip_serializing_if = "GuardRecord::is_empty")]
    guard: GuardRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    forward_agent: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    env: Vec<EnvRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    history: Option<bool>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvRecord {
    name: String,
    value: String,
}

/// The prod guard's settings. Patterns are command globs.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuardRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    deny: Vec<String>,
}

impl GuardRecord {
    fn is_empty(&self) -> bool {
        self.enabled.is_none() && self.allow.is_empty() && self.deny.is_empty()
    }
}

impl SettingsRecord {
    fn is_empty(&self) -> bool {
        self.guard.is_empty()
            && self.forward_agent.is_none()
            && self.identity.is_none()
            && self.history.is_none()
            && self.env.is_empty()
            && self.user.is_none()
            && self.port.is_none()
            && self.jumps.is_none()
            && self.route.is_none()
            && self.methods.is_none()
            && self.identity_files.is_none()
            && self.identities_only.is_none()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum RouteRecord {
    Direct,
    ProxyCommand { command: String },
    Socks5 { host: String, port: u16 },
    HttpConnect { host: String, port: u16 },
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum MethodRecord {
    Agent,
    PublicKey,
    KeyboardInteractive,
    Password,
}

pub(crate) fn to_json(inventory: &Inventory) -> Result<Vec<u8>, serde_json::Error> {
    let file = File {
        version: VERSION,
        groups: inventory
            .groups()
            .map(|g| GroupRecord {
                id: g.id.to_string(),
                name: g.name.as_str().to_owned(),
                parent: g.parent.as_ref().map(ToString::to_string),
                settings: settings_record(&g.settings),
            })
            .collect(),
        hosts: inventory
            .hosts()
            .map(|h| HostRecord {
                id: h.id.to_string(),
                label: h.label.as_str().to_owned(),
                address: h.address.to_string(),
                group: h.group.as_ref().map(ToString::to_string),
                tags: h.tags().iter().map(|t| t.as_str().to_owned()).collect(),
                settings: settings_record(&h.settings),
            })
            .collect(),
    };
    let mut bytes = serde_json::to_vec_pretty(&file)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub(crate) fn from_json(bytes: &[u8]) -> Result<Inventory, String> {
    let file: File = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if file.version != VERSION {
        return Err(format!(
            "it is version {}, and this build reads version {VERSION}; update Tauminus",
            file.version
        ));
    }
    let err = |what: &str, e: &dyn std::fmt::Display| format!("{what}: {e}");
    let groups = file
        .groups
        .into_iter()
        .map(|g| {
            Ok(Group {
                id: GroupId::parse(&g.id).map_err(|e| err("group id", &e))?,
                name: Label::parse(&g.name).map_err(|e| err("group name", &e))?,
                parent: g
                    .parent
                    .map(|p| GroupId::parse(&p))
                    .transpose()
                    .map_err(|e| err("group parent", &e))?,
                settings: settings(g.settings)?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let hosts = file
        .hosts
        .into_iter()
        .map(|h| {
            let mut host = Host::new(
                HostId::parse(&h.id).map_err(|e| err("host id", &e))?,
                Label::parse(&h.label).map_err(|e| err("host label", &e))?,
                HostName::parse(&h.address).map_err(|e| err("host address", &e))?,
            );
            host.group = h
                .group
                .map(|g| GroupId::parse(&g))
                .transpose()
                .map_err(|e| err("host group", &e))?;
            let tags = h
                .tags
                .iter()
                .map(|t| Tag::parse(t))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| err("tag", &e))?;
            host.set_tags(tags).map_err(|e| err("tags", &e))?;
            host.settings = settings(h.settings)?;
            Ok(host)
        })
        .collect::<Result<Vec<_>, String>>()?;
    Inventory::new(groups, hosts).map_err(|e| e.to_string())
}

fn settings_record(s: &Settings) -> SettingsRecord {
    SettingsRecord {
        user: s.user.as_ref().map(ToString::to_string),
        port: s.port,
        jumps: s
            .jumps
            .as_ref()
            .map(|hops| hops.iter().map(ToString::to_string).collect()),
        route: s.route.as_ref().map(|r| match r {
            Route::Direct => RouteRecord::Direct,
            Route::ProxyCommand(command) => RouteRecord::ProxyCommand {
                command: command.clone(),
            },
            Route::Socks5 { host, port } => RouteRecord::Socks5 {
                host: host.to_string(),
                port: *port,
            },
            Route::HttpConnect { host, port } => RouteRecord::HttpConnect {
                host: host.to_string(),
                port: *port,
            },
        }),
        methods: s.methods.as_ref().map(|methods| {
            methods
                .iter()
                .map(|m| match m {
                    AuthMethod::Agent => MethodRecord::Agent,
                    AuthMethod::PublicKey => MethodRecord::PublicKey,
                    AuthMethod::KeyboardInteractive => MethodRecord::KeyboardInteractive,
                    AuthMethod::Password => MethodRecord::Password,
                })
                .collect()
        }),
        identity_files: s.identity_files.clone(),
        identities_only: s.identities_only,
        guard: GuardRecord {
            enabled: s.guard.enabled,
            allow: s
                .guard
                .allow()
                .iter()
                .map(|p| p.as_str().to_owned())
                .collect(),
            deny: s
                .guard
                .deny()
                .iter()
                .map(|p| p.as_str().to_owned())
                .collect(),
        },
        forward_agent: s.forward_agent,
        identity: s.identity.as_ref().map(ToString::to_string),
        history: s.history,
        env: s
            .env
            .iter()
            .map(|v| EnvRecord {
                name: v.name().to_owned(),
                value: v.value().to_owned(),
            })
            .collect(),
    }
}

/// The variables as saved: valid, and no name twice.
fn env(records: &[EnvRecord]) -> Result<Vec<EnvVar>, String> {
    let vars = records
        .iter()
        .map(|v| EnvVar::parse(&v.name, &v.value))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("environment: {e}"))?;
    let sorted = tauminus_domain::env_list(vars).map_err(|e| format!("environment: {e}"))?;
    if sorted.len() < records.len() {
        return Err("environment: a variable is listed twice".into());
    }
    Ok(sorted)
}

fn guard(r: GuardRecord) -> Result<GuardSettings, String> {
    let patterns = |list: Vec<String>| {
        list.iter()
            .map(|p| Pattern::parse(p))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("guard pattern: {e}"))
    };
    GuardSettings::new(r.enabled, patterns(r.allow)?, patterns(r.deny)?)
        .map_err(|e| format!("guard: {e}"))
}

fn settings(r: SettingsRecord) -> Result<Settings, String> {
    let err = |what: &str, e: &dyn std::fmt::Display| format!("{what}: {e}");
    let host = |h: &str| HostName::parse(h).map_err(|e| err("proxy host", &e));
    Ok(Settings {
        user: r
            .user
            .map(|u| UserName::parse(&u))
            .transpose()
            .map_err(|e| err("user", &e))?,
        port: match r.port {
            Some(0) => return Err("port 0".into()),
            port => port,
        },
        jumps: r
            .jumps
            .map(|hops| {
                hops.iter()
                    .map(|h| Hop::parse(h))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()
            .map_err(|e| err("jump", &e))?,
        route: r
            .route
            .map(|route| {
                Ok::<_, String>(match route {
                    RouteRecord::Direct => Route::Direct,
                    RouteRecord::ProxyCommand { command } => Route::ProxyCommand(command),
                    RouteRecord::Socks5 { host: h, port } => Route::Socks5 {
                        host: host(&h)?,
                        port,
                    },
                    RouteRecord::HttpConnect { host: h, port } => Route::HttpConnect {
                        host: host(&h)?,
                        port,
                    },
                })
            })
            .transpose()?,
        methods: r.methods.map(|methods| {
            methods
                .into_iter()
                .map(|m| match m {
                    MethodRecord::Agent => AuthMethod::Agent,
                    MethodRecord::PublicKey => AuthMethod::PublicKey,
                    MethodRecord::KeyboardInteractive => AuthMethod::KeyboardInteractive,
                    MethodRecord::Password => AuthMethod::Password,
                })
                .collect()
        }),
        identity_files: r.identity_files,
        identities_only: r.identities_only,
        guard: guard(r.guard)?,
        forward_agent: r.forward_agent,
        history: r.history,
        identity: r
            .identity
            .map(|i| IdentityId::parse(&i))
            .transpose()
            .map_err(|e| format!("identity: {e}"))?,
        env: env(&r.env)?,
    })
}
