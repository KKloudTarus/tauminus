// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! A serial session over a virtual port pair: `socat` joins two
//! pseudo-terminals, the session opens one end, the test plays the device
//! on the other. Set `TAUMINUS_SOCAT` or have `socat` on `PATH`; without it
//! the tests pass on a workstation and fail in CI on Linux.

#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "test setup runs socat and waits for its ports"
)]

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tauminus_app::ports::{SerialPorts, TerminalEvent, TerminalInput, TerminalStream};
use tauminus_domain::{Encoding, SerialSettings};
use tauminus_serial::NativeSerialPorts;

fn socat() -> Option<PathBuf> {
    let found = std::env::var_os("TAUMINUS_SOCAT")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("PATH").and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join("socat"))
                    .find(|candidate| candidate.is_file())
            })
        });
    assert!(
        found.is_some() || std::env::var_os("CI").is_none() || !cfg!(target_os = "linux"),
        "CI needs socat"
    );
    found
}

/// Two joined pseudo-terminals at `<dir>/port` and `<dir>/device`.
struct Pair {
    child: Child,
    dir: PathBuf,
}

impl Pair {
    fn start(socat: &Path, name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("tauminus-serial-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let child = Command::new(socat)
            .arg(format!(
                "pty,raw,echo=0,link={}",
                dir.join("port").display()
            ))
            .arg(format!(
                "pty,raw,echo=0,link={}",
                dir.join("device").display()
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while !(dir.join("port").exists() && dir.join("device").exists()) {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "socat made no ports"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        Self { child, dir }
    }

    fn port(&self) -> String {
        self.dir.join("port").display().to_string()
    }

    fn device(&self) -> std::fs::File {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.dir.join("device"))
            .unwrap()
    }
}

impl Drop for Pair {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn output_until(stream: &mut TerminalStream, needle: &str) -> String {
    let mut seen = String::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !seen.contains(needle) {
            match stream.events.recv().await {
                Some(TerminalEvent::Output(chunk)) => {
                    seen.push_str(&String::from_utf8_lossy(&chunk));
                }
                other => panic!("ended before {needle:?}: {other:?} {seen}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {needle:?} in {seen:?}"));
    seen
}

async fn ended(stream: &mut TerminalStream) -> bool {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match stream.events.recv().await {
                Some(TerminalEvent::Exited { .. } | TerminalEvent::Lost) | None => return,
                Some(TerminalEvent::Output(_)) => {}
            }
        }
    })
    .await
    .is_ok()
}

#[tokio::test]
async fn a_port_carries_both_ways_converts_and_closes() {
    let Some(socat) = socat() else { return };
    let pair = Pair::start(&socat, "both");
    let mut settings = SerialSettings::new(&pair.port(), 115_200, 8).unwrap();
    settings.encoding = Encoding::Koi8R;
    let mut stream = NativeSerialPorts::allowing_pseudo_terminals()
        .open(&settings)
        .unwrap();
    let mut device = pair.device();

    // The device prints: Привет in KOI8-R.
    device.write_all(b"\xf0\xd2\xc9\xd7\xc5\xd4\r\n").unwrap();
    output_until(&mut stream, "Привет").await;

    // The user types, and the device reads it in its own set.
    stream
        .input
        .send(TerminalInput::Text("ok Ж\r".into()))
        .await
        .unwrap();
    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buffer = [0_u8; 64];
    while !got.ends_with(b"\r") && Instant::now() < deadline {
        let n = device.read(&mut buffer).unwrap();
        got.extend_from_slice(&buffer[..n]);
    }
    assert_eq!(got, b"ok \xf6\r");

    stream.terminator.terminate();
    assert!(
        ended(&mut stream).await,
        "a close ends the session within 2 s"
    );
}

#[tokio::test]
async fn an_unplugged_port_ends_the_session() {
    let Some(socat) = socat() else { return };
    let mut pair = Pair::start(&socat, "unplug");
    let settings = SerialSettings::new(&pair.port(), 9600, 8).unwrap();
    let mut stream = NativeSerialPorts::allowing_pseudo_terminals()
        .open(&settings)
        .unwrap();
    // socat going away is the cable pulled out.
    let _ = pair.child.kill();
    let _ = pair.child.wait();
    assert!(ended(&mut stream).await, "the session ends with the port");
}

#[test]
fn a_missing_port_fails_to_open_and_says_which() {
    let settings = SerialSettings::new("/dev/tauminus-no-such-port", 9600, 8).unwrap();
    let error = NativeSerialPorts::allowing_pseudo_terminals()
        .open(&settings)
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("tauminus-no-such-port")
            || format!("{error:?}").contains("tauminus-no-such-port"),
        "{error:?}"
    );
}

#[test]
fn only_a_serial_device_opens() {
    for path in ["/etc/hostname", "/dev/null", "/tmp", "/dev/tty"] {
        let settings = SerialSettings::new(path, 9600, 8).unwrap();
        assert!(
            NativeSerialPorts::default().open(&settings).is_err(),
            "{path}"
        );
    }
    let Some(socat) = socat() else { return };
    let pair = Pair::start(&socat, "refused");
    let settings = SerialSettings::new(&pair.port(), 9600, 8).unwrap();
    // A pseudo-terminal is a terminal, not a port, unless asked for.
    assert!(NativeSerialPorts::default().open(&settings).is_err());
}

#[tokio::test]
async fn typing_held_by_xoff_arrives_after_xon() {
    let Some(socat) = socat() else { return };
    let pair = Pair::start(&socat, "xoff");
    let mut settings = SerialSettings::new(&pair.port(), 115_200, 8).unwrap();
    settings.flow = tauminus_domain::SerialFlow::Software;
    let stream = NativeSerialPorts::allowing_pseudo_terminals()
        .open(&settings)
        .unwrap();
    let mut device = pair.device();
    device.write_all(b"\x13").unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    for word in ["one ", "two ", "three\r"] {
        stream
            .input
            .send(TerminalInput::Text(word.into()))
            .await
            .unwrap();
    }
    // Held past several of the writer's 100 ms waits.
    tokio::time::sleep(Duration::from_millis(500)).await;
    device.write_all(b"\x11").unwrap();
    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buffer = [0_u8; 64];
    while !got.ends_with(b"\r") && Instant::now() < deadline {
        let n = device.read(&mut buffer).unwrap();
        got.extend_from_slice(&buffer[..n]);
    }
    assert_eq!(got, b"one two three\r");
}
