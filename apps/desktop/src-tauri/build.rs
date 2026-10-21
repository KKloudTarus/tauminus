// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

include!("src/command_names.rs");

fn main() {
    // Every app command gets generated `allow-*` and `deny-*` permissions, and
    // a window can call only the commands its capability file grants.
    let manifest = tauri_build::AppManifest::new().commands(COMMANDS);
    let mut attributes = tauri_build::Attributes::new().app_manifest(manifest);

    // tauri-build embeds its Windows manifest into the app binary only, so test
    // binaries load Common Controls v5 and fail with STATUS_ENTRYPOINT_NOT_FOUND.
    // Embed the manifest through the linker instead, which reaches every
    // binary of this package.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
        let manifest_path = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("windows-app-manifest.xml");
        println!("cargo:rerun-if-changed={}", manifest_path.display());
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest_path.display());
    }

    tauri_build::try_build(attributes).expect("tauri build script failed");
}
