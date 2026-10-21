// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Domain model for Tauminus.
//!
//! This crate holds the rules that stay true whatever transport or UI carries a
//! session. It depends on no runtime, driver, or framework, so every rule here is
//! testable without I/O.

mod flow;
mod output;
mod session;
mod terminal;

pub use flow::{FlowControl, FlowSignal, Watermarks};
pub use output::{OutputBatcher, ReadyBatches};
pub use session::{LocalShell, SessionId, SessionKind, SessionMeta};
pub use terminal::{TerminalSize, TerminalSizeError};
