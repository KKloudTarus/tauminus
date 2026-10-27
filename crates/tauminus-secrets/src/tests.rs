// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The managers' CLIs, played by small scripts that log how they were
//! called and print what the real ones do.

#![cfg(unix)]

use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::*;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("tauminus-secrets-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// A CLI named `name` that logs its arguments and `BW_NOINTERACTION`,
    /// then runs `body`.
    fn cli(&self, name: &str, body: &str) {
        let path = self.0.join(name);
        let log = self.0.join(format!("{name}.log"));
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s|' \"$@\" >> '{log}'\necho \"bw=$BW_NOINTERACTION\" >> '{log}'\n{body}\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.0.join(format!("{name}.log"))).unwrap_or_default()
    }

    fn clis(&self) -> ManagerClis {
        ManagerClis::in_dirs(vec![self.0.clone()], Duration::from_secs(10))
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn op(reference: &str) -> SecretRef {
    SecretRef::OnePassword {
        reference: reference.into(),
    }
}

const ITEM: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";

#[tokio::test]
async fn each_manager_is_asked_once_with_its_fixed_command() {
    let dir = Dir::new("ask");
    dir.cli("op", "printf 'hunter2'");
    dir.cli("bw", "printf 'bw-secret\\n'");
    dir.cli("vault", "printf 'kv-secret\\r\\n'");
    let clis = dir.clis();

    let secret = clis
        .fetch(&op("op://Ops/db/password"), SecretSlot::Password)
        .await
        .unwrap();
    assert_eq!(secret.as_str(), "hunter2");
    assert_eq!(
        dir.log("op"),
        "read|--no-newline|--|op://Ops/db/password|bw=\n"
    );

    let bitwarden = SecretRef::Bitwarden { item: ITEM.into() };
    let secret = clis
        .fetch(&bitwarden, SecretSlot::Passphrase)
        .await
        .unwrap();
    assert_eq!(secret.as_str(), "bw-secret");
    assert_eq!(
        dir.log("bw"),
        format!("get|password|{ITEM}|--nointeraction|bw=true\n")
    );

    let kv = SecretRef::Vault {
        address: "http://127.0.0.1:8200".into(),
        mount: "secret".into(),
        path: "ops/db".into(),
        field: "password".into(),
    };
    let secret = clis.fetch(&kv, SecretSlot::Password).await.unwrap();
    assert_eq!(secret.as_str(), "kv-secret");
    assert_eq!(
        dir.log("vault"),
        "kv|get|-address=http://127.0.0.1:8200|-mount=secret|-field=password|--|ops/db|bw=\n"
    );
}

#[tokio::test]
async fn a_key_keeps_its_last_line_end_and_asks_for_openssh_form() {
    let dir = Dir::new("key");
    dir.cli(
        "op",
        "printf -- '-----BEGIN OPENSSH PRIVATE KEY-----\\nAAAA\\n-----END OPENSSH PRIVATE KEY-----\\n'",
    );
    let key = dir
        .clis()
        .fetch(&op("op://Ops/web/private key"), SecretSlot::PrivateKey)
        .await
        .unwrap();
    assert!(key.ends_with("-----END OPENSSH PRIVATE KEY-----\n"));
    assert!(
        dir.log("op")
            .contains("op://Ops/web/private key?ssh-format=openssh|")
    );
}

#[tokio::test]
async fn a_locked_or_missing_manager_says_how_to_sign_in() {
    let dir = Dir::new("locked");
    dir.cli(
        "op",
        "echo '[ERROR] 2026/10/27 account is not signed in' >&2; exit 1",
    );
    dir.cli("bw", "exit 1");
    dir.cli("vault", "true");
    let clis = dir.clis();

    let error = clis
        .fetch(&op("op://Ops/db/password"), SecretSlot::Password)
        .await
        .unwrap_err();
    assert_eq!(error.manager, "1Password");
    assert_eq!(error.message, "[ERROR] 2026/10/27 account is not signed in");
    assert!(error.to_string().ends_with(
        "if it is locked, unlock the 1Password app with its CLI integration on, or run op signin"
    ));

    let error = clis
        .fetch(
            &SecretRef::Bitwarden { item: ITEM.into() },
            SecretSlot::Password,
        )
        .await
        .unwrap_err();
    assert_eq!(error.message, "bw ended with exit status: 1");

    let empty = SecretRef::Vault {
        address: "https://v.example.com".into(),
        mount: "secret".into(),
        path: "ops/db".into(),
        field: "password".into(),
    };
    let error = clis.fetch(&empty, SecretSlot::Password).await.unwrap_err();
    assert_eq!(error.message, "vault gave an empty secret");

    let nowhere = Dir::new("nowhere");
    let error = nowhere
        .clis()
        .fetch(&op("op://Ops/db/password"), SecretSlot::Password)
        .await
        .unwrap_err();
    assert!(
        error
            .message
            .starts_with("op is not installed on this computer: install the 1Password CLI")
    );
}

#[tokio::test]
async fn a_reference_that_fails_its_grammar_runs_nothing() {
    let dir = Dir::new("grammar");
    dir.cli("op", "printf 'x'");
    let error = dir
        .clis()
        .fetch(
            &op("op://Ops/db/password?attribute=otp"),
            SecretSlot::Password,
        )
        .await
        .unwrap_err();
    assert!(error.message.starts_with("a 1Password reference reads"));
    assert_eq!(dir.log("op"), "");
}

/// Against a real Vault: `TAUMINUS_LIVE_VAULT_CLI` names the folder of a
/// `vault` binary, `TAUMINUS_LIVE_VAULT_ADDR` the server, signed in
/// through `~/.vault-token`, with `ops/db` holding `password` in KV v2 at
/// `secret` and KV v1 at `kv1`, and `ops/key` an OpenSSH key.
#[tokio::test]
#[ignore = "needs a Vault server and its CLI"]
async fn a_real_vault_gives_its_secrets_through_its_cli() {
    let dir = PathBuf::from(std::env::var("TAUMINUS_LIVE_VAULT_CLI").unwrap());
    let address = std::env::var("TAUMINUS_LIVE_VAULT_ADDR").unwrap();
    let clis = ManagerClis::in_dirs(vec![dir], Duration::from_secs(30));
    let kv = |mount: &str, path: &str, field: &str| SecretRef::Vault {
        address: address.clone(),
        mount: mount.into(),
        path: path.into(),
        field: field.into(),
    };
    let v2 = clis
        .fetch(&kv("secret", "ops/db", "password"), SecretSlot::Password)
        .await
        .unwrap();
    assert_eq!(v2.as_str(), "live-kv2-pw");
    let v1 = clis
        .fetch(&kv("kv1", "ops/db", "password"), SecretSlot::Password)
        .await
        .unwrap();
    assert_eq!(v1.as_str(), "live-kv1-pw");
    let key = clis
        .fetch(
            &kv("secret", "ops/key", "private_key"),
            SecretSlot::PrivateKey,
        )
        .await
        .unwrap();
    assert!(key.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"));
    let missing = clis
        .fetch(&kv("secret", "ops/none", "password"), SecretSlot::Password)
        .await
        .unwrap_err();
    assert_eq!(
        missing.message, "No value found at secret/data/ops/none",
        "{missing}"
    );
}

#[tokio::test]
async fn panes_asking_at_once_share_one_run_and_a_later_ask_runs_again() {
    let dir = Dir::new("shared");
    // Slow enough that every ask lands while the first still runs.
    dir.cli("op", "sleep 1; printf 'hunter2'");
    let clis = dir.clis();
    let reference = op("op://Ops/db/password");
    let asks = (0..6).map(|_| clis.fetch(&reference, SecretSlot::Password));
    for secret in futures_join(asks).await {
        assert_eq!(secret.unwrap().as_str(), "hunter2");
    }
    assert_eq!(dir.log("op").lines().count(), 1, "one run for all six");
    clis.fetch(&reference, SecretSlot::Password).await.unwrap();
    assert_eq!(dir.log("op").lines().count(), 2, "nothing kept for later");
}

/// Runs the futures together.
async fn futures_join<F: Future>(futures: impl Iterator<Item = F>) -> Vec<F::Output> {
    let handles: Vec<_> = futures.map(Box::pin).collect();
    let mut out = Vec::new();
    let mut pending: Vec<_> = handles.into_iter().map(Some).collect();
    std::future::poll_fn(|cx| {
        let mut done = true;
        for slot in &mut pending {
            if let Some(future) = slot {
                match future.as_mut().poll(cx) {
                    std::task::Poll::Ready(value) => {
                        out.push(value);
                        *slot = None;
                    }
                    std::task::Poll::Pending => done = false,
                }
            }
        }
        if done {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    out
}

#[tokio::test]
async fn at_most_two_manager_clis_run_at_once() {
    let dir = Dir::new("parallel");
    let marks = dir.0.join("marks");
    dir.cli(
        "op",
        &format!(
            "echo start >> '{marks}'; sleep 0.5; echo end >> '{marks}'; printf 'x'",
            marks = marks.display()
        ),
    );
    let clis = dir.clis();
    let references: Vec<SecretRef> = (0..4)
        .map(|i| op(&format!("op://Ops/db{i}/password")))
        .collect();
    let asks = references
        .iter()
        .map(|reference| clis.fetch(reference, SecretSlot::Password));
    for secret in futures_join(asks).await {
        secret.unwrap();
    }
    let mut running = 0_i32;
    let mut most = 0;
    for mark in std::fs::read_to_string(&marks).unwrap().lines() {
        running += if mark == "start" { 1 } else { -1 };
        most = most.max(running);
    }
    assert_eq!(most, 2);
}
