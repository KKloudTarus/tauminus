// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

fn spec(kind: ForwardKindSpec, bind: &str) -> ForwardSpec {
    ForwardSpec {
        id: None,
        label: "db".into(),
        kind,
        bind: bind.into(),
        port: 5432,
        to_host: Some("db.internal".into()),
        to_port: Some(5432),
        auto_start: true,
    }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "parse_rule takes a fallible id source"
)]
fn new_id() -> Result<ForwardId, CommandError> {
    Ok(ForwardId::parse("fresh").unwrap())
}

#[test]
fn a_rule_round_trips_through_its_spec() {
    for kind in [ForwardKindSpec::Local, ForwardKindSpec::Remote] {
        for bind in ["loopback", "any", "10.0.0.2", "::1"] {
            let rule = parse_rule(spec(kind, bind), new_id).unwrap();
            assert_eq!(rule.id.as_str(), "fresh");
            let back = parse_rule(ForwardSpec::from(&rule), new_id).unwrap();
            assert_eq!(back, rule, "{kind:?} {bind}");
        }
    }
    let mut dynamic = spec(ForwardKindSpec::Dynamic, "loopback");
    dynamic.to_host = None;
    dynamic.to_port = None;
    let rule = parse_rule(dynamic, new_id).unwrap();
    assert_eq!(ForwardSpec::from(&rule).to_host, None);
}

#[test]
fn bind_addresses_must_be_ip_literals() {
    for bind in ["lan.local", "0", "*", "", "127.0.0.1:80"] {
        let error = parse_rule(spec(ForwardKindSpec::Local, bind), new_id).unwrap_err();
        assert!(matches!(error.code, ErrorCode::InvalidArgument), "{bind:?}");
    }
    let rule = parse_rule(spec(ForwardKindSpec::Local, "localhost"), new_id).unwrap();
    assert_eq!(rule.forward.bind().address, BindAddress::Loopback);
}

#[test]
fn local_and_remote_rules_need_a_destination() {
    let mut no_host = spec(ForwardKindSpec::Local, "loopback");
    no_host.to_host = Some("  ".into());
    assert!(parse_rule(no_host, new_id).is_err());
    let mut no_port = spec(ForwardKindSpec::Remote, "loopback");
    no_port.to_port = None;
    assert!(parse_rule(no_port, new_id).is_err());
    let mut zero = spec(ForwardKindSpec::Remote, "loopback");
    zero.to_port = Some(0);
    assert!(parse_rule(zero, new_id).is_err());
    let mut bad = spec(ForwardKindSpec::Local, "loopback");
    bad.to_host = Some("db;rm".into());
    assert!(parse_rule(bad, new_id).is_err());
}

fn rule(id: &str, port: u16) -> ForwardRule {
    let mut spec = spec(ForwardKindSpec::Local, "loopback");
    spec.id = Some(id.into());
    spec.port = port;
    parse_rule(spec, new_id).unwrap()
}

#[test]
fn removed_and_changed_rules_are_stale_and_relabelled_ones_are_not() {
    let before = [rule("a", 1), rule("b", 2), rule("c", 3)];
    let mut relabelled = rule("a", 1);
    relabelled.label = Label::parse("renamed").unwrap();
    relabelled.auto_start = false;
    let after = [relabelled, rule("b", 20)];
    let stale: Vec<_> = stale_rules(&before, &after)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(stale, ["b", "c"]);
    assert_eq!(stale_rules(&[], &after), Vec::<ForwardId>::new());
}
