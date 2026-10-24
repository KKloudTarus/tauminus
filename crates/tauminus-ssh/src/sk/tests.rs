// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use ed25519_dalek::Signer as _;
use russh::keys::ssh_key::public::{
    EcdsaPublicKey, Ed25519PublicKey, SkEcdsaSha2NistP256, SkEd25519,
};

use super::*;

const APP: &str = "ssh:";
const COUNTER: u32 = 0x1234_5678;

/// What a security key returns: authenticator data, then its signature
/// over that and the client data hash.
fn auth_data(application: &str, flags: u8, counter: u32) -> Vec<u8> {
    let mut auth = Sha256::digest(application.as_bytes()).to_vec();
    auth.push(flags);
    auth.extend_from_slice(&counter.to_be_bytes());
    auth
}

fn ed25519() -> (ed25519_dalek::SigningKey, KeyData) {
    let key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
    let public = SkEd25519::new(Ed25519PublicKey(key.verifying_key().to_bytes()), APP);
    (key, KeyData::SkEd25519(public))
}

fn p256() -> (p256::ecdsa::SigningKey, KeyData) {
    let key = p256::ecdsa::SigningKey::from_bytes(&[7; 32].into()).unwrap();
    let point = key.verifying_key().to_sec1_point(false);
    let EcdsaPublicKey::NistP256(point) =
        EcdsaPublicKey::from_sec1_bytes(point.as_bytes()).unwrap()
    else {
        unreachable!("a P-256 point")
    };
    let public = SkEcdsaSha2NistP256::new(point, APP);
    (key, KeyData::SkEcdsaSha2NistP256(public))
}

fn ed25519_assertion(key: &ed25519_dalek::SigningKey, auth: Vec<u8>, data: &[u8]) -> Assertion {
    let mut signed = auth.clone();
    signed.extend_from_slice(&client_data_hash(data));
    Assertion {
        authenticator_data: auth,
        signature: key.sign(&signed).to_bytes().to_vec(),
    }
}

fn p256_assertion(key: &p256::ecdsa::SigningKey, auth: Vec<u8>, data: &[u8]) -> Assertion {
    let mut signed = auth.clone();
    signed.extend_from_slice(&client_data_hash(data));
    let signature: p256::ecdsa::Signature = signature::Signer::sign(key, &signed);
    Assertion {
        authenticator_data: auth,
        signature: signature.to_der().as_bytes().to_vec(),
    }
}

#[test]
fn an_assertion_becomes_the_ssh_signature_and_verifies() {
    let data = b"session id and request";
    let (key, public) = ed25519();
    let assertion = ed25519_assertion(&key, auth_data(APP, 0x01, COUNTER), data);
    let signature = ssh_signature(&public, APP, data, &assertion).unwrap();
    assert_eq!(signature.algorithm(), Algorithm::SkEd25519);
    // The key's 64 bytes, the flags, the counter big-endian.
    assert_eq!(&signature.as_bytes()[64..], &[0x01, 0x12, 0x34, 0x56, 0x78]);

    let (key, public) = p256();
    let assertion = p256_assertion(&key, auth_data(APP, 0x05, COUNTER), data);
    let signature = ssh_signature(&public, APP, data, &assertion).unwrap();
    assert_eq!(signature.algorithm(), Algorithm::SkEcdsaSha2NistP256);
    assert!(
        signature
            .as_bytes()
            .ends_with(&[0x05, 0x12, 0x34, 0x56, 0x78])
    );
}

#[test]
fn each_wrong_answer_fails_by_name() {
    let data = b"session id and request";
    let (key, public) = ed25519();
    let refused = |assertion: &Assertion| match ssh_signature(&public, APP, data, assertion) {
        Err(SkError::Answer(why)) => why,
        other => panic!("{other:?}"),
    };
    // For another application.
    let other = ed25519_assertion(&key, auth_data("ssh:other", 0x01, COUNTER), data);
    assert!(refused(&other).contains("another application"));
    // Over other data.
    let stale = ed25519_assertion(&key, auth_data(APP, 0x01, COUNTER), b"other");
    assert!(refused(&stale).contains("does not verify"));
    // A counter read little-endian would not match what was signed.
    let mut swapped = ed25519_assertion(&key, auth_data(APP, 0x01, COUNTER), data);
    swapped.authenticator_data[33..37].reverse();
    assert!(refused(&swapped).contains("does not verify"));
    // Cut short.
    let short = Assertion {
        authenticator_data: vec![0; 36],
        signature: vec![0; 64],
    };
    assert!(refused(&short).contains("too short"));

    // ECDSA's DER, not passed through as it is.
    let (key, public) = p256();
    let mut raw = p256_assertion(&key, auth_data(APP, 0x01, COUNTER), data);
    raw.signature.truncate(64);
    assert!(matches!(
        ssh_signature(&public, APP, data, &raw),
        Err(SkError::Answer(why)) if why.contains("DER")
    ));
}

/// Answers as a key holding every credential would; counts the asks.
#[derive(Debug, Default)]
struct Counting(std::sync::atomic::AtomicUsize);

impl SecurityKey for Counting {
    fn get_assertion(&self, _: AssertionRequest<'_>) -> Result<Assertion, SecurityKeyError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Err(SecurityKeyError::NoDevice)
    }
}

#[tokio::test]
async fn a_key_file_for_another_application_never_reaches_the_key() {
    use russh::keys::ssh_key::private::{self, KeypairData};
    let (_, KeyData::SkEd25519(public)) = ed25519() else {
        unreachable!()
    };
    let web = SkEd25519::new(*public.public_key(), "example.com");
    let ssh = SkEd25519::new(*public.public_key(), "ssh:work");
    let counting = Arc::new(Counting::default());
    let key: Arc<dyn SecurityKey> = counting.clone();
    for (public, reaches) in [(web, false), (ssh, true)] {
        let keypair = PrivateKey::new(
            KeypairData::SkEd25519(private::SkEd25519::new(public, 0x01, [1u8; 64]).unwrap()),
            "",
        )
        .unwrap();
        let before = counting.0.load(std::sync::atomic::Ordering::Relaxed);
        assert!(sign(&key, &keypair, None, b"data").await.is_err());
        let after = counting.0.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(after > before, reaches);
    }
}
