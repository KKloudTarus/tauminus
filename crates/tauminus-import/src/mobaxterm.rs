// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! MobaXterm's bookmarks, from `MobaXterm.ini` or an exported
//! `.mxtsessions` file: the `[Bookmarks]` section and its numbered
//! siblings, each with `SubRep=` naming its folder (`A\B`), then one
//! session per line, `Name=#icon#type%field%field...#terminal#...`.
//!
//! Mobatek documents none of this; the field positions follow the format
//! as read from v23.6 files (`gplv2/mobaxterm2ssh`) and the importers
//! that agree with it. For SSH (type 0): 1 host, 2 port, 3 user (or
//! `<default>`), 8 to 10 the jump hosts, their ports and users (lists
//! split by `__PIPE__`), 14 the private key, 19 the proxy type. Lines
//! written by older versions are shorter: a missing field is unset.

use std::path::{Path, PathBuf};

use tauminus_app::ports::ConfigHost;

use crate::{Jump, Session};

/// Most sessions read; far more than anyone keeps.
const MAX_SESSIONS: usize = 20_000;

/// MobaXterm's names for its session types, by number.
const TYPES: [&str; 15] = [
    "SSH", "Telnet", "Rsh", "XDMCP", "RDP", "VNC", "FTP", "SFTP", "serial", "file", "shell",
    "browser", "Mosh", "S3", "WSL",
];

/// What MobaXterm writes in place of characters its format uses.
fn unescape(text: &str) -> String {
    text.replace("__PTVIRG__", ";")
        .replace("__PIPE__", "|")
        .replace("__DIEZE__", "#")
        .replace("__DBLQUO__", "\"")
        .replace("__EQUAL__", "=")
        .replace("__PERCENT__", "%")
}

/// A key path, with MobaXterm's placeholders read against `home`.
fn key_path(text: &str, home: &Path) -> Option<PathBuf> {
    let text = unescape(text);
    if text.is_empty() {
        return None;
    }
    let text = text
        .replace("_ProfileDir_", &home.display().to_string())
        .replace(
            "_MyDocuments_",
            &home.join("Documents").display().to_string(),
        )
        .replace("_CurrentDrive_", "C");
    Some(PathBuf::from(text))
}

/// The SSH sessions in `text`, a decoded `MobaXterm.ini` or
/// `.mxtsessions`; `home` stands for `_ProfileDir_` in key paths. O(n).
#[must_use]
pub fn bookmarks(text: &str, home: &Path) -> Vec<ConfigHost> {
    let mut hosts = Vec::new();
    let mut in_bookmarks = false;
    let mut folder: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r').trim();
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let section = section.to_ascii_lowercase();
            in_bookmarks = section == "bookmarks" || section.starts_with("bookmarks_");
            folder.clear();
            continue;
        }
        if !in_bookmarks {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        match name {
            "SubRep" => {
                folder = crate::folder(
                    value
                        .split('\\')
                        .filter(|s| !s.trim().is_empty())
                        .take(crate::MAX_FOLDERS)
                        .map(unescape),
                );
            }
            "ImgNum" => {}
            name => {
                if hosts.len() >= MAX_SESSIONS {
                    break;
                }
                if let Some(mut session) = session(name, value, home) {
                    session.folder.clone_from(&folder);
                    hosts.push(session.into_config());
                }
            }
        }
    }
    hosts
}

/// One session line; `None` if it is not one.
fn session(name: &str, value: &str, home: &Path) -> Option<Session> {
    let parts: Vec<&str> = value.split('#').collect();
    let fields: Vec<&str> = parts.get(2)?.split('%').collect();
    let kind: usize = fields.first()?.trim().parse().ok()?;
    let field = |i: usize| fields.get(i).copied().unwrap_or_default();
    let text = |i: usize| Some(unescape(field(i))).filter(|v| !v.is_empty());
    let (port, port_problem) = match crate::port(field(2)) {
        Ok(port) => (port, None),
        Err(problem) => (None, Some(problem)),
    };
    let mut session = Session {
        name: unescape(name),
        host: unescape(field(1)),
        port,
        user: text(3).filter(|u| u != "<default>"),
        problem: port_problem,
        ..Session::default()
    };
    if kind != 0 {
        let what = TYPES.get(kind).copied().unwrap_or("another kind of");
        session.problem = Some(format!("a {what} session, not SSH"));
        session.port = None;
        return Some(session);
    }
    session.key = key_path(field(14), home);
    // Jump hosts, ports and users: parallel lists for a chain of hops.
    let hops: Vec<&str> = field(8).split("__PIPE__").collect();
    let hop_ports: Vec<&str> = field(9).split("__PIPE__").collect();
    let hop_users: Vec<&str> = field(10).split("__PIPE__").collect();
    for (i, host) in hops.iter().enumerate() {
        let host = unescape(host);
        if host.trim().is_empty() {
            continue;
        }
        session.jumps.push(Jump {
            host,
            port: hop_ports.get(i).and_then(|p| p.trim().parse().ok()),
            user: hop_users
                .get(i)
                .map(|u| unescape(u))
                .filter(|u| !u.is_empty() && u != "<default>"),
        });
    }
    match field(19).trim() {
        "" | "0" => {}
        proxy => {
            let kind = match proxy {
                "1" => "SOCKS 4",
                "2" => "SOCKS 5",
                "3" => "HTTP",
                "4" => "Telnet",
                _ => "command",
            };
            session.problem = Some(format!(
                "goes through a {kind} proxy, which is not imported"
            ));
        }
    }
    Some(session)
}

#[cfg(test)]
mod tests;
