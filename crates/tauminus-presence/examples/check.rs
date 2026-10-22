// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Asks the OS's presence check once and prints what it answered, to try
//! the adapter by hand: `cargo run -p tauminus-presence --example check`.
//! A number of seconds as the argument waits that long first, so an agent
//! can be started for this process (`pkttyagent --process <pid>`).

use tauminus_app::vault::{Presence, Purpose};
use tauminus_presence::OsPresence;

#[expect(clippy::print_stdout, reason = "an example that reports to its user")]
fn main() {
    let presence = OsPresence::new();
    println!("pid {}", std::process::id());
    if let Some(wait) = std::env::args().nth(1).and_then(|s| s.parse().ok()) {
        #[expect(clippy::disallowed_methods, reason = "a plain example thread")]
        std::thread::sleep(std::time::Duration::from_secs(wait));
    }
    println!("{}: available {:?}", presence.name(), presence.available());
    println!("confirm: {:?}", presence.confirm(Purpose::Unlock));
}
