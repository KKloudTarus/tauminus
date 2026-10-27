// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Where an identity's secret lives when a password manager holds it
//! (ADR 0035), and the one command that reads it.
//!
//! A reference reaches a command line, so it is held to a grammar with
//! nothing a shell or `cmd.exe` reads: no option it could pass for, no
//! quote, no expansion, no path out of its place.

use thiserror::Error;

/// Which of an identity's secrets a reference stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretSlot {
    Password,
    PrivateKey,
    Passphrase,
}

/// A secret in a password manager, named the way its CLI reads it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SecretRef {
    /// `op://vault/item/[section/]field`.
    OnePassword { reference: String },
    /// A Bitwarden item's id, whose password is read.
    Bitwarden { item: String },
    /// A field of a secret in HashiCorp Vault's KV engine.
    Vault {
        address: String,
        mount: String,
        path: String,
        field: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SecretRefError {
    #[error("a 1Password reference reads op://vault/item/field or op://vault/item/section/field")]
    OnePassword,
    #[error("a Bitwarden item is named by its id, such as 3f2504e0-4f89-11d3-9a0c-0305e82c3301")]
    BitwardenItem,
    #[error("Bitwarden gives a password or a passphrase; keys come through its SSH agent")]
    BitwardenKey,
    #[error("a Vault address is https://host[:port], or http:// to this computer")]
    VaultAddress,
    #[error("a Vault mount, path and field use letters, digits and _ . - /, and no ..")]
    VaultName,
}

/// The CLI to run, and its arguments, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretCommand {
    pub program: &'static str,
    pub args: Vec<String>,
}

/// A name part: letters, digits, `_`, `.`, `-` and, where `space` allows,
/// spaces; not empty, not `.` or `..`, not starting with `-`.
fn part(text: &str, space: bool, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text != "."
        && text != ".."
        && !text.starts_with('-')
        && !text.starts_with(' ')
        && !text.ends_with(' ')
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b) || (space && b == b' '))
}

/// Parts joined by `/`, each one a [`part`].
fn path(text: &str, max: usize) -> bool {
    text.len() <= max && text.split('/').all(|p| part(p, false, 128))
}

fn uuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// `https://host[:port]`, or `http://` to a loopback address.
fn address(text: &str) -> bool {
    let (rest, secure) = if let Some(rest) = text.strip_prefix("https://") {
        (rest, true)
    } else if let Some(rest) = text.strip_prefix("http://") {
        (rest, false)
    } else {
        return false;
    };
    let (host, port) = if rest.starts_with('[') {
        let Some(end) = rest.find(']') else {
            return false;
        };
        match &rest[end + 1..] {
            "" => (&rest[..=end], None),
            tail => match tail.strip_prefix(':') {
                Some(port) => (&rest[..=end], Some(port)),
                None => return false,
            },
        }
    } else {
        match rest.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (rest, None),
        }
    };
    let host_ok = if let Some(v6) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        !v6.is_empty() && v6.bytes().all(|b| b.is_ascii_hexdigit() || b == b':')
    } else {
        part(host, false, 253) && host.bytes().all(|b| b != b'_')
    };
    let port_ok = port.is_none_or(|p| {
        p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u16>().is_ok_and(|n| n > 0)
    });
    // A name like 127.example.com is no loopback address.
    let local = matches!(host, "localhost" | "[::1]")
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback());
    text.len() <= 256 && host_ok && port_ok && (secure || local)
}

impl SecretRef {
    /// Whether this reference may stand for `slot`, and follows its
    /// manager's grammar.
    pub fn check(&self, slot: SecretSlot) -> Result<(), SecretRefError> {
        match self {
            Self::OnePassword { reference } => {
                let parts: Vec<&str> = reference
                    .strip_prefix("op://")
                    .ok_or(SecretRefError::OnePassword)?
                    .split('/')
                    .collect();
                if reference.len() <= 512
                    && (3..=4).contains(&parts.len())
                    && parts.iter().all(|p| part(p, true, 128))
                {
                    Ok(())
                } else {
                    Err(SecretRefError::OnePassword)
                }
            }
            Self::Bitwarden { item } => {
                if slot == SecretSlot::PrivateKey {
                    Err(SecretRefError::BitwardenKey)
                } else if uuid(item) {
                    Ok(())
                } else {
                    Err(SecretRefError::BitwardenItem)
                }
            }
            Self::Vault {
                address: url,
                mount,
                path: at,
                field,
            } => {
                if !address(url) {
                    Err(SecretRefError::VaultAddress)
                } else if path(mount, 128) && path(at, 512) && part(field, false, 128) {
                    Ok(())
                } else {
                    Err(SecretRefError::VaultName)
                }
            }
        }
    }

    /// The command that prints the secret, checked first.
    pub fn command(&self, slot: SecretSlot) -> Result<SecretCommand, SecretRefError> {
        self.check(slot)?;
        let owned = |args: &[&str]| args.iter().map(|&a| a.to_owned()).collect();
        Ok(match self {
            Self::OnePassword { reference } => {
                let target = if slot == SecretSlot::PrivateKey {
                    format!("{reference}?ssh-format=openssh")
                } else {
                    reference.clone()
                };
                SecretCommand {
                    program: "op",
                    args: owned(&["read", "--no-newline", "--", &target]),
                }
            }
            // Checked as a UUID, so no option; `bw` reads no `--`.
            Self::Bitwarden { item } => SecretCommand {
                program: "bw",
                args: owned(&["get", "password", item, "--nointeraction"]),
            },
            Self::Vault {
                address,
                mount,
                path,
                field,
            } => SecretCommand {
                program: "vault",
                args: owned(&[
                    "kv",
                    "get",
                    &format!("-address={address}"),
                    &format!("-mount={mount}"),
                    &format!("-field={field}"),
                    "--",
                    path,
                ]),
            },
        })
    }

    /// The manager's name, for messages.
    pub fn manager(&self) -> &'static str {
        match self {
            Self::OnePassword { .. } => "1Password",
            Self::Bitwarden { .. } => "Bitwarden",
            Self::Vault { .. } => "Vault",
        }
    }

    /// What signing in to the manager takes, when it is locked.
    pub fn sign_in(&self) -> &'static str {
        match self {
            Self::OnePassword { .. } => {
                "unlock the 1Password app with its CLI integration on, or run op signin"
            }
            Self::Bitwarden { .. } => {
                "run bw unlock and start Tauminus from that shell, with BW_SESSION set"
            }
            Self::Vault { .. } => "run vault login",
        }
    }
}

#[cfg(test)]
mod tests;
