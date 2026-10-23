// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Saved hosts over IPC. Each change returns the whole list, so the view
//! never holds a stale copy; a list is at most 10 000 hosts.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauminus_app::{Forwards, HostBook, HostBookError};
use tauminus_domain::{
    Encoding, EnvError, EnvVar, Group, GroupId, GuardSettings, Hop, Host, HostId, HostName,
    HostsError, IdentityId, Inventory, Label, MAX_ENV, MAX_PATTERNS, Pattern, Route, Settings,
    SnippetId, Tag, UserName, env_list,
};
use tauminus_ssh::OpenSshConfig;
use tauri::State;

use crate::forwards::{ForwardSpec, parse_rule, stale_rules};
use crate::ipc::{CommandError, ErrorCode};
use crate::ssh::{AuthMethodSpec, RouteSpec, SshTargetSpec, local_user};

/// The host book, or why it could not be opened. A damaged file is kept
/// as it is and reported; the rest of the app works without saved hosts.
pub(crate) enum Hosts {
    Ready(Arc<HostBook>),
    Unavailable(String),
}

impl Hosts {
    pub(crate) fn book(&self) -> Result<&HostBook, CommandError> {
        match self {
            Self::Ready(book) => Ok(book),
            Self::Unavailable(reason) => {
                Err(CommandError::new(ErrorCode::StoreFailed, reason.clone()))
            }
        }
    }
}

/// The user's OpenSSH config, or `None` when there is no home directory.
pub(crate) struct SshConfig(pub(crate) Option<OpenSshConfig>);

/// Settings as the view edits them. Every field is optional; a missing one
/// inherits from the group.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SettingsSpec {
    #[serde(default)]
    pub(crate) user: Option<String>,
    #[serde(default)]
    pub(crate) port: Option<u16>,
    /// `ssh -J` form, one hop each. An empty list means no jump hosts.
    #[serde(default)]
    pub(crate) jumps: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) route: Option<RouteSpec>,
    #[serde(default)]
    pub(crate) methods: Option<Vec<AuthMethodSpec>>,
    #[serde(default)]
    pub(crate) identity_files: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) identities_only: Option<bool>,
    #[serde(default)]
    pub(crate) guard: Option<GuardSpec>,
    /// `ssh -A`; `null` inherits.
    #[serde(default)]
    pub(crate) forward_agent: Option<bool>,
    /// Record commands in the history; `null` inherits (on at the root).
    #[serde(default)]
    pub(crate) history: Option<bool>,
    /// Variables sent before the shell starts. A host's value for a name
    /// wins over its groups'.
    #[serde(default)]
    pub(crate) env: Option<Vec<EnvSpec>>,
    /// The vault identity to log in with; `null` inherits.
    #[serde(default)]
    pub(crate) identity: Option<String>,
    /// The shell's character set; `null` inherits (UTF-8 at the root).
    #[serde(default)]
    pub(crate) encoding: Option<EncodingSpec>,
}

/// A character set a host's shell may speak, by its WHATWG name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub(crate) enum EncodingSpec {
    #[serde(rename = "UTF-8")]
    Utf8,
    #[serde(rename = "windows-1252")]
    Windows1252,
    #[serde(rename = "windows-1251")]
    Windows1251,
    #[serde(rename = "KOI8-R")]
    Koi8R,
    #[serde(rename = "ISO-8859-2")]
    Iso8859_2,
    #[serde(rename = "Shift_JIS")]
    ShiftJis,
    #[serde(rename = "EUC-JP")]
    EucJp,
    #[serde(rename = "gb18030")]
    Gb18030,
    #[serde(rename = "Big5")]
    Big5,
    #[serde(rename = "EUC-KR")]
    EucKr,
}

impl From<EncodingSpec> for Encoding {
    fn from(spec: EncodingSpec) -> Self {
        match spec {
            EncodingSpec::Utf8 => Self::Utf8,
            EncodingSpec::Windows1252 => Self::Windows1252,
            EncodingSpec::Windows1251 => Self::Windows1251,
            EncodingSpec::Koi8R => Self::Koi8R,
            EncodingSpec::Iso8859_2 => Self::Iso8859_2,
            EncodingSpec::ShiftJis => Self::ShiftJis,
            EncodingSpec::EucJp => Self::EucJp,
            EncodingSpec::Gb18030 => Self::Gb18030,
            EncodingSpec::Big5 => Self::Big5,
            EncodingSpec::EucKr => Self::EucKr,
        }
    }
}

impl From<Encoding> for EncodingSpec {
    fn from(encoding: Encoding) -> Self {
        match encoding {
            Encoding::Utf8 => Self::Utf8,
            Encoding::Windows1252 => Self::Windows1252,
            Encoding::Windows1251 => Self::Windows1251,
            Encoding::Koi8R => Self::Koi8R,
            Encoding::Iso8859_2 => Self::Iso8859_2,
            Encoding::ShiftJis => Self::ShiftJis,
            Encoding::EucJp => Self::EucJp,
            Encoding::Gb18030 => Self::Gb18030,
            Encoding::Big5 => Self::Big5,
            Encoding::EucKr => Self::EucKr,
        }
    }
}

/// One environment variable, as OpenSSH `SetEnv NAME=value`.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub(crate) struct EnvSpec {
    pub(crate) name: String,
    pub(crate) value: String,
}

/// The prod guard's settings on a host or group.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GuardSpec {
    /// `null` inherits; with no level setting it, hosts tagged `prod` are
    /// guarded.
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
    /// Command globs the built-in rules never ask about.
    #[serde(default)]
    pub(crate) allow: Vec<String>,
    /// Command globs that always ask.
    #[serde(default)]
    pub(crate) deny: Vec<String>,
}

/// Why a command line needs confirmation, empty when it may run.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GuardVerdict {
    pub(crate) findings: Vec<GuardFinding>,
    /// The shell waits for another line before running this one.
    pub(crate) incomplete: bool,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GuardFinding {
    /// A stable rule name such as `recursive-delete`.
    pub(crate) rule: String,
    pub(crate) reason: String,
    /// The command as the shell would run it, at most 200 characters.
    pub(crate) command: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroupView {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) parent: Option<String>,
    pub(crate) settings: SettingsSpec,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HostView {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) address: String,
    pub(crate) group: Option<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) settings: SettingsSpec,
    /// What a connect would use, with the group settings applied, or `null`
    /// when the settings cannot connect; `problem` then says why.
    pub(crate) target: Option<SshTargetSpec>,
    pub(crate) problem: Option<String>,
    /// Commands typed into this host are checked by the prod guard.
    pub(crate) guarded: bool,
    /// The agent is forwarded, by the host's setting or a group's.
    pub(crate) forwards_agent: bool,
    pub(crate) forwards: Vec<ForwardSpec>,
    /// Ids of the snippets typed into each new shell, in order.
    pub(crate) startup: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Type)]
pub(crate) struct HostsView {
    pub(crate) groups: Vec<GroupView>,
    pub(crate) hosts: Vec<HostView>,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HostInput {
    /// Absent for a new host.
    #[serde(default)]
    pub(crate) id: Option<String>,
    pub(crate) label: String,
    pub(crate) address: String,
    #[serde(default)]
    pub(crate) group: Option<String>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    #[serde(default)]
    pub(crate) settings: SettingsSpec,
    /// Absent keeps the rules saved now.
    #[serde(default)]
    pub(crate) forwards: Option<Vec<ForwardSpec>>,
    /// Absent keeps the startup snippets saved now.
    #[serde(default)]
    pub(crate) startup: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroupInput {
    #[serde(default)]
    pub(crate) id: Option<String>,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) parent: Option<String>,
    #[serde(default)]
    pub(crate) settings: SettingsSpec,
}

#[derive(Debug, Clone, Serialize, Type)]
pub(crate) struct SkippedHost {
    pub(crate) alias: String,
    pub(crate) reason: String,
}

#[derive(Debug, Clone, Serialize, Type)]
pub(crate) struct ImportResult {
    pub(crate) added: u32,
    pub(crate) skipped: Vec<SkippedHost>,
    pub(crate) view: HostsView,
}

impl From<HostBookError> for CommandError {
    fn from(error: HostBookError) -> Self {
        let code = match &error {
            HostBookError::Hosts(_) => ErrorCode::InvalidArgument,
            HostBookError::Store(_) | HostBookError::Config(_) => ErrorCode::StoreFailed,
        };
        Self::new(code, error.to_string())
    }
}

fn invalid(error: impl std::fmt::Display) -> CommandError {
    CommandError::new(ErrorCode::InvalidArgument, error.to_string())
}

fn view(inventory: &Inventory) -> HostsView {
    let me = local_user().ok().and_then(|u| UserName::parse(&u).ok());
    HostsView {
        groups: inventory
            .groups()
            .map(|g| GroupView {
                id: g.id.to_string(),
                name: g.name.as_str().to_owned(),
                parent: g.parent.as_ref().map(ToString::to_string),
                settings: settings_spec(&g.settings),
            })
            .collect(),
        hosts: inventory
            .hosts()
            .map(|h| {
                let resolved = me
                    .as_ref()
                    .ok_or_else(|| "the local user name is unknown".to_owned())
                    .and_then(|me| {
                        inventory
                            .resolve_route(&h.id, me)
                            .map_err(|e| e.to_string())
                    });
                let forwards_agent = inventory.forwards_agent(&h.id).unwrap_or(false);
                let (target, problem) = match resolved {
                    Ok(target) => (Some(SshTargetSpec::from(&target)), None),
                    Err(problem) => (None, Some(problem)),
                };
                HostView {
                    id: h.id.to_string(),
                    label: h.label.as_str().to_owned(),
                    address: h.address.to_string(),
                    group: h.group.as_ref().map(ToString::to_string),
                    tags: h.tags().iter().map(|t| t.as_str().to_owned()).collect(),
                    settings: settings_spec(&h.settings),
                    target,
                    problem,
                    guarded: inventory.guarded(&h.id).unwrap_or(false),
                    forwards_agent,
                    forwards: h.forwards.iter().map(ForwardSpec::from).collect(),
                    startup: h.startup.iter().map(ToString::to_string).collect(),
                }
            })
            .collect(),
    }
}

fn settings_spec(s: &Settings) -> SettingsSpec {
    SettingsSpec {
        user: s.user.as_ref().map(ToString::to_string),
        port: s.port,
        jumps: s
            .jumps
            .as_ref()
            .map(|hops| hops.iter().map(ToString::to_string).collect()),
        route: s.route.as_ref().map(RouteSpec::from),
        methods: s
            .methods
            .as_ref()
            .map(|m| m.iter().copied().map(AuthMethodSpec::from).collect()),
        identity_files: s.identity_files.as_ref().map(|files| {
            files
                .iter()
                .map(|f| f.to_string_lossy().into_owned())
                .collect()
        }),
        identities_only: s.identities_only,
        forward_agent: s.forward_agent,
        history: s.history,
        encoding: s.encoding.map(EncodingSpec::from),
        identity: s.identity.as_ref().map(ToString::to_string),
        env: (!s.env.is_empty()).then(|| {
            s.env
                .iter()
                .map(|v| EnvSpec {
                    name: v.name().to_owned(),
                    value: v.value().to_owned(),
                })
                .collect()
        }),
        guard: (!s.guard.is_empty()).then(|| GuardSpec {
            enabled: s.guard.enabled,
            allow: s.guard.allow().iter().map(ToString::to_string).collect(),
            deny: s.guard.deny().iter().map(ToString::to_string).collect(),
        }),
    }
}

fn env_vars(spec: Option<Vec<EnvSpec>>) -> Result<Vec<EnvVar>, CommandError> {
    let vars = spec.unwrap_or_default();
    if vars.len() > MAX_ENV {
        return Err(invalid(EnvError::TooMany));
    }
    let parsed = vars
        .iter()
        .map(|v| EnvVar::parse(&v.name, &v.value).map_err(invalid))
        .collect::<Result<Vec<_>, _>>()?;
    let sorted = env_list(parsed).map_err(invalid)?;
    if sorted.len() < vars.len() {
        return Err(invalid("an environment variable is listed twice"));
    }
    Ok(sorted)
}

fn guard_settings(spec: Option<GuardSpec>) -> Result<GuardSettings, CommandError> {
    let Some(spec) = spec else {
        return Ok(GuardSettings::default());
    };
    let patterns = |list: Vec<String>| {
        if list.len() > MAX_PATTERNS {
            return Err(invalid(format!("at most {MAX_PATTERNS} patterns")));
        }
        list.iter()
            .map(|p| Pattern::parse(p).map_err(invalid))
            .collect::<Result<Vec<_>, _>>()
    };
    GuardSettings::new(spec.enabled, patterns(spec.allow)?, patterns(spec.deny)?).map_err(invalid)
}

/// Most entries in one list field, against oversized input.
const MAX_LIST: usize = 32;
/// Longest identity file path accepted, in bytes; `PATH_MAX` on Linux.
const MAX_PATH: usize = 4096;

fn settings(spec: SettingsSpec) -> Result<Settings, CommandError> {
    let too_long = |what: &str| invalid(format!("at most {MAX_LIST} {what}"));
    if spec.jumps.as_ref().is_some_and(|j| j.len() > MAX_LIST) {
        return Err(too_long("jump hosts"));
    }
    if spec
        .identity_files
        .as_ref()
        .is_some_and(|f| f.len() > MAX_LIST)
    {
        return Err(too_long("identity files"));
    }
    if spec
        .identity_files
        .iter()
        .flatten()
        .any(|f| f.is_empty() || f.len() > MAX_PATH)
    {
        return Err(invalid(
            "an identity file path is empty or longer than 4096 bytes",
        ));
    }
    if spec
        .methods
        .as_ref()
        .is_some_and(|m| m.is_empty() || m.len() > MAX_LIST)
    {
        return Err(invalid(
            "list between 1 and 32 auth methods, or none to inherit",
        ));
    }
    let host = |h: &str| HostName::parse(h).map_err(invalid);
    Ok(Settings {
        user: spec
            .user
            .filter(|u| !u.is_empty())
            .map(|u| UserName::parse(&u))
            .transpose()
            .map_err(invalid)?,
        port: match spec.port {
            Some(0) => return Err(invalid("port must be between 1 and 65535")),
            port => port,
        },
        jumps: spec
            .jumps
            .map(|hops| {
                hops.iter()
                    .map(|h| Hop::parse(h))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()
            .map_err(invalid)?,
        route: spec
            .route
            .map(|route| {
                Ok::<_, CommandError>(match route {
                    RouteSpec::Direct => Route::Direct,
                    RouteSpec::ProxyCommand { command } => {
                        if command.trim().is_empty() || command.len() > 4096 {
                            return Err(invalid("ProxyCommand is empty or too long"));
                        }
                        Route::ProxyCommand(command)
                    }
                    RouteSpec::Socks5 { host: h, port } => Route::Socks5 {
                        host: host(&h)?,
                        port,
                    },
                    RouteSpec::HttpConnect { host: h, port } => Route::HttpConnect {
                        host: host(&h)?,
                        port,
                    },
                })
            })
            .transpose()?,
        methods: spec
            .methods
            .map(|m| m.into_iter().map(Into::into).collect()),
        identity_files: spec
            .identity_files
            .map(|files| files.into_iter().map(PathBuf::from).collect()),
        identities_only: spec.identities_only,
        guard: guard_settings(spec.guard)?,
        forward_agent: spec.forward_agent,
        history: spec.history,
        encoding: spec.encoding.map(Encoding::from),
        env: env_vars(spec.env)?,
        identity: spec
            .identity
            .filter(|i| !i.is_empty())
            .map(|i| IdentityId::parse(&i))
            .transpose()
            .map_err(invalid)?,
    })
}

/// A `ProxyCommand` reaches a shell, so the webview may keep one it was
/// given, unchanged, but never add or change one. They come from importing
/// `~/.ssh/config`, which the user wrote.
fn keep_proxy_command(requested: &Settings, stored: Option<&Settings>) -> Result<(), HostsError> {
    let is_command = matches!(requested.route, Some(Route::ProxyCommand(_)));
    if is_command && stored.and_then(|s| s.route.as_ref()) != requested.route.as_ref() {
        return Err(HostsError::Target(
            "a ProxyCommand can only come from importing ~/.ssh/config".into(),
        ));
    }
    Ok(())
}

fn parse_host(book: &HostBook, input: HostInput) -> Result<Host, CommandError> {
    let id = match input.id {
        Some(id) => HostId::parse(&id),
        None => book.new_host_id(),
    }
    .map_err(invalid)?;
    let mut host = Host::new(
        id,
        Label::parse(&input.label).map_err(invalid)?,
        HostName::parse(input.address.trim()).map_err(invalid)?,
    );
    host.group = input
        .group
        .map(|g| GroupId::parse(&g))
        .transpose()
        .map_err(invalid)?;
    if input.tags.len() > tauminus_domain::MAX_TAGS {
        return Err(invalid(HostsError::TooManyTags));
    }
    let tags = input
        .tags
        .iter()
        .map(|t| Tag::parse(t))
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid)?;
    host.set_tags(tags).map_err(invalid)?;
    host.settings = settings(input.settings)?;
    if let Some(forwards) = input.forwards {
        if forwards.len() > tauminus_domain::MAX_FORWARDS {
            return Err(invalid(tauminus_domain::ForwardError::TooMany));
        }
        host.forwards = forwards
            .into_iter()
            .map(|spec| parse_rule(spec, || book.new_forward_id().map_err(invalid)))
            .collect::<Result<_, _>>()?;
    }
    if let Some(startup) = input.startup {
        if startup.len() > tauminus_domain::MAX_STARTUP {
            return Err(invalid(HostsError::Startup));
        }
        host.startup = startup
            .iter()
            .map(|id| SnippetId::parse(id))
            .collect::<Result<_, _>>()
            .map_err(invalid)?;
    }
    Ok(host)
}

/// What a save leaves as stored because the view did not send it.
#[derive(Debug, Clone, Copy)]
struct Keep {
    forwards: bool,
    startup: bool,
}

/// Saves `host`, keeping its stored `ProxyCommand` and, when the view sent
/// none, its stored forwarding rules and startup snippets.
fn put_host(inventory: &mut Inventory, mut host: Host, keep: Keep) -> Result<(), HostsError> {
    let stored = inventory.host(&host.id);
    keep_proxy_command(&host.settings, stored.map(|h| &h.settings))?;
    if let Some(stored) = stored {
        if keep.forwards {
            host.forwards.clone_from(&stored.forwards);
        }
        if keep.startup {
            host.startup.clone_from(&stored.startup);
        }
    }
    inventory.put_host(host)
}

fn parse_group(book: &HostBook, input: GroupInput) -> Result<Group, CommandError> {
    Ok(Group {
        id: match input.id {
            Some(id) => GroupId::parse(&id),
            None => book.new_group_id(),
        }
        .map_err(invalid)?,
        name: Label::parse(&input.name).map_err(invalid)?,
        parent: input
            .parent
            .map(|p| GroupId::parse(&p))
            .transpose()
            .map_err(invalid)?,
        settings: settings(input.settings)?,
    })
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn hosts_list(hosts: State<'_, Hosts>) -> Result<HostsView, CommandError> {
    Ok(view(&hosts.book()?.snapshot().await))
}

/// Adds a host, or replaces the one with the same id.
#[tauri::command]
#[specta::specta]
pub(crate) async fn host_save(
    hosts: State<'_, Hosts>,
    forwards: State<'_, Arc<Forwards>>,
    host: HostInput,
) -> Result<HostsView, CommandError> {
    let book = hosts.book()?;
    let keep = Keep {
        forwards: host.forwards.is_none(),
        startup: host.startup.is_none(),
    };
    let host = parse_host(book, host)?;
    let id = host.id.clone();
    let mut before = Inventory::default();
    let inventory = book
        .edit(|inventory| {
            before = inventory.clone();
            put_host(inventory, host, keep)
        })
        .await?;
    // A running rule that was removed or changed stops, so nothing keeps
    // listening the way the list no longer says.
    let rules = |inventory: &Inventory| {
        inventory
            .host(&id)
            .map(|h| h.forwards.clone())
            .unwrap_or_default()
    };
    for rule in stale_rules(&rules(&before), &rules(&inventory)) {
        forwards.stop(&id, &rule);
    }
    stop_moved(&forwards, &before, &inventory);
    Ok(view(&inventory))
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn host_delete(
    hosts: State<'_, Hosts>,
    forwards: State<'_, Arc<Forwards>>,
    id: String,
) -> Result<HostsView, CommandError> {
    let id = HostId::parse(&id).map_err(invalid)?;
    let inventory = hosts
        .book()?
        .edit(|inventory| inventory.remove_host(&id).map(drop))
        .await?;
    forwards.stop_host(&id);
    Ok(view(&inventory))
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn group_save(
    hosts: State<'_, Hosts>,
    forwards: State<'_, Arc<Forwards>>,
    group: GroupInput,
) -> Result<HostsView, CommandError> {
    let book = hosts.book()?;
    let group = parse_group(book, group)?;
    let mut before = Inventory::default();
    let inventory = book
        .edit(|inventory| {
            before = inventory.clone();
            keep_proxy_command(
                &group.settings,
                inventory.group(&group.id).map(|g| &g.settings),
            )?;
            inventory.put_group(group)
        })
        .await?;
    stop_moved(&forwards, &before, &inventory);
    Ok(view(&inventory))
}

/// Stops the running rules of every host an edit pointed elsewhere: a
/// new address, user, port, jump host, route, identity, or session option,
/// from the host or a group. A rule keeps forwarding only to where its host
/// still points. O(H·d) for H hosts at depth d.
fn stop_moved(forwards: &Forwards, before: &Inventory, after: &Inventory) {
    for host in moved_hosts(before, after) {
        forwards.stop_host(&host);
    }
}

fn moved_hosts(before: &Inventory, after: &Inventory) -> Vec<HostId> {
    // The name a host without a user logs in as, as a connect resolves it.
    let Some(me) = local_user().ok().and_then(|u| UserName::parse(&u).ok()) else {
        return Vec::new();
    };
    after
        .hosts()
        .filter(|host| before.host(&host.id).is_some())
        .filter(|host| {
            before.resolve(&host.id, &me) != after.resolve(&host.id, &me)
                || before.identity(&host.id) != after.identity(&host.id)
        })
        .map(|host| host.id.clone())
        .collect()
}

/// Removes a group. Its hosts and subgroups move to its parent.
#[tauri::command]
#[specta::specta]
pub(crate) async fn group_delete(
    hosts: State<'_, Hosts>,
    forwards: State<'_, Arc<Forwards>>,
    id: String,
) -> Result<HostsView, CommandError> {
    let id = GroupId::parse(&id).map_err(invalid)?;
    let mut before = Inventory::default();
    let inventory = hosts
        .book()?
        .edit(|inventory| {
            before = inventory.clone();
            inventory.remove_group(&id).map(drop)
        })
        .await?;
    // Its hosts move to the parent group and may inherit other settings.
    stop_moved(&forwards, &before, &inventory);
    Ok(view(&inventory))
}

/// Checks the readings of a command line about to run on a saved host
/// against its prod guard. Lines are never logged: they may hold a secret
/// typed on the command line.
#[tauri::command]
#[specta::specta]
pub(crate) async fn guard_check(
    hosts: State<'_, Hosts>,
    id: String,
    lines: Vec<String>,
) -> Result<GuardVerdict, CommandError> {
    check_lines(hosts.book()?, &id, lines).await
}

/// Most readings of one Enter checked together; the view sends at most 8.
const MAX_CANDIDATES: usize = 8;

/// Checks the readings of one submitted line: the line as typed, the whole
/// screen line, the rows below, the keys typed. Findings from all of them
/// count; `incomplete` comes from the first.
async fn check_lines(
    book: &HostBook,
    id: &str,
    lines: Vec<String>,
) -> Result<GuardVerdict, CommandError> {
    let id = HostId::parse(id).map_err(invalid)?;
    if lines.len() > MAX_CANDIDATES {
        return Err(invalid(format!("at most {MAX_CANDIDATES} lines per check")));
    }
    let policy = book.guard_policy(&id).await.map_err(invalid)?;
    // Lines over `MAX_LINE` bytes come back flagged unread, without a parse.
    // The readings share one budget (8·MAX_LINE characters lexed, 64 Mi
    // characters matched) but may take milliseconds, so they run off the
    // async workers.
    let verdict = tauri::async_runtime::spawn_blocking(move || {
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        policy.check_all(&lines)
    })
    .await
    .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(GuardVerdict {
        findings: verdict
            .findings
            .into_iter()
            .map(|f| GuardFinding {
                rule: f.rule.id().to_owned(),
                reason: f.rule.reason().to_owned(),
                command: f.command,
            })
            .collect(),
        incomplete: verdict.incomplete,
    })
}

/// How many hosts `~/.ssh/config` names, for the first-run offer. Reads the
/// files only; nothing runs.
#[tauri::command]
#[specta::specta]
pub(crate) async fn ssh_config_aliases(config: State<'_, SshConfig>) -> Result<u32, CommandError> {
    let Some(config) = &config.0 else {
        tracing::debug!("no home directory; no ssh config to offer");
        return Ok(0);
    };
    let count = config.alias_count().await;
    tracing::debug!(count, "hosts named in ssh config");
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// Imports `~/.ssh/config` into the "SSH config" group. Hosts already saved
/// are skipped, so importing again adds only new ones.
#[tauri::command]
#[specta::specta]
pub(crate) async fn hosts_import_ssh_config(
    hosts: State<'_, Hosts>,
    config: State<'_, SshConfig>,
) -> Result<ImportResult, CommandError> {
    let config = config.0.as_ref().ok_or_else(|| {
        CommandError::new(
            ErrorCode::StoreFailed,
            "no home directory to read ~/.ssh/config from",
        )
    })?;
    let (report, inventory) = hosts.book()?.import(config).await?;
    Ok(ImportResult {
        added: u32::try_from(report.added.len()).unwrap_or(u32::MAX),
        skipped: report
            .skipped
            .into_iter()
            .map(|(alias, reason)| SkippedHost { alias, reason })
            .collect(),
        view: view(&inventory),
    })
}

#[cfg(test)]
mod tests;
