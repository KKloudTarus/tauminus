// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! USB security keys over CTAP2 HID (`ctap-hid-fido2`), on Linux and
//! macOS. Windows lets only `webauthn.dll` reach them from a process that
//! is not elevated: there, keys go through an agent (ADR 0027).
//!
//! Only bytes move here: the request as CTAP takes it, the answer as CTAP
//! gives it. Reading the answer is the SSH adapter's, where tests check
//! it against a real server. Not tested in CI, which has no key.

use std::sync::{Arc, Mutex, PoisonError};

use ctap_hid_fido2::fidokey::GetAssertionArgsBuilder;
use ctap_hid_fido2::{FidoKeyHid, FidoKeyHidFactory, HidParam, LibCfg};

use crate::{Assertion, AssertionRequest, SecurityKey, SecurityKeyError};

/// The security keys plugged in, asked in turn. One request runs at a
/// time: two sign-ins at once would cross their reports on one key, and
/// one touch answers one of them.
#[derive(Debug, Default)]
pub struct HidSecurityKey {
    busy: Mutex<()>,
}

fn config() -> LibCfg {
    // The touch is shown by the app, not printed by the library.
    LibCfg {
        enable_keep_alive_msg: false,
        keep_alive_msg: String::new(),
        ..LibCfg::init()
    }
}

/// What a CTAP error the library reports as text means here.
fn error(error: &anyhow::Error) -> SecurityKeyError {
    let text = format!("{error:#}");
    let has = |code: &str| text.contains(code);
    if has("CTAP2_ERR_NO_CREDENTIALS") {
        SecurityKeyError::UnknownCredential
    } else if has("CTAP2_ERR_PIN_REQUIRED") {
        SecurityKeyError::PinRequired
    } else if has("CTAP2_ERR_PIN_INVALID") {
        SecurityKeyError::PinInvalid { retries: None }
    } else if has("CTAP2_ERR_PIN_BLOCKED") || has("CTAP2_ERR_PIN_AUTH_BLOCKED") {
        SecurityKeyError::PinBlocked
    } else if has("CTAP2_ERR_USER_ACTION_TIMEOUT") || has("CTAP2_ERR_OPERATION_DENIED") {
        SecurityKeyError::NotConfirmed
    } else {
        SecurityKeyError::Device(text)
    }
}

/// Opens the key, and lets the request's cancel stop it: `CTAPHID_CANCEL`
/// goes between two keep-alive reads, which take the device lock in turn.
fn open(
    param: &HidParam,
    request: &AssertionRequest<'_>,
) -> Result<Arc<FidoKeyHid>, SecurityKeyError> {
    let key = Arc::new(
        FidoKeyHidFactory::create_by_params(std::slice::from_ref(param), &config())
            .map_err(|e| error(&e))?,
    );
    let stop = Arc::clone(&key);
    request.cancel.on_cancel(Arc::new(move || {
        let _ = stop.cancel_selection();
    }))?;
    Ok(key)
}

/// Asks `key` for the assertion, with a touch and a PIN as the request
/// says.
fn ask(key: &FidoKeyHid, request: &AssertionRequest<'_>) -> Result<Assertion, SecurityKeyError> {
    let mut args = GetAssertionArgsBuilder::new(request.rp_id, request.client_data)
        .credential_id(request.credential_id);
    args = match request.pin {
        Some(pin) => args.pin(pin.as_str()),
        None => args.without_pin_and_uv(),
    };
    if !request.user_presence {
        args = args.without_up();
    }
    let mut assertions = key
        .get_assertion_with_args(&args.build())
        .map_err(|e| error(&e))?;
    if assertions.is_empty() {
        return Err(SecurityKeyError::Device("no assertion".into()));
    }
    let assertion = assertions.swap_remove(0);
    Ok(Assertion {
        authenticator_data: assertion.auth_data,
        signature: assertion.signature,
    })
}

/// Whether `key` holds the credential: a silent assertion (no touch, no
/// PIN), as OpenSSH probes, so only the key that has it blinks. Only a
/// signed answer counts: a key that errs for any other reason (busy,
/// options it lacks) never gets the PIN or a touch request.
fn holds(key: &FidoKeyHid, request: &AssertionRequest<'_>) -> bool {
    let probe = GetAssertionArgsBuilder::new(request.rp_id, request.client_data)
        .credential_id(request.credential_id)
        .without_pin_and_uv()
        .without_up()
        .build();
    key.get_assertion_with_args(&probe).is_ok()
}

impl SecurityKey for HidSecurityKey {
    fn get_assertion(&self, request: AssertionRequest<'_>) -> Result<Assertion, SecurityKeyError> {
        let _one = self.busy.lock().unwrap_or_else(PoisonError::into_inner);
        let devices = ctap_hid_fido2::get_fidokey_devices();
        let answer = match devices.as_slice() {
            [] => Err(SecurityKeyError::NoDevice),
            // A PIN goes only to a key that holds the credential: a wrong
            // key would count it against its own retries.
            [only] => {
                let key = open(&only.param, &request)?;
                if request.pin.is_some() && !holds(&key, &request) {
                    return Err(SecurityKeyError::UnknownCredential);
                }
                ask(&key, &request)
            }
            several => {
                for device in several {
                    let Ok(key) = open(&device.param, &request) else {
                        if request.cancel.is_cancelled() {
                            return Err(SecurityKeyError::NotConfirmed);
                        }
                        continue;
                    };
                    if holds(&key, &request) {
                        match ask(&key, &request) {
                            Err(SecurityKeyError::UnknownCredential) => {}
                            answer => return answer,
                        }
                    }
                }
                Err(SecurityKeyError::UnknownCredential)
            }
        };
        // A cancelled request answers nothing, whatever the key said.
        if request.cancel.is_cancelled() {
            return Err(SecurityKeyError::NotConfirmed);
        }
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The library's messages, as `ctapdef::get_ctap_status_message`
    /// writes them, and what each means here.
    #[test]
    fn ctap_errors_map_by_their_code() {
        for (text, expected) in [
            (
                "0x2E CTAP2_ERR_NO_CREDENTIALS    No valid credentials provided.",
                SecurityKeyError::UnknownCredential,
            ),
            (
                "0x36 CTAP2_ERR_PIN_REQUIRED  PIN is required for the selected operation.",
                SecurityKeyError::PinRequired,
            ),
            (
                "0x31 CTAP2_ERR_PIN_INVALID   PIN Invalid.",
                SecurityKeyError::PinInvalid { retries: None },
            ),
            (
                "0x32 CTAP2_ERR_PIN_BLOCKED PIN Blocked.",
                SecurityKeyError::PinBlocked,
            ),
            (
                "0x34 CTAP2_ERR_PIN_AUTH_BLOCKED PIN authentication, pinAuth, blocked.",
                SecurityKeyError::PinBlocked,
            ),
            (
                "0x2F CTAP2_ERR_USER_ACTION_TIMEOUT   Timeout waiting for user interaction.",
                SecurityKeyError::NotConfirmed,
            ),
            (
                "0x27 CTAP2_ERR_OPERATION_DENIED  Not authorized for requested operation.",
                SecurityKeyError::NotConfirmed,
            ),
        ] {
            assert_eq!(error(&anyhow::anyhow!(text)), expected, "{text}");
        }
        assert!(matches!(
            error(&anyhow::anyhow!("read error")),
            SecurityKeyError::Device(_)
        ));
    }
}
