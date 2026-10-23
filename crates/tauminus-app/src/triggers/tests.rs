// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;

use tauminus_domain::{
    Action, Colour, Highlight, Label, RulePattern, TriggerRule, TriggerSet, TriggerSetId,
};

use super::{TriggerBook, TriggerBookError};
use crate::fakes::FakeTriggerStore;
use crate::ports::StoreError;

fn id(text: &str) -> TriggerSetId {
    TriggerSetId::parse(text).unwrap()
}

fn set(name: &str, patterns: &[&str]) -> TriggerSet {
    let rules = patterns
        .iter()
        .map(|p| TriggerRule {
            name: Label::parse(p).unwrap(),
            pattern: RulePattern::parse(p).unwrap(),
            action: Action::Highlight(
                Highlight::new(Some(Colour::Palette(1)), None, false).unwrap(),
            ),
            enabled: true,
        })
        .collect();
    TriggerSet::new(id(name), Label::parse(name).unwrap(), rules).unwrap()
}

async fn book() -> (Arc<FakeTriggerStore>, TriggerBook) {
    let store = Arc::new(FakeTriggerStore::default());
    let book = TriggerBook::open(store.clone()).await.unwrap();
    (store, book)
}

#[tokio::test]
async fn a_change_is_saved_before_it_shows() {
    let (store, book) = book().await;
    book.edit(|l| l.put(set("a", &["x"]))).await.unwrap();
    assert_eq!(store.saved().unwrap().sets().count(), 1);
    store.fail_next();
    let failed = book.edit(|l| l.put(set("b", &["y"]))).await;
    assert!(matches!(
        failed,
        Err(TriggerBookError::Store(StoreError::Io(_)))
    ));
    assert!(book.snapshot().await.get(&id("b")).is_none());
}

#[tokio::test]
async fn panes_with_the_same_sets_share_compiled_rules_until_a_change() {
    let (_, book) = book().await;
    book.edit(|l| {
        l.put(set("errors", &["ERROR"]))?;
        l.put(set("ok", &["OK"]))
    })
    .await
    .unwrap();
    let one = book.prepare(vec![id("errors")]).await.unwrap();
    let two = book.prepare(vec![id("errors")]).await.unwrap();
    assert_eq!(one.handle, two.handle);
    assert_eq!(one.styles.len(), 1);
    let hits = book.scan(one.handle, &["an ERROR here", "OK"]).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].start, hits[0].end), (3, 8));
    // A change makes the handle stale: the pane prepares again.
    book.edit(|l| l.put(set("errors", &["ERROR", "OK"])))
        .await
        .unwrap();
    assert_eq!(book.scan(one.handle, &["OK"]), None);
    let again = book.prepare(vec![id("errors")]).await.unwrap();
    assert_ne!(again.handle, one.handle);
    assert_eq!(book.scan(again.handle, &["OK"]).unwrap().len(), 1);
}

#[tokio::test]
async fn a_pane_whose_host_names_no_set_gets_the_defaults_or_nothing() {
    let (_, book) = book().await;
    let none = book.prepare(vec![]).await.unwrap();
    assert_eq!(none.styles, vec![]);
    book.edit(|l| {
        l.put(set("base", &["WARN"]))?;
        l.set_defaults(vec![id("base")])
    })
    .await
    .unwrap();
    let base = book.prepare(vec![id("deleted")]).await.unwrap();
    assert_eq!(base.styles.len(), 1);
    assert_eq!(book.scan(base.handle, &["WARN"]).unwrap().len(), 1);
    assert_eq!(book.scan(12345, &["WARN"]), None);
}

#[tokio::test]
async fn compiled_rules_are_bounded_least_recently_prepared_first() {
    let (_, book) = book().await;
    book.edit(|l| {
        for i in 0..70 {
            l.put(set(&format!("s{i}"), &["x"]))?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let first = book.prepare(vec![id("s0")]).await.unwrap();
    for i in 1..super::PREPARED {
        book.prepare(vec![id(&format!("s{i}"))]).await.unwrap();
    }
    // Used again, so the next one pushes out s1 instead.
    assert_eq!(book.prepare(vec![id("s0")]).await.unwrap(), first);
    book.prepare(vec![id("s69")]).await.unwrap();
    assert!(book.scan(first.handle, &["x"]).is_some());
    assert_eq!(book.entries().len(), super::PREPARED);
}

#[tokio::test]
async fn sets_that_cannot_compile_together_fail_at_once_until_a_change() {
    let store = Arc::new(FakeTriggerStore::default());
    // Valid alone, too large together: written to the file by hand, as a
    // set from another device could be.
    let mut library = tauminus_domain::TriggerLibrary::default();
    let wide = |name: &str| {
        let rules = (0..30)
            .map(|i| TriggerRule {
                name: Label::parse("r").unwrap(),
                pattern: RulePattern::parse(&format!(r"\w{{20}}{name}{i}")).unwrap(),
                action: Action::Highlight(
                    Highlight::new(Some(Colour::Palette(1)), None, false).unwrap(),
                ),
                enabled: true,
            })
            .collect();
        TriggerSet::read(id(name), Label::parse(name).unwrap(), rules).unwrap()
    };
    library.put(wide("a")).unwrap();
    library.put(wide("b")).unwrap();
    crate::ports::TriggerStore::save(&*store, &library)
        .await
        .unwrap();
    let book = TriggerBook::open(store.clone()).await.unwrap();
    let began = std::time::Instant::now();
    assert!(book.prepare(vec![id("a"), id("b")]).await.is_err());
    let first = began.elapsed();
    let again = std::time::Instant::now();
    assert!(book.prepare(vec![id("a"), id("b")]).await.is_err());
    assert!(
        again.elapsed() * 10 < first,
        "{:?} then {:?}",
        first,
        again.elapsed()
    );
    drop(book);
}

#[tokio::test]
async fn panes_asking_at_once_for_the_same_sets_compile_them_once() {
    let (_, book) = book().await;
    book.edit(|l| l.put(set("errors", &["ERROR"])))
        .await
        .unwrap();
    let book = Arc::new(book);
    let asks: Vec<_> = (0..8)
        .map(|_| {
            let book = Arc::clone(&book);
            tokio::spawn(async move { book.prepare(vec![id("errors")]).await.unwrap() })
        })
        .collect();
    let mut handles = Vec::new();
    for ask in asks {
        handles.push(ask.await.unwrap().handle);
    }
    handles.dedup();
    assert_eq!(handles.len(), 1);
    assert_eq!(book.entries().len(), 1);
}
