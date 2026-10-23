// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! System notifications for triggers that notify, raised from Rust only.
//!
//! The webview has none of the notification plugin's permissions: it names
//! the pane's rule (a handle and an index), and the text is made here from
//! what the user wrote, the rule's name and the saved host's label. Output
//! from the host never goes into a notification: notification centres keep
//! what they show, on the lock screen too, and a host could pose as a
//! prompt of the app's. A notification shows only while the window is in
//! the back, at most one per rule every 30 s and five a minute in all.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use tauminus_domain::{Action, HostId};
use tauri::State;

use crate::hosts::Hosts;
use crate::ipc::{CommandError, ErrorCode};
use crate::triggers::Triggers;

/// The least time between two notifications of one rule of one set of
/// rules (panes with the same sets share them).
const PER_RULE: Duration = Duration::from_secs(30);
/// Notifications in any minute, from every pane together.
const PER_MINUTE: usize = 5;
const TITLE_CHARS: usize = 64;
const BODY_CHARS: usize = 120;

/// When notifications went out, to keep them few. Memory: one entry per
/// rule that notified in the last 30 s, and five times. O(rules) per
/// notification that goes out, O(1) for one that does not.
#[derive(Debug, Default)]
pub(crate) struct Limiter {
    last: HashMap<(u32, u16), Instant>,
    recent: VecDeque<Instant>,
}

impl Limiter {
    /// Whether rule `key` may notify at `now`, counting it if so. O(1)
    /// amortized.
    pub(crate) fn allow(&mut self, key: (u32, u16), now: Instant) -> bool {
        if self
            .last
            .get(&key)
            .is_some_and(|&at| now.duration_since(at) < PER_RULE)
        {
            return false;
        }
        while self
            .recent
            .front()
            .is_some_and(|&at| now.duration_since(at) >= Duration::from_secs(60))
        {
            self.recent.pop_front();
        }
        if self.recent.len() >= PER_MINUTE {
            return false;
        }
        self.recent.push_back(now);
        // Rules that went quiet are forgotten, so the map stays small.
        self.last
            .retain(|_, &mut at| now.duration_since(at) < PER_RULE);
        self.last.insert(key, now);
        true
    }
}

/// Shows a notification: title, then body.
pub(crate) type Show = Box<dyn Fn(String, String) -> Result<(), String> + Send + Sync>;

/// How the app raises notifications: the limiter, whether the window has
/// focus, and the system's way to show one, set once the app is running.
#[derive(Default)]
pub(crate) struct Notices {
    limiter: Mutex<Limiter>,
    focused: AtomicBool,
    show: Mutex<Option<Show>>,
}

impl std::fmt::Debug for Notices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Notices").finish_non_exhaustive()
    }
}

impl Notices {
    pub(crate) fn set_show(&self, show: Show) {
        *self.show.lock().unwrap_or_else(PoisonError::into_inner) = Some(show);
    }

    /// The window gained or lost focus.
    pub(crate) fn set_focused(&self, focused: bool) {
        self.focused.store(focused, Ordering::Relaxed);
    }
}

/// `text` as a notification may show it: control and invisible characters
/// out, runs of white space as one space, at most `max` characters, and,
/// if `escape`, markup escaped, since a Linux notification server may
/// render the body's tags. O(n).
pub(crate) fn notice_text(text: &str, max: usize, escape: bool) -> String {
    let mut out = String::new();
    let mut count = 0;
    let mut space = false;
    for c in text.chars() {
        if c.is_control() || tauminus_domain::invisible(c) {
            if c.is_whitespace() {
                space = !out.is_empty();
            }
            continue;
        }
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if count == max {
            break;
        }
        if space {
            out.push(' ');
            count += 1;
            space = false;
            if count == max {
                break;
            }
        }
        match c {
            '&' if escape => out.push_str("&amp;"),
            '<' if escape => out.push_str("&lt;"),
            '>' if escape => out.push_str("&gt;"),
            _ => out.push(c),
        }
        count += 1;
    }
    out
}

/// Raises a system notification for trigger `rule` of the rules prepared
/// as `handle`, in a pane of saved host `host` (or none). `false` when the
/// window has focus (the app's own notice is enough), the rule notified
/// too recently, or the system refused. Errors only for a rule that is not
/// a notifying one.
#[tauri::command]
#[specta::specta]
pub(crate) async fn trigger_notify(
    hosts: State<'_, Hosts>,
    triggers: State<'_, Triggers>,
    notices: State<'_, Notices>,
    host: Option<String>,
    handle: u32,
    rule: u16,
) -> Result<bool, CommandError> {
    // The window in front: the app's notice is enough, and nothing more is
    // looked up.
    if notices.focused.load(Ordering::Relaxed) {
        return Ok(false);
    }
    let Some((name, action)) = triggers.book()?.trigger(handle, rule) else {
        return Ok(false);
    };
    if action != Action::Notify {
        return Err(CommandError::new(
            ErrorCode::InvalidArgument,
            "that rule does not notify",
        ));
    }
    let allowed = notices
        .limiter
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .allow((handle, rule), Instant::now());
    if !allowed {
        return Ok(false);
    }
    let label = match host.as_deref().map(HostId::parse) {
        Some(Ok(id)) => hosts
            .book()?
            .snapshot()
            .await
            .host(&id)
            .map(|h| h.label.as_str().to_owned()),
        _ => None,
    };
    // Servers render markup in the body only, so only the body is escaped.
    let escape = cfg!(target_os = "linux");
    let title = notice_text(&format!("Tauminus: {}", name.as_str()), TITLE_CHARS, false);
    let body = notice_text(
        &label.map_or_else(
            || "Matched in a terminal".to_owned(),
            |label| format!("Matched on {label}"),
        ),
        BODY_CHARS,
        escape,
    );
    let show = notices.show.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(show) = show.as_ref() else {
        return Ok(false);
    };
    match show(title, body) {
        Ok(()) => Ok(true),
        Err(error) => {
            tracing::warn!(%error, "could not show a notification");
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_show_plain_text_only() {
        assert_eq!(
            notice_text("a <b>x</b> & <img src=/x>", 100, true),
            "a &lt;b&gt;x&lt;/b&gt; &amp; &lt;img src=/x&gt;"
        );
        assert_eq!(notice_text("a <b>", 100, false), "a <b>");
        // Escapes and bidi tricks out, lines as one, a cut by characters.
        assert_eq!(
            notice_text("\u{1b}[31mred\u{202e}txt\u{200b}\n\n  next\r", 100, false),
            "[31mredtxt next"
        );
        assert_eq!(notice_text("ééééé", 3, false), "ééé");
        assert_eq!(notice_text("  \t ", 10, false), "");
    }

    #[test]
    fn a_rule_notifies_at_most_every_30_seconds_and_all_five_a_minute() {
        let mut limiter = Limiter::default();
        let start = Instant::now();
        assert!(limiter.allow((1, 0), start));
        assert!(!limiter.allow((1, 0), start + Duration::from_secs(29)));
        assert!(limiter.allow((1, 0), start + Duration::from_secs(30)));
        // Three more rules fill the minute; a fourth waits.
        for rule in 1..4 {
            assert!(limiter.allow((1, rule), start + Duration::from_secs(31)));
        }
        assert!(!limiter.allow((2, 0), start + Duration::from_secs(32)));
        assert!(limiter.allow((2, 0), start + Duration::from_secs(61)));
    }
}
