// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// The one list of IPC command names. `build.rs` includes this file to generate
// `allow-*` permissions, and a test checks it against the generated bindings
// and the capability file, so the three cannot drift apart.

/// Every IPC command the app exposes.
const COMMANDS: &[&str] = &[
    "session_open",
    "session_write",
    "session_write_bytes",
    "session_resize",
    "session_acknowledge",
    "session_close",
    "ssh_open",
    "ssh_prompt_reply",
    "hosts_list",
    "host_save",
    "host_delete",
    "group_save",
    "group_delete",
    "ssh_config_aliases",
    "hosts_import_ssh_config",
    "host_connect",
    "guard_check",
    "open_link",
    "clipboard_read_text",
    "clipboard_write_text",
];
