// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reading `ssh_config(5)` files: tokens, `Include`, the aliases `Host` lines
//! define, and a resolver for the keywords Tauminus imports.

use std::path::{Path, PathBuf};

use tauminus_app::ports::ConfigHost;

use crate::known_hosts::wildcard;

/// OpenSSH's `READCONF_MAX_DEPTH`.
const MAX_INCLUDE_DEPTH: usize = 16;
/// Largest config file read; real ones are a few KiB.
const MAX_FILE: u64 = 1024 * 1024;
/// Most files and bytes one read takes in, over every `Include`. Depth alone
/// does not bound the work: `Include config config` doubles it per level.
const MAX_FILES: usize = 256;
const MAX_TOTAL: u64 = 8 * 1024 * 1024;
/// Most directory entries one glob component looks at.
const MAX_ENTRIES: usize = 1024;
/// Most aliases imported from one config.
pub(crate) const MAX_ALIASES: usize = 1000;

/// One `keyword arguments` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Line {
    /// Lowercase.
    pub(crate) keyword: String,
    pub(crate) args: Vec<String>,
    /// The text after the keyword as written, for `ProxyCommand`, which
    /// OpenSSH takes verbatim, quotes included.
    pub(crate) rest: String,
}

/// A config file as a tree: an `Include` keeps the lines it brought in,
/// because whether they apply depends on the block it sits in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item {
    Line(Line),
    Include(Vec<Item>),
}

/// Reads `file` and every file it includes. Files that are missing or
/// unreadable are skipped, as OpenSSH skips a missing include. Work is
/// bounded by [`MAX_FILES`] files and [`MAX_TOTAL`] bytes, whatever the
/// includes do. O(total size).
pub(crate) fn read(file: &Path, home: &Path) -> Vec<Item> {
    let mut budget = Budget {
        files: MAX_FILES,
        bytes: MAX_TOTAL,
    };
    let mut items = Vec::new();
    read_into(file, home, 0, &mut budget, &mut items);
    items
}

struct Budget {
    files: usize,
    bytes: u64,
}

fn read_into(file: &Path, home: &Path, depth: usize, budget: &mut Budget, out: &mut Vec<Item>) {
    if depth > MAX_INCLUDE_DEPTH || budget.files == 0 {
        return;
    }
    let Ok(metadata) = std::fs::metadata(file) else {
        return;
    };
    if !metadata.is_file() || metadata.len() > MAX_FILE.min(budget.bytes) {
        return;
    }
    let Ok(bytes) = std::fs::read(file) else {
        return;
    };
    budget.files -= 1;
    budget.bytes = budget.bytes.saturating_sub(bytes.len() as u64);
    for raw in String::from_utf8_lossy(&bytes).lines() {
        let Some(line) = tokenize(raw) else {
            continue;
        };
        if line.keyword == "include" {
            let mut included = Vec::new();
            for pattern in &line.args {
                for path in expand_include(pattern, home) {
                    read_into(&path, home, depth + 1, budget, &mut included);
                }
            }
            out.push(Item::Include(included));
        } else {
            out.push(Item::Line(line));
        }
    }
}

/// Splits a line into its keyword and arguments as OpenSSH `readconf.c`
/// does: the keyword ends at whitespace or `=`, and the rest goes through
/// [`argv_split`]. Returns `None` for blank lines, comments, and lines
/// OpenSSH would reject for unbalanced quotes. O(n).
pub(crate) fn tokenize(raw: &str) -> Option<Line> {
    let line = raw.trim_matches(is_blank);
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let split = line
        .find(|c: char| is_blank(c) || c == '=')
        .unwrap_or(line.len());
    let keyword = line[..split].to_ascii_lowercase();
    // `strspn(s, WHITESPACE "=")` in readconf.c.
    let rest = line[split..].trim_start_matches(|c: char| is_blank(c) || c == '=');
    Some(Line {
        keyword,
        args: argv_split(rest)?,
        rest: rest.to_owned(),
    })
}

/// OpenSSH's `WHITESPACE` for config lines: space and tab, nothing else.
fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t' || c == '\r'
}

/// A port of OpenSSH `misc.c` `argv_split` with `terminate_on_comment`:
/// single or double quotes group words, a backslash escapes a quote, a
/// backslash, or (outside quotes) a space, and a `#` that starts a word ends
/// the line. `None` for an unclosed quote, which OpenSSH rejects. O(n).
fn argv_split(text: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let mut args = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ' ' || chars[i] == '\t' {
            i += 1;
            continue;
        }
        if chars[i] == '#' {
            break;
        }
        let mut quote: Option<char> = None;
        let mut arg = String::new();
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            if c == '\\' {
                let escapable = matches!(next, Some('\'' | '"' | '\\'))
                    || (quote.is_none() && next == Some(' '));
                if let (true, Some(escaped)) = (escapable, next) {
                    arg.push(escaped);
                    i += 2;
                } else {
                    arg.push(c);
                    i += 1;
                }
                continue;
            }
            match quote {
                None if c == ' ' || c == '\t' => break,
                None if c == '"' || c == '\'' => quote = Some(c),
                Some(q) if c == q => quote = None,
                _ => arg.push(c),
            }
            i += 1;
        }
        if quote.is_some() {
            return None;
        }
        args.push(arg);
    }
    Some(args)
}

/// `Include` paths: `~` is the home directory, a relative path is relative
/// to `~/.ssh`, and `*` and `?` match within one path component. Matches
/// are sorted, as glob(3) returns them.
fn expand_include(pattern: &str, home: &Path) -> Vec<PathBuf> {
    let path = if let Some(rest) = pattern.strip_prefix("~/") {
        home.join(rest)
    } else if Path::new(pattern).is_absolute() {
        PathBuf::from(pattern)
    } else {
        home.join(".ssh").join(pattern)
    };
    let mut found = vec![PathBuf::new()];
    for component in path.components() {
        let text = component.as_os_str().to_string_lossy();
        if !text.contains(['*', '?']) {
            for base in &mut found {
                base.push(component.as_os_str());
            }
            continue;
        }
        let mut next = Vec::new();
        for base in &found {
            let Ok(entries) = std::fs::read_dir(base) else {
                continue;
            };
            let mut names: Vec<_> = entries
                .take(MAX_ENTRIES)
                .filter_map(Result::ok)
                .map(|e| e.file_name())
                .filter(|name| {
                    let name = name.to_string_lossy();
                    // As glob(3): a wildcard does not match a leading dot.
                    !name.starts_with('.') && wildcard(text.as_bytes(), name.as_bytes())
                })
                .collect();
            names.sort();
            next.extend(names.into_iter().map(|name| base.join(name)));
        }
        found = next;
    }
    found
}

/// Every line in file order, includes expanded in place.
fn flatten<'a>(items: &'a [Item], out: &mut Vec<&'a Line>) {
    for item in items {
        match item {
            Item::Line(line) => out.push(line),
            Item::Include(children) => flatten(children, out),
        }
    }
}

/// Aliases that `Host` lines name outright: no wildcards, no negations, and
/// no `@`, which `ssh -G` would read as `user@host`. In file order, without
/// repeats, at most [`MAX_ALIASES`]. Included files count whatever block
/// includes them, since the user wrote those names. O(total size).
pub(crate) fn aliases(items: &[Item]) -> Vec<String> {
    let mut lines = Vec::new();
    flatten(items, &mut lines);
    let mut aliases: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in lines.into_iter().filter(|l| l.keyword == "host") {
        for arg in &line.args {
            let concrete = !arg.starts_with('!') && !arg.contains(['*', '?', '@']);
            if concrete && seen.insert(arg.as_str()) {
                aliases.push(arg.clone());
                if aliases.len() == MAX_ALIASES {
                    return aliases;
                }
            }
        }
    }
    aliases
}

/// What `ssh -G alias` would report for the keywords Tauminus imports,
/// worked out without OpenSSH. First value wins, as in OpenSSH, except
/// `IdentityFile`, which accumulates. `Match` blocks are treated as not
/// matching, apart from `Match all`, since most criteria need a live
/// connection or run commands. O(lines).
pub(crate) fn resolve(items: &[Item], alias: &str) -> ConfigHost {
    let mut state = Resolving {
        name: alias.to_owned(),
        host: ConfigHost {
            alias: alias.to_owned(),
            ..ConfigHost::default()
        },
        hostname: None,
        identities_only: None,
    };
    let mut active = true;
    state.walk(items, &mut active, false);
    let Resolving {
        mut host,
        hostname,
        identities_only,
        ..
    } = state;
    // `Host` lines match the name as given, but ssh then lowercases it: the
    // default HostName and `%h` are the lowercase form.
    let name = alias.to_ascii_lowercase();
    host.hostname = hostname.map_or_else(|| name.clone(), |h| expand_hostname(&h, &name));
    if host.identity_files.is_empty() {
        host.identity_files = super::DEFAULT_IDENTITIES
            .iter()
            .map(|name| PathBuf::from(format!("~/.ssh/{name}")))
            .collect();
    }
    host.identities_only = identities_only.unwrap_or(false);
    // `none` turns the keyword off, as in OpenSSH.
    host.proxy_jump = host.proxy_jump.filter(|j| !j.eq_ignore_ascii_case("none"));
    host.proxy_command = host
        .proxy_command
        .filter(|c| !c.eq_ignore_ascii_case("none"));
    host
}

struct Resolving {
    name: String,
    host: ConfigHost,
    hostname: Option<String>,
    identities_only: Option<bool>,
}

impl Resolving {
    /// As OpenSSH `readconf.c`: an included file is read in the state of the
    /// block that includes it; when that block does not apply, nothing in
    /// the file can match; and afterwards the outer state is restored.
    fn walk(&mut self, items: &[Item], active: &mut bool, never: bool) {
        for item in items {
            match item {
                Item::Include(children) => {
                    let outer = *active;
                    self.walk(children, active, never || !outer);
                    *active = outer;
                }
                Item::Line(line) => match line.keyword.as_str() {
                    "host" => *active = !never && host_matches(&line.args, &self.name),
                    "match" => {
                        *active = !never
                            && line.args.len() == 1
                            && line.args[0].eq_ignore_ascii_case("all");
                    }
                    _ if *active => self.apply(line),
                    _ => {}
                },
            }
        }
    }

    fn apply(&mut self, line: &Line) {
        let host = &mut self.host;
        let first = line.args.first().map(String::as_str);
        match line.keyword.as_str() {
            "hostname" => set_once(&mut self.hostname, first),
            "user" => set_once(&mut host.user, first),
            "port" => {
                if host.port.is_none() {
                    host.port = first.and_then(|p| p.parse().ok());
                }
            }
            // Whichever of ProxyJump and ProxyCommand comes first wins and
            // turns the other off, `none` included, as in readconf.c.
            "proxyjump" if host.proxy_jump.is_none() && host.proxy_command.is_none() => {
                host.proxy_jump = first.map(str::to_owned);
            }
            "proxycommand" if host.proxy_jump.is_none() && host.proxy_command.is_none() => {
                // Taken verbatim, quotes included: a shell runs it.
                if !line.rest.is_empty() {
                    host.proxy_command = Some(line.rest.clone());
                }
            }
            "identityfile" => {
                // OpenSSH lists each file once.
                if let Some(file) = first.map(PathBuf::from)
                    && !host.identity_files.contains(&file)
                {
                    host.identity_files.push(file);
                }
            }
            "identitiesonly" if self.identities_only.is_none() => {
                self.identities_only = first.map(|v| v.eq_ignore_ascii_case("yes"));
            }
            _ => {}
        }
    }
}

fn set_once(slot: &mut Option<String>, value: Option<&str>) {
    if slot.is_none() {
        *slot = value.map(str::to_owned);
    }
}

/// `Host` patterns: any positive match, and no negated one. Case matters,
/// as in OpenSSH: `Host Web` does not match `web`.
fn host_matches(patterns: &[String], name: &str) -> bool {
    let mut matched = false;
    for pattern in patterns {
        let (negated, pattern) = match pattern.strip_prefix('!') {
            Some(p) => (true, p),
            None => (false, pattern.as_str()),
        };
        if wildcard(pattern.as_bytes(), name.as_bytes()) {
            if negated {
                return false;
            }
            matched = true;
        }
    }
    matched
}

/// `HostName` may use `%h` for the alias and `%%` for a percent sign.
fn expand_hostname(template: &str, alias: &str) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('h') => out.push_str(alias),
            Some('%') | None => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
        }
    }
    out
}
