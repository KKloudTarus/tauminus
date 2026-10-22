// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

/// Changes one field of a saved vault file.
type Tamper<'a> = Box<dyn Fn(&mut serde_json::Value) + 'a>;

/// Cheap costs at the floor, so tests run fast; the format is the same.
const TEST_KDF: Kdf = Kdf {
    memory_kib: MIN_MEMORY_KIB,
    iterations: MIN_ITERATIONS,
    parallelism: 1,
};

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let mut suffix = [0u8; 8];
        fill(&mut suffix).unwrap();
        let dir = std::env::temp_dir().join(format!("tauminus-vault-{name}-{}", hex(&suffix)));
        Self(dir)
    }

    fn file(&self) -> VaultFile {
        VaultFile::new(self.0.join("vault.json"))
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sample(dir: &Dir) -> VaultFile {
    let file = dir.file();
    let mut vault = file.create(b"correct horse", TEST_KDF).unwrap();
    vault.put("id1", b"secret one".to_vec()).unwrap();
    vault.put("id2", vec![0u8; MAX_RECORD]).unwrap();
    vault.save(&file).unwrap();
    file
}

#[test]
fn records_round_trip_through_the_file() {
    let dir = Dir::new("round");
    let file = sample(&dir);
    let vault = file.open_with_password(b"correct horse").unwrap();
    assert_eq!(vault.get("id1"), Some(&b"secret one"[..]));
    assert_eq!(vault.get("id2").map(<[u8]>::len), Some(MAX_RECORD));
    assert_eq!(vault.ids().collect::<Vec<_>>(), ["id1", "id2"]);
    // Nothing secret is in the file as text.
    let text = std::fs::read_to_string(file.path()).unwrap();
    assert!(!text.contains("secret one") && !text.contains("correct horse"));
}

#[test]
fn a_wrong_password_is_refused() {
    let dir = Dir::new("wrong");
    let file = sample(&dir);
    assert!(matches!(
        file.open_with_password(b"correct horse "),
        Err(VaultError::WrongKey)
    ));
    assert!(matches!(
        file.open_with_password(b""),
        Err(VaultError::WrongKey)
    ));
}

#[test]
fn a_tampered_file_is_refused() {
    let dir = Dir::new("tamper");
    let file = sample(&dir);
    let original: serde_json::Value =
        serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap();
    let flip = |text: &str| -> String {
        let mut bytes = decode(text).unwrap();
        bytes[0] ^= 1;
        encode(&bytes)
    };
    let cases: Vec<(&str, Tamper<'_>)> = vec![
        (
            "record data",
            Box::new(|v| {
                let data = v["records"][0]["data"].as_str().unwrap().to_owned();
                v["records"][0]["data"] = flip(&data).into();
            }),
        ),
        (
            "record nonce",
            Box::new(|v| {
                let nonce = v["records"][0]["nonce"].as_str().unwrap().to_owned();
                v["records"][0]["nonce"] = flip(&nonce).into();
            }),
        ),
        (
            "record id",
            Box::new(|v| v["records"][0]["id"] = "other".into()),
        ),
        (
            "swapped records",
            Box::new(|v| {
                let first = v["records"][0]["data"].clone();
                v["records"][0]["data"] = v["records"][1]["data"].clone();
                v["records"][1]["data"] = first;
            }),
        ),
        (
            "vault id",
            Box::new(|v| v["id"] = "00112233445566778899aabbccddeeff".into()),
        ),
    ];
    for (what, change) in cases {
        let mut value = original.clone();
        change(&mut value);
        std::fs::write(file.path(), serde_json::to_vec(&value).unwrap()).unwrap();
        let result = file.open_with_password(b"correct horse");
        assert!(
            matches!(result, Err(VaultError::Corrupt(_) | VaultError::WrongKey)),
            "{what}: {result:?}"
        );
    }
    // Weaker key derivation than the floor is refused before any work.
    let mut value = original.clone();
    value["kdf"]["memoryKib"] = 8.into();
    std::fs::write(file.path(), serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(matches!(
        file.open_with_password(b"correct horse"),
        Err(VaultError::Corrupt(_))
    ));
    // And costs that would take minutes are refused too.
    let mut value = original;
    value["kdf"]["iterations"] = 1000.into();
    std::fs::write(file.path(), serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(matches!(
        file.open_with_password(b"correct horse"),
        Err(VaultError::Corrupt(_))
    ));
}

#[test]
fn a_truncated_file_is_refused() {
    let dir = Dir::new("truncated");
    let file = sample(&dir);
    let bytes = std::fs::read(file.path()).unwrap();
    for len in [0, 1, bytes.len() / 2, bytes.len() - 2] {
        std::fs::write(file.path(), &bytes[..len]).unwrap();
        assert!(
            matches!(
                file.open_with_password(b"correct horse"),
                Err(VaultError::Corrupt(_))
            ),
            "{len} bytes"
        );
    }
}

#[test]
fn a_keychain_key_opens_the_vault_and_can_be_removed() {
    let dir = Dir::new("keychain");
    let file = sample(&dir);
    let key = SecretKey::random().unwrap();
    let mut vault = file.open_with_password(b"correct horse").unwrap();
    assert!(!file.has_keychain_slot().unwrap());
    vault.set_keychain_key(&key).unwrap();
    vault.save(&file).unwrap();
    assert!(file.has_keychain_slot().unwrap());
    assert_eq!(
        file.open_with_key(&key).unwrap().get("id1"),
        Some(&b"secret one"[..])
    );
    assert!(matches!(
        file.open_with_key(&SecretKey::random().unwrap()),
        Err(VaultError::WrongKey)
    ));
    // The password slot does not open with the keychain key, nor the reverse.
    let mut vault = file.open_with_key(&key).unwrap();
    vault.remove_keychain_key();
    vault.save(&file).unwrap();
    assert!(matches!(
        file.open_with_key(&key),
        Err(VaultError::WrongKey)
    ));
    assert!(file.open_with_password(b"correct horse").is_ok());
}

#[test]
fn changing_the_password_keeps_the_records() {
    let dir = Dir::new("change");
    let file = sample(&dir);
    let mut vault = file.open_with_password(b"correct horse").unwrap();
    vault.change_password(b"battery staple").unwrap();
    vault.save(&file).unwrap();
    assert!(matches!(
        file.open_with_password(b"correct horse"),
        Err(VaultError::WrongKey)
    ));
    let vault = file.open_with_password(b"battery staple").unwrap();
    assert_eq!(vault.get("id1"), Some(&b"secret one"[..]));
}

#[test]
fn changing_the_password_replaces_the_data_key() {
    let dir = Dir::new("rotate");
    let file = sample(&dir);
    // A copy taken before the change, with the old password, yields the old
    // key; nothing saved after the change opens with it.
    let old = file.open_with_password(b"correct horse").unwrap();
    let mut vault = file.open_with_password(b"correct horse").unwrap();
    vault
        .set_keychain_key(&SecretKey::random().unwrap())
        .unwrap();
    vault.change_password(b"battery staple").unwrap();
    assert!(!vault.has_keychain_key());
    vault.save(&file).unwrap();
    let new = file.open_with_password(b"battery staple").unwrap();
    assert_ne!(old.data_key.as_bytes(), new.data_key.as_bytes());
    let (saved, _) = file.read().unwrap();
    let record = &saved.records[0];
    let nonce = decode_exact::<NONCE_LEN>(&record.nonce, "nonce").unwrap();
    let sealed = decode(&record.data).unwrap();
    assert!(
        unseal(
            &old.data_key,
            &nonce,
            &sealed,
            &record_aad(&saved.id, &record.id)
        )
        .is_err()
    );
}

#[test]
fn a_save_never_replaces_another_windows_change() {
    let dir = Dir::new("conflict");
    let file = sample(&dir);
    let mut stale = file.open_with_password(b"correct horse").unwrap();
    let mut other = file.open_with_password(b"correct horse").unwrap();
    other.change_password(b"battery staple").unwrap();
    other.save(&file).unwrap();
    stale.put("id3", b"late".to_vec()).unwrap();
    assert!(matches!(stale.save(&file), Err(VaultError::Conflict)));
    // The old password stays revoked.
    assert!(matches!(
        file.open_with_password(b"correct horse"),
        Err(VaultError::WrongKey)
    ));
    // The window that saved keeps saving.
    other.put("id3", b"fine".to_vec()).unwrap();
    other.save(&file).unwrap();
}

#[test]
fn kdf_costs_are_checked_and_bound_on_every_unlock() {
    let dir = Dir::new("kdfbind");
    let file = sample(&dir);
    let key = SecretKey::random().unwrap();
    let mut vault = file.open_with_password(b"correct horse").unwrap();
    vault.set_keychain_key(&key).unwrap();
    vault.save(&file).unwrap();
    let original: serde_json::Value =
        serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap();
    // Costs below the floor are refused on the keychain path too.
    let mut weak = original.clone();
    weak["kdf"]["memoryKib"] = 8.into();
    std::fs::write(file.path(), serde_json::to_vec(&weak).unwrap()).unwrap();
    assert!(matches!(
        file.open_with_key(&key),
        Err(VaultError::Corrupt(_))
    ));
    // Costs inside the bounds but not the ones the slot was made with fail
    // authentication instead of being saved again.
    let mut other = original.clone();
    other["kdf"]["iterations"] = (MIN_ITERATIONS + 1).into();
    std::fs::write(file.path(), serde_json::to_vec(&other).unwrap()).unwrap();
    assert!(matches!(
        file.open_with_password(b"correct horse"),
        Err(VaultError::WrongKey)
    ));
    // The keychain path checks them too, through the manifest.
    assert!(matches!(
        file.open_with_key(&key),
        Err(VaultError::Corrupt(_))
    ));
    let mut salted = original;
    salted["kdf"]["salt"] = encode(&[0u8; SALT_LEN]).into();
    std::fs::write(file.path(), serde_json::to_vec(&salted).unwrap()).unwrap();
    assert!(matches!(
        file.open_with_key(&key),
        Err(VaultError::Corrupt(_))
    ));
}

#[test]
fn a_record_id_the_manifest_cannot_separate_is_refused() {
    let dir = Dir::new("badid");
    let file = sample(&dir);
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap();
    value["records"][0]["id"] = "id1 x".into();
    std::fs::write(file.path(), serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(matches!(
        file.open_with_password(b"correct horse"),
        Err(VaultError::Corrupt(_))
    ));
}

#[test]
fn a_vault_too_big_to_read_back_is_not_saved() {
    let dir = Dir::new("big");
    let file = sample(&dir);
    let mut vault = file.open_with_password(b"correct horse").unwrap();
    // 256 records of 64 KiB come to over 16 MiB once encoded.
    for n in 0..256 {
        vault
            .put(&format!("big{n}"), vec![1u8; MAX_RECORD])
            .unwrap();
    }
    assert!(matches!(vault.save(&file), Err(VaultError::Limit(_))));
    assert!(file.open_with_password(b"correct horse").is_ok());
}

#[test]
fn limits_and_ids_are_checked() {
    let dir = Dir::new("limits");
    let file = dir.file();
    let mut vault = file.create(b"pw", TEST_KDF).unwrap();
    assert!(matches!(
        file.create(b"pw", TEST_KDF),
        Err(VaultError::Exists)
    ));
    for bad in ["", "a/b", "../x", &"x".repeat(65)] {
        assert!(vault.put(bad, vec![]).is_err(), "{bad:?}");
    }
    assert!(vault.put("big", vec![0; MAX_RECORD + 1]).is_err());
    assert!(!vault.remove("missing"));
    assert!(matches!(
        VaultFile::new(dir.0.join("none.json")).open_with_password(b"pw"),
        Err(VaultError::Missing)
    ));
    assert!(
        Kdf {
            memory_kib: 1,
            ..TEST_KDF
        }
        .check()
        .is_err()
    );
}

#[test]
fn debug_output_shows_no_secrets() {
    let dir = Dir::new("debug");
    let file = sample(&dir);
    let vault = file.open_with_password(b"correct horse").unwrap();
    let shown = format!("{vault:?} {:?}", SecretKey::random().unwrap());
    assert!(!shown.contains("secret one"));
    assert!(shown.contains("SecretKey(..)"));
}

#[test]
fn removing_or_rolling_back_a_record_is_refused() {
    let dir = Dir::new("manifest");
    let file = sample(&dir);
    let older: serde_json::Value =
        serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap();
    let mut vault = file.open_with_password(b"correct horse").unwrap();
    vault.put("id1", b"rotated".to_vec()).unwrap();
    vault.save(&file).unwrap();
    let current: serde_json::Value =
        serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap();
    let write = |value: &serde_json::Value| {
        std::fs::write(file.path(), serde_json::to_vec(value).unwrap()).unwrap();
    };
    let mut dropped = current.clone();
    dropped["records"].as_array_mut().unwrap().remove(1);
    write(&dropped);
    assert!(matches!(
        file.open_with_password(b"correct horse"),
        Err(VaultError::Corrupt(_))
    ));
    let mut rolled_back = current.clone();
    rolled_back["records"][0] = older["records"][0].clone();
    write(&rolled_back);
    assert!(matches!(
        file.open_with_password(b"correct horse"),
        Err(VaultError::Corrupt(_))
    ));
    write(&current);
    let vault = file.open_with_password(b"correct horse").unwrap();
    assert_eq!(vault.get("id1"), Some(&b"rotated"[..]));
}

#[test]
fn a_save_waits_a_bounded_time_for_another_writer() {
    let dir = Dir::new("locked");
    let file = sample(&dir);
    let mut vault = file.open_with_password(b"correct horse").unwrap();
    let held = lock_file(file.path()).unwrap();
    let started = std::time::Instant::now();
    assert!(lock_file(file.path()).is_err());
    let waited = started.elapsed();
    // At least the wait, and not unbounded; loaded CI runners oversleep.
    assert!(waited >= LOCK_WAIT && waited < LOCK_WAIT * 10, "{waited:?}");
    assert!(matches!(vault.save(&file), Err(VaultError::Io(_))));
    drop(held);
    vault.save(&file).unwrap();
}

#[test]
fn the_history_key_is_readable_locked_and_checked_on_unlock() {
    let dir = Dir::new("historykey");
    let file = sample(&dir);
    assert_eq!(file.history_public().unwrap(), None);
    let mut vault = file.open_with_password(b"correct horse").unwrap();
    vault.set_history_public([9; KEY_LEN]);
    vault.save(&file).unwrap();
    assert_eq!(file.history_public().unwrap(), Some([9; KEY_LEN]));
    let vault = file.open_with_password(b"correct horse").unwrap();
    assert_eq!(vault.history_public(), Some([9; KEY_LEN]));
    // A swapped key, so commands would be sealed to someone else, fails the
    // next unlock.
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap();
    value["history"] = encode(&[7; KEY_LEN]).into();
    std::fs::write(file.path(), serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(matches!(
        file.open_with_password(b"correct horse"),
        Err(VaultError::Corrupt(_))
    ));
    // Removing it fails too.
    value.as_object_mut().unwrap().remove("history");
    std::fs::write(file.path(), serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(matches!(
        file.open_with_password(b"correct horse"),
        Err(VaultError::Corrupt(_))
    ));
}

#[test]
fn a_vault_without_a_history_key_keeps_its_old_manifest() {
    // The manifest's additional data from before the history existed.
    let costs = TEST_KDF;
    let salt = [3u8; SALT_LEN];
    let aad = manifest_aad("00112233445566778899aabbccddeeff", costs, &salt, None);
    let Kdf {
        memory_kib,
        iterations,
        parallelism,
    } = costs;
    assert_eq!(
        aad,
        format!(
            "{FORMAT}/{VERSION}/manifest/00112233445566778899aabbccddeeff/argon2id/{memory_kib}/{iterations}/{parallelism}/{}",
            hex(&salt)
        )
        .into_bytes()
    );
}
