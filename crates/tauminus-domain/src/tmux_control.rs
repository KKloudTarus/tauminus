// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! tmux control mode (`tmux -C`): reading what tmux sends, and writing the
//! commands the app sends it. Pure: bytes in, events and command lines out.
//! See ADR 0024 and the CONTROL MODE section of tmux(1).
//!
//! tmux sends one line per notification, and wraps each command's reply in
//! `%begin` and `%end` (or `%error`). `%output` carries a pane's output with
//! bytes below 0x20 and `\` written as `\ooo`; other bytes come as they are.

use std::fmt::Write as _;

use thiserror::Error;

/// Longest line read before the stream is given up on: a pane's output
/// comes in reads of a few KiB, and a captured screen in lines of a row.
pub const MAX_CONTROL_LINE: usize = 1 << 20;
/// Most lines one reply keeps; the rest are counted, not kept.
pub const MAX_REPLY_LINES: usize = 20_000;
/// Most bytes of lines one reply keeps.
pub(crate) const MAX_REPLY_BYTES: usize = 8 << 20;
/// Longest `send-keys` command line, as iTerm2 keeps it: tmux has crashed
/// on longer ones.
const MAX_COMMAND_LINE: usize = 1000;

/// A tmux pane, `%N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(pub u32);

/// A tmux window, `@N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub u32);

impl std::fmt::Display for PaneId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "%{}", self.0)
    }
}

impl std::fmt::Display for WindowId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "@{}", self.0)
    }
}

/// Something tmux said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlEvent {
    /// The reply to a command, in the order the commands were sent.
    Reply {
        ok: bool,
        lines: Vec<String>,
        /// Lines past [`MAX_REPLY_LINES`], dropped.
        dropped: usize,
    },
    Output {
        pane: PaneId,
        data: Vec<u8>,
    },
    /// The pane's output stopped, past `pause-after`: what it printed since
    /// is lost, and its screen must be read again on [`Continue`].
    Pause {
        pane: PaneId,
    },
    Continue {
        pane: PaneId,
    },
    WindowAdd {
        window: WindowId,
    },
    WindowClose {
        window: WindowId,
    },
    WindowRenamed {
        window: WindowId,
        name: String,
    },
    /// A window's panes changed: a split, a closed pane, a resize.
    LayoutChange {
        window: WindowId,
        layout: String,
    },
    /// The client moved to another session.
    SessionChanged {
        name: String,
    },
    /// tmux ends the client; the reason, if it gave one.
    Exit {
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ControlError {
    #[error("tmux sent a line longer than {MAX_CONTROL_LINE} bytes")]
    LineTooLong,
}

#[derive(Debug)]
struct Reply {
    /// `TIME NUMBER FLAGS` of its `%begin`: only a `%end` or `%error`
    /// with the same ends it, so a captured screen holding such a line
    /// stays in the reply.
    guard: Vec<u8>,
    lines: Vec<String>,
    bytes: usize,
    dropped: usize,
}

/// Reads tmux's control stream, as it arrives in pieces.
#[derive(Debug, Default)]
pub struct ControlParser {
    line: Vec<u8>,
    /// Inside a reply: its guard's arguments, its lines so far, their
    /// bytes, and how many were dropped.
    reply: Option<Reply>,
    /// The first `%begin` came: before it, what the login shell printed
    /// (a greeting, with no line break) is not tmux's.
    framed: bool,
}

impl ControlParser {
    /// The events in `bytes`, with what a line left unfinished kept for the
    /// next call. O(n).
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<ControlEvent>, ControlError> {
        let mut events = Vec::new();
        let mut rest = bytes;
        while let Some(end) = rest.iter().position(|&b| b == b'\n') {
            if self.line.len() + end > MAX_CONTROL_LINE {
                return Err(ControlError::LineTooLong);
            }
            self.line.extend_from_slice(&rest[..end]);
            rest = &rest[end + 1..];
            let mut line = std::mem::take(&mut self.line);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if let Some(event) = self.line_event(&line) {
                events.push(event);
            }
        }
        if self.line.len() + rest.len() > MAX_CONTROL_LINE {
            return Err(ControlError::LineTooLong);
        }
        self.line.extend_from_slice(rest);
        Ok(events)
    }

    fn line_event(&mut self, line: &[u8]) -> Option<ControlEvent> {
        let line = if self.framed {
            line
        } else {
            let start = line.windows(7).position(|w| w == b"%begin ")?;
            self.framed = true;
            &line[start..]
        };
        if let Some(reply) = &mut self.reply {
            // Only the guard with the same time, number and flags ends a
            // reply: what a command prints (a pane's rows) may look like one.
            let end = line.strip_prefix(b"%end ").map(|rest| (true, rest));
            let error = line.strip_prefix(b"%error ").map(|rest| (false, rest));
            if let Some((ok, rest)) = end.or(error)
                && rest == reply.guard.as_slice()
            {
                let Reply { lines, dropped, .. } = self.reply.take()?;
                return Some(ControlEvent::Reply { ok, lines, dropped });
            }
            if reply.lines.len() < MAX_REPLY_LINES && reply.bytes + line.len() <= MAX_REPLY_BYTES {
                reply.bytes += line.len();
                reply.lines.push(String::from_utf8_lossy(line).into_owned());
            } else {
                reply.dropped += 1;
            }
            return None;
        }
        // Output is most of the stream: read as bytes, never as text.
        if let Some(rest) = line.strip_prefix(b"%output ") {
            let (pane, value) = split_word(rest);
            return Some(ControlEvent::Output {
                pane: pane_id(pane)?,
                data: unescape(value),
            });
        }
        let text = String::from_utf8_lossy(line);
        let (word, args) = text.split_once(' ').unwrap_or((&text, ""));
        match word {
            "%begin" => {
                self.reply = Some(Reply {
                    guard: line.get(7..).unwrap_or_default().to_vec(),
                    lines: Vec::new(),
                    bytes: 0,
                    dropped: 0,
                });
                None
            }
            "%output" => {
                // The value is bytes: read it from the line, not the text.
                let (pane, value) = split_word(line.get(8..)?);
                Some(ControlEvent::Output {
                    pane: pane_id(pane)?,
                    data: unescape(value),
                })
            }
            "%extended-output" => {
                // `%extended-output %N AGE ... : VALUE`
                let rest = line.get(17..)?;
                let colon = rest.windows(3).position(|w| w == b" : ")?;
                let (pane, _) = split_word(&rest[..colon]);
                Some(ControlEvent::Output {
                    pane: pane_id(pane)?,
                    data: unescape(&rest[colon + 3..]),
                })
            }
            "%pause" => Some(ControlEvent::Pause {
                pane: pane_id(args.as_bytes())?,
            }),
            "%continue" => Some(ControlEvent::Continue {
                pane: pane_id(args.as_bytes())?,
            }),
            "%window-add" => Some(ControlEvent::WindowAdd {
                window: window_id(args)?,
            }),
            "%window-close" | "%unlinked-window-close" => Some(ControlEvent::WindowClose {
                window: window_id(args)?,
            }),
            "%window-renamed" => {
                let (window, name) = args.split_once(' ').unwrap_or((args, ""));
                Some(ControlEvent::WindowRenamed {
                    window: window_id(window)?,
                    name: name.to_owned(),
                })
            }
            "%layout-change" => {
                let mut parts = args.split(' ');
                let window = window_id(parts.next()?)?;
                Some(ControlEvent::LayoutChange {
                    window,
                    layout: parts.next()?.to_owned(),
                })
            }
            "%session-changed" => {
                let (_, name) = args.split_once(' ')?;
                Some(ControlEvent::SessionChanged {
                    name: name.to_owned(),
                })
            }
            "%exit" => Some(ControlEvent::Exit {
                reason: (!args.is_empty()).then(|| args.to_owned()),
            }),
            // The rest (%sessions-changed, %pane-mode-changed, ...) and any
            // line outside a reply that is not a notification.
            _ => None,
        }
    }
}

fn split_word(bytes: &[u8]) -> (&[u8], &[u8]) {
    match bytes.iter().position(|&b| b == b' ') {
        Some(i) => (&bytes[..i], &bytes[i + 1..]),
        None => (bytes, &[][..]),
    }
}

fn pane_id(bytes: &[u8]) -> Option<PaneId> {
    std::str::from_utf8(bytes.strip_prefix(b"%")?)
        .ok()?
        .trim()
        .parse()
        .ok()
        .map(PaneId)
}

fn window_id(text: &str) -> Option<WindowId> {
    text.trim().strip_prefix('@')?.parse().ok().map(WindowId)
}

/// `%output`'s value as bytes: `\ooo` is the byte with that octal value.
/// Anything else, a lone `\` included, is kept as it came. O(n).
pub(crate) fn unescape(value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len());
    let mut i = 0;
    while i < value.len() {
        let b = value[i];
        if b == b'\\'
            && let Some(digits) = value.get(i + 1..i + 4)
            && digits.iter().all(|d| (b'0'..=b'7').contains(d))
        {
            let n = digits.iter().fold(0u16, |n, d| n * 8 + u16::from(d - b'0'));
            if let Ok(byte) = u8::try_from(n) {
                out.push(byte);
                i += 4;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    out
}

/// The panes a window layout names (`b25f,80x24,0,0{40x24,0,0,1,...}`),
/// in its order. O(n).
pub fn layout_panes(layout: &str) -> Vec<PaneId> {
    let mut panes = Vec::new();
    for segment in layout.split(['{', '}', '[', ']']) {
        let fields: Vec<&str> = segment.split(',').collect();
        let mut i = 0;
        while i + 3 < fields.len() {
            let cell = fields[i].split_once('x').is_some_and(|(w, h)| {
                !w.is_empty()
                    && !h.is_empty()
                    && w.bytes().chain(h.bytes()).all(|b| b.is_ascii_digit())
            });
            if cell && let Ok(id) = fields[i + 3].parse() {
                panes.push(PaneId(id));
                i += 4;
            } else {
                i += 1;
            }
        }
    }
    panes
}

/// A tmux version, as `#{version}` gives it: `3.2a`, `3.4`, `next-3.6`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TmuxVersion {
    pub major: u32,
    pub minor: u32,
}

impl TmuxVersion {
    /// The oldest the app drives: `pause-after` and `%extended-output`
    /// came in 3.2.
    pub const MIN: Self = Self { major: 3, minor: 2 };

    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        // OpenBSD's own tmux follows upstream's and names the OS release.
        if text.starts_with("openbsd-") {
            return Some(Self::MIN);
        }
        let text = text.strip_prefix("next-").unwrap_or(text);
        let (major, rest) = text.split_once('.')?;
        let minor: String = rest.chars().take_while(char::is_ascii_digit).collect();
        Some(Self {
            major: major.parse().ok()?,
            minor: minor.parse().ok()?,
        })
    }
}

/// A window, as [`Command::ListWindows`] lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub window: WindowId,
    /// The active pane, the one shown when the window has one.
    pub pane: PaneId,
    pub panes: u32,
    pub name: String,
}

impl WindowInfo {
    /// One line of [`Command::ListWindows`]'s reply.
    pub fn parse(line: &str) -> Option<Self> {
        let mut parts = line.splitn(4, ' ');
        let window = window_id(parts.next()?)?;
        let pane = pane_id(parts.next()?.as_bytes())?;
        let panes = parts.next()?.parse().ok()?;
        Some(Self {
            window,
            pane,
            panes,
            name: parts.next().unwrap_or("").to_owned(),
        })
    }
}

/// Where a pane's cursor is, and whether it shows its alternate screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a terminal mode tmux reports on its own"
)]
pub struct PaneCursor {
    pub x: u16,
    pub y: u16,
    pub alternate: bool,
    /// Cursor keys in application mode (DECCKM).
    pub keypad: bool,
    /// Mouse reports the pane's program asked for: any motion, buttons,
    /// presses (1003, 1002, 1000), and their SGR form (1006).
    pub mouse_any: bool,
    pub mouse_button: bool,
    pub mouse_standard: bool,
    pub mouse_sgr: bool,
}

/// The formats [`PaneCursor::parse`] reads, in its order.
const CURSOR_FORMAT: &str = "#{cursor_x} #{cursor_y} #{alternate_on} #{keypad_cursor_flag} \
    #{mouse_any_flag} #{mouse_button_flag} #{mouse_standard_flag} #{mouse_sgr_flag}";

impl PaneCursor {
    /// [`Command::Cursor`]'s reply line. Modes after the first three may be
    /// missing (an older tmux), and read as off.
    pub fn parse(line: &str) -> Option<Self> {
        let mut parts = line.split(' ');
        let x = parts.next()?.parse().ok()?;
        let y = parts.next()?.parse().ok()?;
        let alternate = parts.next()? == "1";
        let mut flag = || parts.next() == Some("1");
        Some(Self {
            x,
            y,
            alternate,
            keypad: flag(),
            mouse_any: flag(),
            mouse_button: flag(),
            mouse_standard: flag(),
            mouse_sgr: flag(),
        })
    }
}

/// A command the app sends. Each encodes to whole lines; ids need no
/// quoting, and nothing a host sent goes into one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Version,
    /// Each window: `@W %P PANES NAME`.
    ListWindows,
    /// The pane's screen and up to `history` lines above it, with colours.
    Capture {
        pane: PaneId,
        history: u32,
    },
    /// `cursor_x cursor_y alternate_on`.
    Cursor {
        pane: PaneId,
    },
    /// [`Command::Cursor`] then [`Command::Capture`], on one line: two
    /// replies, with no output between them that neither holds.
    Snapshot {
        pane: PaneId,
        history: u32,
    },
    /// The bytes as keys, as they are: no key names, no key table.
    Keys {
        pane: PaneId,
        bytes: Vec<u8>,
    },
    /// The size the client's windows take.
    Size {
        cols: u16,
        rows: u16,
    },
    /// Stop a pane's output once it is this many seconds behind, rather
    /// than buffer it without end.
    PauseAfter {
        seconds: u32,
    },
    Continue {
        pane: PaneId,
    },
    /// Stop sending a pane's output: tmux drops it for this client, and
    /// the pane's program goes on. [`Command::Continue`] starts it again.
    Pause {
        pane: PaneId,
    },
    NewWindow,
    KillWindow {
        window: WindowId,
    },
    Detach,
}

impl Command {
    /// The command as lines for tmux, each ending in `\n`. Keys longer than
    /// one line goes as several commands, so several replies.
    pub fn encode(&self) -> Vec<String> {
        match self {
            Self::Version => vec!["display-message -p '#{version}'\n".to_owned()],
            Self::ListWindows => vec![
                "list-windows -F '#{window_id} #{pane_id} #{window_panes} #{window_name}'\n"
                    .to_owned(),
            ],
            // Rows as they are, not joined: the screen is drawn again row
            // by row, and the cursor lands where it was.
            Self::Capture { pane, history } => {
                vec![format!("capture-pane -p -e -t {pane} -S -{history}\n")]
            }
            Self::Snapshot { pane, history } => vec![format!(
                "display-message -p -t {pane} '{CURSOR_FORMAT}' ; \
                 capture-pane -p -e -t {pane} -S -{history}\n"
            )],
            Self::Cursor { pane } => {
                vec![format!("display-message -p -t {pane} '{CURSOR_FORMAT}'\n")]
            }
            Self::Keys { pane, bytes } => {
                let head = format!("send-keys -H -t {pane}");
                let per_line = (MAX_COMMAND_LINE - head.len() - 1) / 3;
                bytes
                    .chunks(per_line.max(1))
                    .map(|chunk| {
                        let mut line = head.clone();
                        for b in chunk {
                            let _ = write!(line, " {b:02x}");
                        }
                        line.push('\n');
                        line
                    })
                    .collect()
            }
            Self::Size { cols, rows } => vec![format!("refresh-client -C {cols}x{rows}\n")],
            Self::PauseAfter { seconds } => {
                vec![format!("refresh-client -f pause-after={seconds}\n")]
            }
            Self::Continue { pane } => vec![format!("refresh-client -A '{pane}:continue'\n")],
            Self::Pause { pane } => vec![format!("refresh-client -A '{pane}:pause'\n")],
            Self::NewWindow => vec!["new-window\n".to_owned()],
            Self::KillWindow { window } => vec![format!("kill-window -t {window}\n")],
            Self::Detach => vec!["detach-client\n".to_owned()],
        }
    }
}

#[cfg(test)]
mod tests;
