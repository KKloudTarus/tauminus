// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Telnet against a real `telnetd`: the one in `BusyBox`, which runs as
//! any user. Set `TAUMINUS_BUSYBOX` to a `BusyBox` with the `telnetd`
//! applet, or have one on `PATH`. Without one the tests pass on a workstation and fail in CI.

#![cfg(unix)]

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tauminus_app::ports::{TelnetConnector, TerminalEvent, TerminalInput, TerminalStream};
use tauminus_domain::{Encoding, HostName, TelnetTarget, TerminalSize};
use tauminus_telnet::TcpTelnetConnector;

#[expect(
    clippy::disallowed_methods,
    reason = "test setup asks BusyBox for its applets"
)]
fn busybox() -> Option<PathBuf> {
    let found = std::env::var_os("TAUMINUS_BUSYBOX")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("PATH").and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join("busybox"))
                    .find(|candidate| candidate.is_file())
            })
        })
        .filter(|busybox| {
            Command::new(busybox)
                .args(["telnetd", "--help"])
                .output()
                .is_ok_and(|out| String::from_utf8_lossy(&out.stderr).contains("telnetd"))
        });
    // CI installs it on Linux; macOS has no BusyBox.
    assert!(
        found.is_some() || std::env::var_os("CI").is_none() || !cfg!(target_os = "linux"),
        "CI needs BusyBox with telnetd"
    );
    found
}

/// A `telnetd` on a free loopback port, giving a shell with no login.
struct Telnetd {
    child: Child,
    port: u16,
}

impl Telnetd {
    #[expect(
        clippy::disallowed_methods,
        reason = "test setup waits for telnetd to listen"
    )]
    fn start(busybox: &PathBuf) -> Self {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = Command::new(busybox)
            .args([
                "telnetd",
                "-F",
                "-K",
                "-b",
                "127.0.0.1",
                "-l",
                "/bin/sh",
                "-p",
            ])
            .arg(port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // Up once it accepts.
        for _ in 0..100 {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Self { child, port }
    }

    fn target(&self) -> TelnetTarget {
        TelnetTarget {
            host: HostName::parse("127.0.0.1").unwrap(),
            port: self.port,
            encoding: Encoding::Utf8,
        }
    }
}

impl Drop for Telnetd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Output until `needle` shows, or panics after 10 s.
async fn wait_for(stream: &mut TerminalStream, seen: &mut String, needle: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !seen.contains(needle) {
            match stream.events.recv().await {
                Some(TerminalEvent::Output(chunk)) => {
                    seen.push_str(&String::from_utf8_lossy(&chunk));
                }
                Some(TerminalEvent::Exited { .. } | TerminalEvent::Lost) | None => {
                    panic!("ended before {needle:?}: {seen}")
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {needle:?} in: {seen}"));
}

async fn send(stream: &TerminalStream, text: &str) {
    stream
        .input
        .send(TerminalInput::Text(text.into()))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_shell_over_telnet_sees_the_window_size_echoes_and_ends() {
    let Some(busybox) = busybox() else { return };
    let telnetd = Telnetd::start(&busybox);
    let mut stream = TcpTelnetConnector::default()
        .connect(&telnetd.target(), TerminalSize::new(100, 30).unwrap())
        .await
        .unwrap();
    let mut seen = String::new();
    send(&stream, "stty size\r").await;
    wait_for(&mut stream, &mut seen, "30 100").await;
    // The server echoes: the command shows though nothing local echoes it.
    assert!(seen.contains("stty size"), "{seen}");

    stream
        .input
        .send(TerminalInput::Resize(TerminalSize::new(132, 40).unwrap()))
        .await
        .unwrap();
    send(&stream, "stty size\r").await;
    wait_for(&mut stream, &mut seen, "40 132").await;

    send(&stream, "exit\r").await;
    let ended = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match stream.events.recv().await {
                Some(TerminalEvent::Exited { code }) => return code,
                Some(TerminalEvent::Lost) => panic!("the shell ended; nothing was lost"),
                Some(TerminalEvent::Output(_)) => {}
                None => panic!("stream closed without an exit"),
            }
        }
    })
    .await
    .expect("the session ends when the shell does");
    assert_eq!(ended, None, "Telnet has no exit status");
}

#[tokio::test]
async fn a_closed_port_is_refused() {
    // Bound and never listening: the port stays closed, and no other test
    // can take it, as one could a port bound and let go.
    let closed = tokio::net::TcpSocket::new_v4().unwrap();
    closed.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = closed.local_addr().unwrap().port();
    let target = TelnetTarget {
        host: HostName::parse("127.0.0.1").unwrap(),
        port,
        encoding: Encoding::Utf8,
    };
    let error = TcpTelnetConnector::default()
        .connect(&target, TerminalSize::default())
        .await
        .err()
        .unwrap();
    assert!(
        matches!(error, tauminus_app::ports::ConnectError::Refused { .. }),
        "{error}"
    );
}

#[tokio::test]
async fn a_server_that_never_reads_cannot_keep_a_session_open() {
    use tauminus_app::ports::TelnetConnector as _;
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    // Floods requests that each need a reply, and never reads one.
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let flood = [255_u8, 253, 31].repeat(4096);
        while socket.write_all(&flood).await.is_ok() {}
    });
    let target = TelnetTarget {
        host: HostName::parse("127.0.0.1").unwrap(),
        port,
        encoding: Encoding::Utf8,
    };
    let mut stream = TcpTelnetConnector::default()
        .connect(&target, TerminalSize::default())
        .await
        .unwrap();
    // A paste that cannot leave either.
    for _ in 0..8 {
        let _ = stream
            .input
            .try_send(TerminalInput::Data(vec![b'x'; 1 << 20]));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    stream.terminator.terminate();
    let ended = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match stream.events.recv().await {
                Some(TerminalEvent::Exited { .. } | TerminalEvent::Lost) | None => return,
                Some(TerminalEvent::Output(_)) => {}
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "the session ends within 2 s of a close");
    server.abort();
}
