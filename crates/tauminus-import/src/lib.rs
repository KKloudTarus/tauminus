// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Hosts other SSH clients keep, read from their files: a CSV host list,
//! PuTTY's sessions, MobaXterm's bookmarks, SecureCRT's session folder.
//!
//! Each reader turns a file into [`ConfigHost`] entries, as the
//! `~/.ssh/config` import does, so the same checks apply when they are
//! saved. A session that is not SSH, or that needs something Tauminus
//! does not import (a SOCKS proxy, a local proxy command), comes back with
//! its `problem` set: it is reported, not saved. Passwords are never read.
//! Every reader is O(n) in the bytes it reads.

use std::fmt::Write as _;
use std::path::PathBuf;

use tauminus_app::ports::{BoxFuture, ConfigHost, SshConfigSource};

pub mod csv;
pub mod mobaxterm;
pub mod putty;
pub mod securecrt;

/// The text of a file another client wrote, in whatever encoding.
pub use tauminus_charset::decode_file as decode;

/// Hosts already read, as the import use case takes them: handed over
/// once, not copied.
#[derive(Debug)]
pub struct Parsed {
    /// The group they go into: the client's name.
    pub group: &'static str,
    /// What was read, as an error names it.
    pub what: String,
    hosts: std::sync::Mutex<Vec<ConfigHost>>,
}

impl Parsed {
    #[must_use]
    pub fn new(group: &'static str, what: String, hosts: Vec<ConfigHost>) -> Self {
        Self {
            group,
            what,
            hosts: std::sync::Mutex::new(hosts),
        }
    }

    /// The hosts read; empty once the import took them.
    #[must_use]
    pub fn hosts_read(&self) -> Vec<ConfigHost> {
        self.hosts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl SshConfigSource for Parsed {
    fn hosts(&self) -> BoxFuture<'_, Result<Vec<ConfigHost>, std::io::Error>> {
        let hosts = std::mem::take(
            &mut *self
                .hosts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        Box::pin(async move { Ok(hosts) })
    }

    fn group(&self) -> &str {
        self.group
    }

    fn what(&self) -> &str {
        &self.what
    }
}

/// One session read, before the checks a saved host gets.
#[derive(Debug, Default)]
pub(crate) struct Session {
    pub(crate) name: String,
    pub(crate) host: String,
    pub(crate) port: Option<u16>,
    pub(crate) user: Option<String>,
    pub(crate) key: Option<PathBuf>,
    /// Jump hosts as `ssh -J` writes them.
    pub(crate) jumps: Vec<Jump>,
    pub(crate) folder: Vec<String>,
    pub(crate) problem: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Jump {
    pub(crate) host: String,
    pub(crate) port: Option<u16>,
    pub(crate) user: Option<String>,
}

/// Folder levels kept: the import's group takes one of the 16 a group
/// may nest. Deeper ones are cut here, so no file makes more.
pub(crate) const MAX_FOLDERS: usize = 15;
/// Longest folder or session name kept, in characters, as a label is.
const MAX_NAME: usize = 128;
/// Longest key path kept, as the host form takes it.
const MAX_KEY_PATH: usize = 4096;

/// A port as another client wrote it: none when blank, an error when it
/// is not one.
pub(crate) fn port(text: &str) -> Result<Option<u16>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    text.parse::<u16>()
        .map(Some)
        .map_err(|_| format!("the port {text} is not a number"))
}

/// Most characters a folder path keeps in all: each session holds its own
/// copy, and a real path is far shorter.
const MAX_FOLDER_CHARS: usize = 512;

/// A name from a file as it is kept: cut to a label's length, without the
/// bidi controls that would make it show as another name.
pub(crate) fn name(text: &str) -> String {
    text.chars()
        .filter(|&c| !is_bidi_control(c))
        .take(MAX_NAME)
        .collect()
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// The first `MAX_FOLDERS` levels of `folder`, each name kept as [`name`]
/// keeps it, up to 512 characters in all.
pub(crate) fn folder(levels: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut total = 0;
    let mut kept: Vec<String> = levels
        .into_iter()
        .take(MAX_FOLDERS)
        .map(|level| name(&level))
        .take_while(|level| {
            total += level.chars().count();
            total <= MAX_FOLDER_CHARS
        })
        .collect();
    // Collecting from a long Vec reuses its buffer: give the rest back.
    kept.shrink_to_fit();
    kept
}

impl Session {
    pub(crate) fn into_config(self) -> ConfigHost {
        let problem = self
            .problem
            .or_else(|| {
                self.host
                    .trim()
                    .is_empty()
                    .then(|| "no host name is set".to_owned())
            })
            .or_else(|| {
                // `ssh -J` reads a blank hop, or `none`, as no jump at all.
                self.jumps
                    .iter()
                    .any(|j| j.host.trim().is_empty() || j.host.trim().eq_ignore_ascii_case("none"))
                    .then(|| "jumps through a host with no name".to_owned())
            })
            .or_else(|| self.key.as_deref().and_then(key_problem));
        let proxy_jump = (!self.jumps.is_empty()).then(|| {
            self.jumps
                .iter()
                .map(Jump::to_ssh)
                .collect::<Vec<_>>()
                .join(",")
        });
        ConfigHost {
            // Not cut: two long names cut alike would read as one; the
            // label check reports a name too long.
            alias: self
                .name
                .trim()
                .chars()
                .filter(|&c| !is_bidi_control(c))
                .collect(),
            hostname: self.host.trim().to_owned(),
            user: self.user.filter(|u| !u.trim().is_empty()),
            port: self.port.filter(|&p| p != 0),
            proxy_jump,
            identity_files: self.key.into_iter().filter(|k| usable_key(k)).collect(),
            folder: folder(self.folder),
            problem,
            ..ConfigHost::default()
        }
    }
}

impl Jump {
    /// `user@host:port`, an IPv6 address in brackets.
    fn to_ssh(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let mut text = match &self.user {
            Some(user) if !user.is_empty() => format!("{user}@{host}"),
            _ => host,
        };
        if let Some(port) = self.port.filter(|&p| p != 0 && p != 22) {
            let _ = write!(text, ":{port}");
        }
        text
    }
}

/// Why a key path from a file is not taken. One on another machine
/// (`\\server\share`, `//server`) or a Windows device path would make
/// Windows sign in to that machine with the user's credentials when the
/// key is read at connect.
fn key_problem(path: &std::path::Path) -> Option<String> {
    let text = path.to_string_lossy();
    if text.len() > MAX_KEY_PATH {
        return Some("names a key path longer than 4096 bytes".into());
    }
    tauminus_domain::names_another_machine(&text)
        .then(|| "names a key on another machine, which is not imported".into())
}

/// A PuTTY key (`.ppk`) is left out: the SSH adapter reads OpenSSH keys,
/// and Pageant or another agent still signs for it.
fn usable_key(path: &std::path::Path) -> bool {
    !path.as_os_str().is_empty()
        && !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("ppk"))
}
