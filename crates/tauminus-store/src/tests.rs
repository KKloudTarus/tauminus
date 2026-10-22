// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::{AtomicUsize, Ordering};

use tauminus_domain::{
    AuthMethod, Group, GroupId, GuardSettings, Hop, Host, HostId, HostName, Label, Pattern, Route,
    Settings, Tag, UserName,
};

use super::*;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("tauminus-store-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }

    fn store(&self) -> JsonHostStore {
        JsonHostStore::new(self.0.join("hosts.json"))
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rich() -> Inventory {
    let mut prod = Group {
        id: GroupId::parse("prod").unwrap(),
        name: Label::parse("Production").unwrap(),
        parent: None,
        settings: Settings::default(),
    };
    prod.settings.user = Some(UserName::parse("ops").unwrap());
    prod.settings.jumps = Some(vec![Hop::parse("ops@[2001:db8::1]:2222").unwrap()]);
    prod.settings.methods = Some(vec![AuthMethod::Agent, AuthMethod::Password]);
    prod.settings.forward_agent = Some(false);
    prod.settings.env = tauminus_domain::env_list(vec![
        tauminus_domain::EnvVar::parse("LANG", "C.UTF-8").unwrap(),
        tauminus_domain::EnvVar::parse("TEAM", "ops\tdb").unwrap(),
    ])
    .unwrap();
    prod.settings.guard = GuardSettings::new(
        Some(true),
        vec![Pattern::parse("rm -rf /tmp/*").unwrap()],
        vec![Pattern::parse("systemctl stop *").unwrap()],
    )
    .unwrap();
    let eu = Group {
        id: GroupId::parse("eu").unwrap(),
        name: Label::parse("EU 🇪🇺").unwrap(),
        parent: Some(prod.id.clone()),
        settings: Settings {
            route: Some(Route::Socks5 {
                host: HostName::parse("proxy.eu").unwrap(),
                port: 1080,
            }),
            ..Settings::default()
        },
    };
    let mut db = Host::new(
        HostId::parse("db1").unwrap(),
        Label::parse("Máy chủ DB").unwrap(),
        HostName::parse("db1.eu.example.com").unwrap(),
    );
    db.group = Some(eu.id.clone());
    db.set_tags(vec![Tag::parse("db").unwrap(), Tag::parse("prod").unwrap()])
        .unwrap();
    db.settings.port = Some(2200);
    db.settings.identity_files = Some(vec!["~/.ssh/id_db".into()]);
    db.settings.identities_only = Some(true);
    db.settings.jumps = Some(vec![]);
    db.settings.history = Some(false);
    let mut legacy = Host::new(
        HostId::parse("legacy").unwrap(),
        Label::parse("legacy").unwrap(),
        HostName::parse("10.0.0.5").unwrap(),
    );
    legacy.settings.route = Some(Route::ProxyCommand("nc -X connect %h %p".into()));
    Inventory::new(vec![prod, eu], vec![db, legacy]).unwrap()
}

#[tokio::test]
async fn what_is_saved_loads_back_the_same() {
    let dir = Dir::new();
    let store = dir.store();
    let inventory = rich();
    store.save(&inventory).await.unwrap();
    assert_eq!(store.load().await.unwrap(), inventory);
    // And a second save of the loaded copy writes the same bytes.
    let first = std::fs::read(dir.0.join("hosts.json")).unwrap();
    let loaded = store.load().await.unwrap();
    store.save(&loaded).await.unwrap();
    assert_eq!(std::fs::read(dir.0.join("hosts.json")).unwrap(), first);
}

#[tokio::test]
async fn nothing_saved_yet_is_an_empty_list() {
    let dir = Dir::new();
    assert_eq!(dir.store().load().await.unwrap(), Inventory::default());
}

#[tokio::test]
async fn a_damaged_or_newer_file_is_reported_and_kept() {
    let dir = Dir::new();
    std::fs::create_dir_all(&dir.0).unwrap();
    let path = dir.0.join("hosts.json");
    for (label, contents) in [
        ("not json", "{ hosts: ".to_owned()),
        ("newer", r#"{"version": 2, "hosts": []}"#.to_owned()),
        (
            "unknown field",
            r#"{"version": 1, "hosts": [], "vault": {}}"#.to_owned(),
        ),
        (
            "bad address",
            r#"{"version": 1, "hosts": [{"id": "a", "label": "a", "address": "a;b"}]}"#.to_owned(),
        ),
        (
            "bad guard pattern",
            r#"{"version": 1, "hosts": [{"id": "a", "label": "a", "address": "a", "settings": {"guard": {"deny": [" "]}}}]}"#
                .to_owned(),
        ),
        (
            "unknown guard field",
            r#"{"version": 1, "groups": [{"id": "g", "name": "g", "settings": {"guard": {"rules": []}}}]}"#
                .to_owned(),
        ),
        (
            "bad environment name",
            r#"{"version": 1, "hosts": [{"id": "a", "label": "a", "address": "a", "settings": {"env": [{"name": "1X", "value": "v"}]}}]}"#
                .to_owned(),
        ),
        (
            "repeated environment name",
            r#"{"version": 1, "hosts": [{"id": "a", "label": "a", "address": "a", "settings": {"env": [{"name": "X", "value": "1"}, {"name": "X", "value": "2"}]}}]}"#
                .to_owned(),
        ),
        (
            "dangling group",
            r#"{"version": 1, "hosts": [{"id": "a", "label": "a", "address": "a", "group": "g"}]}"#
                .to_owned(),
        ),
    ] {
        std::fs::write(&path, &contents).unwrap();
        let result = dir.store().load().await;
        assert!(
            matches!(result, Err(StoreError::Corrupt(_))),
            "{label}: {result:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            contents,
            "{label}: untouched"
        );
    }
}

#[tokio::test]
async fn an_oversized_file_is_refused_before_it_is_read() {
    let dir = Dir::new();
    std::fs::create_dir_all(&dir.0).unwrap();
    let file = std::fs::File::create(dir.0.join("hosts.json")).unwrap();
    file.set_len(MAX_FILE + 1).unwrap();
    assert!(matches!(
        dir.store().load().await,
        Err(StoreError::Corrupt(_))
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn the_file_is_private_to_the_user() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Dir::new();
    dir.store().save(&rich()).await.unwrap();
    let mode = std::fs::metadata(dir.0.join("hosts.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_write_leaves_the_old_file_and_no_temp_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Dir::new();
    let store = dir.store();
    store.save(&Inventory::default()).await.unwrap();
    let before = std::fs::read(dir.0.join("hosts.json")).unwrap();
    // A directory the store cannot write into makes the write fail.
    std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = store.save(&rich()).await;
    std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(result, Err(StoreError::Io(_))), "{result:?}");
    assert_eq!(std::fs::read(dir.0.join("hosts.json")).unwrap(), before);
    let leftovers: Vec<_> = std::fs::read_dir(&dir.0)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
}

#[tokio::test]
async fn a_change_made_elsewhere_is_not_overwritten() {
    let dir = Dir::new();
    let first = dir.store();
    let second = dir.store();
    first.load().await.unwrap();
    second.load().await.unwrap();
    first.save(&rich()).await.unwrap();
    // The second window still holds the empty list it read.
    let result = second.save(&Inventory::default()).await;
    assert!(matches!(result, Err(StoreError::Conflict)), "{result:?}");
    assert_eq!(
        second.load().await.unwrap(),
        rich(),
        "the first window's change survives"
    );
    // After reloading, the second window may save again.
    second.save(&Inventory::default()).await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn the_directory_is_private_too() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Dir::new();
    let store = JsonHostStore::new(dir.0.join("nested/hosts.json"));
    store.save(&rich()).await.unwrap();
    let mode = std::fs::metadata(dir.0.join("nested"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[test]
fn new_ids_are_valid_and_distinct() {
    let store = JsonHostStore::new(PathBuf::from("unused"));
    let ids: std::collections::HashSet<String> = (0..1000).map(|_| store.new_id()).collect();
    assert_eq!(ids.len(), 1000);
    assert!(
        ids.iter()
            .all(|id| HostId::parse(id).is_ok() && id.len() == 32)
    );
}

#[tokio::test]
async fn a_list_too_large_to_load_again_is_not_saved() {
    let dir = Dir::new();
    let store = dir.store();
    store.save(&rich()).await.unwrap();
    let before = std::fs::read(dir.0.join("hosts.json")).unwrap();
    // Every field at its limit: about 2 KB per host, so 10 000 of them
    // exceed what `load` accepts.
    let tags: Vec<Tag> = (0..tauminus_domain::MAX_TAGS)
        .map(|t| Tag::parse(&format!("{t:02}{}", "t".repeat(30))).unwrap())
        .collect();
    let hosts: Vec<Host> = (0..tauminus_domain::MAX_HOSTS)
        .map(|i| {
            let mut h = Host::new(
                HostId::parse(&format!("{i:064}")).unwrap(),
                Label::parse(&format!("{i:0128}")).unwrap(),
                HostName::parse(&format!("{}.example.com", "a".repeat(60))).unwrap(),
            );
            h.set_tags(tags.clone()).unwrap();
            h
        })
        .collect();
    let huge = Inventory::new(vec![], hosts).unwrap();
    assert!(matches!(store.save(&huge).await, Err(StoreError::Io(_))));
    assert_eq!(std::fs::read(dir.0.join("hosts.json")).unwrap(), before);
}
