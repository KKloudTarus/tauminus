// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! What a connect to a host acts on, as lines a person can check. Sync
//! asks before a host whose lines another device changed is used (ADR
//! 0030), so every setting is here, whether or not it looks harmless: a
//! setting added later is covered without anyone deciding it should be.

use super::{Host, HostId, HostsError, Inventory, Settings};

/// One thing a connect to a host acts on: at `level` 0 the host itself,
/// then each group up to the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exposed {
    pub level: usize,
    /// The host's label or the group's name, to show; a rename alone
    /// changes nothing a connect does.
    pub place: String,
    pub field: &'static str,
    /// The value in full, as `Debug` writes it.
    pub value: String,
}

impl Inventory {
    /// Everything a connect to host `id` acts on, except labels, names and
    /// tags other than `prod`: its address, protocol, the `prod` tag,
    /// forwards and startup snippet ids, and
    /// every setting set on it or a group above it. O(d) for depth d.
    pub fn exposure(&self, id: &HostId) -> Result<Vec<Exposed>, HostsError> {
        let host = self
            .hosts
            .get(id)
            .ok_or_else(|| HostsError::NoSuchHost(id.clone()))?;
        // Destructured in full: a field added to `Host` fails to compile
        // here until it is listed.
        let Host {
            id: _,
            label,
            address,
            // Where it came from changes nothing a connect does.
            origin: _,
            group: _,
            tags,
            settings,
            forwards,
            startup,
            protocol,
        } = host;
        let place = label.as_str().to_owned();
        let line = |field, value: String| Exposed {
            level: 0,
            place: place.clone(),
            field,
            value,
        };
        let mut out = vec![
            line("address", address.as_str().to_owned()),
            line("protocol", format!("{protocol:?}")),
        ];
        // The one tag a connect acts on: it turns the prod guard on.
        if tags.iter().any(|t| t.as_str() == super::PROD_TAG) {
            out.push(line("tag", super::PROD_TAG.to_owned()));
        }
        if !forwards.is_empty() {
            out.push(line("forwards", format!("{forwards:?}")));
        }
        if !startup.is_empty() {
            out.push(line("startup", format!("{startup:?}")));
        }
        settings_lines(&mut out, 0, &place, settings);
        for (level, group) in self.ancestry(host.group.as_ref()).enumerate() {
            settings_lines(&mut out, level + 1, group.name.as_str(), &group.settings);
        }
        Ok(out)
    }
}

/// The settings `settings` sets, one line each.
fn settings_lines(out: &mut Vec<Exposed>, level: usize, place: &str, settings: &Settings) {
    let Settings {
        user,
        port,
        jumps,
        route,
        methods,
        identity_files,
        identities_only,
        identity_agent,
        guard,
        forward_agent,
        env,
        identity,
        history,
        ai_output,
        encoding,
        tmux,
        tmux_tabs,
        triggers,
    } = settings;
    let mut push = |field, value: String| {
        out.push(Exposed {
            level,
            place: place.to_owned(),
            field,
            value,
        });
    };
    macro_rules! set {
        ($($field:ident),*) => {
            $(if let Some(value) = $field {
                push(stringify!($field), format!("{value:?}"));
            })*
        };
    }
    set!(
        user,
        port,
        jumps,
        route,
        methods,
        identity_files,
        identities_only,
        identity_agent,
        forward_agent,
        identity,
        history,
        ai_output,
        encoding,
        tmux,
        tmux_tabs
    );
    if *guard != super::GuardSettings::default() {
        push("guard", format!("{guard:?}"));
    }
    if !env.is_empty() {
        push("env", format!("{env:?}"));
    }
    if !triggers.is_empty() {
        push("triggers", format!("{triggers:?}"));
    }
}
