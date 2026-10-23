// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Stored passwords a server turned down, so no other connect sends them.
//!
//! When a password changes on a directory server, every pane that
//! reconnects after a wake would send the old one from the vault, and a
//! handful of failures locks the account on every machine. The first
//! rejection is remembered for that user name, across hosts, until the app
//! quits; later connects ask the user instead. Only a keyed hash is kept:
//! HMAC-SHA256 under a key made at start and never written anywhere.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use hmac::{Hmac, KeyInit, Mac};
use rand::TryRng;
use sha2::Sha256;
use tauminus_app::ports::Secret;
use zeroize::Zeroizing;

/// Most rejections remembered; past it the oldest go, which only means a
/// stale password may be tried once more.
const MAX_REMEMBERED: usize = 256;

type Fingerprint = [u8; 32];

pub(crate) struct RejectedPasswords {
    key: Zeroizing<[u8; 32]>,
    seen: Mutex<(HashSet<Fingerprint>, Vec<Fingerprint>)>,
    /// One sign-in at a time spends a given password, so panes that
    /// reconnect together hear the first one's verdict instead of each
    /// spending a try. An entry lives while someone holds or waits for it.
    turns: Mutex<HashMap<Fingerprint, Arc<tokio::sync::Mutex<()>>>>,
}

impl std::fmt::Debug for RejectedPasswords {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RejectedPasswords").finish_non_exhaustive()
    }
}

impl Default for RejectedPasswords {
    fn default() -> Self {
        let mut key = Zeroizing::new([0_u8; 32]);
        // Without the OS's randomness nothing else here works either; a
        // fixed key would make the fingerprints plain hashes, so fail
        // loudly instead.
        rand::rngs::SysRng
            .try_fill_bytes(&mut *key)
            .expect("the system random number generator");
        Self {
            key,
            seen: Mutex::default(),
            turns: Mutex::default(),
        }
    }
}

impl RejectedPasswords {
    fn fingerprint(&self, user: &str, password: &Secret) -> Fingerprint {
        let mut mac =
            <Hmac<Sha256> as KeyInit>::new_from_slice(&*self.key).expect("any key length");
        mac.update(user.as_bytes());
        mac.update(&[0]);
        mac.update(password.as_str().as_bytes());
        mac.finalize().into_bytes().into()
    }

    /// Waits until no other sign-in is spending this password, and holds
    /// that turn until the guard drops: after the server's verdict, once
    /// it is recorded. O(t) for t passwords in use at once.
    pub(crate) async fn turn(
        &self,
        user: &str,
        password: &Secret,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let print = self.fingerprint(user, password);
        let lock = {
            let mut turns = self.turns.lock().unwrap_or_else(PoisonError::into_inner);
            turns.retain(|_, lock| Arc::strong_count(lock) > 1);
            Arc::clone(turns.entry(print).or_default())
        };
        lock.lock_owned().await
    }

    /// Whether a server turned down this password for this user before.
    pub(crate) fn contains(&self, user: &str, password: &Secret) -> bool {
        let print = self.fingerprint(user, password);
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .0
            .contains(&print)
    }

    /// Forgets a rejection: the password was taken after all.
    pub(crate) fn remove(&self, user: &str, password: &Secret) {
        let print = self.fingerprint(user, password);
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let (set, order) = &mut *seen;
        if set.remove(&print) {
            order.retain(|p| *p != print);
        }
    }

    pub(crate) fn insert(&self, user: &str, password: &Secret) {
        let print = self.fingerprint(user, password);
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let (set, order) = &mut *seen;
        if set.insert(print) {
            order.push(print);
            if order.len() > MAX_REMEMBERED {
                let oldest = order.remove(0);
                set.remove(&oldest);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejected_password_is_known_by_user_and_password_only() {
        let rejected = RejectedPasswords::default();
        let old = Secret::new("hunter2".into());
        rejected.insert("ops", &old);
        assert!(rejected.contains("ops", &old));
        assert!(!rejected.contains("ops", &Secret::new("hunter3".into())));
        assert!(!rejected.contains("root", &old));
        rejected.remove("ops", &old);
        assert!(!rejected.contains("ops", &old));
        // Another run's key: nothing carries over.
        assert!(!RejectedPasswords::default().contains("ops", &old));
    }

    #[test]
    fn it_forgets_the_oldest_past_its_bound() {
        let rejected = RejectedPasswords::default();
        let first = Secret::new("p0".into());
        rejected.insert("u", &first);
        for i in 1..=MAX_REMEMBERED {
            rejected.insert("u", &Secret::new(format!("p{i}")));
        }
        assert!(!rejected.contains("u", &first));
        assert!(rejected.contains("u", &Secret::new(format!("p{MAX_REMEMBERED}"))));
    }
}
