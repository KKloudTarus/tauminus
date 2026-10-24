// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Password and keyboard-interactive login against a real OpenSSH server
//! that checks passwords through PAM. That needs root, so the tests do not
//! start the server: `ci/password-sshd.sh` does, on a CI runner or in a
//! container, and passes `<port> <user> <password>` in
//! `TAUMINUS_PASSWORD_SSHD`. Skipped when it is unset, except in CI on Linux.

#![cfg(unix)]

mod support;

use std::sync::Arc;

use support::{Scratch, ScriptedPrompter, run_shell};
use tauminus_app::ports::{Answer, ConnectError, Prompt, Prompter, Secret, SshConnector};
use tauminus_domain::{
    AuthMethod, AuthOptions, Endpoint, HostName, Route, SshTarget, TerminalSize, UserName,
};
use tauminus_ssh::{AgentSource, HostKeyStore, RusshConnector};

struct Server {
    port: u16,
    user: String,
    password: String,
}

/// The server `ci/password-sshd.sh` started, or `None` to skip.
fn server() -> Option<Server> {
    let Ok(value) = std::env::var("TAUMINUS_PASSWORD_SSHD") else {
        assert!(
            std::env::var_os("CI").is_none() || !cfg!(target_os = "linux"),
            "CI on Linux must start ci/password-sshd.sh"
        );
        return None;
    };
    let mut parts = value.split(' ');
    let (Some(port), Some(user), Some(password), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        panic!("TAUMINUS_PASSWORD_SSHD must be `<port> <user> <password>`");
    };
    Some(Server {
        port: port.parse().expect("port is a number"),
        user: user.to_owned(),
        password: password.to_owned(),
    })
}

fn connector(scratch: &Scratch) -> RusshConnector {
    let store = HostKeyStore::new(vec![], scratch.path("known_hosts"));
    RusshConnector::new(store, None, AgentSource::None)
}

fn target(server: &Server, methods: Vec<AuthMethod>) -> SshTarget {
    let destination = Endpoint::new(
        UserName::parse(&server.user).unwrap(),
        HostName::parse("127.0.0.1").unwrap(),
        server.port,
    )
    .unwrap();
    let auth = AuthOptions {
        methods,
        identity_files: vec![],
        identities_only: true,
        identity_agent: None,
    };
    SshTarget::new(destination, vec![], Route::Direct, auth).unwrap()
}

fn secret(text: &str) -> Secret {
    text.to_owned().into()
}

fn size() -> TerminalSize {
    TerminalSize::new(80, 24).unwrap()
}

fn dyn_prompter(p: &Arc<ScriptedPrompter>) -> Arc<dyn Prompter> {
    Arc::clone(p) as Arc<dyn Prompter>
}

#[tokio::test]
async fn password_logs_in_to_openssh() {
    let Some(server) = server() else { return };
    let scratch = Scratch::new("pam-password");
    let prompter = ScriptedPrompter::new(vec![
        Answer::TrustHostKey { remember: false },
        Answer::Secret(secret(&server.password)),
    ]);
    let mut stream = connector(&scratch)
        .connect(
            &target(&server, vec![AuthMethod::Password]),
            size(),
            dyn_prompter(&prompter),
        )
        .await
        .unwrap();
    let (output, code) = run_shell(&mut stream, "echo logged-in-$((6 * 7)); exit 3\n").await;
    assert!(output.contains("logged-in-42"), "{output}");
    assert_eq!(code, Some(3));
}

#[tokio::test]
async fn keyboard_interactive_logs_in_through_pam() {
    let Some(server) = server() else { return };
    let scratch = Scratch::new("pam-kbd");
    let prompter = ScriptedPrompter::new(vec![
        Answer::TrustHostKey { remember: false },
        Answer::Responses(vec![secret(&server.password)]),
    ]);
    let mut stream = connector(&scratch)
        .connect(
            &target(&server, vec![AuthMethod::KeyboardInteractive]),
            size(),
            dyn_prompter(&prompter),
        )
        .await
        .unwrap();
    let (output, code) = run_shell(&mut stream, "echo pam-ok; exit 4\n").await;
    assert!(output.contains("pam-ok"), "{output}");
    assert_eq!(code, Some(4));
    let asked = prompter.asked();
    let Some(Prompt::KeyboardInteractive { fields, .. }) = asked.get(1) else {
        panic!("expected a keyboard-interactive prompt, got {asked:?}");
    };
    // PAM's own prompt, hidden as a password should be.
    assert_eq!(fields.len(), 1, "{fields:?}");
    assert!(fields[0].text.contains("assword"), "{fields:?}");
    assert!(!fields[0].echo);
}

#[tokio::test]
async fn wrong_passwords_are_refused_by_openssh() {
    let Some(server) = server() else { return };
    let scratch = Scratch::new("pam-wrong");
    let prompter = ScriptedPrompter::new(vec![
        Answer::TrustHostKey { remember: false },
        Answer::Secret(secret("not-it-1")),
        Answer::Secret(secret("not-it-2")),
        Answer::Secret(secret("not-it-3")),
        Answer::Secret(secret(&server.password)),
    ]);
    let error = connector(&scratch)
        .connect(
            &target(&server, vec![AuthMethod::Password]),
            size(),
            dyn_prompter(&prompter),
        )
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&error, ConnectError::AuthFailed { tried, .. } if tried == &["password"]),
        "{error:?}"
    );
}

fn stored(password: &str) -> Arc<tauminus_app::ports::Credentials> {
    Arc::new(tauminus_app::ports::Credentials {
        password: Some(secret(password)),
        ..tauminus_app::ports::Credentials::default()
    })
}

#[tokio::test]
async fn a_stored_password_signs_in_without_a_prompt() {
    let Some(server) = server() else { return };
    for method in [AuthMethod::Password, AuthMethod::KeyboardInteractive] {
        let scratch = Scratch::new("pam-stored");
        let prompter = ScriptedPrompter::new(vec![Answer::TrustHostKey { remember: false }]);
        let mut stream = connector(&scratch)
            .connect_with(
                &target(&server, vec![method]),
                size(),
                dyn_prompter(&prompter),
                stored(&server.password),
            )
            .await
            .unwrap();
        let (_, code) = run_shell(&mut stream, "exit 0\n").await;
        assert_eq!(code, Some(0));
        assert_eq!(prompter.asked().len(), 1, "{method:?}: only the host key");
    }
}

#[tokio::test]
async fn a_wrong_stored_password_is_tried_once_then_asked() {
    let Some(server) = server() else { return };
    let scratch = Scratch::new("pam-stored-wrong");
    let prompter = ScriptedPrompter::new(vec![
        Answer::TrustHostKey { remember: false },
        Answer::Secret(secret(&server.password)),
    ]);
    let mut stream = connector(&scratch)
        .connect_with(
            &target(&server, vec![AuthMethod::Password]),
            size(),
            dyn_prompter(&prompter),
            stored("not-it"),
        )
        .await
        .unwrap();
    let (_, code) = run_shell(&mut stream, "exit 0\n").await;
    assert_eq!(code, Some(0));
    assert_eq!(prompter.asked().len(), 2);
}
