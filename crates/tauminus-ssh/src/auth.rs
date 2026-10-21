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
use russh::keys::{Certificate, HashAlg, PrivateKey, PublicKey, decode_secret_key};
use tauminus_app::ports::{Answer, ChallengeField, ConnectError, Prompt, Prompter};
use tauminus_domain::{AuthMethod, AuthOptions, Endpoint};
use tracing::debug;
use zeroize::Zeroizing;

use crate::Client;

/// Default key files, strongest first. Hardware-backed `*_sk` keys need a
/// FIDO device and are used through an agent instead.
const DEFAULT_KEYS: &[&str] = &["id_ed25519", "id_ecdsa", "id_rsa"];
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
        let mut tried: Vec<String> = Vec::new();
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
                        self.keyboard_interactive(&user, &mut server).await?
                    }
                    AuthMethod::Password => self.password(&user, &mut server).await?,
                };
                match outcome {
                    Outcome::Done => return Ok(()),
                    Outcome::Tried => {
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
        Err(ConnectError::AuthFailed {
            endpoint: self.endpoint.to_string(),
            tried,
        })
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
            let mut signer = KeyFileSigner {
                path: &path,
                text: &text,
                prompter: self.prompter,
                key: None,
                gave_up: false,
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
            if server.settle(result) {
                return Ok(Outcome::Done);
            }
        }
        Ok(outcome)
    }

    async fn keyboard_interactive(
        &mut self,
        user: &str,
        server: &mut Server,
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
                    } else {
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

    async fn password(&mut self, user: &str, server: &mut Server) -> Result<Outcome, ConnectError> {
        let mut outcome = Outcome::Skipped;
        for _ in 0..ATTEMPTS {
            if !server.permits(MethodKind::Password) || server.partial {
                break;
            }
            let answer = self
                .prompter
                .ask(Prompt::Password {
                    endpoint: self.endpoint.to_string(),
                })
                .await;
            let Answer::Secret(password) = answer else {
                return Err(ConnectError::Cancelled);
            };
            let result = self
                .handle
                .authenticate_password(user, password.as_str())
                .await
                .map_err(protocol)?;
            outcome = Outcome::Tried;
            if server.settle(result) {
                return Ok(Outcome::Done);
            }
        }
        Ok(outcome)
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
        if self.options.identities_only {
            return Vec::new();
        }
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
            Self::Send => {
                ConnectError::Protocol("the connection closed during authentication".into())
            }
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
    key: Option<Arc<PrivateKey>>,
    gave_up: bool,
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
        match self.unlock().await {
            Some(key) => append_signature(&key, hash, data),
            None => Ok(append_refused_signature(identity, hash, data)),
        }
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

fn protocol(error: impl std::fmt::Display) -> ConnectError {
    ConnectError::Protocol(error.to_string())
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
        AgentSource::None => None,
    }
}

#[cfg(windows)]
async fn connect_agent(
    source: &AgentSource,
) -> Option<AgentClient<impl AgentStream + Send + Unpin + use<>>> {
    match source {
        AgentSource::Environment => AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent")
            .await
            .ok(),
        AgentSource::Path(path) => AgentClient::connect_named_pipe(path).await.ok(),
        AgentSource::None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
