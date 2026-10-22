// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Asks the OS to confirm the user is at the machine before the vault's
//! keychain key is used: `LocalAuthentication` on macOS (Touch ID, or the
//! login password), Windows Hello, polkit on Linux. Every call blocks until
//! the user answers or [`PRESENCE_TIMEOUT`] passes; callers run it off the
//! async workers. Each check is asked afresh: no earlier answer is reused.
//!
//! It guards the app's own unlock. A program running as the user can read
//! the keychain entry without it.

use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use tauminus_app::vault::{PRESENCE_TIMEOUT, Presence, PresenceError, Purpose};

#[cfg(all(unix, not(target_os = "macos")))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

/// How long an answer to "can this system ask?" is kept. The view asks with
/// every vault status (each focus, each connect with an identity); on Linux
/// that is a new system-bus connection and a list of every polkit action.
/// Only the settings read it: an unlock always asks the OS afresh.
const AVAILABLE_FOR: Duration = Duration::from_secs(30);

/// The OS's presence check.
#[derive(Debug, Default)]
pub struct OsPresence {
    /// The app's window, which Windows Hello's prompt opens over; set once
    /// the window exists.
    window: OnceLock<isize>,
    available: Mutex<Option<(Instant, Result<(), String>)>>,
}

impl OsPresence {
    pub fn new() -> Self {
        Self::default()
    }

    /// The window the prompt belongs to, as a native handle (an `HWND` on
    /// Windows; unused elsewhere).
    pub fn set_window(&self, handle: isize) {
        let _ = self.window.set(handle);
    }
}

impl Presence for OsPresence {
    fn name(&self) -> String {
        #[cfg(target_os = "macos")]
        return macos::name();
        #[cfg(windows)]
        return "Windows Hello".into();
        #[cfg(not(any(target_os = "macos", windows)))]
        return "system authentication".into();
    }

    fn available(&self) -> Result<(), String> {
        let mut kept = self
            .available
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some((at, answer)) = kept.as_ref()
            && at.elapsed() < AVAILABLE_FOR
        {
            return answer.clone();
        }
        let answer = ask_available();
        *kept = Some((Instant::now(), answer.clone()));
        answer
    }

    fn confirm(&self, purpose: Purpose) -> Result<(), PresenceError> {
        let reason = purpose.reason();
        let answer = {
            #[cfg(target_os = "macos")]
            {
                macos::confirm(reason, PRESENCE_TIMEOUT)
            }
            #[cfg(windows)]
            {
                windows::confirm(reason, self.window.get().copied(), PRESENCE_TIMEOUT)
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                let _ = reason;
                linux::confirm(purpose, PRESENCE_TIMEOUT)
            }
            #[cfg(not(any(unix, windows)))]
            {
                let _ = (reason, &self.window);
                Err(PresenceError::Unavailable(
                    "not built for this system".into(),
                ))
            }
        };
        match &answer {
            Ok(()) => tracing::info!("presence confirmed"),
            Err(error) => tracing::info!(%error, "presence not confirmed"),
        }
        answer
    }
}

fn ask_available() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return macos::available();
    #[cfg(windows)]
    return windows::available();
    #[cfg(all(unix, not(target_os = "macos")))]
    return linux::available();
    #[cfg(not(any(unix, windows)))]
    return Err("not built for this system".into());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the OS's own calls (`LocalAuthentication`, `UserConsentVerifier`,
    /// polkit over D-Bus) on whatever machine runs the tests. CI runners
    /// have no sensor and no agent: the check must say so, and must not let
    /// an unlock through.
    #[test]
    fn the_os_check_runs_and_refuses_where_it_cannot_ask() {
        let presence = OsPresence::new();
        assert_ne!(presence.name(), "");
        let available = presence.available();
        // Which way this machine went, for the CI log.
        #[expect(clippy::print_stdout, reason = "the test reports what the OS answered")]
        {
            println!("{}: available {available:?}", presence.name());
        }
        if let Err(reason) = available {
            assert_ne!(reason, "");
            assert!(
                presence.confirm(Purpose::Unlock).is_err(),
                "an unavailable check confirmed presence"
            );
        }
    }

    #[test]
    fn availability_is_kept_for_a_while_then_asked_again() {
        let presence = OsPresence::new();
        let fresh = Instant::now();
        *presence.available.lock().unwrap() = Some((fresh, Ok(())));
        assert_eq!(presence.available(), Ok(()));
        assert_eq!(
            presence.available.lock().unwrap().as_ref().unwrap().0,
            fresh
        );

        let Some(stale) = fresh.checked_sub(AVAILABLE_FOR + Duration::from_secs(1)) else {
            return;
        };
        *presence.available.lock().unwrap() = Some((stale, Ok(())));
        let _ = presence.available();
        assert!(presence.available.lock().unwrap().as_ref().unwrap().0 > stale);
    }
}
