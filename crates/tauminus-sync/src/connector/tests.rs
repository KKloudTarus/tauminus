// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::Path;

use tauminus_app::ports::{SyncData, SyncPart};

use super::*;

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("tauminus-connector-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn connector(root: &Path) -> Connector {
    Connector::new(root.join("state")).with_kdf(Kdf::LEAST)
}

fn folder(root: &Path) -> SyncTarget {
    let shared = root.join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    SyncTarget::Folder(shared.to_string_lossy().into_owned())
}

fn secret(text: &str) -> Secret {
    Secret::new(text.into())
}

#[tokio::test]
async fn a_folder_is_created_joined_and_synced_through() {
    let root = scratch("folder");
    let connector = connector(&root);
    let target = folder(&root);
    let key = connector
        .create(&target, &secret("correct horse battery"))
        .await
        .unwrap();
    let again = connector
        .create(&target, &secret("correct horse battery"))
        .await;
    assert!(matches!(again, Err(SyncError::Target(_))), "{again:?}");
    let joined = connector
        .join(&target, &secret("correct horse battery"))
        .await
        .unwrap();
    assert_eq!(*joined, *key);
    let wrong = connector
        .join(&target, &secret("horse battery staple"))
        .await;
    assert!(matches!(wrong, Err(SyncError::Sealed(_))), "{wrong:?}");

    let remote = connector.open(&target, &key).unwrap();
    remote.merge(SyncData::default()).await.unwrap();
    remote.adopt(SyncPart::Hosts).await.unwrap();
    remote.adopt(SyncPart::Snippets).await.unwrap();
    remote.publish().await.unwrap();
    assert!(
        root.join("state").join("state").is_file(),
        "this device's state"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn the_target_is_kept_until_closed_and_a_bad_one_is_refused() {
    let root = scratch("kept");
    let connector = connector(&root);
    let target = folder(&root);
    let first = connector.target(&target).unwrap();
    assert!(Arc::ptr_eq(&first, &connector.target(&target).unwrap()));
    connector.close();
    assert!(!Arc::ptr_eq(&first, &connector.target(&target).unwrap()));
    for bad in [
        SyncTarget::Folder("relative/dir".into()),
        SyncTarget::Folder(root.join("missing").to_string_lossy().into_owned()),
        SyncTarget::Git(secret(&root.to_string_lossy())),
        SyncTarget::Git(secret("--upload-pack=touch /tmp/x")),
    ] {
        let refused = connector
            .create(&bad, &secret("correct horse battery"))
            .await;
        assert!(
            matches!(refused, Err(SyncError::Target(_))),
            "{bad:?}: {refused:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn wiping_removes_the_state_read_only_files_included() {
    let root = scratch("wipe");
    let connector = connector(&root);
    connector.wipe().await.unwrap();
    let pack = root.join("state").join("git").join("objects");
    std::fs::create_dir_all(&pack).unwrap();
    // Git writes its packs read-only.
    let file = pack.join("pack-1.pack");
    std::fs::write(&file, b"pack").unwrap();
    let mut permissions = std::fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&file, permissions).unwrap();
    connector.wipe().await.unwrap();
    assert!(!root.join("state").exists());
    let _ = std::fs::remove_dir_all(&root);
}
