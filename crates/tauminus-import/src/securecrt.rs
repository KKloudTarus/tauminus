// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SecureCRT's sessions: its `Config/Sessions` folder holds one `.ini`
//! file per session, a folder per folder. A line is `S:"name"=text` or
//! `D:"name"=` eight hex digits; binary and list values (`B:`, `Z:`) and
//! their continuation lines are skipped. `Default.ini`, `Default_*.ini`
//! and `__FolderData__.ini` are SecureCRT's own, not sessions.
//!
//! The port is `[SSH2] Port` for SSH2; SSH1 is not imported. A key file
//! counts when `Use Global Public Key` is off. A `Firewall Name` of
//! `Session:<path>` is a jump through that saved session; a named
//! firewall is not imported. Passwords stay where they are.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tauminus_app::ports::ConfigHost;

use crate::{Jump, Session};

/// Most sessions read; far more than anyone keeps.
const MAX_SESSIONS: usize = 20_000;

/// One session file: its folders below `Sessions`, its file name, and its
/// decoded text.
#[derive(Debug, Clone)]
pub struct File {
    pub folder: Vec<String>,
    pub name: String,
    pub text: String,
}

/// Whether `name` (a file in the sessions tree) is a session SecureCRT
/// keeps, not one of its own files.
#[must_use]
pub fn is_session_file(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".ini") else {
        return false;
    };
    stem != "__FolderData__" && stem != "Default" && !stem.starts_with("Default_")
}

/// The values a session's import reads.
const KEPT: [&str; 11] = [
    "Protocol Name",
    "Hostname",
    "Username",
    "[SSH2] Port",
    "Port",
    "Use Global Public Key",
    "Identity Filename V2",
    "Identity Filename",
    "Firewall Name",
    "Is Session",
    "[SSH1] Port",
];

/// The `S:` and `D:` values of one file an import reads, by name. O(n).
fn values(text: &str) -> HashMap<String, Value> {
    let mut values = HashMap::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        // Continuation lines of `B:` and `Z:` values start with a space.
        if line.starts_with(' ') {
            continue;
        }
        let Some((kind, rest)) = line.split_once(":\"") else {
            continue;
        };
        let Some((name, value)) = rest.split_once("\"=") else {
            continue;
        };
        // Only what an import reads is kept, of the hundreds of values a
        // session file holds.
        if !KEPT.contains(&name) {
            continue;
        }
        let value = match kind {
            "S" => Value::Text(value.to_owned()),
            "D" => match u32::from_str_radix(value.trim(), 16) {
                Ok(n) => Value::Number(n),
                Err(_) => continue,
            },
            _ => continue,
        };
        values.insert(name.to_owned(), value);
    }
    values
}

#[derive(Debug, Clone)]
enum Value {
    Text(String),
    Number(u32),
}

/// The sessions in `files`; `config` is the `Config` folder, which
/// `${VDS_CONFIG_PATH}` in a key path names. O(n).
#[must_use]
pub fn sessions(files: impl IntoIterator<Item = File>, config: &Path) -> Vec<ConfigHost> {
    let mut read: Vec<(Vec<String>, String, HashMap<String, Value>)> = Vec::new();
    for file in files {
        if read.len() >= MAX_SESSIONS {
            break;
        }
        if !is_session_file(&file.name) {
            continue;
        }
        let name = file.name.trim_end_matches(".ini").to_owned();
        let values = values(&file.text);
        if matches!(values.get("Is Session"), Some(Value::Number(0))) {
            continue;
        }
        read.push((file.folder, name, values));
    }
    // Jump targets by their path, as `Session:<path>` names them.
    let by_path: HashMap<String, usize> = read
        .iter()
        .enumerate()
        .map(|(i, (folder, name, _))| (path_of(folder, name), i))
        .collect();
    read.iter()
        .enumerate()
        .map(|(at, (folder, name, values))| {
            let mut session = session_of(name, values, config);
            session.folder.clone_from(folder);
            match hops(&read, &by_path, at, config) {
                Ok(jumps) => session.jumps = jumps,
                Err(problem) => {
                    session.problem.get_or_insert(problem);
                }
            }
            session.into_config()
        })
        .collect()
}

/// Longest chain of sessions jumped through, as the domain allows.
const MAX_HOPS: usize = 8;

type Read = (Vec<String>, String, HashMap<String, Value>);

/// The jump hosts of session `at`, outermost first: each `Session:<path>`
/// firewall in turn, through saved sessions that are SSH2 themselves. A
/// loop, a session not here, a named firewall, or a chain longer than
/// eight is the session's problem. O(h) for h hops.
fn hops(
    read: &[Read],
    by_path: &HashMap<String, usize>,
    at: usize,
    config: &Path,
) -> Result<Vec<Jump>, String> {
    let mut jumps = Vec::new();
    let mut seen = vec![at];
    let mut current = at;
    loop {
        let firewall = match read[current].2.get("Firewall Name") {
            Some(Value::Text(firewall)) => firewall.as_str(),
            _ => "",
        };
        let Some(target) = firewall.strip_prefix("Session:") else {
            if firewall.is_empty() || firewall.eq_ignore_ascii_case("none") {
                jumps.reverse();
                return Ok(jumps);
            }
            return Err(format!(
                "goes through the firewall {firewall}, which is not imported"
            ));
        };
        // SecureCRT on Windows may write the folders with `\`.
        let key = target.replace('\\', "/");
        let Some(&next) = by_path.get(key.trim_matches('/')) else {
            return Err(format!(
                "jumps through the session {target}, which is not here"
            ));
        };
        if seen.contains(&next) {
            return Err(format!("jumps through the session {target} in a loop"));
        }
        if jumps.len() >= MAX_HOPS {
            return Err(format!("jumps through more than {MAX_HOPS} sessions"));
        }
        let (_, name, values) = &read[next];
        let hop = session_of(name, values, config);
        if let Some(problem) = hop.problem {
            return Err(format!("jumps through the session {target} ({problem})"));
        }
        jumps.push(Jump {
            host: hop.host,
            port: hop.port,
            user: hop.user,
        });
        seen.push(next);
        current = next;
    }
}

fn path_of(folder: &[String], name: &str) -> String {
    folder
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(name))
        .collect::<Vec<_>>()
        .join("/")
}

fn session_of(name: &str, values: &HashMap<String, Value>, config: &Path) -> Session {
    let text = |key: &str| match values.get(key) {
        Some(Value::Text(t)) if !t.is_empty() => Some(t.clone()),
        _ => None,
    };
    let number = |key: &str| match values.get(key) {
        Some(Value::Number(n)) => Some(*n),
        _ => None,
    };
    let protocol = text("Protocol Name").unwrap_or_else(|| "SSH2".into());
    let ssh2 = protocol.eq_ignore_ascii_case("SSH2");
    let port_key = if ssh2 { "[SSH2] Port" } else { "Port" };
    let port = number(port_key).map(u16::try_from);
    let mut session = Session {
        name: name.to_owned(),
        host: text("Hostname").unwrap_or_default(),
        port: port.and_then(Result::ok),
        user: text("Username"),
        ..Session::default()
    };
    if !ssh2 {
        session.problem = Some(format!("a {protocol} session, not SSH2"));
        return session;
    }
    if let Some(Err(_)) = port {
        session.problem = Some("the port is out of range".into());
    }
    if number("Use Global Public Key") == Some(0) {
        session.key = text("Identity Filename V2")
            .or_else(|| text("Identity Filename"))
            .map(|k| key_path(&k, config));
    }
    session
}

/// A key path with SecureCRT's variables read and its `::rawkey` suffix
/// cut. SecureCRT names a key by its public half (`Identity.pub`), the
/// private one beside it without the suffix.
fn key_path(text: &str, config: &Path) -> PathBuf {
    let text = text.split("::").next().unwrap_or_default();
    let text = text.strip_suffix(".pub").unwrap_or(text);
    PathBuf::from(text.replace("${VDS_CONFIG_PATH}", &config.display().to_string()))
}

#[cfg(test)]
mod tests;
