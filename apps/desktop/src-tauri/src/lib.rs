// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Composition root: builds the adapters, hands them to the use cases, and
//! exposes the use cases to the webview as IPC commands.

mod commands;
mod hosts;
mod ipc;
mod links;
mod ssh;

use std::sync::Arc;

use tauminus_app::{HostBook, LocalShells, SessionConfig, SessionManager, SshSessions};
use tauminus_pty::NativePtyLauncher;
use tauminus_ssh::{AgentSource, HostKeyStore, OpenSshConfig, RusshConnector};
use tauminus_store::JsonHostStore;
use tauri::Manager;
use tauri::webview::PageLoadEvent;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// Typed command registry. The same builder serves IPC at runtime and
/// generates `apps/desktop/src/ipc/bindings.ts`.
pub fn specta_builder<R: tauri::Runtime>() -> tauri_specta::Builder<R> {
    tauri_specta::Builder::<R>::new().commands(tauri_specta::collect_commands![
        commands::session_open,
        commands::session_write,
        commands::session_write_bytes,
        commands::session_resize,
        commands::session_acknowledge,
        commands::session_close,
        ssh::ssh_open,
        ssh::ssh_prompt_reply,
        hosts::hosts_list,
        hosts::host_save,
        hosts::host_delete,
        hosts::group_save,
        hosts::group_delete,
        hosts::ssh_config_aliases,
        hosts::hosts_import_ssh_config,
        ssh::host_connect,
        links::open_link,
    ])
}

/// Where the app keeps its files and finds the user's.
pub(crate) struct Paths {
    pub(crate) config_dir: std::path::PathBuf,
    pub(crate) home: Option<std::path::PathBuf>,
}

/// Builds every adapter and use case and hands them to Tauri as state. The
/// app and the IPC tests share it, so a command never asks for state the
/// app does not manage.
pub(crate) fn install<R: tauri::Runtime>(app: &tauri::App<R>, paths: Paths) {
    let Paths { config_dir, home } = paths;
    let sessions = Arc::new(SessionManager::new(SessionConfig::default()));
    app.manage(LocalShells::new(
        Arc::new(NativePtyLauncher),
        Arc::clone(&sessions),
    ));
    app.manage(Arc::new(ssh::PromptBroker::default()));

    // Keys the user trusts here go to Tauminus's own file; the user's
    // OpenSSH files are read but never written.
    let remember_in = config_dir.join("known_hosts");
    let store = HostKeyStore::new(HostKeyStore::openssh_files(home.as_deref()), remember_in);
    let connector = RusshConnector::new(store, home.clone(), AgentSource::Environment);
    app.manage(SshSessions::new(Arc::new(connector), Arc::clone(&sessions)));
    app.manage(sessions);

    let store = Arc::new(JsonHostStore::new(config_dir.join("hosts.json")));
    let book = tauri::async_runtime::block_on(HostBook::open(store));
    app.manage(match book {
        Ok(book) => hosts::Hosts::Ready(Arc::new(book)),
        Err(e) => {
            // Keep the file as it is; saving now would destroy it.
            tracing::error!(error = %e, "saved hosts unavailable");
            hosts::Hosts::Unavailable(e.to_string())
        }
    });
    let local = ssh::local_user().unwrap_or_default();
    app.manage(hosts::SshConfig(
        home.map(|home| OpenSshConfig::user(home, local)),
    ));
}

pub fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("TAUMINUS_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let builder = specta_builder();
    tauri::Builder::default()
        .invoke_handler(builder.invoke_handler())
        .setup(|app| {
            let paths = Paths {
                config_dir: app.path().app_config_dir()?,
                home: app.path().home_dir().ok(),
            };
            install(app, paths);
            Ok(())
        })
        // A reload or navigation destroys every view, and nothing can attach
        // to the old sessions again. End them instead of leaking shells.
        .on_page_load(|webview, payload| {
            if payload.event() == PageLoadEvent::Started {
                webview.state::<Arc<ssh::PromptBroker>>().cancel_all();
                let closed = webview.state::<Arc<SessionManager>>().close_all();
                if closed > 0 {
                    info!(closed, "page reloaded; closed orphaned sessions");
                }
            }
        })
        .run(tauri::generate_context!())?;
    Ok(())
}

#[cfg(test)]
mod ipc_tests;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use specta_typescript::Typescript;

    include!("command_names.rs");

    fn manifest_dir() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    /// Regenerates the TypeScript bindings. CI runs the test suite and then
    /// fails if the committed file differs from the generated one.
    #[test]
    fn export_typescript_bindings() {
        let target = manifest_dir().join("../src/ipc/bindings.ts");
        // REUSE-IgnoreStart
        super::specta_builder::<tauri::Wry>()
            .export(
                Typescript::default().header(
                    "// SPDX-FileCopyrightText: 2026 Tauminus contributors\n\
                     // SPDX-License-Identifier: GPL-3.0-or-later\n\
                     // Generated by `cargo test -p tauminus-desktop`. Do not edit.\n",
                ),
                &target,
            )
            .expect("bindings export");
        // REUSE-IgnoreEnd
    }

    /// The command list in `command_names.rs`, the commands registered with
    /// specta, and the main window's capability must name the same commands.
    #[test]
    fn command_lists_agree() {
        let expected: BTreeSet<String> = COMMANDS.iter().map(ToString::to_string).collect();

        let rendered =
            std::env::temp_dir().join(format!("tauminus-bindings-{}.ts", std::process::id()));
        super::specta_builder::<tauri::Wry>()
            .export(Typescript::default(), &rendered)
            .expect("bindings render");
        let bindings = std::fs::read_to_string(&rendered).expect("rendered bindings");
        let _ = std::fs::remove_file(&rendered);
        // Commands returning `Result` render as `__TAURI_INVOKE("name"`, the
        // others as `__TAURI_INVOKE<T>("name"`.
        let registered: BTreeSet<String> = bindings
            .split("__TAURI_INVOKE")
            .skip(1)
            .filter_map(|rest| rest.split_once("(\"").map(|(_, name)| name))
            .filter_map(|rest| rest.split('"').next())
            .map(ToString::to_string)
            .collect();
        assert_eq!(registered, expected, "specta registry vs command_names.rs");

        let capability = std::fs::read_to_string(manifest_dir().join("capabilities/main.json"))
            .expect("capability file");
        let granted: BTreeSet<String> = capability
            .split("\"allow-")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .map(|name| name.replace('-', "_"))
            .collect();
        assert_eq!(
            granted, expected,
            "capabilities/main.json vs command_names.rs"
        );
    }
}
