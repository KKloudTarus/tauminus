// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Use cases for Tauminus and the ports that adapters implement.
//!
//! Adapters (PTY, SSH, storage) implement the traits in [`ports`]. The desktop
//! shell builds the use cases with concrete adapters and calls them from IPC
//! commands. Nothing in this crate knows about Tauri or a specific transport
//! library.

mod ai;
mod connections;
#[cfg(test)]
mod fakes;
mod files;
mod fill;
mod forwards;
pub mod history;
mod hosts;
mod local;
mod mosh;
pub mod ports;
pub mod recording;
mod serial;
mod session;
mod snippets;
mod ssh;
pub mod sync;
mod telnet;
mod tmux;
mod triggers;
pub mod trust;
pub mod vault;

pub use ai::{AiSettings, AiSettingsInput, Assistant, AssistantError, Preview, Reply};
pub use connections::{Connections, Login, LoginError};
pub use files::{
    Direction, Files, FilesError, MAX_RUNNING, MAX_SESSIONS, PER_SESSION, TransferState,
    TransferStatus,
};
pub use fill::{Fill, FillError, Offer};
pub use forwards::{Forwards, HostMark, RuleState, StartError, Ticket};
pub use hosts::{HostBook, HostBookError, IMPORT_GROUP, ImportReport, Reading};
pub use local::LocalShells;
pub use mosh::{MoshOpenError, MoshSessions};
pub use serial::SerialSessions;
pub use session::{RECENT_OUTPUT, SessionConfig, SessionError, SessionManager, SessionSlot};
pub use snippets::{Skipped, SnippetBook, SnippetBookError, startup_text};
pub use ssh::{SshOpenError, SshSessions};
pub use sync::{SyncRunError, SyncService, SyncServiceError, Syncer};
pub use telnet::TelnetSessions;
pub use tmux::{
    ControlId, Opened, TabWindow, TmuxError, TmuxSessions, TopologyEvent, TopologySink,
};
pub use triggers::{Prepared, TriggerBook, TriggerBookError};
