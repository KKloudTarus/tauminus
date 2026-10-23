// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Use cases for Tauminus and the ports that adapters implement.
//!
//! Adapters (PTY, SSH, storage) implement the traits in [`ports`]. The desktop
//! shell builds the use cases with concrete adapters and calls them from IPC
//! commands. Nothing in this crate knows about Tauri or a specific transport
//! library.

mod connections;
#[cfg(test)]
mod fakes;
mod forwards;
pub mod history;
mod hosts;
mod local;
pub mod ports;
mod session;
mod ssh;
pub mod vault;

pub use connections::{Connections, Login, LoginError};
pub use forwards::{Forwards, HostMark, RuleState, StartError, Ticket};
pub use hosts::{HostBook, HostBookError, IMPORT_GROUP, ImportReport};
pub use local::LocalShells;
pub use session::{SessionConfig, SessionError, SessionManager, SessionSlot};
pub use ssh::{SshOpenError, SshSessions};
