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
    Answer, OutputSink, PartFile, Prompter, ReceiveFolder, SinkClosed, SshConnector,
    TransferAnswer, TransferEvent, TransferHost,
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
    fn offered(&self, _id: SessionId, _offer: u64, _wants: Wants) {
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
        .answer_transfer(id, 1, TransferAnswer::Receive(Box::new(folder.clone())))
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

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

/// Runs `rz` in a fresh folder, and waits for its offer.
async fn rz_waiting(sessions: &SessionManager, id: SessionId, host: &Host, dir: &str) {
    sessions
        .write_text(id, format!("rm -rf {dir}; mkdir {dir}; cd {dir} && rz\n"))
        .await
        .unwrap();
    until("rz", || host.offers.load(Ordering::Relaxed) == 1).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an sshd with lrzsz: TAUMINUS_SZ_SSHD"]
async fn files_sent_to_rz_arrive_whole_and_an_existing_one_is_skipped() {
    let scratch = Scratch::new("zmodem-rz");
    let (sessions, id, host, screen) = shell(&scratch).await;
    let big: Vec<u8> = (0..3_000_000u32)
        .map(|i| (i * 7 + i / 3).to_le_bytes()[0])
        .collect();
    sessions
        .write_text(
            id,
            "rm -rf /tmp/rzt; mkdir /tmp/rzt; echo old > /tmp/rzt/old.txt; cd /tmp/rzt && rz\n"
                .into(),
        )
        .await
        .unwrap();
    until("rz", || host.offers.load(Ordering::Relaxed) == 1).await;
    let files: Vec<Box<dyn tauminus_app::ports::OutgoingFile>> = vec![
        Box::new(MemFile("big.bin", big.clone())),
        Box::new(MemFile("old.txt", b"new\n".to_vec())),
        Box::new(MemFile("small.txt", b"hello\n".to_vec())),
    ];
    sessions
        .answer_transfer(id, 1, TransferAnswer::Send(files))
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
        matches!(
            events.last(),
            Some(TransferEvent::Finished { folder: None, files, skipped, .. })
                if files == &["big.bin", "small.txt"] && skipped == &["old.txt"]
        ),
        "{events:?}"
    );
    sessions
        .write_text(id, "sha256sum big.bin; cat old.txt small.txt\n".into())
        .await
        .unwrap();
    let want = hex(&big);
    until("the hash", || screen.text().contains(&want)).await;
    until("the files", || screen.text().contains("old\r\nhello")).await;
    sessions.close(id).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an sshd with lrzsz: TAUMINUS_SZ_SSHD"]
async fn a_stop_mid_send_runs_none_of_the_file_on_the_host() {
    let scratch = Scratch::new("zmodem-rz-stop");
    let (sessions, id, host, screen) = shell(&scratch).await;
    sessions
        .write_text(id, "rm -f /tmp/pwned\n".into())
        .await
        .unwrap();
    rz_waiting(&sessions, id, &host, "/tmp/rzs").await;
    // Text lines that would run, were they typed into the shell.
    let lines = b"touch /tmp/pwned\n".repeat(400_000);
    let files: Vec<Box<dyn tauminus_app::ports::OutgoingFile>> =
        vec![Box::new(MemFile("lines.txt", lines))];
    sessions
        .answer_transfer(id, 1, TransferAnswer::Send(files))
        .unwrap();
    until("a file", || {
        host.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, TransferEvent::Progress { .. }))
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
    // Give whatever was on its way time to land, then ask.
    tokio::time::sleep(Duration::from_secs(2)).await;
    for _ in 0..50 {
        if sessions
            .write_text(
                id,
                "\nls /tmp/pwned 2>&1 | tail -1; echo checked-$((6*7))\n".into(),
            )
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    until("the check", || screen.text().contains("checked-42")).await;
    let text = screen.text();
    assert!(
        text.contains("No such file"),
        "{}",
        &text[text.len().saturating_sub(600)..]
    );
    sessions.close(id).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an sshd with lrzsz: TAUMINUS_SZ_SSHD"]
async fn rz_killed_mid_send_runs_none_of_the_file_on_the_host() {
    let scratch = Scratch::new("zmodem-rz-killed");
    let (sessions, id, host, screen) = shell(&scratch).await;
    // As the view does: it acknowledges what it showed, so a shell that
    // echoes what reached it does not hold the session back.
    let sessions = Arc::new(sessions);
    let acking = {
        let (sessions, screen) = (Arc::clone(&sessions), screen.clone());
        tokio::spawn(async move {
            let mut acked = 0;
            loop {
                let shown = screen.0.lock().unwrap().len();
                let _ = sessions.acknowledge(id, shown - acked);
                acked = shown;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    };
    sessions
        .write_text(
            id,
            "rm -rf /tmp/rzk /tmp/killed-pwned; mkdir /tmp/rzk && cd /tmp/rzk && timeout --foreground 2 rz\n"
                .into(),
        )
        .await
        .unwrap();
    until("rz", || host.offers.load(Ordering::Relaxed) == 1).await;
    // Lines that would run, were the shell to read them, and more than
    // goes in 2 s.
    let lines = b"touch /tmp/killed-pwned\n".repeat(4_000_000);
    let files: Vec<Box<dyn tauminus_app::ports::OutgoingFile>> =
        vec![Box::new(MemFile("lines.txt", lines))];
    sessions
        .answer_transfer(id, 1, TransferAnswer::Send(files))
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
        "{events:?}"
    );
    // What was on its way still reaches the tty, which echoes it: wait
    // until the screen is still, as a user would.
    let mut seen = 0;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let now = screen.0.lock().unwrap().len();
        if now == seen {
            break;
        }
        seen = now;
    }
    // Ctrl+C clears what reached the line, then the shell answers.
    for _ in 0..50 {
        if sessions.write_text(id, "\u{3}".into()).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    sessions
        .write_text(
            id,
            "ls /tmp/killed-pwned 2>&1 | tail -1; echo checked-$((6*7))\n".into(),
        )
        .await
        .unwrap();
    until("the check", || screen.text().contains("checked-42")).await;
    let text = screen.text();
    assert!(
        text.contains("No such file"),
        "{}",
        &text[text.len().saturating_sub(800)..]
    );
    acking.abort();
    sessions.close(id).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an sshd with lrzsz: TAUMINUS_SZ_SSHD"]
async fn in_bash_rz_killed_mid_send_leaves_no_line_for_enter() {
    let scratch = Scratch::new("zmodem-rz-bash");
    let (sessions, id, host, screen) = shell(&scratch).await;
    let sessions = Arc::new(sessions);
    let acking = {
        let (sessions, screen) = (Arc::clone(&sessions), screen.clone());
        tokio::spawn(async move {
            let mut acked = 0;
            loop {
                let shown = screen.0.lock().unwrap().len();
                let _ = sessions.acknowledge(id, shown - acked);
                acked = shown;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    };
    // bash, with readline: its `C-x C-x` and `C-u` are what the cancel
    // must get right.
    sessions
        .write_text(id, "exec bash --norc -i\n".into())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    sessions
        .write_text(
            id,
            "rm -rf /tmp/rzb /tmp/bash-pwned*; mkdir /tmp/rzb && cd /tmp/rzb && timeout --foreground 2 rz\n"
                .into(),
        )
        .await
        .unwrap();
    until("rz", || host.offers.load(Ordering::Relaxed) == 1).await;
    let lines = b"touch /tmp/bash-pwned\n".repeat(4_000_000);
    let files: Vec<Box<dyn tauminus_app::ports::OutgoingFile>> =
        vec![Box::new(MemFile("lines.txt", lines))];
    sessions
        .answer_transfer(id, 1, TransferAnswer::Send(files))
        .unwrap();
    until("the stop", || {
        matches!(
            host.events.lock().unwrap().last(),
            Some(TransferEvent::Failed { .. })
        )
    })
    .await;
    let mut seen = 0;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let now = screen.0.lock().unwrap().len();
        if now == seen {
            break;
        }
        seen = now;
    }
    // Enter, with no Ctrl+C first: whatever was left on the line runs.
    sessions.write_text(id, "\r".into()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    sessions
        .write_text(
            id,
            "ls /tmp/bash-pwned* 2>&1 | tail -1; echo checked-$((6*7))\n".into(),
        )
        .await
        .unwrap();
    until("the check", || screen.text().contains("checked-42")).await;
    let text = screen.text();
    assert!(
        text.contains("No such file"),
        "{}",
        &text[text.len().saturating_sub(800)..]
    );
    acking.abort();
    sessions.close(id).unwrap();
}
