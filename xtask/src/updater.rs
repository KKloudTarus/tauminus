// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Signs a release's update bundles and writes `latest.json` (ADR 0033),
//! in a job that builds nothing, so no build script or package ever runs
//! next to the key.
//!
//! Signatures are minisign's, as `tauri signer` writes them: the bundle's
//! BLAKE2b-512 signed with Ed25519, then that signature and a trusted
//! comment signed again. The trusted comment names the version, which the
//! app requires (`requireSignedVersion`), so an endpoint cannot pair an
//! old bundle with a new version number.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use blake2::{Blake2b256, Blake2b512, Digest};
use data_encoding::BASE64;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Map, Value, json};

const HELP: &str = "\
usage: cargo xtask updater-sign <dir> --version <x.y.z> --base-url <url> [--notes <file>] [--dry-run]

Signs every update bundle in <dir> and writes <bundle>.sig and latest.json
there. The key comes from TAURI_SIGNING_PRIVATE_KEY and
TAURI_SIGNING_PRIVATE_KEY_PASSWORD, and must be the one whose public half
is in tauri.conf.json. --dry-run signs with a key made for the run instead,
to check the files without the real key.";

/// What the app's updater looks a bundle up by, for each kind of file.
const KINDS: &[(&str, &[&str])] = &[
    (".AppImage", &["linux-x86_64", "linux-x86_64-appimage"]),
    (".deb", &["linux-x86_64-deb"]),
    (".rpm", &["linux-x86_64-rpm"]),
    ("-setup.exe", &["windows-x86_64", "windows-x86_64-nsis"]),
    (".msi", &["windows-x86_64-msi"]),
    // One universal app serves both Macs.
    (
        ".app.tar.gz",
        &[
            "darwin-aarch64",
            "darwin-x86_64",
            "darwin-aarch64-app",
            "darwin-x86_64-app",
        ],
    ),
];

/// Most bytes of release notes put in `latest.json`.
const MAX_NOTES: usize = 8000;

pub(crate) fn run(args: &[String], root: &Path) -> Result<()> {
    let mut dir = None;
    let (mut version, mut base, mut notes, mut dry) = (None, None, None, false);
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let mut value = || {
            rest.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--version" => version = Some(value()?),
            "--base-url" => base = Some(value()?),
            "--notes" => notes = Some(PathBuf::from(value()?)),
            "--dry-run" => dry = true,
            "--help" | "-h" => {
                #[expect(clippy::print_stdout, reason = "help text is the output")]
                {
                    println!("{HELP}");
                }
                return Ok(());
            }
            other if !other.starts_with('-') && dir.is_none() => dir = Some(PathBuf::from(other)),
            other => bail!("unknown argument `{other}`\n\n{HELP}"),
        }
    }
    let (Some(dir), Some(version), Some(base)) = (dir, version, base) else {
        bail!("{HELP}");
    };
    let notes = match notes {
        Some(path) => {
            std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?
        }
        None => String::new(),
    };
    let key = if dry {
        Key::ephemeral()?
    } else {
        let key = Key::from_env()?;
        key.matches(&app_public_key(root)?)?;
        key
    };
    let signed = sign_dir(&dir, &key, &version, &base, &notes, now())?;
    #[expect(clippy::print_stdout, reason = "what was signed is the output")]
    {
        for name in &signed {
            println!("signed {name}");
        }
        println!("wrote {}", dir.join("latest.json").display());
    }
    Ok(())
}

/// A minisign secret key.
pub(crate) struct Key {
    id: [u8; 8],
    signing: SigningKey,
}

impl Key {
    fn from_env() -> Result<Self> {
        let file = std::env::var("TAURI_SIGNING_PRIVATE_KEY")
            .context("TAURI_SIGNING_PRIVATE_KEY is not set")?;
        let password = std::env::var("TAURI_SIGNING_PRIVATE_KEY_PASSWORD").unwrap_or_default();
        Self::decode(&file, &password)
    }

    /// A key for one run, from the system's random source.
    fn ephemeral() -> Result<Self> {
        let mut seed = [0; 32];
        let mut id = [0; 8];
        getrandom::fill(&mut seed).map_err(|e| anyhow::anyhow!("no random source: {e}"))?;
        getrandom::fill(&mut id).map_err(|e| anyhow::anyhow!("no random source: {e}"))?;
        Ok(Self {
            id,
            signing: SigningKey::from_bytes(&seed),
        })
    }

    /// A key as `tauri signer generate` writes it: base64 of minisign's
    /// secret key file, its secret part encrypted under scrypt.
    pub(crate) fn decode(file: &str, password: &str) -> Result<Self> {
        let text = String::from_utf8(
            BASE64
                .decode(file.trim().as_bytes())
                .context("the key is not base64")?,
        )
        .context("the key is not text")?;
        let line = text.lines().nth(1).context("the key has no second line")?;
        let raw = BASE64
            .decode(line.trim().as_bytes())
            .context("the key's second line is not base64")?;
        ensure!(
            raw.len() == 158,
            "a minisign secret key is 158 bytes, not {}",
            raw.len()
        );
        ensure!(
            &raw[..6] == b"EdScB2",
            "not an Ed25519 key under scrypt and BLAKE2b"
        );
        let salt = &raw[6..38];
        let ops = u64::from_le_bytes(raw[38..46].try_into()?);
        let mem = u64::from_le_bytes(raw[46..54].try_into()?);
        let mut secret: [u8; 104] = raw[54..158].try_into()?;
        let (log_n, r, p) = scrypt_params(ops, mem);
        let params = scrypt::Params::new(log_n, r, p)
            .map_err(|e| anyhow::anyhow!("the key's scrypt limits: {e}"))?;
        let mut stream = [0; 104];
        scrypt::scrypt(password.as_bytes(), salt, &params, &mut stream)
            .map_err(|e| anyhow::anyhow!("scrypt: {e}"))?;
        for (byte, mask) in secret.iter_mut().zip(stream) {
            *byte ^= mask;
        }
        let id: [u8; 8] = secret[..8].try_into()?;
        let pair: [u8; 64] = secret[8..72].try_into()?;
        let mut sum = Blake2b256::new();
        sum.update(b"Ed");
        sum.update(id);
        sum.update(pair);
        ensure!(
            sum.finalize().as_slice() == &secret[72..104],
            "the key's password is wrong"
        );
        let signing = SigningKey::from_keypair_bytes(&pair)
            .map_err(|e| anyhow::anyhow!("the key's pair: {e}"))?;
        Ok(Self { id, signing })
    }

    /// The public half as `tauri signer generate` writes it.
    pub(crate) fn public(&self) -> String {
        let mut raw = b"Ed".to_vec();
        raw.extend_from_slice(&self.id);
        raw.extend_from_slice(self.signing.verifying_key().as_bytes());
        let id: String = self.id.iter().rev().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02X}");
            out
        });
        let text = format!(
            "untrusted comment: minisign public key: {id}\n{}\n",
            BASE64.encode(&raw)
        );
        BASE64.encode(text.as_bytes())
    }

    fn matches(&self, app: &str) -> Result<()> {
        let decode = |b64: &str| -> Result<Vec<u8>> {
            let text = String::from_utf8(BASE64.decode(b64.trim().as_bytes())?)?;
            Ok(BASE64.decode(text.lines().nth(1).unwrap_or_default().trim().as_bytes())?)
        };
        ensure!(
            decode(&self.public())? == decode(app)?,
            "the signing key is not the one whose public half tauri.conf.json holds"
        );
        Ok(())
    }

    /// The `.sig` file for `data`, as `tauri signer sign` writes it, with
    /// `version` in its trusted comment.
    pub(crate) fn sign(&self, data: &[u8], file: &str, version: &str, time: u64) -> String {
        let hash = Blake2b512::digest(data);
        let signature = self.signing.sign(&hash).to_bytes();
        let comment = format!("timestamp:{time}\tfile:{file}\tversion:{version}");
        let mut global = signature.to_vec();
        global.extend_from_slice(comment.as_bytes());
        let global = self.signing.sign(&global).to_bytes();
        let mut head = b"ED".to_vec();
        head.extend_from_slice(&self.id);
        head.extend_from_slice(&signature);
        let text = format!(
            "untrusted comment: signature from tauri secret key\n{}\ntrusted comment: {comment}\n{}\n",
            BASE64.encode(&head),
            BASE64.encode(&global)
        );
        BASE64.encode(text.as_bytes())
    }
}

/// libsodium's `pickparams`, which minisign uses to turn the key's limits
/// into scrypt's N, r and p.
fn scrypt_params(ops: u64, mem: u64) -> (u8, u32, u32) {
    let ops = ops.max(32_768);
    let r: u64 = 8;
    let log_n = |max_n: u64| {
        let mut log = 1u8;
        while log < 63 && (1u64 << log) <= max_n / 2 {
            log += 1;
        }
        log
    };
    if ops < mem / 32 {
        (log_n(ops / (r * 4)), 8, 1)
    } else {
        let n = log_n(mem / (r * 128));
        let max_rp = ((ops / 4) / (1u64 << n)).min(0x3fff_ffff);
        (n, 8, u32::try_from(max_rp / r).unwrap_or(u32::MAX))
    }
}

fn app_public_key(root: &Path) -> Result<String> {
    let path = root.join("apps/desktop/src-tauri/tauri.conf.json");
    let config: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    config["plugins"]["updater"]["pubkey"]
        .as_str()
        .map(str::to_owned)
        .context("tauri.conf.json has no plugins.updater.pubkey")
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Signs each bundle in `dir` and writes `latest.json`. Every kind must be
/// there once, named for `version`. Returns the bundles signed.
pub(crate) fn sign_dir(
    dir: &Path,
    key: &Key,
    version: &str,
    base: &str,
    notes: &str,
    time: u64,
) -> Result<Vec<String>> {
    ensure!(
        !version.is_empty()
            && version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b)),
        "a version is letters, digits and `.-+`"
    );
    ensure!(base.starts_with("https://"), "the base URL must be https");
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| {
            Path::new(name).extension().is_none_or(|ext| ext != "sig") && name != "latest.json"
        })
        .collect();
    names.sort();
    let mut platforms = Map::new();
    let mut signed = Vec::new();
    for (suffix, targets) in KINDS {
        let found: Vec<&String> = names.iter().filter(|n| n.ends_with(suffix)).collect();
        let [name] = found.as_slice() else {
            bail!(
                "expected one `*{suffix}` bundle in {}, found {}",
                dir.display(),
                found.len()
            );
        };
        ensure!(
            name.contains(version),
            "{name} is not named for version {version}"
        );
        ensure!(
            name.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b)),
            "{name} has a character a download URL would change"
        );
        let data = std::fs::read(dir.join(name)).with_context(|| format!("reading {name}"))?;
        let signature = key.sign(&data, name, version, time);
        verify(&key.public(), &data, &signature, version)?;
        std::fs::write(dir.join(format!("{name}.sig")), &signature)?;
        let url = format!("{}/{name}", base.trim_end_matches('/'));
        for target in *targets {
            platforms.insert(
                (*target).to_owned(),
                json!({ "signature": signature, "url": url }),
            );
        }
        signed.push((*name).clone());
    }
    let notes: String = notes.trim().chars().take(MAX_NOTES).collect();
    let latest = json!({ "version": version, "notes": notes, "platforms": platforms });
    std::fs::write(
        dir.join("latest.json"),
        serde_json::to_string_pretty(&latest)? + "\n",
    )?;
    Ok(signed)
}

/// Checks `signature` as the app's updater does: against `public`, and
/// naming `version`.
pub(crate) fn verify(public: &str, data: &[u8], signature: &str, version: &str) -> Result<()> {
    let text = |b64: &str| -> Result<String> {
        Ok(String::from_utf8(BASE64.decode(b64.trim().as_bytes())?)?)
    };
    let key = minisign_verify::PublicKey::decode(&text(public)?)
        .map_err(|e| anyhow::anyhow!("public key: {e}"))?;
    let sig = minisign_verify::Signature::decode(&text(signature)?)
        .map_err(|e| anyhow::anyhow!("signature: {e}"))?;
    key.verify(data, &sig, false)
        .map_err(|e| anyhow::anyhow!("the signature does not check: {e}"))?;
    ensure!(
        sig.trusted_comment()
            .split('\t')
            .any(|pair| pair == format!("version:{version}")),
        "the signature does not name version {version}"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
