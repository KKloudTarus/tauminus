// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// A vault in memory with one password.
#[derive(Default)]
struct Memory {
    password: StdMutex<Option<String>>,
    identities: Arc<StdMutex<Vec<Identity>>>,
    keychain: StdMutex<bool>,
    next: AtomicUsize,
    /// Saves fail with this, as on a full disk or another window's write.
    failing: StdMutex<Option<VaultError>>,
}

struct MemoryOpen {
    owner: Arc<Memory>,
    identities: Vec<Identity>,
}

impl VaultBackend for Arc<Memory> {
    fn exists(&self) -> bool {
        self.password.lock().unwrap().is_some()
    }

    fn create(&self, password: &Secret) -> Result<Box<dyn OpenVault>, VaultError> {
        let mut slot = self.password.lock().unwrap();
        if slot.is_some() {
            return Err(VaultError::Exists);
        }
        *slot = Some(password.as_str().to_owned());
        Ok(Box::new(MemoryOpen {
            owner: Arc::clone(self),
            identities: Vec::new(),
        }))
    }

    fn unlock(&self, password: &Secret) -> Result<Box<dyn OpenVault>, VaultError> {
        match self.password.lock().unwrap().as_deref() {
            None => Err(VaultError::Missing),
            Some(p) if p == password.as_str() => Ok(Box::new(MemoryOpen {
                owner: Arc::clone(self),
                identities: self.identities.lock().unwrap().clone(),
            })),
            Some(_) => Err(VaultError::WrongKey),
        }
    }

    fn unlock_with_keychain(&self) -> Result<Box<dyn OpenVault>, VaultError> {
        if !*self.keychain.lock().unwrap() {
            return Err(VaultError::KeychainUnavailable("off".into()));
        }
        Ok(Box::new(MemoryOpen {
            owner: Arc::clone(self),
            identities: self.identities.lock().unwrap().clone(),
        }))
    }

    fn keychain(&self) -> KeychainState {
        if *self.keychain.lock().unwrap() {
            KeychainState::On
        } else {
            KeychainState::Off
        }
    }

    fn new_id(&self) -> String {
        format!("id{}", self.next.fetch_add(1, Ordering::Relaxed))
    }
}

impl OpenVault for MemoryOpen {
    fn identities(&self) -> Result<Vec<Identity>, VaultError> {
        Ok(self.identities.clone())
    }

    fn identity(&self, id: &str) -> Result<Option<Identity>, VaultError> {
        Ok(self.identities.iter().find(|i| i.id == id).cloned())
    }

    fn identity_count(&self) -> usize {
        self.identities.len()
    }

    fn put_identity(&mut self, identity: &Identity) -> Result<(), VaultError> {
        self.identities.retain(|i| i.id != identity.id);
        self.identities.push(identity.clone());
        Ok(())
    }

    fn remove_identity(&mut self, id: &str) -> bool {
        let before = self.identities.len();
        self.identities.retain(|i| i.id != id);
        self.identities.len() < before
    }

    fn change_password(&mut self, password: &Secret) -> Result<(), VaultError> {
        *self.owner.password.lock().unwrap() = Some(password.as_str().to_owned());
        Ok(())
    }

    fn set_keychain(&mut self, on: bool) -> Result<(), VaultError> {
        *self.owner.keychain.lock().unwrap() = on;
        Ok(())
    }

    fn save(&mut self) -> Result<(), VaultError> {
        if let Some(error) = self.owner.failing.lock().unwrap().clone() {
            return Err(error);
        }
        self.owner
            .identities
            .lock()
            .unwrap()
            .clone_from(&self.identities);
        Ok(())
    }
}

fn secret(text: &str) -> Secret {
    Secret::new(text.to_owned())
}

fn vault() -> (Arc<Memory>, Arc<Vault>) {
    let memory = Arc::new(Memory::default());
    let vault = Vault::new(Arc::new(Arc::clone(&memory)), DEFAULT_IDLE);
    (memory, vault)
}

fn input(name: &str) -> IdentityInput {
    IdentityInput {
        name: name.into(),
        ..IdentityInput::default()
    }
}

#[tokio::test]
async fn a_locked_vault_gives_nothing_out() {
    let (_, vault) = vault();
    assert_eq!(vault.identities().await, Err(VaultError::Locked));
    assert_eq!(
        vault.unlock(secret("whatever1")).await,
        Err(VaultError::Missing)
    );
    vault.create(secret("correct horse")).await.unwrap();
    let id = vault.save_identity(input("prod")).await.unwrap();
    vault.lock().await;
    assert!(!vault.status().await.unlocked);
    assert_eq!(
        vault.credentials(&id).await.unwrap_err(),
        VaultError::Locked
    );
    assert_eq!(
        vault.unlock(secret("correct horse!")).await,
        Err(VaultError::WrongKey)
    );
    vault.unlock(secret("correct horse")).await.unwrap();
    assert_eq!(vault.identities().await.unwrap().len(), 1);
}

#[tokio::test]
async fn short_master_passwords_are_refused() {
    let (_, vault) = vault();
    assert!(matches!(
        vault.create(secret("short")).await,
        Err(VaultError::Invalid(_))
    ));
    assert!(!vault.status().await.exists);
}

#[tokio::test]
async fn secrets_are_write_only_and_kept_unless_replaced() {
    let (_, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    let id = vault
        .save_identity(IdentityInput {
            user: Some("deploy".into()),
            password: Some(secret("hunter2")),
            private_key: Some(secret("-----BEGIN OPENSSH PRIVATE KEY-----")),
            public_key: Some("ssh-ed25519 AAAA deploy".into()),
            ..input("Deploy")
        })
        .await
        .unwrap();
    let summary = &vault.identities().await.unwrap()[0];
    assert_eq!(
        summary,
        &IdentitySummary {
            id: id.clone(),
            name: "Deploy".into(),
            user: Some("deploy".into()),
            has_password: true,
            has_key: true,
            public_key: Some("ssh-ed25519 AAAA deploy".into()),
        }
    );
    // An edit that leaves secrets out keeps them.
    vault
        .save_identity(IdentityInput {
            id: Some(id.clone()),
            user: Some("ops".into()),
            ..input("Ops")
        })
        .await
        .unwrap();
    let credentials = vault.credentials(&id).await.unwrap();
    assert_eq!(credentials.user.as_deref(), Some("ops"));
    assert_eq!(
        credentials.password.as_deref().map(String::as_str),
        Some("hunter2")
    );
    assert!(credentials.private_key.is_some());
    // An empty secret removes it.
    vault
        .save_identity(IdentityInput {
            id: Some(id.clone()),
            password: Some(secret("")),
            ..input("Ops")
        })
        .await
        .unwrap();
    assert!(vault.credentials(&id).await.unwrap().password.is_none());
    // Debug output shows no secret.
    assert!(!format!("{credentials:?}").contains("hunter2"));
}

#[tokio::test]
async fn identities_are_validated_and_deleted() {
    let (_, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    for bad in [
        input(""),
        input(&"x".repeat(129)),
        input("a\u{1b}b"),
        IdentityInput {
            id: Some("../x".into()),
            ..input("x")
        },
        IdentityInput {
            password: Some(secret(&"p".repeat(MAX_SECRET + 1))),
            ..input("x")
        },
    ] {
        assert!(matches!(
            vault.save_identity(bad).await,
            Err(VaultError::Invalid(_))
        ));
    }
    assert_eq!(
        vault.delete_identity("nope".into()).await,
        Err(VaultError::NoSuchIdentity("nope".into()))
    );
    let id = vault.save_identity(input("a")).await.unwrap();
    vault.delete_identity(id).await.unwrap();
    assert_eq!(vault.identities().await.unwrap(), []);
}

#[tokio::test(start_paused = true)]
async fn it_locks_itself_after_going_unused() {
    let (_, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    vault.set_idle(MIN_IDLE).await.unwrap();
    tokio::time::sleep(MIN_IDLE / 2).await;
    vault.touch().await;
    // Use resets the clock.
    tokio::time::sleep(MIN_IDLE / 2 + Duration::from_secs(5)).await;
    assert!(vault.status().await.unlocked);
    tokio::time::sleep(MIN_IDLE).await;
    assert!(!vault.status().await.unlocked);
    assert!(vault.set_idle(Duration::from_secs(1)).await.is_err());
}

#[tokio::test]
async fn changing_the_password_needs_the_current_one() {
    let (memory, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    assert_eq!(
        vault
            .change_password(secret("not it at all"), secret("battery staple"))
            .await,
        Err(VaultError::WrongKey)
    );
    vault
        .change_password(secret("correct horse"), secret("battery staple"))
        .await
        .unwrap();
    assert_eq!(
        memory.password.lock().unwrap().as_deref(),
        Some("battery staple")
    );
}

#[tokio::test]
async fn the_keychain_unlocks_once_turned_on() {
    let (_, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    assert_eq!(vault.status().await.keychain, KeychainState::Off);
    assert!(matches!(
        vault.set_keychain(true, None).await,
        Err(VaultError::Invalid(_))
    ));
    assert_eq!(
        vault.set_keychain(true, Some(secret("wrong one"))).await,
        Err(VaultError::WrongKey)
    );
    vault
        .set_keychain(true, Some(secret("correct horse")))
        .await
        .unwrap();
    vault.lock().await;
    vault.unlock_with_keychain().await.unwrap();
    assert_eq!(vault.status().await.keychain, KeychainState::On);
}

#[tokio::test]
async fn a_failed_save_locks_so_memory_never_holds_what_the_file_does_not() {
    for error in [VaultError::Conflict, VaultError::Io("disk full".into())] {
        let (memory, vault) = vault();
        vault.create(secret("correct horse")).await.unwrap();
        vault.save_identity(input("kept")).await.unwrap();
        *memory.failing.lock().unwrap() = Some(error.clone());
        assert_eq!(vault.save_identity(input("lost")).await, Err(error));
        assert!(!vault.status().await.unlocked);
        *memory.failing.lock().unwrap() = None;
        vault.unlock(secret("correct horse")).await.unwrap();
        let names: Vec<_> = vault
            .identities()
            .await
            .unwrap()
            .into_iter()
            .map(|i| i.name)
            .collect();
        assert_eq!(names, ["kept"]);
    }
}

#[tokio::test]
async fn a_refused_edit_keeps_the_vault_open() {
    let (_, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    let edit = IdentityInput {
        id: Some("missing".into()),
        ..input("x")
    };
    assert!(matches!(
        vault.save_identity(edit).await,
        Err(VaultError::NoSuchIdentity(_))
    ));
    assert!(vault.status().await.unlocked);
}

#[test]
fn time_asleep_counts_toward_the_idle_time() {
    // After a suspend the wall clock is ahead of the monotonic one.
    let slept = Used {
        monotonic: Instant::now(),
        wall: std::time::SystemTime::now() - Duration::from_hours(8),
    };
    assert!(slept.elapsed() >= Duration::from_hours(8));
    // A wall clock set back does not shorten the idle time.
    let set_back = Used {
        monotonic: Instant::now(),
        wall: std::time::SystemTime::now() + Duration::from_hours(1),
    };
    assert!(set_back.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn an_identity_user_must_be_one_ssh_accepts() {
    let (_, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    for user in ["a b", "x".repeat(300).as_str()] {
        let bad = IdentityInput {
            user: Some(user.into()),
            ..input("bad")
        };
        assert!(matches!(
            vault.save_identity(bad).await,
            Err(VaultError::Invalid(_))
        ));
    }
    let padded = IdentityInput {
        user: Some(" deploy ".into()),
        ..input("ok")
    };
    let id = vault.save_identity(padded).await.unwrap();
    assert_eq!(
        vault.credentials(&id).await.unwrap().user.as_deref(),
        Some("deploy")
    );
}

#[tokio::test]
async fn using_a_vault_past_its_idle_time_locks_it_instead_of_renewing() {
    let (_, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    // As after a suspend: the wall clock says an hour, the tick has not run.
    let asleep = || Used {
        monotonic: Instant::now(),
        wall: std::time::SystemTime::now() - Duration::from_hours(1),
    };
    vault.state.lock().await.open.as_mut().unwrap().used = asleep();
    vault.touch().await;
    assert!(!vault.status().await.unlocked);
    vault.unlock(secret("correct horse")).await.unwrap();
    vault.state.lock().await.open.as_mut().unwrap().used = asleep();
    assert_eq!(vault.identities().await, Err(VaultError::Locked));
}

#[tokio::test]
async fn a_save_refused_as_too_big_keeps_the_vault_open_and_unchanged() {
    let (memory, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    let id = vault.save_identity(input("kept")).await.unwrap();
    *memory.failing.lock().unwrap() =
        Some(VaultError::Invalid("the vault would be over 16 MiB".into()));
    assert!(matches!(
        vault.save_identity(input("new")).await,
        Err(VaultError::Invalid(_))
    ));
    let renamed = IdentityInput {
        id: Some(id.clone()),
        ..input("renamed")
    };
    assert!(vault.save_identity(renamed).await.is_err());
    assert!(vault.status().await.unlocked);
    let names: Vec<_> = vault
        .identities()
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.name)
        .collect();
    assert_eq!(names, ["kept"]);
}

#[tokio::test]
async fn every_change_locks_when_its_save_fails() {
    for op in 0..3 {
        let (memory, vault) = vault();
        vault.create(secret("correct horse")).await.unwrap();
        let id = vault.save_identity(input("kept")).await.unwrap();
        *memory.failing.lock().unwrap() = Some(VaultError::Io("disk full".into()));
        let result = match op {
            0 => vault.delete_identity(id).await,
            1 => vault
                .change_password(secret("correct horse"), secret("battery staple"))
                .await
                .map(drop),
            _ => vault.set_keychain(false, None).await,
        };
        assert!(result.is_err(), "op {op}");
        assert!(!vault.status().await.unlocked, "op {op}");
    }
}

#[tokio::test]
async fn a_keychain_that_refuses_its_key_leaves_the_vault_open() {
    let (memory, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    *memory.failing.lock().unwrap() = Some(VaultError::KeychainUnavailable("locked".into()));
    assert!(matches!(
        vault
            .set_keychain(true, Some(secret("correct horse")))
            .await,
        Err(VaultError::KeychainUnavailable(_))
    ));
    assert!(vault.status().await.unlocked);
}

#[tokio::test]
async fn locking_during_an_unlock_wins() {
    let (_, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    vault.lock().await;
    // An unlock that read the lock count before this lock.
    let locks = vault.state.lock().await.locks;
    let opened = Arc::clone(&vault.backend)
        .unlock(&secret("correct horse"))
        .unwrap();
    vault.lock().await;
    vault.keep(opened, locks).await;
    assert!(!vault.status().await.unlocked);
}

#[tokio::test]
async fn a_password_change_the_keychain_refuses_still_changes_the_password() {
    let (memory, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    *memory.failing.lock().unwrap() = Some(VaultError::KeychainUnavailable("locked".into()));
    assert_eq!(
        vault
            .change_password(secret("correct horse"), secret("battery staple"))
            .await,
        Ok(Some("locked".into()))
    );
    assert!(vault.status().await.unlocked);
}

#[tokio::test]
async fn a_public_key_is_read_from_one_identity() {
    let (_, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    // The public key goes with a private key, as the desktop sends them.
    let with_key = IdentityInput {
        private_key: Some(secret("-----BEGIN OPENSSH PRIVATE KEY-----")),
        public_key: Some("ssh-ed25519 AAAA k".into()),
        ..input("k")
    };
    let id = vault.save_identity(with_key).await.unwrap();
    assert_eq!(
        vault.public_key(&id).await.unwrap().as_deref(),
        Some("ssh-ed25519 AAAA k")
    );
    assert!(matches!(
        vault.public_key("nope").await,
        Err(VaultError::NoSuchIdentity(_))
    ));
}
