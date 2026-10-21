// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

fn gid(id: &str) -> GroupId {
    GroupId::parse(id).unwrap()
}

fn hid(id: &str) -> HostId {
    HostId::parse(id).unwrap()
}

fn group(id: &str, parent: Option<&str>) -> Group {
    Group {
        id: gid(id),
        name: Label::parse(id).unwrap(),
        parent: parent.map(gid),
        settings: Settings::default(),
    }
}

fn host(id: &str, group: Option<&str>) -> Host {
    let mut host = Host::new(
        hid(id),
        Label::parse(id).unwrap(),
        HostName::parse("db.example.com").unwrap(),
    );
    host.group = group.map(gid);
    host
}

fn user(name: &str) -> UserName {
    UserName::parse(name).unwrap()
}

#[test]
fn names_ids_and_tags_are_validated() {
    assert!(HostId::parse("h-1_a").is_ok());
    for bad in ["", "a b", "a/b", &"x".repeat(65)] {
        assert!(HostId::parse(bad).is_err(), "{bad:?}");
    }
    assert_eq!(Label::parse("  Prod DB  ").unwrap().as_str(), "Prod DB");
    assert!(Label::parse("   ").is_err());
    assert!(Label::parse("a\u{1b}[31m").is_err());
    assert!(Label::parse("Máy chủ 🚀").is_ok());
    assert_eq!(Tag::parse(" Prod ").unwrap().as_str(), "prod");
    assert!(Tag::parse("eu west").is_err());
}

#[test]
fn tags_are_sorted_unique_and_bounded() {
    let mut h = host("h", None);
    let tags = ["web", "prod", "web"]
        .map(|t| Tag::parse(t).unwrap())
        .to_vec();
    h.set_tags(tags).unwrap();
    assert_eq!(
        h.tags().iter().map(Tag::as_str).collect::<Vec<_>>(),
        ["prod", "web"]
    );
    let many = (0..=MAX_TAGS)
        .map(|i| Tag::parse(&format!("t{i}")).unwrap())
        .collect();
    assert_eq!(h.set_tags(many), Err(HostsError::TooManyTags));
}

#[test]
fn the_nearest_setting_wins_and_defaults_fill_the_rest() {
    let mut root = group("root", None);
    root.settings.user = Some(user("ops"));
    root.settings.port = Some(2222);
    root.settings.jumps = Some(vec![Hop {
        user: None,
        host: HostName::parse("bastion").unwrap(),
        port: None,
    }]);
    let mut team = group("team", Some("root"));
    team.settings.port = Some(2200);
    let mut h = host("h", Some("team"));
    h.settings.identities_only = Some(true);
    let inventory = Inventory::new(vec![root, team], vec![h]).unwrap();

    let target = inventory.resolve(&hid("h"), &user("me")).unwrap();
    assert_eq!(target.destination.to_string(), "ops@db.example.com:2200");
    assert_eq!(target.jumps.len(), 1);
    assert_eq!(
        target.jumps[0].to_string(),
        "me@bastion",
        "a hop without a user is the local user"
    );
    assert!(target.auth.identities_only);
    assert_eq!(target.auth.methods, AuthOptions::default().methods);
    assert_eq!(target.route, Route::Direct);
}

#[test]
fn an_empty_jump_list_overrides_a_group_s() {
    let mut root = group("root", None);
    root.settings.jumps = Some(vec![Hop {
        user: None,
        host: HostName::parse("bastion").unwrap(),
        port: None,
    }]);
    let mut h = host("h", Some("root"));
    h.settings.jumps = Some(vec![]);
    let inventory = Inventory::new(vec![root], vec![h]).unwrap();
    assert_eq!(
        inventory.resolve(&hid("h"), &user("me")).unwrap().jumps,
        Vec::<Endpoint>::new()
    );
}

#[test]
fn resolving_a_missing_host_says_so() {
    // Settings that cannot connect are refused when stored; see
    // `settings_no_level_may_hold_are_refused`.
    let inventory = Inventory::default();
    assert_eq!(
        inventory.resolve(&hid("missing"), &user("me")),
        Err(HostsError::NoSuchHost(hid("missing")))
    );
}

#[test]
fn construction_rejects_broken_references() {
    assert!(matches!(
        Inventory::new(vec![group("a", Some("ghost"))], vec![]),
        Err(HostsError::NoSuchGroup(_))
    ));
    assert!(matches!(
        Inventory::new(vec![], vec![host("h", Some("ghost"))]),
        Err(HostsError::NoSuchGroup(_))
    ));
    assert!(matches!(
        Inventory::new(vec![group("a", None), group("a", None)], vec![]),
        Err(HostsError::Duplicate(_))
    ));
    assert!(matches!(
        Inventory::new(vec![group("a", Some("b")), group("b", Some("a"))], vec![]),
        Err(HostsError::Cycle(_))
    ));
}

#[test]
fn moving_a_group_under_its_descendant_is_refused_and_changes_nothing() {
    let mut inventory = Inventory::new(
        vec![
            group("a", None),
            group("b", Some("a")),
            group("c", Some("b")),
        ],
        vec![],
    )
    .unwrap();
    let before = inventory.clone();
    assert_eq!(
        inventory.put_group(group("a", Some("c"))),
        Err(HostsError::Cycle(gid("a")))
    );
    assert_eq!(inventory, before);
    assert_eq!(
        inventory.put_group(group("a", Some("a"))),
        Err(HostsError::Cycle(gid("a")))
    );
    assert_eq!(inventory, before);
}

#[test]
fn depth_is_limited_also_for_a_subtree_that_moves() {
    let chain: Vec<Group> = (0..MAX_GROUP_DEPTH)
        .map(|i| {
            group(
                &format!("g{i}"),
                (i > 0).then(|| format!("g{}", i - 1)).as_deref(),
            )
        })
        .collect();
    let mut inventory = Inventory::new(chain, vec![]).unwrap();
    let deepest = format!("g{}", MAX_GROUP_DEPTH - 1);
    assert_eq!(
        inventory.put_group(group("one-more", Some(&deepest))),
        Err(HostsError::TooDeep)
    );

    // A two-level tree moved under the deepest group would exceed the limit
    // at its leaf, though its own root would not.
    let mut shallow =
        Inventory::new(vec![group("x", None), group("y", Some("x"))], vec![]).unwrap();
    for i in 0..MAX_GROUP_DEPTH - 1 {
        shallow
            .put_group(group(
                &format!("g{i}"),
                (i > 0).then(|| format!("g{}", i - 1)).as_deref(),
            ))
            .unwrap();
    }
    let before = shallow.clone();
    let parent = format!("g{}", MAX_GROUP_DEPTH - 2);
    assert_eq!(
        shallow.put_group(group("x", Some(&parent))),
        Err(HostsError::TooDeep)
    );
    assert_eq!(shallow, before);
}

#[test]
fn removing_a_group_moves_its_contents_to_its_parent() {
    let mut inventory = Inventory::new(
        vec![
            group("a", None),
            group("b", Some("a")),
            group("c", Some("b")),
        ],
        vec![host("h", Some("b")), host("k", Some("a"))],
    )
    .unwrap();
    inventory.remove_group(&gid("b")).unwrap();
    assert_eq!(inventory.group(&gid("c")).unwrap().parent, Some(gid("a")));
    assert_eq!(inventory.host(&hid("h")).unwrap().group, Some(gid("a")));
    assert_eq!(inventory.host(&hid("k")).unwrap().group, Some(gid("a")));
    inventory.remove_group(&gid("a")).unwrap();
    assert_eq!(inventory.host(&hid("h")).unwrap().group, None);
    assert_eq!(inventory.hosts().count(), 2, "no host is lost");
}

#[test]
fn hosts_need_an_existing_group() {
    let mut inventory = Inventory::default();
    assert_eq!(
        inventory.put_host(host("h", Some("g"))),
        Err(HostsError::NoSuchGroup(gid("g")))
    );
    inventory.put_group(group("g", None)).unwrap();
    inventory.put_host(host("h", Some("g"))).unwrap();
    assert_eq!(inventory.remove_host(&hid("h")).unwrap().id, hid("h"));
    assert_eq!(
        inventory.remove_host(&hid("h")),
        Err(HostsError::NoSuchHost(hid("h")))
    );
}

/// A small deterministic generator, so a failure replays exactly.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(self.0 >> 33).unwrap() % bound
    }
}

/// Every group's parent exists, there are no cycles, depth is bounded, and
/// every host's group exists.
fn assert_invariants(inventory: &Inventory, step: usize) {
    for g in inventory.groups() {
        let depth = inventory.check_ancestry(&g.id);
        assert!(
            depth.as_ref().is_ok_and(|&d| d <= MAX_GROUP_DEPTH),
            "step {step}: group {} {depth:?}",
            g.id
        );
    }
    for h in inventory.hosts() {
        assert!(
            h.group
                .as_ref()
                .is_none_or(|id| inventory.group(id).is_some()),
            "step {step}: host {}",
            h.id
        );
    }
}

#[test]
fn random_edits_keep_every_invariant() {
    for seed in 0..200 {
        let mut rng = Lcg(seed);
        let mut inventory = Inventory::default();
        for step in 0..200 {
            let groups: Vec<GroupId> = inventory.groups().map(|g| g.id.clone()).collect();
            let some_group = |rng: &mut Lcg| {
                (!groups.is_empty() && rng.next(4) > 0)
                    .then(|| groups[rng.next(groups.len())].clone())
            };
            match rng.next(5) {
                0 | 1 => {
                    let id = format!("g{}", rng.next(24));
                    let parent = some_group(&mut rng);
                    let _ = inventory.put_group(Group {
                        parent,
                        ..group(&id, None)
                    });
                }
                2 => {
                    let mut h = host(&format!("h{}", rng.next(24)), None);
                    h.group = some_group(&mut rng);
                    let _ = inventory.put_host(h);
                }
                3 => {
                    if let Some(id) = some_group(&mut rng) {
                        inventory.remove_group(&id).unwrap();
                    }
                }
                _ => {
                    let _ = inventory.remove_host(&hid(&format!("h{}", rng.next(24))));
                }
            }
            assert_invariants(&inventory, step);
        }
    }
}

#[test]
fn hops_parse_like_ssh_j() {
    let hop = |text: &str| Hop::parse(text).unwrap();
    assert_eq!(
        hop("bastion"),
        Hop {
            user: None,
            host: HostName::parse("bastion").unwrap(),
            port: None
        }
    );
    assert_eq!(hop("ops@bastion:2222").user, Some(user("ops")));
    assert_eq!(hop("ops@bastion:2222").port, Some(2222));
    assert_eq!(hop("[2001:db8::1]:22").host.as_str(), "2001:db8::1");
    assert_eq!(hop("2001:db8::1").port, None);
    assert_eq!(hop("a@corp@bastion").user, Some(user("a@corp")));
    for bad in [
        "",
        "bastion:0",
        "bastion:x",
        "[::1",
        "[::1]x",
        "-oProxyCommand=x",
        "a;b",
    ] {
        assert!(Hop::parse(bad).is_err(), "{bad:?}");
    }
    assert_eq!(Hop::parse_list("none").unwrap(), vec![]);
    assert_eq!(Hop::parse_list("a, b@c:2").unwrap().len(), 2);
}

#[test]
fn hops_print_in_the_form_they_parse_from() {
    for text in [
        "bastion",
        "ops@bastion:2222",
        "[2001:db8::1]:22",
        "2001:db8::1",
        "a@corp@b",
    ] {
        assert_eq!(Hop::parse(text).unwrap().to_string(), text);
    }
}

#[test]
fn jumps_and_route_inherit_together() {
    let mut prod = group("prod", None);
    prod.settings.jumps = Some(vec![Hop::parse("bastion").unwrap()]);
    let mut legacy = host("legacy", Some("prod"));
    legacy.settings.route = Some(Route::ProxyCommand("nc %h %p".into()));
    let plain = host("plain", Some("prod"));
    let inventory = Inventory::new(vec![prod], vec![legacy, plain]).unwrap();
    let me = user("me");
    // The host's ProxyCommand replaces the group's jump hosts.
    let legacy = inventory.resolve(&hid("legacy"), &me).unwrap();
    assert_eq!(legacy.jumps, Vec::<Endpoint>::new());
    assert!(matches!(legacy.route, Route::ProxyCommand(_)));
    // A host that sets neither takes the group's.
    assert_eq!(
        inventory.resolve(&hid("plain"), &me).unwrap().jumps.len(),
        1
    );
}

#[test]
fn settings_no_level_may_hold_are_refused() {
    let mut inventory = Inventory::default();
    let mut zero = host("zero", None);
    zero.settings.port = Some(0);
    assert!(matches!(
        inventory.put_host(zero),
        Err(HostsError::Target(_))
    ));
    let mut both = host("both", None);
    both.settings.route = Some(Route::ProxyCommand("nc %h %p".into()));
    both.settings.jumps = Some(vec![Hop::parse("j").unwrap()]);
    assert!(matches!(
        inventory.put_host(both),
        Err(HostsError::Target(_))
    ));
    let mut many = group("many", None);
    many.settings.jumps = Some(
        (0..=SshTarget::MAX_JUMPS)
            .map(|i| Hop::parse(&format!("h{i}")).unwrap())
            .collect(),
    );
    assert!(matches!(
        inventory.put_group(many),
        Err(HostsError::Target(_))
    ));
    assert_eq!(inventory, Inventory::default());
}

#[test]
fn removing_a_group_keeps_how_its_hosts_connect() {
    let mut prod = group("prod", None);
    prod.settings.user = Some(user("deploy"));
    prod.settings.jumps = Some(vec![Hop::parse("bastion").unwrap()]);
    let mut own = host("own", Some("prod"));
    own.settings.user = Some(user("root"));
    let inherits = host("inherits", Some("prod"));
    let mut inventory = Inventory::new(vec![prod], vec![own, inherits]).unwrap();
    let me = user("me");
    let before: Vec<_> = ["own", "inherits"]
        .map(|h| inventory.resolve(&hid(h), &me).unwrap())
        .to_vec();
    inventory.remove_group(&gid("prod")).unwrap();
    let after: Vec<_> = ["own", "inherits"]
        .map(|h| inventory.resolve(&hid(h), &me).unwrap())
        .to_vec();
    assert_eq!(before, after);
}

#[test]
fn a_cycle_away_from_the_first_group_is_named_as_a_cycle() {
    let result = Inventory::new(
        vec![
            group("a", Some("b")),
            group("b", Some("c")),
            group("c", Some("b")),
        ],
        vec![],
    );
    assert!(matches!(result, Err(HostsError::Cycle(_))), "{result:?}");
}
