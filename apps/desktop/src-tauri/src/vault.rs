// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The vault over IPC. Secrets go in and never come back out: the view
//! sends a password or a key to store, and gets back only whether one is
//! stored. Every secret is moved into a wiped buffer as soon as it arrives.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauminus_app::ports::Secret;
use tauminus_app::vault::{
    IdentityInput, IdentitySummary, KeychainState, MAX_SECRET, PresenceError, Vault, VaultError,
    VaultStatus,
};
use tauri::State;

use crate::ipc::{CommandError, ErrorCode};

impl From<VaultError> for CommandError {
    fn from(error: VaultError) -> Self {
        let code = match &error {
            VaultError::Locked => ErrorCode::VaultLocked,
            VaultError::WrongKey => ErrorCode::WrongPassword,
            VaultError::Missing | VaultError::NoSuchIdentity(_) => ErrorCode::NotFound,
            VaultError::Exists | VaultError::Invalid(_) => ErrorCode::InvalidArgument,
            VaultError::KeychainUnavailable(_)
            | VaultError::Presence(PresenceError::Unavailable(_)) => ErrorCode::Unavailable,
            VaultError::Corrupt(_) | VaultError::Io(_) | VaultError::Conflict => {
                ErrorCode::StoreFailed
            }
            VaultError::Presence(PresenceError::Cancelled | PresenceError::TimedOut) => {
                ErrorCode::Cancelled
            }
            VaultError::Presence(PresenceError::Denied) => ErrorCode::AuthFailed,
        };
        Self::new(code, error.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub(crate) enum KeychainSpec {
    Unavailable {
        reason: String,
    },
    Off,
    /// With `presence`, the OS confirms the user is present before the
    /// keychain's key is used.
    On {
        presence: bool,
    },
}

/// The OS's presence check: what it is called, and why it cannot run
/// here, when it cannot.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PresenceSpec {
    pub(crate) name: String,
    pub(crate) unavailable: Option<String>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VaultStatusSpec {
    pub(crate) exists: bool,
    pub(crate) unlocked: bool,
    pub(crate) keychain: KeychainSpec,
    pub(crate) presence: PresenceSpec,
    /// Minutes unused before the vault locks itself.
    pub(crate) idle_minutes: u32,
}

impl From<VaultStatus> for VaultStatusSpec {
    fn from(status: VaultStatus) -> Self {
        Self {
            exists: status.exists,
            unlocked: status.unlocked,
            keychain: match status.keychain {
                KeychainState::Unavailable(reason) => KeychainSpec::Unavailable { reason },
                KeychainState::Off => KeychainSpec::Off,
                KeychainState::On { presence } => KeychainSpec::On { presence },
            },
            presence: PresenceSpec {
                name: status.presence.name,
                unavailable: status.presence.available.err(),
            },
            idle_minutes: u32::try_from(status.idle.as_secs() / 60).unwrap_or(u32::MAX),
        }
    }
}

/// An identity as the view sees it: no secret, only whether one is stored.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IdentityView {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) user: Option<String>,
    pub(crate) has_password: bool,
    pub(crate) has_key: bool,
    pub(crate) public_key: Option<String>,
    /// This device shares it through sync.
    pub(crate) sync: bool,
}

impl From<IdentitySummary> for IdentityView {
    fn from(summary: IdentitySummary) -> Self {
        Self {
            id: summary.id,
            name: summary.name,
            user: summary.user,
            has_password: summary.has_password,
            has_key: summary.has_key,
            public_key: summary.public_key,
            sync: summary.sync,
        }
    }
}

/// A key Tauminus generates for an identity.
#[derive(Debug, Clone, Copy, Deserialize, Type)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum KeyKindSpec {
    Ed25519,
    EcdsaP256,
    EcdsaP384,
    EcdsaP521,
    Rsa3072,
    Rsa4096,
}

impl From<KeyKindSpec> for tauminus_domain::KeyKind {
    fn from(kind: KeyKindSpec) -> Self {
        match kind {
            KeyKindSpec::Ed25519 => Self::Ed25519,
            KeyKindSpec::EcdsaP256 => Self::EcdsaP256,
            KeyKindSpec::EcdsaP384 => Self::EcdsaP384,
            KeyKindSpec::EcdsaP521 => Self::EcdsaP521,
            KeyKindSpec::Rsa3072 => Self::Rsa3072,
            KeyKindSpec::Rsa4096 => Self::Rsa4096,
        }
    }
}

/// An edit. Secrets are write-only: leave one out to keep it, send "" to
/// remove it. `generate` makes a new key in place of `private_key`.
#[derive(Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IdentityEdit {
    #[serde(default)]
    pub(crate) id: Option<String>,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) user: Option<String>,
    #[serde(default)]
    pub(crate) password: Option<String>,
    #[serde(default)]
    pub(crate) private_key: Option<String>,
    #[serde(default)]
    pub(crate) passphrase: Option<String>,
    #[serde(default)]
    pub(crate) generate: Option<KeyKindSpec>,
    /// Whether this device shares it through sync; left out keeps it.
    #[serde(default)]
    pub(crate) sync: Option<bool>,
}

impl std::fmt::Debug for IdentityEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdentityEdit")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

fn secret(text: String) -> Secret {
    Secret::new(text)
}

type VaultState<'a> = State<'a, Arc<Vault>>;

#[tauri::command]
#[specta::specta]
pub(crate) async fn vault_status(vault: VaultState<'_>) -> Result<VaultStatusSpec, CommandError> {
    Ok(vault.status().await.into())
}

/// Creates the vault with a master password of at least 8 characters.
#[tauri::command]
#[specta::specta]
pub(crate) async fn vault_create(
    vault: VaultState<'_>,
    password: String,
) -> Result<(), CommandError> {
    Ok(vault.create(secret(password)).await?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn vault_unlock(
    vault: VaultState<'_>,
    password: String,
) -> Result<(), CommandError> {
    Ok(vault.unlock(secret(password)).await?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn vault_unlock_keychain(vault: VaultState<'_>) -> Result<(), CommandError> {
    Ok(vault.unlock_with_keychain().await?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn vault_lock(vault: VaultState<'_>) -> Result<(), CommandError> {
    vault.lock().await;
    Ok(())
}

/// Counts as activity for the idle timer.
#[tauri::command]
#[specta::specta]
pub(crate) async fn vault_touch(vault: VaultState<'_>) -> Result<(), CommandError> {
    vault.touch().await;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn vault_set_idle(
    vault: VaultState<'_>,
    minutes: u32,
) -> Result<(), CommandError> {
    Ok(vault
        .set_idle(Duration::from_secs(u64::from(minutes) * 60))
        .await?)
}

/// Replaces the master password; the current one is asked again. Returns
/// why the keychain was turned off, if it refused its new key: the password
/// changed all the same.
#[tauri::command]
#[specta::specta]
pub(crate) async fn vault_change_password(
    vault: VaultState<'_>,
    current: String,
    next: String,
) -> Result<Option<String>, CommandError> {
    Ok(vault.change_password(secret(current), secret(next)).await?)
}

/// Makes unlocking with the keychain ask the OS to confirm the user is
/// present (Touch ID, Windows Hello, polkit), or stop asking. The OS asks
/// once now, either way; the check runs here, not in the view.
#[tauri::command]
#[specta::specta]
pub(crate) async fn vault_set_presence(
    vault: VaultState<'_>,
    on: bool,
) -> Result<(), CommandError> {
    Ok(vault.set_presence(on).await?)
}

/// Lets the OS keychain unlock the vault, or stops it. Turning it on takes
/// the master password again.
#[tauri::command]
#[specta::specta]
pub(crate) async fn vault_set_keychain(
    vault: VaultState<'_>,
    on: bool,
    password: Option<String>,
) -> Result<(), CommandError> {
    Ok(vault.set_keychain(on, password.map(secret)).await?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn identities_list(
    vault: VaultState<'_>,
) -> Result<Vec<IdentityView>, CommandError> {
    Ok(vault
        .identities()
        .await?
        .into_iter()
        .map(IdentityView::from)
        .collect())
}

/// Adds or edits an identity. Returns its id.
#[tauri::command]
#[specta::specta]
pub(crate) async fn identity_save(
    vault: VaultState<'_>,
    gate: State<'_, crate::sync::Gate>,
    hosts: State<'_, crate::hosts::Hosts>,
    identity: IdentityEdit,
) -> Result<String, CommandError> {
    // Into wiped buffers first, so every return below wipes them.
    let IdentityEdit {
        id,
        name,
        user,
        password,
        private_key,
        passphrase,
        generate,
        sync,
    } = identity;
    let password = password.map(secret);
    let mut private_key = private_key.map(secret);
    let mut passphrase = passphrase.map(secret);
    let mut generated = None;
    if generate.is_some() && private_key.as_ref().is_some_and(|k| k.is_empty()) {
        return Err(CommandError::new(
            ErrorCode::InvalidArgument,
            "a key cannot be generated and removed at once",
        ));
    }
    if let Some(kind) = generate {
        // The comment names the identity, as ssh-keygen names user@host.
        let comment: String = name
            .chars()
            .filter(|c| c.is_ascii_graphic() && *c != '\'')
            .take(64)
            .collect();
        let key = tauri::async_runtime::spawn_blocking(move || {
            tauminus_ssh::generate_key(kind.into(), &format!("tauminus-{comment}"))
        })
        .await
        .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))?
        .map_err(|e| CommandError::new(ErrorCode::Internal, e))?;
        private_key = Some(Secret::new(key.private_key.to_string()));
        // A generated key has no passphrase: the vault encrypts it.
        passphrase = Some(Secret::new(String::new()));
        generated = Some(key.public_key);
    }
    let public_key = match &private_key {
        // Already known; decoding the new key again would only cost time.
        _ if generated.is_some() => generated,
        Some(pem) if !pem.is_empty() => {
            // Bounded before decoding: the key's own KDF runs on decode.
            if pem.len() > MAX_SECRET {
                return Err(CommandError::new(
                    ErrorCode::InvalidArgument,
                    "the private key is over 16 KiB",
                ));
            }
            let pem = pem.clone();
            let passphrase = passphrase.clone();
            let line = tauri::async_runtime::spawn_blocking(move || {
                public_key_of(&pem, passphrase.as_deref().map(String::as_str))
            })
            .await
            .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))??;
            Some(line)
        }
        _ => None,
    };
    let input = IdentityInput {
        id,
        name,
        user,
        password,
        private_key,
        passphrase,
        public_key,
        sync,
    };
    let (id, before, after) = vault.save_identity_printed(input).await?;
    gate.identities_edited(&hosts, &before, &after).await;
    Ok(id)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn identity_delete(
    vault: VaultState<'_>,
    gate: State<'_, crate::sync::Gate>,
    hosts: State<'_, crate::hosts::Hosts>,
    id: String,
) -> Result<(), CommandError> {
    let (before, after) = vault.delete_identity_printed(id).await?;
    gate.identities_edited(&hosts, &before, &after).await;
    Ok(())
}

/// The `authorized_keys` line of an OpenSSH private key, which must decode
/// with `passphrase` so a typo is caught when saving, not when connecting.
fn public_key_of(pem: &str, passphrase: Option<&str>) -> Result<String, CommandError> {
    let invalid = |e: &dyn std::fmt::Display| {
        CommandError::new(
            ErrorCode::InvalidArgument,
            format!("the private key does not open: {e}"),
        )
    };
    let passphrase = passphrase.filter(|p| !p.is_empty());
    tauminus_ssh::public_key_line(pem, passphrase).map_err(|e| invalid(&e))
}

#[cfg(test)]
mod tests;
