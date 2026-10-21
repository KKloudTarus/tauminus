// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! These tests start real processes on a real PTY.

use std::time::Duration;

use super::*;

const TIMEOUT: Duration = Duration::from_secs(10);

fn shell(program: &str, args: &[&str]) -> LocalShell {
    LocalShell {
        program: Some(program.into()),
        args: args.iter().map(ToString::to_string).collect(),
        cwd: None,
    }
}

/// Reads events until the transport reports an exit.
async fn collect_until_exit(events: &mut mpsc::Receiver<TerminalEvent>) -> (Vec<u8>, Option<u32>) {
    let mut output = Vec::new();
    let result = tokio::time::timeout(TIMEOUT, async {
        loop {
            match events.recv().await {
                Some(TerminalEvent::Output(chunk)) => output.extend(chunk),
                Some(TerminalEvent::Exited { code }) => return code,
                None => panic!("event channel closed before exit"),
            }
        }
    })
    .await;
    let code = result.expect("process exits before timeout");
    (output, code)
}

/// Reads events until `needle` appears in the output.
#[cfg(unix)]
async fn read_until(events: &mut mpsc::Receiver<TerminalEvent>, needle: &str) -> String {
    let mut output = Vec::new();
    tokio::time::timeout(TIMEOUT, async {
        while !String::from_utf8_lossy(&output).contains(needle) {
            match events.recv().await {
                Some(TerminalEvent::Output(chunk)) => output.extend(chunk),
                other => panic!("expected {needle:?}, got {other:?} after {output:?}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "timed out waiting for {needle:?}; got {:?}",
            String::from_utf8_lossy(&output)
        )
    });
    String::from_utf8_lossy(&output).into_owned()
}

#[test]
fn missing_program_fails_to_launch() {
    let result = NativePtyLauncher.launch(
        &shell("/nonexistent/tauminus-no-such-shell", &[]),
        TerminalSize::default(),
    );
    assert!(
        matches!(result, Err(LaunchError::Spawn { .. })),
        "{result:?}"
    );
}

#[test]
fn appimage_launcher_variables_do_not_reach_the_shell() {
    let mut command = CommandBuilder::new("sh");
    command.env("APPIMAGE", "/tmp/Tauminus.AppImage");
    command.env("LD_LIBRARY_PATH", "/tmp/.mount/usr/lib");
    command.env("HOME_MARKER", "kept");
    remove_launcher_environment(&mut command);
    assert_eq!(command.get_env("APPIMAGE"), None);
    assert_eq!(command.get_env("LD_LIBRARY_PATH"), None);
    assert!(command.get_env("HOME_MARKER").is_some());
}

#[test]
fn user_library_path_is_kept_outside_appimage() {
    let mut command = CommandBuilder::new("sh");
    command.env_remove("APPIMAGE");
    command.env("LD_LIBRARY_PATH", "/opt/lib");
    remove_launcher_environment(&mut command);
    assert!(command.get_env("LD_LIBRARY_PATH").is_some());
}

#[cfg(unix)]
mod unix {
    use super::*;

    #[tokio::test]
    async fn reports_output_and_exit_code() {
        let mut handle = NativePtyLauncher
            .launch(
                &shell("/bin/sh", &["-c", "printf hello; exit 7"]),
                TerminalSize::default(),
            )
            .unwrap();
        let (output, code) = collect_until_exit(&mut handle.events).await;
        assert_eq!(String::from_utf8_lossy(&output), "hello");
        assert_eq!(code, Some(7));
    }

    #[tokio::test]
    async fn input_reaches_the_shell() {
        let mut handle = NativePtyLauncher
            .launch(
                &shell("/bin/sh", &["-c", "read line; echo got:$line"]),
                TerminalSize::default(),
            )
            .unwrap();
        handle
            .input
            .send(TerminalInput::Data(b"ping\n".to_vec()))
            .await
            .unwrap();
        read_until(&mut handle.events, "got:ping").await;
    }

    #[tokio::test]
    async fn initial_size_and_resize_are_visible_to_the_shell() {
        let size = TerminalSize::new(100, 30).unwrap();
        let mut handle = NativePtyLauncher
            .launch(
                &shell("/bin/sh", &["-c", "stty size; read x; stty size"]),
                size,
            )
            .unwrap();
        read_until(&mut handle.events, "30 100").await;

        let resized = TerminalSize::new(132, 40).unwrap();
        handle
            .input
            .send(TerminalInput::Resize(resized))
            .await
            .unwrap();
        handle
            .input
            .send(TerminalInput::Data(b"\n".to_vec()))
            .await
            .unwrap();
        read_until(&mut handle.events, "40 132").await;
    }

    #[tokio::test]
    async fn terminate_kills_a_running_shell() {
        let mut handle = NativePtyLauncher
            .launch(
                &shell("/bin/sh", &["-c", "echo ready; sleep 60"]),
                TerminalSize::default(),
            )
            .unwrap();
        read_until(&mut handle.events, "ready").await;
        handle.terminator.terminate();
        let (_, code) = collect_until_exit(&mut handle.events).await;
        assert_ne!(code, Some(0), "killed shell does not report success");
    }

    #[tokio::test]
    async fn dropping_the_terminator_kills_the_shell() {
        let TerminalStream {
            input,
            mut events,
            terminator,
        } = NativePtyLauncher
            .launch(
                &shell("/bin/sh", &["-c", "echo ready; sleep 60"]),
                TerminalSize::default(),
            )
            .unwrap();
        read_until(&mut events, "ready").await;
        drop(terminator);
        collect_until_exit(&mut events).await;
        drop(input);
    }

    #[tokio::test]
    async fn terminate_works_while_the_writer_is_blocked() {
        // `sleep` never reads stdin, so 4 MiB of input fills the PTY buffer and
        // blocks the writer thread. Termination must not wait behind it.
        let mut handle = NativePtyLauncher
            .launch(
                &shell("/bin/sh", &["-c", "echo ready; sleep 60"]),
                TerminalSize::default(),
            )
            .unwrap();
        read_until(&mut handle.events, "ready").await;
        for _ in 0..64 {
            if handle
                .input
                .try_send(TerminalInput::Data(vec![b'a'; 64 * 1024]))
                .is_err()
            {
                break;
            }
        }
        handle.terminator.terminate();
        collect_until_exit(&mut handle.events).await;
    }

    #[tokio::test]
    async fn kill_ends_a_shell_that_ignores_hangup() {
        let mut handle = NativePtyLauncher
            .launch(
                &shell(
                    "/bin/sh",
                    &["-c", "trap '' HUP; echo ready; while :; do sleep 1; done"],
                ),
                TerminalSize::default(),
            )
            .unwrap();
        read_until(&mut handle.events, "ready").await;
        handle.terminator.terminate();
        let hung_up = tokio::time::timeout(
            Duration::from_millis(500),
            collect_until_exit(&mut handle.events),
        )
        .await;
        assert!(
            hung_up.is_err(),
            "SIGHUP is ignored, so the shell keeps running"
        );

        handle.terminator.kill();
        let (_, code) = collect_until_exit(&mut handle.events).await;
        assert_eq!(code, None, "killed by a signal");
    }

    #[tokio::test]
    async fn session_ends_when_the_shell_exits_even_if_a_job_holds_the_pty() {
        // The background job keeps the PTY open for 30 s after the shell exits.
        let mut handle = NativePtyLauncher
            .launch(
                &shell(
                    "/bin/sh",
                    &["-c", "nohup sleep 30 >/dev/null 2>&1 & echo bye; exit 4"],
                ),
                TerminalSize::default(),
            )
            .unwrap();
        let started = std::time::Instant::now();
        let (output, code) = collect_until_exit(&mut handle.events).await;
        assert_eq!(code, Some(4));
        assert!(String::from_utf8_lossy(&output).contains("bye"));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "exit is not held back by EOF"
        );
        handle.terminator.kill();
    }

    #[tokio::test]
    async fn terminator_does_nothing_after_the_shell_is_reaped() {
        let mut handle = NativePtyLauncher
            .launch(
                &shell("/bin/sh", &["-c", "exit 0"]),
                TerminalSize::default(),
            )
            .unwrap();
        collect_until_exit(&mut handle.events).await;
        // The pid is released now; these must not signal anything.
        handle.terminator.terminate();
        handle.terminator.kill();
    }

    #[tokio::test]
    async fn sets_term_for_full_color_programs() {
        let mut handle = NativePtyLauncher
            .launch(
                &shell(
                    "/bin/sh",
                    &["-c", "printf '%s %s' \"$TERM\" \"$COLORTERM\""],
                ),
                TerminalSize::default(),
            )
            .unwrap();
        let (output, _) = collect_until_exit(&mut handle.events).await;
        assert_eq!(String::from_utf8_lossy(&output), "xterm-256color truecolor");
    }

    #[tokio::test]
    async fn runs_in_requested_working_directory() {
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let mut spec = shell("/bin/sh", &["-c", "pwd -P"]);
        spec.cwd = Some(dir.clone());
        let mut handle = NativePtyLauncher
            .launch(&spec, TerminalSize::default())
            .unwrap();
        let (output, _) = collect_until_exit(&mut handle.events).await;
        assert_eq!(
            String::from_utf8_lossy(&output).trim(),
            dir.to_string_lossy()
        );
    }

    #[tokio::test]
    async fn large_output_arrives_complete() {
        // 4 MiB through a 1 MiB queue: the reader must block and resume.
        let mut handle = NativePtyLauncher
            .launch(
                &shell("/bin/sh", &["-c", "head -c 4194304 /dev/zero | tr '\\0' x"]),
                TerminalSize::default(),
            )
            .unwrap();
        let (output, code) = collect_until_exit(&mut handle.events).await;
        assert_eq!(code, Some(0));
        assert_eq!(output.len(), 4 * 1024 * 1024);
        assert!(output.iter().all(|&b| b == b'x'));
    }
}

#[cfg(windows)]
mod windows {
    use super::*;

    #[tokio::test]
    async fn reports_output_and_exit_code() {
        let mut handle = NativePtyLauncher
            .launch(
                &shell("cmd.exe", &["/C", "echo hello && exit 7"]),
                TerminalSize::default(),
            )
            .unwrap();
        let (output, code) = collect_until_exit(&mut handle.events).await;
        assert!(String::from_utf8_lossy(&output).contains("hello"));
        assert_eq!(code, Some(7));
    }
}
