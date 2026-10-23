// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port forwarding rules saved with a host: local (`ssh -L`), remote
//! (`ssh -R`), and dynamic (`ssh -D`, a SOCKS proxy).

use std::net::{IpAddr, Ipv4Addr};

use thiserror::Error;

use crate::hosts::{ForwardId, Label};
use crate::ssh::HostName;

/// Most rules one host keeps.
pub const MAX_FORWARDS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ForwardError {
    #[error("at most {MAX_FORWARDS} forwarding rules per host")]
    TooMany,
    #[error("forwarding rule {0} is listed twice")]
    Duplicate(String),
    #[error("the destination port must be between 1 and 65535")]
    DestinationPort,
}

/// Where a listener binds. Anything but loopback opens the tunnel to the
/// network, as OpenSSH's `GatewayPorts` does, so it is a choice the rule
/// states rather than a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindAddress {
    /// 127.0.0.1 and `::1` on this machine; `localhost` on the server.
    Loopback,
    /// Every interface: `0.0.0.0` and `::` here, `""` on the server
    /// (RFC 4254 section 7.1).
    Any,
    Ip(IpAddr),
}

impl BindAddress {
    /// True when only this machine (or, for a remote rule, only the
    /// server) can reach the listener.
    pub fn is_loopback(self) -> bool {
        match self {
            Self::Loopback => true,
            Self::Any => false,
            // `::ffff:127.0.0.1` is loopback too.
            Self::Ip(ip) => ip.to_canonical().is_loopback(),
        }
    }

    /// The address a remote rule asks the server to bind, as
    /// `tcpip-forward` spells it.
    pub fn remote_spelling(self) -> String {
        match self {
            Self::Loopback => "localhost".to_owned(),
            Self::Any => String::new(),
            Self::Ip(ip) => ip.to_canonical().to_string(),
        }
    }

    /// The addresses a local listener binds, first one required. Loopback
    /// also tries `::1`, which a machine without IPv6 lacks.
    pub fn local_addresses(self) -> Vec<IpAddr> {
        match self {
            Self::Loopback => vec![
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
            ],
            Self::Any => vec![
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
            ],
            // `::ffff:a.b.c.d` binds as the IPv4 address it stands for.
            Self::Ip(ip) => vec![ip.to_canonical()],
        }
    }
}

/// A listener: address and port. Port 0 lets the OS (or the server) pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Bind {
    pub address: BindAddress,
    pub port: u16,
}

/// Where forwarded connections go.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Destination {
    pub host: HostName,
    port: u16,
}

impl Destination {
    pub fn new(host: HostName, port: u16) -> Result<Self, ForwardError> {
        if port == 0 {
            return Err(ForwardError::DestinationPort);
        }
        Ok(Self { host, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Forward {
    /// Listens here; each connection goes to `to` as the server sees it.
    Local { bind: Bind, to: Destination },
    /// The server listens; each connection comes back to `to` as this
    /// machine sees it.
    Remote { bind: Bind, to: Destination },
    /// Listens here as a SOCKS proxy; the client names each destination.
    Dynamic { bind: Bind },
}

impl Forward {
    pub fn bind(&self) -> Bind {
        match self {
            Self::Local { bind, .. } | Self::Remote { bind, .. } | Self::Dynamic { bind } => *bind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardRule {
    pub id: ForwardId,
    pub label: Label,
    pub forward: Forward,
    /// Start when the host connects.
    pub auto_start: bool,
}

/// Checks a host's rules: at most [`MAX_FORWARDS`], ids unique. O(r²) with
/// r ≤ 64, which beats hashing at this size.
pub(crate) fn check_rules(rules: &[ForwardRule]) -> Result<(), ForwardError> {
    if rules.len() > MAX_FORWARDS {
        return Err(ForwardError::TooMany);
    }
    for (index, rule) in rules.iter().enumerate() {
        if rules[..index].iter().any(|r| r.id == rule.id) {
            return Err(ForwardError::Duplicate(rule.id.to_string()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::Ipv6Addr;

    use super::*;

    fn rule(id: &str) -> ForwardRule {
        ForwardRule {
            id: ForwardId::parse(id).unwrap(),
            label: Label::parse("db").unwrap(),
            forward: Forward::Dynamic {
                bind: Bind {
                    address: BindAddress::Loopback,
                    port: 1080,
                },
            },
            auto_start: false,
        }
    }

    #[test]
    fn loopback_is_told_apart_from_the_network() {
        assert!(BindAddress::Loopback.is_loopback());
        assert!(BindAddress::Ip(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2))).is_loopback());
        assert!(BindAddress::Ip(IpAddr::V6(Ipv6Addr::LOCALHOST)).is_loopback());
        let mapped: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(BindAddress::Ip(mapped).is_loopback());
        let mapped_lan: IpAddr = "::ffff:10.0.0.1".parse().unwrap();
        assert!(!BindAddress::Ip(mapped_lan).is_loopback());
        assert!(!BindAddress::Any.is_loopback());
        assert!(!BindAddress::Ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2))).is_loopback());
    }

    #[test]
    fn the_server_gets_the_rfc_spelling() {
        assert_eq!(BindAddress::Loopback.remote_spelling(), "localhost");
        assert_eq!(BindAddress::Any.remote_spelling(), "");
        let ip = BindAddress::Ip(IpAddr::V6(Ipv6Addr::LOCALHOST));
        assert_eq!(ip.remote_spelling(), "::1");
        let mapped = BindAddress::Ip("::ffff:10.0.0.1".parse().unwrap());
        assert_eq!(mapped.remote_spelling(), "10.0.0.1");
        assert_eq!(
            mapped.local_addresses(),
            ["10.0.0.1".parse::<IpAddr>().unwrap()]
        );
    }

    #[test]
    fn a_destination_needs_a_port() {
        let host = HostName::parse("db.internal").unwrap();
        assert_eq!(
            Destination::new(host.clone(), 0),
            Err(ForwardError::DestinationPort)
        );
        assert_eq!(Destination::new(host, 5432).unwrap().port(), 5432);
    }

    #[test]
    fn rules_are_bounded_and_unique() {
        assert!(check_rules(&[rule("a"), rule("b")]).is_ok());
        assert_eq!(
            check_rules(&[rule("a"), rule("a")]),
            Err(ForwardError::Duplicate("a".into()))
        );
        let many: Vec<_> = (0..=MAX_FORWARDS).map(|i| rule(&format!("r{i}"))).collect();
        assert_eq!(check_rules(&many), Err(ForwardError::TooMany));
        assert!(check_rules(&many[..MAX_FORWARDS]).is_ok());
    }
}
