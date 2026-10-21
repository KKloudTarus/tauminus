// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tauminus_domain::UserName;
use tokio::net::TcpListener;

use super::*;

const TIMEOUT: Duration = Duration::from_secs(5);

fn endpoint(host: &str, port: u16) -> Endpoint {
    Endpoint::new(
        UserName::parse("u").unwrap(),
        HostName::parse(host).unwrap(),
        port,
    )
    .unwrap()
}

/// A TCP server that sends `greeting`, then echoes what it receives.
async fn echo_server(greeting: &'static [u8]) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _ = socket.write_all(greeting).await;
                let mut buf = [0u8; 1024];
                while let Ok(n) = socket.read(&mut buf).await {
                    if n == 0 || socket.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    address
}

async fn roundtrip(io: &mut BoxedIo, greeting: &[u8]) {
    let mut banner = vec![0u8; greeting.len()];
    io.read_exact(&mut banner).await.unwrap();
    assert_eq!(banner, greeting, "first bytes are the server's");
    io.write_all(b"ping").await.unwrap();
    let mut back = [0u8; 4];
    io.read_exact(&mut back).await.unwrap();
    assert_eq!(&back, b"ping");
}

#[tokio::test]
async fn direct_connects_to_the_endpoint() {
    let server = echo_server(b"SSH-2.0-test\r\n").await;
    let mut io = open(
        &Route::Direct,
        &endpoint("127.0.0.1", server.port()),
        TIMEOUT,
    )
    .await
    .unwrap();
    roundtrip(&mut io, b"SSH-2.0-test\r\n").await;
}

#[tokio::test]
async fn refused_port_is_reported_as_refused() {
    // A port that was just released. Another process may grab it between
    // the drop and the connect, so try a few. A bound but not listening
    // socket would not do: macOS drops the SYN instead of refusing it.
    for _ in 0..5 {
        let port = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        match open(&Route::Direct, &endpoint("127.0.0.1", port), TIMEOUT).await {
            Err(ConnectError::Refused { port: p, .. }) => {
                assert_eq!(p, port);
                return;
            }
            Ok(_) => {}
            Err(other) => panic!("{other:?}"),
        }
    }
    panic!("every released port was taken again");
}

#[tokio::test]
async fn unknown_host_is_reported_as_unresolvable() {
    // RFC 6761 reserves .invalid: it never resolves.
    let error = open(&Route::Direct, &endpoint("tauminus.invalid", 22), TIMEOUT)
        .await
        .err()
        .unwrap();
    assert!(matches!(error, ConnectError::Resolve { .. }), "{error:?}");
}

/// A SOCKS5 server for one connection: no auth, CONNECT by domain only.
/// `reply` 0 relays to `target`; any other code is sent back as a refusal.
async fn socks5_server(target: SocketAddr, reply: u8) -> SocketAddr {
    socks5_server_recording(target, reply, Arc::default()).await
}

/// Like `socks5_server`, and records each request's address type.
async fn socks5_server_recording(
    target: SocketAddr,
    reply: u8,
    types: Arc<Mutex<Vec<u8>>>,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        let mut greeting = [0u8; 3];
        client.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 1, 0]);
        client.write_all(&[5, 0]).await.unwrap();
        let mut head = [0u8; 4];
        client.read_exact(&mut head).await.unwrap();
        assert_eq!(&head[..3], &[5, 1, 0], "CONNECT");
        let address_len = match head[3] {
            1 => 4,
            4 => 16,
            3 => {
                let mut len = [0u8; 1];
                client.read_exact(&mut len).await.unwrap();
                usize::from(len[0])
            }
            other => panic!("address type {other}"),
        };
        let mut rest = vec![0u8; address_len + 2];
        client.read_exact(&mut rest).await.unwrap();
        types.lock().unwrap().push(head[3]);
        if reply != 0 {
            client
                .write_all(&[5, reply, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            return;
        }
        let mut upstream = TcpStream::connect(target).await.unwrap();
        client
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
            .await
            .unwrap();
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    });
    address
}

#[tokio::test]
async fn socks5_tunnels_to_the_endpoint_by_name() {
    let server = echo_server(b"SSH-2.0-socks\r\n").await;
    let proxy = socks5_server(server, 0).await;
    let route = Route::Socks5 {
        host: HostName::parse("127.0.0.1").unwrap(),
        port: proxy.port(),
    };
    let mut io = open(&route, &endpoint("localhost", server.port()), TIMEOUT)
        .await
        .unwrap();
    roundtrip(&mut io, b"SSH-2.0-socks\r\n").await;
}

#[tokio::test]
async fn socks5_sends_address_literals_as_addresses() {
    let server = echo_server(b"SSH-2.0-socks\r\n").await;
    let types = Arc::new(Mutex::new(Vec::new()));
    let proxy = socks5_server_recording(server, 0, Arc::clone(&types)).await;
    let route = Route::Socks5 {
        host: HostName::parse("127.0.0.1").unwrap(),
        port: proxy.port(),
    };
    let mut io = open(&route, &endpoint("127.0.0.1", server.port()), TIMEOUT)
        .await
        .unwrap();
    roundtrip(&mut io, b"SSH-2.0-socks\r\n").await;
    assert_eq!(*types.lock().unwrap(), [1], "IPv4 address type");
}

#[tokio::test]
async fn socks5_refusal_explains_the_reply_code() {
    let proxy = socks5_server("127.0.0.1:9".parse().unwrap(), 5).await;
    let route = Route::Socks5 {
        host: HostName::parse("127.0.0.1").unwrap(),
        port: proxy.port(),
    };
    let error = open(&route, &endpoint("db", 22), TIMEOUT)
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&error, ConnectError::Proxy(m) if m.contains("connection refused")),
        "{error:?}"
    );
}

/// An HTTP CONNECT proxy for one connection. On success it writes the 200
/// response and the upstream's first bytes in the same write, to catch a
/// client that reads past the header.
async fn http_proxy(target: SocketAddr, status: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            client.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
        }
        assert!(String::from_utf8_lossy(&request).starts_with("CONNECT "));
        if status != "200" {
            client
                .write_all(format!("HTTP/1.1 {status} Forbidden\r\n\r\n").as_bytes())
                .await
                .unwrap();
            return;
        }
        let mut upstream = TcpStream::connect(target).await.unwrap();
        let mut banner = vec![0u8; 15];
        upstream.read_exact(&mut banner).await.unwrap();
        let mut response = b"HTTP/1.1 200 Connection established\r\n\r\n".to_vec();
        response.extend_from_slice(&banner);
        client.write_all(&response).await.unwrap();
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    });
    address
}

#[tokio::test]
async fn http_connect_keeps_bytes_that_follow_the_header() {
    let server = echo_server(b"SSH-2.0-http\r\n\0").await;
    let proxy = http_proxy(server, "200").await;
    let route = Route::HttpConnect {
        host: HostName::parse("127.0.0.1").unwrap(),
        port: proxy.port(),
    };
    let mut io = open(&route, &endpoint("127.0.0.1", server.port()), TIMEOUT)
        .await
        .unwrap();
    roundtrip(&mut io, b"SSH-2.0-http\r\n\0").await;
}

#[tokio::test]
async fn http_connect_reports_the_proxy_status() {
    let proxy = http_proxy("127.0.0.1:9".parse().unwrap(), "403").await;
    let route = Route::HttpConnect {
        host: HostName::parse("127.0.0.1").unwrap(),
        port: proxy.port(),
    };
    let error = open(&route, &endpoint("db", 22), TIMEOUT)
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&error, ConnectError::Proxy(m) if m.contains("403")),
        "{error:?}"
    );
}

#[tokio::test]
async fn http_connect_rejects_an_endless_header() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        // Read the request first and close only after the client does:
        // closing with unread data sends a reset, and Windows then drops
        // what the client has not read yet.
        let mut request = vec![0u8; 1024];
        let _ = client.read(&mut request).await;
        let _ = client.write_all(b"HTTP/1.1 200 OK\r\n").await;
        let filler = vec![b'x'; 64 * 1024];
        let _ = client.write_all(&filler).await;
        let mut rest = [0u8; 1];
        let _ = client.read(&mut rest).await;
    });
    let route = Route::HttpConnect {
        host: HostName::parse("127.0.0.1").unwrap(),
        port,
    };
    let error = open(&route, &endpoint("db", 22), TIMEOUT)
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&error, ConnectError::Proxy(m) if m.contains("too large")),
        "{error:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn proxy_command_carries_the_stream_over_stdio() {
    // `cat` echoes stdin to stdout: a stand-in for `nc %h %p`.
    let mut io = open(
        &Route::ProxyCommand("cat".into()),
        &endpoint("db", 22),
        TIMEOUT,
    )
    .await
    .unwrap();
    io.write_all(b"ping").await.unwrap();
    let mut back = [0u8; 4];
    io.read_exact(&mut back).await.unwrap();
    assert_eq!(&back, b"ping");
}

#[cfg(unix)]
#[tokio::test]
async fn proxy_command_receives_expanded_tokens() {
    let mut io = open(
        &Route::ProxyCommand("printf '%h:%p:%r'".into()),
        &endpoint("db.internal", 2222),
        TIMEOUT,
    )
    .await
    .unwrap();
    let mut out = String::new();
    io.read_to_string(&mut out).await.unwrap();
    assert_eq!(out, "db.internal:2222:u");
}

#[tokio::test]
async fn proxy_command_with_an_unknown_token_is_rejected_before_spawning() {
    let error = open(
        &Route::ProxyCommand("nc %h %C".into()),
        &endpoint("db", 22),
        TIMEOUT,
    )
    .await
    .err()
    .unwrap();
    assert!(
        matches!(&error, ConnectError::Proxy(m) if m.contains("%C")),
        "{error:?}"
    );
}
