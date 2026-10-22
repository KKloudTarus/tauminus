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
