// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port forwarding over IPC: the rules a host saves, and starting,
//! stopping, and watching them.

use std::net::IpAddr;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauminus_app::ports::Prompter;
use tauminus_app::{Forwards, HostMark, Login, RuleState, StartError};
use tauminus_domain::{
    Bind, BindAddress, Destination, Forward, ForwardId, ForwardRule, HostId, HostName, Label,
};
use tauri::State;
use tauri::ipc::Channel;

use crate::ipc::{CommandError, ErrorCode};
use crate::ssh::{PromptBroker, PromptRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ForwardKindSpec {
    Local,
    Remote,
    Dynamic,
}

/// One rule as the host editor shows and saves it. `bind` is `loopback`,
/// `any`, or an IP address; anything but loopback opens the tunnel to the
/// network.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ForwardSpec {
    /// Absent for a new rule.
    #[serde(default)]
    pub(crate) id: Option<String>,
    pub(crate) label: String,
    pub(crate) kind: ForwardKindSpec,
    pub(crate) bind: String,
    pub(crate) port: u16,
    /// Required for local and remote rules, absent for dynamic ones.
    #[serde(default)]
    pub(crate) to_host: Option<String>,
    #[serde(default)]
    pub(crate) to_port: Option<u16>,
    #[serde(default)]
    pub(crate) auto_start: bool,
}

fn invalid(error: impl std::fmt::Display) -> CommandError {
    CommandError::new(ErrorCode::InvalidArgument, error.to_string())
}

impl From<&ForwardRule> for ForwardSpec {
    fn from(rule: &ForwardRule) -> Self {
        let (kind, bind, to) = match &rule.forward {
            Forward::Local { bind, to } => (ForwardKindSpec::Local, bind, Some(to)),
            Forward::Remote { bind, to } => (ForwardKindSpec::Remote, bind, Some(to)),
            Forward::Dynamic { bind } => (ForwardKindSpec::Dynamic, bind, None),
        };
        Self {
            id: Some(rule.id.to_string()),
            label: rule.label.as_str().to_owned(),
            kind,
            bind: match bind.address {
                BindAddress::Loopback => "loopback".to_owned(),
                BindAddress::Any => "any".to_owned(),
                BindAddress::Ip(ip) => ip.to_string(),
            },
            port: bind.port,
            to_host: to.map(|d| d.host.to_string()),
            to_port: to.map(Destination::port),
            auto_start: rule.auto_start,
        }
    }
}

/// Validates a rule from the view. `new_id` names a rule that has none.
pub(crate) fn parse_rule(
    spec: ForwardSpec,
    new_id: impl FnOnce() -> Result<ForwardId, CommandError>,
) -> Result<ForwardRule, CommandError> {
    // An IP literal only: a name would leave a resolver to decide what
    // the listener exposes.
    let address = match spec.bind.trim() {
        "loopback" | "localhost" => BindAddress::Loopback,
        "any" => BindAddress::Any,
        ip => BindAddress::Ip(
            ip.parse::<IpAddr>()
                .map_err(|_| invalid(format!("bind address {ip:?} is not an IP address")))?,
        ),
    };
    let bind = Bind {
        address,
        port: spec.port,
    };
    let to = || -> Result<Destination, CommandError> {
        let host = spec
            .to_host
            .as_deref()
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .ok_or_else(|| invalid("this rule needs a destination host"))?;
        let port = spec
            .to_port
            .ok_or_else(|| invalid("this rule needs a destination port"))?;
        Destination::new(HostName::parse(host).map_err(invalid)?, port).map_err(invalid)
    };
    let forward = match spec.kind {
        ForwardKindSpec::Local => Forward::Local { bind, to: to()? },
        ForwardKindSpec::Remote => Forward::Remote { bind, to: to()? },
        ForwardKindSpec::Dynamic => Forward::Dynamic { bind },
    };
    Ok(ForwardRule {
        id: match spec.id {
            Some(id) => ForwardId::parse(&id).map_err(invalid)?,
            None => new_id()?,
        },
        label: Label::parse(&spec.label).map_err(invalid)?,
        forward,
        auto_start: spec.auto_start,
    })
}

/// Rules in `before` that `after` removed or changed. O(b·a), both at most
/// 64.
pub(crate) fn stale_rules(before: &[ForwardRule], after: &[ForwardRule]) -> Vec<ForwardId> {
    before
        .iter()
        .filter(|old| {
            !after
                .iter()
                .any(|new| new.id == old.id && new.forward == old.forward)
        })
        .map(|old| old.id.clone())
        .collect()
}

/// A rule that ran or failed since the app started.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ForwardState {
    pub(crate) host: String,
    pub(crate) rule: String,
    /// The port listening, while it runs.
    pub(crate) port: Option<u16>,
    /// Logging in or binding now.
    pub(crate) starting: bool,
    pub(crate) open: u32,
    pub(crate) total: u32,
    /// Connections reset because their client fell too far behind.
    pub(crate) resets: u32,
    /// Why it stopped or could not start.
    pub(crate) ended: Option<String>,
}

impl From<RuleState> for ForwardState {
    fn from(state: RuleState) -> Self {
        Self {
            host: state.host.to_string(),
            rule: state.rule.to_string(),
            port: state.port,
            starting: state.starting,
            open: u32::try_from(state.status.open).unwrap_or(u32::MAX),
            total: u32::try_from(state.status.total).unwrap_or(u32::MAX),
            resets: u32::try_from(state.status.resets).unwrap_or(u32::MAX),
            ended: state.status.ended,
        }
    }
}

impl From<StartError> for CommandError {
    fn from(error: StartError) -> Self {
        match error {
            StartError::Connect(error) => tauminus_app::SshOpenError::Connect(error).into(),
            StartError::Tunnel(error) => Self::new(ErrorCode::ConnectFailed, error.to_string()),
            StartError::Busy => Self::new(ErrorCode::LimitReached, StartError::Busy.to_string()),
            // The user stopped it; nothing to report.
            StartError::Stopped => Self::new(ErrorCode::Cancelled, StartError::Stopped.to_string()),
        }
    }
}

async fn rule(
    hosts: &crate::hosts::Hosts,
    host: &HostId,
    id: &ForwardId,
) -> Result<ForwardRule, CommandError> {
    hosts
        .book()?
        .forward_rules(host)
        .await
        .map_err(invalid)?
        .into_iter()
        .find(|r| r.id == *id)
        .ok_or_else(|| CommandError::new(ErrorCode::NotFound, format!("no forwarding rule {id}")))
}

/// Starts a saved rule, reusing a live connection to its host or logging
/// in, with any question on `on_prompt`. Returns the port listening.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::too_many_arguments,
    reason = "Tauri passes each IPC argument separately"
)]
pub(crate) async fn forward_start(
    hosts: State<'_, crate::hosts::Hosts>,
    vault: State<'_, Arc<tauminus_app::vault::Vault>>,
    gate: State<'_, crate::sync::Gate>,
    broker: State<'_, Arc<PromptBroker>>,
    forwards: State<'_, Arc<Forwards>>,
    host: String,
    rule_id: String,
    on_prompt: Channel<PromptRequest>,
) -> Result<u16, CommandError> {
    let host_id = HostId::parse(&host).map_err(invalid)?;
    let rule_id = ForwardId::parse(&rule_id).map_err(invalid)?;
    // Before reading anything, so an edit that lands meanwhile wins.
    let ticket = forwards.ticket(&host_id, &rule_id);
    let login = crate::ssh::login(&hosts, &vault, &gate, &host).await?;
    let rule = rule(&hosts, &login.host, &rule_id).await?;
    gate.check(&hosts, &login.host).await?;
    let prompter = crate::ssh::prompter(&broker, on_prompt);
    Ok(forwards.start(&login, &rule, ticket, prompter).await?)
}

/// Stops a rule. False when it was not running.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn forward_stop(
    forwards: State<'_, Arc<Forwards>>,
    host: String,
    rule_id: String,
) -> Result<bool, CommandError> {
    let host = HostId::parse(&host).map_err(invalid)?;
    let rule = ForwardId::parse(&rule_id).map_err(invalid)?;
    Ok(forwards.stop(&host, &rule))
}

/// Every rule that ran or failed since the app started.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn forwards_status(forwards: State<'_, Arc<Forwards>>) -> Vec<ForwardState> {
    forwards
        .list()
        .into_iter()
        .map(ForwardState::from)
        .collect()
}

/// Starts the host's rules marked to start with it, on the connection
/// forwards share for that host, after its shell is open. Runs in the
/// background; a rule that fails shows its reason in the status list.
pub(crate) fn start_with_host(
    hosts: &crate::hosts::Hosts,
    gate: &crate::sync::Gate,
    forwards: &Arc<Forwards>,
    login: Login,
    mark: HostMark,
    prompter: Arc<dyn Prompter>,
) {
    let crate::hosts::Hosts::Ready(book) = hosts else {
        return;
    };
    let hosts = crate::hosts::Hosts::Ready(Arc::clone(book));
    let book = Arc::clone(book);
    let gate = gate.clone();
    let forwards = Arc::clone(forwards);
    tauri::async_runtime::spawn(async move {
        let Ok(rules) = book.forward_rules(&login.host).await else {
            return;
        };
        for id in rules.into_iter().filter(|r| r.auto_start).map(|r| r.id) {
            // Each rule is read again as saved now, after its ticket: one
            // removed, changed, or whose host went while an earlier rule
            // logged in starts as saved, or not at all.
            let ticket = forwards.ticket_since(&login.host, &id, mark);
            let Ok(saved) = book.forward_rules(&login.host).await else {
                return;
            };
            let Some(rule) = saved.into_iter().find(|r| r.id == id && r.auto_start) else {
                continue;
            };
            // A sync may have changed the rules since the login's check.
            if let Err(error) = gate.check(&hosts, &login.host).await {
                tracing::warn!(rule = %rule.id, error = %error.message, "a rule did not start with its host");
                return;
            }
            if let Err(error) = forwards
                .start(&login, &rule, ticket, Arc::clone(&prompter))
                .await
            {
                tracing::warn!(rule = %rule.id, %error, "a rule did not start with its host");
            }
        }
    });
}

#[cfg(test)]
mod tests;
