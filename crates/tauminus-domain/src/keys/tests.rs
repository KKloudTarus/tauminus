// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

const LINE: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGxvY2Fs alice@laptop";

#[test]
fn public_key_lines_are_checked() {
    assert!(PublicKeyLine::parse(LINE).is_ok());
    assert!(PublicKeyLine::parse("ssh-ed25519 AAAA").is_ok());
    assert!(PublicKeyLine::parse("ecdsa-sha2-nistp256 AAAA+/= x").is_ok());
    for bad in [
        "",
        "ssh-ed25519",
        "rm -rf / x",
        "ssh-ed25519 AAAA'; rm -rf ~ '",
        "ssh-ed25519 AAAA it's mine",
        "ssh-ed25519 AAAA tab\there",
        "ssh-ed25519 AAAA$(reboot)x y",
    ] {
        assert!(PublicKeyLine::parse(bad).is_err(), "{bad:?}");
    }
    assert!(PublicKeyLine::parse(&format!("ssh-ed25519 AAAA {}", "x".repeat(300))).is_err());
}

#[cfg(unix)]
fn run(home: &std::path::Path, script: &str) -> bool {
    use std::io::Write;
    let mut child = std::process::Command::new("sh")
        .arg("-s")
        .env("HOME", home)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    child.wait().unwrap().success()
}

#[cfg(unix)]
#[test]
fn the_install_script_adds_a_key_once_and_keeps_the_rest() {
    use std::os::unix::fs::PermissionsExt;
    let home = std::env::temp_dir().join(format!("tauminus-keys-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let key = PublicKeyLine::parse(LINE).unwrap();
    let script = install_script(&key);
    let file = home.join(".ssh/authorized_keys");

    // No ~/.ssh yet: both are made private.
    assert!(run(&home, &script));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), format!("{LINE}\n"));
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&home.join(".ssh")), 0o700);
    assert_eq!(mode(&file), 0o600);

    // Already there: nothing changes.
    assert!(run(&home, &script));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), format!("{LINE}\n"));

    // A last line without a newline stays its own line.
    std::fs::write(&file, "ssh-rsa AAAAB other").unwrap();
    assert!(run(&home, &script));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!("ssh-rsa AAAAB other\n{LINE}\n")
    );
    // The same key with another comment, with options, or with CRLF is
    // already there.
    let data = key.data();
    for existing in [
        format!("ssh-ed25519 {data} old-comment\n"),
        format!("from=\"10.0.0.1\" ssh-ed25519 {data}\n"),
        format!("ssh-ed25519 {data}\r\n"),
    ] {
        std::fs::write(&file, &existing).unwrap();
        assert!(run(&home, &script));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), existing);
    }
    // A key whose data merely starts the same is another key.
    std::fs::write(&file, format!("ssh-ed25519 {data}xyz\n")).unwrap();
    assert!(run(&home, &script));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!("ssh-ed25519 {data}xyz\n{LINE}\n")
    );
    // Loose permissions from before lose group and other write.
    std::fs::set_permissions(home.join(".ssh"), std::fs::Permissions::from_mode(0o775)).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o664)).unwrap();
    assert!(run(&home, &script));
    assert_eq!(mode(&home.join(".ssh")), 0o755);
    assert_eq!(mode(&file), 0o644);
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn a_stored_key_with_any_comment_can_be_installed() {
    let accented = PublicKeyLine::installable("ssh-ed25519 AAAA nghĩa@laptop").unwrap();
    assert_eq!(accented.as_str(), "ssh-ed25519 AAAA ngh_a@laptop");
    let quoted = PublicKeyLine::installable("ssh-ed25519 AAAA o'brien@x").unwrap();
    assert_eq!(quoted.as_str(), "ssh-ed25519 AAAA o_brien@x");
    let bare = PublicKeyLine::installable("ssh-ed25519 AAAA").unwrap();
    assert_eq!(bare.as_str(), "ssh-ed25519 AAAA");
    assert_eq!(bare.data(), "AAAA");
    // The key itself is still checked.
    assert!(PublicKeyLine::installable("ssh-ed25519 AA'AA x").is_err());
}
