// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The byte stream to the first SSH hop: a direct TCP connection, a
//! `ProxyCommand`, a SOCKS5 proxy, or an HTTP CONNECT proxy.

use std::io;
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};
use std::time::Duration;

use tauminus_app::ports::ConnectError;
use tauminus_domain::{Endpoint, HostName, Route, expand_proxy_command};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tracing::debug;

/// Any duplex byte stream russh can run over.
pub(crate) trait Io: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Io for T {}

pub(crate) type BoxedIo = Box<dyn Io>;

/// Opens the stream to `endpoint` along `route`. `timeout` bounds each TCP
/// connect and each proxy handshake.
pub(crate) async fn open(
    route: &Route,
    endpoint: &Endpoint,
    timeout: Duration,
) -> Result<BoxedIo, ConnectError> {
    match route {
        Route::Direct => Ok(Box::new(tcp(&endpoint.host, endpoint.port, timeout).await?)),
        Route::ProxyCommand(template) => {
            let command = expand_proxy_command(template, endpoint)
                .map_err(|e| ConnectError::Proxy(e.to_string()))?;
            Ok(Box::new(ProxyCommandIo::spawn(&command)?))
        }
        Route::Socks5 { host, port } => {
            let mut stream = tcp(host, *port, timeout).await?;
            within(timeout, host, *port, socks5_connect(&mut stream, endpoint)).await?;
            Ok(Box::new(stream))
        }
        Route::HttpConnect { host, port } => {
            let mut stream = tcp(host, *port, timeout).await?;
            within(timeout, host, *port, http_connect(&mut stream, endpoint)).await?;
            Ok(Box::new(stream))
        }
    }
}

async fn within<T>(
    timeout: Duration,
    host: &HostName,
    port: u16,
    work: impl Future<Output = Result<T, ConnectError>>,
) -> Result<T, ConnectError> {
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| ConnectError::Timeout {
            host: host.to_string(),
            port,
            seconds: timeout.as_secs(),
        })?
}

/// Resolves `host` and tries each address in order until one connects.
async fn tcp(host: &HostName, port: u16, timeout: Duration) -> Result<TcpStream, ConnectError> {
    let addresses: Vec<_> = within(timeout, host, port, async {
        tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|_| ConnectError::Resolve {
                host: host.to_string(),
            })
    })
    .await?
    .collect();
    if addresses.is_empty() {
        return Err(ConnectError::Resolve {
            host: host.to_string(),
        });
    }
    let mut last = None;
    for address in addresses {
        match tokio::time::timeout(timeout, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => {
                // Keystrokes are tiny packets; Nagle would delay each one.
                stream.set_nodelay(true)?;
                return Ok(stream);
            }
            Ok(Err(e)) => {
                debug!(%address, error = %e, "connect failed");
                last = Some(if e.kind() == io::ErrorKind::ConnectionRefused {
                    ConnectError::Refused {
                        host: host.to_string(),
                        port,
                    }
                } else {
                    ConnectError::Io(e)
                });
            }
            Err(_) => {
                last = Some(ConnectError::Timeout {
                    host: host.to_string(),
                    port,
                    seconds: timeout.as_secs(),
                });
            }
        }
    }
    Err(last.unwrap_or(ConnectError::Resolve {
        host: host.to_string(),
    }))
}

/// A `ProxyCommand` child whose stdout and stdin carry the connection. The
/// child is killed when the stream drops.
pub(crate) struct ProxyCommandIo {
    _child: Child,
    stdout: ChildStdout,
    stdin: ChildStdin,
}

impl ProxyCommandIo {
    fn spawn(command: &str) -> Result<Self, ConnectError> {
        let mut process = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", command]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", command]);
            c
        };
        let mut child = process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ConnectError::Proxy(format!("could not start ProxyCommand: {e}")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ConnectError::Proxy("no stdout".into()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ConnectError::Proxy("no stdin".into()))?;
        Ok(Self {
            _child: child,
            stdout,
            stdin,
        })
    }
}

impl AsyncRead for ProxyCommandIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for ProxyCommandIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stdin).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stdin).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stdin).poll_shutdown(cx)
    }
}

/// RFC 1928, no authentication. Names go to the proxy unresolved, so the
/// lookup happens there and does not leak from the client.
async fn socks5_connect<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    endpoint: &Endpoint,
) -> Result<(), ConnectError> {
    let proxy = |message: &str| ConnectError::Proxy(format!("SOCKS5: {message}"));
    stream.write_all(&[5, 1, 0]).await?;
    let mut choice = [0u8; 2];
    stream.read_exact(&mut choice).await?;
    if choice != [5, 0] {
        return Err(proxy(
            "the proxy requires authentication, which is not supported",
        ));
    }
    let host = endpoint.host.as_str();
    let mut request = Vec::with_capacity(7 + host.len());
    request.extend_from_slice(&[5, 1, 0]);
    // Address literals go as addresses; some proxies refuse them as names.
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => {
            request.push(1);
            request.extend_from_slice(&v4.octets());
        }
        Ok(std::net::IpAddr::V6(v6)) => {
            request.push(4);
            request.extend_from_slice(&v6.octets());
        }
        Err(_) => {
            let length = u8::try_from(host.len()).map_err(|_| proxy("host name too long"))?;
            request.extend_from_slice(&[3, length]);
            request.extend_from_slice(host.as_bytes());
        }
    }
    request.extend_from_slice(&endpoint.port.to_be_bytes());
    stream.write_all(&request).await?;

    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    if head[0] != 5 {
        return Err(proxy("unexpected reply version"));
    }
    if head[1] != 0 {
        return Err(proxy(socks5_reply_meaning(head[1])));
    }
    // Consume the bound address so the SSH banner is the next byte read.
    let address_len = match head[3] {
        1 => 4,
        4 => 16,
        3 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            usize::from(len[0])
        }
        _ => return Err(proxy("unknown address type in reply")),
    };
    let mut rest = vec![0u8; address_len + 2];
    stream.read_exact(&mut rest).await?;
    Ok(())
}

fn socks5_reply_meaning(code: u8) -> &'static str {
    match code {
        1 => "general failure",
        2 => "connection not allowed by ruleset",
        3 => "network unreachable",
        4 => "host unreachable",
        5 => "connection refused",
        6 => "TTL expired",
        7 => "command not supported",
        8 => "address type not supported",
        _ => "unknown error",
    }
}

/// Largest proxy response header accepted, so a hostile proxy cannot make us
/// buffer without bound.
const MAX_HTTP_HEADER: usize = 8 * 1024;

/// HTTP CONNECT tunnel. Reads the response one byte at a time: the SSH server
/// speaks first, so its banner can arrive right behind the 200 response, and
/// reading past the header would lose it. Costs at most `MAX_HTTP_HEADER`
/// reads, once per connection.
async fn http_connect<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    endpoint: &Endpoint,
) -> Result<(), ConnectError> {
    let authority = if endpoint.host.as_str().contains(':') {
        format!("[{}]:{}", endpoint.host, endpoint.port)
    } else {
        format!("{}:{}", endpoint.host, endpoint.port)
    };
    stream
        .write_all(format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes())
        .await?;
    let mut header = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= MAX_HTTP_HEADER {
            return Err(ConnectError::Proxy(
                "HTTP proxy response header too large".into(),
            ));
        }
        if stream.read(&mut byte).await? == 0 {
            return Err(ConnectError::Proxy(
                "HTTP proxy closed the connection".into(),
            ));
        }
        header.push(byte[0]);
    }
    let status_line = String::from_utf8_lossy(&header);
    let status_line = status_line.lines().next().unwrap_or_default();
    let mut parts = status_line.split_whitespace();
    let version = parts.next().unwrap_or_default();
    let code = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") || code != "200" {
        return Err(ConnectError::Proxy(format!(
            "HTTP proxy refused: {status_line}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
