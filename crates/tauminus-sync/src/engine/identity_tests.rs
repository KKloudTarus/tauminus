// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Two devices sharing a vault identity through a folder, each with its own
//! vault file, as the app runs them.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tauminus_app::ports::{KeyDeriver, Secret};
use tauminus_app::sync::SharedIdentities;
use tauminus_app::trust::Trust;
use tauminus_app::vault::{IdentityInput, Vault};
use tauminus_app::{HostBook, SnippetBook, Syncer};
use tauminus_store::vault::{Kdf as VaultKdf, VaultFile};
use tauminus_store::{FileVaultBackend, JsonHostStore, JsonSnippetStore, JsonTrustStore, KeyStore};
use zeroize::Zeroizing;

use super::*;
use crate::seal::Kdf;
use crate::target::Folder;

struct NoKeychain;

impl KeyStore for NoKeychain {
    fn available(&self) -> Result<(), String> {
        Err("none here".into())
    }
    fn get(&self, _: &str) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        Ok(None)
    }
    fn set(&self, _: &str, _: &[u8]) -> Result<(), String> {
        Err("none here".into())
    }
    fn delete(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Every key's public half is `ssh-ed25519 <key>`.
struct Derive;

impl KeyDeriver for Derive {
    fn public_key(&self, private_key: &str, _: Option<&str>) -> Result<String, String> {
        Ok(format!("ssh-ed25519 {}", private_key.len()))
    }
}

struct Device {
    hosts: HostBook,
    snippets: SnippetBook,
    vault: Arc<Vault>,
    trust: Trust,
    remote: Remote,
}

/// Reads no key.
struct NoDerive;

impl KeyDeriver for NoDerive {
    fn public_key(&self, _: &str, _: Option<&str>) -> Result<String, String> {
        Err("unsupported key type".into())
    }
}

impl Device {
    async fn sync(&self) -> Merged {
        self.sync_with(Arc::new(Derive)).await
    }

    async fn sync_with(&self, deriver: Arc<dyn KeyDeriver>) -> Merged {
        let shared = SharedIdentities {
            vault: &self.vault,
            deriver,
        };
        Syncer::default()
            .run(
                &self.hosts,
                &self.snippets,
                Some(&shared),
                &self.trust,
                &self.remote,
            )
            .await
            .unwrap()
    }

    async fn check_identity(&self, id: &str) -> Result<(), tauminus_app::trust::TrustError> {
        self.trust
            .check_identity(
                async |ids| self.vault.identity_prints(Some(ids)).await.unwrap(),
                id,
            )
            .await
    }

    /// Accepts all that waits, as shown.
    async fn accept_all(&self) {
        let prints = self.vault.identity_prints(None).await.unwrap();
        let pending = self
            .trust
            .pending(&self.hosts, Some(&self.snippets), &prints, 100)
            .await
            .unwrap();
        let seen: Vec<_> = pending
            .reviews
            .into_iter()
            .map(|r| (r.subject, r.fingerprint))
            .collect();
        let stale = self
            .trust
            .accept(&self.hosts, Some(&self.snippets), &prints, &seen)
            .await
            .unwrap();
        assert_eq!(stale, []);
    }
}

fn password(identities: &[tauminus_app::ports::SyncedIdentity]) -> Option<String> {
    identities[0]
        .password
        .as_deref()
        .map(|p| p.as_str().to_owned())
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tauminus-ids-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn device(dir: &Path, target: &Arc<Folder>, key: SyncKey) -> Device {
    std::fs::create_dir_all(dir).unwrap();
    let backend =
        FileVaultBackend::new(VaultFile::new(dir.join("vault.json")), Arc::new(NoKeychain))
            .with_kdf(VaultKdf {
                memory_kib: 19 * 1024,
                iterations: 2,
                parallelism: 1,
            });
    let vault = Vault::new(Arc::new(backend), tauminus_app::vault::DEFAULT_IDLE);
    vault
        .create(Secret::new("vault password".into()))
        .await
        .unwrap();
    Device {
        hosts: HostBook::open(Arc::new(JsonHostStore::new(dir.join("hosts.json"))))
            .await
            .unwrap(),
        snippets: SnippetBook::open(Arc::new(JsonSnippetStore::new(dir.join("snippets.json"))))
            .await
            .unwrap(),
        vault,
        trust: Trust::open(Arc::new(JsonTrustStore::new(dir.join("sync-review.json"))))
            .await
            .unwrap(),
        remote: Remote::new(
            Arc::clone(target) as Arc<dyn Target>,
            key,
            dir.join("sync-state.json"),
        ),
    }
}

async fn pair(name: &str) -> (Device, Device, PathBuf) {
    let root = scratch(name);
    let target = Arc::new(Folder::new(root.join("target")));
    let key = create(target.as_ref(), "correct horse", Kdf::LEAST).unwrap();
    let joined = join(target.as_ref(), "correct horse").unwrap();
    let a = device(&root.join("a"), &target, key).await;
    let b = device(&root.join("b"), &target, joined).await;
    (a, b, root)
}

fn identity(name: &str, password: &str, sync: bool) -> IdentityInput {
    IdentityInput {
        name: name.into(),
        user: Some("deploy".into()),
        password: Some(Secret::new(password.into())),
        private_key: Some(Secret::new("PRIVATE-KEY-CANARY-3f1".into())),
        sync: Some(sync),
        ..IdentityInput::default()
    }
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[tokio::test]
async fn a_shared_identity_reaches_the_other_device_and_only_as_ciphertext() {
    let (a, b, root) = pair("shared").await;
    let id = a
        .vault
        .save_identity(identity("deploy", "PASSWORD-CANARY-9c2", true))
        .await
        .unwrap();
    a.vault
        .save_identity(identity("mine", "LOCAL-CANARY-77a", false))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    let on_b = b.vault.identities().await.unwrap();
    assert_eq!(on_b.len(), 1, "only the shared one");
    assert_eq!(on_b[0].id, id);
    assert!(on_b[0].sync);
    assert!(on_b[0].has_key);
    assert_eq!(
        on_b[0].public_key.as_deref(),
        Some("ssh-ed25519 22"),
        "derived on B"
    );
    // The target and both devices' sync state hold no secret in clear.
    for file in files_under(&root.join("target")).into_iter().chain(
        files_under(&root.join("a")).into_iter().filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("sync-")
        }),
    ) {
        let bytes = std::fs::read(&file).unwrap();
        for canary in [
            "PASSWORD-CANARY-9c2",
            "PRIVATE-KEY-CANARY-3f1",
            "LOCAL-CANARY-77a",
        ] {
            assert!(
                !bytes.windows(canary.len()).any(|w| w == canary.as_bytes()),
                "{} holds {canary}",
                file.display()
            );
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_password_both_devices_changed_clashes_without_its_values() {
    let (a, b, root) = pair("clash").await;
    let id = a
        .vault
        .save_identity(identity("deploy", "first-password", true))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    for (device, password) in [(&a, "from-a-password"), (&b, "from-b-password")] {
        let mut edit = identity("deploy", password, true);
        edit.id = Some(id.clone());
        edit.private_key = None;
        device.vault.save_identity(edit).await.unwrap();
    }
    a.sync().await;
    let on_b = b.sync().await;
    let clash = on_b
        .clashes
        .iter()
        .find(|c| c.field == "password")
        .expect("a clash on the password");
    assert_eq!(
        (clash.kept.as_str(), clash.lost.as_str()),
        ("(secret)", "(secret)")
    );
    // Nor are they in the object B wrote for the others.
    let state = b.remote.inner.load().unwrap().0;
    let name = object_name(&state.device, state.seq);
    let payload = b
        .remote
        .inner
        .read_object(&state.device, state.seq, &name)
        .unwrap();
    assert_ne!(payload.clashes.len(), 0);
    let told = serde_json::to_string(&payload.clashes).unwrap();
    assert!(!told.contains("from-"), "{told}");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn an_identity_no_longer_shared_elsewhere_stays_here_unshared() {
    let (a, b, root) = pair("unshare").await;
    let id = a
        .vault
        .save_identity(identity("deploy", "first-password", true))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    a.vault.delete_identity(id.clone()).await.unwrap();
    a.sync().await;
    b.sync().await;
    let on_b = b.vault.identities().await.unwrap();
    assert_eq!(on_b.len(), 1, "B keeps its copy");
    assert!(!on_b[0].sync, "and shares it no more");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn an_identity_another_device_changed_waits_for_review_shared_or_not() {
    let (a, b, root) = pair("review").await;
    let id = a
        .vault
        .save_identity(identity("deploy", "first-password", true))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    assert!(b.check_identity(&id).await.is_err(), "new from A");
    b.accept_all().await;
    b.check_identity(&id).await.unwrap();
    // A swaps the key; then stops sharing, leaving B's copy as it came.
    let mut edit = identity("deploy", "first-password", true);
    edit.id = Some(id.clone());
    edit.private_key = Some(Secret::new("ANOTHER-KEY".into()));
    a.vault.save_identity(edit).await.unwrap();
    a.sync().await;
    b.sync().await;
    assert!(b.check_identity(&id).await.is_err(), "changed by A");
    a.vault.delete_identity(id.clone()).await.unwrap();
    a.sync().await;
    b.sync().await;
    let on_b = b.vault.identities().await.unwrap();
    assert!(!on_b[0].sync);
    assert!(
        b.check_identity(&id).await.is_err(),
        "unshared, it still holds what A put there"
    );
    b.accept_all().await;
    b.check_identity(&id).await.unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn an_identity_this_device_does_not_take_is_never_sent_back_as_its_edit() {
    let (a, b, root) = pair("decline").await;
    let id = a
        .vault
        .save_identity(identity("deploy", "pw-one", true))
        .await
        .unwrap();
    a.sync().await;
    // B cannot read the key: B takes nothing, twice, and A keeps it shared.
    let first = b.sync_with(Arc::new(NoDerive)).await;
    assert!(
        first.notices.iter().any(|n| n.contains("its key")),
        "{:?}",
        first.notices
    );
    let again = b.sync_with(Arc::new(NoDerive)).await;
    assert!(
        again.notices.iter().any(|n| n.contains("its key")),
        "offered again"
    );
    assert_eq!(b.vault.identities().await.unwrap().len(), 0);
    a.sync().await;
    assert!(a.vault.identities().await.unwrap()[0].sync);
    b.sync().await;
    // A changes it, and B cannot read the new key: A's edit stands.
    let mut edit = identity("deploy", "pw-two", true);
    edit.id = Some(id.clone());
    edit.private_key = Some(Secret::new("NEW-KEY".into()));
    a.vault.save_identity(edit).await.unwrap();
    a.sync().await;
    b.sync_with(Arc::new(NoDerive)).await;
    b.sync_with(Arc::new(NoDerive)).await;
    let on_a = a.sync().await;
    assert!(on_a.clashes.is_empty(), "{:?}", on_a.clashes);
    let shared = a.vault.synced_identities().await.unwrap();
    assert_eq!(password(&shared).as_deref(), Some("pw-two"));
    let shared = b.vault.synced_identities().await.unwrap();
    assert_eq!(
        password(&shared).as_deref(),
        Some("pw-one"),
        "B kept its own"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn an_upgrade_from_unsealed_state_keeps_the_device_and_its_edits() {
    let (a, b, root) = pair("upgrade").await;
    let id = a
        .vault
        .save_identity(identity("deploy", "pw-one", true))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    // As the build before sealing wrote them.
    let dir = root.join("a");
    for (file, kind) in [("sync-state.json", "state"), ("sync-state.doc", "doc")] {
        let path = dir.join(file);
        let bytes = std::fs::read(&path).unwrap();
        let plain = a.remote.inner.key.open_local(kind, &bytes).unwrap();
        std::fs::write(&path, &*plain).unwrap();
    }
    let mut edit = identity("deploy", "pw-local-edit", true);
    edit.id = Some(id.clone());
    edit.private_key = None;
    a.vault.save_identity(edit).await.unwrap();
    let on_a = a.sync().await;
    assert!(on_a.clashes.is_empty(), "{:?}", on_a.clashes);
    let shared = a.vault.synced_identities().await.unwrap();
    assert_eq!(password(&shared).as_deref(), Some("pw-local-edit"));
    // Sealed again, and the same device on the target.
    for file in ["sync-state.json", "sync-state.doc"] {
        assert!(crate::seal::is_sealed_local(
            &std::fs::read(dir.join(file)).unwrap()
        ));
    }
    let devices = std::fs::read_dir(root.join("target").join("devices"))
        .unwrap()
        .count();
    assert_eq!(devices, 2);
    b.sync().await;
    let shared = b.vault.synced_identities().await.unwrap();
    assert_eq!(password(&shared).as_deref(), Some("pw-local-edit"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn an_edit_here_of_an_identity_whose_change_was_not_taken_still_goes() {
    let (a, b, root) = pair("edit-declined").await;
    let id = a
        .vault
        .save_identity(identity("deploy", "pw-one", true))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    // A swaps the key, which B cannot read; B renames the user meanwhile.
    let mut edit = identity("deploy", "pw-one", true);
    edit.id = Some(id.clone());
    edit.private_key = Some(Secret::new("NEW-KEY".into()));
    a.vault.save_identity(edit).await.unwrap();
    a.sync().await;
    b.sync_with(Arc::new(NoDerive)).await;
    let mut mine = identity("deploy", "pw-one", true);
    mine.id = Some(id.clone());
    mine.private_key = None;
    mine.user = Some("root-from-b".into());
    b.vault.save_identity(mine).await.unwrap();
    b.sync_with(Arc::new(NoDerive)).await;
    a.sync().await;
    let on_a = a.vault.synced_identities().await.unwrap();
    assert_eq!(on_a[0].user.as_deref(), Some("root-from-b"));
    assert_eq!(
        on_a[0].private_key.as_deref().map(String::as_str),
        Some("NEW-KEY"),
        "and A's key stands"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_change_not_taken_is_offered_again_when_it_came_through_a_third_device() {
    let (a, b, root) = pair("third").await;
    let target = Arc::new(Folder::new(root.join("target")));
    let key = join(target.as_ref(), "correct horse").unwrap();
    let c = device(&root.join("c"), &target, key).await;
    a.vault
        .save_identity(identity("deploy", "pw-one", true))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    let mut own = identity("b-own", "pw-b", true);
    own.private_key = None;
    b.vault.save_identity(own).await.unwrap();
    b.sync().await;
    // A leaves: what it wrote is on the target only in B's object.
    let gone = a.remote.inner.load().unwrap().0.device.clone();
    std::fs::remove_dir_all(root.join("target").join("devices").join(&gone)).unwrap();
    c.sync_with(Arc::new(NoDerive)).await;
    let again = c.sync_with(Arc::new(NoDerive)).await;
    assert!(
        again.notices.iter().any(|n| n.contains("its key")),
        "{:?}",
        again.notices
    );
    c.sync().await;
    assert_eq!(c.vault.identities().await.unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_value_another_device_stamps_with_this_one_s_id_is_not_kept_as_its_own() {
    let (a, b, root) = pair("forged-stamp").await;
    let id = a
        .vault
        .save_identity(identity("deploy", "pw-one", true))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    // A device with the sync key writes B's id into a stamp a year ahead,
    // on a key B cannot read.
    let victim = b.remote.inner.load().unwrap().0.device;
    let (mut hostile, _) = a.remote.inner.load().unwrap();
    hostile.doc = a.remote.inner.load_doc_for_test();
    hostile.device = "ffffffffffffffffffffffffffffffff".into();
    hostile.seq = 1;
    hostile.untold.clear();
    let key = format!("identity:{id}");
    let record = hostile.doc.records.get_mut(&key).unwrap();
    let ahead = now_ms() + 365 * 24 * 3600 * 1000;
    record.fields.insert(
        "key".into(),
        crate::doc::Cell {
            at: crate::stamp::Stamp {
                ms: ahead,
                n: 0,
                device: victim.clone(),
            },
            value: serde_json::json!({"privateKey": "FORGED-KEY"}),
        },
    );
    let name = object_name(&hostile.device, 1);
    let plain = serde_json::to_vec(&PayloadOut::of(&hostile, now_ms())).unwrap();
    let target = Folder::new(root.join("target"));
    target
        .put_new(
            &name,
            &b.remote
                .inner
                .key
                .seal(&name, &pack(&plain).unwrap())
                .unwrap(),
        )
        .unwrap();
    b.sync_with(Arc::new(NoDerive)).await;
    let base = b.remote.inner.load_doc_for_test();
    let kept = &base.records[&key].fields["key"];
    assert_ne!(kept.at.device, victim, "{kept:?}");
    assert!(!kept.value.to_string().contains("FORGED"));
    let _ = std::fs::remove_dir_all(&root);
}

impl Inner {
    fn load_doc_for_test(&self) -> Doc {
        self.load_doc(&self.doc_path(), "doc").unwrap().unwrap()
    }
}

/// How far ahead of now B's clock runs in the test below.
static AHEAD: AtomicU64 = AtomicU64::new(0);

fn later() -> u64 {
    now_ms() + AHEAD.load(Ordering::SeqCst)
}

/// The objects B writes over six hourly syncs while A's identity reaches
/// it through `deriver`.
async fn writes_over_six_hours(deriver: Arc<dyn KeyDeriver>, name: &str) -> u64 {
    let (a, b, root) = pair(name).await;
    a.vault
        .save_identity(identity("deploy", "pw-one", true))
        .await
        .unwrap();
    a.sync().await;
    let target = Arc::new(Folder::new(root.join("target")));
    let b = Device {
        remote: Remote::with_clock(
            Arc::clone(&target) as Arc<dyn Target>,
            join(target.as_ref(), "correct horse").unwrap(),
            root.join("b").join("sync-state.json"),
            later,
        ),
        ..b
    };
    b.sync_with(Arc::clone(&deriver)).await;
    let start = b.remote.inner.load().unwrap().0.seq;
    for _ in 0..6 {
        AHEAD.fetch_add(61 * 60 * 1000, Ordering::SeqCst);
        b.sync_with(Arc::clone(&deriver)).await;
    }
    AHEAD.store(0, Ordering::SeqCst);
    let end = b.remote.inner.load().unwrap().0.seq;
    let _ = std::fs::remove_dir_all(&root);
    end - start
}

#[tokio::test]
async fn a_decline_that_lasts_writes_no_object_for_it() {
    assert_eq!(
        writes_over_six_hours(Arc::new(NoDerive), "lasting").await,
        0
    );
    assert_eq!(writes_over_six_hours(Arc::new(Derive), "taken").await, 0);
}

/// A folder whose objects stop reading while `down`.
struct Flaky {
    folder: Folder,
    down: AtomicBool,
}

impl Target for Flaky {
    fn refresh(&self) -> std::io::Result<()> {
        self.folder.refresh()
    }
    fn list(&self) -> std::io::Result<Vec<String>> {
        self.folder.list()
    }
    fn get(&self, name: &str, limit: u64) -> std::io::Result<Vec<u8>> {
        if name != KEY && self.down.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("connection reset"));
        }
        self.folder.get(name, limit)
    }
    fn put_new(&self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        self.folder.put_new(name, bytes)
    }
    fn remove(&self, name: &str) -> std::io::Result<()> {
        self.folder.remove(name)
    }
    fn flush(&self) -> std::io::Result<()> {
        self.folder.flush()
    }
}

#[tokio::test]
async fn a_read_again_that_fails_stays_owed() {
    let (a, b, root) = pair("flaky").await;
    a.vault
        .save_identity(identity("deploy", "pw-one", true))
        .await
        .unwrap();
    a.sync().await;
    let flaky = Arc::new(Flaky {
        folder: Folder::new(root.join("target")),
        down: AtomicBool::new(false),
    });
    let b = Device {
        remote: Remote::new(
            Arc::clone(&flaky) as Arc<dyn Target>,
            join(&flaky.folder, "correct horse").unwrap(),
            root.join("b").join("sync-state.json"),
        ),
        ..b
    };
    b.sync_with(Arc::new(NoDerive)).await;
    flaky.down.store(true, Ordering::SeqCst);
    b.sync_with(Arc::new(NoDerive)).await;
    flaky.down.store(false, Ordering::SeqCst);
    b.sync().await;
    assert_eq!(b.vault.identities().await.unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_clash_another_device_told_is_shown_once_while_read_again() {
    let (a, b, root) = pair("told").await;
    let x = a
        .vault
        .save_identity(identity("x", "pw-one", true))
        .await
        .unwrap();
    let y = a
        .vault
        .save_identity(identity("y", "pw-one", true))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    let mut on_b = identity("x", "from-b", true);
    on_b.id = Some(x.clone());
    on_b.private_key = None;
    b.vault.save_identity(on_b).await.unwrap();
    b.sync().await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    // A wins x's password, and swaps y's key, which B cannot read.
    let mut on_a = identity("x", "from-a", true);
    on_a.id = Some(x);
    on_a.private_key = None;
    a.vault.save_identity(on_a).await.unwrap();
    let mut key = identity("y", "pw-one", true);
    key.id = Some(y);
    key.private_key = Some(Secret::new("NEW-KEY-Y".into()));
    a.vault.save_identity(key).await.unwrap();
    a.sync().await;
    let mut shown = Vec::new();
    for _ in 0..3 {
        shown.push(b.sync_with(Arc::new(NoDerive)).await.clashes.len());
    }
    assert_eq!(shown[1..], [0, 0], "{shown:?}");
    let _ = std::fs::remove_dir_all(&root);
}
