// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Deciding whether to trust a server's host key or host certificate.
//!
//! Tauminus reads the user's and the system's `known_hosts` files and writes
//! only to its own file, so it never edits `~/.ssh`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use russh::keys::ssh_key::certificate::CertType;
use russh::keys::ssh_key::{Certificate, HashAlg, PublicKey};
use tauminus_app::ports::{Answer, ConnectError, Prompt, Prompter};
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

use crate::known_hosts::{self, KnownHosts, Verdict};

/// Where host keys are read from and remembered to.
#[derive(Debug, Clone)]
pub struct HostKeyStore {
    read_from: Vec<PathBuf>,
    remember_in: PathBuf,
}

impl HostKeyStore {
    /// Reads `read_from` in order (missing files are fine) and appends newly
    /// trusted keys to `remember_in`, which is also read.
    pub fn new(read_from: Vec<PathBuf>, remember_in: PathBuf) -> Self {
        Self {
            read_from,
            remember_in,
        }
    }

    /// The OpenSSH locations: `~/.ssh/known_hosts`, `~/.ssh/known_hosts2`,
    /// and the system-wide files.
    pub fn openssh_files(home: Option<&Path>) -> Vec<PathBuf> {
        let mut files = Vec::new();
        if let Some(home) = home {
            files.push(home.join(".ssh/known_hosts"));
            files.push(home.join(".ssh/known_hosts2"));
        }
        if cfg!(unix) {
            files.push("/etc/ssh/ssh_known_hosts".into());
            files.push("/etc/ssh/ssh_known_hosts2".into());
        }
        files
    }

    /// Reads every file and parses them off the async workers: a large file
    /// costs tens of milliseconds to parse. Called once per hop, so a key
    /// remembered on one hop is seen by the next. O(total size).
    ///
    /// A file that exists but cannot be read fails the connect: skipping it
    /// would turn a changed key into a new one.
    pub(crate) async fn load(&self) -> Result<Arc<KnownHosts>, ConnectError> {
        let mut contents = Vec::new();
        for file in self
            .read_from
            .iter()
            .chain(std::iter::once(&self.remember_in))
        {
            match tokio::fs::read(file).await {
                Ok(bytes) => contents.push((file.clone(), bytes)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(ConnectError::Io(std::io::Error::new(
                        e.kind(),
                        format!("cannot read {}: {e}", file.display()),
                    )));
                }
            }
        }
        let parse = move || {
            let mut known = KnownHosts::default();
            for (file, bytes) in &contents {
                for skipped in known.extend(bytes, file) {
                    warn!(at = %skipped.location, reason = skipped.reason, "skipping known_hosts line");
                }
            }
            known
        };
        tokio::task::spawn_blocking(parse)
            .await
            .map(Arc::new)
            .map_err(|e| ConnectError::Io(std::io::Error::other(e)))
    }

    /// Appends a hashed line for `key`. The file is created private to the
    /// user (0600 on Unix).
    async fn remember(&self, host: &str, port: u16, key: &PublicKey) -> std::io::Result<()> {
        if let Some(parent) = self.remember_in.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let line = known_hosts::hashed_line(host, port, key)?;
        let mut options = tokio::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&self.remember_in).await?;
        file.write_all(format!("{line}\n").as_bytes()).await?;
        file.flush().await
    }

    /// Decides whether to continue with `presented`, given `known` from
    /// [`Self::load`], asking the user only when nothing about the key is on
    /// record.
    pub(crate) async fn check(
        &self,
        known: &KnownHosts,
        host: &str,
        port: u16,
        presented: &Presented<'_>,
        prompter: &Arc<dyn Prompter>,
    ) -> Result<(), ConnectError> {
        let revoked = |recorded: &known_hosts::Location| ConnectError::HostKeyRevoked {
            host: host.to_owned(),
            port,
            recorded: recorded.to_string(),
        };
        let key = match presented {
            Presented::Certificate(cert) => {
                match trusted_certificate(known, host, port, cert, unix_now()) {
                    Ok(()) => return Ok(()),
                    Err(CertRejection::Revoked(recorded)) => return Err(revoked(recorded)),
                    Err(CertRejection::Untrusted(reason)) => {
                        // OpenSSH falls back to the plain key when a
                        // certificate is not trusted; so do we.
                        debug!(
                            host,
                            port, reason, "host certificate not trusted; checking its key"
                        );
                        PublicKey::from(cert.public_key().clone())
                    }
                }
            }
            Presented::Key(key) => (*key).clone(),
        };
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        match known.verify(host, port, &key) {
            Verdict::Known(_) => return Ok(()),
            Verdict::Changed { recorded } => {
                return Err(ConnectError::HostKeyChanged {
                    host: host.to_owned(),
                    port,
                    fingerprint,
                    recorded: recorded.to_string(),
                });
            }
            Verdict::Revoked(recorded) => return Err(revoked(&recorded)),
            Verdict::Unknown => {}
        }
        let answer = prompter
            .ask(Prompt::UnknownHostKey {
                host: host.to_owned(),
                port,
                algorithm: key.algorithm().to_string(),
                fingerprint,
                // An authority covers this host, yet it sent no certificate
                // that checks out: what an interceptor without the CA's key
                // would do.
                certificate_expected: !known.authorities_for(host, port).is_empty(),
            })
            .await;
        match answer {
            Answer::TrustHostKey { remember } => {
                if remember && let Err(e) = self.remember(host, port, &key).await {
                    warn!(error = %e, "could not remember the host key; continuing");
                }
                Ok(())
            }
            _ => Err(ConnectError::HostKeyRejected {
                host: host.to_owned(),
                port,
            }),
        }
    }
}

/// What the server presented during key exchange.
#[derive(Debug)]
pub(crate) enum Presented<'a> {
    Key(&'a PublicKey),
    Certificate(&'a Certificate),
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Why a host certificate was not accepted.
#[derive(Debug)]
enum CertRejection<'a> {
    /// Its key or its authority is `@revoked`. The connection must stop;
    /// falling back to the plain key would turn a revocation into a prompt.
    Revoked(&'a known_hosts::Location),
    /// Anything else. The plain key inside is checked instead, as OpenSSH does.
    Untrusted(&'static str),
}

/// A host certificate is trusted when neither its key nor its authority is
/// revoked, a `@cert-authority` for this host signed it, it is in its
/// validity window, it is a host certificate, it names this host, and it
/// carries no critical options. Empty principals are rejected, stricter than
/// OpenSSH.
fn trusted_certificate<'a>(
    known: &'a KnownHosts,
    host: &str,
    port: u16,
    cert: &Certificate,
    now: u64,
) -> Result<(), CertRejection<'a>> {
    let certified = PublicKey::from(cert.public_key().clone());
    if let Some(recorded) = known.revocation(host, port, &certified) {
        return Err(CertRejection::Revoked(recorded));
    }
    let authorities = known.authorities_for(host, port);
    if authorities.is_empty() {
        return Err(CertRejection::Untrusted("no @cert-authority for this host"));
    }
    let fingerprints: Vec<_> = authorities
        .iter()
        .map(|ca| ca.fingerprint(HashAlg::Sha256))
        .collect();
    cert.validate_at(now, &fingerprints).map_err(|_| {
        CertRejection::Untrusted("signature, validity window, or authority does not check out")
    })?;
    let ca = PublicKey::from(cert.signature_key().clone());
    if let Some(recorded) = known.revocation(host, port, &ca) {
        return Err(CertRejection::Revoked(recorded));
    }
    if cert.cert_type() != CertType::Host {
        return Err(CertRejection::Untrusted("not a host certificate"));
    }
    if !cert.critical_options().is_empty() {
        return Err(CertRejection::Untrusted(
            "host certificates must not carry critical options",
        ));
    }
    let host = host.to_ascii_lowercase();
    if !cert
        .valid_principals()
        .iter()
        .any(|principal| principal.to_ascii_lowercase() == host)
    {
        return Err(CertRejection::Untrusted(
            "certificate does not name this host",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
