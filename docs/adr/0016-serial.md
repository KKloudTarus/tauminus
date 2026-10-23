# ADR 0016: Serial ports

- Status: accepted
- Date: 2026-10-23

## Context

v0.3 opens serial consoles: routers, boards, and anything on a USB
adapter. A user picks a port the system lists, or types a path, and the
line settings: speed, data bits, parity, stop bits, flow control. Most
consoles use 115200 8N1 with no flow control.

## Decisions

### Ports are opened, not saved as hosts

A serial port has no address and no login; it is a device on this
computer. "Open a serial port" in the palette lists what is attached,
with what the system says each one is, and opens the choice in a tab.
A workspace keeps a serial pane's port and settings and opens it again,
and lists the port among what it will open before it does.

Only a serial device opens. On Unix the path must resolve to a
character device that is not one of this user's terminals: `/dev/tty`,
`/dev/console`, `/dev/pts/*`, and console ttys would hand the pane that
terminal's keystrokes. On Windows only a `COMn` name opens, so a path
cannot reach a share (and send NTLM to it), a pipe, or a disk.
The speed is one of the standard rates (300 to 2 000 000 baud), which
every UART driver accepts.

### `serialport` behind its own crate, on threads

`tauminus-serial` wraps `serialport` 4 (MPL-2.0), built without
`libudev` so it needs no system library: on Linux it lists ports from
sysfs. `deny.toml` keeps it there. Its API blocks, so as for the local
PTY a reader and a writer each run on a thread. The reader wakes every
100 ms to see whether the session was closed, so a close takes effect
within that; a read error, as when the cable is pulled, ends the
session, after the bytes still waiting are shown. The writer waits as
long as the device holds the line with XOFF or CTS, a poll at a time,
and gives up only on a close or a real error; it never drains, since a
held line would block a drain forever. Output converts through `tauminus-charset` like SSH and Telnet,
and a resize is dropped: a serial line has no window size.

### Tested against a virtual port pair

`socat` joins two pseudo-terminals; the session opens one, the test
plays the device on the other. The tests check output and typed input
both ways in KOI8-R, typing held by XOFF arriving after XON, that a
close ends the session, that losing the far end does, and that files,
`/dev/null`, `/dev/tty`, and pseudo-terminals are refused. CI installs `socat` on Linux.

## Consequences

- Opening a port needs the user's permission on it (the `dialout` group
  on most Linux systems); the error says which port could not open.
- Ports are not saved on their own; a workspace is how to keep one.
