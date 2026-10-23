// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;
use std::sync::atomic::Ordering;

use tauminus_domain::{Bind, BindAddress, Forward, ForwardId, ForwardRule, Label};

use super::*;
use crate::connections::Login;
use crate::fakes::{FakeConnector, login, prompter};
use crate::ports::SshConnector;

fn rule(id: &str, port: u16) -> ForwardRule {
    ForwardRule {
        id: ForwardId::parse(id).unwrap(),
        label: Label::parse(id).unwrap(),
        forward: Forward::Dynamic {
            bind: Bind {
                address: BindAddress::Loopback,
                port,
            },
        },
        auto_start: false,
    }
}

fn ticket(forwards: &Forwards, login: &Login, rule: &str) -> Ticket {
    forwards.ticket(&login.host, &ForwardId::parse(rule).unwrap())
}

fn setup() -> (Arc<FakeConnector>, Forwards) {
    let connector = Arc::new(FakeConnector::default());
    let connections = Arc::new(Connections::new(
        Arc::clone(&connector) as Arc<dyn SshConnector>
    ));
    (connector, Forwards::new(connections))
}

#[tokio::test]
async fn rules_on_one_host_share_a_login_and_list_their_ports() {
    let (connector, forwards) = setup();
    let db = login("db", "db.internal");
    assert_eq!(
        forwards
            .start(
                &db,
                &rule("socks", 1080),
                ticket(&forwards, &db, "socks"),
                prompter()
            )
            .await
            .unwrap(),
        1080
    );
    let picked = forwards
        .start(
            &db,
            &rule("any", 0),
            ticket(&forwards, &db, "any"),
            prompter(),
        )
        .await
        .unwrap();
    assert_ne!(picked, 0);
    assert_eq!(connector.logins(), 1);
    let list = forwards.list();
    let ports: Vec<_> = list.iter().map(|s| (s.rule.as_str(), s.port)).collect();
    assert_eq!(ports, [("any", Some(picked)), ("socks", Some(1080))]);
}

#[tokio::test]
async fn starting_a_running_rule_again_changes_nothing() {
    let (connector, forwards) = setup();
    let db = login("db", "db.internal");
    forwards
        .start(
            &db,
            &rule("socks", 0),
            ticket(&forwards, &db, "socks"),
            prompter(),
        )
        .await
        .unwrap();
    let port = forwards.list()[0].port;
    forwards
        .start(
            &db,
            &rule("socks", 0),
            ticket(&forwards, &db, "socks"),
            prompter(),
        )
        .await
        .unwrap();
    assert_eq!(forwards.list()[0].port, port);
    assert_eq!(
        connector.made.lock().unwrap()[0]
            .forwards
            .lock()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn a_lost_connection_shows_and_a_restart_logs_in_again() {
    let (connector, forwards) = setup();
    let db = login("db", "db.internal");
    forwards
        .start(
            &db,
            &rule("socks", 1080),
            ticket(&forwards, &db, "socks"),
            prompter(),
        )
        .await
        .unwrap();
    connector.made.lock().unwrap()[0].drop_link();
    let state = &forwards.list()[0];
    assert_eq!(state.port, None);
    assert_eq!(state.status.ended.as_deref(), Some("the connection closed"));
    forwards
        .start(
            &db,
            &rule("socks", 1080),
            ticket(&forwards, &db, "socks"),
            prompter(),
        )
        .await
        .unwrap();
    assert_eq!(connector.logins(), 2);
    assert_eq!(forwards.list()[0].status.ended, None);
}

#[tokio::test]
async fn a_failed_start_is_listed_with_its_reason_and_stop_forgets() {
    let (connector, forwards) = setup();
    let db = login("db", "db.internal");
    connector.refuse.store(true, Ordering::Relaxed);
    assert!(
        forwards
            .start(
                &db,
                &rule("socks", 1080),
                ticket(&forwards, &db, "socks"),
                prompter()
            )
            .await
            .is_err()
    );
    let state = &forwards.list()[0];
    assert_eq!(state.port, None);
    assert_eq!(state.status.ended.as_deref(), Some("cancelled"));
    assert!(
        !forwards.stop(&db.host, &state.rule),
        "a failed rule was not running"
    );
    assert_eq!(forwards.list(), Vec::new());

    connector.refuse.store(false, Ordering::Relaxed);
    forwards
        .start(&db, &rule("a", 1), ticket(&forwards, &db, "a"), prompter())
        .await
        .unwrap();
    forwards
        .start(
            &login("web", "web"),
            &rule("b", 2),
            ticket(&forwards, &login("web", "web"), "b"),
            prompter(),
        )
        .await
        .unwrap();
    assert!(forwards.stop(&db.host, &ForwardId::parse("a").unwrap()));
    forwards.stop_host(&HostId::parse("web").unwrap());
    assert_eq!(forwards.list(), Vec::new());
}

fn slow_setup() -> (Arc<FakeConnector>, Arc<Forwards>) {
    let connector = Arc::new(FakeConnector {
        delay: std::time::Duration::from_millis(50),
        ..FakeConnector::default()
    });
    let connections = Arc::new(Connections::new(
        Arc::clone(&connector) as Arc<dyn SshConnector>
    ));
    (connector, Arc::new(Forwards::new(connections)))
}

#[tokio::test]
async fn a_second_start_while_the_first_logs_in_is_refused_and_harms_nothing() {
    let (connector, forwards) = slow_setup();
    let db = login("db", "db.internal");
    let first = {
        let forwards = Arc::clone(&forwards);
        let db = db.clone();
        tokio::spawn(async move {
            forwards
                .start(
                    &db,
                    &rule("pg", 5432),
                    ticket(&forwards, &db, "pg"),
                    prompter(),
                )
                .await
        })
    };
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let second = forwards
        .start(
            &db,
            &rule("pg", 5432),
            ticket(&forwards, &db, "pg"),
            prompter(),
        )
        .await;
    assert!(matches!(second, Err(StartError::Busy)));
    assert_eq!(first.await.unwrap().unwrap(), 5432);
    assert_eq!(forwards.list()[0].port, Some(5432));
    assert_eq!(connector.made.lock().unwrap()[0].live_forwards(), 1);
}

#[tokio::test]
async fn a_stop_during_the_login_wins() {
    let (connector, forwards) = slow_setup();
    let db = login("db", "db.internal");
    let started = {
        let forwards = Arc::clone(&forwards);
        let db = db.clone();
        tokio::spawn(async move {
            forwards
                .start(
                    &db,
                    &rule("pg", 5432),
                    ticket(&forwards, &db, "pg"),
                    prompter(),
                )
                .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    assert!(forwards.stop(&db.host, &ForwardId::parse("pg").unwrap()));
    assert!(matches!(started.await.unwrap(), Err(StartError::Stopped)));
    assert_eq!(forwards.list(), Vec::new());
    assert_eq!(connector.made.lock().unwrap()[0].live_forwards(), 0);
    // Let the connection go, so the next start logs in again.
    connector.made.lock().unwrap().clear();

    // The same for deleting the host.
    let started = {
        let forwards = Arc::clone(&forwards);
        let db = db.clone();
        tokio::spawn(async move {
            forwards
                .start(
                    &db,
                    &rule("pg", 5432),
                    ticket(&forwards, &db, "pg"),
                    prompter(),
                )
                .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    forwards.stop_host(&db.host);
    assert!(matches!(started.await.unwrap(), Err(StartError::Stopped)));
    assert_eq!(forwards.list(), Vec::new());
}

#[tokio::test]
async fn a_rule_edited_while_it_runs_starts_again_as_saved() {
    let (connector, forwards) = setup();
    let db = login("db", "db.internal");
    forwards
        .start(
            &db,
            &rule("pg", 5432),
            ticket(&forwards, &db, "pg"),
            prompter(),
        )
        .await
        .unwrap();
    let moved = forwards
        .start(
            &db,
            &rule("pg", 6543),
            ticket(&forwards, &db, "pg"),
            prompter(),
        )
        .await
        .unwrap();
    assert_eq!(moved, 6543);
    let connection = &connector.made.lock().unwrap()[0];
    assert_eq!(
        connection.live_forwards(),
        1,
        "the old listener was dropped"
    );
    assert_eq!(forwards.list()[0].port, Some(6543));
}

#[tokio::test]
async fn a_rule_read_before_a_stop_never_starts() {
    let (connector, forwards) = setup();
    let db = login("db", "db.internal");
    // An auto-start took its ticket and read the rule; then an edit
    // removed the rule, before anything ran.
    let stale = ticket(&forwards, &db, "pg");
    assert!(!forwards.stop(&db.host, &ForwardId::parse("pg").unwrap()));
    let started = forwards
        .start(&db, &rule("pg", 5432), stale, prompter())
        .await;
    assert!(matches!(started, Err(StartError::Stopped)));
    assert_eq!(connector.logins(), 0, "no login for a rule that is gone");
    // The same after the host is deleted.
    let stale = ticket(&forwards, &db, "pg");
    forwards.stop_host(&db.host);
    let started = forwards
        .start(&db, &rule("pg", 5432), stale, prompter())
        .await;
    assert!(matches!(started, Err(StartError::Stopped)));
    // A fresh ticket starts it.
    let fresh = ticket(&forwards, &db, "pg");
    forwards
        .start(&db, &rule("pg", 5432), fresh, prompter())
        .await
        .unwrap();
}

#[tokio::test]
async fn a_start_stopped_and_started_again_keeps_the_second() {
    let (connector, forwards) = slow_setup();
    let db = login("db", "db.internal");
    let spawn_start = |port: u16| {
        let forwards = Arc::clone(&forwards);
        let db = db.clone();
        tokio::spawn(async move {
            let ticket = ticket(&forwards, &db, "pg");
            forwards
                .start(&db, &rule("pg", port), ticket, prompter())
                .await
        })
    };
    let first = spawn_start(5432);
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    assert!(forwards.stop(&db.host, &ForwardId::parse("pg").unwrap()));
    let second = spawn_start(6543);
    assert!(matches!(first.await.unwrap(), Err(StartError::Stopped)));
    assert_eq!(second.await.unwrap().unwrap(), 6543);
    let states = forwards.list();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].port, Some(6543));
    let live: usize = connector
        .made
        .lock()
        .unwrap()
        .iter()
        .map(|c| c.live_forwards())
        .sum();
    assert_eq!(live, 1, "the first start's forward was let go");
}

#[tokio::test]
async fn a_host_that_points_elsewhere_restarts_its_rule() {
    let (connector, forwards) = setup();
    let db = login("db", "db.internal");
    forwards
        .start(
            &db,
            &rule("pg", 5432),
            ticket(&forwards, &db, "pg"),
            prompter(),
        )
        .await
        .unwrap();
    let moved = login("db", "db2.internal");
    forwards
        .start(
            &moved,
            &rule("pg", 5432),
            ticket(&forwards, &moved, "pg"),
            prompter(),
        )
        .await
        .unwrap();
    assert_eq!(connector.logins(), 2, "a login to the new address");
    let live: usize = connector
        .made
        .lock()
        .unwrap()
        .iter()
        .map(|c| c.live_forwards())
        .sum();
    assert_eq!(live, 1);
}

#[tokio::test]
async fn a_host_moved_after_it_was_resolved_starts_nothing_on_the_old_login() {
    let (connector, forwards) = setup();
    let db = login("db", "db.internal");
    // The shell resolved the host, then an edit moved it.
    let mark = forwards.host_mark(&db.host);
    forwards.stop_host(&db.host);
    let id = ForwardId::parse("pg").unwrap();
    let ticket = forwards.ticket_since(&db.host, &id, mark);
    let started = forwards
        .start(&db, &rule("pg", 5432), ticket, prompter())
        .await;
    assert!(matches!(started, Err(StartError::Stopped)));
    assert_eq!(connector.logins(), 0);
}
