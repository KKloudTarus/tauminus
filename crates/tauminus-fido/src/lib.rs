// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! FIDO2 security keys as SSH signs with them (ADR 0027): a CTAP2
//! `GetAssertion` for a credential an OpenSSH `sk-` key file names.
//!
//! The port carries what CTAP carries, raw: the authenticator data and the
//! signature as the key returns them. Reading them (the flags, the
//! counter, an ECDSA signature's DER) happens in the SSH adapter, where a
//! test can check it against a real server; an adapter here only moves
//! bytes to and from a device.

use std::sync::{Arc, Mutex, PoisonError};

use zeroize::Zeroizing;

#[cfg(not(windows))]
mod hid;

#[cfg(not(windows))]
pub use hid::HidSecurityKey;

/// What to ask a security key for.
#[derive(Debug)]
pub struct AssertionRequest<'a> {
    /// The relying party: the key file's application, `ssh:` by default.
    pub rp_id: &'a str,
    /// The data to sign: the key signs over its SHA-256, the client data
    /// hash, as `libfido2` and `webauthn.dll` take it.
    pub client_data: &'a [u8],
    /// The key file's key handle.
    pub credential_id: &'a [u8],
    /// Whether the key must see a touch (the key file's flag 0x01).
    pub user_presence: bool,
    /// The key's PIN, when its flags ask for user verification (0x04).
    pub pin: Option<&'a Zeroizing<String>>,
    /// Set once the caller no longer wants the answer.
    pub cancel: &'a Cancel,
}

type Stop = Arc<dyn Fn() + Send + Sync>;

/// Ends a request the caller gave up on, as when the user cancelled the
/// touch: the adapter registers how to stop its device, and `cancel` runs
/// it, so the key stops waiting for a touch at once instead of at its own
/// timeout, holding the device until then.
#[derive(Default)]
pub struct Cancel(Mutex<(bool, Option<Stop>)>);

impl std::fmt::Debug for Cancel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Cancel").field(&self.is_cancelled()).finish()
    }
}

impl Cancel {
    /// Cancels the request: stops the device registered, if any.
    pub fn cancel(&self) {
        let stop = {
            let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            state.0 = true;
            state.1.take()
        };
        if let Some(stop) = stop {
            stop();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).0
    }

    /// Registers how to stop the device now asked, replacing the last one.
    /// Already cancelled: returns `NotConfirmed`, and the device is not
    /// asked.
    pub fn on_cancel(&self, stop: Stop) -> Result<(), SecurityKeyError> {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.0 {
            return Err(SecurityKeyError::NotConfirmed);
        }
        state.1 = Some(stop);
        Ok(())
    }
}

/// What a security key answered, as CTAP returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assertion {
    /// SHA-256 of the relying party, a flags byte, a big-endian counter,
    /// and any extensions.
    pub authenticator_data: Vec<u8>,
    /// Over the authenticator data and the client data hash (SHA-256 of
    /// the client data): raw for Ed25519, DER for ECDSA P-256.
    pub signature: Vec<u8>,
}

/// Why a key gave no assertion.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecurityKeyError {
    #[error("no security key is plugged in")]
    NoDevice,
    #[error("no security key here holds this credential")]
    UnknownCredential,
    #[error("the security key wants its PIN")]
    PinRequired,
    #[error("the PIN is wrong")]
    PinInvalid { retries: Option<u8> },
    #[error("the security key's PIN is blocked")]
    PinBlocked,
    #[error("the security key was not touched in time")]
    NotConfirmed,
    #[error("the security key failed: {0}")]
    Device(String),
}

/// A FIDO2 security key, or whatever stands in for one.
pub trait SecurityKey: Send + Sync + std::fmt::Debug + 'static {
    /// One request at a time per adapter: a key answers one touch.
    /// Blocks until the key answers or times out: callers run it off the
    /// async runtime.
    fn get_assertion(&self, request: AssertionRequest<'_>) -> Result<Assertion, SecurityKeyError>;
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn cancel_stops_the_device_registered_once_and_refuses_later_ones() {
        let stopped = Arc::new(AtomicUsize::new(0));
        let cancel = Cancel::default();
        let count = Arc::clone(&stopped);
        cancel
            .on_cancel(Arc::new(move || {
                count.fetch_add(1, Ordering::Relaxed);
            }))
            .unwrap();
        assert!(!cancel.is_cancelled());
        cancel.cancel();
        cancel.cancel();
        assert_eq!(stopped.load(Ordering::Relaxed), 1);
        assert!(cancel.is_cancelled());
        assert_eq!(
            cancel.on_cancel(Arc::new(|| {})),
            Err(SecurityKeyError::NotConfirmed)
        );
    }
}
