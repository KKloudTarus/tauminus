// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The vault use case: locking, unlocking, auto-lock, and the identities it
//! keeps. Encryption and the file belong to the adapter behind
//! [`VaultBackend`]; this module decides when the decrypted contents exist.
//!
//! While locked, nothing decrypted is held. Unlocking keeps the opened vault
//! in memory until [`Vault::lock`], or until it goes unused for the idle
//! time. Secrets leave only through [`Vault::credentials`], for a connect;
//! the view sees [`IdentitySummary`] values, which hold none.

use std::sync::{Arc, Weak};
use std::time::Duration;

use thiserror::Error;
use tokio::sync::{Mutex, watch};
use tokio::time::Instant;
use zeroize::Zeroizing;

use crate::ports::Secret;

/// Idle time before the vault locks itself, unless changed.
pub const DEFAULT_IDLE: Duration = Duration::from_mins(15);
/// Shortest and longest idle time a user may choose.
pub const MIN_IDLE: Duration = Duration::from_mins(1);
pub const MAX_IDLE: Duration = Duration::from_hours(24);
/// Most identities one vault keeps.
pub const MAX_IDENTITIES: usize = 1000;
const MAX_ID: usize = 64;
const MAX_NAME: usize = 128;
/// Largest password, key, or passphrase an identity stores.
pub const MAX_SECRET: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VaultError {
    #[error("no vault exists yet")]
    Missing,
    #[error("a vault already exists")]
    Exists,
    #[error("the vault is locked")]
    Locked,
    #[error("the password is wrong")]
    WrongKey,
    #[error("no identity {0}")]
    NoSuchIdentity(String),
    #[error("{0}")]
    Invalid(String),
    #[error("the OS keychain is unavailable: {0}")]
    KeychainUnavailable(String),
    #[error("the vault file is damaged: {0}")]
    Corrupt(String),
    #[error("could not read or write the vault: {0}")]
    Io(String),
    /// Another window saved the vault; this one locked to reload it.
    #[error("the vault changed in another window; unlock it again to reload")]
    Conflict,
    /// The OS did not confirm the user is present; the password still
    /// unlocks.
    #[error("{0}; the master password still unlocks the vault")]
    Presence(PresenceError),
}

/// Why the OS did not confirm the user is present.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PresenceError {
    #[error("the system did not confirm it is you")]
    Denied,
    #[error("the system check was cancelled")]
    Cancelled,
    #[error("the system check got no answer in time")]
    TimedOut,
    #[error("this system cannot confirm it is you: {0}")]
    Unavailable(String),
}

/// Asks the OS to confirm the user is at the machine before the keychain's
/// key is used: Touch ID or the login password on macOS, Windows Hello,
/// polkit on Linux. It guards the app's own unlock; a program running as
/// the user can still read the keychain entry.
pub trait Presence: Send + Sync + 'static {
    /// What the check is called here, for the settings.
    fn name(&self) -> String;
    /// `Err` with the reason when this system cannot ask.
    fn available(&self) -> Result<(), String>;
    /// Asks, saying what for, and blocks until the user answers, gives up,
    /// or [`PRESENCE_TIMEOUT`] passes. Asks afresh every time: no earlier
    /// answer is reused.
    fn confirm(&self, purpose: Purpose) -> Result<(), PresenceError>;
}

/// What a presence check is asked for, so the OS's prompt says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Unlock,
    /// Turning the check on or off.
    Change,
}

impl Purpose {
    /// The prompt's reason, as the OS shows it.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Unlock => "unlock the Tauminus vault",
            Self::Change => "change how the Tauminus vault unlocks",
        }
    }
}

/// The longest a presence prompt waits for the user.
pub const PRESENCE_TIMEOUT: Duration = Duration::from_secs(60);

/// No presence check on this system.
#[derive(Debug, Default)]
pub struct NoPresence;

impl Presence for NoPresence {
    fn name(&self) -> String {
        "presence check".into()
    }

    fn available(&self) -> Result<(), String> {
        Err("not built for this system".into())
    }

    fn confirm(&self, _purpose: Purpose) -> Result<(), PresenceError> {
        Err(PresenceError::Unavailable(
            "not built for this system".into(),
        ))
    }
}

/// Whether the OS can confirm presence, and what it is called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceState {
    pub name: String,
    /// `Err` with the reason when it cannot.
    pub available: Result<(), String>,
}

/// Whether the OS keychain can unlock the vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeychainState {
    /// No keychain on this system, with the reason.
    Unavailable(String),
    Off,
    /// On; with `presence`, the OS confirms the user is present first.
    On {
        presence: bool,
    },
}

/// A reusable login: user, password, and key, any of which may be absent.
#[derive(Clone, PartialEq, Eq)]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub user: Option<String>,
    pub password: Option<Secret>,
    /// An OpenSSH private key, as `ssh-keygen` writes it.
    pub private_key: Option<Secret>,
    /// The key's passphrase, when it has one.
    pub passphrase: Option<Secret>,
    /// The key's public half, in `authorized_keys` form, kept beside it so
    /// the list can show it without decrypting the key.
    pub public_key: Option<String>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| ".."))
            .field("private_key", &self.private_key.as_ref().map(|_| ".."))
            .finish_non_exhaustive()
    }
}

/// What the view may see of an identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentitySummary {
    pub id: String,
    pub name: String,
    pub user: Option<String>,
    pub has_password: bool,
    pub has_key: bool,
    pub public_key: Option<String>,
}

impl From<&Identity> for IdentitySummary {
    fn from(identity: &Identity) -> Self {
        Self {
            id: identity.id.clone(),
            name: identity.name.clone(),
            user: identity.user.clone(),
            has_password: identity.password.is_some(),
            has_key: identity.private_key.is_some(),
            public_key: identity.public_key.clone(),
        }
    }
}

/// An edit from the view. Secrets are write-only: `None` keeps what the
/// vault holds, `Some("")` removes it.
#[derive(Debug, Clone, Default)]
pub struct IdentityInput {
    /// `None` for a new identity.
    pub id: Option<String>,
    pub name: String,
    pub user: Option<String>,
    pub password: Option<Secret>,
    pub private_key: Option<Secret>,
    pub passphrase: Option<Secret>,
    pub public_key: Option<String>,
}

/// What a connect may use from an identity.
#[derive(Clone, Default)]
pub struct Credentials {
    pub user: Option<String>,
    pub password: Option<Secret>,
    pub private_key: Option<Secret>,
    pub passphrase: Option<Secret>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

/// The vault file and the keychain, behind the adapter. Every method may
/// block (Argon2id, disk, keychain); the use case runs them off the async
/// workers.
pub trait VaultBackend: Send + Sync + 'static {
    fn exists(&self) -> bool;
    fn create(&self, password: &Secret) -> Result<Box<dyn OpenVault>, VaultError>;
    fn unlock(&self, password: &Secret) -> Result<Box<dyn OpenVault>, VaultError>;
    /// Opens with the keychain's key through a slot that asks for the
    /// presence check, or one that does not: the caller ran the check.
    fn unlock_with_keychain(&self, presence: bool) -> Result<Box<dyn OpenVault>, VaultError>;
    fn keychain(&self) -> KeychainState;
    fn new_id(&self) -> String;
    /// The history's public key, read without unlocking; `None` before the
    /// history's keys exist.
    fn history_public(&self) -> Option<[u8; 32]>;
    /// The recordings' public key, read without unlocking; `None` before
    /// the recordings' keys exist.
    fn recordings_public(&self) -> Option<[u8; 32]>;
}

/// An unlocked vault. Changes are kept in memory until `save`.
pub trait OpenVault: Send {
    fn identities(&self) -> Result<Vec<Identity>, VaultError>;
    /// One identity, decoding only its record. O(log n).
    fn identity(&self, id: &str) -> Result<Option<Identity>, VaultError>;
    fn identity_count(&self) -> usize;
    fn put_identity(&mut self, identity: &Identity) -> Result<(), VaultError>;
    fn remove_identity(&mut self, id: &str) -> bool;
    fn change_password(&mut self, password: &Secret) -> Result<(), VaultError>;
    /// Stores a fresh key in the keychain and a slot for it, or removes both.
    fn set_keychain(&mut self, on: bool) -> Result<(), VaultError>;
    /// Replaces the keychain slot with one that asks for the presence check
    /// before it is used, or one that does not, under a fresh key.
    fn set_presence(&mut self, on: bool) -> Result<(), VaultError>;
    /// The history's keys, made on first use. `true` when they were just
    /// made and the vault must be saved. A password change replaces them,
    /// keeping the old ones as `previous` until the history has moved.
    fn history_keys(&mut self) -> Result<(HistoryKeys, bool), VaultError>;
    /// Drops the given generations of the history's earlier keys, once the
    /// history no longer needs them; generations added since (a password
    /// change while the history moved) stay. `true` when the vault must be
    /// saved.
    fn forget_previous_history_keys(&mut self, known: &[PreviousHistoryKeys]) -> bool;
    /// The recordings' key pair, made on first use; `true` when it was
    /// just made and the vault must be saved. It outlives password changes:
    /// a recording is sealed to it for good.
    fn recording_keys(&mut self) -> Result<(RecordingKeys, bool), VaultError>;
    /// The AI assistant's API key, if one is saved.
    fn ai_key(&self) -> Result<Option<AiKey>, VaultError>;
    /// Saves the AI assistant's API key, or removes it.
    fn set_ai_key(&mut self, key: Option<&AiKey>) -> Result<(), VaultError>;
    /// Writes the vault. Fails with [`VaultError::Conflict`] when another
    /// window saved it since it was read.
    fn save(&mut self) -> Result<(), VaultError>;
}

/// An API key for the AI assistant, and the one origin it may be sent to.
/// Wiped when dropped.
#[derive(Clone)]
pub struct AiKey {
    pub origin: String,
    pub key: Secret,
}

impl std::fmt::Debug for AiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiKey")
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

/// The key pair recordings are sealed to. Wiped when dropped.
pub struct RecordingKeys {
    pub secret: Zeroizing<[u8; 32]>,
    pub public: [u8; 32],
}

impl std::fmt::Debug for RecordingKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordingKeys")
            .field("public", &self.public)
            .finish_non_exhaustive()
    }
}

/// The history's keys: one opens its database, the other opens commands
/// sealed while the vault was locked. Wiped when dropped.
#[derive(Clone)]
pub struct HistoryKeys {
    pub database: Zeroizing<[u8; 32]>,
    pub sealing: Zeroizing<[u8; 32]>,
    pub public: [u8; 32],
    /// The keys before each password change since the history last moved
    /// to the current ones, newest first: the database may still be under
    /// one of them, and entries sealed while locked may be sealed to any.
    pub previous: Vec<PreviousHistoryKeys>,
}

/// The history's keys before a password change. Wiped when dropped.
#[derive(Clone)]
pub struct PreviousHistoryKeys {
    pub database: Zeroizing<[u8; 32]>,
    pub sealing: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for PreviousHistoryKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviousHistoryKeys")
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for HistoryKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryKeys").finish_non_exhaustive()
    }
}

/// The vault's state for the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultStatus {
    pub exists: bool,
    pub unlocked: bool,
    pub keychain: KeychainState,
    pub presence: PresenceState,
    pub idle: Duration,
}

struct Open {
    vault: Box<dyn OpenVault>,
    used: Used,
}

struct State {
    open: Option<Open>,
    idle: Duration,
    /// Counts locks, so an unlock that was still deriving its key when the
    /// user locked does not open the vault after all.
    locks: u64,
    /// Counts unlocks: each time the vault opens, a new session.
    sessions: u64,
    /// Whether the vault is open, for whoever follows it (the history).
    unlocked: watch::Sender<bool>,
}

/// Locks and unlocks the vault and keeps it locked when unused. Operations
/// on the open vault are serialized by one async mutex; each costs O(i) in
/// the identities, i ≤ 1000.
pub struct Vault {
    backend: Arc<dyn VaultBackend>,
    presence: Arc<dyn Presence>,
    /// One presence prompt at a time.
    asking: Mutex<()>,
    state: Mutex<State>,
    /// One Argon2id run at a time: each takes the file's memory cost (64 MiB
    /// by default), so parallel unlocks must not add up.
    kdf: Mutex<()>,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault").finish_non_exhaustive()
    }
}

impl Vault {
    /// The vault, locked, with a task that locks it after `idle` unused.
    /// The task stops when the vault is dropped.
    pub fn new(backend: Arc<dyn VaultBackend>, idle: Duration) -> Arc<Self> {
        Self::with_presence(backend, idle, Arc::new(NoPresence))
    }

    /// As [`Vault::new`], with the OS's presence check for keychain unlock.
    pub fn with_presence(
        backend: Arc<dyn VaultBackend>,
        idle: Duration,
        presence: Arc<dyn Presence>,
    ) -> Arc<Self> {
        let vault = Arc::new(Self {
            backend,
            presence,
            asking: Mutex::new(()),
            state: Mutex::new(State {
                open: None,
                unlocked: watch::Sender::new(false),
                idle: idle.clamp(MIN_IDLE, MAX_IDLE),
                locks: 0,
                sessions: 0,
            }),
            kdf: Mutex::new(()),
        });
        tokio::spawn(auto_lock(Arc::downgrade(&vault)));
        vault
    }

    /// Follows whether the vault is open: true after an unlock, false after
    /// a lock, an idle lock, or a save that locked it.
    pub async fn watch(&self) -> watch::Receiver<bool> {
        self.state.lock().await.unlocked.subscribe()
    }

    /// Whether the vault is open now: past its idle time it locks first,
    /// so a wake from suspend cannot read a vault that should be locked.
    pub async fn is_open(&self) -> bool {
        self.session().await.is_some()
    }

    /// Which unlock the vault is open from, now: past its idle time it
    /// locks first. `None` while locked. A lock and an unlock in between
    /// give a new number, even to a follower that saw neither.
    pub async fn session(&self) -> Option<u64> {
        let mut state = self.state.lock().await;
        state.expire();
        state.open.is_some().then_some(state.sessions)
    }

    pub async fn status(&self) -> VaultStatus {
        let backend = Arc::clone(&self.backend);
        let presence = Arc::clone(&self.presence);
        let (exists, keychain, available) =
            blocking(move || Ok((backend.exists(), backend.keychain(), presence.available())))
                .await
                .unwrap_or_else(|e| {
                    (
                        false,
                        KeychainState::Unavailable(e.to_string()),
                        Err(e.to_string()),
                    )
                });
        let mut state = self.state.lock().await;
        state.expire();
        VaultStatus {
            exists,
            unlocked: state.open.is_some(),
            keychain,
            presence: PresenceState {
                name: self.presence.name(),
                available,
            },
            idle: state.idle,
        }
    }

    pub async fn create(&self, password: Secret) -> Result<(), VaultError> {
        check_password(&password)?;
        let backend = Arc::clone(&self.backend);
        let _kdf = self.kdf.lock().await;
        let locks = self.state.lock().await.locks;
        let opened = blocking(move || backend.create(&password)).await?;
        self.keep(opened, locks).await;
        Ok(())
    }

    pub async fn unlock(&self, password: Secret) -> Result<(), VaultError> {
        let backend = Arc::clone(&self.backend);
        let _kdf = self.kdf.lock().await;
        let locks = self.state.lock().await.locks;
        let opened = blocking(move || backend.unlock(&password)).await?;
        self.keep(opened, locks).await;
        Ok(())
    }

    /// Unlocks with the keychain's key, after the OS confirms the user is
    /// present when the slot asks for it. The slot's kind is read from the
    /// file, and only a slot of that kind opens: a file edited after the
    /// check opens nothing.
    pub async fn unlock_with_keychain(&self) -> Result<(), VaultError> {
        let locks = self.state.lock().await.locks;
        let backend = Arc::clone(&self.backend);
        let presence = matches!(
            blocking(move || Ok(backend.keychain())).await?,
            KeychainState::On { presence: true }
        );
        if presence {
            self.confirm(Purpose::Unlock).await?;
        }
        let backend = Arc::clone(&self.backend);
        let opened = blocking(move || backend.unlock_with_keychain(presence)).await?;
        self.keep(opened, locks).await;
        Ok(())
    }

    /// Makes unlocking with the keychain ask the OS to confirm the user is
    /// present, or stop asking. Either way the OS asks once now: turning it
    /// on shows it works, and turning it off takes the same proof.
    pub async fn set_presence(&self, on: bool) -> Result<(), VaultError> {
        if !self.is_open().await {
            return Err(VaultError::Locked);
        }
        // Nothing to change without a keychain slot: say so before asking.
        let backend = Arc::clone(&self.backend);
        if !matches!(
            blocking(move || Ok(backend.keychain())).await?,
            KeychainState::On { .. }
        ) {
            return Err(VaultError::Invalid(
                "turn on unlocking with the keychain first".into(),
            ));
        }
        self.confirm(Purpose::Change).await?;
        self.with_open(move |vault| {
            vault.set_presence(on)?;
            saved(vault)
        })
        .await
    }

    /// Asks the OS to confirm the user is present, one prompt at a time.
    async fn confirm(&self, purpose: Purpose) -> Result<(), VaultError> {
        let _one = self.asking.lock().await;
        let presence = Arc::clone(&self.presence);
        blocking(move || presence.confirm(purpose).map_err(VaultError::Presence)).await
    }

    /// Drops the decrypted contents. Safe to call when locked.
    pub async fn lock(&self) {
        let mut state = self.state.lock().await;
        state.open = None;
        state.locks += 1;
        state.publish();
    }

    /// Counts as use, for the idle timer; the view calls it on activity.
    pub async fn touch(&self) {
        let mut state = self.state.lock().await;
        state.expire();
        if let Some(open) = state.open.as_mut() {
            open.used = Used::now();
        }
    }

    pub async fn set_idle(&self, idle: Duration) -> Result<(), VaultError> {
        if !(MIN_IDLE..=MAX_IDLE).contains(&idle) {
            return Err(VaultError::Invalid(
                "the idle time must be between 1 minute and 24 hours".into(),
            ));
        }
        self.state.lock().await.idle = idle;
        Ok(())
    }

    pub async fn identities(&self) -> Result<Vec<IdentitySummary>, VaultError> {
        self.with_open(|vault| {
            let mut list: Vec<IdentitySummary> = vault
                .identities()?
                .iter()
                .map(IdentitySummary::from)
                .collect();
            // One lowercase copy per name, not one per comparison.
            list.sort_by_cached_key(|a| a.name.to_lowercase());
            Ok(list)
        })
        .await
    }

    /// What the view may see of identity `id`, without counting as use of
    /// the vault: an offer made because of output must not keep it open.
    /// O(log n).
    pub async fn peek(&self, id: &str) -> Result<Option<IdentitySummary>, VaultError> {
        let id = id.to_owned();
        self.with_open_as(false, move |vault| {
            Ok(vault.identity(&id)?.as_ref().map(IdentitySummary::from))
        })
        .await
    }

    /// Adds or edits an identity and saves the vault. Returns its id.
    pub async fn save_identity(&self, input: IdentityInput) -> Result<String, VaultError> {
        check_identity(&input)?;
        let new_id = self.backend.new_id();
        self.with_open(move |vault| {
            let current = if let Some(id) = &input.id {
                Some(
                    vault
                        .identity(id)?
                        .ok_or_else(|| VaultError::NoSuchIdentity(id.clone()))?,
                )
            } else if vault.identity_count() >= MAX_IDENTITIES {
                return Err(
                    VaultError::Invalid(format!("at most {MAX_IDENTITIES} identities")).into(),
                );
            } else {
                None
            };
            let keep = |given: Option<Secret>, old: Option<Secret>| match given {
                Some(secret) if secret.is_empty() => None,
                Some(secret) => Some(secret),
                None => old,
            };
            let old = current.clone();
            let key_changed = input.private_key.is_some();
            let identity = Identity {
                id: input.id.clone().unwrap_or(new_id),
                name: input.name.trim().to_owned(),
                user: input
                    .user
                    .as_deref()
                    .map(str::trim)
                    .filter(|u| !u.is_empty())
                    .map(str::to_owned),
                password: keep(
                    input.password.clone(),
                    old.as_ref().and_then(|o| o.password.clone()),
                ),
                private_key: keep(
                    input.private_key.clone(),
                    old.as_ref().and_then(|o| o.private_key.clone()),
                ),
                passphrase: keep(
                    input.passphrase.clone(),
                    old.as_ref().and_then(|o| o.passphrase.clone()),
                ),
                public_key: if key_changed {
                    input.public_key.clone()
                } else {
                    old.and_then(|o| o.public_key)
                },
            };
            let id = identity.id.clone();
            vault.put_identity(&identity)?;
            match vault.save() {
                Ok(()) => Ok(id),
                // Refused before writing (the vault is full): put back what
                // was there, and keep the vault open.
                Err(error @ VaultError::Invalid(_)) => {
                    match &current {
                        Some(previous) => vault.put_identity(previous)?,
                        None => {
                            vault.remove_identity(&id);
                        }
                    }
                    Err(error.into())
                }
                Err(error) => Err(Failed {
                    error,
                    discard: true,
                }),
            }
        })
        .await
    }

    pub async fn delete_identity(&self, id: String) -> Result<(), VaultError> {
        self.with_open(move |vault| {
            if !vault.remove_identity(&id) {
                return Err(VaultError::NoSuchIdentity(id).into());
            }
            saved(vault)
        })
        .await
    }

    /// An identity's public key line, if it has a key. O(log n).
    pub async fn public_key(&self, id: &str) -> Result<Option<String>, VaultError> {
        let id = id.to_owned();
        self.with_open(move |vault| {
            Ok(vault
                .identity(&id)?
                .ok_or(VaultError::NoSuchIdentity(id))?
                .public_key)
        })
        .await
    }

    /// The secrets an identity holds, for one connect.
    pub async fn credentials(&self, id: &str) -> Result<Credentials, VaultError> {
        let id = id.to_owned();
        self.with_open(move |vault| {
            let identity = vault.identity(&id)?.ok_or(VaultError::NoSuchIdentity(id))?;
            Ok(Credentials {
                user: identity.user,
                password: identity.password,
                private_key: identity.private_key,
                passphrase: identity.passphrase,
            })
        })
        .await
    }

    /// The AI assistant's API key. Needs the vault unlocked, and counts as
    /// use.
    pub async fn ai_key(&self) -> Result<Option<AiKey>, VaultError> {
        self.with_open(|vault| Ok(vault.ai_key()?)).await
    }

    /// Saves the AI assistant's API key, or removes it, and saves the vault.
    pub async fn save_ai_key(&self, key: Option<AiKey>) -> Result<(), VaultError> {
        self.with_open(move |vault| {
            vault.set_ai_key(key.as_ref())?;
            vault.save()?;
            Ok(())
        })
        .await
    }

    /// Replaces the master password. Needs the vault unlocked; the view
    /// asks for the current password again before calling it.
    ///
    /// Returns why the keychain was turned off, when it refused the new key:
    /// the password has changed all the same.
    pub async fn change_password(
        &self,
        current: Secret,
        next: Secret,
    ) -> Result<Option<String>, VaultError> {
        check_password(&next)?;
        let backend = Arc::clone(&self.backend);
        let _kdf = self.kdf.lock().await;
        // Proves the user knows the current password, not only that the
        // vault was left unlocked.
        blocking(move || backend.unlock(&current).map(drop)).await?;
        let changed = self
            .with_open(move |vault| {
                // A step that fails partway leaves the open copy half changed.
                vault.change_password(&next).map_err(|error| Failed {
                    error,
                    discard: true,
                })?;
                match saved(vault) {
                    Ok(()) => Ok(None),
                    Err(Failed {
                        error: VaultError::KeychainUnavailable(reason),
                        ..
                    }) => Ok(Some(reason)),
                    Err(failed) => Err(failed),
                }
            })
            .await;
        // The history's keys changed with the password: a new session makes
        // the history's next use reopen it and move it to them.
        if changed.is_ok() {
            let mut state = self.state.lock().await;
            state.sessions += 1;
            state.publish();
        }
        changed
    }

    /// Turning the keychain on needs the master password again: from then
    /// on the vault opens without it.
    pub async fn set_keychain(&self, on: bool, current: Option<Secret>) -> Result<(), VaultError> {
        if on {
            let current = current.ok_or_else(|| {
                VaultError::Invalid("the master password is needed to turn this on".into())
            })?;
            let backend = Arc::clone(&self.backend);
            let _kdf = self.kdf.lock().await;
            blocking(move || backend.unlock(&current).map(drop)).await?;
        }
        self.with_open(move |vault| {
            vault.set_keychain(on)?;
            saved(vault)
        })
        .await
    }

    /// Keeps an opened vault unless the user locked since `locks` was read.
    /// The history's keys, made and saved on first use. Needs the vault
    /// open.
    pub async fn history_keys(&self) -> Result<HistoryKeys, VaultError> {
        self.with_open(|vault| {
            let (keys, made) = vault.history_keys()?;
            if made {
                saved(vault)?;
            }
            Ok(keys)
        })
        .await
    }

    /// Drops the history's keys from before the last password change: the
    /// history has moved to the new ones.
    pub async fn forget_previous_history_keys(
        &self,
        known: Vec<PreviousHistoryKeys>,
    ) -> Result<(), VaultError> {
        self.with_open(move |vault| {
            if vault.forget_previous_history_keys(&known) {
                saved(vault)?;
            }
            Ok(())
        })
        .await
    }

    /// The recordings' key pair, made and saved on first use. Needs the
    /// vault open.
    pub async fn recording_keys(&self) -> Result<RecordingKeys, VaultError> {
        self.with_open(|vault| {
            let (keys, made) = vault.recording_keys()?;
            if made {
                saved(vault)?;
            }
            Ok(keys)
        })
        .await
    }

    /// The recordings' public key, while locked too.
    pub async fn recordings_public(&self) -> Option<[u8; 32]> {
        let backend = Arc::clone(&self.backend);
        blocking(move || Ok(backend.recordings_public()))
            .await
            .ok()
            .flatten()
    }

    /// The history's public key, while locked too.
    pub async fn history_public(&self) -> Option<[u8; 32]> {
        let backend = Arc::clone(&self.backend);
        blocking(move || Ok(backend.history_public()))
            .await
            .ok()
            .flatten()
    }

    async fn keep(&self, vault: Box<dyn OpenVault>, locks: u64) {
        let mut state = self.state.lock().await;
        if state.locks == locks {
            state.sessions += 1;
            state.open = Some(Open {
                vault,
                used: Used::now(),
            });
        }
        state.publish();
    }

    /// Runs `work` on the open vault, off the async workers, and counts it
    /// as use.
    async fn with_open<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut dyn OpenVault) -> Result<T, Failed> + Send + 'static,
    ) -> Result<T, VaultError> {
        self.with_open_as(true, work).await
    }

    /// As [`Vault::with_open`]; a call that is not `use` leaves the idle
    /// time as it was: what output from a host triggers must not keep the
    /// vault open.
    async fn with_open_as<T: Send + 'static>(
        &self,
        used: bool,
        work: impl FnOnce(&mut dyn OpenVault) -> Result<T, Failed> + Send + 'static,
    ) -> Result<T, VaultError> {
        let mut state = self.state.lock().await;
        state.expire();
        let mut open = state.open.take().ok_or(VaultError::Locked)?;
        let joined = tokio::task::spawn_blocking(move || {
            let result = work(open.vault.as_mut());
            (open, result)
        })
        .await;
        let Ok((open, result)) = joined else {
            state.publish();
            return Err(VaultError::Io("the vault worker stopped".into()));
        };
        // A save failed: memory holds a change the file does not, or the
        // file holds another window's change. Drop this copy, so what the
        // next unlock reads is what is on disk.
        if !matches!(result, Err(Failed { discard: true, .. })) {
            state.open = Some(if used {
                Open {
                    used: Used::now(),
                    ..open
                }
            } else {
                open
            });
        }
        state.publish();
        result.map_err(|failed| failed.error)
    }
}

/// Why work on the open vault failed, and whether the open copy no longer
/// matches the file.
struct Failed {
    error: VaultError,
    discard: bool,
}

impl From<VaultError> for Failed {
    /// Checks fail before anything changes, so the open copy stays.
    fn from(error: VaultError) -> Self {
        Self {
            error,
            discard: false,
        }
    }
}

/// Saves the open vault; on failure the caller's changes are dropped with it.
fn saved(vault: &mut dyn OpenVault) -> Result<(), Failed> {
    vault.save().map_err(|error| Failed {
        // The file was saved, only the keychain refused its key: memory and
        // file agree, so the vault stays open.
        discard: !matches!(error, VaultError::KeychainUnavailable(_)),
        error,
    })
}

/// When the vault was last used, on two clocks. The monotonic one stops
/// while the machine sleeps on Linux and macOS, so a vault left open over a
/// night's suspend would wake up open; the wall clock counts that sleep.
#[derive(Debug, Clone, Copy)]
struct Used {
    monotonic: Instant,
    wall: std::time::SystemTime,
}

impl Used {
    fn now() -> Self {
        Self {
            monotonic: Instant::now(),
            wall: std::time::SystemTime::now(),
        }
    }

    /// The longer of the two; a wall clock set back counts as no time.
    fn elapsed(self) -> Duration {
        let wall = self.wall.elapsed().unwrap_or_default();
        self.monotonic.elapsed().max(wall)
    }
}

/// Locks the vault once it has gone unused for its idle time. Wakes at most
/// every 10 s, so a lock lands within 10 s of the deadline.
async fn auto_lock(vault: Weak<Vault>) {
    const TICK: Duration = Duration::from_secs(10);
    loop {
        tokio::time::sleep(TICK).await;
        let Some(vault) = vault.upgrade() else {
            return;
        };
        vault.state.lock().await.expire();
    }
}

impl State {
    /// Locks a vault unused for its idle time. Every use checks first, so
    /// a wake from suspend cannot renew a vault the tick has not locked yet.
    fn expire(&mut self) {
        if self
            .open
            .as_ref()
            .is_some_and(|o| o.used.elapsed() >= self.idle)
        {
            self.open = None;
            tracing::info!("vault locked after {} s unused", self.idle.as_secs());
            self.publish();
        }
    }

    /// Tells followers whether the vault is open, when that changed.
    fn publish(&self) {
        let open = self.open.is_some();
        self.unlocked
            .send_if_modified(|was| std::mem::replace(was, open) != open);
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, VaultError> + Send + 'static,
) -> Result<T, VaultError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| VaultError::Io(e.to_string()))?
}

/// At least 8 characters: Argon2id slows guessing, it does not make a
/// short password long. At most 1024 bytes.
fn check_password(password: &Secret) -> Result<(), VaultError> {
    let chars = password.chars().count();
    if chars < 8 || password.len() > 1024 {
        return Err(VaultError::Invalid(
            "the master password must be 8 to 1024 characters".into(),
        ));
    }
    Ok(())
}

fn check_identity(input: &IdentityInput) -> Result<(), VaultError> {
    let invalid = |message: &str| Err(VaultError::Invalid(message.into()));
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
        return invalid("a name must be 1 to 128 characters with no control characters");
    }
    if let Some(id) = &input.id
        && (id.is_empty()
            || id.len() > MAX_ID
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    {
        return invalid("an identity id must be 1 to 64 letters, digits, '-' or '_'");
    }
    // Checked as a connect would, so a saved identity never fails there.
    if let Some(user) = input
        .user
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        && let Err(e) = tauminus_domain::UserName::parse(user)
    {
        return Err(VaultError::Invalid(e.to_string()));
    }
    for secret in [&input.password, &input.private_key, &input.passphrase]
        .into_iter()
        .flatten()
    {
        if secret.len() > MAX_SECRET {
            return invalid("a secret is over 16 KiB");
        }
    }
    if input
        .public_key
        .as_ref()
        .is_some_and(|k| k.len() > MAX_SECRET || k.chars().any(char::is_control))
    {
        return invalid("a public key must be one line of at most 16 KiB");
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
