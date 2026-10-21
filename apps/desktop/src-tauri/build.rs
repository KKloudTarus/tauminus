// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

include!("src/command_names.rs");

fn main() {
    // Every app command gets generated `allow-*` and `deny-*` permissions, and
    // a window can call only the commands its capability file grants.
    let manifest = tauri_build::AppManifest::new().commands(COMMANDS);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .expect("tauri build script failed");
}
