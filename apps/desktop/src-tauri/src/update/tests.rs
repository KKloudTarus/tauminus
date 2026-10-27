// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::shown;

#[test]
fn release_text_keeps_its_lines_and_loses_what_could_mislead() {
    assert_eq!(shown("fix\n\u{1b}[31mred\u{202e}x", 100), "fix\n[31mredx");
    assert_eq!(shown("abcdef", 3), "abc…");
    assert_eq!(shown("abc", 3), "abc");
    // Only what is kept counts toward the cut.
    assert_eq!(shown("abc\u{1b}", 3), "abc");
}

#[test]
fn a_download_comes_from_the_project_s_releases() {
    let url = |s: &str| tauri::Url::parse(s).unwrap();
    assert!(super::trusted_source(&url(
        "https://github.com/KKloudTarus/tauminus/releases/download/v1/x.AppImage"
    )));
    // Debug builds test against a local server; release builds never.
    let local = super::trusted_source(&url("http://127.0.0.1:8099/x"));
    assert_eq!(local, cfg!(debug_assertions));
}

/// The check and download the app runs, against a local server. Debug
/// builds only: release builds refuse its plain HTTP, as they should. The
/// client heeds `HTTP_PROXY`, so a proxy set without `NO_PROXY=127.0.0.1`
/// fails these tests.
#[cfg(debug_assertions)]
mod signed {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use serde_json::{Value, json};
    use tauri::Manager;
    use tauri::ipc::Channel;
    use tauri::test::{MockRuntime, mock_builder, mock_context, noop_assets};

    use super::super::{CommandError, Updates, download, update_check};

    /// A bundle and its signatures, made with the test key in
    /// `xtask/tests/fixtures`, which signs nothing anyone installs.
    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/update")
            .join(name);
        std::fs::read(path).unwrap()
    }

    fn text(name: &str) -> String {
        String::from_utf8(fixture(name)).unwrap()
    }

    /// The app's own updater settings, from `tauri.conf.json`.
    fn app_settings() -> Value {
        let conf: Value = serde_json::from_slice(
            &std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tauri.conf.json")).unwrap(),
        )
        .unwrap();
        conf["plugins"]["updater"].clone()
    }

    /// Serves `latest.json`, announcing 99.0.0 with `signature`, and `bundle`,
    /// over HTTP on 127.0.0.1 until the test process ends. Returns the
    /// `latest.json` URL.
    fn serve(bundle: Vec<u8>, signature: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let latest = json!({
            "version": "99.0.0",
            "notes": "",
            "pub_date": "2026-10-27T00:00:00Z",
            "platforms": {
                "tauminus-test": { "url": format!("{base}/bundle"), "signature": signature },
            },
        })
        .to_string()
        .into_bytes();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                    request.push(byte[0]);
                }
                let body = if request.starts_with(b"GET /latest.json ") {
                    &latest
                } else {
                    &bundle
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body);
            }
        });
        format!("{base}/latest.json")
    }

    /// Checks for an update the way the app does, with the app's updater
    /// settings except the endpoint and, unless `app_key`, the public key,
    /// then downloads it: the bytes `update_install` would install, or why
    /// there are none.
    async fn fetch(
        bundle: Vec<u8>,
        signature: &str,
        app_key: bool,
    ) -> Result<Vec<u8>, CommandError> {
        let mut settings = app_settings();
        settings["endpoints"] = json!([serve(bundle, &text(signature))]);
        if !app_key {
            settings["pubkey"] = json!(text("signer.key.pub").trim());
        }
        let mut context = mock_context(noop_assets());
        context
            .config_mut()
            .plugins
            .0
            .insert("updater".into(), settings);
        let app = mock_builder()
            .plugin(
                tauri_plugin_updater::Builder::new()
                    .target("tauminus-test")
                    .build(),
            )
            .build(context)
            .unwrap();
        app.manage(Updates::default());
        let found = update_check::<MockRuntime>(app.handle().clone(), app.state())
            .await
            .unwrap();
        assert_eq!(found.map(|view| view.version).as_deref(), Some("99.0.0"));
        let update = app.state::<Updates>().0.lock().unwrap().take().unwrap();
        download(update, Channel::new(|_| Ok(())))
            .await
            .map(|(_, bytes)| bytes)
    }

    /// Why the bundle was not handed on; a bundle that was fails the test.
    fn refusal(fetched: Result<Vec<u8>, CommandError>) -> String {
        let error = fetched.expect_err("the bundle would have installed");
        format!("{error:?}")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_bundle_installs_only_as_it_was_signed_for_its_version() {
        let bundle = fixture("bundle");
        assert_eq!(
            fetch(bundle.clone(), "bundle-99.0.0.sig", false)
                .await
                .unwrap(),
            bundle
        );

        let mut tampered = bundle.clone();
        tampered[0] ^= 1;
        let refused = refusal(fetch(tampered, "bundle-99.0.0.sig", false).await);
        assert!(
            refused.contains("signature verification failed"),
            "{refused}"
        );

        // Signed for 98.0.0, announced as 99.0.0: an older bundle passed off
        // as the new one.
        let refused = refusal(fetch(bundle.clone(), "bundle-98.0.0.sig", false).await);
        assert!(refused.contains("signed for version 98.0.0"), "{refused}");

        // `requireSignedVersion` in tauri.conf.json refuses a signature that
        // names no version.
        let refused = refusal(fetch(bundle.clone(), "bundle-unversioned.sig", false).await);
        assert!(
            refused.contains("does not specify the version"),
            "{refused}"
        );

        // The app's own key did not make the test signature.
        let refused = refusal(fetch(bundle, "bundle-99.0.0.sig", true).await);
        assert!(refused.contains("different key"), "{refused}");
    }
}
