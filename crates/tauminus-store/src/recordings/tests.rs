// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use curve25519_dalek::montgomery::MontgomeryPoint;

use super::*;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("tauminus-recordings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn keys() -> ([u8; 32], [u8; 32]) {
    let secret = [7u8; 32];
    (secret, MontgomeryPoint::mul_base_clamped(secret).to_bytes())
}

fn meta(store: &FileRecordingStore, started_ms: i64) -> RecordingMeta {
    RecordingMeta {
        id: store.new_id(),
        title: "db".into(),
        host: Some("db".into()),
        started_ms,
        duration_ms: 0,
        bytes: 0,
        truncated: false,
        complete: false,
    }
}

fn text(store: &FileRecordingStore, id: &str, secret: &[u8; 32]) -> (String, bool) {
    let mut out = Vec::new();
    let complete = store
        .read(id, secret, &mut |bytes| {
            out.extend_from_slice(bytes);
            true
        })
        .unwrap();
    (String::from_utf8(out).unwrap(), complete)
}

#[test]
fn a_recording_is_listed_while_written_and_reads_back_sealed() {
    let dir = Dir::new("roundtrip");
    let store = FileRecordingStore::new(dir.0.clone());
    let (secret, public) = keys();
    let older = meta(&store, 1_000);
    store
        .create(&older, &public)
        .unwrap()
        .finish(&older)
        .unwrap();
    let begun = meta(&store, 2_000);
    let mut sink = store.create(&begun, &public).unwrap();
    sink.write(b"{\"version\":2}\n[0.1, \"o\", \"secret output\"]\n")
        .unwrap();
    sink.flush().unwrap();
    // Listed while it records, newest first, as not complete.
    let listed = store.list().unwrap();
    assert_eq!(
        listed.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
        [begun.id.clone(), older.id.clone()]
    );
    assert!(!listed[0].complete);
    let (so_far, complete) = text(&store, &begun.id, &secret);
    assert!(so_far.contains("secret output") && !complete);
    let mut done = begun.clone();
    done.complete = true;
    done.duration_ms = 1234;
    done.bytes = 13;
    sink.finish(&done).unwrap();
    assert_eq!(store.list().unwrap()[0], done);
    let (all, complete) = text(&store, &begun.id, &secret);
    assert!(complete);
    assert!(all.ends_with("\"secret output\"]\n"));
    // Nothing on disk shows the output.
    for entry in std::fs::read_dir(&dir.0).unwrap().flatten() {
        let bytes = std::fs::read(entry.path()).unwrap();
        assert!(
            !bytes.windows(13).any(|w| w == b"secret output"),
            "{:?}",
            entry.path()
        );
    }
}

#[test]
fn checkpoints_and_bookmarks_seal_too_and_go_with_the_recording() {
    let dir = Dir::new("extras");
    let store = FileRecordingStore::new(dir.0.clone());
    let (secret, public) = keys();
    let m = meta(&store, 1);
    store.create(&m, &public).unwrap().finish(&m).unwrap();
    assert_eq!(store.checkpoints(&m.id, &secret).unwrap(), None);
    store.save_checkpoints(&m.id, &public, b"first").unwrap();
    store.save_checkpoints(&m.id, &public, b"second").unwrap();
    assert_eq!(
        store.checkpoints(&m.id, &secret).unwrap(),
        Some(b"second".to_vec())
    );
    let marks = vec![Bookmark {
        at_ms: 5000,
        label: "deploy".into(),
        comment: "the restart that failed".into(),
    }];
    store.save_bookmarks(&m.id, &public, &marks).unwrap();
    assert_eq!(store.bookmarks(&m.id, &secret).unwrap(), marks);
    store.save_bookmarks(&m.id, &public, &[]).unwrap();
    assert_eq!(store.bookmarks(&m.id, &secret).unwrap(), []);
    assert!(store.delete(&m.id).unwrap());
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
    assert!(!store.delete(&m.id).unwrap());
}

#[test]
fn an_id_from_outside_never_names_a_file_outside_the_folder() {
    let dir = Dir::new("ids");
    let store = FileRecordingStore::new(dir.0.clone());
    let (secret, _) = keys();
    for bad in [
        "../hosts",
        "..",
        "",
        "ABCDEF00ABCDEF00ABCDEF00ABCDEF00",
        "a/b",
    ] {
        assert!(store.read(bad, &secret, &mut |_| true).is_err(), "{bad}");
        assert!(store.delete(bad).is_err(), "{bad}");
    }
    assert!(valid_id(&store.new_id()));
}

#[cfg(unix)]
#[test]
fn the_folder_and_files_are_this_users_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Dir::new("modes");
    let store = FileRecordingStore::new(dir.0.clone());
    let (_, public) = keys();
    let m = meta(&store, 1);
    store.create(&m, &public).unwrap().finish(&m).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir.0), 0o700);
    for entry in std::fs::read_dir(&dir.0).unwrap().flatten() {
        assert_eq!(mode(&entry.path()), 0o600, "{:?}", entry.path());
    }
}
