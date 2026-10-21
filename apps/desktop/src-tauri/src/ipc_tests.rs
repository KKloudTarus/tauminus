// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Commands called through Tauri's mock runtime, with the state `install`
//! sets up. A command that asks for state the app does not manage fails
//! here, not in the user's hands.

use serde_json::{Value, json};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{INVOKE_KEY, get_ipc_response, mock_builder, mock_context, noop_assets};
use tauri::webview::InvokeRequest;
use tauri::{App, WebviewWindowBuilder};

use crate::{Paths, install, specta_builder};

struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn app(name: &str) -> (App<tauri::test::MockRuntime>, Scratch) {
    let dir = std::env::temp_dir().join(format!("tauminus-ipc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // A home whose ssh config names nothing, so no `ssh -G` runs.
    std::fs::create_dir_all(dir.join("home/.ssh")).unwrap();
    std::fs::write(dir.join("home/.ssh/config"), "# empty\n").unwrap();
    let app = mock_builder()
        .invoke_handler(specta_builder().invoke_handler())
        .build(mock_context(noop_assets()))
        .unwrap();
    install(
        &app,
        Paths {
            config_dir: dir.join("config"),
            home: Some(dir.join("home")),
        },
    );
    (app, Scratch(dir))
}

fn call(app: &App<tauri::test::MockRuntime>, cmd: &str, args: Value) -> Result<Value, Value> {
    let webview = match tauri::Manager::get_webview_window(app, "main") {
        Some(webview) => webview,
        None => WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::default())
            .build()
            .unwrap(),
    };
    get_ipc_response(
        &webview,
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: InvokeBody::Json(args),
            headers: tauri::http::HeaderMap::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
    .map(|body| body.deserialize::<Value>().unwrap())
}

#[test]
fn hosts_commands_run_against_the_managed_state() {
    let (app, _dir) = app("hosts");
    assert_eq!(call(&app, "ssh_config_aliases", json!({})), Ok(json!(0)));
    assert_eq!(
        call(&app, "hosts_list", json!({})),
        Ok(json!({"groups": [], "hosts": []}))
    );

    let group = call(
        &app,
        "group_save",
        json!({"group": {"name": "prod", "settings": {"user": "ops"}}}),
    )
    .unwrap();
    let group_id = group["groups"][0]["id"].as_str().unwrap().to_owned();
    let saved = call(
        &app,
        "host_save",
        json!({"host": {"label": "db", "address": "db.internal", "group": group_id}}),
    )
    .unwrap();
    let host = &saved["hosts"][0];
    assert_eq!(host["target"]["destination"]["user"], "ops");
    let host_id = host["id"].as_str().unwrap().to_owned();

    let imported = call(&app, "hosts_import_ssh_config", json!({})).unwrap();
    assert_eq!(imported["added"], 0);
    let after = call(&app, "host_delete", json!({"id": host_id})).unwrap();
    assert_eq!(after["hosts"], json!([]));
    assert!(call(&app, "group_delete", json!({"id": group_id})).is_ok());
}

#[test]
fn the_webview_cannot_add_a_proxy_command() {
    let (app, _dir) = app("proxy");
    let refused = call(
        &app,
        "host_save",
        json!({"host": {
            "label": "evil",
            "address": "db.internal",
            "settings": {"route": {"kind": "proxyCommand", "command": "touch /tmp/pwned"}}
        }}),
    );
    let error = refused.unwrap_err();
    assert_eq!(error["code"], "invalid_argument", "{error}");
    assert!(error["message"].as_str().unwrap().contains("ProxyCommand"));

    let group = call(
        &app,
        "group_save",
        json!({"group": {"name": "g", "settings": {"route": {"kind": "proxyCommand", "command": "id"}}}}),
    );
    assert!(group.is_err());
    assert_eq!(
        call(&app, "hosts_list", json!({})),
        Ok(json!({"groups": [], "hosts": []}))
    );
}

#[test]
fn an_unreadable_host_file_is_reported_and_left_alone() {
    let dir = std::env::temp_dir().join(format!("tauminus-ipc-broken-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::write(dir.join("config/hosts.json"), "{ not json").unwrap();
    let _cleanup = Scratch(dir.clone());
    let app = mock_builder()
        .invoke_handler(specta_builder().invoke_handler())
        .build(mock_context(noop_assets()))
        .unwrap();
    install(
        &app,
        Paths {
            config_dir: dir.join("config"),
            home: None,
        },
    );
    let error = call(&app, "hosts_list", json!({})).unwrap_err();
    assert_eq!(error["code"], "store_failed");
    let save = call(
        &app,
        "host_save",
        json!({"host": {"label": "a", "address": "a"}}),
    );
    assert!(save.is_err());
    assert_eq!(
        std::fs::read_to_string(dir.join("config/hosts.json")).unwrap(),
        "{ not json"
    );
    assert_eq!(
        call(&app, "ssh_config_aliases", json!({})),
        Ok(json!(0)),
        "no home: nothing to offer"
    );
}
