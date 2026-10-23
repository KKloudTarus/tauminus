// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! What Mosh's bootstrap asks of the server and what it answers.
//!
//! `mosh-server new` starts a server for this one session and prints
//! `MOSH CONNECT <port> <key>`; the client then talks UDP to that port
//! with that key. Nothing else from the output is trusted.

use thiserror::Error;

/// A Mosh session key: 128 bits, as 22 characters of unpadded base64.
#[derive(Clone, PartialEq, Eq)]
pub struct MoshKey(String);

impl MoshKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for MoshKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key opens the session: never in logs.
        f.write_str("MoshKey(..)")
    }
}

/// Where the client connects, from the server's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoshConnect {
    pub port: u16,
    pub key: MoshKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MoshError {
    #[error("mosh-server is not installed on the host, or not on its PATH")]
    NoServer,
    #[error("the host has no UTF-8 locale for mosh-server")]
    NoUtf8Locale,
    #[error("mosh-server did not say where to connect: {0}")]
    NoAnswer(String),
}

impl MoshConnect {
    /// Reads the server's answer. The connect line must stand on its own
    /// line, with a port and a key of the shape Mosh makes; anything else
    /// in the output is ignored, and a failure says why when it can.
    pub fn parse(output: &str) -> Result<Self, MoshError> {
        for line in output.lines() {
            let mut words = line.trim_end_matches('\r').split(' ');
            if let (Some("MOSH"), Some("CONNECT"), Some(port), Some(key), None) = (
                words.next(),
                words.next(),
                words.next(),
                words.next(),
                words.next(),
            ) && let Ok(port) = port.parse::<u16>()
                && port > 0
                && key.len() == 22
                && key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
            {
                return Ok(Self {
                    port,
                    key: MoshKey(key.to_owned()),
                });
            }
        }
        if output.contains("needs a UTF-8 native locale") {
            return Err(MoshError::NoUtf8Locale);
        }
        // dash, bash, zsh, and fish each say it their way.
        if output.lines().any(|line| {
            line.contains("mosh-server")
                && (line.contains("not found") || line.contains("Unknown command"))
        }) {
            return Err(MoshError::NoServer);
        }
        Err(MoshError::NoAnswer(last_line(output)))
    }
}

/// The script that starts the server, run with `sh`. When sshd's `PATH`
/// has no `mosh-server`, it also looks in the one the login shell sets
/// (Homebrew's, for one), whatever that shell is; a login profile can take
/// a while, so it runs only then. It tries the host's own locale and each
/// UTF-8 one hosts commonly have, the host's last when it sets none. `-s`
/// binds the address the SSH connection came in on.
pub const MOSH_SERVER_SCRIPT: &str = r#"if ! command -v mosh-server >/dev/null 2>&1; then
  login=$("${SHELL:-sh}" -lc 'printf "\ntauminus-path=%s\n" "$PATH"' 2>/dev/null </dev/null |
    sed -n 's/^tauminus-path=//p' | tail -n 1)
  [ -n "$login" ] && PATH=$login:$PATH && export PATH
fi
locales='"" C.UTF-8 en_US.UTF-8'
[ -z "$LANG$LC_ALL$LC_CTYPE" ] && locales='C.UTF-8 en_US.UTF-8 ""'
eval "set -- $locales"
for locale do
  if [ -z "$locale" ]; then set --; else set -- -l "LANG=$locale" -l "LC_ALL=$locale"; fi
  out=$(mosh-server new -s -c 256 "$@" 2>&1 </dev/null)
  case "$out" in
    *"MOSH CONNECT"*) printf '%s\n' "$out"; exit 0 ;;
    *"UTF-8 native locale"*) continue ;;
    *) printf '%s\n' "$out"; exit 1 ;;
  esac
done
printf '%s\n' "$out"
exit 1
"#;

/// The last line with text, cut to 200 characters, for an error.
fn last_line(output: &str) -> String {
    let line = output
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no output");
    // A connect line that failed the checks may still hold a key.
    if line.trim_start().starts_with("MOSH CONNECT") {
        return "a malformed MOSH CONNECT line".into();
    }
    line.chars()
        .filter(|c| !c.is_control() && !is_format(*c))
        .take(200)
        .collect()
}

/// Unicode format characters (bidi overrides, zero-width marks, line and
/// paragraph separators), which would let a server reorder how its text
/// reads.
fn is_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_connect_line_is_found_among_the_rest() {
        let output = "\r\nMOSH CONNECT 60001 4tWIc8U4T0theuVzWIX0Pg\r\n\
                      mosh-server (mosh 1.4.0) [build mosh 1.4.0]\n\
                      [mosh-server detached, pid = 42]\n";
        let connect = MoshConnect::parse(output).unwrap();
        assert_eq!(connect.port, 60001);
        assert_eq!(connect.key.as_str(), "4tWIc8U4T0theuVzWIX0Pg");
        assert_eq!(format!("{:?}", connect.key), "MoshKey(..)");
    }

    #[test]
    fn anything_not_shaped_like_mosh_is_refused() {
        for bad in [
            "MOSH CONNECT 0 4tWIc8U4T0theuVzWIX0Pg",
            "MOSH CONNECT 70000 4tWIc8U4T0theuVzWIX0Pg",
            "MOSH CONNECT 60001 short",
            "MOSH CONNECT 60001 4tWIc8U4T0theuVzWIX0P;",
            "MOSH CONNECT 60001 4tWIc8U4T0theuVzWIX0Pg extra",
            "echo MOSH CONNECT 60001 4tWIc8U4T0theuVzWIX0Pg",
        ] {
            assert!(
                matches!(MoshConnect::parse(bad), Err(MoshError::NoAnswer(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_missing_server_or_locale_says_so() {
        for missing in [
            "sh: 1: exec: mosh-server: not found\n",
            "zsh: command not found: mosh-server\n",
            "bash: line 5: mosh-server: command not found\n",
            "fish: Unknown command: mosh-server\n",
        ] {
            assert_eq!(
                MoshConnect::parse(missing),
                Err(MoshError::NoServer),
                "{missing}"
            );
        }
        // Not every missing file is a missing server.
        assert!(matches!(
            MoshConnect::parse("bind: No such file or directory\n"),
            Err(MoshError::NoAnswer(_))
        ));
        assert_eq!(
            MoshConnect::parse("MOSH CONNECT 60001 4tWIc8U4T0theuVzWIX0Pg \n"),
            Err(MoshError::NoAnswer("a malformed MOSH CONNECT line".into()))
        );
        assert_eq!(
            MoshConnect::parse("mosh-server needs a UTF-8 native locale to run.\n"),
            Err(MoshError::NoUtf8Locale)
        );
        assert_eq!(
            MoshConnect::parse("bind\u{202e}: Address in use\x1b[31m\n\n"),
            Err(MoshError::NoAnswer("bind: Address in use[31m".into()))
        );
    }
}
