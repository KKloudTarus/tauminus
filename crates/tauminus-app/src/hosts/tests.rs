// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::ports::BoxFuture;

use super::*;

#[derive(Default)]
struct MemoryStore {
    saved: StdMutex<Option<Inventory>>,
    fail: AtomicBool,
    /// The next save reports that another window changed the list.
    conflict: AtomicBool,
    next: AtomicUsize,
}

impl HostStore for MemoryStore {
    fn load(&self) -> BoxFuture<'_, Result<Inventory, StoreError>> {
        let inventory = self.saved.lock().unwrap().clone().unwrap_or_default();
        Box::pin(async move { Ok(inventory) })
    }

    fn save<'a>(&'a self, inventory: &'a Inventory) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            if self.fail.load(Ordering::Relaxed) {
                return Err(StoreError::Io(std::io::Error::other("disk full")));
            }
            if self.conflict.swap(false, Ordering::Relaxed) {
                return Err(StoreError::Conflict);
            }
            *self.saved.lock().unwrap() = Some(inventory.clone());
            Ok(())
        })
    }

    fn new_id(&self) -> String {
        format!("id{}", self.next.fetch_add(1, Ordering::Relaxed))
    }
}

struct Config(Vec<ConfigHost>);

impl SshConfigSource for Config {
    fn hosts(&self) -> BoxFuture<'_, Result<Vec<ConfigHost>, std::io::Error>> {
        let hosts = self.0.clone();
        Box::pin(async move { Ok(hosts) })
    }
}

fn entry(alias: &str, hostname: &str) -> ConfigHost {
    ConfigHost {
        alias: alias.into(),
        hostname: hostname.into(),
        ..ConfigHost::default()
    }
}

async fn book() -> (Arc<MemoryStore>, HostBook) {
    let store = Arc::new(MemoryStore::default());
    let book = HostBook::open(Arc::clone(&store) as Arc<dyn HostStore>)
        .await
        .unwrap();
    (store, book)
}

fn sample(book: &HostBook, label: &str) -> Host {
    Host::new(
        book.new_host_id().unwrap(),
        Label::parse(label).unwrap(),
        HostName::parse("db.example.com").unwrap(),
    )
}

#[tokio::test]
async fn an_edit_is_saved_before_it_is_visible() {
    let (store, book) = book().await;
    let host = sample(&book, "db");
    let id = host.id.clone();
    book.edit(|inventory| inventory.put_host(host))
        .await
        .unwrap();
    assert!(book.snapshot().await.host(&id).is_some());
    assert!(
        store
            .saved
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .host(&id)
            .is_some()
    );
}

#[tokio::test]
async fn a_failed_save_changes_nothing() {
    let (store, book) = book().await;
    store.fail.store(true, Ordering::Relaxed);
    let host = sample(&book, "db");
    let result = book.edit(|inventory| inventory.put_host(host)).await;
    assert!(matches!(result, Err(HostBookError::Store(_))));
    assert_eq!(book.snapshot().await, Inventory::default());
}

#[tokio::test]
async fn a_rejected_change_is_not_saved() {
    let (store, book) = book().await;
    let mut host = sample(&book, "db");
    host.group = Some(GroupId::parse("ghost").unwrap());
    let result = book.edit(|inventory| inventory.put_host(host)).await;
    assert!(matches!(
        result,
        Err(HostBookError::Hosts(HostsError::NoSuchGroup(_)))
    ));
    assert!(store.saved.lock().unwrap().is_none());
}

#[tokio::test]
async fn import_maps_settings_into_one_group_and_skips_repeats() {
    let (_, book) = book().await;
    let mut web = entry("web", "web.internal");
    web.user = Some("deploy".into());
    web.port = Some(2222);
    web.proxy_jump = Some("ops@bastion:2200".into());
    web.identity_files = vec!["/home/me/.ssh/id_work".into()];
    web.identities_only = true;
    let mut legacy = entry("legacy", "legacy.internal");
    legacy.proxy_command = Some("nc %h %p".into());
    legacy.port = Some(22);
    // Same address and port as web, reached another way: a separate host.
    let mut same_address = entry("same-address", "WEB.internal");
    same_address.port = Some(2222);
    let config = Config(vec![
        web,
        legacy,
        entry("bad", "bad host"),
        same_address,
        entry("web", "elsewhere.internal"),
    ]);

    let (report, inventory) = book.import(&config).await.unwrap();
    let added: Vec<_> = report
        .added
        .iter()
        .map(|id| inventory.host(id).unwrap().label.as_str())
        .collect();
    assert_eq!(added, ["web", "legacy", "same-address"]);
    let skipped: Vec<_> = report
        .skipped
        .iter()
        .map(|(alias, _)| alias.as_str())
        .collect();
    // An invalid name, or an alias already saved.
    assert_eq!(skipped, ["bad", "web"], "{:?}", report.skipped);
    let me = UserName::parse("me").unwrap();
    let web = inventory
        .hosts()
        .find(|h| h.label.as_str() == "web")
        .unwrap();
    let target = inventory.resolve(&web.id, &me).unwrap();
    assert_eq!(target.destination.to_string(), "deploy@web.internal:2222");
    assert_eq!(target.jumps[0].to_string(), "ops@bastion:2200");
    assert!(target.auth.identities_only);
    let legacy = inventory
        .hosts()
        .find(|h| h.label.as_str() == "legacy")
        .unwrap();
    assert_eq!(legacy.settings.port, None, "port 22 is left to the default");
    assert!(matches!(
        legacy.settings.route,
        Some(Route::ProxyCommand(_))
    ));
    assert_eq!(
        inventory
            .groups()
            .filter(|g| g.name.as_str() == IMPORT_GROUP)
            .count(),
        1
    );

    let (again, after) = book.import(&config).await.unwrap();
    assert!(again.added.is_empty(), "importing twice adds nothing");
    assert_eq!(after.hosts().count(), inventory.hosts().count());
    assert_eq!(after.groups().count(), 1, "the import group is reused");
}

#[tokio::test]
async fn after_a_conflict_the_other_window_s_list_is_taken() {
    let (store, book) = book().await;
    // Another window saved a host meanwhile.
    let theirs = sample(&book, "theirs");
    let theirs_id = theirs.id.clone();
    let mut other = Inventory::default();
    other.put_host(theirs).unwrap();
    *store.saved.lock().unwrap() = Some(other);
    store.conflict.store(true, Ordering::Relaxed);

    let mine = sample(&book, "mine");
    let result = book
        .edit(|inventory| inventory.put_host(mine.clone()))
        .await;
    assert!(
        matches!(result, Err(HostBookError::Store(StoreError::Conflict))),
        "{result:?}"
    );
    assert!(
        book.snapshot().await.host(&theirs_id).is_some(),
        "reloaded from the store"
    );
    // Trying again keeps both.
    let both = book
        .edit(|inventory| inventory.put_host(mine))
        .await
        .unwrap();
    assert_eq!(both.hosts().count(), 2);
}
