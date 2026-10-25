// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tauminus_app::HostBook;
use tauminus_app::cloud::Instance;
use tauminus_app::ports::{BoxFuture, HostStore, StoreError};
use tauminus_domain::{Inventory, Origin};

use super::*;

/// A provider with one instance, as a CLI would list it.
struct One;

impl CloudInventory for One {
    fn list<'a>(&'a self, request: &'a CloudRequest) -> BoxFuture<'a, Result<Listing, String>> {
        let scope = request.scope.clone();
        Box::pin(async move {
            if scope.as_deref() == Some("broken") {
                return Err("doctl failed (exit status: 1): Unable to authenticate you".into());
            }
            Ok(Listing {
                provider: "DigitalOcean".into(),
                account: "b6fc48a1".into(),
                whole: vec!["do:b6fc48a1".into()],
                instances: vec![Instance {
                    origin: Origin::parse("do:b6fc48a1:7").unwrap(),
                    name: "web".into(),
                    public: Some("203.0.113.7".into()),
                    private: None,
                    user: Some("root".into()),
                    tags: vec![],
                    region: "nyc3".into(),
                }],
            })
        })
    }
}

/// Hosts kept in memory.
#[derive(Default)]
struct Memory(Mutex<Inventory>, AtomicUsize);

impl HostStore for Memory {
    fn load(&self) -> BoxFuture<'_, Result<Inventory, StoreError>> {
        let inventory = self.0.lock().unwrap().clone();
        Box::pin(async move { Ok(inventory) })
    }

    fn save<'a>(&'a self, inventory: &'a Inventory) -> BoxFuture<'a, Result<(), StoreError>> {
        *self.0.lock().unwrap() = inventory.clone();
        Box::pin(async { Ok(()) })
    }

    fn new_id(&self) -> String {
        format!("id{}", self.1.fetch_add(1, Ordering::Relaxed))
    }
}

async fn book() -> HostBook {
    HostBook::open(Arc::new(Memory::default())).await.unwrap()
}

fn ask(scope: Option<&str>) -> CloudAsk {
    CloudAsk {
        provider: CloudProvider::DigitalOcean,
        scope: scope.map(str::to_owned),
        address: CloudAddress::Public,
    }
}

#[tokio::test]
async fn a_preview_saves_nothing_and_its_token_imports_it_once() {
    let cloud = Cloud::with(Box::new(One), Box::new(|_| "/usr/local/bin/doctl".into()));
    let book = book().await;
    let shown = list(&cloud, &book, ask(None)).await.unwrap();
    assert_eq!(shown.program, "/usr/local/bin/doctl");
    assert_eq!(shown.counts.add, 1);
    assert_eq!(shown.add[0].label, "web");
    assert_eq!(book.snapshot().await.hosts().count(), 0, "nothing saved");
    // Another token, an earlier one say: nothing.
    assert!(import(&cloud, &book, shown.token + 1).await.is_err());
    let done = import(&cloud, &book, shown.token).await.unwrap();
    assert_eq!(done.result.added, 1);
    assert_eq!(book.snapshot().await.hosts().count(), 1);
    // Once: the list is gone after.
    assert!(import(&cloud, &book, shown.token).await.is_err());
    // Listed again: nothing new to add.
    let again = list(&cloud, &book, ask(None)).await.unwrap();
    assert_eq!(again.counts.add, 0);
}

#[tokio::test]
async fn a_failing_cli_is_told_with_its_words() {
    let cloud = Cloud::with(Box::new(One), Box::new(|_| String::new()));
    let error = list(&cloud, &book().await, ask(Some("broken")))
        .await
        .unwrap_err();
    assert!(
        error.message.contains("Unable to authenticate"),
        "{error:?}"
    );
}
