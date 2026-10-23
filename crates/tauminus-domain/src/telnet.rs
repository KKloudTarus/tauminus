// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Where a Telnet session connects.

use crate::ssh::{Encoding, HostName};

/// The well-known Telnet port.
pub const TELNET_PORT: u16 = 23;

/// A Telnet connection: no login at the transport, so no user; the shell
/// or the device asks once connected. Telnet carries everything in clear
/// text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelnetTarget {
    pub host: HostName,
    pub port: u16,
    pub encoding: Encoding,
}
