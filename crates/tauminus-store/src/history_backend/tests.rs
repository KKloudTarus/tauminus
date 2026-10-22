// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;

use tauminus_app::history::{History, Recorded};
use tauminus_app::ports::Secret;
use tauminus_app::vault::{DEFAULT_IDLE, Vault};
use tauminus_domain::{HistoryEntry, HistoryQuery, Retention};

use super::*;
use crate::vault::{Kdf, VaultFile};
use crate::{FileVaultBackend, KeyStore};

struct NoKeychain;

impl KeyStore for NoKeychain {
    fn available(&self) -> Result<(), String> {
        Err("none".into())
    }
    fn get(&self, _: &str) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, String> {
        Ok(None)
    }
    fn set(&self, _: &str, _: &[u8]) -> Result<(), String> {
        Err("none".into())
    }
    fn delete(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
}

fn entry(command: &str, output: &str) -> HistoryEntry {
    HistoryEntry {
        host: Some("web".into()),
        label: "web".into(),
        // Now: anything over a year old is pruned on open.
        at_ms: FileHistoryBackend::new(Path::new("")).now_ms(),
        command: command.into(),
        output: output.into(),
        exit: Some(0),
    }
}

/// The real vault, history database, and pending file, end to end.
#[tokio::test(flavor = "multi_thread")]
async fn commands_recorded_locked_and_open_are_all_found_and_never_plain_on_disk() {
    let mut suffix = [0u8; 8];
    rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut suffix).unwrap();
    let dir = std::env::temp_dir().join(format!("tauminus-hist-e2e-{suffix:02x?}"));
    let vault_backend =
        FileVaultBackend::new(VaultFile::new(dir.join("vault.json")), Arc::new(NoKeychain))
            .with_kdf(Kdf {
                memory_kib: 19 * 1024,
                iterations: 2,
                parallelism: 1,
            });
    let vault = Vault::new(Arc::new(vault_backend), DEFAULT_IDLE);
    let history = History::new(
        Arc::new(FileHistoryBackend::new(&dir)),
        Arc::clone(&vault),
        Retention::default(),
    )
    .await;
    let password = || Secret::new("correct horse".into());
    vault.create(password()).await.unwrap();
    let all = HistoryQuery {
        limit: 10,
        ..HistoryQuery::default()
    };
    history.search(all.clone()).await.unwrap();
    assert_eq!(
        history
            .record(entry("kubectl get pods", "web-1 Running"))
            .await,
        Ok(Recorded::Stored)
    );
    vault.lock().await;
    assert_eq!(
        history
            .record(entry("psql -c 'drop table x'", "DROP TABLE"))
            .await,
        Ok(Recorded::Sealed)
    );
    for file in [
        "history.db",
        "history.db-wal",
        "history.pending",
        "vault.json",
    ] {
        let Ok(bytes) = std::fs::read(dir.join(file)) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        for word in ["kubectl", "Running", "psql", "DROP TABLE", "SQLite format"] {
            assert!(!text.contains(word), "{word} is readable in {file}");
        }
    }
    vault.unlock(password()).await.unwrap();
    let hits = history.search(all).await.unwrap();
    let commands: Vec<_> = hits.iter().map(|h| h.command.as_str()).collect();
    assert_eq!(commands, ["psql -c 'drop table x'", "kubectl get pods"]);
    assert_eq!(
        std::fs::metadata(dir.join("history.pending"))
            .unwrap()
            .len(),
        0
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_history_file_from_another_vault_is_set_aside() {
    use tauminus_app::history::HistoryBackend;
    let mut suffix = [0u8; 8];
    rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut suffix).unwrap();
    let dir = std::env::temp_dir().join(format!("tauminus-hist-aside-{suffix:02x?}"));
    let backend = FileHistoryBackend::new(&dir);
    let keys = |byte| HistoryKeys {
        database: zeroize::Zeroizing::new([byte; 32]),
        sealing: zeroize::Zeroizing::new([byte; 32]),
        public: [0; 32],
    };
    let old = backend.open(&keys(1)).unwrap();
    old.insert(&[entry("old", "")], &|| true).unwrap();
    drop(old);
    // A new vault, so a new key: recording goes on in a fresh file.
    let new = backend.open(&keys(2)).unwrap();
    new.insert(&[entry("new", "")], &|| true).unwrap();
    let set_aside = std::fs::read_dir(&dir)
        .unwrap()
        .filter(|f| {
            f.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".unreadable")
        })
        .count();
    assert_eq!(set_aside, 1);
    let _ = std::fs::remove_dir_all(&dir);
}
