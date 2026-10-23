// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::hosts::{HostId, IdentityId};
use crate::ssh::Endpoint;
use std::fmt;
use std::path::PathBuf;

/// Identifies one terminal session for the lifetime of the process.
///
/// Ids are never reused within a process, so a late message for a closed
/// session cannot reach a newer one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(u64);

impl SessionId {
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "session-{}", self.0)
    }
}

/// Which transport carries a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionKind {
    Local,
    Ssh,
    Telnet,
    Serial,
    Mosh,
}

/// Facts about a session that policies need, independent of its transport.
/// Prod guard and recording read this; the pump does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMeta {
    pub kind: SessionKind,
    /// The saved host the session logs in to, when it was opened from one:
    /// what may be typed into it from the vault depends on it.
    pub host: Option<SavedHost>,
}

/// A saved host as a session opened from it logged in: its id, the vault
/// identity, and the user and address. The host may be edited while the
/// session stays open; these say what the session is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedHost {
    pub id: HostId,
    pub identity: Option<IdentityId>,
    pub login: Endpoint,
}

impl SessionMeta {
    pub const fn local() -> Self {
        Self {
            kind: SessionKind::Local,
            host: None,
        }
    }

    /// A session of `kind` that is not a saved host's.
    pub const fn of(kind: SessionKind) -> Self {
        Self { kind, host: None }
    }
}

/// What to run for a local terminal session.
///
/// `program: None` means the user's login shell, which the PTY adapter resolves
/// per platform.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct LocalShell {
    pub program: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Set on top of what the app inherited, as `mosh-client` needs its
    /// session key in `MOSH_KEY`.
    pub env: Vec<(String, String)>,
}

impl fmt::Debug for LocalShell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Values can be secrets, as Mosh's session key: names only.
        let names: Vec<&str> = self.env.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("LocalShell")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field("env", &names)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_display_is_stable_for_logs() {
        assert_eq!(SessionId::new(42).to_string(), "session-42");
    }

    #[test]
    fn session_id_round_trips_raw_value() {
        assert_eq!(SessionId::new(u64::MAX).get(), u64::MAX);
    }

    #[test]
    fn default_local_shell_uses_login_shell() {
        let shell = LocalShell::default();
        assert_eq!(shell.program, None);
        assert_eq!(shell.args, Vec::<String>::new());
        assert_eq!(shell.cwd, None);
    }
}
