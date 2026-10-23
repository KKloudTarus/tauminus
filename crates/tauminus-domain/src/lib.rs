// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Domain model for Tauminus.
//!
//! This crate holds the rules that stay true whatever transport or UI carries a
//! session. It depends on no runtime, driver, or framework, so every rule here is
//! testable without I/O.

mod flow;
mod forward;
mod guard;
mod history;
mod hosts;
mod keys;
mod output;
mod session;
mod sftp;
mod snippet;
mod ssh;
mod terminal;

pub use flow::{FlowControl, FlowSignal, Watermarks};
pub use forward::{
    Bind, BindAddress, Destination, Forward, ForwardError, ForwardRule, MAX_FORWARDS,
};
pub use guard::{
    Finding, GuardSettings, MAX_LINE, MAX_PATTERNS, MAX_POLICY_PATTERNS, Pattern, PatternError,
    Policy, Rule, Verdict,
};
pub use history::{
    EXCERPT_FROM, ExitFilter, HistoryEntry, HistoryHit, HistoryQuery, HistoryQueryError,
    MAX_COMMAND, MAX_OUTPUT, MAX_RESULTS, Retention,
};
pub use hosts::{
    ForwardId, Group, GroupId, Hop, Host, HostId, HostsError, IdentityId, Inventory, Label,
    MAX_GROUP_DEPTH, MAX_GROUPS, MAX_HOSTS, MAX_STARTUP, MAX_TAGS, PROD_TAG, Settings, SnippetId,
    Tag,
};
pub use keys::{KeyKind, PublicKeyLine, PublicKeyLineError, install_script};
pub use output::{OutputBatcher, ReadyBatches};
pub use session::{LocalShell, SessionId, SessionKind, SessionMeta};
pub use sftp::{
    LocalOs, MAX_NAME, NameError as SftpNameError, Source, Start, TAIL_CHECK, local_name, plan,
    remote_join, remote_parent, tail_start,
};
pub use snippet::{
    Library, MAX_BODY, MAX_FILLED, MAX_LIBRARY, MAX_SNIPPETS, MAX_VALUE, MAX_VARIABLES, Snippet,
    SnippetError, Variable, fill, variables,
};
pub use ssh::{
    AuthMethod, AuthOptions, Encoding, Endpoint, EnvError, EnvVar, HostName, MAX_ENV, NameError,
    ProxyCommandError, Route, SessionOptions, SshTarget, TargetError, UserName, env_list,
    expand_proxy_command,
};
pub use terminal::{TerminalSize, TerminalSizeError};
