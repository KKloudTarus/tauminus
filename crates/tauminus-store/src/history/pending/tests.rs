// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

fn log(name: &str) -> PendingLog {
    let mut suffix = [0u8; 8];
    fill(&mut suffix).unwrap();
    let dir = std::env::temp_dir().join(format!("tauminus-pending-{name}-{suffix:02x?}"));
    PendingLog::new(dir.join("history.pending"))
}

fn entry(command: &str) -> HistoryEntry {
    HistoryEntry {
        host: Some("web".into()),
        label: "web".into(),
        at_ms: 42,
        command: command.into(),
        output: "secret output".into(),
        exit: Some(0),
    }
}

#[test]
fn sealed_entries_open_only_with_the_private_key() {
    let log = log("round");
    let keys = HistoryKeyPair::random().unwrap();
    assert!(log.append(&keys.public(), &entry("first")).unwrap());
    assert!(log.append(&keys.public(), &entry("second")).unwrap());
    let bytes = std::fs::read(log.path()).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("first") && !text.contains("secret output"));
    let read = log.read(&keys).unwrap();
    assert_eq!(read.unreadable, 0);
    assert_eq!(read.entries, [entry("first"), entry("second")]);
    assert_eq!(read.consumed, std::fs::metadata(log.path()).unwrap().len());
    // Another key opens nothing.
    let other = log.read(&HistoryKeyPair::random().unwrap()).unwrap();
    assert_eq!(other.entries.len(), 0);
    assert_eq!(other.unreadable, 2);
    log.clear(read.consumed).unwrap();
    assert_eq!(log.read(&keys).unwrap().entries.len(), 0);
    let _ = std::fs::remove_dir_all(log.path().parent().unwrap());
}

#[test]
fn a_frame_cut_short_or_changed_is_skipped() {
    let log = log("damage");
    let keys = HistoryKeyPair::random().unwrap();
    for n in 0..3 {
        log.append(&keys.public(), &entry(&format!("cmd{n}")))
            .unwrap();
    }
    let mut bytes = std::fs::read(log.path()).unwrap();
    // Flip a byte in the second frame's ciphertext.
    let first = 4 + u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    bytes[first + 4 + HEADER + 3] ^= 1;
    // And cut the last frame short, as a crash mid-append would.
    bytes.truncate(bytes.len() - 10);
    std::fs::write(log.path(), &bytes).unwrap();
    let read = log.read(&keys).unwrap();
    assert_eq!(read.entries, [entry("cmd0")]);
    assert_eq!(read.unreadable, 1);
    let _ = std::fs::remove_dir_all(log.path().parent().unwrap());
}

#[test]
fn the_public_key_follows_from_the_secret() {
    let keys = HistoryKeyPair::random().unwrap();
    let again = HistoryKeyPair::from_secret(keys.secret().clone());
    assert_eq!(keys.public(), again.public());
    assert!(!format!("{keys:?}").contains(&format!("{:?}", keys.secret().as_bytes())));
}

#[test]
fn a_low_order_public_key_is_refused() {
    let log = log("low");
    assert!(log.append(&[0u8; 32], &entry("x")).is_err());
}

#[test]
fn a_missing_file_is_empty() {
    let log = log("missing");
    let keys = HistoryKeyPair::random().unwrap();
    assert_eq!(log.read(&keys).unwrap().entries.len(), 0);
    log.clear(0).unwrap();
}

#[test]
fn appends_after_a_crash_mid_append_are_kept() {
    let log = log("torn");
    let keys = HistoryKeyPair::random().unwrap();
    for n in 0..3 {
        log.append(&keys.public(), &entry(&format!("a{n}")))
            .unwrap();
    }
    let len = std::fs::metadata(log.path()).unwrap().len();
    std::fs::OpenOptions::new()
        .write(true)
        .open(log.path())
        .unwrap()
        .set_len(len - 10)
        .unwrap();
    for n in 0..3 {
        log.append(&keys.public(), &entry(&format!("b{n}")))
            .unwrap();
    }
    let read = log.read(&keys).unwrap();
    let commands: Vec<_> = read.entries.iter().map(|e| e.command.as_str()).collect();
    // The torn frame is gone; nothing after it is.
    assert_eq!(commands, ["a0", "a1", "b0", "b1", "b2"]);
    assert_eq!(read.unreadable, 0);
    let _ = std::fs::remove_dir_all(log.path().parent().unwrap());
}

#[test]
fn clearing_keeps_what_was_appended_after_the_read() {
    let log = log("late");
    let keys = HistoryKeyPair::random().unwrap();
    log.append(&keys.public(), &entry("read")).unwrap();
    let read = log.read(&keys).unwrap();
    // Another window appends between the read and the clear.
    log.append(&keys.public(), &entry("late")).unwrap();
    log.clear(read.consumed).unwrap();
    let left = log.read(&keys).unwrap();
    assert_eq!(left.entries, [entry("late")]);
    let _ = std::fs::remove_dir_all(log.path().parent().unwrap());
}
