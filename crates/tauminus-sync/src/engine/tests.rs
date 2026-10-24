// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Two devices sharing a folder, each with its own saved files: v0.5's
//! exit criteria, end to end through the real stores and sealing.

use std::path::PathBuf;
use std::sync::Arc;

use tauminus_app::ports::SyncPart;
use tauminus_app::{HostBook, SnippetBook, Syncer};
use tauminus_domain::{Host, HostId, HostName, Label};
use tauminus_store::{JsonHostStore, JsonSnippetStore};

use super::*;
use crate::seal::Kdf;
use crate::target::Folder;

struct Device {
    hosts: HostBook,
    snippets: SnippetBook,
    remote: Remote,
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tauminus-sync-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn device(dir: &Path, target: &Arc<Folder>, key: SyncKey) -> Device {
    std::fs::create_dir_all(dir).unwrap();
    Device {
        hosts: HostBook::open(Arc::new(JsonHostStore::new(dir.join("hosts.json"))))
            .await
            .unwrap(),
        snippets: SnippetBook::open(Arc::new(JsonSnippetStore::new(dir.join("snippets.json"))))
            .await
            .unwrap(),
        remote: Remote::new(
            Arc::clone(target) as Arc<dyn Target>,
            key,
            dir.join("sync-state.json"),
        ),
    }
}

impl Device {
    async fn sync(&self) -> Merged {
        Syncer::default()
            .run(&self.hosts, &self.snippets, &self.remote)
            .await
            .unwrap()
    }

    async fn host(&self, id: &str) -> Option<Host> {
        self.hosts
            .snapshot()
            .await
            .host(&HostId::parse(id).unwrap())
            .cloned()
    }
}

/// Two devices on one folder, the second joined with the passphrase.
async fn pair(name: &str) -> (Device, Device, PathBuf) {
    let root = scratch(name);
    let target = Arc::new(Folder::new(root.join("target")));
    let key = create(target.as_ref(), "correct horse", Kdf::LEAST).unwrap();
    let joined = join(target.as_ref(), "correct horse").unwrap();
    assert_eq!(joined.bytes(), key.bytes());
    let a = device(&root.join("a"), &target, key).await;
    let b = device(&root.join("b"), &target, joined).await;
    (a, b, root)
}

fn new_host(id: &str, label: &str, address: &str) -> Host {
    Host::new(
        HostId::parse(id).unwrap(),
        Label::parse(label).unwrap(),
        HostName::parse(address).unwrap(),
    )
}

#[tokio::test]
async fn two_devices_edit_one_host_offline_and_sync_without_losing_either() {
    let (a, b, root) = pair("offline").await;
    a.hosts
        .edit(|i| i.put_host(new_host("h1", "db", "db.internal")))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    assert_eq!(b.host("h1").await.unwrap().label.as_str(), "db");

    // Offline: A renames the host, B sets its port and a tag.
    a.hosts
        .edit(|i| {
            let mut host = i.host(&HostId::parse("h1").unwrap()).unwrap().clone();
            host.label = Label::parse("db-primary").unwrap();
            i.put_host(host)
        })
        .await
        .unwrap();
    b.hosts
        .edit(|i| {
            let mut host = i.host(&HostId::parse("h1").unwrap()).unwrap().clone();
            host.settings.port = Some(2222);
            host.set_tags(vec![tauminus_domain::Tag::parse("prod").unwrap()])?;
            i.put_host(host)
        })
        .await
        .unwrap();
    a.sync().await;
    let on_b = b.sync().await;
    assert!(on_b.clashes.is_empty(), "{:?}", on_b.clashes);
    a.sync().await;
    for device in [&a, &b] {
        let host = device.host("h1").await.unwrap();
        assert_eq!(host.label.as_str(), "db-primary");
        assert_eq!(host.settings.port, Some(2222));
        assert_eq!(host.tags().len(), 1);
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn the_target_holds_only_ciphertext() {
    let (a, b, root) = pair("cipher").await;
    let canary = "canary-7f3a.internal";
    a.hosts
        .edit(|i| i.put_host(new_host("h1", "canary-label-91c", canary)))
        .await
        .unwrap();
    a.snippets
        .edit(|l| {
            l.put(tauminus_domain::Snippet::new(
                tauminus_domain::SnippetId::parse("s1").unwrap(),
                Label::parse("deploy").unwrap(),
                None,
                "echo canary-body-55e".into(),
            )?)
        })
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    assert!(b.host("h1").await.is_some());
    let mut files = 0;
    let mut stack = vec![root.join("target")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            files += 1;
            let bytes = std::fs::read(&path).unwrap();
            assert!(
                bytes.starts_with(b"TMS1") || bytes.starts_with(b"TMK1"),
                "{path:?} is not sealed"
            );
            for secret in [
                canary,
                "canary-label-91c",
                "canary-body-55e",
                "deploy",
                "h1",
            ] {
                assert!(
                    !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
                    "{secret} in {path:?}"
                );
                assert!(
                    !path.to_string_lossy().contains(secret),
                    "{secret} in a name"
                );
            }
        }
    }
    // The key file, and one object per device: older ones are gone.
    assert_eq!(files, 3);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn the_same_field_changed_on_both_devices_keeps_the_later_and_names_the_other() {
    let (a, b, root) = pair("clash").await;
    a.hosts
        .edit(|i| i.put_host(new_host("h1", "db", "db.internal")))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    for (device, label) in [(&a, "from-a"), (&b, "from-b")] {
        device
            .hosts
            .edit(|i| {
                let mut host = i.host(&HostId::parse("h1").unwrap()).unwrap().clone();
                host.label = Label::parse(label).unwrap();
                i.put_host(host)
            })
            .await
            .unwrap();
    }
    a.sync().await;
    let on_b = b.sync().await;
    assert_eq!(on_b.clashes.len(), 1, "{:?}", on_b.clashes);
    let clash = &on_b.clashes[0];
    assert_eq!(clash.field, "label");
    assert_eq!(
        [clash.kept.as_str(), clash.lost.as_str()]
            .iter()
            .copied()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["from-a", "from-b"])
    );
    a.sync().await;
    assert_eq!(
        a.host("h1").await.unwrap().label,
        b.host("h1").await.unwrap().label,
        "both devices settle on one"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_delete_wins_over_a_later_edit_on_another_device() {
    let (a, b, root) = pair("delete").await;
    a.hosts
        .edit(|i| i.put_host(new_host("h1", "db", "db.internal")))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    a.hosts
        .edit(|i| i.remove_host(&HostId::parse("h1").unwrap()).map(|_| ()))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    b.hosts
        .edit(|i| {
            let mut host = i.host(&HostId::parse("h1").unwrap()).unwrap().clone();
            host.settings.port = Some(2200);
            i.put_host(host)
        })
        .await
        .unwrap();
    a.sync().await;
    let on_b = b.sync().await;
    assert_eq!(on_b.clashes.len(), 1, "{:?}", on_b.clashes);
    a.sync().await;
    assert!(a.host("h1").await.is_none());
    assert!(b.host("h1").await.is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_wrong_passphrase_and_a_newer_device_stop_the_sync() {
    let (a, b, root) = pair("refuse").await;
    let target = Folder::new(root.join("target"));
    assert!(matches!(join(&target, "wrong"), Err(SyncError::Sealed(_))));
    a.sync().await;
    // A device on a newer format writes its object.
    let key = join(&target, "correct horse").unwrap();
    let device = "ffffffffffffffffffffffffffffffff";
    let name = object_name(device, 1);
    let payload = serde_json::json!({
        "schema": SCHEMA + 1, "hostsVersion": 99, "snippetsVersion": 1,
        "device": device, "seq": 1, "doc": {"devices": [], "records": {}}
    });
    target
        .put_new(
            &name,
            &key.seal(&name, &pack(payload.to_string().as_bytes()).unwrap())
                .unwrap(),
        )
        .unwrap();
    let result = Syncer::default()
        .run(&b.hosts, &b.snippets, &b.remote)
        .await;
    assert!(
        matches!(
            result,
            Err(tauminus_app::SyncRunError::Sync(SyncError::Newer(_)))
        ),
        "{result:?}"
    );
    // A forged object, sealed with another key, is not read, and the sync
    // goes on with what the other devices wrote.
    target.remove(&name).unwrap();
    let forged = SyncKey::random().unwrap().seal(&name, b"{}").unwrap();
    target.put_new(&name, &forged).unwrap();
    let merged = b.sync().await;
    assert_eq!(merged.notices.len(), 1, "{:?}", merged.notices);
    assert!(
        merged.notices[0].contains("were not read"),
        "{:?}",
        merged.notices
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn an_older_object_offered_again_is_not_read() {
    let (a, b, root) = pair("replay").await;
    let target = Folder::new(root.join("target"));
    a.hosts
        .edit(|i| i.put_host(new_host("h1", "db", "db.internal")))
        .await
        .unwrap();
    a.sync().await;
    let old_name = target
        .list()
        .unwrap()
        .into_iter()
        .find(|n| n != KEY)
        .unwrap();
    let old = target.get(&old_name, MAX_OBJECT).unwrap();
    a.hosts
        .edit(|i| i.remove_host(&HostId::parse("h1").unwrap()).map(|_| ()))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    assert!(b.host("h1").await.is_none());
    // The newer object withheld, the old one put back: B keeps what it has.
    for name in target.list().unwrap().into_iter().filter(|n| n != KEY) {
        target.remove(&name).unwrap();
    }
    target.put_new(&old_name, &old).unwrap();
    b.sync().await;
    assert!(
        b.host("h1").await.is_none(),
        "the replay brought nothing back"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_sync_that_changed_nothing_writes_nothing() {
    let (a, b, root) = pair("idle").await;
    a.hosts
        .edit(|i| i.put_host(new_host("h1", "db", "db.internal")))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    let target = Folder::new(root.join("target"));
    let before = target.list().unwrap();
    a.sync().await;
    b.sync().await;
    assert_eq!(target.list().unwrap(), before, "no new objects");
    // An edit is published again.
    a.hosts
        .edit(|i| i.put_host(new_host("h2", "web", "web.internal")))
        .await
        .unwrap();
    a.sync().await;
    assert_ne!(target.list().unwrap(), before);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_damaged_newest_object_falls_back_to_the_one_before() {
    let (a, b, root) = pair("fallback").await;
    let target = Folder::new(root.join("target"));
    a.hosts
        .edit(|i| i.put_host(new_host("h1", "db", "db.internal")))
        .await
        .unwrap();
    a.sync().await;
    let good = target
        .list()
        .unwrap()
        .into_iter()
        .find(|n| n != KEY)
        .unwrap();
    let (device, seq) = parse_object(&good).unwrap();
    // A newer object for A, damaged: B reads A's good one instead.
    target
        .put_new(&object_name(device, seq + 1), b"TMS1 not sealed")
        .unwrap();
    let merged = b.sync().await;
    assert!(b.host("h1").await.is_some());
    assert_eq!(merged.notices.len(), 1, "{:?}", merged.notices);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_short_passphrase_is_refused() {
    let root = scratch("short");
    let target = Folder::new(root.join("target"));
    assert!(matches!(
        create(&target, "short", Kdf::LEAST),
        Err(SyncError::Sealed(_))
    ));
    let _ = std::fs::remove_dir_all(&root);
}

async fn rename(device: &Device, id: &str, label: &str) {
    device
        .hosts
        .edit(|i| {
            let mut host = i.host(&HostId::parse(id).unwrap()).unwrap().clone();
            host.label = Label::parse(label).unwrap();
            i.put_host(host)
        })
        .await
        .unwrap();
}

async fn label(device: &Device, id: &str) -> Option<String> {
    device.host(id).await.map(|h| h.label.as_str().to_owned())
}

/// Two devices that both hold host `h1`, labelled `db`.
async fn shared(name: &str) -> (Device, Device, PathBuf) {
    let (a, b, root) = pair(name).await;
    a.hosts
        .edit(|i| i.put_host(new_host("h1", "db", "db.internal")))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    (a, b, root)
}

#[tokio::test]
async fn a_sync_cut_short_after_writing_is_redone_whole() {
    let (a, b, root) = shared("redo").await;
    let state = root.join("a").join("sync-state.json");
    let saved = std::fs::read(&state).unwrap();
    rename(&a, "h1", "db-2").await;
    a.sync().await;
    // As if it stopped before saving its state: the object is out, the
    // state is the one before.
    std::fs::write(&state, &saved).unwrap();
    rename(&a, "h1", "db-3").await;
    a.sync().await;
    b.sync().await;
    assert_eq!(label(&b, "h1").await.as_deref(), Some("db-3"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_sync_whose_publish_failed_does_not_take_others_values_for_its_own() {
    let (a, b, root) = shared("down").await;
    rename(&b, "h1", "v1").await;
    b.sync().await;
    // A has an edit of its own to publish, takes v1 and saves both, but the
    // target is gone before it publishes.
    a.hosts
        .edit(|i| i.put_host(new_host("h2", "web", "web.internal")))
        .await
        .unwrap();
    let target = root.join("target");
    let aside = root.join("target-away");
    let local = SyncData {
        hosts: a.hosts.snapshot().await,
        snippets: a.snippets.snapshot().await,
    };
    let merged = a.remote.merge(local.clone()).await.unwrap();
    assert!(
        a.hosts
            .replace_if(&local.hosts, merged.data.hosts.clone())
            .await
            .unwrap()
    );
    a.remote.adopt(SyncPart::Hosts).await.unwrap();
    a.remote.adopt(SyncPart::Snippets).await.unwrap();
    // A file where the folder was: nothing can be written there.
    std::fs::rename(&target, &aside).unwrap();
    std::fs::write(&target, b"").unwrap();
    assert!(a.remote.publish().await.is_err());
    std::fs::remove_file(&target).unwrap();
    std::fs::rename(&aside, &target).unwrap();
    // B renames again; A's next sync must not undo it.
    rename(&b, "h1", "v2").await;
    b.sync().await;
    a.sync().await;
    assert_eq!(label(&a, "h1").await.as_deref(), Some("v2"));
    b.sync().await;
    assert_eq!(label(&b, "h1").await.as_deref(), Some("v2"));
    assert!(b.host("h2").await.is_some(), "A's edit got out after all");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn the_device_whose_value_lost_is_told_on_its_next_sync() {
    let (a, b, root) = shared("told").await;
    rename(&b, "h1", "from-b").await;
    rename(&a, "h1", "from-a").await;
    a.sync().await;
    let on_b = b.sync().await;
    assert_eq!(on_b.clashes.len(), 1, "{:?}", on_b.clashes);
    // The device that synced last wins; A learns its value lost.
    assert_eq!(label(&b, "h1").await.as_deref(), Some("from-b"));
    let on_a = a.sync().await;
    assert_eq!(on_a.clashes.len(), 1, "{:?}", on_a.clashes);
    assert_eq!(on_a.clashes[0].lost, "from-a");
    assert_eq!(on_a.clashes[0].kept, "from-b");
    // Told once.
    assert_eq!(a.sync().await.clashes.len(), 0);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_delete_against_an_edit_is_a_clash_either_way() {
    let (a, b, root) = shared("delete-edit").await;
    rename(&b, "h1", "edited").await;
    b.sync().await;
    a.hosts
        .edit(|i| i.remove_host(&HostId::parse("h1").unwrap()).map(|_| ()))
        .await
        .unwrap();
    let on_a = a.sync().await;
    assert_eq!(on_a.clashes.len(), 1, "{:?}", on_a.clashes);
    assert_eq!(on_a.clashes[0].field, crate::doc::DELETED);
    let on_b = b.sync().await;
    assert!(
        on_b.clashes.iter().any(|c| c.field == crate::doc::DELETED),
        "B is told its edit lost: {:?}",
        on_b.clashes
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_device_that_lost_its_state_does_not_undo_newer_edits() {
    let (a, b, root) = shared("reset").await;
    b.hosts
        .edit(|i| i.put_host(new_host("h2", "web", "web.internal")))
        .await
        .unwrap();
    b.sync().await;
    a.sync().await;
    rename(&b, "h1", "renamed").await;
    b.hosts
        .edit(|i| i.remove_host(&HostId::parse("h2").unwrap()).map(|_| ()))
        .await
        .unwrap();
    b.sync().await;
    // A loses its sync state, as on a reinstall, with its old hosts.json.
    std::fs::remove_file(root.join("a").join("sync-state.json")).unwrap();
    let on_a = Syncer::default()
        .run(&a.hosts, &a.snippets, &a.remote)
        .await
        .unwrap();
    assert!(!on_a.clashes.is_empty(), "A is told its old values lost");
    b.sync().await;
    for device in [&a, &b] {
        assert_eq!(label(device, "h1").await.as_deref(), Some("renamed"));
    }
    assert!(b.host("h2").await.is_none(), "the delete stands");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn tags_two_devices_add_past_the_limit_are_cut_alike_and_said() {
    let (a, b, root) = shared("tags").await;
    for (device, prefix) in [(&a, "a"), (&b, "b")] {
        device
            .hosts
            .edit(|i| {
                let mut host = i.host(&HostId::parse("h1").unwrap()).unwrap().clone();
                let tags = (0..20)
                    .map(|n| tauminus_domain::Tag::parse(&format!("{prefix}{n:02}")).unwrap())
                    .collect();
                host.set_tags(tags)?;
                i.put_host(host)
            })
            .await
            .unwrap();
    }
    a.sync().await;
    let on_b = b.sync().await;
    assert_eq!(on_b.notices.len(), 1, "{:?}", on_b.notices);
    a.sync().await;
    let tags = |h: Host| h.tags().to_vec();
    assert_eq!(tags(a.host("h1").await.unwrap()).len(), 32);
    assert_eq!(
        tags(a.host("h1").await.unwrap()),
        tags(b.host("h1").await.unwrap())
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn the_same_value_on_both_devices_is_no_clash() {
    let (a, b, root) = shared("same").await;
    rename(&a, "h1", "same").await;
    rename(&b, "h1", "same").await;
    a.sync().await;
    assert_eq!(b.sync().await.clashes, []);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn an_object_naming_another_is_not_read() {
    let (a, b, root) = shared("misnamed").await;
    let target = Folder::new(root.join("target"));
    let key = join(&target, "correct horse").unwrap();
    // Sealed under its own name, but saying it is device ffff…'s seq 9.
    let device = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    let name = object_name(device, 1);
    let payload = serde_json::json!({
        "schema": SCHEMA, "hostsVersion": 1, "snippetsVersion": 1,
        "device": "ffffffffffffffffffffffffffffffff", "seq": 9, "doc": {"devices": [], "records": {}}
    });
    target
        .put_new(
            &name,
            &key.seal(&name, &pack(payload.to_string().as_bytes()).unwrap())
                .unwrap(),
        )
        .unwrap();
    let merged = b.sync().await;
    assert!(merged.notices.iter().any(|n| n.contains("were not read")));
    assert!(a.host("h1").await.is_some());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_device_keeps_one_object_on_the_target() {
    let (a, _b, root) = shared("one").await;
    rename(&a, "h1", "one").await;
    a.sync().await;
    rename(&a, "h1", "two").await;
    a.sync().await;
    let target = Folder::new(root.join("target"));
    let device = std::fs::read_dir(root.join("target").join("devices"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    for d in device {
        let own = target
            .list()
            .unwrap()
            .into_iter()
            .filter(|n| n.contains(&d))
            .count();
        assert_eq!(own, 1, "{d}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A folder that counts the objects read from it.
struct Counting {
    folder: Folder,
    gets: std::sync::atomic::AtomicUsize,
}

impl Target for Counting {
    fn refresh(&self) -> std::io::Result<()> {
        self.folder.refresh()
    }
    fn list(&self) -> std::io::Result<Vec<String>> {
        self.folder.list()
    }
    fn get(&self, name: &str, limit: u64) -> std::io::Result<Vec<u8>> {
        self.gets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
async fn a_sync_with_nothing_new_anywhere_reads_no_object() {
    let (a, _b, root) = shared("nothing-new").await;
    let counting = Arc::new(Counting {
        folder: Folder::new(root.join("target")),
        gets: std::sync::atomic::AtomicUsize::new(0),
    });
    let c = device(
        &root.join("c"),
        &Arc::new(Folder::new(root.join("target"))),
        join(&counting.folder, "correct horse").unwrap(),
    )
    .await;
    let c = Device {
        remote: Remote::new(
            Arc::clone(&counting) as Arc<dyn Target>,
            join(&counting.folder, "correct horse").unwrap(),
            root.join("c").join("sync-state.json"),
        ),
        ..c
    };
    c.sync().await;
    assert!(c.host("h1").await.is_some());
    let first = counting.gets.load(std::sync::atomic::Ordering::SeqCst);
    assert!(first > 0);
    c.sync().await;
    c.sync().await;
    assert_eq!(
        counting.gets.load(std::sync::atomic::Ordering::SeqCst),
        first,
        "nothing new: nothing read"
    );
    // A new object from A is read, once.
    rename(&a, "h1", "again").await;
    a.sync().await;
    c.sync().await;
    assert_eq!(label(&c, "h1").await.as_deref(), Some("again"));
    assert_eq!(
        counting.gets.load(std::sync::atomic::Ordering::SeqCst),
        first + 1
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Every object on the target, opened, as text.
fn opened(root: &Path) -> Vec<String> {
    let target = Folder::new(root.join("target"));
    let key = join(&target, "correct horse").unwrap();
    target
        .list()
        .unwrap()
        .into_iter()
        .filter(|n| n != KEY)
        .map(|n| {
            let sealed = target.get(&n, MAX_OBJECT).unwrap();
            let packed = key.open(&n, &sealed).unwrap();
            String::from_utf8(unpack(&packed, MAX_PLAIN).unwrap().to_vec()).unwrap()
        })
        .collect()
}

/// `device` again, as after a restart: what was in memory is gone.
fn restarted(device: Device, root: &Path, dir: &str) -> Device {
    let target = Arc::new(Folder::new(root.join("target")));
    let key = join(target.as_ref(), "correct horse").unwrap();
    Device {
        remote: Remote::new(target, key, root.join(dir).join("sync-state.json")),
        ..device
    }
}

fn snippet(id: &str, body: &str) -> tauminus_domain::Snippet {
    tauminus_domain::Snippet::new(
        tauminus_domain::SnippetId::parse(id).unwrap(),
        Label::parse("deploy").unwrap(),
        None,
        body.into(),
    )
    .unwrap()
}

async fn body(device: &Device, id: &str) -> String {
    device
        .snippets
        .snapshot()
        .await
        .get(&tauminus_domain::SnippetId::parse(id).unwrap())
        .unwrap()
        .body()
        .to_owned()
}

#[tokio::test]
async fn what_one_device_deletes_leaves_every_object() {
    let (a, b, root) = pair("leaves").await;
    a.hosts
        .edit(|i| i.put_host(new_host("h1", "db", "canary-deleted.internal")))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    b.hosts
        .edit(|i| i.remove_host(&HostId::parse("h1").unwrap()).map(|_| ()))
        .await
        .unwrap();
    b.sync().await;
    a.sync().await;
    // A wrote nothing of its own: it writes the base the others changed a
    // little later, in one object for however many edits came meanwhile.
    let target = Arc::new(Folder::new(root.join("target")));
    let a = Device {
        remote: Remote::with_clock(
            target.clone(),
            join(target.as_ref(), "correct horse").unwrap(),
            root.join("a").join("sync-state.json"),
            sixty_one_minutes_on,
        ),
        ..a
    };
    a.sync().await;
    let objects = opened(&root);
    assert_eq!(objects.len(), 2);
    for text in objects {
        assert!(!text.contains("canary-deleted"), "{text}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_restart_between_saving_the_lists_and_the_base_keeps_the_others_values_theirs() {
    let (a, b, root) = shared("restart").await;
    rename(&b, "h1", "v1").await;
    b.sync().await;
    // A saves v1 to its lists, then stops before its base is saved.
    let local = SyncData {
        hosts: a.hosts.snapshot().await,
        snippets: a.snippets.snapshot().await,
    };
    let merged = a.remote.merge(local.clone()).await.unwrap();
    assert!(
        a.hosts
            .replace_if(&local.hosts, merged.data.hosts)
            .await
            .unwrap()
    );
    let a = restarted(a, &root, "a");
    rename(&b, "h1", "v2").await;
    b.sync().await;
    a.sync().await;
    b.sync().await;
    assert_eq!(label(&a, "h1").await.as_deref(), Some("v2"));
    assert_eq!(label(&b, "h1").await.as_deref(), Some("v2"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_first_sync_cut_short_is_still_a_first_sync_after() {
    let (a, b, root) = shared("first-cut").await;
    a.snippets
        .edit(|l| l.put(snippet("s1", "echo old")))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    b.snippets
        .edit(|l| l.put(snippet("s1", "echo new")))
        .await
        .unwrap();
    b.sync().await;
    // A loses its state, keeps its old lists, and its first sync stops
    // once the hosts are saved.
    std::fs::remove_file(root.join("a").join("sync-state.json")).unwrap();
    let a = restarted(a, &root, "a");
    let local = SyncData {
        hosts: a.hosts.snapshot().await,
        snippets: a.snippets.snapshot().await,
    };
    let merged = a.remote.merge(local.clone()).await.unwrap();
    assert!(
        a.hosts
            .replace_if(&local.hosts, merged.data.hosts)
            .await
            .unwrap()
    );
    a.remote.adopt(SyncPart::Hosts).await.unwrap();
    let a = restarted(a, &root, "a");
    a.sync().await;
    b.sync().await;
    assert_eq!(body(&a, "s1").await, "echo new");
    assert_eq!(body(&b, "s1").await, "echo new");
    let _ = std::fs::remove_dir_all(&root);
}

/// Makes the target folder unwritable until `restore` is called.
fn break_target(root: &Path) {
    let target = root.join("target");
    std::fs::rename(&target, root.join("target-away")).unwrap();
    std::fs::write(&target, b"").unwrap();
}

fn restore(root: &Path) {
    let target = root.join("target");
    std::fs::remove_file(&target).unwrap();
    std::fs::rename(root.join("target-away"), &target).unwrap();
}

#[tokio::test]
async fn a_clash_whose_publish_failed_is_told_on_the_next() {
    let (a, b, root) = shared("untold").await;
    rename(&b, "h1", "from-b").await;
    b.sync().await;
    rename(&a, "h1", "from-a").await;
    let local = SyncData {
        hosts: a.hosts.snapshot().await,
        snippets: a.snippets.snapshot().await,
    };
    let merged = a.remote.merge(local.clone()).await.unwrap();
    assert_eq!(merged.clashes.len(), 1);
    assert!(
        a.hosts
            .replace_if(&local.hosts, merged.data.hosts)
            .await
            .unwrap()
    );
    a.remote.adopt(SyncPart::Hosts).await.unwrap();
    a.remote.adopt(SyncPart::Snippets).await.unwrap();
    break_target(&root);
    assert!(a.remote.publish().await.is_err());
    restore(&root);
    // Nothing new: the retry still publishes, and still lists the clash.
    assert_eq!(a.sync().await.clashes.len(), 1);
    let on_b = b.sync().await;
    assert_eq!(on_b.clashes.len(), 1, "B is told its value lost");
    assert_eq!(label(&b, "h1").await.as_deref(), Some("from-a"));
    // Told once.
    assert_eq!(a.sync().await.clashes, []);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn an_edit_whose_publish_failed_gets_out_on_a_sync_with_nothing_new() {
    let (a, b, root) = shared("retry").await;
    rename(&a, "h1", "out").await;
    let local = SyncData {
        hosts: a.hosts.snapshot().await,
        snippets: a.snippets.snapshot().await,
    };
    let merged = a.remote.merge(local.clone()).await.unwrap();
    assert!(
        a.hosts
            .replace_if(&local.hosts, merged.data.hosts)
            .await
            .unwrap()
    );
    a.remote.adopt(SyncPart::Hosts).await.unwrap();
    a.remote.adopt(SyncPart::Snippets).await.unwrap();
    break_target(&root);
    assert!(a.remote.publish().await.is_err());
    restore(&root);
    a.sync().await;
    b.sync().await;
    assert_eq!(label(&b, "h1").await.as_deref(), Some("out"));
    let _ = std::fs::remove_dir_all(&root);
}

/// This device's object names on the target.
fn own_objects(root: &Path, other_than: &[&str]) -> Vec<String> {
    Folder::new(root.join("target"))
        .list()
        .unwrap()
        .into_iter()
        .filter(|n| n != KEY && !other_than.iter().any(|o| n.contains(o)))
        .collect()
}

#[tokio::test]
async fn a_device_whose_object_went_writes_it_again() {
    let (a, b, root) = shared("gone-own").await;
    let names = own_objects(&root, &[]);
    assert_eq!(names.len(), 2);
    let target = Folder::new(root.join("target"));
    for name in &names {
        target.remove(name).unwrap();
    }
    a.sync().await;
    assert_eq!(own_objects(&root, &[]).len(), 1);
    let c = restarted(
        device(
            &root.join("c"),
            &Arc::new(target),
            join(&Folder::new(root.join("target")), "correct horse").unwrap(),
        )
        .await,
        &root,
        "c",
    );
    c.sync().await;
    assert!(c.host("h1").await.is_some());
    drop(b);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn an_object_number_the_target_planted_past_this_devices_is_removed() {
    let (a, b, root) = shared("planted").await;
    let target = Folder::new(root.join("target"));
    let before = own_objects(&root, &[]);
    rename(&a, "h1", "mark").await;
    a.sync().await;
    let a_object = own_objects(&root, &[])
        .into_iter()
        .find(|n| !before.contains(n))
        .unwrap();
    let (device, _) = parse_object(&a_object).unwrap();
    let planted = object_name(device, u64::MAX);
    target.put_new(&planted, b"TMS1 planted").unwrap();
    for n in 0..2 {
        a.hosts
            .edit(|i| i.put_host(new_host(&format!("p{n}"), "web", "web.internal")))
            .await
            .unwrap();
        a.sync().await;
    }
    b.sync().await;
    assert!(b.host("p1").await.is_some());
    assert!(!target.list().unwrap().contains(&planted));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn forged_objects_are_tried_a_few_times_once_and_said_once() {
    let (_a, b, root) = shared("flood").await;
    let target = Folder::new(root.join("target"));
    let device = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    for seq in 1..=20 {
        target
            .put_new(&object_name(device, seq), b"TMS1 forged")
            .unwrap();
    }
    let first = b.sync().await;
    assert_eq!(first.notices.len(), 1, "{:?}", first.notices);
    assert!(
        first.notices[0].starts_with("3 object(s)"),
        "{:?}",
        first.notices
    );
    // Three more each sync, one line each, until each was tried once.
    for _ in 0..6 {
        assert_eq!(b.sync().await.notices.len(), 1);
    }
    assert_eq!(b.sync().await.notices, Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_device_whose_objects_vanished_is_told_once() {
    let (a, b, root) = shared("vanished").await;
    let before = own_objects(&root, &[]);
    rename(&b, "h1", "b-mark").await;
    b.sync().await;
    let b_object = own_objects(&root, &[])
        .into_iter()
        .find(|n| !before.contains(n))
        .unwrap();
    a.sync().await;
    Folder::new(root.join("target")).remove(&b_object).unwrap();
    assert_eq!(a.sync().await.notices.len(), 1);
    assert_eq!(a.sync().await.notices, Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&root);
}

fn ninety_one_days_on() -> u64 {
    now_ms() + 91 * 24 * 60 * 60 * 1000
}

#[tokio::test]
async fn an_object_not_written_for_ninety_days_is_not_read() {
    let (a, _b, root) = shared("stale").await;
    let target = Arc::new(Folder::new(root.join("target")));
    let key = join(target.as_ref(), "correct horse").unwrap();
    let later = Device {
        remote: Remote::with_clock(
            target.clone(),
            key,
            root.join("c").join("sync-state.json"),
            ninety_one_days_on,
        ),
        ..device(
            &root.join("c"),
            &target,
            join(target.as_ref(), "correct horse").unwrap(),
        )
        .await
    };
    let merged = later.sync().await;
    assert!(later.host("h1").await.is_none());
    assert!(
        merged.notices.iter().all(|n| n.contains("90 days")),
        "{:?}",
        merged.notices
    );
    assert_eq!(merged.notices.len(), 2);
    drop(a);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn variables_two_devices_add_past_the_limit_are_cut_alike() {
    let (a, b, root) = shared("env").await;
    for (device, prefix) in [(&a, "A"), (&b, "B")] {
        device
            .hosts
            .edit(|i| {
                let mut host = i.host(&HostId::parse("h1").unwrap()).unwrap().clone();
                host.settings.env = (0..20)
                    .map(|n| {
                        tauminus_domain::EnvVar::parse(&format!("{prefix}{n:02}"), "x").unwrap()
                    })
                    .collect();
                i.put_host(host)
            })
            .await
            .unwrap();
    }
    a.sync().await;
    let on_b = b.sync().await;
    assert_eq!(on_b.notices.len(), 1, "{:?}", on_b.notices);
    a.sync().await;
    let env = |h: Host| h.settings.env.len();
    assert_eq!(env(a.host("h1").await.unwrap()), 32);
    assert_eq!(
        a.host("h1").await.unwrap().settings.env,
        b.host("h1").await.unwrap().settings.env
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A folder whose reads fail while `down` is set.
struct Flaky {
    folder: Folder,
    down: std::sync::atomic::AtomicBool,
}

impl Target for Flaky {
    fn refresh(&self) -> std::io::Result<()> {
        self.folder.refresh()
    }
    fn list(&self) -> std::io::Result<Vec<String>> {
        self.folder.list()
    }
    fn get(&self, name: &str, limit: u64) -> std::io::Result<Vec<u8>> {
        if name != KEY && self.down.load(std::sync::atomic::Ordering::SeqCst) {
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
async fn an_object_the_target_failed_to_send_is_read_on_the_next_sync() {
    let (a, b, root) = shared("flaky").await;
    let flaky = Arc::new(Flaky {
        folder: Folder::new(root.join("target")),
        down: std::sync::atomic::AtomicBool::new(false),
    });
    let a = Device {
        remote: Remote::new(
            Arc::clone(&flaky) as Arc<dyn Target>,
            join(&flaky.folder, "correct horse").unwrap(),
            root.join("a").join("sync-state.json"),
        ),
        ..a
    };
    rename(&b, "h1", "from-b").await;
    b.sync().await;
    flaky.down.store(true, std::sync::atomic::Ordering::SeqCst);
    let merged = a.sync().await;
    assert_eq!(merged.notices.len(), 1, "{:?}", merged.notices);
    flaky.down.store(false, std::sync::atomic::Ordering::SeqCst);
    a.sync().await;
    assert_eq!(label(&a, "h1").await.as_deref(), Some("from-b"));
    let _ = std::fs::remove_dir_all(&root);
}

/// A's device id, from its object on the target.
async fn device_of(a: &Device, root: &Path) -> String {
    static MARKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let before = own_objects(root, &[]);
    let mark = MARKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    rename(a, "h1", &format!("mark-{mark}")).await;
    a.sync().await;
    let name = own_objects(root, &[])
        .into_iter()
        .find(|n| !before.contains(n))
        .unwrap();
    parse_object(&name).unwrap().0.to_owned()
}

/// A's object number on the target.
fn seq_of(root: &Path, device: &str) -> u64 {
    own_objects(root, &[])
        .iter()
        .filter_map(|n| parse_object(n))
        .filter(|(d, _)| *d == device)
        .map(|(_, s)| s)
        .max()
        .unwrap()
}

#[tokio::test]
async fn names_the_target_planted_right_after_this_devices_do_not_stop_it() {
    let (a, b, root) = shared("planted-next").await;
    let device = device_of(&a, &root).await;
    let seq = seq_of(&root, &device);
    let target = Folder::new(root.join("target"));
    for n in 1..=4 {
        target
            .put_new(&object_name(&device, seq + n), b"TMS1 planted")
            .unwrap();
    }
    // And a folder where the one after goes: no listing shows it.
    std::fs::create_dir_all(
        root.join("target")
            .join(object_name(&device, seq + 1).replace('/', std::path::MAIN_SEPARATOR_STR)),
    )
    .ok();
    rename(&a, "h1", "through").await;
    a.sync().await;
    b.sync().await;
    assert_eq!(label(&b, "h1").await.as_deref(), Some("through"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_name_planted_far_ahead_hides_no_later_object() {
    let (a, b, root) = shared("far-ahead").await;
    let device = device_of(&a, &root).await;
    b.sync().await;
    Folder::new(root.join("target"))
        .put_new(&object_name(&device, u64::MAX), b"TMS1 planted")
        .unwrap();
    b.sync().await;
    rename(&a, "h1", "after").await;
    a.sync().await;
    b.sync().await;
    assert_eq!(label(&b, "h1").await.as_deref(), Some("after"));
    let _ = std::fs::remove_dir_all(&root);
}

/// Copies `dir`'s sync state files aside, or back.
fn copy_state(root: &Path, dir: &str, aside: &str, back: bool) {
    for ext in ["json", "doc"] {
        let from = root.join(dir).join(format!("sync-state.{ext}"));
        let to = root.join(format!("{aside}.{ext}"));
        let (from, to) = if back { (to, from) } else { (from, to) };
        std::fs::copy(from, to).unwrap();
    }
}

#[tokio::test]
async fn a_state_restored_from_a_backup_still_gets_its_edits_out() {
    let (a, b, root) = shared("backup").await;
    copy_state(&root, "a", "backup", false);
    for n in 0..3 {
        rename(&a, "h1", &format!("on-{n}")).await;
        a.sync().await;
    }
    b.sync().await;
    copy_state(&root, "a", "backup", true);
    let a = restarted(a, &root, "a");
    a.hosts
        .edit(|i| i.put_host(new_host("h2", "after", "after.internal")))
        .await
        .unwrap();
    a.sync().await;
    b.sync().await;
    assert!(b.host("h2").await.is_some());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn an_edit_whose_saved_state_lost_its_last_write_still_gets_out() {
    let (a, b, root) = shared("lost-header").await;
    rename(&a, "h1", "mine").await;
    let local = SyncData {
        hosts: a.hosts.snapshot().await,
        snippets: a.snippets.snapshot().await,
    };
    let merged = a.remote.merge(local.clone()).await.unwrap();
    assert!(
        a.hosts
            .replace_if(&local.hosts, merged.data.hosts)
            .await
            .unwrap()
    );
    let header = root.join("a").join("sync-state.json");
    let before = std::fs::read(&header).unwrap();
    a.remote.adopt(SyncPart::Hosts).await.unwrap();
    // The header written with the base is lost; the base is not.
    std::fs::write(&header, &before).unwrap();
    let a = restarted(a, &root, "a");
    a.sync().await;
    b.sync().await;
    assert_eq!(label(&b, "h1").await.as_deref(), Some("mine"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_base_gone_from_beside_its_state_joins_again() {
    let (a, b, root) = shared("lost-doc").await;
    rename(&b, "h1", "b-new").await;
    b.sync().await;
    // A, offline, still shows "db"; its base file is lost.
    std::fs::remove_file(root.join("a").join("sync-state.doc")).unwrap();
    let a = restarted(a, &root, "a");
    let merged = a.sync().await;
    assert!(
        merged.notices.iter().any(|n| n.contains("incomplete")),
        "{:?}",
        merged.notices
    );
    b.sync().await;
    assert_eq!(label(&a, "h1").await.as_deref(), Some("b-new"));
    assert_eq!(label(&b, "h1").await.as_deref(), Some("b-new"));
    let _ = std::fs::remove_dir_all(&root);
}

fn thirty_one_days_on() -> u64 {
    now_ms() + 31 * 24 * 60 * 60 * 1000
}

#[tokio::test]
async fn a_device_with_nothing_new_writes_its_object_again_within_thirty_days() {
    let (a, _b, root) = shared("keepalive").await;
    let device = device_of(&a, &root).await;
    let seq = seq_of(&root, &device);
    a.sync().await;
    assert_eq!(seq_of(&root, &device), seq, "nothing new: nothing written");
    let target = Arc::new(Folder::new(root.join("target")));
    let later = Device {
        remote: Remote::with_clock(
            target.clone(),
            join(target.as_ref(), "correct horse").unwrap(),
            root.join("a").join("sync-state.json"),
            thirty_one_days_on,
        ),
        ..a
    };
    later.sync().await;
    assert_eq!(seq_of(&root, &device), seq + 1, "written again");
    later.sync().await;
    assert_eq!(seq_of(&root, &device), seq + 1, "and not again");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_burst_of_edits_elsewhere_costs_one_object_here() {
    let (a, b, root) = shared("burst").await;
    let device = device_of(&a, &root).await;
    b.sync().await;
    let seq = seq_of(&root, &device);
    for n in 0..3 {
        rename(&b, "h1", &format!("burst-{n}")).await;
        b.sync().await;
        a.sync().await;
    }
    assert_eq!(label(&a, "h1").await.as_deref(), Some("burst-2"));
    assert_eq!(seq_of(&root, &device), seq, "nothing of A's own yet");
    let target = Arc::new(Folder::new(root.join("target")));
    let later = Device {
        remote: Remote::with_clock(
            target.clone(),
            join(target.as_ref(), "correct horse").unwrap(),
            root.join("a").join("sync-state.json"),
            sixty_one_minutes_on,
        ),
        ..a
    };
    later.sync().await;
    later.sync().await;
    assert_eq!(seq_of(&root, &device), seq + 1);
    let _ = std::fs::remove_dir_all(&root);
}

/// A file past [`MAX_OBJECT`] at `name`, sparse, so it costs no disk.
fn plant_oversized(root: &Path, name: &str) {
    let path = root.join("target").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::File::create(&path)
        .unwrap()
        .set_len(MAX_OBJECT + 1)
        .unwrap();
}

#[tokio::test]
async fn an_oversized_file_planted_in_either_folder_stops_nothing() {
    let (a, b, root) = shared("oversized").await;
    let a_device = device_of(&a, &root).await;
    b.sync().await;
    let b_device = device_of(&b, &root).await;
    a.sync().await;
    let a_seq = seq_of(&root, &a_device);
    plant_oversized(&root, &object_name(&a_device, a_seq + 1));
    plant_oversized(&root, &object_name(&b_device, u64::MAX));
    rename(&a, "h1", "from-a").await;
    a.sync().await;
    rename(&b, "h1", "from-b").await;
    b.sync().await;
    a.sync().await;
    assert_eq!(label(&a, "h1").await.as_deref(), Some("from-b"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_number_found_failed_is_not_the_one_its_owner_writes_next() {
    let (a, b, root) = shared("reused").await;
    let b_device = device_of(&b, &root).await;
    a.sync().await;
    let b_seq = seq_of(&root, &b_device);
    Folder::new(root.join("target"))
        .put_new(&object_name(&b_device, b_seq + 1), b"TMS1 forged")
        .unwrap();
    a.sync().await;
    rename(&b, "h1", "after-forged").await;
    b.sync().await;
    a.sync().await;
    assert_eq!(label(&a, "h1").await.as_deref(), Some("after-forged"));
    let _ = std::fs::remove_dir_all(&root);
}

fn sixty_one_minutes_on() -> u64 {
    now_ms() + 61 * 60 * 1000
}

#[tokio::test]
async fn an_object_read_half_copied_is_read_whole_later() {
    let (a, b, root) = shared("half").await;
    let b_device = device_of(&b, &root).await;
    let name = object_name(&b_device, seq_of(&root, &b_device));
    let path = root.join("target").join(&name);
    let whole = std::fs::read(&path).unwrap();
    // A reads it while a sync tool is still copying it.
    std::fs::write(&path, &whole[..whole.len() / 2]).unwrap();
    a.sync().await;
    assert_ne!(label(&a, "h1").await, label(&b, "h1").await);
    std::fs::write(&path, &whole).unwrap();
    let target = Arc::new(Folder::new(root.join("target")));
    let later = Device {
        remote: Remote::with_clock(
            target.clone(),
            join(target.as_ref(), "correct horse").unwrap(),
            root.join("a").join("sync-state.json"),
            sixty_one_minutes_on,
        ),
        ..a
    };
    later.sync().await;
    assert_eq!(label(&later, "h1").await, label(&b, "h1").await);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn folders_planted_at_this_devices_next_names_are_removed() {
    let (a, b, root) = shared("planted-dirs").await;
    let device = device_of(&a, &root).await;
    let seq = seq_of(&root, &device);
    for n in 1..=5 {
        std::fs::create_dir_all(root.join("target").join(object_name(&device, seq + n))).unwrap();
    }
    rename(&a, "h1", "past-folders").await;
    a.sync().await;
    b.sync().await;
    assert_eq!(label(&b, "h1").await.as_deref(), Some("past-folders"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn devices_a_target_invents_crowd_out_none_read_before() {
    let (a, b, root) = shared("crowd").await;
    device_of(&a, &root).await;
    b.sync().await;
    let target = Folder::new(root.join("target"));
    for d in 1..=6 {
        let device = format!("{d:032x}");
        for seq in 1..=3 {
            target
                .put_new(&object_name(&device, seq), b"TMS1 forged")
                .unwrap();
        }
    }
    rename(&a, "h1", "not-crowded").await;
    a.sync().await;
    b.sync().await;
    assert_eq!(label(&b, "h1").await.as_deref(), Some("not-crowded"));
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn links_planted_in_either_folder_stop_nothing() {
    let (a, b, root) = shared("links-planted").await;
    let a_device = device_of(&a, &root).await;
    b.sync().await;
    let b_device = device_of(&b, &root).await;
    a.sync().await;
    let a_seq = seq_of(&root, &a_device);
    let at = |name: String| root.join("target").join(name);
    std::os::unix::fs::symlink("/etc/hostname", at(object_name(&a_device, a_seq + 1))).unwrap();
    std::os::unix::fs::symlink("/nonexistent", at(object_name(&b_device, u64::MAX))).unwrap();
    rename(&a, "h1", "from-a").await;
    a.sync().await;
    rename(&b, "h1", "from-b").await;
    b.sync().await;
    a.sync().await;
    assert_eq!(label(&a, "h1").await.as_deref(), Some("from-b"));
    let _ = std::fs::remove_dir_all(&root);
}

fn a_year_on() -> u64 {
    now_ms() + 365 * 24 * 60 * 60 * 1000
}

#[tokio::test]
async fn a_failure_noted_while_the_clock_ran_fast_is_tried_again() {
    let (a, b, root) = shared("fast-failure").await;
    let b_device = device_of(&b, &root).await;
    let name = object_name(&b_device, seq_of(&root, &b_device));
    let path = root.join("target").join(&name);
    let whole = std::fs::read(&path).unwrap();
    std::fs::write(&path, &whole[..whole.len() / 2]).unwrap();
    let target = Arc::new(Folder::new(root.join("target")));
    let fast = Device {
        remote: Remote::with_clock(
            target.clone(),
            join(target.as_ref(), "correct horse").unwrap(),
            root.join("a").join("sync-state.json"),
            a_year_on,
        ),
        ..a
    };
    fast.sync().await;
    std::fs::write(&path, &whole).unwrap();
    // The clock set right: the failure is in its future, so long ago.
    let a = restarted(fast, &root, "a");
    a.sync().await;
    assert_eq!(label(&a, "h1").await, label(&b, "h1").await);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn names_planted_above_a_restored_devices_own_never_number_it_back() {
    let (a, b, root) = shared("restore-flood").await;
    copy_state(&root, "a", "backup", false);
    let device = device_of(&a, &root).await;
    for n in 0..3 {
        rename(&a, "h1", &format!("mid-{n}")).await;
        a.sync().await;
    }
    b.sync().await;
    let real = seq_of(&root, &device);
    let target = Folder::new(root.join("target"));
    for n in 1..=20 {
        target
            .put_new(&object_name(&device, real + n), b"TMS1 planted")
            .unwrap();
    }
    copy_state(&root, "a", "backup", true);
    let a = restarted(a, &root, "a");
    rename(&a, "h1", "after-restore").await;
    a.sync().await;
    assert!(seq_of(&root, &device) > real);
    b.sync().await;
    assert_eq!(label(&b, "h1").await.as_deref(), Some("after-restore"));
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn a_planted_folder_that_will_not_go_stops_no_sync() {
    use std::os::unix::fs::PermissionsExt as _;
    let (a, b, root) = shared("stuck-folder").await;
    let device = device_of(&a, &root).await;
    let seq = seq_of(&root, &device);
    let inner = root
        .join("target")
        .join(object_name(&device, seq + 50))
        .join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join("f"), b"x").unwrap();
    std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o500)).unwrap();
    rename(&a, "h1", "past-stuck").await;
    a.sync().await;
    // Once more, to number past the folder that stays.
    a.sync().await;
    let written = own_objects(&root, &[]);
    // Nothing new: nothing written, though the folder stays.
    a.sync().await;
    a.sync().await;
    assert_eq!(own_objects(&root, &[]), written);
    b.sync().await;
    assert_eq!(label(&b, "h1").await.as_deref(), Some("past-stuck"));
    std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o700)).unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn folders_that_will_not_go_never_fill_what_is_remembered_of_them() {
    use std::os::unix::fs::PermissionsExt as _;
    let (a, _b, root) = shared("stuck-many").await;
    let device = device_of(&a, &root).await;
    let seq = seq_of(&root, &device);
    let plant = |at: u64| {
        let inner = root
            .join("target")
            .join(object_name(&device, at))
            .join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("f"), b"x").unwrap();
        std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o500)).unwrap();
        inner
    };
    let mut planted: Vec<_> = (0..16).map(|n| plant(seq + 1000 + n)).collect();
    rename(&a, "h1", "first").await;
    a.sync().await;
    a.sync().await;
    planted.push(plant(seq + 2000));
    a.sync().await;
    a.sync().await;
    let written = own_objects(&root, &[]);
    a.sync().await;
    a.sync().await;
    assert_eq!(
        own_objects(&root, &[]),
        written,
        "nothing new: nothing written"
    );
    for inner in planted {
        std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let _ = std::fs::remove_dir_all(&root);
}
