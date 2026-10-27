// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Composition root: builds the adapters, hands them to the use cases, and
//! exposes the use cases to the webview as IPC commands.

mod ai;
mod clipboard;
mod cloud;
mod commands;
mod drops;
mod edit;
mod files;
mod fill;
mod forwards;
mod history;
mod hosts;
mod import;
mod ipc;
mod links;
mod mosh;
mod notices;
mod recordings;
mod serial;
mod shells;
mod snippets;
mod ssh;
mod sync;
mod telnet;
mod tmux;
mod triggers;
mod update;
mod vault;
mod zmodem;

use std::sync::Arc;

use tauminus_app::vault::Vault;
use tauminus_app::{
    HostBook, LocalShells, SessionConfig, SessionManager, SnippetBook, SshSessions, TriggerBook,
};
use tauminus_pty::NativePtyLauncher;
use tauminus_ssh::{AgentSource, HostKeyStore, OpenSshConfig, RusshConnector};
use tauminus_store::vault::VaultFile;
use tauminus_store::{
    FileHistoryBackend, FileVaultBackend, JsonHostStore, JsonSnippetStore, JsonTriggerStore,
    OsKeychain,
};
use tauri::Manager;
use tauri::webview::PageLoadEvent;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// Typed command registry. The same builder serves IPC at runtime and
/// generates `apps/desktop/src/ipc/bindings.ts`.
#[expect(clippy::too_many_lines, reason = "one line per command")]
pub fn specta_builder<R: tauri::Runtime>() -> tauri_specta::Builder<R> {
    tauri_specta::Builder::<R>::new().commands(tauri_specta::collect_commands![
        commands::session_open,
        shells::local_shells,
        commands::session_write,
        fill::session_fill_offer,
        fill::session_fill_password,
        tmux::tmux_open,
        tmux::tmux_pane_attach,
        tmux::tmux_new_window,
        tmux::tmux_detach,
        zmodem::zmodem_listen,
        zmodem::zmodem_receive,
        zmodem::zmodem_choose::<tauri::Wry>,
        zmodem::zmodem_cancel,
        zmodem::zmodem_reveal,
        ai::ai_settings_get,
        ai::ai_settings_set,
        ai::ai_key_set,
        ai::ai_key_clear,
        ai::ai_ask,
        ai::ai_preview,
        ai::ai_explain,
        commands::session_write_bytes,
        commands::session_resize,
        commands::session_probe,
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
        snippets::snippets_list,
        snippets::snippet_save,
        snippets::snippet_delete,
        snippets::snippet_command,
        snippets::host_startup,
        triggers::trigger_sets_list,
        triggers::trigger_set_save,
        triggers::trigger_set_delete,
        triggers::trigger_defaults_save,
        triggers::trigger_pattern_check,
        triggers::trigger_prepare,
        triggers::trigger_scan,
        triggers::trigger_fire,
        notices::trigger_notify,
        telnet::telnet_connect,
        mosh::mosh_connect,
        recordings::recording_start,
        recordings::recording_stop,
        recordings::recordings_active,
        recordings::recordings_list,
        recordings::recording_read,
        recordings::recording_checkpoints,
        recordings::recording_save_checkpoints,
        recordings::recording_bookmarks,
        recordings::recording_save_bookmarks,
        recordings::recording_delete,
        serial::serial_ports,
        serial::serial_open,
        hosts::hosts_import_ssh_config,
        import::hosts_import_from::<tauri::Wry>,
        cloud::cloud_preview,
        cloud::cloud_import,
        update::update_check::<tauri::Wry>,
        update::update_install::<tauri::Wry>,
        ssh::host_connect::<tauri::Wry>,
        forwards::forward_start,
        forwards::forward_stop,
        forwards::forwards_status,
        files::sftp_open,
        files::sftp_close,
        files::sftp_list,
        files::sftp_change,
        files::local_list,
        files::local_home,
        files::transfer_download,
        files::transfer_upload,
        files::transfers_status,
        files::transfer_cancel,
        drops::file_drops,
        edit::sftp_edit,
        edit::edit_done,
        edit::edits_status,
        hosts::guard_check,
        links::open_link,
        clipboard::clipboard_read_text,
        clipboard::clipboard_write_text,
        vault::vault_status,
        vault::vault_create,
        vault::vault_unlock,
        vault::vault_unlock_keychain,
        vault::vault_lock,
        vault::vault_touch,
        vault::vault_set_idle,
        vault::vault_change_password,
        vault::vault_set_keychain,
        vault::vault_set_presence,
        vault::identities_list,
        vault::identity_save,
        vault::identity_delete,
        ssh::identity_install,
        history::history_record,
        history::history_search,
        history::history_output,
        history::history_clear,
        sync::sync_status,
        sync::sync_listen,
        sync::sync_create,
        sync::sync_join,
        sync::sync_now,
        sync::sync_leave,
        sync::sync_reviews,
        sync::sync_review_accept,
        sync::sync_choose_folder::<tauri::Wry>,
    ])
}

/// Where the app keeps its files and finds the user's.
pub(crate) struct Paths {
    pub(crate) config_dir: std::path::PathBuf,
    pub(crate) home: Option<std::path::PathBuf>,
    /// The user's downloads folder: files a host sends go to `Tauminus`
    /// in it.
    pub(crate) downloads: Option<std::path::PathBuf>,
}

/// The snippet and highlight libraries, each kept as it is and reported
/// when its file cannot be read.
fn manage_libraries<R: tauri::Runtime>(app: &tauri::App<R>, config_dir: &std::path::Path) {
    let store = Arc::new(JsonSnippetStore::new(config_dir.join("snippets.json")));
    let library = tauri::async_runtime::block_on(SnippetBook::open(store));
    app.manage(match library {
        Ok(book) => snippets::Snippets::Ready(Arc::new(book)),
        Err(e) => {
            // Keep the file as it is; saving now would destroy it.
            tracing::error!(error = %e, "saved snippets unavailable");
            snippets::Snippets::Unavailable(e.to_string())
        }
    });
    let store = Arc::new(JsonTriggerStore::new(config_dir.join("triggers.json")));
    let library = tauri::async_runtime::block_on(TriggerBook::open(store));
    app.manage(match library {
        Ok(book) => triggers::Triggers::Ready(Arc::new(book)),
        Err(e) => {
            // Keep the file as it is; saving now would destroy it.
            tracing::error!(error = %e, "saved highlight sets unavailable");
            triggers::Triggers::Unavailable(e.to_string())
        }
    });
}

/// Builds every adapter and use case and hands them to Tauri as state. The
/// app and the IPC tests share it, so a command never asks for state the
/// app does not manage.
pub(crate) fn install<R: tauri::Runtime>(app: &tauri::App<R>, paths: Paths) {
    let Paths {
        config_dir,
        home,
        downloads,
    } = paths;
    let sessions = Arc::new(SessionManager::new(SessionConfig::default()));
    let transfers = Arc::new(zmodem::Transfers::default());
    sessions
        .set_transfer_host(Arc::clone(&transfers) as Arc<dyn tauminus_app::ports::TransferHost>);
    app.manage(transfers);
    app.manage(zmodem::Choosing::default());
    app.manage(cloud::Cloud::new(tauminus_cloud::Clis::new()));
    app.manage(zmodem::ReceiveRoot(
        downloads
            .or_else(|| home.as_ref().map(|home| home.join("Downloads")))
            .unwrap_or_else(|| config_dir.join("received"))
            .join("Tauminus"),
    ));
    manage_local_shells(app, &sessions);
    app.manage(Arc::new(ssh::PromptBroker::default()));
    app.manage(clipboard::Clipboard::default());
    app.manage(notices::Notices::default());

    // Keys the user trusts here go to Tauminus's own file; the user's
    // OpenSSH files are read but never written.
    let remember_in = config_dir.join("known_hosts");
    let store = HostKeyStore::new(HostKeyStore::openssh_files(home.as_deref()), remember_in);
    let connector = RusshConnector::new(store, home.clone(), AgentSource::Environment);
    #[cfg(not(windows))]
    let connector = connector.with_security_key(Arc::new(tauminus_fido::HidSecurityKey::default()));
    let connector: Arc<dyn tauminus_app::ports::SshConnector> = Arc::new(connector);
    let connections = Arc::new(tauminus_app::Connections::new(Arc::clone(&connector)));
    app.manage(Arc::new(tauminus_app::Files::new(Arc::clone(&connections))));
    app.manage(files::UploadState(config_dir.join("transfers")));
    app.manage(files::Home(home.clone()));
    app.manage(drops::Drops::default());
    app.manage(Arc::new(edit::Edits::new(edit::edit_root())));
    app.manage(Arc::new(tauminus_app::Forwards::new(connections)));
    manage_terminals(app, connector, &sessions);
    let recording_sessions = Arc::clone(&sessions);
    let fill_sessions = Arc::clone(&sessions);
    let ai_sessions = Arc::clone(&sessions);
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
    manage_libraries(app, &config_dir);
    let backend = FileVaultBackend::new(
        VaultFile::new(config_dir.join("vault.json")),
        Arc::new(OsKeychain::new()),
    );
    let presence = Arc::new(tauminus_presence::OsPresence::new());
    // Windows Hello's prompt opens over the app's window.
    #[cfg(windows)]
    if let Some(hwnd) = app
        .get_webview_window("main")
        .and_then(|window| window.hwnd().ok())
    {
        presence.set_window(hwnd.0 as isize);
    }
    let vault = tauri::async_runtime::block_on(async {
        Vault::with_presence(
            Arc::new(backend),
            tauminus_app::vault::DEFAULT_IDLE,
            presence,
        )
    });
    manage_history(app, &vault, &config_dir);
    app.manage(Arc::new(tauminus_app::recording::Recordings::new(
        Arc::new(tauminus_store::FileRecordingStore::new(
            config_dir.join("recordings"),
        )),
        Arc::clone(&vault),
        recording_sessions,
    )));
    let trust = open_trust(&config_dir);
    let fill_snippets = match &*app.state::<snippets::Snippets>() {
        snippets::Snippets::Ready(book) => Some(Arc::clone(book)),
        snippets::Snippets::Unavailable(_) => None,
    };
    // Without the review list no saved host's password is typed.
    let fill = match (&*app.state::<hosts::Hosts>(), &trust) {
        (hosts::Hosts::Ready(book), Ok(trust)) => Some(Arc::new(
            tauminus_app::Fill::new(fill_sessions, Arc::clone(book), Arc::clone(&vault))
                .with_trust(Arc::clone(trust), fill_snippets),
        )),
        _ => None,
    };
    app.manage(fill::FillState(fill));
    manage_assistant(app, ai_sessions, Arc::clone(&vault), &config_dir, &trust);
    manage_sync(app, &vault, &config_dir, trust);
    app.manage(vault);
    let local = ssh::local_user().unwrap_or_default();
    app.manage(hosts::SshConfig(
        home.map(|home| OpenSshConfig::user(home, local)),
    ));
}

/// The command history, sealed to the vault.
fn manage_history<R: tauri::Runtime>(
    app: &tauri::App<R>,
    vault: &Arc<Vault>,
    config_dir: &std::path::Path,
) {
    let history = tauri::async_runtime::block_on(tauminus_app::history::History::new(
        Arc::new(FileHistoryBackend::new(config_dir)),
        Arc::clone(vault),
        tauminus_domain::Retention::default(),
    ));
    app.manage(history);
}

/// The sync review list every use of a saved host checks.
fn open_trust(config_dir: &std::path::Path) -> Result<Arc<tauminus_app::trust::Trust>, String> {
    let store = Arc::new(tauminus_store::JsonTrustStore::new(
        config_dir.join("sync-review.json"),
    ));
    tauri::async_runtime::block_on(tauminus_app::trust::Trust::open(store))
        .map(Arc::new)
        .map_err(|e| {
            tracing::error!(error = %e, "sync review list unavailable");
            e.to_string()
        })
}

/// Sync, and the review list every use of a saved host checks. Without
/// the review list no saved host is used; without the saved lists there is
/// nothing to sync.
fn manage_sync<R: tauri::Runtime>(
    app: &tauri::App<R>,
    vault: &Arc<Vault>,
    config_dir: &std::path::Path,
    trust: Result<Arc<tauminus_app::trust::Trust>, String>,
) {
    let snippets = match &*app.state::<snippets::Snippets>() {
        snippets::Snippets::Ready(book) => Some(Arc::clone(book)),
        snippets::Snippets::Unavailable(_) => None,
    };
    let service = match (&*app.state::<hosts::Hosts>(), &snippets, &trust) {
        (hosts::Hosts::Ready(hosts), Some(snippets), Ok(trust)) => {
            Ok(Arc::new(tauminus_app::SyncService::new(
                Arc::clone(vault),
                Arc::clone(hosts),
                Arc::clone(snippets),
                Arc::clone(trust),
                Arc::new(tauminus_sync::connector::Connector::new(
                    config_dir.join("sync"),
                )),
                Arc::new(sync::SshKeys),
            )))
        }
        (hosts::Hosts::Unavailable(reason), _, _) => Err(reason.clone()),
        (_, None, _) => Err("the saved snippets could not be read".to_owned()),
        (_, _, Err(reason)) => Err(reason.clone()),
    };
    app.manage(sync::Gate::new(trust, snippets, Arc::clone(vault)));
    let state = sync::SyncState::new(service);
    sync::schedule(&state);
    app.manage(state);
}

/// Shells on this computer: the default one, and the others it has by id.
fn manage_local_shells<R: tauri::Runtime>(app: &tauri::App<R>, sessions: &Arc<SessionManager>) {
    app.manage(LocalShells::new(
        Arc::new(NativePtyLauncher),
        Arc::clone(sessions),
    ));
    app.manage(shells::ShellCatalog::default());
}

/// The session kinds that reach other machines: serial ports, Telnet,
/// Mosh, tmux tabs, and SSH shells.
fn manage_terminals<R: tauri::Runtime>(
    app: &tauri::App<R>,
    connector: Arc<dyn tauminus_app::ports::SshConnector>,
    sessions: &Arc<SessionManager>,
) {
    app.manage(tauminus_app::SerialSessions::new(
        Arc::new(tauminus_serial::NativeSerialPorts::default()),
        Arc::clone(sessions),
    ));
    app.manage(tauminus_app::TelnetSessions::new(
        Arc::new(tauminus_telnet::TcpTelnetConnector::default()),
        Arc::clone(sessions),
    ));
    app.manage(tauminus_app::MoshSessions::new(
        Arc::clone(&connector),
        Arc::new(tauminus_mosh::NativeMoshClient::new(Arc::new(
            NativePtyLauncher,
        ))),
        Arc::clone(sessions),
    ));
    app.manage(Arc::new(tauminus_app::TmuxSessions::new(
        Arc::clone(&connector),
        Arc::clone(sessions),
    )));
    app.manage(SshSessions::new(connector, Arc::clone(sessions)));
}

/// The AI assistant, off until the user turns it on; it needs the saved
/// hosts to find whether a session's host lets it read its output.
fn manage_assistant<R: tauri::Runtime>(
    app: &tauri::App<R>,
    sessions: Arc<SessionManager>,
    vault: Arc<Vault>,
    config_dir: &std::path::Path,
    trust: &Result<Arc<tauminus_app::trust::Trust>, String>,
) {
    let snippets = match &*app.state::<snippets::Snippets>() {
        snippets::Snippets::Ready(book) => Some(Arc::clone(book)),
        snippets::Snippets::Unavailable(_) => None,
    };
    let assistant = match &*app.state::<hosts::Hosts>() {
        hosts::Hosts::Ready(book) => match trust {
            // Without the review list no saved host lends its output.
            Err(reason) => Err(reason.clone()),
            Ok(trust) => tauminus_ai::OpenAiCompatible::new().map(|transport| {
                Arc::new(
                    tauminus_app::Assistant::new(
                        sessions,
                        Arc::clone(book),
                        vault,
                        Arc::new(tauminus_store::JsonAiSettingsStore::new(
                            config_dir.join("ai.json"),
                        )),
                        Arc::new(transport),
                    )
                    .with_trust(Arc::clone(trust), snippets),
                )
            }),
        },
        hosts::Hosts::Unavailable(_) => Err("saved hosts could not be read".to_owned()),
    };
    app.manage(ai::AiState(assistant));
}

pub fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("TAUMINUS_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let builder = specta_builder();
    tauri::Builder::default()
        // Called from Rust only (`notices`); the window has none of its
        // permissions.
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(update::Updates::default())
        .invoke_handler(builder.invoke_handler())
        .setup(|app| {
            let paths = Paths {
                config_dir: app.path().app_config_dir()?,
                home: app.path().home_dir().ok(),
                downloads: app.path().download_dir().ok(),
            };
            install(app, paths);
            let notifier = app.handle().clone();
            app.state::<notices::Notices>()
                .set_show(Box::new(move |title, body| {
                    use tauri_plugin_notification::NotificationExt as _;
                    notifier
                        .notification()
                        .builder()
                        .title(title)
                        .body(body)
                        .show()
                        .map_err(|e| e.to_string())
                }));
            if let Some(window) = app.get_webview_window("main") {
                app.state::<notices::Notices>()
                    .set_focused(window.is_focused().unwrap_or(true));
                let handle = app.handle().clone();
                let this = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::Focused(focused) = event {
                        handle.state::<notices::Notices>().set_focused(*focused);
                    }
                    if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop {
                        paths,
                        position,
                    }) = event
                    {
                        // Per drop: the window may have moved to another screen.
                        let scale = this.scale_factor().unwrap_or(1.0);
                        handle.state::<drops::Drops>().dropped(
                            paths,
                            position.x / scale,
                            position.y / scale,
                        );
                    }
                });
            }
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
        // Plugins' permissions (`name:...`) give the webview commands this
        // list does not name: notifications are raised from Rust only.
        let parsed: serde_json::Value = serde_json::from_str(&capability).expect("capability JSON");
        let permissions = parsed["permissions"].as_array().expect("a permission list");
        for permission in permissions {
            let name = permission.as_str().expect("permissions by name");
            assert!(
                name.starts_with("allow-"),
                "capabilities/main.json grants {name}, which is no app command"
            );
        }
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
