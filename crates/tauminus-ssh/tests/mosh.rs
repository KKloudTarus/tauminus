// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Mosh end to end: SSH to a local sshd starts the real `mosh-server`,
//! and the real `mosh-client` carries the terminal. Needs mosh installed,
//! or `TAUMINUS_MOSH_BIN` naming a directory with both programs.

mod support;

use std::sync::Arc;
use std::time::Duration;

use support::{
    Scratch, ScriptedPrompter, Sshd, SshdOptions, endpoint, key_auth, keygen, sshd_binary, target,
};
use tauminus_app::ports::{MoshClient, Prompter, SshConnector, TerminalEvent, TerminalInput};
use tauminus_domain::{MOSH_SERVER_SCRIPT, MoshConnect, Route, TerminalSize};
use tauminus_mosh::NativeMoshClient;
use tauminus_pty::NativePtyLauncher;
use tauminus_ssh::{AgentSource, HostKeyStore, RusshConnector};

/// The directory with `mosh-server` and `mosh-client`, if mosh is here.
fn mosh_bin() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("TAUMINUS_MOSH_BIN") {
        return Some(dir.into());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find(|dir| dir.join("mosh-server").is_file())
}

#[tokio::test]
async fn a_mosh_session_starts_over_ssh_and_runs_commands() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let Some(bin) = mosh_bin() else {
        assert!(
            std::env::var_os("CI").is_none() || cfg!(windows),
            "CI needs mosh"
        );
        return;
    };
    let scratch = Scratch::new("mosh");
    keygen(&scratch.0, &["-q", "-t", "ed25519", "-N", "", "-f", "id"]);
    let sshd = Sshd::start(
        &sshd_bin,
        &[&scratch.path("id.pub")],
        None,
        SshdOptions {
            // One line: sshd takes only the first SetEnv. A server whose
            // client is gone ends within a minute, even when this test
            // fails halfway.
            extra: vec![format!(
                "SetEnv PATH={}:/usr/bin:/bin MOSH_SERVER_NETWORK_TMOUT=60",
                bin.display()
            )],
            ..SshdOptions::default()
        },
    );
    let connector = RusshConnector::new(
        HostKeyStore::new(vec![], scratch.path("known_hosts")),
        None,
        AgentSource::None,
    );
    // `localhost` is ::1 first, where this sshd does not listen: SSH falls
    // back to 127.0.0.1, and Mosh must follow it there, not ask DNS again.
    let target = target(
        endpoint("localhost", sshd.port),
        vec![],
        Route::Direct,
        key_auth(&[scratch.path("id")]),
    );
    let prompter = ScriptedPrompter::trust();
    let outcome = connector
        .run_script(
            &target,
            Arc::clone(&prompter) as Arc<dyn Prompter>,
            Arc::default(),
            MOSH_SERVER_SCRIPT,
        )
        .await
        .unwrap();
    let connect =
        MoshConnect::parse(&outcome.output).unwrap_or_else(|e| panic!("{e}: {}", outcome.output));

    let client =
        NativeMoshClient::new(Arc::new(NativePtyLauncher)).with_program(bin.join("mosh-client"));
    let size = TerminalSize::new(100, 30).unwrap();
    let mut stream = client
        .launch(outcome.peer.expect("reached over TCP"), &connect, size)
        .await
        .unwrap();
    stream
        .input
        .send(TerminalInput::Text("echo MOSH-$((6*7))\r".into()))
        .await
        .unwrap();
    // Mosh draws a screen rather than replaying a log: the answer shows up
    // somewhere in what it draws.
    let mut seen = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        while !String::from_utf8_lossy(&seen).contains("MOSH-42") {
            match stream.events.recv().await {
                Some(TerminalEvent::Output(chunk)) => seen.extend(chunk),
                other => panic!("{other:?} before the answer"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no answer in {}", String::from_utf8_lossy(&seen)));
    stream
        .input
        .send(TerminalInput::Text("exit\r".into()))
        .await
        .unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match stream.events.recv().await {
                Some(TerminalEvent::Output(_)) => {}
                other => return other,
            }
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(ended, Some(TerminalEvent::Exited { .. })),
        "{ended:?}"
    );
}
