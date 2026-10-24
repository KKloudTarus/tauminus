// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

const EXPORT: &str = r#"Windows Registry Editor Version 5.00

[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions]

[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions\Default%20Settings]
"HostName"=""

[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions\prod%20db]
"HostName"="10.0.0.5"
"PortNumber"=dword:00000016
"UserName"="deploy"
"Protocol"="ssh"
"PublicKeyFile"="C:\\keys\\prod.pem"
"ProxyMethod"=dword:00000006
"ProxyHost"="bastion.example.com"
"ProxyPort"=dword:000008ae
"ProxyUsername"="ops"

[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions\router]
"HostName"="192.168.1.1"
"Protocol"="telnet"

[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions\socks]
"HostName"="far.example.com"
"ProxyMethod"=dword:00000002

[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions\ppk%20%c3%a9]
"HostName"="me@legacy.example.com"
"PublicKeyFile"="C:\\keys\\legacy.ppk"
"Comment"=hex(2):63,00,3a,00,5c,00,00,00

[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Jumplist]
"HostName"="not.a.session"
"#;

#[test]
fn a_regedit_export_reads_every_session_but_the_template() {
    let hosts = registry_export(EXPORT);
    let names: Vec<&str> = hosts.iter().map(|h| h.alias.as_str()).collect();
    assert_eq!(names, ["prod db", "router", "socks", "ppk é"]);
    let prod = &hosts[0];
    assert_eq!(prod.hostname, "10.0.0.5");
    assert_eq!(prod.port, Some(22));
    assert_eq!(prod.user.as_deref(), Some("deploy"));
    assert_eq!(prod.identity_files, [PathBuf::from(r"C:\keys\prod.pem")]);
    assert_eq!(
        prod.proxy_jump.as_deref(),
        Some("ops@bastion.example.com:2222")
    );
    assert_eq!(prod.problem, None);
    assert_eq!(
        hosts[1].problem.as_deref(),
        Some("a telnet session, not SSH")
    );
    assert_eq!(
        hosts[2].problem.as_deref(),
        Some("goes through a SOCKS 5 proxy, which is not imported")
    );
    // `user@host`, and a PuTTY key left to Pageant.
    let legacy = &hosts[3];
    assert_eq!(legacy.hostname, "legacy.example.com");
    assert_eq!(legacy.user.as_deref(), Some("me"));
    assert_eq!(legacy.identity_files, Vec::<PathBuf>::new());
}

#[test]
fn a_utf16_export_and_regedit4_read_the_same() {
    let wide: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain(
            EXPORT
                .replace('\n', "\r\n")
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        )
        .collect();
    assert_eq!(registry_export(&decode(&wide)), registry_export(EXPORT));
    let old = EXPORT.replace("Windows Registry Editor Version 5.00", "REGEDIT4");
    assert_eq!(registry_export(&old), registry_export(EXPORT));
}

#[test]
fn kitty_keys_and_the_older_proxy_type_are_read() {
    let text = r#"REGEDIT4

[HKEY_CURRENT_USER\Software\9bis.com\KiTTY\Sessions\k1]
"HostName"="k1.internal"
"ProxyType"=dword:00000002
"ProxySOCKSVersion"=dword:00000004
"#;
    let hosts = registry_export(text);
    assert_eq!(hosts.len(), 1);
    assert_eq!(
        hosts[0].problem.as_deref(),
        Some("goes through a SOCKS 4 proxy, which is not imported")
    );
}

#[test]
fn unix_session_files_read_as_the_registry_does() {
    let hosts = session_files([
        (
            "web%2Fprod",
            "HostName=web.internal\nPortNumber=2200\nUserName=deploy\nProtocol=ssh\n",
        ),
        ("Default%20Settings", "HostName=\n"),
        ("serial", "Protocol=serial\nSerialLine=/dev/ttyUSB0\n"),
    ]);
    assert_eq!(hosts.len(), 2);
    assert_eq!(hosts[0].alias, "web/prod");
    assert_eq!(hosts[0].port, Some(2200));
    assert_eq!(
        hosts[1].problem.as_deref(),
        Some("a serial session, not SSH")
    );
}

#[test]
fn names_unescape_in_either_case_and_keep_a_stray_percent() {
    assert_eq!(unescape("a%20b%2fc"), "a b/c");
    assert_eq!(unescape("100%"), "100%");
    assert_eq!(unescape("%zz"), "%zz");
}

#[test]
fn session_dirs_follow_putty_s_order() {
    let home = std::path::Path::new("/home/me");
    let dirs = session_dirs(home, |name| {
        (name == "PUTTYDIR").then(|| "/opt/putty".to_owned())
    });
    assert_eq!(dirs[0], PathBuf::from("/opt/putty/sessions"));
    assert_eq!(
        dirs.last().unwrap(),
        &PathBuf::from("/home/me/.putty/sessions")
    );
}

fn one(values: &[(&str, Value)]) -> ConfigHost {
    let values: HashMap<String, Value> = values
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect();
    session("s", &values).unwrap()
}

fn t(text: &str) -> Value {
    Value::Text(text.to_owned())
}

#[test]
fn host_names_read_as_putty_prepares_them() {
    // `user@` wins over UserName; one `:suffix` goes; spaces go.
    let host = one(&[
        ("HostName", t(" alice@web .internal:2200")),
        ("UserName", t("bob")),
    ]);
    assert_eq!(host.user.as_deref(), Some("alice"));
    assert_eq!(host.hostname, "web.internal");
    let v6 = one(&[("HostName", t("2001:db8::1"))]);
    assert_eq!(
        v6.hostname, "2001:db8::1",
        "more than one colon is an address"
    );
    // The jump host the same way, its user not doubled.
    let jump = one(&[
        ("HostName", t("inner")),
        ("ProxyMethod", Value::Number(6)),
        ("ProxyHost", t("ops@bastion")),
        ("ProxyUsername", t("ops")),
    ]);
    assert_eq!(jump.proxy_jump.as_deref(), Some("ops@bastion"));
}

#[test]
fn a_jump_with_no_host_and_a_port_out_of_range_are_reported() {
    let empty = one(&[("HostName", t("inner")), ("ProxyMethod", Value::Number(6))]);
    assert_eq!(
        empty.problem.as_deref(),
        Some("jumps through a host with no name")
    );
    let none = one(&[
        ("HostName", t("inner")),
        ("ProxyMethod", Value::Number(6)),
        ("ProxyHost", t("none")),
    ]);
    assert_eq!(
        none.problem.as_deref(),
        Some("jumps through a host with no name")
    );
    let port = one(&[("HostName", t("h")), ("PortNumber", Value::Number(99_999))]);
    assert_eq!(
        port.problem.as_deref(),
        Some("the port 99999 is out of range")
    );
}

#[test]
fn a_key_on_another_machine_is_reported() {
    for path in [
        r"\\attacker\k\id",
        "//attacker/k/id",
        r"\\?\UNC\a\b",
        r"\\.\pipe\x",
        r"\??\UNC\attacker\k",
    ] {
        let host = one(&[("HostName", t("h")), ("PublicKeyFile", t(path))]);
        assert_eq!(
            host.problem.as_deref(),
            Some("names a key on another machine, which is not imported"),
            "{path}"
        );
    }
    let local = one(&[("HostName", t("h")), ("PublicKeyFile", t(r"C:\keys\id"))]);
    assert_eq!(local.problem, None);
}

#[test]
fn a_plus_sign_is_not_a_hex_digit() {
    assert_eq!(unescape("%+1x"), "%+1x");
}

#[test]
fn only_hex_values_run_on_over_lines() {
    // A string ending in `\` is not continued, whatever follows it.
    let mut text = String::from(
        "REGEDIT4\n[HKEY_CURRENT_USER\\Software\\SimonTatham\\PuTTY\\Sessions\\a]\n\"HostName\"=\"hex\\\"\n",
    );
    text.push_str(&"x\\\n".repeat(200_000));
    let started = std::time::Instant::now();
    let hosts = registry_export(&text);
    assert_eq!(hosts.len(), 1);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}
