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
            // The app's own origin, which the ACL treats as local.
            url: if cfg!(windows) {
                "http://tauri.localhost"
            } else {
                "tauri://localhost"
            }
            .parse()
            .unwrap(),
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
fn ssh_open_refuses_a_proxy_command_before_connecting() {
    let (app, _dir) = app("open");
    // The channels are not reached: the route is checked first.
    let refused = call(
        &app,
        "ssh_open",
        json!({
            "target": {
                "destination": {"user": "me", "host": "db", "port": 22},
                "route": {"kind": "proxyCommand", "command": "id"}
            },
            "size": {"cols": 80, "rows": 24},
            "onOutput": "__CHANNEL__:1",
            "onExit": "__CHANNEL__:2",
            "onPrompt": "__CHANNEL__:3"
        }),
    );
    let error = refused.unwrap_err();
    assert_eq!(error["code"], "invalid_argument", "{error}");
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

#[test]
fn triggers_fire_on_lines_and_notify_only_from_the_backend_s_own_text() {
    let (app, _dir) = app("triggers");
    let rule = |pattern: &str, action: Value| json!({"name": pattern, "pattern": pattern, "enabled": true, "action": action});
    let sets = call(
        &app,
        "trigger_set_save",
        json!({"set": {"name": "Watch", "rules": [
            rule("ERROR", json!({"kind": "highlight", "style": {"fg": "1"}})),
            rule("deploy done", json!({"kind": "notify"})),
            rule("token=\\S+", json!({"kind": "capture"})),
            rule("\\[y/N\\]", json!({"kind": "send", "text": "y", "enter": true})),
        ]}}),
    )
    .unwrap();
    let set = sets[0]["id"].as_str().unwrap().to_owned();
    call(&app, "trigger_defaults_save", json!({"ids": [set]})).unwrap();
    let prepared = call(&app, "trigger_prepare", json!({"host": null})).unwrap();
    assert_eq!(prepared["styles"].as_array().unwrap().len(), 1);
    let triggers: Vec<&str> = prepared["triggers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["action"]["kind"].as_str().unwrap())
        .collect();
    assert_eq!(triggers, ["notify", "capture", "send"]);
    let handle = prepared["handle"].clone();

    let fired = call(
        &app,
        "trigger_fire",
        json!({"handle": handle, "lines": [
            "ERROR only colours",
            "deploy done in 3s",
            "login ok, token=ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "Continue? [y/N]",
        ]}),
    )
    .unwrap();
    let fired = fired.as_array().unwrap();
    let rules: Vec<u64> = fired.iter().map(|f| f["rule"].as_u64().unwrap()).collect();
    assert_eq!(rules, [0, 1, 2]);
    // A captured line comes back with the secret hidden, as history hides it.
    let captured = fired[1]["text"].as_str().unwrap();
    assert!(
        !captured.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
        "{captured}"
    );

    // No system notification while the window has focus; then one, made of
    // the rule's name only, and not again within 30 s.
    let shown = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
    let notices = tauri::Manager::state::<crate::notices::Notices>(&app);
    let seen = std::sync::Arc::clone(&shown);
    notices.set_show(Box::new(move |title, body| {
        seen.lock().unwrap().push((title, body));
        Ok(())
    }));
    notices.set_focused(true);
    let notify = |rule: u64| {
        call(
            &app,
            "trigger_notify",
            json!({"host": null, "handle": handle, "rule": rule}),
        )
    };
    assert_eq!(notify(0), Ok(json!(false)));
    notices.set_focused(false);
    assert_eq!(notify(0), Ok(json!(true)));
    assert_eq!(notify(0), Ok(json!(false)));
    assert_eq!(
        shown.lock().unwrap().as_slice(),
        [(
            "Tauminus: deploy done".to_owned(),
            "Matched in a terminal".to_owned()
        )]
    );
    // A rule that does not notify cannot be made to.
    assert!(notify(2).is_err());
}
