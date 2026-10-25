// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::{AtomicUsize, Ordering};

use tauminus_domain::{
    AuthMethod, Group, GroupId, GuardSettings, Hop, Host, HostId, HostName, IdentityAgent, Label,
    Pattern, Route, Settings, Tag, UserName,
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
async fn forwarding_rules_load_back_and_only_they_raise_the_version() {
    use tauminus_domain::{Bind, BindAddress, Destination, Forward, ForwardId, ForwardRule};
    let dir = Dir::new();
    let store = dir.store();
    let version = || {
        let bytes = std::fs::read(dir.0.join("hosts.json")).unwrap();
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["version"].clone()
    };
    let mut inventory = rich();
    store.save(&inventory).await.unwrap();
    assert_eq!(
        version(),
        1,
        "a file with no rules stays readable by older builds"
    );
    let mut host = inventory
        .host(&HostId::parse("db1").unwrap())
        .unwrap()
        .clone();
    let bind = |address, port| Bind { address, port };
    let to = |host: &str, port| Destination::new(HostName::parse(host).unwrap(), port).unwrap();
    let rule = |id: &str, forward, auto_start| ForwardRule {
        id: ForwardId::parse(id).unwrap(),
        label: Label::parse(id).unwrap(),
        forward,
        auto_start,
    };
    host.forwards = vec![
        rule(
            "pg",
            Forward::Local {
                bind: bind(BindAddress::Loopback, 5432),
                to: to("db.internal", 5432),
            },
            true,
        ),
        rule(
            "web",
            Forward::Remote {
                bind: bind(BindAddress::Any, 0),
                to: to("::1", 8080),
            },
            false,
        ),
        rule(
            "socks",
            Forward::Dynamic {
                bind: bind(BindAddress::Ip("192.168.1.5".parse().unwrap()), 1080),
            },
            false,
        ),
    ];
    inventory.put_host(host).unwrap();
    store.save(&inventory).await.unwrap();
    assert_eq!(version(), 2);
    assert_eq!(store.load().await.unwrap(), inventory);
}

#[tokio::test]
async fn startup_snippets_load_back_and_raise_the_version_to_3() {
    use tauminus_domain::SnippetId;
    let dir = Dir::new();
    let store = dir.store();
    let mut inventory = rich();
    let mut host = inventory
        .host(&HostId::parse("db1").unwrap())
        .unwrap()
        .clone();
    host.startup = vec![
        SnippetId::parse("tmux").unwrap(),
        SnippetId::parse("env").unwrap(),
    ];
    inventory.put_host(host).unwrap();
    store.save(&inventory).await.unwrap();
    let bytes = std::fs::read(dir.0.join("hosts.json")).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["version"], 3);
    assert_eq!(store.load().await.unwrap(), inventory);
}

#[tokio::test]
async fn an_encoding_loads_back_and_raises_the_version_to_4() {
    use tauminus_domain::Encoding;
    let dir = Dir::new();
    let store = dir.store();
    let mut inventory = rich();
    let mut host = inventory
        .host(&HostId::parse("db1").unwrap())
        .unwrap()
        .clone();
    host.settings.encoding = Some(Encoding::EucJp);
    inventory.put_host(host).unwrap();
    store.save(&inventory).await.unwrap();
    let bytes = std::fs::read(dir.0.join("hosts.json")).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["version"], 4);
    assert!(String::from_utf8_lossy(&bytes).contains("\"encoding\": \"EUC-JP\""));
    assert_eq!(store.load().await.unwrap(), inventory);
}

#[tokio::test]
async fn an_encoding_set_on_a_group_alone_raises_the_version_too() {
    use tauminus_domain::Encoding;
    let dir = Dir::new();
    let store = dir.store();
    let mut inventory = rich();
    let mut prod = inventory
        .groups()
        .find(|g| g.id.as_str() == "prod")
        .unwrap()
        .clone();
    prod.settings.encoding = Some(Encoding::Koi8R);
    inventory.put_group(prod).unwrap();
    store.save(&inventory).await.unwrap();
    let bytes = std::fs::read(dir.0.join("hosts.json")).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["version"], 4);
    assert_eq!(store.load().await.unwrap(), inventory);
}

#[tokio::test]
async fn a_telnet_host_loads_back_and_raises_the_version_to_5() {
    use tauminus_domain::Protocol;
    let dir = Dir::new();
    let store = dir.store();
    let mut inventory = rich();
    let mut host = inventory
        .host(&HostId::parse("db1").unwrap())
        .unwrap()
        .clone();
    host.protocol = Protocol::Telnet;
    inventory.put_host(host).unwrap();
    store.save(&inventory).await.unwrap();
    let bytes = std::fs::read(dir.0.join("hosts.json")).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["version"], 5);
    assert_eq!(store.load().await.unwrap(), inventory);
}

#[tokio::test]
async fn a_tmux_session_and_one_turned_off_load_back_at_version_6() {
    use tauminus_domain::{Tmux, TmuxSession};
    let dir = Dir::new();
    let store = dir.store();
    let mut inventory = rich();
    let mut prod = inventory
        .groups()
        .find(|g| g.id.as_str() == "prod")
        .unwrap()
        .clone();
    prod.settings.tmux = Some(Tmux::Join(TmuxSession::parse("work").unwrap()));
    inventory.put_group(prod).unwrap();
    let mut host = inventory
        .host(&HostId::parse("db1").unwrap())
        .unwrap()
        .clone();
    host.settings.tmux = Some(Tmux::Off);
    inventory.put_host(host).unwrap();
    store.save(&inventory).await.unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.0.join("hosts.json")).unwrap()).unwrap();
    assert_eq!(json["version"], 6);
    assert_eq!(store.load().await.unwrap(), inventory);
}

#[tokio::test]
async fn highlight_sets_load_back_in_order_at_version_8() {
    use tauminus_domain::TriggerSetId;
    let dir = Dir::new();
    let store = dir.store();
    let mut inventory = rich();
    let mut prod = inventory
        .groups()
        .find(|g| g.id.as_str() == "prod")
        .unwrap()
        .clone();
    prod.settings.triggers = vec![
        TriggerSetId::parse("errors").unwrap(),
        TriggerSetId::parse("logs").unwrap(),
    ];
    inventory.put_group(prod).unwrap();
    store.save(&inventory).await.unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.0.join("hosts.json")).unwrap()).unwrap();
    assert_eq!(json["version"], 8);
    assert_eq!(store.load().await.unwrap(), inventory);
}

#[tokio::test]
async fn the_ai_s_leave_to_read_output_loads_back_at_version_9() {
    let dir = Dir::new();
    let store = dir.store();
    let mut inventory = rich();
    let mut prod = inventory
        .groups()
        .find(|g| g.id.as_str() == "prod")
        .unwrap()
        .clone();
    prod.settings.ai_output = Some(true);
    inventory.put_group(prod).unwrap();
    store.save(&inventory).await.unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.0.join("hosts.json")).unwrap()).unwrap();
    assert_eq!(json["version"], 9);
    assert!(
        json["groups"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["settings"]["aiOutput"] == true)
    );
    assert_eq!(store.load().await.unwrap(), inventory);
}

#[tokio::test]
async fn tmux_tabs_load_back_at_version_10() {
    let dir = Dir::new();
    let store = dir.store();
    let mut inventory = rich();
    let mut prod = inventory
        .groups()
        .find(|g| g.id.as_str() == "prod")
        .unwrap()
        .clone();
    prod.settings.tmux_tabs = Some(true);
    inventory.put_group(prod).unwrap();
    store.save(&inventory).await.unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.0.join("hosts.json")).unwrap()).unwrap();
    assert_eq!(json["version"], 10);
    assert_eq!(store.load().await.unwrap(), inventory);
}

#[tokio::test]
async fn an_identity_agent_loads_back_at_version_11() {
    let dir = Dir::new();
    let store = dir.store();
    let mut inventory = rich();
    let mut prod = inventory
        .groups()
        .find(|g| g.id.as_str() == "prod")
        .unwrap()
        .clone();
    prod.settings.identity_agent = Some(IdentityAgent::Path("~/secretive.ssh".into()));
    inventory.put_group(prod).unwrap();
    store.save(&inventory).await.unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.0.join("hosts.json")).unwrap()).unwrap();
    assert_eq!(json["version"], 11);
    assert_eq!(store.load().await.unwrap(), inventory);
    // `none` and `SSH_AUTH_SOCK` keep their meaning.
    for agent in [IdentityAgent::None, IdentityAgent::Environment] {
        let mut inventory = rich();
        let mut prod = inventory
            .groups()
            .find(|g| g.id.as_str() == "prod")
            .unwrap()
            .clone();
        prod.settings.identity_agent = Some(agent);
        inventory.put_group(prod).unwrap();
        store.save(&inventory).await.unwrap();
        assert_eq!(store.load().await.unwrap(), inventory);
    }
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
        ("newer", r#"{"version": 12, "hosts": []}"#.to_owned()),
        (
            "highlight set id",
            r#"{"version": 8, "hosts": [{"id": "a", "label": "a", "address": "a", "settings": {"triggers": ["no spaces"]}}]}"#.to_owned(),
        ),
        (
            "a highlight set twice",
            r#"{"version": 8, "hosts": [{"id": "a", "label": "a", "address": "a", "settings": {"triggers": ["x", "x"]}}]}"#.to_owned(),
        ),
        (
            "tmux name",
            r#"{"version": 6, "hosts": [{"id": "a", "label": "a", "address": "a", "settings": {"tmux": "a b"}}]}"#.to_owned(),
        ),
        (
            "unknown protocol",
            r#"{"version": 5, "hosts": [{"id": "a", "label": "a", "address": "a", "protocol": "rlogin"}]}"#.to_owned(),
        ),
        (
            "unknown encoding",
            r#"{"version": 4, "hosts": [{"id": "a", "label": "a", "address": "a", "settings": {"encoding": "UTF-7"}}]}"#.to_owned(),
        ),
        (
            "startup snippet id",
            r#"{"version": 3, "hosts": [{"id": "a", "label": "a", "address": "a", "startup": ["no spaces"]}]}"#.to_owned(),
        ),
        (
            "startup snippet twice",
            r#"{"version": 3, "hosts": [{"id": "a", "label": "a", "address": "a", "startup": ["s", "s"]}]}"#.to_owned(),
        ),
        (
            "dynamic rule with a destination",
            r#"{"version": 2, "hosts": [{"id": "a", "label": "a", "address": "a", "forwards": [{"id": "f", "label": "f", "kind": "dynamic", "bind": "loopback", "port": 1080, "toHost": "x", "toPort": 1}]}]}"#.to_owned(),
        ),
        (
            "local rule without a destination",
            r#"{"version": 2, "hosts": [{"id": "a", "label": "a", "address": "a", "forwards": [{"id": "f", "label": "f", "kind": "local", "bind": "loopback", "port": 1}]}]}"#.to_owned(),
        ),
        (
            "bad bind address",
            r#"{"version": 2, "hosts": [{"id": "a", "label": "a", "address": "a", "forwards": [{"id": "f", "label": "f", "kind": "dynamic", "bind": "lan", "port": 1}]}]}"#.to_owned(),
        ),
        (
            "rule listed twice",
            r#"{"version": 2, "hosts": [{"id": "a", "label": "a", "address": "a", "forwards": [{"id": "f", "label": "f", "kind": "dynamic", "bind": "any", "port": 1}, {"id": "f", "label": "g", "kind": "dynamic", "bind": "any", "port": 2}]}]}"#.to_owned(),
        ),
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

/// A right-to-left mark, as Hebrew and Arabic input methods type, in a
/// saved label: it loads as it was saved.
#[tokio::test]
async fn a_label_with_a_direction_mark_loads() {
    let dir = Dir::new();
    std::fs::create_dir_all(&dir.0).unwrap();
    std::fs::write(
        dir.0.join("hosts.json"),
        "{\"version\": 1, \"hosts\": [{\"id\": \"a\", \"label\": \"\u{5e9}\u{5e8}\u{5ea}\u{200f} 1\", \"address\": \"a\"}]}",
    )
    .unwrap();
    let inventory = dir.store().load().await.unwrap();
    assert_eq!(inventory.hosts().count(), 1);
}

#[tokio::test]
async fn a_relative_identity_agent_is_reported_and_kept() {
    let dir = Dir::new();
    std::fs::create_dir_all(&dir.0).unwrap();
    let path = dir.0.join("hosts.json");
    let contents = r#"{"version": 11, "hosts": [{"id": "a", "label": "a", "address": "a", "settings": {"identityAgent": "agent.sock"}}]}"#;
    std::fs::write(&path, contents).unwrap();
    let result = dir.store().load().await;
    assert!(matches!(result, Err(StoreError::Corrupt(_))), "{result:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
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

mod snippet_file {
    use tauminus_app::ports::{SnippetStore, StoreError};
    use tauminus_domain::{Label, Library, Snippet, SnippetId};

    use super::Dir;
    use crate::JsonSnippetStore;

    fn snippet(id: &str, folder: Option<&str>, body: &str) -> Snippet {
        Snippet::new(
            SnippetId::parse(id).unwrap(),
            Label::parse(id).unwrap(),
            folder.map(|f| Label::parse(f).unwrap()),
            body.to_owned(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn a_library_loads_back_as_saved() {
        let dir = Dir::new();
        let store = JsonSnippetStore::new(dir.0.join("snippets.json"));
        assert_eq!(store.load().await.unwrap(), Library::default());
        let library = Library::new(vec![
            snippet("logs", Some("Ops"), "journalctl -u {{unit:nginx}} -f"),
            snippet("disk", None, "df -h\nfree -m\n"),
        ])
        .unwrap();
        store.save(&library).await.unwrap();
        assert_eq!(store.load().await.unwrap(), library);
        assert_eq!(store.new_id().len(), 32);
    }

    #[tokio::test]
    async fn a_bad_or_newer_file_is_reported_and_kept() {
        let dir = Dir::new();
        std::fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("snippets.json");
        for (label, contents) in [
            ("newer", r#"{"version": 2}"#),
            (
                "blank body",
                r#"{"version": 1, "snippets": [{"id": "a", "name": "a", "body": "  "}]}"#,
            ),
            (
                "twice",
                r#"{"version": 1, "snippets": [{"id": "a", "name": "a", "body": "x"}, {"id": "a", "name": "b", "body": "y"}]}"#,
            ),
            (
                "unknown field",
                r#"{"version": 1, "snippets": [{"id": "a", "name": "a", "body": "x", "run": true}]}"#,
            ),
        ] {
            std::fs::write(&path, contents).unwrap();
            let store = JsonSnippetStore::new(path.clone());
            assert!(
                matches!(store.load().await, Err(StoreError::Corrupt(_))),
                "{label}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), contents, "{label}");
        }
    }

    #[tokio::test]
    async fn a_change_from_another_window_is_not_overwritten() {
        let dir = Dir::new();
        let path = dir.0.join("snippets.json");
        let one = JsonSnippetStore::new(path.clone());
        let two = JsonSnippetStore::new(path);
        one.load().await.unwrap();
        two.load().await.unwrap();
        let library = Library::new(vec![snippet("a", None, "true")]).unwrap();
        one.save(&library).await.unwrap();
        assert!(matches!(
            two.save(&Library::default()).await,
            Err(StoreError::Conflict)
        ));
    }
}

#[tokio::test]
async fn a_mosh_host_loads_back_at_version_7() {
    use tauminus_domain::Protocol;
    let dir = Dir::new();
    let store = dir.store();
    let mut inventory = rich();
    let mut host = inventory
        .host(&HostId::parse("db1").unwrap())
        .unwrap()
        .clone();
    host.protocol = Protocol::Mosh;
    inventory.put_host(host).unwrap();
    store.save(&inventory).await.unwrap();
    let bytes = std::fs::read(dir.0.join("hosts.json")).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["version"], 7);
    assert_eq!(store.load().await.unwrap(), inventory);
}

#[tokio::test]
async fn the_review_list_is_parsed_again_only_when_its_bytes_change() {
    let dir = Dir::new();
    let path = dir.0.join("sync-review.json");
    let here = JsonTrustStore::new(path.clone());
    let other = JsonTrustStore::new(path);
    let (empty, version) = here.load().await.unwrap();
    assert!(empty.is_empty());
    let mut notes = Approvals::new();
    notes.insert(HostId::parse("h").unwrap(), None);
    let (_, theirs) = other.load().await.unwrap();
    other.save(&notes, theirs).await.unwrap();
    // Saving what was read before the other window's save is refused,
    // whatever was loaded since.
    let (first, now) = here.load().await.unwrap();
    assert_eq!(*first, notes, "another window's save is seen");
    assert!(matches!(
        here.save(&Approvals::new(), version).await,
        Err(StoreError::Conflict)
    ));
    let (again, _) = here.load().await.unwrap();
    assert!(Arc::ptr_eq(&first, &again), "same bytes, no parse");
    here.save(&Approvals::new(), now).await.unwrap();
    assert!(here.load().await.unwrap().0.is_empty());
}
