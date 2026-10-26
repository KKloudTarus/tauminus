// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::{Path, PathBuf};

use super::*;

/// A key `tauri signer generate` made for these tests, password `fixture`.
/// It signs nothing anyone installs.
fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("xtask-updater-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn bundles(&self, version: &str) {
        for name in [
            format!("Tauminus_{version}_amd64.AppImage"),
            format!("Tauminus_{version}_amd64.deb"),
            format!("Tauminus-{version}-1.x86_64.rpm"),
            format!("Tauminus_{version}_x64-setup.exe"),
            format!("Tauminus_{version}_x64_en-US.msi"),
            format!("Tauminus_{version}_universal.app.tar.gz"),
        ] {
            std::fs::write(self.0.join(&name), format!("bytes of {name}")).unwrap();
        }
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const BASE: &str = "https://github.com/KKloudTarus/tauminus/releases/download/v1.2.3";

#[test]
fn a_key_tauri_made_decodes_to_the_public_key_tauri_wrote() {
    let key = Key::decode(&fixture("updater-test.key"), "fixture").unwrap();
    key.matches(&fixture("updater-test.key.pub")).unwrap();
    let error = Key::decode(&fixture("updater-test.key"), "guess")
        .err()
        .unwrap();
    assert!(error.to_string().contains("password is wrong"), "{error}");
}

#[test]
fn every_bundle_is_signed_for_its_version_and_listed_for_each_target() {
    let dir = Dir::new("all");
    dir.bundles("1.2.3");
    let key = Key::decode(&fixture("updater-test.key"), "fixture").unwrap();
    let signed = sign_dir(&dir.0, &key, "1.2.3", BASE, "Faster sync.\n", 1_800_000_000).unwrap();
    assert_eq!(signed.len(), 6);
    let latest: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.0.join("latest.json")).unwrap()).unwrap();
    assert_eq!(latest["version"], "1.2.3");
    assert_eq!(latest["notes"], "Faster sync.");
    let platforms = latest["platforms"].as_object().unwrap();
    for target in [
        "linux-x86_64",
        "linux-x86_64-appimage",
        "linux-x86_64-deb",
        "linux-x86_64-rpm",
        "windows-x86_64",
        "windows-x86_64-nsis",
        "windows-x86_64-msi",
        "darwin-aarch64",
        "darwin-x86_64",
        "darwin-aarch64-app",
        "darwin-x86_64-app",
    ] {
        let entry = &platforms[target];
        let url = entry["url"].as_str().unwrap();
        let name = url.strip_prefix(&format!("{BASE}/")).unwrap();
        let data = std::fs::read(dir.0.join(name)).unwrap();
        let signature = entry["signature"].as_str().unwrap();
        // Against the public key file tauri wrote, as the app checks.
        verify(&fixture("updater-test.key.pub"), &data, signature, "1.2.3").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.0.join(format!("{name}.sig"))).unwrap(),
            signature
        );
        // Signed for this version only.
        assert!(verify(&fixture("updater-test.key.pub"), &data, signature, "1.2.4").is_err());
        // For these bytes only.
        assert!(
            verify(
                &fixture("updater-test.key.pub"),
                b"other",
                signature,
                "1.2.3"
            )
            .is_err()
        );
    }
    assert_eq!(
        platforms["linux-x86_64"]["url"],
        format!("{BASE}/Tauminus_1.2.3_amd64.AppImage")
    );
    assert_eq!(
        platforms["windows-x86_64"]["url"],
        format!("{BASE}/Tauminus_1.2.3_x64-setup.exe")
    );
}

#[test]
fn a_missing_stale_or_doubled_bundle_stops_the_signing() {
    let key = Key::ephemeral().unwrap();
    let dir = Dir::new("missing");
    dir.bundles("1.2.3");
    std::fs::remove_file(dir.0.join("Tauminus_1.2.3_x64_en-US.msi")).unwrap();
    let error = sign_dir(&dir.0, &key, "1.2.3", BASE, "", 0).unwrap_err();
    assert!(error.to_string().contains("`*.msi`"), "{error}");

    let stale = Dir::new("stale");
    stale.bundles("1.2.2");
    let error = sign_dir(&stale.0, &key, "1.2.3", BASE, "", 0).unwrap_err();
    assert!(
        error.to_string().contains("not named for version 1.2.3"),
        "{error}"
    );

    let doubled = Dir::new("doubled");
    doubled.bundles("1.2.3");
    std::fs::write(doubled.0.join("Tauminus_1.2.3_arm64.deb"), "x").unwrap();
    let error = sign_dir(&doubled.0, &key, "1.2.3", BASE, "", 0).unwrap_err();
    assert!(error.to_string().contains("found 2"), "{error}");
    assert!(!doubled.0.join("latest.json").exists());
}

#[test]
fn a_key_that_is_not_the_app_s_signs_nothing() {
    let key = Key::decode(&fixture("updater-test.key"), "fixture").unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let error = key.matches(&app_public_key(root).unwrap()).unwrap_err();
    assert!(error.to_string().contains("tauri.conf.json"), "{error}");
}

#[test]
fn an_http_base_or_an_odd_version_is_refused() {
    let key = Key::ephemeral().unwrap();
    let dir = Dir::new("odd");
    dir.bundles("1.2.3");
    assert!(sign_dir(&dir.0, &key, "1.2.3", "http://example.com", "", 0).is_err());
    assert!(sign_dir(&dir.0, &key, "1.2.3\tversion:9", BASE, "", 0).is_err());
}
