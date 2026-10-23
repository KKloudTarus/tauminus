// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The server side of a SOCKS handshake, for dynamic forwarding: SOCKS5
//! (RFC 1928) with no authentication, and SOCKS4 and 4a. Only `CONNECT`.
//!
//! Every read has a fixed bound: a SOCKS5 request is at most 262 bytes
//! after a greeting of at most 257, and a SOCKS4 user id or 4a name at most
//! 255. The caller bounds the time.

use std::net::{Ipv4Addr, Ipv6Addr};

use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Longest SOCKS 4 user id or 4a host name read, without the NUL.
const MAX_FIELD: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Version {
    Four,
    Five,
}

/// What the client asked to reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) version: Version,
    /// An IP address or a host name for the server to resolve.
    pub(crate) host: String,
    pub(crate) port: u16,
}

#[derive(Debug, Error)]
pub(crate) enum SocksError {
    #[error("not a SOCKS 4 or 5 client (first byte {0:#04x})")]
    Version(u8),
    #[error("the client offers no method without authentication")]
    NoMethod,
    #[error("only CONNECT is supported, not command {0}")]
    Command(u8),
    #[error("unknown address type {0}")]
    AddressType(u8),
    #[error("the host name is not a valid name")]
    Name,
    #[error("port 0 cannot be reached")]
    Port,
    #[error("a field is longer than {MAX_FIELD} bytes")]
    TooLong,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Why a connect failed, as the reply reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    General,
    CommandNotSupported,
    AddressTypeNotSupported,
}

/// Reads the greeting and the request. On a request it cannot serve it
/// sends the refusal itself before returning the error.
pub(crate) async fn accept<S>(stream: &mut S) -> Result<Request, SocksError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match stream.read_u8().await? {
        4 => accept_four(stream).await,
        5 => accept_five(stream).await,
        other => Err(SocksError::Version(other)),
    }
}

async fn accept_five<S>(stream: &mut S) -> Result<Request, SocksError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let count = usize::from(stream.read_u8().await?);
    let mut methods = [0_u8; 255];
    stream.read_exact(&mut methods[..count]).await?;
    if !methods[..count].contains(&0) {
        stream.write_all(&[5, 0xff]).await?;
        return Err(SocksError::NoMethod);
    }
    stream.write_all(&[5, 0]).await?;
    let mut head = [0_u8; 4];
    stream.read_exact(&mut head).await?;
    let [version, command, _reserved, kind] = head;
    if version != 5 {
        return Err(SocksError::Version(version));
    }
    let host = match kind {
        1 => {
            let mut ip = [0_u8; 4];
            stream.read_exact(&mut ip).await?;
            Ipv4Addr::from(ip).to_string()
        }
        3 => {
            let len = usize::from(stream.read_u8().await?);
            let mut name = [0_u8; 255];
            stream.read_exact(&mut name[..len]).await?;
            host_name(&name[..len])?
        }
        4 => {
            let mut ip = [0_u8; 16];
            stream.read_exact(&mut ip).await?;
            Ipv6Addr::from(ip).to_string()
        }
        other => {
            refuse(stream, Version::Five, Refusal::AddressTypeNotSupported).await?;
            return Err(SocksError::AddressType(other));
        }
    };
    let port = stream.read_u16().await?;
    if command != 1 {
        refuse(stream, Version::Five, Refusal::CommandNotSupported).await?;
        return Err(SocksError::Command(command));
    }
    if port == 0 {
        refuse(stream, Version::Five, Refusal::General).await?;
        return Err(SocksError::Port);
    }
    Ok(Request {
        version: Version::Five,
        host,
        port,
    })
}

async fn accept_four<S>(stream: &mut S) -> Result<Request, SocksError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let command = stream.read_u8().await?;
    let port = stream.read_u16().await?;
    let mut ip = [0_u8; 4];
    stream.read_exact(&mut ip).await?;
    // The user id means nothing here; it is read and dropped.
    read_nul_terminated(stream).await?;
    // SOCKS4a: 0.0.0.x with x non-zero means a name follows.
    let host = if ip[..3] == [0, 0, 0] && ip[3] != 0 {
        host_name(&read_nul_terminated(stream).await?)?
    } else {
        Ipv4Addr::from(ip).to_string()
    };
    if command != 1 {
        refuse(stream, Version::Four, Refusal::CommandNotSupported).await?;
        return Err(SocksError::Command(command));
    }
    if port == 0 {
        refuse(stream, Version::Four, Refusal::General).await?;
        return Err(SocksError::Port);
    }
    Ok(Request {
        version: Version::Four,
        host,
        port,
    })
}

async fn read_nul_terminated<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>, SocksError> {
    let mut field = Vec::new();
    loop {
        match stream.read_u8().await? {
            0 => return Ok(field),
            _ if field.len() == MAX_FIELD => return Err(SocksError::TooLong),
            byte => field.push(byte),
        }
    }
}

/// A name the server can resolve: printable ASCII host characters only, so
/// nothing odd reaches the server's resolver or a log line.
fn host_name(bytes: &[u8]) -> Result<String, SocksError> {
    let text = std::str::from_utf8(bytes).map_err(|_| SocksError::Name)?;
    tauminus_domain::HostName::parse(text).map_err(|_| SocksError::Name)?;
    Ok(text.to_owned())
}

/// Tells the client its connection is through.
pub(crate) async fn grant<S: AsyncWrite + Unpin>(
    stream: &mut S,
    version: Version,
) -> std::io::Result<()> {
    match version {
        Version::Four => stream.write_all(&[0, 0x5a, 0, 0, 0, 0, 0, 0]).await,
        Version::Five => stream.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await,
    }
}

/// Tells the client its connection failed.
pub(crate) async fn refuse<S: AsyncWrite + Unpin>(
    stream: &mut S,
    version: Version,
    why: Refusal,
) -> std::io::Result<()> {
    match version {
        Version::Four => stream.write_all(&[0, 0x5b, 0, 0, 0, 0, 0, 0]).await,
        Version::Five => {
            let code = match why {
                Refusal::General => 1,
                Refusal::CommandNotSupported => 7,
                Refusal::AddressTypeNotSupported => 8,
            };
            stream.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `accept` on `input` and returns its result and what it wrote.
    async fn run(input: &[u8]) -> (Result<Request, SocksError>, Vec<u8>) {
        let (mut client, mut server) = tokio::io::duplex(64 * 1024);
        client.write_all(input).await.unwrap();
        client.shutdown().await.unwrap();
        let result = accept(&mut server).await;
        drop(server);
        let mut written = Vec::new();
        client.read_to_end(&mut written).await.unwrap();
        (result, written)
    }

    fn request(version: Version, host: &str, port: u16) -> Request {
        Request {
            version,
            host: host.to_owned(),
            port,
        }
    }

    #[tokio::test]
    async fn socks5_reads_every_address_type() {
        let ipv4 = [5, 1, 0, 5, 1, 0, 1, 10, 0, 0, 7, 0x1f, 0x90];
        let (result, written) = run(&ipv4).await;
        assert_eq!(result.unwrap(), request(Version::Five, "10.0.0.7", 8080));
        assert_eq!(written, [5, 0]);

        let mut name = vec![5, 2, 2, 0, 5, 1, 0, 3, 11];
        name.extend_from_slice(b"db.internal");
        name.extend_from_slice(&5432_u16.to_be_bytes());
        let (result, _) = run(&name).await;
        assert_eq!(result.unwrap(), request(Version::Five, "db.internal", 5432));

        let mut ipv6 = vec![5, 1, 0, 5, 1, 0, 4];
        ipv6.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        ipv6.extend_from_slice(&22_u16.to_be_bytes());
        let (result, _) = run(&ipv6).await;
        assert_eq!(result.unwrap(), request(Version::Five, "::1", 22));
    }

    #[tokio::test]
    async fn socks5_without_the_no_auth_method_is_refused() {
        let (result, written) = run(&[5, 1, 2]).await;
        assert!(matches!(result, Err(SocksError::NoMethod)));
        assert_eq!(written, [5, 0xff]);
    }

    #[tokio::test]
    async fn socks5_bind_and_unknown_address_types_get_their_codes() {
        let (result, written) = run(&[5, 1, 0, 5, 2, 0, 1, 1, 2, 3, 4, 0, 80]).await;
        assert!(matches!(result, Err(SocksError::Command(2))));
        assert_eq!(written[2..4], [5, 7]);
        let (result, written) = run(&[5, 1, 0, 5, 1, 0, 9]).await;
        assert!(matches!(result, Err(SocksError::AddressType(9))));
        assert_eq!(written[2..4], [5, 8]);
    }

    #[tokio::test]
    async fn names_with_odd_bytes_are_refused() {
        for name in [&b"a b"[..], b"a\nb", b"\xff\xfe", b""] {
            let mut input = vec![5, 1, 0, 5, 1, 0, 3, u8::try_from(name.len()).unwrap()];
            input.extend_from_slice(name);
            input.extend_from_slice(&[0, 80]);
            let (result, _) = run(&input).await;
            assert!(matches!(result, Err(SocksError::Name)), "{name:?}");
        }
    }

    #[tokio::test]
    async fn port_zero_is_refused() {
        let (result, written) = run(&[5, 1, 0, 5, 1, 0, 1, 1, 2, 3, 4, 0, 0]).await;
        assert!(matches!(result, Err(SocksError::Port)));
        assert_eq!(written[2..4], [5, 1]);
        let (result, written) = run(&[4, 1, 0, 0, 1, 2, 3, 4, 0]).await;
        assert!(matches!(result, Err(SocksError::Port)));
        assert_eq!(written, [0, 0x5b, 0, 0, 0, 0, 0, 0]);
    }

    #[tokio::test]
    async fn socks4_and_4a() {
        let mut four = vec![4, 1, 0, 80, 192, 168, 0, 1];
        four.extend_from_slice(b"user\0");
        let (result, _) = run(&four).await;
        assert_eq!(result.unwrap(), request(Version::Four, "192.168.0.1", 80));

        let mut four_a = vec![4, 1, 1, 187, 0, 0, 0, 9, 0];
        four_a.extend_from_slice(b"example.org\0");
        let (result, _) = run(&four_a).await;
        assert_eq!(result.unwrap(), request(Version::Four, "example.org", 443));
    }

    #[tokio::test]
    async fn a_socks4_field_without_an_end_stops_at_the_bound() {
        let mut input = vec![4, 1, 0, 80, 1, 2, 3, 4];
        input.extend(std::iter::repeat_n(b'a', 10_000));
        let (result, _) = run(&input).await;
        assert!(matches!(result, Err(SocksError::TooLong)));
    }

    #[tokio::test]
    async fn a_truncated_request_is_an_io_error_and_other_protocols_are_refused() {
        let (result, _) = run(&[5, 1, 0, 5, 1]).await;
        assert!(matches!(result, Err(SocksError::Io(_))));
        let (result, written) = run(b"GET / HTTP/1.1\r\n").await;
        assert!(matches!(result, Err(SocksError::Version(b'G'))));
        assert_eq!(written, [0_u8; 0]);
    }

    #[tokio::test]
    async fn replies_have_the_lengths_clients_expect() {
        let mut out = Vec::new();
        grant(&mut out, Version::Five).await.unwrap();
        assert_eq!(out.len(), 10);
        out.clear();
        refuse(&mut out, Version::Four, Refusal::General)
            .await
            .unwrap();
        assert_eq!(out, [0, 0x5b, 0, 0, 0, 0, 0, 0]);
    }
}
