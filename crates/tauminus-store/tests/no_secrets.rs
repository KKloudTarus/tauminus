// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The v0.2 exit criterion: secrets never appear in the files Tauminus
//! writes or in its logs. This runs the vault and the history end to end,
//! with every log line captured at TRACE, then scans every file in the
//! config directory and every log line for each secret.

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

use tauminus_app::history::History;
use tauminus_app::ports::Secret;
use tauminus_app::vault::{DEFAULT_IDLE, IdentityInput, Vault};
use tauminus_domain::{HistoryEntry, HistoryQuery, Retention};
use tauminus_store::vault::{Kdf, VaultFile};
use tauminus_store::{FileHistoryBackend, FileVaultBackend, KeyStore};

const MASTER: &str = "master-correct-horse-1";
const NEW_MASTER: &str = "master-battery-staple-2";
const PASSWORD: &str = "identity-hunter2-3";
const PASSPHRASE: &str = "passphrase-swordfish-4";
/// A private key body: any part of it showing would be a leak.
const KEY_BODY: &str = "b3BlbnNzaC1rZXktdjEAAAAABG5vbmUtUFJJVkFURS1LRVktNQ";
const COMMAND: &str = "mysql --password=command-secret-6";
const OUTPUT: &str = "output-secret-7 printed by the server";

/// Log lines, captured.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A keychain in memory, which keeps what it is given, as a real one does.
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

fn secret(text: &str) -> Secret {
    Secret::new(text.into())
}

#[tokio::test(flavor = "multi_thread")]
async fn no_secret_reaches_a_file_or_a_log() {
    let logs = Captured::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    // Global, so the blocking threads the vault and history run on log
    // into it too.
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let mut suffix = [0u8; 8];
    rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut suffix).unwrap();
    let dir = std::env::temp_dir().join(format!("tauminus-no-secrets-{suffix:02x?}"));
    let keys: Arc<dyn KeyStore> = Arc::new(Keys::default());
    let backend =
        FileVaultBackend::new(VaultFile::new(dir.join("vault.json")), keys).with_kdf(Kdf {
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

    // Everything that handles a secret: create, identities, keychain, a
    // wrong password, a password change, history open and locked.
    vault.create(secret(MASTER)).await.unwrap();
    let id = vault
        .save_identity(IdentityInput {
            name: "prod".into(),
            user: Some("deploy".into()),
            password: Some(secret(PASSWORD)),
            private_key: Some(secret(&format!(
                "-----BEGIN OPENSSH PRIVATE KEY-----\n{KEY_BODY}\n-----END OPENSSH PRIVATE KEY-----\n"
            ))),
            passphrase: Some(secret(PASSPHRASE)),
            public_key: Some("ssh-ed25519 AAAA prod".into()),
            ..IdentityInput::default()
        })
        .await
        .unwrap();
    let shown = format!("{:?}", vault.credentials(&id).await.unwrap());
    vault
        .set_keychain(true, Some(secret(MASTER)))
        .await
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let entry = HistoryEntry {
        host: Some("db".into()),
        label: "db".into(),
        at_ms: i64::try_from(now).unwrap(),
        command: COMMAND.into(),
        output: OUTPUT.into(),
        exit: Some(0),
    };
    let all = HistoryQuery {
        limit: 10,
        ..HistoryQuery::default()
    };
    history.search(all.clone()).await.unwrap();
    history.record(entry.clone()).await.unwrap();
    vault.lock().await;
    history.record(entry).await.unwrap();
    assert!(vault.unlock(secret("not the password")).await.is_err());
    vault.unlock_with_keychain().await.unwrap();
    assert_eq!(history.search(all).await.unwrap().len(), 2);
    let keys = vault.history_keys().await.unwrap();
    let hex = |bytes: &[u8]| {
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{b:02x}"));
            out
        })
    };
    let key_texts = [hex(&keys.database[..]), hex(&keys.sealing[..])];
    vault
        .change_password(secret(MASTER), secret(NEW_MASTER))
        .await
        .unwrap();
    vault.lock().await;
    drop(history);

    scan(&dir, &logs, shown, &key_texts);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Fails on any secret in the logs, in `shown`, or in a file in `dir`.
fn scan(dir: &std::path::Path, logs: &Captured, shown: String, keys: &[String]) {
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(!logs.is_empty(), "the run logged something to scan");
    let mut places = vec![
        ("the logs".to_owned(), logs.into_bytes()),
        ("Debug of credentials".to_owned(), shown.into_bytes()),
    ];
    for file in std::fs::read_dir(dir).unwrap() {
        let path = file.unwrap().path();
        places.push((path.display().to_string(), std::fs::read(&path).unwrap()));
    }
    assert!(
        places.len() >= 4,
        "vault, history, and pending files were scanned"
    );
    for (place, bytes) in &places {
        let text = String::from_utf8_lossy(bytes);
        for secret in [
            MASTER,
            NEW_MASTER,
            PASSWORD,
            PASSPHRASE,
            KEY_BODY,
            "command-secret-6",
            "output-secret-7",
        ] {
            assert!(!text.contains(secret), "{secret:?} is in {place}");
        }
        for key in keys {
            assert!(!text.contains(key.as_str()), "a history key is in {place}");
        }
    }
}
