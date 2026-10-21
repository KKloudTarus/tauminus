// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

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
}

/// Facts about a session that policies need, independent of its transport.
/// Prod guard and recording read this; the pump does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMeta {
    pub kind: SessionKind,
}

impl SessionMeta {
    pub const fn local() -> Self {
        Self {
            kind: SessionKind::Local,
        }
    }
}

/// What to run for a local terminal session.
///
/// `program: None` means the user's login shell, which the PTY adapter resolves
/// per platform.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalShell {
    pub program: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
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
