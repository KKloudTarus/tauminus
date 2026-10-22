// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The OS keychain through keyring-core: Keychain Services on macOS, the
//! Credential Manager on Windows, the Secret Service on Linux and the BSDs.
//! Each call blocks on the platform's service; callers run it off the async
//! workers.

use zeroize::Zeroizing;

use crate::KeyStore;

/// Names every entry Tauminus keeps, so the user can find them.
const SERVICE: &str = "io.github.kkloudtarus.tauminus";

/// The platform's credential store, or why there is none (for example, no
/// Secret Service on the session bus).
///
/// The store opens on first use, not at startup: reaching the Secret Service
/// can wait on D-Bus timeouts, and every call already runs off the async
/// workers.
#[derive(Debug, Default)]
pub struct OsKeychain {
    ready: std::sync::OnceLock<Result<(), String>>,
}

impl OsKeychain {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(target_os = "macos")]
fn platform_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, String> {
    apple_native_keyring_store::keychain::Store::new()
        .map(|s| s as std::sync::Arc<keyring_core::CredentialStore>)
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "windows")]
fn platform_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, String> {
    windows_native_keyring_store::Store::new()
        .map(|s| s as std::sync::Arc<keyring_core::CredentialStore>)
        .map_err(|e| e.to_string())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, String> {
    zbus_secret_service_keyring_store::Store::new()
        .map(|s| s as std::sync::Arc<keyring_core::CredentialStore>)
        .map_err(|e| format!("no Secret Service: {e}"))
}

#[cfg(not(any(unix, windows)))]
fn platform_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, String> {
    Err("no keychain on this platform".into())
}

impl KeyStore for OsKeychain {
    /// Opens the platform store once, making it keyring-core's default.
    fn available(&self) -> Result<(), String> {
        self.ready
            .get_or_init(|| platform_store().map(keyring_core::set_default_store))
            .clone()
    }

    fn get(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        self.available()?;
        let entry = keyring_core::Entry::new(SERVICE, name).map_err(|e| e.to_string())?;
        match entry.get_secret() {
            Ok(secret) => Ok(Some(Zeroizing::new(secret))),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, name: &str, secret: &[u8]) -> Result<(), String> {
        self.available()?;
        keyring_core::Entry::new(SERVICE, name)
            .and_then(|entry| entry.set_secret(secret))
            .map_err(|e| e.to_string())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        self.available()?;
        let entry = keyring_core::Entry::new(SERVICE, name).map_err(|e| e.to_string())?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}
