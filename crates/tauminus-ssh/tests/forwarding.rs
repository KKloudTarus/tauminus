// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Local, remote, and dynamic forwarding against a real OpenSSH server.
//! sshd runs on this machine, so "the server's side" is loopback here too.

#![cfg(unix)]

mod support;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use support::*;
use tauminus_app::ports::{SshConnection, SshConnector, TerminalInput};
use tauminus_domain::{
    Bind, BindAddress, Destination, Forward, HostName, Route, SessionOptions, TerminalSize,
};
use tauminus_ssh::{AgentSource, HostKeyStore, RusshConnector};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

struct Setup {
    _sshd: Sshd,
    _scratch: Scratch,
    connection: Arc<dyn SshConnection>,
}

async fn setup() -> Option<Setup> {
    setup_with(|connector| connector).await
}

/// As [`setup`], with the connector changed first, such as its limits.
async fn setup_with(configure: impl FnOnce(RusshConnector) -> RusshConnector) -> Option<Setup> {
    let binary = sshd_binary()?;
    let scratch = Scratch::new("forward");
    keygen(&scratch.0, &["-q", "-t", "ed25519", "-N", "", "-f", "id"]);
    let sshd = Sshd::start(
        &binary,
        &[&scratch.path("id.pub")],
        None,
        SshdOptions::default(),
    );
    std::fs::write(
        scratch.path("known_hosts"),
        format!("[127.0.0.1]:{} {}", sshd.port, sshd.host_public_key()),
    )
    .unwrap();
    let store = HostKeyStore::new(
        vec![scratch.path("known_hosts")],
        scratch.path("tauminus_known_hosts"),
    );
    let connector = configure(RusshConnector::new(
        store,
        Some(scratch.0.clone()),
        AgentSource::None,
    ));
    let target = target(
        sshd.endpoint(),
        vec![],
        Route::Direct,
        key_auth(&[scratch.path("id")]),
    );
    let connection = connector
        .connection(
            &target,
            ScriptedPrompter::new(vec![]),
            Arc::default(),
            false,
        )
        .await
        .expect("logs in");
    Some(Setup {
        _sshd: sshd,
        _scratch: scratch,
        connection,
    })
}

/// A TCP server on loopback that echoes everything back.
async fn echo_server() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let (mut read, mut write) = socket.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });
    port
}

/// A TCP server that writes without end and never reads.
async fn firehose_server() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let chunk = vec![b'x'; 64 * 1024];
                while socket.write_all(&chunk).await.is_ok() {}
            });
        }
    });
    port
}

fn loopback(port: u16) -> Bind {
    Bind {
        address: BindAddress::Loopback,
        port,
    }
}

fn to_loopback(port: u16) -> Destination {
    Destination::new(HostName::parse("127.0.0.1").unwrap(), port).unwrap()
}

/// Sends `size` bytes through `port` and checks the same bytes come back.
async fn round_trip(port: u16, size: usize) {
    let socket = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap();
    let (mut read, mut write) = socket.into_split();
    let sent: Vec<u8> = (0..size)
        .map(|i| u8::try_from(i * 7 % 251).unwrap())
        .collect();
    let expected = sent.clone();
    let writer = tokio::spawn(async move {
        write.write_all(&sent).await.unwrap();
        write.shutdown().await.unwrap();
    });
    let mut received = Vec::with_capacity(size);
    tokio::time::timeout(Duration::from_secs(30), read.read_to_end(&mut received))
        .await
        .expect("the echo comes back in time")
        .unwrap();
    writer.await.unwrap();
    assert_eq!(received.len(), expected.len());
    assert!(received == expected, "the bytes came back changed");
}

#[tokio::test]
async fn local_forward_carries_bytes_both_ways() {
    let Some(setup) = setup().await else { return };
    let echo = echo_server().await;
    let forward = Forward::Local {
        bind: loopback(0),
        to: to_loopback(echo),
    };
    let handle = setup.connection.start_forward(&forward).await.unwrap();
    assert_ne!(handle.port(), 0, "port 0 reports the port picked");
    round_trip(handle.port(), 4 * 1024 * 1024).await;
    round_trip(handle.port(), 1).await;
    let status = handle.status();
    assert_eq!(status.total, 2);
    assert_eq!(status.ended, None);
}

#[tokio::test]
async fn remote_forward_brings_the_servers_connections_back() {
    let Some(setup) = setup().await else { return };
    let echo = echo_server().await;
    let forward = Forward::Remote {
        bind: loopback(0),
        to: to_loopback(echo),
    };
    let handle = setup.connection.start_forward(&forward).await.unwrap();
    assert_ne!(handle.port(), 0, "the server's pick is reported");
    round_trip(handle.port(), 1024 * 1024).await;
    assert_eq!(handle.status().total, 1);
    // The server listens on IPv6 loopback too, where it has one.
    if std::net::TcpListener::bind("[::1]:0").is_ok() {
        let mut six = TcpStream::connect(("::1", handle.port())).await.unwrap();
        six.write_all(b"six").await.unwrap();
        let mut back = [0; 3];
        six.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"six");
    }
    // Stopping cancels the listener on the server.
    let port = handle.port();
    drop(handle);
    wait_until_refused(port).await;
}

#[tokio::test]
async fn dynamic_forward_serves_socks5_and_socks4a() {
    let Some(setup) = setup().await else { return };
    let echo = echo_server().await;
    let forward = Forward::Dynamic { bind: loopback(0) };
    let handle = setup.connection.start_forward(&forward).await.unwrap();
    let proxy = SocketAddr::from((Ipv4Addr::LOCALHOST, handle.port()));

    let mut five = TcpStream::connect(proxy).await.unwrap();
    five.write_all(&[5, 1, 0]).await.unwrap();
    let mut choice = [0; 2];
    five.read_exact(&mut choice).await.unwrap();
    assert_eq!(choice, [5, 0]);
    let mut request = vec![5, 1, 0, 3, 9];
    request.extend_from_slice(b"localhost");
    request.extend_from_slice(&echo.to_be_bytes());
    five.write_all(&request).await.unwrap();
    let mut reply = [0; 10];
    five.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply[..2], [5, 0], "granted");
    five.write_all(b"through socks5").await.unwrap();
    let mut back = [0; 14];
    five.read_exact(&mut back).await.unwrap();
    assert_eq!(&back, b"through socks5");

    let mut four = TcpStream::connect(proxy).await.unwrap();
    let mut request = vec![4, 1];
    request.extend_from_slice(&echo.to_be_bytes());
    request.extend_from_slice(&[0, 0, 0, 1, 0]);
    request.extend_from_slice(b"127.0.0.1\0");
    four.write_all(&request).await.unwrap();
    let mut reply = [0; 8];
    four.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply[1], 0x5a, "granted");
    four.write_all(b"4a").await.unwrap();
    let mut back = [0; 2];
    four.read_exact(&mut back).await.unwrap();
    assert_eq!(&back, b"4a");

    // A port nothing listens on is refused, not hung.
    let closed = free_loopback_port();
    let mut refused = TcpStream::connect(proxy).await.unwrap();
    let mut request = vec![5, 1, 0, 5, 1, 0, 1, 127, 0, 0, 1];
    request.extend_from_slice(&closed.to_be_bytes());
    refused.write_all(&request).await.unwrap();
    let mut reply = [0; 12];
    tokio::time::timeout(Duration::from_secs(20), refused.read_exact(&mut reply))
        .await
        .expect("the refusal comes in time")
        .unwrap();
    assert_eq!(reply[2..4], [5, 1], "general failure");
}

#[tokio::test]
async fn stopping_closes_the_listener_and_what_it_carries() {
    let Some(setup) = setup().await else { return };
    let echo = echo_server().await;
    let forward = Forward::Local {
        bind: loopback(0),
        to: to_loopback(echo),
    };
    let handle = setup.connection.start_forward(&forward).await.unwrap();
    let port = handle.port();
    let mut open = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap();
    open.write_all(b"ping").await.unwrap();
    let mut back = [0; 4];
    open.read_exact(&mut back).await.unwrap();
    assert_eq!(handle.status().open, 1);
    drop(handle);
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), open.read_to_end(&mut rest))
        .await
        .expect("the carried connection closes")
        .ok();
    wait_until_refused(port).await;
}

#[tokio::test]
async fn a_forward_keeps_the_connection_after_its_shell_and_owner_are_gone() {
    let Some(setup) = setup().await else { return };
    let echo = echo_server().await;
    let size = TerminalSize::new(80, 24).unwrap();
    let mut shell = setup
        .connection
        .open_shell(size, &SessionOptions::default())
        .await
        .unwrap();
    let forward = Forward::Local {
        bind: loopback(0),
        to: to_loopback(echo),
    };
    let handle = setup.connection.start_forward(&forward).await.unwrap();
    shell
        .input
        .send(TerminalInput::Data(b"exit\n".to_vec()))
        .await
        .unwrap();
    let (_, code) = collect(&mut shell).await;
    assert_eq!(code, Some(0));
    let Setup {
        _sshd: sshd,
        connection,
        ..
    } = setup;
    drop(connection);
    // Only the forward holds the connection now.
    round_trip(handle.port(), 64 * 1024).await;
    assert_eq!(handle.status().ended, None);
    let port = handle.port();
    assert_eq!(sshd.sessions(), 1, "the forward reused the shell's login");
    drop(handle);
    wait_until_refused(port).await;
    // Nothing holds the connection now, so the session ends.
    let deadline = Instant::now() + Duration::from_secs(5);
    while sshd.sessions() > 0 {
        assert!(
            Instant::now() < deadline,
            "the session outlived its last holder"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn the_server_going_away_ends_the_forward() {
    let Some(setup) = setup().await else { return };
    let forward = Forward::Dynamic { bind: loopback(0) };
    let handle = setup.connection.start_forward(&forward).await.unwrap();
    let Setup {
        _sshd: sshd,
        connection,
        ..
    } = setup;
    sshd.kill_sessions();
    tokio::time::timeout(Duration::from_secs(10), connection.closed())
        .await
        .expect("the end is noticed");
    assert!(!connection.is_open());
    let mut status = handle.watch();
    tokio::time::timeout(
        Duration::from_secs(5),
        status.wait_for(|s| s.ended.is_some()),
    )
    .await
    .expect("the forward reports it")
    .unwrap();
    assert!(connection.start_forward(&forward).await.is_err());
}

#[tokio::test]
async fn a_peer_that_stops_reading_does_not_freeze_the_shell_or_other_forwards() {
    let Some(setup) = setup().await else { return };
    let firehose = firehose_server().await;
    let echo = echo_server().await;
    let size = TerminalSize::new(80, 24).unwrap();
    let mut shell = setup
        .connection
        .open_shell(size, &SessionOptions::default())
        .await
        .unwrap();
    let flood = setup
        .connection
        .start_forward(&Forward::Local {
            bind: loopback(0),
            to: to_loopback(firehose),
        })
        .await
        .unwrap();
    let steady = setup
        .connection
        .start_forward(&Forward::Local {
            bind: loopback(0),
            to: to_loopback(echo),
        })
        .await
        .unwrap();
    // Connects and never reads.
    let stuck = TcpStream::connect((Ipv4Addr::LOCALHOST, flood.port()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    for i in 0..5 {
        let started = Instant::now();
        round_trip(steady.port(), 64 * 1024).await;
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a forward took {:?}",
            started.elapsed()
        );
        let started = Instant::now();
        shell
            .input
            .send(TerminalInput::Data(
                format!("echo $((40+{i}))x\n").into_bytes(),
            ))
            .await
            .unwrap();
        wait_for_output(&mut shell, &format!("{}x", 40 + i)).await;
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the shell took {:?}",
            started.elapsed()
        );
    }
    let mut flood_status = flood.watch();
    tokio::time::timeout(
        Duration::from_secs(10),
        flood_status.wait_for(|s| s.open == 0),
    )
    .await
    .expect("the peer that fell behind is closed")
    .unwrap();
    assert_eq!(flood.status().resets, 1, "and counted as reset");
    assert_eq!(steady.status().resets, 0);
    // With a reset, so what it got cannot pass for the whole stream.
    let mut stuck = stuck;
    let mut sink = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(10), stuck.read_to_end(&mut sink))
        .await
        .expect("the stream ends");
    assert!(read.is_err(), "ended cleanly after {} bytes", sink.len());
}

#[tokio::test]
async fn stopping_ends_a_socks_handshake_in_progress() {
    let Some(setup) = setup().await else { return };
    let handle = setup
        .connection
        .start_forward(&Forward::Dynamic { bind: loopback(0) })
        .await
        .unwrap();
    let mut half = TcpStream::connect((Ipv4Addr::LOCALHOST, handle.port()))
        .await
        .unwrap();
    half.write_all(&[5]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(handle);
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), half.read_to_end(&mut rest))
        .await
        .expect("closed at once, not after the 10 s handshake limit")
        .ok();
}

/// Reads shell output until `needle`, which only the command's result
/// contains, appears.
async fn wait_for_output(shell: &mut tauminus_app::ports::TerminalStream, needle: &str) {
    let mut seen = String::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !seen.contains(needle) {
            match shell.events.recv().await {
                Some(tauminus_app::ports::TerminalEvent::Output(chunk)) => {
                    seen.push_str(&String::from_utf8_lossy(&chunk));
                }
                other => panic!("the shell ended: {other:?}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the shell answers; saw {seen:?}"));
}

#[tokio::test]
async fn someone_else_on_the_ipv6_loopback_port_fails_the_rule() {
    let Some(setup) = setup().await else { return };
    // Only where this machine has IPv6 loopback.
    let Ok(squatter) = std::net::TcpListener::bind("[::1]:0") else {
        return;
    };
    let port = squatter.local_addr().unwrap().port();
    let forward = Forward::Dynamic {
        bind: loopback(port),
    };
    let error = setup.connection.start_forward(&forward).await.unwrap_err();
    assert!(error.to_string().contains("[::1]"), "{error}");
    // Its IPv4 listener is gone too.
    wait_until_refused(port).await;
}

fn free_loopback_port() -> u16 {
    std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Waits until nothing accepts on `port`, which a stopped listener may
/// take a moment to reach.
async fn wait_until_refused(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        .await
        .is_ok()
    {
        assert!(Instant::now() < deadline, "port {port} still accepts");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_rule_restarts_on_the_same_port_at_once() {
    let Some(setup) = setup().await else { return };
    let echo = echo_server().await;
    let port = free_loopback_port();
    for (kind, forward) in [
        (
            "local",
            Forward::Local {
                bind: loopback(port),
                to: to_loopback(echo),
            },
        ),
        (
            "remote",
            Forward::Remote {
                bind: loopback(port),
                to: to_loopback(echo),
            },
        ),
    ] {
        let first = setup.connection.start_forward(&forward).await.unwrap();
        round_trip(first.port(), 1024).await;
        first.stop(Duration::from_secs(2)).await;
        let again = setup
            .connection
            .start_forward(&forward)
            .await
            .unwrap_or_else(|e| panic!("{kind}: {e}"));
        assert_eq!(again.port(), port);
        round_trip(again.port(), 1024).await;
        again.stop(Duration::from_secs(2)).await;
    }
}

/// A TCP server that sends `bytes` and then keeps the connection open,
/// idle, without reading.
async fn burst_server(bytes: usize) -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let _ = socket.write_all(&vec![b'x'; bytes]).await;
                tokio::time::sleep(Duration::from_secs(120)).await;
                drop(socket);
            });
        }
    });
    port
}

/// Connects with a tiny receive buffer and never reads, so what the
/// forward sends piles up on our side rather than in the kernel.
async fn stuck_client(port: u16) -> TcpStream {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    socket
        .connect(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        .await
        .unwrap()
}

async fn wait_for_resets(handle: &tauminus_app::ports::ForwardHandle, resets: u64) {
    let mut status = handle.watch();
    tokio::time::timeout(
        Duration::from_secs(10),
        status.wait_for(|s| s.resets >= resets && s.open == 0),
    )
    .await
    .unwrap_or_else(|_| panic!("no reset: {:?}", handle.status()))
    .unwrap();
}

#[tokio::test]
async fn a_peer_that_takes_nothing_for_the_stall_limit_is_reset() {
    let Some(setup) =
        setup_with(|c| c.with_forward_limits(64 * 1024 * 1024, Duration::from_millis(300))).await
    else {
        return;
    };
    let source = burst_server(3 * 1024 * 1024).await;
    let handle = setup
        .connection
        .start_forward(&Forward::Local {
            bind: loopback(0),
            to: to_loopback(source),
        })
        .await
        .unwrap();
    let _stuck = stuck_client(handle.port()).await;
    wait_for_resets(&handle, 1).await;
}

#[tokio::test]
async fn the_biggest_holder_out_of_budget_is_reset_itself() {
    let Some(setup) =
        setup_with(|c| c.with_forward_limits(1024 * 1024, Duration::from_secs(60))).await
    else {
        return;
    };
    let handle = setup
        .connection
        .start_forward(&Forward::Local {
            bind: loopback(0),
            // More than the kernel buffers on loopback, so it queues here.
            to: to_loopback(burst_server(12 * 1024 * 1024).await),
        })
        .await
        .unwrap();
    let _stuck = stuck_client(handle.port()).await;
    wait_for_resets(&handle, 1).await;
}
