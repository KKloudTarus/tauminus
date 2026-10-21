// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The shell process: waiting for it to exit, and signalling it safely.
//!
//! Safety invariant: a pid is signalled only while holding `ProcessGuard`'s
//! lock with `exited == false`, and the child is reaped only while holding the
//! same lock after setting `exited = true`. A reaped pid can be reused by an
//! unrelated process; because reaping and signalling exclude each other, no
//! signal ever reaches a pid after it has been released.

use std::sync::{Mutex, MutexGuard, PoisonError};

use portable_pty::{Child, ChildKiller, ExitStatus};
use tracing::{debug, warn};

/// Whether the shell has been reaped, under the lock that orders reaping
/// against signalling.
#[derive(Debug, Default)]
pub(crate) struct ProcessGuard {
    exited: Mutex<bool>,
}

impl ProcessGuard {
    fn lock(&self) -> MutexGuard<'_, bool> {
        self.exited.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `signal` only if the process has not been reaped yet.
    pub(crate) fn while_alive(&self, signal: impl FnOnce()) {
        let exited = self.lock();
        if !*exited {
            signal();
        }
    }
}

/// Blocks until the shell exits, reaps it under the guard, and returns its
/// exit code. `None` means the shell was killed by a signal or its status
/// could not be read.
pub(crate) fn wait_for_exit(
    mut child: Box<dyn Child + Send + Sync>,
    guard: &ProcessGuard,
) -> Option<u32> {
    let status = reap(&mut child, guard);
    match status {
        Ok(status) if status.signal().is_none() => Some(status.exit_code()),
        Ok(_) => None,
        Err(e) => {
            warn!(error = %e, "failed to collect shell exit status");
            None
        }
    }
}

/// Linux: block in `waitid(WNOWAIT)`, which waits for the exit without
/// releasing the pid, then reap under the guard.
#[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
fn reap(
    child: &mut Box<dyn Child + Send + Sync>,
    guard: &ProcessGuard,
) -> std::io::Result<ExitStatus> {
    use nix::sys::wait::{Id, WaitPidFlag, waitid};
    use nix::unistd::Pid;

    if let Some(pid) = child.process_id().and_then(|pid| i32::try_from(pid).ok()) {
        loop {
            match waitid(
                Id::Pid(Pid::from_raw(pid)),
                WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT,
            ) {
                Ok(_) => break,
                Err(nix::errno::Errno::EINTR) => {}
                Err(e) => {
                    debug!(error = %e, "waitid failed; falling back to a plain wait");
                    break;
                }
            }
        }
    }
    let mut exited = guard.lock();
    *exited = true;
    child.wait()
}

/// macOS and other Unix systems without `waitid` in nix: poll `try_wait`, which
/// reaps, while holding the guard. 100 ms polling bounds exit detection
/// latency and costs one syscall per session per 100 ms.
#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_os = "freebsd"))
))]
fn reap(
    child: &mut Box<dyn Child + Send + Sync>,
    guard: &ProcessGuard,
) -> std::io::Result<ExitStatus> {
    loop {
        {
            let mut exited = guard.lock();
            if let Some(status) = child.try_wait()? {
                *exited = true;
                return Ok(status);
            }
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "a dedicated waiter thread, not an async worker"
        )]
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Windows keeps the pid reserved while the process handle is open, and
/// portable-pty holds that handle, so a plain wait is safe.
#[cfg(windows)]
fn reap(
    child: &mut Box<dyn Child + Send + Sync>,
    guard: &ProcessGuard,
) -> std::io::Result<ExitStatus> {
    let status = child.wait();
    *guard.lock() = true;
    status
}

/// Signals that end the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    /// SIGHUP: what closing a terminal window sends.
    HangUp,
    /// SIGKILL: cannot be ignored.
    Kill,
}

/// Sends `ending` to the foreground process group and the shell's group.
/// Group ids 0 and 1 would address our own group or init and are skipped.
#[cfg(unix)]
pub(crate) fn signal_groups(foreground: Option<i32>, shell: Option<i32>, ending: Ending) {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;

    let signal = match ending {
        Ending::HangUp => Signal::SIGHUP,
        Ending::Kill => Signal::SIGKILL,
    };
    let mut groups = [foreground, shell];
    if groups[0] == groups[1] {
        groups[1] = None;
    }
    for group in groups.into_iter().flatten().filter(|&g| g > 1) {
        if let Err(e) = killpg(Pid::from_raw(group), signal) {
            debug!(group, ?signal, error = %e, "signal to process group failed; it has likely exited");
        }
    }
}

/// Terminates the shell through portable-pty's killer: `TerminateProcess` on
/// Windows, SIGHUP to the shell on Unix.
pub(crate) fn kill_with(killer: &Mutex<Box<dyn ChildKiller + Send + Sync>>) {
    let mut killer = killer.lock().unwrap_or_else(PoisonError::into_inner);
    if let Err(e) = killer.kill() {
        debug!(error = %e, "kill failed; shell has likely exited");
    }
}
