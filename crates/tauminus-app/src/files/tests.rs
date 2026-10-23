// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::*;
use crate::fakes::{FakeConnector, login, prompter};
use crate::ports::SshConnector;

fn files() -> (Arc<FakeConnector>, Files) {
    let connector = Arc::new(FakeConnector::default());
    let connections = Arc::new(Connections::new(
        Arc::clone(&connector) as Arc<dyn SshConnector>
    ));
    (connector, Files::new(connections))
}

fn sftp(connector: &FakeConnector) -> Arc<crate::fakes::FakeSftp> {
    Arc::clone(&connector.made.lock().unwrap()[0].sftp)
}

async fn settle(files: &Files) -> Vec<TransferState> {
    settle_within(files, Duration::from_secs(5)).await
}

async fn settle_within(files: &Files, limit: Duration) -> Vec<TransferState> {
    tokio::time::timeout(limit, async {
        loop {
            let states = files.transfers();
            if states
                .iter()
                .all(|t| !matches!(t.status, TransferStatus::Waiting | TransferStatus::Running))
            {
                return states;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("transfers finish: {:?}", files.transfers()))
}

#[tokio::test]
async fn a_download_lands_under_its_checked_name_in_the_folder() {
    let (connector, files) = files();
    let id = files
        .open(&login("db", "db.internal"), prompter())
        .await
        .unwrap();
    files
        .download(id, "/var/log/app.log", Path::new("/tmp/into"), false)
        .unwrap();
    let states = settle(&files).await;
    assert_eq!(states[0].status, TransferStatus::Done);
    assert_eq!(states[0].name, "app.log");
    let job = sftp(&connector).downloads.lock().unwrap()[0].clone();
    assert_eq!(job.local, Path::new("/tmp/into/app.log"));
    assert_eq!(job.host, "db");
}

#[tokio::test]
async fn names_that_would_leave_the_folder_are_refused_before_anything_runs() {
    let (connector, files) = files();
    let id = files
        .open(&login("db", "db.internal"), prompter())
        .await
        .unwrap();
    for remote in ["/srv/..", "/srv/.", "/srv/a\nb", "/srv/\u{202E}txt.exe"] {
        let result = files.download(id, remote, Path::new("/tmp/into"), false);
        assert!(matches!(result, Err(FilesError::Name(..))), "{remote:?}");
    }
    assert!(sftp(&connector).downloads.lock().unwrap().is_empty());
    assert_eq!(files.transfers(), Vec::new());
}

#[tokio::test]
async fn transfers_take_turns_per_session_and_four_run_at_most() {
    let (connector, files) = files();
    let ids: Vec<u64> = futures_open(&files, 6).await;
    let fake = sftp(&connector);
    *fake.delay.lock().unwrap() = Duration::from_millis(30);
    for &id in &ids {
        for n in 0..3 {
            files
                .upload(
                    id,
                    Path::new(&format!("/tmp/f{n}")),
                    &format!("/srv/{id}"),
                    true,
                    "/tmp/state".into(),
                )
                .unwrap();
        }
    }
    // The same target on the same host from another session waits for none:
    // it is refused while the first runs.
    let again = files.upload(
        ids[1],
        Path::new("/tmp/f0"),
        &format!("/srv/{}", ids[0]),
        true,
        "/tmp/state".into(),
    );
    assert!(matches!(again, Err(FilesError::Busy(_))), "{again:?}");
    let states = settle(&files).await;
    assert_eq!(states.len(), 18);
    assert!(states.iter().all(|t| t.status == TransferStatus::Done));
    // Six sessions share one connection and its fake, so the count is
    // across sessions: never more than four at once.
    assert_eq!(fake.most_at_once.load(Ordering::SeqCst), MAX_RUNNING);
}

async fn futures_open(files: &Files, n: usize) -> Vec<u64> {
    let mut ids = Vec::new();
    for _ in 0..n {
        ids.push(
            files
                .open(&login("db", "db.internal"), prompter())
                .await
                .unwrap(),
        );
    }
    ids
}

#[tokio::test]
async fn one_session_runs_a_few_transfers_at_a_time() {
    let (connector, files) = files();
    let id = files
        .open(&login("db", "db.internal"), prompter())
        .await
        .unwrap();
    let fake = sftp(&connector);
    *fake.delay.lock().unwrap() = Duration::from_millis(20);
    for n in 0..8 {
        files
            .download(id, &format!("/srv/f{n}"), Path::new("/tmp"), true)
            .unwrap();
    }
    settle(&files).await;
    assert_eq!(fake.most_at_once.load(Ordering::SeqCst), PER_SESSION);
}

#[tokio::test]
async fn cancelling_a_running_or_waiting_transfer_stops_it() {
    let (connector, files) = files();
    let id = files
        .open(&login("db", "db.internal"), prompter())
        .await
        .unwrap();
    *sftp(&connector).delay.lock().unwrap() = Duration::from_secs(60);
    let running = files
        .download(id, "/srv/big", Path::new("/tmp"), true)
        .unwrap();
    let waiting = files
        .download(id, "/srv/next", Path::new("/tmp"), true)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(files.cancel(running));
    assert!(files.cancel(waiting));
    let states = settle(&files).await;
    assert!(states.iter().all(|t| t.status == TransferStatus::Cancelled));
    assert!(!files.cancel(running), "already finished");
}

#[tokio::test]
async fn closing_a_session_cancels_its_transfers_and_forgets_it() {
    let (connector, files) = files();
    let id = files
        .open(&login("db", "db.internal"), prompter())
        .await
        .unwrap();
    *sftp(&connector).delay.lock().unwrap() = Duration::from_secs(60);
    files
        .download(id, "/srv/big", Path::new("/tmp"), true)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    files.close(id);
    assert_eq!(settle(&files).await[0].status, TransferStatus::Cancelled);
    assert!(matches!(files.sftp(id), Err(FilesError::NoSession)));
    assert!(matches!(
        files.download(id, "/srv/x", Path::new("/tmp"), true),
        Err(FilesError::NoSession)
    ));
}

#[tokio::test]
async fn sessions_are_bounded() {
    let (_, files) = files();
    futures_open(&files, MAX_SESSIONS).await;
    let more = files.open(&login("db", "db.internal"), prompter()).await;
    assert!(matches!(more, Err(FilesError::TooManySessions)));
}

#[tokio::test]
async fn a_dropped_connection_logs_in_again_and_resumes() {
    let (connector, files) = files();
    let id = files
        .open(&login("db", "db.internal"), prompter())
        .await
        .unwrap();
    let fake = sftp(&connector);
    fake.drops.store(2, Ordering::SeqCst);
    files
        .download(id, "/srv/big", Path::new("/tmp"), true)
        .unwrap();
    let states = settle(&files).await;
    assert_eq!(states[0].status, TransferStatus::Done);
    // Twice dropped, then through: the same job three times.
    assert_eq!(fake.downloads.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn a_connection_that_keeps_dropping_fails_the_transfer_after_three_tries() {
    let (connector, files) = files();
    let id = files
        .open(&login("db", "db.internal"), prompter())
        .await
        .unwrap();
    let fake = sftp(&connector);
    fake.drops.store(100, Ordering::SeqCst);
    files
        .upload(id, Path::new("/tmp/f"), "/srv", true, "/tmp/state".into())
        .unwrap();
    let states = settle(&files).await;
    assert!(
        matches!(&states[0].status, TransferStatus::Failed(reason) if reason.contains("closed")),
        "{:?}",
        states[0].status
    );
    assert_eq!(
        fake.uploads.lock().unwrap().len(),
        4,
        "the first try and three more"
    );
}

#[tokio::test]
async fn two_downloads_to_one_local_file_are_not_run_together() {
    let (connector, files) = files();
    let id = files
        .open(&login("db", "db.internal"), prompter())
        .await
        .unwrap();
    *sftp(&connector).delay.lock().unwrap() = Duration::from_millis(50);
    files
        .download(id, "/srv/a.log", Path::new("/tmp"), true)
        .unwrap();
    let again = files.download(id, "/var/a.log", Path::new("/tmp"), true);
    assert!(matches!(again, Err(FilesError::Busy(_))), "{again:?}");
    settle(&files).await;
    // Once the first ends, the file may be downloaded again.
    files
        .download(id, "/var/a.log", Path::new("/tmp"), true)
        .unwrap();
    settle(&files).await;
}

#[tokio::test]
async fn a_failed_login_while_reconnecting_uses_the_tries_left() {
    let (connector, files) = files();
    let id = files
        .open(&login("db", "db.internal"), prompter())
        .await
        .unwrap();
    let fake = sftp(&connector);
    fake.drops.store(2, Ordering::SeqCst);
    // The live connection goes away and the network is down at first.
    connector.made.lock().unwrap()[0].drop_link();
    connector.unreachable.store(true, Ordering::SeqCst);
    files
        .download(id, "/srv/big", Path::new("/tmp"), true)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(8)).await;
    connector.unreachable.store(false, Ordering::SeqCst);
    let states = settle_within(&files, Duration::from_secs(5)).await;
    assert_eq!(states[0].status, TransferStatus::Done, "{states:?}");
}

#[tokio::test]
async fn two_uploads_to_one_remote_file_are_not_run_together() {
    let (connector, files) = files();
    let id = files
        .open(&login("db", "db.internal"), prompter())
        .await
        .unwrap();
    *sftp(&connector).delay.lock().unwrap() = Duration::from_millis(50);
    files
        .upload(id, Path::new("/tmp/a/x"), "/srv", true, "/tmp/s".into())
        .unwrap();
    let again = files.upload(id, Path::new("/tmp/b/x"), "/srv", true, "/tmp/s".into());
    assert!(matches!(again, Err(FilesError::Busy(_))), "{again:?}");
    settle(&files).await;
}

#[tokio::test]
async fn a_refused_login_stops_the_retries_for_the_whole_session() {
    let (connector, files) = files();
    let id = files
        .open(&login("db", "db.internal"), prompter())
        .await
        .unwrap();
    let fake = sftp(&connector);
    fake.drops.store(100, Ordering::SeqCst);
    connector.made.lock().unwrap()[0].drop_link();
    // Every new login is cancelled, as a user closing the prompt.
    connector.refuse.store(true, Ordering::SeqCst);
    for n in 0..3 {
        files
            .download(id, &format!("/srv/f{n}"), Path::new("/tmp"), true)
            .unwrap();
    }
    let states = settle(&files).await;
    assert!(
        states
            .iter()
            .all(|t| matches!(t.status, TransferStatus::Failed(_)))
    );
    // One login was tried for the session, not three per transfer.
    assert_eq!(connector.logins(), 1, "only the first login succeeded");
    assert!(fake.downloads.lock().unwrap().len() <= 3, "each tried once");
}
