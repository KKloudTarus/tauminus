// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

fn op(reference: &str) -> SecretRef {
    SecretRef::OnePassword {
        reference: reference.into(),
    }
}

fn vault(address: &str, mount: &str, path: &str, field: &str) -> SecretRef {
    SecretRef::Vault {
        address: address.into(),
        mount: mount.into(),
        path: path.into(),
        field: field.into(),
    }
}

#[test]
fn a_1password_reference_names_a_field_and_nothing_a_shell_reads() {
    for good in [
        "op://Private/db prod/password",
        "op://Ops/web-1/ssh keys/private key",
        "op://a/b/c",
    ] {
        assert_eq!(op(good).check(SecretSlot::Password), Ok(()), "{good}");
    }
    for bad in [
        "op://Private/db/password?attribute=otp",
        "op://Private/db",
        "op://Private/a/b/c/d",
        "op://Private/../password",
        "op://Private/-db/password",
        "op://Private/db/pass\"word",
        "op://Private/db/pass%word",
        "op://Private/db/pass&word",
        "op://Private/db/ password",
        "op://Private//password",
        "https://Private/db/password",
        "--out-file=/tmp/x",
    ] {
        assert_eq!(
            op(bad).check(SecretSlot::Password),
            Err(SecretRefError::OnePassword),
            "{bad}"
        );
    }
    assert!(
        op(&format!("op://v/i/{}", "x".repeat(129)))
            .check(SecretSlot::Password)
            .is_err()
    );
}

#[test]
fn a_bitwarden_reference_is_an_item_id_for_a_password() {
    let item = SecretRef::Bitwarden {
        item: "3f2504e0-4f89-11d3-9a0c-0305e82c3301".into(),
    };
    assert_eq!(item.check(SecretSlot::Password), Ok(()));
    assert_eq!(item.check(SecretSlot::Passphrase), Ok(()));
    assert_eq!(
        item.check(SecretSlot::PrivateKey),
        Err(SecretRefError::BitwardenKey)
    );
    for bad in [
        "github",
        "--session",
        "3f2504e0-4f89-11d3-9a0c-0305e82c330",
        "3f2504e0-4f89-11d3-9a0c-0305e82c3301x",
        "3f2504e04f8911d39a0c0305e82c3301",
        "3f2504e0-4f89-11d3-9a0c-0305e82c33&1",
    ] {
        let item = SecretRef::Bitwarden { item: bad.into() };
        assert_eq!(
            item.check(SecretSlot::Password),
            Err(SecretRefError::BitwardenItem),
            "{bad}"
        );
    }
}

#[test]
fn a_vault_reference_reaches_a_secure_address_and_stays_in_its_mount() {
    for (address, ok) in [
        ("https://vault.example.com", true),
        ("https://vault.example.com:8200", true),
        ("https://[2001:db8::1]:8200", true),
        ("http://127.0.0.1:8200", true),
        ("http://localhost:8200", true),
        ("http://[::1]", true),
        ("http://vault.example.com:8200", false),
        ("http://127.example.com", false),
        ("https://vault.example.com/v1", false),
        ("https://vault.example.com:82000", false),
        ("https://vault.example.com:", false),
        ("https://user@vault.example.com", false),
        ("https://-vault", false),
        ("https://[::1", false),
        ("ftp://vault.example.com", false),
        ("-address=https://evil", false),
    ] {
        let reference = vault(address, "secret", "ops/db", "password");
        assert_eq!(
            reference.check(SecretSlot::Password).is_ok(),
            ok,
            "{address}"
        );
    }
    for (mount, path, field) in [
        ("secret", "../sys/policy", "password"),
        ("secret", "ops/db", "-format=json"),
        ("-mount=x", "ops/db", "password"),
        ("secret", "/ops/db", "password"),
        ("secret", "ops db", "password"),
        ("secret", "ops/db", "pass/word"),
        ("", "ops/db", "password"),
    ] {
        assert_eq!(
            vault("https://v.example.com", mount, path, field).check(SecretSlot::Password),
            Err(SecretRefError::VaultName),
            "{mount} {path} {field}"
        );
    }
}

#[test]
fn each_reference_runs_one_fixed_command() {
    let key = op("op://Ops/web-1/private key")
        .command(SecretSlot::PrivateKey)
        .unwrap();
    assert_eq!(key.program, "op");
    assert_eq!(
        key.args,
        [
            "read",
            "--no-newline",
            "--",
            "op://Ops/web-1/private key?ssh-format=openssh"
        ]
    );
    let password = op("op://Ops/web-1/password")
        .command(SecretSlot::Password)
        .unwrap();
    assert_eq!(password.args[3], "op://Ops/web-1/password");

    let bw = SecretRef::Bitwarden {
        item: "3f2504e0-4f89-11d3-9a0c-0305e82c3301".into(),
    }
    .command(SecretSlot::Password)
    .unwrap();
    assert_eq!(
        (bw.program, bw.args),
        (
            "bw",
            vec![
                "get".to_owned(),
                "password".to_owned(),
                "3f2504e0-4f89-11d3-9a0c-0305e82c3301".to_owned(),
                "--nointeraction".to_owned()
            ]
        )
    );

    let kv = vault("https://v.example.com:8200", "secret", "ops/db", "password")
        .command(SecretSlot::Password)
        .unwrap();
    assert_eq!(kv.program, "vault");
    assert_eq!(
        kv.args,
        [
            "kv",
            "get",
            "-address=https://v.example.com:8200",
            "-mount=secret",
            "-field=password",
            "--",
            "ops/db"
        ]
    );
    // A reference that fails its check runs nothing.
    assert!(op("op://x").command(SecretSlot::Password).is_err());
}
