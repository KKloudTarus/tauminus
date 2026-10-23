// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tauminus_app::ports::{BoxFuture, HostStore, StoreError};

use super::*;
use tauminus_domain::{MAX_ENV, MAX_LINE};

#[derive(Default)]
struct MemoryStore {
    saved: Mutex<Inventory>,
    next: AtomicUsize,
}

impl HostStore for MemoryStore {
    fn load(&self) -> BoxFuture<'_, Result<Inventory, StoreError>> {
        let inventory = self.saved.lock().unwrap().clone();
        Box::pin(async move { Ok(inventory) })
    }

    fn save<'a>(&'a self, inventory: &'a Inventory) -> BoxFuture<'a, Result<(), StoreError>> {
        *self.saved.lock().unwrap() = inventory.clone();
        Box::pin(async { Ok(()) })
    }

    fn new_id(&self) -> String {
        format!("id{}", self.next.fetch_add(1, Ordering::Relaxed))
    }
}

async fn book() -> HostBook {
    HostBook::open(Arc::new(MemoryStore::default()))
        .await
        .unwrap()
}

fn input(label: &str, address: &str) -> HostInput {
    HostInput {
        id: None,
        label: label.into(),
        address: address.into(),
        group: None,
        tags: vec![],
        settings: SettingsSpec::default(),
        forwards: None,
        startup: None,
        protocol: ProtocolSpec::Ssh,
    }
}

#[tokio::test]
async fn a_saved_host_lists_with_its_resolved_target() {
    let book = book().await;
    let group = parse_group(
        &book,
        GroupInput {
            id: None,
            name: "prod".into(),
            parent: None,
            settings: SettingsSpec {
                user: Some("ops".into()),
                jumps: Some(vec!["bastion:2222".into()]),
                ..SettingsSpec::default()
            },
        },
    )
    .unwrap();
    let group_id = group.id.to_string();
    book.edit(|i| i.put_group(group)).await.unwrap();
    let mut db = input("DB", " db.internal ");
    db.group = Some(group_id.clone());
    db.tags = vec!["Prod".into(), "db".into()];
    db.settings.port = Some(2200);
    let host = parse_host(&book, db).unwrap();
    let listed = view(&book.edit(|i| i.put_host(host)).await.unwrap());

    let db = &listed.hosts[0];
    assert_eq!(db.address, "db.internal");
    assert_eq!(db.tags, ["db", "prod"]);
    assert_eq!(db.group.as_deref(), Some(group_id.as_str()));
    let target = db.target.as_ref().expect("resolved");
    assert_eq!(target.destination.user, "ops");
    assert_eq!(target.destination.port, 2200);
    assert_eq!(target.jumps.len(), 1);
    assert_eq!(target.jumps[0].port, 2222);
    assert!(target.methods.is_some(), "defaults are spelled out");
    assert_eq!(db.problem, None);
}

#[test]
fn a_stored_proxy_command_may_be_kept_but_not_added_or_changed() {
    let command = |c: &str| Settings {
        route: Some(Route::ProxyCommand(c.into())),
        ..Settings::default()
    };
    let imported = command("nc %h %p");
    assert!(
        keep_proxy_command(&imported, Some(&imported)).is_ok(),
        "unchanged"
    );
    assert!(
        keep_proxy_command(&command("id"), Some(&imported)).is_err(),
        "changed"
    );
    assert!(
        keep_proxy_command(&imported, None).is_err(),
        "added to a new host"
    );
    assert!(
        keep_proxy_command(&imported, Some(&Settings::default())).is_err(),
        "added later"
    );
    assert!(
        keep_proxy_command(&Settings::default(), Some(&imported)).is_ok(),
        "removed"
    );
}

#[tokio::test]
async fn invalid_input_is_refused_before_anything_is_saved() {
    let book = book().await;
    let cases = [
        input("", "db"),
        input("db", "db;rm"),
        HostInput {
            settings: SettingsSpec {
                port: Some(0),
                ..SettingsSpec::default()
            },
            ..input("db", "db")
        },
        HostInput {
            settings: SettingsSpec {
                jumps: Some(vec!["-oProxyCommand=x".into()]),
                ..SettingsSpec::default()
            },
            ..input("db", "db")
        },
        HostInput {
            settings: SettingsSpec {
                methods: Some(vec![]),
                ..SettingsSpec::default()
            },
            ..input("db", "db")
        },
        HostInput {
            tags: vec!["two words".into()],
            ..input("db", "db")
        },
        HostInput {
            id: Some("../etc".into()),
            ..input("db", "db")
        },
    ];
    for (index, case) in cases.into_iter().enumerate() {
        let error = parse_host(&book, case)
            .err()
            .unwrap_or_else(|| panic!("case {index} accepted"));
        assert_eq!(error.code, ErrorCode::InvalidArgument, "case {index}");
    }
}

#[tokio::test]
async fn settings_survive_a_round_trip_through_the_view() {
    let book = book().await;
    let spec = SettingsSpec {
        user: Some("deploy".into()),
        port: Some(2222),
        jumps: Some(vec!["ops@[2001:db8::1]:22".into(), "edge".into()]),
        route: Some(RouteSpec::Socks5 {
            host: "proxy".into(),
            port: 1080,
        }),
        methods: Some(vec![AuthMethodSpec::Agent, AuthMethodSpec::Password]),
        identity_files: Some(vec!["~/.ssh/id_work".into()]),
        identities_only: Some(true),
        guard: Some(GuardSpec {
            enabled: Some(true),
            allow: vec!["rm -rf /tmp/*".into()],
            deny: vec!["systemctl stop *".into()],
        }),
        forward_agent: Some(true),
        history: Some(false),
        identity: Some("work-1".into()),
        encoding: Some(EncodingSpec::EucKr),
        env: Some(vec![
            EnvSpec {
                name: "LANG".into(),
                value: "C.UTF-8".into(),
            },
            EnvSpec {
                name: "TEAM".into(),
                value: "ops".into(),
            },
        ]),
    };
    let parsed = settings(spec.clone()).unwrap();
    let back = settings_spec(&parsed);
    assert_eq!(
        serde_json::to_value(&back).unwrap(),
        serde_json::to_value(&spec).unwrap()
    );
    // An empty user means inherit.
    let inherit = settings(SettingsSpec {
        user: Some(String::new()),
        ..SettingsSpec::default()
    })
    .unwrap();
    assert_eq!(inherit.user, None);
    let _ = book;
}

#[test]
fn an_unavailable_store_is_reported_on_every_call() {
    let hosts = Hosts::Unavailable("hosts.json is unreadable".into());
    let error = hosts.book().err().unwrap();
    assert_eq!(error.code, ErrorCode::StoreFailed);
}

#[tokio::test]
async fn prod_hosts_are_guarded_and_their_commands_checked() {
    let book = book().await;
    let mut prod = parse_host(
        &book,
        HostInput {
            tags: vec!["prod".into()],
            ..input("db", "db")
        },
    )
    .unwrap();
    prod.id = HostId::parse("db").unwrap();
    let mut dev = parse_host(&book, input("dev", "dev")).unwrap();
    dev.id = HostId::parse("dev").unwrap();
    let inventory = book
        .edit(|inventory| {
            inventory.put_host(prod)?;
            inventory.put_host(dev)
        })
        .await
        .unwrap();
    let guarded: Vec<_> = view(&inventory)
        .hosts
        .iter()
        .map(|h| (h.id.clone(), h.guarded))
        .collect();
    assert_eq!(guarded, [("db".into(), true), ("dev".into(), false)]);

    let verdict = check_lines(&book, "db", vec!["sudo rm -rf /srv".into()])
        .await
        .unwrap();
    assert_eq!(verdict.findings.len(), 1);
    assert_eq!(verdict.findings[0].rule, "recursive-delete");
    assert_eq!(verdict.findings[0].command, "rm -rf /srv");
    assert_eq!(verdict.findings[0].reason, "deletes files recursively");
    assert!(!verdict.incomplete);
    assert!(
        check_lines(&book, "db", vec!["echo 'open".into(), "ls".into()])
            .await
            .unwrap()
            .incomplete
    );
    let unguarded = check_lines(&book, "dev", vec!["rm -rf /srv".into()])
        .await
        .unwrap();
    assert!(unguarded.findings.is_empty());
    let long = check_lines(&book, "db", vec!["a".repeat(MAX_LINE + 1)])
        .await
        .unwrap();
    assert_eq!(long.findings[0].rule, "unreadable");
    // Findings from every reading count, once each.
    let merged = check_lines(
        &book,
        "db",
        vec!["ls".into(), "rm -rf /a".into(), "rm -rf /a".into()],
    )
    .await
    .unwrap();
    assert_eq!(merged.findings.len(), 1);
    assert!(
        check_lines(&book, "db", vec!["ls".into(); 9])
            .await
            .is_err()
    );
    for bad in ["nope", "../x"] {
        let error = check_lines(&book, bad, vec!["ls".into()])
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{bad}");
    }
}

#[test]
fn guard_patterns_are_validated() {
    let spec = |allow: Vec<String>| SettingsSpec {
        guard: Some(GuardSpec {
            enabled: None,
            allow,
            deny: vec![],
        }),
        ..SettingsSpec::default()
    };
    assert!(settings(spec(vec!["ls *".into()])).is_ok());
    assert!(settings(spec(vec![" ".into()])).is_err());
    assert!(settings(spec(vec!["a\u{1b}".into()])).is_err());
    let many = (0..=MAX_PATTERNS).map(|n| n.to_string()).collect();
    assert!(settings(spec(many)).is_err());
}

#[test]
fn environment_variables_are_validated() {
    let spec = |vars: &[(&str, &str)]| SettingsSpec {
        env: Some(
            vars.iter()
                .map(|(name, value)| EnvSpec {
                    name: (*name).into(),
                    value: (*value).into(),
                })
                .collect(),
        ),
        ..SettingsSpec::default()
    };
    // Sorted on the way in.
    let parsed = settings(spec(&[("B", "1"), ("A", "2")])).unwrap();
    assert_eq!(
        parsed.env.iter().map(EnvVar::name).collect::<Vec<_>>(),
        ["A", "B"]
    );
    for bad in [
        spec(&[("1A", "x")]),
        spec(&[("A", "line\nbreak")]),
        spec(&[("A", "1"), ("A", "2")]),
    ] {
        assert_eq!(settings(bad).unwrap_err().code, ErrorCode::InvalidArgument);
    }
    let many: Vec<(String, String)> = (0..=MAX_ENV)
        .map(|n| (format!("V{n}"), "x".into()))
        .collect();
    let many: Vec<(&str, &str)> = many.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
    assert!(settings(spec(&many)).is_err());
}

#[tokio::test]
async fn forwarding_rules_save_list_and_survive_an_edit_that_omits_them() {
    use crate::forwards::{ForwardKindSpec, ForwardSpec};
    let book = book().await;
    let mut db = input("DB", "db.internal");
    db.forwards = Some(vec![ForwardSpec {
        id: None,
        label: "postgres".into(),
        kind: ForwardKindSpec::Local,
        bind: "loopback".into(),
        port: 15432,
        to_host: Some("127.0.0.1".into()),
        to_port: Some(5432),
        auto_start: true,
    }]);
    let host = parse_host(&book, db).unwrap();
    let id = host.id.to_string();
    let listed = view(
        &book
            .edit(|i| {
                put_host(
                    i,
                    host,
                    Keep {
                        forwards: false,
                        startup: true,
                    },
                )
            })
            .await
            .unwrap(),
    );
    let rules = &listed.hosts[0].forwards;
    assert_eq!(rules.len(), 1);
    assert!(rules[0].id.is_some(), "a new rule gets an id");
    assert_eq!(rules[0].port, 15432);

    // An edit from a view that sends no rules keeps them.
    let mut renamed = input("DB primary", "db.internal");
    renamed.id = Some(id.clone());
    let host = parse_host(&book, renamed).unwrap();
    let listed = view(
        &book
            .edit(|i| {
                put_host(
                    i,
                    host,
                    Keep {
                        forwards: true,
                        startup: true,
                    },
                )
            })
            .await
            .unwrap(),
    );
    assert_eq!(listed.hosts[0].label, "DB primary");
    assert_eq!(listed.hosts[0].forwards.len(), 1);

    // One that sends an empty list removes them.
    let mut cleared = input("DB primary", "db.internal");
    cleared.id = Some(id);
    cleared.forwards = Some(vec![]);
    let host = parse_host(&book, cleared).unwrap();
    let listed = view(
        &book
            .edit(|i| {
                put_host(
                    i,
                    host,
                    Keep {
                        forwards: false,
                        startup: true,
                    },
                )
            })
            .await
            .unwrap(),
    );
    assert_eq!(listed.hosts[0].forwards.len(), 0);
}

#[tokio::test]
async fn startup_snippets_save_and_survive_an_edit_that_omits_them() {
    let book = book().await;
    let mut db = input("DB", "db.internal");
    db.startup = Some(vec!["tmux".into(), "env".into()]);
    let host = parse_host(&book, db).unwrap();
    let id = host.id.to_string();
    let keep = |startup| Keep {
        forwards: true,
        startup,
    };
    let listed = view(&book.edit(|i| put_host(i, host, keep(false))).await.unwrap());
    assert_eq!(listed.hosts[0].startup, ["tmux", "env"]);

    let mut renamed = input("DB primary", "db.internal");
    renamed.id = Some(id.clone());
    let host = parse_host(&book, renamed).unwrap();
    let listed = view(&book.edit(|i| put_host(i, host, keep(true))).await.unwrap());
    assert_eq!(listed.hosts[0].startup, ["tmux", "env"]);

    let mut twice = input("DB", "db.internal");
    twice.id = Some(id.clone());
    twice.startup = Some(vec!["a".into(), "a".into()]);
    let host = parse_host(&book, twice).unwrap();
    assert!(book.edit(|i| put_host(i, host, keep(false))).await.is_err());
    let mut bad = input("DB", "db.internal");
    bad.id = Some(id);
    bad.startup = Some(vec!["not an id".into()]);
    assert!(parse_host(&book, bad).is_err());
}

#[tokio::test]
async fn hosts_an_edit_points_elsewhere_are_the_moved_ones() {
    let book = book().await;
    let group = parse_group(
        &book,
        GroupInput {
            id: Some("g".into()),
            name: "g".into(),
            parent: None,
            settings: SettingsSpec::default(),
        },
    )
    .unwrap();
    book.edit(|i| i.put_group(group)).await.unwrap();
    let mut db = input("db", "db.internal");
    db.id = Some("db".into());
    db.group = Some("g".into());
    let mut web = input("web", "web.internal");
    web.id = Some("web".into());
    for host in [db, web] {
        let host = parse_host(&book, host).unwrap();
        book.edit(|i| i.put_host(host)).await.unwrap();
    }
    let before = book.snapshot().await;

    // A rename moves nothing; a new address moves that host only.
    let mut renamed = before.clone();
    let mut web = renamed
        .host(&HostId::parse("web").unwrap())
        .unwrap()
        .clone();
    web.label = Label::parse("Web").unwrap();
    renamed.put_host(web).unwrap();
    assert_eq!(moved_hosts(&before, &renamed), Vec::<HostId>::new());
    let mut readdressed = before.clone();
    let mut web = readdressed
        .host(&HostId::parse("web").unwrap())
        .unwrap()
        .clone();
    web.address = HostName::parse("web2.internal").unwrap();
    readdressed.put_host(web).unwrap();
    assert_eq!(
        moved_hosts(&before, &readdressed),
        [HostId::parse("web").unwrap()]
    );

    // A group's user moves every host in it.
    let mut regrouped = before.clone();
    let mut g = regrouped
        .group(&GroupId::parse("g").unwrap())
        .unwrap()
        .clone();
    g.settings.user = Some(UserName::parse("ops").unwrap());
    regrouped.put_group(g).unwrap();
    assert_eq!(
        moved_hosts(&before, &regrouped),
        [HostId::parse("db").unwrap()]
    );
}

#[tokio::test]
async fn a_telnet_host_lists_as_one_and_connects_on_23() {
    let book = book().await;
    let mut router = input("router", "10.0.0.1");
    router.protocol = ProtocolSpec::Telnet;
    let host = parse_host(&book, router).unwrap();
    let id = host.id.clone();
    let keep = Keep {
        forwards: true,
        startup: true,
    };
    let listed = view(&book.edit(|i| put_host(i, host, keep)).await.unwrap());
    assert_eq!(listed.hosts[0].protocol, ProtocolSpec::Telnet);
    let target = book.telnet_target(&id).await.unwrap();
    assert_eq!(target.port, tauminus_domain::TELNET_PORT);
    assert_eq!(target.host.as_str(), "10.0.0.1");
}
