// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use rand::{RngExt as _, SeedableRng as _};
use serde_json::json;

use super::*;

fn values(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn state(records: &[(&str, &[(&str, Value)])]) -> BTreeMap<String, BTreeMap<String, Value>> {
    records
        .iter()
        .map(|(k, v)| ((*k).to_owned(), values(v)))
        .collect()
}

/// A random doc from `seed`: a few records, fields and tombstones.
fn random_doc(rng: &mut rand::rngs::StdRng, device: &str) -> Doc {
    let mut doc = Doc::default();
    for _ in 0..rng.random_range(0..6) {
        let key = format!("host:{}", rng.random_range(0..4));
        let record = doc.records.entry(key).or_default();
        for _ in 0..rng.random_range(0..4) {
            let field = format!("f{}", rng.random_range(0..3));
            record.fields.insert(
                field,
                Cell {
                    at: Stamp {
                        ms: rng.random_range(0..5),
                        n: rng.random_range(0..2),
                        device: device.to_owned(),
                    },
                    value: json!(rng.random_range(0..3)),
                },
            );
        }
        if rng.random_bool(0.3) {
            record.gone = Some(Stamp {
                ms: rng.random_range(0..5),
                n: 0,
                device: device.to_owned(),
            });
        }
    }
    doc
}

fn joined(docs: &[&Doc]) -> Doc {
    let mut out = Doc::default();
    for doc in docs {
        out.merge(doc);
    }
    out
}

#[test]
fn merging_is_commutative_associative_and_idempotent() {
    let mut rng = rand::rngs::StdRng::seed_from_u64(7);
    for _ in 0..2000 {
        let a = random_doc(&mut rng, "a");
        let b = random_doc(&mut rng, "b");
        let c = random_doc(&mut rng, "c");
        assert_eq!(joined(&[&a, &b]), joined(&[&b, &a]));
        assert_eq!(
            joined(&[&joined(&[&a, &b]), &c]),
            joined(&[&a, &joined(&[&b, &c])])
        );
        let ab = joined(&[&a, &b]);
        assert_eq!(joined(&[&ab, &ab]), ab);
        assert_eq!(joined(&[&ab, &a]), ab);
    }
}

#[test]
fn edits_to_different_fields_of_one_record_both_survive() {
    let base_state = state(&[("host:h", &[("label", json!("db")), ("port", json!(22))])]);
    let mut clock_a = Clock::new("a".into());
    let (base, _) = stamp(&Doc::default(), &base_state, &mut clock_a, 100, true);
    let mut clock_b = Clock::new("b".into());
    clock_b.observe(base.latest().unwrap());
    // Offline: A renames, B moves the port.
    let (a, _) = stamp(
        &base,
        &state(&[("host:h", &[("label", json!("db-1")), ("port", json!(22))])]),
        &mut clock_a,
        200,
        true,
    );
    let (b, _) = stamp(
        &base,
        &state(&[("host:h", &[("label", json!("db")), ("port", json!(2222))])]),
        &mut clock_b,
        150,
        true,
    );
    let merged = joined(&[&a, &b]);
    let host = merged.records["host:h"].values();
    assert_eq!(host["label"], json!("db-1"));
    assert_eq!(host["port"], json!(2222));
}

#[test]
fn a_delete_wins_over_any_edit_and_keeps_no_value() {
    let mut clock_a = Clock::new("a".into());
    let (base, _) = stamp(
        &Doc::default(),
        &state(&[(
            "host:h",
            &[("label", json!("db")), ("secret", json!("s3cret"))],
        )]),
        &mut clock_a,
        100,
        true,
    );
    let mut clock_b = Clock::new("b".into());
    clock_b.observe(base.latest().unwrap());
    // A deletes the host; B, later, edits it.
    let (a, _) = stamp(&base, &BTreeMap::new(), &mut clock_a, 200, true);
    assert!(!a.records["host:h"].alive());
    assert_eq!(a.records["host:h"].fields.len(), 0, "the values went too");
    let (b, _) = stamp(
        &base,
        &state(&[(
            "host:h",
            &[("label", json!("db-2")), ("secret", json!("s3cret"))],
        )]),
        &mut clock_b,
        300,
        true,
    );
    for merged in [joined(&[&a, &b]), joined(&[&b, &a])] {
        assert!(!merged.records["host:h"].alive());
        assert!(!serde_json::to_string(&merged).unwrap().contains("s3cret"));
    }
    // Stamping the record again over the delete does not bring it back.
    let merged = joined(&[&a, &b]);
    let (again, changed) = stamp(
        &merged,
        &state(&[("host:h", &[("label", json!("db-3"))])]),
        &mut clock_b,
        400,
        true,
    );
    assert!(!again.records["host:h"].alive());
    assert!(changed.contains(&("host:h".to_owned(), "label".to_owned())));
}

#[test]
fn the_wire_form_says_each_device_once_and_reads_back_alike() {
    let mut rng = rand::rngs::StdRng::seed_from_u64(11);
    for _ in 0..500 {
        let doc = joined(&[
            &random_doc(&mut rng, "0123456789abcdef0123456789abcdef"),
            &random_doc(&mut rng, "fedcba9876543210fedcba9876543210"),
        ]);
        let text = serde_json::to_string(&doc).unwrap();
        assert!(
            text.matches("0123456789abcdef0123456789abcdef").count() <= 1,
            "{text}"
        );
        assert_eq!(serde_json::from_str::<Doc>(&text).unwrap(), doc);
    }
    // A stamp naming a device the table lacks is refused.
    let bad = r#"{"devices":["a"],"records":{"host:h":[null,{"label":[1,0,5,"x"]}]}}"#;
    assert!(serde_json::from_str::<Doc>(bad).is_err());
    // A tombstone that carries values loses them on reading.
    let tomb = r#"{"devices":["a"],"records":{"host:h":[[2,0,0],{"label":[1,0,0,"x"]}]}}"#;
    let doc: Doc = serde_json::from_str(tomb).unwrap();
    assert_eq!(doc.records["host:h"].fields.len(), 0);
}

#[test]
fn a_field_taken_away_stays_away_and_a_key_unchanged_keeps_its_stamp() {
    let mut clock = Clock::new("a".into());
    let (base, _) = stamp(
        &Doc::default(),
        &state(&[(
            "host:h",
            &[("label", json!("db")), ("settings.user", json!("ops"))],
        )]),
        &mut clock,
        100,
        true,
    );
    let (doc, changed) = stamp(
        &base,
        &state(&[("host:h", &[("label", json!("db"))])]),
        &mut clock,
        200,
        true,
    );
    assert_eq!(
        changed,
        BTreeSet::from([("host:h".to_owned(), "settings.user".to_owned())])
    );
    assert_eq!(
        doc.records["host:h"].fields["label"],
        base.records["host:h"].fields["label"]
    );
    assert!(!doc.records["host:h"].values().contains_key("settings.user"));
}

#[test]
fn the_same_field_changed_on_both_devices_is_a_clash_whichever_syncs_first() {
    let mut clock_a = Clock::new("a".into());
    let (base, _) = stamp(
        &Doc::default(),
        &state(&[("host:h", &[("label", json!("db"))])]),
        &mut clock_a,
        100,
        true,
    );
    let mut clock_b = Clock::new("b".into());
    clock_b.observe(base.latest().unwrap());
    let (a, a_changed) = stamp(
        &base,
        &state(&[("host:h", &[("label", json!("A"))])]),
        &mut clock_a,
        200,
        true,
    );
    let (b, b_changed) = stamp(
        &base,
        &state(&[("host:h", &[("label", json!("B"))])]),
        &mut clock_b,
        300,
        true,
    );
    let merged = joined(&[&a, &b]);
    // On A, B's later value won; on B, its own won over A's.
    let on_a = clashes("a", &base, &a, &b, &merged, &a_changed);
    let on_b = clashes("b", &base, &b, &a, &merged, &b_changed);
    for found in [on_a, on_b] {
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kept, json!("B"));
        assert_eq!(found[0].lost, json!("A"));
    }
}

#[test]
fn a_tombstone_is_dropped_once_old() {
    let mut clock = Clock::new("a".into());
    let (base, _) = stamp(
        &Doc::default(),
        &state(&[
            ("host:h", &[("label", json!("secret-name"))]),
            ("host:k", &[("label", json!("kept"))]),
        ]),
        &mut clock,
        100,
        true,
    );
    let (mut doc, _) = stamp(
        &base,
        &state(&[("host:k", &[("label", json!("kept"))])]),
        &mut clock,
        1_000,
        true,
    );
    doc.expire(1_000 + 29, 30);
    assert!(doc.records.contains_key("host:h"), "kept while young");
    doc.expire(1_000 + 31, 30);
    assert!(!doc.records.contains_key("host:h"));
    assert!(doc.records.contains_key("host:k"), "a live record stays");
    assert!(!serde_json::to_string(&doc).unwrap().contains("secret-name"));
}

#[test]
fn a_doc_shows_no_value_when_debugged() {
    let mut doc = Doc::default();
    doc.records.insert(
        "identity:a".into(),
        Record {
            fields: BTreeMap::from([(
                "password".to_owned(),
                Cell {
                    at: Stamp {
                        ms: 1,
                        n: 0,
                        device: "a".into(),
                    },
                    value: Value::from("hunter2-canary"),
                },
            )]),
            gone: None,
        },
    );
    assert!(!format!("{doc:?}").contains("hunter2"));
}
