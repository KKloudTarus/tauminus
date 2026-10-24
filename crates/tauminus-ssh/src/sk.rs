// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Signing with an OpenSSH FIDO2 key file (`sk-`) through a security key
//! (ADR 0027). The file names the credential; the key signs; this module
//! turns its answer into the SSH signature and checks it before it goes.

use std::sync::Arc;

use russh::keys::ssh_key::private::KeypairData;
use russh::keys::ssh_key::public::KeyData;
use russh::keys::ssh_key::{Algorithm, Signature};
use russh::keys::{PrivateKey, signature::Verifier};
use sha2::{Digest, Sha256};
use tauminus_fido::{Assertion, AssertionRequest, Cancel, SecurityKey, SecurityKeyError};
use zeroize::Zeroizing;

/// The key file's flag for a touch, as OpenSSH writes it.
pub(crate) const USER_PRESENCE: u8 = 0x01;
/// The key file's flag for user verification: a PIN.
pub(crate) const USER_VERIFICATION: u8 = 0x04;
/// SHA-256 of the application, the flags byte, the counter.
const AUTH_DATA_MIN: usize = 32 + 1 + 4;

/// Why a FIDO2 signature did not come.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SkError {
    #[error(transparent)]
    Key(#[from] SecurityKeyError),
    #[error("{0}")]
    Answer(&'static str),
}

/// What a FIDO2 key file holds: the credential, not a secret.
pub(crate) struct Credential<'a> {
    pub(crate) public: KeyData,
    pub(crate) application: &'a str,
    pub(crate) flags: u8,
    pub(crate) key_handle: &'a [u8],
}

/// The FIDO2 credential `key` names, if it is one.
pub(crate) fn credential(key: &PrivateKey) -> Option<Credential<'_>> {
    match key.key_data() {
        KeypairData::SkEd25519(sk) => Some(Credential {
            public: KeyData::SkEd25519(sk.public().clone()),
            application: sk.public().application(),
            flags: sk.flags(),
            key_handle: sk.key_handle(),
        }),
        KeypairData::SkEcdsaSha2NistP256(sk) => Some(Credential {
            public: KeyData::SkEcdsaSha2NistP256(sk.public().clone()),
            application: sk.public().application(),
            flags: sk.flags(),
            key_handle: sk.key_handle(),
        }),
        _ => None,
    }
}

/// What the key signs over, with the authenticator data: SHA-256 of the
/// data to sign, as OpenSSH's middleware passes it.
#[cfg(test)]
pub(crate) fn client_data_hash(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// The SSH signature an assertion makes for `data`: the key's signature
/// (Ed25519 raw, ECDSA as two mpints from its DER), then the flags byte
/// and the counter from the authenticator data. Checked against the
/// public key before it is returned, so a wrong answer fails here, by
/// name, not as a key the server refused. O(n) in `data`.
pub(crate) fn ssh_signature(
    public: &KeyData,
    application: &str,
    data: &[u8],
    assertion: &Assertion,
) -> Result<Signature, SkError> {
    let auth = &assertion.authenticator_data;
    if auth.len() < AUTH_DATA_MIN {
        return Err(SkError::Answer("the security key's answer is too short"));
    }
    if auth[..32] != Sha256::digest(application.as_bytes())[..] {
        return Err(SkError::Answer(
            "the security key answered for another application",
        ));
    }
    let (flags, counter) = (auth[32], &auth[33..37]);
    let (algorithm, mut bytes) = match public {
        KeyData::SkEd25519(_) => {
            if assertion.signature.len() != 64 {
                return Err(SkError::Answer(
                    "the security key's signature is not Ed25519",
                ));
            }
            (Algorithm::SkEd25519, assertion.signature.clone())
        }
        KeyData::SkEcdsaSha2NistP256(_) => {
            let der = p256::ecdsa::Signature::from_der(&assertion.signature)
                .map_err(|_| SkError::Answer("the security key's signature is not ECDSA DER"))?;
            let mpints = Signature::try_from(&der)
                .map_err(|_| SkError::Answer("the security key's signature does not encode"))?;
            (Algorithm::SkEcdsaSha2NistP256, mpints.as_bytes().to_vec())
        }
        _ => return Err(SkError::Answer("not a FIDO2 key")),
    };
    bytes.push(flags);
    bytes.extend_from_slice(counter);
    let signature = Signature::new(algorithm, bytes)
        .map_err(|_| SkError::Answer("the security key's signature does not encode"))?;
    public
        .verify(data, &signature)
        .map_err(|_| SkError::Answer("the security key's signature does not verify"))?;
    Ok(signature)
}

/// Asks `key` to sign `data` for the credential in `keypair`, off the
/// async runtime: the key may wait for a touch.
pub(crate) async fn sign(
    key: &Arc<dyn SecurityKey>,
    keypair: &PrivateKey,
    pin: Option<Zeroizing<String>>,
    data: &[u8],
) -> Result<Signature, SkError> {
    let credential = credential(keypair).ok_or(SkError::Answer("not a FIDO2 key"))?;
    // OpenSSH enrolls under `ssh:` only. A file naming a web site's
    // credential must not get the key to sign for that site.
    if !credential.application.starts_with("ssh:") {
        return Err(SkError::Answer("not an SSH application"));
    }
    let (key, rp_id, handle, flags, client_data) = (
        Arc::clone(key),
        credential.application.to_owned(),
        credential.key_handle.to_vec(),
        credential.flags,
        data.to_vec(),
    );
    let cancel = Arc::new(Cancel::default());
    // Dropped unanswered (the touch cancelled, the connect gone): the key
    // is told to stop, so it frees the device for the next sign.
    let mut guard = CancelOnDrop(Some(Arc::clone(&cancel)));
    let assertion = tokio::task::spawn_blocking(move || {
        key.get_assertion(AssertionRequest {
            rp_id: &rp_id,
            client_data: &client_data,
            credential_id: &handle,
            user_presence: flags & USER_PRESENCE != 0,
            pin: pin.as_ref(),
            cancel: &cancel,
        })
    })
    .await
    .map_err(|_| SkError::Answer("the security key's task ended"))??;
    guard.0 = None;
    ssh_signature(&credential.public, credential.application, data, &assertion)
}

/// Cancels the request it holds when dropped before it was answered.
struct CancelOnDrop(Option<Arc<Cancel>>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            cancel.cancel();
        }
    }
}

#[cfg(test)]
mod tests;
