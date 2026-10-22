// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! polkit over the system bus: the session's authentication agent asks for
//! the user's own password (or a fingerprint, where PAM uses fprintd). The
//! actions ship with the package as `polkit/io.github.kkloudtarus.tauminus.policy`
//! with `auth_self`, so polkit asks every time; without them installed, as
//! in an `AppImage`, the check is unavailable. Each purpose has its own
//! action, since only polkit's policy file sets the prompt's message.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use tauminus_app::vault::{PresenceError, Purpose};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::Value;

/// The actions in the policy file: unlocking, and changing whether
/// unlocking asks.
pub(crate) const UNLOCK: &str = "io.github.kkloudtarus.tauminus.unlock-vault";
pub(crate) const CHANGE: &str = "io.github.kkloudtarus.tauminus.change-unlock-check";

fn action(purpose: Purpose) -> &'static str {
    match purpose {
        Purpose::Unlock => UNLOCK,
        Purpose::Change => CHANGE,
    }
}
/// `AllowUserInteraction`: the agent may ask.
const ALLOW_INTERACTION: u32 = 1;
const CANCELLED: &str = "org.freedesktop.PolicyKit1.Error.Cancelled";

/// One action description, as `EnumerateActions` returns it.
type Action = (
    String,
    String,
    String,
    String,
    String,
    String,
    u32,
    u32,
    u32,
    HashMap<String, String>,
);

fn authority(connection: &Connection) -> zbus::Result<Proxy<'_>> {
    Proxy::new(
        connection,
        "org.freedesktop.PolicyKit1",
        "/org/freedesktop/PolicyKit1/Authority",
        "org.freedesktop.PolicyKit1.Authority",
    )
}

pub(crate) fn available() -> Result<(), String> {
    let connection = Connection::system().map_err(|e| format!("no system bus: {e}"))?;
    let actions: Vec<Action> = authority(&connection)
        .and_then(|proxy| proxy.call("EnumerateActions", &("",)))
        .map_err(|e| format!("polkit is not running: {e}"))?;
    let installed = |id: &str| actions.iter().any(|action| action.0 == id);
    if installed(UNLOCK) && installed(CHANGE) {
        Ok(())
    } else {
        Err("the polkit action for Tauminus is not installed".into())
    }
}

pub(crate) fn confirm(purpose: Purpose, timeout: Duration) -> Result<(), PresenceError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let unavailable = |e: &dyn std::fmt::Display| PresenceError::Unavailable(e.to_string());
    // The call itself gives up a little after the prompt does, so the
    // thread waiting on it ends even if the cancel below is lost.
    let connection = zbus::blocking::connection::Builder::system()
        .map(|builder| builder.method_timeout(timeout + Duration::from_secs(5)))
        .and_then(zbus::blocking::connection::Builder::build)
        .map_err(|e| unavailable(&e))?;
    let action = action(purpose);
    let pid = std::process::id();
    let start = start_time().map_err(|e| unavailable(&e))?;
    let uid = std::fs::metadata("/proc/self")
        .map_err(|e| unavailable(&e))?
        .uid();
    let uid = i32::try_from(uid).map_err(|e| unavailable(&e))?;
    let cancel = format!("tauminus-{pid}-{}", NEXT.fetch_add(1, Ordering::Relaxed));

    let (send, answer) = mpsc::sync_channel(1);
    let asking = connection.clone();
    let id = cancel.clone();
    std::thread::spawn(move || {
        // This process, named with its start time so a reused pid is not
        // taken for it.
        let subject = (
            "unix-process",
            HashMap::from([
                ("pid", Value::from(pid)),
                ("start-time", Value::from(start)),
                ("uid", Value::from(uid)),
            ]),
        );
        let details: HashMap<&str, &str> = HashMap::new();
        let result = authority(&asking).and_then(|proxy| {
            proxy.call::<_, _, (bool, bool, HashMap<String, String>)>(
                "CheckAuthorization",
                &(subject, action, details, ALLOW_INTERACTION, id.as_str()),
            )
        });
        let _ = send.try_send(result);
    });
    match answer.recv_timeout(timeout) {
        Ok(Ok((true, _, _))) => Ok(()),
        // A challenge left unanswered: no agent in this session.
        Ok(Ok((false, true, _))) => Err(PresenceError::Unavailable(
            "no polkit authentication agent is running".into(),
        )),
        Ok(Ok((false, false, _))) => Err(PresenceError::Denied),
        Ok(Err(zbus::Error::MethodError(name, _, _))) if name.as_str() == CANCELLED => {
            Err(PresenceError::Cancelled)
        }
        Ok(Err(e)) => Err(unavailable(&e)),
        Err(_) => {
            // Closes the agent's dialog.
            let _ = authority(&connection).and_then(|proxy| {
                proxy.call::<_, _, ()>("CancelCheckAuthorization", &(cancel.as_str(),))
            });
            Err(PresenceError::TimedOut)
        }
    }
}

/// This process's start time in clock ticks since boot, as polkit names a
/// process: field 22 of `/proc/self/stat`.
fn start_time() -> std::io::Result<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat")?;
    parse_start_time(&stat).ok_or_else(|| std::io::Error::other("unreadable /proc/self/stat"))
}

/// Field 22, counted after the command name, which may hold spaces and
/// parentheses: the fields after its last `)` start at field 3.
fn parse_start_time(stat: &str) -> Option<u64> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(22 - 3)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_start_time_is_field_22_whatever_the_name() {
        let stat =
            "1234 (a b) c)) S 1 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 987654 1000 10";
        assert_eq!(parse_start_time(stat), Some(987_654));
        assert_eq!(parse_start_time("garbage"), None);
        assert!(start_time().unwrap() > 0);
    }

    #[test]
    fn the_actions_match_the_policy_file() {
        let policy = include_str!(
            "../../../apps/desktop/src-tauri/polkit/io.github.kkloudtarus.tauminus.policy"
        );
        for id in [UNLOCK, CHANGE] {
            assert!(policy.contains(&format!("<action id=\"{id}\">")), "{id}");
        }
        // Asked every time, never kept, and only at the active local seat.
        assert!(!policy.contains("_keep"));
        assert_eq!(
            policy
                .matches("<allow_active>auth_self</allow_active>")
                .count(),
            2
        );
        assert_eq!(policy.matches("<allow_any>no</allow_any>").count(), 2);
        assert_eq!(
            policy
                .matches("<allow_inactive>no</allow_inactive>")
                .count(),
            2
        );
    }
}
