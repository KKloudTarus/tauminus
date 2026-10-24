// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! PuTTY's saved sessions, and KiTTY's, which use the same names: from the
//! Windows registry (`HKCU\Software\SimonTatham\PuTTY\Sessions`), from a
//! `.reg` file `regedit` exported, or from the files PuTTY keeps on Unix
//! (`~/.putty/sessions`), one `Key=value` per line.
//!
//! A session's name is escaped: `%XX` for a byte PuTTY would not put in a
//! key or file name. The SSH jump host (`ProxyMethod` 6) becomes a jump
//! host; another proxy is reported, not imported. `Default Settings` is
//! PuTTY's template, not a session.

use std::collections::HashMap;
use std::path::PathBuf;

use tauminus_app::ports::ConfigHost;

use crate::{Jump, Session, decode};

/// One stored value: PuTTY writes strings and numbers only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Number(u32),
}

/// Most sessions read; far more than anyone keeps.
const MAX_SESSIONS: usize = 20_000;
/// Longest value read across continued lines; PuTTY's are short.
const MAX_VALUE: usize = 64 * 1024;

/// A session's name as PuTTY escaped it, back to its text. `%XX` is read
/// in either case of hex; PuTTY writes upper case. O(n).
#[must_use]
pub fn unescape(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = name.get(i + 1..i + 3)
            && hex.bytes().all(|b| b.is_ascii_hexdigit())
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    decode(&out)
}

/// The session named `name` (unescaped) from its values. `None` for
/// PuTTY's template.
#[must_use]
pub fn session<S: std::hash::BuildHasher>(
    name: &str,
    values: &HashMap<String, Value, S>,
) -> Option<ConfigHost> {
    if name == "Default Settings" {
        return None;
    }
    let text = |key: &str| match values.get(key) {
        Some(Value::Text(t)) if !t.is_empty() => Some(t.clone()),
        _ => None,
    };
    let number = |key: &str| match values.get(key) {
        Some(Value::Number(n)) => Some(*n),
        Some(Value::Text(t)) => t.trim().parse().ok(),
        None => None,
    };
    let mut problem = None;
    let mut port = |key: &str| match number(key) {
        None => None,
        Some(n) => u16::try_from(n).ok().or_else(|| {
            problem.get_or_insert_with(|| format!("the port {n} is out of range"));
            None
        }),
    };
    let (host, login) = split_host(&text("HostName").unwrap_or_default());
    let port_number = port("PortNumber");
    let mut session = Session {
        name: name.to_owned(),
        host,
        port: port_number,
        user: login.or_else(|| text("UserName")),
        key: text("PublicKeyFile").map(PathBuf::from),
        ..Session::default()
    };
    let protocol = text("Protocol").unwrap_or_else(|| "ssh".into());
    if !protocol.eq_ignore_ascii_case("ssh") {
        session.problem = Some(format!("a {protocol} session, not SSH"));
    }
    match proxy_method(&number) {
        0 => {}
        6 => {
            let (host, login) = split_host(&text("ProxyHost").unwrap_or_default());
            let proxy_port = port("ProxyPort");
            session.jumps.push(Jump {
                host,
                port: proxy_port,
                user: login.or_else(|| text("ProxyUsername")),
            });
        }
        method => {
            let kind = match method {
                1 => "SOCKS 4",
                2 => "SOCKS 5",
                3 => "HTTP",
                4 => "Telnet",
                5 => "local command",
                _ => "SSH command",
            };
            session.problem.get_or_insert_with(|| {
                format!("goes through a {kind} proxy, which is not imported")
            });
        }
    }
    if let Some(problem) = problem {
        session.problem.get_or_insert(problem);
    }
    Some(session.into_config())
}

/// A host name as PuTTY reads one before connecting (`prepare_session`):
/// `user@` in front names the user, over `UserName`; a `:suffix` after a
/// name with one colon is dropped, not taken as a port; spaces go.
fn split_host(written: &str) -> (String, Option<String>) {
    let written = written.trim_start_matches([' ', '\t']);
    let (login, host) = match written.rsplit_once('@') {
        Some((login, host)) => (Some(login.to_owned()), host),
        None => (None, written),
    };
    let host = match (host.find(':'), host.rfind(':')) {
        (Some(first), Some(last)) if first == last && !host.starts_with('[') => &host[..first],
        _ => host,
    };
    (
        host.replace([' ', '\t'], ""),
        login.filter(|l| !l.is_empty()),
    )
}

/// `ProxyMethod`, or the older `ProxyType` it replaced.
fn proxy_method(number: &impl Fn(&str) -> Option<u32>) -> u32 {
    if let Some(method) = number("ProxyMethod") {
        return method;
    }
    match number("ProxyType") {
        Some(1) => 3,
        Some(2) if number("ProxySOCKSVersion") == Some(4) => 1,
        Some(2) => 2,
        Some(3) => 4,
        Some(4) => 5,
        _ => 0,
    }
}

/// The sessions in a `.reg` file `regedit` exported: any key below a
/// `...\Sessions\` key, PuTTY's or KiTTY's. Decode the file with
/// [`decode`] first. O(n).
#[must_use]
pub fn registry_export(text: &str) -> Vec<ConfigHost> {
    let mut sessions: Vec<(String, HashMap<String, Value>)> = Vec::new();
    // Whether the key read now is a session; another key's values are not
    // one's.
    let mut in_session = false;
    let mut lines = text.lines().map(|l| l.trim_end_matches('\r'));
    while let Some(line) = lines.next() {
        let line = line.trim();
        if let Some(key) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let lower = key.to_ascii_lowercase();
            let session = lower
                .rfind("\\sessions\\")
                .map(|at| &key[at + "\\sessions\\".len()..])
                .filter(|name| !name.is_empty() && !name.contains('\\'));
            in_session = session.is_some();
            if let Some(name) = session {
                if sessions.len() >= MAX_SESSIONS {
                    break;
                }
                sessions.push((unescape(name), HashMap::new()));
            }
            continue;
        }
        let Some((_, values)) = sessions.last_mut().filter(|_| in_session) else {
            continue;
        };
        // `"Name"=` then a string, `dword:`, or `hex(2):` continued with `\`.
        let Some(rest) = line.strip_prefix('"') else {
            continue;
        };
        let Some((name, value)) = split_reg_name(rest) else {
            continue;
        };
        let mut value = value.to_owned();
        // Only a `hex` value runs on over lines, each ending in `\`.
        if value.starts_with("hex") {
            while value.ends_with('\\') && value.len() < MAX_VALUE {
                value.pop();
                value.push_str(lines.next().unwrap_or_default().trim());
            }
        }
        if let Some(parsed) = reg_value(&value) {
            values.insert(name, parsed);
        }
    }
    sessions
        .into_iter()
        .filter_map(|(name, values)| session(&name, &values))
        .collect()
}

/// `Name"=value` after the opening quote, with `\\` and `\"` read.
fn split_reg_name(rest: &str) -> Option<(String, &str)> {
    let mut name = String::new();
    let mut chars = rest.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => name.push(chars.next()?.1),
            '"' => return rest[i + 1..].strip_prefix('=').map(|v| (name, v)),
            c => name.push(c),
        }
    }
    None
}

fn reg_value(value: &str) -> Option<Value> {
    if let Some(text) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            out.push(if c == '\\' { chars.next()? } else { c });
        }
        return Some(Value::Text(out));
    }
    if let Some(hex) = value.strip_prefix("dword:") {
        return u32::from_str_radix(hex.trim(), 16).ok().map(Value::Number);
    }
    // An expandable string: UTF-16LE bytes, ending in a zero character.
    if let Some(hex) = value.strip_prefix("hex(2):") {
        let bytes: Vec<u8> = hex
            .split(',')
            .filter_map(|b| u8::from_str_radix(b.trim(), 16).ok())
            .collect();
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&p| u16::from_le_bytes(p))
            .take_while(|&u| u != 0)
            .collect();
        return Some(Value::Text(String::from_utf16_lossy(&units)));
    }
    None
}

/// The sessions in PuTTY's Unix files, given as (file name, contents):
/// `Key=value` per line, numbers in decimal. O(n).
#[must_use]
pub fn session_files<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<ConfigHost> {
    files
        .into_iter()
        .take(MAX_SESSIONS)
        .filter_map(|(file, text)| {
            let values: HashMap<String, Value> = text
                .lines()
                .filter_map(|line| line.trim_end_matches('\r').split_once('='))
                .map(|(k, v)| (k.to_owned(), Value::Text(v.to_owned())))
                .collect();
            session(&unescape(file), &values)
        })
        .collect()
}

/// Where PuTTY keeps its sessions on Unix, first match first: the
/// `PUTTYSESSIONS` or `PUTTYDIR` the environment names, then
/// `$XDG_CONFIG_HOME/putty`, `~/.config/putty` and `~/.putty`.
#[must_use]
pub fn session_dirs(home: &std::path::Path, env: impl Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(sessions) = env("PUTTYSESSIONS") {
        dirs.push(PathBuf::from(sessions));
    }
    if let Some(dir) = env("PUTTYDIR") {
        dirs.push(PathBuf::from(dir).join("sessions"));
    }
    if let Some(config) = env("XDG_CONFIG_HOME") {
        dirs.push(PathBuf::from(config).join("putty/sessions"));
    }
    dirs.push(home.join(".config/putty/sessions"));
    dirs.push(home.join(".putty/sessions"));
    dirs
}

#[cfg(windows)]
pub use registry::{KITTY, PUTTY, read_registry};

#[cfg(windows)]
mod registry;

#[cfg(test)]
mod tests;
