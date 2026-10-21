// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Hosts from the user's OpenSSH config, for import.
//!
//! The aliases come from the `Host` lines of `~/.ssh/config` and the files
//! it includes. Each alias is then resolved with `ssh -G`, which is
//! OpenSSH's own answer, system config and `Match` blocks included. Without
//! an `ssh` binary, a resolver for the keywords Tauminus imports does the
//! work instead; it agrees with `ssh -G` on the test corpus but skips
//! `Match` blocks.
//!
//! `ssh -G` evaluates `Match exec` lines, which run commands from the
//! user's config, as every `ssh` invocation does.

mod parse;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tauminus_app::ports::{BoxFuture, ConfigHost, SshConfigSource};
use tokio::process::Command;
use tokio::sync::Semaphore;

/// `ssh -G` runs at once. Each takes a few milliseconds.
const PARALLEL: usize = 8;
/// One `ssh -G` should take milliseconds; a `Match exec` that hangs is cut off.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
/// The whole import: after this, aliases not resolved yet use the built-in
/// resolver. 1000 aliases at 8 in parallel would otherwise take up to 625 s.
const IMPORT_DEADLINE: Duration = Duration::from_secs(30);
/// `IdentityFile` values OpenSSH uses, and `ssh -G` prints, when none is
/// configured.
pub(crate) const DEFAULT_IDENTITIES: &[&str] = &[
    "id_rsa",
    "id_ecdsa",
    "id_ecdsa_sk",
    "id_ed25519",
    "id_ed25519_sk",
    "id_xmss",
    "id_dsa",
];

/// The user's OpenSSH client configuration.
#[derive(Debug, Clone)]
pub struct OpenSshConfig {
    file: PathBuf,
    home: PathBuf,
    local_user: String,
    ssh: Option<PathBuf>,
    /// Pass `-F file` to `ssh -G`. Off for the real config, so the system
    /// config applies as it does for the user's own `ssh`.
    explicit: bool,
}

impl OpenSshConfig {
    /// `~/.ssh/config`, resolved with the `ssh` found on `PATH`, if any.
    pub fn user(home: PathBuf, local_user: String) -> Self {
        Self {
            file: home.join(".ssh").join("config"),
            ssh: find_ssh(),
            home,
            local_user,
            explicit: false,
        }
    }

    /// `file` alone, resolved with `ssh` if given. For tests and tools.
    pub fn file(file: PathBuf, home: PathBuf, local_user: String, ssh: Option<PathBuf>) -> Self {
        Self {
            file,
            home,
            local_user,
            ssh,
            explicit: true,
        }
    }

    /// How many hosts the config names, reading files only.
    pub async fn alias_count(&self) -> usize {
        let (file, home) = (self.file.clone(), self.home.clone());
        tokio::task::spawn_blocking(move || parse::aliases(&parse::read(&file, &home)).len())
            .await
            .unwrap_or(0)
    }

    async fn read(&self) -> std::io::Result<Vec<ConfigHost>> {
        let (file, home) = (self.file.clone(), self.home.clone());
        let lines = tokio::task::spawn_blocking(move || parse::read(&file, &home))
            .await
            .map_err(std::io::Error::other)?;
        let aliases = parse::aliases(&lines);
        let Some(ssh) = &self.ssh else {
            return Ok(aliases
                .iter()
                .map(|alias| self.tidy(parse::resolve(&lines, alias)))
                .collect());
        };
        let permits = Arc::new(Semaphore::new(PARALLEL));
        let mut tasks = tokio::task::JoinSet::new();
        for (index, alias) in aliases.iter().enumerate() {
            let permits = Arc::clone(&permits);
            let mut command = Command::new(ssh);
            command.arg("-G");
            if self.explicit {
                command.arg("-F").arg(&self.file);
            }
            command
                .arg("--")
                .arg(alias)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let alias = alias.clone();
            tasks.spawn(async move {
                let _permit = permits.acquire_owned().await;
                let host = match run_ssh_g(command).await {
                    Ok(output) => parse_ssh_g(&alias, &output),
                    Err(problem) => ConfigHost {
                        alias,
                        problem: Some(problem),
                        ..ConfigHost::default()
                    },
                };
                (index, host)
            });
        }
        let mut hosts: Vec<Option<ConfigHost>> = vec![None; aliases.len()];
        let collect = async {
            while let Some(joined) = tasks.join_next().await {
                if let Ok((index, host)) = joined {
                    hosts[index] = Some(host);
                }
            }
        };
        if tokio::time::timeout(IMPORT_DEADLINE, collect)
            .await
            .is_err()
        {
            tracing::warn!("ssh -G took longer than the import allows");
        }
        // Dropping the set kills what still runs.
        drop(tasks);
        // OpenSSH's answer is the reference: an alias it could not resolve
        // is reported, never guessed by the built-in resolver.
        let hosts = hosts.into_iter().zip(&aliases).map(|(host, alias)| {
            host.unwrap_or_else(|| ConfigHost {
                alias: alias.clone(),
                problem: Some(format!(
                    "ssh -G did not answer within {} s",
                    IMPORT_DEADLINE.as_secs()
                )),
                ..ConfigHost::default()
            })
        });
        Ok(hosts.map(|h| self.tidy(h)).collect())
    }

    /// Drops what equals the default, so the saved host inherits it: the
    /// local user, port 22, and the default key list `ssh -G` prints when no
    /// `IdentityFile` is set. With `IdentitiesOnly`, listed files are kept,
    /// since dropping them would leave no key to try.
    fn tidy(&self, mut host: ConfigHost) -> ConfigHost {
        if host.user.as_deref() == Some(self.local_user.as_str()) {
            host.user = None;
        }
        if host.port == Some(22) {
            host.port = None;
        }
        let defaults_only = host.identity_files.iter().all(|f| is_default_identity(f));
        if defaults_only && !host.identities_only {
            host.identity_files.clear();
        }
        host
    }
}

impl SshConfigSource for OpenSshConfig {
    fn hosts(&self) -> BoxFuture<'_, Result<Vec<ConfigHost>, std::io::Error>> {
        Box::pin(self.read())
    }
}

fn is_default_identity(path: &Path) -> bool {
    let in_ssh_dir = path
        .parent()
        .is_some_and(|dir| dir.ends_with(".ssh") || dir == Path::new("~/.ssh"));
    in_ssh_dir
        && path
            .file_name()
            .is_some_and(|name| DEFAULT_IDENTITIES.iter().any(|d| name == *d))
}

/// Reads the keywords Tauminus imports from `ssh -G` output, one
/// `keyword value` per line.
fn parse_ssh_g(alias: &str, output: &str) -> ConfigHost {
    let mut host = ConfigHost {
        alias: alias.to_owned(),
        hostname: alias.to_owned(),
        ..ConfigHost::default()
    };
    for line in output.lines() {
        let Some((keyword, value)) = line.split_once(' ') else {
            continue;
        };
        let value = value.trim();
        match keyword {
            "hostname" => value.clone_into(&mut host.hostname),
            "user" => host.user = Some(value.to_owned()),
            "port" => host.port = value.parse().ok(),
            "proxyjump" if !value.eq_ignore_ascii_case("none") => {
                host.proxy_jump = Some(value.to_owned());
            }
            "proxycommand" if !value.eq_ignore_ascii_case("none") => {
                host.proxy_command = Some(value.to_owned());
            }
            "identityfile" => host.identity_files.push(PathBuf::from(value)),
            "identitiesonly" => host.identities_only = value.eq_ignore_ascii_case("yes"),
            _ => {}
        }
    }
    host
}

/// Runs one `ssh -G` in its own process group, so a timeout also ends what
/// a `Match exec` line started. The error is OpenSSH's first line of
/// complaint, shortened.
async fn run_ssh_g(mut command: Command) -> Result<String, String> {
    #[cfg(unix)]
    command.process_group(0);
    let child = command
        .spawn()
        .map_err(|e| format!("could not run ssh -G: {e}"))?;
    let pid = child.id();
    match tokio::time::timeout(RESOLVE_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let first = stderr
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with("Pseudo-terminal"))
                .unwrap_or("ssh -G failed");
            Err(first.chars().take(200).collect())
        }
        Ok(Err(e)) => Err(format!("could not run ssh -G: {e}")),
        Err(_) => {
            kill_group(pid);
            Err(format!(
                "ssh -G did not answer within {} s",
                RESOLVE_TIMEOUT.as_secs()
            ))
        }
    }
}

#[cfg(unix)]
fn kill_group(pid: Option<u32>) {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
        let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: Option<u32>) {
    // `kill_on_drop` ends ssh itself; Windows has no process groups to end
    // with it.
}

/// The `ssh` binary on `PATH`. Relative entries are skipped: they resolve
/// against the working directory, where anyone could plant an `ssh`.
fn find_ssh() -> Option<PathBuf> {
    let name = if cfg!(windows) { "ssh.exe" } else { "ssh" };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(test)]
mod tests;
