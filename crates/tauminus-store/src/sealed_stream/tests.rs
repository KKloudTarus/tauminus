// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::PathBuf;

use super::*;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("tauminus-sealed-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn keys() -> ([u8; 32], [u8; 32]) {
    let mut secret = [0u8; 32];
    fill(&mut secret).unwrap();
    (secret, MontgomeryPoint::mul_base_clamped(secret).to_bytes())
}

fn read_all_plain(
    path: &Path,
    id: &str,
    secret: &[u8; 32],
) -> Result<(Vec<u8>, Ending), SealedError> {
    let mut out = Vec::new();
    let ending = read(path, Kind::Recording, id, secret, &mut |plain| {
        out.extend_from_slice(plain);
        true
    })?;
    Ok((out, ending))
}

#[test]
fn what_is_written_reads_back_whole_and_only_with_the_secret() {
    let dir = Dir::new("roundtrip");
    let path = dir.0.join("a");
    let (secret, public) = keys();
    let mut writer = SealedWriter::create(&path, Kind::Recording, "a", &public).unwrap();
    let big = vec![b'x'; FRAME_PLAIN * 2 + 10];
    writer.write(b"header\n").unwrap();
    writer.write(&big).unwrap();
    writer.finish(b"end\n").unwrap();
    let (plain, ending) = read_all_plain(&path, "a", &secret).unwrap();
    assert_eq!(ending, Ending::Complete);
    assert_eq!(plain.len(), 7 + big.len() + 4);
    assert!(plain.ends_with(b"xend\n"));
    // Nothing in the file is the plain text.
    let raw = std::fs::read(&path).unwrap();
    assert!(!raw.windows(6).any(|w| w == b"header"));
    let (other, _) = keys();
    assert!(matches!(
        read_all_plain(&path, "a", &other),
        Err(SealedError::Tampered(0))
    ));
    // Nor does a frame open under another file's id.
    assert!(matches!(
        read_all_plain(&path, "b", &secret),
        Err(SealedError::Tampered(0))
    ));
}

#[test]
fn a_file_cut_short_reads_up_to_the_cut() {
    let dir = Dir::new("cut");
    let path = dir.0.join("a");
    let (secret, public) = keys();
    let mut writer = SealedWriter::create(&path, Kind::Recording, "a", &public).unwrap();
    writer.write(b"one\n").unwrap();
    writer.write(b"two\n").unwrap();
    writer.flush().unwrap();
    drop(writer);
    let (plain, ending) = read_all_plain(&path, "a", &secret).unwrap();
    assert_eq!(
        (plain.as_slice(), ending),
        (&b"one\ntwo\n"[..], Ending::CutShort)
    );
    // Torn mid-frame, as by a crash: the whole frames before it stay.
    let raw = std::fs::read(&path).unwrap();
    std::fs::write(&path, &raw[..raw.len() - 3]).unwrap();
    let (plain, ending) = read_all_plain(&path, "a", &secret).unwrap();
    assert_eq!(
        (plain.as_slice(), ending),
        (&b"one\n"[..], Ending::CutShort)
    );
}

#[test]
fn frames_moved_or_added_after_the_end_fail() {
    let dir = Dir::new("moved");
    let path = dir.0.join("a");
    let (secret, public) = keys();
    let mut writer = SealedWriter::create(&path, Kind::Recording, "a", &public).unwrap();
    writer.write(b"one\n").unwrap();
    writer.write(b"two\n").unwrap();
    writer.finish(b"").unwrap();
    let raw = std::fs::read(&path).unwrap();
    // Each frame here is 4 + 24 + 4 + 16 bytes.
    let frame = 4 + NONCE + 4 + TAG;
    let start = 8 + 32;
    let mut swapped = raw[..start].to_vec();
    swapped.extend_from_slice(&raw[start + frame..start + 2 * frame]);
    swapped.extend_from_slice(&raw[start..start + frame]);
    swapped.extend_from_slice(&raw[start + 2 * frame..]);
    std::fs::write(&path, &swapped).unwrap();
    assert!(matches!(
        read_all_plain(&path, "a", &secret),
        Err(SealedError::Tampered(0))
    ));
    let mut extended = raw.clone();
    extended.extend_from_slice(&raw[start..start + frame]);
    std::fs::write(&path, &extended).unwrap();
    assert!(matches!(
        read_all_plain(&path, "a", &secret),
        Err(SealedError::Tampered(3))
    ));
    std::fs::write(&path, b"not sealed at all").unwrap();
    assert!(matches!(
        read_all_plain(&path, "a", &secret),
        Err(SealedError::NotSealed)
    ));
}

#[cfg(unix)]
#[test]
fn only_this_user_can_read_the_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Dir::new("mode");
    let path = dir.0.join("a");
    let (_, public) = keys();
    SealedWriter::create(&path, Kind::Notes, "a", &public)
        .unwrap()
        .finish(b"")
        .unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    // An existing file is never replaced.
    assert!(SealedWriter::create(&path, Kind::Notes, "a", &public).is_err());
}

#[test]
fn a_file_made_with_a_low_order_key_is_refused() {
    let dir = Dir::new("loworder");
    let path = dir.0.join("a");
    let (secret, public) = keys();
    // Anyone can make this one: its shared secret is all zeros.
    let mut forged = MAGIC.to_vec();
    forged.extend_from_slice(&[0; 32]);
    std::fs::write(&path, &forged).unwrap();
    assert!(matches!(
        read_all_plain(&path, "a", &secret),
        Err(SealedError::NotSealed)
    ));
    // Nor is a file sealed to such a key.
    assert!(SealedWriter::create(&dir.0.join("b"), Kind::Recording, "b", &[0; 32]).is_err());
    let _ = public;
}
