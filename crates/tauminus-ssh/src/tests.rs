// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use russh::keys::EcdsaCurve;
use russh::keys::HashAlg;

use super::*;

#[test]
fn recorded_types_go_first_and_sha1_rsa_is_never_offered() {
    let ecdsa = Algorithm::Ecdsa {
        curve: EcdsaCurve::NistP256,
    };
    let keys = preferred(std::slice::from_ref(&ecdsa), false).key;
    assert_eq!(keys.first(), Some(&ecdsa));
    assert!(!keys.contains(&Algorithm::Rsa { hash: None }));

    // An RSA key on record brings every SHA-2 RSA signature type forward.
    let keys = preferred(&[Algorithm::Rsa { hash: None }], false).key;
    assert_eq!(
        &keys[..2],
        [
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha512)
            },
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha256)
            }
        ]
    );
}

#[test]
fn nothing_recorded_keeps_the_default_order() {
    let keys = preferred(&[], false).key;
    let default: Vec<_> = russh::Preferred::DEFAULT
        .key
        .iter()
        .filter(|a| !matches!(a, Algorithm::Rsa { hash: None }))
        .cloned()
        .collect();
    assert_eq!(keys.as_ref(), default.as_slice());
}

#[test]
fn certificate_algorithms_only_when_an_authority_exists() {
    assert!(preferred(&[], false).host_key_certificates.is_empty());
    assert_eq!(
        preferred(&[], true).host_key_certificates,
        preferred(&[], true).key
    );
}
