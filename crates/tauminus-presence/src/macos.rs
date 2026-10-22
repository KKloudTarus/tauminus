// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! `LocalAuthentication`: Touch ID, or the login password where there is no
//! sensor or it is locked out (`LAPolicyDeviceOwnerAuthentication`).

#![expect(
    unsafe_code,
    reason = "LocalAuthentication through objc2, whose generated bindings are unsafe"
)]

use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LABiometryType, LAContext, LAError, LAPolicy};
use tauminus_app::vault::PresenceError;

const POLICY: LAPolicy = LAPolicy::DeviceOwnerAuthentication;

pub(crate) fn available() -> Result<(), String> {
    // SAFETY: a fresh context, used on this thread only, asked nothing.
    let context = unsafe { LAContext::new() };
    // SAFETY: as above; the policy is one LocalAuthentication defines.
    unsafe { context.canEvaluatePolicy_error(POLICY) }
        .map_err(|error| error.localizedDescription().to_string())
}

/// "Touch ID" on a Mac whose sensor can answer, else what answers instead:
/// the login password (or a paired Apple Watch).
pub(crate) fn name() -> String {
    // SAFETY: a fresh context, used on this thread only, asked nothing.
    let context = unsafe { LAContext::new() };
    // `biometryType` is set by `canEvaluatePolicy`, for a biometric policy.
    // SAFETY: as above.
    let biometric = unsafe {
        context.canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthenticationWithBiometrics)
    }
    .is_ok();
    // SAFETY: reading a property of our own context.
    if biometric && unsafe { context.biometryType() } == LABiometryType::TouchID {
        "Touch ID".into()
    } else {
        "your Mac login password".into()
    }
}

pub(crate) fn confirm(reason: &str, timeout: Duration) -> Result<(), PresenceError> {
    // A fresh context each time, which reuses no earlier Touch ID match.
    // SAFETY: a fresh context, kept alive until the evaluation answers or
    // is invalidated below.
    let context = unsafe { LAContext::new() };
    // SAFETY: setting a documented property on our own context.
    unsafe { context.setTouchIDAuthenticationAllowableReuseDuration(0.0) };
    let (send, answer) = mpsc::sync_channel::<Option<isize>>(1);
    // The reply runs once, on a private queue: it sends the error code, or
    // `None` for success. A `SyncSender` is `Send`, as the block must be.
    let reply = RcBlock::new(move |ok: Bool, error: *mut NSError| {
        let code = if ok.as_bool() {
            None
        } else {
            // SAFETY: LocalAuthentication passes a valid NSError or null.
            Some(unsafe { error.as_ref() }.map_or(LAError::AuthenticationFailed.0, NSError::code))
        };
        let _ = send.try_send(code);
    });
    let reason = NSString::from_str(reason);
    // SAFETY: the context outlives the call; `reason` is non-empty; the
    // reply block is sendable.
    unsafe { context.evaluatePolicy_localizedReason_reply(POLICY, &reason, &reply) };
    match answer.recv_timeout(timeout) {
        Ok(None) => Ok(()),
        Ok(Some(code)) => Err(error(code)),
        Err(_) => {
            // Ends the prompt; its reply then fails with AppCancel.
            // SAFETY: invalidating our own context.
            unsafe { context.invalidate() };
            Err(PresenceError::TimedOut)
        }
    }
}

fn error(code: isize) -> PresenceError {
    match LAError(code) {
        LAError::UserCancel | LAError::SystemCancel | LAError::AppCancel => {
            PresenceError::Cancelled
        }
        LAError::AuthenticationFailed | LAError::UserFallback => PresenceError::Denied,
        LAError::PasscodeNotSet | LAError::NotInteractive => {
            PresenceError::Unavailable(format!("LocalAuthentication error {code}"))
        }
        _ => PresenceError::Unavailable(format!("LocalAuthentication error {code}")),
    }
}
