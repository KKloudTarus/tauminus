// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::PathBuf;

use super::*;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tauminus-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn every_place_searched_past_path_is_absolute_on_this_system() {
    for dir in known_dirs() {
        assert!(dir.is_absolute(), "{}", dir.display());
    }
}

#[test]
fn text_from_a_cli_shows_only_what_reads_as_itself() {
    assert_eq!(
        shown("  denied\u{202e}nimda \u{200b}\u{1b}[31m ", 100),
        "deniednimda [31m"
    );
    assert_eq!(shown("abcdef", 3), "abc");
}

#[cfg(windows)]
#[test]
fn a_program_goes_before_a_batch_file_of_its_name() {
    let dir = Dir::new("windows");
    std::fs::write(dir.0.join("bw.cmd"), "@echo off\r\n").unwrap();
    let finder = Finder::in_dirs(vec![dir.0.clone()]);
    assert_eq!(finder.find("bw", "x").unwrap(), dir.0.join("bw.cmd"));
    std::fs::write(dir.0.join("bw.exe"), b"MZ").unwrap();
    assert_eq!(finder.find("bw", "x").unwrap(), dir.0.join("bw.exe"));
}

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use super::*;

    impl Dir {
        fn script(&self, name: &str, body: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }
    }

    #[test]
    fn a_cli_is_found_only_where_it_can_run() {
        let dir = Dir::new("find");
        let finder = Finder::in_dirs(vec![dir.0.clone()]);
        assert_eq!(
            finder.find("op", "get it from 1password.com").unwrap_err(),
            "op is not installed on this computer: get it from 1password.com"
        );
        std::fs::write(dir.0.join("op"), "#!/bin/sh\n").unwrap();
        assert!(finder.find("op", "x").is_err(), "a file that cannot run");
        let path = dir.script("op", "true");
        assert_eq!(finder.find("op", "x").unwrap(), path);
    }

    #[tokio::test]
    async fn a_run_passes_its_arguments_and_reads_both_outputs() {
        let dir = Dir::new("run");
        let path = dir.script(
            "echo",
            "printf '%s|' \"$@\"; printf '%s' \"$EXTRA\"; printf '\\n\\n  first\\033 line\\nsecond\\n' >&2; exit 3",
        );
        let run = run(
            "echo",
            &path,
            &["a b", "--", "-x", "$HOME", "'q'"],
            &[("EXTRA", "set")],
            Duration::from_secs(10),
            1024,
            0,
        )
        .await
        .unwrap();
        assert_eq!(run.status.code(), Some(3));
        assert_eq!(&run.stdout[..], b"a b|--|-x|$HOME|'q'|set");
        assert_eq!(run.error, "first line");
    }

    #[tokio::test]
    async fn a_run_stops_at_its_cap_and_its_deadline() {
        let dir = Dir::new("bounds");
        let loud = dir.script("loud", "head -c 2048 /dev/zero");
        let error = run("loud", &loud, &[], &[], Duration::from_secs(10), 1024, 0)
            .await
            .unwrap_err();
        assert_eq!(error, "loud printed more than Tauminus reads");
        let exact = run("loud", &loud, &[], &[], Duration::from_secs(10), 2048, 2049)
            .await
            .unwrap();
        assert_eq!(exact.stdout.len(), 2048);

        let slow = dir.script("slow", "exec sleep 30");
        let started = std::time::Instant::now();
        let error = run("slow", &slow, &[], &[], Duration::from_millis(300), 1024, 0)
            .await
            .unwrap_err();
        assert_eq!(error, "slow took longer than 0 s");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_cli_that_would_read_input_reads_nothing() {
        let dir = Dir::new("stdin");
        let path = dir.script("ask", "read answer; printf '%s' \"[$answer]\"");
        let run = run("ask", &path, &[], &[], Duration::from_secs(10), 64, 0)
            .await
            .unwrap();
        assert_eq!(&run.stdout[..], b"[]");
    }
}
