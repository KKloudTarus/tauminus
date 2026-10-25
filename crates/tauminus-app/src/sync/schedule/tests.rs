// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use tauminus_domain::{Host, HostId, HostName, Label};
use tokio::sync::Notify;

use super::*;
use crate::ports::SyncTarget;
use crate::sync::service::tests::{Connector, PASSPHRASE, service};
use crate::vault::tests::secret;

/// Lets the loop run until it waits on time again. A run reads the vault
/// on the blocking pool, which runs on real time: each turn gives it a
/// millisecond of that.
async fn settle() {
    for _ in 0..200 {
        tokio::task::yield_now().await;
        #[expect(
            clippy::disallowed_methods,
            reason = "holds the paused clock still while the blocking pool runs"
        )]
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[tokio::test(start_paused = true)]
async fn syncs_on_unlock_on_a_tick_and_after_edits_settle() {
    let connector = Arc::new(Connector::default());
    let (vault, hosts, service) = service(Arc::clone(&connector)).await;
    vault.create(secret("vault password")).await.unwrap();
    service
        .create(SyncTarget::Folder("/srv/sync".into()), secret(PASSPHRASE))
        .await
        .unwrap();
    vault.lock().await;
    let kick = Arc::new(Notify::new());
    let reports = Arc::new(Mutex::new(0));
    let looping = tokio::spawn(Arc::clone(&service).keep_in_sync(Arc::clone(&kick), {
        let reports = Arc::clone(&reports);
        move |_| *reports.lock().unwrap() += 1
    }));
    let runs = || connector.opened.lock().unwrap().len();
    settle().await;
    assert_eq!(runs(), 0);

    vault.unlock(secret("vault password")).await.unwrap();
    settle().await;
    assert_eq!(runs(), 1, "on unlock");
    assert_eq!(*reports.lock().unwrap(), 1);

    // The run's own save is no edit: nothing runs before the tick.
    tokio::time::advance(EVERY.saturating_sub(Duration::from_secs(1))).await;
    settle().await;
    assert_eq!(runs(), 1);
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    assert_eq!(runs(), 2, "on the tick");

    hosts
        .edit(|i| {
            i.put_host(Host::new(
                HostId::parse("mine").unwrap(),
                Label::parse("mine").unwrap(),
                HostName::parse("db.internal").unwrap(),
            ))
        })
        .await
        .unwrap();
    settle().await;
    tokio::time::advance(SETTLE.saturating_sub(Duration::from_secs(1))).await;
    settle().await;
    assert_eq!(runs(), 2, "edits settle first");
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    assert_eq!(runs(), 3, "after the edit");

    kick.notify_one();
    settle().await;
    assert_eq!(runs(), 4, "kicked");

    // A run outside the schedule, as Sync now: its saves are no edit.
    hosts
        .edit(|i| i.remove_host(&HostId::parse("theirs").unwrap()).map(|_| ()))
        .await
        .unwrap();
    service.run().await.unwrap();
    settle().await;
    tokio::time::advance(SETTLE * 2).await;
    settle().await;
    assert_eq!(runs(), 5, "no extra run after a manual one");

    let closed = connector.closed.load(Ordering::SeqCst);
    vault.lock().await;
    settle().await;
    assert_eq!(
        connector.closed.load(Ordering::SeqCst),
        closed + 1,
        "lock drops the target"
    );
    tokio::time::advance(EVERY * 2).await;
    settle().await;
    assert_eq!(runs(), 5, "nothing runs locked");
    looping.abort();
}
