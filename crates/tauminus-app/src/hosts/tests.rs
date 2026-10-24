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

/// Another client's sessions, in its own group.
struct Putty(Vec<ConfigHost>);

impl SshConfigSource for Putty {
    fn hosts(&self) -> BoxFuture<'_, Result<Vec<ConfigHost>, std::io::Error>> {
        let hosts = self.0.clone();
        Box::pin(async move { Ok(hosts) })
    }
    fn group(&self) -> &'static str {
        "PuTTY"
    }
    fn what(&self) -> &'static str {
        "PuTTY's sessions"
    }
}

struct Unreadable;

impl SshConfigSource for Unreadable {
    fn hosts(&self) -> BoxFuture<'_, Result<Vec<ConfigHost>, std::io::Error>> {
        Box::pin(async { Err(std::io::Error::other("bad file")) })
    }
    fn what(&self) -> &'static str {
        "sessions.ini"
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
    web.identity_agent = Some("/Users/me/secretive/socket.ssh".into());
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
    assert_eq!(
        target.auth.identity_agent,
        Some(IdentityAgent::Path("/Users/me/secretive/socket.ssh".into()))
    );
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

#[tokio::test]
async fn imports_agent_forwarding_and_environment() {
    let (_, book) = book().await;
    let mut fa = entry("fa", "fa.internal");
    fa.forward_agent = Some(true);
    fa.set_env = vec![("TEAM".into(), "db".into()), ("LANG".into(), "C".into())];
    let mut bad = entry("bad-env", "bad.internal");
    bad.set_env = vec![("1BAD".into(), "x".into())];
    let (report, inventory) = book.import(&Config(vec![fa, bad])).await.unwrap();
    assert_eq!(report.added.len(), 1);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].0, "bad-env");
    let host = inventory.host(&report.added[0]).unwrap();
    assert_eq!(host.settings.forward_agent, Some(true));
    assert_eq!(
        host.settings
            .env
            .iter()
            .map(|v| (v.name(), v.value()))
            .collect::<Vec<_>>(),
        [("LANG", "C"), ("TEAM", "db")]
    );
}

#[tokio::test]
async fn another_client_s_folders_become_groups_in_its_own_group() {
    let (_, book) = book().await;
    let in_folder = |alias: &str, folder: &[&str]| ConfigHost {
        folder: folder.iter().map(|f| (*f).to_owned()).collect(),
        ..entry(alias, &format!("{alias}.internal"))
    };
    let source = Putty(vec![
        in_folder("web", &["prod", "eu"]),
        in_folder("db", &["prod", "eu"]),
        in_folder("cache", &["prod"]),
        in_folder("lab", &[]),
        // Not a label: the host stays in the folder above it.
        in_folder("odd", &["prod", " "]),
    ]);
    let (report, inventory) = book.import(&source).await.unwrap();
    assert_eq!(report.added.len(), 5, "{:?}", report.skipped);
    let path = |label: &str| {
        let host = inventory
            .hosts()
            .find(|h| h.label.as_str() == label)
            .unwrap();
        let mut names = Vec::new();
        let mut at = host.group.clone();
        while let Some(id) = at {
            let group = inventory.groups().find(|g| g.id == id).unwrap();
            names.insert(0, group.name.as_str().to_owned());
            at = group.parent.clone();
        }
        names.join("/")
    };
    assert_eq!(path("web"), "PuTTY/prod/eu");
    assert_eq!(path("db"), "PuTTY/prod/eu");
    assert_eq!(path("cache"), "PuTTY/prod");
    assert_eq!(path("lab"), "PuTTY");
    assert_eq!(path("odd"), "PuTTY/prod");
    let groups = inventory.groups().count();
    assert_eq!(groups, 3, "one group per folder, made once");

    // Again, with a new host in a known folder: the groups are reused.
    let again = Putty(vec![in_folder("api", &["prod", "eu"])]);
    let (report, inventory) = book.import(&again).await.unwrap();
    assert_eq!(report.added.len(), 1);
    assert_eq!(inventory.groups().count(), groups);
    assert_eq!(path("web"), "PuTTY/prod/eu");
}

#[tokio::test]
async fn a_source_that_cannot_be_read_is_named_in_the_error() {
    let (_, book) = book().await;
    let error = book.import(&Unreadable).await.err().unwrap();
    assert_eq!(error.to_string(), "could not read sessions.ini: bad file");
}

fn depth_of(inventory: &Inventory, label: &str) -> usize {
    let host = inventory
        .hosts()
        .find(|h| h.label.as_str() == label)
        .unwrap();
    let mut depth = 0;
    let mut at = host.group.clone();
    while let Some(id) = at {
        depth += 1;
        at = inventory
            .groups()
            .find(|g| g.id == id)
            .unwrap()
            .parent
            .clone();
    }
    depth
}

#[tokio::test]
async fn deep_folders_stop_at_the_deepest_group_allowed() {
    let (_, book) = book().await;
    let folder: Vec<String> = (0..20).map(|i| format!("f{i}")).collect();
    let source = Putty(vec![ConfigHost {
        folder,
        ..entry("deep", "deep.internal")
    }]);
    let (report, inventory) = book.import(&source).await.unwrap();
    assert_eq!(report.added.len(), 1, "{:?}", report.skipped);
    assert_eq!(depth_of(&inventory, "deep"), 16);
}

#[tokio::test]
async fn a_full_group_list_keeps_every_host_in_the_deepest_group_made() {
    let (_, book) = book().await;
    let hosts: Vec<ConfigHost> = (0..1001)
        .map(|i| ConfigHost {
            folder: vec![format!("f{i}")],
            ..entry(&format!("h{i}"), &format!("h{i}.internal"))
        })
        .collect();
    let (report, inventory) = book.import(&Putty(hosts)).await.unwrap();
    assert_eq!(report.added.len(), 1001, "{:?}", report.skipped.first());
    assert_eq!(inventory.groups().count(), 1000);
    // The last host found no room for its folder: it is in PuTTY itself.
    assert_eq!(depth_of(&inventory, "h1000"), 1);
}

#[tokio::test]
async fn a_folder_named_like_another_import_s_group_is_not_taken_for_it() {
    let (_, book) = book().await;
    let foreign = Putty(vec![ConfigHost {
        folder: vec![IMPORT_GROUP.to_owned()],
        ..entry("a", "a.internal")
    }]);
    book.import(&foreign).await.unwrap();
    let (_, inventory) = book
        .import(&Config(vec![entry("b", "b.internal")]))
        .await
        .unwrap();
    let b = inventory.hosts().find(|h| h.label.as_str() == "b").unwrap();
    let group = inventory
        .groups()
        .find(|g| Some(&g.id) == b.group.as_ref())
        .unwrap();
    assert_eq!(group.name.as_str(), IMPORT_GROUP);
    assert_eq!(
        group.parent, None,
        "the top-level group, not PuTTY's folder"
    );
}

#[tokio::test]
async fn a_skipped_host_makes_no_group() {
    let (_, book) = book().await;
    let source = Putty(vec![ConfigHost {
        folder: vec!["Empty".into()],
        ..entry("bad", "not a host!")
    }]);
    let (report, inventory) = book.import(&source).await.unwrap();
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(inventory.groups().count(), 0);
}

#[tokio::test]
async fn a_full_host_list_makes_no_group_for_hosts_it_skips() {
    let (_, book) = book().await;
    let fill: Vec<ConfigHost> = (0..MAX_HOSTS - 1)
        .map(|i| entry(&format!("h{i}"), "h.internal"))
        .collect();
    book.import(&Config(fill)).await.unwrap();
    let in_folder = |alias: &str, folder: &[&str]| ConfigHost {
        folder: folder.iter().map(|f| (*f).to_owned()).collect(),
        ..entry(alias, "p.internal")
    };
    let (report, inventory) = book
        .import(&Putty(vec![
            in_folder("p1", &["A"]),
            in_folder("p2", &["B", "C"]),
        ]))
        .await
        .unwrap();
    assert_eq!(report.added.len(), 1);
    assert_eq!(
        report.skipped,
        [("p2".to_owned(), "the host list is full".to_owned())]
    );
    let names: Vec<&str> = inventory.groups().map(|g| g.name.as_str()).collect();
    assert!(!names.contains(&"B") && !names.contains(&"C"), "{names:?}");
}
