// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::{AtomicUsize, Ordering};

use tauminus_domain::{Host, HostId, HostName, Label};
use tokio::sync::Notify;
use zeroize::Zeroizing;

use super::*;
use crate::fakes::{FakeHostStore, FakeSnippetStore, FakeTrustStore};
use crate::ports::{BoxFuture, SyncData, SyncKeyBytes, SyncPart, SyncRemote};
use crate::vault::tests::{secret, vault};

pub(crate) const PASSPHRASE: &str = "correct horse battery";

/// Merges by adding host `theirs`; waits on `hold` first, when given.
struct Remote {
    hold: Option<Arc<Notify>>,
    fail: bool,
}

impl SyncRemote for Remote {
    fn merge(&self, local: SyncData) -> BoxFuture<'_, Result<Merged, SyncError>> {
        Box::pin(async move {
            if let Some(hold) = &self.hold {
                hold.notified().await;
            }
            if self.fail {
                return Err(SyncError::Target("unreachable".into()));
            }
            let mut data = local;
            data.hosts
                .put_host(Host::new(
                    HostId::parse("theirs").unwrap(),
                    Label::parse("theirs").unwrap(),
                    HostName::parse("db.internal").unwrap(),
                ))
                .unwrap();
            Ok(Merged {
                data,
                repairs: vec![Repair::HostToRoot(HostId::parse("theirs").unwrap())],
                clashes: vec![],
                notices: vec!["a device went back".into()],
            })
        })
    }

    fn adopt(&self, _: SyncPart) -> BoxFuture<'_, Result<(), SyncError>> {
        Box::pin(async { Ok(()) })
    }

    fn publish(&self) -> BoxFuture<'_, Result<(), SyncError>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Default)]
pub(crate) struct Connector {
    pub(crate) hold: Option<Arc<Notify>>,
    pub(crate) fail: std::sync::atomic::AtomicBool,
    pub(crate) wiped: AtomicUsize,
    pub(crate) closed: AtomicUsize,
    pub(crate) opened: StdMutex<Vec<(SyncTarget, [u8; 32])>>,
}

impl SyncConnector for Connector {
    fn create<'a>(
        &'a self,
        _: &'a SyncTarget,
        passphrase: &'a Secret,
    ) -> BoxFuture<'a, Result<SyncKeyBytes, SyncError>> {
        Box::pin(async move {
            if passphrase.chars().count() < 12 {
                return Err(SyncError::Sealed("short".into()));
            }
            Ok(Zeroizing::new([1; 32]))
        })
    }

    fn join<'a>(
        &'a self,
        _: &'a SyncTarget,
        passphrase: &'a Secret,
    ) -> BoxFuture<'a, Result<SyncKeyBytes, SyncError>> {
        Box::pin(async move {
            if passphrase.as_str() != PASSPHRASE {
                return Err(SyncError::Sealed("wrong passphrase".into()));
            }
            Ok(Zeroizing::new([2; 32]))
        })
    }

    fn open(
        &self,
        target: &SyncTarget,
        key: &SyncKeyBytes,
    ) -> Result<Arc<dyn SyncRemote>, SyncError> {
        self.opened.lock().unwrap().push((target.clone(), **key));
        Ok(Arc::new(Remote {
            hold: self.hold.clone(),
            fail: self.fail.load(Ordering::SeqCst),
        }))
    }

    fn close(&self) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }

    fn wipe(&self) -> BoxFuture<'_, Result<(), SyncError>> {
        self.wiped.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

pub(crate) async fn service(
    connector: Arc<Connector>,
) -> (Arc<Vault>, Arc<HostBook>, Arc<SyncService>) {
    let (_, vault) = vault();
    let hosts = Arc::new(
        HostBook::open(Arc::new(FakeHostStore::default()))
            .await
            .unwrap(),
    );
    let snippets = Arc::new(
        SnippetBook::open(Arc::new(FakeSnippetStore::default()))
            .await
            .unwrap(),
    );
    let trust = Arc::new(
        Trust::open(Arc::new(FakeTrustStore::default()))
            .await
            .unwrap(),
    );
    let service = Arc::new(SyncService::new(
        Arc::clone(&vault),
        Arc::clone(&hosts),
        snippets,
        trust,
        connector,
    ));
    (vault, hosts, service)
}

fn git() -> SyncTarget {
    SyncTarget::Git(secret("https://me:ghp_token@example.com/me/sync.git"))
}

#[tokio::test]
async fn create_keeps_the_target_and_key_in_the_vault_after_a_fresh_start() {
    let connector = Arc::new(Connector::default());
    let (vault, _, service) = service(Arc::clone(&connector)).await;
    assert_eq!(service.status().await.setup, Setup::Locked);
    let locked = service.create(git(), secret(PASSPHRASE)).await;
    assert!(
        matches!(locked, Err(SyncServiceError::Vault(VaultError::Locked))),
        "{locked:?}"
    );
    vault.create(secret("vault password")).await.unwrap();
    assert_eq!(service.status().await.setup, Setup::Off);
    let short = service.create(git(), secret("short")).await;
    assert!(
        matches!(short, Err(SyncServiceError::Sync(SyncError::Sealed(_)))),
        "{short:?}"
    );
    assert!(vault.sync_setup().await.unwrap().is_none());

    service.create(git(), secret(PASSPHRASE)).await.unwrap();
    assert!(
        connector.wiped.load(Ordering::SeqCst) >= 1,
        "an earlier target's state goes"
    );
    let setup = vault.sync_setup().await.unwrap().unwrap();
    assert_eq!(setup.target, git());
    assert_eq!(*setup.key, [1; 32]);
    assert_eq!(
        service.status().await.setup,
        Setup::On {
            through: Through::Git,
            shown: "https://example.com/me/sync.git".into()
        }
    );
    let again = service.join(git(), secret(PASSPHRASE)).await;
    assert!(matches!(again, Err(SyncServiceError::On)), "{again:?}");
}

#[tokio::test]
async fn a_run_saves_the_merge_and_reports_it_and_its_failure() {
    let connector = Arc::new(Connector::default());
    let (vault, hosts, service) = service(Arc::clone(&connector)).await;
    vault.create(secret("vault password")).await.unwrap();
    assert!(matches!(service.run().await, Err(SyncServiceError::Off)));
    let wrong = service.join(git(), secret("not the passphrase")).await;
    assert!(
        matches!(wrong, Err(SyncServiceError::Sync(SyncError::Sealed(_)))),
        "{wrong:?}"
    );
    service.join(git(), secret(PASSPHRASE)).await.unwrap();

    let summary = service.run().await.unwrap();
    assert_eq!(summary.notices, ["a device went back"]);
    assert_eq!(
        summary.repairs,
        ["host theirs moved to the top: its group was gone"]
    );
    assert!(
        hosts
            .snapshot()
            .await
            .host(&HostId::parse("theirs").unwrap())
            .is_some()
    );
    assert_eq!(connector.opened.lock().unwrap()[0], (git(), [2; 32]));
    let status = service.status().await;
    assert!(!status.running);
    let last = status.last.unwrap();
    assert!(last.changed, "the merge saved a host");
    assert_eq!(last.outcome, Ok(summary));
    service.run().await.unwrap();
    assert!(!service.status().await.last.unwrap().changed, "nothing new");

    connector.fail.store(true, Ordering::SeqCst);
    assert!(service.run().await.is_err());
    let outcome = service.status().await.last.unwrap().outcome;
    assert_eq!(outcome, Err("the sync target: unreachable".into()));

    vault.lock().await;
    let locked = service.run().await;
    assert!(
        matches!(locked, Err(SyncServiceError::Vault(VaultError::Locked))),
        "{locked:?}"
    );
}

#[tokio::test]
async fn one_sync_runs_at_a_time_and_leaving_waits_for_it() {
    let hold = Arc::new(Notify::new());
    let connector = Arc::new(Connector {
        hold: Some(Arc::clone(&hold)),
        ..Connector::default()
    });
    let (vault, _, service) = service(Arc::clone(&connector)).await;
    vault.create(secret("vault password")).await.unwrap();
    service
        .create(SyncTarget::Folder("/srv/sync".into()), secret(PASSPHRASE))
        .await
        .unwrap();
    let running = tokio::spawn({
        let service = Arc::clone(&service);
        async move { service.run().await }
    });
    while !service.status().await.running {
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        service.run().await,
        Err(SyncServiceError::Running)
    ));
    let leaving = tokio::spawn({
        let service = Arc::clone(&service);
        async move { service.leave().await }
    });
    tokio::task::yield_now().await;
    assert!(vault.sync_setup().await.unwrap().is_some(), "leaving waits");
    hold.notify_one();
    running.await.unwrap().unwrap();
    leaving.await.unwrap().unwrap();
    assert!(vault.sync_setup().await.unwrap().is_none());
    assert!(connector.closed.load(Ordering::SeqCst) >= 1);
    assert_eq!(
        service.status().await,
        Status {
            setup: Setup::Off,
            running: false,
            last: None
        }
    );
}

#[test]
fn a_remote_is_shown_without_its_password_or_token() {
    for (url, expected) in [
        (
            "https://me:tok@example.com/a.git",
            "https://example.com/a.git",
        ),
        ("https://tok@example.com", "https://example.com"),
        (
            "ssh://git@example.com:22/a@b.git",
            "ssh://example.com:22/a@b.git",
        ),
        ("git@example.com:me/a.git", "git@example.com:me/a.git"),
        ("https://example.com/a.git", "https://example.com/a.git"),
        (
            "https://example.com/a.git?private_token=tok",
            "https://example.com/a.git",
        ),
        ("https://example.com/a.git#tok", "https://example.com/a.git"),
        ("https://u@example.com?tok/x", "https://example.com"),
    ] {
        assert_eq!(shown(&SyncTarget::Git(secret(url))), expected, "{url}");
    }
    assert_eq!(shown(&SyncTarget::Folder("/srv/s".into())), "/srv/s");
    let s3 = |prefix: &str| {
        SyncTarget::S3(crate::ports::S3Target {
            endpoint: "https://s3.example.com/".into(),
            region: "auto".into(),
            bucket: "team".into(),
            prefix: prefix.into(),
            key_id: "AKID".into(),
            secret: secret("s3-secret"),
        })
    };
    assert_eq!(shown(&s3("")), "team at https://s3.example.com");
    assert_eq!(shown(&s3("/a/b/")), "team/a/b at https://s3.example.com");
    assert!(!format!("{:?}", s3("")).contains("s3-secret"));
}
