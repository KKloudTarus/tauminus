// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! FIDO2 key files signing through a security key, with no agent, against
//! a real OpenSSH server (ADR 0027). No key is plugged in here: a stand-in
//! signs as OpenSSH's test provider (`sk-dummy.so`) would, whose key handle
//! is the private key. Everything but the USB transport runs as with a
//! key: the key file read, the assertion asked for, its authenticator data
//! and signature turned into the SSH signature, the server's check. Needs
//! `TAUMINUS_SK_SSHD` as `sk_agent.rs` does; ignored unless asked for.

#![cfg(unix)]

mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use support::{Scratch, run_shell};
use tauminus_app::ports::{Answer, BoxFuture, Prompt, Prompter, SshConnector};
use tauminus_domain::{
    AuthMethod, AuthOptions, Endpoint, HostName, Route, SshTarget, TerminalSize, UserName,
};
use tauminus_fido::{Assertion, AssertionRequest, SecurityKey, SecurityKeyError};
use tauminus_ssh::{AgentSource, HostKeyStore, RusshConnector};
use zeroize::Zeroizing;

/// Signs as `sk-dummy.so` would: the credential is the private key, raw
/// Ed25519 (64 bytes) or a PEM P-256 key. Counts the assertions asked for.
/// `fails`, when set, is what it answers instead. It waits `wait` first,
/// as for a touch, unless the request is cancelled, which it records.
#[derive(Debug, Default)]
struct DummyKey {
    asked: Mutex<Vec<(String, bool)>>,
    fails: Option<fn() -> SecurityKeyError>,
    wait: Duration,
    cancelled: AtomicBool,
    /// Wants a PIN for every assertion, as a key set to `alwaysUv`.
    always_uv: bool,
    pins: Mutex<Vec<Option<String>>>,
}

impl SecurityKey for DummyKey {
    fn get_assertion(&self, request: AssertionRequest<'_>) -> Result<Assertion, SecurityKeyError> {
        self.asked
            .lock()
            .unwrap()
            .push((request.rp_id.to_owned(), request.user_presence));
        let started = Instant::now();
        while started.elapsed() < self.wait {
            if request.cancel.is_cancelled() {
                self.cancelled.store(true, Ordering::Relaxed);
                return Err(SecurityKeyError::NotConfirmed);
            }
            #[expect(
                clippy::disallowed_methods,
                reason = "a key's CTAP call blocks; the port runs on a blocking thread"
            )]
            std::thread::sleep(Duration::from_millis(5));
        }
        self.pins
            .lock()
            .unwrap()
            .push(request.pin.map(|pin| pin.as_str().to_owned()));
        if self.always_uv && request.pin.is_none() {
            return Err(SecurityKeyError::PinRequired);
        }
        if let Some(fails) = self.fails {
            return Err(fails());
        }
        let mut auth = Sha256::digest(request.rp_id.as_bytes()).to_vec();
        auth.push(u8::from(request.user_presence));
        auth.extend_from_slice(&0x1234_5678u32.to_be_bytes());
        let mut signed = auth.clone();
        signed.extend_from_slice(&Sha256::digest(request.client_data));
        let signature = if let Ok(pair) = <&[u8; 64]>::try_from(request.credential_id) {
            let key = ed25519_dalek::SigningKey::from_keypair_bytes(pair)
                .map_err(|e| SecurityKeyError::Device(e.to_string()))?;
            ed25519_dalek::Signer::sign(&key, &signed)
                .to_bytes()
                .to_vec()
        } else {
            let pem = std::str::from_utf8(request.credential_id)
                .map_err(|_| SecurityKeyError::UnknownCredential)?;
            let secret = <p256::SecretKey as p256::pkcs8::DecodePrivateKey>::from_pkcs8_pem(pem)
                .or_else(|_| p256::SecretKey::from_sec1_pem(pem))
                .map_err(|_| SecurityKeyError::UnknownCredential)?;
            let key = p256::ecdsa::SigningKey::from(secret);
            let signature: p256::ecdsa::Signature = signature::Signer::sign(&key, &signed);
            signature.to_der().as_bytes().to_vec()
        };
        Ok(Assertion {
            authenticator_data: auth,
            signature,
        })
    }
}

/// Trusts the host key; the touch notice stays up until it is withdrawn,
/// as on a screen where the user has not touched the key yet, or is
/// cancelled at once when `cancel_touch` is set.
#[derive(Default)]
struct Waiting {
    asked: Mutex<Vec<String>>,
    cancel_touch: bool,
    /// The PIN typed when asked; none cancels.
    pin: Option<&'static str>,
}

impl Prompter for Waiting {
    fn ask(&self, prompt: Prompt) -> BoxFuture<'_, Answer> {
        self.asked.lock().unwrap().push(format!("{prompt:?}"));
        Box::pin(async move {
            match prompt {
                Prompt::UnknownHostKey { .. } => Answer::TrustHostKey { remember: false },
                Prompt::SecurityKeyTouch { .. } if self.cancel_touch => Answer::Cancel,
                Prompt::SecurityKeyPin { .. } => match self.pin {
                    Some(pin) => Answer::Secret(Zeroizing::new(pin.to_owned())),
                    None => Answer::Cancel,
                },
                Prompt::SecurityKeyTouch { .. } => std::future::pending().await,
                _ => Answer::Cancel,
            }
        })
    }
}

fn setup() -> (u16, String, PathBuf) {
    let value = std::env::var("TAUMINUS_SK_SSHD").expect("TAUMINUS_SK_SSHD");
    let parts: Vec<&str> = value.split(' ').collect();
    let [port, user, _agent, keys] = parts[..] else {
        panic!("TAUMINUS_SK_SSHD must be `<port> <user> <agent socket> <key dir>`");
    };
    (port.parse().unwrap(), user.to_owned(), keys.into())
}

/// Connects with key file `key` through `security_key`, with no agent.
async fn connect(
    key: &str,
    security_key: &Arc<DummyKey>,
    prompter: &Arc<Waiting>,
) -> Result<tauminus_app::ports::TerminalStream, tauminus_app::ports::ConnectError> {
    let (port, user, keys) = setup();
    let scratch = Scratch::new(key);
    let connector = RusshConnector::new(
        HostKeyStore::new(vec![], scratch.path("known_hosts")),
        None,
        AgentSource::None,
    )
    .with_security_key(Arc::clone(security_key) as Arc<dyn SecurityKey>);
    let target = SshTarget::new(
        Endpoint::new(
            UserName::parse(&user).unwrap(),
            HostName::parse("127.0.0.1").unwrap(),
            port,
        )
        .unwrap(),
        vec![],
        Route::Direct,
        AuthOptions {
            methods: vec![AuthMethod::PublicKey],
            identity_files: vec![keys.join(key)],
            identities_only: true,
            identity_agent: None,
        },
    )
    .unwrap();
    connector
        .connect(
            &target,
            TerminalSize::new(80, 24).unwrap(),
            Arc::clone(prompter) as Arc<dyn Prompter>,
        )
        .await
}

/// Logs in with key file `key` through the stand-in, with no agent.
async fn logs_in_with(key: &str) {
    let security_key = Arc::new(DummyKey::default());
    let prompter = Arc::new(Waiting::default());
    let mut stream = connect(key, &security_key, &prompter)
        .await
        .unwrap_or_else(|e| panic!("{key}: {e}"));
    let (output, code) = run_shell(&mut stream, "echo direct-$((6*7)); exit 0\n").await;
    assert!(output.contains("direct-42"), "{key}: {output}");
    assert_eq!(code, Some(0));
    // The key was asked once, for `ssh:`, with a touch; the user was told
    // to touch it.
    assert_eq!(
        *security_key.asked.lock().unwrap(),
        vec![("ssh:".to_owned(), true)]
    );
    assert!(
        prompter
            .asked
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.starts_with("SecurityKeyTouch")),
        "{:?}",
        prompter.asked.lock().unwrap()
    );
}

#[tokio::test]
#[ignore = "needs an sshd that takes sk- keys: TAUMINUS_SK_SSHD"]
async fn an_ed25519_sk_key_file_signs_through_the_security_key() {
    logs_in_with("id_ed25519-sk").await;
}

#[tokio::test]
#[ignore = "needs an sshd that takes sk- keys: TAUMINUS_SK_SSHD"]
async fn an_ecdsa_sk_key_file_signs_through_the_security_key() {
    logs_in_with("id_ecdsa-sk").await;
}

/// Fails to connect with `id_ed25519-sk` and returns the error text and
/// how many times the key was asked.
async fn fails_with(security_key: DummyKey, prompter: Waiting) -> (String, Arc<DummyKey>) {
    let security_key = Arc::new(security_key);
    let Err(error) = connect("id_ed25519-sk", &security_key, &Arc::new(prompter)).await else {
        panic!("signed in");
    };
    (error.to_string(), security_key)
}

#[tokio::test]
#[ignore = "needs an sshd that takes sk- keys: TAUMINUS_SK_SSHD"]
async fn a_missing_security_key_is_named_and_asked_once() {
    let (error, key) = fails_with(
        DummyKey {
            fails: Some(|| SecurityKeyError::NoDevice),
            ..DummyKey::default()
        },
        Waiting::default(),
    )
    .await;
    assert!(error.contains("no security key plugged in"), "{error}");
    assert_eq!(key.asked.lock().unwrap().len(), 1);
}

#[tokio::test]
#[ignore = "needs an sshd that takes sk- keys: TAUMINUS_SK_SSHD"]
async fn a_key_that_does_not_hold_the_credential_is_named() {
    let (error, _) = fails_with(
        DummyKey {
            fails: Some(|| SecurityKeyError::UnknownCredential),
            ..DummyKey::default()
        },
        Waiting::default(),
    )
    .await;
    assert!(error.contains("does not hold"), "{error}");
}

#[tokio::test]
#[ignore = "needs an sshd that takes sk- keys: TAUMINUS_SK_SSHD"]
async fn a_cancelled_touch_ends_the_key_and_is_named() {
    let started = Instant::now();
    let (error, key) = fails_with(
        DummyKey {
            wait: Duration::from_secs(30),
            ..DummyKey::default()
        },
        Waiting {
            cancel_touch: true,
            ..Waiting::default()
        },
    )
    .await;
    assert!(error.contains("touch was cancelled"), "{error}");
    assert_eq!(key.asked.lock().unwrap().len(), 1);
    // The key was told to stop, well before it would have timed out.
    tokio::time::timeout(Duration::from_secs(5), async {
        while !key.cancelled.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the request was cancelled");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
#[ignore = "needs an sshd that takes sk- keys: TAUMINUS_SK_SSHD"]
async fn a_key_that_always_verifies_gets_the_pin_on_a_second_ask() {
    let security_key = Arc::new(DummyKey {
        always_uv: true,
        ..DummyKey::default()
    });
    let prompter = Arc::new(Waiting {
        pin: Some("4321"),
        ..Waiting::default()
    });
    let mut stream = connect("id_ed25519-sk", &security_key, &prompter)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let (output, _) = run_shell(&mut stream, "echo uv-$((6*7)); exit 0\n").await;
    assert!(output.contains("uv-42"), "{output}");
    assert_eq!(
        *security_key.pins.lock().unwrap(),
        vec![None, Some("4321".to_owned())]
    );
}

#[tokio::test]
#[ignore = "needs an sshd that takes sk- keys: TAUMINUS_SK_SSHD"]
async fn a_pin_not_given_is_named_and_the_key_asked_no_more() {
    let (error, key) = fails_with(
        DummyKey {
            always_uv: true,
            ..DummyKey::default()
        },
        Waiting::default(),
    )
    .await;
    assert!(error.contains("PIN was not given"), "{error}");
    assert_eq!(key.asked.lock().unwrap().len(), 1);
}
