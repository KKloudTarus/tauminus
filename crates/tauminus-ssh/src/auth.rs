// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! User authentication, in the order the target's `AuthOptions` asks for.
//!
//! The client first sends `none`, which every server answers with the
//! methods it accepts, so a method the server refuses is never tried and the
//! user is never asked for a password the server would not take. When a
//! method succeeds partially (`AuthenticationMethods publickey,password`),
//! the list starts over against the methods the server now wants, as OpenSSH
//! does.
//!
//! Keys are offered by their public half first. A key file is decrypted, and
//! its passphrase asked for, only after the server says it would accept that
//! key.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use russh::MethodKind;
use russh::Signer;
use russh::client::{AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::signature::Signer as _;
use russh::keys::ssh_encoding::Encode;
use russh::keys::ssh_key::private::KeypairData;
use russh::keys::{
    Certificate, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey, decode_secret_key,
};
use tauminus_app::ports::{
    Answer, ChallengeField, ConnectError, Credentials, Prompt, Prompter, Secret,
};
use tauminus_domain::{AuthMethod, AuthOptions, Endpoint};
use tauminus_fido::SecurityKey;
use tracing::debug;
use zeroize::Zeroizing;

use crate::Client;
use crate::rejected::RejectedPasswords;
use crate::sk;

/// Default key files, strongest first, then the FIDO2 (`*_sk`) ones
/// OpenSSH also looks for. Those sign through the security key when one
/// is set (`RusshConnector::with_security_key`), else through an agent:
/// listed here, `IdentitiesOnly` lets the agent's copy through.
const DEFAULT_KEYS: &[&str] = &[
    "id_ed25519",
    "id_ecdsa",
    "id_rsa",
    "id_ed25519_sk",
    "id_ecdsa_sk",
];
/// Named among what was tried when a FIDO2 key was passed over and
/// nothing else signed in: the user then knows what is missing.
const SKIPPED_FIDO: &str = "a FIDO2 key, which only ssh-agent signs with";
/// How long a security key may take to sign when no touch is asked: a
/// silent assertion takes well under a second.
const NO_TOUCH_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
/// Attempts per secret before giving up on it, as OpenSSH does.
const ATTEMPTS: usize = 3;
/// Keyboard-interactive rounds answered before giving up on a looping server.
const MAX_CHALLENGE_ROUNDS: usize = 10;
/// Passes over the method list. Each pass after the first follows a partial
/// success; real servers chain two or three methods.
const MAX_PASSES: usize = 4;

pub(crate) struct Authenticator<'a> {
    pub(crate) handle: &'a mut Handle<Client>,
    pub(crate) endpoint: &'a Endpoint,
    pub(crate) options: &'a AuthOptions,
    pub(crate) prompter: &'a Arc<dyn Prompter>,
    pub(crate) home: Option<&'a Path>,
    pub(crate) agent: &'a AgentSource,
    /// Signs for FIDO2 key files; none leaves them to an agent.
    pub(crate) security_key: Option<&'a Arc<dyn SecurityKey>>,
    /// From a vault identity: tried before key files and prompts.
    pub(crate) credentials: &'a Credentials,
    /// Stored passwords a server turned down; not sent again.
    pub(crate) rejected: &'a RejectedPasswords,
    /// Set when the server closed the connection itself.
    pub(crate) goodbye: &'a std::sync::OnceLock<String>,
    /// This sign-in's turn to spend the stored password; dropped once the
    /// server has judged it.
    pub(crate) turn: Option<tokio::sync::OwnedMutexGuard<()>>,
    /// The stored password was the last answer the server got, so a
    /// failure now is about it, not a later factor.
    pub(crate) stored_last: bool,
    /// A FIDO2 key was passed over: only an agent signs with it.
    pub(crate) skipped_fido: bool,
    /// Why the security key last did not sign, named among what was tried.
    pub(crate) fido_failed: Vec<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Done,
    /// Something was sent to the server and it did not finish the login.
    Tried,
    /// Nothing to offer: no agent, no key files, a passphrase given up on.
    Skipped,
}

/// What the server last said it would accept.
struct Server {
    /// `None` until the server says, or after it sends an empty list.
    allowed: Option<Vec<MethodKind>>,
    /// Set when a method succeeded partially and the list should restart.
    partial: bool,
}

impl Server {
    fn permits(&self, kind: MethodKind) -> bool {
        self.allowed
            .as_ref()
            .is_none_or(|methods| methods.contains(&kind))
    }

    /// Records a reply. Returns true on success.
    fn settle(&mut self, result: AuthResult) -> bool {
        match result {
            AuthResult::Success => true,
            AuthResult::Failure {
                remaining_methods,
                partial_success,
            } => {
                let methods = remaining_methods.to_vec();
                // An empty list tells us nothing; OpenSSH then falls back to
                // its own preferences.
                self.allowed = (!methods.is_empty()).then_some(methods);
                self.partial |= partial_success;
                false
            }
        }
    }
}

fn kind(method: AuthMethod) -> MethodKind {
    match method {
        AuthMethod::Agent | AuthMethod::PublicKey => MethodKind::PublicKey,
        AuthMethod::KeyboardInteractive => MethodKind::KeyboardInteractive,
        AuthMethod::Password => MethodKind::Password,
    }
}

impl Authenticator<'_> {
    /// Bounded: at most `MAX_PASSES` passes over the configured methods, each
    /// method with its own bound on prompts and requests.
    pub(crate) async fn run(mut self) -> Result<(), ConnectError> {
        let user = self.endpoint.user.as_str().to_owned();
        let mut server = Server {
            allowed: None,
            partial: false,
        };
        let first = self
            .handle
            .authenticate_none(&user)
            .await
            .map_err(protocol)?;
        if server.settle(first) {
            return Ok(());
        }
        self.link_alive()?;
        let mut tried: Vec<String> = Vec::new();
        // The identity's key goes before any agent key: servers stop after
        // MaxAuthTries (6) keys, and an agent may hold more than that.
        match self.identity_key(&user, &mut server).await? {
            Outcome::Done => return Ok(()),
            Outcome::Tried => tried.push(method_name(AuthMethod::PublicKey).to_owned()),
            Outcome::Skipped => {}
        }
        // The identity's password answers the first password prompt, from
        // either method, once.
        // Unless a server already turned it down: then the user is asked,
        // instead of every reconnect spending one of the account's tries.
        // Another sign-in spending the same password goes first; its verdict
        // decides whether this one sends it.
        let mut stored = None;
        if let Some(password) = &self.credentials.password {
            let turn = self.rejected.turn(&user, password).await;
            if !self.rejected.contains(&user, password) {
                stored = Some(password.clone());
                self.turn = Some(turn);
            }
        }
        for _ in 0..MAX_PASSES {
            server.partial = false;
            for method in self.options.methods.clone() {
                if !server.permits(kind(method)) {
                    continue;
                }
                let outcome = match method {
                    AuthMethod::Agent => self.agent(&user, &mut server).await?,
                    AuthMethod::PublicKey => self.key_files(&user, &mut server).await?,
                    AuthMethod::KeyboardInteractive => {
                        self.keyboard_interactive(&user, &mut server, &mut stored)
                            .await?
                    }
                    AuthMethod::Password => self.password(&user, &mut server, &mut stored).await?,
                };
                match outcome {
                    Outcome::Done => return Ok(()),
                    Outcome::Tried => {
                        // A key or the agent refused, or the link went away.
                        self.link_alive()?;
                        let name = method_name(method).to_owned();
                        if !tried.contains(&name) {
                            tried.push(name);
                        }
                    }
                    Outcome::Skipped => {}
                }
                if server.partial {
                    break;
                }
            }
            if !server.partial {
                break;
            }
        }
        if self.skipped_fido {
            tried.push(SKIPPED_FIDO.to_owned());
        }
        for reason in &self.fido_failed {
            tried.push((*reason).to_owned());
        }
        Err(ConnectError::AuthFailed {
            endpoint: self.endpoint.to_string(),
            tried,
        })
    }

    /// Records why a security key did not sign, once per reason.
    fn note_fido(&mut self, reason: Option<&'static str>) {
        if let Some(reason) = reason
            && !self.fido_failed.contains(&reason)
        {
            self.fido_failed.push(reason);
        }
    }

    async fn agent(&mut self, user: &str, server: &mut Server) -> Result<Outcome, ConnectError> {
        let Some(agent) = connect_agent(self.agent).await else {
            return Ok(Outcome::Skipped);
        };
        let mut signer = AgentSigner(agent);
        let identities = match signer.0.request_identities().await {
            Ok(identities) => identities,
            Err(e) => {
                debug!(error = %e, "ssh-agent did not list identities");
                return Ok(Outcome::Skipped);
            }
        };
        let only = if self.options.identities_only {
            Some(self.identity_file_public_keys().await)
        } else {
            None
        };
        let mut outcome = Outcome::Skipped;
        for identity in identities {
            if !server.permits(MethodKind::PublicKey) || server.partial {
                break;
            }
            let key = match &identity {
                AgentIdentity::PublicKey { key, .. } => key.clone(),
                AgentIdentity::Certificate { certificate, .. } => {
                    PublicKey::from(certificate.public_key().clone())
                }
            };
            if only
                .as_ref()
                .is_some_and(|keys| !keys.iter().any(|k| k.key_data() == key.key_data()))
            {
                continue;
            }
            let hash = self.rsa_hash(&key).await?;
            let result = match identity {
                AgentIdentity::PublicKey { key, .. } => {
                    self.handle
                        .authenticate_publickey_with(user, key, hash, &mut signer)
                        .await
                }
                AgentIdentity::Certificate { certificate, .. } => {
                    self.handle
                        .authenticate_certificate_with(user, certificate, hash, &mut signer)
                        .await
                }
            };
            outcome = Outcome::Tried;
            if server.settle(result.map_err(SignError::into_connect_error)?) {
                return Ok(Outcome::Done);
            }
        }
        Ok(outcome)
    }

    async fn key_files(
        &mut self,
        user: &str,
        server: &mut Server,
    ) -> Result<Outcome, ConnectError> {
        let mut outcome = Outcome::Skipped;
        for path in self.candidate_keys() {
            if !server.permits(MethodKind::PublicKey) || server.partial {
                break;
            }
            let Some(text) = read_key_file(&path).await else {
                continue;
            };
            let Some(public) = public_key_of(&path, &text).await else {
                debug!(path = %path.display(), "skipping unreadable key");
                continue;
            };
            if needs_security_key(&public) && self.security_key.is_none() {
                debug!(path = %path.display(), "skipping a FIDO2 key: only an agent signs with it");
                self.skipped_fido = true;
                continue;
            }
            let mut signer = KeyFileSigner {
                path: &path,
                text: &text,
                prompter: self.prompter,
                security_key: self.security_key,
                key: None,
                gave_up: false,
                fido_failed: None,
            };
            let hash = self.rsa_hash(&public).await?;
            if let Some(certificate) = read_certificate(&with_suffix(&path, "-cert.pub")).await
                // Compare key material only: a comment does not make a different key.
                && certificate.public_key() == public.key_data()
            {
                let result = self
                    .handle
                    .authenticate_certificate_with(user, certificate, hash, &mut signer)
                    .await
                    .map_err(SignError::into_connect_error)?;
                outcome = Outcome::Tried;
                self.note_fido(signer.fido_failed);
                if server.settle(result) {
                    return Ok(Outcome::Done);
                }
                if signer.gave_up || server.partial || !server.permits(MethodKind::PublicKey) {
                    continue;
                }
            }
            let result = self
                .handle
                .authenticate_publickey_with(user, public, hash, &mut signer)
                .await
                .map_err(SignError::into_connect_error)?;
            outcome = Outcome::Tried;
            self.note_fido(signer.fido_failed);
            if server.settle(result) {
                return Ok(Outcome::Done);
            }
        }
        Ok(outcome)
    }

    /// A wrong stored password ends one exchange; the user then gets the
    /// prompt, as with the password method.
    async fn keyboard_interactive(
        &mut self,
        user: &str,
        server: &mut Server,
        stored: &mut Option<Secret>,
    ) -> Result<Outcome, ConnectError> {
        let had = stored.is_some();
        self.stored_last = false;
        let result = self.keyboard_exchange(user, server, stored).await;
        let spent = had && stored.is_none();
        // Turned down: a failure right after the stored password, or a
        // goodbye in its place (which russh reports as an error here).
        let refused = match &result {
            Ok(Outcome::Tried) => !server.partial && self.server_answered(),
            Err(_) => self.goodbye.get().is_some(),
            Ok(_) => false,
        };
        if spent
            && self.stored_last
            && refused
            && let Some(password) = &self.credentials.password
        {
            self.rejected.insert(user, password);
        }
        if spent {
            self.turn = None;
        }
        let outcome = result?;
        self.link_alive()?;
        if outcome == Outcome::Tried
            && spent
            && !server.partial
            && server.permits(MethodKind::KeyboardInteractive)
        {
            return self.keyboard_exchange(user, server, &mut None).await;
        }
        Ok(outcome)
    }

    async fn keyboard_exchange(
        &mut self,
        user: &str,
        server: &mut Server,
        stored: &mut Option<Secret>,
    ) -> Result<Outcome, ConnectError> {
        let mut response = self
            .handle
            .authenticate_keyboard_interactive_start(user, None)
            .await
            .map_err(protocol)?;
        // Round k looks at the reply to the k-th answer, so the reply to the
        // last answer allowed is still read.
        for round in 0..=MAX_CHALLENGE_ROUNDS {
            match response {
                KeyboardInteractiveAuthResponse::Success => return Ok(Outcome::Done),
                KeyboardInteractiveAuthResponse::Failure {
                    remaining_methods,
                    partial_success,
                } => {
                    server.settle(AuthResult::Failure {
                        remaining_methods,
                        partial_success,
                    });
                    return Ok(Outcome::Tried);
                }
                KeyboardInteractiveAuthResponse::InfoRequest {
                    name,
                    instructions,
                    prompts,
                } => {
                    if round == MAX_CHALLENGE_ROUNDS {
                        break;
                    }
                    let answers = if prompts.is_empty() {
                        Vec::new()
                    } else if let [only] = prompts.as_slice()
                        && Self::asks_for_password(only)
                        && let Some(password) = stored.take()
                    {
                        self.stored_last = true;
                        vec![password]
                    } else {
                        self.stored_last = false;
                        let answer = self
                            .prompter
                            .ask(Prompt::KeyboardInteractive {
                                endpoint: self.endpoint.to_string(),
                                name,
                                instructions,
                                fields: prompts
                                    .into_iter()
                                    .map(|p| ChallengeField {
                                        text: p.prompt,
                                        echo: p.echo,
                                    })
                                    .collect(),
                            })
                            .await;
                        match answer {
                            Answer::Responses(responses) => responses,
                            _ => return Err(ConnectError::Cancelled),
                        }
                    };
                    // russh takes plain strings; ours are wiped when dropped.
                    let plain = answers
                        .iter()
                        .map(|secret| secret.as_str().to_owned())
                        .collect();
                    response = self
                        .handle
                        .authenticate_keyboard_interactive_respond(plain)
                        .await
                        .map_err(protocol)?;
                }
            }
        }
        Ok(Outcome::Tried)
    }

    /// The vault identity's key, decoded with its stored passphrase.
    async fn identity_key(
        &mut self,
        user: &str,
        server: &mut Server,
    ) -> Result<Outcome, ConnectError> {
        let Some(pem) = &self.credentials.private_key else {
            return Ok(Outcome::Skipped);
        };
        // Keys follow the host's method list, as key files and the agent do.
        let keys_allowed = self
            .options
            .methods
            .iter()
            .any(|m| matches!(m, AuthMethod::PublicKey | AuthMethod::Agent));
        if !keys_allowed || !server.permits(MethodKind::PublicKey) || server.partial {
            return Ok(Outcome::Skipped);
        }
        let key = match decode_off_thread(pem, self.credentials.passphrase.clone()).await {
            Ok(key) => Arc::new(key),
            Err(error) => {
                debug!(%error, "the identity's key does not decode");
                return Ok(Outcome::Skipped);
            }
        };
        if needs_security_key(key.public_key()) {
            let Some(security_key) = self.security_key else {
                debug!("skipping the identity's FIDO2 key: only an agent signs with it");
                self.skipped_fido = true;
                return Ok(Outcome::Skipped);
            };
            let mut signer = SecurityKeySigner {
                security_key,
                key: &key,
                prompter: self.prompter,
                shown: "the identity's key",
                failed: None,
            };
            let result = self
                .handle
                .authenticate_publickey_with(user, key.public_key().clone(), None, &mut signer)
                .await
                .map_err(SignError::into_connect_error)?;
            self.note_fido(signer.failed);
            if server.settle(result) {
                return Ok(Outcome::Done);
            }
            return Ok(Outcome::Tried);
        }
        let hash = self.rsa_hash(key.public_key()).await?;
        let result = self
            .handle
            .authenticate_publickey(user, PrivateKeyWithHashAlg::new(key, hash))
            .await
            .map_err(protocol)?;
        if server.settle(result) {
            return Ok(Outcome::Done);
        }
        Ok(Outcome::Tried)
    }

    async fn password(
        &mut self,
        user: &str,
        server: &mut Server,
        stored: &mut Option<Secret>,
    ) -> Result<Outcome, ConnectError> {
        let mut outcome = Outcome::Skipped;
        for _ in 0..ATTEMPTS {
            if !server.permits(MethodKind::Password) || server.partial {
                break;
            }
            let from_store = stored.is_some();
            let password = if let Some(password) = stored.take() {
                password
            } else {
                let answer = self
                    .prompter
                    .ask(Prompt::Password {
                        endpoint: self.endpoint.to_string(),
                    })
                    .await;
                let Answer::Secret(password) = answer else {
                    return Err(ConnectError::Cancelled);
                };
                password
            };
            let result = self
                .handle
                .authenticate_password(user, password.as_str())
                .await
                .map_err(protocol)?;
            outcome = Outcome::Tried;
            if server.settle(result) {
                // Typed and taken, and the same as the vault's: it was the
                // link, or another host, that turned it down before.
                if !from_store && self.credentials.password.as_ref() == Some(&password) {
                    self.rejected.remove(user, &password);
                }
                return Ok(Outcome::Done);
            }
            // Recorded before the link check: a server may say goodbye
            // right after turning it down.
            if from_store && !server.partial && self.server_answered() {
                self.rejected.insert(user, &password);
            }
            if from_store {
                self.turn = None;
            }
            self.link_alive()?;
        }
        Ok(outcome)
    }

    /// russh reports a link that ended mid-login as a refusal with no
    /// methods left. A server that said goodbye first (OpenSSH does after
    /// `MaxAuthTries`) refused, and asking again would only spend more of
    /// the account's tries; a link that just went away is the network.
    /// Whether the last failure came from the server: the link is up, or
    /// the server closed it with a goodbye. A link that only went away
    /// says nothing about the password, which then is not marked.
    fn server_answered(&self) -> bool {
        !self.handle.is_closed() || self.goodbye.get().is_some()
    }

    fn link_alive(&self) -> Result<(), ConnectError> {
        if !self.handle.is_closed() {
            return Ok(());
        }
        Err(match self.goodbye.get() {
            Some(said) => {
                ConnectError::Protocol(format!("the server closed the connection: {said}"))
            }
            None => ConnectError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "the connection closed during authentication",
            )),
        })
    }

    fn asks_for_password(prompt: &russh::client::Prompt) -> bool {
        asks_for_password(&prompt.prompt, prompt.echo)
    }

    /// RSA keys need the server's preferred SHA-2 hash; others need none.
    async fn rsa_hash(&self, key: &PublicKey) -> Result<Option<HashAlg>, ConnectError> {
        if key.algorithm().is_rsa() {
            Ok(self
                .handle
                .best_supported_rsa_hash()
                .await
                .map_err(protocol)?
                .flatten())
        } else {
            Ok(None)
        }
    }

    fn candidate_keys(&self) -> Vec<PathBuf> {
        if !self.options.identity_files.is_empty() {
            return self
                .options
                .identity_files
                .iter()
                .map(|path| expand_home(path, self.home))
                .collect();
        }
        // With no files listed, the default ones, as OpenSSH does whether or
        // not IdentitiesOnly is set.
        let Some(home) = self.home else {
            return Vec::new();
        };
        DEFAULT_KEYS
            .iter()
            .map(|name| home.join(".ssh").join(name))
            .collect()
    }

    /// Public keys of the configured identity files, for `IdentitiesOnly`.
    async fn identity_file_public_keys(&self) -> Vec<PublicKey> {
        let mut keys = Vec::new();
        for path in self.candidate_keys() {
            let text = read_key_file(&path).await.unwrap_or_default();
            if let Some(key) = public_key_of(&path, &text).await {
                keys.push(key);
            }
        }
        keys
    }
}

/// A FIDO2 key (`sk-`): its file holds no secret, only what the security
/// key needs to sign. Offered from a file it could not be signed for, and
/// the failed signature ended the whole connect; an agent signs for it.
fn needs_security_key(key: &PublicKey) -> bool {
    matches!(
        key.algorithm(),
        russh::keys::Algorithm::SkEd25519 | russh::keys::Algorithm::SkEcdsaSha2NistP256
    )
}

/// The public half of a key file without decrypting it: OpenSSH-format keys
/// store it in the clear; older PEM keys need the `.pub` file next to them.
async fn public_key_of(path: &Path, text: &str) -> Option<PublicKey> {
    if let Ok(key) = PrivateKey::from_openssh(text) {
        return Some(key.public_key().clone());
    }
    if let Some(public) = read_key_file(&with_suffix(path, ".pub")).await
        && let Ok(key) = PublicKey::from_openssh(public.trim())
    {
        return Some(key);
    }
    // An unencrypted PEM key decodes without a passphrase or a KDF.
    decode_secret_key(text, None)
        .ok()
        .map(|key| key.public_key().clone())
}

/// Why signing failed. A signer that returns an error leaves russh waiting
/// for a signature, so the only errors are ones that end the connect.
#[derive(Debug)]
enum SignError {
    Send,
    Sign(String),
}

impl From<russh::SendError> for SignError {
    fn from(_: russh::SendError) -> Self {
        Self::Send
    }
}

impl SignError {
    fn into_connect_error(self) -> ConnectError {
        match self {
            Self::Send => ConnectError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "the connection closed during authentication",
            )),
            Self::Sign(message) => ConnectError::Protocol(format!("could not sign: {message}")),
        }
    }
}

/// Signs with a key file, decrypting it on the first request. The server
/// asks for a signature only after it accepted the public key.
struct KeyFileSigner<'a> {
    path: &'a Path,
    text: &'a Zeroizing<String>,
    prompter: &'a Arc<dyn Prompter>,
    security_key: Option<&'a Arc<dyn SecurityKey>>,
    key: Option<Arc<PrivateKey>>,
    gave_up: bool,
    /// Why the security key did not sign for this file.
    fido_failed: Option<&'static str>,
}

impl KeyFileSigner<'_> {
    /// Decodes the key, asking for its passphrase when it is encrypted. The
    /// KDF costs about 200 ms per attempt, so it runs off the async workers.
    async fn unlock(&mut self) -> Option<Arc<PrivateKey>> {
        if let Some(key) = &self.key {
            return Some(Arc::clone(key));
        }
        if self.gave_up {
            return None;
        }
        let decoded = match decode_off_thread(self.text, None).await {
            Ok(key) => Some(key),
            Err(russh::keys::Error::KeyIsEncrypted) => self.ask_passphrase().await,
            Err(e) => {
                debug!(path = %self.path.display(), error = %e, "cannot decode key");
                None
            }
        };
        self.gave_up = decoded.is_none();
        self.key = decoded.map(Arc::new);
        self.key.clone()
    }

    async fn ask_passphrase(&self) -> Option<PrivateKey> {
        for _ in 0..ATTEMPTS {
            let answer = self
                .prompter
                .ask(Prompt::Passphrase {
                    key_path: self.path.display().to_string(),
                })
                .await;
            let Answer::Secret(passphrase) = answer else {
                return None;
            };
            if let Ok(key) = decode_off_thread(self.text, Some(passphrase)).await {
                return Some(key);
            }
        }
        None
    }
}

async fn decode_off_thread(
    text: &Zeroizing<String>,
    passphrase: Option<Zeroizing<String>>,
) -> Result<PrivateKey, russh::keys::Error> {
    let text = text.clone();
    tokio::task::spawn_blocking(move || {
        decode_secret_key(&text, passphrase.as_deref().map(String::as_str))
    })
    .await
    .unwrap_or(Err(russh::keys::Error::CouldNotReadKey))
}

impl Signer for KeyFileSigner<'_> {
    type Error = SignError;

    async fn auth_sign(
        &mut self,
        identity: &AgentIdentity,
        hash: Option<HashAlg>,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, SignError> {
        let Some(key) = self.unlock().await else {
            return Ok(append_refused_signature(identity, hash, data));
        };
        if let Some(security_key) = self.security_key
            && sk::credential(&key).is_some()
        {
            let shown = self.path.display().to_string();
            let mut signer = SecurityKeySigner {
                security_key,
                key: &key,
                prompter: self.prompter,
                shown: &shown,
                failed: None,
            };
            let answer = signer.auth_sign(identity, hash, data).await;
            // Asked again for the same file, the key would fail the same
            // way, or make the user touch it twice.
            if signer.failed.is_some() {
                self.fido_failed = signer.failed;
                self.gave_up = true;
            }
            return answer;
        }
        append_signature(&key, hash, data)
    }
}

/// Signs with a FIDO2 key through the security key: its PIN asked first
/// when the key file wants one, then the touch, which the user may cancel.
/// A key that does not sign, or a cancel, sends a signature the server
/// refuses, so authentication moves on.
struct SecurityKeySigner<'a> {
    security_key: &'a Arc<dyn SecurityKey>,
    key: &'a PrivateKey,
    prompter: &'a Arc<dyn Prompter>,
    /// The key file, as the prompts name it.
    shown: &'a str,
    /// Why it did not sign, once it did not.
    failed: Option<&'static str>,
}

/// Why the security key did not sign, as the failed sign-in names it.
fn security_key_failure(error: &sk::SkError) -> &'static str {
    use tauminus_fido::SecurityKeyError as E;
    match error {
        sk::SkError::Key(E::NoDevice) => "a FIDO2 key, with no security key plugged in",
        sk::SkError::Key(E::UnknownCredential) => {
            "a FIDO2 key, which the security key plugged in does not hold"
        }
        sk::SkError::Key(E::PinRequired) => "a FIDO2 key, whose security key wants a PIN",
        sk::SkError::Key(E::PinInvalid { .. }) => "a FIDO2 key, whose PIN the security key refused",
        sk::SkError::Key(E::PinBlocked) => "a FIDO2 key, whose security key has its PIN blocked",
        sk::SkError::Key(E::NotConfirmed) => "a FIDO2 key, which was not touched in time",
        sk::SkError::Key(E::Device(_)) | sk::SkError::Answer(_) => {
            "a FIDO2 key, which the security key did not sign with"
        }
    }
}

impl SecurityKeySigner<'_> {
    async fn sign(&mut self, data: &[u8]) -> Option<russh::keys::ssh_key::Signature> {
        let credential = sk::credential(self.key)?;
        let wants_pin = credential.flags & sk::USER_VERIFICATION != 0;
        let touch = credential.flags & sk::USER_PRESENCE != 0;
        let pin = if wants_pin {
            Some(self.pin().await?)
        } else {
            None
        };
        match self.attempt(pin, touch, data).await {
            // A key set to always verify (CTAP 2.1 `alwaysUv`) wants its
            // PIN though the file did not ask for one: asked once, as
            // OpenSSH does.
            Err(sk::SkError::Key(tauminus_fido::SecurityKeyError::PinRequired)) if !wants_pin => {
                let pin = self.pin().await?;
                self.attempt(Some(pin), touch, data).await.ok()
            }
            result => result.ok(),
        }
    }

    /// The key's PIN, from the user.
    async fn pin(&mut self) -> Option<Zeroizing<String>> {
        let answer = self
            .prompter
            .ask(Prompt::SecurityKeyPin {
                key_path: self.shown.to_owned(),
            })
            .await;
        if let Answer::Secret(pin) = answer {
            return Some(pin);
        }
        self.failed = Some("a FIDO2 key, whose PIN was not given");
        None
    }

    /// One request to the key, with the touch notice up while it waits.
    /// A failure is recorded in `failed`.
    async fn attempt(
        &mut self,
        pin: Option<Zeroizing<String>>,
        touch: bool,
        data: &[u8],
    ) -> Result<russh::keys::ssh_key::Signature, sk::SkError> {
        const CANCELLED: sk::SkError = sk::SkError::Answer("cancelled");
        let signing = sk::sign(self.security_key, self.key, pin, data);
        let result = if touch {
            // Dropped once the key answers: the touch notice goes away.
            let asked = self.prompter.ask(Prompt::SecurityKeyTouch {
                key_path: self.shown.to_owned(),
            });
            tokio::select! {
                biased;
                result = signing => result,
                _ = asked => {
                    self.failed = Some("a FIDO2 key, whose touch was cancelled");
                    return Err(CANCELLED);
                }
            }
        } else {
            // No touch bounds it: a key that never answers ends here.
            let Ok(result) = tokio::time::timeout(NO_TOUCH_WAIT, signing).await else {
                self.failed = Some("a FIDO2 key, which did not answer");
                return Err(CANCELLED);
            };
            result
        };
        result.inspect_err(|error| {
            debug!(%error, "the security key did not sign");
            self.failed = Some(security_key_failure(error));
        })
    }
}

impl Signer for SecurityKeySigner<'_> {
    type Error = SignError;

    async fn auth_sign(
        &mut self,
        identity: &AgentIdentity,
        hash: Option<HashAlg>,
        mut data: Vec<u8>,
    ) -> Result<Vec<u8>, SignError> {
        let Some(signature) = self.sign(&data).await else {
            return Ok(append_refused_signature(identity, hash, data));
        };
        let failed = |e: &dyn std::fmt::Display| SignError::Sign(e.to_string());
        let encoded = signature.encode_vec().map_err(|e| failed(&e))?;
        encoded.encode(&mut data).map_err(|e| failed(&e))?;
        Ok(data)
    }
}

/// Signs through ssh-agent. An agent that refuses, for example because the
/// user declined a confirmation, moves on to the next key.
struct AgentSigner<S: AgentStream + Send + Unpin>(AgentClient<S>);

impl<S: AgentStream + Send + Unpin + 'static> Signer for AgentSigner<S> {
    type Error = SignError;

    async fn auth_sign(
        &mut self,
        identity: &AgentIdentity,
        hash: Option<HashAlg>,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, SignError> {
        match self.0.sign_request(identity, hash, data.clone()).await {
            Ok(signed) => Ok(signed),
            Err(e) => {
                debug!(error = %e, "ssh-agent did not sign");
                Ok(append_refused_signature(identity, hash, data))
            }
        }
    }
}

/// `data` followed by its signature as an SSH string, as russh expects.
fn append_signature(
    key: &PrivateKey,
    hash: Option<HashAlg>,
    mut data: Vec<u8>,
) -> Result<Vec<u8>, SignError> {
    let failed = |e: &dyn std::fmt::Display| SignError::Sign(e.to_string());
    let signature = match key.key_data() {
        KeypairData::Rsa(rsa) => (rsa, hash).try_sign(&data),
        keypair => keypair.try_sign(&data),
    }
    .map_err(|e| failed(&e))?;
    let encoded = signature.encode_vec().map_err(|e| failed(&e))?;
    encoded.encode(&mut data).map_err(|e| failed(&e))?;
    Ok(data)
}

/// A well-formed signature the server will refuse. Sending it ends this
/// attempt with an ordinary failure, so authentication moves on; an error
/// instead would leave russh waiting for a signature for good. It costs one
/// attempt against the server's `MaxAuthTries`.
fn append_refused_signature(
    identity: &AgentIdentity,
    hash: Option<HashAlg>,
    mut data: Vec<u8>,
) -> Vec<u8> {
    let algorithm = match identity {
        AgentIdentity::PublicKey { key, .. } => key.algorithm(),
        AgentIdentity::Certificate { certificate, .. } => certificate.public_key().algorithm(),
    };
    let name = if matches!(algorithm, russh::keys::Algorithm::Rsa { .. }) {
        match hash {
            Some(HashAlg::Sha512) => "rsa-sha2-512",
            Some(HashAlg::Sha256) => "rsa-sha2-256",
            _ => "ssh-rsa",
        }
    } else {
        algorithm.as_str()
    };
    let mut blob = Vec::new();
    // Encoding into a Vec cannot fail.
    let _ = name.encode(&mut blob);
    let _ = [0u8; 64].as_slice().encode(&mut blob);
    let _ = blob.encode(&mut data);
    data
}

fn method_name(method: AuthMethod) -> &'static str {
    match method {
        AuthMethod::Agent => "ssh-agent",
        AuthMethod::PublicKey => "public key",
        AuthMethod::KeyboardInteractive => "keyboard-interactive",
        AuthMethod::Password => "password",
    }
}

fn protocol(error: russh::Error) -> ConnectError {
    crate::link_or_protocol(error)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// `~/x` and `~` become paths under `home`, as in `ssh_config`.
fn expand_home(path: &Path, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~"), home) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

async fn read_certificate(path: &Path) -> Option<Certificate> {
    let text = read_key_file(path).await?;
    Certificate::from_openssh(text.trim()).ok()
}

/// Most bytes read from a key, public key, or certificate file. An RSA
/// 16384-bit private key is about 12 KiB.
const MAX_KEY_FILE: u64 = 64 * 1024;

/// A key file's text, if it is a regular file of sane size. The type is
/// checked before opening: opening a FIFO would wait for a writer forever,
/// and `/dev/zero` would never end. Identity paths can come from an
/// imported config, so they are not trusted to point at a key.
async fn read_key_file(path: &Path) -> Option<Zeroizing<String>> {
    use tokio::io::AsyncReadExt as _;
    let metadata = tokio::fs::metadata(path).await.ok()?;
    if !metadata.is_file() || metadata.len() > MAX_KEY_FILE {
        return None;
    }
    let file = tokio::fs::File::open(path).await.ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_KEY_FILE + 1)
        .read_to_end(&mut bytes)
        .await
        .ok()?;
    if bytes.len() as u64 > MAX_KEY_FILE {
        return None;
    }
    String::from_utf8(bytes).ok().map(Zeroizing::new)
}

/// Where to find ssh-agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AgentSource {
    /// `SSH_AUTH_SOCK` on Unix; the OpenSSH agent pipe on Windows.
    #[default]
    Environment,
    /// A socket (Unix) or named pipe (Windows) path, like `IdentityAgent`.
    Path(PathBuf),
    /// PuTTY's Pageant. Windows only; elsewhere there is no agent.
    Pageant,
    /// Do not use an agent.
    None,
}

#[cfg(unix)]
async fn connect_agent(
    source: &AgentSource,
) -> Option<AgentClient<impl AgentStream + Send + Unpin + use<>>> {
    match source {
        AgentSource::Environment => AgentClient::connect_env().await.ok(),
        AgentSource::Path(path) => AgentClient::connect_uds(path).await.ok(),
        AgentSource::Pageant | AgentSource::None => None,
    }
}

/// The first agent that answers and whose pipe has a trusted owner; see
/// `windows_agent`. O(c) for c candidates, at most two.
#[cfg(windows)]
async fn connect_agent(
    source: &AgentSource,
) -> Option<AgentClient<impl AgentStream + Send + Unpin + use<>>> {
    for candidate in crate::windows_agent::candidates(source, std::env::var_os("SSH_AUTH_SOCK")) {
        match crate::windows_agent::connect(&candidate).await {
            Ok(pipe) => return Some(AgentClient::connect(pipe)),
            // Someone else's pipe under an agent's name: worth a warning.
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                tracing::warn!(?candidate, %error, "refusing an agent pipe");
            }
            Err(error) => tracing::debug!(?candidate, %error, "no agent here"),
        }
    }
    None
}

/// One hidden field that names a password is how PAM asks for one. A
/// one-time code or a token also comes as one hidden field, sometimes called
/// a "one-time password", and must not get the stored password.
fn asks_for_password(text: &str, echo: bool) -> bool {
    let text = text.to_ascii_lowercase();
    !echo
        && text.contains("password")
        && ![
            "one-time",
            "one time",
            "otp",
            "oath",
            "token",
            "code",
            "verification",
            "pin",
        ]
        .iter()
        .any(|word| text.contains(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_plain_password_prompt_gets_the_stored_password() {
        assert!(asks_for_password("Password: ", false));
        assert!(asks_for_password("alice@host's password:", false));
        assert!(!asks_for_password("Password: ", true));
        assert!(!asks_for_password("Verification code: ", false));
        assert!(!asks_for_password(
            "One-time password (OATH) for `alice':",
            false
        ));
        assert!(!asks_for_password(
            "Enter PASSCODE / token password:",
            false
        ));
        assert!(!asks_for_password("Passwort:", false));
    }

    #[tokio::test]
    async fn only_small_regular_files_are_read_as_keys() {
        let dir = std::env::temp_dir().join(format!("tauminus-keyread-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("small"), "key text").unwrap();
        let big = std::fs::File::create(dir.join("big")).unwrap();
        big.set_len(MAX_KEY_FILE + 1).unwrap();
        assert_eq!(
            read_key_file(&dir.join("small"))
                .await
                .as_deref()
                .map(String::as_str),
            Some("key text")
        );
        assert!(read_key_file(&dir.join("big")).await.is_none());
        assert!(read_key_file(&dir).await.is_none(), "a directory");
        #[cfg(unix)]
        {
            assert!(read_key_file(Path::new("/dev/zero")).await.is_none());
            // A FIFO must be refused without waiting for a writer.
            let fifo = dir.join("fifo");
            let status = std::process::Command::new("mkfifo").arg(&fifo).status();
            if status.is_ok_and(|s| s.success()) {
                let read =
                    tokio::time::timeout(std::time::Duration::from_secs(2), read_key_file(&fifo))
                        .await;
                assert!(matches!(read, Ok(None)), "returned at once with nothing");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tilde_paths_expand_under_home() {
        let home = Path::new("/home/alice");
        assert_eq!(
            expand_home(Path::new("~/.ssh/id_work"), Some(home)),
            home.join(".ssh/id_work")
        );
        assert_eq!(
            expand_home(Path::new("/keys/id"), Some(home)),
            PathBuf::from("/keys/id")
        );
        assert_eq!(expand_home(Path::new("~/k"), None), PathBuf::from("~/k"));
    }

    #[test]
    fn suffixes_append_to_the_file_name() {
        assert_eq!(
            with_suffix(Path::new("/k/id_ed25519"), "-cert.pub"),
            PathBuf::from("/k/id_ed25519-cert.pub")
        );
    }

    #[test]
    fn unknown_or_empty_server_methods_allow_everything() {
        let mut server = Server {
            allowed: None,
            partial: false,
        };
        assert!(server.permits(MethodKind::Password));
        let failure = |methods: &[MethodKind], partial_success| AuthResult::Failure {
            remaining_methods: russh::MethodSet::from(methods),
            partial_success,
        };
        assert!(!server.settle(failure(&[MethodKind::PublicKey], false)));
        assert!(!server.permits(MethodKind::Password));
        assert!(server.permits(MethodKind::PublicKey));
        assert!(!server.partial);
        server.settle(failure(&[], true));
        assert!(server.permits(MethodKind::Password));
        assert!(server.partial);
    }
}
