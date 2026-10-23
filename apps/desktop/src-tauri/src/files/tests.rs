// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

#[test]
fn paths_from_the_view_are_bounded() {
    assert!(checked("/srv").is_ok());
    assert!(checked("").is_err());
    assert!(checked("a\0b").is_err());
    assert!(checked(&"a".repeat(MAX_PATH + 1)).is_err());
}

#[test]
fn a_local_listing_shows_links_as_links_and_names_its_parent() {
    let dir = std::env::temp_dir().join(format!("tauminus-local-list-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("file"), b"abc").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("file", dir.join("link")).unwrap();
    let listing = read_local(&dir).unwrap();
    let mut kinds: Vec<_> = listing
        .entries
        .iter()
        .map(|e| (e.name.clone(), e.kind, e.size))
        .collect();
    kinds.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(kinds[0], ("file".into(), EntryKindSpec::File, 3.0));
    #[cfg(unix)]
    assert_eq!(kinds[1].1, EntryKindSpec::Link);
    assert!(
        kinds
            .iter()
            .any(|k| k.0 == "sub" && k.1 == EntryKindSpec::Dir)
    );
    assert!(listing.parent.is_some());
    assert!(read_local(&dir.join("missing")).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_errors_map_to_stable_codes() {
    let code = |error: FilesError| CommandError::from(error).code;
    assert!(matches!(code(FilesError::NoSession), ErrorCode::NotFound));
    assert!(matches!(
        code(FilesError::TooManySessions),
        ErrorCode::LimitReached
    ));
    assert!(matches!(
        code(FilesError::Sftp(SftpError::Cancelled)),
        ErrorCode::Cancelled
    ));
    assert!(matches!(
        code(FilesError::Connect(
            tauminus_app::ports::ConnectError::AuthFailed {
                endpoint: "a@b".into(),
                tried: vec![],
            }
        )),
        ErrorCode::AuthFailed
    ));
}
