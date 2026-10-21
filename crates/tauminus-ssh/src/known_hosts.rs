// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! OpenSSH `known_hosts` files, as specified in sshd(8).
//!
//! Supported: comma-separated host patterns with `*` and `?`, negation with
//! `!`, `[host]:port` for ports other than 22, hashed names (`|1|salt|hash`,
//! HMAC-SHA1), the `@revoked` and `@cert-authority` markers, comments, and
//! blank lines. Lines that do not parse, including lines that are not UTF-8,
//! are skipped and reported, as OpenSSH does, so one corrupt line cannot hide
//! every other key.
//!
//! Keys are decoded only for lines whose host matches, as OpenSSH does:
//! decoding is most of the cost of a large file, and most lines name other
//! hosts.
//!
//! Cost: loading is O(n) in the file size. Checking a host is O(L * k) for L
//! lines of k patterns, plus one HMAC per hashed line, plus one key decode
//! per matching line. Each pattern match is O(|p| * |s|) in the worst case
//! and linear for ordinary patterns.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use data_encoding::BASE64;
use hmac::{Hmac, KeyInit, Mac};
use rand::TryRng;
use russh::keys::ssh_key::PublicKey;
use sha1::Sha1;

/// Where a `known_hosts` line came from, for messages such as "line 12 of
/// `~/.ssh/known_hosts`".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub file: PathBuf,
    pub line: usize,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.file.display(), self.line)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    None,
    Revoked,
    CertAuthority,
}

#[derive(Debug, Clone)]
enum Hosts {
    /// Comma-separated patterns, lowercased, each possibly negated.
    Patterns(Vec<(bool, String)>),
    /// `|1|salt|hash`: HMAC-SHA1 of the host name, keyed by the salt.
    Hashed { salt: Vec<u8>, hash: Vec<u8> },
}

#[derive(Debug, Clone)]
struct Entry {
    marker: Marker,
    hosts: Hosts,
    /// `type base64`, decoded on first use.
    key_text: String,
    key: OnceLock<Option<PublicKey>>,
    location: Location,
}

/// A line that could not be parsed. Reported, never fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub location: Location,
    pub reason: &'static str,
}

/// The outcome of checking a server's key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The key is recorded for this host.
    Known(Location),
    /// The host has recorded keys and this one is none of them, whatever its
    /// type, as OpenSSH decides (`HOST_CHANGED`). The connection must not
    /// proceed. The client offers recorded key types first, so a server that
    /// still has its recorded key presents it.
    Changed { recorded: Location },
    /// The key is marked `@revoked`. The connection must not proceed.
    Revoked(Location),
    /// Nothing is recorded for this host.
    Unknown,
}

/// OpenSSH ignores a host pattern this long (`match.c`), so a line holding
/// one never matches.
const MAX_PATTERN: usize = 1023;

/// Every `known_hosts` entry Tauminus consults, from several files.
#[derive(Debug, Clone, Default)]
pub struct KnownHosts {
    entries: Vec<Entry>,
}

impl KnownHosts {
    /// Parses one file's contents. O(n) in its length.
    pub fn parse(contents: &[u8], file: &Path) -> (Self, Vec<Skipped>) {
        let mut known = Self::default();
        let skipped = known.extend(contents, file);
        (known, skipped)
    }

    /// Adds another file's entries after the existing ones. Each line is
    /// decoded on its own, so one line that is not UTF-8 loses only itself.
    pub fn extend(&mut self, contents: &[u8], file: &Path) -> Vec<Skipped> {
        let mut skipped = Vec::new();
        for (index, line) in contents.split(|&b| b == b'\n').enumerate() {
            let location = Location {
                file: file.to_path_buf(),
                line: index + 1,
            };
            let Ok(line) = std::str::from_utf8(line) else {
                skipped.push(Skipped {
                    location,
                    reason: "not UTF-8",
                });
                continue;
            };
            match parse_line(line, location.clone()) {
                Ok(Some(entry)) => self.entries.push(entry),
                Ok(None) => {}
                Err(reason) => skipped.push(Skipped { location, reason }),
            }
        }
        skipped
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Checks `key` for `host` on `port`.
    ///
    /// A revoked key wins over everything, then an exact match. Any other
    /// recorded key, of any type, means the key changed.
    pub fn verify(&self, host: &str, port: u16, key: &PublicKey) -> Verdict {
        let name = host_name(host, port);
        let mut changed: Option<&Location> = None;
        let mut known: Option<&Location> = None;
        for (entry, recorded) in self.matching(&name, |m| m != Marker::CertAuthority) {
            if entry.marker == Marker::Revoked {
                if same_key(recorded, key) {
                    return Verdict::Revoked(entry.location.clone());
                }
            } else if same_key(recorded, key) {
                known.get_or_insert(&entry.location);
            } else {
                changed.get_or_insert(&entry.location);
            }
        }
        match (known, changed) {
            (Some(location), _) => Verdict::Known(location.clone()),
            (None, Some(recorded)) => Verdict::Changed {
                recorded: recorded.clone(),
            },
            (None, None) => Verdict::Unknown,
        }
    }

    /// Certificate authorities trusted to sign host keys for `host`.
    pub fn authorities_for(&self, host: &str, port: u16) -> Vec<&PublicKey> {
        let name = host_name(host, port);
        self.matching(&name, |m| m == Marker::CertAuthority)
            .map(|(_, key)| key)
            .collect()
    }

    /// The `@revoked` line for `key` and `host`, if there is one.
    pub fn revocation(&self, host: &str, port: u16, key: &PublicKey) -> Option<&Location> {
        let name = host_name(host, port);
        self.matching(&name, |m| m == Marker::Revoked)
            .find(|(_, revoked)| same_key(revoked, key))
            .map(|(entry, _)| &entry.location)
    }

    /// Key types recorded for `host`, in file order without repeats, so the
    /// client can ask the server for one of them first (OpenSSH
    /// `order_hostkeyalgs`).
    pub fn recorded_algorithms(&self, host: &str, port: u16) -> Vec<russh::keys::Algorithm> {
        let name = host_name(host, port);
        let mut algorithms = Vec::new();
        for (_, key) in self.matching(&name, |m| m == Marker::None) {
            let algorithm = key.algorithm();
            if !algorithms.contains(&algorithm) {
                algorithms.push(algorithm);
            }
        }
        algorithms
    }

    /// Entries whose marker passes `marker` and whose hosts match `name`,
    /// with their decoded keys. Lines whose key does not decode are skipped.
    fn matching<'a, 'n>(
        &'a self,
        name: &'n str,
        marker: impl Fn(Marker) -> bool + 'n,
    ) -> impl Iterator<Item = (&'a Entry, &'a PublicKey)> + 'n
    where
        'a: 'n,
    {
        self.entries
            .iter()
            .filter(move |e| marker(e.marker) && e.matches(name))
            .filter_map(|e| e.key().map(|key| (e, key)))
    }
}

impl Entry {
    fn key(&self) -> Option<&PublicKey> {
        self.key
            .get_or_init(|| {
                let key = PublicKey::from_openssh(&self.key_text).ok();
                if key.is_none() {
                    tracing::warn!(at = %self.location, "skipping known_hosts line with an invalid key");
                }
                key
            })
            .as_ref()
    }

    fn matches(&self, name: &str) -> bool {
        match &self.hosts {
            Hosts::Patterns(patterns) => matches_patterns(patterns, name),
            Hosts::Hashed { salt, hash } => {
                hmac_sha1(salt, name.as_bytes()).verify_slice(hash).is_ok()
            }
        }
    }
}

/// The name as OpenSSH writes it: `host` on port 22, `[host]:port` otherwise.
fn host_name(host: &str, port: u16) -> String {
    let host = host.to_ascii_lowercase();
    if port == 22 {
        host
    } else {
        format!("[{host}]:{port}")
    }
}

fn same_key(a: &PublicKey, b: &PublicKey) -> bool {
    a.key_data() == b.key_data()
}

fn parse_line(line: &str, location: Location) -> Result<Option<Entry>, &'static str> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    let mut fields = line.split_whitespace();
    let mut first = fields.next().ok_or("empty line")?;
    let marker = match first {
        "@revoked" => Marker::Revoked,
        "@cert-authority" => Marker::CertAuthority,
        other if other.starts_with('@') => return Err("unknown marker"),
        _ => Marker::None,
    };
    if marker != Marker::None {
        first = fields.next().ok_or("marker without host patterns")?;
    }
    let hosts = parse_hosts(first)?;
    let algorithm = fields.next().ok_or("missing key type")?;
    let encoded = fields.next().ok_or("missing key")?;
    // Decoded on first match; the decoder also rejects a type name that
    // disagrees with the key blob.
    Ok(Some(Entry {
        marker,
        hosts,
        key_text: format!("{algorithm} {encoded}"),
        key: OnceLock::new(),
        location,
    }))
}

fn parse_hosts(field: &str) -> Result<Hosts, &'static str> {
    if let Some(rest) = field.strip_prefix("|1|") {
        let (salt, hash) = rest.split_once('|').ok_or("malformed hashed host")?;
        let salt = BASE64
            .decode(salt.as_bytes())
            .map_err(|_| "malformed hashed host")?;
        let hash = BASE64
            .decode(hash.as_bytes())
            .map_err(|_| "malformed hashed host")?;
        if salt.len() != 20 || hash.len() != 20 {
            return Err("malformed hashed host");
        }
        return Ok(Hosts::Hashed { salt, hash });
    }
    let patterns: Vec<(bool, String)> = field
        .split(',')
        .filter(|p| !p.is_empty())
        .map(|p| match p.strip_prefix('!') {
            Some(negated) => (true, negated.to_ascii_lowercase()),
            None => (false, p.to_ascii_lowercase()),
        })
        .collect();
    if patterns.is_empty() {
        return Err("no host patterns");
    }
    if patterns.iter().any(|(_, p)| p.len() >= MAX_PATTERN) {
        return Err("host pattern too long");
    }
    Ok(Hosts::Patterns(patterns))
}

/// sshd(8): a host matches when any pattern matches and no negated pattern
/// matches. A negated match rejects the line outright.
fn matches_patterns(patterns: &[(bool, String)], name: &str) -> bool {
    let mut matched = false;
    for (negated, pattern) in patterns {
        if wildcard(pattern.as_bytes(), name.as_bytes()) {
            if *negated {
                return false;
            }
            matched = true;
        }
    }
    matched
}

/// Glob match with `*` (any run) and `?` (one byte). Two pointers, and on a
/// mismatch it retries from the most recent `*` one byte further. Each
/// backtrack advances the text position, so the worst case is
/// O(|pattern| * |text|) and ordinary host patterns run in linear time.
fn wildcard(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some(b'*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == b'?' || c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    p = sp + 1;
                    t = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == b'*')
}

fn hmac_sha1(key: &[u8], message: &[u8]) -> Hmac<Sha1> {
    let mut mac =
        <Hmac<Sha1> as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message);
    mac
}

/// A new line recording `key` for `host`, with the host name hashed so a
/// stolen file does not list every server the user visits. The salt comes
/// from the operating system's random source. O(1).
pub fn hashed_line(host: &str, port: u16, key: &PublicKey) -> Result<String, std::io::Error> {
    let mut salt = [0u8; 20];
    rand::rngs::SysRng
        .try_fill_bytes(&mut salt)
        .map_err(std::io::Error::other)?;
    Ok(hashed_line_with_salt(host, port, key, salt))
}

fn hashed_line_with_salt(host: &str, port: u16, key: &PublicKey, salt: [u8; 20]) -> String {
    let name = host_name(host, port);
    let hash = hmac_sha1(&salt, name.as_bytes()).finalize().into_bytes();
    let encoded_key = key
        .to_openssh()
        .map(|k| k.split_whitespace().take(2).collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    format!(
        "|1|{}|{} {encoded_key}",
        BASE64.encode(&salt),
        BASE64.encode(&hash)
    )
}

#[cfg(test)]
mod tests;
