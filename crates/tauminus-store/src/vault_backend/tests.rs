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
        sync: false,
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

#[test]
fn forgetting_drops_only_the_generations_the_history_moved_past() {
    let (_dir, backend) = backend(Arc::default());
    let mut open = backend.create(&secret("correct horse")).unwrap();
    let (first, _) = open.history_keys().unwrap();
    open.change_password(&secret("battery staple 1")).unwrap();
    let (second, _) = open.history_keys().unwrap();
    assert_eq!(second.previous.len(), 1);
    assert_eq!(second.previous[0].database[..], first.database[..]);
    // The history starts moving past the first generation, and the password
    // changes again meanwhile.
    let known = second.previous.clone();
    open.change_password(&secret("battery staple 2")).unwrap();
    assert!(open.forget_previous_history_keys(&known));
    let (third, _) = open.history_keys().unwrap();
    assert_eq!(third.previous.len(), 1, "the second generation stays");
    assert_eq!(third.previous[0].database[..], second.database[..]);
    assert!(
        !open.forget_previous_history_keys(&known),
        "nothing more to drop"
    );
}

#[test]
fn recording_keys_are_made_once_and_outlive_a_password_change() {
    let (_dir, backend) = backend(Arc::default());
    let mut open = backend.create(&secret("correct horse")).unwrap();
    assert_eq!(backend.recordings_public(), None);
    let (first, made) = open.recording_keys().unwrap();
    assert!(made);
    open.save().unwrap();
    assert_eq!(backend.recordings_public(), Some(first.public));
    let mut open = backend.unlock(&secret("correct horse")).unwrap();
    open.change_password(&secret("battery staple")).unwrap();
    open.save().unwrap();
    // Every recording is sealed to this pair: a new password keeps it.
    let mut open = backend.unlock(&secret("battery staple")).unwrap();
    let (again, made) = open.recording_keys().unwrap();
    assert!(!made);
    assert_eq!(*first.secret, *again.secret);
    assert_eq!(first.public, again.public);
}

#[test]
fn the_ai_key_is_kept_with_its_origin_and_can_be_removed() {
    use tauminus_app::vault::AiKey;
    let (_dir, backend) = backend(Arc::default());
    let mut open = backend.create(&secret("correct horse")).unwrap();
    assert!(open.ai_key().unwrap().is_none());
    open.set_ai_key(Some(&AiKey {
        origin: "https://api.openai.com".into(),
        key: secret("sk-test"),
    }))
    .unwrap();
    open.save().unwrap();
    let mut open = backend.unlock(&secret("correct horse")).unwrap();
    let key = open.ai_key().unwrap().unwrap();
    assert_eq!(key.origin, "https://api.openai.com");
    assert_eq!(key.key.as_str(), "sk-test");
    assert!(open.identities().unwrap().is_empty(), "not an identity");
    assert!(!format!("{key:?}").contains("sk-test"));
    open.set_ai_key(None).unwrap();
    open.save().unwrap();
    let open = backend.unlock(&secret("correct horse")).unwrap();
    assert!(open.ai_key().unwrap().is_none());
}

#[test]
fn the_sync_setup_is_kept_with_its_target_and_can_be_removed() {
    use tauminus_app::ports::{S3Target, SyncTarget};
    use tauminus_app::vault::SyncSetup;
    let (_dir, backend) = backend(Arc::default());
    let mut open = backend.create(&secret("correct horse")).unwrap();
    assert!(open.sync_setup().unwrap().is_none());
    for target in [
        SyncTarget::Git(secret("https://user:token@example.com/sync.git")),
        SyncTarget::Folder("/srv/shared/tauminus".into()),
        SyncTarget::Folder(String::new()),
        SyncTarget::S3(S3Target {
            endpoint: "https://s3.example.com".into(),
            region: "auto".into(),
            bucket: "team".into(),
            prefix: String::new(),
            key_id: "AKID".into(),
            secret: secret("token-s3"),
        }),
    ] {
        open.set_sync_setup(Some(&SyncSetup {
            target: target.clone(),
            key: Zeroizing::new([7; 32]),
        }))
        .unwrap();
        open.save().unwrap();
        let again = backend.unlock(&secret("correct horse")).unwrap();
        let setup = again.sync_setup().unwrap().unwrap();
        assert_eq!(setup.target, target);
        assert_eq!(*setup.key, [7; 32]);
        assert!(again.identities().unwrap().is_empty(), "not an identity");
        assert!(!format!("{setup:?}").contains("token"));
        open = again;
    }
    let nul = SyncSetup {
        target: SyncTarget::Folder("a\0b".into()),
        key: Zeroizing::new([7; 32]),
    };
    assert!(open.set_sync_setup(Some(&nul)).is_err());
    open.set_sync_setup(None).unwrap();
    open.save().unwrap();
    let open = backend.unlock(&secret("correct horse")).unwrap();
    assert!(open.sync_setup().unwrap().is_none());
}
