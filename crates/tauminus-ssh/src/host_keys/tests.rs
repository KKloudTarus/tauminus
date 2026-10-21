// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::PathBuf;
use std::sync::Mutex;

use tauminus_app::ports::{Answer, BoxFuture, Prompt, Prompter};

use super::*;

const ED1: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFXDxFWctSi4sWGAKtsbNMmPPk7f4g0PC2U9KxVt/PCl";
const ED2: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOqN/RGiIYlFZ+d7wuTD9F9viNKB6AY8w5porFrBDf5f";
const EC1: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBMDnrWYlMcR8tUU3pU+QhCJgREyuT18iV60JrMP0q4biF+VG0WTBuiy1EDVc0yL5cNbDkb+pQ+yYtUhyL+ZjZnc=";

fn key(text: &str) -> PublicKey {
    PublicKey::from_openssh(text).unwrap()
}

#[derive(Default)]
struct ScriptedPrompter {
    answers: Mutex<Vec<Answer>>,
    asked: Mutex<Vec<Prompt>>,
}

impl ScriptedPrompter {
    fn answering(answers: Vec<Answer>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers),
            asked: Mutex::default(),
        })
    }

    fn asked(&self) -> Vec<Prompt> {
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

/// A scratch directory with a store that reads `system` and remembers to `own`.
struct Fixture {
    dir: PathBuf,
    store: HostKeyStore,
}

/// Gives every fixture its own directory, even for tests sharing a name.
static NEXT_FIXTURE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl Fixture {
    fn new(name: &str, system: &str) -> Self {
        let n = NEXT_FIXTURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("tauminus-hk-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("system_known_hosts"), system).unwrap();
        let store = HostKeyStore::new(
            vec![dir.join("system_known_hosts")],
            dir.join("own/known_hosts"),
        );
        Self { dir, store }
    }

    fn own(&self) -> PathBuf {
        self.dir.join("own/known_hosts")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn check_with(
    store: &HostKeyStore,
    host: &str,
    port: u16,
    presented: &Presented<'_>,
    prompter: &Arc<dyn Prompter>,
) -> Result<(), ConnectError> {
    let known = store.load().await?;
    store.check(&known, host, port, presented, prompter).await
}

fn as_dyn(prompter: &Arc<ScriptedPrompter>) -> Arc<dyn Prompter> {
    Arc::clone(prompter) as Arc<dyn Prompter>
}

#[tokio::test]
async fn unknown_key_asks_and_remembers_when_told_to() {
    let fixture = Fixture::new("remember", "");
    let prompter = ScriptedPrompter::answering(vec![Answer::TrustHostKey { remember: true }]);
    let k = key(ED1);
    check_with(
        &fixture.store,
        "db.example.com",
        22,
        &Presented::Key(&k),
        &as_dyn(&prompter),
    )
    .await
    .unwrap();

    let asked = prompter.asked();
    assert!(matches!(
        &asked[..],
        [Prompt::UnknownHostKey { host, port: 22, algorithm, fingerprint, certificate_expected: false }]
            if host == "db.example.com" && algorithm == "ssh-ed25519" && fingerprint.starts_with("SHA256:")
    ));
    let written = std::fs::read_to_string(fixture.own()).unwrap();
    assert!(written.starts_with("|1|"), "remembered hashed: {written}");
    assert!(!written.contains("db.example.com"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(fixture.own())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    // Second connect: known, no prompt.
    let silent = ScriptedPrompter::answering(vec![]);
    check_with(
        &fixture.store,
        "db.example.com",
        22,
        &Presented::Key(&k),
        &as_dyn(&silent),
    )
    .await
    .unwrap();
    assert_eq!(silent.asked(), Vec::<Prompt>::new());
}

#[tokio::test]
async fn trusting_once_does_not_write_anything() {
    let fixture = Fixture::new("once", "");
    let prompter = ScriptedPrompter::answering(vec![Answer::TrustHostKey { remember: false }]);
    let k = key(ED1);
    check_with(
        &fixture.store,
        "db",
        22,
        &Presented::Key(&k),
        &as_dyn(&prompter),
    )
    .await
    .unwrap();
    assert!(!fixture.own().exists());
}

#[tokio::test]
async fn declining_rejects_the_connection() {
    let fixture = Fixture::new("decline", "");
    let prompter = ScriptedPrompter::answering(vec![Answer::Cancel]);
    let k = key(ED1);
    let result = check_with(
        &fixture.store,
        "db",
        22,
        &Presented::Key(&k),
        &as_dyn(&prompter),
    )
    .await;
    assert!(matches!(result, Err(ConnectError::HostKeyRejected { .. })));
}

#[tokio::test]
async fn changed_key_is_refused_without_asking() {
    let fixture = Fixture::new("changed", &format!("db {ED1}\n"));
    let prompter = ScriptedPrompter::answering(vec![Answer::TrustHostKey { remember: true }]);
    let k = key(ED2);
    let result = check_with(
        &fixture.store,
        "db",
        22,
        &Presented::Key(&k),
        &as_dyn(&prompter),
    )
    .await;
    match result {
        Err(ConnectError::HostKeyChanged {
            recorded,
            fingerprint,
            ..
        }) => {
            assert!(recorded.ends_with("system_known_hosts:1"), "{recorded}");
            assert!(fingerprint.starts_with("SHA256:"));
        }
        other => panic!("expected HostKeyChanged, got {other:?}"),
    }
    assert!(
        prompter.asked().is_empty(),
        "a changed key is never offered for trust"
    );
}

#[tokio::test]
async fn revoked_key_is_refused_without_asking() {
    let fixture = Fixture::new("revoked", &format!("@revoked * {ED1}\n"));
    let prompter = ScriptedPrompter::answering(vec![Answer::TrustHostKey { remember: true }]);
    let k = key(ED1);
    let result = check_with(
        &fixture.store,
        "db",
        22,
        &Presented::Key(&k),
        &as_dyn(&prompter),
    )
    .await;
    assert!(matches!(result, Err(ConnectError::HostKeyRevoked { .. })));
    assert_eq!(prompter.asked(), Vec::<Prompt>::new());
}

#[tokio::test]
async fn a_key_of_another_type_is_refused_as_changed() {
    // OpenSSH treats it as HOST_CHANGED; a prompt here would let an
    // interceptor in just by offering a type that is not on record.
    let fixture = Fixture::new("other", &format!("db {EC1}\n"));
    let prompter = ScriptedPrompter::answering(vec![Answer::TrustHostKey { remember: true }]);
    let k = key(ED1);
    let result = check_with(
        &fixture.store,
        "db",
        22,
        &Presented::Key(&k),
        &as_dyn(&prompter),
    )
    .await;
    assert!(
        matches!(result, Err(ConnectError::HostKeyChanged { .. })),
        "{result:?}"
    );
    assert_eq!(prompter.asked(), Vec::<Prompt>::new());
}

#[tokio::test]
async fn a_plain_key_where_a_certificate_was_expected_is_flagged() {
    let fixture = Fixture::new("expected", &format!("@cert-authority db {EC1}\n"));
    let prompter = ScriptedPrompter::answering(vec![Answer::Cancel]);
    let k = key(ED1);
    let _ = check_with(
        &fixture.store,
        "db",
        22,
        &Presented::Key(&k),
        &as_dyn(&prompter),
    )
    .await;
    assert!(matches!(
        &prompter.asked()[..],
        [Prompt::UnknownHostKey {
            certificate_expected: true,
            ..
        }]
    ));
}

#[tokio::test]
async fn an_unreadable_file_fails_closed() {
    let fixture = Fixture::new("unreadable", "");
    // A directory where a file should be: reading it fails, and skipping it
    // would turn a changed key into a new one.
    std::fs::remove_file(fixture.dir.join("system_known_hosts")).unwrap();
    std::fs::create_dir(fixture.dir.join("system_known_hosts")).unwrap();
    let prompter = ScriptedPrompter::answering(vec![Answer::TrustHostKey { remember: true }]);
    let k = key(ED1);
    let result = check_with(
        &fixture.store,
        "db",
        22,
        &Presented::Key(&k),
        &as_dyn(&prompter),
    )
    .await;
    assert!(
        matches!(&result, Err(ConnectError::Io(e)) if e.to_string().contains("system_known_hosts")),
        "{result:?}"
    );
    assert_eq!(prompter.asked(), Vec::<Prompt>::new());
}

#[tokio::test]
async fn a_line_that_is_not_utf8_hides_only_itself() {
    let fixture = Fixture::new("latin1", "");
    let mut contents = b"# r\xe9seau\n".to_vec();
    contents.extend_from_slice(format!("db {ED1}\n").as_bytes());
    std::fs::write(fixture.dir.join("system_known_hosts"), contents).unwrap();
    let prompter = ScriptedPrompter::answering(vec![]);
    let k = key(ED2);
    let result = check_with(
        &fixture.store,
        "db",
        22,
        &Presented::Key(&k),
        &as_dyn(&prompter),
    )
    .await;
    assert!(
        matches!(result, Err(ConnectError::HostKeyChanged { .. })),
        "{result:?}"
    );
}

/// Host certificates signed with `ssh-keygen -s`. Skipped when ssh-keygen is
/// missing, except in CI.
mod certificates {
    use super::*;

    struct Certs {
        dir: PathBuf,
        ca_pub: String,
    }

    impl Certs {
        fn make(name: &str) -> Option<Self> {
            let dir =
                std::env::temp_dir().join(format!("tauminus-cert-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            for file in ["ca", "host"] {
                if !run(&dir, &["-q", "-t", "ed25519", "-N", "", "-f", file]) {
                    assert!(
                        std::env::var_os("CI").is_none(),
                        "ssh-keygen must be installed in CI"
                    );
                    return None;
                }
            }
            let ca_pub = std::fs::read_to_string(dir.join("ca.pub")).unwrap();
            Some(Self { dir, ca_pub })
        }

        /// Signs the host key; `extra` adds ssh-keygen flags.
        fn sign(&self, principals: &str, extra: &[&str]) -> Certificate {
            let _ = std::fs::remove_file(self.dir.join("host-cert.pub"));
            let mut args = vec!["-q", "-s", "ca", "-I", "test", "-n", principals];
            args.extend_from_slice(extra);
            args.push("host.pub");
            assert!(run(&self.dir, &args));
            Certificate::from_openssh(
                &std::fs::read_to_string(self.dir.join("host-cert.pub")).unwrap(),
            )
            .unwrap()
        }

        fn host_key(&self) -> PublicKey {
            PublicKey::from_openssh(&std::fs::read_to_string(self.dir.join("host.pub")).unwrap())
                .unwrap()
        }
    }

    impl Drop for Certs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn run(dir: &Path, args: &[&str]) -> bool {
        std::process::Command::new("ssh-keygen")
            .args(args)
            .current_dir(dir)
            .stderr(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    async fn check(
        known: &str,
        host: &str,
        cert: &Certificate,
    ) -> (Result<(), ConnectError>, Vec<Prompt>) {
        let fixture = Fixture::new(&format!("cert-{host}"), known);
        let prompter = ScriptedPrompter::answering(vec![Answer::Cancel]);
        let result = check_with(
            &fixture.store,
            host,
            22,
            &Presented::Certificate(cert),
            &as_dyn(&prompter),
        )
        .await;
        (result, prompter.asked())
    }

    #[tokio::test]
    async fn valid_host_certificate_is_trusted_without_asking() {
        let Some(certs) = Certs::make("valid") else {
            return;
        };
        let cert = certs.sign("db.example.com", &["-h"]);
        let (result, asked) = check(
            &format!("@cert-authority *.example.com {}", certs.ca_pub),
            "db.example.com",
            &cert,
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(asked, Vec::<Prompt>::new());
    }

    #[tokio::test]
    async fn certificates_that_fail_a_check_fall_back_to_the_plain_key() {
        let Some(certs) = Certs::make("fallback") else {
            return;
        };
        let authority = format!("@cert-authority *.example.com {}", certs.ca_pub);
        let cases = [
            (
                "wrong principal",
                certs.sign("other.example.com", &["-h"]),
                authority.clone(),
            ),
            (
                "user certificate",
                certs.sign("db.example.com", &[]),
                authority.clone(),
            ),
            (
                "expired",
                certs.sign("db.example.com", &["-h", "-V", "20200101:20200102"]),
                authority.clone(),
            ),
            (
                "no authority for host",
                certs.sign("db.example.com", &["-h"]),
                format!("@cert-authority *.other.net {}", certs.ca_pub),
            ),
        ];
        for (label, cert, known) in cases {
            let (result, asked) = check(&known, "db.example.com", &cert).await;
            assert!(
                matches!(result, Err(ConnectError::HostKeyRejected { .. })),
                "{label}: {result:?}"
            );
            assert_eq!(
                asked.len(),
                1,
                "{label}: falls back to asking about the plain key"
            );
        }
    }

    #[tokio::test]
    async fn a_revoked_authority_or_certified_key_refuses_without_asking() {
        let Some(certs) = Certs::make("revoked") else {
            return;
        };
        let authority = format!("@cert-authority *.example.com {}", certs.ca_pub);
        let host_key = certs.host_key().to_openssh().unwrap();
        let cert = certs.sign("db.example.com", &["-h"]);
        for (label, known) in [
            (
                "authority",
                format!("{authority}\n@revoked * {}", certs.ca_pub),
            ),
            (
                "certified key",
                format!("{authority}\n@revoked * {host_key}"),
            ),
        ] {
            let (result, asked) = check(&known, "db.example.com", &cert).await;
            assert!(
                matches!(result, Err(ConnectError::HostKeyRevoked { .. })),
                "{label}: {result:?}"
            );
            assert_eq!(asked, Vec::<Prompt>::new(), "{label}");
        }
    }

    #[tokio::test]
    async fn fallback_uses_the_certified_key_on_record() {
        let Some(certs) = Certs::make("recorded") else {
            return;
        };
        let cert = certs.sign("other.example.com", &["-h"]);
        let recorded = certs.host_key().to_openssh().unwrap();
        let (result, asked) = check(
            &format!("db.example.com {recorded}"),
            "db.example.com",
            &cert,
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(asked, Vec::<Prompt>::new());
    }
}
