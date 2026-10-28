// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The shells this computer has, for a local tab to open by name.
//!
//! A local tab with no choice runs the platform default: the login shell on
//! Unix, `%ComSpec%` (`cmd.exe`) on Windows. On Unix that is the shell the
//! user picked, so only Windows lists others, found where their installers
//! put them: Windows PowerShell under the system root, PowerShell 7 under
//! Program Files or on `PATH`, Git Bash under Program Files, and the
//! distributions `wsl.exe --list` names.

use tauminus_domain::LocalShell;

/// A shell a local tab can open, under the id the app chooses it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownShell {
    /// Stable across runs: `powershell`, `pwsh`, `cmd`, `git-bash`, or
    /// `wsl:<distribution>`.
    pub id: String,
    /// What to call it in a menu or a tab.
    pub name: String,
    pub shell: LocalShell,
}

/// The shells found, and whether WSL answered. When it did not, its
/// distributions are missing from `shells`, not gone from the computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellListing {
    pub shells: Vec<KnownShell>,
    pub wsl_answered: bool,
}

/// The shells found now, in the order to offer them; none off Windows.
///
/// Blocks on file checks and on `wsl.exe --list`, for at most three
/// seconds, so async callers should run it on a blocking thread.
pub fn known_shells() -> ShellListing {
    #[cfg(windows)]
    let listing = windows_shells(&Lookup {
        var: &|name| std::env::var(name).ok(),
        exists: &|path| std::path::Path::new(path).is_file(),
        wsl: &query_wsl,
    });
    #[cfg(not(windows))]
    let listing = ShellListing {
        shells: Vec::new(),
        wsl_answered: true,
    };
    listing
}

/// How long `wsl.exe --list` may take before it is stopped.
#[cfg(windows)]
const WSL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// What the search reads, so it can be tested on any platform.
#[cfg(any(windows, test))]
struct Lookup<'a> {
    /// An environment variable, if set.
    var: &'a dyn Fn(&str) -> Option<String>,
    /// Whether a file exists at this Windows path.
    exists: &'a dyn Fn(&str) -> bool,
    /// The distributions the `wsl.exe` at this path lists, or `None` when
    /// it did not answer in time.
    wsl: &'a dyn Fn(&str) -> Option<Vec<String>>,
}

#[cfg(any(windows, test))]
fn windows_shells(lookup: &Lookup<'_>) -> ShellListing {
    // Every variable read here names a folder or a program: a relative one
    // would resolve against wherever the app was started.
    let var = |name: &str| (lookup.var)(name).filter(|value| is_absolute(value));
    let found = |path: String| (lookup.exists)(&path).then_some(path);
    let root = var("SystemRoot").unwrap_or_else(|| r"C:\Windows".to_owned());
    let system = format!(r"{}\System32", root.trim_end_matches('\\'));
    let program_files = var("ProgramFiles");
    let mut shells = Vec::new();

    if let Some(path) = found(format!(r"{system}\WindowsPowerShell\v1.0\powershell.exe")) {
        shells.push(known(
            "powershell",
            "Windows PowerShell",
            path,
            &["-NoLogo"],
        ));
    }
    let pwsh = program_files
        .as_deref()
        .and_then(|dir| found(format!(r"{dir}\PowerShell\7\pwsh.exe")))
        .or_else(|| on_path(lookup, "pwsh.exe"));
    if let Some(path) = pwsh {
        shells.push(known("pwsh", "PowerShell 7", path, &["-NoLogo"]));
    }
    // Every Windows has it, so it is offered even where the check cannot see it.
    let cmd = var("ComSpec")
        .and_then(found)
        .unwrap_or_else(|| format!(r"{system}\cmd.exe"));
    shells.push(known("cmd", "Command Prompt", cmd, &[]));
    let git_bash = [
        program_files.map(|dir| format!(r"{dir}\Git\bin\bash.exe")),
        var("LOCALAPPDATA").map(|dir| format!(r"{dir}\Programs\Git\bin\bash.exe")),
    ]
    .into_iter()
    .flatten()
    .find_map(found);
    if let Some(path) = git_bash {
        shells.push(known("git-bash", "Git Bash", path, &["--login", "-i"]));
    }
    let mut wsl_answered = true;
    if let Some(wsl) = found(format!(r"{system}\wsl.exe")) {
        let distributions = (lookup.wsl)(&wsl);
        wsl_answered = distributions.is_some();
        for distribution in distributions.into_iter().flatten() {
            shells.push(KnownShell {
                id: format!("wsl:{distribution}"),
                name: format!("{distribution} (WSL)"),
                shell: LocalShell {
                    program: Some(wsl.clone()),
                    args: vec!["-d".to_owned(), distribution],
                    ..LocalShell::default()
                },
            });
        }
    }
    ShellListing {
        shells,
        wsl_answered,
    }
}

#[cfg(any(windows, test))]
fn known(id: &str, name: &str, program: String, args: &[&str]) -> KnownShell {
    KnownShell {
        id: id.to_owned(),
        name: name.to_owned(),
        shell: LocalShell {
            program: Some(program),
            args: args.iter().map(|&arg| arg.to_owned()).collect(),
            ..LocalShell::default()
        },
    }
}

/// `exe` in the first `PATH` folder that has it. Relative and drive-relative
/// folders such as `.` or `\tools` are skipped: they resolve against
/// wherever the app was started.
#[cfg(any(windows, test))]
fn on_path(lookup: &Lookup<'_>, exe: &str) -> Option<String> {
    (lookup.var)("PATH")?
        .split(';')
        .filter(|dir| is_absolute(dir))
        .map(|dir| format!(r"{}\{exe}", dir.trim_end_matches('\\')))
        .find(|path| (lookup.exists)(path))
}

/// `C:\…` or `\\server\…`.
#[cfg(any(windows, test))]
fn is_absolute(dir: &str) -> bool {
    let bytes = dir.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    drive || dir.starts_with(r"\\")
}

/// Distribution names from `wsl.exe --list --quiet`. It writes UTF-16LE,
/// or UTF-8 when `WSL_UTF8` is set, which only newer releases honour.
/// Docker Desktop's own distributions are not shells to open, and a name
/// starting with `-` would read as an option after `-d`.
#[cfg(any(windows, test))]
fn wsl_names(stdout: &[u8]) -> Vec<String> {
    let text = if stdout.contains(&0) {
        let units: Vec<u16> = stdout
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_le_bytes(pair))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(stdout).into_owned()
    };
    text.lines()
        .map(|line| line.trim_matches(|c: char| c == '\u{feff}' || c == '\0' || c.is_whitespace()))
        .filter(|name| !name.is_empty() && !name.starts_with("docker-desktop"))
        .filter(|name| !name.starts_with('-') && !name.chars().any(char::is_control))
        .map(str::to_owned)
        .collect()
}

/// The distributions `wsl.exe` lists, or `None` when it does not answer in
/// time: a cold `LxssManager` can take longer. It is then stopped, so no
/// listing leaves a process behind. A `wsl.exe` that cannot start, or that
/// fails, as the one Windows ships before WSL is installed does, answers
/// that there are none.
#[cfg(windows)]
fn query_wsl(wsl: &str) -> Option<Vec<String>> {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    /// Keeps a console window from flashing up for `wsl.exe`.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let Ok(mut child) = Command::new(wsl)
        .args(["--list", "--quiet"])
        .env("WSL_UTF8", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    else {
        return Some(Vec::new());
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Some(Vec::new());
    };
    let (send, read) = std::sync::mpsc::sync_channel(1);
    // Ends when wsl.exe closes its output: when it exits, or is killed below.
    let reading = std::thread::Builder::new()
        .name("wsl-list".into())
        .spawn(move || {
            let mut out = Vec::new();
            let _ = stdout.read_to_end(&mut out);
            let _ = send.send(out);
        });
    let answer = match reading {
        Ok(_) => read.recv_timeout(WSL_TIMEOUT).ok(),
        Err(_) => None,
    };
    let Some(out) = answer else {
        tracing::warn!("wsl.exe --list did not answer in time; stopped it");
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let succeeded = child.wait().is_ok_and(|status| status.success());
    Some(if succeeded {
        wsl_names(&out)
    } else {
        Vec::new()
    })
}

#[cfg(test)]
mod tests;
