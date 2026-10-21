// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Local shell sessions over the platform pseudo-terminal: openpty on Unix,
//! `ConPTY` on Windows.
//!
//! PTY reads and writes block, so each session runs three OS threads:
//! - the reader moves output into a bounded channel and blocks when the
//!   session pump stops reading, which is how backpressure reaches the shell;
//! - the writer applies input and resizes in arrival order;
//! - the waiter blocks until the shell exits and reports the exit code.
//!
//! The session ends when the shell exits, as in a terminal emulator, not when
//! the PTY reaches EOF: a background job started with `nohup` can keep the PTY
//! open long after the shell is gone. The reader then lingers until that job
//! writes or closes the terminal, which is the same lifetime a terminal window
//! would give it.
//!
//! Termination bypasses the input queue. [`PtyTerminator`] signals the process
//! groups directly, so a writer stuck on a full PTY input buffer cannot delay
//! it. See [`process`] for why those signals can never hit a reused pid.

mod process;

use std::io::{Read, Write};
use std::sync::{Arc, Mutex, PoisonError, mpsc as std_mpsc};
use std::thread;
use std::time::Duration;

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tauminus_app::ports::{
    LaunchError, PtyLauncher, TerminalEvent, TerminalInput, TerminalStream, Terminator,
};
use tauminus_domain::{LocalShell, TerminalSize};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Read buffer per session. Matches the default output batch so one read can
/// fill one IPC message.
const READ_BUFFER_BYTES: usize = 64 * 1024;
/// Queued output chunks per session: at most 4 x 64 KiB = 256 KiB buffered
/// before the reader blocks. A small queue keeps Ctrl-C responsive during a
/// flood, because less old output stands in front of the prompt.
const EVENT_QUEUE: usize = 4;
/// Queued input messages per session before writers wait.
const INPUT_QUEUE: usize = 64;
/// After the shell exits, how long the waiter lets the reader drain output
/// still in the PTY before reporting the exit.
const EXIT_DRAIN: Duration = Duration::from_millis(200);

/// Launches local shells with the operating system's PTY.
#[derive(Debug, Default, Clone, Copy)]
pub struct NativePtyLauncher;

impl PtyLauncher for NativePtyLauncher {
    fn launch(
        &self,
        shell: &LocalShell,
        size: TerminalSize,
    ) -> Result<TerminalStream, LaunchError> {
        let command = command_for(shell);
        let program = command.get_argv().first().map_or_else(
            || "default shell".to_owned(),
            |p| p.to_string_lossy().into_owned(),
        );
        let launch_error = |source: std::io::Error| LaunchError::Spawn {
            program: program.clone(),
            source,
        };

        let pair = native_pty_system()
            .openpty(pty_size(size))
            .map_err(|e| launch_error(std::io::Error::other(e)))?;
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| launch_error(std::io::Error::other(e)))?;
        // The child holds its own copy of the slave side. Closing ours lets the
        // reader see EOF when the child exits.
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(std::io::Error::other)?;
        let writer = pair.master.take_writer().map_err(std::io::Error::other)?;
        let master = Arc::new(Mutex::new(pair.master));
        let guard = Arc::new(process::ProcessGuard::default());
        let terminator = PtyTerminator {
            guard: Arc::clone(&guard),
            killer: Mutex::new(child.clone_killer()),
            #[cfg(unix)]
            shell_pid: child.process_id().and_then(|pid| i32::try_from(pid).ok()),
            #[cfg(unix)]
            master: Arc::clone(&master),
        };

        let (events_tx, events_rx) = mpsc::channel(EVENT_QUEUE);
        let (input_tx, input_rx) = mpsc::channel(INPUT_QUEUE);
        // Closed by the reader when it stops, so the waiter knows the PTY is
        // drained.
        let (reader_done_tx, reader_done_rx) = std_mpsc::channel::<()>();

        let reader_events = events_tx.clone();
        thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || read_loop(reader, &reader_events, reader_done_tx))?;
        thread::Builder::new()
            .name("pty-writer".into())
            .spawn(move || write_loop(&master, writer, input_rx))?;
        thread::Builder::new()
            .name("pty-waiter".into())
            .spawn(move || {
                let code = process::wait_for_exit(child, &guard);
                let _ = reader_done_rx.recv_timeout(EXIT_DRAIN);
                let _ = events_tx.blocking_send(TerminalEvent::Exited { code });
            })?;

        Ok(TerminalStream {
            input: input_tx,
            events: events_rx,
            terminator: Box::new(terminator),
        })
    }
}

/// Ends the shell the way closing a terminal window does.
///
/// `terminate` sends SIGHUP to the terminal's foreground process group and to
/// the shell's group, so a running `sleep` or `vim` ends with the shell.
/// `kill` sends SIGKILL to the same groups, for shells that ignore SIGHUP. On
/// Windows both terminate the shell process, and closing the pseudo console
/// ends its clients. Both do nothing once the shell has been reaped.
///
/// Dropping the terminator terminates the shell: once the session's owner is
/// gone, nothing can read its output.
struct PtyTerminator {
    guard: Arc<process::ProcessGuard>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    #[cfg(unix)]
    shell_pid: Option<i32>,
    /// Read for the terminal's foreground process group, which only Unix has.
    #[cfg(unix)]
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
}

impl PtyTerminator {
    fn end(&self, ending: process::Ending) {
        // Lock order: guard, then master. The writer takes only the master and
        // the waiter only the guard, so this cannot deadlock.
        self.guard.while_alive(|| {
            #[cfg(unix)]
            {
                let foreground = self
                    .master
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .process_group_leader();
                process::signal_groups(foreground, self.shell_pid, ending);
            }
            if cfg!(windows) || ending == process::Ending::HangUp {
                process::kill_with(&self.killer);
            }
        });
    }
}

impl Terminator for PtyTerminator {
    fn terminate(&self) {
        self.end(process::Ending::HangUp);
    }

    fn kill(&self) {
        self.end(process::Ending::Kill);
    }
}

impl Drop for PtyTerminator {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// Variables that the `AppImage` launcher sets for the app itself. A shell that
/// inherits them loads the app's bundled libraries instead of the system's.
const APPIMAGE_VARIABLES: &[&str] = &[
    "APPDIR",
    "APPIMAGE",
    "ARGV0",
    "OWD",
    "LD_LIBRARY_PATH",
    "GDK_PIXBUF_MODULE_FILE",
    "GIO_MODULE_DIR",
    "GSETTINGS_SCHEMA_DIR",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    "GTK_PATH",
    "GTK_IM_MODULE_FILE",
    "PYTHONHOME",
    "PYTHONPATH",
];

fn command_for(shell: &LocalShell) -> CommandBuilder {
    let mut command = match &shell.program {
        Some(program) => {
            let mut command = CommandBuilder::new(program);
            command.args(&shell.args);
            command
        }
        None => CommandBuilder::new_default_prog(),
    };
    if let Some(cwd) = &shell.cwd {
        command.cwd(cwd);
    }
    remove_launcher_environment(&mut command);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command
}

fn remove_launcher_environment(command: &mut CommandBuilder) {
    if command.get_env("APPIMAGE").is_some() {
        for name in APPIMAGE_VARIABLES {
            command.env_remove(name);
        }
    }
}

fn pty_size(size: TerminalSize) -> PtySize {
    PtySize {
        rows: size.rows(),
        cols: size.cols(),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Moves PTY output into the event queue until EOF. `_drained` is dropped
/// when the loop returns, which tells the waiter the PTY is drained.
fn read_loop(
    mut reader: Box<dyn Read + Send>,
    events: &mpsc::Sender<TerminalEvent>,
    _drained: std_mpsc::Sender<()>,
) {
    let mut buffer = vec![0u8; READ_BUFFER_BYTES];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return,
            Ok(n) => {
                if events
                    .blocking_send(TerminalEvent::Output(buffer[..n].to_vec()))
                    .is_err()
                {
                    // The session is gone; its terminator has ended the shell.
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            // Linux reports EIO on the master once the slave side closes.
            Err(e) => {
                debug!(error = %e, "pty read ended");
                return;
            }
        }
    }
}

fn write_loop(
    master: &Mutex<Box<dyn MasterPty + Send>>,
    mut writer: Box<dyn Write + Send>,
    mut input: mpsc::Receiver<TerminalInput>,
) {
    while let Some(message) = input.blocking_recv() {
        match message {
            TerminalInput::Data(bytes) => {
                if let Err(e) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
                    debug!(error = %e, "pty write failed; shell has exited");
                    return;
                }
            }
            TerminalInput::Resize(size) => {
                let resized = master
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .resize(pty_size(size));
                if let Err(e) = resized {
                    warn!(error = %e, "pty resize failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
