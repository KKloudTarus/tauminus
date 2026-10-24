// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! FIDO2 keys (`sk-ssh-ed25519@openssh.com`, `sk-ecdsa-sha2-nistp256@
//! openssh.com`) through an ssh-agent, against a real OpenSSH server.
//! The agent signs; a security key would be touched there, not here.
//! Without a key at hand, OpenSSH's own test provider (`sk-dummy.so`)
//! stands in. Needs `TAUMINUS_SK_SSHD=<port> <user> <agent socket> <key
//! dir>`, the key dir holding `id_ed25519-sk` and `id_ecdsa-sk`; ignored
//! unless asked for.

#![cfg(unix)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use support::{Scratch, ScriptedPrompter, run_shell};
use tauminus_app::ports::{Answer, Prompter, SshConnector};
use tauminus_domain::{
    AuthMethod, AuthOptions, Endpoint, HostName, Route, SshTarget, TerminalSize, UserName,
};
use tauminus_ssh::{AgentSource, HostKeyStore, RusshConnector};

struct Setup {
    port: u16,
    user: String,
    agent: PathBuf,
    keys: PathBuf,
}

fn setup() -> Setup {
    let value = std::env::var("TAUMINUS_SK_SSHD").expect("TAUMINUS_SK_SSHD");
    let parts: Vec<&str> = value.split(' ').collect();
    let [port, user, agent, keys] = parts[..] else {
        panic!("TAUMINUS_SK_SSHD must be `<port> <user> <agent socket> <key dir>`");
    };
    Setup {
        port: port.parse().unwrap(),
        user: user.to_owned(),
        agent: agent.into(),
        keys: keys.into(),
    }
}

/// Logs in with the agent's key that `key` names, and nothing else.
async fn logs_in_with(key: &str) {
    let setup = setup();
    let scratch = Scratch::new(key);
    let connector = RusshConnector::new(
        HostKeyStore::new(vec![], scratch.path("known_hosts")),
        None,
        AgentSource::Path(setup.agent.clone()),
    );
    let target = SshTarget::new(
        Endpoint::new(
            UserName::parse(&setup.user).unwrap(),
            HostName::parse("127.0.0.1").unwrap(),
            setup.port,
        )
        .unwrap(),
        vec![],
        Route::Direct,
        AuthOptions {
            methods: vec![AuthMethod::Agent],
            identity_files: vec![setup.keys.join(key)],
            identities_only: true,
        },
    )
    .unwrap();
    let prompter = ScriptedPrompter::new(vec![Answer::TrustHostKey { remember: false }]);
    let mut stream = connector
        .connect(
            &target,
            TerminalSize::new(80, 24).unwrap(),
            Arc::clone(&prompter) as Arc<dyn Prompter>,
        )
        .await
        .unwrap_or_else(|e| panic!("{key}: {e}"));
    let (output, code) = run_shell(&mut stream, "echo signed-in-$((6*7)); exit 0\n").await;
    assert!(output.contains("signed-in-42"), "{key}: {output}");
    assert_eq!(code, Some(0));
}

#[tokio::test]
#[ignore = "needs an sshd and an agent holding sk- keys: TAUMINUS_SK_SSHD"]
async fn an_ed25519_sk_key_in_the_agent_logs_in() {
    logs_in_with("id_ed25519-sk").await;
}

#[tokio::test]
#[ignore = "needs an sshd and an agent holding sk- keys: TAUMINUS_SK_SSHD"]
async fn an_ecdsa_sk_key_in_the_agent_logs_in() {
    logs_in_with("id_ecdsa-sk").await;
}
