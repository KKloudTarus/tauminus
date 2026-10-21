// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Use cases for Tauminus and the ports that adapters implement.
//!
//! Adapters (PTY, SSH, storage) implement the traits in [`ports`]. The desktop
//! shell builds the use cases with concrete adapters and calls them from IPC
//! commands. Nothing in this crate knows about Tauri or a specific transport
//! library.

mod local;
pub mod ports;
mod session;

pub use local::LocalShells;
pub use session::{SessionConfig, SessionError, SessionManager, SessionSlot};
