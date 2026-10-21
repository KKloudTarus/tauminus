// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! A throwaway OpenSSH server for integration tests, run as the current user
//! on a free loopback port.

#![allow(dead_code, reason = "each test binary uses a different subset")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tauminus_app::ports::{
    Answer, BoxFuture, Prompt, Prompter, TerminalEvent, TerminalInput, TerminalStream,
};
use tauminus_domain::{AuthMethod, AuthOptions, Endpoint, HostName, Route, SshTarget, UserName};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A fresh directory under the system temp dir, removed on drop.
pub(crate) struct Scratch(pub(crate) PathBuf);

impl Scratch {
    pub(crate) fn new(name: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("tauminus-it-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn keygen(dir: &Path, args: &[&str]) {
    let status = Command::new("ssh-keygen")
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("ssh-keygen runs");
    assert!(status.success(), "ssh-keygen {args:?} failed");
}

/// The sshd binary, or `None` to skip. CI must have it.
pub(crate) fn sshd_binary() -> Option<PathBuf> {
    let candidates = [
        "/usr/sbin/sshd",
        "/usr/local/sbin/sshd",
        "/opt/homebrew/sbin/sshd",
    ];
    let found = candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.exists());
    if found.is_none() {
        assert!(
            std::env::var_os("CI").is_none(),
            "sshd must be installed in CI"
        );
    }
    found
}

pub(crate) fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .expect("USER is set")
}

#[derive(Default)]
pub(crate) struct SshdOptions {
    /// Extra `sshd_config` lines.
    pub(crate) extra: Vec<String>,
    /// Sign a host certificate with this CA private key file.
    pub(crate) host_ca: Option<PathBuf>,
}

pub(crate) struct Sshd {
    child: Child,
    pub(crate) port: u16,
    pub(crate) dir: Scratch,
}

impl Sshd {
    /// Starts sshd with a fresh ed25519 host key that accepts `authorized`
    /// public keys and certificates from `user_ca` if given.
    pub(crate) fn start(
        binary: &Path,
        authorized: &[&Path],
        user_ca: Option<&Path>,
        options: SshdOptions,
    ) -> Self {
        let dir = Scratch::new("sshd");
        keygen(&dir.0, &["-q", "-t", "ed25519", "-N", "", "-f", "host"]);
        let mut authorized_keys = String::new();
        for key in authorized {
            authorized_keys.push_str(&std::fs::read_to_string(key).unwrap());
        }
        std::fs::write(dir.path("authorized_keys"), authorized_keys).unwrap();
        let port = free_port();
        let mut config = vec![
            format!("Port {port}"),
            "ListenAddress 127.0.0.1".into(),
            format!("HostKey {}", dir.path("host").display()),
            format!("PidFile {}", dir.path("pid").display()),
            format!(
                "AuthorizedKeysFile {}",
                dir.path("authorized_keys").display()
            ),
            "StrictModes no".into(),
            "UsePAM no".into(),
            "PasswordAuthentication no".into(),
            "KbdInteractiveAuthentication no".into(),
            "AllowTcpForwarding yes".into(),
            "LogLevel ERROR".into(),
        ];
        if let Some(ca) = user_ca {
            config.push(format!("TrustedUserCAKeys {}", ca.display()));
        }
        if let Some(ca) = &options.host_ca {
            keygen(
                &dir.0,
                &[
                    "-q",
                    "-s",
                    ca.to_str().unwrap(),
                    "-I",
                    "it-host",
                    "-h",
                    "-n",
                    "127.0.0.1",
                    "host.pub",
                ],
            );
            config.push(format!(
                "HostCertificate {}",
                dir.path("host-cert.pub").display()
            ));
        }
        config.extend(options.extra);
        std::fs::write(dir.path("sshd_config"), config.join("\n") + "\n").unwrap();
        let child = Command::new(binary)
            .args(["-D", "-e", "-f"])
            .arg(dir.path("sshd_config"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sshd starts");
        wait_for_port(port);
        Self { child, port, dir }
    }

    pub(crate) fn host_public_key(&self) -> String {
        std::fs::read_to_string(self.dir.path("host.pub")).unwrap()
    }

    pub(crate) fn endpoint(&self) -> Endpoint {
        endpoint("127.0.0.1", self.port)
    }
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(crate) fn endpoint(host: &str, port: u16) -> Endpoint {
    Endpoint::new(
        UserName::parse(&current_user()).unwrap(),
        HostName::parse(host).unwrap(),
        port,
    )
    .unwrap()
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn wait_for_port(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "test setup polls a child process"
        )]
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("sshd did not listen on port {port}");
}

/// Public-key-only auth with the given identity files.
pub(crate) fn key_auth(files: &[PathBuf]) -> AuthOptions {
    AuthOptions {
        methods: vec![AuthMethod::PublicKey],
        identity_files: files.to_vec(),
        identities_only: true,
    }
}

pub(crate) fn target(
    destination: Endpoint,
    jumps: Vec<Endpoint>,
    route: Route,
    auth: AuthOptions,
) -> SshTarget {
    SshTarget::new(destination, jumps, route, auth).unwrap()
}

/// Answers prompts from a script and records what was asked.
pub(crate) struct ScriptedPrompter {
    answers: Mutex<Vec<Answer>>,
    asked: Mutex<Vec<Prompt>>,
}

impl ScriptedPrompter {
    /// `answers` are given in the order the prompts will come.
    pub(crate) fn new(mut answers: Vec<Answer>) -> std::sync::Arc<Self> {
        answers.reverse();
        std::sync::Arc::new(Self {
            answers: Mutex::new(answers),
            asked: Mutex::default(),
        })
    }

    pub(crate) fn trust() -> std::sync::Arc<Self> {
        Self::new(vec![Answer::TrustHostKey { remember: true }])
    }

    pub(crate) fn asked(&self) -> Vec<Prompt> {
        self.asked.lock().unwrap().clone()
    }
}

impl Prompter for ScriptedPrompter {
    fn ask(&self, prompt: Prompt) -> BoxFuture<'_, Answer> {
        self.asked.lock().unwrap().push(prompt);
        let answer = self.answers.lock().unwrap().pop().unwrap_or(Answer::Cancel);
        Box::pin(async move { answer })
    }
}

/// Sends `input` to the shell and collects output until it exits.
pub(crate) async fn run_shell(stream: &mut TerminalStream, input: &str) -> (String, Option<u32>) {
    stream
        .input
        .send(TerminalInput::Data(input.as_bytes().to_vec()))
        .await
        .unwrap();
    collect(stream).await
}

pub(crate) async fn collect(stream: &mut TerminalStream) -> (String, Option<u32>) {
    let mut output = Vec::new();
    let code = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match stream.events.recv().await {
                Some(TerminalEvent::Output(chunk)) => output.extend(chunk),
                Some(TerminalEvent::Exited { code }) => return code,
                None => panic!("stream closed before exit"),
            }
        }
    })
    .await
    .expect("shell exits in time");
    (String::from_utf8_lossy(&output).into_owned(), code)
}
