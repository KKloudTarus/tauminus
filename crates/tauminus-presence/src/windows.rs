// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Windows Hello through `UserConsentVerifier`: face, fingerprint, or the
//! Hello PIN. A desktop app asks for the prompt over its own window, so it
//! opens in front.

#![expect(
    unsafe_code,
    reason = "the HWND interop call that puts the Hello prompt over the app's window"
)]

use std::sync::mpsc;
use std::time::Duration;

use tauminus_app::vault::PresenceError;
use windows::Security::Credentials::UI::{
    UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::IUserConsentVerifierInterop;
use windows::core::HSTRING;
use windows_future::IAsyncOperation;

pub(crate) fn available() -> Result<(), String> {
    let availability = UserConsentVerifier::CheckAvailabilityAsync()
        .and_then(|op| op.join())
        .map_err(|e| e.message())?;
    match availability {
        UserConsentVerifierAvailability::Available => Ok(()),
        UserConsentVerifierAvailability::DeviceNotPresent => Err("no Windows Hello device".into()),
        UserConsentVerifierAvailability::NotConfiguredForUser => {
            Err("Windows Hello is not set up for this user".into())
        }
        UserConsentVerifierAvailability::DisabledByPolicy => {
            Err("Windows Hello is turned off by policy".into())
        }
        UserConsentVerifierAvailability::DeviceBusy => {
            Err("the Windows Hello device is busy".into())
        }
        other => Err(format!("Windows Hello is unavailable ({})", other.0)),
    }
}

pub(crate) fn confirm(
    reason: &str,
    window: Option<isize>,
    timeout: Duration,
) -> Result<(), PresenceError> {
    let message = HSTRING::from(reason);
    let operation =
        request(&message, window).map_err(|e| PresenceError::Unavailable(e.message()))?;
    let (send, answer) = mpsc::sync_channel(1);
    let waiting = operation.clone();
    std::thread::spawn(move || {
        let _ = send.try_send(waiting.join());
    });
    match answer.recv_timeout(timeout) {
        Ok(Ok(result)) => verdict(result),
        Ok(Err(e)) => Err(PresenceError::Unavailable(e.message())),
        Err(_) => {
            let _ = operation.Cancel();
            Err(PresenceError::TimedOut)
        }
    }
}

fn request(
    message: &HSTRING,
    window: Option<isize>,
) -> windows::core::Result<IAsyncOperation<UserConsentVerificationResult>> {
    let Some(window) = window else {
        return UserConsentVerifier::RequestVerificationAsync(message);
    };
    let interop = windows::core::factory::<UserConsentVerifier, IUserConsentVerifierInterop>()?;
    // SAFETY: `window` is the app's own top-level window handle, which
    // lives as long as the app; the message outlives the call.
    unsafe { interop.RequestVerificationForWindowAsync(HWND(window as _), message) }
}

fn verdict(result: UserConsentVerificationResult) -> Result<(), PresenceError> {
    match result {
        UserConsentVerificationResult::Verified => Ok(()),
        UserConsentVerificationResult::Canceled => Err(PresenceError::Cancelled),
        UserConsentVerificationResult::RetriesExhausted => Err(PresenceError::Denied),
        UserConsentVerificationResult::DeviceNotPresent => {
            Err(PresenceError::Unavailable("no Windows Hello device".into()))
        }
        UserConsentVerificationResult::NotConfiguredForUser => Err(PresenceError::Unavailable(
            "Windows Hello is not set up for this user".into(),
        )),
        UserConsentVerificationResult::DisabledByPolicy => Err(PresenceError::Unavailable(
            "Windows Hello is turned off by policy".into(),
        )),
        UserConsentVerificationResult::DeviceBusy => Err(PresenceError::Unavailable(
            "the Windows Hello device is busy".into(),
        )),
        other => Err(PresenceError::Unavailable(format!(
            "Windows Hello answered {}",
            other.0
        ))),
    }
}
