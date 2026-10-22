// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! A password change moves the history to new keys: an old copy of the
//! vault, opened with the old password, gives keys that open nothing the
//! history holds afterwards.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tauminus_app::history::History;
use tauminus_app::ports::Secret;
use tauminus_app::vault::{DEFAULT_IDLE, Vault};
use tauminus_domain::{HistoryEntry, HistoryQuery, Retention};
use tauminus_store::history::{HistoryDb, HistoryError};
use tauminus_store::vault::{Kdf, SecretKey, VaultFile};
use tauminus_store::{FileHistoryBackend, FileVaultBackend, KeyStore};

#[derive(Default)]
struct Keys(Mutex<HashMap<String, Vec<u8>>>);

impl KeyStore for Keys {
    fn available(&self) -> Result<(), String> {
        Ok(())
    }

    fn get(&self, name: &str) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, String> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .map(zeroize::Zeroizing::new))
    }

    fn set(&self, name: &str, secret: &[u8]) -> Result<(), String> {
        self.0.lock().unwrap().insert(name.into(), secret.to_vec());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        self.0.lock().unwrap().remove(name);
        Ok(())
    }
}

fn entry(command: &str) -> HistoryEntry {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    HistoryEntry {
        host: None,
        label: "local".into(),
        at_ms: i64::try_from(now).unwrap(),
        command: command.into(),
        output: String::new(),
        exit: Some(0),
    }
}

fn all() -> HistoryQuery {
    HistoryQuery {
        limit: 10,
        ..HistoryQuery::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_old_vault_and_password_open_no_history_after_a_change() {
    let mut suffix = [0u8; 8];
    rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut suffix).unwrap();
    let dir = std::env::temp_dir().join(format!("tauminus-rotate-{suffix:02x?}"));
    let file = VaultFile::new(dir.join("vault.json"));
    let backend = FileVaultBackend::new(file.clone(), Arc::new(Keys::default())).with_kdf(Kdf {
        memory_kib: 19 * 1024,
        iterations: 2,
        parallelism: 1,
    });
    let vault = Vault::new(Arc::new(backend), DEFAULT_IDLE);
    let history = History::new(
        Arc::new(FileHistoryBackend::new(&dir)),
        Arc::clone(&vault),
        Retention::default(),
    )
    .await;
    vault
        .create(Secret::new("old password 1".into()))
        .await
        .unwrap();
    history.record(entry("before the change")).await.unwrap();
    // The attacker's copy, from a backup taken before the change.
    std::fs::copy(dir.join("vault.json"), dir.join("old.json")).unwrap();

    vault
        .change_password(
            Secret::new("old password 1".into()),
            Secret::new("new password 2".into()),
        )
        .await
        .unwrap();
    history.record(entry("after the change")).await.unwrap();
    let found: Vec<String> = history
        .search(all())
        .await
        .unwrap()
        .into_iter()
        .map(|hit| hit.command)
        .collect();
    assert_eq!(found, ["after the change", "before the change"]);

    // The old copy and the old password still open the old copy...
    let old = VaultFile::new(dir.join("old.json"))
        .open_with_password(b"old password 1")
        .unwrap();
    let old_key = SecretKey::from_bytes(old.get("history-database").unwrap()).unwrap();
    // ...but its history key opens nothing now.
    assert!(matches!(
        HistoryDb::open(&dir.join("history.db"), &old_key),
        Err(HistoryError::WrongKey)
    ));
    // The history has moved, so the vault keeps no old keys.
    let current = file.open_with_password(b"new password 2").unwrap();
    assert!(current.get("history-database-previous").is_none());
    assert!(current.get("history-sealing-previous").is_none());
    drop(history);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn two_changes_before_the_history_opens_keep_everything() {
    use tauminus_app::history::HistoryBackend;
    let mut suffix = [0u8; 8];
    rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut suffix).unwrap();
    let dir = std::env::temp_dir().join(format!("tauminus-rotate2-{suffix:02x?}"));
    let file = VaultFile::new(dir.join("vault.json"));
    let backend = FileVaultBackend::new(file.clone(), Arc::new(Keys::default())).with_kdf(Kdf {
        memory_kib: 19 * 1024,
        iterations: 2,
        parallelism: 1,
    });
    let vault = Vault::new(Arc::new(backend), DEFAULT_IDLE);
    vault
        .create(Secret::new("password one 1".into()))
        .await
        .unwrap();
    let history = History::new(
        Arc::new(FileHistoryBackend::new(&dir)),
        Arc::clone(&vault),
        Retention::default(),
    )
    .await;
    history.record(entry("under the first key")).await.unwrap();
    // The history is closed while the password changes twice.
    drop(history);
    vault
        .change_password(
            Secret::new("password one 1".into()),
            Secret::new("password two 2".into()),
        )
        .await
        .unwrap();
    // Another window seals a command to the key of the moment.
    let public = vault.history_public().await.unwrap();
    assert!(
        FileHistoryBackend::new(&dir)
            .seal(&public, &entry("sealed between the changes"))
            .unwrap()
    );
    vault
        .change_password(
            Secret::new("password two 2".into()),
            Secret::new("password three 3".into()),
        )
        .await
        .unwrap();

    let history = History::new(
        Arc::new(FileHistoryBackend::new(&dir)),
        Arc::clone(&vault),
        Retention::default(),
    )
    .await;
    let mut found: Vec<String> = history
        .search(all())
        .await
        .unwrap()
        .into_iter()
        .map(|hit| hit.command)
        .collect();
    found.sort();
    assert_eq!(found, ["sealed between the changes", "under the first key"]);
    let unreadable = std::fs::read_dir(&dir)
        .unwrap()
        .filter(|f| {
            f.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("unreadable")
        })
        .count();
    assert_eq!(unreadable, 0, "nothing was set aside");
    let current = file.open_with_password(b"password three 3").unwrap();
    assert!(current.get("history-database-previous").is_none());
    drop(history);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_window_with_the_older_vault_leaves_the_moved_history_alone() {
    let mut suffix = [0u8; 8];
    rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut suffix).unwrap();
    let dir = std::env::temp_dir().join(format!("tauminus-rotate-win-{suffix:02x?}"));
    let window = || {
        let backend = FileVaultBackend::new(
            VaultFile::new(dir.join("vault.json")),
            Arc::new(Keys::default()),
        )
        .with_kdf(Kdf {
            memory_kib: 19 * 1024,
            iterations: 2,
            parallelism: 1,
        });
        Vault::new(Arc::new(backend), DEFAULT_IDLE)
    };
    let a = window();
    a.create(Secret::new("password one 1".into()))
        .await
        .unwrap();
    let history_a = History::new(
        Arc::new(FileHistoryBackend::new(&dir)),
        Arc::clone(&a),
        Retention::default(),
    )
    .await;
    history_a.record(entry("from window a")).await.unwrap();
    // Window B unlocks the same vault, and has not used the history yet.
    let b = window();
    b.unlock(Secret::new("password one 1".into()))
        .await
        .unwrap();
    let history_b = History::new(
        Arc::new(FileHistoryBackend::new(&dir)),
        Arc::clone(&b),
        Retention::default(),
    )
    .await;
    a.change_password(
        Secret::new("password one 1".into()),
        Secret::new("password two 2".into()),
    )
    .await
    .unwrap();
    history_a.record(entry("after the change")).await.unwrap();
    // B's copy holds the old keys: it records, sealed, and moves nothing.
    assert!(history_b.record(entry("from window b")).await.is_ok());
    let aside = std::fs::read_dir(&dir)
        .unwrap()
        .filter(|f| {
            f.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("unreadable")
        })
        .count();
    assert_eq!(aside, 0, "the history was set aside");
    let found: Vec<String> = history_a
        .search(all())
        .await
        .unwrap()
        .into_iter()
        .map(|hit| hit.command)
        .collect();
    assert_eq!(found, ["after the change", "from window a"]);
    drop((history_a, history_b));
    let _ = std::fs::remove_dir_all(&dir);
}
