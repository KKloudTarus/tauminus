// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::Path;

use super::*;

// Public halves of throwaway keys made with ssh-keygen for these tests.
const ED1: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFXDxFWctSi4sWGAKtsbNMmPPk7f4g0PC2U9KxVt/PCl";
const ED2: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOqN/RGiIYlFZ+d7wuTD9F9viNKB6AY8w5porFrBDf5f";
const EC1: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBMDnrWYlMcR8tUU3pU+QhCJgREyuT18iV60JrMP0q4biF+VG0WTBuiy1EDVc0yL5cNbDkb+pQ+yYtUhyL+ZjZnc=";
const CA1: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGjaUcbLwUibUWMXfJWOQ07axg3pBqlGNlURv+Dqbu8M";
/// Written by `ssh-keygen -H` for db.example.com with key ED1.
const OPENSSH_HASHED: &str = "|1|2MS4uVciN5fOUYXh5vQ7PDltxIM=|QktluJLiCOmvDoDIV8MIq7CbaY8=";

fn key(text: &str) -> PublicKey {
    PublicKey::from_openssh(text).expect("test key parses")
}

fn known(text: &str) -> KnownHosts {
    let (known, skipped) = KnownHosts::parse(text.as_bytes(), Path::new("known_hosts"));
    assert_eq!(skipped, Vec::new(), "every test line parses");
    known
}

fn at(line: usize) -> Location {
    Location {
        file: "known_hosts".into(),
        line,
    }
}

#[test]
fn recorded_key_is_known() {
    let hosts = known(&format!("db.example.com {ED1}\n"));
    assert_eq!(
        hosts.verify("db.example.com", 22, &key(ED1)),
        Verdict::Known(at(1))
    );
}

#[test]
fn host_names_match_case_insensitively() {
    let hosts = known(&format!("DB.Example.COM {ED1}\n"));
    assert_eq!(
        hosts.verify("db.EXAMPLE.com", 22, &key(ED1)),
        Verdict::Known(at(1))
    );
}

#[test]
fn different_key_of_the_same_type_is_a_change() {
    let hosts = known(&format!("# comment\n\ndb {ED1}\n"));
    assert_eq!(
        hosts.verify("db", 22, &key(ED2)),
        Verdict::Changed { recorded: at(3) }
    );
}

#[test]
fn a_key_of_another_type_is_a_change_too() {
    // OpenSSH: HOST_CHANGED whatever the type. A MITM must not get a
    // trust prompt just by offering a type that is not on record.
    let hosts = known(&format!("db {EC1}\n"));
    assert_eq!(
        hosts.verify("db", 22, &key(ED1)),
        Verdict::Changed { recorded: at(1) }
    );
}

#[test]
fn recorded_algorithms_list_plain_keys_in_file_order() {
    let hosts = known(&format!(
        "db {EC1}\n@revoked db {ED2}\n@cert-authority db {CA1}\ndb {ED1}\ndb {ED1}\nweb {ED2}\n"
    ));
    assert_eq!(
        hosts.recorded_algorithms("db", 22),
        [key(EC1).algorithm(), key(ED1).algorithm()]
    );
    assert_eq!(hosts.recorded_algorithms("other", 22), []);
}

#[test]
fn a_match_on_any_line_wins_over_a_changed_line() {
    // Hosts often have several keys; one matching line is enough.
    let hosts = known(&format!("db {ED2}\ndb {ED1}\n"));
    assert_eq!(hosts.verify("db", 22, &key(ED1)), Verdict::Known(at(2)));
}

#[test]
fn unknown_host_is_unknown() {
    let hosts = known(&format!("db {ED1}\n"));
    assert_eq!(hosts.verify("web", 22, &key(ED1)), Verdict::Unknown);
}

#[test]
fn non_standard_ports_use_bracket_form() {
    let hosts = known(&format!("[bastion]:2222 {ED1}\n"));
    assert_eq!(
        hosts.verify("bastion", 2222, &key(ED1)),
        Verdict::Known(at(1))
    );
    assert_eq!(hosts.verify("bastion", 22, &key(ED1)), Verdict::Unknown);
}

#[test]
fn wildcards_and_negation_follow_sshd() {
    let hosts = known(&format!(
        "*.prod.example.com,!db3.prod.example.com {ED1}\nweb?.example.com {ED1}\n"
    ));
    let k = key(ED1);
    assert_eq!(
        hosts.verify("db1.prod.example.com", 22, &k),
        Verdict::Known(at(1))
    );
    assert_eq!(
        hosts.verify("db3.prod.example.com", 22, &k),
        Verdict::Unknown
    );
    assert_eq!(
        hosts.verify("web1.example.com", 22, &k),
        Verdict::Known(at(2))
    );
    assert_eq!(hosts.verify("web12.example.com", 22, &k), Verdict::Unknown);
}

#[test]
fn reads_hashed_entries_written_by_openssh() {
    let hosts = known(&format!("{OPENSSH_HASHED} {ED1}\n"));
    assert_eq!(
        hosts.verify("db.example.com", 22, &key(ED1)),
        Verdict::Known(at(1))
    );
    assert_eq!(
        hosts.verify("db2.example.com", 22, &key(ED1)),
        Verdict::Unknown
    );
}

#[test]
fn revoked_key_is_rejected_even_when_also_listed() {
    let hosts = known(&format!("db {ED1}\n@revoked * {ED1}\n"));
    assert_eq!(hosts.verify("db", 22, &key(ED1)), Verdict::Revoked(at(2)));
    assert_eq!(hosts.revocation("db", 22, &key(ED1)), Some(&at(2)));
    assert_eq!(hosts.revocation("db", 22, &key(ED2)), None);
}

#[test]
fn cert_authorities_are_listed_per_host_and_never_trusted_as_host_keys() {
    let hosts = known(&format!("@cert-authority *.example.com {CA1}\n"));
    assert_eq!(hosts.authorities_for("db.example.com", 22), vec![&key(CA1)]);
    assert_eq!(
        hosts.authorities_for("other.net", 22),
        Vec::<&PublicKey>::new()
    );
    assert_eq!(
        hosts.verify("db.example.com", 22, &key(CA1)),
        Verdict::Unknown
    );
}

#[test]
fn bad_lines_are_skipped_and_reported() {
    let mut text = format!(
        "db {ED1}\n@bogus db {ED1}\nonly-a-host\n|1|short|x= {ED1}\n{} {ED1}\n",
        "a".repeat(MAX_PATTERN)
    )
    .into_bytes();
    // A Latin-1 comment: only this line is lost.
    text.extend_from_slice(b"# caf\xe9\n");
    text.extend_from_slice(format!("web {ED2}\n").as_bytes());
    let (hosts, skipped) = KnownHosts::parse(&text, Path::new("known_hosts"));
    assert_eq!(hosts.len(), 2);
    let reasons: Vec<_> = skipped
        .iter()
        .map(|s| (s.location.line, s.reason))
        .collect();
    assert_eq!(
        reasons,
        [
            (2, "unknown marker"),
            (3, "missing key type"),
            (4, "malformed hashed host"),
            (5, "host pattern too long"),
            (6, "not UTF-8"),
        ]
    );
    assert_eq!(hosts.verify("web", 22, &key(ED2)), Verdict::Known(at(7)));
}

#[test]
fn lines_with_undecodable_keys_are_ignored_when_they_match() {
    let blob = ED1.split(' ').nth(1).unwrap();
    // Wrong type for the blob, and a blob that is not base64.
    let hosts = known(&format!("db ssh-rsa {blob}\ndb ssh-ed25519 not-base64\n"));
    assert_eq!(hosts.verify("db", 22, &key(ED1)), Verdict::Unknown);
}

#[test]
fn several_files_keep_their_own_locations() {
    let mut hosts = known(&format!("db {ED1}\n"));
    let skipped = hosts.extend(
        format!("web {ED2}\n").as_bytes(),
        Path::new("/etc/ssh/ssh_known_hosts"),
    );
    assert_eq!(skipped, Vec::new());
    assert_eq!(
        hosts.verify("web", 22, &key(ED2)),
        Verdict::Known(Location {
            file: "/etc/ssh/ssh_known_hosts".into(),
            line: 1
        })
    );
}

#[test]
fn hashed_lines_we_write_verify_and_hide_the_host() {
    let line = hashed_line_with_salt("db.example.com", 2222, &key(ED1), [7; 20]);
    assert!(line.starts_with("|1|"));
    assert!(!line.contains("db.example.com"));
    let hosts = known(&format!("{line}\n"));
    assert_eq!(
        hosts.verify("db.example.com", 2222, &key(ED1)),
        Verdict::Known(at(1))
    );
    assert_eq!(
        hosts.verify("db.example.com", 22, &key(ED1)),
        Verdict::Unknown
    );
}

#[test]
fn random_salts_differ_between_lines() {
    assert_ne!(
        hashed_line("db", 22, &key(ED1)).unwrap(),
        hashed_line("db", 22, &key(ED1)).unwrap()
    );
}

#[test]
fn wildcard_matching_edge_cases() {
    let cases: &[(&str, &str, bool)] = &[
        ("*", "", true),
        ("*", "anything", true),
        ("", "", true),
        ("", "a", false),
        ("?", "", false),
        ("a*b", "ab", true),
        ("a*b", "axxb", true),
        ("a*b", "axxbc", false),
        ("*.*.com", "a.b.com", true),
        ("a**b", "ab", true),
        ("*a*a*a*a*b", &"a".repeat(30), false),
        ("[h]:2?22", "[h]:2022", true),
    ];
    for (pattern, text, expected) in cases {
        assert_eq!(
            wildcard(pattern.as_bytes(), text.as_bytes()),
            *expected,
            "{pattern:?} vs {text:?}"
        );
    }
}

/// Compares host matching with OpenSSH's own `ssh-keygen -F` on a corpus of
/// line shapes. Skipped when ssh-keygen is not installed, except in CI.
#[test]
fn host_matching_agrees_with_ssh_keygen() {
    if std::process::Command::new("ssh-keygen")
        .arg("-?")
        .stderr(std::process::Stdio::null())
        .status()
        .is_err()
    {
        assert!(
            std::env::var_os("CI").is_none(),
            "ssh-keygen must be installed in CI"
        );
        return;
    }
    let corpus = format!(
        "*.prod.example.com,!db3.prod.example.com {ED1}\n\
         [bastion]:2222 {ED1}\n\
         web?.example.com {ED1}\n\
         10.0.0.* {ED1}\n\
         [10.0.0.9]:* {ED1}\n\
         {OPENSSH_HASHED} {ED1}\n\
         *.internal,!*.dmz.internal {ED1}\n"
    );
    let dir = std::env::temp_dir().join(format!("tauminus-kh-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("known_hosts");
    std::fs::write(&file, &corpus).unwrap();
    let hosts = known(&corpus);

    let queries: &[(&str, u16)] = &[
        ("db1.prod.example.com", 22),
        ("db3.prod.example.com", 22),
        ("bastion", 2222),
        ("bastion", 22),
        ("web1.example.com", 22),
        ("web12.example.com", 22),
        ("10.0.0.5", 22),
        ("10.0.0.9", 2200),
        ("10.0.1.5", 22),
        ("db.example.com", 22),
        ("api.internal", 22),
        ("gw.dmz.internal", 22),
    ];
    for (host, port) in queries {
        let name = host_name(host, *port);
        let child = std::process::Command::new("ssh-keygen")
            .args(["-F", &name, "-f"])
            .arg(&file)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let openssh_found = String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| !line.starts_with('#') && !line.is_empty());
        let ours_found = matches!(hosts.verify(host, *port, &key(ED1)), Verdict::Known(_));
        assert_eq!(
            ours_found, openssh_found,
            "{name}: ours {ours_found}, ssh-keygen {openssh_found}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
