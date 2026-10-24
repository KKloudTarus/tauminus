// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tauminus-import-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_securecrt_tree_reads_from_its_config_or_sessions_folder() {
    let config = scratch("crt");
    let sessions = config.join("Sessions");
    std::fs::create_dir_all(sessions.join("Prod/EU")).unwrap();
    std::fs::write(
        sessions.join("Prod/EU/web.ini"),
        "\u{feff}S:\"Hostname\"=web.internal\r\nS:\"Protocol Name\"=SSH2\r\n",
    )
    .unwrap();
    std::fs::write(
        sessions.join("__FolderData__.ini"),
        "S:\"Folder List\"=Prod:\n",
    )
    .unwrap();
    std::fs::write(sessions.join("readme.txt"), "not a session").unwrap();
    for picked in [&config, &sessions] {
        let parsed = read(ImportKind::SecureCrt, picked, Path::new("/h")).unwrap();
        assert_eq!(parsed.group, "SecureCRT");
        assert_eq!(parsed.hosts_read().len(), 1, "{picked:?}");
        assert_eq!(parsed.hosts_read()[0].hostname, "web.internal");
        assert_eq!(parsed.hosts_read()[0].folder, ["Prod", "EU"]);
    }
    let _ = std::fs::remove_dir_all(&config);
}

#[cfg(unix)]
#[test]
fn links_in_a_session_folder_are_not_followed() {
    let config = scratch("links");
    let sessions = config.join("Sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let outside = scratch("outside");
    std::fs::write(outside.join("secret.ini"), "S:\"Hostname\"=outside\n").unwrap();
    std::os::unix::fs::symlink(&outside, sessions.join("away")).unwrap();
    std::os::unix::fs::symlink(outside.join("secret.ini"), sessions.join("x.ini")).unwrap();
    // A loop back to the root.
    std::os::unix::fs::symlink(&sessions, sessions.join("loop")).unwrap();
    let parsed = read(ImportKind::SecureCrt, &config, Path::new("/h")).unwrap();
    assert_eq!(parsed.hosts_read().len(), 0, "{:?}", parsed.hosts_read());
    let _ = std::fs::remove_dir_all(&config);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn only_a_regular_file_within_the_limit_is_read() {
    let dir = scratch("capped");
    assert!(read_capped(&dir, MAX_FILE).is_err(), "a folder");
    let big = dir.join("big.csv");
    std::fs::write(&big, vec![b'a'; 2048]).unwrap();
    assert!(read_capped(&big, 1024).is_err());
    assert_eq!(read_capped(&big, 4096).unwrap().len(), 2048);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn each_file_kind_reads_into_its_own_group() {
    let dir = scratch("kinds");
    let csv = dir.join("hosts.csv");
    std::fs::write(&csv, "name,host\nweb,web.internal\n").unwrap();
    let reg = dir.join("putty.reg");
    let export = "REGEDIT4\r\n\r\n[HKEY_CURRENT_USER\\Software\\SimonTatham\\PuTTY\\Sessions\\db]\r\n\"HostName\"=\"db.internal\"\r\n";
    let wide: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain(export.encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    std::fs::write(&reg, wide).unwrap();
    let moba = dir.join("MobaXterm.ini");
    std::fs::write(
        &moba,
        b"[Bookmarks]\r\nSubRep=\r\nna\xefve= #109#0%c.internal%22%u\r\n",
    )
    .unwrap();
    for (kind, path, group, host) in [
        (ImportKind::Csv, &csv, "CSV import", "web.internal"),
        (ImportKind::PuttyExport, &reg, "PuTTY", "db.internal"),
        (ImportKind::MobaXterm, &moba, "MobaXterm", "c.internal"),
    ] {
        let parsed = read(kind, path, Path::new("/h")).unwrap();
        assert_eq!(parsed.group, group);
        assert_eq!(parsed.hosts_read()[0].hostname, host, "{kind:?}");
    }
    // An ANSI name, as MobaXterm writes it.
    let parsed = read(ImportKind::MobaXterm, &moba, Path::new("/h")).unwrap();
    assert_eq!(parsed.hosts_read()[0].alias, "naïve");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(not(windows))]
#[test]
fn putty_s_unix_sessions_read_from_its_folder() {
    let home = scratch("putty-home");
    let sessions = home.join(".putty/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("web%20one"),
        "HostName=web.internal\nPortNumber=2200\n",
    )
    .unwrap();
    // Only when no PUTTY* variable points elsewhere, as in CI.
    if std::env::var_os("PUTTYDIR").is_none()
        && std::env::var_os("PUTTYSESSIONS").is_none()
        && std::env::var_os("XDG_CONFIG_HOME").is_none()
    {
        let parsed = putty_here(&home).unwrap();
        assert_eq!(parsed.hosts_read().len(), 1);
        assert_eq!(parsed.hosts_read()[0].alias, "web one");
        assert_eq!(parsed.hosts_read()[0].port, Some(2200));
    }
    let empty = scratch("putty-none");
    if std::env::var_os("PUTTYDIR").is_none() && std::env::var_os("PUTTYSESSIONS").is_none() {
        assert!(putty_here(&empty).is_err());
    }
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&empty);
}

#[test]
fn a_long_skipped_list_is_cut_and_its_text_cleaned() {
    let report = tauminus_app::ImportReport {
        added: vec![],
        skipped: (0..60)
            .map(|i| (format!("h{i}\u{202e}"), "x".repeat(200)))
            .collect(),
    };
    let result = import_result(report, &tauminus_domain::Inventory::default());
    assert_eq!(result.skipped.len(), 50);
    assert_eq!(result.skipped_count, 60);
    assert_eq!(result.skipped[0].alias, "h0", "the bidi override is gone");
    assert_eq!(
        result.skipped[0].reason.chars().count(),
        129,
        "128 and an ellipsis"
    );
}

#[cfg(unix)]
#[test]
fn a_pipe_picked_as_the_file_is_refused_without_waiting() {
    let dir = scratch("fifo");
    let fifo = dir.join("hosts.csv");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRWXU).unwrap();
    assert!(read_capped(&fifo, MAX_FILE).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_sessions_link_is_not_followed() {
    let config = scratch("crt-link");
    let elsewhere = scratch("crt-elsewhere");
    std::fs::write(elsewhere.join("far.ini"), "S:\"Hostname\"=far\n").unwrap();
    std::os::unix::fs::symlink(&elsewhere, config.join("Sessions")).unwrap();
    // `Config` with a linked `Sessions`: the picked folder itself is read,
    // and the link in it is not followed.
    let parsed = read(ImportKind::SecureCrt, &config, Path::new("/h")).unwrap();
    assert_eq!(parsed.hosts_read().len(), 0);
    let _ = std::fs::remove_dir_all(&config);
    let _ = std::fs::remove_dir_all(&elsewhere);
}
