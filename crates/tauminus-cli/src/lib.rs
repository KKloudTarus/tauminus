// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The user's own CLIs, found and run the one way Tauminus runs any:
//! cloud listings (ADR 0032) and password managers (ADR 0035).
//!
//! A CLI is looked up only in absolute folders: the absolute entries of
//! `PATH`, then where its installer puts it. It runs with a fixed
//! argument list and no shell, reads nothing, is killed when the caller
//! gives up or its deadline passes, and its output is capped. On Windows
//! an `.exe` goes before a `.cmd`, which runs through `cmd.exe`.

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;
use zeroize::Zeroizing;

/// Bytes of the error output read, for its first line.
const MAX_STDERR: u64 = 64 << 10;
/// Characters of a CLI's error line shown.
pub const MAX_ERROR: usize = 256;

/// Where CLIs are often installed, past the app's `PATH`, which a desktop
/// app started from a dock may lack. Only on Unix: on Windows a path like
/// these is relative to the current drive, whose root any user may make
/// folders in.
#[cfg(unix)]
fn known_dirs() -> Vec<PathBuf> {
    [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/opt/local/bin",
        "/snap/bin",
        "/run/current-system/sw/bin",
        "/usr/bin",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

/// The installers' own folders, under the system's program folders and
/// the user's own.
#[cfg(windows)]
fn known_dirs() -> Vec<PathBuf> {
    let under =
        |var: &str, rest: &str| std::env::var_os(var).map(|root| PathBuf::from(root).join(rest));
    [
        under("ProgramFiles", r"Amazon\AWSCLIV2"),
        under("ProgramFiles", r"Microsoft SDKs\Azure\CLI2\wbin"),
        under("ProgramFiles(x86)", r"Microsoft SDKs\Azure\CLI2\wbin"),
        under("ProgramFiles", r"Google\Cloud SDK\google-cloud-sdk\bin"),
        under(
            "ProgramFiles(x86)",
            r"Google\Cloud SDK\google-cloud-sdk\bin",
        ),
        under("LOCALAPPDATA", r"Google\Cloud SDK\google-cloud-sdk\bin"),
        under("LOCALAPPDATA", r"Programs\doctl"),
        under("ProgramFiles", "1Password CLI"),
        // Not `%ProgramData%`: any user may make folders there.
        under("LOCALAPPDATA", r"Microsoft\WinGet\Links"),
        under("APPDATA", "npm"),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Where to look for CLIs.
#[derive(Debug, Clone, Default)]
pub struct Finder {
    /// Folders to search in place of `PATH` and the usual places, for
    /// tests.
    dirs: Option<Vec<PathBuf>>,
}

impl Finder {
    /// `PATH`'s absolute entries, then the usual places.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `dirs` alone, for tests.
    #[must_use]
    pub fn in_dirs(dirs: Vec<PathBuf>) -> Self {
        Self { dirs: Some(dirs) }
    }

    /// The file `name` runs from. A relative `PATH` entry would name a
    /// file in whatever folder the app started in, so it is passed over.
    ///
    /// # Errors
    /// Not found, with `install` saying how to get it.
    pub fn find(&self, name: &str, install: &str) -> Result<PathBuf, String> {
        let dirs: Vec<PathBuf> = if let Some(dirs) = &self.dirs {
            dirs.clone()
        } else {
            let path = std::env::var_os("PATH").unwrap_or_default();
            std::env::split_paths(&path)
                .chain(known_dirs())
                .filter(|dir| dir.is_absolute())
                .collect()
        };
        // `az`, `gcloud` and npm's `bw` are batch files on Windows.
        let names: &[String] = &if cfg!(windows) {
            vec![format!("{name}.exe"), format!("{name}.cmd")]
        } else {
            vec![name.to_owned()]
        };
        dirs.iter()
            .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
            .find(|candidate| is_program(candidate))
            .ok_or_else(|| format!("{name} is not installed on this computer: {install}"))
    }
}

fn is_program(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// How one run went.
pub struct Run {
    pub status: ExitStatus,
    /// What it printed, wiped when dropped: it may be a secret.
    pub stdout: Zeroizing<Vec<u8>>,
    /// The first line of its error output, as [`shown`] keeps text.
    pub error: String,
}

impl std::fmt::Debug for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Run")
            .field("status", &self.status)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

/// Times a CLI is started again while its file is being written, as an
/// update or another thread's spawn can leave it for a moment.
const BUSY_TRIES: u32 = 5;

/// Runs `program` with `args` and `env` set on top of the app's own, and
/// reads at most `max_out` bytes of its output, within `deadline`. The
/// output buffer starts with room for `reserve` bytes: a secret's whole
/// cap and the 32 bytes the reader keeps spare, so it never moves and
/// leaves a copy behind; a listing's none, since wiping a buffer wipes all
/// its room.
///
/// # Errors
/// It did not start, printed more than `max_out`, or ran past the
/// deadline; each names `name`.
pub async fn run(
    name: &str,
    program: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    deadline: Duration,
    max_out: usize,
    reserve: usize,
) -> Result<Run, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, value) in env {
        command.env(key, value);
    }
    let mut tries = 0;
    let mut child = loop {
        match command.spawn() {
            Ok(child) => break child,
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < BUSY_TRIES => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(20 * u64::from(tries))).await;
            }
            Err(e) => return Err(format!("{name} could not start: {e}")),
        }
    };
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(format!("{name} could not start"));
    };
    let work = async {
        let mut out = Zeroizing::new(Vec::with_capacity(reserve));
        // It may echo what it was given, or what it read.
        let mut err = Zeroizing::new(Vec::new());
        let mut out_reader = (&mut stdout).take(max_out as u64 + 1);
        let mut err_reader = (&mut stderr).take(MAX_STDERR);
        let (read, _) = tokio::join!(
            out_reader.read_to_end(&mut out),
            err_reader.read_to_end(&mut err),
        );
        read.map_err(|e| e.to_string())?;
        if out.len() > max_out {
            return Err(format!("{name} printed more than Tauminus reads"));
        }
        let status = child.wait().await.map_err(|e| e.to_string())?;
        Ok::<_, String>((status, out, err))
    };
    let (status, stdout, err) = tokio::time::timeout(deadline, work)
        .await
        .map_err(|_| format!("{name} took longer than {} s", deadline.as_secs()))??;
    let error = Zeroizing::new(String::from_utf8_lossy(&err).into_owned());
    let error = error
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| shown(l, MAX_ERROR))
        .unwrap_or_default();
    Ok(Run {
        status,
        stdout,
        error,
    })
}

/// Text from a CLI as it may be shown: control characters, those that
/// change how text reads (bidi controls), and those that show nothing
/// (zero-width, fillers, tag characters) out, so it cannot pass for
/// other text; cut to `max` characters.
pub fn shown(text: &str, max: usize) -> String {
    text.chars()
        .filter(|&c| !c.is_control() && !unseen(c))
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Bidi controls, and characters that draw nothing or a blank: Unicode's
/// format characters (Cf) and the fillers that render as space.
fn unseen(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'..='\u{1160}'
            | '\u{17B4}'..='\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{2800}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

#[cfg(test)]
mod tests;
