# ADR 0024: tmux windows as tabs

- Status: accepted
- Date: 2026-10-24

## Context

v0.4 shows a host's tmux windows as native tabs through tmux control
mode. A saved host can already join a tmux session on every connect
(ADR 0017), rendered by tmux inside one pane. In control mode tmux sends
each pane's output and its changes as text lines, and takes commands.
A panel of two (architecture, and an advocate who read tmux's, iTerm2's
and WezTerm's sources) weighed where to read the protocol, how panes
map onto the session pipeline, flow control, and what the first slice
holds.

## Decisions

### A host setting, and `tmux -C` on a channel with no PTY

`tmux_tabs` on a host or group (off at the root, `hosts.json` v10) shows
the host's tmux session as tabs. The backend runs
`exec tmux -u -C new-session -A -s NAME` through the login shell on an
exec channel with no PTY. Control clients skip tmux's terminal check;
without a PTY nothing echoes, there is no DCS wrapper, and closing the
channel detaches while the session goes on. The host must use UTF-8: the
stream carries every pane's bytes and is passed through untouched.

### The protocol is read in Rust

`tauminus_domain::tmux_control` reads the stream (`%begin`/`%end`/`%error`
replies, `%output` with its `\ooo` escapes, `%extended-output`, window
and layout changes, `%exit`) and writes the commands, with no I/O. A
reply ends only on the `%end` or `%error` with its `%begin`'s time,
number and flags: a captured screen whose rows read `%end ...` or
`%output ...` stays inside the reply, and cannot write into another pane
or end the client. It bounds a line to 1 MiB and a reply to 20 000 lines
and 8 MiB. Each pane's output
stays on the backend's output path with its own flow control. The app
adds about 3% to what tmux itself takes; control mode carries a pane at
about 40% of a plain PTY's rate, and `\ooo` makes line breaks 4 bytes
each on the link.

### Each shown pane is a session

`TmuxSessions` (app layer) reads the channel and hands each pane's output
to that pane's queue without waiting. One task writes commands, in order,
so the reader never waits on a write that waits on the reader. Each
shown pane is attached to the `SessionManager` as a session with the
saved host's meta, so batching, flow control, recording, triggers,
password fill, the prod guard and the AI assistant work on it as on SSH.
Keys go as `send-keys -H` (bytes as they are, no key names), at most
1000 bytes a command line. A pane whose queue passes 1 MiB is behind: it
is paused in tmux (`refresh-client -A %N:pause`, which drops its output
for this client while its program goes on) until its view takes the
queue below 256 KiB; it then continues and its screen is read again, so
a flood skips to the screen as tmux has it, and the other panes and the
replies go on. A pane no tab shows is paused too, so a busy hidden
window costs the link nothing. `%N:off`, which stops tmux reading the
pane, is never sent: under a flood it crashed tmux 3.4, and with it
every session on that server. A tmux that leaves 65 536 commands
unanswered ends the client, and the app's own questions time out after
15 s. Command lines are wiped once written: one may carry a password the
vault types.

When a pane is shown, the backend reads its cursor, then its screen and
2000 lines of history (`capture-pane -e`), as two commands on one line,
and draws them first. Output that comes before the screen's reply is
already on it, and is dropped.
The view drops what xterm.js answers to a program's queries: tmux
answers them. A cursor report is dropped only while a query for one is
open, since Shift+F3 sends the same bytes.

### Windows as tabs, one pane each

The view opens a tab per window. A new window, a closed one, and a window
whose shown pane went (from its `%layout-change`) come as topology
events. Closing a tab stops showing its window; the window goes on in
tmux, and the client detaches when no tab is left, whether its last tab
closed or its last shown window exited. "New tmux window" and
"Detach from tmux" are palette commands.

### What the first slice refuses

tmux before 3.2 and a session with a split window are refused after
detaching, with a reason, as is a host where tmux does not start; the
host is then joined the plain way, as one terminal. What the login shell
prints before tmux starts is skipped.

## Consequences

- A pane flooding faster than its view draws loses the output between
  falling behind and catching up; its screen is right again after.
- A split made later from another client shows its active pane only.
- tmux's prefix key reaches the pane's program as a key: tmux's own key
  bindings do not apply in a tab.
- Copy mode, tmux's clipboard, and more than one session per connection
  are left out.
- A tab that loses its connection shows as lost; connecting the host
  again brings the windows back.
- With several clients attached, tmux's `window-size` option decides the
  size, and a tab may show less than its pane.
- A password typed by the vault into a tab travels as hex on a
  `send-keys` command line, as any key does, and sits in the tmux
  server's memory while it parses the line.
- A screen row that guesses a reply's exact time and command number
  could still end it; the guess is a 32-bit time and a counter.
