// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Whether the end of a session's output asks for a login password, and
//! for whom: what the vault checks before it types one.
//!
//! The host writes the prompt, so its text never chooses a password; it
//! only refuses one. A prompt that names another user (`password for
//! root:`) or another machine (`ops@other's password:`) is not asking for
//! this host's login.

use std::sync::LazyLock;

use regex::Regex;

/// Bytes of output kept to find the prompt in.
pub const PROMPT_TAIL: usize = 512;

/// A login password prompt, and whose login it asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordPrompt {
    pub user: String,
    /// The machine, when the prompt names it (OpenSSH's do; sudo's do not:
    /// they ask on the machine they run on).
    pub host: Option<String>,
}

/// sudo's prompt, as it prints it and in its translations:
/// `[sudo] password for ops:`.
static SUDO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\[sudo\] (?:password|passwort|kennwort|mot de passe|contraseña|senha|mật khẩu|パスワード|密码) ?(?:for|für|de|para|pour|cho|の|，)? ?([A-Za-z0-9._][A-Za-z0-9._-]*)\s?[:：]\s*$",
    )
    .unwrap_or_else(|e| unreachable!("a fixed pattern: {e}"))
});

/// OpenSSH's prompts: `ops@db.example's password:`, and from 8.4 on with
/// keyboard-interactive, `(ops@db.example) Password:`. The host may be an
/// IPv6 address, bracketed or not.
static SSH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:([A-Za-z0-9._][A-Za-z0-9._-]*)@([A-Za-z0-9.\-:\[\]%]+)'s password|\(([A-Za-z0-9._][A-Za-z0-9._-]*)@([A-Za-z0-9.\-:\[\]%]+)\) password)\s?:\s*$",
    )
    .unwrap_or_else(|e| unreachable!("a fixed pattern: {e}"))
});

/// Escape sequences: CSI, OSC (to BEL or ST), their one-byte C1 forms, and
/// two-byte ones.
static ESCAPES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:\x1b\[|\x{9b})[0-?]*[ -/]*[@-~]|(?:\x1b\]|\x{9d})[^\x07\x1b\x{9c}]*(?:\x07|\x1b\\|\x{9c})?|\x1b.",
    )
    .unwrap_or_else(|e| unreachable!("a fixed pattern: {e}"))
});

/// The line output ends on, as a terminal would leave it: escapes out, and
/// only what follows the last line break or carriage return. O(n).
pub fn last_line(tail: &[u8]) -> String {
    let text = String::from_utf8_lossy(tail);
    let text = ESCAPES.replace_all(&text, "");
    let line = text.rsplit(['\n', '\r']).next().unwrap_or_default();
    line.chars().filter(|c| !c.is_control()).collect()
}

/// The login password prompt `tail` ends on, with nothing typed after it:
/// sudo's, or OpenSSH's, which say whose login they ask for. Anything
/// else is not filled, a bare `Password:` included: a second hop, `su`,
/// telnet, a database client, and a router all print one, and nothing in
/// it says whose password it wants. O(n).
pub fn password_prompt(tail: &[u8]) -> Option<PasswordPrompt> {
    let line = last_line(tail);
    let line = line.trim_start();
    if let Some(found) = SUDO.captures(line) {
        return Some(PasswordPrompt {
            user: found[1].to_owned(),
            host: None,
        });
    }
    let found = SSH.captures(line)?;
    let (user, host) = match (found.get(1), found.get(2)) {
        (Some(user), Some(host)) => (user, host),
        _ => (found.get(3)?, found.get(4)?),
    };
    Some(PasswordPrompt {
        user: user.as_str().to_owned(),
        host: Some(
            host.as_str()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_owned(),
        ),
    })
}

impl PasswordPrompt {
    /// Whether it asks for `user`'s login on `address`: the same user
    /// (user names are case-sensitive), and the same machine when it names
    /// one (host names are not).
    pub fn asks_for(&self, user: &str, address: &str) -> bool {
        let address = address.trim_start_matches('[').trim_end_matches(']');
        self.user == user
            && self
                .host
                .as_deref()
                .is_none_or(|host| host.eq_ignore_ascii_case(address))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Option<PasswordPrompt> {
        password_prompt(text.as_bytes())
    }

    #[test]
    fn sudo_and_openssh_prompts_say_whose_login_they_ask_for() {
        assert_eq!(
            at("$ sudo ls\r\n[sudo] password for ops: "),
            Some(PasswordPrompt {
                user: "ops".into(),
                host: None
            })
        );
        assert_eq!(
            at("[sudo] Passwort für ops: ").map(|p| p.user),
            Some("ops".into())
        );
        assert_eq!(
            at("\u{1b}[1mops@db.example's password:\u{1b}[0m "),
            Some(PasswordPrompt {
                user: "ops".into(),
                host: Some("db.example".into())
            })
        );
        assert_eq!(
            at("(ops@db.example) Password: "),
            Some(PasswordPrompt {
                user: "ops".into(),
                host: Some("db.example".into())
            })
        );
        assert_eq!(
            at("ops@[2001:db8::5]'s password: ").and_then(|p| p.host),
            Some("2001:db8::5".into())
        );
    }

    #[test]
    fn prompts_that_do_not_say_whose_login_get_nothing() {
        for text in [
            // A second hop, su, telnet, a database client, a router.
            "Password: ",
            "Enter password: ",
            "Password for 'ops@other':",
            "ops@other.example\u{2019}s password: ",
            // Other secrets, and lines with typing.
            "Enter passphrase for key '/home/ops/.ssh/id_ed25519': ",
            "One-time password (OATH) for `ops': ",
            "Verification code: ",
            "New password: ",
            "[sudo] password for ops: hunter2",
            "[sudo] password for ops: \r\n$ ",
            "Your password has expired. Change it now? [y/N]? ",
            "a log line with [sudo] password for ops: in it",
        ] {
            assert_eq!(at(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_prompt_for_someone_else_does_not_ask_for_this_login() {
        let sudo = at("[sudo] password for root: ").unwrap();
        assert!(!sudo.asks_for("ops", "db.example"));
        // User names are case-sensitive.
        assert!(
            !at("[sudo] password for Ops: ")
                .unwrap()
                .asks_for("ops", "db")
        );
        let other = at("ops@other.example's password: ").unwrap();
        assert!(!other.asks_for("ops", "db.example"));
        assert!(other.asks_for("ops", "Other.Example"));
        let ipv6 = at("(ops@2001:db8::5) Password: ").unwrap();
        assert!(ipv6.asks_for("ops", "[2001:db8::5]"));
        assert!(!ipv6.asks_for("ops", "2001:db8::6"));
    }

    #[test]
    fn c1_escapes_are_escapes() {
        assert_eq!(
            last_line("x\u{9b}2Kops@db's password: ".as_bytes()),
            "xops@db's password: "
        );
    }
}
