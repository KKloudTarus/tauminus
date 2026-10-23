// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Mosh's own client, `mosh-client`, run on a PTY as the `mosh` script
//! runs it: the server's address and port as arguments, its key in
//! `MOSH_KEY`. Mosh's protocol stays upstream's; this only finds the
//! program and starts it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauminus_app::ports::{BoxFuture, LaunchError, MoshClient, PtyLauncher, TerminalStream};
use tauminus_domain::{LocalShell, MoshConnect, TerminalSize};

/// Where package managers put `mosh-client` when a GUI app's `PATH` does
/// not reach: Homebrew on Apple silicon and Intel, `MacPorts`, Nix.
const KNOWN_DIRS: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/opt/local/bin",
    "/run/current-system/sw/bin",
    "/usr/bin",
];

pub struct NativeMoshClient {
    launcher: Arc<dyn PtyLauncher>,
    /// A `mosh-client` to use instead of looking for one.
    program: Option<PathBuf>,
}

impl std::fmt::Debug for NativeMoshClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeMoshClient")
            .field("program", &self.program)
            .finish_non_exhaustive()
    }
}

impl NativeMoshClient {
    pub fn new(launcher: Arc<dyn PtyLauncher>) -> Self {
        Self {
            launcher,
            program: None,
        }
    }

    /// Runs `program` as the client, for tests.
    #[must_use]
    pub fn with_program(mut self, program: PathBuf) -> Self {
        self.program = Some(program);
        self
    }

    fn program(&self) -> Result<PathBuf, LaunchError> {
        if let Some(program) = &self.program {
            return Ok(program.clone());
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        // Relative entries (".", or an empty one) would name a file in
        // whatever directory the app was started from.
        let dirs = std::env::split_paths(&path)
            .filter(|dir| dir.is_absolute())
            .chain(KNOWN_DIRS.iter().map(PathBuf::from));
        find("mosh-client", dirs)
            .ok_or_else(|| LaunchError::Spawn {
                program: "mosh-client".into(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "not installed on this computer: install Mosh (brew install mosh, apt install mosh)",
                ),
            })
    }
}

/// The first `name` in `dirs` that is a file.
fn find(name: &str, dirs: impl Iterator<Item = PathBuf>) -> Option<PathBuf> {
    dirs.map(|dir| dir.join(name))
        .find(|candidate| is_program(candidate))
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

/// `mosh-client` refuses to start without a UTF-8 locale. One that names
/// UTF-8 is kept; otherwise the client gets the one every system has.
fn locale(vars: impl Fn(&str) -> Option<String>) -> Option<(String, String)> {
    let current = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|name| vars(name).filter(|v| !v.is_empty()));
    let utf8 = current.as_deref().is_some_and(|v| {
        let v = v.to_ascii_lowercase();
        v.contains("utf-8") || v.contains("utf8")
    });
    if utf8 {
        return None;
    }
    let fallback = if cfg!(target_os = "macos") {
        "en_US.UTF-8"
    } else {
        "C.UTF-8"
    };
    Some(("LC_ALL".into(), fallback.into()))
}

impl MoshClient for NativeMoshClient {
    fn ready(&self) -> Result<(), LaunchError> {
        if cfg!(windows) {
            return Err(LaunchError::Spawn {
                program: "mosh-client".into(),
                source: std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "Mosh runs on macOS and Linux",
                ),
            });
        }
        self.program().map(drop)
    }

    fn launch<'a>(
        &'a self,
        address: std::net::IpAddr,
        connect: &'a MoshConnect,
        size: TerminalSize,
    ) -> BoxFuture<'a, Result<TerminalStream, LaunchError>> {
        let launcher = Arc::clone(&self.launcher);
        let program = self.program();
        let mut env = vec![("MOSH_KEY".to_owned(), connect.key.as_str().to_owned())];
        env.extend(locale(|name| std::env::var(name).ok()));
        let args = vec![address.to_string(), connect.port.to_string()];
        Box::pin(async move {
            let shell = LocalShell {
                program: Some(program?.to_string_lossy().into_owned()),
                args,
                cwd: None,
                env,
            };
            // A fork of the whole app: not on an async worker.
            tokio::task::spawn_blocking(move || launcher.launch(&shell, size))
                .await
                .map_err(|e| LaunchError::Io(std::io::Error::other(e)))?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_utf8_locale_is_kept_and_any_other_replaced() {
        let with = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(locale(with(&[("LANG", "en_US.UTF-8")])), None);
        assert_eq!(locale(with(&[("LC_ALL", "de_DE.utf8")])), None);
        let fixed = locale(with(&[("LANG", "C")])).unwrap();
        assert_eq!(fixed.0, "LC_ALL");
        assert!(fixed.1.ends_with("UTF-8"));
        assert!(locale(with(&[])).is_some());
        // LC_ALL wins over LANG, as it does for the C library.
        assert!(locale(with(&[("LC_ALL", "POSIX"), ("LANG", "en_US.UTF-8")])).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn the_client_is_found_in_the_first_dir_that_has_it() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("tauminus-mosh-find-{}", std::process::id()));
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("mosh-client"), "not a program").unwrap();
        std::fs::write(b.join("mosh-client"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(
            b.join("mosh-client"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let found = find("mosh-client", [a, b.clone()].into_iter());
        assert_eq!(found, Some(b.join("mosh-client")));
        assert_eq!(find("mosh-client", [root.join("none")].into_iter()), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
