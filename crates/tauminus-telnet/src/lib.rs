// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Telnet sessions: the protocol, and a stream over TCP.

pub mod protocol;
mod stream;

pub use stream::TcpTelnetConnector;
