// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SSH user keys: the kinds Tauminus generates, and how a public key is
//! added to a server's `authorized_keys`.

use thiserror::Error;

/// A key Tauminus can generate. Ed25519 is the default; RSA is for servers
/// that accept nothing newer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Ed25519,
    EcdsaP256,
    EcdsaP384,
    EcdsaP521,
    Rsa3072,
    Rsa4096,
}

/// Longest public key line accepted: an RSA-16384 key is about 2.8 KiB.
const MAX_LINE: usize = 8 * 1024;
const MAX_COMMENT: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("not a public key line: {0}")]
pub struct PublicKeyLineError(&'static str);

/// One `authorized_keys` line, `<type> <base64> [comment]`, holding nothing
/// a shell would interpret inside single quotes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKeyLine(String);

impl PublicKeyLine {
    /// Checks the shape only; the SSH layer checks that the key decodes.
    /// O(n) in the line's length.
    pub fn parse(line: &str) -> Result<Self, PublicKeyLineError> {
        let line = line.trim();
        if line.is_empty() || line.len() > MAX_LINE {
            return Err(PublicKeyLineError("empty or too long"));
        }
        let mut parts = line.splitn(3, ' ');
        let kind = parts.next().unwrap_or_default();
        let data = parts.next().ok_or(PublicKeyLineError("no key data"))?;
        let comment = parts.next().unwrap_or_default();
        let kind_ok = (kind.starts_with("ssh-")
            || kind.starts_with("ecdsa-sha2-")
            || kind.starts_with("sk-"))
            && kind
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-@.".contains(&b));
        if !kind_ok {
            return Err(PublicKeyLineError("unknown key type"));
        }
        let data_ok = !data.is_empty()
            && data
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b));
        if !data_ok {
            return Err(PublicKeyLineError("key data is not base64"));
        }
        // Printable ASCII without a quote: the line goes inside '...'.
        let comment_ok = comment.len() <= MAX_COMMENT
            && comment
                .bytes()
                .all(|b| (b' '..=b'~').contains(&b) && b != b'\'');
        if !comment_ok {
            return Err(PublicKeyLineError(
                "the comment must be printable ASCII without a quote",
            ));
        }
        Ok(Self(line.to_owned()))
    }

    /// A stored key's line made safe to install: the comment keeps its
    /// printable ASCII, other characters (accents, quotes) become `_`, as
    /// the comment is only a label. O(n).
    pub fn installable(line: &str) -> Result<Self, PublicKeyLineError> {
        let line = line.trim();
        let mut parts = line.splitn(3, ' ');
        let kind = parts.next().unwrap_or_default();
        let data = parts.next().unwrap_or_default();
        let comment: String = parts
            .next()
            .unwrap_or_default()
            .chars()
            .map(|c| {
                if (' '..='~').contains(&c) && c != '\'' {
                    c
                } else {
                    '_'
                }
            })
            .take(MAX_COMMENT)
            .collect();
        let comment = comment.trim();
        if comment.is_empty() {
            Self::parse(&format!("{kind} {data}"))
        } else {
            Self::parse(&format!("{kind} {data} {comment}"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The base64 key data, which names the key whatever its comment.
    pub fn data(&self) -> &str {
        self.0.split(' ').nth(1).unwrap_or_default()
    }
}

/// A POSIX `sh` script that adds `key` to `~/.ssh/authorized_keys` once,
/// as `ssh-copy-id` does: it creates `~/.ssh` (0700) and the file (0600)
/// when missing, takes group and other write off both (sshd refuses keys it
/// thinks others could edit), ends a last line that has no newline, and
/// changes nothing when the same key is there already, whatever its comment,
/// options, or line ending. Exits 0 when the key is present after.
pub fn install_script(key: &PublicKeyLine) -> String {
    let line = key.as_str();
    let data = key.data();
    format!(
        "set -e\n\
         umask 077\n\
         mkdir -p \"$HOME/.ssh\"\n\
         f=\"$HOME/.ssh/authorized_keys\"\n\
         touch \"$f\"\n\
         chmod go-w \"$HOME/.ssh\" \"$f\"\n\
         if awk -v k='{data}' '{{ sub(/\\r$/, \"\"); for (i = 1; i <= NF; i++) if ($i == k) found = 1 }} END {{ exit !found }}' \"$f\"; then exit 0; fi\n\
         if [ -s \"$f\" ] && [ \"$(tail -c 1 \"$f\" | od -An -c | tr -d ' ')\" != '\\n' ]; then echo >> \"$f\"; fi\n\
         printf '%s\\n' '{line}' >> \"$f\"\n"
    )
}

/// Whether a key path names a file on another machine, in Windows'
/// terms: `\\server\share`, `//server`, or any device path
/// (`\\?\`, `\\.\`, `\??\`) but one on a drive letter. Looking such
/// a file up makes Windows sign in to that machine with the user's
/// credentials, so a key path never may. O(1).
#[must_use]
pub fn names_another_machine(path: &str) -> bool {
    let path = path.replace('/', "\\");
    let device = ["\\\\?\\", "\\\\.\\", "\\??\\"]
        .iter()
        .find_map(|prefix| path.strip_prefix(prefix));
    match device {
        // A device path is local only when it names a drive: `C:\`.
        Some(rest) => {
            let rest = rest.as_bytes();
            !(rest.len() >= 2 && rest[0].is_ascii_alphabetic() && rest[1] == b':')
        }
        None => path.starts_with("\\\\"),
    }
}

#[cfg(test)]
mod tests;
