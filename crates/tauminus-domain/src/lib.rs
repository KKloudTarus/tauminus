// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Domain model for Tauminus.
//!
//! This crate holds the rules that stay true whatever transport or UI carries a
//! session. It depends on no runtime, driver, or framework, so every rule here is
//! testable without I/O.

mod flow;
mod guard;
mod hosts;
mod output;
mod session;
mod ssh;
mod terminal;

pub use flow::{FlowControl, FlowSignal, Watermarks};
pub use guard::{
    Finding, GuardSettings, MAX_LINE, MAX_PATTERNS, MAX_POLICY_PATTERNS, Pattern, PatternError,
    Policy, Rule, Verdict,
};
pub use hosts::{
    Group, GroupId, Hop, Host, HostId, HostsError, IdentityId, Inventory, Label, MAX_GROUP_DEPTH,
    MAX_GROUPS, MAX_HOSTS, MAX_TAGS, PROD_TAG, Settings, Tag,
};
pub use output::{OutputBatcher, ReadyBatches};
pub use session::{LocalShell, SessionId, SessionKind, SessionMeta};
pub use ssh::{
    AuthMethod, AuthOptions, Endpoint, EnvError, EnvVar, HostName, MAX_ENV, NameError,
    ProxyCommandError, Route, SessionOptions, SshTarget, TargetError, UserName, env_list,
    expand_proxy_command,
};
pub use terminal::{TerminalSize, TerminalSizeError};
