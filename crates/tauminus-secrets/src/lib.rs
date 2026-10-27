// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Secrets read from a password manager through its own CLI, signed in
//! as the user already is (ADR 0035): `op`, `bw` and `vault`. Tauminus
//! keeps no token; a manager that is locked says so, and the user signs
//! in the way the manager asks.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tauminus_app::ports::{BoxFuture, Secret};
use tauminus_app::vault::{SecretError, SecretSource};
use tauminus_cli::Finder;
use tauminus_domain::{SecretRef, SecretSlot};
use tokio::sync::{OnceCell, Semaphore};
use zeroize::{Zeroize, Zeroizing};

/// How long a CLI may take: longer than an unlock prompt it raises.
const DEADLINE: Duration = Duration::from_secs(120);
/// Bytes of a secret read: a 16384-bit OpenSSH key is about 12 KiB.
const MAX_SECRET: usize = 64 << 10;
/// CLIs running at once: a reconnect of many panes starts a few, not one
/// each.
const PARALLEL: usize = 2;

type Fetched = Result<Secret, SecretError>;
type Reading = HashMap<(SecretRef, SecretSlot), Arc<OnceCell<Fetched>>>;

/// The password managers' CLIs.
#[derive(Debug)]
pub struct ManagerClis {
    finder: Finder,
    deadline: Duration,
    running: Semaphore,
    /// Reads under way, so callers asking for the same secret at once
    /// share one run; each leaves when its run ends, so nothing is kept.
    reading: Mutex<Reading>,
}

impl Default for ManagerClis {
    fn default() -> Self {
        Self {
            finder: Finder::new(),
            deadline: DEADLINE,
            running: Semaphore::new(PARALLEL),
            reading: Mutex::default(),
        }
    }
}

impl ManagerClis {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks for the CLIs in `dirs` alone, for tests.
    #[must_use]
    pub fn in_dirs(dirs: Vec<std::path::PathBuf>, deadline: Duration) -> Self {
        Self {
            finder: Finder::in_dirs(dirs),
            deadline,
            ..Self::default()
        }
    }

    /// `reference` read once for everyone asking at the same time.
    async fn shared(&self, reference: &SecretRef, slot: SecretSlot) -> Fetched {
        let key = (reference.clone(), slot);
        let cell = Arc::clone(
            self.reading
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(key.clone())
                .or_default(),
        );
        let fetched = cell
            .get_or_init(|| async {
                let _turn = self.running.acquire().await;
                self.read(reference, slot).await
            })
            .await
            .clone();
        let mut reading = self.reading.lock().unwrap_or_else(PoisonError::into_inner);
        if reading.get(&key).is_some_and(|c| Arc::ptr_eq(c, &cell)) {
            reading.remove(&key);
        }
        fetched
    }

    async fn read(&self, reference: &SecretRef, slot: SecretSlot) -> Result<Secret, SecretError> {
        let failed = |message: String| SecretError {
            manager: reference.manager(),
            message,
            sign_in: reference.sign_in(),
        };
        let command = reference.command(slot).map_err(|e| failed(e.to_string()))?;
        let program = self
            .finder
            .find(command.program, install(reference))
            .map_err(failed)?;
        // Bitwarden asks on a terminal for what it lacks unless told not to.
        let env: &[(&str, &str)] = match reference {
            SecretRef::Bitwarden { .. } => &[("BW_NOINTERACTION", "true")],
            _ => &[],
        };
        let args: Vec<&str> = command.args.iter().map(String::as_str).collect();
        let mut run = tauminus_cli::run(
            command.program,
            &program,
            &args,
            env,
            self.deadline,
            MAX_SECRET,
            MAX_SECRET + 64,
        )
        .await
        .map_err(failed)?;
        if !run.status.success() {
            let said = if run.error.is_empty() {
                format!("{} ended with {}", command.program, run.status)
            } else {
                run.error
            };
            return Err(failed(said));
        }
        // Moved, not copied, so the only copy is the one wiped on drop.
        let mut bytes = std::mem::take(&mut *run.stdout);
        if slot != SecretSlot::PrivateKey {
            // A line end the CLI adds is no part of a password.
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
                if bytes.last() == Some(&b'\r') {
                    bytes.pop();
                }
            }
        }
        if bytes.is_empty() {
            return Err(failed(format!("{} gave an empty secret", command.program)));
        }
        match String::from_utf8(bytes) {
            Ok(text) => Ok(Zeroizing::new(text)),
            Err(error) => {
                error.into_bytes().zeroize();
                Err(failed(format!(
                    "{} gave a secret that is not text",
                    command.program
                )))
            }
        }
    }
}

impl SecretSource for ManagerClis {
    fn fetch<'a>(
        &'a self,
        reference: &'a SecretRef,
        slot: SecretSlot,
    ) -> BoxFuture<'a, Result<Secret, SecretError>> {
        Box::pin(self.shared(reference, slot))
    }
}

fn install(reference: &SecretRef) -> &'static str {
    match reference {
        SecretRef::OnePassword { .. } => {
            "install the 1Password CLI: https://developer.1password.com/docs/cli/get-started/"
        }
        SecretRef::Bitwarden { .. } => "install the Bitwarden CLI: https://bitwarden.com/help/cli/",
        SecretRef::Vault { .. } => "install Vault: https://developer.hashicorp.com/vault/install",
    }
}

#[cfg(test)]
mod tests;
