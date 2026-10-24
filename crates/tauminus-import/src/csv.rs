// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! A host list as CSV, with a header row naming the columns. The header
//! names other tools write are taken (Termius, mRemoteNG, Devolutions),
//! in any case and with any punctuation: `Hostname/IP`, `Host`,
//! `Address`; `Label`, `Name`; `Username`, `User`; `Port`; `Groups`,
//! `Folder`; `Protocol`, `Type`. The separator is whichever of `,`, `;`
//! and tab the header uses most. Fields follow RFC 4180: quoted with `"`,
//! a `""` inside one is a quote, and a quoted field may hold the
//! separator or a line break. A line may end in CR LF, LF, or CR alone,
//! as Excel's Macintosh CSV writes; blank lines before the header are
//! skipped.
//!
//! A group is a path, `/` or `\` between its levels. mRemoteNG names a
//! parent by its id instead (`Id`, `Parent`, `NodeType` columns): its
//! containers become folders and are not hosts.

use std::collections::HashMap;

use tauminus_app::ports::ConfigHost;

use crate::Session;

/// Most rows read: a host list has room for 10 000 hosts.
const MAX_ROWS: usize = 20_000;
/// Most fields read from a row; the rest are left out.
const MAX_FIELDS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Column {
    Name,
    Host,
    Port,
    User,
    Group,
    Protocol,
    Id,
    Parent,
    NodeType,
}

/// The column a header names, if Tauminus reads it.
fn column(header: &str) -> Option<Column> {
    let key: String = header
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    Some(match key.as_str() {
        "label" | "name" | "alias" | "title" => Column::Name,
        "hostnameip" | "hostname" | "host" | "address" | "ip" | "server" => Column::Host,
        "port" => Column::Port,
        "username" | "user" | "login" | "credentialusername" => Column::User,
        "groups" | "group" | "folder" | "path" => Column::Group,
        "protocol" | "type" | "connectiontype" => Column::Protocol,
        "id" => Column::Id,
        "parent" => Column::Parent,
        "nodetype" => Column::NodeType,
        _ => return None,
    })
}

/// The hosts in `text`. A row whose protocol is not SSH, or whose port is
/// not a number, comes back with its problem set. O(n).
#[must_use]
pub fn hosts(text: &str) -> Vec<ConfigHost> {
    let text = text
        .trim_start_matches('\u{feff}')
        .trim_start_matches(['\r', '\n']);
    let header = text.split(['\r', '\n']).next().unwrap_or_default();
    // The most used of the three; on a tie, the first listed.
    let separator = [',', ';', '\t']
        .into_iter()
        .enumerate()
        .max_by_key(|&(at, s)| (header.matches(s).count(), std::cmp::Reverse(at)))
        .map_or(',', |(_, s)| s);
    let mut rows = records(text, separator);
    let Some(header) = rows.next() else {
        return Vec::new();
    };
    // Each column Tauminus reads, found once.
    let mut index: HashMap<Column, usize> = HashMap::new();
    for (i, name) in header.iter().enumerate() {
        if let Some(column) = column(name) {
            index.entry(column).or_insert(i);
        }
    }
    let mut sessions = Vec::new();
    // mRemoteNG: containers by id, with their parent.
    let mut containers: HashMap<String, (String, String)> = HashMap::new();
    for row in rows.take(MAX_ROWS) {
        let field = |want: Column| {
            index
                .get(&want)
                .and_then(|&i| row.get(i))
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        if field(Column::NodeType).is_some_and(|t| t.eq_ignore_ascii_case("container")) {
            if let Some(id) = field(Column::Id) {
                containers.insert(
                    id,
                    (
                        crate::name(&field(Column::Name).unwrap_or_default()),
                        field(Column::Parent).unwrap_or_default(),
                    ),
                );
            }
            continue;
        }
        if row.iter().all(|f| f.trim().is_empty()) {
            continue;
        }
        let host = field(Column::Host).unwrap_or_default();
        let mut session = Session {
            name: field(Column::Name).unwrap_or_else(|| host.clone()),
            host,
            user: field(Column::User),
            ..Session::default()
        };
        match crate::port(&field(Column::Port).unwrap_or_default()) {
            Ok(port) => session.port = port,
            Err(problem) => session.problem = Some(problem),
        }
        if let Some(protocol) = field(Column::Protocol)
            && !is_ssh(&protocol)
        {
            session.problem = Some(format!("a {protocol} session, not SSH"));
        }
        let group = field(Column::Group);
        let parent = field(Column::Parent);
        sessions.push((session, group, parent));
    }
    sessions
        .into_iter()
        .map(|(mut session, group, parent)| {
            session.folder = match (parent, group) {
                (Some(parent), _) if !containers.is_empty() => folder_of(&containers, &parent),
                (_, Some(group)) => group
                    .split(['/', '\\'])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .take(crate::MAX_FOLDERS)
                    .map(str::to_owned)
                    .collect(),
                _ => Vec::new(),
            };
            session.into_config()
        })
        .collect()
}

fn is_ssh(protocol: &str) -> bool {
    let p = protocol.to_ascii_lowercase();
    matches!(p.as_str(), "ssh" | "ssh2" | "sftp" | "ssh shell") || p.starts_with("ssh ")
}

/// The container names from the root down to `id`, the innermost
/// [`crate::MAX_FOLDERS`] of them: a deeper chain, or a loop, stops there.
/// O(d) for d ≤ 15.
fn folder_of(containers: &HashMap<String, (String, String)>, id: &str) -> Vec<String> {
    let mut path = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    let mut at = id;
    while let Some((name, parent)) = containers.get(at) {
        if path.len() >= crate::MAX_FOLDERS || seen.contains(&at) {
            break;
        }
        seen.push(at);
        path.push(name.clone());
        at = parent;
    }
    path.reverse();
    path
}

/// RFC 4180 records. O(n).
fn records(text: &str, separator: char) -> impl Iterator<Item = Vec<String>> + '_ {
    let mut chars = text.chars().peekable();
    std::iter::from_fn(move || {
        chars.peek()?;
        let mut record = Vec::new();
        let mut field = String::new();
        let mut quoted = false;
        loop {
            match chars.next() {
                Some('"') if quoted => {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        quoted = false;
                    }
                }
                // Inside quotes, a separator or line break is text.
                Some(c) if quoted => field.push(c),
                None | Some('\n') => break,
                Some('\r') => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    break;
                }
                Some('"') if field.is_empty() => quoted = true,
                Some(c) if c == separator => {
                    let done = std::mem::take(&mut field);
                    if record.len() < MAX_FIELDS {
                        record.push(done);
                    }
                }
                Some(c) => field.push(c),
            }
        }
        if record.len() < MAX_FIELDS {
            record.push(field);
        }
        Some(record)
    })
}

#[cfg(test)]
mod tests;
