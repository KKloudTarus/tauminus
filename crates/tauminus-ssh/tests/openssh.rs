// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The russh adapter against a real OpenSSH server. Each test starts its own
//! sshd as the current user; tests skip when sshd is not installed, except
//! in CI.

#![cfg(unix)]

mod support;

use std::sync::Arc;

use support::*;
use tauminus_app::ports::{Answer, ConnectError, Prompt, Prompter, SshConnector, TerminalInput};
use tauminus_domain::{AuthMethod, AuthOptions, Route, TerminalSize};
use tauminus_ssh::{AgentSource, HostKeyStore, RusshConnector};

struct Client {
    scratch: Scratch,
    connector: RusshConnector,
}

impl Client {
    /// A connector that reads no system `known_hosts` and remembers to scratch.
    fn new(known_hosts: &str) -> Self {
        let scratch = Scratch::new("client");
        std::fs::write(scratch.path("known_hosts"), known_hosts).unwrap();
        let store = HostKeyStore::new(
            vec![scratch.path("known_hosts")],
            scratch.path("tauminus_known_hosts"),
        );
        let connector = RusshConnector::new(store, Some(scratch.0.clone()), AgentSource::None);
        Self { scratch, connector }
    }

    fn with_agent(mut self, socket: std::path::PathBuf) -> Self {
        let store = HostKeyStore::new(
            vec![self.scratch.path("known_hosts")],
            self.scratch.path("tauminus_known_hosts"),
        );
        self.connector = RusshConnector::new(
            store,
            Some(self.scratch.0.clone()),
            AgentSource::Path(socket),
        );
        self
    }

    /// Writes a user key; returns its path.
    fn user_key(&self, name: &str, passphrase: &str) -> std::path::PathBuf {
        keygen(
            &self.scratch.0,
            &["-q", "-t", "ed25519", "-N", passphrase, "-f", name],
        );
        self.scratch.path(name)
    }
}

fn prompter(p: &Arc<ScriptedPrompter>) -> Arc<dyn Prompter> {
    Arc::clone(p) as Arc<dyn Prompter>
}

const SIZE: fn() -> TerminalSize = || TerminalSize::new(100, 30).unwrap();

#[tokio::test]
async fn key_login_asks_about_the_host_once_then_remembers_it() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let client = Client::new("");
    let key = client.user_key("id", "");
    let sshd = Sshd::start(
        &sshd_bin,
        &[&client.scratch.path("id.pub")],
        None,
        SshdOptions::default(),
    );
    let target = target(sshd.endpoint(), vec![], Route::Direct, key_auth(&[key]));

    let first = ScriptedPrompter::trust();
    let mut stream = client
        .connector
        .connect(&target, SIZE(), prompter(&first))
        .await
        .unwrap();
    let (output, code) = run_shell(&mut stream, "echo sum=$((40+2)); exit 3\n").await;
    assert!(output.contains("sum=42"), "{output}");
    assert_eq!(code, Some(3));
    assert!(
        matches!(&first.asked()[..], [Prompt::UnknownHostKey { port, .. }] if *port == sshd.port)
    );

    let second = ScriptedPrompter::new(vec![]);
    let mut stream = client
        .connector
        .connect(&target, SIZE(), prompter(&second))
        .await
        .unwrap();
    let (_, code) = run_shell(&mut stream, "exit 0\n").await;
    assert_eq!(code, Some(0));
    assert_eq!(second.asked(), Vec::<Prompt>::new(), "remembered host key");
}

#[tokio::test]
async fn changed_host_key_stops_the_connection() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let other = Scratch::new("other-host");
    keygen(&other.0, &["-q", "-t", "ed25519", "-N", "", "-f", "fake"]);
    let fake = std::fs::read_to_string(other.path("fake.pub")).unwrap();
    let probe = Client::new("");
    let key = probe.user_key("id", "");
    let sshd = Sshd::start(
        &sshd_bin,
        &[&probe.scratch.path("id.pub")],
        None,
        SshdOptions::default(),
    );
    let client = Client::new(&format!("[127.0.0.1]:{} {fake}", sshd.port));
    std::fs::copy(&key, client.scratch.path("id")).unwrap();

    let asked = ScriptedPrompter::trust();
    let target = target(
        sshd.endpoint(),
        vec![],
        Route::Direct,
        key_auth(&[client.scratch.path("id")]),
    );
    let error = client
        .connector
        .connect(&target, SIZE(), prompter(&asked))
        .await
        .err()
        .unwrap();
    assert!(
        matches!(error, ConnectError::HostKeyChanged { .. }),
        "{error:?}"
    );
    assert_eq!(asked.asked(), Vec::<Prompt>::new());
}

#[tokio::test]
async fn host_certificate_from_a_known_authority_needs_no_prompt() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let ca = Scratch::new("host-ca");
    keygen(&ca.0, &["-q", "-t", "ed25519", "-N", "", "-f", "ca"]);
    let ca_pub = std::fs::read_to_string(ca.path("ca.pub")).unwrap();
    let client = Client::new(&format!("@cert-authority [127.0.0.1]:* {ca_pub}"));
    let key = client.user_key("id", "");
    let sshd = Sshd::start(
        &sshd_bin,
        &[&client.scratch.path("id.pub")],
        None,
        SshdOptions {
            host_ca: Some(ca.path("ca")),
            ..SshdOptions::default()
        },
    );
    let silent = ScriptedPrompter::new(vec![]);
    let target = target(sshd.endpoint(), vec![], Route::Direct, key_auth(&[key]));
    let mut stream = client
        .connector
        .connect(&target, SIZE(), prompter(&silent))
        .await
        .unwrap();
    let (_, code) = run_shell(&mut stream, "exit 0\n").await;
    assert_eq!(code, Some(0));
    assert_eq!(silent.asked(), Vec::<Prompt>::new());
}

#[tokio::test]
async fn user_certificate_logs_in_without_an_authorized_key() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let client = Client::new("");
    let key = client.user_key("id", "");
    keygen(
        &client.scratch.0,
        &["-q", "-t", "ed25519", "-N", "", "-f", "user_ca"],
    );
    keygen(
        &client.scratch.0,
        &[
            "-q",
            "-s",
            "user_ca",
            "-I",
            "it-user",
            "-n",
            &current_user(),
            "id.pub",
        ],
    );
    let sshd = Sshd::start(
        &sshd_bin,
        &[],
        Some(&client.scratch.path("user_ca.pub")),
        SshdOptions::default(),
    );
    let target = target(sshd.endpoint(), vec![], Route::Direct, key_auth(&[key]));
    let mut stream = client
        .connector
        .connect(&target, SIZE(), prompter(&ScriptedPrompter::trust()))
        .await
        .unwrap();
    let (_, code) = run_shell(&mut stream, "exit 0\n").await;
    assert_eq!(code, Some(0));
}

#[tokio::test]
async fn encrypted_key_asks_for_its_passphrase() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let client = Client::new("");
    let key = client.user_key("id", "correct horse");
    let sshd = Sshd::start(
        &sshd_bin,
        &[&client.scratch.path("id.pub")],
        None,
        SshdOptions::default(),
    );
    let target = target(
        sshd.endpoint(),
        vec![],
        Route::Direct,
        key_auth(std::slice::from_ref(&key)),
    );

    let answers = ScriptedPrompter::new(vec![
        Answer::TrustHostKey { remember: true },
        Answer::Secret("wrong".to_owned().into()),
        Answer::Secret("correct horse".to_owned().into()),
    ]);
    let mut stream = client
        .connector
        .connect(&target, SIZE(), prompter(&answers))
        .await
        .unwrap();
    let (_, code) = run_shell(&mut stream, "exit 0\n").await;
    assert_eq!(code, Some(0));
    let passphrase_prompts = answers
        .asked()
        .iter()
        .filter(|p| matches!(p, Prompt::Passphrase { .. }))
        .count();
    assert_eq!(
        passphrase_prompts, 2,
        "asked again after a wrong passphrase"
    );

    let give_up = ScriptedPrompter::new(vec![Answer::Cancel]);
    let error = client
        .connector
        .connect(&target, SIZE(), prompter(&give_up))
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&error, ConnectError::AuthFailed { tried, .. } if tried == &["public key"]),
        "{error:?}"
    );
}

#[tokio::test]
async fn unauthorized_key_reports_what_was_tried() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let client = Client::new("");
    let key = client.user_key("id", "");
    let sshd = Sshd::start(&sshd_bin, &[], None, SshdOptions::default());
    let options = AuthOptions {
        methods: vec![AuthMethod::PublicKey, AuthMethod::Password],
        identity_files: vec![key],
        identities_only: true,
    };
    let target = target(sshd.endpoint(), vec![], Route::Direct, options);
    let error = client
        .connector
        .connect(&target, SIZE(), prompter(&ScriptedPrompter::trust()))
        .await
        .err()
        .unwrap();
    // The server does not accept passwords, so the user is never asked for one.
    assert!(
        matches!(&error, ConnectError::AuthFailed { tried, .. } if tried == &["public key"]),
        "{error:?}"
    );
}

#[tokio::test]
async fn agent_keys_log_in() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let client = Client::new("");
    let key = client.user_key("id", "");
    let socket = client.scratch.path("agent.sock");
    let mut agent = std::process::Command::new("ssh-agent")
        .args(["-D", "-a"])
        .arg(&socket)
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("ssh-agent starts");
    for _ in 0..200 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let added = std::process::Command::new("ssh-add")
        .arg(&key)
        .env("SSH_AUTH_SOCK", &socket)
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(added.success());
    std::fs::remove_file(&key).unwrap(); // only the agent has the key now

    let sshd = Sshd::start(
        &sshd_bin,
        &[&client.scratch.path("id.pub")],
        None,
        SshdOptions::default(),
    );
    let client = client.with_agent(socket);
    let options = AuthOptions {
        methods: vec![AuthMethod::Agent],
        identity_files: vec![],
        identities_only: false,
    };
    let target = target(sshd.endpoint(), vec![], Route::Direct, options);
    let mut stream = client
        .connector
        .connect(&target, SIZE(), prompter(&ScriptedPrompter::trust()))
        .await
        .unwrap();
    let (_, code) = run_shell(&mut stream, "exit 0\n").await;
    assert_eq!(code, Some(0));
    let _ = agent.kill();
    let _ = agent.wait();
}

#[tokio::test]
async fn proxy_jump_reaches_a_host_through_a_bastion() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let client = Client::new("");
    let key = client.user_key("id", "");
    let pub_key = client.scratch.path("id.pub");
    let bastion = Sshd::start(&sshd_bin, &[&pub_key], None, SshdOptions::default());
    let inner = Sshd::start(&sshd_bin, &[&pub_key], None, SshdOptions::default());
    let answers = ScriptedPrompter::new(vec![
        Answer::TrustHostKey { remember: true },
        Answer::TrustHostKey { remember: true },
    ]);
    let target = target(
        inner.endpoint(),
        vec![bastion.endpoint()],
        Route::Direct,
        key_auth(&[key]),
    );
    let mut stream = client
        .connector
        .connect(&target, SIZE(), prompter(&answers))
        .await
        .unwrap();
    let (output, code) = run_shell(&mut stream, "echo jumped; exit 0\n").await;
    assert!(output.contains("jumped"));
    assert_eq!(code, Some(0));
    let hosts: Vec<u16> = answers
        .asked()
        .iter()
        .filter_map(|p| match p {
            Prompt::UnknownHostKey { port, .. } => Some(*port),
            _ => None,
        })
        .collect();
    assert_eq!(
        hosts,
        [bastion.port, inner.port],
        "each hop verifies its own host key"
    );
}

#[tokio::test]
async fn proxy_command_carries_the_connection() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let client = Client::new("");
    let key = client.user_key("id", "");
    let sshd = Sshd::start(
        &sshd_bin,
        &[&client.scratch.path("id.pub")],
        None,
        SshdOptions::default(),
    );
    // bash's /dev/tcp stands in for `nc %h %p`.
    let route =
        Route::ProxyCommand("exec bash -c 'exec 3<>/dev/tcp/%h/%p; cat <&3 & exec cat >&3'".into());
    let target = target(sshd.endpoint(), vec![], route, key_auth(&[key]));
    let mut stream = client
        .connector
        .connect(&target, SIZE(), prompter(&ScriptedPrompter::trust()))
        .await
        .unwrap();
    let (output, code) = run_shell(&mut stream, "echo via-proxy-command; exit 0\n").await;
    assert!(output.contains("via-proxy-command"));
    assert_eq!(code, Some(0));
}

#[tokio::test]
async fn pty_size_resize_and_large_output() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let client = Client::new("");
    let key = client.user_key("id", "");
    let sshd = Sshd::start(
        &sshd_bin,
        &[&client.scratch.path("id.pub")],
        None,
        SshdOptions::default(),
    );
    let target = target(sshd.endpoint(), vec![], Route::Direct, key_auth(&[key]));
    let mut stream = client
        .connector
        .connect(&target, SIZE(), prompter(&ScriptedPrompter::trust()))
        .await
        .unwrap();
    stream
        .input
        .send(TerminalInput::Resize(TerminalSize::new(132, 40).unwrap()))
        .await
        .unwrap();
    let (output, code) = run_shell(
        &mut stream,
        "stty size; head -c 10485760 /dev/zero | tr '\\0' x | wc -c; exit 0\n",
    )
    .await;
    assert!(
        output.contains("40 132"),
        "resize reached the remote pty: {output}"
    );
    assert!(output.contains("10485760"), "{output}");
    assert_eq!(code, Some(0));

    // 10 MiB straight to the terminal arrives complete.
    let mut stream = client
        .connector
        .connect(&target, SIZE(), prompter(&ScriptedPrompter::new(vec![])))
        .await
        .unwrap();
    let (output, _) = run_shell(
        &mut stream,
        "head -c 10485760 /dev/zero | tr '\\0' x; exit 0\n",
    )
    .await;
    assert!(output.matches('x').count() >= 10_485_760);
}

#[tokio::test]
async fn terminate_ends_a_running_session() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let client = Client::new("");
    let key = client.user_key("id", "");
    let sshd = Sshd::start(
        &sshd_bin,
        &[&client.scratch.path("id.pub")],
        None,
        SshdOptions::default(),
    );
    let target = target(sshd.endpoint(), vec![], Route::Direct, key_auth(&[key]));
    let mut stream = client
        .connector
        .connect(&target, SIZE(), prompter(&ScriptedPrompter::trust()))
        .await
        .unwrap();
    stream
        .input
        .send(TerminalInput::Data(b"sleep 60\n".to_vec()))
        .await
        .unwrap();
    stream.terminator.terminate();
    let (_, code) = collect(&mut stream).await;
    assert_eq!(code, None);
}

/// A connected shell with the client's key authorized, for tests that need
/// a session and nothing special.
async fn session(
    sshd_bin: &std::path::Path,
) -> (Client, Sshd, tauminus_app::ports::TerminalStream) {
    let client = Client::new("");
    let key = client.user_key("id", "");
    let sshd = Sshd::start(
        sshd_bin,
        &[&client.scratch.path("id.pub")],
        None,
        SshdOptions::default(),
    );
    let target = target(sshd.endpoint(), vec![], Route::Direct, key_auth(&[key]));
    let stream = client
        .connector
        .connect(&target, SIZE(), prompter(&ScriptedPrompter::trust()))
        .await
        .unwrap();
    (client, sshd, stream)
}

/// Twice the 2 MiB window OpenSSH opens for a session channel.
const PASTE: usize = 4 * 1024 * 1024;

#[tokio::test]
async fn a_paste_larger_than_the_window_into_an_echoing_program_completes() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let (_client, _sshd, mut stream) = session(&sshd_bin).await;
    // `head` exits after the paste; raw mode keeps the pty from rewriting it.
    stream
        .input
        .send(TerminalInput::Data(
            format!("stty raw -echo; head -c {PASTE}; exit 0\n").into_bytes(),
        ))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    stream
        .input
        .send(TerminalInput::Data(vec![b'x'; PASTE]))
        .await
        .unwrap();
    let (output, code) =
        tokio::time::timeout(std::time::Duration::from_secs(30), collect(&mut stream))
            .await
            .expect("no deadlock between the paste and its echo");
    assert!(output.matches('x').count() >= PASTE, "the echo came back");
    assert_eq!(code, Some(0));
}

#[tokio::test]
async fn terminate_works_while_a_write_waits_for_the_window() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let (_client, _sshd, mut stream) = session(&sshd_bin).await;
    // Nothing reads stdin, so the paste fills the window and the write waits.
    stream
        .input
        .send(TerminalInput::Data(b"stty raw; sleep 60\n".to_vec()))
        .await
        .unwrap();
    stream
        .input
        .send(TerminalInput::Data(vec![b'x'; 8 * 1024 * 1024]))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    stream.terminator.terminate();
    let (_, code) = tokio::time::timeout(std::time::Duration::from_secs(5), collect(&mut stream))
        .await
        .expect("terminate ends the session");
    assert_eq!(code, None);
}

#[tokio::test]
async fn no_passphrase_is_asked_for_a_key_the_server_would_refuse() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let client = Client::new("");
    let refused = client.user_key("refused", "never asked");
    let accepted = client.user_key("accepted", "");
    let sshd = Sshd::start(
        &sshd_bin,
        &[&client.scratch.path("accepted.pub")],
        None,
        SshdOptions::default(),
    );
    let target = target(
        sshd.endpoint(),
        vec![],
        Route::Direct,
        key_auth(&[refused, accepted]),
    );
    let answers = ScriptedPrompter::new(vec![Answer::TrustHostKey { remember: true }]);
    let mut stream = client
        .connector
        .connect(&target, SIZE(), prompter(&answers))
        .await
        .unwrap();
    let (_, code) = run_shell(&mut stream, "exit 0\n").await;
    assert_eq!(code, Some(0));
    assert!(
        !answers
            .asked()
            .iter()
            .any(|p| matches!(p, Prompt::Passphrase { .. })),
        "{:?}",
        answers.asked()
    );
}

#[tokio::test]
async fn a_partial_success_restarts_with_what_the_server_still_wants() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let client = Client::new("");
    let first = client.user_key("first", "");
    let second = client.user_key("second", "");
    let sshd = Sshd::start(
        &sshd_bin,
        &[
            &client.scratch.path("first.pub"),
            &client.scratch.path("second.pub"),
        ],
        None,
        SshdOptions {
            // Two different keys, one after the other.
            extra: vec!["AuthenticationMethods publickey,publickey".into()],
            ..SshdOptions::default()
        },
    );
    let target = target(
        sshd.endpoint(),
        vec![],
        Route::Direct,
        key_auth(&[first, second]),
    );
    let mut stream = client
        .connector
        .connect(&target, SIZE(), prompter(&ScriptedPrompter::trust()))
        .await
        .unwrap();
    let (_, code) = run_shell(&mut stream, "exit 0\n").await;
    assert_eq!(code, Some(0));
}

/// Answers the host key prompt after `delay`.
struct SlowTrust(std::time::Duration);

impl Prompter for SlowTrust {
    fn ask(&self, _prompt: Prompt) -> tauminus_app::ports::BoxFuture<'_, Answer> {
        let delay = self.0;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            Answer::TrustHostKey { remember: true }
        })
    }
}

#[tokio::test]
async fn time_spent_reading_the_fingerprint_does_not_count_against_the_server() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let mut client = Client::new("");
    let key = client.user_key("id", "");
    client.connector = client
        .connector
        .with_connect_timeout(std::time::Duration::from_secs(1));
    let sshd = Sshd::start(
        &sshd_bin,
        &[&client.scratch.path("id.pub")],
        None,
        SshdOptions::default(),
    );
    let target = target(sshd.endpoint(), vec![], Route::Direct, key_auth(&[key]));
    let slow: Arc<dyn Prompter> = Arc::new(SlowTrust(std::time::Duration::from_secs(2)));
    let mut stream = client
        .connector
        .connect(&target, SIZE(), slow)
        .await
        .unwrap();
    let (_, code) = run_shell(&mut stream, "exit 0\n").await;
    assert_eq!(code, Some(0));
}

#[tokio::test]
async fn an_abandoned_connect_never_records_the_key_it_was_asking_about() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let client = Client::new("");
    let key = client.user_key("id", "");
    let sshd = Sshd::start(
        &sshd_bin,
        &[&client.scratch.path("id.pub")],
        None,
        SshdOptions::default(),
    );
    let target = target(sshd.endpoint(), vec![], Route::Direct, key_auth(&[key]));
    let slow: Arc<dyn Prompter> = Arc::new(SlowTrust(std::time::Duration::from_secs(1)));
    let abandoned = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        client.connector.connect(&target, SIZE(), slow),
    )
    .await;
    assert!(
        abandoned.is_err(),
        "the connect was still waiting on the prompt"
    );
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(
        !client.scratch.path("tauminus_known_hosts").exists(),
        "a late answer must not trust a key for a connection nobody uses"
    );
}

#[tokio::test]
async fn identities_only_with_no_files_uses_the_default_key_names() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let client = Client::new("");
    let dot_ssh = client.scratch.path(".ssh");
    std::fs::create_dir_all(&dot_ssh).unwrap();
    keygen(
        &dot_ssh,
        &["-q", "-t", "ed25519", "-N", "", "-f", "id_ed25519"],
    );
    let sshd = Sshd::start(
        &sshd_bin,
        &[&dot_ssh.join("id_ed25519.pub")],
        None,
        SshdOptions::default(),
    );
    let options = AuthOptions {
        methods: vec![AuthMethod::PublicKey],
        identity_files: vec![],
        identities_only: true,
    };
    let target = target(sshd.endpoint(), vec![], Route::Direct, options);
    let mut stream = client
        .connector
        .connect(&target, SIZE(), prompter(&ScriptedPrompter::trust()))
        .await
        .unwrap();
    let (_, code) = run_shell(&mut stream, "exit 0\n").await;
    assert_eq!(code, Some(0));
}

/// Starts ssh-agent on a socket in `dir`, holding the key at `key`, which is
/// then removed so only the agent has it.
async fn start_agent(
    dir: &std::path::Path,
    key: &std::path::Path,
) -> (std::process::Child, std::path::PathBuf) {
    let socket = dir.join("agent.sock");
    let agent = std::process::Command::new("ssh-agent")
        .args(["-D", "-a"])
        .arg(&socket)
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("ssh-agent starts");
    for _ in 0..200 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let added = std::process::Command::new("ssh-add")
        .arg(key)
        .env("SSH_AUTH_SOCK", &socket)
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(added.success());
    std::fs::remove_file(key).unwrap();
    (agent, socket)
}

#[tokio::test]
async fn the_agent_reaches_the_server_only_when_forwarding_is_on() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let client = Client::new("");
    let key = client.user_key("id", "");
    let (mut agent, socket) = start_agent(&client.scratch.0, &key).await;
    let sshd = Sshd::start(
        &sshd_bin,
        &[&client.scratch.path("id.pub")],
        None,
        SshdOptions::default(),
    );
    let client = client.with_agent(socket);
    let options = AuthOptions {
        methods: vec![AuthMethod::Agent],
        identity_files: vec![],
        identities_only: false,
    };
    for forward in [true, false] {
        let mut target = target(sshd.endpoint(), vec![], Route::Direct, options.clone());
        target.session.forward_agent = forward;
        let mut stream = client
            .connector
            .connect(&target, SIZE(), prompter(&ScriptedPrompter::trust()))
            .await
            .unwrap();
        // `ssh-add -l` exits 0 listing keys, and 2 with no agent to ask.
        let (output, code) = run_shell(&mut stream, "ssh-add -l; exit $?\n").await;
        if forward {
            assert_eq!(code, Some(0), "{output}");
            assert!(output.contains("ED25519"), "{output}");
        } else {
            assert_eq!(code, Some(2), "{output}");
        }
    }
    let _ = agent.kill();
    let _ = agent.wait();
}

#[tokio::test]
async fn environment_variables_reach_the_shell_the_server_accepts() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let client = Client::new("");
    let key = client.user_key("id", "");
    let sshd = Sshd::start(
        &sshd_bin,
        &[&client.scratch.path("id.pub")],
        None,
        SshdOptions {
            extra: vec!["AcceptEnv TAUMINUS_*".into()],
            ..SshdOptions::default()
        },
    );
    let mut target = target(sshd.endpoint(), vec![], Route::Direct, key_auth(&[key]));
    target.session.env = tauminus_domain::env_list(vec![
        tauminus_domain::EnvVar::parse("TAUMINUS_GREETING", "xin chào").unwrap(),
        tauminus_domain::EnvVar::parse("REFUSED_BY_SERVER", "x").unwrap(),
    ])
    .unwrap();
    let mut stream = client
        .connector
        .connect(&target, SIZE(), prompter(&ScriptedPrompter::trust()))
        .await
        .unwrap();
    let (output, code) = run_shell(
        &mut stream,
        "echo \"[$TAUMINUS_GREETING][$REFUSED_BY_SERVER]\"; exit 0\n",
    )
    .await;
    assert_eq!(code, Some(0));
    assert!(output.contains("[xin chào][]"), "{output}");
}

#[tokio::test]
async fn a_forwarded_agent_request_with_no_local_agent_fails_fast() {
    let Some(sshd_bin) = sshd_binary() else {
        return;
    };
    let client = Client::new("");
    let key = client.user_key("id", "");
    let sshd = Sshd::start(
        &sshd_bin,
        &[&client.scratch.path("id.pub")],
        None,
        SshdOptions::default(),
    );
    // Forwarding is on, but the agent socket does not exist.
    let client = client.with_agent(client_missing_agent());
    let mut target = target(sshd.endpoint(), vec![], Route::Direct, key_auth(&[key]));
    target.session.forward_agent = true;
    let mut stream = client
        .connector
        .connect(&target, SIZE(), prompter(&ScriptedPrompter::trust()))
        .await
        .unwrap();
    // The channel is closed rather than left open, so ssh-add returns.
    let (output, code) = run_shell(&mut stream, "ssh-add -l; exit $?\n").await;
    assert_ne!(code, Some(0), "{output}");
}

fn client_missing_agent() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("tauminus-no-agent-{}.sock", std::process::id()))
}
