// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tauminus_app::ports::{BoxFuture, StoreError, TriggerStore};

use super::*;

#[derive(Default)]
struct MemoryStore {
    saved: Mutex<TriggerLibrary>,
    next: AtomicUsize,
}

impl TriggerStore for MemoryStore {
    fn load(&self) -> BoxFuture<'_, Result<TriggerLibrary, StoreError>> {
        let library = self.saved.lock().unwrap().clone();
        Box::pin(async move { Ok(library) })
    }

    fn save<'a>(&'a self, library: &'a TriggerLibrary) -> BoxFuture<'a, Result<(), StoreError>> {
        *self.saved.lock().unwrap() = library.clone();
        Box::pin(async { Ok(()) })
    }

    fn new_id(&self) -> String {
        format!("set{}", self.next.fetch_add(1, Ordering::Relaxed))
    }
}

fn id(text: &str) -> TriggerSetId {
    TriggerSetId::parse(text).unwrap()
}

async fn book() -> TriggerBook {
    TriggerBook::open(Arc::new(MemoryStore::default()))
        .await
        .unwrap()
}

fn rule(pattern: &str, fg: Option<&str>, bg: Option<&str>) -> RuleSpec {
    RuleSpec {
        name: pattern.to_owned(),
        pattern: pattern.to_owned(),
        enabled: true,
        action: ActionSpec::Highlight {
            style: StyleSpec {
                fg: fg.map(str::to_owned),
                bg: bg.map(str::to_owned),
                line: false,
            },
        },
    }
}

fn input(name: &str, rules: Vec<RuleSpec>) -> TriggerSetInput {
    TriggerSetInput {
        id: None,
        name: name.to_owned(),
        rules,
    }
}

#[tokio::test]
async fn a_set_reads_back_as_the_view_sent_it() {
    let book = book().await;
    let set = parse(
        id("errors"),
        input("Errors", vec![rule("ERROR", Some("1"), Some("#200000"))]),
    )
    .unwrap();
    book.edit(|l| l.put(set)).await.unwrap();
    let views = view(&book.snapshot().await);
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].name, "Errors");
    assert_eq!(
        views[0].rules[0].action,
        ActionSpec::Highlight {
            style: StyleSpec {
                fg: Some("1".into()),
                bg: Some("#200000".into()),
                line: false,
            }
        }
    );
    assert_eq!(views[0].rank, None);
}

#[tokio::test]
async fn an_error_names_the_rule_it_is_in() {
    let error = parse(
        id("s"),
        input(
            "s",
            vec![rule("ok", Some("1"), None), rule("bad(", Some("1"), None)],
        ),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(error.message.starts_with("rule 2:"), "{}", error.message);
    let error = parse(id("s"), input("s", vec![rule("x", Some("red"), None)])).unwrap_err();
    assert!(error.message.contains("palette"), "{}", error.message);
    let error = parse(id("s"), input("s", vec![rule("x", None, Some(""))])).unwrap_err();
    assert!(error.message.contains("colour"), "{}", error.message);
}

#[tokio::test]
async fn a_pattern_check_says_why_or_nothing() {
    assert_eq!(trigger_pattern_check("ERROR|WARN".into()).await, None);
    assert!(trigger_pattern_check("(".into()).await.is_some());
    assert!(trigger_pattern_check(String::new()).await.is_some());
}

#[tokio::test]
async fn the_view_carries_the_order_of_the_sets_every_session_gets() {
    let book = book().await;
    for name in ["a", "b", "c"] {
        let set = parse(id(name), input(name, vec![rule("x", Some("1"), None)])).unwrap();
        book.edit(|l| l.put(set)).await.unwrap();
    }
    book.edit(|l| l.set_defaults(vec![id("c"), id("a")]))
        .await
        .unwrap();
    let ranks: Vec<(String, Option<u32>)> = view(&book.snapshot().await)
        .into_iter()
        .map(|v| (v.name, v.rank))
        .collect();
    assert_eq!(
        ranks,
        vec![
            ("a".into(), Some(1)),
            ("b".into(), None),
            ("c".into(), Some(0))
        ]
    );
}

#[test]
fn text_to_send_holds_no_control_characters() {
    let send = |text: &str, enter: bool| RuleSpec {
        action: ActionSpec::Send {
            text: text.to_owned(),
            enter,
        },
        ..rule("x", None, None)
    };
    assert!(parse(id("s"), input("s", vec![send("yes", true)])).is_ok());
    for (text, enter) in [("y\r", false), ("\u{1b}[201~", false), ("", false)] {
        let error = parse(id("s"), input("s", vec![send(text, enter)])).unwrap_err();
        assert!(error.message.starts_with("rule 1:"), "{}", error.message);
    }
}
