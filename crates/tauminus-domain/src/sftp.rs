// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Rules for moving files over SFTP that need no server: which names a
//! server sends may become local files, how remote paths join, and when a
//! transfer may resume.

use thiserror::Error;

/// Longest name component written locally, in bytes; most file systems
/// allow 255.
pub const MAX_NAME: usize = 255;

/// Bytes compared at the end of both copies before a resume.
pub const TAIL_CHECK: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NameError {
    #[error("the name is empty")]
    Empty,
    #[error("{0:?} is not a file name")]
    Dots(String),
    #[error("the name contains {0:?}, which a local file name cannot")]
    Character(char),
    #[error("the name is longer than {MAX_NAME} bytes")]
    TooLong,
    #[error("{0:?} names a device on Windows")]
    Device(String),
    #[error("a name on Windows cannot end with a dot or a space")]
    Trailing,
    #[error("names ending in {0} are kept for partial copies")]
    Reserved(&'static str),
}

/// Suffixes of a partial copy and its record: a server must not be able to
/// plant either through a download.
const RESERVED_SUFFIXES: [&str; 2] = [".tauminus-part", ".tauminus-part.info"];

/// Which local rules apply on top of the common ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalOs {
    Unix,
    Windows,
}

impl LocalOs {
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// Characters that reorder text: a name like `\u{202E}txt.exe` shows as
/// `exe.txt`.
fn reorders(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Checks a name a server sent before it becomes one component of a local
/// path. Refuses what could leave the chosen folder (`..`, separators),
/// what hides the real name (control and reordering characters), and on
/// Windows what the OS treats specially. O(n) in the name's length.
pub fn local_name(name: &str, os: LocalOs) -> Result<&str, NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name == "." || name == ".." {
        return Err(NameError::Dots(name.to_owned()));
    }
    if name.len() > MAX_NAME {
        return Err(NameError::TooLong);
    }
    // Case-folded: on macOS and Windows `X.TAUMINUS-PART` is the same file.
    let folded = name.to_ascii_lowercase();
    if let Some(suffix) = RESERVED_SUFFIXES.iter().find(|s| folded.ends_with(*s)) {
        return Err(NameError::Reserved(suffix));
    }
    let windows = os == LocalOs::Windows;
    if let Some(c) = name.chars().find(|&c| {
        c == '/'
            || c == '\\'
            || c.is_control()
            || reorders(c)
            || (windows && matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    }) {
        return Err(NameError::Character(c));
    }
    if windows {
        if name.ends_with('.') || name.ends_with(' ') {
            return Err(NameError::Trailing);
        }
        // `CON`, `con.txt`, and `COM1.tar.gz` all open the device; so do
        // COM and LPT with a superscript ¹ ² ³.
        let stem = name.split('.').next().unwrap_or(name).trim_end();
        let upper = stem.to_ascii_uppercase();
        let numbered = |prefix: &str| {
            upper.strip_prefix(prefix).is_some_and(|rest| {
                matches!(
                    rest,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        };
        let device = matches!(
            upper.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || numbered("COM")
            || numbered("LPT");
        if device {
            return Err(NameError::Device(name.to_owned()));
        }
    }
    Ok(name)
}

/// `dir` and `name` as one remote path. SFTP paths use `/` whatever the
/// server's OS.
pub fn remote_join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// The parent of a remote path, or `None` at the root. `/a/b` is `/a`,
/// `/a` is `/`.
pub fn remote_parent(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.rfind('/') {
        Some(0) => Some("/"),
        Some(at) => Some(&trimmed[..at]),
        None => Some("."),
    }
}

/// What a partial copy was made from, saved beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Which saved host, and which file on it.
    pub host: String,
    pub path: String,
    pub size: u64,
    /// Seconds since the epoch, as SFTP reports it.
    pub modified: u64,
}

/// How a transfer begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// From the first byte, replacing any partial copy.
    Fresh,
    /// From `offset`, once the last [`TAIL_CHECK`] bytes before it match
    /// on both sides.
    Resume { offset: u64 },
}

/// Decides whether a partial copy of `partial` bytes, recorded as made
/// from `recorded`, may continue against the source as it is `now`. It
/// may only when the record names the same file with the same size and
/// time, and the copy is shorter than the source: anything else could
/// splice two versions into one file that looks complete.
pub fn plan(recorded: Option<&Source>, now: &Source, partial: u64) -> Start {
    match recorded {
        Some(recorded) if recorded == now && partial > 0 && partial < now.size => {
            Start::Resume { offset: partial }
        }
        _ => Start::Fresh,
    }
}

/// Where the tail comparison starts for a resume at `offset`.
pub fn tail_start(offset: u64) -> u64 {
    offset.saturating_sub(TAIL_CHECK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_names_pass_everywhere() {
        for name in [
            "notes.txt",
            ".bashrc",
            "a b",
            "Máy chủ.log",
            "x".repeat(255).as_str(),
        ] {
            assert_eq!(local_name(name, LocalOs::Unix), Ok(name));
            assert_eq!(local_name(name, LocalOs::Windows), Ok(name));
        }
        // A colon is fine on Unix, where backups often carry one.
        assert!(local_name("backup-10:00", LocalOs::Unix).is_ok());
    }

    #[test]
    fn names_that_leave_the_folder_or_hide_are_refused() {
        for (name, os) in [
            ("", LocalOs::Unix),
            (".", LocalOs::Unix),
            ("..", LocalOs::Unix),
            ("../.bashrc", LocalOs::Unix),
            ("a/b", LocalOs::Unix),
            ("a\\b", LocalOs::Unix),
            ("a\0b", LocalOs::Unix),
            ("a\nb", LocalOs::Unix),
            ("a\u{7f}", LocalOs::Unix),
            ("a\u{85}", LocalOs::Unix),
            ("\u{202E}txt.exe", LocalOs::Unix),
            ("a\u{2066}b", LocalOs::Unix),
            ("C:x", LocalOs::Windows),
            ("file:stream", LocalOs::Windows),
            ("a*b", LocalOs::Windows),
            ("trailing.", LocalOs::Windows),
            ("trailing ", LocalOs::Windows),
            ("CON", LocalOs::Windows),
            ("con.txt", LocalOs::Windows),
            ("Com1.tar.gz", LocalOs::Windows),
            ("lpt9", LocalOs::Windows),
            ("nul .txt", LocalOs::Windows),
            ("COM¹.txt", LocalOs::Windows),
            ("lpt³", LocalOs::Windows),
            ("CONOUT$", LocalOs::Windows),
            ("conin$.log", LocalOs::Windows),
            ("x.tauminus-part", LocalOs::Unix),
            ("x.tauminus-part.info", LocalOs::Unix),
            ("x.TAUMINUS-PART", LocalOs::Windows),
        ] {
            assert!(local_name(name, os).is_err(), "{name:?} on {os:?}");
        }
        assert_eq!(
            local_name(&"x".repeat(256), LocalOs::Unix),
            Err(NameError::TooLong)
        );
        // Not devices: COM0, COM10, CONSOLE.
        for name in ["COM0", "COM10", "console", "aux1"] {
            assert!(local_name(name, LocalOs::Windows).is_ok(), "{name}");
        }
    }

    #[test]
    fn remote_paths_join_and_split_with_slashes() {
        assert_eq!(remote_join("/srv", "a"), "/srv/a");
        assert_eq!(remote_join("/", "a"), "/a");
        assert_eq!(remote_join("", "a"), "a");
        assert_eq!(remote_parent("/srv/app/"), Some("/srv"));
        assert_eq!(remote_parent("/srv"), Some("/"));
        assert_eq!(remote_parent("/"), None);
        assert_eq!(remote_parent("logs"), Some("."));
    }

    fn source(size: u64, modified: u64) -> Source {
        Source {
            host: "db".into(),
            path: "/var/db.img".into(),
            size,
            modified,
        }
    }

    #[test]
    fn a_resume_needs_the_same_file_unchanged_and_unfinished() {
        let now = source(1_000, 7);
        assert_eq!(plan(Some(&now), &now, 400), Start::Resume { offset: 400 });
        assert_eq!(plan(None, &now, 400), Start::Fresh, "no record");
        assert_eq!(
            plan(Some(&source(1_000, 8)), &now, 400),
            Start::Fresh,
            "touched"
        );
        assert_eq!(
            plan(Some(&source(999, 7)), &now, 400),
            Start::Fresh,
            "resized"
        );
        let mut elsewhere = now.clone();
        elsewhere.host = "web".into();
        assert_eq!(
            plan(Some(&elsewhere), &now, 400),
            Start::Fresh,
            "another host"
        );
        assert_eq!(plan(Some(&now), &now, 0), Start::Fresh, "nothing yet");
        assert_eq!(plan(Some(&now), &now, 1_000), Start::Fresh, "already whole");
        assert_eq!(
            plan(Some(&now), &now, 2_000),
            Start::Fresh,
            "longer than the source"
        );
    }

    #[test]
    fn the_tail_check_starts_before_the_offset() {
        assert_eq!(tail_start(10), 0);
        assert_eq!(tail_start(TAIL_CHECK + 5), 5);
    }
}
