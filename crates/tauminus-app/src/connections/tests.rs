// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::*;
use crate::fakes::{FakeConnector, login, prompter};

fn registry(connector: &Arc<FakeConnector>) -> Connections {
    Connections::new(Arc::clone(connector) as Arc<dyn SshConnector>)
}

#[tokio::test]
async fn a_live_connection_is_reused_while_held() {
    let connector = Arc::new(FakeConnector::default());
    let connections = registry(&connector);
    let db = login("db", "db.internal");
    let first = connections.acquire(&db, prompter()).await.unwrap();
    let second = connections.acquire(&db, prompter()).await.unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(connector.logins(), 1);
    drop((first, second));
    connector.made.lock().unwrap().clear();
    // Nothing holds it, so it is gone and the next acquire logs in.
    let _third = connections.acquire(&db, prompter()).await.unwrap();
    assert_eq!(connector.logins(), 2);
}

#[tokio::test]
async fn a_closed_connection_or_a_changed_host_logs_in_again() {
    let connector = Arc::new(FakeConnector::default());
    let connections = registry(&connector);
    let db = login("db", "db.internal");
    let first = connections.acquire(&db, prompter()).await.unwrap();
    connector.made.lock().unwrap()[0]
        .closed
        .store(true, Ordering::Relaxed);
    let second = connections.acquire(&db, prompter()).await.unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    // The host now points elsewhere: same id, different target.
    let moved = login("db", "db2.internal");
    let third = connections.acquire(&moved, prompter()).await.unwrap();
    assert!(!Arc::ptr_eq(&second, &third));
    // And a different identity is a different login too.
    let mut other = moved.clone();
    other.identity = Some(IdentityId::parse("ops").unwrap());
    let fourth = connections.acquire(&other, prompter()).await.unwrap();
    assert!(!Arc::ptr_eq(&third, &fourth));
    assert_eq!(connector.logins(), 4);
}

#[tokio::test]
async fn acquires_at_once_share_one_login() {
    let connector = Arc::new(FakeConnector {
        delay: Duration::from_millis(50),
        ..FakeConnector::default()
    });
    let connections = Arc::new(registry(&connector));
    let db = login("db", "db.internal");
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let connections = Arc::clone(&connections);
            let db = db.clone();
            tokio::spawn(async move { connections.acquire(&db, prompter()).await.unwrap() })
        })
        .collect();
    let mut got = Vec::new();
    for task in tasks {
        got.push(task.await.unwrap());
    }
    assert_eq!(connector.logins(), 1);
    assert!(got.iter().all(|c| Arc::ptr_eq(c, &got[0])));
    // Another host does not wait on this one's lock.
    let _web = connections
        .acquire(&login("web", "web.internal"), prompter())
        .await
        .unwrap();
    assert_eq!(connector.logins(), 2);
}

#[tokio::test]
async fn a_failed_login_is_not_kept() {
    let connector = Arc::new(FakeConnector::default());
    let connections = registry(&connector);
    let db = login("db", "db.internal");
    connector.refuse.store(true, Ordering::Relaxed);
    assert!(connections.acquire(&db, prompter()).await.is_err());
    connector.refuse.store(false, Ordering::Relaxed);
    assert!(connections.acquire(&db, prompter()).await.is_ok());
}

#[tokio::test]
async fn acquired_connections_never_forward_the_agent() {
    let connector = Arc::new(FakeConnector::default());
    let connections = registry(&connector);
    let mut agent = login("bastion", "bastion.internal");
    agent.target.session.forward_agent = true;
    let _forward = connections.acquire(&agent, prompter()).await.unwrap();
    assert_eq!(*connector.logins.lock().unwrap(), [false]);
}
