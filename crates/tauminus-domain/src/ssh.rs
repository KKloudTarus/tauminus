// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! What an SSH connection targets, validated before any network or process
//! work starts.
//!
//! Host and user names end up in a `ProxyCommand` that a shell runs, so they
//! are restricted to characters with no meaning to a shell. A name that
//! passes validation cannot inject a command.

use std::fmt;
use std::path::PathBuf;

use thiserror::Error;

/// A DNS name, an IPv4 address, or an IPv6 literal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostName(String);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NameError {
    #[error("host name is empty")]
    EmptyHost,
    #[error("host name is longer than 253 characters")]
    HostTooLong,
    #[error(
        "host name label {label:?} is empty, longer than 63 characters, or starts or ends with '-'"
    )]
    BadLabel { label: String },
    #[error("host name contains {found:?}, which is not allowed")]
    HostCharacter { found: char },
    #[error("IPv6 address {0:?} is not valid")]
    BadIpv6(String),
    #[error("user name is empty or longer than 64 characters")]
    UserLength,
    #[error("user name contains {found:?}, which is not allowed")]
    UserCharacter { found: char },
    #[error("port must be between 1 and 65535")]
    Port,
}

impl HostName {
    /// O(n) in the length of the name.
    pub fn parse(text: &str) -> Result<Self, NameError> {
        if text.is_empty() {
            return Err(NameError::EmptyHost);
        }
        if text.len() > 253 {
            return Err(NameError::HostTooLong);
        }
        if text.contains(':') {
            return parse_ipv6(text).map(|()| Self(text.to_owned()));
        }
        if let Some(found) = text
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
        {
            return Err(NameError::HostCharacter { found });
        }
        for label in text.trim_end_matches('.').split('.') {
            if label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
            {
                return Err(NameError::BadLabel {
                    label: label.to_owned(),
                });
            }
        }
        Ok(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HostName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// IPv6 literal: hex groups and colons, optionally an embedded IPv4 tail.
/// Zone ids (`%eth0`) are rejected because `%` starts a `ProxyCommand` token.
fn parse_ipv6(text: &str) -> Result<(), NameError> {
    let bad = || NameError::BadIpv6(text.to_owned());
    if !text
        .chars()
        .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
    {
        return Err(bad());
    }
    let compressions = text.matches("::").count();
    if compressions > 1 || text.contains(":::") {
        return Err(bad());
    }
    let groups: Vec<&str> = text.split(':').collect();
    if groups.len() < 3 && compressions == 0 || groups.len() > 9 {
        return Err(bad());
    }
    for (index, group) in groups.iter().enumerate() {
        let last = index == groups.len() - 1;
        let ipv4_tail = last && group.contains('.');
        let valid = if ipv4_tail {
            group.split('.').count() == 4
                && group.split('.').all(|octet| octet.parse::<u8>().is_ok())
        } else {
            group.len() <= 4 && group.chars().all(|c| c.is_ascii_hexdigit())
        };
        if !valid {
            return Err(bad());
        }
    }
    Ok(())
}

/// A login name. Allows the characters real directories use (letters,
/// digits, `.`, `_`, `-`, `@` for `user@domain`), nothing a shell treats
/// specially.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UserName(String);

impl UserName {
    pub fn parse(text: &str) -> Result<Self, NameError> {
        if text.is_empty() || text.chars().count() > 64 {
            return Err(NameError::UserLength);
        }
        if let Some(found) = text
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@')))
        {
            return Err(NameError::UserCharacter { found });
        }
        if text.starts_with('-') {
            // A leading dash reads as an option to `ssh` and friends.
            return Err(NameError::UserCharacter { found: '-' });
        }
        Ok(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UserName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where to log in: user, host, and port.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub user: UserName,
    pub host: HostName,
    pub port: u16,
}

impl Endpoint {
    pub fn new(user: UserName, host: HostName, port: u16) -> Result<Self, NameError> {
        if port == 0 {
            return Err(NameError::Port);
        }
        Ok(Self { user, host, port })
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.port == 22 {
            write!(f, "{}@{}", self.user, self.host)
        } else {
            write!(f, "{}@{}:{}", self.user, self.host, self.port)
        }
    }
}

/// How the first hop's TCP connection is made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Direct,
    /// A command whose stdin and stdout carry the connection, as in OpenSSH.
    /// The template may use `%h`, `%p`, `%r`, `%n`, and `%%`.
    ProxyCommand(String),
    Socks5 {
        host: HostName,
        port: u16,
    },
    HttpConnect {
        host: HostName,
        port: u16,
    },
}

/// An authentication method, as in OpenSSH `PreferredAuthentications`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthMethod {
    /// Keys held by ssh-agent or Pageant.
    Agent,
    /// Key files on disk.
    PublicKey,
    KeyboardInteractive,
    Password,
}

/// How to authenticate. `methods` are tried in order, each only if the
/// server still accepts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthOptions {
    pub methods: Vec<AuthMethod>,
    /// Explicit key files. Empty means the default `~/.ssh/id_*` names.
    pub identity_files: Vec<PathBuf>,
    /// Offer only the key files, never other keys the agent holds, like
    /// OpenSSH `IdentitiesOnly`. With none listed, the default names.
    pub identities_only: bool,
}

impl Default for AuthOptions {
    fn default() -> Self {
        Self {
            methods: vec![
                AuthMethod::Agent,
                AuthMethod::PublicKey,
                AuthMethod::KeyboardInteractive,
                AuthMethod::Password,
            ],
            identity_files: Vec::new(),
            identities_only: false,
        }
    }
}

/// A complete connection: jump hosts in order, then the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    pub destination: Endpoint,
    pub jumps: Vec<Endpoint>,
    pub route: Route,
    pub auth: AuthOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TargetError {
    #[error("at most {max} jump hosts are supported, got {got}")]
    TooManyJumps { max: usize, got: usize },
    #[error("ProxyCommand and jump hosts cannot be combined")]
    ProxyWithJumps,
}

impl SshTarget {
    /// OpenSSH has no limit, but a chain longer than this is almost certainly
    /// a loop in someone's config.
    pub const MAX_JUMPS: usize = 8;

    pub fn new(
        destination: Endpoint,
        jumps: Vec<Endpoint>,
        route: Route,
        auth: AuthOptions,
    ) -> Result<Self, TargetError> {
        if jumps.len() > Self::MAX_JUMPS {
            return Err(TargetError::TooManyJumps {
                max: Self::MAX_JUMPS,
                got: jumps.len(),
            });
        }
        if !jumps.is_empty() && matches!(route, Route::ProxyCommand(_)) {
            return Err(TargetError::ProxyWithJumps);
        }
        Ok(Self {
            destination,
            jumps,
            route,
            auth,
        })
    }

    /// The first host the TCP connection reaches.
    pub fn first_hop(&self) -> &Endpoint {
        self.jumps.first().unwrap_or(&self.destination)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProxyCommandError {
    #[error("unknown token %{0} in ProxyCommand")]
    UnknownToken(char),
    #[error("ProxyCommand ends with a lone %")]
    TrailingPercent,
}

/// Expands `%h` host, `%p` port, `%r` user, `%n` host as given, `%%` percent.
/// Host and user are validated names, so substituting them into a shell
/// command cannot inject syntax. O(n) in the template length.
pub fn expand_proxy_command(
    template: &str,
    endpoint: &Endpoint,
) -> Result<String, ProxyCommandError> {
    let mut out = String::with_capacity(template.len() + 32);
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('h' | 'n') => out.push_str(endpoint.host.as_str()),
            Some('p') => out.push_str(&endpoint.port.to_string()),
            Some('r') => out.push_str(endpoint.user.as_str()),
            Some('%') => out.push('%'),
            Some(other) => return Err(ProxyCommandError::UnknownToken(other)),
            None => return Err(ProxyCommandError::TrailingPercent),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(user: &str, host: &str, port: u16) -> Endpoint {
        Endpoint::new(
            UserName::parse(user).unwrap(),
            HostName::parse(host).unwrap(),
            port,
        )
        .unwrap()
    }

    #[test]
    fn accepts_dns_names_ipv4_and_ipv6() {
        for host in [
            "example.com",
            "prod-db-2.internal",
            "a",
            "my_host",
            "10.0.0.1",
            "example.com.",
            "::1",
            "fe80::1",
            "2001:db8::8a2e:370:7334",
            "::ffff:192.0.2.1",
        ] {
            assert!(HostName::parse(host).is_ok(), "{host}");
        }
    }

    #[test]
    fn rejects_shell_syntax_and_malformed_hosts() {
        for host in [
            "",
            "host;rm -rf ~",
            "$(id)",
            "a b",
            "`x`",
            "host|nc",
            "-oProxyCommand=x",
            "a..b",
            "-a.com",
            "a-.com",
            "fe80::1%eth0",
            "1:::2",
            "1::2::3",
            "12345::",
            "::ffff:300.0.0.1",
        ] {
            assert!(
                HostName::parse(host).is_err(),
                "{host:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_overlong_hosts_and_labels() {
        assert_eq!(
            HostName::parse(&"a".repeat(254)),
            Err(NameError::HostTooLong)
        );
        assert!(matches!(
            HostName::parse(&format!("{}.com", "a".repeat(64))),
            Err(NameError::BadLabel { .. })
        ));
        assert!(HostName::parse(&format!("{}.com", "a".repeat(63))).is_ok());
    }

    #[test]
    fn user_names_allow_directory_forms_and_reject_shell_syntax() {
        for user in [
            "root",
            "deploy",
            "first.last",
            "svc_backup",
            "alice@corp.example",
        ] {
            assert!(UserName::parse(user).is_ok(), "{user}");
        }
        for user in ["", "a b", "x;y", "$USER", "-oX", "a'b", &"u".repeat(65)] {
            assert!(UserName::parse(user).is_err(), "{user:?}");
        }
    }

    #[test]
    fn port_zero_is_rejected() {
        let result = Endpoint::new(
            UserName::parse("u").unwrap(),
            HostName::parse("h").unwrap(),
            0,
        );
        assert_eq!(result, Err(NameError::Port));
    }

    #[test]
    fn endpoint_display_hides_the_default_port() {
        assert_eq!(endpoint("root", "db", 22).to_string(), "root@db");
        assert_eq!(endpoint("root", "db", 2222).to_string(), "root@db:2222");
    }

    #[test]
    fn expands_proxy_command_tokens() {
        let target = endpoint("deploy", "db.internal", 2222);
        assert_eq!(
            expand_proxy_command("nc -X 5 -x proxy:1080 %h %p # %r %n 100%%", &target),
            Ok("nc -X 5 -x proxy:1080 db.internal 2222 # deploy db.internal 100%".into())
        );
    }

    #[test]
    fn proxy_command_rejects_unknown_tokens() {
        let target = endpoint("u", "h", 22);
        assert_eq!(
            expand_proxy_command("ssh -W %h:%p %C", &target),
            Err(ProxyCommandError::UnknownToken('C'))
        );
        assert_eq!(
            expand_proxy_command("nc %h %", &target),
            Err(ProxyCommandError::TrailingPercent)
        );
    }

    #[test]
    fn target_limits_jumps_and_forbids_proxy_command_with_jumps() {
        let hop = endpoint("u", "bastion", 22);
        let jumps = vec![hop.clone(); SshTarget::MAX_JUMPS + 1];
        assert!(matches!(
            SshTarget::new(hop.clone(), jumps, Route::Direct, AuthOptions::default()),
            Err(TargetError::TooManyJumps { .. })
        ));
        assert_eq!(
            SshTarget::new(
                hop.clone(),
                vec![hop.clone()],
                Route::ProxyCommand("nc %h %p".into()),
                AuthOptions::default()
            ),
            Err(TargetError::ProxyWithJumps)
        );
    }

    #[test]
    fn default_auth_tries_keys_before_interactive_methods() {
        assert_eq!(
            AuthOptions::default().methods,
            [
                AuthMethod::Agent,
                AuthMethod::PublicKey,
                AuthMethod::KeyboardInteractive,
                AuthMethod::Password
            ]
        );
    }

    #[test]
    fn first_hop_is_the_first_jump_or_the_destination() {
        let bastion = endpoint("u", "bastion", 22);
        let db = endpoint("u", "db", 22);
        let direct =
            SshTarget::new(db.clone(), vec![], Route::Direct, AuthOptions::default()).unwrap();
        assert_eq!(direct.first_hop(), &db);
        let jumped = SshTarget::new(
            db,
            vec![bastion.clone()],
            Route::Direct,
            AuthOptions::default(),
        )
        .unwrap();
        assert_eq!(jumped.first_hop(), &bastion);
    }
}
