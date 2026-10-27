// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::{HashMap, HashSet};

use super::*;

/// A Windows machine: its environment, its files, and what WSL lists, with
/// `None` for a WSL that does not answer in time.
struct Machine {
    vars: HashMap<&'static str, &'static str>,
    files: HashSet<&'static str>,
    distributions: Option<Vec<&'static str>>,
}

impl Machine {
    fn new(vars: &[(&'static str, &'static str)], files: &[&'static str]) -> Self {
        Self {
            vars: vars.iter().copied().collect(),
            files: files.iter().copied().collect(),
            distributions: Some(Vec::new()),
        }
    }

    fn listing(&self) -> ShellListing {
        windows_shells(&Lookup {
            var: &|name| self.vars.get(name).map(|&value| value.to_owned()),
            exists: &|path| self.files.contains(path),
            wsl: &|path| {
                assert_eq!(path, r"C:\Windows\System32\wsl.exe");
                self.distributions
                    .as_ref()
                    .map(|names| names.iter().map(|&d| d.to_owned()).collect())
            },
        })
    }

    fn shells(&self) -> Vec<KnownShell> {
        self.listing().shells
    }
}

const VARS: &[(&str, &str)] = &[
    ("SystemRoot", r"C:\Windows"),
    ("ProgramFiles", r"C:\Program Files"),
    ("LOCALAPPDATA", r"C:\Users\me\AppData\Local"),
    ("ComSpec", r"C:\Windows\system32\cmd.exe"),
];

fn ids(shells: &[KnownShell]) -> Vec<&str> {
    shells.iter().map(|s| s.id.as_str()).collect()
}

fn find<'a>(shells: &'a [KnownShell], id: &str) -> &'a KnownShell {
    shells.iter().find(|s| s.id == id).expect("listed")
}

#[test]
fn finds_each_shell_where_its_installer_puts_it() {
    let mut machine = Machine::new(
        VARS,
        &[
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            r"C:\Windows\system32\cmd.exe",
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Windows\System32\wsl.exe",
        ],
    );
    machine.distributions = Some(vec!["Ubuntu", "Debian"]);
    let shells = machine.shells();

    assert_eq!(
        ids(&shells),
        [
            "powershell",
            "pwsh",
            "cmd",
            "git-bash",
            "wsl:Ubuntu",
            "wsl:Debian"
        ]
    );
    let powershell = find(&shells, "powershell");
    assert_eq!(powershell.name, "Windows PowerShell");
    assert_eq!(
        powershell.shell.program.as_deref(),
        Some(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe")
    );
    assert_eq!(powershell.shell.args, ["-NoLogo"]);
    assert_eq!(
        find(&shells, "cmd").shell.program.as_deref(),
        Some(r"C:\Windows\system32\cmd.exe")
    );
    assert_eq!(find(&shells, "git-bash").shell.args, ["--login", "-i"]);
    let ubuntu = find(&shells, "wsl:Ubuntu");
    assert_eq!(ubuntu.name, "Ubuntu (WSL)");
    assert_eq!(
        ubuntu.shell.program.as_deref(),
        Some(r"C:\Windows\System32\wsl.exe")
    );
    assert_eq!(ubuntu.shell.args, ["-d", "Ubuntu"]);
}

#[test]
fn offers_only_the_command_prompt_where_nothing_else_is_installed() {
    // No wsl.exe: `Machine::shells` would fail its assertion if WSL were asked.
    let shells = Machine::new(&[("SystemRoot", r"C:\Windows")], &[]).shells();
    assert_eq!(ids(&shells), ["cmd"]);
    assert_eq!(
        shells[0].shell.program.as_deref(),
        Some(r"C:\Windows\System32\cmd.exe")
    );
}

#[test]
fn finds_powershell_7_on_path_and_skips_relative_folders() {
    let vars = [
        ("SystemRoot", r"C:\Windows"),
        ("PATH", r".;\tools;tools;C:\Scoop\shims\;D:\pwsh"),
    ];
    let shells = Machine::new(
        &vars,
        &[
            r".\pwsh.exe",
            r"\tools\pwsh.exe",
            r"tools\pwsh.exe",
            r"C:\Scoop\shims\pwsh.exe",
        ],
    )
    .shells();
    assert_eq!(
        find(&shells, "pwsh").shell.program.as_deref(),
        Some(r"C:\Scoop\shims\pwsh.exe")
    );
}

#[test]
fn finds_git_bash_installed_for_one_user() {
    let shells = Machine::new(
        VARS,
        &[r"C:\Users\me\AppData\Local\Programs\Git\bin\bash.exe"],
    )
    .shells();
    assert_eq!(
        find(&shells, "git-bash").shell.program.as_deref(),
        Some(r"C:\Users\me\AppData\Local\Programs\Git\bin\bash.exe")
    );
}

#[test]
fn falls_back_to_cmd_in_system32_when_comspec_points_nowhere() {
    let vars = [
        ("SystemRoot", r"C:\Windows\"),
        ("ComSpec", r"C:\gone\cmd.exe"),
    ];
    let shells = Machine::new(&vars, &[]).shells();
    assert_eq!(
        find(&shells, "cmd").shell.program.as_deref(),
        Some(r"C:\Windows\System32\cmd.exe")
    );
}

#[test]
fn ignores_relative_folders_in_the_environment() {
    let vars = [
        ("SystemRoot", "Windows"),
        ("ProgramFiles", "Program Files"),
        ("LOCALAPPDATA", r"\Users\me\AppData\Local"),
        ("ComSpec", "cmd.exe"),
    ];
    let files = [
        r"Program Files\PowerShell\7\pwsh.exe",
        r"\Users\me\AppData\Local\Programs\Git\bin\bash.exe",
        "cmd.exe",
    ];
    let shells = Machine::new(&vars, &files).shells();
    assert_eq!(ids(&shells), ["cmd"]);
    assert_eq!(
        shells[0].shell.program.as_deref(),
        Some(r"C:\Windows\System32\cmd.exe")
    );
}

#[test]
fn says_when_wsl_did_not_answer() {
    let mut machine = Machine::new(VARS, &[r"C:\Windows\System32\wsl.exe"]);
    machine.distributions = None;
    let listing = machine.listing();
    assert!(!listing.wsl_answered);
    assert_eq!(ids(&listing.shells), ["cmd"]);

    machine.distributions = Some(Vec::new());
    assert!(machine.listing().wsl_answered);
    // No wsl.exe at all: nothing to wait for.
    assert!(Machine::new(VARS, &[]).listing().wsl_answered);
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

#[test]
fn reads_wsl_names_in_utf16_as_older_releases_write_them() {
    let output = utf16("\u{feff}Ubuntu-24.04\r\nDebian\r\n\r\n");
    assert_eq!(wsl_names(&output), ["Ubuntu-24.04", "Debian"]);
}

#[test]
fn reads_wsl_names_in_utf8_and_leaves_out_docker_desktop() {
    let output = b"Ubuntu\ndocker-desktop\ndocker-desktop-data\nkali-linux\n";
    assert_eq!(wsl_names(output), ["Ubuntu", "kali-linux"]);
}

#[test]
fn reads_no_wsl_names_from_empty_output() {
    assert_eq!(wsl_names(b""), Vec::<String>::new());
    assert_eq!(wsl_names(&utf16("\r\n")), Vec::<String>::new());
}

#[test]
fn drops_a_torn_byte_and_names_that_are_not_names() {
    // A stray last byte of UTF-16 is left out, not read as half a letter.
    let mut output = utf16("Ubuntu\r\n");
    output.push(b'D');
    assert_eq!(wsl_names(&output), ["Ubuntu"]);
    // A control character, or a leading dash that `-d` would take as an option.
    let output = utf16("Deb\u{7}ian\r\n-x\r\n--help\r\nkali\r\n");
    assert_eq!(wsl_names(&output), ["kali"]);
}
