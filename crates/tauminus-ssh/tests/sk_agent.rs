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
use tauminus_app::ports::{Answer, ConnectError, Prompter, SshConnector};
use tauminus_app::vault::Credentials;
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
    logs_in(key, vec![AuthMethod::Agent]).await;
}

/// Signs in to the test server as set up: `home` holds default key files,
/// `options` the methods and identity files, `credentials` a vault
/// identity's.
async fn connect(
    home: Option<PathBuf>,
    options: AuthOptions,
    credentials: Credentials,
) -> Result<tauminus_app::ports::TerminalStream, ConnectError> {
    let setup = setup();
    let scratch = Scratch::new("sk-connect");
    let connector = RusshConnector::new(
        HostKeyStore::new(vec![], scratch.path("known_hosts")),
        home,
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
        options,
    )
    .unwrap();
    let prompter = ScriptedPrompter::new(vec![Answer::TrustHostKey { remember: false }]);
    connector
        .connect_with(
            &target,
            TerminalSize::new(80, 24).unwrap(),
            Arc::clone(&prompter) as Arc<dyn Prompter>,
            Arc::new(credentials),
        )
        .await
}

/// Logs in with `methods`, `key` the only identity file.
async fn logs_in(key: &str, methods: Vec<AuthMethod>) {
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
            methods,
            identity_files: vec![setup.keys.join(key)],
            identities_only: true,
            identity_agent: None,
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

#[tokio::test]
#[ignore = "needs an sshd and an agent holding sk- keys: TAUMINUS_SK_SSHD"]
async fn an_sk_key_file_tried_first_leaves_the_agent_to_sign() {
    // The key file holds no secret: only the security key can sign. With
    // none to ask, the file's key is not offered, and the agent signs.
    logs_in(
        "id_ed25519-sk",
        vec![AuthMethod::PublicKey, AuthMethod::Agent],
    )
    .await;
}

#[tokio::test]
#[ignore = "needs an sshd and an agent holding sk- keys: TAUMINUS_SK_SSHD"]
async fn a_default_sk_key_in_home_lets_the_agent_sign_with_identities_only() {
    // `IdentitiesOnly` with no identity file: the default names count, the
    // `_sk` ones among them, as OpenSSH's do.
    let setup = setup();
    let home = Scratch::new("sk-home");
    let ssh = home.path(".ssh");
    std::fs::create_dir_all(&ssh).unwrap();
    std::fs::copy(setup.keys.join("id_ed25519-sk"), ssh.join("id_ed25519_sk")).unwrap();
    let mut stream = connect(
        Some(home.path("")),
        AuthOptions {
            methods: vec![AuthMethod::Agent],
            identity_files: vec![],
            identities_only: true,
            identity_agent: None,
        },
        Credentials::default(),
    )
    .await
    .unwrap();
    let (output, _) = run_shell(&mut stream, "echo home-$((6*7)); exit 0\n").await;
    assert!(output.contains("home-42"), "{output}");
}

#[tokio::test]
#[ignore = "needs an sshd and an agent holding sk- keys: TAUMINUS_SK_SSHD"]
async fn a_vault_identity_s_sk_key_leaves_the_agent_to_sign() {
    let setup = setup();
    let pem = std::fs::read_to_string(setup.keys.join("id_ecdsa-sk")).unwrap();
    let mut stream = connect(
        None,
        AuthOptions {
            methods: vec![AuthMethod::Agent],
            identity_files: vec![],
            identities_only: false,
            identity_agent: None,
        },
        Credentials {
            private_key: Some(pem.into()),
            ..Credentials::default()
        },
    )
    .await
    .unwrap();
    let (output, _) = run_shell(&mut stream, "echo vault-$((6*7)); exit 0\n").await;
    assert!(output.contains("vault-42"), "{output}");
}

#[tokio::test]
#[ignore = "needs an sshd and an agent holding sk- keys: TAUMINUS_SK_SSHD"]
async fn with_no_agent_the_failure_names_the_fido2_key() {
    let setup = setup();
    let error = connect(
        None,
        AuthOptions {
            methods: vec![AuthMethod::PublicKey],
            identity_files: vec![setup.keys.join("id_ed25519-sk")],
            identities_only: true,
            identity_agent: None,
        },
        Credentials::default(),
    )
    .await
    .map(|_| ())
    .unwrap_err();
    assert!(error.to_string().contains("FIDO2"), "{error}");
}
