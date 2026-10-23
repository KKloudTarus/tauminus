// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Starts and stops saved forwarding rules, and knows which run.
//!
//! Each rule is stopped, starting, running, or failed. A start marks the
//! rule starting before it logs in, so a second start waits its turn by
//! being refused, and a stop during the login wins: the start finds its
//! mark gone and drops what it made.
//!
//! A caller takes a [`Ticket`] before it reads the rule it will start. A
//! stop of that rule, or of its host, after the ticket was taken voids
//! it, even when nothing ran yet: so a rule read just before an edit
//! removed or changed it never starts as it was.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tauminus_domain::{Forward, ForwardId, ForwardRule, HostId, IdentityId, SshTarget};
use thiserror::Error;

use crate::connections::{Connections, Login};
use crate::ports::{ConnectError, ForwardHandle, ForwardStatus, Prompter, TunnelError};

#[derive(Debug, Error)]
pub enum StartError {
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error(transparent)]
    Tunnel(#[from] TunnelError),
    #[error("this rule is already starting")]
    Busy,
    #[error("the rule was stopped while it started")]
    Stopped,
}

/// How long a restart waits for the old listener to let go of its port.
const RELEASE: std::time::Duration = std::time::Duration::from_secs(2);

/// What the view shows for one rule that ran since the app started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleState {
    pub host: HostId,
    pub rule: ForwardId,
    /// The port listening, while it runs.
    pub port: Option<u16>,
    /// Logging in or binding now.
    pub starting: bool,
    pub status: ForwardStatus,
}

enum Entry {
    /// Logging in or binding; `start` is the attempt's number.
    Starting {
        start: u64,
    },
    Running(Box<Running>),
    /// It could not start; the reason is shown until the next try.
    Failed(String),
}

/// What a rule runs and where: an edit of the rule or of its host may
/// change either, and the next start then starts it again.
struct Running {
    handle: ForwardHandle,
    forward: Forward,
    target: SshTarget,
    identity: Option<IdentityId>,
}

type Key = (HostId, ForwardId);

#[derive(Default)]
struct Table {
    entries: HashMap<Key, Entry>,
    next: u64,
    /// Bumped by each stop of a rule, and of a host. Absent is 0.
    rule_stops: HashMap<Key, u64>,
    host_stops: HashMap<HostId, u64>,
}

/// The host's stop count when a caller resolved it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostMark(u64);

/// What a caller knew when it read a rule; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket {
    host: u64,
    rule: u64,
}

impl Table {
    fn ticket(&self, key: &Key) -> Ticket {
        Ticket {
            host: self.host_stops.get(&key.0).copied().unwrap_or(0),
            rule: self.rule_stops.get(key).copied().unwrap_or(0),
        }
    }
}

pub struct Forwards {
    connections: Arc<Connections>,
    table: Mutex<Table>,
}

impl std::fmt::Debug for Forwards {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Forwards").finish_non_exhaustive()
    }
}

impl Forwards {
    pub fn new(connections: Arc<Connections>) -> Self {
        Self {
            connections,
            table: Mutex::default(),
        }
    }

    /// Take this before reading the rule to start. O(1).
    pub fn ticket(&self, host: &HostId, rule: &ForwardId) -> Ticket {
        self.lock().ticket(&(host.clone(), rule.clone()))
    }

    /// Where the host's stops stand now, for a caller that resolves the
    /// host before it knows which rules to start: a ticket taken later
    /// with [`Forwards::ticket_since`] is void if the host was stopped in
    /// between, as an edit that moved it does. O(1).
    pub fn host_mark(&self, host: &HostId) -> HostMark {
        HostMark(self.lock().host_stops.get(host).copied().unwrap_or(0))
    }

    /// A ticket for `rule` that also requires no stop of the host since
    /// `mark`. O(1).
    pub fn ticket_since(&self, host: &HostId, rule: &ForwardId, mark: HostMark) -> Ticket {
        let mut ticket = self.ticket(host, rule);
        ticket.host = mark.0;
        ticket
    }

    /// Starts `rule` on a connection to `login`'s host, reusing a live one.
    /// A rule already running as saved keeps running and reports its port;
    /// one running an older version of the rule starts again.
    pub async fn start(
        &self,
        login: &Login,
        rule: &ForwardRule,
        ticket: Ticket,
        prompter: Arc<dyn Prompter>,
    ) -> Result<u16, StartError> {
        let key = (login.host.clone(), rule.id.clone());
        let (start, old) = {
            let mut table = self.lock();
            if table.ticket(&key) != ticket {
                return Err(StartError::Stopped);
            }
            match table.entries.get(&key) {
                Some(Entry::Starting { .. }) => return Err(StartError::Busy),
                Some(Entry::Running(running))
                    if running.forward == rule.forward
                        && running.target == login.target
                        && running.identity == login.identity
                        && running.handle.status().ended.is_none() =>
                {
                    return Ok(running.handle.port());
                }
                // Stopped, failed, ended, or an older version: (re)start.
                _ => {}
            }
            table.next += 1;
            let start = table.next;
            let old = table.entries.insert(key.clone(), Entry::Starting { start });
            (start, old)
        };
        // An older version lets go of its port before the new one binds.
        if let Some(Entry::Running(running)) = old {
            running.handle.stop(RELEASE).await;
            // A stop while it let go wins before any login starts.
            if !self.is_current(&key, start) {
                return Err(StartError::Stopped);
            }
        }
        let started = async {
            let connection = self.connections.acquire(login, prompter).await?;
            Ok::<_, StartError>(connection.start_forward(&rule.forward).await?)
        }
        .await;
        let leftover = {
            let mut table = self.lock();
            let ours = matches!(
                table.entries.get(&key),
                Some(Entry::Starting { start: current }) if *current == start
            );
            if ours {
                return match started {
                    Ok(handle) => {
                        let port = handle.port();
                        table.entries.insert(
                            key,
                            Entry::Running(Box::new(Running {
                                handle,
                                forward: rule.forward.clone(),
                                target: login.target.clone(),
                                identity: login.identity.clone(),
                            })),
                        );
                        Ok(port)
                    }
                    Err(error) => {
                        table.entries.insert(key, Entry::Failed(error.to_string()));
                        Err(error)
                    }
                };
            }
            started.ok()
        };
        // Stopped meanwhile: what it made goes, and its port is free
        // before this returns.
        if let Some(handle) = leftover {
            handle.stop(RELEASE).await;
        }
        Err(StartError::Stopped)
    }

    /// Stops a rule, or the start under way, and forgets it. False when it
    /// was neither running nor starting.
    pub fn stop(&self, host: &HostId, rule: &ForwardId) -> bool {
        let key = (host.clone(), rule.clone());
        let mut table = self.lock();
        *table.rule_stops.entry(key.clone()).or_default() += 1;
        let removed = table.entries.remove(&key);
        matches!(removed, Some(Entry::Running(_) | Entry::Starting { .. }))
    }

    /// Stops every rule of `host`, as when the host is deleted. O(n).
    pub fn stop_host(&self, host: &HostId) {
        let mut table = self.lock();
        *table.host_stops.entry(host.clone()).or_default() += 1;
        table.entries.retain(|(h, _), _| h != host);
    }

    /// Every rule that runs, starts, ran, or failed, sorted by host then
    /// rule. O(n log n).
    pub fn list(&self) -> Vec<RuleState> {
        let mut states: Vec<RuleState> = self
            .lock()
            .entries
            .iter()
            .map(|((host, rule), entry)| {
                let (port, status) = match entry {
                    Entry::Running(running) => {
                        let status = running.handle.status();
                        (
                            status.ended.is_none().then(|| running.handle.port()),
                            status,
                        )
                    }
                    Entry::Starting { .. } => (None, ForwardStatus::default()),
                    Entry::Failed(reason) => (
                        None,
                        ForwardStatus {
                            ended: Some(reason.clone()),
                            ..ForwardStatus::default()
                        },
                    ),
                };
                RuleState {
                    host: host.clone(),
                    rule: rule.clone(),
                    port,
                    starting: matches!(entry, Entry::Starting { .. }),
                    status,
                }
            })
            .collect();
        states.sort_by(|a, b| (&a.host, &a.rule).cmp(&(&b.host, &b.rule)));
        states
    }

    fn is_current(&self, key: &Key, start: u64) -> bool {
        matches!(
            self.lock().entries.get(key),
            Some(Entry::Starting { start: current }) if *current == start
        )
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests;
