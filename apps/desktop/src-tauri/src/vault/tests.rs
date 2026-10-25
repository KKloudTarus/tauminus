// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

#[test]
fn vault_errors_map_to_stable_codes() {
    let code = |error: VaultError| CommandError::from(error).code;
    assert_eq!(code(VaultError::Locked), ErrorCode::VaultLocked);
    assert_eq!(code(VaultError::WrongKey), ErrorCode::WrongPassword);
    assert_eq!(code(VaultError::Missing), ErrorCode::NotFound);
    assert_eq!(
        code(VaultError::KeychainUnavailable("x".into())),
        ErrorCode::Unavailable
    );
    assert_eq!(
        code(VaultError::Corrupt("x".into())),
        ErrorCode::StoreFailed
    );
    for (error, expected) in [
        (PresenceError::Cancelled, ErrorCode::Cancelled),
        (PresenceError::TimedOut, ErrorCode::Cancelled),
        (PresenceError::Denied, ErrorCode::AuthFailed),
        (
            PresenceError::Unavailable("x".into()),
            ErrorCode::Unavailable,
        ),
    ] {
        assert_eq!(code(VaultError::Presence(error)), expected);
    }
}

#[test]
fn the_status_says_why_the_presence_check_cannot_run() {
    let status = VaultStatusSpec::from(VaultStatus {
        exists: true,
        unlocked: false,
        keychain: KeychainState::On { presence: true },
        presence: tauminus_app::vault::PresenceState {
            name: "Touch ID".into(),
            available: Err("no sensor".into()),
        },
        idle: Duration::from_mins(15),
    });
    assert!(matches!(
        status.keychain,
        KeychainSpec::On { presence: true }
    ));
    assert_eq!(status.presence.name, "Touch ID");
    assert_eq!(status.presence.unavailable.as_deref(), Some("no sensor"));
}

#[test]
fn an_edit_never_shows_its_secrets_in_debug() {
    let edit = IdentityEdit {
        id: None,
        name: "prod".into(),
        user: None,
        password: Some("hunter2".into()),
        private_key: Some("-----BEGIN OPENSSH PRIVATE KEY-----".into()),
        passphrase: Some("swordfish".into()),
        generate: None,
        sync: None,
    };
    let shown = format!("{edit:?}");
    assert!(
        !shown.contains("hunter2") && !shown.contains("swordfish") && !shown.contains("OPENSSH")
    );
}

#[test]
fn a_key_that_does_not_open_is_refused_when_saved() {
    assert_eq!(
        public_key_of("not a key", None).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
}

#[test]
fn every_key_kind_maps_to_the_domain() {
    use tauminus_domain::KeyKind;
    assert_eq!(KeyKind::from(KeyKindSpec::Ed25519), KeyKind::Ed25519);
    assert_eq!(KeyKind::from(KeyKindSpec::Rsa4096), KeyKind::Rsa4096);
    let parsed: KeyKindSpec = serde_json::from_str("\"ecdsa-p256\"").unwrap();
    assert_eq!(KeyKind::from(parsed), KeyKind::EcdsaP256);
}
