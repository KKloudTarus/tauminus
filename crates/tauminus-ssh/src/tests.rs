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

/// Base64 both ways, for patching a key's bytes in a test.
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ABC[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn unbase64(text: &str) -> Vec<u8> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let digits: Vec<u8> = text
        .bytes()
        .filter(|c| c.is_ascii_alphanumeric() || *c == b'+' || *c == b'/')
        .collect();
    let mut out = Vec::new();
    for chunk in digits.chunks(4) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, c)| n | u32::from(value(*c)) << (18 - 6 * i));
        for i in 0..chunk.len() - 1 {
            out.push(n.to_be_bytes()[1 + i]);
        }
    }
    out
}

#[test]
fn a_key_from_elsewhere_with_a_costly_kdf_is_refused_before_it_runs() {
    use russh::keys::ssh_key::{LineEnding, PrivateKey};
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let pem = key
        .encrypt(&mut rand::rng(), "pass phrase")
        .unwrap()
        .to_openssh(LineEnding::LF)
        .unwrap();
    assert!(public_key_line_bounded(&pem, Some("pass phrase")).is_ok());
    // ssh-keygen's 16 rounds sit after the salt in the KDF's options.
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let mut bytes = unbase64(&body);
    let at = bytes.windows(6).position(|w| w == b"bcrypt").unwrap() + 6 + 4 + 4 + 16;
    assert_eq!(bytes[at..at + 4], 16u32.to_be_bytes());
    bytes[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    let costly = format!(
        "-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----\n",
        base64(&bytes)
    );
    let started = std::time::Instant::now();
    let refused = public_key_line_bounded(&costly, Some("pass phrase")).unwrap_err();
    assert!(refused.contains("4294967295 rounds"), "{refused}");
    assert!(started.elapsed() < Duration::from_secs(1));
    let pkcs8 =
        "-----BEGIN ENCRYPTED PRIVATE KEY-----\nMIIB\n-----END ENCRYPTED PRIVATE KEY-----\n";
    assert!(
        public_key_line_bounded(pkcs8, Some("x"))
            .unwrap_err()
            .contains("PKCS#8")
    );
}
