// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use super::*;

const TEST_KDF: Kdf = Kdf {
    memory_kib: 19 * 1024,
    iterations: 2,
    parallelism: 1,
};

#[derive(Default)]
struct MemoryKeys {
    entries: Mutex<HashMap<String, Vec<u8>>>,
    broken: Option<String>,
}

impl KeyStore for MemoryKeys {
    fn available(&self) -> Result<(), String> {
        self.broken.clone().map_or(Ok(()), Err)
    }

    fn get(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        Ok(self
            .entries
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .map(Zeroizing::new))
    }

    fn set(&self, name: &str, secret: &[u8]) -> Result<(), String> {
        self.entries
            .lock()
            .unwrap()
            .insert(name.into(), secret.to_vec());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        self.entries.lock().unwrap().remove(name);
        Ok(())
    }
}

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn backend(keys: Arc<MemoryKeys>) -> (Dir, FileVaultBackend) {
    backend_with(keys)
}

fn backend_with(keys: Arc<dyn KeyStore>) -> (Dir, FileVaultBackend) {
    let mut name = [0u8; 8];
    rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut name).unwrap();
    let dir = Dir(std::env::temp_dir()
        .join(format!("tauminus-vb-{name:02x?}").replace(['[', ']', ',', ' '], "")));
    let file = VaultFile::new(dir.0.join("vault.json"));
    (dir, FileVaultBackend::new(file, keys).with_kdf(TEST_KDF))
}

fn secret(text: &str) -> Secret {
    Secret::new(text.into())
}

fn identity(id: &str) -> Identity {
    Identity {
        id: id.into(),
        name: "Deploy".into(),
        user: Some("deploy".into()),
        password: Some(secret("hunter2")),
        private_key: Some(secret("-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n")),
        passphrase: None,
        public_key: Some("ssh-ed25519 AAAA deploy@host".into()),
    }
}

#[test]
fn identities_survive_the_file_and_stay_encrypted() {
    let (_dir, backend) = backend(Arc::default());
    let mut open = backend.create(&secret("correct horse")).unwrap();
    open.put_identity(&identity("a1")).unwrap();
    open.save().unwrap();
    let text = std::fs::read_to_string(backend.file.path()).unwrap();
    assert!(!text.contains("hunter2") && !text.contains("deploy") && !text.contains("OPENSSH"));
    let open = backend.unlock(&secret("correct horse")).unwrap();
    assert_eq!(open.identities().unwrap(), [identity("a1")]);
    assert!(matches!(
        backend.unlock(&secret("wrong password")),
        Err(VaultError::WrongKey)
    ));
}

#[test]
fn the_keychain_holds_a_key_only_while_turned_on() {
    let keys = Arc::new(MemoryKeys::default());
    let (_dir, backend) = backend(Arc::clone(&keys));
    let mut open = backend.create(&secret("correct horse")).unwrap();
    assert_eq!(backend.keychain(), KeychainState::Off);
    assert!(matches!(
        backend.unlock_with_keychain(false),
        Err(VaultError::KeychainUnavailable(_))
    ));
    open.set_keychain(true).unwrap();
    open.save().unwrap();
    assert_eq!(backend.keychain(), KeychainState::On { presence: false });
    assert_eq!(keys.entries.lock().unwrap().len(), 1);
    let mut open = backend.unlock_with_keychain(false).unwrap();
    open.set_keychain(false).unwrap();
    open.save().unwrap();
    assert!(keys.entries.lock().unwrap().is_empty());
    assert_eq!(backend.keychain(), KeychainState::Off);
    // A key that no longer matches the file opens nothing, and says so
    // without blaming a password nobody typed.
    keys.entries
        .lock()
        .unwrap()
        .insert(key_name(&backend.file.vault_id().unwrap()), vec![7; 32]);
    assert!(matches!(
        backend.unlock_with_keychain(false),
        Err(VaultError::KeychainUnavailable(_))
    ));
}

#[test]
fn a_new_password_gives_the_keychain_a_new_key() {
    let keys = Arc::new(MemoryKeys::default());
    let (_dir, backend) = backend(Arc::clone(&keys));
    let mut open = backend.create(&secret("correct horse")).unwrap();
    open.set_keychain(true).unwrap();
    open.save().unwrap();
    let name = key_name(&backend.file.vault_id().unwrap());
    let before = keys.entries.lock().unwrap()[&name].clone();
    open.change_password(&secret("battery staple")).unwrap();
    open.save().unwrap();
    assert_ne!(keys.entries.lock().unwrap()[&name], before);
    assert_eq!(backend.keychain(), KeychainState::On { presence: false });
    assert!(backend.unlock_with_keychain(false).is_ok());
    assert!(backend.unlock(&secret("battery staple")).is_ok());
}

#[test]
fn a_missing_keychain_is_reported() {
    let keys = Arc::new(MemoryKeys {
        broken: Some("no Secret Service on the session bus".into()),
        ..MemoryKeys::default()
    });
    let (_dir, backend) = backend(keys);
    let mut open = backend.create(&secret("correct horse")).unwrap();
    assert_eq!(
        backend.keychain(),
        KeychainState::Unavailable("no Secret Service on the session bus".into())
    );
    assert!(matches!(
        open.set_keychain(true),
        Err(VaultError::KeychainUnavailable(_))
    ));
}

#[test]
fn removed_identities_are_gone_after_saving() {
    let (_dir, backend) = backend(Arc::default());
    let mut open = backend.create(&secret("correct horse")).unwrap();
    open.put_identity(&identity("a1")).unwrap();
    open.put_identity(&identity("a2")).unwrap();
    assert!(open.remove_identity("a1"));
    assert!(!open.remove_identity("a1"));
    open.save().unwrap();
    let open = backend.unlock(&secret("correct horse")).unwrap();
    assert_eq!(open.identities().unwrap().len(), 1);
}

#[test]
fn one_identity_is_found_by_id() {
    let (_dir, backend) = backend(Arc::default());
    let mut open = backend.create(&secret("correct horse")).unwrap();
    open.put_identity(&identity("a1")).unwrap();
    open.put_identity(&identity("a2")).unwrap();
    assert_eq!(open.identity("a2").unwrap(), Some(identity("a2")));
    assert_eq!(open.identity("a3").unwrap(), None);
    assert_eq!(open.identity_count(), 2);
}

#[cfg(unix)]
#[test]
fn a_failed_save_leaves_the_keychain_able_to_open_the_file() {
    use std::os::unix::fs::PermissionsExt;
    let keys = Arc::new(MemoryKeys::default());
    let (dir, backend) = backend(Arc::clone(&keys));
    let mut open = backend.create(&secret("correct horse")).unwrap();
    open.set_keychain(true).unwrap();
    open.save().unwrap();
    let before = keys.entries.lock().unwrap().clone();
    open.change_password(&secret("battery staple")).unwrap();
    std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o500)).unwrap();
    let failed = open.save();
    std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(failed.is_err());
    assert_eq!(*keys.entries.lock().unwrap(), before);
    assert!(backend.unlock_with_keychain(false).is_ok());
    assert!(backend.unlock(&secret("correct horse")).is_ok());
}

/// A keychain that is reachable but refuses to store anything.
struct RefusingKeys;

impl KeyStore for RefusingKeys {
    fn available(&self) -> Result<(), String> {
        Ok(())
    }

    fn get(&self, _: &str) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        Ok(None)
    }

    fn set(&self, _: &str, _: &[u8]) -> Result<(), String> {
        Err("the keyring is locked".into())
    }

    fn delete(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn a_key_the_keychain_refuses_leaves_no_slot() {
    let (_dir, backend) = backend_with(Arc::new(RefusingKeys));
    let mut open = backend.create(&secret("correct horse")).unwrap();
    open.set_keychain(true).unwrap();
    assert!(matches!(
        open.save(),
        Err(VaultError::KeychainUnavailable(_))
    ));
    assert_eq!(backend.keychain(), KeychainState::Off);
    assert!(backend.unlock(&secret("correct horse")).is_ok());
}

#[test]
fn a_new_password_the_keychain_refuses_is_saved_with_the_keychain_off() {
    let (_dir, backend) = backend(Arc::default());
    let mut open = backend.create(&secret("correct horse")).unwrap();
    open.set_keychain(true).unwrap();
    open.save().unwrap();
    // The same file, once the keychain starts refusing new keys.
    let refusing =
        FileVaultBackend::new(backend.file.clone(), Arc::new(RefusingKeys)).with_kdf(TEST_KDF);
    let mut open = refusing.unlock(&secret("correct horse")).unwrap();
    open.change_password(&secret("battery staple")).unwrap();
    assert!(matches!(
        open.save(),
        Err(VaultError::KeychainUnavailable(_))
    ));
    assert_eq!(refusing.keychain(), KeychainState::Off);
    assert!(refusing.unlock(&secret("battery staple")).is_ok());
    assert!(matches!(
        refusing.unlock(&secret("correct horse")),
        Err(VaultError::WrongKey)
    ));
}

#[test]
fn history_keys_are_made_once_and_never_silently_replaced() {
    let (_dir, backend) = backend(Arc::default());
    let mut open = backend.create(&secret("correct horse")).unwrap();
    let (first, made) = open.history_keys().unwrap();
    assert!(made);
    open.save().unwrap();
    let mut open = backend.unlock(&secret("correct horse")).unwrap();
    let (again, made) = open.history_keys().unwrap();
    assert!(!made);
    assert_eq!(*first.database, *again.database);
    assert_eq!(backend.history_public(), Some(first.public));
}

#[test]
fn presence_swaps_the_keychain_key_and_slot() {
    let keys = Arc::new(MemoryKeys::default());
    let (_dir, backend) = backend(Arc::clone(&keys));
    let mut open = backend.create(&secret("correct horse")).unwrap();
    assert!(open.set_presence(true).is_err(), "needs the keychain on");
    open.set_keychain(true).unwrap();
    open.save().unwrap();
    let plain = keys
        .entries
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .unwrap();
    open.set_presence(true).unwrap();
    open.save().unwrap();
    assert_eq!(backend.keychain(), KeychainState::On { presence: true });
    let fresh = keys
        .entries
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .unwrap();
    assert_ne!(plain, fresh, "a fresh key");
    assert!(backend.unlock_with_keychain(false).is_err());
    let mut open = backend.unlock_with_keychain(true).unwrap();
    // A new password keeps the slot's kind.
    open.change_password(&secret("battery staple")).unwrap();
    open.save().unwrap();
    assert_eq!(backend.keychain(), KeychainState::On { presence: true });
    let mut open = backend.unlock_with_keychain(true).unwrap();
    open.set_presence(false).unwrap();
    open.save().unwrap();
    assert_eq!(backend.keychain(), KeychainState::On { presence: false });
    assert!(backend.unlock_with_keychain(false).is_ok());
}

#[test]
fn turning_the_keychain_off_drops_a_presence_slot_too() {
    let keys = Arc::new(MemoryKeys::default());
    let (_dir, backend) = backend(Arc::clone(&keys));
    let mut open = backend.create(&secret("correct horse")).unwrap();
    open.set_keychain(true).unwrap();
    open.set_presence(true).unwrap();
    open.save().unwrap();
    assert_eq!(backend.keychain(), KeychainState::On { presence: true });
    let mut open = backend.unlock(&secret("correct horse")).unwrap();
    open.set_keychain(false).unwrap();
    open.save().unwrap();
    assert_eq!(backend.keychain(), KeychainState::Off);
    assert_eq!(backend.file.keychain_slot().unwrap(), None);
    assert!(keys.entries.lock().unwrap().is_empty());
}

#[test]
fn turning_the_keychain_on_again_keeps_the_presence_check() {
    let (_dir, backend) = backend(Arc::default());
    let mut open = backend.create(&secret("correct horse")).unwrap();
    open.set_keychain(true).unwrap();
    open.set_presence(true).unwrap();
    open.set_keychain(true).unwrap();
    open.save().unwrap();
    assert_eq!(backend.keychain(), KeychainState::On { presence: true });
}

#[test]
fn a_presence_key_the_keychain_refuses_leaves_no_slot() {
    let (_dir, backend) = backend(Arc::default());
    let mut open = backend.create(&secret("correct horse")).unwrap();
    open.set_keychain(true).unwrap();
    open.save().unwrap();
    let refusing =
        FileVaultBackend::new(backend.file.clone(), Arc::new(RefusingKeys)).with_kdf(TEST_KDF);
    let mut open = refusing.unlock(&secret("correct horse")).unwrap();
    open.set_presence(true).unwrap();
    assert!(matches!(
        open.save(),
        Err(VaultError::KeychainUnavailable(_))
    ));
    assert_eq!(refusing.keychain(), KeychainState::Off);
    assert!(refusing.unlock(&secret("correct horse")).is_ok());
}
