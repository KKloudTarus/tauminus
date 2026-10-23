# ADR 0015: Telnet

- Status: accepted
- Date: 2026-10-23

## Context

v0.3 adds Telnet for routers, switches, serial consoles behind terminal
servers, and old systems that speak nothing else. It carries everything
in clear text, has no host key, and no login of its own: the remote
shell or device asks.

## Decisions

### A saved host has a protocol

`Host.protocol` is SSH or Telnet. A Telnet host uses its address, its
own port or 23 (a group's port is an SSH port), and the nearest
character set; user, jump hosts, keys, and the vault identity do not
apply, and the editor says so, beside a warning that passwords cross in
clear text. A Telnet host in a group that goes through a jump host or a
proxy is refused rather than reached directly behind the group's back.

The backend holds the line: resolving a Telnet host for SSH fails, so no
shell, file transfer, forward, or key install reaches it over SSH, and
switching a host's protocol stops its running forwards. The prod guard,
the history, and broadcast treat it as any saved host. Startup snippets
do not run: Telnet reports a session running once TCP connects, so the
text would go to a device's login prompt. `hosts.json` becomes version 5
only when a Telnet host is saved.

### The protocol is written here, without a network

`tauminus-telnet` holds a sans-IO state machine: bytes in, screen data
and replies out. It frames RFC 854 (IAC doubling, CR NUL), negotiates as
RFC 1143's Q method so an option never loops, and handles the options a
terminal needs: ECHO and SGA from the server, NAWS and TTYPE from this
side, BINARY both ways. Every other option is refused once. A
subnegotiation is cut at 1 KiB. No crate was worth a dependency for
about 400 lines, and the machine is tested byte by byte.

The adapter runs a reader and a writer task over a TCP stream, as the
SSH bridge does, sharing the state behind a mutex held only to work
bytes out. A close always gets through: a reply waiting on a peer that
stopped reading gives way to it, and the writer is dropped with what it
still holds, so a stalled device cannot keep a session's socket alive. Character sets convert with the same `tauminus-charset` the
SSH bridge uses, now a crate of its own, the one place `encoding_rs`
may be used.

### Tested against a real telnetd

BusyBox's `telnetd` runs as any user with a shell and no login, so the
tests start one on a free loopback port: the window size reaches
`stty`, a resize reaches it again, the server's echo shows, and the
session ends with the shell. CI installs `busybox-static` on Linux.

## Consequences

- Telnet sessions have no exit status; they end when the connection
  closes.
- A Telnet password is visible to anyone on the path. The editor's
  warning is the only defence the protocol allows.
