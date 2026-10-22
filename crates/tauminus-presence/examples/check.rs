// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Asks the OS's presence check once and prints what it answered, to try
//! the adapter by hand on a machine with Touch ID, Windows Hello, or a
//! polkit agent: `cargo run -p tauminus-presence --example check`. The
//! steps are in `docs/testing/presence.md`.
//!
//! - `check available` only asks whether the check can run (CI does this).
//! - `check <seconds>` waits that long first, so an agent can be started
//!   for this process (`pkttyagent --process <pid>`).
//! - `check` asks for real: the OS shows its prompt.

use tauminus_app::vault::{Presence, Purpose};
use tauminus_presence::OsPresence;

#[expect(clippy::print_stdout, reason = "an example that reports to its user")]
fn main() {
    let presence = OsPresence::new();
    let argument = std::env::args().nth(1);
    println!("pid {}", std::process::id());
    println!("{}: available {:?}", presence.name(), presence.available());
    if argument.as_deref() == Some("available") {
        return;
    }
    if let Some(wait) = argument.and_then(|s| s.parse().ok()) {
        #[expect(clippy::disallowed_methods, reason = "a plain example thread")]
        std::thread::sleep(std::time::Duration::from_secs(wait));
    }
    let started = std::time::Instant::now();
    let answer = presence.confirm(Purpose::Unlock);
    println!("confirm: {answer:?} after {:.1?}", started.elapsed());
}
