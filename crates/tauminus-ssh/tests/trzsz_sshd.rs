// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Files trzsz's `tsz` sends over a real SSH shell, through the session
//! pump (ADR 0031): the Python `tsz` (pip trzsz 1.1.7) and trzsz-go's
//! (1.2.0, at `/opt/go/tsz`), plain and inside tmux on the host. Needs an
//! OpenSSH server with both, tmux, and the files `/tmp/a.txt` and
//! `/tmp/big.bin`: `TAUMINUS_TRZSZ_SSHD=<port> <user> <password>`.
//! Ignored unless asked for.

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
    Answer, OutputSink, PartFile, Prompter, ReceiveFolder, SinkClosed, SshConnector,
    TransferAnswer, TransferEvent, TransferHost, TransferKind,
};
use tauminus_app::{SessionConfig, SessionManager};
use tauminus_domain::{
    AuthMethod, AuthOptions, Endpoint, HostName, Route, SessionId, SessionKind, SessionMeta,
    SshTarget, TerminalSize, UserName, Wants,
};
use tauminus_ssh::{AgentSource, HostKeyStore, RusshConnector};

#[derive(Debug, Default)]
struct Host {
    offers: AtomicUsize,
    events: Mutex<Vec<TransferEvent>>,
}

impl TransferHost for Host {
    fn offered(&self, _id: SessionId, _offer: u64, _wants: Wants, _kind: TransferKind) {
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
    let value = std::env::var("TAUMINUS_TRZSZ_SSHD").expect("TAUMINUS_TRZSZ_SSHD");
    let mut parts = value.split(' ');
    let (Some(port), Some(user), Some(password)) = (parts.next(), parts.next(), parts.next())
    else {
        panic!("TAUMINUS_TRZSZ_SSHD must be `<port> <user> <password>`");
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
            identity_agent: None,
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

/// The hex SHA-256 of `bytes`.
fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

/// Types `command` until `seen` shows: trzsz reads what is typed for
/// half a second after it ends, and drops it.
async fn asked(
    sessions: &SessionManager,
    id: SessionId,
    screen: &Screen,
    command: &str,
    seen: &str,
) {
    for _ in 0..60 {
        let _ = sessions.write_text(id, format!("{command}\n")).await;
        for _ in 0..10 {
            if screen.text().contains(seen) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    panic!("never: {seen} after {command}: {}", screen.text());
}

/// Runs `command`, takes what it sends, and checks it against the host's
/// own hash; returns the session for more.
async fn take(command: &str, name: &str) -> (SessionManager, SessionId, Screen) {
    let scratch = Scratch::new("trzsz");
    let (sessions, id, host, screen) = shell(&scratch).await;
    sessions
        .write_text(id, format!("{command}\n"))
        .await
        .unwrap();
    until("the offer", || host.offers.load(Ordering::Relaxed) == 1).await;
    assert!(
        sessions.write_text(id, "x".into()).await.is_err(),
        "keys wait"
    );
    let folder = Folder::default();
    sessions
        .answer_transfer(id, 1, TransferAnswer::Receive(Box::new(folder.clone())))
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
        matches!(events.last(), Some(TransferEvent::Finished { files, .. }) if files == &[name]),
        "{events:?}"
    );
    let got = folder.0.lock().unwrap().get(name).cloned().unwrap();
    // The shell is back once tsz has read what was typed meanwhile: its
    // hash of the file matches what came.
    let sum = hex(&got);
    asked(
        &sessions,
        id,
        &screen,
        &format!("sha256sum /tmp/{name}"),
        &sum,
    )
    .await;
    (sessions, id, screen)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an sshd with trzsz: TAUMINUS_TRZSZ_SSHD"]
async fn python_tsz_sends_a_large_file_whole() {
    let (sessions, id, screen) = take("tsz /tmp/big.bin", "big.bin").await;
    // No file bytes reached the view.
    assert!(screen.text().len() < 8192, "{}", screen.text().len());
    sessions.close(id).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an sshd with trzsz: TAUMINUS_TRZSZ_SSHD"]
async fn go_tsz_sends_a_large_file_whole() {
    let (sessions, id, screen) = take("/opt/go/tsz /tmp/big.bin", "big.bin").await;
    assert!(screen.text().len() < 8192, "{}", screen.text().len());
    sessions.close(id).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an sshd with trzsz: TAUMINUS_TRZSZ_SSHD"]
async fn go_tsz_inside_tmux_on_the_host_sends_its_file() {
    let (sessions, id, _) = take(
        "tmux new-session -s tz$RANDOM '/opt/go/tsz /tmp/big.bin; bash'",
        "big.bin",
    )
    .await;
    sessions.write_text(id, "exit\n".into()).await.ok();
    sessions.close(id).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an sshd with trzsz: TAUMINUS_TRZSZ_SSHD"]
async fn ctrl_c_stops_tsz_and_leaves_the_shell_usable() {
    for command in ["tsz /tmp/big.bin", "/opt/go/tsz /tmp/big.bin"] {
        let scratch = Scratch::new("trzsz-cancel");
        let (sessions, id, host, screen) = shell(&scratch).await;
        sessions
            .write_text(id, format!("{command}\n"))
            .await
            .unwrap();
        until("the offer", || host.offers.load(Ordering::Relaxed) == 1).await;
        sessions
            .answer_transfer(id, 1, TransferAnswer::Receive(Box::new(Folder::default())))
            .unwrap();
        until("a file", || {
            host.events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, TransferEvent::File { .. }))
        })
        .await;
        sessions.write_text(id, "\u{3}".into()).await.unwrap();
        until("the stop", || {
            matches!(
                host.events.lock().unwrap().last(),
                Some(TransferEvent::Failed { .. })
            )
        })
        .await;
        asked(&sessions, id, &screen, "echo back-$((6*7))", "back-42").await;
        // Nothing of the file reached the view.
        assert!(
            screen.text().len() < 8192,
            "{command}: {}",
            screen.text().len()
        );
        sessions.close(id).unwrap();
    }
}

/// A local file in memory, as the desktop's opened file would be.
#[derive(Debug)]
struct MemFile(&'static str, Vec<u8>);

impl tauminus_app::ports::OutgoingFile for MemFile {
    fn name(&self) -> &str {
        self.0
    }

    fn size(&self) -> u64 {
        self.1.len() as u64
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let from = usize::try_from(offset).unwrap().min(self.1.len());
        let n = buf.len().min(self.1.len() - from);
        buf[..n].copy_from_slice(&self.1[from..from + n]);
        Ok(n)
    }
}

/// Bytes that look like nothing in particular, the same each run.
fn noise(len: usize) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

/// Sends a file to `trz` (`command`, run in a fresh folder `dir`), and
/// checks the host's hash of what it saved.
async fn give(command: &str, dir: &str) {
    let scratch = Scratch::new("trz");
    let (sessions, id, host, screen) = shell(&scratch).await;
    sessions
        .write_text(
            id,
            format!("rm -rf {dir}; mkdir {dir}; cd {dir} && {command}\n"),
        )
        .await
        .unwrap();
    until("the offer", || host.offers.load(Ordering::Relaxed) == 1).await;
    let big = noise(4_000_000);
    sessions
        .answer_transfer(
            id,
            1,
            TransferAnswer::Send(vec![
                Box::new(MemFile("small.txt", b"hello to trz\n".to_vec())),
                Box::new(MemFile("big.bin", big.clone())),
            ]),
        )
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
    asked(
        &sessions,
        id,
        &screen,
        &format!("sha256sum {dir}/big.bin"),
        &hex(&big),
    )
    .await;
    sessions.close(id).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an sshd with trzsz: TAUMINUS_TRZSZ_SSHD"]
async fn python_trz_takes_files_whole() {
    give("trz", "/tmp/up-py").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an sshd with trzsz: TAUMINUS_TRZSZ_SSHD"]
async fn go_trz_takes_files_whole() {
    give("/opt/go/trz", "/tmp/up-go").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an sshd with trzsz: TAUMINUS_TRZSZ_SSHD"]
async fn trz_killed_mid_file_runs_no_line_of_it_in_bash_or_zsh() {
    // Each line a command, should a shell run what it reads.
    let mut lines = Vec::new();
    while lines.len() < 50_000_000 {
        lines.extend_from_slice(b"touch /tmp/pwned-trz\n");
    }
    for shell_name in ["bash", "zsh"] {
        let scratch = Scratch::new("trz-kill");
        let (sessions, id, host, screen) = shell(&scratch).await;
        sessions
            .write_text(id, format!("rm -f /tmp/pwned-trz; exec {shell_name}\n"))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        sessions
            .write_text(
                id,
                "rm -rf /tmp/k; mkdir /tmp/k; cd /tmp/k && timeout -s KILL 2 /opt/go/trz\n".into(),
            )
            .await
            .unwrap();
        until("the offer", || host.offers.load(Ordering::Relaxed) == 1).await;
        sessions
            .answer_transfer(
                id,
                1,
                TransferAnswer::Send(vec![Box::new(MemFile("lines.sh", lines.clone()))]),
            )
            .unwrap();
        until("the stop", || {
            matches!(
                host.events.lock().unwrap().last(),
                Some(TransferEvent::Failed { .. } | TransferEvent::Finished { .. })
            )
        })
        .await;
        let events = host.events.lock().unwrap().clone();
        assert!(
            matches!(events.last(), Some(TransferEvent::Failed { .. })),
            "{shell_name}: {events:?}"
        );
        // Enter with nothing typed, then ask: nothing ran.
        sessions.write_text(id, "\n".into()).await.ok();
        asked(
            &sessions,
            id,
            &screen,
            "test -e /tmp/pwned-trz && echo RAN-$((1+1)) || echo CLEAN-$((2+2))",
            "CLEAN-4",
        )
        .await;
        assert!(!screen.text().contains("RAN-2"), "{shell_name}");
        sessions.close(id).unwrap();
    }
}
