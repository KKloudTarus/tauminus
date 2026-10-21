// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The `known_hosts` parser against files written by real tools and by hand.
//!
//! - `github.txt`: the keys GitHub publishes at api.github.com/meta, in the
//!   form its documentation tells users to add.
//! - `hashed.txt`: what `ssh-keygen -H` wrote for a plain file of six hosts,
//!   which splits `name,address` into one hashed line each.
//! - `mixed.txt`: hand-edited shapes: comments, a tab separator, a CRLF line,
//!   IPv6 with and without a port, a trailing key comment, `@cert-authority`,
//!   `@revoked`, a hashed line next to plain ones, a truncated DSA key, and
//!   lines that are not entries at all.
//!
//! Host matching is also checked against OpenSSH's own `ssh-keygen -F`.

use std::path::{Path, PathBuf};

use russh::keys::PublicKey;
use tauminus_ssh::known_hosts::{KnownHosts, Location, Verdict};

const GITHUB_ED25519: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";
const GITHUB_ECDSA: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBEmKSENjQEezOmxkZMy7opKgwFB9nkt5YRrYMjNuG5N87uRgg6CLrbo5wAdT/y6v0mKV0U2w0WZ2YB/++Tpockg=";
// Throwaway keys made with ssh-keygen for the tests.
const ED1: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFXDxFWctSi4sWGAKtsbNMmPPk7f4g0PC2U9KxVt/PCl";
const ED2: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOqN/RGiIYlFZ+d7wuTD9F9viNKB6AY8w5porFrBDf5f";
const EC1: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBMDnrWYlMcR8tUU3pU+QhCJgREyuT18iV60JrMP0q4biF+VG0WTBuiy1EDVc0yL5cNbDkb+pQ+yYtUhyL+ZjZnc=";
const CA1: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGjaUcbLwUibUWMXfJWOQ07axg3pBqlGNlURv+Dqbu8M";

fn key(text: &str) -> PublicKey {
    PublicKey::from_openssh(text).expect("test key parses")
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/known_hosts")
        .join(name)
}

/// Parses a fixture; returns it with the skipped lines as (line, reason).
fn load(name: &str) -> (KnownHosts, Vec<(usize, &'static str)>) {
    let path = fixture(name);
    let contents = std::fs::read(&path).expect("fixture is readable");
    let (hosts, skipped) = KnownHosts::parse(&contents, &path);
    let skipped = skipped
        .into_iter()
        .map(|s| (s.location.line, s.reason))
        .collect();
    (hosts, skipped)
}

fn at(name: &str, line: usize) -> Location {
    Location {
        file: fixture(name),
        line,
    }
}

/// The name OpenSSH looks up: `host`, or `[host]:port` off port 22.
fn lookup_name(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_owned()
    } else {
        format!("[{host}]:{port}")
    }
}

/// Whether `ssh-keygen -F` finds a plain (unmarked) line for the host, or
/// `None` when ssh-keygen is not installed outside CI.
fn openssh_finds(file: &Path, host: &str, port: u16) -> Option<bool> {
    let Ok(child) = std::process::Command::new("ssh-keygen")
        .args(["-F", &lookup_name(host, port), "-f"])
        .arg(file)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        assert!(
            std::env::var_os("CI").is_none(),
            "ssh-keygen must be installed in CI"
        );
        return None;
    };
    let output = child.wait_with_output().expect("ssh-keygen runs");
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('@')),
    )
}

/// Checks that host matching agrees with OpenSSH for every query.
fn agrees_with_openssh(name: &str, hosts: &KnownHosts, queries: &[(&str, u16)]) {
    for &(host, port) in queries {
        let Some(openssh) = openssh_finds(&fixture(name), host, port) else {
            return;
        };
        let ours = !hosts.recorded_algorithms(host, port).is_empty();
        assert_eq!(
            ours,
            openssh,
            "{name}: {}: ours {ours}, ssh-keygen {openssh}",
            lookup_name(host, port)
        );
    }
}

#[test]
fn github_published_keys() {
    let name = "github.txt";
    let (hosts, skipped) = load(name);
    assert_eq!(skipped, []);
    assert_eq!(hosts.len(), 3);
    assert_eq!(
        hosts.verify("github.com", 22, &key(GITHUB_ED25519)),
        Verdict::Known(at(name, 1))
    );
    assert_eq!(
        hosts.verify("GitHub.com", 22, &key(GITHUB_ECDSA)),
        Verdict::Known(at(name, 2))
    );
    assert_eq!(
        hosts.verify("github.com", 22, &key(ED1)),
        Verdict::Changed {
            recorded: at(name, 1)
        }
    );
    assert_eq!(
        hosts.verify("github.com", 443, &key(GITHUB_ED25519)),
        Verdict::Unknown
    );
    agrees_with_openssh(
        name,
        &hosts,
        &[
            ("github.com", 22),
            ("github.com", 443),
            ("gist.github.com", 22),
        ],
    );
}

#[test]
fn file_hashed_by_ssh_keygen() {
    let name = "hashed.txt";
    let (hosts, skipped) = load(name);
    assert_eq!(skipped, []);
    assert_eq!(hosts.len(), 7);
    let cases: &[(&str, u16, &str, Verdict)] = &[
        (
            "github.com",
            22,
            GITHUB_ED25519,
            Verdict::Known(at(name, 1)),
        ),
        ("github.com", 22, GITHUB_ECDSA, Verdict::Known(at(name, 2))),
        ("gitlab.example.org", 22, ED1, Verdict::Known(at(name, 4))),
        ("192.0.2.10", 22, ED1, Verdict::Known(at(name, 5))),
        ("git.example.net", 7999, EC1, Verdict::Known(at(name, 6))),
        ("192.0.2.20", 2222, ED1, Verdict::Known(at(name, 7))),
        (
            "gitlab.example.org",
            22,
            EC1,
            Verdict::Changed {
                recorded: at(name, 4),
            },
        ),
        ("git.example.net", 22, EC1, Verdict::Unknown),
        ("192.0.2.20", 22, ED1, Verdict::Unknown),
    ];
    for (host, port, text, expected) in cases {
        assert_eq!(
            &hosts.verify(host, *port, &key(text)),
            expected,
            "{host}:{port}"
        );
    }
    agrees_with_openssh(
        name,
        &hosts,
        &[
            ("github.com", 22),
            ("gitlab.example.org", 22),
            ("192.0.2.10", 22),
            ("git.example.net", 7999),
            ("git.example.net", 22),
            ("192.0.2.20", 2222),
            ("example.org", 22),
        ],
    );
}

#[test]
fn hand_edited_file() {
    let name = "mixed.txt";
    let (hosts, skipped) = load(name);
    assert_eq!(skipped, [(12, "missing key type"), (13, "unknown marker")]);
    let cases: &[(&str, u16, &str, Verdict)] = &[
        // A trailing comment after the key is allowed.
        ("gitlab.example.org", 22, ED1, Verdict::Known(at(name, 2))),
        ("192.0.2.10", 22, ED1, Verdict::Known(at(name, 2))),
        // Tab separator; `?` matches exactly one character.
        ("web1.example.com", 22, EC1, Verdict::Known(at(name, 4))),
        ("web12.example.com", 22, EC1, Verdict::Unknown),
        // CRLF line; the negated host is not covered.
        ("api.corp.example.com", 22, ED2, Verdict::Known(at(name, 5))),
        ("vpn.corp.example.com", 22, ED2, Verdict::Unknown),
        // Listed by the wildcard but revoked on its own.
        (
            "old.corp.example.com",
            22,
            ED2,
            Verdict::Revoked(at(name, 9)),
        ),
        ("2001:db8::1", 2222, EC1, Verdict::Known(at(name, 6))),
        ("2001:db8::2", 22, ED1, Verdict::Known(at(name, 7))),
        // The authority's key is no host key: offered as one, it is a change.
        (
            "db.corp.example.com",
            22,
            CA1,
            Verdict::Changed {
                recorded: at(name, 5),
            },
        ),
        // A key that does not decode is skipped when it matches.
        ("legacy.example.com", 22, ED1, Verdict::Unknown),
    ];
    for (host, port, text, expected) in cases {
        assert_eq!(
            &hosts.verify(host, *port, &key(text)),
            expected,
            "{host}:{port}"
        );
    }
    assert_eq!(
        hosts.authorities_for("db.corp.example.com", 22),
        [&key(CA1)]
    );
    assert_eq!(
        hosts.authorities_for("db.example.com", 22),
        Vec::<&PublicKey>::new()
    );
    assert_eq!(
        hosts.revocation("old.corp.example.com", 22, &key(ED2)),
        Some(&at(name, 9))
    );
    agrees_with_openssh(
        name,
        &hosts,
        &[
            ("gitlab.example.org", 22),
            ("192.0.2.10", 22),
            ("web1.example.com", 22),
            ("web12.example.com", 22),
            ("api.corp.example.com", 22),
            ("vpn.corp.example.com", 22),
            ("2001:db8::1", 2222),
            ("2001:db8::1", 22),
            ("2001:db8::2", 22),
            ("unlisted.example.com", 22),
        ],
    );
}
