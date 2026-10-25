// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::fakes::Derive;
use crate::vault::tests::{secret, vault};

fn input(name: &str, sync: bool) -> IdentityInput {
    IdentityInput {
        name: name.into(),
        user: Some("deploy".into()),
        password: Some(secret("hunter2-long")),
        sync: Some(sync),
        ..IdentityInput::default()
    }
}

fn remote(id: &str, name: &str, key: Option<&str>) -> SyncedIdentity {
    SyncedIdentity {
        id: id.into(),
        name: name.into(),
        user: Some("ops".into()),
        password: None,
        private_key: key.map(secret),
        passphrase: None,
    }
}

async fn open() -> Arc<Vault> {
    let (_, vault) = vault();
    vault.create(secret("vault password")).await.unwrap();
    vault
}

#[tokio::test]
async fn only_identities_this_device_shares_go() {
    let vault = open().await;
    let shared_id = vault.save_identity(input("shared", true)).await.unwrap();
    vault.save_identity(input("mine", false)).await.unwrap();
    let synced = vault.synced_identities().await.unwrap();
    assert_eq!(synced.len(), 1);
    assert_eq!(synced[0].id, shared_id);
    assert_eq!(
        synced[0].password.as_deref().map(String::as_str),
        Some("hunter2-long")
    );
    assert!(!format!("{synced:?}").contains("hunter2"));
}

#[tokio::test]
async fn what_comes_is_taken_with_its_key_derived_here_and_never_over_a_local_one() {
    let vault = open().await;
    let mine = vault.save_identity(input("mine", false)).await.unwrap();
    let expected = vault.synced_identities().await.unwrap();
    let merged = vec![
        remote("theirs", "theirs", Some("KEY1")),
        remote("broken", "broken", Some("bad key")),
        // Under the id of one this device keeps to itself.
        remote(&mine, "planted", None),
    ];
    let applied = vault
        .apply_synced(expected, merged, Arc::new(Derive))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(applied.refused, ["broken", mine.as_str()]);
    let notices = applied.notices;
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(notices.iter().any(|n| n.contains("keeps to itself")));
    assert!(
        notices
            .iter()
            .any(|n| n.contains("broken") && n.contains("not a key"))
    );
    let all = vault.identities().await.unwrap();
    let theirs = all.iter().find(|i| i.id == "theirs").unwrap();
    assert!(theirs.sync);
    assert_eq!(theirs.public_key.as_deref(), Some("ssh-ed25519 KEY1"));
    assert!(all.iter().all(|i| i.id != "broken"));
    let local = all.iter().find(|i| i.id == mine).unwrap();
    assert_eq!(local.name, "mine", "untouched");
    assert!(!local.sync, "never turned on from the target");
}

#[tokio::test]
async fn one_gone_from_the_target_stays_here_unshared_and_a_change_meanwhile_waits() {
    let vault = open().await;
    let id = vault.save_identity(input("shared", true)).await.unwrap();
    let expected = vault.synced_identities().await.unwrap();
    // Edited here while the sync ran: nothing is taken.
    let mut edit = input("renamed", true);
    edit.id = Some(id.clone());
    vault.save_identity(edit).await.unwrap();
    let stale = vault
        .apply_synced(expected, Vec::new(), Arc::new(Derive))
        .await
        .unwrap();
    assert!(stale.is_none());
    let expected = vault.synced_identities().await.unwrap();
    vault
        .apply_synced(expected, Vec::new(), Arc::new(Derive))
        .await
        .unwrap()
        .unwrap();
    let kept = &vault.identities().await.unwrap()[0];
    assert_eq!(kept.name, "renamed");
    assert!(!kept.sync);
}

#[tokio::test]
async fn prints_change_with_what_an_identity_sends_and_show_no_secret() {
    let vault = open().await;
    let id = vault.save_identity(input("shared", true)).await.unwrap();
    let first = vault.identity_prints(None).await.unwrap();
    let again = vault.identity_prints(None).await.unwrap();
    assert_eq!(first, again, "one review key");
    let print = &first[&id];
    assert!(!format!("{print:?}").contains("hunter2"));
    assert!(
        print
            .lines
            .iter()
            .any(|(f, v)| *f == "identity login" && v == "a password")
    );
    let mut edit = input("shared", true);
    edit.id = Some(id.clone());
    edit.password = Some(secret("another-password"));
    vault.save_identity(edit).await.unwrap();
    let changed = vault.identity_prints(None).await.unwrap();
    assert_ne!(changed[&id].digest, print.digest);
    // One kept here has a print too: an identity a sync stopped sharing
    // keeps what it brought, and hosts using it stay held.
    let mine = vault.save_identity(input("mine", false)).await.unwrap();
    assert_eq!(vault.identity_prints(None).await.unwrap().len(), 2);
    let asked = vault
        .identity_prints(Some(vec![mine.clone()]))
        .await
        .unwrap();
    assert_eq!(
        asked.keys().collect::<Vec<_>>(),
        [&mine],
        "only those asked"
    );
}

#[tokio::test]
async fn a_save_and_a_delete_say_what_the_identity_was_just_before_and_after() {
    let vault = open().await;
    let (id, before, after) = vault
        .save_identity_printed(input("shared", true))
        .await
        .unwrap();
    assert!(before.is_empty());
    assert!(after.contains_key(&id));
    let mut edit = input("shared", true);
    edit.id = Some(id.clone());
    edit.password = Some(secret("changed-password"));
    let (_, was, now) = vault.save_identity_printed(edit).await.unwrap();
    assert_ne!(was[&id].digest, now[&id].digest);
    // Only the identity saved is read.
    let (mine, was, now) = vault
        .save_identity_printed(input("mine", false))
        .await
        .unwrap();
    assert!(was.is_empty());
    assert_eq!(now.keys().collect::<Vec<_>>(), [&mine]);
    let (was, now) = vault.delete_identity_printed(id.clone()).await.unwrap();
    assert!(was.contains_key(&id) && !now.contains_key(&id));
}

#[tokio::test]
async fn sync_prints_take_the_merge_only_over_what_this_device_shares() {
    let vault = open().await;
    let shared = vault.save_identity(input("shared", true)).await.unwrap();
    let mine = vault.save_identity(input("mine", false)).await.unwrap();
    let expected = vault.synced_identities().await.unwrap();
    let merged = vec![
        remote(&shared, "renamed", None),
        remote(&mine, "planted", None),
        remote("new", "new", None),
    ];
    let (before, after) = vault
        .sync_prints(expected.clone(), merged.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before.len(), 2, "every identity, shared or not");
    assert_ne!(before[&shared], after[&shared]);
    assert_eq!(
        before[&mine], after[&mine],
        "never taken over one kept here"
    );
    assert!(!before.contains_key("new") && after.contains_key("new"));
    // A share changed since the merge began: nothing to go on.
    let mut edit = input("shared", false);
    edit.id = Some(shared);
    vault.save_identity(edit).await.unwrap();
    assert!(vault.sync_prints(expected, merged).await.unwrap().is_none());
}

#[tokio::test]
async fn at_the_limit_what_is_new_waits_and_changes_go_in() {
    let vault = open().await;
    let shared = vault.save_identity(input("shared", true)).await.unwrap();
    let expected = vault.synced_identities().await.unwrap();
    let mut merged = vec![remote(&shared, "renamed", None)];
    merged.extend((0..MAX_IDENTITIES).map(|i| remote(&format!("new{i:04}"), "new", None)));
    merged.sort_by(|a, b| a.id.cmp(&b.id));
    let applied = vault
        .apply_synced(expected, merged, Arc::new(Derive))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(applied.refused.len(), 1, "one over the limit");
    assert!(applied.notices[0].contains("not all taken"));
    let all = vault.identities().await.unwrap();
    assert_eq!(all.len(), MAX_IDENTITIES);
    assert!(all.iter().any(|i| i.id == shared && i.name == "renamed"));
}

#[tokio::test]
async fn a_key_that_was_not_derived_is_never_given_the_old_public_half() {
    let vault = open().await;
    let mut with_key = input("shared", true);
    with_key.private_key = Some(secret("KEY1"));
    with_key.public_key = Some("ssh-ed25519 KEY1".into());
    let id = vault.save_identity(with_key).await.unwrap();
    let expected = vault.synced_identities().await.unwrap();
    let mut merged = vec![remote(&id, "shared", Some("KEY2"))];
    // Over the limit: nothing is derived.
    merged.extend((0..MAX_IDENTITIES).map(|i| remote(&format!("new{i:04}"), "new", None)));
    let applied = vault
        .apply_synced(expected, merged, Arc::new(Derive))
        .await
        .unwrap()
        .unwrap();
    assert!(applied.refused.contains(&id));
    let saved = vault.identities().await.unwrap();
    let saved = saved.iter().find(|i| i.id == id).unwrap();
    assert_eq!(saved.public_key.as_deref(), Some("ssh-ed25519 KEY1"));
}
