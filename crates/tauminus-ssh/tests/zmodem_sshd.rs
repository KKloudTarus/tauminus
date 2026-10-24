// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Files lrzsz's `sz` sends over a real SSH shell, through the session
//! pump (ADR 0025). Needs an OpenSSH server with lrzsz and the files
//! `/tmp/a.txt` and `/tmp/big.bin`, as `ci/password-sshd.sh` starts it in
//! a container: `TAUMINUS_SZ_SSHD=<port> <user> <password>`. Ignored
//! unless asked for.

#![cfg(unix)]

mod support;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};
use support::{Scratch, ScriptedPrompter};
use tauminus_app::ports::{
    Answer, OutputSink, PartFile, Prompter, ReceiveFolder, SinkClosed, SshConnector, TransferEvent,
    TransferHost,
};
use tauminus_app::{SessionConfig, SessionManager};
use tauminus_domain::{
    AuthMethod, AuthOptions, Endpoint, HostName, Route, SessionId, SessionKind, SessionMeta,
    SshTarget, TerminalSize, UserName,
};
use tauminus_ssh::{AgentSource, HostKeyStore, RusshConnector};

#[derive(Debug, Default)]
struct Host {
    offers: AtomicUsize,
    events: Mutex<Vec<TransferEvent>>,
}

impl TransferHost for Host {
    fn offered(&self, _id: SessionId, _offer: u64) {
        self.offers.fetch_add(1, Ordering::Relaxed);
    }

    fn event(&self, _id: SessionId, event: TransferEvent) {
        self.events.lock().unwrap().push(event);
    }
}

#[derive(Debug, Default, Clone)]
struct Folder(Arc<Mutex<HashMap<String, Vec<u8>>>>);

#[derive(Debug)]
struct Part(String, Vec<u8>, Folder);

impl ReceiveFolder for Folder {
    fn create(&mut self, name: &str) -> std::io::Result<(String, Box<dyn PartFile>)> {
        Ok((
            name.to_owned(),
            Box::new(Part(name.to_owned(), Vec::new(), self.clone())),
        ))
    }

    fn shown(&self) -> String {
        "memory".into()
    }
}

impl PartFile for Part {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.1.extend_from_slice(bytes);
        Ok(())
    }

    fn finish(self: Box<Self>) -> std::io::Result<()> {
        let Self(name, bytes, folder) = *self;
        folder.0.lock().unwrap().insert(name, bytes);
        Ok(())
    }

    fn discard(self: Box<Self>) {}
}

#[derive(Clone, Default)]
struct Screen(Arc<Mutex<Vec<u8>>>);

impl Screen {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl OutputSink for Screen {
    fn send_output(&self, batch: Vec<u8>) -> Result<(), SinkClosed> {
        self.0.lock().unwrap().extend_from_slice(&batch);
        Ok(())
    }

    fn send_exit(&self, _code: Option<u32>) {}
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..3000 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never: {what}");
}

/// A shell on the server, in a session that takes transfers.
async fn shell(scratch: &Scratch) -> (SessionManager, SessionId, Arc<Host>, Screen) {
    let value = std::env::var("TAUMINUS_SZ_SSHD").expect("TAUMINUS_SZ_SSHD");
    let mut parts = value.split(' ');
    let (Some(port), Some(user), Some(password)) = (parts.next(), parts.next(), parts.next())
    else {
        panic!("TAUMINUS_SZ_SSHD must be `<port> <user> <password>`");
    };
    let connector = RusshConnector::new(
        HostKeyStore::new(vec![], scratch.path("known_hosts")),
        None,
        AgentSource::None,
    );
    let target = SshTarget::new(
        Endpoint::new(
            UserName::parse(user).unwrap(),
            HostName::parse("127.0.0.1").unwrap(),
            port.parse().unwrap(),
        )
        .unwrap(),
        vec![],
        Route::Direct,
        AuthOptions {
            methods: vec![AuthMethod::Password],
            identity_files: vec![],
            identities_only: true,
        },
    )
    .unwrap();
    let prompter = ScriptedPrompter::new(vec![
        Answer::TrustHostKey { remember: false },
        Answer::Secret(password.to_owned().into()),
    ]);
    let stream = connector
        .connect(
            &target,
            TerminalSize::new(120, 40).unwrap(),
            Arc::clone(&prompter) as Arc<dyn Prompter>,
        )
        .await
        .unwrap();

    let sessions = SessionManager::new(SessionConfig::default());
    let host = Arc::new(Host::default());
    sessions.set_transfer_host(Arc::clone(&host) as Arc<dyn TransferHost>);
    let screen = Screen::default();
    let id = sessions.attach(
        sessions.reserve().unwrap(),
        stream,
        screen.clone(),
        SessionMeta {
            transfers: true,
            ..SessionMeta::of(SessionKind::Ssh)
        },
    );
    (sessions, id, host, screen)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an sshd with lrzsz: TAUMINUS_SZ_SSHD"]
async fn sz_over_ssh_arrives_whole_and_the_shell_comes_back() {
    let scratch = Scratch::new("zmodem");
    let (sessions, id, host, screen) = shell(&scratch).await;
    sessions
        .write_text(id, "sz /tmp/a.txt /tmp/big.bin\n".into())
        .await
        .unwrap();
    until("the offer", || host.offers.load(Ordering::Relaxed) == 1).await;
    // Keys while it waits are refused, not sent into the protocol.
    assert!(sessions.write_text(id, "x".into()).await.is_err());
    let folder = Folder::default();
    sessions
        .answer_transfer(id, 1, Some(Box::new(folder.clone())))
        .unwrap();
    until("the end", || {
        matches!(
            host.events.lock().unwrap().last(),
            Some(TransferEvent::Finished { .. } | TransferEvent::Failed { .. })
        )
    })
    .await;
    let events = host.events.lock().unwrap().clone();
    assert!(
        matches!(events.last(), Some(TransferEvent::Finished { files, .. }) if files.len() == 2),
        "{events:?}"
    );
    let big = folder.0.lock().unwrap().get("big.bin").cloned().unwrap();
    assert_eq!(big.len(), 3_000_000);
    assert_eq!(folder.0.lock().unwrap().get("a.txt").unwrap(), b"hello\n");

    // The shell is back: its own hash of the file matches what came.
    let hex = Sha256::digest(&big)
        .iter()
        .fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        });
    sessions
        .write_text(id, "sha256sum /tmp/big.bin\n".into())
        .await
        .unwrap();
    until("the hash", || screen.text().contains(&hex)).await;
    // No file bytes reached the view: the offer's start, the commands,
    // and their output only.
    assert!(screen.text().len() < 4096, "{}", screen.text().len());
    sessions.close(id).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an sshd with lrzsz: TAUMINUS_SZ_SSHD"]
async fn ctrl_c_stops_sz_and_leaves_the_shell_usable() {
    let scratch = Scratch::new("zmodem-cancel");
    let (sessions, id, host, screen) = shell(&scratch).await;
    sessions
        .write_text(id, "sz /tmp/big.bin\n".into())
        .await
        .unwrap();
    until("the offer", || host.offers.load(Ordering::Relaxed) == 1).await;
    sessions
        .answer_transfer(id, 1, Some(Box::new(Folder::default())))
        .unwrap();
    until("a file", || {
        host.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, TransferEvent::File { .. }))
    })
    .await;
    // Typed with other keys: the write is not sent, and stops the transfer.
    sessions.write_text(id, "q\u{3}".into()).await.unwrap();
    until("the stop", || {
        matches!(
            host.events.lock().unwrap().last(),
            Some(TransferEvent::Failed { .. } | TransferEvent::Finished { .. })
        )
    })
    .await;
    // The shell answers again: `sz` has gone.
    for _ in 0..50 {
        if sessions
            .write_text(id, "echo back-$((6*7))\n".into())
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    until("the shell", || screen.text().contains("back-42")).await;
    sessions.close(id).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an sshd with lrzsz: TAUMINUS_SZ_SSHD"]
async fn a_file_too_large_to_take_stops_sz_at_once() {
    // `/tmp/huge.bin` is 5 GiB, sparse: past the 32-bit sizes Zmodem has.
    let scratch = Scratch::new("zmodem-huge");
    let (sessions, id, host, screen) = shell(&scratch).await;
    sessions
        .write_text(id, "sz /tmp/huge.bin; echo sz-ended-$?\n".into())
        .await
        .unwrap();
    until("the offer", || host.offers.load(Ordering::Relaxed) == 1).await;
    let folder = Folder::default();
    sessions
        .answer_transfer(id, 1, Some(Box::new(folder.clone())))
        .unwrap();
    until("the stop", || {
        matches!(
            host.events.lock().unwrap().last(),
            Some(TransferEvent::Failed { .. })
        )
    })
    .await;
    // The host heard the cancel: `sz` ends without retrying for minutes.
    until("sz ended", || screen.text().contains("sz-ended-")).await;
    assert!(folder.0.lock().unwrap().is_empty());
    sessions.close(id).unwrap();
}
